//! Каталоги XDG, `api.env` и `config.toml` (Архитектура/nuncio - Данные и
//! хранилище.md, разделы 1 и 6). Настройки — владелец Rust.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::ProxySettings;

/// Каталоги приложения (XDG, переопределяются переменными окружения).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config: PathBuf, // ~/.config/nuncio
    pub data: PathBuf,   // ~/.local/share/nuncio
    pub cache: PathBuf,  // ~/.cache/nuncio
    pub state: PathBuf,  // ~/.local/state/nuncio (журналы)
}

impl Paths {
    /// Каталоги по переменным окружения: `NUNCIO_HOME` (всё в одном каталоге,
    /// для проверок), иначе `XDG_*_HOME`, иначе стандартные под `HOME`.
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Self {
        if let Some(root) = get("NUNCIO_HOME").filter(|s| !s.is_empty()) {
            let r = PathBuf::from(root);
            return Self { config: r.join("config"), data: r.join("data"), cache: r.join("cache"), state: r.join("state") };
        }
        let home = PathBuf::from(get("HOME").unwrap_or_else(|| ".".into()));
        let xdg = |var: &str, def: &str| {
            get(var).filter(|s| s.starts_with('/')).map(PathBuf::from).unwrap_or_else(|| home.join(def)).join("nuncio")
        };
        Self {
            config: xdg("XDG_CONFIG_HOME", ".config"),
            data: xdg("XDG_DATA_HOME", ".local/share"),
            cache: xdg("XDG_CACHE_HOME", ".cache"),
            state: xdg("XDG_STATE_HOME", ".local/state"),
        }
    }

    pub fn system() -> Self {
        Self::from_env(|k| std::env::var(k).ok())
    }

    pub fn database(&self) -> PathBuf {
        self.data.join("nuncio.db")
    }
}

/// `api_id` и `api_hash` (вне репозитория, Р-7).
#[derive(Clone, PartialEq, Eq)]
pub struct ApiCredentials {
    pub api_id: i32,
    pub api_hash: String,
}

impl std::fmt::Debug for ApiCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // api_hash в журнал не попадает.
        write!(f, "ApiCredentials {{ api_id: {}, api_hash: <скрыт> }}", self.api_id)
    }
}

/// Разбирает `KEY=VALUE` (комментарии `#`, кавычки вокруг значения).
pub fn parse_env(text: &str) -> HashMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| {
            let v = v.trim();
            let v = v.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or(v);
            let v = v.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')).unwrap_or(v);
            (k.trim().trim_start_matches("export ").to_string(), v.to_string())
        })
        .collect()
}

/// Читает `api.env`. `None` — файла нет или в нём нет обоих ключей.
pub fn load_api_credentials(config_dir: &Path) -> Option<ApiCredentials> {
    let text = std::fs::read_to_string(config_dir.join("api.env")).ok()?;
    let env = parse_env(&text);
    let api_id = env.get("API_ID")?.parse().ok().filter(|&n: &i32| n > 0)?;
    let api_hash = env.get("API_HASH").filter(|h| h.len() == 32 && h.chars().all(|c| c.is_ascii_hexdigit()))?;
    Some(ApiCredentials { api_id, api_hash: api_hash.clone() })
}

/// `config.toml` (версионируется полем `version`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    /// Язык интерфейса: `ru`, `en` или пусто (по системной локали, Вх-8).
    pub lang: String,
    /// Последняя выбранная страна при входе (Вх-7).
    pub last_country: String,
    pub proxy: ProxySettings,
    /// Тестовые серверы Telegram (запасной путь, Э-2 п. 7).
    pub test_servers: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { version: 1, lang: String::new(), last_country: String::new(), proxy: ProxySettings::default(),
            test_servers: false }
    }
}

impl Config {
    /// Читает файл; нет файла — значения по умолчанию, ошибка разбора — тоже
    /// (но с текстом ошибки для журнала).
    pub fn load(config_dir: &Path) -> (Self, Option<String>) {
        match std::fs::read_to_string(config_dir.join("config.toml")) {
            Err(_) => (Self::default(), None),
            Ok(text) => match toml::from_str(&text) {
                Ok(c) => (c, None),
                Err(e) => (Self::default(), Some(format!("config.toml не разобран: {e}"))),
            },
        }
    }

    /// Пишет атомарно (временный файл и переименование), права 0600 (там пароль прокси).
    pub fn save(&self, config_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(config_dir)?;
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = config_dir.join(".config.toml.tmp");
        std::fs::write(&tmp, text)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(tmp, config_dir.join("config.toml"))
    }
}

/// Язык интерфейса: из настроек, иначе из системной локали, иначе английский.
pub fn ui_language(cfg: &Config, get: impl Fn(&str) -> Option<String>) -> &'static str {
    match cfg.lang.as_str() {
        "ru" => return "ru",
        "en" => return "en",
        _ => {}
    }
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(v) = get(var).filter(|v| !v.is_empty()) {
            return if v.starts_with("ru") { "ru" } else { "en" };
        }
    }
    "en"
}

/// Страна из системной локали (`ru_RU.UTF-8` → `RU`), Вх-7.
pub fn locale_country(get: impl Fn(&str) -> Option<String>) -> Option<String> {
    for var in ["LC_ALL", "LC_ADDRESS", "LC_TELEPHONE", "LANG"] {
        if let Some(v) = get(var) {
            let base = v.split(['.', '@']).next().unwrap_or("");
            if let Some((_, c)) = base.split_once('_') {
                if c.len() == 2 && c.chars().all(|x| x.is_ascii_alphabetic()) {
                    return Some(c.to_ascii_uppercase());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    }

    #[test]
    fn paths_follow_xdg() {
        let p = Paths::from_env(env(&[("HOME", "/home/u"), ("XDG_CONFIG_HOME", "/cfg"), ("XDG_DATA_HOME", "rel")]));
        assert_eq!(p.config, PathBuf::from("/cfg/nuncio"));
        assert_eq!(p.data, PathBuf::from("/home/u/.local/share/nuncio"), "относительный XDG игнорируется");
        assert_eq!(p.state, PathBuf::from("/home/u/.local/state/nuncio"));
        let p = Paths::from_env(env(&[("HOME", "/home/u"), ("NUNCIO_HOME", "/tmp/n")]));
        assert_eq!(p.database(), PathBuf::from("/tmp/n/data/nuncio.db"));
    }

    #[test]
    fn api_env_is_parsed_strictly() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_api_credentials(dir.path()).is_none());
        std::fs::write(dir.path().join("api.env"), "# ключи\nAPI_ID=12345\nAPI_HASH=\"0123456789abcdef0123456789abcdef\"\n")
            .unwrap();
        let c = load_api_credentials(dir.path()).unwrap();
        assert_eq!(c.api_id, 12345);
        assert!(!format!("{c:?}").contains("0123456789abcdef"), "api_hash попал в Debug");
        std::fs::write(dir.path().join("api.env"), "API_ID=x\nAPI_HASH=0123456789abcdef0123456789abcdef\n").unwrap();
        assert!(load_api_credentials(dir.path()).is_none());
        std::fs::write(dir.path().join("api.env"), "API_ID=1\nAPI_HASH=short\n").unwrap();
        assert!(load_api_credentials(dir.path()).is_none());
    }

    #[test]
    fn config_roundtrip_and_bad_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Config::load(dir.path()).0, Config::default());
        let mut c = Config { lang: "en".into(), last_country: "DE".into(), ..Default::default() };
        c.proxy.kind = crate::model::ProxyKind::Socks5;
        c.proxy.addr = "127.0.0.1:1080".into();
        c.save(dir.path()).unwrap();
        assert_eq!(Config::load(dir.path()), (c, None));
        std::fs::write(dir.path().join("config.toml"), "version = [").unwrap();
        let (d, err) = Config::load(dir.path());
        assert_eq!(d, Config::default());
        assert!(err.is_some());
    }

    #[test]
    fn language_and_country_from_locale() {
        let c = Config::default();
        assert_eq!(ui_language(&c, env(&[("LANG", "ru_RU.UTF-8")])), "ru");
        assert_eq!(ui_language(&c, env(&[("LANG", "de_DE.UTF-8")])), "en");
        assert_eq!(ui_language(&Config { lang: "en".into(), ..c.clone() }, env(&[("LANG", "ru_RU.UTF-8")])), "en");
        assert_eq!(locale_country(env(&[("LANG", "ru_RU.UTF-8")])).as_deref(), Some("RU"));
        assert_eq!(locale_country(env(&[("LANG", "C.UTF-8")])), None);
        assert_eq!(locale_country(env(&[("LC_ALL", "pt_BR@euro")])).as_deref(), Some("BR"));
    }
}
