//! Разметка Slint и отображение моделей домена в неё. Типы Slint создаются
//! только в потоке UI (Архитектура/nuncio - Приложение (Rust и Slint).md,
//! раздел 2): `Renderer` живёт там же и получает готовые модели.

use std::collections::HashMap;
use std::io::Cursor;
use std::rc::Rc;

use domain::chat::ChatIntent;
use domain::login::{Banner, CriticalKind, Intent, LoginError, LoginModel, Screen, Step};
use domain::model::{ConnState, ProxyKind, ProxySettings};
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};

slint::include_modules!();

pub mod avatar;
pub mod chatview;

mod flags {
    include!(concat!(env!("OUT_DIR"), "/flags.rs"));
}

/// Действие пользователя, которое уходит из UI в поток домена.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Login(Intent),
    Chat(ChatIntent),
    Quit,
    ResetData,
}

/// PNG флага страны (из Noto Color Emoji, извлечён при сборке).
pub fn flag_png(iso2: &str) -> Option<&'static [u8]> {
    flags::FLAGS.binary_search_by(|(k, _)| (*k).cmp(iso2)).ok().map(|i| flags::FLAGS[i].1)
}

/// Число флагов в наборе.
pub fn flag_count() -> usize {
    flags::FLAGS.len()
}

fn decode_png(data: &[u8]) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let mut dec = png::Decoder::new(Cursor::new(data));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba {
        return None; // флаги пишутся при сборке в RGBA
    }
    buf.truncate(info.buffer_size());
    Some(SharedPixelBuffer::clone_from_slice(&buf, info.width, info.height))
}

/// Растр QR-кода: модуль = `scale` пикселей, без полей (поля даёт белая подложка).
pub fn qr_pixels(url: &str, scale: u32) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let code = qrcode::QrCode::with_error_correction_level(url.as_bytes(), qrcode::EcLevel::M).ok()?;
    let n = code.width() as u32;
    let colors = code.to_colors();
    let side = n * scale;
    let mut px = SharedPixelBuffer::<Rgba8Pixel>::new(side, side);
    let buf = px.make_mut_slice();
    for y in 0..side {
        for x in 0..side {
            let dark = colors[((y / scale) * n + x / scale) as usize] == qrcode::Color::Dark;
            let v = if dark { 0 } else { 255 };
            buf[(y * side + x) as usize] = Rgba8Pixel { r: v, g: v, b: v, a: 255 };
        }
    }
    Some(px)
}

fn mmss(secs: i64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Переносит состояние моделей в свойства Slint. Держит кэш, чтобы не
/// пересоздавать списки и картинки на каждом кадре.
#[derive(Default)]
pub struct Renderer {
    flags: HashMap<String, Image>,
    qr_url: String,
    countries_key: (usize, String, Option<usize>, String),
    countries: Option<Rc<VecModel<CountryRow>>>,
}

impl Renderer {
    fn flag(&mut self, iso: &str) -> Image {
        if let Some(img) = self.flags.get(iso) {
            return img.clone();
        }
        let img = flag_png(iso).and_then(decode_png).map(Image::from_rgba8).unwrap_or_default();
        self.flags.insert(iso.to_string(), img.clone());
        img
    }

    pub fn render(&mut self, app: &App, m: &LoginModel) {
        let shell = app.global::<Shell>();
        let login = app.global::<Login>();
        let proxy = app.global::<Proxy>();

        match m.screen() {
            Screen::Starting => shell.set_screen(0),
            Screen::Critical(c) => {
                shell.set_screen(1);
                shell.set_critical_kind(match c.kind {
                    CriticalKind::KeyringUnavailable => 0,
                    CriticalKind::KeyringDismissed => 1,
                    CriticalKind::NoApiCredentials => 2,
                    CriticalKind::DatabaseCorrupt => 3,
                    CriticalKind::NetworkFailed => 4,
                });
                shell.set_critical_detail(c.detail.into());
            }
            Screen::Login => shell.set_screen(2),
            Screen::Main => shell.set_screen(3),
        }
        shell.set_conn(match m.conn {
            Some(ConnState::Connecting) => 1,
            Some(ConnState::WaitingNetwork) => 2,
            Some(ConnState::Connected) | None => 0,
        });
        shell.set_busy(m.busy);
        shell.set_user_name(m.user.as_ref().map(|u| u.display_name()).unwrap_or_default().into());

        login.set_lang(m.lang.clone().into());
        login.set_step(match m.step {
            Step::Qr => 0,
            Step::Phone => 1,
            Step::Countries => 2,
            Step::Code => 3,
            Step::Password => 4,
            Step::SignUp => 5,
        });
        login.set_busy(m.busy);
        let (err, detail) = match &m.error {
            None => (0, String::new()),
            Some(LoginError::InvalidPhone) => (1, String::new()),
            Some(LoginError::PhoneBanned) => (2, String::new()),
            Some(LoginError::InvalidCode) => (3, String::new()),
            Some(LoginError::CodeExpired) => (4, String::new()),
            Some(LoginError::InvalidPassword) => (5, String::new()),
            Some(LoginError::InvalidName) => (6, String::new()),
            Some(LoginError::Flood { .. }) => (if m.flood_left() > 0 { 7 } else { 0 }, String::new()),
            Some(LoginError::Network) => (8, String::new()),
            Some(LoginError::QrFailed) => (9, String::new()),
            Some(LoginError::Other(t)) => (10, t.clone()),
        };
        login.set_error(err);
        login.set_error_detail(detail.into());
        login.set_flood_time(mmss(m.flood_left()).into());
        login.set_banner(match m.banner {
            None => 0,
            Some(Banner::SessionEnded) => 1,
            Some(Banner::KeyringLostKey) => 2,
            Some(Banner::SessionBroken) => 3,
        });

        // QR: картинка пересоздаётся только при смене токена.
        if m.qr_url != self.qr_url {
            self.qr_url = m.qr_url.clone();
            match (!m.qr_url.is_empty()).then(|| qr_pixels(&m.qr_url, 4)).flatten() {
                Some(px) => {
                    login.set_qr(Image::from_rgba8(px));
                    login.set_qr_ready(true);
                }
                None => login.set_qr_ready(false),
            }
        }

        // Страна и номер.
        if let Some(c) = m.selected_country() {
            let flag = self.flag(&c.iso2);
            login.set_country_flag(flag);
            login.set_country_name(c.name.clone().into());
            login.set_country_code(format!("+{}", m.dial_code()).into());
        }
        let formatted = m.formatted_national();
        if login.get_phone_text() != formatted.as_str() {
            login.set_phone_text(formatted.into());
        }
        let key = (m.countries.len(), m.country_filter.clone(), m.country, m.lang.clone());
        if key != self.countries_key || self.countries.is_none() {
            self.countries_key = key;
            let rows: Vec<CountryRow> = m
                .filtered_countries()
                .into_iter()
                .map(|i| {
                    let c = &m.countries[i];
                    CountryRow {
                        iso: c.iso2.clone().into(),
                        name: c.name.clone().into(),
                        code: c.codes.first().map(|k| format!("+{}", k.code)).unwrap_or_default().into(),
                        flag: self.flag(&c.iso2),
                        selected: Some(i) == m.country,
                    }
                })
                .collect();
            let model = Rc::new(VecModel::from(rows));
            login.set_countries(ModelRc::from(model.clone()));
            self.countries = Some(model);
        }

        // Код.
        login.set_code_length(i32::try_from(m.code_info.length).unwrap_or(5).clamp(1, 8));
        login.set_code_kind(match m.code_info.kind.as_str() {
            "app" => 0,
            "sms" | "fragment" => 1,
            "call" | "flash_call" | "missed_call" => 2,
            _ => 3,
        });
        if login.get_code_text() != m.code.as_str() {
            login.set_code_text(m.code.clone().into());
        }
        let chars: Vec<SharedString> = m.code.chars().map(|c| SharedString::from(c.to_string())).collect();
        login.set_code_chars(ModelRc::new(VecModel::from(chars)));
        login.set_phone_sent(m.phone_sent().into());
        login.set_resend_time(if m.resend_left() > 0 { mmss(m.resend_left()) } else { String::new() }.into());
        login.set_resend_kind(match m.code_info.next_kind.as_str() {
            "" => 0,
            "sms" | "fragment" => 1,
            "call" | "flash_call" | "missed_call" => 2,
            _ => 3,
        });
        login.set_password_hint(m.password_hint.clone().into());
        login.set_terms(m.terms.clone().into());
        login.set_terms_accepted(m.terms_accepted);

        // Прокси.
        proxy.set_open(m.proxy.open);
        proxy.set_checking(m.proxy.checking);
        match &m.proxy.check {
            None => proxy.set_check_state(0),
            Some(Ok(ms)) => {
                proxy.set_check_state(1);
                proxy.set_check_text(ms.to_string().into());
            }
            Some(Err(e)) => {
                proxy.set_check_state(2);
                proxy.set_check_text(e.clone().into());
            }
        }
    }

    /// Заполнить поля диалога прокси при открытии.
    pub fn fill_proxy(app: &App, p: &ProxySettings) {
        let proxy = app.global::<Proxy>();
        proxy.set_kind(match p.kind {
            ProxyKind::None => 0,
            ProxyKind::Socks5 => 1,
            ProxyKind::MtProxy => 2,
        });
        let (host, port) = p.addr.rsplit_once(':').unwrap_or((p.addr.as_str(), ""));
        proxy.set_host(host.into());
        proxy.set_port(port.into());
        proxy.set_user(p.user.clone().into());
        proxy.set_password(p.password.clone().into());
        proxy.set_secret(p.secret.clone().into());
    }
}

fn proxy_from_ui(app: &App) -> ProxySettings {
    let p = app.global::<Proxy>();
    let host = p.get_host().trim().to_string();
    let port = p.get_port().trim().to_string();
    ProxySettings {
        kind: match p.get_kind() {
            1 => ProxyKind::Socks5,
            2 => ProxyKind::MtProxy,
            _ => ProxyKind::None,
        },
        addr: if host.is_empty() { String::new() } else { format!("{host}:{port}") },
        user: p.get_user().to_string(),
        password: p.get_password().to_string(),
        secret: p.get_secret().trim().to_string(),
    }
}

/// Привязывает колбэки Slint к отправке действий.
pub fn bind(app: &App, send: impl Fn(Action) + Clone + 'static) {
    let login = app.global::<Login>();
    let s = send.clone();
    let i = move |intent: Intent| s(Action::Login(intent));
    macro_rules! on {
        ($setter:ident, $intent:expr) => {{
            let i = i.clone();
            login.$setter(move || i($intent));
        }};
    }
    on!(on_show_phone, Intent::ShowPhone);
    on!(on_show_qr, Intent::ShowQr);
    on!(on_open_countries, Intent::OpenCountries);
    on!(on_close_countries, Intent::CloseCountries);
    on!(on_submit_phone, Intent::SubmitPhone);
    on!(on_resend_code, Intent::ResendCode);
    on!(on_change_number, Intent::ChangeNumber);
    on!(on_toggle_terms, Intent::ToggleTerms);
    {
        let i = i.clone();
        let w = app.as_weak();
        login.on_back(move || {
            let step = w.upgrade().map(|a| a.global::<Login>().get_step()).unwrap_or(0);
            i(if step == 5 { Intent::BackFromSignUp } else { Intent::BackFromPassword })
        });
    }
    {
        let i = i.clone();
        let w = app.as_weak();
        login.on_open_proxy(move || {
            i(Intent::OpenProxy);
            // Поля диалога заполнит `fill_proxy` после применения намерения.
            let _ = &w;
        });
    }
    {
        let i = i.clone();
        login.on_filter_countries(move |t| i(Intent::FilterCountries(t.to_string())));
    }
    {
        let i = i.clone();
        login.on_pick_country(move |iso| i(Intent::PickCountry(iso.to_string())));
    }
    {
        let i = i.clone();
        login.on_phone_edited(move |t| i(Intent::PhoneEdited(t.to_string())));
    }
    {
        let i = i.clone();
        login.on_code_edited(move |t| i(Intent::CodeEdited(t.to_string())));
    }
    {
        let i = i.clone();
        login.on_submit_password(move |t| i(Intent::SubmitPassword(t.to_string())));
    }
    {
        let i = i.clone();
        login.on_submit_sign_up(move |f, l| {
            i(Intent::SubmitSignUp { first_name: f.to_string(), last_name: l.to_string() })
        });
    }
    {
        let i = i.clone();
        login.on_set_language(move |l| i(Intent::SetLanguage(l.to_string())));
    }

    let proxy = app.global::<Proxy>();
    {
        let i = i.clone();
        let w = app.as_weak();
        proxy.on_changed(move || {
            if let Some(a) = w.upgrade() {
                i(Intent::ProxyDraft(proxy_from_ui(&a)));
            }
        });
    }
    {
        let i = i.clone();
        proxy.on_test(move || i(Intent::TestProxy));
    }
    {
        let i = i.clone();
        proxy.on_save(move || i(Intent::SaveProxy));
    }
    {
        let i = i.clone();
        proxy.on_cancel(move || i(Intent::CancelProxy));
    }

    let chat = app.global::<Chat>();
    {
        let s = send.clone();
        chat.on_open(move |id| {
            if let Ok(id) = id.parse() {
                s(Action::Chat(ChatIntent::OpenChat(id)));
            }
        });
    }
    {
        let s = send.clone();
        chat.on_back(move || s(Action::Chat(ChatIntent::CloseChat)));
    }
    {
        let s = send.clone();
        chat.on_send(move || s(Action::Chat(ChatIntent::Send)));
    }
    {
        let s = send.clone();
        chat.on_composer_edited(move |t| s(Action::Chat(ChatIntent::ComposerEdited(t.to_string()))));
    }
    {
        let s = send.clone();
        chat.on_load_older(move || s(Action::Chat(ChatIntent::LoadOlder)));
    }
    {
        let i = i.clone();
        chat.on_log_out(move || i(Intent::LogOut));
    }

    let shell = app.global::<Shell>();
    {
        let i = i.clone();
        shell.on_retry(move || i(Intent::Retry));
    }
    {
        let i = i.clone();
        shell.on_log_out(move || i(Intent::LogOut));
    }
    {
        let s = send.clone();
        shell.on_quit(move || s(Action::Quit));
    }
    shell.on_reset_data(move || send(Action::ResetData));
}

/// Переключает язык интерфейса (Вх-8): `ru` или `en` (исходные строки — английские).
pub fn apply_language(lang: &str) {
    // Для английского перевода нет: строки в разметке уже английские.
    let _ = slint::select_bundled_translation(if lang == "ru" { "ru" } else { "" });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_are_embedded_and_decodable() {
        assert!(flag_count() > 240, "флагов {}", flag_count());
        for iso in ["RU", "US", "DE", "GB", "UA", "KZ", "JP"] {
            let px = decode_png(flag_png(iso).unwrap_or_else(|| panic!("нет флага {iso}"))).unwrap();
            assert!(px.width() <= 48 && px.height() <= 48 && px.width().max(px.height()) == 48);
            // Флаг не пустой: есть непрозрачные пиксели.
            assert!(px.as_slice().iter().filter(|p| p.a > 200).count() > 500, "{iso} почти прозрачный");
        }
        assert!(flag_png("ZZ").is_none());
    }

    #[test]
    fn qr_matrix_is_scaled() {
        let px = qr_pixels("tg://login?token=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", 4).unwrap();
        assert_eq!(px.width() % 4, 0);
        assert!(px.width() >= 25 * 4);
        // Угловой поисковый узор: левый верхний модуль тёмный.
        assert_eq!(px.as_slice()[0].r, 0);
    }

    /// Каждая строка `@tr` переведена на русский (Вх-8: оба языка сразу).
    #[test]
    fn russian_translation_is_complete() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let po = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/lang/ru/LC_MESSAGES/ui.po")).unwrap();
        let mut missing = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "slint") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for part in text.split("@tr(\"").skip(1) {
                let id = &part[..part.find('"').unwrap()];
                let entry = format!("msgid \"{id}\"\nmsgstr \"");
                match po.find(&entry) {
                    Some(i) if !po[i + entry.len()..].starts_with('"') => {}
                    _ => missing.push(id.to_string()),
                }
            }
        }
        assert!(missing.is_empty(), "нет перевода: {missing:?}");
    }

    #[test]
    fn time_format() {
        assert_eq!(mmss(42), "0:42");
        assert_eq!(mmss(125), "2:05");
    }
}
