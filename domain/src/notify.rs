//! Уведомления о новых сообщениях (В9, Нс-4): что показывать. Показ — у
//! сетевого слоя (D-Bus `org.freedesktop.Notifications`).
//!
//! Уведомлять, если: уведомления включены; сообщение входящее и не служебное;
//! пришло только что (не старше минуты — после долгого отсутствия догоняющие
//! обновления не превращаются в лавину); чат не без звука; и чат не открыт в
//! видимом окне. На чат — одно уведомление, последнее заменяет прежнее.

use crate::chat::{ChatEffect, ChatModel};
use crate::model::{DomainUpdate, PeerKind, UpdatesBatch};

/// Старше этого (с) — не уведомлять.
pub const FRESH_SECS: i64 = 60;

/// Подпись вложения словом (для тела уведомления).
fn media_word(kind: &str, ru: bool) -> &'static str {
    match (kind, ru) {
        ("photo", true) => "Фото",
        ("photo", false) => "Photo",
        ("video", true) => "Видео",
        ("video", false) => "Video",
        ("voice", true) => "Голосовое сообщение",
        ("voice", false) => "Voice message",
        ("video_note", true) => "Видеосообщение",
        ("video_note", false) => "Video message",
        ("sticker", true) => "Стикер",
        ("sticker", false) => "Sticker",
        ("document", true) => "Файл",
        ("document", false) => "File",
        (_, true) => "Вложение",
        (_, false) => "Attachment",
    }
}

impl ChatModel {
    pub(crate) fn notifications(&self, b: &UpdatesBatch, out: &mut Vec<ChatEffect>) {
        let s = &self.app_settings;
        if !s.notify {
            return;
        }
        let ru = self.lang == "ru";
        let mut shown: Vec<ChatEffect> = Vec::new();
        for u in &b.updates {
            let DomainUpdate::NewMessage(m) = u else { continue };
            if m.out || !m.service.is_empty() || self.now - m.date > FRESH_SECS {
                continue;
            }
            if self.window_active && self.open == Some(m.peer_id) {
                continue;
            }
            let row = self.all_rows.iter().find(|r| r.peer.id == m.peer_id);
            if row.is_some_and(|r| r.dialog.muted || r.peer.is_self) {
                continue;
            }
            let peer = row.map(|r| &r.peer).or_else(|| b.peers.iter().find(|p| p.id == m.peer_id));
            let title = peer.map(|p| p.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| {
                if ru { "Новое сообщение".into() } else { "New message".into() }
            });
            let body = if s.notify_hide_text {
                if ru { "Новое сообщение".into() } else { "New message".into() }
            } else {
                let text = crate::chat::snippet(m);
                let text = if m.text.trim().is_empty() && !m.media.is_empty() {
                    media_word(&m.media, ru).to_string()
                } else {
                    text
                };
                // В группе — кто написал.
                let group = PeerKind::of(m.peer_id) != PeerKind::User;
                let sender = b.peers.iter().chain(self.all_rows.iter().map(|r| &r.peer)).find(|p| p.id == m.from_id);
                match sender.filter(|_| group) {
                    Some(p) if !p.name.is_empty() => format!("{}: {text}", p.name),
                    _ => text,
                }
            };
            shown.retain(|e| !matches!(e, ChatEffect::Notify { peer_id, .. } if *peer_id == m.peer_id));
            shown.push(ChatEffect::Notify { peer_id: m.peer_id, title, body, sound: s.notify_sound });
        }
        out.extend(shown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chatstore::DialogRow;
    use crate::model::{Dialog, Message, Peer};

    fn model() -> ChatModel {
        let mut m = ChatModel::new(1, "ru", 1000);
        m.all_rows = vec![
            DialogRow { peer: Peer { id: 10, name: "Анна".into(), ..Default::default() }, ..Default::default() },
            DialogRow {
                peer: Peer { id: 11, name: "Тихий".into(), ..Default::default() },
                dialog: Dialog { muted: true, ..Default::default() },
                ..Default::default()
            },
        ];
        m
    }

    fn batch(msgs: Vec<Message>) -> UpdatesBatch {
        UpdatesBatch { updates: msgs.into_iter().map(DomainUpdate::NewMessage).collect(), ..Default::default() }
    }

    fn msg(peer: i64, date: i64, text: &str) -> Message {
        Message { peer_id: peer, id: 1, date, text: text.into(), from_id: peer, ..Default::default() }
    }

    #[test]
    fn who_gets_notified() {
        let mut m = model();
        let mut out = Vec::new();
        m.notifications(&batch(vec![msg(10, 990, "привет"), msg(10, 995, "как дела"), msg(11, 999, "тихо"),
            msg(10, 100, "старое")]), &mut out);
        assert_eq!(out, vec![ChatEffect::Notify { peer_id: 10, title: "Анна".into(), body: "как дела".into(), sound: true }],
            "одно на чат — последнее; без звука и старое — нет");
        m.open = Some(10);
        out.clear();
        m.notifications(&batch(vec![msg(10, 999, "ещё")]), &mut out);
        assert!(out.is_empty(), "открытый чат в видимом окне");
        m.window_active = false;
        m.app_settings.notify_hide_text = true;
        m.notifications(&batch(vec![msg(10, 999, "секрет")]), &mut out);
        assert!(matches!(&out[..], [ChatEffect::Notify { body, .. }] if body == "Новое сообщение"), "окно скрыто, текст скрыт");
        m.app_settings.notify = false;
        out.clear();
        m.notifications(&batch(vec![msg(10, 999, "x")]), &mut out);
        assert!(out.is_empty());
    }
}
