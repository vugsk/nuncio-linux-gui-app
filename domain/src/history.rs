//! Окно истории и известные непрерывные участки (Архитектура/nuncio -
//! Приложение (Rust и Slint).md: «границы окна и дырки отслеживает store»).
//!
//! В таблицу `messages` сообщения попадают из разных мест: страницы истории,
//! обновления, цитаты ответов, закреплённые, верх списка диалогов. Подряд
//! показывать можно только то, про что известно, что между сообщениями ничего
//! не пропущено. Такие участки `[lo, hi]` хранятся в `history_ranges`: `lo = 0`
//! значит «от начала чата», `hi = LIVE` — «до настоящего», дальше участок
//! продолжают обновления (их непрерывность гарантирует pts, при разрыве
//! приходит Reset и участок обрезается).

use rusqlite::{params, OptionalExtension, Transaction};

use crate::chatstore::{message_from_row, MSG_COLS};
use crate::model::{Message, CHANNEL_SHIFT};
use crate::store::{Store, StoreError};

/// Верх участка «до настоящего».
pub const LIVE: i32 = i32::MAX;

/// Непрерывный участок истории чата (включительно).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub peer_id: i64,
    pub lo: i32,
    pub hi: i32,
}

impl Span {
    /// Участок по запросу `getHistory(offset_id, add_offset, limit)` и ответу
    /// (`ids` — id полученных сообщений). Правила Telegram: без `add_offset` —
    /// `limit` сообщений старше `offset_id` (0 — самые новые); с
    /// `add_offset = -k` в ответ попадают и k сообщений c id не меньше `offset_id`.
    pub fn from_page(peer_id: i64, offset_id: i32, add_offset: i32, limit: i32, ids: &[i32]) -> Option<Span> {
        let min = ids.iter().copied().filter(|&i| i > 0).min();
        let max = ids.iter().copied().filter(|&i| i > 0).max();
        let newer_wanted = (-add_offset).clamp(0, limit);
        let older_wanted = limit - newer_wanted;
        let (older, newer) = if offset_id == 0 {
            (ids.len() as i32, 0)
        } else {
            let o = ids.iter().filter(|&&i| i > 0 && i < offset_id).count() as i32;
            (o, ids.len() as i32 - o)
        };
        // Низ: меньше, чем просили старше, — дошли до начала чата; старше id 1
        // сообщений не бывает.
        let lo = if (older_wanted > 0 && older < older_wanted) || offset_id == 1 {
            0
        } else {
            match (min, offset_id) {
                (Some(m), _) => m,
                (None, 0) => return None,
                (None, o) => o, // просили только новее и ничего не получили
            }
        };
        // Верх: без смещения — до самого нового или до offset_id; со смещением —
        // меньше, чем просили новее, значит дошли до настоящего.
        let hi = if offset_id == 0 {
            LIVE
        } else if newer_wanted == 0 {
            offset_id - 1
        } else if newer < newer_wanted {
            LIVE
        } else {
            max.unwrap_or(offset_id - 1)
        };
        (lo <= hi).then_some(Span { peer_id, lo, hi })
    }
}

/// Где держится окно истории.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Anchor {
    /// Последние сообщения (и очередь отправки в конце).
    #[default]
    Latest,
    /// Вокруг сообщения: `before` старше него и `after` начиная с него.
    At(i32),
}

/// Окно истории из базы и что о нём известно.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Window {
    pub messages: Vec<Message>,
    /// Якорь внутри известного участка (иначе окно пустое — спросить сервер).
    pub found: bool,
    /// Окно доходит до начала чата.
    pub start: bool,
    /// Окно доходит до настоящего (и показывает очередь отправки).
    pub end: bool,
    /// Старше нужного в базе нет, но участок не от начала: подгрузить с сервера.
    pub need_older: bool,
    pub need_newer: bool,
    /// Первое сообщение чата, если начало истории известно (для полосы прокрутки).
    pub first_id: Option<i32>,
}

/// Добавить участок, слив его с пересекающимися и соседними.
pub(crate) fn add_range(tx: &Transaction, s: Span) -> rusqlite::Result<()> {
    let (mut lo, mut hi) = (s.lo, s.hi);
    {
        let mut st = tx.prepare_cached(
            "DELETE FROM history_ranges WHERE peer_id = ?1 AND lo <= ?3 + 1 AND hi + 1 >= ?2 RETURNING lo, hi",
        )?;
        let old: Vec<(i32, i32)> =
            st.query_map(params![s.peer_id, lo, hi], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        for (a, b) in old {
            lo = lo.min(a);
            hi = hi.max(b);
        }
    }
    tx.execute("INSERT INTO history_ranges (peer_id, lo, hi) VALUES (?1, ?2, ?3)", params![s.peer_id, lo, hi])?;
    Ok(())
}

/// Участок «до настоящего» больше не продолжается обновлениями (разрыв):
/// обрезать его по последнему известному сообщению.
pub(crate) fn cut_live(tx: &Transaction, peer_id: i64) -> rusqlite::Result<()> {
    let lo: Option<i32> = tx
        .query_row("SELECT lo FROM history_ranges WHERE peer_id = ?1 AND hi = ?2", params![peer_id, LIVE], |r| r.get(0))
        .optional()?;
    let Some(lo) = lo else { return Ok(()) };
    let top: Option<i32> = tx.query_row(
        "SELECT MAX(id) FROM messages WHERE peer_id = ?1 AND id >= ?2 AND id > 0",
        params![peer_id, lo],
        |r| r.get(0),
    )?;
    match top {
        Some(t) => tx.execute("UPDATE history_ranges SET hi = ?3 WHERE peer_id = ?1 AND lo = ?2", params![peer_id, lo, t])?,
        None => tx.execute("DELETE FROM history_ranges WHERE peer_id = ?1 AND lo = ?2", params![peer_id, lo])?,
    };
    Ok(())
}

/// Общий Reset (разница слишком длинная): участки личных чатов и групп
/// больше не продолжаются. У каналов свой pts и свой ResetChannel.
pub(crate) fn cut_live_all(tx: &Transaction) -> rusqlite::Result<()> {
    let peers: Vec<i64> = {
        let mut st = tx.prepare_cached("SELECT peer_id FROM history_ranges WHERE hi = ?1 AND peer_id >= ?2")?;
        let v = st.query_map(params![LIVE, -CHANNEL_SHIFT], |r| r.get(0))?.collect::<Result<_, _>>()?;
        v
    };
    for p in peers {
        cut_live(tx, p)?;
    }
    Ok(())
}

/// Новое сообщение из обновлений: если у чата нет участка «до настоящего»,
/// оно его начинает.
pub(crate) fn note_new(tx: &Transaction, peer_id: i64, id: i32) -> rusqlite::Result<()> {
    let live: bool = tx
        .query_row("SELECT 1 FROM history_ranges WHERE peer_id = ?1 AND hi = ?2", params![peer_id, LIVE], |_| Ok(true))
        .optional()?
        .unwrap_or(false);
    if !live && id > 0 {
        add_range(tx, Span { peer_id, lo: id, hi: LIVE })?;
    }
    Ok(())
}

impl Store {
    fn select_messages(&self, sql_where: &str, order: &str, p: &[&dyn rusqlite::ToSql]) -> Result<Vec<Message>, StoreError> {
        let mut st = self.conn().prepare_cached(&format!(
            "SELECT {}, COALESCE(o.error, '') FROM messages m
             LEFT JOIN outbox o ON o.local_id = -m.id AND m.id < 0
             WHERE {sql_where} ORDER BY {order}",
            MSG_COLS.split(", ").map(|c| format!("m.{c}")).collect::<Vec<_>>().join(", ")
        ))?;
        let v = st
            .query_map(p, |r| {
                let mut m = message_from_row(r, 0)?;
                m.send_error = r.get(16)?;
                Ok(m)
            })?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    /// Участки чата по возрастанию (для проверок и отладки).
    pub fn history_ranges(&self, peer_id: i64) -> Result<Vec<(i32, i32)>, StoreError> {
        let mut st = self.conn().prepare_cached("SELECT lo, hi FROM history_ranges WHERE peer_id = ?1 ORDER BY lo")?;
        let v = st.query_map(params![peer_id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        Ok(v)
    }

    /// Окно истории вокруг якоря в пределах одного непрерывного участка.
    pub fn history_window(&self, peer_id: i64, anchor: Anchor, before: usize, after: usize) -> Result<Window, StoreError> {
        let conn = self.conn();
        let range: Option<(i32, i32)> = match anchor {
            Anchor::Latest => conn
                .query_row("SELECT lo, hi FROM history_ranges WHERE peer_id = ?1 AND hi = ?2", params![peer_id, LIVE],
                    |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?,
            Anchor::At(x) => conn
                .query_row(
                    "SELECT lo, hi FROM history_ranges WHERE peer_id = ?1 AND lo <= ?2 AND hi >= ?2",
                    params![peer_id, x],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?,
        };
        let first_id: Option<i32> = conn.query_row(
            "SELECT MIN(m.id) FROM history_ranges r JOIN messages m ON m.peer_id = r.peer_id AND m.id > 0 AND m.id <= r.hi
             WHERE r.peer_id = ?1 AND r.lo = 0",
            params![peer_id],
            |r| r.get(0),
        )?;
        let locals = || {
            self.select_messages("m.peer_id = ?1 AND m.id < 0", "m.id DESC", &[&peer_id])
        };
        let Some((lo, hi)) = range else {
            let latest = anchor == Anchor::Latest;
            return Ok(Window {
                messages: if latest { locals()? } else { Vec::new() },
                end: latest,
                first_id,
                ..Default::default()
            });
        };
        let (x, newer_limit) = match anchor {
            Anchor::Latest => (i64::from(hi) + 1, 0),
            Anchor::At(x) => (i64::from(x), after),
        };
        let lo1 = lo.max(1);
        let mut older = self.select_messages(
            "m.peer_id = ?1 AND m.id >= ?2 AND m.id < ?3",
            &format!("m.id DESC LIMIT {before}"),
            &[&peer_id, &lo1, &x],
        )?;
        older.reverse();
        let newer = if newer_limit > 0 {
            self.select_messages(
                "m.peer_id = ?1 AND m.id >= ?2 AND m.id <= ?3",
                &format!("m.id ASC LIMIT {newer_limit}"),
                &[&peer_id, &x, &hi],
            )?
        } else {
            Vec::new()
        };
        let older_short = older.len() < before;
        let newer_short = newer.len() < newer_limit || anchor == Anchor::Latest;
        let mut w = Window {
            found: true,
            start: older_short && lo == 0,
            need_older: older_short && lo != 0,
            end: newer_short && hi == LIVE,
            need_newer: newer_short && hi != LIVE,
            first_id,
            messages: older,
        };
        w.messages.extend(newer);
        if w.end {
            w.messages.extend(locals()?);
        }
        Ok(w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Dialog, DomainUpdate, UpdateState, UpdatesBatch};

    fn msg(peer_id: i64, id: i32) -> Message {
        Message { peer_id, id, date: i64::from(id), from_id: peer_id, text: format!("m{id}"), ..Default::default() }
    }

    fn ids(w: &Window) -> Vec<i32> {
        w.messages.iter().map(|m| m.id).collect()
    }

    fn page(s: &mut Store, peer: i64, offset: i32, add: i32, limit: i32, ids: &[i32]) {
        let msgs: Vec<Message> = ids.iter().map(|&i| msg(peer, i)).collect();
        s.save_history(&msgs, &[], Span::from_page(peer, offset, add, limit, ids)).unwrap();
    }

    #[test]
    fn span_rules() {
        // Последняя страница: [min, настоящее]; неполная — от начала.
        assert_eq!(Span::from_page(5, 0, 0, 3, &[10, 9, 8]), Some(Span { peer_id: 5, lo: 8, hi: LIVE }));
        assert_eq!(Span::from_page(5, 0, 0, 3, &[10, 9]), Some(Span { peer_id: 5, lo: 0, hi: LIVE }));
        assert_eq!(Span::from_page(5, 0, 0, 3, &[]), Some(Span { peer_id: 5, lo: 0, hi: LIVE }), "пустой чат");
        // Старше offset_id: до offset_id − 1.
        assert_eq!(Span::from_page(5, 8, 0, 2, &[7, 5]), Some(Span { peer_id: 5, lo: 5, hi: 7 }));
        assert_eq!(Span::from_page(5, 8, 0, 2, &[7]), Some(Span { peer_id: 5, lo: 0, hi: 7 }));
        // Новее: offset_id + add_offset = −limit.
        assert_eq!(Span::from_page(5, 11, -2, 2, &[12, 11]), Some(Span { peer_id: 5, lo: 11, hi: 12 }));
        assert_eq!(Span::from_page(5, 11, -2, 2, &[11]), Some(Span { peer_id: 5, lo: 11, hi: LIVE }));
        assert_eq!(Span::from_page(5, 11, -2, 2, &[]), Some(Span { peer_id: 5, lo: 11, hi: LIVE }));
        // Вокруг: по 2 с каждой стороны.
        assert_eq!(Span::from_page(5, 50, -2, 4, &[52, 50, 49, 47]), Some(Span { peer_id: 5, lo: 47, hi: 52 }));
        assert_eq!(Span::from_page(5, 50, -2, 4, &[50, 49, 47]), Some(Span { peer_id: 5, lo: 47, hi: LIVE }));
        assert_eq!(Span::from_page(5, 50, -2, 4, &[51, 50, 49]), Some(Span { peer_id: 5, lo: 0, hi: 51 }));
        // Самое старое сообщение: offset_id = 1, add_offset = −1.
        assert_eq!(Span::from_page(5, 1, -1, 1, &[3]), Some(Span { peer_id: 5, lo: 0, hi: 3 }));
    }

    #[test]
    fn windows_stay_inside_known_ranges() {
        let mut s = Store::in_memory().unwrap();
        // Последние 5 (100..=104) и отдельно цитата 20 — без участка.
        page(&mut s, 7, 0, 0, 5, &[104, 103, 102, 101, 100]);
        s.save_history(&[msg(7, 20)], &[], None).unwrap();
        let w = s.history_window(7, Anchor::Latest, 50, 0).unwrap();
        assert_eq!(ids(&w), vec![100, 101, 102, 103, 104], "цитата 20 вне участка не попадает в окно");
        assert!(w.found && w.end && w.need_older && !w.start);
        // Вокруг 20 — не найдено: надо спросить сервер.
        let w = s.history_window(7, Anchor::At(20), 10, 10).unwrap();
        assert!(!w.found && w.messages.is_empty());
        // Страница старше 100 до начала: участки сливаются.
        page(&mut s, 7, 100, 0, 50, &[99, 98, 20]);
        assert_eq!(s.history_ranges(7).unwrap(), vec![(0, LIVE)]);
        let w = s.history_window(7, Anchor::At(99), 5, 2).unwrap();
        assert_eq!(ids(&w), vec![20, 98, 99, 100]);
        assert!(w.start && !w.need_older && !w.end && !w.need_newer);
        assert_eq!(w.first_id, Some(20));
    }

    #[test]
    fn jump_far_creates_island_and_joins_later() {
        let mut s = Store::in_memory().unwrap();
        page(&mut s, 7, 0, 0, 3, &[1000, 999, 998]);
        // Переход к 500: страница вокруг.
        page(&mut s, 7, 500, -2, 4, &[502, 501, 500, 499, 498]);
        assert_eq!(s.history_ranges(7).unwrap(), vec![(498, 502), (998, LIVE)]);
        let w = s.history_window(7, Anchor::At(500), 10, 10).unwrap();
        assert_eq!(ids(&w), vec![498, 499, 500, 501, 502]);
        assert!(w.need_older && w.need_newer && !w.end);
        // Подгрузка вниз до настоящего сливает остров с последним участком.
        page(&mut s, 7, 503, -3, 3, &[997, 600, 503]);
        // Соседние участки (502|503 и 997|998) сливаются: между ними id нет.
        assert_eq!(s.history_ranges(7).unwrap(), vec![(498, LIVE)]);
        let w = s.history_window(7, Anchor::At(997), 1, 2).unwrap();
        assert_eq!(ids(&w), vec![600, 997, 998]);
    }

    #[test]
    fn locals_only_at_the_end() {
        let mut s = Store::in_memory().unwrap();
        page(&mut s, 7, 0, 0, 3, &[10, 9, 8]);
        s.outbox_enqueue(&crate::outbox::Outgoing { peer_id: 7, text: "в очереди", entities: &[], reply_to: 0, random_id: 42 }, 1, 100).unwrap();
        let w = s.history_window(7, Anchor::Latest, 50, 0).unwrap();
        assert_eq!(w.messages.last().unwrap().text, "в очереди");
        let w = s.history_window(7, Anchor::At(9), 1, 1).unwrap();
        assert_eq!(ids(&w), vec![8, 9], "окно не доходит до конца — очереди нет");
        let w = s.history_window(7, Anchor::At(9), 1, 5).unwrap();
        assert!(w.end && w.messages.last().unwrap().id < 0);
    }

    #[test]
    fn updates_extend_and_resets_cut() {
        let mut s = Store::in_memory().unwrap();
        let batch = |updates, pts| UpdatesBatch {
            updates,
            peers: vec![],
            state: Some(UpdateState { pts, qts: 0, seq: 0, date: 1 }),
            channel_states: vec![],
        };
        // Первое сообщение нового чата начинает участок.
        s.apply_batch(&batch(vec![DomainUpdate::NewMessage(msg(7, 50))], 1)).unwrap();
        s.apply_batch(&batch(vec![DomainUpdate::NewMessage(msg(7, 51))], 2)).unwrap();
        assert_eq!(s.history_ranges(7).unwrap(), vec![(50, LIVE)]);
        // Разрыв: участок обрезан по последнему; новые — новый участок.
        s.apply_batch(&batch(vec![DomainUpdate::Reset], 3)).unwrap();
        assert_eq!(s.history_ranges(7).unwrap(), vec![(50, 51)]);
        s.apply_batch(&batch(vec![DomainUpdate::NewMessage(msg(7, 90))], 4)).unwrap();
        assert_eq!(s.history_ranges(7).unwrap(), vec![(50, 51), (90, LIVE)]);
        // Канал: ResetChannel стирает историю и участки.
        let ch = -CHANNEL_SHIFT - 5;
        s.apply_batch(&batch(vec![DomainUpdate::NewMessage(msg(ch, 3))], 5)).unwrap();
        s.apply_batch(&batch(vec![DomainUpdate::ResetChannel { peer_id: ch }], 6)).unwrap();
        assert!(s.history_ranges(ch).unwrap().is_empty());
    }

    #[test]
    fn dialog_top_starts_or_breaks_live_range() {
        let mut s = Store::in_memory().unwrap();
        let d = |top| Dialog { peer_id: 7, top_message: top, ..Default::default() };
        s.save_peers(&[crate::model::Peer { id: 7, name: "Анна".into(), ..Default::default() }]).unwrap();
        s.save_dialogs(&[d(10)], &[msg(7, 10)], &[]).unwrap();
        assert_eq!(s.history_ranges(7).unwrap(), vec![(10, LIVE)]);
        // Верх уже известен (пришёл обновлением) — участок продолжается.
        s.apply_batch(&UpdatesBatch { updates: vec![DomainUpdate::NewMessage(msg(7, 11))], ..Default::default() }).unwrap();
        s.save_dialogs(&[d(11)], &[msg(7, 11)], &[]).unwrap();
        assert_eq!(s.history_ranges(7).unwrap(), vec![(10, LIVE)]);
        // Верх новый и неизвестный (обновления пропущены): старый участок обрезан.
        s.save_dialogs(&[d(40)], &[msg(7, 40)], &[]).unwrap();
        assert_eq!(s.history_ranges(7).unwrap(), vec![(10, 11), (40, LIVE)]);
    }
}
