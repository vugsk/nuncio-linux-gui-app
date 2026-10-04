//! Настройки (веха В9, Дизайн - Настройки: Нс-1…Нс-8): модальное окно с
//! разделами. Значения приложения живут в `config.toml` (раздел `[app]`,
//! владелец — Rust) и переживают выход из аккаунта; профиль и сеансы — у
//! Telegram. Здесь — состояние окна и что сделать при изменении; применение
//! (тема, язык, кэш) — у приложения через `ChatEffect::Settings*`.

use serde::{Deserialize, Serialize};

use crate::chat::{ChatEffect, ChatModel, Pending};
use crate::model::{Command, CommandResult, Payload, Profile, ProxySettings, Session};

/// Тема (Дз-1, Нс-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    System,
    Dark,
    Light,
    File,
}

/// Значения настроек приложения (`[app]` в `config.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub theme: ThemeChoice,
    /// Файл темы TOML (Нс-7), для `ThemeChoice::File`.
    pub theme_file: String,
    /// Размер текста сообщений, px (12…20).
    pub text_size: u32,
    /// «Графика: GPU» (Нс-8), применяется после перезапуска.
    pub gpu: bool,
    pub notify: bool,
    pub notify_hide_text: bool,
    pub notify_sound: bool,
    pub tray: bool,
    /// Закрытие окна сворачивает в трей (Нс-5), если трей доступен.
    pub close_to_tray: bool,
    /// Лимит кэша медиа, МБ (0 — без лимита).
    pub cache_limit_mb: u64,
    /// Куда предлагать сохранять файлы (пусто — «Загрузки» системы).
    pub downloads_dir: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::System,
            theme_file: String::new(),
            text_size: TEXT_SIZE_DEFAULT,
            gpu: false,
            notify: true,
            notify_hide_text: false,
            notify_sound: true,
            tray: true,
            close_to_tray: true,
            cache_limit_mb: 1024,
            downloads_dir: String::new(),
        }
    }
}

pub const TEXT_SIZE_DEFAULT: u32 = 13;
pub const TEXT_SIZE_MIN: u32 = 12;
pub const TEXT_SIZE_MAX: u32 = 20;
/// Варианты лимита кэша, МБ (0 — без лимита).
pub const CACHE_LIMITS: [u64; 5] = [512, 1024, 2048, 5120, 0];

impl AppSettings {
    /// Лимит кэша в байтах (без лимита — очень много).
    pub fn cache_limit_bytes(&self) -> i64 {
        if self.cache_limit_mb == 0 { i64::MAX / 4 } else { self.cache_limit_mb as i64 * 1024 * 1024 }
    }
}

/// Разделы окна (Нс-2, Нс-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Section {
    Profile,
    #[default]
    Appearance,
    Notifications,
    Data,
    Connection,
    Account,
}

impl Section {
    pub const ALL: [Section; 6] =
        [Section::Profile, Section::Appearance, Section::Notifications, Section::Data, Section::Connection, Section::Account];
}

/// Изменение одного значения из окна.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Theme(ThemeChoice),
    TextSize(u32),
    Lang(String),
    Gpu(bool),
    Notify(bool),
    HideText(bool),
    Sound(bool),
    Tray(bool),
    CloseToTray(bool),
    CacheLimit(u64),
}

/// Действия окна настроек.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsIntent {
    Open,
    Close,
    Section(Section),
    Change(Change),
    /// Файл темы — системным диалогом.
    PickThemeFile,
    /// Папка загрузок — системным диалогом выбора папки.
    PickDownloads,
    ClearCache,
    ProfileDraft { first_name: String, last_name: String, about: String, username: String },
    SaveProfile,
    PickPhoto,
    TerminateSession(i64),
    TerminateOthers,
    ProxyDraft(ProxySettings),
    TestProxy,
    SaveProxy,
    AskLogout,
    CancelLogout,
}

/// Состояние окна настроек.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SettingsView {
    pub section: Section,
    pub profile: Option<Profile>,
    pub draft: Profile,
    pub profile_saving: bool,
    pub profile_error: String,
    /// Своё фото (скачанное, как аватар).
    pub photo_path: String,
    pub sessions: Vec<Session>,
    pub sessions_loading: bool,
    pub proxy: ProxySettings,
    pub proxy_check: Option<Result<u32, String>>,
    pub proxy_checking: bool,
    /// Размеры кэша медиа и базы, байт (заполняет приложение).
    pub cache_bytes: i64,
    pub db_bytes: i64,
    pub confirm_logout: bool,
}

impl ChatModel {
    pub(crate) fn settings_intent(&mut self, i: SettingsIntent, out: &mut Vec<ChatEffect>) {
        if i == SettingsIntent::Open {
            self.settings = Some(SettingsView { proxy: self.proxy_settings.clone(), ..Default::default() });
            self.open_section(Section::Appearance, out);
            return;
        }
        let Some(v) = self.settings.as_mut() else { return };
        match i {
            SettingsIntent::Open => {}
            SettingsIntent::Close => self.settings = None,
            SettingsIntent::Section(s) => self.open_section(s, out),
            SettingsIntent::Change(c) => self.change(c, out),
            SettingsIntent::PickThemeFile => {
                let title = if self.lang == "ru" { "Файл темы" } else { "Theme file" }.to_string();
                self.send(Command::PickFiles { multiple: false, title }, Pending::PickTheme, out);
            }
            SettingsIntent::PickDownloads => {
                let title = if self.lang == "ru" { "Папка загрузок" } else { "Downloads folder" }.to_string();
                self.send(Command::PickFolder { title }, Pending::PickDownloads, out);
            }
            SettingsIntent::ClearCache => {
                self.media_paths.clear();
                out.push(ChatEffect::ClearCache);
                out.push(ChatEffect::MeasureStorage);
            }
            SettingsIntent::ProfileDraft { first_name, last_name, about, username } => {
                v.draft.first_name = first_name;
                v.draft.last_name = last_name;
                v.draft.about = about.chars().take(70).collect();
                v.draft.username = username;
                v.profile_error.clear();
            }
            SettingsIntent::SaveProfile => {
                if v.profile_saving {
                    return;
                }
                let Some(p) = v.profile.clone() else { return };
                let d = v.draft.clone();
                if d.first_name.trim().is_empty() {
                    v.profile_error = if self.lang == "ru" { "Имя не может быть пустым" } else { "First name is required" }.into();
                    return;
                }
                v.profile_saving = true;
                let username = (d.username.trim() != p.username).then(|| d.username.trim().trim_start_matches('@').to_string());
                self.send(
                    Command::UpdateProfile {
                        first_name: d.first_name.trim().into(),
                        last_name: d.last_name.trim().into(),
                        about: d.about.trim().into(),
                        username,
                    },
                    Pending::Profile,
                    out,
                );
            }
            SettingsIntent::PickPhoto => {
                let title = if self.lang == "ru" { "Фото профиля" } else { "Profile photo" }.to_string();
                self.send(Command::PickFiles { multiple: false, title }, Pending::PickPhoto, out);
            }
            SettingsIntent::TerminateSession(hash) => {
                v.sessions_loading = true;
                self.send(Command::TerminateSession { hash }, Pending::Sessions, out);
            }
            SettingsIntent::TerminateOthers => {
                v.sessions_loading = true;
                self.send(Command::TerminateSession { hash: 0 }, Pending::Sessions, out);
            }
            SettingsIntent::ProxyDraft(p) => {
                v.proxy = p;
                v.proxy_check = None;
            }
            SettingsIntent::TestProxy => {
                if !v.proxy_checking {
                    v.proxy_checking = true;
                    v.proxy_check = None;
                    let p = v.proxy.clone();
                    self.send(Command::TestProxy(p), Pending::ProxyCheck, out);
                }
            }
            SettingsIntent::SaveProxy => {
                let p = v.proxy.clone();
                self.proxy_settings = p.clone();
                self.send(Command::SetProxy(p.clone()), Pending::Other, out);
                out.push(ChatEffect::SaveProxy(p));
                let t = if self.lang == "ru" { "Соединение сохранено" } else { "Connection saved" };
                self.toast(t);
            }
            SettingsIntent::AskLogout => v.confirm_logout = true,
            SettingsIntent::CancelLogout => v.confirm_logout = false,
        }
    }

    fn open_section(&mut self, s: Section, out: &mut Vec<ChatEffect>) {
        let Some(v) = self.settings.as_mut() else { return };
        v.section = s;
        v.confirm_logout = false;
        match s {
            Section::Profile => {
                if v.profile.is_none() {
                    self.send(Command::GetProfile, Pending::Profile, out);
                }
            }
            Section::Account => {
                v.sessions_loading = true;
                self.send(Command::GetSessions, Pending::Sessions, out);
            }
            Section::Data => out.push(ChatEffect::MeasureStorage),
            _ => {}
        }
    }

    fn change(&mut self, c: Change, out: &mut Vec<ChatEffect>) {
        let s = &mut self.app_settings;
        match c {
            Change::Theme(t) => {
                if t == ThemeChoice::File && s.theme_file.is_empty() {
                    // Файла ещё нет — сначала выбрать.
                    let title = if self.lang == "ru" { "Файл темы" } else { "Theme file" }.to_string();
                    self.send(Command::PickFiles { multiple: false, title }, Pending::PickTheme, out);
                    return;
                }
                s.theme = t;
            }
            Change::TextSize(n) => s.text_size = n.clamp(TEXT_SIZE_MIN, TEXT_SIZE_MAX),
            Change::Lang(l) => {
                out.push(ChatEffect::SetLanguage(l));
                return;
            }
            Change::Gpu(on) => {
                s.gpu = on;
                let t = if self.lang == "ru" { "Применится после перезапуска" } else { "Takes effect after restart" };
                self.toast(t);
            }
            Change::Notify(on) => s.notify = on,
            Change::HideText(on) => s.notify_hide_text = on,
            Change::Sound(on) => s.notify_sound = on,
            Change::Tray(on) => s.tray = on,
            Change::CloseToTray(on) => s.close_to_tray = on,
            Change::CacheLimit(mb) => s.cache_limit_mb = mb,
        }
        out.push(ChatEffect::SettingsChanged(self.app_settings.clone()));
    }

    /// Ответы сети для окна настроек.
    pub(crate) fn settings_result(&mut self, p: Pending, r: &CommandResult, out: &mut Vec<ChatEffect>) {
        let ru = self.lang == "ru";
        match p {
            Pending::Profile => {
                if let Some(v) = self.settings.as_mut() {
                    v.profile_saving = false;
                    match (&r.error, &r.payload) {
                        (None, Payload::Profile(prof)) => {
                            v.profile = Some(prof.clone());
                            v.draft = prof.clone();
                            if prof.me.photo_id != 0 {
                                let (peer, photo_id, dc) = (prof.me.peer_ref(), prof.me.photo_id, prof.me.photo_dc);
                                self.send(Command::DownloadAvatar { peer, photo_id, dc }, Pending::ProfilePhoto, out);
                            }
                        }
                        (Some(e), _) => {
                            v.profile_error = match e.text.as_str() {
                                "USERNAME_OCCUPIED" => if ru { "Имя пользователя занято" } else { "Username is taken" }.into(),
                                "USERNAME_INVALID" => if ru { "Недопустимое имя пользователя" } else { "Invalid username" }.into(),
                                t => t.to_string(),
                            }
                        }
                        _ => {}
                    }
                }
            }
            Pending::Sessions => {
                if let Some(v) = self.settings.as_mut() {
                    v.sessions_loading = false;
                    match (&r.error, &r.payload) {
                        (None, Payload::Sessions(list)) => {
                            let mut list = list.clone();
                            list.sort_by_key(|x| (!x.current, -x.date_active));
                            v.sessions = list;
                        }
                        (Some(e), _) => {
                            let t = e.text.clone();
                            self.toast(&t);
                        }
                        _ => {}
                    }
                }
            }
            Pending::ProxyCheck => {
                if let Some(v) = self.settings.as_mut() {
                    v.proxy_checking = false;
                    v.proxy_check = Some(match (&r.error, &r.payload) {
                        (None, Payload::ProxyCheck { rtt_ms }) => Ok(*rtt_ms),
                        (Some(e), _) => Err(e.text.clone()),
                        _ => Err(String::new()),
                    });
                }
            }
            Pending::ProfilePhoto => {
                if let (Some(v), Payload::File(path)) = (self.settings.as_mut(), &r.payload) {
                    v.photo_path = path.clone();
                }
            }
            Pending::PickTheme => {
                if let (None, Payload::FilePaths(paths)) = (&r.error, &r.payload) {
                    if let Some(path) = paths.first() {
                        self.app_settings.theme_file = path.clone();
                        self.app_settings.theme = ThemeChoice::File;
                        out.push(ChatEffect::SettingsChanged(self.app_settings.clone()));
                    }
                }
            }
            Pending::PickDownloads => {
                if let (None, Payload::FilePaths(paths)) = (&r.error, &r.payload) {
                    if let Some(path) = paths.first() {
                        self.app_settings.downloads_dir = path.clone();
                        out.push(ChatEffect::SettingsChanged(self.app_settings.clone()));
                    }
                }
            }
            Pending::PickPhoto => {
                if let (None, Payload::FilePaths(paths)) = (&r.error, &r.payload) {
                    if let Some(path) = paths.first() {
                        if let Some(v) = self.settings.as_mut() {
                            v.profile_saving = true;
                        }
                        self.send(Command::SetProfilePhoto { path: path.clone() }, Pending::Profile, out);
                    }
                }
            }
            _ => {}
        }
    }

    /// Размеры хранилища для раздела «Данные и кэш» (приложение меряет).
    pub fn set_storage_sizes(&mut self, cache: i64, db: i64) {
        if let Some(v) = self.settings.as_mut() {
            v.cache_bytes = cache;
            v.db_bytes = db;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::ChatIntent;
    use crate::model::ErrorInfo;

    fn sends(fx: &[ChatEffect]) -> Vec<Command> {
        fx.iter().filter_map(|e| match e { ChatEffect::Send { cmd, .. } => Some(cmd.clone()), _ => None }).collect()
    }

    #[test]
    fn changes_are_saved_and_clamped() {
        let mut m = ChatModel::new(1, "ru", 0);
        m.intent(ChatIntent::Settings(SettingsIntent::Open));
        let fx = m.intent(ChatIntent::Settings(SettingsIntent::Change(Change::TextSize(99))));
        assert_eq!(m.app_settings.text_size, TEXT_SIZE_MAX);
        assert!(matches!(&fx[..], [ChatEffect::SettingsChanged(s)] if s.text_size == 20));
        let fx = m.intent(ChatIntent::Settings(SettingsIntent::Change(Change::Theme(ThemeChoice::File))));
        assert!(matches!(&sends(&fx)[..], [Command::PickFiles { .. }]), "темы из файла без файла — сначала выбрать");
        assert_eq!(m.app_settings.theme, ThemeChoice::System);
        let fx = m.intent(ChatIntent::Settings(SettingsIntent::Change(Change::Lang("en".into()))));
        assert_eq!(fx, vec![ChatEffect::SetLanguage("en".into())]);
    }

    #[test]
    fn profile_load_edit_save_and_errors() {
        let mut m = ChatModel::new(1, "ru", 0);
        m.intent(ChatIntent::Settings(SettingsIntent::Open));
        let fx = m.intent(ChatIntent::Settings(SettingsIntent::Section(Section::Profile)));
        assert_eq!(sends(&fx), vec![Command::GetProfile]);
        let id = m.peek_id() - 1;
        let prof = Profile { first_name: "Пётр".into(), username: "petr".into(), ..Default::default() };
        m.event(&crate::model::Event::Result(CommandResult { cmd_id: id, error: None, payload: Payload::Profile(prof) }));
        assert_eq!(m.settings.as_ref().unwrap().draft.first_name, "Пётр");
        m.intent(ChatIntent::Settings(SettingsIntent::ProfileDraft {
            first_name: "Пётр".into(),
            last_name: "И".into(),
            about: "о".repeat(100),
            username: "@petr2".into(),
        }));
        assert_eq!(m.settings.as_ref().unwrap().draft.about.chars().count(), 70);
        let fx = m.intent(ChatIntent::Settings(SettingsIntent::SaveProfile));
        assert!(matches!(&sends(&fx)[..], [Command::UpdateProfile { username: Some(u), .. }] if u == "petr2"));
        let id = m.peek_id() - 1;
        let err = ErrorInfo { kind: crate::model::ErrorKind::Rpc, code: 400, text: "USERNAME_OCCUPIED".into(), retry_after: 0 };
        m.event(&crate::model::Event::Result(CommandResult { cmd_id: id, error: Some(err), payload: Payload::None }));
        assert_eq!(m.settings.as_ref().unwrap().profile_error, "Имя пользователя занято");
    }
}
