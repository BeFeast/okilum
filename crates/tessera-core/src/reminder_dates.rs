//! Deterministic Gregorian date selections for the Remind me action (#724).
//!
//! Accept the whole selection, never a date hidden inside arbitrary prose. The
//! caller supplies the local calendar date; this module does not read a clock,
//! choose a notification time, write Tasks, or interpret locale-dependent slashes.
use time::{Date, Month};

/// Parse ISO YYYY-MM-DD, day-first D.M[.YYYY], or a day with a full English,
/// Russian or Hebrew Gregorian month name (and optional four-digit year).
/// English also accepts month-first order and a comma after the day.
/// Without a year, today is eligible; otherwise choose the next valid occurrence.
/// Explicit dates, including past dates, are preserved rather than rolled forward.
pub fn parse(selection: &str, today: Date) -> Option<Date> {
    let text = selection.trim().to_lowercase();
    if text.is_empty() || text.len() > 128 {
        return None;
    }
    let (day, month, year) = if text.contains('-') {
        let parts: Vec<_> = text.split('-').collect();
        if parts.len() != 3 || parts[1].len() != 2 || parts[2].len() != 2 {
            return None;
        }
        (number(parts[2])?, number(parts[1])?, Some(year(parts[0])?))
    } else if text.contains('.') {
        let parts: Vec<_> = text.split('.').collect();
        match parts.as_slice() {
            [day, month] => (number(day)?, number(month)?, None),
            [day, month, y] => (number(day)?, number(month)?, Some(year(y)?)),
            _ => return None,
        }
    } else {
        let parts: Vec<_> = text.split_whitespace().collect();
        if !(2..=3).contains(&parts.len()) {
            return None;
        }
        let y = match parts.get(2) {
            Some(s) => Some(year(s)?),
            None => None,
        };
        if let Some(month) = month_number(parts[1]) {
            (number(parts[0])?, month, y)
        } else {
            // Month-first and its comma are English-only, not locale guessing.
            let month = english_month(parts[0])?;
            (
                number(parts[1].strip_suffix(',').unwrap_or(parts[1]))?,
                month,
                y,
            )
        }
    };
    let month = Month::try_from(month).ok()?;
    if let Some(year) = year {
        return Date::from_calendar_date(year, month, day).ok();
    }
    // Eight years cover the leap-day gap across a non-leap century (2096–2104).
    for offset in 0..=8 {
        let y = today.year().checked_add(offset)?;
        if let Ok(candidate) = Date::from_calendar_date(y, month, day) {
            if candidate >= today {
                return Some(candidate);
            }
        }
    }
    None
}

fn number(s: &str) -> Option<u8> {
    (!s.is_empty() && s.len() <= 2 && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

fn year(s: &str) -> Option<i32> {
    (s.len() == 4 && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse::<i32>().ok().filter(|&y| y > 0))
        .flatten()
}

fn english_month(s: &str) -> Option<u8> {
    const NAMES: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    NAMES
        .iter()
        .position(|&name| name == s)
        .map(|i| i as u8 + 1)
}

fn month_number(s: &str) -> Option<u8> {
    if let Some(month) = english_month(s) {
        return Some(month);
    }
    const RUSSIAN: [(&str, &str); 12] = [
        ("январь", "января"),
        ("февраль", "февраля"),
        ("март", "марта"),
        ("апрель", "апреля"),
        ("май", "мая"),
        ("июнь", "июня"),
        ("июль", "июля"),
        ("август", "августа"),
        ("сентябрь", "сентября"),
        ("октябрь", "октября"),
        ("ноябрь", "ноября"),
        ("декабрь", "декабря"),
    ];
    if let Some(i) = RUSSIAN.iter().position(|&(a, b)| s == a || s == b) {
        return Some(i as u8 + 1);
    }
    const HEBREW: [&str; 12] = [
        "ינואר",
        "פברואר",
        "מרץ",
        "אפריל",
        "מאי",
        "יוני",
        "יולי",
        "אוגוסט",
        "ספטמבר",
        "אוקטובר",
        "נובמבר",
        "דצמבר",
    ];
    let s = s.strip_prefix('ב').unwrap_or(s);
    HEBREW
        .iter()
        .position(|&name| name == s)
        .map(|i| i as u8 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn supported_languages_and_numeric_forms_agree() {
        for input in [
            "1 ноября",
            "1 ноябрь",
            "1 November",
            "November 1, 2026",
            "1 נובמבר",
            "1 בנובמבר",
            "01.11",
            "1.11.2026",
            "2026-11-01",
            "  1\u{a0}НОЯБРЯ  2026  ",
        ] {
            assert_eq!(
                parse(input, date!(2026 - 10 - 08)),
                Some(date!(2026 - 11 - 01)),
                "{input}"
            );
        }
    }

    #[test]
    fn missing_year_uses_today_or_next_occurrence() {
        assert_eq!(
            parse("08.10", date!(2026 - 10 - 08)),
            Some(date!(2026 - 10 - 08))
        );
        assert_eq!(
            parse("07.10", date!(2026 - 10 - 08)),
            Some(date!(2027 - 10 - 07))
        );
        assert_eq!(
            parse("1 January", date!(2026 - 12 - 31)),
            Some(date!(2027 - 01 - 01))
        );
        assert_eq!(
            parse("29.02", date!(2097 - 01 - 01)),
            Some(date!(2104 - 02 - 29))
        );
    }

    #[test]
    fn explicit_year_never_rolls_forward() {
        assert_eq!(
            parse("2024-02-29", date!(2026 - 10 - 08)),
            Some(date!(2024 - 02 - 29))
        );
        assert_eq!(parse("29 февраля 2025", date!(2026 - 10 - 08)), None);
        assert_eq!(parse("01.01", date!(9999 - 12 - 31)), None);
    }

    #[test]
    fn invalid_or_ambiguous_selection_has_no_action() {
        for input in [
            "",
            "November",
            "tomorrow",
            "1/11",
            "11/01/2026",
            "2026-1-01",
            "31.04",
            "0.11",
            "1.13",
            "1.11.26",
            "1.11.0000",
            "1 November extra",
            "meet 1 November",
            "1 November 2026 tomorrow",
            "1.11.2026.",
            "-1.11",
            "+1.11",
            "2026-11-01T09:00",
            "1, November",
            "א׳ תשרי",
            "١.١١",
        ] {
            assert_eq!(parse(input, date!(2026 - 10 - 08)), None, "{input}");
        }
    }

    #[test]
    fn all_localized_months_match_calendar_order() {
        let ru = [
            "января",
            "февраля",
            "марта",
            "апреля",
            "мая",
            "июня",
            "июля",
            "августа",
            "сентября",
            "октября",
            "ноября",
            "декабря",
        ];
        let he = [
            "ינואר",
            "פברואר",
            "מרץ",
            "אפריל",
            "מאי",
            "יוני",
            "יולי",
            "אוגוסט",
            "ספטמבר",
            "אוקטובר",
            "נובמבר",
            "דצמבר",
        ];
        for (i, names) in ru.iter().zip(he).enumerate() {
            for name in [*names.0, names.1] {
                let d = parse(&format!("1 {name} 2026"), date!(2026 - 01 - 01)).unwrap();
                assert_eq!(u8::from(d.month()), i as u8 + 1);
            }
        }
    }
}
