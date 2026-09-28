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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Dialogs,
    History { peer_id: i64, older: bool },
    Outbox { local_id: i64 },
    Avatar { peer_id: i64 },
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
        self.rows.iter().find(|r| r.peer.id == peer_id).map(|r| r.peer.peer_ref())
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
                out.push(ChatEffect::Db(DbOp::PumpOutbox));
            }
            ChatIntent::OpenChat(peer_id) => {
                if self.open == Some(peer_id) {
                    return out;
                }
                self.open = Some(peer_id);
                self.history.clear();
                self.history_complete = false;
                self.history_limit = HISTORY_PAGE as usize;
                self.composer.clear();
                self.error = None;
                out.push(ChatEffect::Db(DbOp::ReloadHistory { peer_id, limit: self.history_limit }));
                if let Some(peer) = self.peer_ref(peer_id) {
                    self.history_loading = true;
                    self.send(Command::LoadHistory { peer, offset_id: 0, limit: HISTORY_PAGE },
                        Pending::History { peer_id, older: false }, &mut out);
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
                let random_id = self.random_id();
                out.push(ChatEffect::Db(DbOp::Enqueue { peer_id, text, reply_to: 0, random_id }));
                out.push(ChatEffect::Db(DbOp::ReloadHistory { peer_id, limit: self.history_limit }));
                out.push(ChatEffect::Db(DbOp::ReloadRows));
                out.push(ChatEffect::Db(DbOp::PumpOutbox));
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
            self.send(Command::SendMessage { peer, text: it.text, random_id: it.random_id },
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
            Pending::Other => {}
        }
    }
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
        assert_eq!(s[1].1, Command::MarkRead { peer: PeerRef { id: 10, access_hash: 30 }, max_id: 55 });
        assert!(fx.contains(&ChatEffect::Db(DbOp::MarkReadLocal { peer_id: 10, max_id: 55 })));
        // Прочитанный чат не шлёт MarkRead.
        assert_eq!(sends(&m.intent(ChatIntent::OpenChat(20))).len(), 1);
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
        assert_eq!(cmd, Command::SendMessage { peer: PeerRef { id: 10, access_hash: 30 }, text: "привет".into(), random_id: 5 });
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
}
