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

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "snapshots".into());
    std::fs::create_dir_all(&dir).unwrap();
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Offscreen(window.clone()))).unwrap();
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
