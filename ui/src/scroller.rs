//! Своя прокрутка истории (решение по П-2: `ListView` оценивает высоту и
//! дёргается, «прокрутить к строке» нет; переход — сменой окна модели).
//!
//! Вид держится за **опорную строку**: линия опоры на высоте `p` от верха
//! видимой области; строки до опоры лежат в контейнере, прижатом к линии
//! снизу, строки от опоры — в контейнере, прижатом к ней сверху. Раскладку
//! внутри контейнеров делает Slint, поэтому новые строки сверху, пересчёт
//! высот и подгрузка окна не сдвигают то, что видно. Rust решает только,
//! какие строки показать (видимые и запас), и держит края: до начала чата
//! выше не прокрутить, у конца истории вид прижат к низу.
//!
//! Высоты строк Slint сообщает после раскладки; до этого — оценка.

use std::collections::HashMap;

/// Запас строк за краями видимой области, px.
pub const MARGIN: f32 = 300.0;
/// Высота ещё не измеренной строки.
pub const ESTIMATE: f32 = 48.0;

/// Порядок строк окна: сообщения по id, затем очередь отправки (ключ −local_id).
pub fn rank(key: i64) -> (bool, i64) {
    if key > 0 {
        (false, key)
    } else {
        (true, -key)
    }
}

/// Что показать: строки `above` (до опоры) и `below` (от опоры), линия опоры.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub above: std::ops::Range<usize>,
    pub below: std::ops::Range<usize>,
    pub pivot: f32,
    /// Вид у конца истории (кнопка «вниз» не нужна, новые — сразу видны).
    pub at_bottom: bool,
    /// Вид упёрся в край окна, а за ним ещё есть сообщения.
    pub need_older: bool,
    pub need_newer: bool,
    /// Верхняя и нижняя видимые строки (индексы окна).
    pub top_row: Option<usize>,
    pub bottom_row: Option<usize>,
}

#[derive(Debug, Default)]
pub struct Scroller {
    keys: Vec<i64>,
    heights: HashMap<i64, f32>,
    /// Опорная строка (None — после последней: вид прижат к низу).
    split: Option<i64>,
    p: f32,
    view_h: f32,
    start: bool,
    end: bool,
    /// Отложенный переход: строк с таким ключом в окне ещё нет.
    pending: Option<i64>,
}

impl Scroller {
    pub fn new() -> Self {
        Self::default()
    }

    fn h(&self, i: usize) -> f32 {
        self.heights.get(&self.keys[i]).copied().unwrap_or(ESTIMATE)
    }

    fn measured(&self, i: usize) -> bool {
        self.heights.contains_key(&self.keys[i])
    }

    fn split_index(&self) -> usize {
        match self.split {
            None => self.keys.len(),
            Some(k) => self.keys.partition_point(|x| rank(*x) < rank(k)),
        }
    }

    pub fn keys(&self) -> &[i64] {
        &self.keys
    }

    /// Новое окно строк. Опорная строка остаётся на месте; если её больше нет
    /// (удалена), опорой становится следующая по порядку.
    pub fn set_rows(&mut self, keys: Vec<i64>, start: bool, end: bool) {
        self.keys = keys;
        self.start = start;
        self.end = end;
        if self.heights.len() > 4 * self.keys.len() + 512 {
            let keep: std::collections::HashSet<i64> = self.keys.iter().copied().collect();
            self.heights.retain(|k, _| keep.contains(k));
        }
        if let Some(target) = self.pending {
            if self.keys.iter().any(|k| rank(*k) >= rank(target)) {
                self.pending = None;
                self.jump_to(target);
                return;
            }
        }
        let s = self.split_index();
        self.split = self.keys.get(s).copied();
        self.normalize();
    }

    pub fn set_view_height(&mut self, h: f32) {
        if self.split.is_none() {
            self.p = h; // у низа — остаёмся у низа
        }
        self.view_h = h;
        self.normalize();
    }

    pub fn view_height(&self) -> f32 {
        self.view_h
    }

    /// Высота строки после раскладки.
    pub fn set_height(&mut self, key: i64, h: f32) -> bool {
        let old = self.heights.insert(key, h);
        if old == Some(h) {
            return false;
        }
        self.normalize();
        true
    }

    /// Колесо или тачпад: `dy > 0` — содержимое вниз (к старым сообщениям).
    pub fn scroll(&mut self, dy: f32) {
        self.p += dy;
        self.normalize();
    }

    /// Сообщение (или первое после него) — к верху видимой области.
    pub fn jump_to(&mut self, key: i64) {
        let i = self.keys.partition_point(|x| rank(*x) < rank(key));
        if i >= self.keys.len() && !self.end {
            self.pending = Some(key); // окно ещё не пришло
            return;
        }
        self.split = self.keys.get(i).copied();
        self.p = if self.split.is_some() { 0.0 } else { self.view_h };
        self.normalize();
    }

    pub fn jump_bottom(&mut self) {
        self.pending = None;
        self.split = None;
        self.p = self.view_h;
        self.normalize();
    }

    /// Опору — к видимой области, края — по началу и концу истории.
    fn normalize(&mut self) {
        let n = self.keys.len();
        let mut s = self.split_index();
        // Опорная строка целиком выше видимой области — опора ниже.
        while s < n && self.measured(s) && self.p + self.h(s) <= 0.0 {
            self.p += self.h(s);
            s += 1;
        }
        // Линия опоры ниже видимой области — опора выше.
        while s > 0 && self.p > self.view_h && self.measured(s - 1) {
            s -= 1;
            self.p -= self.h(s);
        }
        // Дальше загруженных строк не прокрутить: у края окна вид ждёт подгрузки
        // (иначе быстрое колесо уносит в пустоту). Неизмеренные — по оценке.
        let top = self.p - (0..s).map(|i| self.h(i)).sum::<f32>();
        if top > 0.0 {
            self.p -= top;
        }
        let bottom = self.p + (s..n).map(|i| self.h(i)).sum::<f32>();
        if bottom < self.view_h {
            self.p += self.view_h - bottom;
        }
        // Конец истории: вид прижат к низу (короткая история — тоже у низа).
        if self.end && (s..n).all(|i| self.measured(i)) && bottom <= self.view_h + 0.5 {
            s = n;
            self.p = self.view_h;
        }
        self.split = self.keys.get(s).copied();
    }

    pub fn at_bottom(&self) -> bool {
        self.end && self.split.is_none() && (self.p - self.view_h).abs() < 0.5
    }

    pub fn view(&self) -> View {
        let n = self.keys.len();
        let s = self.split_index();
        let mut a = s;
        let mut acc = 0.0;
        while a > 0 && acc < self.p + MARGIN {
            a -= 1;
            acc += self.h(a);
        }
        let mut b = s;
        let mut acc = 0.0;
        while b < n && acc < self.view_h - self.p + MARGIN {
            acc += self.h(b);
            b += 1;
        }
        // Видимые строки: сверху вниз по координатам.
        let (mut top_row, mut bottom_row) = (None, None);
        let mut y = self.p - (a..s).map(|i| self.h(i)).sum::<f32>();
        for i in a..b {
            let h = self.h(i);
            if y + h > 0.0 && y < self.view_h {
                top_row.get_or_insert(i);
                bottom_row = Some(i);
            }
            y += h;
        }
        View {
            above: a..s,
            below: s..b,
            pivot: self.p,
            at_bottom: self.at_bottom(),
            need_older: a == 0 && !self.start,
            need_newer: b == n && !self.end,
            top_row,
            bottom_row,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Окно ids `r`, все строки по 50 px и измерены (как после раскладки).
    fn scroller(r: std::ops::RangeInclusive<i64>, start: bool, end: bool) -> Scroller {
        let mut s = Scroller::new();
        s.set_view_height(400.0);
        let keys: Vec<i64> = r.collect();
        for k in &keys {
            s.heights.insert(*k, 50.0);
        }
        s.set_rows(keys, start, end);
        s
    }

    #[test]
    fn starts_at_bottom_and_sticks() {
        let mut s = scroller(1..=100, false, true);
        s.jump_bottom();
        let v = s.view();
        assert!(v.at_bottom);
        assert_eq!(v.below, 100..100);
        assert_eq!(v.bottom_row, Some(99));
        assert_eq!(v.top_row, Some(92), "8 строк по 50 в 400 px");
        // Новое сообщение внизу — вид остаётся у низа.
        s.heights.insert(101, 50.0);
        s.set_rows((1..=101).collect(), false, true);
        assert!(s.view().at_bottom);
        assert_eq!(s.view().bottom_row, Some(100));
    }

    #[test]
    fn scroll_up_keeps_content_when_rows_prepended() {
        let mut s = scroller(51..=100, false, true);
        s.jump_bottom();
        s.scroll(1000.0); // на 20 строк вверх: строка 100 была на [350, 400]
        let v = s.view();
        assert!(!v.at_bottom);
        let top_key = s.keys()[v.top_row.unwrap()];
        assert_eq!(top_key, 73, "вверху — строка на 20 выше верхней видимой до прокрутки (93)");
        // Подгрузили старше: 1..=50 сверху — видимая строка та же.
        let pivot = s.view().pivot;
        s.set_rows((1..=100).collect(), false, true);
        assert_eq!(s.keys()[s.view().top_row.unwrap()], 73);
        assert_eq!(s.view().pivot, pivot, "линия опоры не сдвинулась");
    }

    #[test]
    fn start_of_chat_limits_scroll_and_needs() {
        let mut s = scroller(1..=10, true, false);
        s.jump_to(1);
        s.scroll(500.0);
        let v = s.view();
        assert_eq!(v.top_row, Some(0), "выше первого сообщения не прокрутить");
        assert!(!v.need_older, "начало чата — подгружать нечего");
        assert!(v.need_newer, "низ окна рядом, дальше есть сообщения");
        let mut s = scroller(1..=20, false, true);
        s.jump_to(1);
        assert!(s.view().need_older);
    }

    #[test]
    fn short_history_sits_at_bottom() {
        let mut s = scroller(1..=3, true, true);
        s.jump_to(1);
        let v = s.view();
        assert!(v.at_bottom, "три сообщения — у низа, как в мессенджерах");
        assert_eq!(v.above, 0..3);
        s.scroll(-100.0);
        assert!(s.view().at_bottom);
        s.scroll(100.0);
        assert!(s.view().at_bottom);
    }

    #[test]
    fn jump_waits_for_window_and_targets_next_existing() {
        let mut s = scroller(1..=10, false, false);
        let before = (s.split, s.p);
        s.jump_to(500);
        assert_eq!((s.split, s.p), before, "окна вокруг 500 ещё нет — вид не прыгает");
        for k in 480..=520 {
            s.heights.insert(k, 50.0);
        }
        // Сообщения 500 нет (удалено): опора — следующее.
        s.set_rows((480..=520).filter(|k| *k != 500).collect(), false, false);
        let v = s.view();
        assert_eq!(s.keys()[v.below.start], 501);
        assert_eq!(v.pivot, 0.0);
        assert_eq!(s.keys()[v.top_row.unwrap()], 501, "цель вверху видимой области");
    }

    #[test]
    fn deleted_pivot_and_locals_order() {
        assert!(rank(5) < rank(6) && rank(1_000_000) < rank(-1) && rank(-1) < rank(-2));
        let mut s = scroller(1..=100, false, true);
        s.jump_to(40);
        s.set_rows((1..=100).filter(|k| *k != 40).collect(), false, true);
        assert_eq!(s.keys()[s.view().below.start], 41);
    }

    #[test]
    fn fast_wheel_stops_at_window_edge() {
        let mut s = scroller(51..=100, false, true);
        s.jump_bottom();
        s.scroll(100_000.0);
        let v = s.view();
        assert_eq!(v.top_row, Some(0), "у края окна, а не в пустоте");
        assert!(v.need_older);
        s.scroll(-100_000.0);
        assert!(s.view().at_bottom);
    }

    #[test]
    fn measured_heights_change_does_not_move_pivot() {
        let mut s = scroller(1..=100, false, false);
        s.jump_to(50);
        let before = s.view().pivot;
        s.set_height(49, 300.0); // строка выше опоры стала выше
        assert_eq!(s.view().pivot, before, "строки от опоры не сдвинулись");
        s.set_height(55, 10.0);
        assert_eq!(s.view().pivot, before);
    }
}
