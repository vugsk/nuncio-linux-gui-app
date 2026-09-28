//! Время в списке и чате на языке интерфейса (Дизайн - Список диалогов,
//! раздел 2): сегодня — ЧЧ:ММ, вчера, день недели в пределах недели, дальше
//! ДД.ММ.ГГ; разделители дат в истории — «25 сентября» / «September 25».

use chrono::{DateTime, Datelike, Local, TimeZone};

fn local(ts: i64) -> Option<DateTime<Local>> {
    Local.timestamp_opt(ts, 0).single()
}

/// Время в строке диалога.
pub fn dialog_time(ts: i64, now: i64, lang: &str) -> String {
    let (Some(t), Some(n)) = (local(ts), local(now)) else { return String::new() };
    let days = (n.date_naive() - t.date_naive()).num_days();
    let ru = lang == "ru";
    match days {
        0 => t.format("%H:%M").to_string(),
        1 => if ru { "вчера" } else { "yesterday" }.into(),
        2..=6 => {
            let d = t.weekday().num_days_from_monday() as usize;
            let names: [&str; 7] = if ru {
                ["пн", "вт", "ср", "чт", "пт", "сб", "вс"]
            } else {
                ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
            };
            names[d].into()
        }
        _ => t.format("%d.%m.%y").to_string(),
    }
}

/// Время сообщения: ЧЧ:ММ.
pub fn message_time(ts: i64) -> String {
    local(ts).map(|t| t.format("%H:%M").to_string()).unwrap_or_default()
}

/// Ключ дня (для разделителей дат).
pub fn day_key(ts: i64) -> i64 {
    local(ts).map(|t| i64::from(t.num_days_from_ce())).unwrap_or(0)
}

/// Разделитель дат в истории.
pub fn date_label(ts: i64, now: i64, lang: &str) -> String {
    let (Some(t), Some(n)) = (local(ts), local(now)) else { return String::new() };
    let m = t.month0() as usize;
    let ru = lang == "ru";
    let month = if ru {
        ["января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября",
            "декабря"][m]
    } else {
        ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October",
            "November", "December"][m]
    };
    let day = t.day();
    let base = if ru { format!("{day} {month}") } else { format!("{month} {day}") };
    if t.year() == n.year() {
        base
    } else {
        format!("{base} {}", t.year())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
        Local.with_ymd_and_hms(y, m, d, h, min, 0).single().unwrap().timestamp()
    }

    #[test]
    fn dialog_times() {
        let now = at(2026, 9, 28, 12, 0);
        assert_eq!(dialog_time(at(2026, 9, 28, 9, 5), now, "ru"), "09:05");
        assert_eq!(dialog_time(at(2026, 9, 27, 23, 0), now, "ru"), "вчера");
        assert_eq!(dialog_time(at(2026, 9, 27, 23, 0), now, "en"), "yesterday");
        assert_eq!(dialog_time(at(2026, 9, 24, 10, 0), now, "ru"), "чт");
        assert_eq!(dialog_time(at(2026, 9, 1, 10, 0), now, "ru"), "01.09.26");
        assert_eq!(date_label(at(2026, 9, 25, 10, 0), now, "ru"), "25 сентября");
        assert_eq!(date_label(at(2025, 9, 25, 10, 0), now, "en"), "September 25 2025");
        assert_eq!(message_time(at(2026, 9, 28, 7, 3)), "07:03");
        assert_ne!(day_key(at(2026, 9, 28, 0, 1)), day_key(at(2026, 9, 27, 23, 59)));
    }
}
