//! Модель экрана входа и старта (Дизайн/nuncio - Дизайн - Вход.md, Дизайн -
//! Состояния.md). Чистая: `intent`/`event` меняют состояние и возвращают
//! эффекты (команды сети, сохранение настроек, очистка данных), без окна и
//! без потоков — на этом держатся тесты.

use std::collections::HashMap;

use crate::model::{
    AuthState, AuthStep, CodeInfo, Command, CommandResult, ConnState, Country, ErrorInfo, ErrorKind, Event,
    Payload, ProxySettings, StartupErrorKind, User,
};
use crate::phone;

/// Какой экран показан.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    /// Старт: открывается связка ключей, восстанавливается сессия.
    Starting,
    /// Критическое состояние (Со-4).
    Critical(Critical),
    Login,
    /// Вошли (главное окно — В4).
    Main,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CriticalKind {
    KeyringUnavailable,
    KeyringDismissed,
    NoApiCredentials,
    DatabaseCorrupt,
    NetworkFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Critical {
    pub kind: CriticalKind,
    /// Технические подробности (для «Подробнее»; без секретов).
    pub detail: String,
}

/// Шаг входа (Дизайн - Вход, раздел 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Step {
    #[default]
    Qr,
    Phone,
    Countries,
    Code,
    Password,
    SignUp,
}

/// Ошибка на шаге (Дизайн - Вход, раздел 4). Тексты — в интерфейсе (`@tr`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginError {
    InvalidPhone,
    PhoneBanned,
    InvalidCode,
    CodeExpired,
    InvalidPassword,
    InvalidName,
    /// FLOOD_WAIT: до какого момента (unix) кнопка неактивна.
    Flood { until: i64 },
    Network,
    QrFailed,
    /// Прочее: код ошибки сервера или текст.
    Other(String),
}

/// Полоса над карточкой входа (Со-4: «Сеанс завершён на другом устройстве»).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Banner {
    SessionEnded,
    KeyringLostKey,
    SessionBroken,
}

/// Диалог «Настройки соединения» (Вх-4, шаг 7).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProxyDialog {
    pub open: bool,
    pub draft: ProxySettings,
    pub checking: bool,
    /// Результат проверки: Ok(мс) или текст ошибки.
    pub check: Option<Result<u32, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoginModel {
    pub screen: Option<Screen>,
    pub step: Step,
    pub busy: bool,
    pub error: Option<LoginError>,
    pub banner: Option<Banner>,
    pub conn: Option<ConnState>,
    pub lang: String,

    pub qr_url: String,
    pub qr_expires: i64,

    pub countries: Vec<Country>,
    pub country: Option<usize>,
    pub country_filter: String,
    pub default_country: String,
    /// Национальная часть номера (цифры).
    pub national: String,

    pub code_info: CodeInfo,
    pub code: String,
    pub resend_at: i64,

    pub password_hint: String,
    pub terms: String,
    pub terms_accepted: bool,

    pub user: Option<User>,
    pub proxy: ProxyDialog,
    pub saved_proxy: ProxySettings,
    /// Тема рабочего стола из портала (0 — не задано, 1 — тёмная, 2 — светлая).
    pub system_theme: i32,

    pub now: i64,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    phone_sent: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Countries,
    SendCode,
    ResendCode,
    SignIn,
    Password,
    SignUp,
    LogOut,
    TestProxy,
    Other,
}

/// Намерение пользователя.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    Tick(i64),
    ShowPhone,
    ShowQr,
    OpenCountries,
    CloseCountries,
    FilterCountries(String),
    PickCountry(String),
    PhoneEdited(String),
    SubmitPhone,
    CodeEdited(String),
    ResendCode,
    ChangeNumber,
    SubmitPassword(String),
    BackFromPassword,
    ToggleTerms,
    SubmitSignUp { first_name: String, last_name: String },
    BackFromSignUp,
    SetLanguage(String),
    OpenProxy,
    ProxyDraft(ProxySettings),
    TestProxy,
    SaveProxy,
    CancelProxy,
    Retry,
    LogOut,
    DismissError,
}

/// Эффект для внешнего мира.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Send { id: u64, cmd: Command },
    /// Сохранить в config.toml: язык, последнюю страну, прокси.
    SaveConfig,
    /// Переключить перевод интерфейса.
    ApplyLanguage(String),
    /// Удалить локальные данные аккаунта (Нс-6, Со-6).
    WipeLocalData,
    /// Закрыть приложение.
    Quit,
}

impl LoginModel {
    pub fn new(lang: &str, default_country: &str, proxy: ProxySettings, now: i64) -> Self {
        Self {
            screen: Some(Screen::Starting),
            lang: lang.to_string(),
            default_country: default_country.to_string(),
            saved_proxy: proxy,
            now,
            next_id: 1,
            ..Default::default()
        }
    }

    /// Экран критической ошибки, найденной до сети (нет api.env, повреждена база).
    pub fn critical(&mut self, kind: CriticalKind, detail: String) {
        self.screen = Some(Screen::Critical(Critical { kind, detail }));
    }

    pub fn screen(&self) -> Screen {
        self.screen.clone().unwrap_or(Screen::Starting)
    }

    fn send(&mut self, cmd: Command, pending: Pending, out: &mut Vec<Effect>) {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, pending);
        out.push(Effect::Send { id, cmd });
    }

    /// Выбранная страна.
    pub fn selected_country(&self) -> Option<&Country> {
        self.country.and_then(|i| self.countries.get(i))
    }

    /// Код набора выбранной страны (без «+»).
    pub fn dial_code(&self) -> String {
        self.selected_country().and_then(|c| c.codes.first()).map(|c| c.code.clone()).unwrap_or_default()
    }

    /// Номер с маской для поля ввода.
    pub fn formatted_national(&self) -> String {
        let pattern = self
            .selected_country()
            .and_then(|c| c.codes.first())
            .and_then(|code| phone::pattern_for(code, &self.national));
        phone::format_national(pattern, &self.national)
    }

    /// Страны под фильтром (поиск по названию на обоих языках и по коду).
    pub fn filtered_countries(&self) -> Vec<usize> {
        let f = self.country_filter.trim().to_lowercase();
        let fd = phone::digits(&f);
        self.countries
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                f.is_empty()
                    || c.name.to_lowercase().contains(&f)
                    || c.default_name.to_lowercase().contains(&f)
                    || (!fd.is_empty() && c.codes.iter().any(|k| k.code.starts_with(&fd)))
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Сколько секунд до повторной отправки кода (0 — можно).
    pub fn resend_left(&self) -> i64 {
        (self.resend_at - self.now).max(0)
    }

    /// Сколько секунд до конца FLOOD_WAIT (0 — нет).
    pub fn flood_left(&self) -> i64 {
        match self.error {
            Some(LoginError::Flood { until }) => (until - self.now).max(0),
            _ => 0,
        }
    }

    /// Нужен ли секундный таймер (обратный отсчёт на экране).
    pub fn needs_tick(&self) -> bool {
        self.resend_left() > 0 || self.flood_left() > 0 || (self.step == Step::Qr && self.qr_expires > 0)
    }

    fn enter_qr(&mut self, out: &mut Vec<Effect>) {
        self.step = Step::Qr;
        self.error = None;
        self.qr_url.clear();
        self.send(Command::RequestQr, Pending::Other, out);
    }

    fn map_error(&self, e: &ErrorInfo) -> LoginError {
        match e.kind {
            ErrorKind::Flood => LoginError::Flood { until: self.now + i64::from(e.retry_after.max(1)) },
            ErrorKind::Network | ErrorKind::Cancelled => LoginError::Network,
            _ => match e.text.as_str() {
                "PHONE_NUMBER_INVALID" | "PHONE_NUMBER_UNOCCUPIED" => LoginError::InvalidPhone,
                "PHONE_NUMBER_BANNED" => LoginError::PhoneBanned,
                "PHONE_CODE_INVALID" | "PHONE_CODE_EMPTY" => LoginError::InvalidCode,
                "PHONE_CODE_EXPIRED" | "PHONE_CODE_HASH_EMPTY" => LoginError::CodeExpired,
                "PASSWORD_HASH_INVALID" => LoginError::InvalidPassword,
                "FIRSTNAME_INVALID" | "LASTNAME_INVALID" => LoginError::InvalidName,
                t if t.starts_with("FLOOD_WAIT") => LoginError::Flood { until: self.now + 60 },
                t => LoginError::Other(if e.code != 0 { format!("{} {}", e.code, t) } else { t.to_string() }),
            },
        }
    }

    fn choose_default_country(&mut self) {
        if self.country.is_some() {
            return;
        }
        let want = self.default_country.to_ascii_uppercase();
        self.country = self.countries.iter().position(|c| c.iso2 == want).or(if self.countries.is_empty() {
            None
        } else {
            Some(0)
        });
    }

    /// Намерение пользователя.
    pub fn intent(&mut self, i: Intent) -> Vec<Effect> {
        let mut out = Vec::new();
        match i {
            Intent::Tick(now) => self.now = now,
            Intent::DismissError => self.error = None,
            Intent::ShowPhone => {
                self.step = Step::Phone;
                self.error = None;
            }
            Intent::ShowQr => self.enter_qr(&mut out),
            Intent::OpenCountries => {
                self.step = Step::Countries;
                self.country_filter.clear();
            }
            Intent::CloseCountries => self.step = Step::Phone,
            Intent::FilterCountries(f) => self.country_filter = f,
            Intent::PickCountry(iso) => {
                if let Some(i) = self.countries.iter().position(|c| c.iso2 == iso) {
                    self.country = Some(i);
                    self.default_country = iso;
                    out.push(Effect::SaveConfig);
                }
                self.step = Step::Phone;
            }
            Intent::PhoneEdited(text) => {
                let mut d = phone::digits(&text);
                // Вставка полного номера с кодом страны: страна определяется сама.
                if text.trim_start().starts_with('+') {
                    if let Some((c, code)) = phone::detect(&self.countries, &d) {
                        self.country = self.countries.iter().position(|x| x.iso2 == c.iso2);
                        d = d[code.code.len()..].to_string();
                    }
                }
                d.truncate(15);
                self.national = d;
                if matches!(self.error, Some(LoginError::InvalidPhone)) {
                    self.error = None;
                }
            }
            Intent::SubmitPhone => {
                if self.busy || self.flood_left() > 0 {
                    return out;
                }
                if self.national.len() < 4 || self.selected_country().is_none() {
                    self.error = Some(LoginError::InvalidPhone);
                    return out;
                }
                let full = format!("+{}{}", self.dial_code(), self.national);
                self.phone_sent = full.clone();
                self.busy = true;
                self.error = None;
                self.send(Command::SendCode { phone: full }, Pending::SendCode, &mut out);
            }
            Intent::CodeEdited(text) => {
                let mut d = phone::digits(&text);
                let max = if self.code_info.length > 0 { self.code_info.length as usize } else { 8 };
                d.truncate(max);
                self.code = d;
                if matches!(self.error, Some(LoginError::InvalidCode)) {
                    self.error = None;
                }
                // Все ячейки заполнены — отправляем сразу (как в Telegram Desktop).
                if !self.busy && self.code_info.length > 0 && self.code.len() == self.code_info.length as usize {
                    self.busy = true;
                    let code = self.code.clone();
                    self.send(Command::SignIn { code }, Pending::SignIn, &mut out);
                }
            }
            Intent::ResendCode => {
                if !self.busy && self.resend_left() == 0 && !self.code_info.next_kind.is_empty() {
                    self.busy = true;
                    self.send(Command::ResendCode, Pending::ResendCode, &mut out);
                }
            }
            Intent::ChangeNumber => {
                self.send(Command::CancelCode, Pending::Other, &mut out);
                self.step = Step::Phone;
                self.code.clear();
                self.error = None;
            }
            Intent::SubmitPassword(pw) => {
                if self.busy || pw.is_empty() {
                    return out;
                }
                self.busy = true;
                self.error = None;
                self.send(Command::CheckPassword { password: pw }, Pending::Password, &mut out);
            }
            Intent::BackFromPassword | Intent::BackFromSignUp => {
                self.step = Step::Phone;
                self.error = None;
                self.code.clear();
            }
            Intent::ToggleTerms => self.terms_accepted = !self.terms_accepted,
            Intent::SubmitSignUp { first_name, last_name } => {
                if self.busy || !self.terms_accepted {
                    return out;
                }
                if first_name.trim().is_empty() {
                    self.error = Some(LoginError::InvalidName);
                    return out;
                }
                self.busy = true;
                self.error = None;
                self.send(
                    Command::SignUp { first_name: first_name.trim().into(), last_name: last_name.trim().into() },
                    Pending::SignUp,
                    &mut out,
                );
            }
            Intent::SetLanguage(lang) => {
                if lang != self.lang {
                    self.lang = lang.clone();
                    out.push(Effect::ApplyLanguage(lang.clone()));
                    out.push(Effect::SaveConfig);
                    self.send(Command::GetCountries { lang }, Pending::Countries, &mut out);
                }
            }
            Intent::OpenProxy => {
                self.proxy = ProxyDialog { open: true, draft: self.saved_proxy.clone(), ..Default::default() };
            }
            Intent::ProxyDraft(p) => {
                self.proxy.draft = p;
                self.proxy.check = None;
            }
            Intent::TestProxy => {
                if !self.proxy.checking {
                    self.proxy.checking = true;
                    self.proxy.check = None;
                    let p = self.proxy.draft.clone();
                    self.send(Command::TestProxy(p), Pending::TestProxy, &mut out);
                }
            }
            Intent::SaveProxy => {
                self.saved_proxy = self.proxy.draft.clone();
                self.proxy.open = false;
                let p = self.saved_proxy.clone();
                self.send(Command::SetProxy(p), Pending::Other, &mut out);
                out.push(Effect::SaveConfig);
            }
            Intent::CancelProxy => self.proxy.open = false,
            Intent::Retry => {
                if let Screen::Critical(c) = self.screen() {
                    match c.kind {
                        CriticalKind::KeyringUnavailable | CriticalKind::KeyringDismissed | CriticalKind::NetworkFailed => {
                            self.screen = Some(Screen::Starting);
                            self.send(Command::RetryStartup, Pending::Other, &mut out);
                        }
                        // Без api.env и с повреждённой базой повтор — это перезапуск приложения.
                        CriticalKind::NoApiCredentials | CriticalKind::DatabaseCorrupt => {}
                    }
                }
            }
            Intent::LogOut => {
                if !self.busy {
                    self.busy = true;
                    self.send(Command::LogOut, Pending::LogOut, &mut out);
                }
            }
        }
        out
    }

    /// Событие сетевого слоя.
    pub fn event(&mut self, e: Event) -> Vec<Effect> {
        let mut out = Vec::new();
        match e {
            Event::Connection(c) => self.conn = Some(c),
            Event::Startup { kind, text } => {
                let kind = match kind {
                    StartupErrorKind::KeyringUnavailable => CriticalKind::KeyringUnavailable,
                    StartupErrorKind::KeyringDismissed => CriticalKind::KeyringDismissed,
                    StartupErrorKind::Config => CriticalKind::NoApiCredentials,
                    StartupErrorKind::Other => CriticalKind::NetworkFailed,
                };
                self.busy = false;
                self.screen = Some(Screen::Critical(Critical { kind, detail: text }));
            }
            Event::Transfer { .. } => {}
            Event::SystemTheme(v) => self.system_theme = v,
            Event::Fatal(text) => {
                self.screen = Some(Screen::Critical(Critical { kind: CriticalKind::NetworkFailed, detail: text }));
            }
            Event::Auth(a) => self.auth(a, &mut out),
            Event::Updates(_) => {} // обновления чатов — модель чата
            Event::Result(r) => self.result(r),
        }
        out
    }

    fn auth(&mut self, a: AuthState, out: &mut Vec<Effect>) {
        match a.step {
            AuthStep::Unknown => {}
            AuthStep::LoggedOut => {
                let was_in = matches!(self.screen(), Screen::Main) || self.user.is_some();
                self.user = None;
                self.busy = false;
                self.banner = match a.reason.as_str() {
                    "session_ended" => Some(Banner::SessionEnded),
                    "keyring_no_key" => Some(Banner::KeyringLostKey),
                    "session_broken" => Some(Banner::SessionBroken),
                    _ => None,
                };
                // Выход и завершённая сессия удаляют локальные данные (Нс-6, Со-6).
                if was_in || matches!(a.reason.as_str(), "logged_out" | "session_ended") {
                    out.push(Effect::WipeLocalData);
                }
                let first = !matches!(self.screen(), Screen::Login);
                self.screen = Some(Screen::Login);
                if first && self.countries.is_empty() {
                    let lang = self.lang.clone();
                    self.send(Command::GetCountries { lang }, Pending::Countries, out);
                }
                self.national.clear();
                self.code.clear();
                self.enter_qr(out);
            }
            AuthStep::Qr => {
                self.screen = Some(Screen::Login);
                if self.step == Step::Qr {
                    self.qr_url = a.qr_url;
                    self.qr_expires = a.qr_expires_unix;
                    if matches!(self.error, Some(LoginError::QrFailed)) {
                        self.error = None;
                    }
                }
            }
            AuthStep::Code => {
                self.screen = Some(Screen::Login);
                self.step = Step::Code;
                self.resend_at = self.now + i64::from(a.code.timeout);
                self.code_info = a.code;
                self.code.clear();
                self.error = None;
                self.busy = false;
            }
            AuthStep::Password => {
                self.screen = Some(Screen::Login);
                self.step = Step::Password;
                self.password_hint = a.password_hint;
                self.error = None;
                self.busy = false;
            }
            AuthStep::SignUp => {
                self.screen = Some(Screen::Login);
                self.step = Step::SignUp;
                self.terms = a.terms;
                self.terms_accepted = false;
                self.error = None;
                self.busy = false;
            }
            AuthStep::Ready => {
                self.screen = Some(Screen::Main);
                self.user = a.user;
                self.banner = None;
                self.error = None;
                self.busy = false;
                self.code.clear();
                self.phone_sent.clear();
            }
        }
    }

    fn result(&mut self, r: CommandResult) {
        let pending = if r.cmd_id == 0 {
            // Ошибка фонового QR-входа (cmd_id 0).
            if r.error.is_some() && self.step == Step::Qr {
                self.error = Some(LoginError::QrFailed);
            }
            return;
        } else {
            match self.pending.remove(&r.cmd_id) {
                Some(p) => p,
                None => return,
            }
        };
        match pending {
            Pending::Countries => {
                if let Payload::Countries(list) = r.payload {
                    let selected = self.selected_country().map(|c| c.iso2.clone());
                    let mut list = list;
                    list.sort_by_cached_key(|c| c.name.to_lowercase());
                    self.countries = list;
                    self.country = selected.and_then(|iso| self.countries.iter().position(|c| c.iso2 == iso));
                    self.choose_default_country();
                }
            }
            Pending::TestProxy => {
                self.proxy.checking = false;
                self.proxy.check = Some(match (r.error, r.payload) {
                    (None, Payload::ProxyCheck { rtt_ms }) => Ok(rtt_ms),
                    (Some(e), _) => Err(e.text),
                    _ => Err(String::new()),
                });
            }
            Pending::Other => {}
            Pending::SendCode | Pending::ResendCode | Pending::SignIn | Pending::Password | Pending::SignUp
            | Pending::LogOut => {
                if let Some(e) = r.error {
                    self.busy = false;
                    let mapped = self.map_error(&e);
                    if pending == Pending::SignIn && mapped == LoginError::InvalidCode {
                        self.code.clear();
                    }
                    self.error = Some(mapped);
                }
                // Успех приходит отдельным AuthState; busy снимается там.
                // У выхода с ошибкой сервера локальные данные всё равно удалены (LoggedOut).
            }
        }
    }

    /// Номер, на который отправлен код, с маской: «+7 999 123 4567».
    pub fn phone_sent(&self) -> String {
        let d = phone::digits(&self.phone_sent);
        match phone::detect(&self.countries, &d) {
            Some((_, code)) => {
                let national = &d[code.code.len()..];
                format!("+{} {}", code.code, phone::format_national(phone::pattern_for(code, national), national))
            }
            None => self.phone_sent.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CountryCode;

    fn countries() -> Vec<Country> {
        let c = |iso: &str, name: &str, code: &str| Country {
            iso2: iso.into(),
            name: name.into(),
            default_name: name.into(),
            codes: vec![CountryCode { code: code.into(), prefixes: vec![], patterns: vec!["XXX XXX XXXX".into()] }],
        };
        vec![c("RU", "Россия", "7"), c("DE", "Германия", "49"), c("RO", "Румыния", "40")]
    }

    fn sent(effects: &[Effect]) -> Vec<(u64, Command)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Send { id, cmd } => Some((*id, cmd.clone())),
                _ => None,
            })
            .collect()
    }

    fn ok(id: u64) -> Event {
        Event::Result(CommandResult { cmd_id: id, error: None, payload: Payload::None })
    }

    fn err(id: u64, kind: ErrorKind, text: &str, retry: u32) -> Event {
        Event::Result(CommandResult {
            cmd_id: id,
            error: Some(ErrorInfo { kind, code: 400, text: text.into(), retry_after: retry }),
            payload: Payload::None,
        })
    }

    fn step(s: AuthStep) -> AuthState {
        AuthState { step: s, ..Default::default() }
    }

    /// Модель после старта: вышли, QR запрошен, страны получены.
    fn logged_out() -> LoginModel {
        let mut m = LoginModel::new("ru", "DE", ProxySettings::default(), 1000);
        let fx = m.event(Event::Auth(step(AuthStep::LoggedOut)));
        let cmds = sent(&fx);
        assert!(matches!(cmds[0].1, Command::GetCountries { .. }), "{cmds:?}");
        assert_eq!(cmds[1].1, Command::RequestQr);
        assert_eq!(m.screen(), Screen::Login);
        assert_eq!(m.step, Step::Qr, "QR первым (Вх-1)");
        m.event(Event::Result(CommandResult { cmd_id: cmds[0].0, error: None, payload: Payload::Countries(countries()) }));
        m
    }

    #[test]
    fn default_country_and_mask() {
        let mut m = logged_out();
        assert_eq!(m.selected_country().unwrap().iso2, "DE", "страна по умолчанию из локали (Вх-7)");
        assert_eq!(m.countries[0].name, "Германия", "список отсортирован по названию");
        m.intent(Intent::ShowPhone);
        m.intent(Intent::PhoneEdited("+7 999 1234567".into()));
        assert_eq!(m.selected_country().unwrap().iso2, "RU", "вставка с кодом определяет страну");
        assert_eq!(m.formatted_national(), "999 123 4567");
        m.intent(Intent::FilterCountries("рум".into()));
        assert_eq!(m.filtered_countries().len(), 1);
        m.intent(Intent::FilterCountries("4".into()));
        assert_eq!(m.filtered_countries().len(), 2, "поиск по коду");
        let fx = m.intent(Intent::PickCountry("RO".into()));
        assert!(fx.contains(&Effect::SaveConfig), "последняя страна запоминается (Вх-7)");
        assert_eq!(m.dial_code(), "40");
    }

    #[test]
    fn phone_code_password_flow() {
        let mut m = logged_out();
        m.intent(Intent::ShowPhone);
        m.intent(Intent::PhoneEdited("12".into()));
        assert!(sent(&m.intent(Intent::SubmitPhone)).is_empty());
        assert_eq!(m.error, Some(LoginError::InvalidPhone));
        m.intent(Intent::PickCountry("RU".into()));
        m.intent(Intent::PhoneEdited("9991234567".into()));
        let fx = m.intent(Intent::SubmitPhone);
        let (id, cmd) = sent(&fx)[0].clone();
        assert_eq!(cmd, Command::SendCode { phone: "+79991234567".into() });
        assert!(m.busy);
        assert!(sent(&m.intent(Intent::SubmitPhone)).is_empty(), "повторное нажатие во время проверки");
        m.event(ok(id));
        m.event(Event::Auth(AuthState {
            step: AuthStep::Code,
            code: CodeInfo { kind: "app".into(), length: 5, next_kind: "sms".into(), timeout: 30 },
            ..Default::default()
        }));
        assert_eq!((m.step, m.busy, m.resend_left()), (Step::Code, false, 30));
        assert_eq!(m.phone_sent(), "+7 999 123 4567");
        assert!(sent(&m.intent(Intent::ResendCode)).is_empty(), "повтор до таймера");
        // Неполный код не отправляется, полный — сразу.
        assert!(sent(&m.intent(Intent::CodeEdited("12-3".into()))).is_empty());
        let (id, cmd) = sent(&m.intent(Intent::CodeEdited("123456789".into())))[0].clone();
        assert_eq!(cmd, Command::SignIn { code: "12345".into() });
        m.event(err(id, ErrorKind::Rpc, "PHONE_CODE_INVALID", 0));
        assert_eq!(m.error, Some(LoginError::InvalidCode));
        assert!(m.code.is_empty(), "неверный код стирается");
        let (id, _) = sent(&m.intent(Intent::CodeEdited("54321".into())))[0].clone();
        m.event(ok(id));
        m.event(Event::Auth(AuthState { step: AuthStep::Password, password_hint: "кот".into(), ..Default::default() }));
        assert_eq!((m.step, m.password_hint.as_str()), (Step::Password, "кот"));
        let (id, cmd) = sent(&m.intent(Intent::SubmitPassword("pw".into())))[0].clone();
        assert_eq!(cmd, Command::CheckPassword { password: "pw".into() });
        m.event(err(id, ErrorKind::Rpc, "PASSWORD_HASH_INVALID", 0));
        assert_eq!(m.error, Some(LoginError::InvalidPassword));
        let (id, _) = sent(&m.intent(Intent::SubmitPassword("pw2".into())))[0].clone();
        m.event(ok(id));
        let user = User { id: 1, first_name: "Имя".into(), ..Default::default() };
        m.event(Event::Auth(AuthState { step: AuthStep::Ready, user: Some(user.clone()), ..Default::default() }));
        assert_eq!(m.screen(), Screen::Main);
        assert_eq!(m.user, Some(user));
        m.intent(Intent::Tick(1031));
        assert_eq!(m.resend_left(), 0);
    }

    #[test]
    fn flood_wait_blocks_button_with_countdown() {
        let mut m = logged_out();
        m.intent(Intent::ShowPhone);
        m.intent(Intent::PickCountry("RU".into()));
        m.intent(Intent::PhoneEdited("9991234567".into()));
        let (id, _) = sent(&m.intent(Intent::SubmitPhone))[0].clone();
        m.event(err(id, ErrorKind::Flood, "FLOOD_WAIT", 42));
        assert_eq!(m.flood_left(), 42);
        assert!(m.needs_tick());
        assert!(sent(&m.intent(Intent::SubmitPhone)).is_empty(), "кнопка неактивна до конца таймера");
        m.intent(Intent::Tick(1042));
        assert!(!sent(&m.intent(Intent::SubmitPhone)).is_empty());
    }

    #[test]
    fn qr_shown_and_back_from_phone_requests_new_one() {
        let mut m = logged_out();
        m.event(Event::Auth(AuthState { step: AuthStep::Qr, qr_url: "tg://login?token=a".into(), qr_expires_unix: 1030,
            ..Default::default() }));
        assert_eq!(m.qr_url, "tg://login?token=a");
        m.intent(Intent::ShowPhone);
        // QR, пришедший, пока открыт шаг номера, шаг не переключает.
        m.event(Event::Auth(AuthState { step: AuthStep::Qr, qr_url: "tg://login?token=b".into(), ..Default::default() }));
        assert_eq!(m.step, Step::Phone);
        assert_eq!(sent(&m.intent(Intent::ShowQr))[0].1, Command::RequestQr);
        assert_eq!(m.step, Step::Qr);
        // Ошибка фонового QR — «Обновить».
        m.event(err(0, ErrorKind::Network, "", 0));
        assert_eq!(m.error, Some(LoginError::QrFailed));
    }

    #[test]
    fn sign_up_requires_terms() {
        let mut m = logged_out();
        m.event(Event::Auth(AuthState { step: AuthStep::SignUp, terms: "Условия".into(), ..Default::default() }));
        let submit = |m: &mut LoginModel, f: &str| {
            sent(&m.intent(Intent::SubmitSignUp { first_name: f.into(), last_name: String::new() }))
        };
        assert!(submit(&mut m, "Имя").is_empty(), "без флажка (Вх-9)");
        m.intent(Intent::ToggleTerms);
        assert!(submit(&mut m, "  ").is_empty());
        assert_eq!(m.error, Some(LoginError::InvalidName));
        assert_eq!(submit(&mut m, " Имя ")[0].1, Command::SignUp { first_name: "Имя".into(), last_name: String::new() });
    }

    #[test]
    fn session_ended_wipes_and_shows_banner() {
        let mut m = LoginModel::new("ru", "RU", ProxySettings::default(), 0);
        m.event(Event::Auth(AuthState { step: AuthStep::Ready, user: Some(User::default()), ..Default::default() }));
        let fx = m.event(Event::Auth(AuthState { step: AuthStep::LoggedOut, reason: "session_ended".into(),
            ..Default::default() }));
        assert!(fx.contains(&Effect::WipeLocalData), "Со-6");
        assert_eq!(m.banner, Some(Banner::SessionEnded));
        assert_eq!((m.screen(), m.step), (Screen::Login, Step::Qr));
        // Первый запуск без сессии ничего не удаляет.
        let mut fresh = LoginModel::new("ru", "RU", ProxySettings::default(), 0);
        assert!(!fresh.event(Event::Auth(step(AuthStep::LoggedOut))).contains(&Effect::WipeLocalData));
    }

    #[test]
    fn critical_states_and_retry() {
        let mut m = LoginModel::new("ru", "RU", ProxySettings::default(), 0);
        m.event(Event::Startup { kind: StartupErrorKind::KeyringUnavailable, text: "нет службы".into() });
        assert!(matches!(m.screen(), Screen::Critical(Critical { kind: CriticalKind::KeyringUnavailable, .. })));
        assert_eq!(sent(&m.intent(Intent::Retry))[0].1, Command::RetryStartup);
        assert_eq!(m.screen(), Screen::Starting);
        m.critical(CriticalKind::NoApiCredentials, String::new());
        assert!(sent(&m.intent(Intent::Retry)).is_empty());
    }

    #[test]
    fn language_switch_reloads_countries() {
        let mut m = logged_out();
        let fx = m.intent(Intent::SetLanguage("en".into()));
        assert!(fx.contains(&Effect::ApplyLanguage("en".into())));
        assert!(fx.contains(&Effect::SaveConfig));
        let (id, cmd) = sent(&fx)[0].clone();
        assert_eq!(cmd, Command::GetCountries { lang: "en".into() });
        let mut en = countries();
        en[1].name = "Germany".into();
        let before = m.selected_country().unwrap().iso2.clone();
        m.event(Event::Result(CommandResult { cmd_id: id, error: None, payload: Payload::Countries(en) }));
        assert_eq!(m.selected_country().unwrap().iso2, before, "выбор страны сохраняется");
        assert!(m.intent(Intent::SetLanguage("en".into())).is_empty());
    }

    #[test]
    fn proxy_dialog() {
        let mut m = logged_out();
        m.intent(Intent::OpenProxy);
        let p = ProxySettings { kind: crate::model::ProxyKind::Socks5, addr: "h:1".into(), ..Default::default() };
        m.intent(Intent::ProxyDraft(p.clone()));
        let (id, cmd) = sent(&m.intent(Intent::TestProxy))[0].clone();
        assert_eq!(cmd, Command::TestProxy(p.clone()));
        assert!(m.proxy.checking);
        m.event(Event::Result(CommandResult { cmd_id: id, error: None, payload: Payload::ProxyCheck { rtt_ms: 90 } }));
        assert_eq!(m.proxy.check, Some(Ok(90)));
        let fx = m.intent(Intent::SaveProxy);
        assert_eq!(sent(&fx)[0].1, Command::SetProxy(p.clone()));
        assert!(fx.contains(&Effect::SaveConfig));
        assert_eq!(m.saved_proxy, p);
        assert!(!m.proxy.open);
    }
}
