//! Панель стикеров в поле ввода: установленные наборы и недавние, отправка
//! по щелчку (ссылкой на документ, с ответом, если он выбран). Картинки
//! стикеров панели скачиваются, пока панель открыта (`sticker_files`).

use std::collections::HashMap;

use crate::chat::{ChatEffect, ChatModel, Compose, Pending};
use crate::media::sticker_image;
use crate::model::{Command, CommandResult, FileRef, Media, Payload, StickerSetInfo};

/// Набор «Недавние» (не настоящий id набора).
pub const RECENT: i64 = 0;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StickerPanel {
    pub open: bool,
    /// Установленные наборы (запрошены — при первом открытии).
    pub sets: Vec<StickerSetInfo>,
    pub sets_requested: bool,
    /// Показываемый набор (`RECENT` — недавние).
    pub current: i64,
    /// Стикеры загруженных наборов.
    pub stickers: HashMap<i64, Vec<Media>>,
    pub loading: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StickerIntent {
    Toggle,
    Close,
    Select(i64),
    Send(usize),
}

impl ChatModel {
    pub(crate) fn sticker_intent(&mut self, i: StickerIntent, out: &mut Vec<ChatEffect>) {
        match i {
            StickerIntent::Toggle if self.sticker_panel.open => self.sticker_panel.open = false,
            StickerIntent::Toggle => {
                self.sticker_panel.open = true;
                if !self.sticker_panel.sets_requested {
                    self.sticker_panel.sets_requested = true;
                    self.send(Command::GetStickerSets, Pending::StickerSets, out);
                }
                let cur = self.sticker_panel.current;
                self.load_sticker_set(cur, out);
            }
            StickerIntent::Close => self.sticker_panel.open = false,
            StickerIntent::Select(id) => {
                self.sticker_panel.current = id;
                self.load_sticker_set(id, out);
            }
            StickerIntent::Send(n) => {
                let Some(file) = self.sticker_panel.stickers.get(&self.sticker_panel.current)
                    .and_then(|v| v.get(n)).and_then(|m| m.file.clone()) else { return };
                let Some(peer) = self.open.and_then(|p| self.peer_ref(p)) else { return };
                let reply_to = match std::mem::take(&mut self.compose) {
                    Compose::Reply { id, .. } => id,
                    other => {
                        self.compose = other;
                        0
                    }
                };
                let random_id = self.random_id();
                self.send(Command::SendSticker { peer, file, random_id, reply_to }, Pending::SendSticker, out);
            }
        }
    }

    fn load_sticker_set(&mut self, id: i64, out: &mut Vec<ChatEffect>) {
        if self.sticker_panel.stickers.contains_key(&id) || self.sticker_panel.loading == Some(id) {
            self.pump_media(out);
            return;
        }
        let access_hash = self.sticker_panel.sets.iter().find(|s| s.id == id).map_or(0, |s| s.access_hash);
        self.sticker_panel.loading = Some(id);
        self.send(Command::GetStickerSet { id, access_hash }, Pending::StickerSet { id }, out);
    }

    pub(crate) fn sticker_sets_result(&mut self, r: &CommandResult) {
        if let (None, Payload::StickerSets(sets)) = (&r.error, &r.payload) {
            self.sticker_panel.sets = sets.clone();
        }
    }

    pub(crate) fn sticker_set_result(&mut self, id: i64, r: &CommandResult, out: &mut Vec<ChatEffect>) {
        if self.sticker_panel.loading == Some(id) {
            self.sticker_panel.loading = None;
        }
        match (&r.error, &r.payload) {
            (None, Payload::Stickers { stickers, .. }) => {
                self.sticker_panel.stickers.insert(id, stickers.clone());
                self.media_window_changed(out);
                self.pump_media(out);
            }
            (Some(e), _) => {
                let t = crate::errors::error_text(e, self.lang == "ru");
                self.toast(&t);
            }
            _ => {}
        }
    }

    pub(crate) fn sticker_sent(&mut self, r: &CommandResult) {
        if let Some(e) = &r.error {
            let ru = self.lang == "ru";
            let why = crate::errors::error_text(e, ru);
            let t = if ru { format!("Стикер не отправлен: {why}") } else { format!("Sticker not sent: {why}") };
            self.toast(&t);
        }
    }

    /// Стикеры открытого набора панели (пусто — панель закрыта или не загружено).
    pub fn panel_stickers(&self) -> &[Media] {
        if !self.sticker_panel.open {
            return &[];
        }
        self.sticker_panel.stickers.get(&self.sticker_panel.current).map_or(&[], Vec::as_slice)
    }

    /// Картинки стикеров открытой панели (для скачивания и показа).
    pub fn sticker_files(&self) -> Vec<FileRef> {
        self.panel_stickers().iter().filter_map(sticker_image).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::ChatIntent;
    use crate::model::{Event, MediaKind, Peer};

    fn sends(out: &[ChatEffect]) -> Vec<(u64, Command)> {
        out.iter().filter_map(|e| if let ChatEffect::Send { id, cmd } = e { Some((*id, cmd.clone())) } else { None })
            .collect()
    }

    fn reply(id: u64, payload: Payload) -> Event {
        Event::Result(CommandResult { cmd_id: id, error: None, payload })
    }

    fn sticker(id: i64) -> Media {
        Media {
            kind: MediaKind::Sticker,
            mime: "image/webp".into(),
            file: Some(FileRef { id, access_hash: 3, ..Default::default() }),
            ..Default::default()
        }
    }

    #[test]
    fn panel_loads_and_sends_with_reply() {
        let mut m = ChatModel::new(1, "ru", 1000);
        m.extra_peers.insert(10, Peer { id: 10, access_hash: 30, ..Default::default() });
        m.open = Some(10);
        let fx = m.intent(ChatIntent::Stickers(StickerIntent::Toggle));
        let s = sends(&fx);
        assert!(matches!(&s[..], [(_, Command::GetStickerSets), (_, Command::GetStickerSet { id: RECENT, .. })]));
        m.event(&reply(s[0].0, Payload::StickerSets(vec![StickerSetInfo { id: 7, access_hash: 70, title: "Т".into(),
            count: 2 }])));
        m.event(&reply(s[1].0, Payload::Stickers { set_id: RECENT, stickers: vec![sticker(5)] }));
        assert_eq!(m.panel_stickers().len(), 1);
        let fx = m.intent(ChatIntent::Stickers(StickerIntent::Select(7)));
        assert!(matches!(&sends(&fx)[..], [(_, Command::GetStickerSet { id: 7, access_hash: 70 })]), "набор по id и хэшу");
        m.intent(ChatIntent::Stickers(StickerIntent::Select(RECENT)));
        m.compose = Compose::Reply { id: 42, author: String::new(), snippet: String::new() };
        let fx = m.intent(ChatIntent::Stickers(StickerIntent::Send(0)));
        assert!(matches!(&sends(&fx)[..], [(_, Command::SendSticker { reply_to: 42, ref file, .. })] if file.id == 5));
        assert_eq!(m.compose, Compose::Plain, "ответ израсходован");
    }
}
