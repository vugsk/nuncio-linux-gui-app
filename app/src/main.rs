//! Точка входа: собирает части вместе (Архитектура/nuncio - Приложение (Rust и
//! Slint).md, раздел 2). Потоки: UI (главный, цикл Slint), домен (модель и
//! эффекты), БД (единственный владелец SQLite), насос (`next_event` сети).
//! Сигналы SIGINT/SIGTERM/SIGHUP закрывают окно штатно: сеть останавливается
//! через `nn_shutdown`, файл сессии дописывается (Границы и C ABI, «Сигналы»).

mod logger;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bridge::{GoBackend, StartParams};
use domain::config::{self, Config, Paths};
use domain::chat::{ChatEffect, ChatModel, DbOp, DIALOGS_WINDOW};
use domain::history::Anchor;
use domain::login::{CriticalKind, Effect, LoginModel, Screen};
use domain::model::Event;
use domain::store::{DbThread, Store};
use domain::{Backend, Next};
use slint::{ComponentHandle, Rgba8Pixel, SharedPixelBuffer};
use ui::Action;

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

enum Msg {
    Action(Action),
    Event(Event),
    Stop,
}

/// Снимок модели для UI: кладётся в слот, UI забирает последний (не чаще кадра).
#[derive(Default)]
struct Outbox {
    model: Option<LoginModel>,
    chat: Option<ChatModel>,
    avatars: HashMap<String, SharedPixelBuffer<Rgba8Pixel>>,
    images_ver: u64,
    lang: Option<String>,
    scheduled: bool,
}

/// Сторона круглого аватара в пикселях: 48 px в списке, ×2 для чёткости на HiDPI.
const AVATAR_PX: u32 = 96;

struct Domain {
    model: LoginModel,
    cfg: Config,
    paths: Paths,
    db: Option<DbThread>,
    backend: Option<Arc<dyn Backend>>,
    ui: slint::Weak<ui::App>,
    out: Arc<Mutex<Outbox>>,
    quit: bool,
    chat: Option<ChatModel>,
    /// Картинки для UI: аватары по пути файла и вложения окна истории
    /// (`photo:ключ`, `strip:id`, `full:ключ`); версия растёт при изменении.
    avatars: HashMap<String, SharedPixelBuffer<Rgba8Pixel>>,
    images_ver: u64,
    bad_avatars: HashSet<String>,
    bench_sent: bool,
}

/// Миниатюра файла в панели подготовки, px (2× для 48 px в интерфейсе).
const PREP_PX: u32 = 96;

/// Выбранный файл: размер, MIME по расширению, стороны картинки по заголовку.
fn inspect_file(path: &str) -> Option<domain::attach::PrepFile> {
    let meta = std::fs::metadata(path).ok().filter(|m| m.is_file())?;
    let name = std::path::Path::new(path).file_name()?.to_string_lossy().into_owned();
    let mime = domain::attach::mime_of(&name).to_string();
    let (width, height) = if mime == "image/jpeg" || mime == "image/png" {
        ui::photo::image_size(path).map(|(w, h)| (w as i32, h as i32)).unwrap_or((0, 0))
    } else {
        (0, 0)
    };
    Some(domain::attach::PrepFile {
        path: path.to_string(),
        name,
        mime,
        size: meta.len() as i64,
        image: width > 0,
        width,
        height,
    })
}

/// Отложенное раскодирование картинки (выполняется, только если её ещё нет).
type Decode = Box<dyn FnOnce() -> Option<SharedPixelBuffer<Rgba8Pixel>>>;

impl Domain {
    fn publish(&self, lang: Option<String>) {
        let mut o = self.out.lock().unwrap();
        o.model = Some(self.model.clone());
        o.chat = self.chat.clone();
        if o.images_ver != self.images_ver {
            o.images_ver = self.images_ver;
            o.avatars = self.avatars.clone();
        }
        if lang.is_some() {
            o.lang = lang;
        }
        if o.scheduled {
            return;
        }
        o.scheduled = true;
        let out = self.out.clone();
        let ui = self.ui.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let (model, chat, avatars, lang) = {
                let mut o = out.lock().unwrap();
                o.scheduled = false;
                (o.model.take(), o.chat.take(), o.avatars.clone(), o.lang.take())
            };
            if let Some(lang) = lang {
                ui::apply_language(&lang);
            }
            if let (Some(app), Some(m)) = (ui.upgrade(), model) {
                RENDERER.with(|r| r.borrow_mut().render(&app, &m));
                if let Some(c) = chat {
                    ui::chatview::render(&app, &c, &avatars);
                }
            }
        });
    }

    fn apply(&mut self, effects: Vec<Effect>) {
        let mut lang = None;
        for e in effects {
            match e {
                Effect::Send { id, cmd } => {
                    if let Some(b) = &self.backend {
                        if let Err(err) = b.submit(id, cmd) {
                            log::warn!("команда не отправлена: {err}");
                        }
                    }
                }
                Effect::SaveConfig => {
                    self.cfg.lang = self.model.lang.clone();
                    self.cfg.last_country = self.model.default_country.clone();
                    self.cfg.proxy = self.model.saved_proxy.clone();
                    if let Err(err) = self.cfg.save(&self.paths.config) {
                        log::warn!("config.toml не сохранён: {err}");
                    }
                }
                Effect::ApplyLanguage(l) => lang = Some(l),
                Effect::WipeLocalData => {
                    log::info!("удаление локальных данных аккаунта");
                    self.chat = None;
                    self.avatars.clear();
                    if let Some(db) = &self.db {
                        db.run(|s| {
                            if let Err(e) = s.wipe_account() {
                                log::error!("очистка базы: {e}");
                            }
                        });
                    }
                    let _ = std::fs::remove_dir_all(self.paths.cache.join("media"));
                }
                Effect::Quit => self.quit = true,
            }
        }
        self.publish(lang);
    }

    /// Эффекты модели чата: команды сети и операции с базой по порядку;
    /// перечитывания списка и истории схлопываются и выполняются в конце.
    fn run_chat(&mut self, fx: Vec<ChatEffect>) {
        let mut queue: VecDeque<ChatEffect> = fx.into();
        loop {
            let (mut rows, mut history) = (false, None::<(i64, Anchor, usize, usize)>);
            while let Some(e) = queue.pop_front() {
                match e {
                    ChatEffect::Send { id, cmd } => {
                        if let Some(b) = &self.backend {
                            if let Err(err) = b.submit(id, cmd) {
                                log::warn!("команда не отправлена: {err}");
                            }
                        }
                    }
                    ChatEffect::Db(DbOp::ReloadRows) => rows = true,
                    ChatEffect::Clipboard(text) => {
                        // Буфер обмена (Чт-9); на Wayland — через data-control.
                        match arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
                            Ok(()) => {}
                            Err(e) => log::warn!("буфер обмена: {e}"),
                        }
                    }
                    ChatEffect::Db(DbOp::SearchPeers { query }) => {
                        let found = self.db.as_ref().and_then(|db| db.call(move |s| s.search_peers(&query, 50)));
                        if let (Some(Ok(peers)), Some(chat)) = (found, self.chat.as_mut()) {
                            chat.set_picker_results(peers);
                        }
                    }
                    ChatEffect::Db(DbOp::ReloadFolders) => {
                        let f = self.db.as_ref().and_then(|db| db.call(|s| s.folders()));
                        if let (Some(Ok(f)), Some(chat)) = (f, self.chat.as_mut()) {
                            chat.set_folders(f);
                        }
                    }
                    ChatEffect::InspectFiles { paths, append } => {
                        let files = paths.iter().filter_map(|p| inspect_file(p)).collect();
                        if let Some(chat) = self.chat.as_mut() {
                            chat.set_prepared(files, append);
                        }
                    }
                    ChatEffect::ClearCache => self.clear_cache(),
                    ChatEffect::OpenFile(path) => {
                        if let Err(e) = std::process::Command::new("xdg-open").arg(&path).spawn() {
                            log::warn!("xdg-open: {e}");
                        }
                    }
                    ChatEffect::Db(DbOp::MediaLookup { keys }) => {
                        let now = now();
                        let found = self.db.as_ref().and_then(|db| {
                            db.call(move |s| {
                                s.media_touch(&keys, now)?;
                                s.media_paths(&keys)
                            })
                        });
                        match (found, self.chat.as_mut()) {
                            (Some(Ok(f)), Some(chat)) => queue.extend(chat.set_media_paths(f)),
                            (Some(Err(e)), _) => log::error!("кэш вложений: {e}"),
                            _ => {}
                        }
                    }
                    ChatEffect::Db(DbOp::MediaSaved { key, path, size }) => {
                        // Записать в индекс; сверх лимита — вытеснить давнее, кроме видимого.
                        let now = now();
                        // Размер неизвестен заранее — по файлу.
                        let size = if size > 0 { size } else { std::fs::metadata(&path).map(|m| m.len() as i64).unwrap_or(0) };
                        let keep: HashSet<String> =
                            self.chat.as_ref().map(|c| c.window_media_keys().into_iter().collect()).unwrap_or_default();
                        let gone = self.db.as_ref().and_then(|db| {
                            db.call(move |s| {
                                s.media_saved(&key, &path, size, now)?;
                                s.media_evict(domain::media::CACHE_LIMIT, &keep)
                            })
                        });
                        match gone {
                            Some(Ok(paths)) => {
                                for p in paths {
                                    let _ = std::fs::remove_file(p);
                                }
                            }
                            Some(Err(e)) => log::error!("кэш вложений: {e}"),
                            None => {}
                        }
                    }
                    ChatEffect::Db(DbOp::SearchLocal { query }) => {
                        let q = query.clone();
                        let found = self.db.as_ref().and_then(|db| db.call(move |s| s.search_local(&q, 100)));
                        match (found, self.chat.as_mut()) {
                            (Some(Ok(msgs)), Some(chat)) => chat.set_local_search(&query, msgs),
                            (Some(Err(e)), _) => log::error!("поиск по базе: {e}"),
                            _ => {}
                        }
                    }
                    ChatEffect::Db(DbOp::ReloadPinned { peer_id }) => {
                        let pins = self.db.as_ref().and_then(|db| db.call(move |s| s.pinned(peer_id)));
                        if let (Some(Ok(p)), Some(chat)) = (pins, self.chat.as_mut()) {
                            chat.set_pinned(peer_id, p);
                        }
                    }
                    ChatEffect::Db(DbOp::PumpOutbox) => {
                        let now = now();
                        let ready = self.db.as_ref().and_then(|db| {
                            db.call(move |s| {
                                s.outbox_expire(now, domain::outbox::TTL_SECS)?;
                                s.outbox_ready(now)
                            })
                        });
                        match ready {
                            Some(Ok(items)) => {
                                if let Some(chat) = self.chat.as_mut() {
                                    queue.extend(chat.set_outbox_ready(items));
                                }
                            }
                            Some(Err(e)) => log::error!("очередь отправки: {e}"),
                            None => {}
                        }
                    }
                    ChatEffect::Db(DbOp::ReloadHistory { peer_id, anchor, before, after }) => {
                        history = Some((peer_id, anchor, before, after));
                    }
                    ChatEffect::Db(op) => self.db_write(op),
                }
            }
            if rows {
                let list = self
                    .db
                    .as_ref()
                    // Все диалоги (основной список и архив): фильтр и папки — в модели.
                    .and_then(|db| db.call(|s| s.dialog_rows(-1, DIALOGS_WINDOW)))
                    .and_then(|r| r.map_err(|e| log::error!("список диалогов: {e}")).ok())
                    .unwrap_or_default();
                for r in &list {
                    self.decode_avatar(&r.avatar_path);
                }
                if self.chat.as_ref().is_some_and(|c| c.rows.len() != list.len()) {
                    log::info!("список: {} диалогов из базы", list.len());
                }
                if let Some(chat) = self.chat.as_mut() {
                    // Для замеров (В4): открыть n-й диалог (или «self» — Избранное:
                    // «прочитано» там ни на кого не влияет), как только список загружен.
                    let bench = std::env::var("NUNCIO_BENCH_OPEN").ok();
                    let target = match bench.as_deref() {
                        Some("self") => list.iter().find(|r| r.peer.is_self).map(|r| r.peer.id),
                        Some(n) => n.parse::<usize>().ok().and_then(|n| list.get(n).map(|r| r.peer.id)),
                        None => None,
                    };
                    // Для проверки kill -9 (В5): n сообщений в очередь первому собеседнику.
                    let bench_send = std::env::var("NUNCIO_BENCH_SEND").ok().and_then(|v| v.parse::<usize>().ok());
                    let first_user = list.iter().find(|r| r.peer.id > 0 && !r.peer.is_self).map(|r| r.peer.id);
                    queue.extend(chat.set_rows(list));
                    if let (Some(n), Some(peer), false) = (bench_send, first_user, self.bench_sent) {
                        self.bench_sent = true;
                        let queued = self.db.as_ref().and_then(|db| db.call(|s| s.outbox_len())).and_then(Result::ok);
                        if queued == Some(0) {
                            for i in 1..=n {
                                queue.extend(chat.enqueue_text(peer, format!("исходящее {i}")));
                            }
                        }
                    }
                    if let (Some(id), None) = (target, chat.open) {
                        log::info!("замер: открыт диалог из списка");
                        queue.extend(chat.intent(domain::chat::ChatIntent::OpenChat(id)));
                    }
                }
            }
            if let Some((peer_id, anchor, before, after)) = history {
                let loaded = self.db.as_ref().and_then(|db| {
                    db.call(move |s| {
                        let win = s.history_window(peer_id, anchor, before, after)?;
                        let msgs = &win.messages;
                        // Имена отправителей для групп (серии, Чт-2).
                        let mut senders = HashMap::new();
                        for m in msgs {
                            for id in [m.from_id, m.fwd_from_id] {
                                if id != 0 && !senders.contains_key(&id) {
                                    if let Some(p) = s.peer(id)?.filter(|p| !p.name.is_empty()) {
                                        senders.insert(id, p.name);
                                    }
                                }
                            }
                        }
                        // Цитаты ответов (Чт-3): автор и первая строка исходного из базы.
                        let mut quotes = HashMap::new();
                        for m in msgs {
                            if m.reply_to > 0 && !quotes.contains_key(&m.reply_to) {
                                if let Some(src) = s.message(peer_id, m.reply_to)? {
                                    let author = if src.out {
                                        String::new() // «Вы» подставит интерфейс
                                    } else {
                                        s.peer(src.from_id)?.map(|p| p.name).unwrap_or_default()
                                    };
                                    quotes.insert(m.reply_to, (author, domain::chat::snippet(&src)));
                                }
                            }
                        }
                        Ok::<_, domain::store::StoreError>((win, senders, quotes))
                    })
                });
                match loaded {
                    Some(Ok((msgs, senders, quotes))) => {
                        if let Some(chat) = self.chat.as_mut() {
                            queue.extend(chat.set_history(peer_id, msgs, senders));
                            queue.extend(chat.set_quotes(peer_id, quotes));
                        }
                    }
                    Some(Err(e)) => log::error!("история: {e}"),
                    None => {}
                }
            }
            if queue.is_empty() {
                break;
            }
        }
        self.sync_media_images();
    }

    /// Очистить кэш файлов (временный пункт меню до настроек В9): индекс и
    /// весь каталог `cache/media`, в том числе недокачанное.
    fn clear_cache(&mut self) {
        let indexed = self.db.as_ref().and_then(|db| db.call(|s| s.media_clear())).and_then(Result::ok).unwrap_or_default();
        let dir = self.paths.cache.join("media");
        let mut freed = 0u64;
        let mut remove = |p: &std::path::Path| {
            if let Ok(m) = std::fs::metadata(p) {
                if m.is_file() && std::fs::remove_file(p).is_ok() {
                    freed += m.len();
                }
            }
        };
        for p in &indexed {
            remove(std::path::Path::new(p));
        }
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                remove(&e.path());
            }
        }
        log::info!("кэш очищен: {} файлов в индексе, освобождено {} КБ", indexed.len(), freed / 1024);
        self.avatars.retain(|k, _| !k.starts_with("photo:") && !k.starts_with("full:"));
        self.images_ver += 1;
        if let Some(chat) = self.chat.as_mut() {
            let ru = chat.lang == "ru";
            let size = domain::media::size_label(freed as i64, ru);
            chat.toast(&if ru { format!("Кэш очищен: {size}") } else { format!("Cache cleared: {size}") });
        }
    }

    /// Картинки вложений окна истории: фото для ленты (скачанное), размытое
    /// мини-превью (ещё нет), полное — для окна просмотра. Всё вне окна —
    /// выбрасывается: память ограничена окном (План, В7).
    fn sync_media_images(&mut self) {
        let Some(chat) = self.chat.as_ref() else { return };
        let mut want: Vec<(String, Decode)> = Vec::new();
        let mut keep: HashSet<String> = HashSet::new();
        for (id, f) in domain::media::feed_images(&chat.history) {
            let (tw, th) = (f.w as u32, f.h as u32);
            let path = match &f.file {
                Some(file) => chat.media_paths.get(&file.key()),
                None => Some(&f.local).filter(|l| !l.is_empty()),
            };
            match (path, f.photo_key()) {
                (Some(path), Some(k)) => {
                    keep.insert(k.clone());
                    if !self.avatars.contains_key(&k) && !self.bad_avatars.contains(&k) {
                        let path = path.clone();
                        want.push((k, Box::new(move || ui::photo::load_feed(&path, tw, th))));
                    }
                }
                _ if !f.stripped.is_empty() => {
                    let k = f.strip_key(chat.open.unwrap_or(0), id);
                    keep.insert(k.clone());
                    if !self.avatars.contains_key(&k) && !self.bad_avatars.contains(&k) {
                        let stripped = f.stripped;
                        want.push((k, Box::new(move || ui::photo::stripped(&stripped, tw, th))));
                    }
                }
                _ => {}
            }
        }
        // Миниатюры панели подготовки (Св-4).
        for f in chat.prepare.iter().flat_map(|p| &p.files).filter(|f| f.image) {
            let k = format!("prep:{}", f.path);
            keep.insert(k.clone());
            if !self.avatars.contains_key(&k) && !self.bad_avatars.contains(&k) {
                let path = f.path.clone();
                want.push((k, Box::new(move || ui::photo::load_feed(&path, PREP_PX, PREP_PX))));
            }
        }
        if let Some(key) = chat.viewer_key() {
            if let Some(path) = chat.media_paths.get(&key) {
                let k = format!("full:{key}");
                keep.insert(k.clone());
                if !self.avatars.contains_key(&k) {
                    let path = path.clone();
                    want.push((k, Box::new(move || ui::photo::load_full(&path, 2560, 1440))));
                }
            }
        }
        let before = self.avatars.len();
        self.avatars.retain(|k, _| {
            !["photo:", "strip:", "full:", "local:", "prep:"].iter().any(|p| k.starts_with(p)) || keep.contains(k)
        });
        let mut changed = before != self.avatars.len();
        for (k, load) in want {
            match load() {
                Some(px) => {
                    self.avatars.insert(k, px);
                    changed = true;
                }
                None => {
                    log::warn!("вложение не раскодировано");
                    self.bad_avatars.insert(k);
                }
            }
        }
        if changed {
            self.images_ver += 1;
        }
    }

    fn db_write(&self, op: DbOp) {
        let Some(db) = &self.db else { return };
        let self_id = self.chat.as_ref().map(|c| c.self_id).unwrap_or(0);
        let now = now();
        let res = db.call(move |s| match op {
            DbOp::EnqueueMedia { peer_id, items, reply_to, group } => items.iter().try_for_each(|it| {
                let o = domain::outbox::Outgoing { peer_id, text: &it.text, entities: &it.entities, reply_to,
                    random_id: it.random_id, media: Some(&it.media), group };
                s.outbox_enqueue(&o, self_id, now).map(|_| ())
            }),
            DbOp::Enqueue { peer_id, text, entities, reply_to, random_id } => {
                let o = domain::outbox::Outgoing { peer_id, text: &text, entities: &entities, reply_to, random_id, ..Default::default() };
                s.outbox_enqueue(&o, self_id, now).map(|_| ())
            }
            DbOp::OutboxSending { local_id } => s.outbox_sending(local_id),
            DbOp::OutboxDone { local_id, sent } => s.outbox_done(local_id, sent.as_ref()),
            DbOp::OutboxRequeue { local_id, next_try } => s.outbox_requeue(local_id, next_try),
            DbOp::OutboxFail { local_id, error } => s.outbox_fail(local_id, &error),
            DbOp::OutboxRetry { local_id } => s.outbox_retry(local_id, now),
            DbOp::OutboxDelete { local_id } => s.outbox_delete(local_id),
            DbOp::PumpOutbox | DbOp::SearchPeers { .. } | DbOp::ReloadPinned { .. } | DbOp::ReloadFolders
            | DbOp::SearchLocal { .. } | DbOp::MediaLookup { .. } | DbOp::MediaSaved { .. } => Ok(()),
            DbOp::SaveFolders(f) => s.save_folders(&f),
            DbOp::ApplyLocal(updates) => {
                s.apply_batch(&domain::model::UpdatesBatch { updates, ..Default::default() }).map(|_| ())
            }
            DbOp::DropChat { peer_id, dialog } => s.drop_chat(peer_id, dialog),
            DbOp::SavePeers(peers) => s.save_peers(&peers),
            DbOp::SaveDialogs { dialogs, messages, peers } => s.save_dialogs(&dialogs, &messages, &peers),
            DbOp::SaveHistory { messages, peers, span } => s.save_history(&messages, &peers, span),
            DbOp::ApplyBatch(b) => s.apply_batch(&b).map(|_| ()),
            DbOp::SetAvatar { peer_id, path } => s.set_avatar_path(peer_id, &path),
            DbOp::MarkReadLocal { peer_id, max_id } => s.mark_read_local(peer_id, max_id),
            DbOp::ReloadRows | DbOp::ReloadHistory { .. } => Ok(()),
        });
        if let Some(Err(e)) = res {
            // Пачка не записана: состояние в базе не сдвинулось, при следующем
            // старте Go запросит разницу заново (О-5).
            log::error!("запись в базу: {e}");
        }
    }

    fn decode_avatar(&mut self, path: &str) {
        if path.is_empty() || self.avatars.contains_key(path) || self.bad_avatars.contains(path) {
            return;
        }
        match ui::avatar::load(path, AVATAR_PX) {
            Some(px) => {
                self.images_ver += 1;
                self.avatars.insert(path.to_string(), px);
            }
            None => {
                self.bad_avatars.insert(path.to_string());
            }
        }
    }

    /// Вход и выход меняют главное окно: модель чата живёт, пока вошли.
    fn sync_chat(&mut self) {
        match (self.model.screen(), self.chat.is_some()) {
            (Screen::Main, false) => {
                let self_id = self.model.user.as_ref().map(|u| u.id).unwrap_or(0);
                let mut chat = ChatModel::new(self_id, &self.model.lang, now());
                let fx = chat.enter();
                self.chat = Some(chat);
                self.run_chat(fx);
            }
            (Screen::Login | Screen::Critical(_), true) => {
                self.chat = None;
                self.avatars.clear();
            }
            _ => {}
        }
    }

    fn handle(&mut self, msg: Msg) {
        self.model.now = now();
        if let Some(c) = self.chat.as_mut() {
            c.now = self.model.now;
            c.lang = self.model.lang.clone();
        }
        match msg {
            Msg::Event(e) => {
                let before = screen_name(&self.model);
                let chat_fx = self.chat.as_mut().map(|c| c.event(&e)).unwrap_or_default();
                let fx = self.model.event(e);
                let after = screen_name(&self.model);
                if before != after {
                    log::info!("экран: {after}");
                }
                self.run_chat(chat_fx);
                self.apply(fx);
                self.sync_chat();
                self.publish(None);
            }
            Msg::Action(Action::Chat(i)) => {
                if let Some(c) = self.chat.as_mut() {
                    let fx = c.intent(i);
                    self.run_chat(fx);
                }
                self.publish(None);
            }
            // «Повторить» на экране повреждённой базы: открыть базу ещё раз.
            Msg::Action(Action::Login(domain::login::Intent::Retry))
                if matches!(self.model.screen(), Screen::Critical(ref c) if c.kind == CriticalKind::DatabaseCorrupt) =>
            {
                match Store::open(&self.paths.database()) {
                    Ok(s) => {
                        self.db = Some(DbThread::spawn(s));
                        self.model.screen = Some(Screen::Starting);
                        start_network(self);
                    }
                    Err(e) => self.model.critical(CriticalKind::DatabaseCorrupt, e.to_string()),
                }
                self.publish(None);
            }
            Msg::Action(Action::Login(i)) => {
                let opening_proxy = matches!(i, domain::login::Intent::OpenProxy);
                let fx = self.model.intent(i);
                self.apply(fx);
                if opening_proxy {
                    let p = self.model.proxy.draft.clone();
                    let _ = self.ui.upgrade_in_event_loop(move |app| ui::Renderer::fill_proxy(&app, &p));
                }
            }
            Msg::Action(Action::Quit) => self.quit = true,
            Msg::Action(Action::ResetData) => {
                // Со-5: удаляем базу и открываем заново; сессия (session.bin) остаётся.
                let db = self.paths.database();
                for suffix in ["", "-wal", "-shm"] {
                    let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
                }
                match Store::open(&db) {
                    Ok(s) => {
                        log::info!("локальная база пересоздана");
                        self.db = Some(DbThread::spawn(s));
                        self.model.screen = Some(Screen::Starting);
                        start_network(self);
                    }
                    Err(e) => self.model.critical(CriticalKind::DatabaseCorrupt, e.to_string()),
                }
                self.publish(None);
            }
            Msg::Stop => self.quit = true,
        }
    }
}

fn screen_name(m: &LoginModel) -> &'static str {
    match m.screen() {
        Screen::Starting => "старт",
        Screen::Critical(_) => "критическое состояние",
        Screen::Login => "вход",
        Screen::Main => "главное окно",
    }
}

thread_local! {
    static RENDERER: RefCell<ui::Renderer> = RefCell::new(ui::Renderer::default());
}

static TX: Mutex<Option<mpsc::Sender<Msg>>> = Mutex::new(None);

fn send(msg: Msg) {
    if let Some(tx) = TX.lock().unwrap().as_ref() {
        let _ = tx.send(msg);
    }
}

/// Название системы для initConnection (видно в списке сеансов Telegram).
fn system_version() -> String {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string()))
        })
        .unwrap_or_else(|| "Linux".into())
}

/// Запускает сеть (Go) и насос событий. Go-рантайм стартует один раз за процесс.
fn start_network(d: &mut Domain) {
    if d.backend.is_some() {
        return;
    }
    let Some(creds) = config::load_api_credentials(&d.paths.config) else {
        d.model.critical(CriticalKind::NoApiCredentials, String::new());
        return;
    };
    let update_state = d.db.as_ref().and_then(|db| db.call(|s| s.update_state())).and_then(Result::ok).flatten();
    let channel_states =
        d.db.as_ref().and_then(|db| db.call(|s| s.channel_states())).and_then(Result::ok).unwrap_or_default();
    let params = StartParams {
        api_id: creds.api_id,
        api_hash: creds.api_hash,
        data_dir: d.paths.data.display().to_string(),
        log_dir: d.paths.state.display().to_string(),
        device_model: "nuncio".into(),
        system_version: system_version(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        lang_code: d.model.lang.clone(),
        system_lang_code: d.model.lang.clone(),
        test_servers: d.cfg.test_servers,
        proxy: d.cfg.proxy.clone(),
        keyring_app: std::env::var("NUNCIO_KEYRING_APP").unwrap_or_default(),
        cache_dir: d.paths.cache.display().to_string(),
        update_state,
        channel_states,
    };
    match GoBackend::start(&params) {
        Ok(b) => {
            let b: Arc<dyn Backend> = Arc::new(b);
            d.backend = Some(b.clone());
            std::thread::Builder::new()
                .name("nuncio-pump".into())
                .spawn(move || pump(b))
                .expect("не удалось запустить поток-насос");
        }
        Err(e) => d.model.critical(CriticalKind::NetworkFailed, e.to_string()),
    }
}

/// Поток-насос: только забирает события и передаёт их домену.
fn pump(b: Arc<dyn Backend>) {
    loop {
        match b.next_event(Duration::from_millis(500)) {
            Ok(Next::Event(e)) => send(Msg::Event(e)),
            Ok(Next::Timeout) => {}
            Ok(Next::Stopped) => return,
            Err(e) => {
                log::error!("насос: {e}");
                return;
            }
        }
    }
}

fn domain_loop(mut d: Domain, rx: mpsc::Receiver<Msg>) {
    d.publish(Some(d.model.lang.clone()));
    while !d.quit {
        let wait = if d.model.needs_tick() {
            Duration::from_secs(1)
        } else if d.chat.is_some() {
            Duration::from_secs(10) // очередь отправки и время в списке
        } else {
            Duration::from_secs(30)
        };
        match rx.recv_timeout(wait) {
            Ok(msg) => d.handle(msg),
            Err(RecvTimeoutError::Timeout) => {
                d.model.now = now();
                if let Some(c) = d.chat.as_mut() {
                    let fx = c.intent(domain::chat::ChatIntent::Tick(d.model.now));
                    d.run_chat(fx);
                }
                d.publish(None);
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = slint::invoke_from_event_loop(|| {
        let _ = slint::quit_event_loop();
    });
    if let Some(b) = d.backend.take() {
        if let Err(e) = b.shutdown(Duration::from_secs(3)) {
            log::warn!("остановка сети: {e}");
        }
    }
    drop(d.db.take());
    log::info!("домен остановлен");
}

fn main() {
    let paths = Paths::system();
    let dirs_err = paths.create_private().err();
    logger::init(&paths.state);
    if let Some(e) = dirs_err {
        log::warn!("каталоги приложения: {e}");
    }
    log::info!("запуск nuncio {}", env!("CARGO_PKG_VERSION"));

    let (cfg, cfg_err) = Config::load(&paths.config);
    if let Some(e) = cfg_err {
        log::warn!("{e}");
    }
    let env = |k: &str| std::env::var(k).ok();
    let lang = config::ui_language(&cfg, env);
    ui::apply_language(lang);
    let country = if cfg.last_country.is_empty() {
        config::locale_country(env).unwrap_or_else(|| "RU".into())
    } else {
        cfg.last_country.clone()
    };
    let model = LoginModel::new(lang, &country, cfg.proxy.clone(), now());

    let app = ui::App::new().expect("не удалось создать окно (нужен доступный бэкенд Slint)");
    // Для замеров (В4): размер окна «ШxВ».
    if let Some((w, h)) = std::env::var("NUNCIO_WINDOW_SIZE").ok().and_then(|v| {
        let (w, h) = v.split_once('x')?;
        Some((w.parse().ok()?, h.parse().ok()?))
    }) {
        app.window().set_size(slint::PhysicalSize::new(w, h));
    }
    let (tx, rx) = mpsc::channel();
    *TX.lock().unwrap() = Some(tx);
    ui::bind(&app, |a| send(Msg::Action(a)));

    let mut d = Domain {
        model,
        cfg,
        paths: paths.clone(),
        db: None,
        backend: None,
        ui: app.as_weak(),
        out: Arc::new(Mutex::new(Outbox::default())),
        quit: false,
        chat: None,
        avatars: HashMap::new(),
        images_ver: 0,
        bad_avatars: HashSet::new(),
        bench_sent: false,
    };
    match Store::open(&paths.database()) {
        Ok(s) => {
            // После сбоя «отправляется» → «в очередь» (тот же random_id, дубля не будет).
            match s.outbox_recover() {
                Ok(n) if n > 0 => log::info!("очередь отправки: {n} сообщений возвращены в очередь"),
                Err(e) => log::error!("очередь отправки: {e}"),
                _ => {}
            }
            d.db = Some(DbThread::spawn(s));
            start_network(&mut d);
        }
        Err(e) => {
            log::error!("база: {e}");
            d.model.critical(CriticalKind::DatabaseCorrupt, e.to_string());
        }
    }
    let domain = std::thread::Builder::new()
        .name("nuncio-domain".into())
        .spawn(move || domain_loop(d, rx))
        .expect("не удалось запустить поток домена");

    // Штатное завершение по сигналам: закрыть окно, остановить сеть.
    if let Ok(mut signals) =
        signal_hook::iterator::Signals::new([signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGHUP])
    {
        std::thread::Builder::new()
            .name("nuncio-signals".into())
            .spawn(move || {
                if let Some(sig) = signals.forever().next() {
                    log::info!("сигнал {sig}: завершение");
                    send(Msg::Stop);
                }
            })
            .expect("не удалось запустить поток сигналов");
    }

    // Для замеров (В4, метод П-5): полный кадр через take_snapshot().
    let _bench = std::env::var("NUNCIO_BENCH_FRAMES").ok().and_then(|v| v.parse::<usize>().ok()).map(|n| {
        let weak = app.as_weak();
        let t = slint::Timer::default();
        t.start(slint::TimerMode::SingleShot, Duration::from_secs(35), move || {
            let Some(app) = weak.upgrade() else { return };
            let mut ms: Vec<f64> = (0..n)
                .map(|_| {
                    let t0 = std::time::Instant::now();
                    let _ = app.window().take_snapshot();
                    t0.elapsed().as_secs_f64() * 1000.0
                })
                .collect();
            ms.sort_by(f64::total_cmp);
            let avg = ms.iter().sum::<f64>() / ms.len().max(1) as f64;
            let p95 = ms[(ms.len() * 95 / 100).min(ms.len() - 1)];
            log::info!("кадр: среднее {avg:.1} мс, p95 {p95:.1} мс, максимум {:.1} мс ({n} кадров)", ms[ms.len() - 1]);
        });
        t
    });
    app.run().expect("окно завершилось с ошибкой");
    send(Msg::Stop);
    let _ = domain.join();
    log::info!("выход");
}
