//! Вложения (В7): индекс кэша файлов в базе и логика скачивания в модели
//! чата. Файлы пишет Go (`cache/media`), Rust ведёт индекс: что скачано,
//! размер, когда нужно было; по нему вытесняет старое сверх лимита и чистит
//! кэш по команде (Данные и хранилище, раздел 5).

use std::collections::{HashMap, HashSet};

use rusqlite::params;

use crate::chat::{ChatEffect, ChatModel, DbOp, Pending};
use crate::model::{Command, FileRef, Media, MediaKind, Message};
use crate::store::{Store, StoreError};

/// Одновременных автоскачиваний (фото и превью в окне истории).
pub const AUTO_PARALLEL: usize = 4;
/// Автоскачивание — только небольших файлов (фото для ленты, превью).
pub const AUTO_MAX_SIZE: i64 = 3 * 1024 * 1024;
/// Лимит кэша по умолчанию (настройка — В9).
pub const CACHE_LIMIT: i64 = 1024 * 1024 * 1024;

impl Store {
    /// Пути скачанных файлов по ключам.
    pub fn media_paths(&self, keys: &[String]) -> Result<HashMap<String, String>, StoreError> {
        let mut st = self.conn().prepare_cached("SELECT cache_path FROM media WHERE file_id = ?1 AND state = 1")?;
        let mut out = HashMap::new();
        for k in keys {
            if let Ok(p) = st.query_row(params![k], |r| r.get::<_, String>(0)) {
                if std::path::Path::new(&p).exists() {
                    out.insert(k.clone(), p);
                }
            }
        }
        Ok(out)
    }

    /// Файл скачан (или снова понадобился).
    pub fn media_saved(&self, key: &str, path: &str, size: i64, now: i64) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO media (file_id, kind, size, cache_path, state, last_used) VALUES (?1, 0, ?2, ?3, 1, ?4)
             ON CONFLICT(file_id) DO UPDATE SET size = ?2, cache_path = ?3, state = 1, last_used = ?4",
            params![key, size, path, now],
        )?;
        Ok(())
    }

    pub fn media_touch(&self, keys: &[String], now: i64) -> Result<(), StoreError> {
        let mut st = self.conn().prepare_cached("UPDATE media SET last_used = ?2 WHERE file_id = ?1")?;
        for k in keys {
            st.execute(params![k, now])?;
        }
        Ok(())
    }

    pub fn media_total(&self) -> Result<i64, StoreError> {
        Ok(self.conn().query_row("SELECT COALESCE(SUM(size), 0) FROM media WHERE state = 1", [], |r| r.get(0))?)
    }

    /// Вытеснить давно не нужное сверх лимита (кроме `keep` — видно сейчас).
    /// Возвращает пути файлов, которые надо удалить с диска.
    pub fn media_evict(&mut self, limit: i64, keep: &HashSet<String>) -> Result<Vec<String>, StoreError> {
        let mut total = self.media_total()?;
        if total <= limit {
            return Ok(Vec::new());
        }
        let tx = self.conn_mut().transaction()?;
        let mut victims = Vec::new();
        {
            let mut st = tx.prepare("SELECT file_id, cache_path, size FROM media WHERE state = 1 ORDER BY last_used")?;
            let rows: Vec<(String, String, i64)> =
                st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
            for (key, path, size) in rows {
                if total <= limit {
                    break;
                }
                if keep.contains(&key) {
                    continue;
                }
                victims.push((key, path));
                total -= size;
            }
        }
        for (key, _) in &victims {
            tx.execute("DELETE FROM media WHERE file_id = ?1", params![key])?;
        }
        tx.commit()?;
        Ok(victims.into_iter().map(|(_, p)| p).collect())
    }

    /// Очистить кэш целиком: пути файлов для удаления.
    pub fn media_clear(&mut self) -> Result<Vec<String>, StoreError> {
        let tx = self.conn_mut().transaction()?;
        let paths: Vec<String> = {
            let mut st = tx.prepare("SELECT cache_path FROM media")?;
            let v = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
            v
        };
        tx.execute("DELETE FROM media", [])?;
        tx.commit()?;
        Ok(paths)
    }
}

/// Скачивание в работе.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub cmd_id: u64,
    pub done: i64,
    pub total: i64,
}

/// Файлы вложения, которые показываются в ленте без действий пользователя.
pub fn auto_files(m: &Media) -> Vec<&FileRef> {
    let mut v = Vec::new();
    // Самоуничтожающееся — только по нажатию и без кэша.
    if m.self_destruct() {
        return v;
    }
    if m.kind == MediaKind::Sticker {
        v.extend(sticker_image(m).filter(|f| f.size <= AUTO_MAX_SIZE));
        // Видеостикер и TGS играют в ленте сами — сам файл (маленький).
        if matches!(m.mime.as_str(), "video/webm" | "application/x-tgsticker") {
            v.extend(m.file.as_ref().filter(|f| f.size <= AUTOPLAY_MAX_SIZE));
        }
        return v;
    }
    match m.kind {
        MediaKind::Photo => v.extend(m.file.as_ref()),
        MediaKind::Video | MediaKind::Document | MediaKind::VideoNote | MediaKind::Audio | MediaKind::Gif => {
            v.extend(m.thumb.as_ref())
        }
        MediaKind::WebPage => v.extend(m.webpage.as_ref().and_then(|w| w.photo.as_ref())),
        _ => {}
    }
    v.retain(|f| f.size <= AUTO_MAX_SIZE);
    // GIF и кружки играют в ленте сами (без звука) — сам файл, если небольшой.
    if matches!(m.kind, MediaKind::Gif | MediaKind::VideoNote) {
        v.extend(m.file.as_ref().filter(|f| f.size <= AUTOPLAY_MAX_SIZE));
    }
    v
}

/// GIF и кружки до этого размера скачиваются сами и играют в ленте.
pub const AUTOPLAY_MAX_SIZE: i64 = 8 * 1024 * 1024;

/// Сторона кружка (видеосообщения) в ленте, px.
pub const ROUND_SIZE: i32 = 240;

impl ChatModel {
    /// Ключи файлов, нужных окну истории (для поиска в кэше).
    pub fn window_media_keys(&self) -> Vec<String> {
        let mut keys = Vec::new();
        keys.extend(self.sticker_files().iter().map(|f| f.key()));
        for m in self.history.iter().chain(self.shared_images_src()) {
            if let Some(mi) = &m.media_info {
                keys.extend(auto_files(mi).into_iter().map(|f| f.key()));
                if let Some(f) = &mi.file {
                    keys.push(f.key());
                }
            }
        }
        keys.sort();
        keys.dedup();
        keys
    }

    /// Результат `MediaLookup`: что из окна уже в кэше; остальное — в очередь.
    pub fn set_media_paths(&mut self, found: HashMap<String, String>) -> Vec<ChatEffect> {
        self.media_paths.extend(found);
        let mut out = Vec::new();
        self.pump_media(&mut out);
        out
    }

    /// Автоскачивание фото и превью окна истории (новые снизу — первыми).
    pub(crate) fn pump_media(&mut self, out: &mut Vec<ChatEffect>) {
        let Some(peer_id) = self.open else { return };
        let Some(peer) = self.peer_ref(peer_id) else { return };
        let mut want: Vec<(FileRef, i32, String)> = Vec::new();
        for m in self.history.iter().rev().chain(self.shared_images_src()) {
            let Some(mi) = &m.media_info else { continue };
            for f in auto_files(mi) {
                let k = f.key();
                if !self.media_paths.contains_key(&k) && !self.transfers.contains_key(&k)
                    && !self.media_failed.contains(&k) && !want.iter().any(|w| w.0.key() == k)
                {
                    want.push((f.clone(), m.id, mi.name.clone()));
                }
            }
        }
        // Панель стикеров: картинки открытого набора.
        for f in self.sticker_files() {
            let k = f.key();
            if !self.media_paths.contains_key(&k) && !self.transfers.contains_key(&k) && !self.media_failed.contains(&k)
                && !want.iter().any(|w| w.0.key() == k)
            {
                want.push((f, 0, String::new()));
            }
        }
        let running = self.transfers.len();
        for (file, msg_id, name) in want.into_iter().take(AUTO_PARALLEL.saturating_sub(running)) {
            self.start_download(file, name, peer, msg_id, out);
        }
    }

    fn start_download(&mut self, file: FileRef, name: String, peer: crate::model::PeerRef, msg_id: i32,
        out: &mut Vec<ChatEffect>) {
        let key = file.key();
        let id = self.peek_id();
        self.transfers.insert(key.clone(), Transfer { cmd_id: id, done: 0, total: file.size });
        let size = file.size;
        self.send(Command::DownloadFile { file, name, peer, msg_id }, Pending::Download { key, size }, out);
    }

    /// Нажатие на вложение: скачать, отменить или открыть скачанное.
    pub(crate) fn media_click(&mut self, msg_id: i32, out: &mut Vec<ChatEffect>) {
        if msg_id < 0 {
            self.cancel_upload(msg_id, out);
            return;
        }
        let Some(m) = self.find_msg(msg_id).cloned() else { return };
        let Some(mi) = m.media_info.clone() else { return };
        let Some(peer_id) = self.open else { return };
        let Some(peer) = self.peer_ref(peer_id) else { return };
        if mi.self_destruct() {
            let ru = self.lang == "ru";
            if mi.expired() {
                self.toast(if ru { "Срок просмотра истёк" } else { "This media has expired" });
                return;
            }
            // С первого открытия у получателя идёт срок (сервер потом удалит).
            if !m.out && self.contents_read.insert((peer_id, msg_id)) {
                self.send(Command::ReadContents { peer, ids: vec![msg_id] }, Pending::Other, out);
            }
            for f in [&mi.full, &mi.file].into_iter().flatten() {
                self.ephemeral.entry(f.key()).or_insert(None);
            }
        }
        // Фото и видео — окно просмотра (файл полного размера или само видео
        // скачивается там; видео играет, когда скачано — В8).
        if viewable(&mi) {
            self.viewer = Some(msg_id);
            self.viewer_seq += 1;
            if let Some(full) = mi.full.clone().or(mi.file.clone()) {
                let k = full.key();
                if !self.media_paths.contains_key(&k) && !self.transfers.contains_key(&k) {
                    self.start_download(full, String::new(), peer, msg_id, out);
                }
            }
            return;
        }
        let Some(file) = mi.file.clone() else { return };
        let key = file.key();
        if matches!(mi.kind, MediaKind::Voice | MediaKind::Audio | MediaKind::VideoNote) && !self.transfers.contains_key(&key) {
            // Голосовое и аудио — играть в ленте; не скачано — сначала скачать.
            self.audio_play = Some(msg_id);
            self.audio_seq += 1;
            if !self.media_paths.contains_key(&key) {
                self.media_failed.remove(&key);
                self.start_download(file, mi.name.clone(), peer, msg_id, out);
            }
            return;
        }
        if let Some(t) = self.transfers.get(&key) {
            // Второе нажатие во время скачивания — отмена.
            let cmd_id = t.cmd_id;
            self.send(Command::CancelTransfer { cmd_id }, Pending::Other, out);
            return;
        }
        match self.media_paths.get(&key) {
            Some(path) => out.push(ChatEffect::OpenFile(path.clone())),
            None => {
                self.media_failed.remove(&key);
                self.start_download(file, mi.name.clone(), peer, msg_id, out);
            }
        }
    }

    pub(crate) fn vote(&mut self, msg_id: i32, option: usize, out: &mut Vec<ChatEffect>) {
        let Some(peer_id) = self.open else { return };
        let Some(peer) = self.peer_ref(peer_id) else { return };
        let Some(poll) = self.history.iter().find(|m| m.id == msg_id).and_then(|m| m.media_info.as_ref())
            .and_then(|mi| mi.poll.clone()) else { return };
        if poll.closed || poll.options.iter().any(|o| o.chosen) {
            return;
        }
        if let Some(o) = poll.options.get(option) {
            self.send(Command::SendVote { peer, msg_id, options: vec![o.option.clone()] }, Pending::Vote { peer, msg_id }, out);
        }
    }

    /// Ход передачи.
    pub fn transfer_progress(&mut self, cmd_id: u64, done: i64, total: i64) {
        // У альбома одна команда на несколько сообщений: ход у всех.
        for t in self.transfers.values_mut().filter(|t| t.cmd_id == cmd_id) {
            t.done = done;
            if total > 0 {
                t.total = total;
            }
        }
    }

    /// Скачивание закончилось (путь или ошибка).
    pub(crate) fn download_done(&mut self, key: String, size: i64, path: Option<String>, out: &mut Vec<ChatEffect>) {
        self.transfers.remove(&key);
        match path {
            // Самоуничтожающееся: не в кэш, удалится после просмотра.
            Some(p) if self.ephemeral.contains_key(&key) => {
                self.ephemeral.insert(key.clone(), Some(p.clone()));
                self.media_paths.insert(key, p);
            }
            Some(p) => {
                out.push(ChatEffect::Db(DbOp::MediaSaved { key: key.clone(), path: p.clone(), size }));
                self.media_paths.insert(key, p);
            }
            None => {
                self.media_failed.insert(key);
            }
        }
        self.pump_media(out);
    }

    /// Окно сменилось: пути нужных файлов — из базы; старые пути забываются.
    pub(crate) fn media_window_changed(&mut self, out: &mut Vec<ChatEffect>) {
        let keys = self.window_media_keys();
        let viewer = self.viewer_key();
        let keep: HashSet<&String> = keys.iter().chain(viewer.as_ref()).collect();
        self.media_paths.retain(|k, _| keep.contains(k));
        if !keys.is_empty() {
            out.push(ChatEffect::Db(DbOp::MediaLookup { keys }));
        }
        self.refetch_incomplete(out);
    }

    /// Сообщения окна, сохранённые неполно: вложение без описания (из базы до
    /// подробных вложений) или совсем пустое, — один раз перечитать с сервера
    /// (только чтение, getMessages); ответ сохраняется и окно перерисовывается.
    fn refetch_incomplete(&mut self, out: &mut Vec<ChatEffect>) {
        let Some(peer_id) = self.open else { return };
        let Some(peer) = self.peer_ref(peer_id) else { return };
        let ids: Vec<i32> = self
            .history
            .iter()
            .filter(|m| m.id > 0 && m.service.is_empty() && m.media_info.is_none())
            .filter(|m| !m.media.is_empty() || m.text.is_empty())
            .map(|m| m.id)
            .filter(|id| !self.refetched.contains(&(peer_id, *id)))
            .take(100)
            .collect();
        if ids.is_empty() {
            return;
        }
        self.refetched.extend(ids.iter().map(|id| (peer_id, *id)));
        self.send(Command::LoadMessages { peer, ids }, Pending::Quotes { peer_id }, out);
    }

    /// Удалить скачанные самоуничтожающиеся файлы (окно просмотра закрыто,
    /// чат сменился). Ещё качающиеся удалятся, когда докачаются и снова сюда.
    pub(crate) fn purge_ephemeral(&mut self, out: &mut Vec<ChatEffect>) {
        let done: Vec<String> = self.ephemeral.iter().filter(|(_, p)| p.is_some()).map(|(k, _)| k.clone()).collect();
        for k in done {
            if let Some(Some(p)) = self.ephemeral.remove(&k) {
                self.media_paths.remove(&k);
                out.push(ChatEffect::DeleteFile(p));
            }
        }
    }

    /// Ключ файла, открытого в окне просмотра.
    pub fn viewer_key(&self) -> Option<String> {
        let m = self.find_msg(self.viewer?)?;
        let mi = m.media_info.as_ref()?;
        mi.full.as_ref().or(mi.file.as_ref()).or(mi.webpage.as_ref().and_then(|w| w.photo.as_ref())).map(|f| f.key())
    }

    /// Сообщения около видимых (запас `NEAR` с каждой стороны): только их
    /// картинки держатся раскодированными. Пока вид не сообщил — последние.
    pub fn near_visible(&self) -> HashSet<i32> {
        const NEAR: usize = 10;
        let n = self.history.len();
        let idx = |id: i32| self.history.iter().position(|m| m.id == id);
        let (a, b) = match self.visible.and_then(|(t, b)| Some((idx(t)?, idx(b)?))) {
            Some((t, b)) => (t.min(b), t.max(b)),
            None => (n.saturating_sub(NEAR), n.saturating_sub(1)),
        };
        let (from, to) = (a.saturating_sub(NEAR), (b + NEAR + 1).min(n));
        self.history[from..to].iter().map(|m| m.id).collect()
    }

    /// Сообщения вкладки «Медиа», чьи картинки нужны сетке (остальные вкладки
    /// сами ничего не скачивают).
    fn shared_images_src(&self) -> &[Message] {
        match &self.shared {
            Some(s) if s.filter == crate::model::SearchFilter::Media => self.shared_items(),
            _ => &[],
        }
    }

    /// Фото чата в окне истории по порядку (стрелки окна просмотра).
    pub fn photo_ids(&self) -> Vec<i32> {
        self.history
            .iter()
            .filter(|m| m.media_info.as_ref().is_some_and(|mi| viewable(mi) && mi.kind != MediaKind::WebPage))
            .map(|m| m.id)
            .collect()
    }
}

/// Наибольший размер картинки в ленте (крупнее прежних 320 px — как в Telegram).
pub const FEED_MAX_W: i32 = 420;
pub const FEED_MAX_H: i32 = 460;

/// Открывается в окне просмотра: фото, видео (В8), превью ссылки с фото.
/// Сторона стикера в ленте, px (как в Telegram Desktop — около 200).
pub const STICKER_SIZE: i32 = 192;

/// Размер стикера в ленте: вписать в `STICKER_SIZE` с сохранением пропорций.
pub fn sticker_size(w: i32, h: i32) -> (i32, i32) {
    let (w, h) = (w.max(1), h.max(1));
    let k = STICKER_SIZE as f32 / w.max(h) as f32;
    (((w as f32 * k).round() as i32).max(1), ((h as f32 * k).round() as i32).max(1))
}

/// Что показывать у стикера: статичный WebP — сам файл; анимированный (TGS)
/// и видеостикер (WebM) — пока превью от сервера, первый кадр (решение
/// пользователя: анимацию — отдельным шагом).
pub fn sticker_image(mi: &Media) -> Option<&FileRef> {
    if mi.mime == "image/webp" { mi.file.as_ref().or(mi.thumb.as_ref()) } else { mi.thumb.as_ref() }
}

pub fn viewable(mi: &Media) -> bool {
    match mi.kind {
        // Кружок играет в ленте (со звуком по щелчку), не в окне просмотра.
        MediaKind::Photo | MediaKind::Video | MediaKind::Gif => true,
        MediaKind::WebPage => mi.webpage.as_ref().is_some_and(|w| w.photo.is_some()),
        _ => false,
    }
}

/// Размер для ленты (Чт-11): вписать в 420×460; маленькое — не больше чем вдвое.
pub fn feed_size(w: i32, h: i32) -> (i32, i32) {
    if w <= 0 || h <= 0 {
        return (320, 240);
    }
    let k = (FEED_MAX_W as f32 / w as f32).min(FEED_MAX_H as f32 / h as f32).min(2.0);
    (((w as f32) * k).round().max(60.0) as i32, ((h as f32) * k).round().max(60.0) as i32)
}

/// Картинка вложения в ленте: какой файл, мини-превью и размер ячейки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedImage {
    pub file: Option<FileRef>,
    pub stripped: Vec<u8>,
    pub w: i32,
    pub h: i32,
    /// Место в альбоме: (номер, всего); у одиночного — (0, 1).
    pub album: (usize, usize),
    /// Своё неотправленное: файл на диске (показывается до выгрузки).
    pub local: String,
    /// Кружок (видеосообщение): квадрат с круглой маской.
    pub round: bool,
    /// Стикер: без пузыря и скругления, с прозрачностью.
    pub sticker: bool,
}

impl FeedImage {
    /// Ключ раскодированной картинки скачанного файла (размер — часть ключа).
    pub fn photo_key(&self) -> Option<String> {
        match &self.file {
            Some(f) if self.sticker => Some(format!("sticker:{}:{}x{}", f.key(), self.w, self.h)),
            Some(f) => Some(format!("photo:{}:{}x{}{}", f.key(), self.w, self.h, if self.round { "r" } else { "" })),
            None if !self.local.is_empty() => Some(format!("local:{}:{}x{}", self.local, self.w, self.h)),
            None => None,
        }
    }

    /// Ключ размытого мини-превью.
    pub fn strip_key(&self, peer_id: i64, msg_id: i32) -> String {
        format!("strip:{peer_id}:{msg_id}:{}x{}{}", self.w, self.h, if self.round { "r" } else { "" })
    }
}

/// Ширина альбома и промежуток между ячейками.
pub const ALBUM_W: i32 = 420;
pub const ALBUM_GAP: i32 = 2;

/// Ячейка альбома.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Сетка альбома из `n` (Чт-11): при нечётном числе первое — во всю ширину,
/// остальные парами квадратами. Возвращает ячейки и высоту альбома.
pub fn album_layout(n: usize) -> (Vec<Cell>, i32) {
    let half = (ALBUM_W - ALBUM_GAP) / 2;
    let mut cells = Vec::with_capacity(n);
    let mut y = 0;
    let mut rest = n;
    if n % 2 == 1 {
        cells.push(Cell { x: 0, y: 0, w: ALBUM_W, h: 260 });
        y = 260 + ALBUM_GAP;
        rest -= 1;
    }
    for i in 0..rest {
        let x = if i % 2 == 0 { 0 } else { half + ALBUM_GAP };
        cells.push(Cell { x, y, w: half, h: half });
        if i % 2 == 1 {
            y += half + ALBUM_GAP;
        }
    }
    (cells, (y - ALBUM_GAP).max(0))
}

/// Можно ли вложению быть в сетке альбома.
fn visual(mi: &Media) -> bool {
    matches!(mi.kind, MediaKind::Photo | MediaKind::Video)
}

/// Альбомы окна: подряд идущие фото и видео с одним `grouped_id` (2 и больше).
/// Результат — отрезки индексов `history`.
pub fn albums(history: &[Message]) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < history.len() {
        let g = history[i].grouped_id;
        let mut j = i + 1;
        if g != 0 && history[i].media_info.as_deref().is_some_and(visual) {
            while j < history.len()
                && history[j].grouped_id == g
                && history[j].media_info.as_deref().is_some_and(visual)
            {
                j += 1;
            }
            if j - i >= 2 {
                out.push(i..j);
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Картинки ленты для окна истории по id сообщения: фото, превью видео,
/// фото превью ссылки; в альбоме — размер ячейки.
pub fn feed_images(history: &[Message]) -> HashMap<i32, FeedImage> {
    let mut out = HashMap::new();
    for m in history {
        let Some(mi) = &m.media_info else { continue };
        if mi.self_destruct() {
            continue; // в ленте — заглушка, не картинка
        }
        if mi.kind == MediaKind::Sticker {
            let (w, h) = sticker_size(mi.width, mi.height);
            if let Some(file) = sticker_image(mi) {
                out.insert(m.id, FeedImage { file: Some(file.clone()), stripped: Vec::new(), w, h, album: (0, 1),
                    local: String::new(), round: false, sticker: true });
            }
            continue;
        }
        let (file, w, h, stripped) = match mi.kind {
            MediaKind::Photo => (mi.file.clone(), mi.width, mi.height, mi.stripped.clone()),
            MediaKind::Video | MediaKind::VideoNote | MediaKind::Gif => {
                (mi.thumb.clone(), mi.width, mi.height, mi.stripped.clone())
            }
            MediaKind::WebPage => match &mi.webpage {
                Some(wp) if wp.photo.is_some() => {
                    (wp.photo.clone(), wp.photo_width, wp.photo_height, wp.photo_stripped.clone())
                }
                _ => continue,
            },
            _ => continue,
        };
        if file.is_none() && stripped.is_empty() && mi.local_path.is_empty() {
            continue;
        }
        let round = mi.kind == MediaKind::VideoNote;
        let (w, h) = if round { (ROUND_SIZE, ROUND_SIZE) } else { feed_size(w, h) };
        out.insert(m.id, FeedImage { file, stripped, w, h, album: (0, 1), local: mi.local_path.clone(), round,
            sticker: false });
    }
    for r in albums(history) {
        let (cells, _) = album_layout(r.len());
        for (k, (i, c)) in r.clone().zip(cells).enumerate() {
            if let Some(f) = out.get_mut(&history[i].id) {
                f.w = c.w;
                f.h = c.h;
                f.album = (k, r.len());
            }
        }
    }
    out
}

/// Байты в «12,3 МБ».
pub fn size_label(n: i64, ru: bool) -> String {
    let (kb, mb, gb) = if ru { ("КБ", "МБ", "ГБ") } else { ("KB", "MB", "GB") };
    let f = n as f64;
    let s = if f >= 1e9 {
        format!("{:.1} {gb}", f / 1_073_741_824.0)
    } else if f >= 1e6 {
        format!("{:.1} {mb}", f / 1_048_576.0)
    } else {
        format!("{:.0} {kb}", (f / 1024.0).max(1.0))
    };
    if ru { s.replace('.', ",") } else { s }
}

/// Длительность «1:05».
pub fn duration_label(secs: i32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

pub fn has_media(m: &Message) -> bool {
    m.media_info.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_index_evicts_oldest_but_keeps_visible() {
        let mut s = Store::in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        for (i, k) in ["a", "b", "c"].iter().enumerate() {
            let p = dir.path().join(k);
            std::fs::write(&p, b"x").unwrap();
            s.media_saved(k, p.to_str().unwrap(), 100, i as i64).unwrap();
        }
        assert_eq!(s.media_total().unwrap(), 300);
        let found = s.media_paths(&["a".into(), "zz".into()]).unwrap();
        assert!(found.contains_key("a") && !found.contains_key("zz"));
        s.media_touch(&["a".into()], 10).unwrap();
        // Лимит 150: уходят самые давние («b», «c»), но «c» видно сейчас — уходит «b», затем «a»?
        let keep: HashSet<String> = ["c".to_string()].into();
        let gone = s.media_evict(150, &keep).unwrap();
        assert_eq!(gone.len(), 2, "«b» (давнее) и «a»; видимое «c» остаётся");
        assert!(gone.iter().any(|p| p.ends_with("/b")) && gone.iter().any(|p| p.ends_with("/a")));
        assert_eq!(s.media_total().unwrap(), 100);
        assert_eq!(s.media_clear().unwrap().len(), 1);
        assert_eq!(s.media_total().unwrap(), 0);
    }

    #[test]
    fn albums_are_runs_of_one_group() {
        let photo = |id: i32, g: i64| Message {
            id,
            grouped_id: g,
            media_info: Some(Box::new(Media { kind: MediaKind::Photo, width: 800, height: 600, stripped: vec![1], ..Default::default() })),
            ..Default::default()
        };
        let text = |id: i32| Message { id, text: "x".into(), ..Default::default() };
        let h = vec![photo(1, 0), photo(2, 7), photo(3, 7), photo(4, 7), text(5), photo(6, 9), photo(7, 8)];
        assert_eq!(albums(&h), vec![1..4], "одиночные с grouped_id — не альбом");
        let (cells, height) = album_layout(3);
        assert_eq!(cells[0], Cell { x: 0, y: 0, w: 420, h: 260 });
        assert_eq!(cells[2].x, 211);
        assert_eq!(height, 260 + 2 + 209);
        let (cells, height) = album_layout(4);
        assert_eq!((cells.len(), height), (4, 209 * 2 + 2));
        let f = feed_images(&h);
        assert_eq!((f[&1].w, f[&1].h, f[&1].album), (420, 315, (0, 1)));
        assert_eq!((f[&3].w, f[&3].h, f[&3].album), (209, 209, (1, 3)));
        assert!(!f.contains_key(&5));
        assert!(f[&3].photo_key().is_none() && f[&3].strip_key(1, 3) == "strip:1:3:209x209");
    }

    #[test]
    fn incomplete_refetched_once() {
        let mut m = ChatModel::new(1, "ru", 0);
        m.open = Some(10);
        m.extra_peers.insert(10, crate::model::Peer { id: 10, access_hash: 3, ..Default::default() });
        m.history = vec![
            Message { id: 1, text: "текст".into(), ..Default::default() },
            Message { id: 2, media: "photo".into(), ..Default::default() },
            Message { id: 3, ..Default::default() },
            Message { id: 4, media: "photo".into(), media_info: Some(Box::default()), ..Default::default() },
        ];
        let mut out = Vec::new();
        m.media_window_changed(&mut out);
        let asked: Vec<_> = out.iter().filter_map(|e| match e {
            ChatEffect::Send { cmd: Command::LoadMessages { ids, .. }, .. } => Some(ids.clone()),
            _ => None,
        }).collect();
        assert_eq!(asked, vec![vec![2, 3]], "фото без описания и пустое; полное и обычный текст — нет");
        let mut again = Vec::new();
        m.media_window_changed(&mut again);
        assert!(!again.iter().any(|e| matches!(e, ChatEffect::Send { .. })), "один раз за сеанс");
    }

    #[test]
    fn decoded_only_near_visible() {
        let mut m = ChatModel::new(1, "ru", 0);
        m.history = (1..=100).map(|id| Message { id, ..Default::default() }).collect();
        assert_eq!(m.near_visible().len(), 20, "до вида — последние (с запасом)");
        m.visible = Some((50, 55));
        let near = m.near_visible();
        assert!(near.contains(&40) && near.contains(&65) && !near.contains(&39) && !near.contains(&66));
    }

    #[test]
    fn labels_and_sizes() {
        assert_eq!(feed_size(1280, 960), (420, 315));
        assert_eq!(feed_size(600, 900), (307, 460), "высокое — по высоте");
        assert_eq!(feed_size(100, 50), (200, 100), "маленькое — не больше чем вдвое");
        assert_eq!(feed_size(100, 4000), (60, 460), "узкое — не уже 60 px");
        assert_eq!(size_label(1_500_000, true), "1,4 МБ");
        assert_eq!(size_label(10, false), "1 KB");
        assert_eq!(duration_label(65), "1:05");
    }
}
