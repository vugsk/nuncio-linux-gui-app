//! Вкладки общих медиа в панели «Сведения» (Св-1): медиа, файлы, ссылки,
//! голосовые. Это поиск по чату с пустым запросом и фильтром по типу
//! (`messages.search`), постранично от новых к старым. Сообщения вкладки
//! открываются тем же путём, что и в ленте: фото и видео — окно просмотра,
//! файлы — скачать и открыть, голосовые — проиграть.

use crate::chat::{ChatEffect, ChatModel, DbOp, Pending};
use crate::media::{viewable, FeedImage};
use crate::model::{Command, CommandResult, MediaKind, Message, Payload, SearchFilter};

/// Сообщений за запрос.
pub const SHARED_PAGE: i32 = 60;

/// Сторона ячейки сетки медиа (px), картинка раскодируется вдвое крупнее.
pub const GRID_CELL: i32 = 116;

/// Открытая вкладка общих медиа и её содержимое.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SharedMedia {
    pub peer_id: i64,
    pub filter: SearchFilter,
    pub items: Vec<Message>,
    /// Всего по данным сервера.
    pub total: i32,
    pub loading: bool,
    pub complete: bool,
    pub(crate) seq: u64,
}

impl ChatModel {
    /// Вкладка по умолчанию при открытии «Сведений»: у групп — участники
    /// (`All`), у личных чатов и каналов (участников там нет, Св-6) — медиа.
    pub(crate) fn default_shared_tab(&mut self, out: &mut Vec<ChatEffect>) {
        let group = self.open_peer().is_some_and(|p| {
            p.kind == crate::model::PeerKind::Chat || (p.kind == crate::model::PeerKind::Channel && !p.broadcast)
        });
        if self.shared.is_none() && !group {
            self.shared_tab(SearchFilter::Media, out);
        }
    }

    /// Выбрана вкладка. `SearchFilter::All` — вкладка «Участники» (или ничего).
    pub(crate) fn shared_tab(&mut self, filter: SearchFilter, out: &mut Vec<ChatEffect>) {
        if filter == SearchFilter::All {
            self.shared = None;
            return;
        }
        let Some(peer_id) = self.open else { return };
        if self.shared.as_ref().is_some_and(|s| s.filter == filter && s.peer_id == peer_id) {
            return;
        }
        let seq = self.shared.as_ref().map_or(0, |s| s.seq);
        self.shared = Some(SharedMedia { peer_id, filter, seq, ..Default::default() });
        self.shared_request(out);
    }

    /// Следующая страница (список докручен до конца).
    pub(crate) fn shared_more(&mut self, out: &mut Vec<ChatEffect>) {
        if self.shared.as_ref().is_some_and(|s| !s.loading && !s.complete) {
            self.shared_request(out);
        }
    }

    /// Открыт другой чат: та же вкладка — для него.
    pub(crate) fn shared_chat_changed(&mut self, out: &mut Vec<ChatEffect>) {
        let filter = self.shared.take().map(|s| s.filter);
        if self.panel != Some(crate::chatlist::Panel::Info) {
            return;
        }
        match filter {
            Some(f) => self.shared_tab(f, out),
            None => self.default_shared_tab(out),
        }
    }

    fn shared_request(&mut self, out: &mut Vec<ChatEffect>) {
        let Some(s) = self.shared.as_mut() else { return };
        let peer_id = s.peer_id;
        let Some(peer) = self.peer_ref(peer_id) else { return };
        let s = self.shared.as_mut().expect("проверено выше");
        s.seq += 1;
        s.loading = true;
        let offset_id = s.items.last().map_or(0, |m| m.id);
        let (filter, seq) = (s.filter, s.seq);
        let cmd = Command::SearchMessages { peer: Some(peer), query: String::new(), filter, min_date: 0, max_date: 0,
            from: None, offset_id, limit: SHARED_PAGE, offset_rate: 0, offset_peer: None };
        self.send(cmd, Pending::Shared { peer_id, seq }, out);
    }

    pub(crate) fn shared_result(&mut self, peer_id: i64, seq: u64, r: &CommandResult, out: &mut Vec<ChatEffect>) {
        let Some(s) = self.shared.as_mut().filter(|s| s.peer_id == peer_id && s.seq == seq) else { return };
        s.loading = false;
        match (&r.error, &r.payload) {
            (None, Payload::History { messages, peers, total, .. }) => {
                s.total = *total;
                s.complete = messages.len() < SHARED_PAGE as usize;
                // Сервер может вернуть уже показанное на стыке страниц.
                let known: std::collections::HashSet<i32> = s.items.iter().map(|m| m.id).collect();
                s.items.extend(messages.iter().filter(|m| !known.contains(&m.id)).cloned());
                log::info!("общие медиа: {:?} {} из {}", s.filter, s.items.len(), s.total);
                out.push(ChatEffect::Db(DbOp::SaveHistory { messages: messages.clone(), peers: peers.clone(), span: None }));
                self.media_window_changed(out);
            }
            (Some(e), _) => {
                s.complete = true;
                let t = crate::errors::error_text(e, self.lang == "ru");
                self.toast(&t);
            }
            _ => s.complete = true,
        }
    }

    /// Сообщение открытого чата: из окна истории или из вкладки общих медиа.
    pub fn find_msg(&self, id: i32) -> Option<&Message> {
        self.history.iter().find(|m| m.id == id).or_else(|| self.shared_items().iter().find(|m| m.id == id))
    }

    /// Сообщения открытой вкладки (пусто, если вкладки нет или она от другого чата).
    pub fn shared_items(&self) -> &[Message] {
        match &self.shared {
            Some(s) if Some(s.peer_id) == self.open => &s.items,
            _ => &[],
        }
    }

    /// Ячейки сетки «Медиа»: картинка (фото или кадр видео) размером под ячейку.
    pub fn shared_images(&self) -> Vec<(i32, FeedImage)> {
        if self.shared.as_ref().is_none_or(|s| s.filter != SearchFilter::Media) {
            return Vec::new();
        }
        self.shared_items()
            .iter()
            .filter_map(|m| {
                let mi = m.media_info.as_ref()?;
                let file = match mi.kind {
                    MediaKind::Photo => mi.file.clone(),
                    MediaKind::Video | MediaKind::Gif | MediaKind::VideoNote => mi.thumb.clone(),
                    _ => return None,
                };
                if file.is_none() && mi.stripped.is_empty() {
                    return None;
                }
                // Под «cover» в квадрате: короткая сторона — две ячейки.
                let (w, h) = (mi.width.max(1), mi.height.max(1));
                let side = 2 * GRID_CELL;
                let (w, h) = if w < h { (side, (side * h / w).min(4 * side)) } else { ((side * w / h).min(4 * side), side) };
                Some((m.id, FeedImage { file, stripped: mi.stripped.clone(), w, h, album: (0, 1), local: String::new(),
                    round: false }))
            })
            .collect()
    }

    /// Фото и видео для стрелок окна просмотра: из истории или из вкладки.
    pub fn viewer_ids(&self) -> Vec<i32> {
        let in_history = self.viewer.is_some_and(|v| self.history.iter().any(|m| m.id == v));
        let src: &[Message] = if in_history { &self.history } else { self.shared_items() };
        let mut ids: Vec<i32> = src
            .iter()
            .filter(|m| m.media_info.as_ref().is_some_and(|mi| viewable(mi) && mi.kind != MediaKind::WebPage))
            .map(|m| m.id)
            .collect();
        // Вкладка идёт от новых к старым, история — наоборот: стрелка вправо — новее.
        if !in_history {
            ids.reverse();
        }
        ids
    }
}

/// Ссылка сообщения для вкладки «Ссылки»: адрес, заголовок, сайт.
pub fn link_of(m: &Message) -> Option<(String, String, String)> {
    if let Some(wp) = m.media_info.as_ref().and_then(|mi| mi.webpage.as_ref()).filter(|w| !w.url.is_empty()) {
        let title = if wp.title.is_empty() { wp.url.clone() } else { wp.title.clone() };
        return Some((wp.url.clone(), title, wp.site.clone()));
    }
    // Иначе — первая ссылка в тексте (смещения сущностей — в UTF-16).
    let utf16: Vec<u16> = m.text.encode_utf16().collect();
    m.entities.iter().find_map(|e| {
        let text = || {
            let a = usize::try_from(e.offset).ok()?;
            let b = a.checked_add(usize::try_from(e.length).ok()?)?;
            Some(String::from_utf16_lossy(utf16.get(a..b)?))
        };
        match e.kind {
            crate::model::EntityKind::TextUrl if !e.url.is_empty() => Some((e.url.clone(), text()?, String::new())),
            crate::model::EntityKind::Url => {
                let t = text()?;
                let url = if t.contains("://") { t.clone() } else { format!("https://{t}") };
                Some((url, t, String::new()))
            }
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Media, Peer};

    fn model() -> ChatModel {
        let mut m = ChatModel::new(1, "ru", 1000);
        m.open = Some(10);
        m.extra_peers.insert(10, Peer { id: 10, access_hash: 30, ..Default::default() });
        m
    }

    fn photo(id: i32) -> Message {
        Message {
            peer_id: 10,
            id,
            media_info: Some(Box::new(Media { kind: MediaKind::Photo, width: 800, height: 600, stripped: vec![1],
                ..Default::default() })),
            ..Default::default()
        }
    }

    fn sent(out: &[ChatEffect]) -> Vec<(u64, Command)> {
        out.iter().filter_map(|e| if let ChatEffect::Send { id, cmd } = e { Some((*id, cmd.clone())) } else { None })
            .collect()
    }

    fn reply(id: u64, messages: Vec<Message>, total: i32) -> CommandResult {
        CommandResult { cmd_id: id, error: None, payload: Payload::History { messages, peers: vec![], total, next_rate: 0 } }
    }

    #[test]
    fn tab_pages_and_viewer() {
        let mut m = model();
        let mut out = Vec::new();
        m.shared_tab(SearchFilter::Media, &mut out);
        let s = sent(&out);
        assert!(matches!(&s[..], [(_, Command::SearchMessages { filter: SearchFilter::Media, offset_id: 0, ref query, .. })]
            if query.is_empty()));
        let first: Vec<Message> = (0..SHARED_PAGE).map(|i| photo(500 - i)).collect();
        out.clear();
        out.extend(m.event(&crate::model::Event::Result(reply(s[0].0, first, 100))));
        assert_eq!(m.shared_items().len(), SHARED_PAGE as usize);
        assert_eq!(m.shared_images().len(), SHARED_PAGE as usize, "у каждого фото — ячейка");
        // Вторая страница — от последнего показанного.
        out.clear();
        m.shared_more(&mut out);
        let s = sent(&out);
        assert!(matches!(&s[..], [(_, Command::SearchMessages { offset_id, .. })] if *offset_id == 500 - SHARED_PAGE + 1));
        out.extend(m.event(&crate::model::Event::Result(reply(s[0].0, vec![photo(400), photo(441)], 100))));
        assert!(m.shared.as_ref().unwrap().complete);
        assert_eq!(m.shared_items().len(), SHARED_PAGE as usize + 1, "повтор на стыке не дублируется");
        // Фото из вкладки (его нет в истории) открывается в окне просмотра.
        out.clear();
        m.intent(crate::chat::ChatIntent::MediaClick(450));
        assert_eq!(m.viewer, Some(450));
        assert_eq!(m.find_msg(450).map(|x| x.id), Some(450));
        m.intent(crate::chat::ChatIntent::ViewerStep(1));
        assert_eq!(m.viewer, Some(451), "вправо — новее");
        // Вкладка «Участники» закрывает общие медиа.
        m.shared_tab(SearchFilter::All, &mut out);
        assert!(m.shared.is_none());
    }
}
