//! Модель главного окна вертикали (В4): список диалогов и открытый чат.
//! Чистая: намерения и события превращаются в эффекты — команды сети и
//! операции с базой, которые выполняет поток домена; результаты базы
//! возвращаются методами `set_rows` и `set_history`.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::chatstore::DialogRow;
use crate::history::{Anchor, Span, Window};
use crate::model::{
    Command, CommandResult, Dialog, Entity, Event, Message, Payload, Peer, PeerRef, UpdatesBatch,
};

/// Сколько диалогов держать в списке (окно модели; остальные — из базы по требованию, В5).
pub const DIALOGS_WINDOW: usize = 500;
/// Страница истории с сервера.
pub const HISTORY_PAGE: i32 = 50;
/// Окно истории в памяти: последние `LATEST_WINDOW` или по `WINDOW_HALF`
/// с каждой стороны от якоря (Приложение, раздел «Окно истории»).
pub const LATEST_WINDOW: usize = 100;
pub const WINDOW_HALF: usize = 50;
/// Страниц диалогов при старте (по 100): не упираться в ограничитель частоты.
pub const DIALOG_PAGES: u32 = 5;
/// Одновременных загрузок аватаров.
pub const AVATARS_INFLIGHT: usize = 3;

/// Операция с базой (выполняет поток домена в потоке БД).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbOp {
    SaveDialogs { dialogs: Vec<Dialog>, messages: Vec<Message>, peers: Vec<Peer> },
    /// `span` — непрерывный участок страницы истории (у цитат и закреплённых нет).
    SaveHistory { messages: Vec<Message>, peers: Vec<Peer>, span: Option<Span> },
    ApplyBatch(UpdatesBatch),
    SetAvatar { peer_id: i64, path: String },
    MarkReadLocal { peer_id: i64, max_id: i32 },
    /// Перечитать список (результат — `set_rows`).
    ReloadRows,
    /// Перечитать окно истории открытого чата (результат — `set_history`).
    ReloadHistory { peer_id: i64, anchor: Anchor, before: usize, after: usize },
    /// Очередь отправки (О-9).
    Enqueue { peer_id: i64, text: String, entities: Vec<Entity>, reply_to: i32, random_id: i64 },
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
    /// Папки (В6): сохранить с сервера; перечитать (результат — `set_folders`).
    SaveFolders(Vec<crate::model::Folder>),
    ReloadFolders,
    /// Поиск по базе (результат — `set_local_search`).
    SearchLocal { query: String },
    /// Настройки диалога по действию (как пришли бы обновлением).
    ApplyLocal(Vec<crate::model::DomainUpdate>),
    /// История очищена или чат удалён: убрать из базы (dialog — и сам диалог).
    DropChat { peer_id: i64, dialog: bool },
    /// Вложения: что из нужного окну уже в кэше (результат — `set_media_paths`); скачано.
    MediaLookup { keys: Vec<String> },
    MediaSaved { key: String, path: String, size: i64 },
    /// Вложения в очередь отправки (одно или альбом с общим `group`).
    EnqueueMedia { peer_id: i64, items: Vec<crate::attach::EnqueueItem>, reply_to: i32, group: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEffect {
    Send { id: u64, cmd: Command },
    Db(DbOp),
    /// Скопировать в буфер обмена (Чт-9).
    Clipboard(String),
    /// Открыть скачанный файл во внешней программе.
    OpenFile(String),
    /// Осмотреть выбранные файлы (размер, картинка ли) — затем `set_prepared`.
    InspectFiles { paths: Vec<String>, append: bool },
    /// Очистить кэш файлов (временный пункт меню до настроек В9).
    ClearCache,
    /// Взять картинку из буфера обмена (если там нет текста) в панель подготовки.
    PasteImage,
    /// Настройки (В9): сохранить и применить значения приложения.
    SettingsChanged(crate::settings::AppSettings),
    /// Сменить язык интерфейса (сохранить и применить).
    SetLanguage(String),
    /// Прокси сохранён в окне настроек — в config.toml.
    SaveProxy(crate::model::ProxySettings),
    /// Измерить кэш и базу для раздела «Данные и кэш».
    MeasureStorage,
    /// Показать уведомление о сообщении (В9, D-Bus): одно на чат.
    Notify { peer_id: i64, title: String, body: String, sound: bool },
    /// Показать окно (щелчок по уведомлению или значку в трее).
    RaiseWindow,
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

/// Куда поставить вид истории, когда окно будет готово (выполняет интерфейс).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jump {
    /// Сообщение (или первое после него) — вверху видимой области.
    To(i32),
    /// Конец истории.
    Bottom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatIntent {
    OpenChat(i64),
    CloseChat,
    ComposerEdited(String),
    /// Отправить текст поля с форматированием (Дз-15), смещения в UTF-16.
    Send(Vec<Entity>),
    /// Вид дошёл до края окна: сдвинуть окно (или подгрузить с сервера).
    NeedOlder,
    NeedNewer,
    /// Переход к сообщению сменой окна модели (ответ, закреплённое, Чт-3, Чт-14).
    JumpTo(i32),
    /// Кнопка «вниз» (Чт-10).
    JumpBottom,
    /// Полоса прокрутки: доля от первого сообщения чата до последнего, в тысячных.
    JumpPermille(i32),
    /// Самое новое видимое сообщение: «прочитано» по мере просмотра.
    Seen(i32),
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
    /// Ширина ленты истории, px: по ней раскладывается текст с эмодзи (О-14).
    ViewWidth(i32),
    /// Папки, поиск, правая панель, действия над чатом (В6, chatlist.rs).
    List(crate::chatlist::ListIntent),
    /// Вложение: скачать, отменить, открыть (В7).
    MediaClick(i32),
    /// Голос в опросе: сообщение и вариант.
    Vote(i32, usize),
    /// Окно просмотра: соседнее фото (−1, +1) или закрыть.
    ViewerStep(i32),
    CloseViewer,
    /// Видимые строки истории: ключи верхней и нижней (интерфейс, при смене).
    Visible(i32, i32),
    /// Окно просмотра: сохранить, переслать, показать в чате.
    ViewerSave,
    ViewerForward,
    ViewerShowInChat,
    /// Отправка вложений (Св-4): скрепка, «+ ещё», «как фото», убрать файл, отмена.
    Attach,
    PrepareMore,
    PrepareAsPhoto(bool),
    PrepareRemove(usize),
    PrepareCancel,
    ClearCache,
    /// Ctrl+V в поле ввода: картинка из буфера — в панель подготовки (Св-4).
    PasteImage,
    /// Окно настроек (В9).
    Settings(crate::settings::SettingsIntent),
    /// Щелчок по уведомлению: открыть чат и показать окно.
    NotificationClicked(i64),
    /// Окно в фокусе или нет (свёрнуто, за другим, в трее): для открытого
    /// чата в неактивном окне уведомление тоже есть.
    WindowActive(bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pending {
    Dialogs,
    History { peer_id: i64, offset_id: i32, add_offset: i32, limit: i32 },
    Read { peer_id: i64 },
    Outbox { local_id: i64 },
    Avatar { peer_id: i64 },
    Action,
    Pinned { peer_id: i64 },
    Quotes { peer_id: i64 },
    PickerSearch,
    Contacts,
    Folders,
    GlobalSearch { query: String },
    ServerPeers { query: String },
    ChatInfo { peer_id: i64 },
    ChatSearch { peer_id: i64, seq: u64 },
    ChatAction { peer_id: i64, kind: crate::model::ChatActionKind },
    Download { key: String, size: i64 },
    /// Системный диалог выбора файлов; `append` — «+ ещё» к панели подготовки.
    Pick { append: bool },
    /// Окно настроек (В9).
    Profile,
    Sessions,
    ProxyCheck,
    PickTheme,
    PickDownloads,
    PickPhoto,
    ProfilePhoto,
    /// Вложения из очереди (одно или альбом).
    OutboxMedia { local_ids: Vec<i64> },
    /// Сохранение файла из окна просмотра.
    Save,
    /// Голос в опросе: после ответа — перечитать сообщение (результаты).
    Vote { peer: PeerRef, msg_id: i32 },
    Other,
}

#[derive(Debug, Clone, Default)]
pub struct ChatModel {
    /// Видимый список: по текущему фильтру (основной, архив, папка).
    pub rows: Vec<DialogRow>,
    /// Все диалоги из базы (основной список и архив): поиск собеседников, папки.
    pub all_rows: Vec<DialogRow>,
    /// Папки, фильтр списка, поиск, правая панель, подтверждение (В6, chatlist.rs).
    pub folders: Vec<crate::model::Folder>,
    pub filter: crate::chatlist::ListFilter,
    pub search: Option<crate::chatlist::GlobalSearch>,
    pub panel: Option<crate::chatlist::Panel>,
    pub info: Option<crate::model::ChatInfo>,
    pub chat_search: Option<crate::chatlist::ChatSearch>,
    pub confirm: Option<crate::chatlist::Confirm>,
    /// Вложения (В7, media.rs): скачивания в работе, скачанное окна, неудачи.
    pub transfers: HashMap<String, crate::media::Transfer>,
    pub media_paths: HashMap<String, String>,
    pub(crate) media_failed: HashSet<String>,
    /// Неполные сообщения (до подробных вложений или пустые), уже перечитанные
    /// с сервера в этом сеансе: повторно не просить.
    pub(crate) refetched: HashSet<(i64, i32)>,
    /// Окно просмотра фото (Св-3): сообщение и номер открытия.
    pub viewer: Option<i32>,
    pub viewer_seq: u64,
    /// Голосовое или аудио, которое попросили проиграть (В8), и номер просьбы:
    /// интерфейс запускает плеер, когда файл скачан.
    pub audio_play: Option<i32>,
    pub audio_seq: u64,
    /// Панель подготовки вложений (Св-4).
    pub prepare: Option<crate::attach::Prepare>,
    /// Окно настроек и значения приложения (В9; значения — из config.toml).
    pub settings: Option<crate::settings::SettingsView>,
    pub app_settings: crate::settings::AppSettings,
    pub proxy_settings: crate::model::ProxySettings,
    /// Окно в фокусе: для открытого чата уведомлений нет.
    pub window_active: bool,
    pub open: Option<i64>,
    /// Окно истории открытого чата, по возрастанию id (очередь отправки в конце).
    pub history: Vec<Message>,
    pub history_loading: bool,
    /// Где держится окно и что о нём известно (сообщения — в `history`).
    pub anchor: Anchor,
    pub window: Window,
    /// Растёт с каждым окном из базы: интерфейс просит подгрузку раз на окно.
    pub window_version: u64,
    /// Линия «Новые сообщения» (Чт-10): после этого id, пока чат открыт.
    pub unread_mark: Option<i32>,
    /// Переход для интерфейса и его номер (интерфейс выполняет каждый номер один
    /// раз). Появляется, когда окно для него уже прочитано из базы.
    pub jump: Option<(u64, Jump)>,
    jump_seq: u64,
    pending_jump: Option<Jump>,
    first_requested: bool,
    /// «Прочитано» до этого id отправлено; `seen` — что видно сейчас.
    read_sent: i32,
    seen: i32,
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
    /// Содержимое, которое модель кладёт в поле сама (правка, очистка после
    /// отправки), и его номер: интерфейс заменяет поле, когда номер меняется.
    pub composer_entities: Vec<Entity>,
    pub composer_seq: u64,
    pub sending: bool,
    pub error: Option<String>,
    pub dialogs_loading: bool,
    pub self_id: i64,
    pub now: i64,
    pub lang: String,
    /// Ширина ленты истории, px (0 — ещё неизвестна).
    pub view_width: i32,
    /// Видимые строки истории (ключи верхней и нижней): картинки вложений
    /// раскодируются только около них (память — по экрану, не по окну).
    pub visible: Option<(i32, i32)>,
    next_id: u64,
    pub(crate) pending: HashMap<u64, Pending>,
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
            window_active: true,
            ..Default::default()
        }
    }

    /// Id, который получит следующая команда.
    pub(crate) fn peek_id(&self) -> u64 {
        self.next_id
    }

    pub(crate) fn send(&mut self, cmd: Command, p: Pending, out: &mut Vec<ChatEffect>) {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, p);
        out.push(ChatEffect::Send { id, cmd });
    }

    /// Открытый собеседник.
    pub fn open_row(&self) -> Option<&DialogRow> {
        let id = self.open?;
        self.all_rows.iter().find(|r| r.peer.id == id)
    }

    pub(crate) fn peer_ref(&self, peer_id: i64) -> Option<PeerRef> {
        self.open_peer_of(peer_id).map(|p| p.peer_ref())
    }

    fn open_peer_of(&self, peer_id: i64) -> Option<&Peer> {
        self.all_rows.iter().find(|r| r.peer.id == peer_id).map(|r| &r.peer).or_else(|| self.extra_peers.get(&peer_id))
    }

    /// Собеседник открытого чата (в том числе ещё не в списке диалогов).
    pub fn open_peer(&self) -> Option<&Peer> {
        self.open_peer_of(self.open?)
    }

    pub fn toast(&mut self, text: &str) {
        self.toast = Some((text.to_string(), self.now + 3));
    }

    fn history_msg(&self, id: i32) -> Option<&Message> {
        self.history.iter().find(|m| m.id == id)
    }

    pub(crate) fn load_dialogs_page(&mut self, offset: Option<(i64, i32, PeerRef)>, out: &mut Vec<ChatEffect>) {
        self.dialogs_loading = true;
        let (offset_date, offset_id, offset_peer) = match offset {
            Some((d, i, p)) => (d, i, Some(p)),
            None => (0, 0, None),
        };
        self.send(Command::LoadDialogs { limit: 100, offset_date, offset_id, offset_peer }, Pending::Dialogs, out);
    }

    /// Вход выполнен: показать сохранённое и обновить с сервера.
    pub fn enter(&mut self) -> Vec<ChatEffect> {
        let mut out = vec![ChatEffect::Db(DbOp::ReloadRows), ChatEffect::Db(DbOp::ReloadFolders)];
        self.pages = 0;
        self.load_dialogs_page(None, &mut out);
        self.send(Command::LoadFolders, Pending::Folders, &mut out);
        out
    }

    /// Результат `ReloadRows`: список и очередь аватаров.
    pub fn set_rows(&mut self, rows: Vec<DialogRow>) -> Vec<ChatEffect> {
        self.all_rows = rows;
        self.apply_filter();
        for r in &self.all_rows {
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

    /// Результат `ReloadHistory`: окно из базы. Якоря нет в известных участках —
    /// страница вокруг него с сервера; окно дошло до настоящего — дальше держим
    /// последние сообщения.
    pub fn set_history(&mut self, peer_id: i64, mut w: Window, senders: HashMap<i64, String>) -> Vec<ChatEffect> {
        let mut out = Vec::new();
        if self.open != Some(peer_id) {
            return out;
        }
        self.history = std::mem::take(&mut w.messages);
        self.senders = senders;
        self.window = w;
        self.window_version += 1;
        if !self.window.found && !self.history_request_pending(peer_id) {
            match self.anchor {
                Anchor::Latest => self.load_page(peer_id, 0, 0, HISTORY_PAGE, &mut out),
                Anchor::At(x) => self.load_page(peer_id, x, -(WINDOW_HALF as i32), 2 * WINDOW_HALF as i32, &mut out),
            }
        }
        if matches!(self.anchor, Anchor::At(_)) && self.window.end {
            self.anchor = Anchor::Latest;
        }
        self.media_window_changed(&mut out);
        let ready = match self.pending_jump {
            Some(Jump::Bottom) => true,
            Some(Jump::To(_)) => self.window.found,
            None => false,
        };
        if ready {
            self.jump_seq += 1;
            self.jump = self.pending_jump.take().map(|j| (self.jump_seq, j));
        }
        out
    }

    fn history_request_pending(&self, peer_id: i64) -> bool {
        self.pending.values().any(|p| matches!(p, Pending::History { peer_id: x, .. } if *x == peer_id))
    }

    fn load_page(&mut self, peer_id: i64, offset_id: i32, add_offset: i32, limit: i32, out: &mut Vec<ChatEffect>) {
        let Some(peer) = self.peer_ref(peer_id) else { return };
        self.history_loading = true;
        self.send(Command::LoadHistory { peer, offset_id, add_offset, limit },
            Pending::History { peer_id, offset_id, add_offset, limit }, out);
    }

    fn set_jump(&mut self, j: Jump) {
        self.pending_jump = Some(j);
    }

    fn reload_at(&mut self, anchor: Anchor, out: &mut Vec<ChatEffect>) {
        self.anchor = anchor;
        self.reload_open(out);
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

    pub(crate) fn random_id(&mut self) -> i64 {
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
                self.fill_composer(String::new(), Vec::new());
                self.error = None;
                self.window = Window::default();
                self.first_requested = false;
                self.seen = 0;
                // Вход к первому непрочитанному с линией «Новые сообщения» (Чт-10).
                let (unread, read_in) = self.open_row().map(|r| (r.dialog.unread_count, r.dialog.read_inbox_max_id))
                    .unwrap_or((0, 0));
                self.read_sent = read_in;
                if unread > 0 {
                    self.unread_mark = Some(read_in);
                    self.anchor = Anchor::At(read_in + 1);
                    self.set_jump(Jump::To(read_in + 1));
                    self.load_page(peer_id, read_in + 1, -(WINDOW_HALF as i32), 2 * WINDOW_HALF as i32, &mut out);
                } else {
                    self.unread_mark = None;
                    self.anchor = Anchor::Latest;
                    self.set_jump(Jump::Bottom);
                    self.load_page(peer_id, 0, 0, HISTORY_PAGE, &mut out);
                }
                self.reload_open(&mut out);
                out.push(ChatEffect::Db(DbOp::ReloadPinned { peer_id }));
                if let Some(peer) = self.peer_ref(peer_id) {
                    self.send(Command::LoadPinned { peer }, Pending::Pinned { peer_id }, &mut out);
                }
            }
            ChatIntent::CloseChat => {
                self.open = None;
                self.history.clear();
            }
            ChatIntent::ComposerEdited(t) => self.composer = t,
            ChatIntent::Send(entities) => {
                // Сразу в очередь (О-9): сообщение видно в чате как «отправляется».
                let (text, entities) = trim_with_entities(&self.composer, entities);
                let Some(peer_id) = self.open else { return out };
                if self.prepare.is_some() {
                    // Панель подготовки открыта: текст поля — подпись к вложениям (Св-4).
                    self.fill_composer(String::new(), Vec::new());
                    let reply_to = match std::mem::take(&mut self.compose) {
                        Compose::Reply { id, .. } => id,
                        _ => 0,
                    };
                    return self.send_prepared(text, entities, reply_to);
                }
                if text.is_empty() {
                    return out;
                }
                self.fill_composer(String::new(), Vec::new());
                self.error = None;
                match std::mem::take(&mut self.compose) {
                    Compose::Edit { id, .. } => {
                        if let Some(peer) = self.peer_ref(peer_id) {
                            self.send(Command::EditMessage { peer, id, text, entities }, Pending::Action, &mut out);
                        }
                    }
                    Compose::Reply { id, .. } => out = self.enqueue_reply(peer_id, text, entities, id),
                    Compose::Plain => out = self.enqueue_reply(peer_id, text, entities, 0),
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
                    self.fill_composer(m.text, m.entities);
                }
            }
            ChatIntent::CancelCompose => {
                if matches!(self.compose, Compose::Edit { .. }) {
                    self.fill_composer(String::new(), Vec::new());
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
                    if !self.all_rows.iter().any(|r| r.peer.id == peer.id) {
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
                // Клик: к показанному закреплённому, в полосе — следующее (Чт-14).
                if let Some(id) = self.pinned.get(self.pinned_index).map(|m| m.id) {
                    self.pinned_index = (self.pinned_index + 1) % self.pinned.len();
                    out.extend(self.intent(ChatIntent::JumpTo(id)));
                }
            }
            ChatIntent::JumpTo(id) => {
                if self.open.is_some() && id > 0 {
                    self.set_jump(Jump::To(id));
                    self.reload_at(Anchor::At(id), &mut out);
                }
            }
            ChatIntent::JumpBottom => {
                if self.open.is_some() {
                    self.set_jump(Jump::Bottom);
                    self.reload_at(Anchor::Latest, &mut out);
                }
            }
            ChatIntent::JumpPermille(f) => {
                let f = f as f32 / 1000.0;
                let top = self.open_row().map(|r| r.dialog.top_message).unwrap_or(0)
                    .max(self.history.iter().map(|m| m.id).max().unwrap_or(0));
                if let (Some(first), true) = (self.window.first_id, top > 0) {
                    if f >= 0.999 {
                        out.extend(self.intent(ChatIntent::JumpBottom));
                    } else {
                        let id = first + ((top - first) as f32 * f.clamp(0.0, 1.0)) as i32;
                        out.extend(self.intent(ChatIntent::JumpTo(id.max(1))));
                    }
                }
            }
            ChatIntent::Seen(id) => {
                let Some(peer_id) = self.open else { return out };
                let incoming = self.history.iter().any(|m| m.id > self.read_sent && m.id <= id && !m.out);
                if id > self.seen {
                    self.seen = id;
                }
                log::debug!("видно до {id}: отмечено до {}, окно {}..{}, входящие {incoming}", self.read_sent,
                    self.history.first().map(|m| m.id).unwrap_or(0), self.history.last().map(|m| m.id).unwrap_or(0));
                if incoming && !self.pending.values().any(|p| *p == Pending::Read { peer_id }) {
                    self.mark_read(peer_id, &mut out);
                }
            }
            ChatIntent::DismissToast => self.toast = None,
            ChatIntent::List(li) => self.list_intent(li, &mut out),
            ChatIntent::MediaClick(id) => self.media_click(id, &mut out),
            ChatIntent::Vote(id, option) => self.vote(id, option, &mut out),
            ChatIntent::CloseViewer => self.viewer = None,
            ChatIntent::ViewerSave => self.viewer_save(&mut out),
            ChatIntent::ViewerForward => out = self.viewer_to_chat(true),
            ChatIntent::ViewerShowInChat => out = self.viewer_to_chat(false),
            ChatIntent::Attach => self.attach(false, &mut out),
            ChatIntent::PrepareMore => self.attach(true, &mut out),
            ChatIntent::PrepareAsPhoto(on) => self.prepare_as_photo(on),
            ChatIntent::PrepareRemove(i) => self.prepare_remove(i),
            ChatIntent::PrepareCancel => self.prepare = None,
            ChatIntent::Settings(si) => self.settings_intent(si, &mut out),
            ChatIntent::WindowActive(v) => self.window_active = v,
            ChatIntent::NotificationClicked(peer_id) => {
                out = self.intent(ChatIntent::OpenChat(peer_id));
                out.push(ChatEffect::RaiseWindow);
            }
            ChatIntent::PasteImage => {
                if self.open.is_some() {
                    out.push(ChatEffect::PasteImage);
                }
            }
            ChatIntent::ClearCache => {
                self.media_paths.clear();
                out.push(ChatEffect::ClearCache);
            }
            ChatIntent::ViewerStep(d) => {
                let ids = self.photo_ids();
                let pos = self.viewer.and_then(|v| ids.iter().position(|x| *x == v));
                if let Some(next) = pos.and_then(|p| ids.get((p as i64 + i64::from(d)).max(0) as usize)) {
                    let next = *next;
                    self.media_click(next, &mut out);
                }
            }
            ChatIntent::ViewWidth(w) => self.view_width = w.max(0),
            ChatIntent::Visible(top, bottom) => self.visible = Some((top, bottom)),
            ChatIntent::NeedOlder => {
                let Some(peer_id) = self.open else { return out };
                if self.window.start || self.history_request_pending(peer_id) {
                    return out;
                }
                let Some(first) = self.history.iter().map(|m| m.id).find(|&id| id > 0) else { return out };
                if self.window.need_older {
                    self.load_page(peer_id, first, 0, HISTORY_PAGE, &mut out);
                } else {
                    self.reload_at(Anchor::At(first), &mut out);
                }
            }
            ChatIntent::NeedNewer => {
                let Some(peer_id) = self.open else { return out };
                if self.window.end || self.history_request_pending(peer_id) {
                    return out;
                }
                let Some(last) = self.history.iter().map(|m| m.id).filter(|&id| id > 0).max() else { return out };
                if self.window.need_newer {
                    self.load_page(peer_id, last + 1, -HISTORY_PAGE, HISTORY_PAGE, &mut out);
                } else {
                    self.reload_at(Anchor::At(last), &mut out);
                }
            }
        }
        out
    }

    /// Поставить текст в очередь отправки (О-9).
    pub fn enqueue_text(&mut self, peer_id: i64, text: String) -> Vec<ChatEffect> {
        self.enqueue_reply(peer_id, text, Vec::new(), 0)
    }

    fn fill_composer(&mut self, text: String, entities: Vec<Entity>) {
        self.composer = text;
        self.composer_entities = entities;
        self.composer_seq += 1;
    }

    fn enqueue_reply(&mut self, peer_id: i64, text: String, entities: Vec<Entity>, reply_to: i32) -> Vec<ChatEffect> {
        let random_id = self.random_id();
        let mut out = vec![ChatEffect::Db(DbOp::Enqueue { peer_id, text, entities, reply_to, random_id })];
        self.after_enqueue(peer_id, &mut out);
        out
    }

    /// После постановки в очередь: своё видно сразу (к концу истории), очередь — в путь.
    pub(crate) fn after_enqueue(&mut self, peer_id: i64, out: &mut Vec<ChatEffect>) {
        if self.open == Some(peer_id) {
            self.set_jump(Jump::Bottom);
            self.reload_at(Anchor::Latest, out);
        }
        out.push(ChatEffect::Db(DbOp::ReloadRows));
        out.push(ChatEffect::Db(DbOp::PumpOutbox));
    }

    fn mark_read(&mut self, peer_id: i64, out: &mut Vec<ChatEffect>) {
        let max_id = self.seen;
        log::debug!("прочитано до {max_id}");
        let Some(peer) = self.peer_ref(peer_id) else { return };
        self.read_sent = max_id;
        self.send(Command::MarkRead { peer, max_id }, Pending::Read { peer_id }, out);
        out.push(ChatEffect::Db(DbOp::MarkReadLocal { peer_id, max_id }));
        out.push(ChatEffect::Db(DbOp::ReloadRows));
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

    pub(crate) fn reload_open(&self, out: &mut Vec<ChatEffect>) {
        if let Some(peer_id) = self.open {
            let (before, after) = match self.anchor {
                Anchor::Latest => (LATEST_WINDOW, 0),
                Anchor::At(_) => (WINDOW_HALF, WINDOW_HALF),
            };
            out.push(ChatEffect::Db(DbOp::ReloadHistory { peer_id, anchor: self.anchor, before, after }));
        }
    }

    /// Результат `PumpOutbox`: готовые к отправке (по одному на чат).
    pub fn set_outbox_ready(&mut self, items: Vec<crate::outbox::OutboxItem>) -> Vec<ChatEffect> {
        let mut out = Vec::new();
        let busy = |p: &Pending, l: i64| match p {
            Pending::Outbox { local_id } => *local_id == l,
            Pending::OutboxMedia { local_ids } => local_ids.contains(&l),
            _ => false,
        };
        // Вложения: подряд идущие одного альбома (или одно) — одной командой.
        let mut media: Vec<crate::outbox::OutboxItem> = Vec::new();
        let flush = |m: &mut Self, media: &mut Vec<crate::outbox::OutboxItem>, out: &mut Vec<ChatEffect>| {
            if !media.is_empty() {
                m.send_media_items(std::mem::take(media), out);
            }
        };
        for it in items {
            if self.pending.values().any(|p| busy(p, it.local_id)) {
                continue;
            }
            if it.media.is_some() {
                let same = media.last().is_some_and(|l| l.peer_id == it.peer_id && l.group != 0 && l.group == it.group);
                if !same {
                    flush(self, &mut media, &mut out);
                }
                media.push(it);
                continue;
            }
            let peer = PeerRef { id: it.peer_id, access_hash: it.access_hash };
            out.push(ChatEffect::Db(DbOp::OutboxSending { local_id: it.local_id }));
            self.send(Command::SendMessageReply { peer, text: it.text, random_id: it.random_id, reply_to: it.reply_to,
                entities: it.entities },
                Pending::Outbox { local_id: it.local_id }, &mut out);
        }
        flush(self, &mut media, &mut out);
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
                self.notifications(b, &mut out);
                // Входящие открытого чата становятся прочитанными, когда их видно
                // (интерфейс сообщает `Seen`), а не сразу. Закреплённые — заново:
                // закрепление, открепление и удаление приходят обновлениями.
                self.reload_open(&mut out);
                if let Some(peer_id) = self.open {
                    out.push(ChatEffect::Db(DbOp::ReloadPinned { peer_id }));
                }
                let reset_open = b.updates.iter().any(|u| {
                    matches!(u, crate::model::DomainUpdate::ResetChannel { peer_id } if Some(*peer_id) == self.open)
                });
                if let (true, Some(peer_id)) = (reset_open, self.open) {
                    // История канала стёрта: окно заново с сервера.
                    self.set_jump(Jump::Bottom);
                    self.anchor = Anchor::Latest;
                    self.load_page(peer_id, 0, 0, HISTORY_PAGE, &mut out);
                }
                if b.updates.iter().any(|u| matches!(u, crate::model::DomainUpdate::FoldersChanged)) {
                    self.send(Command::LoadFolders, Pending::Folders, &mut out);
                }
                if b.updates.iter().any(|u| matches!(u, crate::model::DomainUpdate::Reset | crate::model::DomainUpdate::PinnedDialogs)) {
                    self.pages = 0;
                    self.load_dialogs_page(None, &mut out);
                }
            }
            Event::Result(r) => self.result(r, &mut out),
            Event::Transfer { cmd_id, done, total } => self.transfer_progress(*cmd_id, *done, *total),
            Event::NotificationClicked(peer_id) => out = self.intent(ChatIntent::NotificationClicked(*peer_id)),
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
            Pending::History { peer_id, offset_id, add_offset, limit } => {
                self.history_loading = self.history_request_pending(peer_id);
                if let Payload::History { messages, peers, .. } = &r.payload {
                    let ids: Vec<i32> = messages.iter().map(|m| m.id).collect();
                    let span = Span::from_page(peer_id, offset_id, add_offset, limit, &ids);
                    out.push(ChatEffect::Db(DbOp::SaveHistory { messages: messages.clone(), peers: peers.clone(), span }));
                    if self.open == Some(peer_id) {
                        self.reload_open(out);
                        // Начало чата — для полосы прокрутки (самое старое сообщение).
                        let start_known = self.window.first_id.is_some() || span.is_some_and(|s| s.lo == 0);
                        if !start_known && !self.first_requested {
                            self.first_requested = true;
                            self.load_page(peer_id, 1, -1, 1, out);
                        }
                    }
                    out.push(ChatEffect::Db(DbOp::ReloadRows));
                } else if let Some(e) = &r.error {
                    self.error = Some(e.text.clone());
                }
            }
            Pending::Read { peer_id } => {
                // Пока ответ шёл, видно стало больше — ещё раз.
                if self.open == Some(peer_id) && self.seen > self.read_sent
                    && self.history.iter().any(|m| m.id > self.read_sent && m.id <= self.seen && !m.out)
                {
                    self.mark_read(peer_id, out);
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
                    if let Some(peer_id) = self.open {
                        out.push(ChatEffect::Db(DbOp::ReloadPinned { peer_id }));
                    }
                }
            }
            Pending::Pinned { peer_id } => {
                if let Payload::History { messages, peers, .. } = &r.payload {
                    out.push(ChatEffect::Db(DbOp::SaveHistory { messages: messages.clone(), peers: peers.clone(), span: None }));
                    out.push(ChatEffect::Db(DbOp::ReloadPinned { peer_id }));
                }
            }
            Pending::Quotes { peer_id } => {
                if let Payload::History { messages, peers, .. } = &r.payload {
                    out.push(ChatEffect::Db(DbOp::SaveHistory { messages: messages.clone(), peers: peers.clone(), span: None }));
                    if self.open == Some(peer_id) {
                        self.reload_open(out);
                    }
                }
            }
            p @ (Pending::Folders
            | Pending::GlobalSearch { .. }
            | Pending::ServerPeers { .. }
            | Pending::ChatInfo { .. }
            | Pending::ChatSearch { .. }
            | Pending::ChatAction { .. }) => self.list_result(p, r, out),
            Pending::Download { key, size } => {
                let path = match (&r.error, &r.payload) {
                    (None, Payload::File(p)) => Some(p.clone()),
                    _ => None,
                };
                self.download_done(key, size, path, out);
            }
            Pending::Vote { peer, msg_id } => match &r.error {
                Some(e) => {
                    let t = e.text.clone();
                    self.toast(&t);
                }
                None => {
                    self.send(Command::LoadMessages { peer, ids: vec![msg_id] }, Pending::Quotes { peer_id: peer.id }, out)
                }
            },
            Pending::Pick { append } => self.picked(append, r, out),
            Pending::OutboxMedia { local_ids } => self.media_sent(local_ids, r, out),
            Pending::Save => self.saved(r),
            p @ (Pending::Profile
            | Pending::Sessions
            | Pending::ProxyCheck
            | Pending::PickTheme
            | Pending::PickDownloads
            | Pending::PickPhoto
            | Pending::ProfilePhoto) => self.settings_result(p, r, out),
            Pending::Other => {}
            Pending::PickerSearch | Pending::Contacts => {
                if let Payload::Peers(peers) = &r.payload {
                    out.push(ChatEffect::Db(DbOp::SavePeers(peers.clone())));
                    let q = self.picker.as_ref().map(|p| p.query.trim().to_string()).unwrap_or_default();
                    out.push(ChatEffect::Db(DbOp::SearchPeers { query: q }));
                }
            }
        }
    }
}

/// Пробелы по краям отправляемого текста убираются, смещения форматирования
/// (UTF-16) сдвигаются и обрезаются по новому тексту.
pub fn trim_with_entities(text: &str, entities: Vec<Entity>) -> (String, Vec<Entity>) {
    let lead = text.len() - text.trim_start().len();
    let t = text.trim();
    let shift = text[..lead].encode_utf16().count() as i32;
    let len = t.encode_utf16().count() as i32;
    let es = entities
        .into_iter()
        .filter_map(|mut e| {
            let start = (e.offset - shift).max(0);
            let end = (e.offset + e.length - shift).min(len);
            e.offset = start;
            e.length = end - start;
            (e.length > 0).then_some(e)
        })
        .collect();
    (t.to_string(), es)
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
    fn entities_survive_trim_and_storage() {
        use crate::model::{decode_entities, encode_entities, EntityKind};
        let e = |kind, offset, length| Entity { kind, offset, length, url: String::new() };
        // «  жирный 👍 » — пробелы по краям уходят, смещения (UTF-16) сдвигаются.
        let (t, es) = trim_with_entities("  жирный 👍 ", vec![e(EntityKind::Bold, 2, 6), e(EntityKind::Italic, 9, 3)]);
        assert_eq!(t, "жирный 👍");
        assert_eq!(es, vec![e(EntityKind::Bold, 0, 6), e(EntityKind::Italic, 7, 2)], "эмодзи — 2 единицы UTF-16");
        let link = Entity { kind: EntityKind::TextUrl, offset: 1, length: 4, url: "https://a.b/c d".into() };
        let all = vec![e(EntityKind::Code, 0, 3), link];
        assert_eq!(decode_entities(&encode_entities(&all)), all);
        let mut s = crate::store::Store::in_memory().unwrap();
        s.outbox_enqueue(&crate::outbox::Outgoing { peer_id: 7, text: "код и ссылка", entities: &all, reply_to: 0, random_id: 9, ..Default::default() }, 1, 100).unwrap();
        assert_eq!(s.outbox_ready(100).unwrap()[0].entities, all);
        let w = s.history_window(7, crate::history::Anchor::Latest, 10, 0).unwrap();
        assert_eq!(w.messages[0].entities, all, "локальная строка очереди тоже с форматированием");
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

    fn win(msgs: Vec<Message>, f: impl FnOnce(&mut Window)) -> Window {
        let mut w = Window { messages: msgs, found: true, ..Default::default() };
        f(&mut w);
        w
    }

    fn page_ids(r: std::ops::RangeInclusive<i32>) -> Vec<Message> {
        r.map(|i| Message { peer_id: 10, id: i, ..Default::default() }).collect()
    }

    #[test]
    fn open_chat_at_first_unread_or_bottom() {
        let mut m = ChatModel::new(1, "ru", 0);
        let mut unread = row(10, 3, 55, 0);
        unread.dialog.read_inbox_max_id = 52;
        m.set_rows(vec![unread, row(20, 0, 9, 0)]);
        let fx = m.intent(ChatIntent::OpenChat(10));
        let s = sends(&fx);
        let peer = PeerRef { id: 10, access_hash: 30 };
        assert_eq!(s[0].1, Command::LoadHistory { peer, offset_id: 53, add_offset: -50, limit: 100 }, "вокруг непрочитанного");
        assert_eq!(s[1].1, Command::LoadPinned { peer });
        assert!(fx.contains(&ChatEffect::Db(DbOp::ReloadHistory { peer_id: 10, anchor: Anchor::At(53), before: 50, after: 50 })));
        assert_eq!(m.unread_mark, Some(52));
        assert_eq!(m.jump, None, "переход — когда окно готово");
        m.set_history(10, win(page_ids(40..=60), |_| {}), HashMap::new());
        assert_eq!(m.jump.map(|j| j.1), Some(Jump::To(53)));
        assert!(!s.iter().any(|(_, c)| matches!(c, Command::MarkRead { .. })), "прочитанным — по мере просмотра");
        // Прочитанный чат — к концу.
        let fx = m.intent(ChatIntent::OpenChat(20));
        assert_eq!(sends(&fx)[0].1, Command::LoadHistory { peer: PeerRef { id: 20, access_hash: 60 }, offset_id: 0,
            add_offset: 0, limit: 50 });
        m.set_history(20, win(vec![], |w| w.end = true), HashMap::new());
        assert_eq!((m.anchor, m.unread_mark, m.jump.map(|j| j.1)), (Anchor::Latest, None, Some(Jump::Bottom)));
    }

    #[test]
    fn window_moves_inside_base_then_asks_server() {
        let mut m = ChatModel::new(1, "ru", 0);
        m.set_rows(vec![row(10, 0, 100, 0)]);
        let hid = sends(&m.intent(ChatIntent::OpenChat(10)))[0].0;
        let fx = m.event(&ok(hid, Payload::History { messages: page_ids(51..=100), peers: vec![], total: 70, next_rate: 0 }));
        let span = Some(Span { peer_id: 10, lo: 51, hi: crate::history::LIVE });
        assert!(fx.iter().any(|e| matches!(e, ChatEffect::Db(DbOp::SaveHistory { span: s, .. }) if *s == span)));
        // Начало неизвестно — самое старое сообщение для полосы прокрутки.
        let (first_id, first) = sends(&fx)[0].clone();
        assert_eq!(first, Command::LoadHistory { peer: PeerRef { id: 10, access_hash: 30 }, offset_id: 1, add_offset: -1, limit: 1 });
        m.event(&ok(first_id, Payload::History { messages: page_ids(1..=1), peers: vec![], total: 70, next_rate: 0 }));
        // В базе старше нет: страница с сервера, повторов пока грузится нет.
        m.set_history(10, win(page_ids(51..=100), |w| { w.end = true; w.need_older = true; }), HashMap::new());
        let (id, cmd) = sends(&m.intent(ChatIntent::NeedOlder))[0].clone();
        assert_eq!(cmd, Command::LoadHistory { peer: PeerRef { id: 10, access_hash: 30 }, offset_id: 51, add_offset: 0, limit: 50 });
        assert!(sends(&m.intent(ChatIntent::NeedOlder)).is_empty(), "пока грузится — без повторов");
        let fx = m.event(&ok(id, Payload::History { messages: page_ids(31..=50), peers: vec![], total: 70, next_rate: 0 }));
        let span = Some(Span { peer_id: 10, lo: 0, hi: 50 });
        assert!(fx.iter().any(|e| matches!(e, ChatEffect::Db(DbOp::SaveHistory { span: s, .. }) if *s == span)));
        // В базе есть — окно сдвигается без сервера; дошли до начала — стоп.
        m.set_history(10, win(page_ids(31..=100), |w| w.end = true), HashMap::new());
        let fx = m.intent(ChatIntent::NeedOlder);
        assert!(sends(&fx).is_empty());
        assert!(fx.contains(&ChatEffect::Db(DbOp::ReloadHistory { peer_id: 10, anchor: Anchor::At(31), before: 50, after: 50 })));
        m.set_history(10, win(page_ids(1..=80), |w| { w.start = true; w.need_newer = true; }), HashMap::new());
        assert!(m.intent(ChatIntent::NeedOlder).is_empty());
        // Вниз: в базе нет — страница новее.
        let fx = m.intent(ChatIntent::NeedNewer);
        assert_eq!(sends(&fx)[0].1, Command::LoadHistory { peer: PeerRef { id: 10, access_hash: 30 }, offset_id: 81,
            add_offset: -50, limit: 50 });
        // Окно дошло до настоящего — дальше держим последние.
        m.set_history(10, win(page_ids(40..=100), |w| w.end = true), HashMap::new());
        assert_eq!(m.anchor, Anchor::Latest);
    }

    #[test]
    fn jumps_and_missing_anchor() {
        let mut m = ChatModel::new(1, "ru", 0);
        m.set_rows(vec![row(10, 0, 1000, 0)]);
        m.intent(ChatIntent::OpenChat(10));
        m.pending.clear();
        let fx = m.intent(ChatIntent::JumpTo(400));
        assert!(fx.contains(&ChatEffect::Db(DbOp::ReloadHistory { peer_id: 10, anchor: Anchor::At(400), before: 50, after: 50 })));
        // Якоря нет в базе — страница вокруг него; переход ждёт окна.
        let fx = m.set_history(10, Window::default(), HashMap::new());
        assert_eq!(sends(&fx)[0].1, Command::LoadHistory { peer: PeerRef { id: 10, access_hash: 30 }, offset_id: 400,
            add_offset: -50, limit: 100 });
        assert!(m.jump.is_none_or(|j| j.1 != Jump::To(400)));
        // Полоса прокрутки: доля между первым и последним.
        m.set_history(10, win(page_ids(350..=450), |w| w.first_id = Some(100)), HashMap::new());
        let seq = m.jump.unwrap().0;
        assert_eq!(m.jump.unwrap().1, Jump::To(400));
        m.intent(ChatIntent::JumpPermille(500));
        assert_eq!(m.anchor, Anchor::At(550));
        m.set_history(10, win(page_ids(500..=600), |w| w.first_id = Some(100)), HashMap::new());
        assert!(m.jump.unwrap().0 > seq, "каждый переход — новый номер");
        m.intent(ChatIntent::JumpPermille(1000));
        m.set_history(10, win(page_ids(900..=1000), |w| w.end = true), HashMap::new());
        assert_eq!((m.anchor, m.jump.map(|j| j.1)), (Anchor::Latest, Some(Jump::Bottom)));
        // Закреплённое: переход к показанному, в полосе — следующее.
        m.set_pinned(10, vec![Message { id: 700, pinned: true, ..Default::default() },
            Message { id: 300, pinned: true, ..Default::default() }]);
        m.intent(ChatIntent::NextPinned);
        assert_eq!((m.anchor, m.pinned_index), (Anchor::At(700), 1));
    }

    #[test]
    fn send_goes_through_outbox() {
        use crate::outbox::OutboxItem;
        let mut m = ChatModel::new(1, "ru", 100);
        m.set_rows(vec![row(10, 0, 1, 0)]);
        m.intent(ChatIntent::OpenChat(10));
        m.intent(ChatIntent::ComposerEdited("   ".into()));
        assert!(m.intent(ChatIntent::Send(vec![])).is_empty(), "пустое не отправляется");
        m.intent(ChatIntent::ComposerEdited(" привет ".into()));
        let fx = m.intent(ChatIntent::Send(vec![]));
        let Some(ChatEffect::Db(DbOp::Enqueue { peer_id: 10, text, random_id: r1, .. })) = fx.first() else {
            panic!("{fx:?}")
        };
        assert_eq!(text, "привет");
        assert!(m.composer.is_empty(), "поле очищается сразу");
        assert!(fx.contains(&ChatEffect::Db(DbOp::PumpOutbox)));
        m.intent(ChatIntent::ComposerEdited("ещё".into()));
        let Some(ChatEffect::Db(DbOp::Enqueue { random_id: r2, .. })) = m.intent(ChatIntent::Send(vec![])).first().cloned() else {
            panic!()
        };
        assert_ne!(*r1, r2);

        // Очередь: отправка готовых, повтор одного и того же не дублируется.
        let item = |local_id: i64| OutboxItem { local_id, peer_id: 10, access_hash: 30, random_id: 5, text: "привет".into(), reply_to: 0, entities: vec![], media: None, group: 0 };
        let fx = m.set_outbox_ready(vec![item(1)]);
        assert_eq!(fx[0], ChatEffect::Db(DbOp::OutboxSending { local_id: 1 }));
        let (id, cmd) = sends(&fx)[0].clone();
        assert_eq!(cmd, Command::SendMessageReply { peer: PeerRef { id: 10, access_hash: 30 }, text: "привет".into(),
            random_id: 5, reply_to: 0, entities: vec![] });
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
    fn updates_apply_and_seen_marks_read() {
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
        assert!(sends(&fx).is_empty(), "не видно — не прочитано");
        m.set_history(10, win(page_ids(1..=3), |w| w.end = true), HashMap::new());
        let fx = m.intent(ChatIntent::Seen(2));
        let (rid, cmd) = sends(&fx)[0].clone();
        assert_eq!(cmd, Command::MarkRead { peer: PeerRef { id: 10, access_hash: 30 }, max_id: 2 });
        assert!(fx.contains(&ChatEffect::Db(DbOp::MarkReadLocal { peer_id: 10, max_id: 2 })));
        assert!(sends(&m.intent(ChatIntent::Seen(3))).is_empty(), "пока ответа нет — ждём");
        let fx = m.event(&ok(rid, Payload::None));
        assert_eq!(sends(&fx)[0].1, Command::MarkRead { peer: PeerRef { id: 10, access_hash: 30 }, max_id: 3 });
        assert!(sends(&m.intent(ChatIntent::Seen(3))).is_empty(), "уже прочитано");
        let pin = UpdatesBatch {
            updates: vec![DomainUpdate::Pinned { peer_id: 10, ids: vec![2], pinned: true }],
            ..Default::default()
        };
        assert!(m.event(&Event::Updates(pin)).contains(&ChatEffect::Db(DbOp::ReloadPinned { peer_id: 10 })),
            "закрепление обновлением — полоса перечитывается сразу");
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
        m.set_history(10, win(vec![msg(1, false, "вопрос\nвторая строка"), msg(2, true, "мой ответ"), msg(3, false, "ещё")],
            |w| w.end = true), HashMap::new());
        m
    }

    #[test]
    fn reply_and_edit_modes() {
        let mut m = chat_with_history();
        m.intent(ChatIntent::Reply(1));
        assert_eq!(m.compose, Compose::Reply { id: 1, author: "p10".into(), snippet: "вопрос".into() });
        m.intent(ChatIntent::ComposerEdited("ок".into()));
        let fx = m.intent(ChatIntent::Send(vec![]));
        assert!(matches!(&fx[0], ChatEffect::Db(DbOp::Enqueue { reply_to: 1, .. })), "ответ через очередь с reply_to");
        assert_eq!(m.compose, Compose::Plain);
        // Правка только своего: чужое не берётся.
        m.intent(ChatIntent::Edit(1));
        assert_eq!(m.compose, Compose::Plain);
        m.intent(ChatIntent::Edit(2));
        assert_eq!(m.composer, "мой ответ");
        m.intent(ChatIntent::ComposerEdited("исправлено".into()));
        let (_, cmd) = sends(&m.intent(ChatIntent::Send(vec![])))[0].clone();
        assert_eq!(cmd, Command::EditMessage { peer: PeerRef { id: 10, access_hash: 30 }, id: 2, text: "исправлено".into(),
            entities: vec![] });
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
