//! Поле ввода сообщения — гибрид (решение Дз-17, спайк «Гибридное поле
//! ввода»): невидимый `TextInput` Slint остаётся движком (клавиши, IME,
//! буфер обмена, выделение клавишами, переходы по словам), а геометрию,
//! эмодзи-картинки, форматирование, каретку и выделение рисует наша
//! раскладка (`richtext`). Здесь — состояние поля без Slint: текст, стили,
//! свой стек отмены (после программной правки история `TextInput`
//! пропадает), Backspace по графемам, вертикальные переходы по нашим строкам.

use std::time::{Duration, Instant};

use domain::model::{Entity, EntityKind};
use unicode_segmentation::UnicodeSegmentation;

use crate::richtext::{self, Fonts, Line, Span, Style};

/// Вид форматирования на панели (Дз-15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Bold,
    Italic,
    Underline,
    Strike,
    Mono,
}

impl Format {
    fn style(self) -> Style {
        let p = Style::PLAIN;
        match self {
            Format::Bold => Style { bold: true, ..p },
            Format::Italic => Style { italic: true, ..p },
            Format::Underline => Style { underline: true, ..p },
            Format::Strike => Style { strike: true, ..p },
            Format::Mono => Style { mono: true, ..p },
        }
    }

    fn kind(self) -> EntityKind {
        match self {
            Format::Bold => EntityKind::Bold,
            Format::Italic => EntityKind::Italic,
            Format::Underline => EntityKind::Underline,
            Format::Strike => EntityKind::Strike,
            Format::Mono => EntityKind::Code,
        }
    }

    const ALL: [Format; 5] = [Format::Bold, Format::Italic, Format::Underline, Format::Strike, Format::Mono];
}

/// Участок одного вида форматирования (или ссылки с адресом).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Mark {
    start: usize,
    end: usize,
    kind: MarkKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MarkKind {
    Format(Format),
    Link(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    text: String,
    marks: Vec<Mark>,
    cursor: usize,
    anchor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Typing,
    Other,
}

/// Прямоугольник выделения в координатах поля.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Что поменять в невидимом `TextInput` после нашей правки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Apply {
    pub text: Option<String>,
    pub anchor: usize,
    pub cursor: usize,
}

/// Подряд набранное объединяется в один шаг отмены, если пауза меньше этой.
const COALESCE: Duration = Duration::from_millis(1200);

pub struct Composer {
    text: String,
    marks: Vec<Mark>,
    cursor: usize,
    anchor: usize,
    preedit: String,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last: Option<(EditKind, Instant)>,
    width: f32,
    lines: Vec<Line>,
    /// Цель по x при переходах вверх и вниз (держится, пока идём по строкам).
    goal_x: Option<f32>,
    pub fonts: Fonts,
}

impl Default for Composer {
    fn default() -> Self {
        Self {
            text: String::new(),
            marks: Vec::new(),
            cursor: 0,
            anchor: 0,
            preedit: String::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            last: None,
            width: 300.0,
            lines: vec![Line::default()],
            goal_x: None,
            fonts: Fonts::new(),
        }
    }
}

fn prev_boundary(text: &str, pos: usize) -> usize {
    text[..pos].grapheme_indices(true).next_back().map(|(i, _)| i).unwrap_or(0)
}

fn next_boundary(text: &str, pos: usize) -> usize {
    text[pos..].graphemes(true).next().map(|g| pos + g.len()).unwrap_or(text.len())
}

/// Байтовое смещение в UTF-16.
fn utf16(text: &str, byte: usize) -> i32 {
    text[..byte].encode_utf16().count() as i32
}

fn byte_of_utf16(text: &str, off: i32) -> usize {
    let mut u = 0;
    for (b, c) in text.char_indices() {
        if u >= off {
            return b;
        }
        u += c.len_utf16() as i32;
    }
    text.len()
}

impl Composer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn selection(&self) -> (usize, usize) {
        (self.anchor.min(self.cursor), self.anchor.max(self.cursor))
    }

    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    pub fn has_selection(&self) -> bool {
        self.anchor != self.cursor
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot { text: self.text.clone(), marks: self.marks.clone(), cursor: self.cursor, anchor: self.anchor }
    }

    fn restore(&mut self, s: Snapshot) -> Apply {
        self.text = s.text;
        self.marks = s.marks;
        self.cursor = s.cursor;
        self.anchor = s.anchor;
        self.relayout();
        Apply { text: Some(self.text.clone()), anchor: self.anchor, cursor: self.cursor }
    }

    /// Запомнить состояние до правки (подряд набранное — одним шагом).
    fn push_undo(&mut self, kind: EditKind) {
        let now = Instant::now();
        let merge = kind == EditKind::Typing
            && matches!(self.last, Some((EditKind::Typing, t)) if now.duration_since(t) < COALESCE);
        if !merge {
            self.undo.push(self.snapshot());
            if self.undo.len() > 200 {
                self.undo.remove(0);
            }
        }
        self.last = Some((kind, now));
        self.redo.clear();
    }

    /// Новое содержимое от модели (правка сообщения, очистка после отправки).
    pub fn load(&mut self, text: &str, entities: &[Entity]) -> Apply {
        self.text = text.to_string();
        self.marks = entities
            .iter()
            .filter_map(|e| {
                let kind = match e.kind {
                    EntityKind::Bold => MarkKind::Format(Format::Bold),
                    EntityKind::Italic => MarkKind::Format(Format::Italic),
                    EntityKind::Underline => MarkKind::Format(Format::Underline),
                    EntityKind::Strike => MarkKind::Format(Format::Strike),
                    EntityKind::Code | EntityKind::Pre => MarkKind::Format(Format::Mono),
                    EntityKind::TextUrl => MarkKind::Link(e.url.clone()),
                    _ => return None,
                };
                let start = byte_of_utf16(text, e.offset);
                let end = byte_of_utf16(text, e.offset + e.length);
                (start < end).then_some(Mark { start, end, kind })
            })
            .collect();
        self.cursor = self.text.len();
        self.anchor = self.cursor;
        self.undo.clear();
        self.redo.clear();
        self.last = None;
        self.preedit.clear();
        self.relayout();
        Apply { text: Some(self.text.clone()), anchor: self.anchor, cursor: self.cursor }
    }

    /// `TextInput` изменил текст сам (набор, вставка, вырезание, ввод IME):
    /// находим изменение и сдвигаем стили. Вставка сразу после стиля его
    /// продолжает (как в Telegram), вставка в начале — нет.
    pub fn edited(&mut self, new: &str, cursor: usize, anchor: usize) {
        if new == self.text {
            self.set_cursor(cursor, anchor);
            return;
        }
        let old = std::mem::take(&mut self.text);
        let pre = old.bytes().zip(new.bytes()).take_while(|(a, b)| a == b).count();
        let mut pre = pre.min(old.len()).min(new.len());
        while !old.is_char_boundary(pre) || !new.is_char_boundary(pre) {
            pre -= 1;
        }
        let max_suf = (old.len() - pre).min(new.len() - pre);
        let mut suf = old.bytes().rev().zip(new.bytes().rev()).take(max_suf).take_while(|(a, b)| a == b).count();
        while !old.is_char_boundary(old.len() - suf) || !new.is_char_boundary(new.len() - suf) {
            suf -= 1;
        }
        let removed = pre..old.len() - suf;
        let inserted = new.len() - suf - pre;
        self.text = old.clone();
        let typing = removed.is_empty() && inserted <= 8 && !new[pre..pre + inserted].contains('\n');
        self.push_undo(if typing { EditKind::Typing } else { EditKind::Other });
        self.text = new.to_string();
        let shift = |p: usize| -> usize {
            if p <= removed.start {
                p
            } else if p >= removed.end {
                p - removed.len() + inserted
            } else {
                removed.start
            }
        };
        for m in &mut self.marks {
            let (s, e) = (m.start, m.end);
            let at_insert = |p: usize| removed.is_empty() && p == removed.start;
            // Вставка в начале стиля его не продолжает, на правой границе — продолжает.
            m.start = if at_insert(s) { s + inserted } else { shift(s) };
            m.end = if at_insert(e) && s < e { e + inserted } else { shift(e) };
        }
        self.normalize();
        self.cursor = cursor.min(self.text.len());
        self.anchor = anchor.min(self.text.len());
        self.goal_x = None;
        self.relayout();
    }

    /// Каретка или выделение сменились в `TextInput` (стрелки, Ctrl+A, мышь).
    pub fn set_cursor(&mut self, cursor: usize, anchor: usize) {
        self.cursor = cursor.min(self.text.len());
        self.anchor = anchor.min(self.text.len());
        while !self.text.is_char_boundary(self.cursor) {
            self.cursor -= 1;
        }
        while !self.text.is_char_boundary(self.anchor) {
            self.anchor -= 1;
        }
    }

    pub fn set_preedit(&mut self, p: &str) {
        if self.preedit != p {
            self.preedit = p.to_string();
            self.relayout();
        }
    }

    fn delete_range(&mut self, a: usize, b: usize) -> Apply {
        self.push_undo(EditKind::Other);
        self.text.replace_range(a..b, "");
        let n = b - a;
        for m in &mut self.marks {
            let f = |p: usize| if p <= a { p } else if p >= b { p - n } else { a };
            m.start = f(m.start);
            m.end = f(m.end);
        }
        self.normalize();
        self.cursor = a;
        self.anchor = a;
        self.goal_x = None;
        self.relayout();
        Apply { text: Some(self.text.clone()), anchor: a, cursor: a }
    }

    /// Backspace по графемам: семья, флаг, тон кожи, клавиша удаляются целиком.
    pub fn backspace(&mut self) -> Option<Apply> {
        let (a, b) = self.selection();
        if a != b {
            return Some(self.delete_range(a, b));
        }
        (self.cursor > 0).then(|| {
            let p = prev_boundary(&self.text, self.cursor);
            self.delete_range(p, self.cursor)
        })
    }

    pub fn delete(&mut self) -> Option<Apply> {
        let (a, b) = self.selection();
        if a != b {
            return Some(self.delete_range(a, b));
        }
        (self.cursor < self.text.len()).then(|| {
            let n = next_boundary(&self.text, self.cursor);
            self.delete_range(self.cursor, n)
        })
    }

    pub fn undo(&mut self) -> Option<Apply> {
        let s = self.undo.pop()?;
        self.redo.push(self.snapshot());
        self.last = None;
        Some(self.restore(s))
    }

    pub fn redo(&mut self) -> Option<Apply> {
        let s = self.redo.pop()?;
        self.undo.push(self.snapshot());
        self.last = None;
        Some(self.restore(s))
    }

    /// Слить пересекающиеся и соседние участки одного вида, убрать пустые.
    fn normalize(&mut self) {
        self.marks.retain(|m| m.start < m.end);
        self.marks.sort_by(|a, b| (format!("{:?}", a.kind), a.start).cmp(&(format!("{:?}", b.kind), b.start)));
        let mut out: Vec<Mark> = Vec::new();
        for m in std::mem::take(&mut self.marks) {
            match out.last_mut() {
                Some(l) if l.kind == m.kind && m.start <= l.end => l.end = l.end.max(m.end),
                _ => out.push(m),
            }
        }
        self.marks = out;
    }

    /// Весь ли участок уже в этом стиле.
    fn covered(&self, f: Format, a: usize, b: usize) -> bool {
        let mut pos = a;
        let mut ms: Vec<&Mark> = self.marks.iter().filter(|m| m.kind == MarkKind::Format(f)).collect();
        ms.sort_by_key(|m| m.start);
        for m in ms {
            if m.start <= pos && m.end > pos {
                pos = m.end;
            }
        }
        pos >= b
    }

    fn remove_kind(&mut self, a: usize, b: usize, pred: impl Fn(&MarkKind) -> bool) {
        let mut out = Vec::new();
        for m in std::mem::take(&mut self.marks) {
            if !pred(&m.kind) || m.end <= a || m.start >= b {
                out.push(m);
                continue;
            }
            if m.start < a {
                out.push(Mark { start: m.start, end: a, kind: m.kind.clone() });
            }
            if m.end > b {
                out.push(Mark { start: b, end: m.end, kind: m.kind });
            }
        }
        self.marks = out;
    }

    /// Стиль на выделении: весь выделенный уже в нём — снять, иначе — наложить.
    pub fn toggle(&mut self, f: Format) -> bool {
        let (a, b) = self.selection();
        if a == b {
            return false;
        }
        self.push_undo(EditKind::Other);
        if self.covered(f, a, b) {
            self.remove_kind(a, b, |k| *k == MarkKind::Format(f));
        } else {
            self.marks.push(Mark { start: a, end: b, kind: MarkKind::Format(f) });
        }
        self.normalize();
        self.relayout();
        true
    }

    /// Ссылка под выделенным текстом (пустой адрес — снять ссылку).
    pub fn set_link(&mut self, url: &str) -> bool {
        let (a, b) = self.selection();
        if a == b {
            return false;
        }
        self.push_undo(EditKind::Other);
        self.remove_kind(a, b, |k| matches!(k, MarkKind::Link(_)));
        let url = url.trim();
        if !url.is_empty() {
            let url = if url.contains("://") || url.starts_with("mailto:") { url.to_string() } else { format!("https://{url}") };
            self.marks.push(Mark { start: a, end: b, kind: MarkKind::Link(url) });
        }
        self.normalize();
        self.relayout();
        true
    }

    /// Убрать форматирование с выделения.
    pub fn clear_format(&mut self) -> bool {
        let (a, b) = self.selection();
        if a == b {
            return false;
        }
        self.push_undo(EditKind::Other);
        self.remove_kind(a, b, |_| true);
        self.relayout();
        true
    }

    /// Какие стили есть на выделении (для подсветки кнопок панели).
    pub fn active(&self, f: Format) -> bool {
        let (a, b) = self.selection();
        a != b && self.covered(f, a, b)
    }

    /// Адрес ссылки на выделении (для диалога ссылки).
    pub fn link_at_selection(&self) -> Option<String> {
        let (a, b) = self.selection();
        self.marks.iter().find_map(|m| match &m.kind {
            MarkKind::Link(u) if m.start <= a && m.end >= b => Some(u.clone()),
            _ => None,
        })
    }

    /// Форматирование для отправки (смещения UTF-16).
    pub fn entities(&self) -> Vec<Entity> {
        let mut out: Vec<Entity> = self
            .marks
            .iter()
            .map(|m| {
                let (kind, url) = match &m.kind {
                    MarkKind::Format(f) => (f.kind(), String::new()),
                    MarkKind::Link(u) => (EntityKind::TextUrl, u.clone()),
                };
                let offset = utf16(&self.text, m.start);
                Entity { kind, offset, length: utf16(&self.text, m.end) - offset, url }
            })
            .collect();
        out.sort_by_key(|e| (e.offset, e.kind.code()));
        out
    }

    // ------------------------------------------------------------ геометрия

    /// Текст раскладки: с текстом IME на месте каретки.
    fn display(&self) -> (String, Vec<Span>, Vec<String>) {
        let mut links = Vec::new();
        let mut spans: Vec<Span> = self
            .marks
            .iter()
            .map(|m| {
                let style = match &m.kind {
                    MarkKind::Format(f) => f.style(),
                    MarkKind::Link(u) => {
                        links.push(u.clone());
                        Style { link: links.len() as i32 - 1, ..Style::PLAIN }
                    }
                };
                Span { start: m.start, end: m.end, style }
            })
            .collect();
        if self.preedit.is_empty() {
            return (self.text.clone(), spans, links);
        }
        let at = self.cursor;
        let n = self.preedit.len();
        for s in &mut spans {
            if s.start >= at {
                s.start += n;
            }
            if s.end > at {
                s.end += n;
            }
        }
        spans.push(Span { start: at, end: at + n, style: Style { underline: true, ..Style::PLAIN } });
        let mut t = self.text.clone();
        t.insert_str(at, &self.preedit);
        (t, spans, links)
    }

    pub fn set_width(&mut self, w: f32) {
        if (w - self.width).abs() > 0.5 {
            self.width = w;
            self.relayout();
        }
    }

    fn relayout(&mut self) {
        let (t, spans, _) = self.display();
        // Поле без запаса SLACK: строки ровно по ширине поля.
        self.lines = richtext::layout(&t, &spans, self.width + richtext::SLACK, &self.fonts);
        if self.lines.is_empty() {
            self.lines.push(Line::default());
        }
    }

    /// Строки со ссылками для отрисовки.
    pub fn display_lines(&self) -> (Vec<Line>, Vec<String>) {
        (self.lines.clone(), self.display().2)
    }

    pub fn content_height(&self) -> f32 {
        self.lines.len().max(1) as f32 * richtext::LINE
    }

    /// Смещение в тексте раскладки (с IME) по смещению в тексте поля.
    fn display_pos(&self, pos: usize) -> usize {
        if !self.preedit.is_empty() && pos >= self.cursor {
            pos + self.preedit.len()
        } else {
            pos
        }
    }

    /// Строка, в которой байт `pos` раскладки (на границе строк — следующая,
    /// кроме конца текста).
    fn line_of(&self, pos: usize) -> usize {
        for (i, l) in self.lines.iter().enumerate() {
            let next_start = self.lines.get(i + 1).map(|n| n.start);
            if pos < l.end || next_start.is_none_or(|ns| pos < ns) {
                return i;
            }
        }
        self.lines.len() - 1
    }

    /// Каретка: x, y верха строки.
    pub fn caret(&self) -> (f32, f32) {
        // Каретка — после текста IME, если он есть.
        let pos = self.cursor + self.preedit.len();
        let i = self.line_of(pos);
        (richtext::caret_x(&self.lines[i], pos.clamp(self.lines[i].start, self.lines[i].end), &self.fonts),
            i as f32 * richtext::LINE)
    }

    /// Прямоугольники выделения по строкам.
    pub fn selection_rects(&self) -> Vec<Rect> {
        let (a, b) = self.selection();
        if a == b {
            return Vec::new();
        }
        let (a, b) = (self.display_pos(a), self.display_pos(b));
        let mut out = Vec::new();
        for (i, l) in self.lines.iter().enumerate() {
            let next = self.lines.get(i + 1).map(|n| n.start).unwrap_or(usize::MAX);
            if b <= l.start || a >= next {
                continue;
            }
            let x0 = richtext::caret_x(l, a.clamp(l.start, l.end), &self.fonts);
            let mut x1 = richtext::caret_x(l, b.clamp(l.start, l.end), &self.fonts);
            if b > l.end && b >= next {
                x1 += 4.0; // перенос строки внутри выделения
            }
            if x1 > x0 {
                out.push(Rect { x: x0, y: i as f32 * richtext::LINE, w: x1 - x0, h: richtext::LINE });
            }
        }
        out
    }

    /// Смещение по точке в поле (клик, перетаскивание).
    pub fn hit(&self, x: f32, y: f32) -> usize {
        let i = ((y / richtext::LINE).floor().max(0.0) as usize).min(self.lines.len() - 1);
        let p = richtext::hit(&self.lines[i], x, &self.fonts).min(self.lines[i].end);
        self.text_pos(p)
    }

    fn text_pos(&self, p: usize) -> usize {
        if self.preedit.is_empty() || p <= self.cursor {
            p.min(self.text.len())
        } else {
            p.saturating_sub(self.preedit.len()).max(self.cursor).min(self.text.len())
        }
    }

    /// Стрелки вверх и вниз по нашим строкам (у строк с эмодзи геометрия
    /// своя, родная навигация `TextInput` промахивается).
    pub fn vertical(&mut self, down: bool, extend: bool) -> Apply {
        let (x, _) = self.caret();
        let goal = *self.goal_x.get_or_insert(x);
        let i = self.line_of(self.cursor);
        let target = if down {
            match self.lines.get(i + 1) {
                Some(_) => richtext::hit(&self.lines[i + 1], goal, &self.fonts).min(self.lines[i + 1].end),
                None => self.text.len(),
            }
        } else if i == 0 {
            0
        } else {
            richtext::hit(&self.lines[i - 1], goal, &self.fonts).min(self.lines[i - 1].end)
        };
        self.cursor = target;
        if !extend {
            self.anchor = target;
        }
        Apply { text: None, anchor: self.anchor, cursor: self.cursor }
    }

    /// Home и End — начало и конец нашей строки.
    pub fn home_end(&mut self, end: bool, extend: bool) -> Apply {
        let i = self.line_of(self.cursor);
        self.cursor = if end { self.lines[i].end } else { self.lines[i].start };
        if !extend {
            self.anchor = self.cursor;
        }
        self.goal_x = None;
        Apply { text: None, anchor: self.anchor, cursor: self.cursor }
    }

    /// Двойной клик — слово целиком.
    pub fn select_word(&mut self, at: usize) -> Apply {
        let word = self
            .text
            .split_word_bound_indices()
            .find(|(i, w)| (at >= *i && at < i + w.len()) || (at == self.text.len() && i + w.len() == at));
        if let Some((i, w)) = word {
            self.anchor = i;
            self.cursor = i + w.len();
        }
        Apply { text: None, anchor: self.anchor, cursor: self.cursor }
    }

    /// Мышь: поставить каретку или протянуть выделение.
    pub fn pointer(&mut self, x: f32, y: f32, extend: bool) -> Apply {
        let p = self.hit(x, y);
        self.cursor = p;
        if !extend {
            self.anchor = p;
        }
        self.goal_x = None;
        Apply { text: None, anchor: self.anchor, cursor: self.cursor }
    }

    pub fn formats(&self) -> [bool; 5] {
        Format::ALL.map(|f| self.active(f))
    }
}

// ---------------------------------------------------------------- связь со Slint

use std::cell::RefCell;
use std::rc::Rc;

use domain::chat::{ChatIntent, ChatModel};
use slint::platform::Key;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::{App, Composer as Global, SelRect};

/// Высота поля до прокрутки (как в разметке).
const MAX_H: f32 = 160.0;

struct View {
    c: Composer,
    model_seq: u64,
    sel_seq: i32,
    scroll: f32,
    send: Option<Rc<dyn Fn(ChatIntent)>>,
}

thread_local! {
    static VIEW: RefCell<View> =
        RefCell::new(View { c: Composer::new(), model_seq: u64::MAX, sel_seq: 0, scroll: 0.0, send: None });
}

fn with(app: &App, f: impl FnOnce(&mut View) -> Option<Apply>) {
    VIEW.with(|v| {
        let Ok(mut v) = v.try_borrow_mut() else { return };
        if let Some(a) = f(&mut v) {
            apply(app, &mut v, a);
        }
        publish(app, &mut v);
    });
}

/// Наша правка — в невидимый `TextInput`: текст (если менялся) и выделение.
fn apply(app: &App, v: &mut View, a: Apply) {
    let g = app.global::<Global>();
    if let Some(t) = a.text {
        g.set_text(t.clone().into());
        if let Some(send) = &v.send {
            send(ChatIntent::ComposerEdited(t));
        }
    }
    v.sel_seq += 1;
    g.set_sel_anchor(a.anchor as i32);
    g.set_sel_cursor(a.cursor as i32);
    g.set_sel_seq(v.sel_seq);
}

fn publish(app: &App, v: &mut View) {
    let g = app.global::<Global>();
    let (lines, links) = v.c.display_lines();
    g.set_lines(crate::chatview::rich_rows(lines, &links));
    let rects: Vec<SelRect> =
        v.c.selection_rects().into_iter().map(|r| SelRect { x: r.x, y: r.y, w: r.w, h: r.h }).collect();
    g.set_selection(ModelRc::from(Rc::new(VecModel::from(rects))));
    let (x, y) = v.c.caret();
    g.set_caret_x(x);
    g.set_caret_y(y);
    let h = v.c.content_height();
    g.set_content_height(h);
    // Длинный текст: каретка всегда видна.
    if h <= MAX_H {
        v.scroll = 0.0;
    } else {
        if y < v.scroll {
            v.scroll = y;
        }
        if y + richtext::LINE > v.scroll + MAX_H {
            v.scroll = y + richtext::LINE - MAX_H;
        }
        v.scroll = v.scroll.clamp(0.0, h - MAX_H);
    }
    g.set_scroll_y(v.scroll);
    g.set_has_selection(v.c.has_selection());
    let active = v.c.formats();
    let m = g.get_active();
    if (0..5).any(|i| m.row_data(i) != Some(active[i])) {
        g.set_active(ModelRc::from(Rc::new(VecModel::from(active.to_vec()))));
    }
}

/// Содержимое поля от модели (правка, очистка после отправки, другой чат).
pub fn sync_model(app: &App, m: &ChatModel) {
    with(app, |v| {
        if v.model_seq == m.composer_seq {
            return None;
        }
        v.model_seq = m.composer_seq;
        Some(v.c.load(&m.composer, &m.composer_entities))
    });
}

/// Отправка: текст с форматированием (текст модель уже знает из правок).
pub fn send() {
    VIEW.with(|v| {
        let v = v.borrow();
        if let Some(send) = &v.send {
            send(ChatIntent::Send(v.c.entities()));
        }
    });
}

/// Горячие клавиши поля. Буква — и в латинской, и в русской раскладке.
fn hotkey(text: &str, shift: bool) -> Option<u8> {
    let c = text.chars().next()?.to_lowercase().next()?;
    Some(match (c, shift) {
        ('b' | 'и', false) => 0,
        ('i' | 'ш', false) => 1,
        ('u' | 'г', false) => 2,
        ('x' | 'ч', true) => 3,
        ('m' | 'ь', true) => 4,
        ('k' | 'л', false) => 5,
        ('n' | 'т', true) => 6,
        ('z' | 'я', false) => 10,
        ('z' | 'я', true) | ('y' | 'н', false) => 11,
        _ => return None,
    })
}

fn format(app: &App, v: &mut View, i: u8) {
    match i {
        0..=4 => {
            v.c.toggle(Format::ALL[i as usize]);
        }
        5 if v.c.has_selection() => {
            let g = app.global::<Global>();
            g.set_link_url(v.c.link_at_selection().unwrap_or_default().into());
            g.set_link_open(true);
        }
        6 => {
            v.c.clear_format();
        }
        _ => {}
    }
}

pub fn bind(app: &App, send: impl Fn(ChatIntent) + 'static) {
    VIEW.with(|v| v.borrow_mut().send = Some(Rc::new(send)));
    let g = app.global::<Global>();
    let a = app.as_weak();
    g.on_edited(move |text: SharedString, cursor, anchor| {
        if let Some(app) = a.upgrade() {
            with(&app, |v| {
                v.c.edited(&text, cursor.max(0) as usize, anchor.max(0) as usize);
                if let Some(send) = &v.send {
                    send(ChatIntent::ComposerEdited(text.to_string()));
                }
                None
            });
        }
    });
    let a = app.as_weak();
    g.on_cursor_moved(move |cursor, anchor| {
        if let Some(app) = a.upgrade() {
            with(&app, |v| {
                v.c.set_cursor(cursor.max(0) as usize, anchor.max(0) as usize);
                None
            });
        }
    });
    let a = app.as_weak();
    g.on_preedit(move |p: SharedString| {
        if let Some(app) = a.upgrade() {
            with(&app, |v| {
                v.c.set_preedit(&p);
                None
            });
        }
    });
    let a = app.as_weak();
    g.on_width(move |w| {
        if let Some(app) = a.upgrade() {
            with(&app, |v| {
                v.c.set_width(w);
                None
            });
        }
    });
    let a = app.as_weak();
    g.on_wheel(move |dy| {
        if let Some(app) = a.upgrade() {
            with(&app, |v| {
                v.scroll -= dy;
                None
            });
        }
    });
    let a = app.as_weak();
    g.on_pointer(move |x, y, kind| {
        if let Some(app) = a.upgrade() {
            with(&app, |v| {
                Some(match kind {
                    0 => v.c.pointer(x, y, false),
                    1 => v.c.pointer(x, y, true),
                    _ => {
                        let at = v.c.hit(x, y);
                        v.c.select_word(at)
                    }
                })
            });
        }
    });
    let a = app.as_weak();
    g.on_format(move |i| {
        if let Some(app) = a.upgrade() {
            with(&app, |v| {
                format(&app, v, i as u8);
                None
            });
        }
    });
    let a = app.as_weak();
    g.on_link_apply(move |url: SharedString| {
        if let Some(app) = a.upgrade() {
            app.global::<Global>().set_link_open(false);
            with(&app, |v| {
                v.c.set_link(&url);
                None
            });
        }
    });
    let a = app.as_weak();
    g.on_key(move |text: SharedString, ctrl, shift, alt| {
        let Some(app) = a.upgrade() else { return false };
        let is = |k: Key| text == SharedString::from(k);
        let mut accepted = true;
        with(&app, |v| {
            if is(Key::Return) && !shift && !ctrl && !alt {
                if let Some(send) = &v.send {
                    send(ChatIntent::Send(v.c.entities()));
                }
                return None;
            }
            if is(Key::Backspace) && !ctrl {
                return v.c.backspace();
            }
            if is(Key::Delete) && !ctrl {
                return v.c.delete();
            }
            if (is(Key::UpArrow) || is(Key::DownArrow)) && !ctrl && !alt {
                return Some(v.c.vertical(is(Key::DownArrow), shift));
            }
            if (is(Key::Home) || is(Key::End)) && !ctrl {
                return Some(v.c.home_end(is(Key::End), shift));
            }
            if ctrl && !alt && matches!(text.as_str(), "v" | "V" | "м" | "М") {
                // Текст вставит сам TextInput; картинку из буфера проверит поток домена.
                if let Some(send) = &v.send {
                    send(ChatIntent::PasteImage);
                }
                accepted = false;
                return None;
            }
            if ctrl && !alt {
                match hotkey(&text, shift) {
                    Some(10) => return v.c.undo(),
                    Some(11) => return v.c.redo(),
                    Some(i) => {
                        format(&app, v, i);
                        return None;
                    }
                    None => {}
                }
            }
            accepted = false;
            None
        });
        accepted
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(c: &mut Composer, s: &str) {
        let mut t = c.text().to_string();
        let at = c.cursor;
        t.insert_str(at, s);
        c.edited(&t, at + s.len(), at + s.len());
    }

    #[test]
    fn backspace_by_graphemes() {
        let mut c = Composer::new();
        for g in ["a", "👨‍👩‍👧‍👦", "🇷🇺", "👍🏽", "1️⃣", "é"] {
            typed(&mut c, g);
        }
        let mut expect = c.text().to_string();
        for g in ["é", "1️⃣", "👍🏽", "🇷🇺", "👨‍👩‍👧‍👦"] {
            let a = c.backspace().unwrap();
            expect.truncate(expect.len() - g.len());
            assert_eq!(a.text.as_deref(), Some(expect.as_str()), "{g} удаляется целиком");
        }
        assert_eq!(c.text(), "a");
        c.set_cursor(0, 0);
        assert!(c.backspace().is_none(), "в начале — нечего");
        assert_eq!(c.delete().unwrap().text.as_deref(), Some(""));
    }

    #[test]
    fn typing_extends_style_and_edits_shift_it() {
        let mut c = Composer::new();
        typed(&mut c, "привет мир");
        c.set_cursor(12, 0); // «привет» выделено
        assert!(c.toggle(Format::Bold));
        assert_eq!(c.entities(), vec![Entity { kind: EntityKind::Bold, offset: 0, length: 6, url: String::new() }]);
        // Набор сразу после жирного — тоже жирный; в начале — нет.
        c.set_cursor(12, 12);
        typed(&mut c, "ик");
        assert_eq!(c.entities()[0].length, 8);
        c.set_cursor(0, 0);
        typed(&mut c, "О, ");
        assert_eq!((c.entities()[0].offset, c.entities()[0].length), (3, 8));
        // Удаление внутри стиля сжимает его.
        c.set_cursor("О, при".len(), "О, ".len());
        c.backspace();
        assert_eq!(c.text(), "О, ветик мир");
        assert_eq!((c.entities()[0].offset, c.entities()[0].length), (3, 5));
        // Повторное включение на целиком жирном — снимает.
        c.set_cursor("О, ветик".len(), "О, ".len());
        assert!(c.active(Format::Bold));
        c.toggle(Format::Bold);
        assert!(c.entities().is_empty());
    }

    #[test]
    fn partial_toggle_and_clear_and_link() {
        let mut c = Composer::new();
        typed(&mut c, "один два три");
        c.set_cursor("один два".len(), 0);
        c.toggle(Format::Italic);
        // Снять курсив только с «два»: выделение внутри целиком курсивное.
        c.set_cursor("один два".len(), "один ".len());
        c.toggle(Format::Italic);
        let e = c.entities();
        assert_eq!(e.len(), 1);
        assert_eq!((e[0].offset, e[0].length), (0, 5), "«один » осталось курсивом");
        c.set_cursor(c.text().len(), "один два ".len());
        c.set_link("example.org/x");
        assert_eq!(c.link_at_selection().as_deref(), Some("https://example.org/x"));
        c.set_cursor(c.text().len(), 0);
        c.clear_format();
        assert!(c.entities().is_empty());
    }

    #[test]
    fn undo_redo_own_stack_with_typing_coalesced() {
        let mut c = Composer::new();
        typed(&mut c, "а");
        typed(&mut c, "б");
        typed(&mut c, "в");
        c.backspace();
        assert_eq!(c.text(), "аб");
        assert_eq!(c.undo().unwrap().text.as_deref(), Some("абв"), "отмена Backspace");
        assert_eq!(c.undo().unwrap().text.as_deref(), Some(""), "набор подряд — один шаг");
        assert!(c.undo().is_none());
        assert_eq!(c.redo().unwrap().text.as_deref(), Some("абв"));
        assert_eq!(c.redo().unwrap().text.as_deref(), Some("аб"));
        // Форматирование тоже отменяется.
        c.set_cursor(0, c.text().len());
        c.toggle(Format::Strike);
        assert_eq!(c.entities().len(), 1);
        c.undo();
        assert!(c.entities().is_empty());
    }

    #[test]
    fn load_entities_roundtrip_utf16() {
        let mut c = Composer::new();
        let es = vec![
            Entity { kind: EntityKind::Bold, offset: 0, length: 2, url: String::new() },
            Entity { kind: EntityKind::TextUrl, offset: 3, length: 3, url: "https://a.b".into() },
        ];
        c.load("👍 ок!", &es);
        assert_eq!(c.entities(), es, "эмодзи — две единицы UTF-16");
        assert_eq!(c.selection(), (c.text().len(), c.text().len()), "каретка в конце");
    }

    #[test]
    fn geometry_caret_hit_vertical() {
        let mut c = Composer::new();
        c.set_width(120.0);
        typed(&mut c, "первая строка 👋 длинная, чтобы перенестись");
        assert!(c.lines().len() >= 2, "перенос по ширине поля");
        let (x, y) = c.caret();
        assert!(y > 0.0 && x > 0.0);
        // Клик по координатам каретки — то же смещение.
        let end = c.text().len();
        assert_eq!(c.hit(x, y + 5.0), end);
        // Вверх и обратно вниз — в тот же столбец (примерно).
        c.vertical(false, false);
        assert!(c.selection().0 < end);
        c.vertical(true, false);
        assert_eq!(c.selection().0, end);
        // Home/End по строке.
        c.home_end(false, false);
        let start = c.selection().0;
        assert_eq!(start, c.lines().last().unwrap().start);
        c.home_end(true, true);
        assert_eq!(c.selection(), (start, end));
        assert!(!c.selection_rects().is_empty());
        // Двойной клик — слово.
        let at = c.text().find("длинная").unwrap() + 2;
        let a = c.select_word(at);
        assert_eq!(&c.text()[a.anchor..a.cursor], "длинная");
    }

    #[test]
    fn hotkeys_in_both_layouts() {
        assert_eq!(hotkey("b", false), Some(0));
        assert_eq!(hotkey("И", false), Some(0), "Ctrl+И в русской раскладке — жирный");
        assert_eq!(hotkey("x", true), Some(3));
        assert_eq!(hotkey("x", false), None, "Ctrl+X — вырезать, не зачёркнутый");
        assert_eq!(hotkey("я", false), Some(10));
        assert_eq!(hotkey("Z", true), Some(11));
        assert_eq!(hotkey("c", false), None, "копирование остаётся у TextInput");
    }

    #[test]
    fn preedit_is_drawn_at_caret() {
        let mut c = Composer::new();
        typed(&mut c, "ab");
        c.set_cursor(1, 1);
        let (x0, _) = c.caret();
        c.set_preedit("にほん");
        let (lines, _) = c.display_lines();
        let t: String = lines.iter().flat_map(|l| l.segs.iter().map(|s| s.text.clone())).collect();
        assert_eq!(t, "aにほんb");
        assert!(lines[0].segs.iter().any(|s| s.style.underline), "текст IME подчёркнут");
        assert!(c.caret().0 > x0, "каретка после текста IME");
        c.set_preedit("");
        assert_eq!(c.caret().0, x0);
    }
}
