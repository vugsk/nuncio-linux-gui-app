//! Ошибки команд — человеку на языке интерфейса (В10, локализация). Текст из
//! сетевого слоя — технический (коды Telegram, строки Go по-русски): здесь он
//! заменяется словами по виду ошибки; код сервера остаётся как есть.

use crate::model::{ErrorInfo, ErrorKind};

/// Текст ошибки для всплывающей подсказки, строки ошибки или результата проверки.
pub fn error_text(e: &ErrorInfo, ru: bool) -> String {
    match (e.kind, ru) {
        (ErrorKind::Flood, _) => {
            let wait = wait_label(e.retry_after, ru);
            if ru { format!("Слишком много запросов. Подождите {wait}") } else { format!("Too many requests. Wait {wait}") }
        }
        (ErrorKind::Network, true) if e.code == 1 => "Прокси отклонил соединение".into(),
        (ErrorKind::Network, false) if e.code == 1 => "The proxy rejected the connection".into(),
        (ErrorKind::Network, true) => "Нет соединения".into(),
        (ErrorKind::Network, false) => "No connection".into(),
        (ErrorKind::Cancelled, true) => "Отменено".into(),
        (ErrorKind::Cancelled, false) => "Cancelled".into(),
        (ErrorKind::Auth, true) => "Сеанс завершён".into(),
        (ErrorKind::Auth, false) => "Session ended".into(),
        (ErrorKind::Rpc, _) if e.text.is_empty() => if ru { "Ошибка сервера".into() } else { "Server error".into() },
        (ErrorKind::Rpc, true) => format!("Ошибка сервера: {}", e.text),
        (ErrorKind::Rpc, false) => format!("Server error: {}", e.text),
        (_, true) => "Внутренняя ошибка".into(),
        (_, false) => "Internal error".into(),
    }
}

/// «45 с», «3 мин», «2 ч».
fn wait_label(secs: u32, ru: bool) -> String {
    match secs {
        0..120 => format!("{secs} {}", if ru { "с" } else { "s" }),
        120..7200 => format!("{} {}", secs.div_ceil(60), if ru { "мин" } else { "min" }),
        _ => format!("{} {}", secs.div_ceil(3600), if ru { "ч" } else { "h" }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(kind: ErrorKind, code: i32, text: &str, retry_after: u32) -> ErrorInfo {
        ErrorInfo { kind, code, text: text.into(), retry_after }
    }

    #[test]
    fn texts_by_kind() {
        assert_eq!(error_text(&e(ErrorKind::Network, 0, "сеть: соединение сброшено", 0), false), "No connection");
        assert_eq!(error_text(&e(ErrorKind::Network, 0, "x", 0), true), "Нет соединения");
        assert_eq!(error_text(&e(ErrorKind::Flood, 420, "FLOOD_WAIT", 300), true), "Слишком много запросов. Подождите 5 мин");
        assert_eq!(error_text(&e(ErrorKind::Flood, 420, "FLOOD_WAIT", 30), false), "Too many requests. Wait 30 s");
        assert_eq!(error_text(&e(ErrorKind::Rpc, 400, "MESSAGE_TOO_LONG", 0), false), "Server error: MESSAGE_TOO_LONG");
        assert_eq!(error_text(&e(ErrorKind::Cancelled, 0, "отменено", 0), false), "Cancelled");
        assert_eq!(error_text(&e(ErrorKind::Internal, 0, "неожиданный ответ", 0), false), "Internal error");
    }
}
