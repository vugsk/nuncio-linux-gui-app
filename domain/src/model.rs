//! Сущности домена для стыка с сетью. Отдельны от типов protobuf
//! (Архитектура/nuncio - Приложение (Rust и Slint).md, раздел 1): `bridge`
//! отображает их в `client.proto` и обратно.

/// Прокси (Вх-4, настройки соединения).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProxySettings {
    pub kind: ProxyKind,
    #[serde(default)]
    pub addr: String,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub secret: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyKind {
    #[default]
    None,
    Socks5,
    MtProxy,
}

/// Команда сетевому слою.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Ping(String),
    RequestQr,
    SendCode { phone: String },
    ResendCode,
    CancelCode,
    SignIn { code: String },
    CheckPassword { password: String },
    SignUp { first_name: String, last_name: String },
    LogOut,
    SetProxy(ProxySettings),
    TestProxy(ProxySettings),
    GetCountries { lang: String },
    RetryStartup,
}

/// Шаг входа.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthStep {
    #[default]
    Unknown,
    Qr,
    Code,
    Password,
    SignUp,
    Ready,
    LoggedOut,
}

/// Куда и какой код отправлен.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodeInfo {
    pub kind: String,
    pub length: u32,
    pub next_kind: String,
    pub timeout: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct User {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
    pub username: String,
}

impl User {
    pub fn display_name(&self) -> String {
        let n = format!("{} {}", self.first_name, self.last_name);
        n.trim().to_string()
    }
}

/// Состояние входа от сети.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthState {
    pub step: AuthStep,
    pub qr_url: String,
    pub qr_expires_unix: i64,
    pub code: CodeInfo,
    pub password_hint: String,
    pub has_recovery: bool,
    pub terms: String,
    pub user: Option<User>,
    /// LoggedOut: `logged_out`, `session_ended`, `keyring_no_key`, `session_broken` или пусто.
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Connecting,
    Connected,
    WaitingNetwork,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupErrorKind {
    KeyringUnavailable,
    KeyringDismissed,
    Config,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Network,
    Rpc,
    Flood,
    Auth,
    Cancelled,
    Internal,
}

/// Единая ошибка стыка: реакция выбирается по `kind`, а не по строке.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorInfo {
    pub kind: ErrorKind,
    pub code: i32,
    pub text: String,
    pub retry_after: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CountryCode {
    pub code: String,
    pub prefixes: Vec<String>,
    pub patterns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Country {
    pub iso2: String,
    pub name: String,
    pub default_name: String,
    pub codes: Vec<CountryCode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    None,
    Pong { message: String, core_version: String },
    Countries(Vec<Country>),
    ProxyCheck { rtt_ms: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub cmd_id: u64,
    pub error: Option<ErrorInfo>,
    pub payload: Payload,
}

/// Событие от сети.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Result(CommandResult),
    Connection(ConnState),
    Auth(AuthState),
    Startup { kind: StartupErrorKind, text: String },
    Fatal(String),
}
