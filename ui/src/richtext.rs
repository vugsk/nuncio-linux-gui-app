//! Текст с эмодзи-картинками и форматированием (решения О-14, Дз-13, Дз-17):
//! сообщение раскладывается в Rust на строки из сегментов — текстовые
//! фрагменты остаются `Text` Slint (со своим начертанием), эмодзи — `Image`
//! из набора Noto. Та же раскладка рисует поле ввода: у каждой графемы есть
//! координата, по ней ставятся каретка и выделение.
//!
//! Ширины считаются шейпингом (harfrust, с кернингом) по тем же файлам
//! шрифтов, что выбирает fontconfig для Slint; по спайку Дз-17 так положения
//! совпадают со Slint до 0,01 px. Файлы отображаются в память: страницы общие
//! с копией, которую держит сам Slint. Разрывы строк — UAX#14, как у Slint.

use std::cell::RefCell;
use std::collections::HashMap;

use unicode_segmentation::UnicodeSegmentation;

mod pack {
    include!(concat!(env!("OUT_DIR"), "/emoji.rs"));
}

/// Высота строки и размер эмодзи в ней (спайк: 20 px и 18 px в ячейке 20).
pub const LINE: f32 = 20.0;
pub const EMOJI: f32 = 18.0;
pub const EMOJI_CELL: f32 = EMOJI + 2.0;
/// Запас ширины строки против ошибки оценки.
pub const SLACK: f32 = 4.0;
/// Крупные эмодзи в сообщении из одних эмодзи (Чт-12).
pub const BIG_EMOJI: f32 = 48.0;
/// Кегль текста сообщений и поля ввода.
pub const SIZE: f32 = 13.0;

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

// ---------------------------------------------------------------- стили

/// Начертание и оформление фрагмента (Дз-15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub mono: bool,
    /// Индекс ссылки в таблице ссылок текста (−1 — не ссылка).
    pub link: i32,
}

impl Style {
    pub const PLAIN: Style =
        Style { bold: false, italic: false, underline: false, strike: false, mono: false, link: -1 };

    fn face(&self) -> Face {
        match (self.mono, self.bold, self.italic) {
            (true, true, _) => Face::MonoBold,
            (true, false, _) => Face::Mono,
            (false, true, true) => Face::BoldItalic,
            (false, true, false) => Face::Bold,
            (false, false, true) => Face::Italic,
            (false, false, false) => Face::Regular,
        }
    }
}

/// Стиль на участке текста `[start, end)` в байтах.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub style: Style,
}

/// Стиль в каждой точке: без диапазона — обычный; пересекающиеся
/// диапазоны складываются (жирный курсив).
fn style_at(spans: &[Span], pos: usize) -> Style {
    let mut s = Style::PLAIN;
    for sp in spans.iter().filter(|sp| sp.start <= pos && pos < sp.end) {
        s.bold |= sp.style.bold;
        s.italic |= sp.style.italic;
        s.underline |= sp.style.underline;
        s.strike |= sp.style.strike;
        s.mono |= sp.style.mono;
        if sp.style.link >= 0 {
            s.link = sp.style.link;
        }
    }
    s
}

// ---------------------------------------------------------------- шрифты

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Face {
    Regular,
    Bold,
    Italic,
    BoldItalic,
    Mono,
    MonoBold,
}

impl Face {
    fn pattern(self) -> &'static str {
        match self {
            Face::Regular => "sans-serif",
            Face::Bold => "sans-serif:bold",
            Face::Italic => "sans-serif:italic",
            Face::BoldItalic => "sans-serif:bold:italic",
            Face::Mono => "monospace",
            Face::MonoBold => "monospace:bold",
        }
    }
}

struct Shaped {
    shaper: harfrust::Shaper<'static>,
    scale: f32,
}

fn fc_file(pattern: &str) -> Option<String> {
    let out = std::process::Command::new("fc-match").args(["-f", "%{file}", pattern]).output().ok()?;
    let path = String::from_utf8(out.stdout).ok()?;
    let path = path.trim();
    (!path.is_empty()).then(|| path.to_string())
}

/// Семейство моноширинного шрифта (для `font-family` в разметке).
pub fn mono_family() -> String {
    std::process::Command::new("fc-match")
        .args(["-f", "%{family[0]}", "monospace"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "monospace".into())
}

fn load(face: Face) -> Option<Shaped> {
    let path = fc_file(face.pattern())?;
    let file = std::fs::File::open(path).ok()?;
    // SAFETY: файл шрифта только читается; его не меняют, пока приложение
    // работает (обновление пакета заменяет файл новым, старое отображение живо).
    let map = unsafe { memmap2::Mmap::map(&file) }.ok()?;
    // Шрифты нужны всё время работы: отображение и данные шейпера живут до конца процесса.
    let bytes: &'static [u8] = Box::leak(Box::new(map));
    let font = harfrust::FontRef::new(bytes).ok()?;
    let data: &'static harfrust::ShaperData = Box::leak(Box::new(harfrust::ShaperData::new(&font)));
    let shaper = data.shaper(&font).build();
    let scale = SIZE / shaper.units_per_em() as f32;
    Some(Shaped { shaper, scale })
}

thread_local! {
    /// Начертания загружаются один раз на поток и живут до конца процесса.
    static FACES: RefCell<HashMap<Face, Option<&'static Shaped>>> = RefCell::new(HashMap::new());
}

/// Ширины и позиции графем по шрифтам интерфейса (поток интерфейса).
#[derive(Default)]
pub struct Fonts {
    cache: RefCell<HashMap<(Face, String), Stops>>,
}

/// Позиции границ графем внутри фрагмента: `(смещение в байтах, x)`,
/// первая — `(0, 0)`, последняя — `(len, ширина)`.
pub type Stops = Vec<(usize, f32)>;

impl Fonts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Есть ли основной шрифт (без него — только оценка).
    pub fn available(&self) -> bool {
        self.with_face(Face::Regular, |_| ()).is_some()
    }

    fn with_face<R>(&self, face: Face, f: impl FnOnce(&Shaped) -> R) -> Option<R> {
        let shaped = FACES.with(|faces| {
            *faces.borrow_mut().entry(face).or_insert_with(|| {
                load(face).or_else(|| load(Face::Regular)).map(|s| &*Box::leak(Box::new(s)))
            })
        });
        shaped.map(f)
    }

    /// Границы графем фрагмента одного начертания с их x.
    pub fn stops(&self, style: Style, text: &str) -> Stops {
        let face = style.face();
        if let Some(s) = self.cache.borrow().get(&(face, text.to_string())) {
            return s.clone();
        }
        let per_byte = self
            .with_face(face, |sh| {
                let mut buf = harfrust::UnicodeBuffer::new();
                buf.push_str(text);
                buf.guess_segment_properties();
                let out = sh.shaper.shape(buf, harfrust::ShapeOptions::new());
                // Продвижение по кластерам (байтовые смещения начала кластера).
                let mut adv: Vec<(usize, f32, bool)> = out
                    .glyph_infos()
                    .iter()
                    .zip(out.glyph_positions())
                    .map(|(i, p)| (i.cluster as usize, p.x_advance as f32 * sh.scale, i.glyph_id == 0))
                    .collect();
                adv.sort_by_key(|a| a.0);
                adv
            })
            .unwrap_or_default();
        let mut stops = vec![(0usize, 0.0f32)];
        let mut x = 0.0;
        for (off, g) in text.grapheme_indices(true) {
            let end = off + g.len();
            let mut w: f32 = per_byte.iter().filter(|a| a.0 >= off && a.0 < end).map(|a| a.1).sum();
            let missing = per_byte.is_empty() || per_byte.iter().any(|a| a.0 >= off && a.0 < end && a.2);
            if missing {
                // Нет глифа — Slint возьмёт запасной шрифт, и какой — заранее не
                // узнать (псевдографику «─│» он берёт из CJK — шириной в кегль).
                // Оценка с запасом: строка перенесётся чуть раньше, но за пузырь
                // не вылезет.
                w = SIZE;
            }
            x += w;
            stops.push((end, x));
        }
        self.cache.borrow_mut().insert((face, text.to_string()), stops.clone());
        stops
    }

    pub fn width(&self, style: Style, text: &str) -> f32 {
        self.stops(style, text).last().map(|s| s.1).unwrap_or(0.0)
    }

    /// Кэш растёт с каждым новым словом; предел на всякий случай.
    pub fn trim_cache(&self) {
        let mut c = self.cache.borrow_mut();
        if c.len() > 20_000 {
            c.clear();
        }
    }
}

// ---------------------------------------------------------------- раскладка

/// Сегмент строки: текст одного стиля или эмодзи.
#[derive(Debug, Clone, PartialEq)]
pub struct Seg {
    pub text: String,
    pub emoji: bool,
    pub style: Style,
    /// Байтовые смещения сегмента в исходном тексте.
    pub start: usize,
    pub end: usize,
    /// Положение и ширина в строке.
    pub x: f32,
    pub w: f32,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Line {
    pub segs: Vec<Seg>,
    /// Байтовые смещения строки в исходном тексте (без перевода строки).
    pub start: usize,
    pub end: usize,
}

/// Кусок между возможностями разрыва строки, разбитый по стилю и эмодзи.
struct Atom {
    start: usize,
    end: usize,
    style: Style,
    emoji: bool,
    w: f32,
    /// Перед ним можно перенести строку.
    break_before: bool,
    /// Пробелы в конце (не считаются в ширину при переносе).
    space: bool,
}

fn atoms(text: &str, para: std::ops::Range<usize>, spans: &[Span], fonts: &Fonts) -> Vec<Atom> {
    let s = &text[para.clone()];
    let mut breaks: Vec<usize> = unicode_linebreak::linebreaks(s).map(|(i, _)| i).collect();
    breaks.retain(|&b| b > 0 && b < s.len());
    let mut out = Vec::new();
    let mut word_start = 0;
    let mut bounds = breaks.clone();
    bounds.push(s.len());
    for &word_end in &bounds {
        let word = &s[word_start..word_end];
        let mut first = true;
        let mut cur: Option<(usize, Style)> = None; // начало текущего куска текста
        let flush = |out: &mut Vec<Atom>, from: usize, to: usize, style: Style, first: &mut bool| {
            if from >= to {
                return;
            }
            let t = &s[from..to];
            let space = t.trim_end().is_empty();
            out.push(Atom {
                start: para.start + from,
                end: para.start + to,
                style,
                emoji: false,
                w: fonts.width(style, t),
                break_before: std::mem::take(first) && from > 0,
                space,
            });
        };
        for (off, g) in word.grapheme_indices(true) {
            let at = word_start + off;
            let st = style_at(spans, para.start + at);
            if is_emoji(g) {
                if let Some((from, style)) = cur.take() {
                    flush(&mut out, from, at, style, &mut first);
                }
                out.push(Atom {
                    start: para.start + at,
                    end: para.start + at + g.len(),
                    style: st,
                    emoji: true,
                    w: EMOJI_CELL,
                    break_before: std::mem::take(&mut first) && at > 0,
                    space: false,
                });
                continue;
            }
            // Хвостовые пробелы слова — отдельным куском (не мешают переносу).
            let is_space = g.chars().all(char::is_whitespace);
            // Пробел после текста начинает хвост слова.
            let trailing = |from: usize| is_space && !s[from..at].trim().is_empty();
            match cur {
                Some((from, style)) if style == st && !trailing(from) => {}
                Some((from, style)) => {
                    flush(&mut out, from, at, style, &mut first);
                    cur = Some((at, st));
                }
                None => cur = Some((at, st)),
            }
        }
        if let Some((from, style)) = cur.take() {
            flush(&mut out, from, word_end, style, &mut first);
        }
        word_start = word_end;
    }
    out
}

/// Раскладка текста со стилями в строки шириной до `max` (без полей).
pub fn layout(text: &str, spans: &[Span], max: f32, fonts: &Fonts) -> Vec<Line> {
    let max = (max - SLACK).max(EMOJI_CELL * 2.0);
    let mut lines = Vec::new();
    let mut para_start = 0;
    for para in text.split('\n') {
        let range = para_start..para_start + para.len();
        para_start = range.end + 1;
        let mut line: Vec<Atom> = Vec::new();
        let mut w_all = 0.0f32; // ширина строки с пробелами

        let mut done: Vec<Vec<Atom>> = Vec::new();
        for a in atoms(text, range.clone(), spans, fonts) {
            if a.space {
                w_all += a.w;
                line.push(a);
                continue;
            }
            // Слово целиком: от этого куска до следующей возможности разрыва.
            if a.break_before && !line.is_empty() && w_all + a.w > max {
                done.push(std::mem::take(&mut line));
                w_all = 0.0;
            }
            if a.w > max && !a.emoji {
                // Кусок длиннее строки: по графемам.
                let t = &text[a.start..a.end];
                let stops = fonts.stops(a.style, t);
                let mut from = 0usize;
                let mut from_x = 0.0f32;
                for win in stops.windows(2) {
                    let ((b0, x0), (_, x1)) = (win[0], win[1]);
                    if w_all + (x1 - from_x) > max && b0 > from {
                        line.push(Atom { start: a.start + from, end: a.start + b0, w: x0 - from_x, ..a });
                        done.push(std::mem::take(&mut line));
                        w_all = 0.0;
                        from = b0;
                        from_x = x0;
                    }
                }
                let rest_w = stops.last().map(|s| s.1).unwrap_or(0.0) - from_x;
                line.push(Atom { start: a.start + from, w: rest_w, ..a });
                w_all += rest_w;
                continue;
            }
            w_all += a.w;
            line.push(a);
        }
        done.push(line);
        for atoms in done {
            lines.push(assemble(text, atoms, fonts, range.clone()));
        }
    }
    lines
}

/// Куски строки — в сегменты: соседние одного стиля сливаются, x по шейпингу
/// целого сегмента (как нарисует Slint).
fn assemble(text: &str, atoms: Vec<Atom>, fonts: &Fonts, para: std::ops::Range<usize>) -> Line {
    let mut segs: Vec<Seg> = Vec::new();
    for a in atoms {
        match segs.last_mut() {
            Some(s) if !s.emoji && !a.emoji && s.style == a.style && s.end == a.start => {
                s.end = a.end;
            }
            _ => segs.push(Seg {
                text: String::new(),
                emoji: a.emoji,
                style: a.style,
                start: a.start,
                end: a.end,
                x: 0.0,
                w: 0.0,
            }),
        }
    }
    let mut x = 0.0;
    for s in &mut segs {
        s.text = text[s.start..s.end].to_string();
        s.x = x;
        s.w = if s.emoji { EMOJI_CELL } else { fonts.width(s.style, &s.text) };
        x += s.w;
    }
    let start = segs.first().map(|s| s.start).unwrap_or(para.start);
    let end = segs.last().map(|s| s.end).unwrap_or(para.start);
    // Пустая строка абзаца: начало и конец — в самом абзаце.
    let (start, end) = if segs.is_empty() { (para.start, para.start) } else { (start, end) };
    Line { segs, start, end }
}

/// x каретки перед байтом `pos` строки (pos в пределах строки).
pub fn caret_x(line: &Line, pos: usize, fonts: &Fonts) -> f32 {
    for s in &line.segs {
        if pos <= s.start {
            return s.x;
        }
        if pos < s.end || (pos == s.end && std::ptr::eq(s, line.segs.last().unwrap())) {
            if s.emoji {
                return if pos <= s.start { s.x } else { s.x + s.w };
            }
            let stops = fonts.stops(s.style, &s.text);
            let rel = pos - s.start;
            let x = stops.iter().rev().find(|(b, _)| *b <= rel).map(|(_, x)| *x).unwrap_or(0.0);
            return s.x + x;
        }
    }
    line.segs.last().map(|s| s.x + s.w).unwrap_or(0.0)
}

/// Байт по x в строке (ближайшая граница графемы).
pub fn hit(line: &Line, x: f32, fonts: &Fonts) -> usize {
    let mut best = (line.start, f32::MAX);
    let mut consider = |pos: usize, px: f32| {
        let d = (px - x).abs();
        if d < best.1 {
            best = (pos, d);
        }
    };
    consider(line.start, 0.0);
    for s in &line.segs {
        if s.emoji {
            consider(s.start, s.x);
            consider(s.end, s.x + s.w);
            continue;
        }
        for (b, px) in fonts.stops(s.style, &s.text) {
            consider(s.start + b, s.x + px);
        }
    }
    // Хвостовые пробелы в конце перенесённой строки: каретка — до них.
    best.0
}

/// Ширина строки (для проверок).
pub fn line_width(line: &Line) -> f32 {
    line.segs.iter().filter(|s| s.emoji || !s.text.trim().is_empty()).map(|s| s.x + s.w).fold(0.0, f32::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fonts() -> Fonts {
        let f = Fonts::new();
        assert!(f.available(), "нужен системный шрифт (fc-match sans-serif)");
        f
    }

    fn plain(text: &str, max: f32) -> Vec<Line> {
        layout(text, &[], max, &fonts())
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
    fn wraps_within_width_and_keeps_text() {
        let f = fonts();
        let text = "Праздник 🎉 и снова 🎉🎉 а потом очень-очень-длинное-слово-которое-не-поместится-в-одну-строку-пузыря 👍🏽 конец";
        let lines = layout(text, &[], 200.0, &f);
        assert!(lines.len() > 2);
        for l in &lines {
            assert!(line_width(l) <= 200.0 - SLACK + 0.01, "строка шире предела: {l:?}");
        }
        let emojis: usize = lines.iter().flat_map(|l| &l.segs).filter(|s| s.emoji).count();
        assert_eq!(emojis, 4, "эмодзи не потерялись");
        // Сегменты покрывают текст без пропусков (переводов строк тут нет).
        let joined: String = lines.iter().flat_map(|l| &l.segs).map(|s| s.text.as_str()).collect();
        assert_eq!(joined, text);
        for l in &lines {
            for s in &l.segs {
                assert_eq!(&text[s.start..s.end], s.text);
            }
        }
    }

    #[test]
    fn newlines_and_emoji_row_break() {
        let lines = plain("строка 1 😀\n\nстрока 3", 400.0);
        assert_eq!(lines.len(), 3);
        assert!(lines[1].segs.is_empty(), "пустая строка сохраняется");
        assert_eq!((lines[1].start, lines[1].end), ("строка 1 😀\n".len(), "строка 1 😀\n".len()));
        let row = "🎉".repeat(30);
        let lines = plain(&row, 200.0);
        assert!(lines.len() > 1 && lines.iter().all(|l| l.segs.iter().all(|s| s.emoji)));
    }

    #[test]
    fn styles_split_segments_and_change_width() {
        let f = fonts();
        let text = "обычный жирный курсив код";
        let b = text.find("жирный").unwrap();
        let spans = [
            Span { start: b, end: b + "жирный".len(), style: Style { bold: true, ..Style::PLAIN } },
            Span { start: text.find("курсив").unwrap(), end: text.find(" код").unwrap(), style: Style { italic: true, ..Style::PLAIN } },
            Span { start: text.find("код").unwrap(), end: text.len(), style: Style { mono: true, ..Style::PLAIN } },
        ];
        let lines = layout(text, &spans, 1000.0, &f);
        assert_eq!(lines.len(), 1);
        let styles: Vec<(&str, Style)> = lines[0].segs.iter().map(|s| (s.text.as_str(), s.style)).collect();
        assert_eq!(styles[1], ("жирный", spans[0].style));
        assert!(styles.iter().any(|(t, s)| *t == "код" && s.mono));
        assert!(f.width(spans[0].style, "жирный") > f.width(Style::PLAIN, "жирный"), "жирный шире");
        // Сегменты идут подряд по x.
        for w in lines[0].segs.windows(2) {
            assert!((w[0].x + w[0].w - w[1].x).abs() < 0.01);
        }
    }

    #[test]
    fn caret_and_hit_roundtrip() {
        let f = fonts();
        let text = "Привет 👋 мир, как дела?";
        let lines = layout(text, &[], 1000.0, &f);
        let l = &lines[0];
        let mut prev = -1.0;
        for (b, _) in text.grapheme_indices(true).chain(std::iter::once((text.len(), ""))) {
            let x = caret_x(l, b, &f);
            assert!(x > prev, "каретка растёт по графемам: {b} {x} {prev}");
            assert_eq!(hit(l, x, &f), b, "попадание по x возвращает ту же границу");
            prev = x;
        }
        let e = text.find('👋').unwrap();
        assert!((caret_x(l, e + '👋'.len_utf8(), &f) - caret_x(l, e, &f) - EMOJI_CELL).abs() < 0.01);
    }

    #[test]
    fn measures_like_the_font() {
        let f = fonts();
        let a = f.width(Style::PLAIN, "iiii");
        let b = f.width(Style::PLAIN, "WWWW");
        assert!(b > a * 2.0, "метрики не пропорциональные: {a} {b}");
        let m = Style { mono: true, ..Style::PLAIN };
        assert!((f.width(m, "iiii") - f.width(m, "WWWW")).abs() < 0.01, "моноширинный");
        assert!(f.width(Style::PLAIN, "中文") >= 2.0 * 12.0, "широкие символы");
    }
}
