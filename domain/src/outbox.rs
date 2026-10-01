//! Очередь отправки (решение О-9, срок жизни О-12; Архитектура/nuncio -
//! Приложение (Rust и Slint).md, раздел 4). Сообщение сразу пишется в базу и
//! показывается в чате с локальным id `-local_id`; `random_id` выдаётся один раз
//! и живёт в базе, поэтому повтор после сбоя или перезапуска не создаёт дубль.
//! В каждом чате отправляется одно сообщение за раз — порядок сохраняется.

use rusqlite::{params, OptionalExtension};

use crate::model::{decode_entities, encode_entities, Entity, Media, MediaKind, Message, PeerKind};
use crate::store::{Store, StoreError};

/// Срок жизни сообщения в очереди по умолчанию (О-12): сутки.
pub const TTL_SECS: i64 = 24 * 3600;

pub const QUEUED: i32 = 0;
pub const SENDING: i32 = 1;
pub const FAILED: i32 = 2;

/// Готовое к отправке сообщение.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxItem {
    pub local_id: i64,
    pub peer_id: i64,
    pub access_hash: i64,
    pub random_id: i64,
    pub text: String,
    pub reply_to: i32,
    pub entities: Vec<Entity>,
    /// Вложение (В7); у альбома — общий `group` у нескольких строк подряд.
    pub media: Option<OutMedia>,
    pub group: i64,
}

/// Файл в очереди отправки.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct OutMedia {
    pub path: String,
    pub name: String,
    pub mime: String,
    pub as_photo: bool,
    pub size: i64,
    pub width: i32,
    pub height: i32,
}

impl OutMedia {
    /// Описание своего неотправленного для ленты: файл пока на диске.
    pub fn local_media(&self) -> Media {
        Media {
            kind: if self.as_photo { MediaKind::Photo } else { MediaKind::Document },
            name: self.name.clone(),
            mime: self.mime.clone(),
            size: self.size,
            width: self.width,
            height: self.height,
            local_path: self.path.clone(),
            ..Default::default()
        }
    }
}

fn msg_state(outbox_state: i32) -> i32 {
    match outbox_state {
        QUEUED => 1,
        SENDING => 2,
        _ => 3,
    }
}

/// Что поставить в очередь.
#[derive(Debug, Clone, Copy, Default)]
pub struct Outgoing<'a> {
    pub peer_id: i64,
    pub text: &'a str,
    pub entities: &'a [Entity],
    pub reply_to: i32,
    pub random_id: i64,
    pub media: Option<&'a OutMedia>,
    /// Альбом: одинаковый ненулевой ключ у файлов, уходящих одним запросом.
    pub group: i64,
}

impl Store {
    /// Поставить сообщение в очередь: строка очереди и локальное сообщение в чате.
    pub fn outbox_enqueue(&mut self, o: &Outgoing, self_id: i64, now: i64) -> Result<i64, StoreError> {
        let Outgoing { peer_id, text, entities, reply_to, random_id, media, group } = *o;
        let json = |v: Option<String>| v.unwrap_or_default();
        let media_json = json(media.and_then(|m| serde_json::to_string(m).ok()));
        let shown = json(media.and_then(|m| serde_json::to_string(&m.local_media()).ok()));
        let tx = self.conn_mut().transaction()?;
        tx.execute(
            "INSERT INTO outbox (peer_id, random_id, content, queued_at, reply_to, state, next_try, entities, media, group_key)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, 0, ?6, ?7, ?8)",
            params![peer_id, random_id, text.as_bytes(), now, reply_to, encode_entities(entities), media_json, group],
        )?;
        let local = tx.last_insert_rowid();
        let kind = match PeerKind::of(peer_id) {
            PeerKind::User => 0,
            PeerKind::Chat => 1,
            PeerKind::Channel => 2,
        };
        tx.execute("INSERT OR IGNORE INTO peers (id, kind) VALUES (?1, ?2)", params![peer_id, kind])?;
        tx.execute(
            "INSERT INTO messages (peer_id, id, date, sender_id, text, reply_to, out, random_id, send_state, entities,
                                   media_json, grouped_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, 1, ?8, ?9, ?10)",
            params![peer_id, -local, now, self_id, text, reply_to, random_id, encode_entities(entities), shown, group],
        )?;
        tx.commit()?;
        Ok(local)
    }

    /// Готовые к отправке: по одному (самому старому) на чат, если в этом чате
    /// ничего не отправляется и время попытки наступило. Альбом — целиком:
    /// за первым файлом идут остальные ждущие с тем же ключом.
    pub fn outbox_ready(&self, now: i64) -> Result<Vec<OutboxItem>, StoreError> {
        let first = self.outbox_rows(
            "WHERE o.state = 0 AND o.next_try <= ?1
               AND o.local_id = (SELECT MIN(local_id) FROM outbox x WHERE x.peer_id = o.peer_id AND x.state != 2)
               AND NOT EXISTS (SELECT 1 FROM outbox y WHERE y.peer_id = o.peer_id AND y.state = 1)
             ORDER BY o.local_id",
            params![now],
        )?;
        let mut out = Vec::with_capacity(first.len());
        for it in first {
            let (peer, group, local) = (it.peer_id, it.group, it.local_id);
            out.push(it);
            if group != 0 {
                out.extend(self.outbox_rows(
                    "WHERE o.peer_id = ?1 AND o.group_key = ?2 AND o.state = 0 AND o.local_id > ?3 ORDER BY o.local_id",
                    params![peer, group, local],
                )?);
            }
        }
        Ok(out)
    }

    fn outbox_rows(&self, filter: &str, args: impl rusqlite::Params) -> Result<Vec<OutboxItem>, StoreError> {
        let mut st = self.conn().prepare_cached(&format!(
            "SELECT o.local_id, o.peer_id, COALESCE(p.access_hash, 0), o.random_id, o.content, o.reply_to, o.entities,
                    o.media, o.group_key
             FROM outbox o LEFT JOIN peers p ON p.id = o.peer_id {filter}"
        ))?;
        let v = st
            .query_map(args, |r| {
                let content: Vec<u8> = r.get(4)?;
                let media: String = r.get(7)?;
                Ok(OutboxItem {
                    local_id: r.get(0)?,
                    peer_id: r.get(1)?,
                    access_hash: r.get(2)?,
                    random_id: r.get(3)?,
                    text: String::from_utf8_lossy(&content).into_owned(),
                    reply_to: r.get(5)?,
                    entities: decode_entities(&r.get::<_, String>(6)?),
                    media: serde_json::from_str(&media).ok(),
                    group: r.get(8)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    fn outbox_set(&self, local_id: i64, state: i32, error: &str, next_try: i64, attempt: bool) -> Result<(), StoreError> {
        let c = self.conn();
        c.execute(
            "UPDATE outbox SET state = ?2, error = ?3, next_try = ?4, attempts = attempts + ?5 WHERE local_id = ?1",
            params![local_id, state, error, next_try, i32::from(attempt)],
        )?;
        c.execute("UPDATE messages SET send_state = ?2 WHERE id = ?1", params![-local_id, msg_state(state)])?;
        Ok(())
    }

    pub fn outbox_sending(&self, local_id: i64) -> Result<(), StoreError> {
        self.outbox_set(local_id, SENDING, "", 0, true)
    }

    /// Сетевая ошибка или FLOOD_WAIT: снова в очередь, попытка не раньше `next_try`.
    pub fn outbox_requeue(&self, local_id: i64, next_try: i64) -> Result<(), StoreError> {
        self.outbox_set(local_id, QUEUED, "", next_try, false)
    }

    /// Постоянная ошибка: «не отправлено» с причиной (Чт-6).
    pub fn outbox_fail(&self, local_id: i64, error: &str) -> Result<(), StoreError> {
        self.outbox_set(local_id, FAILED, error, 0, false)
    }

    /// «Повторить» (Чт-6): снова в очередь со свежим сроком жизни.
    pub fn outbox_retry(&self, local_id: i64, now: i64) -> Result<(), StoreError> {
        self.conn().execute("UPDATE outbox SET queued_at = ?2 WHERE local_id = ?1", params![local_id, now])?;
        self.outbox_set(local_id, QUEUED, "", 0, false)
    }

    /// «Удалить» неотправленное: из очереди и из чата.
    pub fn outbox_delete(&mut self, local_id: i64) -> Result<(), StoreError> {
        let tx = self.conn_mut().transaction()?;
        tx.execute("DELETE FROM outbox WHERE local_id = ?1", params![local_id])?;
        tx.execute("DELETE FROM messages WHERE id = ?1", params![-local_id])?;
        tx.commit()?;
        Ok(())
    }

    /// Отправлено: локальная строка заменяется настоящим сообщением одной транзакцией.
    pub fn outbox_done(&mut self, local_id: i64, sent: Option<&Message>) -> Result<(), StoreError> {
        let tx = self.conn_mut().transaction()?;
        tx.execute("DELETE FROM outbox WHERE local_id = ?1", params![local_id])?;
        tx.execute("DELETE FROM messages WHERE id = ?1", params![-local_id])?;
        if let Some(m) = sent.filter(|m| m.id > 0) {
            tx.execute(
                "INSERT INTO messages (peer_id, id, date, sender_id, text, reply_to, out, random_id, send_state)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, 0)
                 ON CONFLICT(peer_id, id) DO UPDATE SET random_id = excluded.random_id",
                params![m.peer_id, m.id, m.date, m.from_id, m.text, m.reply_to, m.random_id],
            )?;
            tx.execute(
                "INSERT INTO dialogs (peer_id, top_message_id, top_date) VALUES (?1, ?2, ?3)
                 ON CONFLICT(peer_id) DO UPDATE SET
                   top_date = CASE WHEN ?2 > dialogs.top_message_id THEN ?3 ELSE dialogs.top_date END,
                   top_message_id = MAX(dialogs.top_message_id, ?2)",
                params![m.peer_id, m.id, m.date],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Срок жизни истёк (О-12): «ошибка: срок истёк», сообщение остаётся в чате.
    pub fn outbox_expire(&self, now: i64, ttl: i64) -> Result<Vec<i64>, StoreError> {
        let mut st = self
            .conn()
            .prepare_cached("SELECT local_id, peer_id FROM outbox WHERE state = 0 AND queued_at + ?2 <= ?1")?;
        let rows: Vec<(i64, i64)> =
            st.query_map(params![now, ttl], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        for (local, _) in &rows {
            self.outbox_fail(*local, "expired")?;
        }
        Ok(rows.into_iter().map(|(_, p)| p).collect())
    }

    /// После перезапуска «отправляется» возвращается в очередь: тот же random_id,
    /// сервер не создаст дубль, если первая попытка успела дойти.
    pub fn outbox_recover(&self) -> Result<usize, StoreError> {
        let n = self.conn().execute("UPDATE outbox SET state = 0 WHERE state = 1", [])?;
        self.conn().execute(
            "UPDATE messages SET send_state = 1 WHERE id < 0 AND -id IN (SELECT local_id FROM outbox WHERE state = 0)",
            [],
        )?;
        Ok(n)
    }

    /// Локальный id по random_id (для сопоставления ответа сервера).
    pub fn outbox_by_random(&self, random_id: i64) -> Result<Option<i64>, StoreError> {
        Ok(self
            .conn()
            .query_row("SELECT local_id FROM outbox WHERE random_id = ?1", params![random_id], |r| r.get(0))
            .optional()?)
    }

    pub fn outbox_len(&self) -> Result<i64, StoreError> {
        Ok(self.conn().query_row("SELECT count(*) FROM outbox", [], |r| r.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Dialog, Peer};

    fn store() -> Store {
        let mut s = Store::in_memory().unwrap();
        s.save_dialogs(
            &[Dialog { peer_id: 10, top_message: 5, ..Default::default() }],
            &[Message { peer_id: 10, id: 5, date: 100, text: "было".into(), ..Default::default() }],
            &[Peer { id: 10, name: "Анна".into(), access_hash: 77, ..Default::default() }],
        )
        .unwrap();
        s
    }

    #[test]
    fn queue_order_one_per_chat_and_done() {
        let mut s = store();
        let a = s.outbox_enqueue(&Outgoing { peer_id: 10, text: "первое", entities: &[], reply_to: 0, random_id: 111, ..Default::default() }, 1, 200).unwrap();
        let b = s.outbox_enqueue(&Outgoing { peer_id: 10, text: "второе", entities: &[], reply_to: 5, random_id: 222, ..Default::default() }, 1, 201).unwrap();
        let c = s.outbox_enqueue(&Outgoing { peer_id: 20, text: "другой чат", entities: &[], reply_to: 0, random_id: 333, ..Default::default() }, 1, 202).unwrap();
        // В истории: отправленное, потом очередь в порядке постановки.
        let h = s.history(10, None, 50).unwrap();
        let texts: Vec<_> = h.iter().map(|m| (m.text.as_str(), m.send_state)).collect();
        assert_eq!(texts, [("было", 0), ("первое", 1), ("второе", 1)]);
        // По одному на чат, самое старое.
        let ready = s.outbox_ready(300).unwrap();
        assert_eq!(ready.iter().map(|i| i.local_id).collect::<Vec<_>>(), [a, c]);
        assert_eq!(ready[0].access_hash, 77);
        s.outbox_sending(a).unwrap();
        assert_eq!(s.outbox_ready(300).unwrap().iter().map(|i| i.local_id).collect::<Vec<_>>(), [c],
            "пока первое отправляется, второе ждёт");
        let sent = Message { peer_id: 10, id: 6, date: 210, from_id: 1, out: true, text: "первое".into(), random_id: 111,
            ..Default::default() };
        s.outbox_done(a, Some(&sent)).unwrap();
        assert_eq!(s.outbox_ready(300).unwrap()[0].local_id, b, "очередь двинулась");
        let h = s.history(10, None, 50).unwrap();
        assert_eq!(h.iter().map(|m| m.id).collect::<Vec<_>>(), [5, 6, -(b as i32)]);
        assert_eq!(s.dialog_rows(0, 10).unwrap()[0].dialog.top_message, 6);
    }

    #[test]
    fn requeue_fail_retry_delete_expire_recover() {
        let mut s = store();
        let a = s.outbox_enqueue(&Outgoing { peer_id: 10, text: "x", entities: &[], reply_to: 0, random_id: 1, ..Default::default() }, 1, 100).unwrap();
        s.outbox_sending(a).unwrap();
        s.outbox_requeue(a, 500).unwrap();
        assert!(s.outbox_ready(400).unwrap().is_empty(), "до следующей попытки");
        assert_eq!(s.outbox_ready(500).unwrap().len(), 1);
        s.outbox_fail(a, "CHAT_WRITE_FORBIDDEN").unwrap();
        let h = s.history(10, None, 50).unwrap();
        assert_eq!((h[1].send_state, h[1].send_error.as_str()), (3, "CHAT_WRITE_FORBIDDEN"));
        assert!(s.outbox_ready(1000).unwrap().is_empty());
        s.outbox_retry(a, 1000).unwrap();
        assert_eq!(s.outbox_ready(1000).unwrap().len(), 1);
        // Срок жизни.
        assert_eq!(s.outbox_expire(1000 + TTL_SECS, TTL_SECS).unwrap(), vec![10]);
        assert_eq!(s.history(10, None, 50).unwrap()[1].send_error, "expired");
        s.outbox_delete(a).unwrap();
        assert_eq!(s.history(10, None, 50).unwrap().len(), 1);
        // Восстановление после сбоя: «отправляется» → «в очереди», random_id прежний.
        let b = s.outbox_enqueue(&Outgoing { peer_id: 10, text: "y", entities: &[], reply_to: 0, random_id: 42, ..Default::default() }, 1, 2000).unwrap();
        s.outbox_sending(b).unwrap();
        assert_eq!(s.outbox_recover().unwrap(), 1);
        let r = s.outbox_ready(2000).unwrap();
        assert_eq!((r[0].local_id, r[0].random_id), (b, 42));
        assert_eq!(s.outbox_by_random(42).unwrap(), Some(b));
        assert_eq!(s.outbox_len().unwrap(), 1);
    }

    #[test]
    fn album_goes_out_whole_and_shows_local_file() {
        let mut s = store();
        let f = |n: &str| OutMedia { path: format!("/tmp/{n}"), name: n.into(), mime: "image/jpeg".into(), as_photo: true,
            size: 10, width: 800, height: 600 };
        let (a, b) = (f("a.jpg"), f("b.jpg"));
        let x = s.outbox_enqueue(&Outgoing { peer_id: 10, text: "подпись", random_id: 1, media: Some(&a), group: 5,
            ..Default::default() }, 1, 100).unwrap();
        let y = s.outbox_enqueue(&Outgoing { peer_id: 10, random_id: 2, media: Some(&b), group: 5, ..Default::default() }, 1, 100)
            .unwrap();
        s.outbox_enqueue(&Outgoing { peer_id: 10, text: "потом", random_id: 3, ..Default::default() }, 1, 100).unwrap();
        let r = s.outbox_ready(100).unwrap();
        assert_eq!(r.iter().map(|i| i.local_id).collect::<Vec<_>>(), [x, y], "альбом целиком, текст после");
        assert_eq!(r[1].media.as_ref(), Some(&b));
        let h = s.history(10, None, 50).unwrap();
        let local = h.iter().find(|m| m.id == -(x as i32)).unwrap();
        let mi = local.media_info.as_deref().unwrap();
        assert_eq!((mi.kind, mi.local_path.as_str(), local.grouped_id), (MediaKind::Photo, "/tmp/a.jpg", 5));
        s.outbox_sending(x).unwrap();
        s.outbox_sending(y).unwrap();
        assert!(s.outbox_ready(100).unwrap().is_empty(), "пока альбом уходит, чат ждёт");
    }

    #[test]
    fn duplicate_random_id_is_rejected() {
        let mut s = store();
        s.outbox_enqueue(&Outgoing { peer_id: 10, text: "x", entities: &[], reply_to: 0, random_id: 7, ..Default::default() }, 1, 1).unwrap();
        assert!(s.outbox_enqueue(&Outgoing { peer_id: 10, text: "x", entities: &[], reply_to: 0, random_id: 7, ..Default::default() }, 1, 1).is_err(), "random_id уникален");
    }
}
