//! Номер телефона: страна по коду набора и маска по шаблону сервера
//! (`help.getCountriesList`: коды, префиксы, шаблоны вида `XXX XXX XXXX`).
//! Дизайн - Вход, Вх-2: сначала страна, потом номер с маской.

use crate::model::{Country, CountryCode};

/// Только цифры.
pub fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

/// Страна и код по полному номеру (цифры с кодом страны): самый длинный код,
/// а при общем коде (+7 у России и Казахстана) — по префиксу номера.
pub fn detect<'a>(countries: &'a [Country], full: &str) -> Option<(&'a Country, &'a CountryCode)> {
    let mut best: Option<(&Country, &CountryCode, usize)> = None;
    for c in countries {
        for code in &c.codes {
            let Some(rest) = full.strip_prefix(code.code.as_str()) else { continue };
            let prefix_len = if code.prefixes.is_empty() {
                0
            } else {
                match code.prefixes.iter().filter(|p| rest.starts_with(p.as_str())).map(|p| p.len()).max() {
                    Some(n) => n + 1, // совпавший префикс сильнее, чем код без префиксов
                    None => continue,
                }
            };
            let score = code.code.len() * 100 + prefix_len;
            if best.is_none_or(|(_, _, s)| score > s) {
                best = Some((c, code, score));
            }
        }
    }
    best.map(|(c, code, _)| (c, code))
}

/// Шаблон для национальной части: самый длинный из подходящих по префиксу.
pub fn pattern_for<'a>(code: &'a CountryCode, national: &str) -> Option<&'a str> {
    code.patterns
        .iter()
        .filter(|p| {
            // В шаблоне могут быть цифры-префиксы: "9XX XXX XXXX".
            p.chars().filter(|c| *c != ' ').zip(national.chars()).all(|(pc, nc)| pc == 'X' || pc == nc)
        })
        .max_by_key(|p| p.len())
        .map(String::as_str)
}

/// Форматирует национальную часть по шаблону; лишние цифры идут без пробелов в конце.
pub fn format_national(pattern: Option<&str>, national: &str) -> String {
    let Some(p) = pattern else { return national.to_string() };
    let mut out = String::new();
    let mut it = national.chars().peekable();
    for pc in p.chars() {
        if it.peek().is_none() {
            break;
        }
        if pc == ' ' {
            out.push(' ');
        } else {
            out.push(it.next().unwrap_or_default());
        }
    }
    out.extend(it);
    out.trim_end().to_string()
}

/// Сколько цифр ждёт шаблон (0 — неизвестно).
pub fn expected_len(pattern: Option<&str>) -> usize {
    pattern.map(|p| p.chars().filter(|c| *c != ' ').count()).unwrap_or(0)
}

/// Флаг страны как последовательность региональных индикаторов (для набора картинок).
pub fn flag_sequence(iso2: &str) -> String {
    iso2.chars()
        .filter(char::is_ascii_alphabetic)
        .map(|c| char::from_u32(0x1F1E6 + (c.to_ascii_uppercase() as u32 - 'A' as u32)).unwrap_or(' '))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn country(iso: &str, code: &str, prefixes: &[&str], patterns: &[&str]) -> Country {
        Country {
            iso2: iso.into(),
            name: iso.into(),
            default_name: iso.into(),
            codes: vec![CountryCode {
                code: code.into(),
                prefixes: prefixes.iter().map(|s| s.to_string()).collect(),
                patterns: patterns.iter().map(|s| s.to_string()).collect(),
            }],
        }
    }

    fn list() -> Vec<Country> {
        vec![
            country("RU", "7", &[], &["XXX XXX XXXX"]),
            country("KZ", "7", &["6", "7"], &["XXX XXX XXXX"]),
            country("US", "1", &[], &["XXX XXX XXXX"]),
            country("AG", "1", &["268"], &["XXX XXXX"]),
            country("GB", "44", &[], &["XXXX XXXXXX"]),
            country("UA", "380", &[], &["XX XXX XXXX"]),
        ]
    }

    #[test]
    fn detects_by_longest_code_and_prefix() {
        let l = list();
        assert_eq!(detect(&l, "79991234567").unwrap().0.iso2, "RU");
        assert_eq!(detect(&l, "77011234567").unwrap().0.iso2, "KZ");
        assert_eq!(detect(&l, "12125551234").unwrap().0.iso2, "US");
        assert_eq!(detect(&l, "12684601234").unwrap().0.iso2, "AG");
        assert_eq!(detect(&l, "380441234567").unwrap().0.iso2, "UA");
        assert!(detect(&l, "999").is_none());
    }

    #[test]
    fn formats_by_pattern() {
        let p = Some("XXX XXX XXXX");
        assert_eq!(format_national(p, "9991234567"), "999 123 4567");
        assert_eq!(format_national(p, "9991"), "999 1");
        assert_eq!(format_national(p, "999"), "999");
        assert_eq!(format_national(p, "999123456789"), "999 123 456789");
        assert_eq!(format_national(None, "123"), "123");
        assert_eq!(expected_len(p), 10);
        let code = CountryCode { code: "1".into(), prefixes: vec![], patterns: vec!["XXX".into(), "9XX XXX".into()] };
        assert_eq!(pattern_for(&code, "912345"), Some("9XX XXX"));
        assert_eq!(pattern_for(&code, "812"), Some("XXX"));
    }

    #[test]
    fn flag_from_iso() {
        assert_eq!(flag_sequence("ru"), "\u{1F1F7}\u{1F1FA}");
        assert_eq!(digits("+7 (999) 123-45-67"), "79991234567");
    }
}
