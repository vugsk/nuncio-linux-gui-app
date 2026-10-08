//! Окно настроек (В9): модель домена → глобальный `Settings` Slint и
//! действия окна → `ChatIntent::Settings`. Поля, которые человек правит
//! (профиль, прокси), переписываются только при загрузке, а не на каждом снимке.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use domain::chat::{ChatIntent, ChatModel};
use domain::model::{ProxyKind, ProxySettings};
use domain::settings::{Change, Section, SettingsIntent, ThemeChoice, CACHE_LIMITS};
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

use crate::{avatar, App, SessionItem, Settings};

#[derive(Default)]
struct State {
    open: bool,
    profile: Option<domain::model::Profile>,
    sender: Option<Rc<dyn Fn(ChatIntent)>>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn send(i: SettingsIntent) {
    let s = STATE.with(|s| s.borrow().sender.clone());
    if let Some(s) = s {
        s(ChatIntent::Settings(i));
    }
}

fn section_of(i: i32) -> Section {
    Section::ALL.get(usize::try_from(i).unwrap_or(0)).copied().unwrap_or_default()
}

fn mb(bytes: i64, ru: bool) -> String {
    domain::media::size_label(bytes.max(0), ru)
}

/// Действия окна настроек.
pub fn bind(app: &App, sender: impl Fn(ChatIntent) + 'static) {
    STATE.with(|s| s.borrow_mut().sender = Some(Rc::new(sender)));
    let g = app.global::<Settings>();
    g.on_close(|| send(SettingsIntent::Close));
    g.on_select(|i| send(SettingsIntent::Section(section_of(i))));
    let a = app.as_weak();
    g.on_profile_edited(move || {
        let Some(app) = a.upgrade() else { return };
        let g = app.global::<Settings>();
        send(SettingsIntent::ProfileDraft {
            first_name: g.get_first_name().into(),
            last_name: g.get_last_name().into(),
            about: g.get_about().into(),
            username: g.get_username().into(),
        });
    });
    g.on_save_profile(|| send(SettingsIntent::SaveProfile));
    g.on_pick_photo(|| send(SettingsIntent::PickPhoto));
    g.on_set_theme(|i| {
        let t = match i {
            1 => ThemeChoice::Dark,
            2 => ThemeChoice::Light,
            3 => ThemeChoice::File,
            _ => ThemeChoice::System,
        };
        send(SettingsIntent::Change(Change::Theme(t)));
    });
    g.on_pick_theme(|| send(SettingsIntent::PickThemeFile));
    g.on_set_text_size(|n| send(SettingsIntent::Change(Change::TextSize(u32::try_from(n).unwrap_or(13)))));
    g.on_set_lang(|i| send(SettingsIntent::Change(Change::Lang(if i == 1 { "en" } else { "ru" }.into()))));
    g.on_set_gpu(|on| send(SettingsIntent::Change(Change::Gpu(on))));
    g.on_set_notify(|on| send(SettingsIntent::Change(Change::Notify(on))));
    g.on_set_hide_text(|on| send(SettingsIntent::Change(Change::HideText(on))));
    g.on_set_sound(|on| send(SettingsIntent::Change(Change::Sound(on))));
    g.on_set_volume(|v| send(SettingsIntent::Change(Change::Volume(u32::try_from(v).unwrap_or(100)))));
    let a = app.as_weak();
    g.on_preview_sound(move || {
        if let Some(app) = a.upgrade() {
            crate::play_notification_sound(app.global::<Settings>().get_volume() as f32 / 100.0);
        }
    });
    g.on_set_tray(|on| send(SettingsIntent::Change(Change::Tray(on))));
    g.on_set_close_tray(|on| send(SettingsIntent::Change(Change::CloseToTray(on))));
    g.on_set_cache_limit(|i| {
        let mb = CACHE_LIMITS.get(usize::try_from(i).unwrap_or(1)).copied().unwrap_or(1024);
        send(SettingsIntent::Change(Change::CacheLimit(mb)));
    });
    g.on_clear_cache(|| send(SettingsIntent::ClearCache));
    g.on_pick_downloads(|| send(SettingsIntent::PickDownloads));
    let a = app.as_weak();
    g.on_proxy_edited(move || {
        let Some(app) = a.upgrade() else { return };
        let g = app.global::<Settings>();
        let kind = match g.get_proxy_kind() {
            1 => ProxyKind::Socks5,
            2 => ProxyKind::MtProxy,
            _ => ProxyKind::None,
        };
        send(SettingsIntent::ProxyDraft(ProxySettings {
            kind,
            addr: g.get_proxy_addr().trim().into(),
            user: g.get_proxy_user().into(),
            password: g.get_proxy_password().into(),
            secret: g.get_proxy_secret().trim().into(),
        }));
    });
    g.on_test_proxy(|| send(SettingsIntent::TestProxy));
    g.on_save_proxy(|| send(SettingsIntent::SaveProxy));
    g.on_terminate(|h| {
        if let Ok(h) = h.parse() {
            send(SettingsIntent::TerminateSession(h));
        }
    });
    g.on_terminate_others(|| send(SettingsIntent::TerminateOthers));
    g.on_ask_logout(|| send(SettingsIntent::AskLogout));
    g.on_cancel_logout(|| send(SettingsIntent::CancelLogout));
}

/// Модель → окно настроек.
pub fn render(app: &App, m: &ChatModel, images: &HashMap<String, SharedPixelBuffer<Rgba8Pixel>>) {
    let g = app.global::<Settings>();
    let ru = m.lang == "ru";
    let Some(v) = &m.settings else {
        g.set_open(false);
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.open = false;
            s.profile = None;
        });
        return;
    };
    let just_opened = STATE.with(|s| !std::mem::replace(&mut s.borrow_mut().open, true));
    g.set_open(true);
    g.set_section(Section::ALL.iter().position(|s| *s == v.section).unwrap_or(1) as i32);
    // Профиль: поля — при загрузке.
    g.set_profile_loading(v.profile.is_none());
    g.set_profile_saving(v.profile_saving);
    g.set_profile_error(v.profile_error.clone().into());
    let changed = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let c = s.profile != v.profile;
        s.profile = v.profile.clone();
        c
    });
    if changed {
        if let Some(p) = &v.profile {
            g.set_first_name(p.first_name.clone().into());
            g.set_last_name(p.last_name.clone().into());
            g.set_about(p.about.clone().into());
            g.set_username(p.username.clone().into());
            g.set_phone(if p.phone.is_empty() { String::new() } else { format!("+{}", p.phone) }.into());
            let name = format!("{} {}", p.first_name, p.last_name);
            g.set_initials(avatar::initials(name.trim()).into());
            let (r, gg, b) = avatar::color_for(p.me.id);
            g.set_tint(slint::Color::from_rgb_u8(r, gg, b));
        }
    }
    let photo = images.get(&v.photo_path).map(|px| Image::from_rgba8(px.clone()));
    g.set_has_photo(photo.is_some());
    if let Some(p) = photo {
        g.set_photo(p);
    }
    // Внешний вид.
    let s = &m.app_settings;
    g.set_theme(match s.theme {
        ThemeChoice::System => 0,
        ThemeChoice::Dark => 1,
        ThemeChoice::Light => 2,
        ThemeChoice::File => 3,
    });
    g.set_theme_file(s.theme_file.clone().into());
    g.set_text_size(s.text_size as i32);
    g.set_lang(i32::from(m.lang != "ru"));
    g.set_gpu(s.gpu);
    // Уведомления.
    g.set_notify(s.notify);
    g.set_hide_text(s.notify_hide_text);
    g.set_sound(s.notify_sound);
    g.set_volume(s.notify_volume as i32);
    g.set_tray(s.tray);
    // Пока трей выключен, о хосте ничего не известно — без подсказки.
    g.set_tray_available(!s.tray || m.tray_available);
    g.set_close_tray(s.close_to_tray);
    // Данные и кэш.
    g.set_cache_size(mb(v.cache_bytes, ru).into());
    g.set_db_size(mb(v.db_bytes, ru).into());
    let limits: Vec<slint::SharedString> = CACHE_LIMITS
        .iter()
        .map(|&l| match l {
            0 => if ru { "Без лимита" } else { "No limit" }.into(),
            l if l >= 1024 => format!("{} {}", l / 1024, if ru { "ГБ" } else { "GB" }).into(),
            l => format!("{l} {}", if ru { "МБ" } else { "MB" }).into(),
        })
        .collect();
    g.set_cache_limits(ModelRc::from(Rc::new(VecModel::from(limits))));
    g.set_cache_limit(CACHE_LIMITS.iter().position(|&l| l == s.cache_limit_mb).unwrap_or(1) as i32);
    g.set_downloads(
        if s.downloads_dir.is_empty() {
            if ru { "Системная папка «Загрузки»" } else { "System Downloads folder" }.to_string()
        } else {
            s.downloads_dir.clone()
        }
        .into(),
    );
    // Соединение: поля — при открытии окна.
    if just_opened {
        let p = &v.proxy;
        g.set_proxy_kind(match p.kind {
            ProxyKind::None => 0,
            ProxyKind::Socks5 => 1,
            ProxyKind::MtProxy => 2,
        });
        g.set_proxy_addr(p.addr.clone().into());
        g.set_proxy_user(p.user.clone().into());
        g.set_proxy_password(p.password.clone().into());
        g.set_proxy_secret(p.secret.clone().into());
    }
    g.set_proxy_checking(v.proxy_checking);
    g.set_proxy_check(
        match &v.proxy_check {
            None => String::new(),
            Some(Ok(ms)) => if ru { format!("Работает, задержка {ms} мс") } else { format!("Works, {ms} ms") },
            Some(Err(e)) if e.is_empty() => if ru { "Не работает".into() } else { "Does not work".into() },
            Some(Err(e)) => if ru { format!("Не работает: {e}") } else { format!("Does not work: {e}") },
        }
        .into(),
    );
    // Аккаунт.
    g.set_sessions_loading(v.sessions_loading);
    let sessions: Vec<SessionItem> = v
        .sessions
        .iter()
        .map(|x| SessionItem {
            hash: x.hash.to_string().into(),
            title: format!("{} · {}", x.device, x.app.trim()).into(),
            subtitle: format!(
                "{} · {} {} · {}",
                x.platform.trim(),
                x.ip,
                x.country,
                domain::timefmt::dialog_time(x.date_active, m.now, &m.lang)
            )
            .into(),
            current: x.current,
        })
        .collect();
    g.set_sessions(ModelRc::from(Rc::new(VecModel::from(sessions))));
    g.set_confirm_logout(v.confirm_logout);
}
