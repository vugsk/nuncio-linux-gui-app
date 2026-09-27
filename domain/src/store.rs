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
        // Бесконтентная FTS5 очищается специальной командой, а не DELETE.
        tx.execute("INSERT INTO messages_fts (messages_fts) VALUES ('delete-all')", [])?;
        for t in ["messages", "dialogs", "peers", "outbox", "media", "update_state", "drafts"] {
            tx.execute(&format!("DELETE FROM {t}"), [])?;
        }
        tx.execute("DELETE FROM settings WHERE key LIKE 'account.%'", [])?;
        tx.commit()?;
        self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
        Ok(())
    }

    /// Прямой доступ для use-case'ов домена в потоке БД.
    pub fn conn(&self) -> &Connection {
        &self.conn
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
        s.conn().execute("INSERT INTO messages_fts (rowid, text) VALUES (1, 'привет мир')", []).unwrap();
        s.wipe_account().unwrap();
        assert_eq!(s.setting("ui.lang").unwrap().as_deref(), Some("en"));
        assert_eq!(s.setting("account.user_id").unwrap(), None);
        let n: i64 = s.conn().query_row("SELECT count(*) FROM peers", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn fts5_is_available() {
        let s = Store::in_memory().unwrap();
        s.conn().execute("INSERT INTO messages_fts (rowid, text) VALUES (7, 'Привет, мир')", []).unwrap();
        let id: i64 = s
            .conn()
            .query_row("SELECT rowid FROM messages_fts WHERE messages_fts MATCH 'привет'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(id, 7);
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
