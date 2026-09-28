//! Модель главного окна → свойства Slint. Списки обновляются построчно
//! (`set_row_data`, добавление и удаление в конце), а не пересоздаются: иначе
//! теряются прокрутка и выделение (Архитектура/nuncio - Приложение (Rust и
//! Slint).md, раздел 3).

use std::collections::HashMap;
use std::rc::Rc;

use domain::chat::{ChatModel, Compose, PickerPurpose};
use domain::chatstore::DialogRow;
use domain::model::{Message, Peer, PeerKind};
use domain::timefmt;
use slint::{Color, ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

use crate::{avatar, App, Chat, DialogItem, MessageItem, PickerItem};

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

/// Кэш картинок аватаров (готовые круглые буферы от потока домена).
#[derive(Default)]
pub struct ChatRenderer {
    dialogs: Option<Rc<VecModel<DialogItem>>>,
    messages: Option<Rc<VecModel<MessageItem>>>,
    images: HashMap<String, Image>,
    open: Option<i64>,
    version: i32,
    picker: Option<Rc<VecModel<PickerItem>>>,
    picker_bound: bool,
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

        let dialogs = match &self.dialogs {
            Some(d) => d.clone(),
            None => {
                let d = Rc::new(VecModel::default());
                chat.set_dialogs(ModelRc::from(d.clone()));
                self.dialogs = Some(d.clone());
                d
            }
        };
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
                }
            })
            .collect();
        sync(&dialogs, items);
        chat.set_dialogs_loading(m.dialogs_loading);

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
        }
        let msgs = match &self.messages {
            Some(x) => x.clone(),
            None => {
                let x = Rc::new(VecModel::default());
                chat.set_messages(ModelRc::from(x.clone()));
                self.messages = Some(x.clone());
                x
            }
        };
        if self.open != m.open {
            self.open = m.open;
            msgs.clear(); // другой чат — окно истории заново
        }
        let group = m.open.is_some_and(|id| PeerKind::of(id) != PeerKind::User);
        let read_out = m.open_row().map(|r| r.dialog.read_outbox_max_id).unwrap_or(0);
        let cx = HistoryCtx { senders: &m.senders, quotes: &m.quotes, group, read_out, now: m.now, lang: &m.lang,
            peer: m.open_peer() };
        let items = message_items(&m.history, &cx);
        sync(&msgs, items);

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
            if chat.get_picker_items().row_count() == 0 || !self.picker_bound {
                chat.set_picker_items(ModelRc::from(model.clone()));
                self.picker_bound = true;
            }
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
        if chat.get_composer() != m.composer.as_str() && !m.sending {
            chat.set_composer(m.composer.clone().into());
        }
        chat.set_error(m.error.clone().unwrap_or_default().into());
        self.version += 1;
        chat.set_history_version(self.version);
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
