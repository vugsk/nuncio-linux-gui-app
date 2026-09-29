//! Модель главного окна → свойства Slint. Списки обновляются построчно
//! (`set_row_data`, добавление и удаление в конце), а не пересоздаются: иначе
//! теряются прокрутка и выделение (Архитектура/nuncio - Приложение (Rust и
//! Slint).md, раздел 3).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use domain::chat::{ChatIntent, ChatModel, Compose, Jump, PickerPurpose};
use domain::chatstore::DialogRow;
use domain::model::{Entity, EntityKind, Message, Peer, PeerKind};
use domain::timefmt;
use slint::{Color, ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

use crate::richtext::{self, Fonts, Span, Style};
use crate::scroller::Scroller;
use crate::{avatar, App, Chat, DialogItem, FolderItem, MemberItem, MessageItem, PickerItem, RichLine, RichSeg, SearchRow};

/// Подписи на языке интерфейса (в Rust: собираются из данных, не из разметки).
pub fn media_label(kind: &str, ru: bool) -> &'static str {
    match (kind, ru) {
        ("photo", true) => "Фото",
        ("photo", false) => "Photo",
        ("video", true) => "Видео",
        ("video", false) => "Video",
        ("gif", _) => "GIF",
        ("sticker", true) => "Стикер",
        ("sticker", false) => "Sticker",
        ("voice", true) => "Голосовое сообщение",
        ("voice", false) => "Voice message",
        ("video_note", true) => "Видеосообщение",
        ("video_note", false) => "Video message",
        ("audio", true) => "Аудио",
        ("audio", false) => "Audio",
        ("document", true) => "Файл",
        ("document", false) => "File",
        ("location", true) => "Геопозиция",
        ("location", false) => "Location",
        ("contact", true) => "Контакт",
        ("contact", false) => "Contact",
        ("poll", true) => "Опрос",
        ("poll", false) => "Poll",
        ("dice", true) => "Кубик",
        ("dice", false) => "Dice",
        (_, true) => "Вложение",
        (_, false) => "Attachment",
    }
}

pub fn service_label(kind: &str, ru: bool) -> &'static str {
    match (kind, ru) {
        ("ChatCreate" | "ChannelCreate", true) => "Чат создан",
        ("ChatCreate" | "ChannelCreate", false) => "Chat created",
        ("ChatAddUser" | "ChatJoinedByLink" | "ChatJoinedByRequest", true) => "Новый участник",
        ("ChatAddUser" | "ChatJoinedByLink" | "ChatJoinedByRequest", false) => "New member",
        ("ChatDeleteUser", true) => "Участник покинул чат",
        ("ChatDeleteUser", false) => "A member left",
        ("PinMessage", true) => "Сообщение закреплено",
        ("PinMessage", false) => "Message pinned",
        ("ChatEditTitle", true) => "Название изменено",
        ("ChatEditTitle", false) => "Title changed",
        ("ChatEditPhoto" | "ChatDeletePhoto", true) => "Фото чата изменено",
        ("ChatEditPhoto" | "ChatDeletePhoto", false) => "Chat photo changed",
        ("PhoneCall", true) => "Звонок",
        ("PhoneCall", false) => "Call",
        ("ContactSignUp", true) => "Теперь в Telegram",
        ("ContactSignUp", false) => "Joined Telegram",
        ("HistoryClear", true) => "История очищена",
        ("HistoryClear", false) => "History cleared",
        (_, true) => "Служебное сообщение",
        (_, false) => "Service message",
    }
}

/// Причина «не отправлено» (Чт-6) на языке интерфейса.
pub fn send_error_label(err: &str, ru: bool) -> String {
    match (err, ru) {
        ("", _) => String::new(),
        ("expired", true) => "Не отправлено: истёк срок".into(),
        ("expired", false) => "Not sent: expired".into(),
        ("CHAT_WRITE_FORBIDDEN" | "CHAT_RESTRICTED" | "USER_BANNED_IN_CHANNEL", true) => {
            "Не отправлено: нет прав писать в этот чат".into()
        }
        ("CHAT_WRITE_FORBIDDEN" | "CHAT_RESTRICTED" | "USER_BANNED_IN_CHANNEL", false) => {
            "Not sent: you can't write here".into()
        }
        ("MESSAGE_TOO_LONG", true) => "Не отправлено: слишком длинное сообщение".into(),
        ("MESSAGE_TOO_LONG", false) => "Not sent: message is too long".into(),
        ("PEER_ID_INVALID" | "USER_IS_BLOCKED" | "YOU_BLOCKED_USER", true) => "Не отправлено: чат недоступен".into(),
        ("PEER_ID_INVALID" | "USER_IS_BLOCKED" | "YOU_BLOCKED_USER", false) => "Not sent: chat unavailable".into(),
        (e, true) => format!("Не отправлено: {e}"),
        (e, false) => format!("Not sent: {e}"),
    }
}

/// Имя собеседника: своё — «Избранное» (Дз-12), удалённый — «Удалённый аккаунт».
pub fn peer_name(p: &Peer, ru: bool) -> String {
    if p.is_self {
        return if ru { "Избранное" } else { "Saved Messages" }.into();
    }
    if p.name == "\0deleted" || (p.name.is_empty() && p.kind == PeerKind::User) {
        return if ru { "Удалённый аккаунт" } else { "Deleted account" }.into();
    }
    p.name.clone()
}

fn rgb((r, g, b): (u8, u8, u8)) -> Color {
    Color::from_rgb_u8(r, g, b)
}

/// Цвета имён отправителей в группах (как цвета заглушек, светлее для текста).
fn sender_color(id: i64) -> Color {
    const NAMES: [(u8, u8, u8); 7] =
        [(0xf5, 0x9f, 0x00), (0x6a, 0xb3, 0xf3), (0x51, 0xcf, 0x66), (0xcc, 0x5d, 0xe8), (0xff, 0x87, 0x87),
            (0x3b, 0xc9, 0xdb), (0xfc, 0xc4, 0x19)];
    rgb(NAMES[(id.unsigned_abs() % NAMES.len() as u64) as usize])
}

fn preview(r: &DialogRow, ru: bool) -> (String, String, bool) {
    let Some(m) = &r.last else { return (String::new(), String::new(), false) };
    if !m.service.is_empty() {
        return (String::new(), service_label(&m.service, ru).into(), true);
    }
    let prefix = if m.out && !r.peer.is_self { if ru { "Вы:" } else { "You:" }.to_string() } else { String::new() };
    let text = m.text.replace('\n', " ");
    if !m.media.is_empty() {
        let label = media_label(&m.media, ru);
        return if text.is_empty() { (prefix, label.into(), true) } else { (prefix, format!("{label}, {text}"), false) };
    }
    (prefix, text, false)
}

/// Синхронизирует VecModel с новым содержимым построчно.
fn sync<T: Clone + PartialEq + 'static>(model: &VecModel<T>, items: Vec<T>) {
    let old = model.row_count();
    for (i, it) in items.iter().enumerate() {
        if i < old {
            if model.row_data(i).as_ref() != Some(it) {
                model.set_row_data(i, it.clone());
            }
        } else {
            model.push(it.clone());
        }
    }
    for i in (items.len()..old).rev() {
        model.remove(i);
    }
}

thread_local! {
    static RENDERER: RefCell<ChatRenderer> = RefCell::new(ChatRenderer::default());
    /// Высоты, отложенные до следующего прохода цикла событий.
    static LATE: RefCell<Late> = RefCell::new(Late::default());
}

/// Отрисовать снимок модели чата (поток интерфейса).
pub fn render(app: &App, m: &ChatModel, avatars: &HashMap<String, SharedPixelBuffer<Rgba8Pixel>>) {
    RENDERER.with(|r| r.borrow_mut().render(app, m, avatars));
}

/// Намерения, которые рождает сама прокрутка (подгрузка, «прочитано»).
pub fn set_sender(f: impl Fn(ChatIntent) + 'static) {
    RENDERER.with(|r| r.borrow_mut().sender = Some(Rc::new(f)));
}

/// Колесо, высота области, высота строки: прямо в потоке интерфейса.
fn trace(what: std::fmt::Arguments) {
    if std::env::var_os("NUNCIO_DEBUG_SCROLL").is_some() {
        eprintln!("событие: {what}");
    }
}

pub fn scroll(app: &App, dy: f32) {
    trace(format_args!("колесо {dy}"));
    with(app, |r| r.scroller.scroll(dy));
}

/// Высота области и строк приходят из `init` и `changed` разметки — иногда
/// прямо во время создания строк повторителем, когда менять модели нельзя.
/// Поэтому здесь только запоминаем, а вид пересчитываем следующим проходом
/// цикла событий.
pub fn set_view_height(app: &App, h: f32) {
    LATE.with(|l| l.borrow_mut().view_h = Some(h));
    schedule(app);
}

pub fn row_height(app: &App, key: i64, h: f32) {
    // Меньше строки текста — строка ещё не разложена.
    if h >= richtext::LINE {
        LATE.with(|l| l.borrow_mut().heights.push((key, h)));
        schedule(app);
    }
}

#[derive(Default)]
struct Late {
    view_h: Option<f32>,
    heights: Vec<(i64, f32)>,
    scheduled: bool,
}

fn schedule(app: &App) {
    let first = LATE.with(|l| !std::mem::replace(&mut l.borrow_mut().scheduled, true));
    if first {
        let weak = app.as_weak();
        slint::Timer::single_shot(std::time::Duration::ZERO, move || {
            if let Some(app) = weak.upgrade() {
                drain_late(&app);
            }
        });
    }
}

fn with(app: &App, f: impl FnOnce(&mut ChatRenderer)) {
    RENDERER.with(|r| {
        if let Ok(mut r) = r.try_borrow_mut() {
            f(&mut r);
            r.apply_view(app);
        }
    });
}

fn drain_late(app: &App) {
    let late = LATE.with(|l| std::mem::take(&mut *l.borrow_mut()));
    if late.view_h.is_none() && late.heights.is_empty() {
        return;
    }
    trace(format_args!("высоты {} вид {:?}", late.heights.len(), late.view_h));
    with(app, |r| {
        if let Some(h) = late.view_h {
            r.scroller.set_view_height(h);
        }
        for (k, h) in late.heights {
            r.scroller.set_height(k, h);
        }
    });
}

/// Модель строк Slint, синхронизируемая по ключам: сдвиг окна вставляет и
/// удаляет строки на краях, а не переписывает все.
#[derive(Default)]
struct RowsModel {
    model: Option<Rc<VecModel<MessageItem>>>,
    items: Vec<MessageItem>,
}

impl RowsModel {
    fn model(&mut self) -> Rc<VecModel<MessageItem>> {
        self.model.get_or_insert_with(|| Rc::new(VecModel::default())).clone()
    }

    fn sync(&mut self, new: &[MessageItem]) {
        let model = self.model();
        let old_first = self.items.first().map(|i| i.key);
        let new_first = new.first().map(|i| i.key);
        if let (Some(of), Some(nf)) = (old_first, new_first) {
            if let Some(k) = new.iter().position(|i| i.key == of) {
                for (i, it) in new[..k].iter().enumerate() {
                    model.insert(i, it.clone());
                    self.items.insert(i, it.clone());
                }
            } else if let Some(j) = self.items.iter().position(|i| i.key == nf) {
                for _ in 0..j {
                    model.remove(0);
                    self.items.remove(0);
                }
            } else {
                model.set_vec(new.to_vec());
                self.items = new.to_vec();
                return;
            }
        }
        for (i, it) in new.iter().enumerate() {
            if i < self.items.len() {
                if self.items[i] != *it {
                    model.set_row_data(i, it.clone());
                    self.items[i] = it.clone();
                }
            } else {
                model.push(it.clone());
                self.items.push(it.clone());
            }
        }
        while self.items.len() > new.len() {
            model.remove(self.items.len() - 1);
            self.items.pop();
        }
    }

    fn clear(&mut self) {
        if let Some(m) = &self.model {
            m.set_vec(Vec::new());
        }
        self.items.clear();
    }
}

/// Кэш картинок аватаров (готовые круглые буферы от потока домена).
#[derive(Default)]
pub struct ChatRenderer {
    dialogs: Option<Rc<VecModel<DialogItem>>>,
    images: HashMap<String, Image>,
    open: Option<i64>,
    picker: Option<Rc<VecModel<PickerItem>>>,
    rich: RichCache,
    // История: всё окно, прокрутка и видимые части.
    items: Vec<MessageItem>,
    pub scroller: Scroller,
    above: RowsModel,
    below: RowsModel,
    jump_seq: u64,
    first_id: Option<i32>,
    top_id: i32,
    /// Уже попрошено: подгрузка у края окна с этим ключом края, «прочитано» до id.
    asked_older: Option<u64>,
    asked_newer: Option<u64>,
    window_version: u64,
    seen_sent: i32,
    sender: Option<Rc<dyn Fn(ChatIntent)>>,
}

/// Раскладка текстов с эмодзи и картинки эмодзи. Готовые модели строк
/// переиспользуются: пока текст и ширина те же, строка списка не меняется.
#[derive(Default)]
struct RichCache {
    fonts: Fonts,
    width: i32,
    lines: HashMap<String, ModelRc<RichLine>>,
    big: HashMap<String, ModelRc<Image>>,
}

/// Стиль по виду сущности (None — вид без оформления: упоминания, спойлеры и т. п.).
fn entity_style(k: EntityKind) -> Option<Style> {
    let p = Style::PLAIN;
    Some(match k {
        EntityKind::Bold => Style { bold: true, ..p },
        EntityKind::Italic => Style { italic: true, ..p },
        EntityKind::Underline => Style { underline: true, ..p },
        EntityKind::Strike => Style { strike: true, ..p },
        EntityKind::Code | EntityKind::Pre => Style { mono: true, ..p },
        EntityKind::TextUrl | EntityKind::Url | EntityKind::Email => Style { link: 0, ..p },
        _ => return None,
    })
}

/// Байтовое смещение по смещению UTF-16.
fn utf16_to_byte(text: &str, off: i32) -> usize {
    let mut u = 0i32;
    for (b, c) in text.char_indices() {
        if u >= off {
            return b;
        }
        u += c.len_utf16() as i32;
    }
    text.len()
}

/// Сущности Telegram (UTF-16) — в стили раскладки (байты) и таблицу ссылок.
/// Ссылка под другим текстом помечается `?`: перед открытием спросить.
pub fn spans_from_entities(text: &str, entities: &[Entity]) -> (Vec<Span>, Vec<String>) {
    let mut spans = Vec::new();
    let mut links = Vec::new();
    for e in entities {
        let Some(mut style) = entity_style(e.kind) else { continue };
        let start = utf16_to_byte(text, e.offset);
        let end = utf16_to_byte(text, e.offset + e.length);
        if start >= end {
            continue;
        }
        if style.link >= 0 {
            let shown = &text[start..end];
            let url = match e.kind {
                EntityKind::TextUrl if shown.trim() == e.url => e.url.clone(),
                EntityKind::TextUrl => format!("?{}", e.url),
                EntityKind::Email => format!("mailto:{shown}"),
                _ if shown.contains("://") => shown.to_string(),
                _ => format!("https://{shown}"),
            };
            style.link = links.len() as i32;
            links.push(url);
        }
        spans.push(Span { start, end, style });
    }
    (spans, links)
}

/// Ширина текста в пузыре по ширине ленты: пузырь не шире min(480, 75 %),
/// внутри по 10 px полей (см. Bubble в main.slint).
pub fn text_width(view: i32) -> f32 {
    (480.0f32).min(view as f32 * 0.75) - 20.0
}

thread_local! {
    /// Картинки эмодзи (общие для истории и поля ввода).
    static EMOJI_IMAGES: RefCell<HashMap<String, Image>> = RefCell::new(HashMap::new());
}

pub fn emoji_image(e: &str) -> Image {
    EMOJI_IMAGES.with(|c| {
        c.borrow_mut()
            .entry(e.to_string())
            .or_insert_with(|| {
                richtext::emoji_png(e).and_then(crate::decode_png).map(Image::from_rgba8).unwrap_or_default()
            })
            .clone()
    })
}

/// Строки раскладки — в модель Slint.
pub fn rich_rows(lines: Vec<richtext::Line>, links: &[String]) -> ModelRc<RichLine> {
    let rows: Vec<RichLine> = lines
        .into_iter()
        .map(|l| RichLine {
            segs: ModelRc::from(Rc::new(VecModel::from(
                l.segs
                    .into_iter()
                    .map(|s| {
                        let st = s.style;
                        RichSeg {
                            image: if s.emoji { emoji_image(&s.text) } else { Image::default() },
                            text: if s.emoji { "".into() } else { s.text.into() },
                            emoji: s.emoji,
                            bold: st.bold,
                            italic: st.italic,
                            underline: st.underline,
                            strike: st.strike,
                            mono: st.mono,
                            link: usize::try_from(st.link).ok().and_then(|i| links.get(i)).cloned().unwrap_or_default().into(),
                        }
                    })
                    .collect::<Vec<_>>(),
            ))),
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(rows)))
}

impl RichCache {
    fn emoji(&mut self, e: &str) -> Image {
        emoji_image(e)
    }

    /// Заполняет `rich`, `lines`, `big_emoji`: сообщение с эмодзи или
    /// форматированием раскладывается в строки из сегментов.
    fn enrich(&mut self, it: &mut MessageItem, msg: &Message, view: i32) {
        let styled = msg.entities.iter().any(|e| entity_style(e.kind).is_some());
        if it.service || it.text.is_empty() || (!styled && !richtext::has_emoji(&it.text)) {
            return;
        }
        let text = it.text.to_string();
        if !styled {
            if let Some(es) = richtext::only_emoji(&text) {
                if let Some(m) = self.big.get(&text) {
                    it.big_emoji = m.clone();
                    return;
                }
                let imgs: Vec<Image> = es.iter().map(|e| self.emoji(e)).collect();
                let m = ModelRc::from(Rc::new(VecModel::from(imgs)));
                self.big.insert(text, m.clone());
                it.big_emoji = m;
                return;
            }
        }
        if view <= 0 {
            return; // ширина ещё неизвестна: пока обычным текстом
        }
        if self.width != view {
            self.width = view;
            self.lines.clear();
        }
        if self.lines.len() > 4000 {
            self.lines.clear(); // окно истории ограничено; кэш не растёт без края
            self.fonts.trim_cache();
        }
        let key = format!("{}\u{0}{}", text, domain::model::encode_entities(&msg.entities));
        if let Some(m) = self.lines.get(&key) {
            it.rich = true;
            it.lines = m.clone();
            return;
        }
        let (spans, links) = spans_from_entities(&text, &msg.entities);
        let lines = richtext::layout(&text, &spans, text_width(view), &self.fonts);
        let m = self.rich_lines(lines, &links);
        self.lines.insert(key, m.clone());
        it.rich = true;
        it.lines = m;
    }

    pub fn rich_lines(&mut self, lines: Vec<richtext::Line>, links: &[String]) -> ModelRc<RichLine> {
        rich_rows(lines, links)
    }
}

impl ChatRenderer {
    fn image(&mut self, path: &str, px: Option<&SharedPixelBuffer<Rgba8Pixel>>) -> Option<Image> {
        if path.is_empty() {
            return None;
        }
        if let Some(i) = self.images.get(path) {
            return Some(i.clone());
        }
        let img = Image::from_rgba8(px?.clone());
        self.images.insert(path.to_string(), img.clone());
        Some(img)
    }

    /// `avatars` — круглые буферы по пути файла (готовит поток домена).
    pub fn render(&mut self, app: &App, m: &ChatModel, avatars: &HashMap<String, SharedPixelBuffer<Rgba8Pixel>>) {
        let chat = app.global::<Chat>();
        let ru = m.lang == "ru";

        let dialogs = self.dialogs.get_or_insert_with(|| Rc::new(VecModel::default())).clone();
        chat.set_dialogs(ModelRc::from(dialogs.clone()));
        let items: Vec<DialogItem> = m
            .rows
            .iter()
            .map(|r| {
                let name = peer_name(&r.peer, ru);
                let (prefix, text, accent) = preview(r, ru);
                let img = self.image(&r.avatar_path, avatars.get(&r.avatar_path));
                let out_state = match &r.last {
                    Some(l) if l.out && !r.peer.is_self => 1 + i32::from(l.id <= r.dialog.read_outbox_max_id),
                    _ => 0,
                };
                DialogItem {
                    id: r.peer.id.to_string().into(),
                    initials: if r.peer.is_self { "★".into() } else { avatar::initials(&name).into() },
                    name: name.into(),
                    color: rgb(avatar::color_for(r.peer.id)),
                    has_avatar: img.is_some() && !r.peer.is_self,
                    avatar: img.unwrap_or_default(),
                    prefix: prefix.into(),
                    preview: text.into(),
                    preview_accent: accent,
                    time: r.last.as_ref().map(|l| timefmt::dialog_time(l.date, m.now, &m.lang)).unwrap_or_default().into(),
                    unread: r.dialog.unread_count,
                    pinned: r.dialog.pinned,
                    selected: Some(r.peer.id) == m.open,
                    out_state,
                    muted: r.dialog.muted,
                    archived: r.dialog.folder == 1,
                    kind: peer_kind_code(&r.peer),
                    is_self: r.peer.is_self,
                    unread_mark: r.dialog.unread_mark,
                }
            })
            .collect();
        sync(&dialogs, items);
        chat.set_dialogs_loading(m.dialogs_loading);
        self.render_lists(app, m, avatars);

        // Открытый чат.
        chat.set_has_open(m.open.is_some());
        if let Some(peer) = m.open_peer() {
            let avatar_path = m.open_row().map(|r| r.avatar_path.clone()).unwrap_or_default();
            let name = peer_name(peer, ru);
            chat.set_open_name(name.clone().into());
            chat.set_open_initials(if peer.is_self { "★".into() } else { avatar::initials(&name).into() });
            chat.set_open_color(rgb(avatar::color_for(peer.id)));
            let img = self.image(&avatar_path, avatars.get(&avatar_path));
            chat.set_open_has_avatar(img.is_some() && !peer.is_self);
            chat.set_open_avatar(img.unwrap_or_default());
            let status = match (peer.kind, peer.broadcast, peer.is_bot) {
                (PeerKind::User, _, true) => if ru { "бот" } else { "bot" },
                (PeerKind::Channel, true, _) => if ru { "канал" } else { "channel" },
                (PeerKind::Chat | PeerKind::Channel, _, _) => if ru { "группа" } else { "group" },
                _ => "",
            };
            chat.set_open_status(status.into());
            chat.set_can_write(!(peer.kind == PeerKind::Channel && peer.broadcast));
            chat.set_open_id(peer.id.to_string().into());
        }
        // Те же модели (повторная установка без изменений ничего не стоит).
        chat.set_above(ModelRc::from(self.above.model()));
        chat.set_below(ModelRc::from(self.below.model()));
        if self.open != m.open {
            // Другой чат — окно истории и прокрутка заново.
            self.open = m.open;
            let h = self.scroller.view_height();
            self.scroller = Scroller::new();
            self.scroller.set_view_height(h);
            self.above.clear();
            self.below.clear();
            self.asked_older = None;
            self.asked_newer = None;
            self.seen_sent = 0;
        }
        let group = m.open.is_some_and(|id| PeerKind::of(id) != PeerKind::User);
        let read_out = m.open_row().map(|r| r.dialog.read_outbox_max_id).unwrap_or(0);
        let cx = HistoryCtx { senders: &m.senders, quotes: &m.quotes, group, read_out, now: m.now, lang: &m.lang,
            peer: m.open_peer() };
        let mut items = message_items(&m.history, &cx);
        let mut divider = m.unread_mark;
        for (it, msg) in items.iter_mut().zip(&m.history) {
            self.rich.enrich(it, msg, m.view_width);
            if divider.is_some_and(|mark| it.key > mark && !it.out) {
                it.unread_divider = true;
                divider = None;
            }
        }
        trace(format_args!("снимок модели, строк {}", items.len()));
        self.scroller.set_rows(items.iter().map(|i| i64::from(i.key)).collect(), m.window.start, m.window.end);
        self.items = items;
        if let Some((seq, j)) = m.jump {
            if seq != self.jump_seq {
                trace(format_args!("переход {j:?}"));
                self.jump_seq = seq;
                match j {
                    Jump::To(id) => self.scroller.jump_to(i64::from(id)),
                    Jump::Bottom => self.scroller.jump_bottom(),
                }
            }
        }
        self.first_id = m.window.first_id;
        self.window_version = m.window_version;
        self.top_id = m.open_row().map(|r| r.dialog.top_message).unwrap_or(0);
        chat.set_unread_below(m.open_row().map(|r| r.dialog.unread_count).unwrap_or(0));
        self.apply_view(app);

        // Ответ или правка.
        match &m.compose {
            Compose::Plain => chat.set_compose_mode(0),
            Compose::Reply { author, snippet, .. } => {
                chat.set_compose_mode(1);
                chat.set_compose_title(author.clone().into());
                chat.set_compose_text(snippet.clone().into());
            }
            Compose::Edit { original, .. } => {
                chat.set_compose_mode(2);
                chat.set_compose_text(original.lines().next().unwrap_or("").to_string().into());
            }
        }
        // Закреплённое (Чт-14).
        match m.pinned.get(m.pinned_index) {
            Some(p) => {
                let text = if p.text.is_empty() { media_label(&p.media, ru).to_string() } else { p.text.replace('\n', " ") };
                chat.set_pinned_text(text.into());
                chat.set_pinned_counter(if m.pinned.len() > 1 {
                    format!("{} / {}", m.pinned_index + 1, m.pinned.len()).into()
                } else {
                    "".into()
                });
            }
            None => chat.set_pinned_text("".into()),
        }
        // Диалог выбора.
        chat.set_picker_open(m.picker.is_some());
        if let Some(p) = &m.picker {
            chat.set_picker_forward(matches!(p.purpose, PickerPurpose::Forward { .. }));
            chat.set_picker_selected(p.selected.len() as i32);
            let items: Vec<PickerItem> = p
                .results
                .iter()
                .map(|x| {
                    let name = peer_name(x, ru);
                    PickerItem {
                        id: x.id.to_string().into(),
                        initials: if x.is_self { "★".into() } else { avatar::initials(&name).into() },
                        name: name.into(),
                        color: rgb(avatar::color_for(x.id)),
                        subtitle: if x.username.is_empty() { "".into() } else { format!("@{}", x.username).into() },
                        selected: p.selected.contains(&x.id),
                    }
                })
                .collect();
            let model = self.picker.get_or_insert_with(|| Rc::new(VecModel::default())).clone();
            chat.set_picker_items(ModelRc::from(model.clone()));
            sync(&model, items);
        }
        // Удаление.
        chat.set_delete_open(m.delete_ask.is_some());
        if let Some(d) = &m.delete_ask {
            chat.set_delete_can_revoke(d.can_revoke);
            chat.set_delete_revoke(d.revoke);
            chat.set_delete_count(d.ids.len() as i32);
        }
        chat.set_toast(m.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default().into());
        chat.set_history_loading(m.history_loading);
        chat.set_sending(m.sending);
        crate::composer::sync_model(app, m);
        chat.set_error(m.error.clone().unwrap_or_default().into());
    }

    /// Видимые строки, линия опоры, ползунок; просьбы о подгрузке и «прочитано».
    pub fn apply_view(&mut self, app: &App) {
        let chat = app.global::<Chat>();
        let v = self.scroller.view();
        if std::env::var_os("NUNCIO_DEBUG_SCROLL").is_some() {
            let key = |i: Option<usize>| i.and_then(|i| self.items.get(i)).map(|x| x.key).unwrap_or(0);
            eprintln!("scroll: окно {}..{} ({}), опора {} p {:.0}, видно {}..{}, низ {}, старше {}, новее {}",
                key(Some(0)), key(self.items.len().checked_sub(1)), self.items.len(), key(Some(v.below.start)),
                v.pivot, key(v.top_row), key(v.bottom_row), v.at_bottom, v.need_older, v.need_newer);
        }
        self.above.sync(&self.items[v.above.clone()]);
        self.below.sync(&self.items[v.below.clone()]);
        chat.set_pivot(v.pivot);
        chat.set_at_bottom(v.at_bottom || self.items.is_empty());
        // Ползунок по хронологии: верхнее видимое сообщение между первым и последним.
        let top_msg = v.top_row.and_then(|i| self.items[i..].iter().find(|x| x.key > 0)).map(|x| x.key);
        let fits = v.top_row == Some(0) && v.at_bottom && !v.need_older && !v.need_newer;
        match (self.first_id, top_msg) {
            (Some(first), Some(id)) if self.top_id > first && !fits => {
                chat.set_thumb_visible(true);
                let f = if v.at_bottom { 1.0 } else { (id - first) as f32 / (self.top_id - first) as f32 };
                chat.set_thumb(f.clamp(0.0, 1.0));
            }
            _ => chat.set_thumb_visible(false),
        }
        let Some(send) = self.sender.clone() else { return };
        // Раз на окно: новое окно (сдвиг или страница с сервера) — можно ещё.
        let w = Some(self.window_version);
        if v.need_older && !self.items.is_empty() && self.asked_older != w {
            self.asked_older = w;
            send(ChatIntent::NeedOlder);
        }
        if v.need_newer && !self.items.is_empty() && self.asked_newer != w {
            self.asked_newer = w;
            send(ChatIntent::NeedNewer);
        }
        let seen = v.seen_row.and_then(|b| self.items[..=b].iter().rev().find(|x| x.key > 0)).map(|x| x.key);
        if let Some(id) = seen.filter(|id| *id > self.seen_sent) {
            self.seen_sent = id;
            trace(format_args!("видно до {id}"));
            send(ChatIntent::Seen(id));
        }
    }
}

/// Вид чата для разметки: 0 личный, 1 группа, 2 канал.
fn peer_kind_code(p: &Peer) -> i32 {
    match p.kind {
        PeerKind::User => 0,
        PeerKind::Channel if p.broadcast => 2,
        _ => 1,
    }
}

/// «5 участников», «1 подписчик» — по правилам русского множественного числа.
fn count_label(n: i32, broadcast: bool, ru: bool) -> String {
    if !ru {
        let w = if broadcast { "subscriber" } else { "member" };
        return format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    }
    let (one, few, many) =
        if broadcast { ("подписчик", "подписчика", "подписчиков") } else { ("участник", "участника", "участников") };
    let (d10, d100) = (n % 10, n % 100);
    let w = if d10 == 1 && d100 != 11 {
        one
    } else if (2..=4).contains(&d10) && !(12..=14).contains(&d100) {
        few
    } else {
        many
    };
    format!("{n} {w}")
}

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(v)))
}

impl ChatRenderer {
    /// Фильтр папок, поиск в шапке, правая панель, подтверждение действий (В6).
    fn render_lists(&mut self, app: &App, m: &ChatModel, avatars: &HashMap<String, SharedPixelBuffer<Rgba8Pixel>>) {
        use domain::chatlist::{ListFilter, Panel};
        let chat = app.global::<Chat>();
        let ru = m.lang == "ru";
        let all = if ru { "Все чаты" } else { "All chats" };
        let archive = if ru { "Архив" } else { "Archive" };
        let title = match m.filter {
            ListFilter::All => all.to_string(),
            ListFilter::Archive => archive.to_string(),
            ListFilter::Folder(id) => m.folders.iter().find(|f| f.id == id).map(|f| f.title.clone()).unwrap_or_default(),
        };
        chat.set_filter_title(title.into());
        let mut folders = vec![FolderItem {
            kind: 0,
            id: 0,
            title: all.into(),
            unread: m.unread_in(ListFilter::All) as i32,
            selected: m.filter == ListFilter::All,
        }];
        for f in &m.folders {
            let t = if f.emoticon.is_empty() { f.title.clone() } else { format!("{} {}", f.emoticon, f.title) };
            folders.push(FolderItem {
                kind: 2,
                id: f.id,
                title: t.into(),
                unread: m.unread_in(ListFilter::Folder(f.id)) as i32,
                selected: m.filter == ListFilter::Folder(f.id),
            });
        }
        if m.all_rows.iter().any(|r| r.dialog.folder == 1) {
            folders.push(FolderItem {
                kind: 1,
                id: 1,
                title: archive.into(),
                unread: m.unread_in(ListFilter::Archive) as i32,
                selected: m.filter == ListFilter::Archive,
            });
        }
        chat.set_folders(model(folders));

        // Поиск в шапке: разделы по источнику, чтобы база и сервер не путались.
        let row_of = |this: &mut Self, p: &Peer, msg: Option<&Message>| -> SearchRow {
            let name = peer_name(p, ru);
            let path = m.all_rows.iter().find(|r| r.peer.id == p.id).map(|r| r.avatar_path.clone()).unwrap_or_default();
            let img = this.image(&path, avatars.get(&path));
            SearchRow {
                header: false,
                peer: p.id.to_string().into(),
                msg: msg.map(|x| x.id.to_string()).unwrap_or_default().into(),
                initials: if p.is_self { "★".into() } else { avatar::initials(&name).into() },
                title: name.into(),
                text: match msg {
                    Some(x) => domain::chat::snippet(x).into(),
                    None if !p.username.is_empty() => format!("@{}", p.username).into(),
                    None => "".into(),
                },
                time: msg.map(|x| timefmt::dialog_time(x.date, m.now, &m.lang)).unwrap_or_default().into(),
                color: rgb(avatar::color_for(p.id)),
                has_avatar: img.is_some() && !p.is_self,
                avatar: img.unwrap_or_default(),
            }
        };
        let header = |t: &str| SearchRow { header: true, title: t.into(), ..Default::default() };
        let mut rows = Vec::new();
        if let Some(s) = &m.search {
            if !s.chats.is_empty() {
                rows.push(header(if ru { "ЧАТЫ" } else { "CHATS" }));
                for p in &s.chats {
                    rows.push(row_of(self, p, None));
                }
            }
            if !s.global_peers.is_empty() {
                rows.push(header(if ru { "ГЛОБАЛЬНЫЙ ПОИСК" } else { "GLOBAL SEARCH" }));
                for p in &s.global_peers {
                    rows.push(row_of(self, p, None));
                }
            }
            let msg_rows = |this: &mut Self, list: &[Message], rows: &mut Vec<SearchRow>| {
                for x in list {
                    if let Some(p) = m.search_peer(x.peer_id) {
                        let p = p.clone();
                        rows.push(row_of(this, &p, Some(x)));
                    }
                }
            };
            if !s.local.is_empty() {
                rows.push(header(if ru { "СООБЩЕНИЯ" } else { "MESSAGES" }));
                msg_rows(self, &s.local, &mut rows);
            }
            if !s.server.is_empty() {
                rows.push(header(if ru { "СООБЩЕНИЯ НА СЕРВЕРЕ" } else { "MESSAGES ON SERVER" }));
                msg_rows(self, &s.server, &mut rows);
            }
            chat.set_search_loading(s.server_loading);
        } else {
            chat.set_search_loading(false);
        }
        trace(format_args!("строк поиска {} (поиск {})", rows.len(), m.search.is_some()));
        chat.set_search_rows(model(rows));

        // Правая панель.
        chat.set_panel(match m.panel {
            None => 0,
            Some(Panel::Info) => 1,
            Some(Panel::Search) => 2,
        });
        if let Some(peer) = m.open_peer() {
            let row = m.open_row();
            chat.set_info_kind(peer_kind_code(peer));
            chat.set_info_self(peer.is_self);
            chat.set_info_muted(row.is_some_and(|r| r.dialog.muted));
            chat.set_info_archived(row.is_some_and(|r| r.dialog.folder == 1));
            chat.set_info_username(if peer.username.is_empty() { "".into() } else { format!("@{}", peer.username).into() });
            let info = m.info.as_ref().filter(|i| i.peer_id == peer.id);
            chat.set_info_about(info.map(|i| i.about.clone()).unwrap_or_default().into());
            chat.set_info_blocked(info.is_some_and(|i| i.blocked));
            let status = match (peer.kind, info) {
                (PeerKind::User, _) if peer.is_bot => if ru { "бот" } else { "bot" }.to_string(),
                (PeerKind::User, _) => String::new(),
                (_, Some(i)) if i.members > 0 => count_label(i.members, peer.broadcast, ru),
                _ => String::new(),
            };
            chat.set_info_status(status.into());
            let mut members: Vec<(i64, i32)> = info.map(|i| i.participants.clone()).unwrap_or_default();
            members.sort_by_key(|(_, role)| -role);
            let names: HashMap<i64, &Peer> = info.map(|i| i.peers.iter().map(|p| (p.id, p)).collect()).unwrap_or_default();
            let items: Vec<MemberItem> = members
                .iter()
                .filter_map(|(id, role)| {
                    let p = names.get(id)?;
                    let name = peer_name(p, ru);
                    Some(MemberItem {
                        id: id.to_string().into(),
                        initials: avatar::initials(&name).into(),
                        name: name.into(),
                        color: rgb(avatar::color_for(*id)),
                        avatar: Image::default(),
                        has_avatar: false,
                        role: match (role, ru) {
                            (2, true) => "владелец",
                            (2, false) => "owner",
                            (1, true) => "админ",
                            (1, false) => "admin",
                            _ => "",
                        }
                        .into(),
                    })
                })
                .collect();
            let mut options = vec![if ru { "Любой отправитель".to_string() } else { "Any sender".to_string() }];
            options.extend(items.iter().map(|i| i.name.to_string()));
            chat.set_cs_from_options(model(options.into_iter().map(Into::into).collect()));
            chat.set_info_members(model(items));
        }
        // Поиск по чату.
        if let Some(cs) = &m.chat_search {
            let me = if ru { "Вы" } else { "You" };
            let sender = |x: &Message| -> String {
                if x.out {
                    return me.into();
                }
                m.senders
                    .get(&x.from_id)
                    .cloned()
                    .or_else(|| m.info.as_ref().and_then(|i| i.peers.iter().find(|p| p.id == x.from_id)).map(|p| peer_name(p, ru)))
                    .or_else(|| m.open_peer().map(|p| peer_name(p, ru)))
                    .unwrap_or_default()
            };
            let rows: Vec<SearchRow> = cs
                .results
                .iter()
                .map(|x| {
                    let name = sender(x);
                    SearchRow {
                        header: false,
                        peer: x.peer_id.to_string().into(),
                        msg: x.id.to_string().into(),
                        initials: avatar::initials(&name).into(),
                        title: name.into(),
                        text: domain::chat::snippet(x).into(),
                        time: timefmt::dialog_time(x.date, m.now, &m.lang).into(),
                        color: rgb(avatar::color_for(x.from_id)),
                        ..Default::default()
                    }
                })
                .collect();
            chat.set_cs_rows(model(rows));
            let counter = match (cs.index, cs.total) {
                (Some(i), t) if t > 0 => if ru { format!("{} из {t}", i + 1) } else { format!("{} of {t}", i + 1) },
                (None, t) if t > 0 => if ru { format!("Найдено: {t}") } else { format!("Found: {t}") },
                _ => String::new(),
            };
            chat.set_cs_counter(counter.into());
            chat.set_cs_filter(cs.filter as i32);
            chat.set_cs_period(cs.period as i32);
            chat.set_cs_loading(cs.loading);
            let from = cs.from.and_then(|f| {
                m.info.as_ref().and_then(|i| i.peers.iter().find(|p| p.id == f)).map(|p| peer_name(p, ru))
            });
            chat.set_cs_from_name(from.unwrap_or_default().into());
        } else {
            chat.set_cs_rows(model(Vec::new()));
            chat.set_cs_counter("".into());
        }

        // Подтверждение опасного действия.
        chat.set_action_confirm(m.confirm.is_some());
        if let Some(c) = &m.confirm {
            use domain::model::ChatActionKind as K;
            let name = m.all_rows.iter().find(|r| r.peer.id == c.peer_id).map(|r| peer_name(&r.peer, ru)).unwrap_or_default();
            let channel = m.all_rows.iter().any(|r| r.peer.id == c.peer_id && r.peer.broadcast);
            let (title, text, ok) = match (c.kind, ru) {
                (K::ClearHistory, true) => ("Очистить историю?".to_string(), "Сообщения будут удалены у вас. Это нельзя отменить.", "Очистить"),
                (K::ClearHistory, false) => ("Clear history?".to_string(), "Messages will be deleted for you. This can't be undone.", "Clear"),
                (K::DeleteChat, true) => (format!("Удалить чат с {name}?"), "История будет удалена у вас.", "Удалить"),
                (K::DeleteChat, false) => (format!("Delete chat with {name}?"), "The history will be deleted for you.", "Delete"),
                (K::Leave, true) if channel => ("Покинуть канал?".to_string(), "Вы больше не будете получать его сообщения.", "Покинуть"),
                (K::Leave, true) => ("Покинуть группу?".to_string(), "Вы больше не будете получать её сообщения.", "Покинуть"),
                (K::Leave, false) if channel => ("Leave channel?".to_string(), "You will no longer receive its messages.", "Leave"),
                (K::Leave, false) => ("Leave group?".to_string(), "You will no longer receive its messages.", "Leave"),
                (K::Block, true) => (format!("Заблокировать {name}?"), "Этот пользователь не сможет писать вам.", "Заблокировать"),
                (K::Block, false) => (format!("Block {name}?"), "This user won't be able to message you.", "Block"),
                (_, _) => (String::new(), "", "OK"),
            };
            chat.set_action_title(title.into());
            chat.set_action_text(text.into());
            chat.set_action_ok(ok.into());
            chat.set_action_can_revoke(c.kind == K::DeleteChat);
            chat.set_action_revoke(c.revoke);
            chat.set_action_revoke_text(if ru { format!("Удалить и у {name}") } else { format!("Also delete for {name}") }.into());
        }
    }
}

/// Строки истории: разделители дат, серии одного автора, подписи.
/// Что нужно строкам истории кроме самих сообщений.
pub struct HistoryCtx<'a> {
    pub senders: &'a HashMap<i64, String>,
    pub quotes: &'a HashMap<i32, (String, String)>,
    pub group: bool,
    pub read_out: i32,
    pub now: i64,
    pub lang: &'a str,
    /// Собеседник открытого чата: для ссылок и прав.
    pub peer: Option<&'a Peer>,
}

pub fn message_items(history: &[Message], cx: &HistoryCtx) -> Vec<MessageItem> {
    let (senders, group, read_out, now, lang) = (cx.senders, cx.group, cx.read_out, cx.now, cx.lang);
    let ru = lang == "ru";
    let mut out = Vec::with_capacity(history.len());
    let mut prev: Option<&Message> = None;
    // Последнее своё отправленное: у него «прочитано» видно всегда (Чт-6).
    let last_out = history.iter().rev().find(|m| m.out && m.send_state == 0).map(|m| m.id);
    for msg in history {
        let new_day = prev.is_none_or(|p| timefmt::day_key(p.date) != timefmt::day_key(msg.date));
        let first = new_day
            || prev.is_none_or(|p| p.from_id != msg.from_id || p.out != msg.out || !p.service.is_empty()
                || msg.date - p.date > 300);
        let service = !msg.service.is_empty();
        out.push(MessageItem {
            date_label: if new_day { timefmt::date_label(msg.date, now, lang).into() } else { "".into() },
            text: if service { service_label(&msg.service, ru).into() } else { msg.text.clone().into() },
            time: timefmt::message_time(msg.date).into(),
            out: msg.out,
            service,
            sender: if group && !msg.out {
                senders.get(&msg.from_id).cloned().unwrap_or_default().into()
            } else {
                "".into()
            },
            sender_color: sender_color(msg.from_id),
            media: if msg.media.is_empty() { "".into() } else { media_label(&msg.media, ru).into() },
            edited: msg.edit_date != 0,
            read: msg.out && msg.id <= read_out,
            first_in_series: first,
            send_state: msg.send_state,
            local_id: if msg.id < 0 { (-i64::from(msg.id)).to_string().into() } else { "".into() },
            send_error: send_error_label(&msg.send_error, ru).into(),
            last_read: Some(msg.id) == last_out && msg.id <= read_out,
            id: if msg.id > 0 { msg.id.to_string().into() } else { "".into() },
            reply_author: match cx.quotes.get(&msg.reply_to) {
                Some((a, _)) if a.is_empty() => if ru { "Вы" } else { "You" }.into(),
                Some((a, _)) => a.clone().into(),
                None if msg.reply_to > 0 => if ru { "Ответ" } else { "Reply" }.into(),
                None => "".into(),
            },
            reply_text: cx.quotes.get(&msg.reply_to).map(|q| q.1.clone()).unwrap_or_default().into(),
            reply_color: Color::from_rgb_u8(0x6a, 0xb3, 0xf3),
            fwd_name: if msg.fwd_from_id != 0 {
                senders.get(&msg.fwd_from_id).cloned().unwrap_or_else(|| {
                    if ru { "скрытого пользователя".into() } else { "hidden user".into() }
                })
            } else {
                msg.fwd_from_name.clone()
            }
            .into(),
            can_edit: msg.out && msg.id > 0 && !service && msg.media.is_empty(),
            can_delete: msg.id > 0 && (msg.out || !group || cx.peer.is_some_and(|p| p.kind == PeerKind::Chat)),
            pinned: msg.pinned,
            has_link: cx.peer.and_then(|p| domain::chat::message_link(p, msg.id)).is_some(),
            key: msg.id, // у очереди id = −local_id
            reply_id: if msg.reply_to > 0 { msg.reply_to.to_string().into() } else { "".into() },
            ..Default::default()
        });
        prev = Some(msg);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: i32, from: i64, date: i64, out: bool) -> Message {
        Message { peer_id: -5, id, from_id: from, date, out, text: format!("m{id}"), ..Default::default() }
    }

    #[test]
    fn series_dates_and_senders() {
        let day = 86_400;
        let base = 1_790_000_000;
        let h = vec![msg(1, 7, base, false), msg(2, 7, base + 10, false), msg(3, 8, base + 20, false),
            msg(4, 1, base + 30, true), msg(5, 7, base + day, false)];
        let senders = HashMap::from([(7, "Пётр".to_string()), (8, "Анна".to_string())]);
        let quotes = HashMap::new();
        let cx = HistoryCtx { senders: &senders, quotes: &quotes, group: true, read_out: 4, now: base + day, lang: "ru",
            peer: None };
        let it = message_items(&h, &cx);
        assert!(it[0].first_in_series && !it[1].first_in_series && it[2].first_in_series);
        assert_eq!(it[0].sender, "Пётр");
        assert_eq!(it[3].sender, "", "своё без имени");
        assert!(it[3].read, "прочитано собеседником");
        assert!(!it[0].date_label.is_empty() && it[1].date_label.is_empty() && !it[4].date_label.is_empty());
        assert!(it[4].first_in_series, "новый день — новая серия");
    }

    #[test]
    fn rich_text_is_cached_per_width() {
        let mut c = RichCache::default();
        let item = |t: &str| MessageItem { text: t.into(), ..Default::default() };
        let mut a = item("привет 👋 мир");
        c.enrich(&mut a, &Message::default(), 700);
        assert!(a.rich && a.lines.row_count() == 1);
        let mut b = item("привет 👋 мир");
        c.enrich(&mut b, &Message::default(), 700);
        assert!(a == b, "тот же текст и ширина — та же модель строк (строка списка не меняется)");
        let mut narrow = item("привет 👋 мир ".repeat(8).trim());
        c.enrich(&mut narrow, &Message::default(), 200);
        assert!(narrow.lines.row_count() > 1, "узкая лента — перенос");
        let mut big = item("🔥 🔥");
        c.enrich(&mut big, &Message::default(), 700);
        assert!(!big.rich && big.big_emoji.row_count() == 2);
        let mut plain = item("без эмодзи 1 #");
        c.enrich(&mut plain, &Message::default(), 700);
        assert!(!plain.rich && plain.big_emoji.row_count() == 0);
        assert_eq!(text_width(1000), 460.0);
        assert_eq!(text_width(400), 280.0);
    }

    #[test]
    fn entities_become_styles_and_links() {
        let text = "жирный 👍 и ссылка тут и example.com";
        let u16 = |s: &str| s.encode_utf16().count() as i32;
        let at = |s: &str| u16(&text[..text.find(s).unwrap()]);
        let e = |kind, from: &str, len: &str, url: &str| Entity { kind, offset: at(from), length: u16(len), url: url.into() };
        let es = vec![
            e(EntityKind::Bold, "жирный", "жирный 👍", ""),
            e(EntityKind::TextUrl, "ссылка", "ссылка", "https://evil.example"),
            e(EntityKind::Url, "example.com", "example.com", ""),
            e(EntityKind::Mention, "тут", "тут", ""),
        ];
        let (spans, links) = spans_from_entities(text, &es);
        assert_eq!(spans.len(), 3, "упоминание без оформления");
        assert_eq!(&text[spans[0].start..spans[0].end], "жирный 👍", "эмодзи — две единицы UTF-16");
        assert!(spans[0].style.bold);
        assert_eq!(links, vec!["?https://evil.example".to_string(), "https://example.com".to_string()],
            "скрытая ссылка — с подтверждением, голый адрес — со схемой");
        let mut c = RichCache::default();
        let msg = Message { text: text.into(), entities: es, ..Default::default() };
        let mut it = MessageItem { text: text.into(), ..Default::default() };
        c.enrich(&mut it, &msg, 1000);
        assert!(it.rich, "форматированное — сегментами");
        let segs = it.lines.row_data(0).unwrap().segs;
        assert!((0..segs.row_count()).any(|i| segs.row_data(i).unwrap().bold));
        assert!((0..segs.row_count()).any(|i| segs.row_data(i).unwrap().link == "?https://evil.example"));
    }

    #[test]
    fn names_and_labels() {
        let me = Peer { id: 1, is_self: true, name: "Я".into(), ..Default::default() };
        assert_eq!(peer_name(&me, true), "Избранное");
        let gone = Peer { id: 2, name: "\0deleted".into(), ..Default::default() };
        assert_eq!(peer_name(&gone, false), "Deleted account");
        assert_eq!(media_label("voice", true), "Голосовое сообщение");
        assert_eq!(service_label("PinMessage", false), "Message pinned");
        assert_eq!(service_label("Нечто", true), "Служебное сообщение");
    }
}
