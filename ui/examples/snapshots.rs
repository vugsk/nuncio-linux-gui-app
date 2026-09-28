//! Снимки экранов программным рендерером на заготовленных данных
//! (Архитектура/nuncio - Приложение (Rust и Slint).md, раздел 8): без окна и
//! без сети. Каждое состояние модели входа рисуется в PNG.
//!
//!     cargo run -p ui --example snapshots -- <каталог>

use std::rc::Rc;

use domain::login::{CriticalKind, Intent, LoginModel};
use domain::model::{
    AuthState, AuthStep, CodeInfo, CommandResult, ConnState, Country, CountryCode, ErrorInfo, ErrorKind, Event,
    Payload, ProxyKind, ProxySettings, User,
};
use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize};

struct Offscreen(Rc<MinimalSoftwareWindow>);

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

const W: u32 = 1100;
const H: u32 = 800;

fn countries() -> Vec<Country> {
    let c = |iso: &str, ru: &str, code: &str| Country {
        iso2: iso.into(),
        name: ru.into(),
        default_name: ru.into(),
        codes: vec![CountryCode { code: code.into(), prefixes: vec![], patterns: vec!["XXX XXX XXXX".into()] }],
    };
    vec![
        c("RU", "Россия", "7"),
        c("RO", "Румыния", "40"),
        c("RW", "Руанда", "250"),
        c("SI", "Словения", "386"),
        c("DE", "Германия", "49"),
        c("JP", "Япония", "81"),
        c("BR", "Бразилия", "55"),
        c("UA", "Украина", "380"),
    ]
}

/// Модель после старта: вышли, QR показан, страны загружены.
fn base(lang: &str) -> LoginModel {
    let mut m = LoginModel::new(lang, "RU", ProxySettings::default(), 1_000_000);
    let fx = m.event(Event::Auth(AuthState { step: AuthStep::LoggedOut, ..Default::default() }));
    let countries_id = fx
        .iter()
        .find_map(|e| match e {
            domain::login::Effect::Send { id, cmd: domain::model::Command::GetCountries { .. } } => Some(*id),
            _ => None,
        })
        .unwrap();
    m.event(Event::Result(CommandResult { cmd_id: countries_id, error: None, payload: Payload::Countries(countries()) }));
    m.event(Event::Auth(AuthState {
        step: AuthStep::Qr,
        qr_url: "tg://login?token=AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA=".into(),
        qr_expires_unix: 1_000_030,
        ..Default::default()
    }));
    m
}

fn code_step(m: &mut LoginModel) {
    m.intent(Intent::ShowPhone);
    m.intent(Intent::PhoneEdited("9991234567".into()));
    m.intent(Intent::SubmitPhone);
    m.event(Event::Auth(AuthState {
        step: AuthStep::Code,
        code: CodeInfo { kind: "app".into(), length: 5, next_kind: "sms".into(), timeout: 42 },
        ..Default::default()
    }));
}

fn err(m: &mut LoginModel, kind: ErrorKind, text: &str, retry: u32) {
    // Ошибка на последнюю отправленную команду.
    let id = (1..100).rev().find(|_| true).unwrap();
    let _ = id;
    let last = m.clone().intent(Intent::DismissError); // не меняет pending
    let _ = last;
    m.event(Event::Result(CommandResult {
        cmd_id: last_id(m),
        error: Some(ErrorInfo { kind, code: 400, text: text.into(), retry_after: retry }),
        payload: Payload::None,
    }));
}

/// id последней отправленной команды: следующий выданный минус один.
fn last_id(m: &LoginModel) -> u64 {
    let mut probe = m.clone();
    match probe.intent(Intent::SetLanguage(if m.lang == "ru" { "en".into() } else { "ru".into() })).iter().find_map(|e| {
        match e {
            domain::login::Effect::Send { id, .. } => Some(*id),
            _ => None,
        }
    }) {
        Some(next) => next - 1,
        None => 0,
    }
}

fn states() -> Vec<(&'static str, LoginModel)> {
    let mut v = Vec::new();

    v.push(("01-qr-ru", base("ru")));
    v.push(("02-qr-en", base("en")));

    let mut m = base("ru");
    m.intent(Intent::ShowPhone);
    m.intent(Intent::PhoneEdited("9991234567".into()));
    v.push(("03-phone", m.clone()));

    let mut e = m.clone();
    e.intent(Intent::SubmitPhone);
    err(&mut e, ErrorKind::Rpc, "PHONE_NUMBER_INVALID", 0);
    v.push(("04-phone-invalid", e));

    let mut f = m.clone();
    f.intent(Intent::SubmitPhone);
    err(&mut f, ErrorKind::Flood, "FLOOD_WAIT", 42);
    v.push(("05-phone-flood", f));

    let mut c = base("ru");
    c.intent(Intent::ShowPhone);
    c.intent(Intent::OpenCountries);
    c.intent(Intent::FilterCountries("р".into()));
    v.push(("06-countries", c));

    let mut k = base("ru");
    code_step(&mut k);
    k.intent(Intent::CodeEdited("123".into()));
    v.push(("07-code", k.clone()));

    let mut ke = base("ru");
    code_step(&mut ke);
    ke.intent(Intent::CodeEdited("12345".into()));
    err(&mut ke, ErrorKind::Rpc, "PHONE_CODE_INVALID", 0);
    v.push(("08-code-invalid", ke));

    let mut kr = k.clone();
    kr.intent(Intent::Tick(1_000_100));
    v.push(("09-code-resend", kr));

    let mut p = base("ru");
    code_step(&mut p);
    p.event(Event::Auth(AuthState {
        step: AuthStep::Password,
        password_hint: "девичья фамилия матери".into(),
        ..Default::default()
    }));
    v.push(("10-password", p.clone()));
    p.intent(Intent::SubmitPassword("x".into()));
    err(&mut p, ErrorKind::Rpc, "PASSWORD_HASH_INVALID", 0);
    v.push(("11-password-invalid", p));

    let mut s = base("ru");
    code_step(&mut s);
    s.event(Event::Auth(AuthState { step: AuthStep::SignUp, terms: "Текст условий…".into(), ..Default::default() }));
    v.push(("12-signup", s.clone()));
    s.intent(Intent::ToggleTerms);
    v.push(("13-signup-accepted", s));

    let mut x = base("ru");
    x.intent(Intent::OpenProxy);
    x.intent(Intent::ProxyDraft(ProxySettings {
        kind: ProxyKind::Socks5,
        addr: "127.0.0.1:1080".into(),
        user: "user".into(),
        ..Default::default()
    }));
    x.intent(Intent::TestProxy);
    x.event(Event::Result(CommandResult { cmd_id: last_id(&x), error: None, payload: Payload::ProxyCheck { rtt_ms: 87 } }));
    v.push(("14-proxy", x));

    let mut b = LoginModel::new("ru", "RU", ProxySettings::default(), 0);
    b.event(Event::Auth(AuthState { step: AuthStep::Ready, user: Some(User::default()), ..Default::default() }));
    b.event(Event::Auth(AuthState { step: AuthStep::LoggedOut, reason: "session_ended".into(), ..Default::default() }));
    b.event(Event::Connection(ConnState::WaitingNetwork));
    v.push(("15-session-ended-offline", b));

    let mut k1 = LoginModel::new("ru", "RU", ProxySettings::default(), 0);
    k1.critical(CriticalKind::KeyringUnavailable, "keyring: связка ключей (Secret Service) недоступна".into());
    v.push(("16-critical-keyring", k1));
    let mut k2 = LoginModel::new("ru", "RU", ProxySettings::default(), 0);
    k2.critical(CriticalKind::DatabaseCorrupt, "база повреждена: file is not a database".into());
    v.push(("17-critical-db", k2));
    let mut k3 = LoginModel::new("en", "RU", ProxySettings::default(), 0);
    k3.critical(CriticalKind::NoApiCredentials, String::new());
    v.push(("18-critical-apienv-en", k3));

    v.push(("19-starting", LoginModel::new("ru", "RU", ProxySettings::default(), 0)));

    let mut mm = LoginModel::new("ru", "RU", ProxySettings::default(), 0);
    mm.event(Event::Auth(AuthState {
        step: AuthStep::Ready,
        user: Some(User { id: 1, first_name: "Герман".into(), ..Default::default() }),
        ..Default::default()
    }));
    v.push(("20-main-placeholder", mm));
    v
}

fn chat_model(open: Option<i64>) -> domain::chat::ChatModel {
    use domain::chatstore::DialogRow;
    use domain::model::{Dialog, Message, Peer, PeerKind};
    let now = 1_790_000_000;
    let row = |id: i64, name: &str, text: &str, out: bool, unread: i32, pinned: bool, ago: i64, media: &str| DialogRow {
        dialog: Dialog { peer_id: id, top_message: 10, unread_count: unread, pinned, read_outbox_max_id: 9,
            ..Default::default() },
        peer: Peer { id, kind: PeerKind::of(id), name: name.into(), is_self: id == 1, ..Default::default() },
        last: Some(Message { peer_id: id, id: 10, date: now - ago, out, text: text.into(), media: media.into(),
            ..Default::default() }),
        top_date: now - ago,
        avatar_path: String::new(),
    };
    let mut m = domain::chat::ChatModel::new(1, "ru", now);
    m.set_rows(vec![
        row(1, "Я", "заметка про сборку", true, 0, true, 60, ""),
        row(100, "Мария", "ок, договорились", true, 0, false, 600, ""),
        row(-200, "Рабочая группа", "релиз готов, можно проверять", false, 3, false, 1200, ""),
        row(300, "Дима", "", false, 0, false, 90_000, "photo"),
        row(-1_000_000_000_400, "Канал новостей", "Обновление 1.2: исправления", false, 12, false, 200_000, ""),
        row(500, "Анна", "Длинное сообщение, которое не помещается в одну строку списка и обрезается многоточием", false, 1, false, 900_000, ""),
    ]);
    if let Some(id) = open {
        m.intent(domain::chat::ChatIntent::OpenChat(id));
        let msg = |mid: i32, from: i64, out: bool, ago: i64, text: &str| Message {
            peer_id: id, id: mid, from_id: from, out, date: now - ago, text: text.into(), ..Default::default()
        };
        let h = vec![
            msg(1, 7, false, 90_000, "вчерашнее сообщение"),
            Message { service: "ChatAddUser".into(), ..msg(2, 7, false, 3000, "") },
            msg(3, 7, false, 1300, "релиз готов, можно проверять"),
            msg(4, 7, false, 1290, "ссылка на сборку ниже"),
            msg(5, 8, false, 1250, "https://example.com/build/1234\nи короткое пояснение"),
            msg(6, 1, true, 1200, "Собираю сейчас, ссылка ниже"),
            Message { edit_date: 5, ..msg(7, 1, true, 1100, "Готово, проверяйте") },
            Message { media: "photo".into(), ..msg(8, 8, false, 600, "Вид с балкона") },
            Message { send_state: 1, ..msg(-1, 1, true, 30, "тест очереди отправки") },
            Message { send_state: 3, send_error: "expired".into(), ..msg(-2, 1, true, 20, "не ушло, нет связи") },
        ];
        let senders = std::collections::HashMap::from([(7, "Пётр".to_string()), (8, "Мария".to_string())]);
        m.set_history(id, h, senders);
    }
    m
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "snapshots".into());
    std::fs::create_dir_all(&dir).unwrap();
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Offscreen(window.clone()))).unwrap();
    // Главное окно: широкое и узкое.
    let mut main_states: Vec<(&str, Option<i64>, u32)> =
        vec![("21-main-list", None, W), ("22-main-group", Some(-200), W), ("23-main-narrow-chat", Some(-200), 700)];
    main_states.push(("24-main-narrow-list", None, 700));
    for (name, open, width) in main_states {
        let app = ui::App::new().unwrap();
        ui::apply_language("ru");
        window.set_size(PhysicalSize::new(width, H));
        app.show().unwrap();
        let mut login = LoginModel::new("ru", "RU", ProxySettings::default(), 0);
        login.event(Event::Auth(AuthState { step: AuthStep::Ready, user: Some(User { id: 1, ..Default::default() }),
            ..Default::default() }));
        ui::Renderer::default().render(&app, &login);
        let chat = chat_model(open);
        ui::chatview::ChatRenderer::default().render(&app, &chat, &Default::default());
        for _ in 0..3 {
            slint::platform::update_timers_and_animations();
            let mut buf = vec![PremultipliedRgbaColor::default(); (width * H) as usize];
            window.request_redraw();
            window.draw_if_needed(|renderer| {
                renderer.render(&mut buf, width as usize);
            });
            if name.is_empty() {
                continue;
            }
            let rgba: Vec<u8> = buf.iter().flat_map(|p| [p.red, p.green, p.blue, 255]).collect();
            let file = std::fs::File::create(format!("{dir}/{name}.png")).unwrap();
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), width, H);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().unwrap().write_image_data(&rgba).unwrap();
        }
        app.hide().unwrap();
        println!("{dir}/{name}.png");
    }

    for (name, model) in states() {
        let app = ui::App::new().unwrap();
        ui::apply_language(&model.lang);
        window.set_size(PhysicalSize::new(W, H));
        app.show().unwrap();
        let mut r = ui::Renderer::default();
        r.render(&app, &model);
        if model.proxy.open {
            ui::Renderer::fill_proxy(&app, &model.proxy.draft);
        }
        slint::platform::update_timers_and_animations();
        let mut buf = vec![PremultipliedRgbaColor::default(); (W * H) as usize];
        window.request_redraw();
        window.draw_if_needed(|renderer| {
            renderer.render(&mut buf, W as usize);
        });
        let rgba: Vec<u8> = buf.iter().flat_map(|p| [p.red, p.green, p.blue, 255]).collect();
        let file = std::fs::File::create(format!("{dir}/{name}.png")).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), W, H);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&rgba).unwrap();
        app.hide().unwrap();
        println!("{dir}/{name}.png");
    }
}
