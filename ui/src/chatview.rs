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
use crate::{avatar, AlbumCell, App, Chat, DialogItem, FolderItem, LiveFrame, MemberItem, MessageItem, PickerItem, PollRow, SharedCell, SharedRow,
    PrepItem, RichLine, RichSeg, SearchRow, ViewerWindow};

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
        ("cancelled", true) => "Не отправлено: отменено".into(),
        ("cancelled", false) => "Not sent: cancelled".into(),
        ("FILE_NOT_FOUND", true) => "Не отправлено: файл не найден".into(),
        ("FILE_NOT_FOUND", false) => "Not sent: file not found".into(),
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

thread_local! {
    static NO_IMAGE: Image = Image::from_rgba8(SharedPixelBuffer::new(1, 1));
}

/// Пустая картинка для полей строк. У Slint `Image::default()` не равна даже
/// самой себе, и строка с ней при каждом снимке считалась бы изменённой
/// (замена строки списка, новая раскладка); копии одной картинки равны.
pub fn no_image() -> Image {
    NO_IMAGE.with(Image::clone)
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

/// Воспроизведение (В8): одно на приложение.
struct Playback {
    player: player::Player,
    msg: i32,
    /// В окне просмотра.
    video: bool,
    /// Кружок со звуком в ленте: кадры — в слой живых кадров.
    row: Option<Masked>,
    ended: bool,
    tick: u32,
}

/// GIF или кружок, играющий в ленте сам, без звука (В8).
struct Preview {
    mid: i32,
    player: player::Player,
    masked: Masked,
}

/// Маска кадров ленты (скругление или круг): по размеру кадра, один раз.
struct Masked {
    round: bool,
    size: (u32, u32),
    mask: Vec<u8>,
}

impl Masked {
    fn new(round: bool) -> Self {
        Self { round, size: (0, 0), mask: Vec::new() }
    }

    fn image(&mut self, f: &mut player::Frame) -> Image {
        if self.size != (f.width, f.height) {
            self.size = (f.width, f.height);
            self.mask = crate::photo::mask(f.width, f.height, crate::photo::radius_for(f.width, f.height, self.round));
        }
        crate::photo::apply_mask(&mut f.rgba, &self.mask);
        Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&f.rgba, f.width, f.height))
    }
}

/// Играют в ленте сами не больше стольких видимых GIF и кружков.
const PREVIEWS_MAX: usize = 3;

fn mmss(secs: f64) -> String {
    let s = secs.max(0.0) as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Нажатие на голосовое, которое уже играет: пауза или продолжение (без
/// домена). `false` — не играет, решает домен (скачать, начать).
pub fn media_toggle(app: &App, msg: i32) -> bool {
    let mut done = false;
    with(app, |r| {
        if let Some(pb) = r.playback.as_mut().filter(|p| p.msg == msg && !p.video) {
            pb.player.set_paused(!pb.player.paused());
            r.overlay_playback();
            done = true;
        }
    });
    done
}

/// Размер текста сообщений (В9): раскладка заново.
pub fn set_text_size(app: &App, px: u32) {
    with(app, |r| r.rich.set_text_size(px as f32));
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
    /// Последние отправленные видимые строки (ключи) — без повторов.
    visible_sent: Option<(i32, i32)>,
    sender: Option<Rc<dyn Fn(ChatIntent)>>,
    /// Окно просмотра фото (Св-3): есть, пока открыто.
    viewer: Option<ViewerWindow>,
    viewer_shown: bool,
    /// Воспроизведение (В8): видео в окне просмотра или голосовое в ленте.
    playback: Option<Playback>,
    previews: Vec<Preview>,
    live: Option<Rc<VecModel<LiveFrame>>>,
    play_timer: slint::Timer,
    app: slint::Weak<App>,
    /// Последняя просьба домена играть голосовое и ещё не начатое (файл качается).
    audio_seq_seen: u64,
    audio_want: Option<i32>,
    /// Просмотр рекламы уже отправлен (строка в окне).
    sp_seen: bool,
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

/// Отступ вложения от края пузыря (фото почти вплотную, как в Telegram).
const MEDIA_PAD: f32 = 4.0;

/// Рамка пузыря вокруг вложения по ширине: фото — отступы, превью ссылки —
/// ещё и полоса слева с текстовыми отступами (как в разметке Bubble).
const CHROME_PHOTO: f32 = 2.0 * MEDIA_PAD;
const CHROME_WEBPAGE: f32 = 39.0;

/// Во сколько раз ужать вложение шириной `w`, чтобы пузырь не вылез за ленту
/// (узкое окно, открытая правая панель).
fn media_scale(w: f32, view: i32, chrome: f32) -> f32 {
    let max = (text_width(view) + 20.0 - chrome).max(120.0);
    if view <= 0 { 1.0 } else { (max / w.max(1.0)).min(1.0) }
}

/// Ширина подписи под вложением: пузырь шириной с вложение (Чт-11), текст
/// переносится по нему. У превью ссылки — по ширине карточки.
fn caption_widths(history: &[Message], view: i32) -> HashMap<i32, f32> {
    use domain::model::MediaKind as K;
    let feed = domain::media::feed_images(history);
    let mut out = HashMap::new();
    for m in history {
        let (Some(f), Some(mi)) = (feed.get(&m.id), &m.media_info) else { continue };
        let full = if f.album.1 > 1 { domain::media::ALBUM_W as f32 } else { f.w as f32 };
        let web = mi.kind == K::WebPage;
        let w = full * media_scale(full, view, if web { CHROME_WEBPAGE } else { CHROME_PHOTO });
        let tw = if web { (w + CHROME_WEBPAGE - 20.0).min(text_width(view)) } else { w + CHROME_PHOTO - 20.0 };
        out.insert(m.id, tw.max(80.0));
    }
    out
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
    fn enrich(&mut self, it: &mut MessageItem, msg: &Message, view: i32, width: f32) {
        // Весь текст сообщений — своей раскладкой (не только с эмодзи): у Text
        // Slint с переносом высота считается не по той ширине, в которую его
        // ставят, — текст вылезал за пузырь и наезжал на соседние строки.
        let styled = msg.entities.iter().any(|e| entity_style(e.kind).is_some());
        if it.service || it.text.is_empty() {
            return;
        }
        let text = it.text.to_string();
        // 1–3 эмодзи крупно без пузыря (Чт-12) — только без вложения.
        if !styled && msg.media_info.is_none() && msg.media.is_empty() {
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
        let key = format!("{}\u{0}{}\u{0}{width}", text, domain::model::encode_entities(&msg.entities));
        if let Some(m) = self.lines.get(&key) {
            it.rich = true;
            it.lines = m.clone();
            return;
        }
        let (spans, links) = spans_from_entities(&text, &msg.entities);
        let lines = richtext::layout(&text, &spans, width, &self.fonts);
        let m = self.rich_lines(lines, &links);
        self.lines.insert(key, m.clone());
        it.rich = true;
        it.lines = m;
    }

    pub fn rich_lines(&mut self, lines: Vec<richtext::Line>, links: &[String]) -> ModelRc<RichLine> {
        rich_rows(lines, links)
    }

    fn set_text_size(&mut self, px: f32) {
        if (self.fonts.size() - px).abs() > f32::EPSILON {
            self.fonts.set_size(px);
            self.lines.clear();
            self.width = 0;
        }
    }

    /// Простой текст с эмодзи (опросы) — строками с картинками эмодзи; без эмодзи — None.
    fn plain_lines(&mut self, text: &str, width: f32) -> Option<ModelRc<RichLine>> {
        if !richtext::has_emoji(text) {
            return None;
        }
        let key = format!("{text}\u{0}\u{0}{width}");
        if let Some(m) = self.lines.get(&key) {
            return Some(m.clone());
        }
        let lines = richtext::layout(text, &[], width, &self.fonts);
        let m = self.rich_lines(lines, &[]);
        self.lines.insert(key, m.clone());
        Some(m)
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
        // Картинки, которых больше нет у потока домена (ушли из окна), — забыть.
        self.images.retain(|k, _| avatars.contains_key(k));

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
                    avatar: img.unwrap_or_else(no_image),
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
                    folder_mask: m
                        .folders
                        .iter()
                        .enumerate()
                        .filter(|(_, f)| domain::chatlist::in_folder(f, r))
                        .fold(0, |acc, (i, _)| acc | (1 << i.min(30))),
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
        let live = self.live.get_or_insert_with(|| Rc::new(VecModel::default())).clone();
        chat.set_live(ModelRc::from(live));
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
            self.visible_sent = None;
        }
        let group = m.open.is_some_and(|id| PeerKind::of(id) != PeerKind::User);
        let read_out = m.open_row().map(|r| r.dialog.read_outbox_max_id).unwrap_or(0);
        let cx = HistoryCtx { senders: &m.senders, quotes: &m.quotes, group, read_out, now: m.now, lang: &m.lang,
            peer: m.open_peer() };
        let mut items = message_items(&m.history, &cx);
        let mut divider = m.unread_mark;
        let captions = caption_widths(&m.history, m.view_width);
        for (it, msg) in items.iter_mut().zip(&m.history) {
            let w = captions.get(&msg.id).copied().unwrap_or_else(|| text_width(m.view_width));
            self.rich.enrich(it, msg, m.view_width, w);
            if divider.is_some_and(|mark| it.key > mark && !it.out) {
                it.unread_divider = true;
                divider = None;
            }
        }
        self.fill_media(&mut items, m, avatars, ru);
        // Реклама — последней строкой ленты, когда видна последняя страница.
        if m.sponsored_here().is_some() {
            items.push(MessageItem { key: SPONSORED_KEY, sponsored: true, ..Default::default() });
        }
        self.app = app.as_weak();
        // Голосовое: домен попросил играть — начать, когда файл скачан.
        if m.audio_seq != self.audio_seq_seen {
            self.audio_seq_seen = m.audio_seq;
            self.audio_want = m.audio_play;
        }
        if let Some(id) = self.audio_want {
            let path = m.find_msg(id).and_then(|x| x.media_info.as_deref())
                .and_then(|mi| mi.file.as_ref()).and_then(|f| m.media_paths.get(&f.key())).cloned();
            if let Some(path) = path {
                self.audio_want = None;
                let round = m.history.iter().find(|x| x.id == id).and_then(|x| x.media_info.as_deref())
                    .is_some_and(|mi| mi.kind == domain::model::MediaKind::VideoNote);
                if round {
                    let s = domain::media::ROUND_SIZE as u32;
                    self.start_row_playback(&path, id, s);
                } else {
                    self.start_playback(&path, id, false, (0, 0));
                }
            }
        }
        self.items = items;
        self.overlay_playback();
        let items = std::mem::take(&mut self.items);
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
        self.sync_prepare(app, m, avatars, ru);
        crate::settingsview::render(app, m, avatars);
        self.sync_viewer(m, avatars, ru);
    }

    /// Картинка из буферов потока домена по ключу (если готова).
    fn media_image(&mut self, key: &str, images: &HashMap<String, SharedPixelBuffer<Rgba8Pixel>>) -> Option<Image> {
        let px = images.get(key)?;
        self.image(key, Some(px))
    }

    /// Вкладки общих медиа в «Сведениях» (Св-1): сетка медиа или список.
    fn fill_shared(&mut self, chat: &Chat, m: &ChatModel, images: &HashMap<String, SharedPixelBuffer<Rgba8Pixel>>,
        ru: bool) {
        use domain::media::{duration_label, size_label};
        use domain::model::{MediaKind as K, SearchFilter as F};
        let s = m.shared.as_ref().filter(|s| Some(s.peer_id) == m.open);
        chat.set_shared_tab(match s.map(|s| s.filter) {
            Some(F::Media) => 1,
            Some(F::Files) => 2,
            Some(F::Links) => 3,
            Some(F::Voice) => 4,
            _ => 0,
        });
        chat.set_shared_loading(s.is_some_and(|s| s.loading));
        chat.set_shared_complete(s.is_none_or(|s| s.complete));
        let peer = m.open.unwrap_or(0);
        let mut cells = Vec::new();
        for (id, f) in m.shared_images() {
            let img = match f.photo_key().and_then(|k| self.media_image(&k, images)) {
                Some(i) => Some(i),
                None => self.media_image(&f.strip_key(peer, id), images),
            };
            let mi = m.find_msg(id).and_then(|x| x.media_info.as_deref());
            let video = mi.is_some_and(|mi| mi.kind != K::Photo);
            let dur = mi.filter(|mi| video && mi.duration > 0).map(|mi| duration_label(mi.duration)).unwrap_or_default();
            cells.push(SharedCell { id, has_img: img.is_some(), img: img.unwrap_or_else(no_image), play: video, dur: dur.into() });
        }
        chat.set_shared_cells(model(cells));
        let mut rows = Vec::new();
        if s.is_some_and(|s| s.filter != F::Media) {
            for x in m.shared_items() {
                let date = domain::timefmt::dialog_time(x.date, m.now, &m.lang);
                if let Some((url, title, site)) = domain::shared::link_of(x).filter(|_| s.is_some_and(|s| s.filter == F::Links)) {
                    let sub = if site.is_empty() { date } else { format!("{site} · {date}") };
                    rows.push(SharedRow { id: x.id, title: title.into(), subtitle: sub.into(), icon: 1, state: 0, progress: 0.0,
                        url: url.into() });
                    continue;
                }
                let Some(mi) = x.media_info.as_deref().filter(|mi| !mi.self_destruct()) else { continue };
                let Some(file) = mi.file.as_ref() else { continue };
                let key = file.key();
                let size = if file.size > 0 { file.size } else { mi.size };
                let (state, progress) = match m.transfers.get(&key) {
                    Some(t) => (1, if t.total > 0 { (t.done as f32 / t.total as f32).clamp(0.0, 1.0) } else { 0.0 }),
                    None if m.media_paths.contains_key(&key) => (2, 1.0),
                    None => (0, 0.0),
                };
                let voice = matches!(mi.kind, K::Voice | K::Audio | K::VideoNote);
                let title = match mi.kind {
                    K::Voice => media_label("voice", ru).to_string(),
                    K::VideoNote => media_label("video", ru).to_string(),
                    K::Audio if !mi.title.is_empty() => {
                        if mi.performer.is_empty() { mi.title.clone() } else { format!("{} — {}", mi.performer, mi.title) }
                    }
                    _ if !mi.name.is_empty() => mi.name.clone(),
                    _ => media_label("document", ru).to_string(),
                };
                let mut sub = Vec::new();
                if mi.duration > 0 {
                    sub.push(duration_label(mi.duration));
                }
                if size > 0 {
                    sub.push(size_label(size, ru));
                }
                sub.push(date);
                rows.push(SharedRow { id: x.id, title: title.into(), subtitle: sub.join(" · ").into(),
                    icon: if voice { 2 } else { 0 }, state, progress, url: "".into() });
            }
        }
        chat.set_shared_rows(model(rows));
    }

    /// Вложения строк истории (В7) и склейка альбомов в одну строку.
    fn fill_media(&mut self, items: &mut Vec<MessageItem>, m: &ChatModel,
        images: &HashMap<String, SharedPixelBuffer<Rgba8Pixel>>, ru: bool) {
        use domain::media::{albums, album_layout, duration_label, feed_images, size_label, ALBUM_W};
        use domain::model::MediaKind as K;
        let feed = feed_images(&m.history);
        let peer = m.open.unwrap_or(0);
        for (it, msg) in items.iter_mut().zip(&m.history) {
            let Some(mi) = &msg.media_info else { continue };
            it.mid = msg.id.to_string().into();
            if let Some(f) = feed.get(&msg.id) {
                let img = match f.photo_key().and_then(|k| self.media_image(&k, images)) {
                    Some(i) => Some(i),
                    None => self.media_image(&f.strip_key(peer, msg.id), images),
                };
                // Узкое окно — вложение ужимается (картинка масштабируется при показе).
                let chrome = if mi.kind == K::WebPage { CHROME_WEBPAGE } else { CHROME_PHOTO };
                let k = media_scale(f.w as f32, m.view_width, chrome);
                it.mw = f.w as f32 * k;
                it.mh = f.h as f32 * k;
                it.mhas_img = img.is_some();
                it.mimg = img.unwrap_or_else(no_image);
            }
            // Файл: скачивается (своё — выгружается), скачан или нет.
            let key = if msg.id < 0 {
                domain::attach::upload_key(-i64::from(msg.id))
            } else {
                mi.file.as_ref().map(|f| f.key()).unwrap_or_default()
            };
            let size = mi.file.as_ref().map(|f| f.size).filter(|s| *s > 0).unwrap_or(mi.size);
            if let Some(t) = m.transfers.get(&key) {
                let total = if t.total > 0 { t.total } else { size };
                it.file_state = 1;
                it.file_progress = if total > 0 { (t.done as f32 / total as f32).clamp(0.0, 1.0) } else { 0.0 };
                let of = if ru { "из" } else { "of" };
                it.file_info = format!("{} {of} {}", size_label(t.done, ru), size_label(total, ru)).into();
            } else {
                // Своё неотправленное — файл уже на диске: не «скачать».
                it.file_state = if m.media_paths.contains_key(&key) || msg.id < 0 { 2 } else { 0 };
                it.file_info = if size > 0 { size_label(size, ru).into() } else { "".into() };
            }
            let has_feed = feed.contains_key(&msg.id);
            // Самоуничтожающееся: строка-заглушка, открывается окном просмотра.
            if mi.self_destruct() {
                it.mkind = 2;
                it.file_ttl = true;
                let video = mi.kind != K::Photo;
                it.file_name = match (video, ru) {
                    (false, true) => "Самоуничтожающееся фото",
                    (false, false) => "Self-destructing photo",
                    (true, true) => "Самоуничтожающееся видео",
                    (true, false) => "Self-destructing video",
                }
                .into();
                it.file_info = match (mi.expired(), msg.out, ru) {
                    (true, _, true) => "Срок просмотра истёк".into(),
                    (true, _, false) => "Expired".into(),
                    (false, true, true) => "Вы отправили".into(),
                    (false, true, false) => "You sent it".into(),
                    (false, false, true) => "Нажмите, чтобы посмотреть".into(),
                    (false, false, false) => "Click to view".into(),
                };
                continue;
            }
            match mi.kind {
                K::Photo if has_feed => it.mkind = 1,
                K::Video | K::Gif | K::VideoNote if has_feed => {
                    it.mkind = 1;
                    it.mplay = mi.kind == K::Video;
                    it.mround = mi.kind == K::VideoNote;
                    it.mgif = mi.kind == K::Gif;
                    it.mdur = if mi.duration > 0 { duration_label(mi.duration).into() } else { "".into() };
                    // Скачанный GIF или кружок — играет в ленте сам (без звука).
                    if it.mround || it.mgif {
                        it.mpath = m.media_paths.get(&key).cloned().unwrap_or_default().into();
                    }
                }
                K::Document | K::Audio | K::Voice | K::Video | K::VideoNote => {
                    it.mkind = 2;
                    it.file_voice = matches!(mi.kind, K::Voice | K::Audio);
                    let name = match mi.kind {
                        K::Voice => media_label("voice", ru).to_string(),
                        K::Audio if !mi.title.is_empty() => {
                            if mi.performer.is_empty() { mi.title.clone() } else { format!("{} — {}", mi.performer, mi.title) }
                        }
                        _ if !mi.name.is_empty() => mi.name.clone(),
                        K::Video | K::VideoNote => media_label("video", ru).to_string(),
                        _ => media_label("document", ru).to_string(),
                    };
                    it.file_name = name.into();
                    if mi.duration > 0 && it.file_state != 1 {
                        let d = duration_label(mi.duration);
                        it.file_info = if it.file_info.is_empty() { d.into() } else { format!("{d}, {}", it.file_info).into() };
                    }
                }
                K::WebPage => {
                    let Some(wp) = &mi.webpage else { continue };
                    it.mkind = 3;
                    it.wp_site = wp.site.clone().into();
                    it.wp_title = wp.title.clone().into();
                    let mut desc: String = wp.description.chars().take(300).collect();
                    if desc.len() < wp.description.len() {
                        desc.push('…');
                    }
                    it.wp_desc = desc.into();
                }
                K::Poll => {
                    let Some(poll) = &mi.poll else { continue };
                    it.mkind = 4;
                    let voted = poll.closed || poll.options.iter().any(|o| o.chosen);
                    it.poll_voted = voted;
                    it.poll_question = poll.question.clone().into();
                    let tw = text_width(m.view_width);
                    if let Some(l) = self.rich.plain_lines(&poll.question, tw) {
                        it.poll_qrich = true;
                        it.poll_qlines = l;
                    }
                    let kind = match (poll.quiz, ru) {
                        (true, true) => "Викторина",
                        (true, false) => "Quiz",
                        (false, true) => "Опрос",
                        (false, false) => "Poll",
                    };
                    let votes = if ru {
                        format!("{kind} · {}", count_votes(poll.total_voters))
                    } else {
                        format!("{kind} · {} vote{}", poll.total_voters, if poll.total_voters == 1 { "" } else { "s" })
                    };
                    it.poll_info = if poll.closed { format!("{votes} · {}", if ru { "завершён" } else { "closed" }) } else { votes }
                        .into();
                    let total = poll.options.iter().map(|o| o.voters).sum::<i32>().max(1);
                    it.poll_options = model(
                        poll.options
                            .iter()
                            .map(|o| PollRow {
                                lines: self.rich.plain_lines(&o.text, tw - 70.0).unwrap_or_default(),
                                rich: richtext::has_emoji(&o.text),
                                text: o.text.clone().into(),
                                share: o.voters as f32 / total as f32,
                                percent: format!("{}%", (o.voters * 100 + total / 2) / total).into(),
                                chosen: o.chosen,
                                correct: o.correct,
                            })
                            .collect(),
                    );
                }
                _ => {}
            }
            if it.mkind != 0 {
                it.media = "".into();
            }
        }
        // Альбомы: строки подряд → одна; ключ — последнего сообщения (прочитано до конца альбома).
        for r in albums(&m.history).into_iter().rev() {
            let (cells, height) = album_layout(r.len());
            let k = media_scale(ALBUM_W as f32, m.view_width, CHROME_PHOTO);
            let group: Vec<MessageItem> = items.drain(r.clone()).collect();
            let mut base = group[0].clone();
            if let Some(cap) = group.iter().find(|g| !g.text.is_empty()) {
                base.text = cap.text.clone();
                base.rich = cap.rich;
                base.lines = cap.lines.clone();
                base.big_emoji = cap.big_emoji.clone();
            }
            base.key = group.last().map(|g| g.key).unwrap_or(base.key);
            base.unread_divider = group.iter().any(|g| g.unread_divider);
            base.edited = group.iter().any(|g| g.edited);
            base.album = model(
                group
                    .iter()
                    .zip(cells)
                    .map(|(g, c)| AlbumCell {
                        id: g.mid.clone(),
                        img: g.mimg.clone(),
                        has_img: g.mhas_img,
                        play: g.mplay,
                        x: c.x as f32 * k,
                        y: c.y as f32 * k,
                        w: c.w as f32 * k,
                        h: c.h as f32 * k,
                    })
                    .collect(),
            );
            base.mkind = 5;
            base.mw = ALBUM_W as f32 * k;
            base.mh = height as f32 * k;
            items.insert(r.start, base);
        }
    }

    /// Панель подготовки вложений (Св-4).
    fn sync_prepare(&mut self, app: &App, m: &ChatModel, images: &HashMap<String, SharedPixelBuffer<Rgba8Pixel>>, ru: bool) {
        let chat = app.global::<Chat>();
        chat.set_prepare_open(m.prepare.is_some());
        let Some(p) = &m.prepare else { return };
        let n = p.files.len();
        let title = match (p.as_photo, ru) {
            (true, true) => format!("Отправить {n} {}", plural(n, "фото", "фото", "фото")),
            (false, true) => format!("Отправить {n} {}", plural(n, "файл", "файла", "файлов")),
            (true, false) => format!("Send {n} photo{}", if n == 1 { "" } else { "s" }),
            (false, false) => format!("Send {n} file{}", if n == 1 { "" } else { "s" }),
        };
        chat.set_prepare_title(title.into());
        chat.set_prepare_as_photo(p.as_photo);
        chat.set_prepare_can_photo(p.can_photo());
        let items: Vec<PrepItem> = p
            .files
            .iter()
            .map(|f| {
                let img = self.media_image(&format!("prep:{}", f.path), images);
                PrepItem {
                    name: f.name.clone().into(),
                    info: domain::media::size_label(f.size, ru).into(),
                    has_img: img.is_some(),
                    img: img.unwrap_or_else(no_image),
                }
            })
            .collect();
        chat.set_prepare_files(model(items));
    }

    /// Окно просмотра фото (Св-3): создать по запросу, наполнить, закрыть.
    fn sync_viewer(&mut self, m: &ChatModel, images: &HashMap<String, SharedPixelBuffer<Rgba8Pixel>>, ru: bool) {
        let msg = m.viewer.and_then(|id| m.find_msg(id));
        // Видео играет, только пока открыто в окне просмотра.
        if self.playback.as_ref().is_some_and(|p| p.video && Some(p.msg) != msg.map(|x| x.id)) {
            self.stop_playback();
        }
        let Some(msg) = msg else {
            if self.viewer_shown {
                self.viewer_shown = false;
                if let Some(w) = &self.viewer {
                    let _ = w.hide();
                    w.set_photo(no_image());
                }
            }
            return;
        };
        if self.viewer.is_none() {
            let Ok(w) = ViewerWindow::new() else { return };
            if let Some(send) = self.sender.clone() {
                let s = send.clone();
                w.on_step(move |d| s(ChatIntent::ViewerStep(d)));
                let s = send.clone();
                w.on_close_viewer(move || s(ChatIntent::CloseViewer));
                let s = send.clone();
                w.on_save(move || s(ChatIntent::ViewerSave));
                let s = send.clone();
                w.on_forward(move || s(ChatIntent::ViewerForward));
                let s = send.clone();
                w.on_show_in_chat(move || s(ChatIntent::ViewerShowInChat));
                w.window().on_close_requested(move || {
                    send(ChatIntent::CloseViewer);
                    slint::CloseRequestResponse::HideWindow
                });
            }
            w.on_toggle_play(|| RENDERER.with(|r| {
                if let Ok(mut r) = r.try_borrow_mut() {
                    r.toggle_video();
                }
            }));
            w.on_seek(|f| RENDERER.with(|r| {
                if let Ok(mut r) = r.try_borrow_mut() {
                    if let Some(pb) = r.playback.as_mut().filter(|p| p.video) {
                        pb.player.seek(f64::from(f) * pb.player.info().duration);
                        pb.ended = false;
                    }
                }
            }));
            w.on_set_volume(|v| RENDERER.with(|r| {
                if let Ok(mut r) = r.try_borrow_mut() {
                    if let Some(pb) = r.playback.as_mut() {
                        pb.player.set_volume(v);
                    }
                    if let Some(w) = &r.viewer {
                        w.set_volume(v);
                    }
                }
            }));
            let weak = w.as_weak();
            w.on_toggle_fullscreen(move || {
                if let Some(w) = weak.upgrade() {
                    let full = w.window().is_fullscreen();
                    w.window().set_fullscreen(!full);
                }
            });
            self.viewer = Some(w);
        }
        // Окно одно на сеанс: создание и удаление окна Slint при каждом
        // просмотре оставляло ~9 МБ (playtest); скрытое — без кадров.
        if !self.viewer_shown {
            if let Some(w) = &self.viewer {
                if w.show().is_err() {
                    return;
                }
                self.viewer_shown = true;
            }
        }
        let key = m.viewer_key().unwrap_or_default();
        let feed = domain::media::feed_images(std::slice::from_ref(msg));
        // Полный размер, пока его нет — картинка из ленты.
        let img = match self.media_image(&format!("full:{key}"), images) {
            Some(i) => Some(i),
            None => feed.get(&msg.id).and_then(|f| {
                let k = f.photo_key().filter(|k| images.contains_key(k)).unwrap_or_else(|| f.strip_key(m.open.unwrap_or(0), msg.id));
                self.media_image(&k, images)
            }),
        };
        let ids = m.viewer_ids();
        let pos = ids.iter().position(|x| *x == msg.id);
        let is_video = msg.media_info.as_deref().is_some_and(|mi| {
            matches!(mi.kind, domain::model::MediaKind::Video | domain::model::MediaKind::VideoNote | domain::model::MediaKind::Gif)
        });
        // Видео скачано — играть (кадр не больше окна, П-6).
        if is_video && !self.playback.as_ref().is_some_and(|p| p.video && p.msg == msg.id) {
            if let Some(path) = m.media_paths.get(&key).cloned() {
                let size = self.viewer.as_ref().map(|w| w.window().size().to_logical(w.window().scale_factor()));
                let (vw, vh) = size.map(|s| (s.width as u32, s.height as u32)).unwrap_or((0, 0));
                self.start_playback(&path, msg.id, true, (vw.clamp(1280, 1920), vh.clamp(720, 1080)));
            }
        }
        let playing = self.playback.as_ref().is_some_and(|p| p.video && p.msg == msg.id);
        let Some(w) = &self.viewer else { return };
        w.set_is_video(is_video);
        if !playing {
            // Кадры видео ставит таймер; до них — картинка из ленты.
            w.set_has_photo(img.is_some());
            w.set_photo(img.unwrap_or_default());
        }
        w.set_ephemeral(msg.media_info.as_ref().is_some_and(|mi| mi.self_destruct()));
        w.set_counter(match pos {
            Some(p) if ids.len() > 1 => format!("{} {} {}", p + 1, if ru { "из" } else { "of" }, ids.len()).into(),
            _ => "".into(),
        });
        w.set_can_prev(pos.is_some_and(|p| p > 0));
        w.set_can_next(pos.is_some_and(|p| p + 1 < ids.len()));
        w.set_caption(msg.text.chars().take(500).collect::<String>().into());
        let author = if msg.out {
            if ru { "Вы".to_string() } else { "You".to_string() }
        } else {
            m.senders.get(&msg.from_id).cloned().or_else(|| m.open_peer().map(|p| peer_name(p, ru))).unwrap_or_default()
        };
        w.set_author(author.into());
        w.set_date(format!("{}, {}", timefmt::date_label(msg.date, m.now, &m.lang), timefmt::message_time(msg.date)).into());
        w.set_progress(match m.transfers.get(&key) {
            Some(t) if t.total > 0 => format!("{} {}%", if ru { "Загрузка" } else { "Loading" }, t.done * 100 / t.total).into(),
            Some(_) => if ru { "Загрузка…" } else { "Loading…" }.into(),
            None => "".into(),
        });
    }

    fn stop_playback(&mut self) {
        if self.previews.is_empty() {
            self.play_timer.stop();
        }
        if let Some(pb) = self.playback.take() {
            if pb.row.is_some() {
                // Кружок со звуком: убрать его кадр и время (дальше — снова превью).
                self.drop_live(pb.msg);
                if let Some(app) = self.app.upgrade() {
                    app.global::<Chat>().set_sound_mid("".into());
                }
            }
            drop(pb);
            player::trim_memory();
            // Окно просмотра и его кадры Slint освобождает позже, на следующих
            // проходах цикла: ещё раз через секунду (П-6: память после закрытия).
            slint::Timer::single_shot(std::time::Duration::from_secs(1), player::trim_memory);
        }
    }

    fn start_playback(&mut self, path: &str, msg: i32, video: bool, max: (u32, u32)) {
        self.stop_playback();
        let (mw, mh) = if video { max } else { (2, 2) };
        match player::Player::open(std::path::Path::new(path), mw, mh) {
            Ok(p) => {
                let i = p.info();
                log::info!("воспроизведение: {}, кадр {}×{}, {:.1} с", if video { "видео" } else { "звук" }, i.width, i.height,
                    i.duration);
                if let Some(w) = self.viewer.as_ref().filter(|_| video) {
                    p.set_volume(w.get_volume());
                }
                self.playback = Some(Playback { player: p, msg, video, row: None, ended: false, tick: 0 });
                self.ensure_timer();
            }
            Err(e) => log::warn!("воспроизведение не началось: {e}"),
        }
    }

    /// Кружок со звуком прямо в ленте (по щелчку, как в Telegram).
    fn start_row_playback(&mut self, path: &str, msg: i32, side: u32) {
        self.stop_playback();
        self.stop_preview(msg);
        match player::Player::open(std::path::Path::new(path), side, side) {
            Ok(p) => {
                log::info!("воспроизведение: кружок, {:.1} с", p.info().duration);
                self.playback = Some(Playback { player: p, msg, video: false, row: Some(Masked::new(true)), ended: false,
                    tick: 0 });
                self.ensure_timer();
            }
            Err(e) => log::warn!("воспроизведение не началось: {e}"),
        }
    }

    fn ensure_timer(&self) {
        if self.play_timer.running() {
            return;
        }
        let weak = self.app.clone();
        self.play_timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(16), move || {
            let Some(app) = weak.upgrade() else { return };
            RENDERER.with(|r| {
                if let Ok(mut r) = r.try_borrow_mut() {
                    r.tick_playback(&app);
                }
            });
        });
    }

    /// Кадр в слой живых кадров ленты (строки ленты не трогаются).
    fn set_live(&self, mid: i32, img: Image) {
        let Some(live) = &self.live else { return };
        let key: slint::SharedString = mid.to_string().into();
        match (0..live.row_count()).find(|&i| live.row_data(i).is_some_and(|l| l.mid == key)) {
            Some(i) => live.set_row_data(i, LiveFrame { mid: key, img }),
            None => live.push(LiveFrame { mid: key, img }),
        }
    }

    fn drop_live(&self, mid: i32) {
        let Some(live) = &self.live else { return };
        let key: slint::SharedString = mid.to_string().into();
        if let Some(i) = (0..live.row_count()).find(|&i| live.row_data(i).is_some_and(|l| l.mid == key)) {
            live.remove(i);
        }
    }

    fn stop_preview(&mut self, mid: i32) {
        if let Some(i) = self.previews.iter().position(|p| p.mid == mid) {
            self.previews.remove(i);
            self.drop_live(mid);
        }
    }

    /// Видимые GIF и кружки играют сами (без звука, по кругу); ушедшие с экрана — стоп.
    fn sync_previews(&mut self, rows: std::ops::RangeInclusive<usize>) {
        let sound = self.playback.as_ref().map(|p| p.msg);
        let want: Vec<(i32, String, u32, u32, bool)> = self
            .items
            .get(rows)
            .unwrap_or_default()
            .iter()
            .filter(|i| (i.mgif || i.mround) && !i.mpath.is_empty())
            .filter_map(|i| Some((i.mid.parse::<i32>().ok()?, i.mpath.to_string(), i.mw as u32, i.mh as u32, i.mround)))
            .filter(|w| Some(w.0) != sound)
            .take(PREVIEWS_MAX)
            .collect();
        let gone: Vec<i32> = self.previews.iter().map(|p| p.mid).filter(|m| !want.iter().any(|w| w.0 == *m)).collect();
        for mid in gone {
            self.stop_preview(mid);
        }
        let mut started = false;
        for (mid, path, w, h, round) in want {
            if self.previews.iter().any(|p| p.mid == mid) {
                continue;
            }
            match player::Player::open_with(std::path::Path::new(&path), w.max(2), h.max(2), true) {
                Ok(player) => {
                    self.previews.push(Preview { mid, player, masked: Masked::new(round) });
                    started = true;
                }
                Err(e) => log::warn!("превью не играет: {e}"),
            }
        }
        if started {
            self.ensure_timer();
        }
    }

    fn toggle_video(&mut self) {
        let Some(pb) = self.playback.as_mut().filter(|p| p.video) else { return };
        if pb.ended {
            pb.player.seek(0.0);
            pb.player.set_paused(false);
            pb.ended = false;
        } else {
            pb.player.set_paused(!pb.player.paused());
        }
    }

    /// Шаг таймера (60 Гц): кадр видео в окно просмотра, ход голосового в ленту.
    fn tick_playback(&mut self, app: &App) {
        // GIF и кружки в ленте: кадр по часам, по кругу.
        let mut frames = Vec::new();
        for p in &mut self.previews {
            if let Some(mut f) = p.player.take_frame() {
                frames.push((p.mid, p.masked.image(&mut f)));
                p.player.recycle(f);
            }
            if p.player.ended() {
                p.player.seek(0.0);
            }
        }
        for (mid, img) in frames {
            self.set_live(mid, img);
        }
        let Some(pb) = self.playback.as_mut() else {
            if self.previews.is_empty() {
                self.play_timer.stop();
            }
            return;
        };
        pb.tick += 1;
        let (video, tick) = (pb.video, pb.tick);
        if let Some(masked) = pb.row.as_mut() {
            // Кружок со звуком в ленте.
            let frame = pb.player.take_frame().map(|mut f| {
                let img = masked.image(&mut f);
                pb.player.recycle(f);
                img
            });
            let (msg, pos, ended) = (pb.msg, pb.player.position(), pb.player.ended());
            let chat = app.global::<Chat>();
            if tick % 6 == 0 || tick == 1 {
                chat.set_sound_mid(msg.to_string().into());
                chat.set_sound_time(mmss(pos).into());
            }
            if let Some(img) = frame {
                self.set_live(msg, img);
            }
            if ended {
                log::info!("воспроизведение закончено");
                self.stop_playback();
                self.drop_live(msg);
                chat.set_sound_mid("".into());
            }
            return;
        }
        if video {
            let frame = pb.player.take_frame();
            let info = pb.player.info();
            let pos = pb.player.position();
            let playing = !pb.player.paused() && !pb.ended;
            if let Some(w) = &self.viewer {
                if let Some(f) = frame {
                    let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&f.rgba, f.width, f.height);
                    w.set_photo(Image::from_rgba8(buf));
                    w.set_has_photo(true);
                    pb.player.recycle(f);
                }
                if tick % 6 == 0 {
                    w.set_play_pos(if info.duration > 0.0 { (pos / info.duration) as f32 } else { 0.0 });
                    w.set_time(format!("{} / {}", mmss(pos), mmss(info.duration)).into());
                    w.set_playing(playing);
                }
            }
            if pb.player.ended() && !pb.ended {
                pb.ended = true;
                log::info!("воспроизведение закончено");
            }
        } else {
            let ended = pb.player.ended();
            if ended {
                log::info!("воспроизведение закончено");
                self.stop_playback();
            }
            if ended || tick % 6 == 0 {
                self.overlay_playback_or_reset(ended);
                self.apply_view(app);
            }
        }
    }

    /// Строка играющего голосового: кнопка «пауза» или «играть», ход, время.
    fn overlay_playback(&mut self) {
        self.overlay_playback_or_reset(false);
    }

    fn overlay_playback_or_reset(&mut self, reset: bool) {
        let state = self.playback.as_ref().filter(|p| !p.video).map(|p| {
            let info = p.player.info();
            let pos = p.player.position();
            (p.msg, p.player.paused(), pos, info.duration)
        });
        for it in self.items.iter_mut().filter(|i| i.mkind == 2 && i.file_voice) {
            match state {
                Some((msg, paused, pos, dur)) if it.mid == msg.to_string().as_str() => {
                    it.file_state = if paused { 4 } else { 3 };
                    it.file_progress = if dur > 0.0 { (pos / dur) as f32 } else { 0.0 };
                    it.file_info = format!("{} / {}", mmss(pos), mmss(dur)).into();
                }
                _ if reset && it.file_state >= 3 => {
                    it.file_state = 2;
                    it.file_progress = 0.0;
                }
                _ => {}
            }
        }
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
        if let Some((t, b)) = v.top_row.zip(v.bottom_row) {
            self.sync_previews(t..=b);
        }
        // Строка рекламы — не сообщение: в диапазон видимых не входит, но её
        // появление в окне — просмотр рекламы.
        // Один раз на показ: строка ушла из ленты — следующее появление снова считается.
        if !self.items.iter().any(|x| x.sponsored) {
            self.sp_seen = false;
        } else if !self.sp_seen && v.bottom_row.is_some_and(|b| self.items.get(b).is_some_and(|x| x.sponsored)) {
            self.sp_seen = true;
            send(ChatIntent::Sponsored(domain::sponsored::SponsoredIntent::Seen));
        }
        let msg_row = |i: usize| self.items[..=i.min(self.items.len().saturating_sub(1))].iter().rev().find(|x| !x.sponsored);
        let vis = v
            .top_row
            .zip(v.bottom_row)
            .filter(|_| !self.items.is_empty())
            .and_then(|(t, b)| Some((msg_row(t)?.key, msg_row(b)?.key)));
        if vis.is_some() && vis != self.visible_sent {
            self.visible_sent = vis;
            if let Some((t, b)) = vis {
                send(ChatIntent::Visible(t, b));
            }
        }
        let seen = v.seen_row.and_then(|b| self.items[..=b].iter().rev().find(|x| x.key > 0)).map(|x| x.key);
        if let Some(id) = seen.filter(|id| *id > self.seen_sent) {
            self.seen_sent = id;
            trace(format_args!("видно до {id}"));
            send(ChatIntent::Seen(id));
        }
    }
}

/// Ключ строки рекламы: отрицательный (не сообщение и не очередь отправки).
const SPONSORED_KEY: i32 = i32::MIN + 1;

/// Реклама в конце ленты (sponsored.rs).
fn fill_sponsored(chat: &Chat, m: &ChatModel) {
    let s = m.sponsored_here();
    chat.set_sp_title(s.map(|s| s.ad.title.clone()).unwrap_or_default().into());
    chat.set_sp_text(s.map(|s| s.ad.text.replace('\n', " ")).unwrap_or_default().into());
    chat.set_sp_button(s.map(|s| s.ad.button.clone()).unwrap_or_default().into());
    chat.set_sp_url(s.map(|s| s.ad.url.clone()).unwrap_or_default().into());
    chat.set_sp_expanded(s.is_some_and(|s| s.expanded));
    chat.set_sp_info_open(s.is_some_and(|s| s.info));
    chat.set_sp_sponsor(s.map(|s| s.ad.sponsor_info.clone()).unwrap_or_default().into());
    chat.set_sp_additional(s.map(|s| s.ad.additional_info.clone()).unwrap_or_default().into());
    let report = s.and_then(|s| s.report.as_ref());
    chat.set_sp_report_title(report.map(|(t, _)| if t.is_empty() { "…".to_string() } else { t.clone() }).unwrap_or_default().into());
    chat.set_sp_report_options(model(
        report.map(|(_, o)| o.iter().map(|(t, _)| t.clone().into()).collect()).unwrap_or_default(),
    ));
}

/// Вид чата для разметки: 0 личный, 1 группа, 2 канал.
/// Форма слова по числу: 1 файл, 2 файла, 5 файлов.
fn plural<'a>(n: usize, one: &'a str, few: &'a str, many: &'a str) -> &'a str {
    match (n % 10, n % 100) {
        (1, x) if x != 11 => one,
        (2..=4, x) if !(12..=14).contains(&x) => few,
        _ => many,
    }
}

/// «5 голосов».
fn count_votes(n: i32) -> String {
    let w = match (n % 10, n % 100) {
        (1, x) if x != 11 => "голос",
        (2..=4, x) if !(12..=14).contains(&x) => "голоса",
        _ => "голосов",
    };
    format!("{n} {w}")
}

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
                avatar: img.unwrap_or_else(no_image),
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
                        avatar: no_image(),
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
        self.fill_shared(&chat, m, avatars, ru);
        fill_sponsored(&chat, m);
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
            mimg: no_image(),
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
        let item = |t: &str| MessageItem { text: t.into(), mimg: no_image(), ..Default::default() };
        let mut a = item("привет 👋 мир");
        c.enrich(&mut a, &Message::default(), 700, text_width(700));
        assert!(a.rich && a.lines.row_count() == 1);
        let mut b = item("привет 👋 мир");
        c.enrich(&mut b, &Message::default(), 700, text_width(700));
        assert!(a == b, "тот же текст и ширина — та же модель строк (строка списка не меняется)");
        let mut narrow = item("привет 👋 мир ".repeat(8).trim());
        c.enrich(&mut narrow, &Message::default(), 200, text_width(200));
        assert!(narrow.lines.row_count() > 1, "узкая лента — перенос");
        let mut big = item("🔥 🔥");
        c.enrich(&mut big, &Message::default(), 700, text_width(700));
        assert!(!big.rich && big.big_emoji.row_count() == 2);
        let mut plain = item("без эмодзи 1 #");
        c.enrich(&mut plain, &Message::default(), 700, text_width(700));
        assert!(plain.rich && plain.big_emoji.row_count() == 0, "и без эмодзи — своей раскладкой (точная высота)");
        let mut long = item(&"─".repeat(300));
        c.enrich(&mut long, &Message::default(), 700, text_width(700));
        assert!(long.lines.row_count() > 3, "слово длиннее строки переносится по графемам");
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
        c.enrich(&mut it, &msg, 1000, text_width(1000));
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
