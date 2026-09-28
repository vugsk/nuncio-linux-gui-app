//! Модель главного окна вертикали (В4): список диалогов и открытый чат.
//! Чистая: намерения и события превращаются в эффекты — команды сети и
//! операции с базой, которые выполняет поток домена; результаты базы
//! возвращаются методами `set_rows` и `set_history`.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::chatstore::DialogRow;
use crate::model::{
    Command, CommandResult, Dialog, Event, Message, Payload, Peer, PeerRef, UpdatesBatch,
};

/// Сколько диалогов держать в списке (окно модели; остальные — из базы по требованию, В5).
pub const DIALOGS_WINDOW: usize = 500;
/// Страница истории.
pub const HISTORY_PAGE: i32 = 50;
/// Страниц диалогов при старте (по 100): не упираться в ограничитель частоты.
pub const DIALOG_PAGES: u32 = 5;
/// Одновременных загрузок аватаров.
pub const AVATARS_INFLIGHT: usize = 3;

/// Операция с базой (выполняет поток домена в потоке БД).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbOp {
    SaveDialogs { dialogs: Vec<Dialog>, messages: Vec<Message>, peers: Vec<Peer> },
    SaveHistory { messages: Vec<Message>, peers: Vec<Peer> },
    ApplyBatch(UpdatesBatch),
    SetAvatar { peer_id: i64, path: String },
    MarkReadLocal { peer_id: i64, max_id: i32 },
    /// Перечитать список (результат — `set_rows`).
    ReloadRows,
    /// Перечитать окно истории открытого чата (результат — `set_history`).
    ReloadHistory { peer_id: i64, limit: usize },
    /// Очередь отправки (О-9).
    Enqueue { peer_id: i64, text: String, reply_to: i32, random_id: i64 },
    /// Поиск собеседников в базе (результат — `set_picker_results`).
    SearchPeers { query: String },
    SavePeers(Vec<Peer>),
    /// Закреплённые открытого чата из базы (результат — `set_pinned`).
    ReloadPinned { peer_id: i64 },
    /// Истёкшие — в «ошибку», готовые — в `outbox_ready` (результат — `set_outbox_ready`).
    PumpOutbox,
    OutboxSending { local_id: i64 },
    OutboxDone { local_id: i64, sent: Option<Message> },
    OutboxRequeue { local_id: i64, next_try: i64 },
    OutboxFail { local_id: i64, error: String },
    OutboxRetry { local_id: i64 },
    OutboxDelete { local_id: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEffect {
    Send { id: u64, cmd: Command },
    Db(DbOp),
    /// Скопировать в буфер обмена (Чт-9).
    Clipboard(String),
}

/// Режим поля ввода: ответ или правка (одна панель над полем, Дз-2).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Compose {
    #[default]
    Plain,
    Reply { id: i32, author: String, snippet: String },
    Edit { id: i32, original: String },
}

/// Зачем открыт диалог выбора (Дз-16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerPurpose {
    Forward { from: i64, ids: Vec<i32> },
    NewChat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    pub purpose: PickerPurpose,
    pub query: String,
    pub results: Vec<Peer>,
    pub selected: Vec<i64>,
}

/// Подтверждение удаления: можно ли «и у собеседника» и отмечено ли.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteAsk {
    pub ids: Vec<i32>,
    pub can_revoke: bool,
    pub revoke: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatIntent {
    OpenChat(i64),
    CloseChat,
    ComposerEdited(String),
    Send,
    LoadOlder,
    Tick(i64),
    /// «Повторить» и «Удалить» у неотправленного (Чт-6).
    RetrySend(i64),
    DeleteUnsent(i64),
    /// Контекстное меню сообщения (Чт-9).
    Reply(i32),
    Edit(i32),
    CancelCompose,
    Forward(Vec<i32>),
    AskDelete(Vec<i32>),
    ToggleRevoke,
    ConfirmDelete,
    CancelDelete,
    Pin(i32),
    Unpin(i32),
    CopyText(i32),
    CopyLink(i32),
    /// Диалог выбора (Дз-16).
    NewChat,
    PickerQuery(String),
    PickerToggle(i64),
    PickerConfirm,
    PickerClose,
    /// Полоса закреплённого (Чт-14): следующее закреплённое.
    NextPinned,
    DismissToast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Dialogs,
    History { peer_id: i64, older: bool },
    Outbox { local_id: i64 },
    Avatar { peer_id: i64 },
    Action,
    Pinned { peer_id: i64 },
    Quotes { peer_id: i64 },
    PickerSearch,
    Contacts,
    Other,
}

#[derive(Debug, Clone, Default)]
pub struct ChatModel {
    pub rows: Vec<DialogRow>,
    pub open: Option<i64>,
    /// Окно истории открытого чата, по возрастанию id.
    pub history: Vec<Message>,
    pub history_complete: bool,
    pub history_loading: bool,
    /// Сколько сообщений держать в окне (растёт при подгрузке вверх).
    pub history_limit: usize,
    /// Имена отправителей истории (группы): id → имя.
    pub senders: HashMap<i64, String>,
    /// Цитаты ответов (Чт-3): id исходного → (автор, первая строка).
    pub quotes: HashMap<i32, (String, String)>,
    pub compose: Compose,
    pub picker: Option<Picker>,
    pub delete_ask: Option<DeleteAsk>,
    /// Закреплённые открытого чата, новые первыми; в полосе — `pinned_index`.
    pub pinned: Vec<Message>,
    pub pinned_index: usize,
    /// Всплывающее сообщение (Со-3) и когда его убрать.
    pub toast: Option<(String, i64)>,
    /// Собеседники вне списка диалогов (новый чат из диалога выбора).
    pub extra_peers: HashMap<i64, Peer>,
    pub composer: String,
    pub sending: bool,
    pub error: Option<String>,
    pub dialogs_loading: bool,
    pub self_id: i64,
    pub now: i64,
    pub lang: String,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    pages: u32,
    avatar_queue: VecDeque<(PeerRef, i64, i32)>,
    avatar_requested: HashSet<(i64, i64)>,
    avatars_inflight: usize,
    random_seq: u64,
}

impl ChatModel {
    pub fn new(self_id: i64, lang: &str, now: i64) -> Self {
        Self {
            self_id,
            lang: lang.into(),
            now,
            next_id: 1 << 32, // у модели входа id с 1: не пересекаются
            history_limit: HISTORY_PAGE as usize,
            ..Default::default()
        }
    }

    fn send(&mut self, cmd: Command, p: Pending, out: &mut Vec<ChatEffect>) {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, p);
        out.push(ChatEffect::Send { id, cmd });
    }

    /// Открытый собеседник.
    pub fn open_row(&self) -> Option<&DialogRow> {
        let id = self.open?;
        self.rows.iter().find(|r| r.peer.id == id)
    }

    fn peer_ref(&self, peer_id: i64) -> Option<PeerRef> {
        self.open_peer_of(peer_id).map(|p| p.peer_ref())
    }

    fn open_peer_of(&self, peer_id: i64) -> Option<&Peer> {
        self.rows.iter().find(|r| r.peer.id == peer_id).map(|r| &r.peer).or_else(|| self.extra_peers.get(&peer_id))
    }

    /// Собеседник открытого чата (в том числе ещё не в списке диалогов).
    pub fn open_peer(&self) -> Option<&Peer> {
        self.open_peer_of(self.open?)
    }

    fn toast(&mut self, text: &str) {
        self.toast = Some((text.to_string(), self.now + 3));
    }

    fn history_msg(&self, id: i32) -> Option<&Message> {
        self.history.iter().find(|m| m.id == id)
    }

    fn load_dialogs_page(&mut self, offset: Option<(i64, i32, PeerRef)>, out: &mut Vec<ChatEffect>) {
        self.dialogs_loading = true;
        let (offset_date, offset_id, offset_peer) = match offset {
            Some((d, i, p)) => (d, i, Some(p)),
            None => (0, 0, None),
        };
        self.send(Command::LoadDialogs { limit: 100, offset_date, offset_id, offset_peer }, Pending::Dialogs, out);
    }

    /// Вход выполнен: показать сохранённое и обновить с сервера.
    pub fn enter(&mut self) -> Vec<ChatEffect> {
        let mut out = vec![ChatEffect::Db(DbOp::ReloadRows)];
        self.pages = 0;
        self.load_dialogs_page(None, &mut out);
        out
    }

    /// Результат `ReloadRows`: список и очередь аватаров.
    pub fn set_rows(&mut self, rows: Vec<DialogRow>) -> Vec<ChatEffect> {
        self.rows = rows;
        for r in &self.rows {
            let key = (r.peer.id, r.peer.photo_id);
            if r.peer.photo_id != 0 && r.avatar_path.is_empty() && !r.peer.min && self.avatar_requested.insert(key) {
                self.avatar_queue.push_back((r.peer.peer_ref(), r.peer.photo_id, r.peer.photo_dc));
            }
        }
        let mut out = Vec::new();
        self.pump_avatars(&mut out);
        out
    }

    fn pump_avatars(&mut self, out: &mut Vec<ChatEffect>) {
        while self.avatars_inflight < AVATARS_INFLIGHT {
            let Some((peer, photo_id, dc)) = self.avatar_queue.pop_front() else { break };
            self.avatars_inflight += 1;
            self.send(Command::DownloadAvatar { peer, photo_id, dc }, Pending::Avatar { peer_id: peer.id }, out);
        }
    }

    /// Результат `ReloadHistory`.
    pub fn set_history(&mut self, peer_id: i64, msgs: Vec<Message>, senders: HashMap<i64, String>) {
        if self.open == Some(peer_id) {
            self.history = msgs;
            self.senders = senders;
        }
    }

    /// Цитаты ответов из базы; недостающие — запросом к серверу (один раз).
    pub fn set_quotes(&mut self, peer_id: i64, quotes: HashMap<i32, (String, String)>) -> Vec<ChatEffect> {
        let mut out = Vec::new();
        if self.open != Some(peer_id) {
            return out;
        }
        self.quotes = quotes;
        let missing: Vec<i32> = self
            .history
            .iter()
            .filter(|m| m.reply_to > 0 && !self.quotes.contains_key(&m.reply_to))
            .map(|m| m.reply_to)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let already = self.pending.values().any(|p| *p == Pending::Quotes { peer_id });
        if !missing.is_empty() && !already {
            if let Some(peer) = self.peer_ref(peer_id) {
                self.send(Command::LoadMessages { peer, ids: missing }, Pending::Quotes { peer_id }, &mut out);
            }
        }
        out
    }

    pub fn set_pinned(&mut self, peer_id: i64, msgs: Vec<Message>) {
        if self.open == Some(peer_id) {
            self.pinned = msgs;
            if self.pinned_index >= self.pinned.len() {
                self.pinned_index = 0;
            }
        }
    }

    pub fn set_picker_results(&mut self, peers: Vec<Peer>) {
        if let Some(p) = self.picker.as_mut() {
            let mut seen = HashSet::new();
            p.results = peers.into_iter().filter(|x| !x.min && seen.insert(x.id)).collect();
        }
    }

    fn random_id(&mut self) -> i64 {
        use std::hash::{BuildHasher, Hasher};
        self.random_seq += 1;
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(self.random_seq);
        h.write_i64(self.now);
        (h.finish() >> 1) as i64 | 1
    }

    pub fn intent(&mut self, i: ChatIntent) -> Vec<ChatEffect> {
        let mut out = Vec::new();
        match i {
            ChatIntent::Tick(now) => {
                self.now = now;
                if self.toast.as_ref().is_some_and(|(_, until)| *until <= now) {
                    self.toast = None;
                }
                out.push(ChatEffect::Db(DbOp::PumpOutbox));
            }
            ChatIntent::OpenChat(peer_id) => {
                if self.open == Some(peer_id) {
                    return out;
                }
                self.open = Some(peer_id);
                self.history.clear();
                self.compose = Compose::Plain;
                self.pinned.clear();
                self.pinned_index = 0;
                self.quotes.clear();
                self.history_complete = false;
                self.history_limit = HISTORY_PAGE as usize;
                self.composer.clear();
                self.error = None;
                out.push(ChatEffect::Db(DbOp::ReloadHistory { peer_id, limit: self.history_limit }));
                out.push(ChatEffect::Db(DbOp::ReloadPinned { peer_id }));
                if let Some(peer) = self.peer_ref(peer_id) {
                    self.history_loading = true;
                    self.send(Command::LoadHistory { peer, offset_id: 0, limit: HISTORY_PAGE },
                        Pending::History { peer_id, older: false }, &mut out);
                    self.send(Command::LoadPinned { peer }, Pending::Pinned { peer_id }, &mut out);
                    let unread = self.open_row().map(|r| (r.dialog.unread_count, r.dialog.top_message));
                    if let Some((n, top)) = unread {
                        if n > 0 {
                            self.send(Command::MarkRead { peer, max_id: top }, Pending::Other, &mut out);
                            out.push(ChatEffect::Db(DbOp::MarkReadLocal { peer_id, max_id: top }));
                            out.push(ChatEffect::Db(DbOp::ReloadRows));
                        }
                    }
                }
            }
            ChatIntent::CloseChat => {
                self.open = None;
                self.history.clear();
            }
            ChatIntent::ComposerEdited(t) => self.composer = t,
            ChatIntent::Send => {
                // Сразу в очередь (О-9): сообщение видно в чате как «отправляется».
                let text = self.composer.trim().to_string();
                let Some(peer_id) = self.open else { return out };
                if text.is_empty() {
                    return out;
                }
                self.composer.clear();
                self.error = None;
                match std::mem::take(&mut self.compose) {
                    Compose::Edit { id, .. } => {
                        if let Some(peer) = self.peer_ref(peer_id) {
                            self.send(Command::EditMessage { peer, id, text }, Pending::Action, &mut out);
                        }
                    }
                    Compose::Reply { id, .. } => out = self.enqueue_reply(peer_id, text, id),
                    Compose::Plain => out = self.enqueue_text(peer_id, text),
                }
            }
            ChatIntent::RetrySend(local_id) => {
                out.push(ChatEffect::Db(DbOp::OutboxRetry { local_id }));
                self.reload_open(&mut out);
                out.push(ChatEffect::Db(DbOp::PumpOutbox));
            }
            ChatIntent::DeleteUnsent(local_id) => {
                out.push(ChatEffect::Db(DbOp::OutboxDelete { local_id }));
                self.reload_open(&mut out);
                out.push(ChatEffect::Db(DbOp::ReloadRows));
            }
            ChatIntent::Reply(id) => {
                if let Some(m) = self.history_msg(id).cloned() {
                    let author = self.author_name(&m);
                    self.compose = Compose::Reply { id, author, snippet: snippet(&m) };
                }
            }
            ChatIntent::Edit(id) => {
                if let Some(m) = self.history_msg(id).filter(|m| m.out && m.id > 0).cloned() {
                    self.compose = Compose::Edit { id, original: m.text.clone() };
                    self.composer = m.text;
                }
            }
            ChatIntent::CancelCompose => {
                if matches!(self.compose, Compose::Edit { .. }) {
                    self.composer.clear();
                }
                self.compose = Compose::Plain;
            }
            ChatIntent::Forward(ids) => {
                if let Some(from) = self.open {
                    self.open_picker(PickerPurpose::Forward { from, ids }, &mut out);
                }
            }
            ChatIntent::NewChat => self.open_picker(PickerPurpose::NewChat, &mut out),
            ChatIntent::PickerQuery(q) => {
                if let Some(p) = self.picker.as_mut() {
                    p.query = q.clone();
                }
                out.push(ChatEffect::Db(DbOp::SearchPeers { query: q.trim().to_string() }));
                // Глобальный поиск людей по username — с трёх знаков.
                if q.trim().chars().count() >= 3 {
                    self.send(Command::SearchPeers { query: q.trim().to_string() }, Pending::PickerSearch, &mut out);
                }
            }
            ChatIntent::PickerToggle(peer_id) => {
                if let Some(p) = self.picker.as_mut() {
                    match p.purpose {
                        PickerPurpose::NewChat => p.selected = vec![peer_id],
                        PickerPurpose::Forward { .. } => {
                            if let Some(i) = p.selected.iter().position(|x| *x == peer_id) {
                                p.selected.remove(i);
                            } else {
                                p.selected.push(peer_id);
                            }
                        }
                    }
                }
            }
            ChatIntent::PickerConfirm => {
                let Some(p) = self.picker.take() else { return out };
                let peers: Vec<Peer> =
                    p.selected.iter().filter_map(|id| p.results.iter().find(|x| x.id == *id).cloned()).collect();
                for peer in &peers {
                    if !self.rows.iter().any(|r| r.peer.id == peer.id) {
                        self.extra_peers.insert(peer.id, peer.clone());
                    }
                }
                match p.purpose {
                    PickerPurpose::NewChat => {
                        if let Some(peer) = peers.first() {
                            out.extend(self.intent(ChatIntent::OpenChat(peer.id)));
                        }
                    }
                    PickerPurpose::Forward { from, ids } => {
                        if let (Some(from), false) = (self.peer_ref(from), peers.is_empty()) {
                            let targets: Vec<(PeerRef, Vec<i64>)> = peers
                                .iter()
                                .map(|x| (x.peer_ref(), ids.iter().map(|_| self.random_id()).collect()))
                                .collect();
                            self.send(Command::Forward { from, ids, targets }, Pending::Action, &mut out);
                        }
                    }
                }
            }
            ChatIntent::PickerClose => self.picker = None,
            ChatIntent::AskDelete(ids) => {
                let Some(peer) = self.open else { return out };
                // «И у собеседника» — в личных чатах и группах (в каналах удаление всегда у всех).
                let own = ids.iter().all(|id| self.history_msg(*id).is_some_and(|m| m.out));
                let can_revoke = crate::model::PeerKind::of(peer) != crate::model::PeerKind::Channel
                    && self.open_peer().is_some_and(|p| !p.is_self);
                self.delete_ask = Some(DeleteAsk { ids, can_revoke, revoke: can_revoke && own });
            }
            ChatIntent::ToggleRevoke => {
                if let Some(d) = self.delete_ask.as_mut().filter(|d| d.can_revoke) {
                    d.revoke = !d.revoke;
                }
            }
            ChatIntent::ConfirmDelete => {
                let (Some(d), Some(peer_id)) = (self.delete_ask.take(), self.open) else { return out };
                if let Some(peer) = self.peer_ref(peer_id) {
                    self.send(Command::DeleteMessages { peer, ids: d.ids, revoke: d.revoke }, Pending::Action, &mut out);
                }
            }
            ChatIntent::CancelDelete => self.delete_ask = None,
            ChatIntent::Pin(id) | ChatIntent::Unpin(id) => {
                let unpin = matches!(i, ChatIntent::Unpin(_));
                if let Some(peer) = self.open.and_then(|p| self.peer_ref(p)) {
                    self.send(Command::PinMessage { peer, id, unpin }, Pending::Action, &mut out);
                }
            }
            ChatIntent::CopyText(id) => {
                if let Some(m) = self.history_msg(id) {
                    out.push(ChatEffect::Clipboard(m.text.clone()));
                    self.toast(if self.lang == "ru" { "Скопировано" } else { "Copied" });
                }
            }
            ChatIntent::CopyLink(id) => {
                if let Some(link) = self.open_peer().and_then(|p| message_link(p, id)) {
                    out.push(ChatEffect::Clipboard(link));
                    self.toast(if self.lang == "ru" { "Ссылка скопирована" } else { "Link copied" });
                }
            }
            ChatIntent::NextPinned => {
                if !self.pinned.is_empty() {
                    self.pinned_index = (self.pinned_index + 1) % self.pinned.len();
                }
            }
            ChatIntent::DismissToast => self.toast = None,
            ChatIntent::LoadOlder => {
                let Some(peer_id) = self.open else { return out };
                if self.history_loading || self.history_complete {
                    return out;
                }
                let Some(oldest) = self.history.first().map(|m| m.id) else { return out };
                let Some(peer) = self.peer_ref(peer_id) else { return out };
                self.history_loading = true;
                self.history_limit += HISTORY_PAGE as usize;
                self.send(Command::LoadHistory { peer, offset_id: oldest, limit: HISTORY_PAGE },
                    Pending::History { peer_id, older: true }, &mut out);
            }
        }
        out
    }

    /// Поставить текст в очередь отправки (О-9).
    pub fn enqueue_text(&mut self, peer_id: i64, text: String) -> Vec<ChatEffect> {
        self.enqueue_reply(peer_id, text, 0)
    }

    fn enqueue_reply(&mut self, peer_id: i64, text: String, reply_to: i32) -> Vec<ChatEffect> {
        let random_id = self.random_id();
        vec![
            ChatEffect::Db(DbOp::Enqueue { peer_id, text, reply_to, random_id }),
            ChatEffect::Db(DbOp::ReloadHistory { peer_id, limit: self.history_limit }),
            ChatEffect::Db(DbOp::ReloadRows),
            ChatEffect::Db(DbOp::PumpOutbox),
        ]
    }

    fn open_picker(&mut self, purpose: PickerPurpose, out: &mut Vec<ChatEffect>) {
        self.picker = Some(Picker { purpose, query: String::new(), results: Vec::new(), selected: Vec::new() });
        out.push(ChatEffect::Db(DbOp::SearchPeers { query: String::new() }));
        self.send(Command::GetContacts, Pending::Contacts, out);
    }

    fn author_name(&self, m: &Message) -> String {
        if m.out {
            return if self.lang == "ru" { "Вы".into() } else { "You".into() };
        }
        self.senders
            .get(&m.from_id)
            .cloned()
            .or_else(|| self.open_peer().map(|p| p.name.clone()))
            .unwrap_or_default()
    }

    fn reload_open(&self, out: &mut Vec<ChatEffect>) {
        if let Some(peer_id) = self.open {
            out.push(ChatEffect::Db(DbOp::ReloadHistory { peer_id, limit: self.history_limit }));
        }
    }

    /// Результат `PumpOutbox`: готовые к отправке (по одному на чат).
    pub fn set_outbox_ready(&mut self, items: Vec<crate::outbox::OutboxItem>) -> Vec<ChatEffect> {
        let mut out = Vec::new();
        for it in items {
            if self.pending.values().any(|p| *p == Pending::Outbox { local_id: it.local_id }) {
                continue;
            }
            let peer = PeerRef { id: it.peer_id, access_hash: it.access_hash };
            out.push(ChatEffect::Db(DbOp::OutboxSending { local_id: it.local_id }));
            self.send(Command::SendMessageReply { peer, text: it.text, random_id: it.random_id, reply_to: it.reply_to },
                Pending::Outbox { local_id: it.local_id }, &mut out);
        }
        if !out.is_empty() {
            self.reload_open(&mut out);
        }
        out
    }

    pub fn event(&mut self, e: &Event) -> Vec<ChatEffect> {
        let mut out = Vec::new();
        match e {
            Event::Connection(crate::model::ConnState::Connected) => out.push(ChatEffect::Db(DbOp::PumpOutbox)),
            Event::Updates(b) => {
                out.push(ChatEffect::Db(DbOp::ApplyBatch(b.clone())));
                out.push(ChatEffect::Db(DbOp::ReloadRows));
                if let Some(peer_id) = self.open {
                    out.push(ChatEffect::Db(DbOp::ReloadHistory { peer_id, limit: self.history_limit }));
                    // Открытый чат: входящее сразу прочитано (как в Telegram Desktop при фокусе).
                    let newest_in = b.updates.iter().filter_map(|u| match u {
                        crate::model::DomainUpdate::NewMessage(m) if m.peer_id == peer_id && !m.out => Some(m.id),
                        _ => None,
                    }).max();
                    if let (Some(max_id), Some(peer)) = (newest_in, self.peer_ref(peer_id)) {
                        self.send(Command::MarkRead { peer, max_id }, Pending::Other, &mut out);
                        out.push(ChatEffect::Db(DbOp::MarkReadLocal { peer_id, max_id }));
                    }
                }
                let reset_open = b.updates.iter().any(|u| {
                    matches!(u, crate::model::DomainUpdate::ResetChannel { peer_id } if Some(*peer_id) == self.open)
                });
                if let (true, Some(peer_id)) = (reset_open, self.open) {
                    if let Some(peer) = self.peer_ref(peer_id) {
                        self.history_complete = false;
                        self.history_loading = true;
                        self.send(Command::LoadHistory { peer, offset_id: 0, limit: HISTORY_PAGE },
                            Pending::History { peer_id, older: false }, &mut out);
                    }
                }
                if b.updates.iter().any(|u| matches!(u, crate::model::DomainUpdate::Reset)) {
                    self.pages = 0;
                    self.load_dialogs_page(None, &mut out);
                }
            }
            Event::Result(r) => self.result(r, &mut out),
            _ => {}
        }
        out
    }

    fn result(&mut self, r: &CommandResult, out: &mut Vec<ChatEffect>) {
        let Some(p) = self.pending.remove(&r.cmd_id) else { return };
        match p {
            Pending::Dialogs => {
                self.dialogs_loading = false;
                if let Payload::Dialogs { dialogs, messages, peers, complete, .. } = &r.payload {
                    out.push(ChatEffect::Db(DbOp::SaveDialogs {
                        dialogs: dialogs.clone(),
                        messages: messages.clone(),
                        peers: peers.clone(),
                    }));
                    out.push(ChatEffect::Db(DbOp::ReloadRows));
                    self.pages += 1;
                    // Следующая страница: смещение — последний диалог страницы.
                    if !complete && !dialogs.is_empty() && self.pages < DIALOG_PAGES {
                        let last = dialogs.last().unwrap();
                        let date = messages
                            .iter()
                            .find(|m| m.peer_id == last.peer_id && m.id == last.top_message)
                            .map(|m| m.date)
                            .unwrap_or(0);
                        let hash = peers.iter().find(|p| p.id == last.peer_id).map(|p| p.access_hash).unwrap_or(0);
                        let peer = PeerRef { id: last.peer_id, access_hash: hash };
                        self.load_dialogs_page(Some((date, last.top_message, peer)), out);
                    }
                } else if let Some(e) = &r.error {
                    self.error = Some(e.text.clone());
                }
            }
            Pending::History { peer_id, older } => {
                self.history_loading = false;
                if let Payload::History { messages, peers, .. } = &r.payload {
                    if self.open == Some(peer_id) {
                        // Неполная страница — дальше сообщений нет.
                        self.history_complete = messages.len() < HISTORY_PAGE as usize;
                    }
                    let _ = older;
                    out.push(ChatEffect::Db(DbOp::SaveHistory { messages: messages.clone(), peers: peers.clone() }));
                    if self.open == Some(peer_id) {
                        out.push(ChatEffect::Db(DbOp::ReloadHistory { peer_id, limit: self.history_limit }));
                    }
                    out.push(ChatEffect::Db(DbOp::ReloadRows));
                } else if let Some(e) = &r.error {
                    self.error = Some(e.text.clone());
                }
            }
            Pending::Outbox { local_id } => {
                use crate::model::ErrorKind;
                match (&r.error, &r.payload) {
                    (None, Payload::Sent(m)) => {
                        out.push(ChatEffect::Db(DbOp::OutboxDone { local_id, sent: Some(m.clone()) }));
                    }
                    (None, _) => out.push(ChatEffect::Db(DbOp::OutboxDone { local_id, sent: None })),
                    // Первая попытка уже дошла (тот же random_id): дубля нет, сообщение придёт обновлением.
                    (Some(e), _) if e.text == "RANDOM_ID_DUPLICATE" => {
                        out.push(ChatEffect::Db(DbOp::OutboxDone { local_id, sent: None }));
                    }
                    (Some(e), _) if e.kind == ErrorKind::Flood => out.push(ChatEffect::Db(DbOp::OutboxRequeue {
                        local_id,
                        next_try: self.now + i64::from(e.retry_after.max(1)),
                    })),
                    (Some(e), _)
                        if matches!(e.kind, ErrorKind::Network | ErrorKind::Cancelled | ErrorKind::Internal)
                            || e.code == 500 || e.code == -500 =>
                    {
                        // Сеть: остаётся в очереди, повтор по подключению или таймеру.
                        out.push(ChatEffect::Db(DbOp::OutboxRequeue { local_id, next_try: self.now + 5 }));
                    }
                    (Some(e), _) => out.push(ChatEffect::Db(DbOp::OutboxFail { local_id, error: e.text.clone() })),
                }
                self.reload_open(out);
                out.push(ChatEffect::Db(DbOp::ReloadRows));
                out.push(ChatEffect::Db(DbOp::PumpOutbox));
            }
            Pending::Avatar { peer_id } => {
                self.avatars_inflight = self.avatars_inflight.saturating_sub(1);
                if let Payload::File(path) = &r.payload {
                    out.push(ChatEffect::Db(DbOp::SetAvatar { peer_id, path: path.clone() }));
                    out.push(ChatEffect::Db(DbOp::ReloadRows));
                }
                self.pump_avatars(out);
            }
            Pending::Action => {
                if let Some(e) = &r.error {
                    let t = e.text.clone();
                    self.toast(&t);
                } else {
                    self.reload_open(out);
                    out.push(ChatEffect::Db(DbOp::ReloadRows));
                }
            }
            Pending::Pinned { peer_id } => {
                if let Payload::History { messages, peers, .. } = &r.payload {
                    out.push(ChatEffect::Db(DbOp::SaveHistory { messages: messages.clone(), peers: peers.clone() }));
                    out.push(ChatEffect::Db(DbOp::ReloadPinned { peer_id }));
                }
            }
            Pending::Quotes { peer_id } => {
                if let Payload::History { messages, peers, .. } = &r.payload {
                    out.push(ChatEffect::Db(DbOp::SaveHistory { messages: messages.clone(), peers: peers.clone() }));
                    if self.open == Some(peer_id) {
                        out.push(ChatEffect::Db(DbOp::ReloadHistory { peer_id, limit: self.history_limit }));
                    }
                }
            }
            Pending::PickerSearch | Pending::Contacts => {
                if let Payload::Peers(peers) = &r.payload {
                    out.push(ChatEffect::Db(DbOp::SavePeers(peers.clone())));
                    let q = self.picker.as_ref().map(|p| p.query.trim().to_string()).unwrap_or_default();
                    out.push(ChatEffect::Db(DbOp::SearchPeers { query: q }));
                }
            }
            Pending::Other => {}
        }
    }
}

/// Первая строка сообщения для цитаты (или вид вложения словом).
pub fn snippet(m: &Message) -> String {
    let line = m.text.lines().next().unwrap_or("").trim();
    if !line.is_empty() {
        return line.chars().take(80).collect();
    }
    m.media.clone()
}

/// Ссылка на сообщение (Чт-9): публичные — t.me/имя/id, каналы и супергруппы —
/// t.me/c/id/сообщение; у личных и обычных групп ссылок нет.
pub fn message_link(p: &Peer, id: i32) -> Option<String> {
    if id <= 0 {
        return None;
    }
    if !p.username.is_empty() && p.kind != crate::model::PeerKind::User {
        return Some(format!("https://t.me/{}/{id}", p.username));
    }
    if p.kind == crate::model::PeerKind::Channel {
        return Some(format!("https://t.me/c/{}/{id}", -p.id - crate::model::CHANNEL_SHIFT));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DomainUpdate, ErrorInfo, ErrorKind, PeerKind};

    fn row(id: i64, unread: i32, top: i32, photo: i64) -> DialogRow {
        DialogRow {
            dialog: Dialog { peer_id: id, top_message: top, unread_count: unread, ..Default::default() },
            peer: Peer { id, kind: PeerKind::of(id), name: format!("p{id}"), access_hash: id * 3, photo_id: photo,
                photo_dc: 2, ..Default::default() },
            last: None,
            top_date: 0,
            avatar_path: String::new(),
        }
    }

    fn sends(fx: &[ChatEffect]) -> Vec<(u64, Command)> {
        fx.iter()
            .filter_map(|e| match e {
                ChatEffect::Send { id, cmd } => Some((*id, cmd.clone())),
                _ => None,
            })
            .collect()
    }

    fn ok(id: u64, payload: Payload) -> Event {
        Event::Result(CommandResult { cmd_id: id, error: None, payload })
    }

    #[test]
    fn dialogs_paging_until_complete_or_limit() {
        let mut m = ChatModel::new(1, "ru", 0);
        let fx = m.enter();
        assert_eq!(fx[0], ChatEffect::Db(DbOp::ReloadRows), "сначала сохранённое из базы");
        let (mut id, _) = sends(&fx)[0].clone();
        for page in 0..DIALOG_PAGES {
            let d = Dialog { peer_id: 100 + i64::from(page), top_message: 7, ..Default::default() };
            let msg = Message { peer_id: d.peer_id, id: 7, date: 1000 - i64::from(page), ..Default::default() };
            let peer = Peer { id: d.peer_id, access_hash: 5, ..Default::default() };
            let fx = m.event(&ok(id, Payload::Dialogs { dialogs: vec![d], messages: vec![msg], peers: vec![peer],
                total: 1000, complete: false }));
            assert!(matches!(fx[0], ChatEffect::Db(DbOp::SaveDialogs { .. })));
            let next = sends(&fx);
            if page + 1 < DIALOG_PAGES {
                match &next[0].1 {
                    Command::LoadDialogs { offset_date, offset_id, offset_peer, .. } => {
                        assert_eq!((*offset_date, *offset_id), (1000 - i64::from(page), 7));
                        assert_eq!(offset_peer.unwrap(), PeerRef { id: 100 + i64::from(page), access_hash: 5 });
                    }
                    c => panic!("{c:?}"),
                }
                id = next[0].0;
            } else {
                assert!(next.is_empty(), "страниц больше предела");
            }
        }
    }

    #[test]
    fn open_chat_loads_history_and_marks_read() {
        let mut m = ChatModel::new(1, "ru", 0);
        m.set_rows(vec![row(10, 3, 55, 0), row(20, 0, 9, 0)]);
        let fx = m.intent(ChatIntent::OpenChat(10));
        assert_eq!(fx[0], ChatEffect::Db(DbOp::ReloadHistory { peer_id: 10, limit: 50 }));
        let s = sends(&fx);
        assert_eq!(s[0].1, Command::LoadHistory { peer: PeerRef { id: 10, access_hash: 30 }, offset_id: 0, limit: 50 });
        assert_eq!(s[1].1, Command::LoadPinned { peer: PeerRef { id: 10, access_hash: 30 } });
        assert_eq!(s[2].1, Command::MarkRead { peer: PeerRef { id: 10, access_hash: 30 }, max_id: 55 });
        assert!(fx.contains(&ChatEffect::Db(DbOp::MarkReadLocal { peer_id: 10, max_id: 55 })));
        // Прочитанный чат не шлёт MarkRead.
        assert_eq!(sends(&m.intent(ChatIntent::OpenChat(20))).len(), 2);
    }

    #[test]
    fn older_history_and_completion() {
        let mut m = ChatModel::new(1, "ru", 0);
        m.set_rows(vec![row(10, 0, 100, 0)]);
        let hid = sends(&m.intent(ChatIntent::OpenChat(10)))[0].0;
        let page: Vec<Message> = (51..=100).map(|i| Message { peer_id: 10, id: i, ..Default::default() }).collect();
        m.event(&ok(hid, Payload::History { messages: page.clone(), peers: vec![], total: 70 }));
        m.set_history(10, page, HashMap::new());
        let fx = m.intent(ChatIntent::LoadOlder);
        let (id, cmd) = sends(&fx)[0].clone();
        assert_eq!(cmd, Command::LoadHistory { peer: PeerRef { id: 10, access_hash: 30 }, offset_id: 51, limit: 50 });
        assert!(sends(&m.intent(ChatIntent::LoadOlder)).is_empty(), "пока грузится — без повторов");
        let older: Vec<Message> = (31..=50).map(|i| Message { peer_id: 10, id: i, ..Default::default() }).collect();
        let fx = m.event(&ok(id, Payload::History { messages: older, peers: vec![], total: 70 }));
        assert!(fx.contains(&ChatEffect::Db(DbOp::ReloadHistory { peer_id: 10, limit: 100 })));
        assert!(m.history_complete);
        assert!(sends(&m.intent(ChatIntent::LoadOlder)).is_empty());
    }

    #[test]
    fn send_goes_through_outbox() {
        use crate::outbox::OutboxItem;
        let mut m = ChatModel::new(1, "ru", 100);
        m.set_rows(vec![row(10, 0, 1, 0)]);
        m.intent(ChatIntent::OpenChat(10));
        m.intent(ChatIntent::ComposerEdited("   ".into()));
        assert!(m.intent(ChatIntent::Send).is_empty(), "пустое не отправляется");
        m.intent(ChatIntent::ComposerEdited(" привет ".into()));
        let fx = m.intent(ChatIntent::Send);
        let Some(ChatEffect::Db(DbOp::Enqueue { peer_id: 10, text, random_id: r1, .. })) = fx.first() else {
            panic!("{fx:?}")
        };
        assert_eq!(text, "привет");
        assert!(m.composer.is_empty(), "поле очищается сразу");
        assert!(fx.contains(&ChatEffect::Db(DbOp::PumpOutbox)));
        m.intent(ChatIntent::ComposerEdited("ещё".into()));
        let Some(ChatEffect::Db(DbOp::Enqueue { random_id: r2, .. })) = m.intent(ChatIntent::Send).first().cloned() else {
            panic!()
        };
        assert_ne!(*r1, r2);

        // Очередь: отправка готовых, повтор одного и того же не дублируется.
        let item = |local_id: i64| OutboxItem { local_id, peer_id: 10, access_hash: 30, random_id: 5, text: "привет".into(), reply_to: 0 };
        let fx = m.set_outbox_ready(vec![item(1)]);
        assert_eq!(fx[0], ChatEffect::Db(DbOp::OutboxSending { local_id: 1 }));
        let (id, cmd) = sends(&fx)[0].clone();
        assert_eq!(cmd, Command::SendMessageReply { peer: PeerRef { id: 10, access_hash: 30 }, text: "привет".into(),
            random_id: 5, reply_to: 0 });
        assert!(sends(&m.set_outbox_ready(vec![item(1)])).is_empty(), "уже отправляется");

        let err = |id: u64, kind: ErrorKind, code: i32, text: &str| {
            Event::Result(CommandResult { cmd_id: id, error: Some(ErrorInfo { kind, code, text: text.into(), retry_after: 7 }),
                payload: Payload::None })
        };
        // Сеть — снова в очередь.
        let fx = m.event(&err(id, ErrorKind::Network, 0, "нет сети"));
        assert!(fx.contains(&ChatEffect::Db(DbOp::OutboxRequeue { local_id: 1, next_try: 105 })));
        // FLOOD_WAIT — через retry_after.
        let (id, _) = sends(&m.set_outbox_ready(vec![item(1)]))[0].clone();
        assert!(m.event(&err(id, ErrorKind::Flood, 420, "FLOOD_WAIT")).contains(&ChatEffect::Db(DbOp::OutboxRequeue { local_id: 1, next_try: 107 })));
        // Постоянная ошибка — «не отправлено».
        let (id, _) = sends(&m.set_outbox_ready(vec![item(1)]))[0].clone();
        assert!(m.event(&err(id, ErrorKind::Rpc, 403, "CHAT_WRITE_FORBIDDEN")).contains(&ChatEffect::Db(DbOp::OutboxFail {
            local_id: 1, error: "CHAT_WRITE_FORBIDDEN".into() })));
        // Дубль random_id — первая попытка дошла.
        let (id, _) = sends(&m.set_outbox_ready(vec![item(1)]))[0].clone();
        assert!(m.event(&err(id, ErrorKind::Rpc, 400, "RANDOM_ID_DUPLICATE")).contains(&ChatEffect::Db(DbOp::OutboxDone { local_id: 1, sent: None })));
        // Успех.
        let (id, _) = sends(&m.set_outbox_ready(vec![item(2)]))[0].clone();
        let sent = Message { peer_id: 10, id: 9, out: true, ..Default::default() };
        assert!(m.event(&ok(id, Payload::Sent(sent.clone()))).contains(&ChatEffect::Db(DbOp::OutboxDone { local_id: 2, sent: Some(sent) })));
        // Подключение и таймер будят очередь.
        assert!(m.event(&Event::Connection(crate::model::ConnState::Connected)).contains(&ChatEffect::Db(DbOp::PumpOutbox)));
        assert!(m.intent(ChatIntent::Tick(200)).contains(&ChatEffect::Db(DbOp::PumpOutbox)));
    }

    #[test]
    fn updates_apply_and_mark_open_chat_read() {
        let mut m = ChatModel::new(1, "ru", 0);
        m.set_rows(vec![row(10, 0, 1, 0)]);
        m.intent(ChatIntent::OpenChat(10));
        let b = UpdatesBatch {
            updates: vec![DomainUpdate::NewMessage(Message { peer_id: 10, id: 2, ..Default::default() }),
                DomainUpdate::NewMessage(Message { peer_id: 10, id: 3, ..Default::default() }),
                DomainUpdate::NewMessage(Message { peer_id: 99, id: 4, ..Default::default() })],
            ..Default::default()
        };
        let fx = m.event(&Event::Updates(b.clone()));
        assert_eq!(fx[0], ChatEffect::Db(DbOp::ApplyBatch(b)), "сначала запись пачки с состоянием");
        assert_eq!(sends(&fx)[0].1, Command::MarkRead { peer: PeerRef { id: 10, access_hash: 30 }, max_id: 3 });
        let reset = UpdatesBatch { updates: vec![DomainUpdate::Reset], ..Default::default() };
        assert!(matches!(sends(&m.event(&Event::Updates(reset)))[0].1, Command::LoadDialogs { .. }));
    }

    #[test]
    fn avatars_limited_inflight_and_not_repeated() {
        let mut m = ChatModel::new(1, "ru", 0);
        let rows: Vec<_> = (1..=10).map(|i| row(i, 0, 1, 500 + i)).collect();
        let fx = m.set_rows(rows.clone());
        let s = sends(&fx);
        assert_eq!(s.len(), AVATARS_INFLIGHT);
        assert!(sends(&m.set_rows(rows.clone())).is_empty(), "повторный список не дублирует загрузки");
        let fx = m.event(&ok(s[0].0, Payload::File("/tmp/a.jpg".into())));
        assert!(fx.contains(&ChatEffect::Db(DbOp::SetAvatar { peer_id: 1, path: "/tmp/a.jpg".into() })));
        assert_eq!(sends(&fx).len(), 1, "освободившееся место — следующий аватар");
    }

    fn chat_with_history() -> ChatModel {
        let mut m = ChatModel::new(1, "ru", 100);
        m.set_rows(vec![row(10, 0, 3, 0), row(20, 0, 1, 0), row(30, 0, 1, 0)]);
        m.intent(ChatIntent::OpenChat(10));
        let msg = |id: i32, out: bool, text: &str| Message { peer_id: 10, id, out, from_id: if out { 1 } else { 10 },
            text: text.into(), ..Default::default() };
        m.set_history(10, vec![msg(1, false, "вопрос\nвторая строка"), msg(2, true, "мой ответ"), msg(3, false, "ещё")],
            HashMap::new());
        m
    }

    #[test]
    fn reply_and_edit_modes() {
        let mut m = chat_with_history();
        m.intent(ChatIntent::Reply(1));
        assert_eq!(m.compose, Compose::Reply { id: 1, author: "p10".into(), snippet: "вопрос".into() });
        m.intent(ChatIntent::ComposerEdited("ок".into()));
        let fx = m.intent(ChatIntent::Send);
        assert!(matches!(&fx[0], ChatEffect::Db(DbOp::Enqueue { reply_to: 1, .. })), "ответ через очередь с reply_to");
        assert_eq!(m.compose, Compose::Plain);
        // Правка только своего: чужое не берётся.
        m.intent(ChatIntent::Edit(1));
        assert_eq!(m.compose, Compose::Plain);
        m.intent(ChatIntent::Edit(2));
        assert_eq!(m.composer, "мой ответ");
        m.intent(ChatIntent::ComposerEdited("исправлено".into()));
        let (_, cmd) = sends(&m.intent(ChatIntent::Send))[0].clone();
        assert_eq!(cmd, Command::EditMessage { peer: PeerRef { id: 10, access_hash: 30 }, id: 2, text: "исправлено".into() });
        m.intent(ChatIntent::Edit(2));
        m.intent(ChatIntent::CancelCompose);
        assert!(m.composer.is_empty(), "отмена правки очищает поле");
    }

    #[test]
    fn forward_to_several_and_new_chat() {
        let mut m = chat_with_history();
        let fx = m.intent(ChatIntent::Forward(vec![1, 3]));
        assert!(fx.contains(&ChatEffect::Db(DbOp::SearchPeers { query: String::new() })));
        assert_eq!(sends(&fx)[0].1, Command::GetContacts);
        let p = |id: i64| Peer { id, name: format!("p{id}"), access_hash: id * 3, ..Default::default() };
        m.set_picker_results(vec![p(20), p(30), p(40), p(20)]);
        assert_eq!(m.picker.as_ref().unwrap().results.len(), 3, "без повторов");
        m.intent(ChatIntent::PickerToggle(20));
        m.intent(ChatIntent::PickerToggle(40));
        m.intent(ChatIntent::PickerToggle(30));
        m.intent(ChatIntent::PickerToggle(30)); // снять
        let (_, cmd) = sends(&m.intent(ChatIntent::PickerConfirm))[0].clone();
        let Command::Forward { from, ids, targets } = cmd else { panic!() };
        assert_eq!((from.id, ids), (10, vec![1, 3]));
        assert_eq!(targets.iter().map(|t| t.0.id).collect::<Vec<_>>(), [20, 40]);
        assert!(targets.iter().all(|t| t.1.len() == 2), "random_id на каждое сообщение");
        assert_ne!(targets[0].1, targets[1].1);
        assert!(m.picker.is_none());
        // Новый чат с собеседником вне списка — открывается.
        m.intent(ChatIntent::NewChat);
        m.set_picker_results(vec![p(99)]);
        m.intent(ChatIntent::PickerToggle(99));
        let fx = m.intent(ChatIntent::PickerConfirm);
        assert_eq!(m.open, Some(99));
        assert_eq!(m.open_peer().unwrap().access_hash, 297);
        assert!(sends(&fx).iter().any(|(_, c)| matches!(c, Command::LoadHistory { peer, .. } if peer.id == 99)));
    }

    #[test]
    fn delete_pin_copy() {
        let mut m = chat_with_history();
        m.intent(ChatIntent::AskDelete(vec![2]));
        assert_eq!(m.delete_ask, Some(DeleteAsk { ids: vec![2], can_revoke: true, revoke: true }));
        m.intent(ChatIntent::ToggleRevoke);
        let (_, cmd) = sends(&m.intent(ChatIntent::ConfirmDelete))[0].clone();
        assert_eq!(cmd, Command::DeleteMessages { peer: PeerRef { id: 10, access_hash: 30 }, ids: vec![2], revoke: false });
        assert_eq!(sends(&m.intent(ChatIntent::Pin(3)))[0].1, Command::PinMessage { peer: PeerRef { id: 10, access_hash: 30 },
            id: 3, unpin: false });
        let fx = m.intent(ChatIntent::CopyText(1));
        assert_eq!(fx, vec![ChatEffect::Clipboard("вопрос\nвторая строка".into())]);
        assert_eq!(m.toast.as_ref().unwrap().0, "Скопировано");
        m.intent(ChatIntent::Tick(200));
        assert!(m.toast.is_none(), "всплывающее исчезает само");
        // Ссылки: публичный канал, частный канал, личный чат — нет.
        let ch = Peer { id: -1_000_000_000_555, kind: crate::model::PeerKind::Channel, ..Default::default() };
        assert_eq!(message_link(&ch, 7).as_deref(), Some("https://t.me/c/555/7"));
        let public = Peer { username: "news".into(), ..ch.clone() };
        assert_eq!(message_link(&public, 7).as_deref(), Some("https://t.me/news/7"));
        assert_eq!(message_link(&Peer { id: 5, ..Default::default() }, 7), None);
    }

    #[test]
    fn quotes_loaded_once() {
        let mut m = chat_with_history();
        m.history.push(Message { peer_id: 10, id: 4, reply_to: 77, ..Default::default() });
        let fx = m.set_quotes(10, HashMap::new());
        assert_eq!(sends(&fx)[0].1, Command::LoadMessages { peer: PeerRef { id: 10, access_hash: 30 }, ids: vec![77] });
        assert!(sends(&m.set_quotes(10, HashMap::new())).is_empty(), "пока грузится — без повторов");
    }
}
