//! Веха В6 в модели главного окна: папки и архив в фильтре списка (Дз-8),
//! поиск в шапке списка (по базе и на сервере — разными разделами), правая
//! панель (сведения о чате Св-1 и поиск по чату Дз-10, Дз-18), действия над
//! чатом из контекстного меню и панели (Сп-4) с подтверждением опасных.

use std::collections::{HashMap, HashSet};

use crate::chat::{ChatEffect, ChatIntent, ChatModel, DbOp, Pending};
use crate::chatstore::DialogRow;
use crate::model::{
    ChatActionKind, Command, CommandResult, DomainUpdate, Folder, Message, Payload, Peer, PeerKind,
    SearchFilter,
};

/// Что показывает список.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListFilter {
    #[default]
    All,
    Archive,
    Folder(i32),
}

/// Поиск в шапке списка: чаты по названию, сообщения в базе и на сервере.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GlobalSearch {
    pub query: String,
    /// Диалоги, чьё название или username совпали (из базы).
    pub chats: Vec<Peer>,
    /// Люди и каналы с сервера, которых нет среди диалогов.
    pub global_peers: Vec<Peer>,
    /// Сообщения в базе (новые первыми).
    pub local: Vec<Message>,
    /// Сообщения с сервера, которых нет среди найденных в базе.
    pub server: Vec<Message>,
    pub server_loading: bool,
    /// Продолжение серверного поиска: 0 — конец.
    pub next_rate: i32,
    /// Имена чатов найденных сообщений.
    pub peers: HashMap<i64, Peer>,
}

/// Правая панель (Дз-3): один режим за раз.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Info,
    Search,
}

/// Период поиска по чату (Дз-18): от сегодняшнего дня назад.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Period {
    #[default]
    Any,
    Day,
    Week,
    Month,
    Year,
}

impl Period {
    pub fn min_date(self, now: i64) -> i64 {
        let day = 86_400;
        match self {
            Period::Any => 0,
            Period::Day => now - day,
            Period::Week => now - 7 * day,
            Period::Month => now - 30 * day,
            Period::Year => now - 365 * day,
        }
    }
}

/// Поиск по открытому чату в правой панели.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChatSearch {
    pub query: String,
    pub filter: SearchFilter,
    pub period: Period,
    pub from: Option<i64>,
    pub results: Vec<Message>,
    /// Всего совпадений по данным сервера.
    pub total: i32,
    /// Выбранный результат (стрелки).
    pub index: Option<usize>,
    pub loading: bool,
    pub complete: bool,
    seq: u64,
}

/// Действие, ждущее подтверждения (очистить, удалить, выйти, заблокировать).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Confirm {
    pub peer_id: i64,
    pub kind: ChatActionKind,
    /// «И у собеседника» — для удаления личного чата.
    pub revoke: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListIntent {
    SetFilter(ListFilter),
    SearchQuery(String),
    SearchMore,
    /// Открыть чат из результатов (и перейти к сообщению).
    OpenResult { peer_id: i64, message_id: i32 },
    ShowPanel(Option<Panel>),
    MembersQuery(String),
    ChatSearchQuery(String),
    ChatSearchFilter(SearchFilter),
    ChatSearchPeriod(Period),
    ChatSearchFrom(Option<i64>),
    ChatSearchMore,
    /// Стрелки: к более старому (+1) или более новому (−1) результату.
    ChatSearchStep(i32),
    ChatSearchOpen(i32),
    /// Действие над чатом; опасные — сначала подтверждение.
    Action { peer_id: i64, kind: ChatActionKind },
    MarkRead(i64),
    /// «В папку…» (Сп-4): добавить чат в папку или убрать из неё.
    ToggleFolder { peer_id: i64, folder_id: i32 },
    ToggleConfirmRevoke,
    ConfirmAction,
    CancelAction,
    /// Вкладка общих медиа в «Сведениях» (`All` — участники).
    SharedTab(SearchFilter),
    SharedMore,
}

const SEARCH_PAGE: i32 = 50;

/// Состав папки по её правилам (как у Telegram): явно включённые — всегда,
/// иначе по видам чатов; исключённые и исключающие условия — прочь.
pub fn in_folder(f: &Folder, r: &DialogRow) -> bool {
    let id = r.peer.id;
    if f.pinned.contains(&id) || f.include.contains(&id) {
        return true;
    }
    if f.chatlist || f.exclude.contains(&id) {
        return false;
    }
    let p = &r.peer;
    let kind_ok = match p.kind {
        PeerKind::User if p.is_bot => f.bots,
        PeerKind::User if p.contact || p.is_self => f.contacts,
        PeerKind::User => f.non_contacts,
        PeerKind::Chat => f.groups,
        PeerKind::Channel if p.broadcast => f.broadcasts,
        PeerKind::Channel => f.groups,
    };
    let d = &r.dialog;
    kind_ok
        && !(f.exclude_muted && d.muted)
        && !(f.exclude_read && d.unread_count == 0 && !d.unread_mark)
        && !(f.exclude_archived && d.folder == 1)
}

/// Чат в папке — убрать (явно включённый — из списка, подходящий по правилам —
/// в исключения); не в папке — включить явно.
pub fn toggle_in_folder(f: &mut Folder, r: &DialogRow) {
    let id = r.peer.id;
    if in_folder(f, r) {
        f.include.retain(|x| *x != id);
        f.pinned.retain(|x| *x != id);
        if in_folder(f, r) {
            f.exclude.push(id);
        }
    } else {
        f.exclude.retain(|x| *x != id);
        f.include.push(id);
    }
}

fn dangerous(kind: ChatActionKind) -> bool {
    matches!(kind, ChatActionKind::ClearHistory | ChatActionKind::DeleteChat | ChatActionKind::Leave
        | ChatActionKind::Block)
}

impl ChatModel {
    /// Видимый список по фильтру.
    pub(crate) fn apply_filter(&mut self) {
        let f = match self.filter {
            ListFilter::All => {
                self.rows = self.all_rows.iter().filter(|r| r.dialog.folder == 0).cloned().collect();
                return;
            }
            ListFilter::Archive => {
                self.rows = self.all_rows.iter().filter(|r| r.dialog.folder == 1).cloned().collect();
                return;
            }
            ListFilter::Folder(id) => match self.folders.iter().find(|f| f.id == id) {
                Some(f) => f.clone(),
                None => {
                    self.filter = ListFilter::All;
                    return self.apply_filter();
                }
            },
        };
        let mut rows: Vec<DialogRow> = self.all_rows.iter().filter(|r| in_folder(&f, r)).cloned().collect();
        // Закреплённые в папке — первыми в её порядке, дальше по времени (у папки свой порядок).
        rows.sort_by_key(|r| {
            let pin = f.pinned.iter().position(|p| *p == r.peer.id).unwrap_or(usize::MAX);
            (pin, -r.top_date, -i64::from(r.dialog.top_message))
        });
        for r in &mut rows {
            r.dialog.pinned = f.pinned.contains(&r.peer.id);
        }
        self.rows = rows;
    }

    /// Непрочитанных чатов в фильтре (счётчик в выпадающем списке папок).
    pub fn unread_in(&self, filter: ListFilter) -> usize {
        let unread = |r: &&DialogRow| r.dialog.unread_count > 0 || r.dialog.unread_mark;
        match filter {
            ListFilter::All => self.all_rows.iter().filter(|r| r.dialog.folder == 0).filter(unread).count(),
            ListFilter::Archive => self.all_rows.iter().filter(|r| r.dialog.folder == 1).filter(unread).count(),
            ListFilter::Folder(id) => self
                .folders
                .iter()
                .find(|f| f.id == id)
                .map(|f| self.all_rows.iter().filter(|r| in_folder(f, r)).filter(unread).count())
                .unwrap_or(0),
        }
    }

    /// Результат `ReloadFolders`.
    pub fn set_folders(&mut self, folders: Vec<Folder>) {
        self.folders = folders;
        self.apply_filter();
    }

    /// Результат `SearchLocal`: сообщения из базы и чаты по названию.
    pub fn set_local_search(&mut self, query: &str, local: Vec<Message>) {
        let q = query.trim().to_lowercase();
        let chats: Vec<Peer> = self
            .all_rows
            .iter()
            .filter(|r| r.peer.name.to_lowercase().contains(&q) || (!r.peer.username.is_empty()
                && r.peer.username.to_lowercase().starts_with(q.trim_start_matches('@'))))
            .map(|r| r.peer.clone())
            .take(20)
            .collect();
        let peers: HashMap<i64, Peer> = self.all_rows.iter().map(|r| (r.peer.id, r.peer.clone())).collect();
        if let Some(s) = self.search.as_mut().filter(|s| s.query.trim().to_lowercase() == q) {
            s.chats = chats;
            s.local = local;
            s.peers.extend(peers);
            let seen: HashSet<(i64, i32)> = s.local.iter().map(|m| (m.peer_id, m.id)).collect();
            s.server.retain(|m| !seen.contains(&(m.peer_id, m.id)));
            log::debug!("поиск: чатов {}, в базе {}", s.chats.len(), s.local.len());
        }
    }

    fn server_search(&mut self, more: bool, out: &mut Vec<ChatEffect>) {
        let Some(s) = self.search.as_ref() else { return };
        let query = s.query.trim().to_string();
        if query.chars().count() < 2 || self.pending.values().any(|p| matches!(p, Pending::GlobalSearch { .. })) {
            return;
        }
        let (rate, offset_peer, offset_id) = match (more, s.server.last()) {
            (true, Some(last)) if s.next_rate > 0 => (s.next_rate, self.peer_ref(last.peer_id), last.id),
            (true, _) => return,
            _ => (0, None, 0),
        };
        if let Some(s) = self.search.as_mut() {
            s.server_loading = true;
        }
        self.send(
            Command::SearchMessages { peer: None, query: query.clone(), filter: SearchFilter::All, min_date: 0,
                max_date: 0, from: None, offset_id, limit: SEARCH_PAGE, offset_rate: rate, offset_peer },
            Pending::GlobalSearch { query: query.clone() },
            out,
        );
        if !more && query.chars().count() >= 3 {
            self.send(Command::SearchPeers { query: query.clone() }, Pending::ServerPeers { query }, out);
        }
    }

    fn chat_search_request(&mut self, more: bool, out: &mut Vec<ChatEffect>) {
        let Some(peer_id) = self.open else { return };
        let Some(peer) = self.peer_ref(peer_id) else { return };
        let now = self.now;
        let Some(cs) = self.chat_search.as_mut() else { return };
        if cs.query.trim().is_empty() && cs.filter == SearchFilter::All && cs.from.is_none() {
            cs.results.clear();
            cs.total = 0;
            cs.index = None;
            return;
        }
        if more && (cs.complete || cs.loading) {
            return;
        }
        if !more {
            cs.results.clear();
            cs.index = None;
            cs.complete = false;
        }
        cs.seq += 1;
        cs.loading = true;
        let offset_id = if more { cs.results.last().map(|m| m.id).unwrap_or(0) } else { 0 };
        let (query, filter, min_date, from, seq) =
            (cs.query.trim().to_string(), cs.filter, cs.period.min_date(now), cs.from, cs.seq);
        // Отправитель — участник группы: access_hash из сведений о чате или из базы.
        let from = from.map(|f| self.member_ref(f));
        let cmd = Command::SearchMessages { peer: Some(peer), query, filter, min_date, max_date: 0, from, offset_id,
            limit: SEARCH_PAGE, offset_rate: 0, offset_peer: None };
        self.send(cmd, Pending::ChatSearch { peer_id, seq }, out);
    }

    fn member_ref(&self, id: i64) -> crate::model::PeerRef {
        self.info
            .as_ref()
            .and_then(|i| i.peers.iter().find(|p| p.id == id))
            .map(|p| p.peer_ref())
            .or_else(|| self.peer_ref(id))
            .unwrap_or(crate::model::PeerRef { id, access_hash: 0 })
    }

    fn load_info(&mut self, query: &str, out: &mut Vec<ChatEffect>) {
        let Some(peer_id) = self.open else { return };
        if let Some(peer) = self.peer_ref(peer_id) {
            self.send(Command::GetChatInfo { peer, members_query: query.to_string() }, Pending::ChatInfo { peer_id },
                out);
        }
    }

    fn run_action(&mut self, peer_id: i64, kind: ChatActionKind, revoke: bool, out: &mut Vec<ChatEffect>) {
        if let Some(peer) = self.peer_ref(peer_id) {
            self.send(Command::ChatAction { peer, kind, revoke }, Pending::ChatAction { peer_id, kind }, out);
        }
    }

    pub(crate) fn list_intent(&mut self, i: ListIntent, out: &mut Vec<ChatEffect>) {
        match i {
            ListIntent::SetFilter(f) => {
                self.filter = f;
                self.apply_filter();
            }
            ListIntent::SearchQuery(q) => {
                if q.trim().is_empty() {
                    self.search = None;
                    return;
                }
                let s = self.search.get_or_insert_with(GlobalSearch::default);
                if s.query == q {
                    return;
                }
                s.query = q.clone();
                s.server.clear();
                s.global_peers.clear();
                s.next_rate = 0;
                out.push(ChatEffect::Db(DbOp::SearchLocal { query: q }));
                self.server_search(false, out);
            }
            ListIntent::SearchMore => self.server_search(true, out),
            ListIntent::OpenResult { peer_id, message_id } => {
                if let Some(p) = self.search.as_ref().and_then(|s| {
                    s.global_peers.iter().chain(s.peers.values()).find(|p| p.id == peer_id).cloned()
                }) {
                    if !self.all_rows.iter().any(|r| r.peer.id == peer_id) {
                        self.extra_peers.insert(peer_id, p);
                    }
                }
                out.extend(self.intent(ChatIntent::OpenChat(peer_id)));
                if message_id > 0 {
                    out.extend(self.intent(ChatIntent::JumpTo(message_id)));
                }
            }
            ListIntent::ShowPanel(p) => {
                self.panel = p;
                match p {
                    Some(Panel::Info) => self.load_info("", out),
                    Some(Panel::Search) => {
                        self.chat_search.get_or_insert_with(ChatSearch::default);
                        if self.info.as_ref().map(|i| i.peer_id) != self.open {
                            self.load_info("", out); // участники — для фильтра «Отправитель»
                        }
                    }
                    None => {
                        self.chat_search = None;
                        self.shared = None;
                    }
                }
            }
            ListIntent::MembersQuery(q) => self.load_info(&q, out),
            ListIntent::SharedTab(f) => self.shared_tab(f, out),
            ListIntent::SharedMore => self.shared_more(out),
            ListIntent::ChatSearchQuery(q) => {
                let cs = self.chat_search.get_or_insert_with(ChatSearch::default);
                if cs.query != q {
                    cs.query = q;
                    self.chat_search_request(false, out);
                }
            }
            ListIntent::ChatSearchFilter(f) => {
                self.chat_search.get_or_insert_with(ChatSearch::default).filter = f;
                self.chat_search_request(false, out);
            }
            ListIntent::ChatSearchPeriod(p) => {
                self.chat_search.get_or_insert_with(ChatSearch::default).period = p;
                self.chat_search_request(false, out);
            }
            ListIntent::ChatSearchFrom(f) => {
                self.chat_search.get_or_insert_with(ChatSearch::default).from = f;
                self.chat_search_request(false, out);
            }
            ListIntent::ChatSearchMore => self.chat_search_request(true, out),
            ListIntent::ChatSearchStep(d) => {
                let Some(cs) = self.chat_search.as_mut() else { return };
                if cs.results.is_empty() {
                    return;
                }
                let next = match cs.index {
                    None => 0,
                    Some(i) => (i as i64 + i64::from(d)).clamp(0, cs.results.len() as i64 - 1) as usize,
                };
                cs.index = Some(next);
                let id = cs.results[next].id;
                let near_end = next + 5 >= cs.results.len();
                out.extend(self.intent(ChatIntent::JumpTo(id)));
                if near_end {
                    self.chat_search_request(true, out);
                }
            }
            ListIntent::ChatSearchOpen(id) => {
                if let Some(cs) = self.chat_search.as_mut() {
                    cs.index = cs.results.iter().position(|m| m.id == id);
                }
                out.extend(self.intent(ChatIntent::JumpTo(id)));
            }
            ListIntent::Action { peer_id, kind } => {
                if dangerous(kind) {
                    // «И у собеседника» по умолчанию — в личных чатах (как в Telegram Desktop).
                    self.confirm = Some(Confirm { peer_id, kind, revoke: false });
                } else {
                    self.run_action(peer_id, kind, false, out);
                }
            }
            ListIntent::MarkRead(peer_id) => {
                let top = self.all_rows.iter().find(|r| r.peer.id == peer_id).map(|r| r.dialog.top_message);
                if let (Some(top), Some(peer)) = (top, self.peer_ref(peer_id)) {
                    self.send(Command::MarkRead { peer, max_id: top }, Pending::Read { peer_id }, out);
                    out.push(ChatEffect::Db(DbOp::MarkReadLocal { peer_id, max_id: top }));
                    out.push(ChatEffect::Db(DbOp::ApplyLocal(vec![DomainUpdate::PeerSettings {
                        peer_id, muted: None, pinned: None, unread_mark: Some(false) }])));
                    out.push(ChatEffect::Db(DbOp::ReloadRows));
                }
            }
            ListIntent::ToggleFolder { peer_id, folder_id } => {
                let Some(row) = self.all_rows.iter().find(|r| r.peer.id == peer_id).cloned() else { return };
                let Some(f) = self.folders.iter_mut().find(|f| f.id == folder_id) else { return };
                toggle_in_folder(f, &row);
                let f = f.clone();
                // Ссылки на все чаты папки: серверу нужны access_hash.
                let ids: HashSet<i64> = f.include.iter().chain(&f.exclude).chain(&f.pinned).copied().collect();
                let peers = ids.into_iter().filter_map(|id| self.peer_ref(id)).collect();
                self.send(Command::UpdateFolder { folder: f, peers }, Pending::Folders, out);
                out.push(ChatEffect::Db(DbOp::SaveFolders(self.folders.clone())));
                self.apply_filter();
            }
            ListIntent::ToggleConfirmRevoke => {
                if let Some(c) = self.confirm.as_mut() {
                    c.revoke = !c.revoke;
                }
            }
            ListIntent::ConfirmAction => {
                if let Some(c) = self.confirm.take() {
                    self.run_action(c.peer_id, c.kind, c.revoke, out);
                }
            }
            ListIntent::CancelAction => self.confirm = None,
        }
    }

    /// Действие выполнено: те же изменения локально, не дожидаясь обновлений.
    fn action_done(&mut self, peer_id: i64, kind: ChatActionKind, out: &mut Vec<ChatEffect>) {
        use ChatActionKind::*;
        let settings = |muted, pinned, unread_mark| DomainUpdate::PeerSettings { peer_id, muted, pinned, unread_mark };
        let local = match kind {
            Mute => vec![settings(Some(true), None, None)],
            Unmute => vec![settings(Some(false), None, None)],
            Pin => vec![settings(None, Some(true), None)],
            Unpin => vec![settings(None, Some(false), None)],
            MarkUnread => vec![settings(None, None, Some(true))],
            Archive => vec![DomainUpdate::PeerFolder { peer_id, folder: 1 }],
            Unarchive => vec![DomainUpdate::PeerFolder { peer_id, folder: 0 }],
            ClearHistory | DeleteChat | Leave => {
                let dialog = kind != ClearHistory;
                out.push(ChatEffect::Db(DbOp::DropChat { peer_id, dialog }));
                if dialog && self.open == Some(peer_id) {
                    out.extend(self.intent(ChatIntent::CloseChat));
                    self.panel = None;
                } else if self.open == Some(peer_id) {
                    self.history.clear();
                }
                Vec::new()
            }
            Block | Unblock => {
                if let Some(i) = self.info.as_mut().filter(|i| i.peer_id == peer_id) {
                    i.blocked = kind == Block;
                }
                Vec::new()
            }
        };
        if !local.is_empty() {
            out.push(ChatEffect::Db(DbOp::ApplyLocal(local)));
        }
        out.push(ChatEffect::Db(DbOp::ReloadRows));
        if self.open == Some(peer_id) {
            out.push(ChatEffect::Db(DbOp::ReloadHistory {
                peer_id,
                anchor: self.anchor,
                before: crate::chat::LATEST_WINDOW,
                after: crate::chat::WINDOW_HALF,
            }));
        }
    }

    pub(crate) fn list_result(&mut self, p: Pending, r: &CommandResult, out: &mut Vec<ChatEffect>) {
        if let Some(e) = &r.error {
            if let Pending::ChatSearch { .. } = p {
                if let Some(cs) = self.chat_search.as_mut() {
                    cs.loading = false;
                }
            }
            if let Pending::GlobalSearch { .. } = p {
                if let Some(s) = self.search.as_mut() {
                    s.server_loading = false;
                }
            }
            if matches!(p, Pending::ChatAction { .. }) {
                let t = e.text.clone();
                self.toast(&t);
            }
            return;
        }
        match (p, &r.payload) {
            (Pending::Folders, Payload::Folders(f)) => {
                out.push(ChatEffect::Db(DbOp::SaveFolders(f.clone())));
                self.set_folders(f.clone());
            }
            (Pending::GlobalSearch { query }, Payload::History { messages, peers, next_rate, .. }) => {
                out.push(ChatEffect::Db(DbOp::SaveHistory { messages: messages.clone(), peers: peers.clone(), span: None }));
                let Some(s) = self.search.as_mut().filter(|s| s.query.trim() == query) else {
                    // Запрос сменился, пока шёл ответ: искать новый.
                    self.server_search(false, out);
                    return;
                };
                s.server_loading = false;
                s.next_rate = *next_rate;
                s.peers.extend(peers.iter().map(|p| (p.id, p.clone())));
                let seen: HashSet<(i64, i32)> = s.local.iter().chain(&s.server).map(|m| (m.peer_id, m.id)).collect();
                s.server.extend(messages.iter().filter(|m| !seen.contains(&(m.peer_id, m.id))).cloned());
                log::debug!("поиск: на сервере {}", s.server.len());
            }
            (Pending::ServerPeers { query }, Payload::Peers(peers)) => {
                out.push(ChatEffect::Db(DbOp::SavePeers(peers.clone())));
                let known: HashSet<i64> = self.all_rows.iter().map(|r| r.peer.id).collect();
                if let Some(s) = self.search.as_mut().filter(|s| s.query.trim() == query) {
                    s.global_peers = peers.iter().filter(|p| !known.contains(&p.id) && !p.min).cloned().collect();
                }
            }
            (Pending::ChatInfo { peer_id }, Payload::ChatInfo(info)) => {
                if self.open == Some(peer_id) {
                    out.push(ChatEffect::Db(DbOp::SavePeers(info.peers.clone())));
                    log::debug!("сведения: участников {}, описание {}", info.participants.len(), !info.about.is_empty());
                    self.info = Some(info.clone());
                }
            }
            (Pending::ChatSearch { peer_id, seq }, Payload::History { messages, peers, total, .. }) => {
                out.push(ChatEffect::Db(DbOp::SaveHistory { messages: messages.clone(), peers: peers.clone(), span: None }));
                if self.open != Some(peer_id) {
                    return;
                }
                let Some(cs) = self.chat_search.as_mut().filter(|c| c.seq == seq) else { return };
                cs.loading = false;
                cs.total = *total;
                cs.complete = messages.len() < SEARCH_PAGE as usize;
                cs.results.extend(messages.iter().cloned());
                log::debug!("поиск по чату: {} из {}", cs.results.len(), cs.total);
            }
            (Pending::ChatAction { peer_id, kind }, _) => self.action_done(peer_id, kind, out),
            _ => {}
        }
    }

    /// Имя собеседника для строки результата поиска.
    pub fn search_peer(&self, id: i64) -> Option<&Peer> {
        self.search
            .as_ref()
            .and_then(|s| s.peers.get(&id))
            .or_else(|| self.all_rows.iter().find(|r| r.peer.id == id).map(|r| &r.peer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Dialog, Event, PeerRef};

    fn row(id: i64, f: impl FnOnce(&mut DialogRow)) -> DialogRow {
        let mut r = DialogRow {
            dialog: Dialog { peer_id: id, top_message: 1, ..Default::default() },
            peer: Peer { id, kind: PeerKind::of(id), name: format!("p{id}"), access_hash: 7, ..Default::default() },
            last: None,
            top_date: id,
            avatar_path: String::new(),
        };
        f(&mut r);
        r
    }

    fn sends(fx: &[ChatEffect]) -> Vec<(u64, Command)> {
        fx.iter().filter_map(|e| match e {
            ChatEffect::Send { id, cmd } => Some((*id, cmd.clone())),
            _ => None,
        }).collect()
    }

    fn ok(id: u64, payload: Payload) -> Event {
        Event::Result(CommandResult { cmd_id: id, error: None, payload })
    }

    #[test]
    fn folder_rules_like_telegram() {
        let contact = row(1, |r| r.peer.contact = true);
        let stranger = row(2, |_| {});
        let bot = row(3, |r| r.peer.is_bot = true);
        let group = row(-4, |_| {});
        let channel = row(-1_000_000_000_005, |r| r.peer.broadcast = true);
        let supergroup = row(-1_000_000_000_006, |_| {});
        let muted_group = row(-7, |r| r.dialog.muted = true);
        let archived = row(-8, |r| r.dialog.folder = 1);
        let read_group = row(-9, |r| r.dialog.unread_count = 0);
        let f = Folder { groups: true, exclude_muted: true, exclude_archived: true, include: vec![2], exclude: vec![-4],
            ..Default::default() };
        let got: Vec<bool> = [&contact, &stranger, &bot, &group, &channel, &supergroup, &muted_group, &archived,
            &read_group].iter().map(|r| in_folder(&f, r)).collect();
        assert_eq!(got, [false, true, false, false, false, true, false, false, true]);
        let unread_only = Folder { contacts: true, non_contacts: true, exclude_read: true, ..Default::default() };
        let unread = row(10, |r| r.dialog.unread_count = 3);
        let marked = row(11, |r| r.dialog.unread_mark = true);
        assert!(in_folder(&unread_only, &unread) && in_folder(&unread_only, &marked) && !in_folder(&unread_only, &stranger));
        let chatlist = Folder { chatlist: true, groups: true, include: vec![-4], ..Default::default() };
        assert!(in_folder(&chatlist, &group) && !in_folder(&chatlist, &supergroup), "папка по ссылке — только явные");
    }

    #[test]
    fn toggle_folder_membership() {
        let contact = row(1, |r| r.peer.contact = true);
        let stranger = row(2, |_| {});
        let mut f = Folder { id: 3, contacts: true, ..Default::default() };
        toggle_in_folder(&mut f, &contact);
        assert!(!in_folder(&f, &contact) && f.exclude == [1], "подходит по правилам — в исключения");
        toggle_in_folder(&mut f, &contact);
        assert!(in_folder(&f, &contact) && f.exclude.is_empty() && f.include == [1]);
        toggle_in_folder(&mut f, &stranger);
        assert!(in_folder(&f, &stranger));
        toggle_in_folder(&mut f, &stranger);
        assert!(!in_folder(&f, &stranger) && !f.include.contains(&2));
        // Папка отправляется целиком, с access_hash её чатов.
        let mut m = ChatModel::new(1, "ru", 0);
        m.set_rows(vec![contact.clone(), stranger.clone()]);
        m.set_folders(vec![Folder { id: 3, contacts: true, ..Default::default() }]);
        let fx = m.intent(ChatIntent::List(ListIntent::ToggleFolder { peer_id: 2, folder_id: 3 }));
        let cmd = sends(&fx)[0].1.clone();
        assert_eq!(cmd, Command::UpdateFolder {
            folder: Folder { id: 3, contacts: true, include: vec![2], ..Default::default() },
            peers: vec![crate::model::PeerRef { id: 2, access_hash: 7 }],
        });
    }

    #[test]
    fn filters_archive_and_folder_order() {
        let mut m = ChatModel::new(1, "ru", 1000);
        m.set_rows(vec![row(10, |_| {}), row(20, |r| r.dialog.folder = 1), row(-30, |_| {}), row(40, |r| r.peer.contact = true)]);
        assert_eq!(m.rows.iter().map(|r| r.peer.id).collect::<Vec<_>>(), [10, -30, 40], "архив не в основном");
        m.intent(ChatIntent::List(ListIntent::SetFilter(ListFilter::Archive)));
        assert_eq!(m.rows.iter().map(|r| r.peer.id).collect::<Vec<_>>(), [20]);
        m.set_folders(vec![Folder { id: 5, contacts: true, groups: true, pinned: vec![40], ..Default::default() }]);
        m.intent(ChatIntent::List(ListIntent::SetFilter(ListFilter::Folder(5))));
        assert_eq!(m.rows.iter().map(|r| r.peer.id).collect::<Vec<_>>(), [40, -30], "закреплённый в папке первым");
        assert!(m.rows[0].dialog.pinned);
        // Папка удалена на сервере — назад к «Все чаты».
        m.set_folders(vec![]);
        assert_eq!(m.filter, ListFilter::All);
    }

    #[test]
    fn search_local_first_then_server_without_duplicates() {
        let mut m = ChatModel::new(1, "ru", 1000);
        m.set_rows(vec![row(10, |r| r.peer.name = "Релизная группа".into()), row(20, |_| {})]);
        let fx = m.intent(ChatIntent::List(ListIntent::SearchQuery("рел".into())));
        assert!(fx.contains(&ChatEffect::Db(DbOp::SearchLocal { query: "рел".into() })));
        let s = sends(&fx);
        let (gid, cmd) = s[0].clone();
        assert!(matches!(cmd, Command::SearchMessages { peer: None, ref query, .. } if query == "рел"));
        assert!(s.iter().any(|(_, c)| matches!(c, Command::SearchPeers { .. })), "люди и каналы на сервере");
        let msg = |peer_id, id| Message { peer_id, id, text: "релиз".into(), ..Default::default() };
        m.set_local_search("рел", vec![msg(10, 5)]);
        assert_eq!(m.search.as_ref().unwrap().chats[0].id, 10, "чат по названию");
        m.event(&ok(gid, Payload::History { messages: vec![msg(10, 5), msg(99, 7)], peers: vec![], total: 2, next_rate: 0 }));
        let s = m.search.as_ref().unwrap();
        assert_eq!((s.local.len(), s.server.len()), (1, 1), "найденное в базе с сервера не повторяется");
        assert_eq!(s.server[0].peer_id, 99);
        // Пустой запрос — список снова обычный.
        m.intent(ChatIntent::List(ListIntent::SearchQuery("  ".into())));
        assert!(m.search.is_none());
    }

    #[test]
    fn chat_search_filters_steps_and_stale_results() {
        let mut m = ChatModel::new(1, "ru", 100_000);
        m.set_rows(vec![row(10, |_| {})]);
        m.intent(ChatIntent::OpenChat(10));
        m.intent(ChatIntent::List(ListIntent::ShowPanel(Some(Panel::Search))));
        let fx = m.intent(ChatIntent::List(ListIntent::ChatSearchQuery("отчёт".into())));
        let (old, _) = sends(&fx)[0].clone();
        let fx = m.intent(ChatIntent::List(ListIntent::ChatSearchPeriod(Period::Week)));
        let (id, cmd) = sends(&fx)[0].clone();
        assert_eq!(cmd, Command::SearchMessages { peer: Some(PeerRef { id: 10, access_hash: 7 }), query: "отчёт".into(),
            filter: SearchFilter::All, min_date: 100_000 - 7 * 86_400, max_date: 0, from: None, offset_id: 0,
            limit: SEARCH_PAGE, offset_rate: 0, offset_peer: None });
        let msg = |id| Message { peer_id: 10, id, ..Default::default() };
        m.event(&ok(old, Payload::History { messages: vec![msg(1)], peers: vec![], total: 1, next_rate: 0 }));
        assert!(m.chat_search.as_ref().unwrap().results.is_empty(), "устаревший ответ не смешивается");
        m.event(&ok(id, Payload::History { messages: vec![msg(30), msg(20)], peers: vec![], total: 2, next_rate: 0 }));
        m.intent(ChatIntent::List(ListIntent::ChatSearchStep(1)));
        assert_eq!(m.anchor, crate::history::Anchor::At(30), "первый результат — самый новый");
        m.intent(ChatIntent::List(ListIntent::ChatSearchStep(1)));
        assert_eq!(m.anchor, crate::history::Anchor::At(20));
        m.intent(ChatIntent::List(ListIntent::ChatSearchStep(1)));
        assert_eq!(m.chat_search.as_ref().unwrap().index, Some(1), "дальше последнего не уходит");
    }

    #[test]
    fn dangerous_actions_confirmed_and_applied_locally() {
        let mut m = ChatModel::new(1, "ru", 0);
        m.set_rows(vec![row(10, |_| {})]);
        let fx = m.intent(ChatIntent::List(ListIntent::Action { peer_id: 10, kind: ChatActionKind::Mute }));
        let (id, cmd) = sends(&fx)[0].clone();
        assert_eq!(cmd, Command::ChatAction { peer: PeerRef { id: 10, access_hash: 7 }, kind: ChatActionKind::Mute, revoke: false });
        let fx = m.event(&ok(id, Payload::None));
        assert!(fx.contains(&ChatEffect::Db(DbOp::ApplyLocal(vec![DomainUpdate::PeerSettings { peer_id: 10,
            muted: Some(true), pinned: None, unread_mark: None }]))));
        // Удаление — только после подтверждения.
        assert!(sends(&m.intent(ChatIntent::List(ListIntent::Action { peer_id: 10, kind: ChatActionKind::DeleteChat }))).is_empty());
        m.intent(ChatIntent::List(ListIntent::ToggleConfirmRevoke));
        let (id, cmd) = sends(&m.intent(ChatIntent::List(ListIntent::ConfirmAction)))[0].clone();
        assert!(matches!(cmd, Command::ChatAction { kind: ChatActionKind::DeleteChat, revoke: true, .. }));
        let fx = m.event(&ok(id, Payload::None));
        assert!(fx.contains(&ChatEffect::Db(DbOp::DropChat { peer_id: 10, dialog: true })));
    }
}
