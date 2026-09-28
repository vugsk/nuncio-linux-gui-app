//! Текст сообщений с эмодзи-картинками (решение О-14, вариант А; спайк
//! «Текст с эмодзи-картинками»): сообщение с эмодзи раскладывается в Rust на
//! строки из сегментов — текстовые фрагменты остаются `Text` Slint, эмодзи —
//! `Image` из набора Noto. Сообщения без эмодзи идут обычным `Text`.
//!
//! Ширины слов считаются по метрикам того же шрифта, которым рисует Slint
//! (fontconfig «sans-serif», у вас Noto Sans), без кернинга; для запаса строка
//! уже на `SLACK` пикселей (у спайка p95 ошибки 0,83 %).

use std::collections::HashMap;
use std::sync::OnceLock;

use skrifa::instance::{LocationRef, Size};
use skrifa::MetadataProvider;
use unicode_segmentation::UnicodeSegmentation;

mod pack {
    include!(concat!(env!("OUT_DIR"), "/emoji.rs"));
}

/// Высота строки и размер эмодзи в ней (спайк: 20 px и 18 px).
pub const LINE: f32 = 20.0;
pub const EMOJI: f32 = 18.0;
/// Запас ширины строки против ошибки оценки.
pub const SLACK: f32 = 4.0;
/// Крупные эмодзи в сообщении из одних эмодзи (Чт-12).
pub const BIG_EMOJI: f32 = 48.0;

fn key(s: &str) -> String {
    s.chars().filter(|&c| c != '\u{FE0F}').collect()
}

/// PNG эмодзи по последовательности (None — нет в наборе).
pub fn emoji_png(s: &str) -> Option<&'static [u8]> {
    let k = key(s);
    let i = pack::EMOJI.binary_search_by(|(e, _, _)| (*e).cmp(k.as_str())).ok()?;
    let (_, off, len) = pack::EMOJI[i];
    pack::EMOJI_PACK.get(off as usize..(off + len) as usize)
}

pub fn emoji_count() -> usize {
    pack::EMOJI.len()
}

/// Графема — эмодзи из набора. Одиночные цифры, `#` и `*` без U+20E3 — не эмодзи.
pub fn is_emoji(g: &str) -> bool {
    let mut cs = g.chars();
    match (cs.next(), cs.next()) {
        (Some(c), None) if c.is_ascii() => return false,
        _ => {}
    }
    emoji_png(g).is_some()
}

pub fn has_emoji(text: &str) -> bool {
    text.graphemes(true).any(is_emoji)
}

/// Сообщение из 1–3 эмодзи и пробелов — крупно без пузыря (Чт-12).
pub fn only_emoji(text: &str) -> Option<Vec<String>> {
    let mut v = Vec::new();
    for g in text.graphemes(true) {
        if g.trim().is_empty() {
            continue;
        }
        if !is_emoji(g) || v.len() == 3 {
            return None;
        }
        v.push(g.to_string());
    }
    (!v.is_empty()).then_some(v)
}

/// Метрики шрифта интерфейса.
pub struct Metrics {
    data: Vec<u8>,
    size: f32,
    cache: std::cell::RefCell<HashMap<char, f32>>,
}

fn font_file() -> Option<Vec<u8>> {
    let out = std::process::Command::new("fc-match").args(["-f", "%{file}", "sans-serif"]).output().ok()?;
    let path = String::from_utf8(out.stdout).ok()?;
    std::fs::read(path.trim()).ok()
}

impl Metrics {
    /// Шрифт по умолчанию системы (как у Slint). None — оценка без метрик.
    pub fn system(size: f32) -> Option<Self> {
        static FONT: OnceLock<Option<Vec<u8>>> = OnceLock::new();
        let data = FONT.get_or_init(font_file).clone()?;
        skrifa::FontRef::new(&data).ok()?;
        Some(Self { data, size, cache: Default::default() })
    }

    /// Ширина символа, px. Символов нет в шрифте (Slint возьмёт другой) — оценка:
    /// широкие (CJK) em, остальные 0,6 em.
    fn advance(&self, c: char) -> f32 {
        if let Some(w) = self.cache.borrow().get(&c) {
            return *w;
        }
        let font = skrifa::FontRef::new(&self.data).expect("проверено в system()");
        let w = match font.charmap().map(c) {
            Some(gid) if gid.to_u32() != 0 => font
                .glyph_metrics(Size::new(self.size), LocationRef::default())
                .advance_width(gid)
                .unwrap_or(self.size * 0.6),
            _ if is_wide(c) => self.size,
            _ => self.size * 0.6,
        };
        self.cache.borrow_mut().insert(c, w);
        w
    }

    pub fn width(&self, s: &str) -> f32 {
        s.chars().map(|c| self.advance(c)).sum()
    }
}

fn is_wide(c: char) -> bool {
    matches!(c as u32, 0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x20000..=0x3FFFD)
}

/// Сегмент строки.
#[derive(Debug, Clone, PartialEq)]
pub enum Seg {
    Text(String),
    Emoji(String),
}

pub type Line = Vec<Seg>;

fn push_text(line: &mut Line, s: &str) {
    if let Some(Seg::Text(t)) = line.last_mut() {
        t.push_str(s);
    } else {
        line.push(Seg::Text(s.to_string()));
    }
}

/// Жадный перенос по ширине `max`: слова целиком, слишком длинные — по графемам;
/// эмодзи — отдельные сегменты ширины `EMOJI` + 2 px; `\n` — новая строка.
pub fn layout(text: &str, max: f32, m: &Metrics) -> Vec<Line> {
    let max = (max - SLACK).max(EMOJI + 2.0);
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line: Line = Vec::new();
        let mut w = 0.0f32;
        // Токены: слово (с пробелами после него) или эмодзи.
        for word in para.split_word_bounds() {
            let graphemes: Vec<&str> = word.graphemes(true).collect();
            let has_e = graphemes.iter().any(|g| is_emoji(g));
            let parts: Vec<(&str, bool)> = if has_e {
                graphemes.iter().map(|g| (*g, is_emoji(g))).collect()
            } else {
                vec![(word, false)]
            };
            for (part, emoji) in parts {
                let pw = if emoji { EMOJI + 2.0 } else { m.width(part) };
                let blank = part.trim().is_empty();
                if w + pw > max && !line.is_empty() && !blank {
                    lines.push(std::mem::take(&mut line));
                    w = 0.0;
                }
                if blank && line.is_empty() {
                    continue; // пробел в начале перенесённой строки не рисуем
                }
                if emoji {
                    line.push(Seg::Emoji(part.to_string()));
                    w += pw;
                } else if pw <= max {
                    push_text(&mut line, part);
                    w += pw;
                } else {
                    // Слово длиннее строки: по графемам.
                    for g in part.graphemes(true) {
                        let gw = m.width(g);
                        if w + gw > max && !line.is_empty() {
                            lines.push(std::mem::take(&mut line));
                            w = 0.0;
                        }
                        push_text(&mut line, g);
                        w += gw;
                    }
                }
            }
        }
        // Хвостовые пробелы не нужны.
        if let Some(Seg::Text(t)) = line.last_mut() {
            let trimmed = t.trim_end().to_string();
            *t = trimmed;
        }
        lines.push(line);
    }
    lines
}

/// Ширина строки (для проверок).
pub fn line_width(line: &Line, m: &Metrics) -> f32 {
    line.iter()
        .map(|s| match s {
            Seg::Text(t) => m.width(t),
            Seg::Emoji(_) => EMOJI + 2.0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> Metrics {
        Metrics::system(13.0).expect("нужен системный шрифт (fc-match sans-serif)")
    }

    #[test]
    fn emoji_set_and_detection() {
        assert!(emoji_count() > 3000);
        for e in ["😀", "❤️", "❤", "👍🏽", "👨‍👩‍👧‍👦", "🇷🇺", "🏳️‍🌈", "1️⃣", "✈️"] {
            assert!(is_emoji(e), "{e} не найден в наборе");
        }
        for s in ["a", "1", "#", "*", "ж", " "] {
            assert!(!is_emoji(s), "{s:?} принят за эмодзи");
        }
        assert!(has_emoji("привет 👋"));
        assert!(!has_emoji("просто текст 123"));
        assert_eq!(only_emoji(" 🎉 🎉"), Some(vec!["🎉".into(), "🎉".into()]));
        assert_eq!(only_emoji("🎉🎉🎉🎉"), None, "больше трёх — обычным путём");
        assert_eq!(only_emoji("ок 🎉"), None);
    }

    #[test]
    fn wraps_within_width_and_keeps_emoji() {
        let m = metrics();
        let text = "Праздник 🎉 и снова 🎉🎉 а потом очень-очень-длинное-слово-которое-не-поместится-в-одну-строку-пузыря 👍🏽 конец";
        let lines = layout(text, 200.0, &m);
        assert!(lines.len() > 2);
        for l in &lines {
            assert!(line_width(l, &m) <= 200.0 - SLACK + 0.01, "строка шире предела: {l:?}");
        }
        let emojis: usize = lines.iter().flatten().filter(|s| matches!(s, Seg::Emoji(_))).count();
        assert_eq!(emojis, 4, "эмодзи не потерялись");
        // Текст не потерялся (без учёта пробелов на переносах).
        let joined: String = lines.iter().flatten().map(|s| match s {
            Seg::Text(t) => t.replace(' ', ""),
            Seg::Emoji(e) => e.clone(),
        }).collect();
        assert_eq!(joined, text.replace(' ', ""));
    }

    #[test]
    fn newlines_and_emoji_row_break() {
        let m = metrics();
        let lines = layout("строка 1 😀\n\nстрока 3", 400.0, &m);
        assert_eq!(lines.len(), 3);
        assert!(lines[1].is_empty(), "пустая строка сохраняется");
        // Ряд из 30 эмодзи переносится по эмодзи.
        let row = "🎉".repeat(30);
        let lines = layout(&row, 200.0, &m);
        assert!(lines.len() > 1 && lines.iter().all(|l| l.iter().all(|s| matches!(s, Seg::Emoji(_)))));
    }

    #[test]
    fn measures_text_like_the_font() {
        let m = metrics();
        let a = m.width("iiii");
        let b = m.width("WWWW");
        assert!(b > a * 2.0, "метрики не пропорциональные: {a} {b}");
        assert!(m.width("中文") >= 2.0 * 12.0, "широкие символы");
    }
}
