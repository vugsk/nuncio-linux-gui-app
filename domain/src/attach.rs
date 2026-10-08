//! Отправка вложений (В7, Св-4): системный диалог выбора файлов, панель
//! подготовки над полем ввода (подпись — само поле ввода, Дз-17), очередь
//! отправки с альбомами до 10 файлов; действия окна просмотра (сохранить,
//! переслать, показать в чате).

use crate::chat::{ChatEffect, ChatIntent, ChatModel, DbOp, Pending};
use crate::media::Transfer;
use crate::model::{Command, CommandResult, Entity, ErrorKind, MediaUpload, Payload, PeerRef};
use crate::outbox::{OutMedia, OutboxItem};

/// Файлов в одном альбоме (предел Telegram).
pub const ALBUM_MAX: usize = 10;
/// «Как фото» — не больше 10 МБ и сумма сторон до 10 000 px (правила Telegram).
pub const PHOTO_MAX_SIZE: i64 = 10 * 1024 * 1024;

/// Выбранный файл: что о нём узнали на диске (размер, картинка ли, стороны).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PrepFile {
    pub path: String,
    pub name: String,
    pub mime: String,
    pub size: i64,
    pub image: bool,
    pub width: i32,
    pub height: i32,
}

impl PrepFile {
    /// Можно ли отправить «как фото».
    pub fn photo_ok(&self) -> bool {
        let (w, h) = (self.width, self.height);
        self.image && self.size <= PHOTO_MAX_SIZE && w > 0 && h > 0 && w + h <= 10_000 && w.max(h) <= 20 * w.min(h)
    }
}

/// Панель подготовки (Св-4).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Prepare {
    pub files: Vec<PrepFile>,
    pub as_photo: bool,
}

impl Prepare {
    pub fn can_photo(&self) -> bool {
        !self.files.is_empty() && self.files.iter().all(PrepFile::photo_ok)
    }
}

/// Файл для постановки в очередь.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnqueueItem {
    pub media: OutMedia,
    pub text: String,
    pub entities: Vec<Entity>,
    pub random_id: i64,
}

/// MIME по расширению (сервер сам различает содержимое; нужно для документа).
pub fn mime_of(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "txt" => "text/plain",
        "mp4" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "flac" => "audio/flac",
        _ => "application/octet-stream",
    }
}

/// Ключ хода выгрузки своего неотправленного в `transfers`.
pub fn upload_key(local_id: i64) -> String {
    format!("up:{local_id}")
}

impl ChatModel {
    /// Скрепка или «+ ещё»: системный диалог выбора файлов.
    pub(crate) fn attach(&mut self, append: bool, out: &mut Vec<ChatEffect>) {
        if self.open.is_none() {
            return;
        }
        let title = if self.lang == "ru" { "Выберите файлы" } else { "Choose files" }.to_string();
        self.send(Command::PickFiles { multiple: true, title }, Pending::Pick { append }, out);
    }

    pub(crate) fn picked(&mut self, append: bool, r: &CommandResult, out: &mut Vec<ChatEffect>) {
        match (&r.error, &r.payload) {
            (None, Payload::FilePaths(paths)) if !paths.is_empty() => {
                out.push(ChatEffect::InspectFiles { paths: paths.clone(), append });
            }
            (Some(e), _) => {
                let t = crate::errors::error_text(e, self.lang == "ru");
                self.toast(&t);
            }
            _ => {}
        }
    }

    /// Файлы осмотрены (приложение): открыть панель подготовки или дополнить её.
    pub fn set_prepared(&mut self, files: Vec<PrepFile>, append: bool) {
        let mut p = if append { self.prepare.take().unwrap_or_default() } else { Prepare::default() };
        let had_photo = append && p.as_photo;
        for f in files {
            if !p.files.iter().any(|x| x.path == f.path) {
                p.files.push(f);
            }
        }
        // «Как фото» по умолчанию, если все — картинки (сжатие как в Telegram).
        p.as_photo = p.can_photo() && (had_photo || !append || p.files.len() == 1);
        self.prepare = if p.files.is_empty() { None } else { Some(p) };
    }

    pub(crate) fn prepare_remove(&mut self, i: usize) {
        if let Some(p) = self.prepare.as_mut() {
            if i < p.files.len() {
                p.files.remove(i);
            }
            p.as_photo &= p.can_photo();
            if p.files.is_empty() {
                self.prepare = None;
            }
        }
    }

    pub(crate) fn prepare_as_photo(&mut self, on: bool) {
        if let Some(p) = self.prepare.as_mut() {
            p.as_photo = on && p.can_photo();
        }
    }

    /// «Отправить»: подпись — текст поля ввода; по 10 файлов на альбом.
    pub(crate) fn send_prepared(&mut self, caption: String, entities: Vec<Entity>, reply_to: i32) -> Vec<ChatEffect> {
        let mut out = Vec::new();
        let (Some(p), Some(peer_id)) = (self.prepare.take(), self.open) else { return out };
        let as_photo = p.as_photo && p.can_photo();
        for (ci, chunk) in p.files.chunks(ALBUM_MAX).enumerate() {
            let group = if chunk.len() > 1 { self.random_id() } else { 0 };
            let items = chunk
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let first = ci == 0 && i == 0;
                    EnqueueItem {
                        media: OutMedia {
                            path: f.path.clone(),
                            name: f.name.clone(),
                            mime: f.mime.clone(),
                            as_photo,
                            size: f.size,
                            width: f.width,
                            height: f.height,
                        },
                        text: if first { caption.clone() } else { String::new() },
                        entities: if first { entities.clone() } else { Vec::new() },
                        random_id: self.random_id(),
                    }
                })
                .collect();
            let reply_to = if ci == 0 { reply_to } else { 0 };
            out.push(ChatEffect::Db(DbOp::EnqueueMedia { peer_id, items, reply_to, group }));
        }
        self.after_enqueue(peer_id, &mut out);
        out
    }

    /// Готовые вложения очереди (одно или альбом) — одной командой.
    pub(crate) fn send_media_items(&mut self, items: Vec<OutboxItem>, out: &mut Vec<ChatEffect>) {
        let Some(first) = items.first() else { return };
        let peer = PeerRef { id: first.peer_id, access_hash: first.access_hash };
        let reply_to = first.reply_to;
        let id = self.peek_id();
        let total: i64 = items.iter().filter_map(|i| i.media.as_ref()).map(|m| m.size).sum();
        let mut local_ids = Vec::with_capacity(items.len());
        let mut uploads = Vec::with_capacity(items.len());
        for it in items {
            let Some(m) = it.media else { continue };
            out.push(ChatEffect::Db(DbOp::OutboxSending { local_id: it.local_id }));
            self.transfers.insert(upload_key(it.local_id), Transfer { cmd_id: id, done: 0, total });
            local_ids.push(it.local_id);
            uploads.push(MediaUpload {
                path: m.path,
                name: m.name,
                mime: m.mime,
                as_photo: m.as_photo,
                random_id: it.random_id,
                caption: it.text,
                entities: it.entities,
            });
        }
        self.send(Command::SendMedia { peer, items: uploads, reply_to }, Pending::OutboxMedia { local_ids }, out);
    }

    /// Ответ на отправку вложений: как у текста (О-9), но на все файлы альбома.
    pub(crate) fn media_sent(&mut self, local_ids: Vec<i64>, r: &CommandResult, out: &mut Vec<ChatEffect>) {
        for l in &local_ids {
            self.transfers.remove(&upload_key(*l));
        }
        let each = |f: &dyn Fn(i64) -> DbOp, out: &mut Vec<ChatEffect>| {
            for l in &local_ids {
                out.push(ChatEffect::Db(f(*l)));
            }
        };
        let now = self.now;
        match (&r.error, &r.payload) {
            (None, Payload::SentList(msgs)) => {
                for (i, l) in local_ids.iter().enumerate() {
                    let sent = msgs.get(i).filter(|m| m.id > 0).cloned();
                    out.push(ChatEffect::Db(DbOp::OutboxDone { local_id: *l, sent }));
                }
            }
            (None, _) => each(&|local_id| DbOp::OutboxDone { local_id, sent: None }, out),
            (Some(e), _) if e.text == "RANDOM_ID_DUPLICATE" => each(&|local_id| DbOp::OutboxDone { local_id, sent: None }, out),
            (Some(e), _) if e.kind == ErrorKind::Flood => {
                let next_try = now + i64::from(e.retry_after.max(1));
                each(&|local_id| DbOp::OutboxRequeue { local_id, next_try }, out);
            }
            // Отмена пользователем — «не отправлено»: можно повторить или удалить.
            (Some(e), _) if e.kind == ErrorKind::Cancelled => {
                each(&|local_id| DbOp::OutboxFail { local_id, error: "cancelled".into() }, out);
            }
            (Some(e), _) if matches!(e.kind, ErrorKind::Network | ErrorKind::Internal) || e.code == 500 || e.code == -500 => {
                each(&|local_id| DbOp::OutboxRequeue { local_id, next_try: now + 5 }, out);
            }
            (Some(e), _) => {
                let error = e.text.clone();
                each(&|local_id| DbOp::OutboxFail { local_id, error: error.clone() }, out);
            }
        }
        self.reload_open(out);
        out.push(ChatEffect::Db(DbOp::ReloadRows));
        out.push(ChatEffect::Db(DbOp::PumpOutbox));
    }

    /// Нажатие на своё неотправленное вложение во время выгрузки — отмена.
    pub(crate) fn cancel_upload(&mut self, msg_id: i32, out: &mut Vec<ChatEffect>) {
        if let Some(t) = self.transfers.get(&upload_key(-i64::from(msg_id))) {
            let cmd_id = t.cmd_id;
            self.send(Command::CancelTransfer { cmd_id }, Pending::Other, out);
        }
    }

    /// Окно просмотра, «сохранить»: системный диалог сохранения (Св-3).
    pub(crate) fn viewer_save(&mut self, out: &mut Vec<ChatEffect>) {
        let ru = self.lang == "ru";
        let Some(id) = self.viewer else { return };
        if self.find_msg(id).and_then(|m| m.media_info.as_ref()).is_some_and(|mi| mi.self_destruct()) {
            self.toast(if ru { "Самоуничтожающееся нельзя сохранить" } else { "Self-destructing media can't be saved" });
            return;
        }
        let Some(path) = self.viewer_key().and_then(|k| self.media_paths.get(&k).cloned()) else {
            self.toast(if ru { "Фото ещё загружается" } else { "Photo is still loading" });
            return;
        };
        let ext = path.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_else(|| "jpg".into());
        let name = format!("photo_{id}.{ext}");
        let title = if ru { "Сохранить фото" } else { "Save photo" }.to_string();
        self.send(Command::SaveFileAs { src: path, name, title }, Pending::Save, out);
    }

    pub(crate) fn saved(&mut self, r: &CommandResult) {
        let ru = self.lang == "ru";
        match (&r.error, &r.payload) {
            (None, Payload::File(p)) if !p.is_empty() => {
                let t = if ru { format!("Сохранено: {p}") } else { format!("Saved: {p}") };
                self.toast(&t);
            }
            (Some(e), _) => {
                let why = crate::errors::error_text(e, ru);
                let t = if ru { format!("Не сохранено: {why}") } else { format!("Not saved: {why}") };
                self.toast(&t);
            }
            _ => {}
        }
    }

    /// «Показать в чате» и «переслать» из окна просмотра: окно закрывается.
    pub(crate) fn viewer_to_chat(&mut self, forward: bool) -> Vec<ChatEffect> {
        let Some(id) = self.viewer.take() else { return Vec::new() };
        if forward {
            self.intent(ChatIntent::Forward(vec![id]))
        } else {
            self.intent(ChatIntent::JumpTo(id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ErrorInfo, Peer};

    fn model() -> ChatModel {
        let mut m = ChatModel::new(1, "ru", 1000);
        m.open = Some(10);
        m.extra_peers.insert(10, Peer { id: 10, access_hash: 30, ..Default::default() });
        m
    }

    fn img(n: &str) -> PrepFile {
        PrepFile { path: format!("/p/{n}"), name: n.into(), mime: mime_of(n).into(), size: 1000, image: true, width: 800,
            height: 600 }
    }

    #[test]
    fn prepare_and_send_album_in_chunks() {
        let mut m = model();
        m.set_prepared(vec![img("a.jpg"), img("b.png")], false);
        assert!(m.prepare.as_ref().unwrap().as_photo, "картинки — как фото");
        m.set_prepared((0..10).map(|i| img(&format!("{i}.jpg"))).collect(), true);
        assert_eq!(m.prepare.as_ref().unwrap().files.len(), 12);
        let fx = m.send_prepared("подпись".into(), vec![], 7);
        let enq: Vec<_> = fx
            .iter()
            .filter_map(|e| match e {
                ChatEffect::Db(DbOp::EnqueueMedia { items, reply_to, group, .. }) => Some((items.clone(), *reply_to, *group)),
                _ => None,
            })
            .collect();
        assert_eq!(enq.len(), 2, "12 файлов — альбомы по 10 и 2");
        assert_eq!((enq[0].0.len(), enq[1].0.len()), (10, 2));
        assert!(enq[0].2 != 0 && enq[1].2 != 0 && enq[0].2 != enq[1].2);
        assert_eq!((enq[0].0[0].text.as_str(), enq[0].1, enq[1].1), ("подпись", 7, 0), "подпись и ответ — у первого");
        assert!(enq[0].0.iter().all(|i| i.media.as_photo));
        assert!(m.prepare.is_none());
    }

    #[test]
    fn non_image_forces_file_and_remove() {
        let mut m = model();
        let doc = PrepFile { path: "/p/a.pdf".into(), name: "a.pdf".into(), mime: "application/pdf".into(), size: 5, ..Default::default() };
        m.set_prepared(vec![img("a.jpg"), doc], false);
        assert!(!m.prepare.as_ref().unwrap().as_photo);
        m.prepare_as_photo(true);
        assert!(!m.prepare.as_ref().unwrap().as_photo, "с документом — только как файл");
        m.prepare_remove(1);
        m.prepare_as_photo(true);
        assert!(m.prepare.as_ref().unwrap().as_photo);
        m.prepare_remove(0);
        assert!(m.prepare.is_none());
        let big = PrepFile { size: PHOTO_MAX_SIZE + 1, ..img("big.jpg") };
        assert!(!big.photo_ok());
    }

    #[test]
    fn upload_progress_cancel_and_result() {
        let mut m = model();
        let item = |l: i64| OutboxItem { local_id: l, peer_id: 10, access_hash: 30, random_id: l * 10, text: String::new(),
            reply_to: 0, entities: vec![], media: Some(OutMedia { path: "/p/a.jpg".into(), size: 100, as_photo: true,
                ..Default::default() }), group: 5 };
        let fx = m.set_outbox_ready(vec![item(1), item(2)]);
        let sends: Vec<_> = fx.iter().filter_map(|e| match e { ChatEffect::Send { id, cmd } => Some((*id, cmd.clone())), _ => None }).collect();
        assert_eq!(sends.len(), 1, "альбом — одной командой");
        let (cmd_id, Command::SendMedia { items, .. }) = sends[0].clone() else { panic!() };
        assert_eq!(items.len(), 2);
        assert_eq!(m.transfers[&upload_key(2)].total, 200);
        assert!(m.set_outbox_ready(vec![item(1), item(2)]).is_empty(), "уже отправляется");
        m.transfer_progress(cmd_id, 150, 200);
        assert_eq!((m.transfers[&upload_key(1)].done, m.transfers[&upload_key(2)].done), (150, 150));
        let mut out = Vec::new();
        m.cancel_upload(-2, &mut out);
        assert!(matches!(&out[..], [ChatEffect::Send { cmd: Command::CancelTransfer { cmd_id: c }, .. }] if *c == cmd_id));
        let r = CommandResult { cmd_id, error: Some(ErrorInfo { kind: ErrorKind::Cancelled, code: 0, text: "отменено".into(), retry_after: 0 }), payload: Payload::None };
        let fx = m.event(&crate::model::Event::Result(r));
        let fails = fx.iter().filter(|e| matches!(e, ChatEffect::Db(DbOp::OutboxFail { error, .. }) if error == "cancelled")).count();
        assert_eq!(fails, 2, "отмена — «не отправлено» у всех файлов альбома");
        assert!(m.transfers.is_empty());
    }
}
