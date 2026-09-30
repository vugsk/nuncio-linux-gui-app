//! Хранилище SQLite (владелец — Rust; Архитектура/nuncio - Данные и хранилище.md,
//! раздел 2). Режим WAL, нумерованные миграции вперёд в одной транзакции,
//! версия схемы в `PRAGMA user_version`. Один поток БД (`DbThread`): остальные
//! шлют ему замыкания, поэтому писатель всегда один.

use std::path::Path;
use std::sync::mpsc;
use std::thread;

use rusqlite::{params, Connection, OptionalExtension};

/// Миграции по порядку: индекс + 1 = версия схемы после применения.
const MIGRATIONS: &[&str] = &[
    // 1 (В3): схема из заметки «Данные и хранилище», раздел 2.
    r#"
    CREATE TABLE peers (
        id          INTEGER PRIMARY KEY,         -- id Telegram (пользователь, чат, канал: со знаком по виду)
        kind        INTEGER NOT NULL,            -- 0 user, 1 chat, 2 channel
        name        TEXT NOT NULL DEFAULT '',
        username    TEXT NOT NULL DEFAULT '',
        access_hash INTEGER NOT NULL DEFAULT 0,
        photo_ref   TEXT NOT NULL DEFAULT ''
    );
    CREATE TABLE dialogs (
        peer_id        INTEGER PRIMARY KEY REFERENCES peers(id),
        top_message_id INTEGER NOT NULL DEFAULT 0,
        unread         INTEGER NOT NULL DEFAULT 0,
        pinned         INTEGER NOT NULL DEFAULT 0,
        folder         INTEGER NOT NULL DEFAULT 0,
        draft          TEXT NOT NULL DEFAULT ''
    );
    CREATE TABLE messages (
        peer_id    INTEGER NOT NULL,
        id         INTEGER NOT NULL,
        date       INTEGER NOT NULL,
        sender_id  INTEGER NOT NULL DEFAULT 0,
        text       TEXT NOT NULL DEFAULT '',
        entities   BLOB,
        reply_to   INTEGER NOT NULL DEFAULT 0,
        send_state INTEGER NOT NULL DEFAULT 0,  -- 0 отправлено, 1 в очереди, 2 отправляется, 3 ошибка
        media_ref  TEXT NOT NULL DEFAULT '',
        PRIMARY KEY (peer_id, id)
    ) WITHOUT ROWID;
    CREATE TABLE outbox (
        local_id   INTEGER PRIMARY KEY AUTOINCREMENT,
        peer_id    INTEGER NOT NULL,
        random_id  INTEGER NOT NULL UNIQUE,
        content    BLOB NOT NULL,
        attempts   INTEGER NOT NULL DEFAULT 0,
        error      TEXT NOT NULL DEFAULT '',
        queued_at  INTEGER NOT NULL
    );
    CREATE TABLE media (
        file_id    TEXT PRIMARY KEY,
        kind       INTEGER NOT NULL,
        size       INTEGER NOT NULL DEFAULT 0,
        cache_path TEXT NOT NULL DEFAULT '',
        state      INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE update_state (
        channel_id INTEGER PRIMARY KEY,           -- 0 — общее состояние
        pts        INTEGER NOT NULL DEFAULT 0,
        qts        INTEGER NOT NULL DEFAULT 0,
        seq        INTEGER NOT NULL DEFAULT 0,
        date       INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE drafts (peer_id INTEGER PRIMARY KEY, text TEXT NOT NULL);
    CREATE VIRTUAL TABLE messages_fts USING fts5(text, content='', tokenize='unicode61');
    "#,
    // 2 (В4): поля для вертикали — фото и признаки собеседника, прочитанность
    // диалога, направление и вид сообщения, индексы для списка и истории.
    r#"
    ALTER TABLE peers ADD COLUMN photo_id INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE peers ADD COLUMN photo_dc INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE peers ADD COLUMN flags INTEGER NOT NULL DEFAULT 0;  -- 1 self, 2 bot, 4 broadcast
    ALTER TABLE dialogs ADD COLUMN read_inbox_max INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE dialogs ADD COLUMN read_outbox_max INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE dialogs ADD COLUMN top_date INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE messages ADD COLUMN out INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE messages ADD COLUMN service TEXT NOT NULL DEFAULT '';
    ALTER TABLE messages ADD COLUMN edit_date INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE messages ADD COLUMN random_id INTEGER NOT NULL DEFAULT 0;
    CREATE INDEX dialogs_order ON dialogs (folder, pinned DESC, top_date DESC);
    "#,
    // 3 (В5): очередь отправки (О-9, О-12): состояние, ответ, время следующей попытки.
    r#"
    ALTER TABLE outbox ADD COLUMN reply_to INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE outbox ADD COLUMN state INTEGER NOT NULL DEFAULT 0;     -- 0 в очереди, 1 отправляется, 2 ошибка
    ALTER TABLE outbox ADD COLUMN next_try INTEGER NOT NULL DEFAULT 0;
    CREATE INDEX outbox_peer ON outbox (peer_id, local_id);
    "#,
    // 4 (В5): «переслано от» и закрепление (Чт-3, Чт-14).
    r#"
    ALTER TABLE messages ADD COLUMN fwd_from_id INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE messages ADD COLUMN fwd_from_name TEXT NOT NULL DEFAULT '';
    ALTER TABLE messages ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
    "#,
    // 5 (В5): известные непрерывные участки истории (окно модели и «дырки»).
    // lo = 0 — от начала чата, hi = 2147483647 — до настоящего (дальше — обновления).
    r#"
    CREATE TABLE history_ranges (
        peer_id INTEGER NOT NULL,
        lo      INTEGER NOT NULL,
        hi      INTEGER NOT NULL,
        PRIMARY KEY (peer_id, lo)
    ) WITHOUT ROWID;
    "#,
    // 6 (В5): форматирование в очереди (Дз-15); у messages столбец entities есть
    // с первой схемы. Формат — строки `вид смещение длина url`.
    r#"
    ALTER TABLE outbox ADD COLUMN entities TEXT NOT NULL DEFAULT '';
    "#,
    // 7 (В6): поиск по базе, папки, настройки диалогов.
    // messages — WITHOUT ROWID, поэтому у каждого сообщения свой ключ в
    // msg_keys, а индекс FTS5 без хранения текста удаляется по нему
    // (contentless_delete). Индекс ведут триггеры: всегда согласован с таблицей.
    // «ё» индексируется как «е» (remove_diacritics работает только для латиницы).
    r#"
    DROP TABLE messages_fts;
    CREATE TABLE msg_keys (
        rid     INTEGER PRIMARY KEY,
        peer_id INTEGER NOT NULL,
        id      INTEGER NOT NULL,
        UNIQUE (peer_id, id)
    );
    CREATE VIRTUAL TABLE messages_fts USING fts5(text, content='', contentless_delete=1,
        tokenize='unicode61 remove_diacritics 2');
    CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
        INSERT INTO msg_keys (peer_id, id) VALUES (new.peer_id, new.id);
        INSERT INTO messages_fts (rowid, text) VALUES (last_insert_rowid(), replace(replace(new.text, 'ё', 'е'), 'Ё', 'Е'));
    END;
    CREATE TRIGGER messages_au AFTER UPDATE OF text ON messages WHEN old.text != new.text BEGIN
        DELETE FROM messages_fts WHERE rowid = (SELECT rid FROM msg_keys WHERE peer_id = new.peer_id AND id = new.id);
        INSERT INTO messages_fts (rowid, text)
            SELECT rid, replace(replace(new.text, 'ё', 'е'), 'Ё', 'Е') FROM msg_keys WHERE peer_id = new.peer_id AND id = new.id;
    END;
    CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
        DELETE FROM messages_fts WHERE rowid = (SELECT rid FROM msg_keys WHERE peer_id = old.peer_id AND id = old.id);
        DELETE FROM msg_keys WHERE peer_id = old.peer_id AND id = old.id;
    END;
    INSERT INTO msg_keys (peer_id, id) SELECT peer_id, id FROM messages;
    INSERT INTO messages_fts (rowid, text)
        SELECT k.rid, replace(replace(m.text, 'ё', 'е'), 'Ё', 'Е') FROM msg_keys k
        JOIN messages m ON m.peer_id = k.peer_id AND m.id = k.id;
    ALTER TABLE dialogs ADD COLUMN muted INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE dialogs ADD COLUMN unread_mark INTEGER NOT NULL DEFAULT 0;
    CREATE TABLE folders (
        id       INTEGER PRIMARY KEY,
        pos      INTEGER NOT NULL,
        title    TEXT NOT NULL,
        emoticon TEXT NOT NULL DEFAULT '',
        flags    INTEGER NOT NULL DEFAULT 0,
        pinned   TEXT NOT NULL DEFAULT '',   -- помеченные id через запятую
        include  TEXT NOT NULL DEFAULT '',
        exclude  TEXT NOT NULL DEFAULT ''
    );
    "#,
    // 8 (В7): вложения подробно (JSON описания), альбомы, индекс кэша файлов.
    r#"
    ALTER TABLE messages ADD COLUMN media_json TEXT NOT NULL DEFAULT '';
    ALTER TABLE messages ADD COLUMN grouped_id INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE media ADD COLUMN last_used INTEGER NOT NULL DEFAULT 0;
    "#,
];

/// Текущая версия схемы.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    /// Файл базы новее этой сборки (миграции только вперёд).
    TooNew(u32),
    /// Файл не является базой SQLite или повреждён (Со-4, Со-5).
    Corrupt(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlite(e) => write!(f, "SQLite: {e}"),
            Self::TooNew(v) => write!(f, "база создана более новой версией приложения (схема {v})"),
            Self::Corrupt(e) => write!(f, "база повреждена: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        match &e {
            rusqlite::Error::SqliteFailure(f, _)
                if matches!(f.code, rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt) =>
            {
                Self::Corrupt(e.to_string())
            }
            _ => Self::Sqlite(e),
        }
    }
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Открывает (создаёт) базу и применяет недостающие миграции.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| StoreError::Corrupt(e.to_string()))?;
        }
        let conn = Connection::open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Self::init(conn)
    }

    /// База в памяти (тесты).
    pub fn in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let check: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if check != "ok" {
            return Err(StoreError::Corrupt(check));
        }
        let mut s = Self { conn };
        s.migrate()?;
        Ok(s)
    }

    pub fn schema_version(&self) -> Result<u32, StoreError> {
        Ok(self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    fn migrate(&mut self) -> Result<(), StoreError> {
        let current = self.schema_version()?;
        if current > SCHEMA_VERSION {
            return Err(StoreError::TooNew(current));
        }
        if current == SCHEMA_VERSION {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", (i + 1) as u32)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Удаляет все данные аккаунта (выход, завершённая сессия: Нс-6, Со-6).
    /// Настройки приложения (не аккаунта) остаются.
    pub fn wipe_account(&mut self) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        // Сообщения первыми: триггер убирает их из индекса поиска и msg_keys;
        // затем индекс очищается целиком на всякий случай.
        for t in ["messages", "msg_keys", "dialogs", "peers", "outbox", "media", "update_state", "drafts", "folders",
            "history_ranges"]
        {
            tx.execute(&format!("DELETE FROM {t}"), [])?;
        }
        tx.execute("INSERT INTO messages_fts (messages_fts) VALUES ('delete-all')", [])?;
        tx.execute("DELETE FROM settings WHERE key LIKE 'account.%'", [])?;
        tx.commit()?;
        self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
        Ok(())
    }

    /// Прямой доступ для use-case'ов домена в потоке БД.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub(crate) fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
}

type Job = Box<dyn FnOnce(&mut Store) + Send>;

/// Поток БД: единственный владелец `Store`.
pub struct DbThread {
    tx: Option<mpsc::Sender<Job>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl DbThread {
    pub fn spawn(store: Store) -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        let handle = thread::Builder::new()
            .name("nuncio-db".into())
            .spawn(move || {
                let mut store = store;
                for job in rx {
                    job(&mut store);
                }
            })
            .expect("не удалось запустить поток БД");
        Self { tx: Some(tx), handle: Some(handle) }
    }

    /// Выполняет замыкание в потоке БД и ждёт результат.
    pub fn call<R: Send + 'static>(&self, f: impl FnOnce(&mut Store) -> R + Send + 'static) -> Option<R> {
        let (rtx, rrx) = mpsc::channel();
        self.tx.as_ref()?.send(Box::new(move |s| {
            let _ = rtx.send(f(s));
        })).ok()?;
        rrx.recv().ok()
    }

    /// Выполняет замыкание в потоке БД, не дожидаясь.
    pub fn run(&self, f: impl FnOnce(&mut Store) + Send + 'static) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Box::new(f));
        }
    }
}

impl Drop for DbThread {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_once_and_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nuncio.db");
        {
            let s = Store::open(&path).unwrap();
            assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
            s.set_setting("ui.lang", "ru").unwrap();
            // Все таблицы схемы на месте.
            for t in ["peers", "dialogs", "messages", "outbox", "media", "update_state", "settings", "drafts",
                "messages_fts"]
            {
                let n: i64 = s
                    .conn()
                    .query_row("SELECT count(*) FROM sqlite_master WHERE name = ?1", [t], |r| r.get(0))
                    .unwrap();
                assert_eq!(n, 1, "нет таблицы {t}");
            }
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.setting("ui.lang").unwrap().as_deref(), Some("ru"));
        assert_eq!(s.setting("нет").unwrap(), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn newer_schema_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nuncio.db");
        drop(Store::open(&path).unwrap());
        Connection::open(&path).unwrap().pragma_update(None, "user_version", SCHEMA_VERSION + 1).unwrap();
        assert!(matches!(Store::open(&path), Err(StoreError::TooNew(_))));
    }

    #[test]
    fn corrupt_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nuncio.db");
        std::fs::write(&path, vec![0x5au8; 8192]).unwrap();
        assert!(matches!(Store::open(&path), Err(StoreError::Corrupt(_))), "мусор принят за базу");
    }

    #[test]
    fn wipe_keeps_app_settings() {
        let mut s = Store::in_memory().unwrap();
        s.set_setting("ui.lang", "en").unwrap();
        s.set_setting("account.user_id", "5").unwrap();
        s.conn().execute("INSERT INTO peers (id, kind, name) VALUES (1, 0, 'x')", []).unwrap();
        s.conn().execute("INSERT INTO messages (peer_id, id, date, text) VALUES (1, 1, 1, 'привет мир')", []).unwrap();
        s.wipe_account().unwrap();
        let n: i64 = s.conn().query_row("SELECT count(*) FROM messages_fts WHERE messages_fts MATCH 'привет'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "индекс поиска очищен");
        assert_eq!(s.setting("ui.lang").unwrap().as_deref(), Some("en"));
        assert_eq!(s.setting("account.user_id").unwrap(), None);
        let n: i64 = s.conn().query_row("SELECT count(*) FROM peers", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn fts5_follows_messages() {
        let s = Store::in_memory().unwrap();
        let c = s.conn();
        let hits = |q: &str| -> i64 {
            c.query_row("SELECT count(*) FROM messages_fts WHERE messages_fts MATCH ?1", [q], |r| r.get(0)).unwrap()
        };
        c.execute("INSERT INTO messages (peer_id, id, date, text) VALUES (5, 7, 1, 'Привет, мир')", []).unwrap();
        assert_eq!(hits("привет"), 1, "регистр и знаки не мешают");
        c.execute("UPDATE messages SET text = 'пока' WHERE peer_id = 5 AND id = 7", []).unwrap();
        assert_eq!((hits("привет"), hits("пока")), (0, 1), "правка переиндексирует");
        c.execute("DELETE FROM messages WHERE peer_id = 5 AND id = 7", []).unwrap();
        assert_eq!(hits("пока"), 0, "удаление убирает из индекса");
    }

    #[test]
    fn db_thread_serializes_calls() {
        let db = DbThread::spawn(Store::in_memory().unwrap());
        for i in 0..100 {
            db.run(move |s| s.set_setting("n", &i.to_string()).unwrap());
        }
        assert_eq!(db.call(|s| s.setting("n").unwrap()).flatten().as_deref(), Some("99"));
    }
}
