//! Темы (В9, Дз-1, Нс-7): тёмная, светлая, «как в системе» (портал
//! color-scheme) и из файла TOML поверх тёмной или светлой. Значения
//! записываются в глобальный `Theme` Slint; размер текста сообщений — туда же.
//!
//! Файл темы: ключи — имена токенов `Theme` (через дефис или подчёркивание),
//! значения — `#rrggbb` или `#rrggbbaa`; `base = "light"` — основа светлая.
//! Неизвестные ключи и неверные значения пропускаются (в журнал).
//!
//! ```toml
//! base = "dark"
//! window-bg = "#101820"
//! bubble-out = "#1f4f3a"
//! ```

use domain::settings::{AppSettings, ThemeChoice};
use slint::{Color, ComponentHandle};

use crate::{App, Theme};

/// Набор цветов в порядке установки в `Theme`.
pub type Palette = Vec<(&'static str, u32)>;

/// Тёмная (значения макетов).
pub fn dark() -> Palette {
    vec![
        ("window-bg", 0x0e1621ff), ("card-bg", 0x17212bff), ("card-border", 0x3d4f61ff), ("field-bg", 0x0e1621ff),
        ("field-border", 0x3d4f61ff), ("field-focus", 0x6ab3f3ff), ("row-bg", 0x182533ff), ("row-selected", 0x2b5278ff),
        ("accent", 0x2b5278ff), ("accent-border", 0x6ab3f3ff), ("accent-hover", 0x34628fff), ("text", 0xe6edf3ff),
        ("text-muted", 0x8a9aa8ff), ("link", 0x6ab3f3ff), ("error", 0xff6b6bff), ("success", 0x2f9e44ff),
        ("strip-bg", 0x182533ff), ("qr-bg", 0xffffffff), ("on-accent", 0xe6edf3ff), ("bubble-in", 0x182533ff),
        ("bubble-in-border", 0x3d4f61ff), ("bubble-out", 0x2b5278ff), ("bubble-out-border", 0x6ab3f3ff),
        ("bubble-error", 0x3a2530ff), ("reply-text", 0xc9d6e2ff), ("soft", 0xffffff10), ("soft-strong", 0xffffff20),
        ("mono-bg", 0xffffff12), ("thumb", 0x8a9bb080), ("thumb-hover", 0x8a9bb0c0), ("poll-chosen", 0x2f5a86ff),
        ("poll-correct", 0x3a6b4aff), ("on-link", 0x1e1e1eff),
    ]
}

/// Светлая (по мотивам Telegram Desktop: белые карточки, голубой акцент,
/// исходящие — светло-зелёные).
pub fn light() -> Palette {
    vec![
        ("window-bg", 0xe9eef2ff), ("card-bg", 0xffffffff), ("card-border", 0xd3dce4ff), ("field-bg", 0xffffffff),
        ("field-border", 0xc5d0daff), ("field-focus", 0x3390ecff), ("row-bg", 0xf4f6f8ff), ("row-selected", 0xd5e7faff),
        ("accent", 0x3390ecff), ("accent-border", 0x2a7fd4ff), ("accent-hover", 0x2a82e0ff), ("text", 0x111a22ff),
        ("text-muted", 0x6d7883ff), ("link", 0x2481ccff), ("error", 0xd93a3aff), ("success", 0x2f9e44ff),
        ("strip-bg", 0xffffffff), ("qr-bg", 0xffffffff), ("on-accent", 0xffffffff), ("bubble-in", 0xffffffff),
        ("bubble-in-border", 0xd3dce4ff), ("bubble-out", 0xe3f7d0ff), ("bubble-out-border", 0xb9dfa0ff),
        ("bubble-error", 0xfbe3e3ff), ("reply-text", 0x3d4a56ff), ("soft", 0x0000000c), ("soft-strong", 0x00000018),
        ("mono-bg", 0x0000000e), ("thumb", 0x6d788380), ("thumb-hover", 0x6d7883c0), ("poll-chosen", 0xbcd9f5ff),
        ("poll-correct", 0xc7e8c0ff), ("on-link", 0xffffffff),
    ]
}

fn parse_color(s: &str) -> Option<u32> {
    let h = s.trim().strip_prefix('#')?;
    let v = u32::from_str_radix(h, 16).ok()?;
    match h.len() {
        6 => Some((v << 8) | 0xff),
        8 => Some(v),
        _ => None,
    }
}

/// Тема из файла TOML: основа и переопределённые цвета; ошибки — списком.
pub fn from_toml(text: &str) -> (Palette, Vec<String>) {
    let mut errors = Vec::new();
    let table: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => return (dark(), vec![format!("файл темы не разобран: {e}")]),
    };
    let mut pal = match table.get("base").and_then(|v| v.as_str()) {
        Some("light") => light(),
        _ => dark(),
    };
    for (k, v) in &table {
        if k == "base" {
            continue;
        }
        let name = k.replace('_', "-");
        let Some(slot) = pal.iter_mut().find(|(n, _)| *n == name) else {
            errors.push(format!("неизвестный ключ «{k}»"));
            continue;
        };
        match v.as_str().and_then(parse_color) {
            Some(c) => slot.1 = c,
            None => errors.push(format!("«{k}»: нужен цвет #rrggbb или #rrggbbaa")),
        }
    }
    (pal, errors)
}

/// Палитра по настройкам и теме рабочего стола (0 — не задано, 1 — тёмная, 2 — светлая).
pub fn palette(s: &AppSettings, system: i32) -> Palette {
    match s.theme {
        ThemeChoice::Dark => dark(),
        ThemeChoice::Light => light(),
        ThemeChoice::System => if system == 2 { light() } else { dark() },
        ThemeChoice::File => match std::fs::read_to_string(&s.theme_file) {
            Ok(text) => {
                let (p, errors) = from_toml(&text);
                for e in errors {
                    log::warn!("тема: {e}");
                }
                p
            }
            Err(e) => {
                log::warn!("тема: файл не прочитан: {e}");
                dark()
            }
        },
    }
}

fn color(v: u32) -> Color {
    Color::from_argb_u8((v & 0xff) as u8, (v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8)
}

/// Записать палитру и размер текста в `Theme`.
pub fn apply(app: &App, pal: &Palette, text_size: u32) {
    let t = app.global::<Theme>();
    for &(name, v) in pal {
        let c = color(v);
        match name {
            "window-bg" => t.set_window_bg(c),
            "card-bg" => t.set_card_bg(c),
            "card-border" => t.set_card_border(c),
            "field-bg" => t.set_field_bg(c),
            "field-border" => t.set_field_border(c),
            "field-focus" => t.set_field_focus(c),
            "row-bg" => t.set_row_bg(c),
            "row-selected" => t.set_row_selected(c),
            "accent" => t.set_accent(c),
            "accent-border" => t.set_accent_border(c),
            "accent-hover" => t.set_accent_hover(c),
            "text" => t.set_text(c),
            "text-muted" => t.set_text_muted(c),
            "link" => t.set_link(c),
            "error" => t.set_error(c),
            "success" => t.set_success(c),
            "strip-bg" => t.set_strip_bg(c),
            "qr-bg" => t.set_qr_bg(c),
            "on-accent" => t.set_on_accent(c),
            "bubble-in" => t.set_bubble_in(c),
            "bubble-in-border" => t.set_bubble_in_border(c),
            "bubble-out" => t.set_bubble_out(c),
            "bubble-out-border" => t.set_bubble_out_border(c),
            "bubble-error" => t.set_bubble_error(c),
            "reply-text" => t.set_reply_text(c),
            "soft" => t.set_soft(c),
            "soft-strong" => t.set_soft_strong(c),
            "mono-bg" => t.set_mono_bg(c),
            "thumb" => t.set_thumb(c),
            "thumb-hover" => t.set_thumb_hover(c),
            "poll-chosen" => t.set_poll_chosen(c),
            "poll-correct" => t.set_poll_correct(c),
            "on-link" => t.set_on_link(c),
            _ => {}
        }
    }
    let size = text_size as f32;
    t.set_msg_size(size);
    t.set_msg_line((size * 20.0 / 13.0).round());
    t.set_msg_emoji((size * 18.0 / 13.0).round());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_theme_over_base() {
        let (p, errors) = from_toml("base = \"light\"\nwindow_bg = \"#101820\"\nsoft = \"#00000022\"\nnope = \"#fff\"\ntext = \"red\"\n");
        let get = |n: &str| p.iter().find(|(k, _)| *k == n).unwrap().1;
        assert_eq!(get("window-bg"), 0x101820ff);
        assert_eq!(get("soft"), 0x00000022);
        assert_eq!(get("card-bg"), 0xffffffff, "остальное — от светлой основы");
        assert_eq!(errors.len(), 2, "неизвестный ключ и неверный цвет: {errors:?}");
        assert_eq!(dark().len(), light().len());
        assert!(dark().iter().zip(light()).all(|(a, b)| a.0 == b.0), "один набор токенов");
    }
}
