//! Диалоги, собеседники и сообщения в SQLite (Архитектура/nuncio - Данные и
//! хранилище.md, разделы 2–3). Пачка обновлений и новое состояние
//! (`update_state`) пишутся одной транзакцией (О-5): после сбоя до коммита Go
//! получит старое состояние и запросит разницу заново, дубли отсекаются по
//! ключу `(peer_id, id)`.

use rusqlite::{params, OptionalExtension, Row, Transaction};

use crate::model::{
    decode_entities, encode_entities, Dialog, DomainUpdate, Folder, Message, Peer, PeerKind, UpdateState,
    UpdatesBatch, CHANNEL_SHIFT,
};
use crate::history;
use crate::store::{Store, StoreError};

/// Запрос пользователя — в выражение FTS5: каждое слово в кавычках
/// (служебные знаки не работают) и как префикс, все слова обязательны.
pub fn fts_query(query: &str) -> String {
    query
        .replace(['ё', 'Ё'], "е")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{}\"*", w.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// «Прочитано до ?2» в чате ?1: непрочитанных меньше на прочитанные входящие,
/// до верха диалога — ноль. Уже прочитанное повторно не вычитается.
const MARK_READ_SQL: &str = "UPDATE dialogs SET
       unread = CASE WHEN ?2 >= top_message_id THEN 0 ELSE MAX(0, unread - (
         SELECT COUNT(*) FROM messages m WHERE m.peer_id = ?1 AND m.id > dialogs.read_inbox_max
           AND m.id <= ?2 AND m.out = 0 AND m.service = '')) END,
       read_inbox_max = MAX(read_inbox_max, ?2)
     WHERE peer_id = ?1";

/// Строка списка: диалог, собеседник и последнее сообщение.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DialogRow {
    pub dialog: Dialog,
    pub peer: Peer,
    pub last: Option<Message>,
    pub top_date: i64,
    pub avatar_path: String,
}

const F_SELF: i64 = 1;
const F_BOT: i64 = 2;
const F_BROADCAST: i64 = 4;
const F_CONTACT: i64 = 8;

fn upsert_peer(tx: &Transaction, p: &Peer) -> rusqlite::Result<()> {
    let kind = match p.kind {
        PeerKind::User => 0,
        PeerKind::Chat => 1,
        PeerKind::Channel => 2,
    };
    let flags = (if p.is_self { F_SELF } else { 0 }) | (if p.is_bot { F_BOT } else { 0 })
        | (if p.broadcast { F_BROADCAST } else { 0 }) | (if p.contact { F_CONTACT } else { 0 });
    // У «min»-сведений access_hash не годится: старый не затираем. Пустое имя
    // (UserEmpty) тоже не затирает известное.
    tx.execute(
        "INSERT INTO peers (id, kind, name, username, access_hash, photo_id, photo_dc, flags)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
           name = CASE WHEN excluded.name = '' THEN peers.name ELSE excluded.name END,
           username = CASE WHEN ?9 THEN peers.username ELSE excluded.username END,
           access_hash = CASE WHEN ?9 AND peers.access_hash != 0 THEN peers.access_hash ELSE excluded.access_hash END,
           photo_id = CASE WHEN ?9 THEN peers.photo_id ELSE excluded.photo_id END,
           photo_dc = CASE WHEN ?9 THEN peers.photo_dc ELSE excluded.photo_dc END,
           photo_ref = CASE WHEN excluded.photo_id != peers.photo_id AND NOT ?9 THEN '' ELSE peers.photo_ref END,
           flags = CASE WHEN ?9 THEN peers.flags ELSE excluded.flags END",
        params![p.id, kind, p.name, p.username, p.access_hash, p.photo_id, p.photo_dc, flags, p.min],
    )?;
    Ok(())
}

pub(crate) fn insert_message(tx: &Transaction, m: &Message) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO messages (peer_id, id, date, sender_id, text, reply_to, send_state, media_ref, out, service,
                               edit_date, random_id, fwd_from_id, fwd_from_name, pinned, entities, media_json,
                               grouped_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
         ON CONFLICT(peer_id, id) DO UPDATE SET text = excluded.text, media_ref = excluded.media_ref,
           entities = excluded.entities, grouped_id = excluded.grouped_id,
           media_json = CASE WHEN excluded.media_json != '' THEN excluded.media_json ELSE messages.media_json END,
           edit_date = excluded.edit_date, service = excluded.service, pinned = excluded.pinned,
           fwd_from_id = excluded.fwd_from_id, fwd_from_name = excluded.fwd_from_name,
           random_id = CASE WHEN excluded.random_id != 0 THEN excluded.random_id ELSE messages.random_id END",
        params![m.peer_id, m.id, m.date, m.from_id, m.text, m.reply_to, m.media, m.out, m.service, m.edit_date,
            m.random_id, m.fwd_from_id, m.fwd_from_name, m.pinned, encode_entities(&m.entities),
            m.media_info.as_ref().and_then(|x| serde_json::to_string(x).ok()).unwrap_or_default(), m.grouped_id],
    )?;
    Ok(())
}

/// Новое сообщение поднимает диалог наверх; входящее сверх прочитанного — +1 к счётчику.
fn bump_dialog(tx: &Transaction, m: &Message, count_unread: bool) -> rusqlite::Result<()> {
    // Собеседник может прийти без сведений (updateShortMessage от незнакомца):
    // заготовка строки, имя допишет следующая пачка с users/chats.
    let kind = match PeerKind::of(m.peer_id) {
        PeerKind::User => 0,
        PeerKind::Chat => 1,
        PeerKind::Channel => 2,
    };
    tx.execute("INSERT OR IGNORE INTO peers (id, kind) VALUES (?1, ?2)", params![m.peer_id, kind])?;
    let unread = i32::from(count_unread && !m.out && m.service.is_empty());
    tx.execute(
        "INSERT INTO dialogs (peer_id, top_message_id, top_date, unread) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(peer_id) DO UPDATE SET
           unread = CASE WHEN ?2 > dialogs.top_message_id AND ?2 > dialogs.read_inbox_max
                         THEN dialogs.unread + ?4 ELSE dialogs.unread END,
           top_date = CASE WHEN ?2 > dialogs.top_message_id THEN ?3 ELSE dialogs.top_date END,
           top_message_id = MAX(dialogs.top_message_id, ?2)",
        params![m.peer_id, m.id, m.date, unread],
    )?;
    Ok(())
}

fn peer_from_row(r: &Row, o: usize) -> rusqlite::Result<Peer> {
    let flags: i64 = r.get(o + 7)?;
    let id: i64 = r.get(o)?;
    Ok(Peer {
        id,
        kind: PeerKind::of(id),
        name: r.get(o + 1)?,
        username: r.get(o + 2)?,
        access_hash: r.get(o + 3)?,
        photo_id: r.get(o + 4)?,
        photo_dc: r.get(o + 5)?,
        is_self: flags & F_SELF != 0,
        is_bot: flags & F_BOT != 0,
        broadcast: flags & F_BROADCAST != 0,
        min: false,
        contact: flags & F_CONTACT != 0,
    })
}

pub(crate) const MSG_COLS: &str = "peer_id, id, date, sender_id, text, reply_to, media_ref, out, service, edit_date, random_id, \
                        send_state, fwd_from_id, fwd_from_name, pinned, entities, media_json, grouped_id";

pub(crate) fn message_from_row(r: &Row, o: usize) -> rusqlite::Result<Message> {
    Ok(Message {
        peer_id: r.get(o)?,
        id: r.get(o + 1)?,
        date: r.get(o + 2)?,
        from_id: r.get(o + 3)?,
        text: r.get(o + 4)?,
        reply_to: r.get(o + 5)?,
        media: r.get(o + 6)?,
        out: r.get(o + 7)?,
        service: r.get(o + 8)?,
        edit_date: r.get(o + 9)?,
        random_id: r.get(o + 10)?,
        send_state: r.get(o + 11)?,
        send_error: String::new(),
        fwd_from_id: r.get(o + 12)?,
        fwd_from_name: r.get(o + 13)?,
        pinned: r.get(o + 14)?,
        entities: decode_entities(&r.get::<_, Option<String>>(o + 15)?.unwrap_or_default()),
        media_info: serde_json::from_str(&r.get::<_, String>(o + 16)?).ok(),
        grouped_id: r.get(o + 17)?,
    })
}

impl Store {
    /// Пачка обновлений и новое состояние — одной транзакцией. Возвращает
    /// собеседников, чьи диалоги или история изменились.
    pub fn apply_batch(&mut self, b: &UpdatesBatch) -> Result<Vec<i64>, StoreError> {
        let tx = self.conn_mut().transaction()?;
        let mut touched = Vec::new();
        for p in &b.peers {
            upsert_peer(&tx, p)?;
        }
        for u in &b.updates {
            match u {
                DomainUpdate::NewMessage(m) => {
                    insert_message(&tx, m)?;
                    bump_dialog(&tx, m, true)?;
                    history::note_new(&tx, m.peer_id, m.id)?;
                    touched.push(m.peer_id);
                }
                DomainUpdate::EditMessage(m) => {
                    insert_message(&tx, m)?;
                    touched.push(m.peer_id);
                }
                DomainUpdate::DeleteMessages { peer_id, ids } => {
                    for id in ids {
                        if *peer_id == 0 {
                            // Личные и группы: id общие для аккаунта, каналы не затрагиваются.
                            let mut st = tx.prepare_cached(
                                "DELETE FROM messages WHERE id = ?1 AND peer_id >= ?2 RETURNING peer_id",
                            )?;
                            let peers: Vec<i64> =
                                st.query_map(params![id, -CHANNEL_SHIFT], |r| r.get(0))?.collect::<Result<_, _>>()?;
                            touched.extend(peers);
                        } else {
                            tx.execute("DELETE FROM messages WHERE peer_id = ?1 AND id = ?2", params![peer_id, id])?;
                            touched.push(*peer_id);
                        }
                    }
                }
                DomainUpdate::ReadInbox { peer_id, max_id, still_unread } if *still_unread < 0 => {
                    // Своё «прочитано»: остаток сервер не сообщил — по базе.
                    tx.execute(MARK_READ_SQL, params![peer_id, max_id])?;
                    touched.push(*peer_id);
                }
                DomainUpdate::ReadInbox { peer_id, max_id, still_unread } => {
                    tx.execute(
                        "UPDATE dialogs SET read_inbox_max = MAX(read_inbox_max, ?2), unread = ?3 WHERE peer_id = ?1",
                        params![peer_id, max_id, still_unread],
                    )?;
                    touched.push(*peer_id);
                }
                DomainUpdate::ReadOutbox { peer_id, max_id } => {
                    tx.execute(
                        "UPDATE dialogs SET read_outbox_max = MAX(read_outbox_max, ?2) WHERE peer_id = ?1",
                        params![peer_id, max_id],
                    )?;
                    touched.push(*peer_id);
                }
                DomainUpdate::Reset => history::cut_live_all(&tx)?,
                DomainUpdate::Pinned { peer_id, ids, pinned } => {
                    for id in ids {
                        tx.execute("UPDATE messages SET pinned = ?3 WHERE peer_id = ?1 AND id = ?2",
                            params![peer_id, id, pinned])?;
                    }
                    touched.push(*peer_id);
                }
                DomainUpdate::FoldersChanged | DomainUpdate::PinnedDialogs => {}
                DomainUpdate::PeerFolder { peer_id, folder } => {
                    tx.execute("UPDATE dialogs SET folder = ?2, pinned = 0 WHERE peer_id = ?1", params![peer_id, folder])?;
                    touched.push(*peer_id);
                }
                DomainUpdate::PeerSettings { peer_id, muted, pinned, unread_mark } => {
                    for (col, v) in [("muted", muted), ("pinned", pinned), ("unread_mark", unread_mark)] {
                        if let Some(v) = v {
                            tx.execute(&format!("UPDATE dialogs SET {col} = ?2 WHERE peer_id = ?1"), params![peer_id, v])?;
                        }
                    }
                    touched.push(*peer_id);
                }
                DomainUpdate::ResetChannel { peer_id } => {
                    tx.execute("DELETE FROM messages WHERE peer_id = ?1", params![peer_id])?;
                    tx.execute("DELETE FROM history_ranges WHERE peer_id = ?1", params![peer_id])?;
                    touched.push(*peer_id);
                }
            }
        }
        for (ch, pts) in &b.channel_states {
            tx.execute(
                "INSERT INTO update_state (channel_id, pts) VALUES (?1, ?2)
                 ON CONFLICT(channel_id) DO UPDATE SET pts = ?2",
                params![ch, pts],
            )?;
        }
        if let Some(s) = b.state {
            tx.execute(
                "INSERT INTO update_state (channel_id, pts, qts, seq, date) VALUES (0, ?1, ?2, ?3, ?4)
                 ON CONFLICT(channel_id) DO UPDATE SET pts = ?1, qts = ?2, seq = ?3, date = ?4",
                params![s.pts, s.qts, s.seq, s.date],
            )?;
        }
        tx.commit()?;
        touched.sort_unstable();
        touched.dedup();
        Ok(touched)
    }

    /// Страница диалогов с сервера: собеседники, последние сообщения, счётчики.
    pub fn save_dialogs(&mut self, dialogs: &[Dialog], messages: &[Message], peers: &[Peer]) -> Result<(), StoreError> {
        let tx = self.conn_mut().transaction()?;
        for p in peers {
            upsert_peer(&tx, p)?;
        }
        // Верх диалога, которого не было в базе, мог прийти мимо обновлений
        // (например, канал без getChannelDifference): участок «до настоящего»
        // тогда не продолжается, а начинается заново с верха.
        for d in dialogs.iter().filter(|d| d.top_message > 0) {
            let known: bool = tx
                .query_row("SELECT 1 FROM messages WHERE peer_id = ?1 AND id = ?2", params![d.peer_id, d.top_message],
                    |_| Ok(true))
                .optional()?
                .unwrap_or(false);
            if !known {
                history::cut_live(&tx, d.peer_id)?;
            }
        }
        for m in messages {
            insert_message(&tx, m)?;
        }
        for d in dialogs.iter().filter(|d| d.top_message > 0) {
            history::add_range(&tx, history::Span { peer_id: d.peer_id, lo: d.top_message, hi: history::LIVE })?;
        }
        for d in dialogs {
            let top_date: i64 = messages
                .iter()
                .find(|m| m.peer_id == d.peer_id && m.id == d.top_message)
                .map(|m| m.date)
                .unwrap_or(0);
            tx.execute(
                "INSERT INTO dialogs (peer_id, top_message_id, unread, pinned, folder, read_inbox_max, read_outbox_max,
                                      top_date, muted, unread_mark)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(peer_id) DO UPDATE SET top_message_id = ?2, unread = ?3, pinned = ?4, folder = ?5,
                   read_inbox_max = ?6, read_outbox_max = ?7,
                   top_date = CASE WHEN ?8 != 0 THEN ?8 ELSE dialogs.top_date END, muted = ?9, unread_mark = ?10",
                params![d.peer_id, d.top_message, d.unread_count, d.pinned, d.folder, d.read_inbox_max_id,
                    d.read_outbox_max_id, top_date, d.muted, d.unread_mark],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Сообщения с сервера. `span` — непрерывный участок, который покрывает
    /// страница истории; у цитат, закреплённых и т. п. его нет.
    pub fn save_history(&mut self, messages: &[Message], peers: &[Peer], span: Option<history::Span>)
        -> Result<(), StoreError> {
        let tx = self.conn_mut().transaction()?;
        for p in peers {
            upsert_peer(&tx, p)?;
        }
        for m in messages {
            insert_message(&tx, m)?;
            bump_dialog(&tx, m, false)?;
        }
        if let Some(s) = span {
            history::add_range(&tx, s)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Список: сначала закреплённые, потом по времени последнего сообщения (Сп, раздел 3).
    /// Папка 0 — основной список, 1 — архив; −1 — все (для папок-фильтров).
    pub fn dialog_rows(&self, folder: i32, limit: usize) -> Result<Vec<DialogRow>, StoreError> {
        let mut st = self.conn().prepare_cached(&format!(
            "SELECT d.peer_id, d.top_message_id, d.unread, d.pinned, d.folder, d.read_inbox_max, d.read_outbox_max,
                    d.top_date,
                    p.id, p.name, p.username, p.access_hash, p.photo_id, p.photo_dc, p.photo_ref, p.flags,
                    m.{}, d.muted, d.unread_mark
             FROM dialogs d
             JOIN peers p ON p.id = d.peer_id
             LEFT JOIN messages m ON m.peer_id = d.peer_id AND m.id = d.top_message_id
             WHERE ?1 < 0 OR d.folder = ?1
             ORDER BY d.pinned DESC, d.top_date DESC, d.top_message_id DESC
             LIMIT ?2",
            MSG_COLS.replace(", ", ", m.")
        ))?;
        let n = MSG_COLS.split(", ").count();
        let rows = st.query_map(params![folder, limit as i64], |r| {
            let dialog = Dialog {
                peer_id: r.get(0)?,
                top_message: r.get(1)?,
                unread_count: r.get(2)?,
                pinned: r.get(3)?,
                folder: r.get(4)?,
                read_inbox_max_id: r.get(5)?,
                read_outbox_max_id: r.get(6)?,
                muted: r.get(16 + n)?,
                unread_mark: r.get(17 + n)?,
            };
            let peer = peer_from_row(r, 8)?;
            let last = match r.get::<_, Option<i64>>(16)? {
                Some(_) => Some(message_from_row(r, 16)?),
                None => None,
            };
            Ok(DialogRow { dialog, peer, last, top_date: r.get(7)?, avatar_path: r.get(14)? })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Окно истории: `limit` сообщений до `before` (не включая), по возрастанию id.
    pub fn history(&self, peer_id: i64, before: Option<i32>, limit: usize) -> Result<Vec<Message>, StoreError> {
        // Отправленные сообщения идут по id; локальные (очередь, id < 0) — в конце
        // по времени постановки: порядок как у отправки.
        let mut st = self.conn().prepare_cached(&format!(
            "SELECT {}, COALESCE(o.error, '') FROM messages m
             LEFT JOIN outbox o ON o.local_id = -m.id AND m.id < 0
             WHERE m.peer_id = ?1 AND ((m.id > 0 AND m.id < ?2) OR (m.id < 0 AND ?2 = 2147483647))
             ORDER BY (m.id < 0) DESC, CASE WHEN m.id < 0 THEN m.id ELSE -m.id END ASC LIMIT ?3",
            MSG_COLS.split(", ").map(|c| format!("m.{c}")).collect::<Vec<_>>().join(", ")
        ))?;
        let mut v: Vec<Message> = st
            .query_map(params![peer_id, before.unwrap_or(i32::MAX), limit as i64], |r| {
                let mut m = message_from_row(r, 0)?;
                m.send_error = r.get(18)?;
                Ok(m)
            })?
            .collect::<Result<_, _>>()?;
        v.reverse();
        Ok(v)
    }

    pub fn peer(&self, id: i64) -> Result<Option<Peer>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id, name, username, access_hash, photo_id, photo_dc, photo_ref, flags FROM peers WHERE id = ?1",
                params![id],
                |r| peer_from_row(r, 0),
            )
            .optional()?)
    }

    pub fn update_state(&self) -> Result<Option<UpdateState>, StoreError> {
        Ok(self
            .conn()
            .query_row("SELECT pts, qts, seq, date FROM update_state WHERE channel_id = 0", [], |r| {
                Ok(UpdateState { pts: r.get(0)?, qts: r.get(1)?, seq: r.get(2)?, date: r.get(3)? })
            })
            .optional()?)
    }

    /// Закреплённые сообщения чата (новые первыми) — полоса Чт-14.
    pub fn pinned(&self, peer_id: i64) -> Result<Vec<Message>, StoreError> {
        let mut st = self.conn().prepare_cached(&format!(
            "SELECT {MSG_COLS} FROM messages WHERE peer_id = ?1 AND pinned = 1 ORDER BY id DESC LIMIT 50"
        ))?;
        let v = st.query_map(params![peer_id], |r| message_from_row(r, 0))?.collect::<Result<_, _>>()?;
        Ok(v)
    }

    /// Одно сообщение (цитата ответа).
    pub fn message(&self, peer_id: i64, id: i32) -> Result<Option<Message>, StoreError> {
        Ok(self
            .conn()
            .query_row(&format!("SELECT {MSG_COLS} FROM messages WHERE peer_id = ?1 AND id = ?2"), params![peer_id, id],
                |r| message_from_row(r, 0))
            .optional()?)
    }

    /// Собеседники для диалога выбора (Дз-16): поиск по имени и username.
    pub fn search_peers(&self, query: &str, limit: usize) -> Result<Vec<Peer>, StoreError> {
        let q = format!("%{}%", query.to_lowercase());
        let mut st = self.conn().prepare_cached(
            "SELECT p.id, p.name, p.username, p.access_hash, p.photo_id, p.photo_dc, p.photo_ref, p.flags
             FROM peers p LEFT JOIN dialogs d ON d.peer_id = p.id
             WHERE p.name != '' AND (lower(p.name) LIKE ?1 OR lower(p.username) LIKE ?1)
             ORDER BY d.top_date IS NULL, d.top_date DESC LIMIT ?2",
        )?;
        let v = st.query_map(params![q, limit as i64], |r| peer_from_row(r, 0))?.collect::<Result<_, _>>()?;
        Ok(v)
    }

    /// Сохранить собеседников без сообщений (контакты, поиск).
    pub fn save_peers(&mut self, peers: &[Peer]) -> Result<(), StoreError> {
        let tx = self.conn_mut().transaction()?;
        for p in peers {
            upsert_peer(&tx, p)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// pts каналов для StartRequest.
    pub fn channel_states(&self) -> Result<Vec<(i64, i32)>, StoreError> {
        let mut st = self.conn().prepare_cached("SELECT channel_id, pts FROM update_state WHERE channel_id != 0")?;
        let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        Ok(v)
    }

    /// Путь к скачанному аватару текущего фото собеседника.
    pub fn set_avatar_path(&self, peer_id: i64, path: &str) -> Result<(), StoreError> {
        self.conn().execute("UPDATE peers SET photo_ref = ?2 WHERE id = ?1", params![peer_id, path])?;
        Ok(())
    }

    /// Локально отметить прочитанным до `max_id` (сервер подтвердит обновлением
    /// со своим счётчиком): непрочитанных меньше на прочитанные входящие.
    pub fn mark_read_local(&self, peer_id: i64, max_id: i32) -> Result<(), StoreError> {
        self.conn().execute(MARK_READ_SQL, params![peer_id, max_id])?;
        Ok(())
    }

    /// История очищена (dialog = false) или чат удалён либо покинут: из базы.
    pub fn drop_chat(&mut self, peer_id: i64, dialog: bool) -> Result<(), StoreError> {
        let tx = self.conn_mut().transaction()?;
        tx.execute("DELETE FROM messages WHERE peer_id = ?1 AND id > 0", params![peer_id])?;
        tx.execute("DELETE FROM history_ranges WHERE peer_id = ?1", params![peer_id])?;
        if dialog {
            tx.execute("DELETE FROM dialogs WHERE peer_id = ?1", params![peer_id])?;
        } else {
            tx.execute("UPDATE dialogs SET unread = 0, unread_mark = 0 WHERE peer_id = ?1", params![peer_id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Папки (порядок — как на сервере).
    pub fn save_folders(&mut self, folders: &[Folder]) -> Result<(), StoreError> {
        let tx = self.conn_mut().transaction()?;
        tx.execute("DELETE FROM folders", [])?;
        let ids = |v: &[i64]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
        for (pos, f) in folders.iter().enumerate() {
            let flags = [f.chatlist, f.contacts, f.non_contacts, f.groups, f.broadcasts, f.bots, f.exclude_muted,
                f.exclude_read, f.exclude_archived]
                .iter()
                .enumerate()
                .fold(0i64, |acc, (i, b)| acc | (i64::from(*b) << i));
            tx.execute(
                "INSERT INTO folders (id, pos, title, emoticon, flags, pinned, include, exclude)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![f.id, pos as i64, f.title, f.emoticon, flags, ids(&f.pinned), ids(&f.include), ids(&f.exclude)],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn folders(&self) -> Result<Vec<Folder>, StoreError> {
        let mut st = self.conn().prepare_cached(
            "SELECT id, title, emoticon, flags, pinned, include, exclude FROM folders ORDER BY pos",
        )?;
        let ids = |s: String| -> Vec<i64> { s.split(',').filter_map(|x| x.parse().ok()).collect() };
        let v = st
            .query_map([], |r| {
                let flags: i64 = r.get(3)?;
                let bit = |i: u32| flags & (1 << i) != 0;
                Ok(Folder {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    emoticon: r.get(2)?,
                    chatlist: bit(0),
                    contacts: bit(1),
                    non_contacts: bit(2),
                    groups: bit(3),
                    broadcasts: bit(4),
                    bots: bit(5),
                    exclude_muted: bit(6),
                    exclude_read: bit(7),
                    exclude_archived: bit(8),
                    pinned: ids(r.get(4)?),
                    include: ids(r.get(5)?),
                    exclude: ids(r.get(6)?),
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    /// Поиск по сообщениям в базе (FTS5): слова запроса — префиксы, все обязательны;
    /// новые первыми. Очередь отправки (id < 0) не ищется.
    pub fn search_local(&self, query: &str, limit: usize) -> Result<Vec<Message>, StoreError> {
        let q = fts_query(query);
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let mut st = self.conn().prepare_cached(&format!(
            "SELECT {} FROM messages_fts f
             JOIN msg_keys k ON k.rid = f.rowid
             JOIN messages m ON m.peer_id = k.peer_id AND m.id = k.id
             WHERE messages_fts MATCH ?1 AND k.id > 0
             ORDER BY m.date DESC LIMIT ?2",
            MSG_COLS.split(", ").map(|c| format!("m.{c}")).collect::<Vec<_>>().join(", ")
        ))?;
        let v = st.query_map(params![q, limit as i64], |r| message_from_row(r, 0))?.collect::<Result<_, _>>()?;
        Ok(v)
    }

    /// Самое старое сообщение истории в базе (для подгрузки вверх).
    pub fn oldest_message(&self, peer_id: i64) -> Result<Option<i32>, StoreError> {
        Ok(self.conn().query_row("SELECT MIN(id) FROM messages WHERE peer_id = ?1", params![peer_id], |r| r.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(id: i64, name: &str) -> Peer {
        Peer { id, kind: PeerKind::of(id), name: name.into(), access_hash: id * 10, ..Default::default() }
    }

    fn msg(peer_id: i64, id: i32, date: i64, text: &str, out: bool) -> Message {
        Message { peer_id, id, date, from_id: if out { 1 } else { peer_id }, out, text: text.into(), ..Default::default() }
    }

    fn batch(updates: Vec<DomainUpdate>, pts: i32) -> UpdatesBatch {
        UpdatesBatch { updates, peers: vec![], state: Some(UpdateState { pts, qts: 0, seq: 0, date: 1 }), channel_states: vec![] }
    }

    #[test]
    fn dialogs_order_and_last_message() {
        let mut s = Store::in_memory().unwrap();
        s.save_dialogs(
            &[
                Dialog { peer_id: 10, top_message: 2, unread_count: 1, ..Default::default() },
                Dialog { peer_id: -20, top_message: 5, ..Default::default() },
                Dialog { peer_id: 30, top_message: 1, pinned: true, ..Default::default() },
            ],
            &[msg(10, 2, 200, "новее", false), msg(-20, 5, 100, "старее", false), msg(30, 1, 50, "закреп", true)],
            &[peer(10, "Анна"), peer(-20, "Группа"), peer(30, "Закреплённый")],
        )
        .unwrap();
        let rows = s.dialog_rows(0, 100).unwrap();
        let names: Vec<_> = rows.iter().map(|r| r.peer.name.as_str()).collect();
        assert_eq!(names, ["Закреплённый", "Анна", "Группа"]);
        assert_eq!(rows[1].last.as_ref().unwrap().text, "новее");
        assert_eq!(rows[1].dialog.unread_count, 1);
        assert_eq!(rows[2].peer.kind, PeerKind::Chat);
        assert_eq!(rows[1].peer.access_hash, 100);
    }

    #[test]
    fn batch_and_state_in_one_transaction() {
        let mut s = Store::in_memory().unwrap();
        s.save_dialogs(&[Dialog { peer_id: 10, top_message: 1, ..Default::default() }], &[msg(10, 1, 100, "a", false)],
            &[peer(10, "Анна")])
            .unwrap();
        // 20 входящих подряд и один дубль.
        let mut ups: Vec<_> = (2..=21).map(|i| DomainUpdate::NewMessage(msg(10, i, 100 + i64::from(i), "x", false))).collect();
        ups.push(DomainUpdate::NewMessage(msg(10, 5, 105, "x", false)));
        let touched = s.apply_batch(&batch(ups, 21)).unwrap();
        assert_eq!(touched, vec![10]);
        assert_eq!(s.history(10, None, 100).unwrap().len(), 21, "дубль не создал вторую запись");
        let row = &s.dialog_rows(0, 10).unwrap()[0];
        assert_eq!((row.dialog.top_message, row.dialog.unread_count, row.top_date), (21, 20, 121));
        assert_eq!(s.update_state().unwrap().unwrap().pts, 21);

        // Прочитано на другом устройстве, правка и удаление.
        s.apply_batch(&batch(vec![DomainUpdate::ReadInbox { peer_id: 10, max_id: 21, still_unread: 0 }], 22)).unwrap();
        s.apply_batch(&batch(vec![DomainUpdate::EditMessage(Message { edit_date: 9, ..msg(10, 3, 103, "правка", false) })], 23))
            .unwrap();
        s.apply_batch(&batch(vec![DomainUpdate::DeleteMessages { peer_id: 0, ids: vec![4, 6] }], 24)).unwrap();
        let mut cb = batch(vec![], 25);
        cb.channel_states = vec![(-1_000_000_000_007, 40)];
        s.apply_batch(&cb).unwrap();
        assert_eq!(s.channel_states().unwrap(), vec![(-1_000_000_000_007, 40)]);
        assert_eq!(s.update_state().unwrap().unwrap().pts, 25, "строка канала не затирает общее состояние");
        let h = s.history(10, None, 100).unwrap();
        assert_eq!(h.len(), 19);
        assert_eq!(h.iter().find(|m| m.id == 3).unwrap().text, "правка");
        assert_eq!(s.dialog_rows(0, 10).unwrap()[0].dialog.unread_count, 0);
        assert_eq!(s.update_state().unwrap().unwrap().pts, 25);
    }

    #[test]
    fn failed_batch_leaves_state_untouched() {
        let mut s = Store::in_memory().unwrap();
        s.apply_batch(&batch(vec![], 5)).unwrap();
        // Искусственный сбой посреди пачки (как сбой диска): откат всей пачки вместе с состоянием.
        s.conn()
            .execute_batch(
                "CREATE TRIGGER fail BEFORE INSERT ON messages WHEN NEW.text = 'сбой'
                 BEGIN SELECT RAISE(ABORT, 'сбой'); END;",
            )
            .unwrap();
        let bad = batch(
            vec![DomainUpdate::NewMessage(msg(999, 1, 1, "до сбоя", false)), DomainUpdate::NewMessage(msg(999, 2, 2, "сбой", false))],
            6,
        );
        assert!(s.apply_batch(&bad).is_err());
        assert_eq!(s.update_state().unwrap().unwrap().pts, 5, "состояние сдвинулось без событий");
        assert!(s.history(999, None, 10).unwrap().is_empty(), "часть пачки осталась");
        // Незнакомый собеседник: заготовка строки, пачка применяется.
        s.apply_batch(&batch(vec![DomainUpdate::NewMessage(msg(999, 3, 3, "от незнакомца", false))], 7)).unwrap();
        assert_eq!(s.dialog_rows(0, 10).unwrap()[0].peer.name, "");
    }

    #[test]
    fn history_window_and_min_peers() {
        let mut s = Store::in_memory().unwrap();
        s.save_history(&(1..=120).map(|i| msg(10, i, i64::from(i), "m", i % 2 == 0)).collect::<Vec<_>>(),
            &[peer(10, "Анна")], None)
            .unwrap();
        let last = s.history(10, None, 50).unwrap();
        assert_eq!((last.first().unwrap().id, last.last().unwrap().id), (71, 120));
        let older = s.history(10, Some(71), 50).unwrap();
        assert_eq!((older.first().unwrap().id, older.last().unwrap().id), (21, 70));
        assert_eq!(s.oldest_message(10).unwrap(), Some(1));
        // «min»-сведения не затирают access_hash и имя пустым.
        let mut b = UpdatesBatch::default();
        b.peers.push(Peer { id: 10, min: true, access_hash: 7, ..Default::default() });
        s.apply_batch(&b).unwrap();
        let p = s.peer(10).unwrap().unwrap();
        assert_eq!((p.access_hash, p.name.as_str()), (100, "Анна"));
        s.set_avatar_path(10, "/tmp/a.jpg").unwrap();
        // Частично прочитано: 1..=120, входящие — нечётные; сервер сказал 10 непрочитанных.
        s.conn().execute("UPDATE dialogs SET unread = 10, read_inbox_max = 100 WHERE peer_id = 10", []).unwrap();
        s.mark_read_local(10, 110).unwrap();
        let unread = |s: &Store| s.dialog_rows(0, 10).unwrap()[0].dialog.unread_count;
        assert_eq!(unread(&s), 5, "прочитаны входящие 101..=109");
        // Своё «прочитано» пришло обновлением без остатка (−1): считается по базе,
        // уже вычтенное повторно не вычитается.
        s.apply_batch(&UpdatesBatch { updates: vec![DomainUpdate::ReadInbox { peer_id: 10, max_id: 110, still_unread: -1 }],
            ..Default::default() }).unwrap();
        assert_eq!(unread(&s), 5, "−1 не обнуляет счётчик");
        s.mark_read_local(10, 120).unwrap();
        assert_eq!(unread(&s), 0, "до верха — всё");
    }

    #[test]
    fn local_search_prefix_words_and_speed() {
        let mut s = Store::in_memory().unwrap();
        let words = ["релиз", "сборка", "проверка", "Пятница", "отчёт", "встреча", "ёлка", "code", "Build", "test"];
        let msgs: Vec<Message> = (1..=10_000)
            .map(|i: i32| {
                let t = format!("{} {} номер {i}", words[i as usize % 10], words[(i as usize / 10) % 10]);
                msg(10 + i64::from(i % 50), i, i64::from(i), &t, false)
            })
            .collect();
        let peers: Vec<Peer> = (10..60).map(|i| peer(i, "p")).collect();
        s.save_history(&msgs, &peers, None).unwrap();
        let t0 = std::time::Instant::now();
        let r = s.search_local("рел", 100).unwrap();
        let first = t0.elapsed();
        assert_eq!(r.len(), 100);
        assert!(r.windows(2).all(|w| w[0].date >= w[1].date), "новые первыми");
        assert!(r.iter().all(|m| m.text.contains("релиз")), "префикс слова");
        // Несколько слов — все обязательны; регистр и ё не мешают.
        let r = s.search_local("пятница ЕЛКА", 100).unwrap();
        assert!(!r.is_empty() && r.iter().all(|m| m.text.contains("Пятница") && m.text.contains("ёлка")));
        assert!(s.search_local("\"*: OR", 10).unwrap().is_empty(), "служебные знаки не ломают запрос");
        // Порог (План, В6): поиск по 10 000 сообщений — меньше 50 мс в отладочной сборке.
        let t0 = std::time::Instant::now();
        for q in ["сборка", "code test", "номер 99", "проверка отчёт"] {
            s.search_local(q, 100).unwrap();
        }
        let avg = t0.elapsed() / 4;
        eprintln!("поиск по 10 000 сообщений: первый {first:?}, средний {avg:?}");
        assert!(avg < std::time::Duration::from_millis(50), "медленно: {avg:?}");
    }

    #[test]
    fn folders_and_dialog_settings() {
        let mut s = Store::in_memory().unwrap();
        let f = Folder { id: 3, title: "Работа".into(), emoticon: "💼".into(), groups: true, exclude_read: true,
            include: vec![10, -1_000_000_000_007], exclude: vec![20], pinned: vec![10], ..Default::default() };
        s.save_folders(&[f.clone(), Folder { id: 4, chatlist: true, title: "Ссылка".into(), ..Default::default() }]).unwrap();
        let back = s.folders().unwrap();
        assert_eq!(back[0], f);
        assert!(back[1].chatlist);
        s.save_dialogs(&[Dialog { peer_id: 10, top_message: 1, muted: true, ..Default::default() }],
            &[msg(10, 1, 100, "a", false)], &[peer(10, "Анна")]).unwrap();
        let row = |s: &Store| s.dialog_rows(-1, 10).unwrap()[0].dialog.clone();
        assert!(row(&s).muted);
        s.apply_batch(&UpdatesBatch { updates: vec![
            DomainUpdate::PeerSettings { peer_id: 10, muted: Some(false), pinned: Some(true), unread_mark: None },
            DomainUpdate::PeerFolder { peer_id: 10, folder: 1 },
        ], ..Default::default() }).unwrap();
        let d = row(&s);
        assert!(!d.muted && d.folder == 1 && !d.pinned, "в архиве закрепление снимается");
        assert!(s.dialog_rows(0, 10).unwrap().is_empty(), "архив не в основном списке");
    }
}
