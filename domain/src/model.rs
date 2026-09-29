//! Сущности домена для стыка с сетью. Отдельны от типов protobuf
//! (Архитектура/nuncio - Приложение (Rust и Slint).md, раздел 1): `bridge`
//! отображает их в `client.proto` и обратно.

/// Прокси (Вх-4, настройки соединения).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProxySettings {
    pub kind: ProxyKind,
    #[serde(default)]
    pub addr: String,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub secret: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyKind {
    #[default]
    None,
    Socks5,
    MtProxy,
}

/// Ссылка на собеседника для запросов. `id` «помеченный», как в Bot API:
/// пользователь +id, группа -id, канал -(1000000000000 + id).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct PeerRef {
    pub id: i64,
    pub access_hash: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PeerKind {
    #[default]
    User,
    Chat,
    Channel,
}

/// Сдвиг помеченных id каналов.
pub const CHANNEL_SHIFT: i64 = 1_000_000_000_000;

impl PeerKind {
    pub fn of(id: i64) -> Self {
        if id > 0 {
            Self::User
        } else if id < -CHANNEL_SHIFT {
            Self::Channel
        } else {
            Self::Chat
        }
    }
}

/// Собеседник: пользователь, группа или канал.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Peer {
    pub id: i64,
    pub kind: PeerKind,
    pub name: String,
    pub username: String,
    pub access_hash: i64,
    pub photo_id: i64,
    pub photo_dc: i32,
    pub is_self: bool,
    pub is_bot: bool,
    pub broadcast: bool,
    /// Неполные сведения: `access_hash` не годится для запросов.
    pub min: bool,
    /// В контактах (правила папок «Контакты» и «Не контакты»).
    pub contact: bool,
}

impl Peer {
    pub fn peer_ref(&self) -> PeerRef {
        PeerRef { id: self.id, access_hash: self.access_hash }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Message {
    pub peer_id: i64,
    pub id: i32,
    pub date: i64,
    pub from_id: i64,
    pub out: bool,
    pub text: String,
    pub reply_to: i32,
    /// Вид вложения (`photo`, `document`, ...); пусто — только текст.
    pub media: String,
    /// Вид служебного действия; пусто — обычное сообщение.
    pub service: String,
    pub random_id: i64,
    pub edit_date: i64,
    /// Очередь отправки (О-9): 0 отправлено, 1 в очереди, 2 отправляется, 3 ошибка.
    pub send_state: i32,
    /// Причина ошибки отправки (`expired` — истёк срок, О-12).
    pub send_error: String,
    /// «Переслано от» (Чт-3): помеченный id или имя, если id скрыт.
    pub fwd_from_id: i64,
    pub fwd_from_name: String,
    pub pinned: bool,
    /// Форматирование (Дз-15), смещения в UTF-16.
    pub entities: Vec<Entity>,
}

/// Вид форматирования (как `MessageEntity` в client.proto).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum EntityKind {
    #[default]
    Other,
    Bold,
    Italic,
    Underline,
    Strike,
    Code,
    Pre,
    TextUrl,
    Url,
    Email,
    Mention,
    Hashtag,
    Spoiler,
    Blockquote,
}

impl EntityKind {
    pub const ALL: [EntityKind; 14] = [
        EntityKind::Other, EntityKind::Bold, EntityKind::Italic, EntityKind::Underline, EntityKind::Strike,
        EntityKind::Code, EntityKind::Pre, EntityKind::TextUrl, EntityKind::Url, EntityKind::Email,
        EntityKind::Mention, EntityKind::Hashtag, EntityKind::Spoiler, EntityKind::Blockquote,
    ];

    pub fn code(self) -> i32 {
        Self::ALL.iter().position(|k| *k == self).unwrap_or(0) as i32
    }

    pub fn from_code(c: i32) -> Self {
        Self::ALL.get(c as usize).copied().unwrap_or_default()
    }
}

/// Участок форматирования: смещение и длина в единицах UTF-16, как в Telegram.
#[derive(Debug, Clone, PartialEq, Eq, Default, Hash)]
pub struct Entity {
    pub kind: EntityKind,
    pub offset: i32,
    pub length: i32,
    pub url: String,
}

/// Сущности в строку для базы: по строке на сущность, `вид смещение длина url`
/// (url последним: в нём могут быть пробелы, но не переводы строк).
pub fn encode_entities(es: &[Entity]) -> String {
    es.iter()
        .map(|e| format!("{} {} {} {}", e.kind.code(), e.offset, e.length, e.url.replace('\n', "")))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn decode_entities(s: &str) -> Vec<Entity> {
    s.lines()
        .filter_map(|l| {
            let mut it = l.splitn(4, ' ');
            let kind = EntityKind::from_code(it.next()?.parse().ok()?);
            let offset = it.next()?.parse().ok()?;
            let length = it.next()?.parse().ok()?;
            Some(Entity { kind, offset, length, url: it.next().unwrap_or("").to_string() })
        })
        .collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dialog {
    pub peer_id: i64,
    pub top_message: i32,
    pub unread_count: i32,
    pub pinned: bool,
    pub folder: i32,
    pub read_inbox_max_id: i32,
    pub read_outbox_max_id: i32,
    /// Уведомления выключены.
    pub muted: bool,
    /// Помечен непрочитанным вручную.
    pub unread_mark: bool,
}

/// Папка (dialogFilter): правила состава над диалогами, id — помеченные.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Folder {
    pub id: i32,
    pub title: String,
    pub emoticon: String,
    /// Папка по ссылке: только явно перечисленные чаты.
    pub chatlist: bool,
    pub contacts: bool,
    pub non_contacts: bool,
    pub groups: bool,
    pub broadcasts: bool,
    pub bots: bool,
    pub exclude_muted: bool,
    pub exclude_read: bool,
    pub exclude_archived: bool,
    pub pinned: Vec<i64>,
    pub include: Vec<i64>,
    pub exclude: Vec<i64>,
}

/// Сведения о чате для правой панели (Св-1).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChatInfo {
    pub peer_id: i64,
    pub about: String,
    /// Участников или подписчиков (0 — неизвестно).
    pub members: i32,
    /// Участник и роль: 0 участник, 1 администратор, 2 создатель.
    pub participants: Vec<(i64, i32)>,
    pub peers: Vec<Peer>,
    pub blocked: bool,
}

/// Фильтр поиска по чату (Дз-18).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchFilter {
    #[default]
    All,
    Media,
    Files,
    Links,
    Voice,
}

/// Действие над чатом (Сп-4, Св-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatActionKind {
    Mute,
    Unmute,
    Archive,
    Unarchive,
    Pin,
    Unpin,
    MarkUnread,
    ClearHistory,
    DeleteChat,
    Leave,
    Block,
    Unblock,
}

/// Состояние обновлений (О-5): пишется одной транзакцией с событиями.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UpdateState {
    pub pts: i32,
    pub qts: i32,
    pub seq: i32,
    pub date: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainUpdate {
    NewMessage(Message),
    EditMessage(Message),
    /// `peer_id` 0 — личные и группы (id сообщений общие для аккаунта).
    DeleteMessages { peer_id: i64, ids: Vec<i32> },
    ReadInbox { peer_id: i64, max_id: i32, still_unread: i32 },
    ReadOutbox { peer_id: i64, max_id: i32 },
    /// Разница слишком велика: перечитать диалоги.
    Reset,
    /// Разница канала слишком велика: его история устарела.
    ResetChannel { peer_id: i64 },
    /// Закреплено или откреплено (Чт-14).
    Pinned { peer_id: i64, ids: Vec<i32>, pinned: bool },
    /// Папки изменены на сервере: перечитать список папок.
    FoldersChanged,
    /// Чат перенесён в архив (1) или из архива (0).
    PeerFolder { peer_id: i64, folder: i32 },
    /// Настройки чата; None — не менялось.
    PeerSettings { peer_id: i64, muted: Option<bool>, pinned: Option<bool>, unread_mark: Option<bool> },
    /// Порядок закреплённых чатов изменился: перечитать диалоги.
    PinnedDialogs,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdatesBatch {
    pub updates: Vec<DomainUpdate>,
    pub peers: Vec<Peer>,
    pub state: Option<UpdateState>,
    /// pts каналов (помеченный id → pts) — в той же транзакции.
    pub channel_states: Vec<(i64, i32)>,
}

/// Команда сетевому слою.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Ping(String),
    RequestQr,
    SendCode { phone: String },
    ResendCode,
    CancelCode,
    SignIn { code: String },
    CheckPassword { password: String },
    SignUp { first_name: String, last_name: String },
    LogOut,
    SetProxy(ProxySettings),
    TestProxy(ProxySettings),
    GetCountries { lang: String },
    RetryStartup,
    LoadDialogs { limit: i32, offset_date: i64, offset_id: i32, offset_peer: Option<PeerRef> },
    /// `add_offset = -k`: и k сообщений с id не меньше `offset_id` (подгрузка вниз, окно вокруг).
    LoadHistory { peer: PeerRef, offset_id: i32, add_offset: i32, limit: i32 },
    SendMessage { peer: PeerRef, text: String, random_id: i64 },
    MarkRead { peer: PeerRef, max_id: i32 },
    DownloadAvatar { peer: PeerRef, photo_id: i64, dc: i32 },
    SendMessageReply { peer: PeerRef, text: String, random_id: i64, reply_to: i32, entities: Vec<Entity> },
    EditMessage { peer: PeerRef, id: i32, text: String, entities: Vec<Entity> },
    DeleteMessages { peer: PeerRef, ids: Vec<i32>, revoke: bool },
    /// Пересылка нескольким адресатам (Дз-16): random_id на каждое сообщение и адресата.
    Forward { from: PeerRef, ids: Vec<i32>, targets: Vec<(PeerRef, Vec<i64>)> },
    PinMessage { peer: PeerRef, id: i32, unpin: bool },
    LoadPinned { peer: PeerRef },
    LoadMessages { peer: PeerRef, ids: Vec<i32> },
    GetContacts,
    SearchPeers { query: String },
    LoadFolders,
    /// Поиск сообщений: `peer` None — по всем чатам на сервере.
    SearchMessages {
        peer: Option<PeerRef>,
        query: String,
        filter: SearchFilter,
        min_date: i64,
        max_date: i64,
        from: Option<PeerRef>,
        offset_id: i32,
        limit: i32,
        offset_rate: i32,
        offset_peer: Option<PeerRef>,
    },
    GetChatInfo { peer: PeerRef, members_query: String },
    ChatAction { peer: PeerRef, kind: ChatActionKind, revoke: bool },
}

/// Шаг входа.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthStep {
    #[default]
    Unknown,
    Qr,
    Code,
    Password,
    SignUp,
    Ready,
    LoggedOut,
}

/// Куда и какой код отправлен.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodeInfo {
    pub kind: String,
    pub length: u32,
    pub next_kind: String,
    pub timeout: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct User {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
    pub username: String,
}

impl User {
    pub fn display_name(&self) -> String {
        let n = format!("{} {}", self.first_name, self.last_name);
        n.trim().to_string()
    }
}

/// Состояние входа от сети.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthState {
    pub step: AuthStep,
    pub qr_url: String,
    pub qr_expires_unix: i64,
    pub code: CodeInfo,
    pub password_hint: String,
    pub has_recovery: bool,
    pub terms: String,
    pub user: Option<User>,
    /// LoggedOut: `logged_out`, `session_ended`, `keyring_no_key`, `session_broken` или пусто.
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Connecting,
    Connected,
    WaitingNetwork,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupErrorKind {
    KeyringUnavailable,
    KeyringDismissed,
    Config,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Network,
    Rpc,
    Flood,
    Auth,
    Cancelled,
    Internal,
}

/// Единая ошибка стыка: реакция выбирается по `kind`, а не по строке.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorInfo {
    pub kind: ErrorKind,
    pub code: i32,
    pub text: String,
    pub retry_after: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CountryCode {
    pub code: String,
    pub prefixes: Vec<String>,
    pub patterns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Country {
    pub iso2: String,
    pub name: String,
    pub default_name: String,
    pub codes: Vec<CountryCode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    None,
    Pong { message: String, core_version: String },
    Countries(Vec<Country>),
    ProxyCheck { rtt_ms: u32 },
    Dialogs { dialogs: Vec<Dialog>, messages: Vec<Message>, peers: Vec<Peer>, total: i32, complete: bool },
    History { messages: Vec<Message>, peers: Vec<Peer>, total: i32, next_rate: i32 },
    Sent(Message),
    File(String),
    Peers(Vec<Peer>),
    Folders(Vec<Folder>),
    ChatInfo(ChatInfo),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub cmd_id: u64,
    pub error: Option<ErrorInfo>,
    pub payload: Payload,
}

/// Событие от сети.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Result(CommandResult),
    Connection(ConnState),
    Auth(AuthState),
    Updates(UpdatesBatch),
    Startup { kind: StartupErrorKind, text: String },
    Fatal(String),
}
