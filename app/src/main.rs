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
    avatars: HashMap<String, SharedPixelBuffer<Rgba8Pixel>>,
    bad_avatars: HashSet<String>,
}

impl Domain {
    fn publish(&self, lang: Option<String>) {
        let mut o = self.out.lock().unwrap();
        o.model = Some(self.model.clone());
        o.chat = self.chat.clone();
        if o.avatars.len() != self.avatars.len() {
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
                    CHAT_RENDERER.with(|r| r.borrow_mut().render(&app, &c, &avatars));
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
            let (mut rows, mut history) = (false, None::<(i64, usize)>);
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
                    ChatEffect::Db(DbOp::ReloadHistory { peer_id, limit }) => history = Some((peer_id, limit)),
                    ChatEffect::Db(op) => self.db_write(op),
                }
            }
            if rows {
                let list = self
                    .db
                    .as_ref()
                    .and_then(|db| db.call(|s| s.dialog_rows(0, DIALOGS_WINDOW)))
                    .and_then(|r| r.map_err(|e| log::error!("список диалогов: {e}")).ok())
                    .unwrap_or_default();
                for r in &list {
                    self.decode_avatar(&r.avatar_path);
                }
                if self.chat.as_ref().is_some_and(|c| c.rows.len() != list.len()) {
                    log::info!("список: {} диалогов из базы", list.len());
                }
                if let Some(chat) = self.chat.as_mut() {
                    // Для замеров (В4): открыть n-й диалог, как только список загружен.
                    let bench = std::env::var("NUNCIO_BENCH_OPEN").ok().and_then(|v| v.parse::<usize>().ok());
                    let target = bench.and_then(|n| list.get(n).map(|r| r.peer.id));
                    queue.extend(chat.set_rows(list));
                    if let (Some(id), None) = (target, chat.open) {
                        queue.extend(chat.intent(domain::chat::ChatIntent::OpenChat(id)));
                    }
                }
            }
            if let Some((peer_id, limit)) = history {
                let loaded = self.db.as_ref().and_then(|db| {
                    db.call(move |s| {
                        let msgs = s.history(peer_id, None, limit)?;
                        // Имена отправителей для групп (серии, Чт-2).
                        let mut senders = HashMap::new();
                        for m in &msgs {
                            if m.from_id != 0 && !senders.contains_key(&m.from_id) {
                                if let Some(p) = s.peer(m.from_id)? {
                                    senders.insert(m.from_id, p.name);
                                }
                            }
                        }
                        Ok::<_, domain::store::StoreError>((msgs, senders))
                    })
                });
                match loaded {
                    Some(Ok((msgs, senders))) => {
                        if let Some(chat) = self.chat.as_mut() {
                            chat.set_history(peer_id, msgs, senders);
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
    }

    fn db_write(&self, op: DbOp) {
        let Some(db) = &self.db else { return };
        let self_id = self.chat.as_ref().map(|c| c.self_id).unwrap_or(0);
        let now = now();
        let res = db.call(move |s| match op {
            DbOp::Enqueue { peer_id, text, reply_to, random_id } => {
                s.outbox_enqueue(peer_id, &text, reply_to, random_id, self_id, now).map(|_| ())
            }
            DbOp::OutboxSending { local_id } => s.outbox_sending(local_id),
            DbOp::OutboxDone { local_id, sent } => s.outbox_done(local_id, sent.as_ref()),
            DbOp::OutboxRequeue { local_id, next_try } => s.outbox_requeue(local_id, next_try),
            DbOp::OutboxFail { local_id, error } => s.outbox_fail(local_id, &error),
            DbOp::OutboxRetry { local_id } => s.outbox_retry(local_id, now),
            DbOp::OutboxDelete { local_id } => s.outbox_delete(local_id),
            DbOp::PumpOutbox => Ok(()),
            DbOp::SaveDialogs { dialogs, messages, peers } => s.save_dialogs(&dialogs, &messages, &peers),
            DbOp::SaveHistory { messages, peers } => s.save_history(&messages, &peers),
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
    static CHAT_RENDERER: RefCell<ui::chatview::ChatRenderer> = RefCell::new(ui::chatview::ChatRenderer::default());
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
        bad_avatars: HashSet::new(),
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
