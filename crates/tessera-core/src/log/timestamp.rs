//! Record timestamps as Unix nanoseconds. Parsing is strict: a value that is
//! not clearly a timestamp is left unparsed and shown as written.

use ::time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};

/// RFC 3339 and its common relaxations: a space or lower-case `t` separator,
/// a comma before the fraction, `±HHMM`/`±HH` offsets. A value without an
/// offset is read as UTC, which displays it exactly as written.
pub fn parse_text(text: &[u8]) -> Option<i64> {
    let b = text.trim_ascii();
    if b.len() < 19 {
        return number_text(b);
    }
    let year = digits(&b[0..4])?;
    let month = digits(&b[5..7])?;
    let day = digits(&b[8..10])?;
    let hour = digits(&b[11..13])?;
    let minute = digits(&b[14..16])?;
    let second = digits(&b[17..19])?;
    if b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't' | b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let mut pos = 19;
    let mut nanos = 0u32;
    if matches!(b.get(pos), Some(b'.' | b',')) {
        pos += 1;
        let start = pos;
        while b.get(pos).is_some_and(u8::is_ascii_digit) {
            if pos - start < 9 {
                nanos = nanos * 10 + u32::from(b[pos] - b'0');
            }
            pos += 1;
        }
        if pos == start {
            return None;
        }
        for _ in (pos - start)..9 {
            nanos *= 10;
        }
    }
    let offset = match &b[pos..] {
        [] => UtcOffset::UTC,
        [b'Z' | b'z'] => UtcOffset::UTC,
        [sign @ (b'+' | b'-'), rest @ ..] => {
            let (h, m) = match rest {
                [h1, h2] => (digits(&[*h1, *h2])?, 0),
                [h1, h2, b':', m1, m2] | [h1, h2, m1, m2] => {
                    (digits(&[*h1, *h2])?, digits(&[*m1, *m2])?)
                }
                _ => return None,
            };
            let sign = if *sign == b'-' { -1 } else { 1 };
            UtcOffset::from_hms(sign * h as i8, sign * m as i8, 0).ok()?
        }
        _ => return None,
    };
    let date = Date::from_calendar_date(year as i32, Month::try_from(month as u8).ok()?, day as u8)
        .ok()?;
    let time = Time::from_hms_nano(hour as u8, minute as u8, second as u8, nanos).ok()?;
    i64::try_from(
        PrimitiveDateTime::new(date, time)
            .assume_offset(offset)
            .unix_timestamp_nanos(),
    )
    .ok()
}

/// Epoch numbers. The unit is inferred from magnitude: seconds up to 1e11
/// (year 5138), then milliseconds, microseconds and nanoseconds.
pub fn parse_number(raw: &[u8]) -> Option<i64> {
    let text = std::str::from_utf8(raw.trim_ascii()).ok()?;
    let scale_of = |whole: i128| match whole.unsigned_abs() {
        n if n < 100_000_000_000 => 1_000_000_000i128,
        n if n < 100_000_000_000_000 => 1_000_000,
        n if n < 100_000_000_000_000_000 => 1_000,
        _ => 1,
    };
    // Plain decimals are computed exactly; floats would drift by hundreds
    // of nanoseconds at today's epoch.
    let (int_part, frac_part) = text.split_once('.').unwrap_or((text, ""));
    if !frac_part.bytes().all(|c| c.is_ascii_digit()) {
        return float_number(text);
    }
    let Ok(whole) = int_part.parse::<i64>() else {
        return float_number(text);
    };
    let whole = i128::from(whole);
    let scale = scale_of(whole);
    let digits = &frac_part[..frac_part.len().min(18)];
    let fraction = if digits.is_empty() {
        0
    } else {
        let numerator: i128 = digits.parse().ok()?;
        numerator * scale / 10i128.pow(digits.len() as u32)
    };
    let sign = if int_part.starts_with('-') { -1 } else { 1 };
    i64::try_from(whole * scale + sign * fraction).ok()
}

fn float_number(text: &str) -> Option<i64> {
    let value: f64 = text.parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    let scale = match value.abs() {
        n if n < 1e11 => 1e9,
        n if n < 1e14 => 1e6,
        n if n < 1e17 => 1e3,
        _ => 1.0,
    };
    let nanos = (value * scale).round();
    (nanos.abs() < 9.2e18).then_some(nanos as i64)
}

fn number_text(b: &[u8]) -> Option<i64> {
    let numeric = !b.is_empty()
        && b.iter()
            .all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'-'));
    if numeric {
        parse_number(b)
    } else {
        None
    }
}

fn digits(b: &[u8]) -> Option<u32> {
    b.iter().try_fold(0u32, |n, c| {
        c.is_ascii_digit().then(|| n * 10 + u32::from(c - b'0'))
    })
}

/// `HH:MM:SS.mmm` of a Unix-nanosecond timestamp in UTC, for the row column.
pub fn clock_utc(nanos: i64) -> String {
    let Ok(at) = ::time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(nanos)) else {
        return String::new();
    };
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        at.hour(),
        at.minute(),
        at.second(),
        at.millisecond()
    )
}

/// `YYYY-MM-DD HH:MM:SS.mmm UTC`, for the detail pane.
pub fn datetime_utc(nanos: i64) -> String {
    let Ok(at) = ::time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(nanos)) else {
        return String::new();
    };
    format!(
        "{:04}-{:02}-{:02} {} UTC",
        at.year(),
        u8::from(at.month()),
        at.day(),
        clock_utc(nanos)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: i64 = 1_759_276_800_000_000_000; // 2025-10-01T00:00:00Z

    #[test]
    fn rfc3339_and_relaxations() {
        assert_eq!(parse_text(b"2025-10-01T00:00:00Z"), Some(BASE));
        assert_eq!(
            parse_text(b"2025-10-01T00:00:00.240Z"),
            Some(BASE + 240_000_000)
        );
        assert_eq!(
            parse_text(b"2025-10-01 00:00:00,5"),
            Some(BASE + 500_000_000)
        );
        assert_eq!(
            parse_text(b"2025-10-01T02:00:00+02:00"),
            Some(BASE),
            "offset is applied"
        );
        assert_eq!(parse_text(b"2025-10-01T02:00:00+0200"), Some(BASE));
        assert_eq!(parse_text(b"2025-09-30T23:00:00-01"), Some(BASE));
        assert_eq!(
            parse_text(b"2025-10-01T00:00:00.123456789123Z"),
            Some(BASE + 123_456_789),
            "digits past nanoseconds are consumed, not rejected"
        );
        assert_eq!(parse_text(b"2025-02-30T00:00:00Z"), None);
        assert_eq!(parse_text(b"2025-10-01T00:00:00 UTC"), None);
        assert_eq!(parse_text(b"yesterday"), None);
        assert_eq!(parse_text(b"1759276800"), Some(BASE));
    }

    #[test]
    fn epoch_units_by_magnitude() {
        assert_eq!(parse_number(b"1759276800"), Some(BASE));
        assert_eq!(parse_number(b"1759276800000"), Some(BASE));
        assert_eq!(parse_number(b"1759276800000000"), Some(BASE));
        assert_eq!(parse_number(b"1759276800000000000"), Some(BASE));
        assert_eq!(parse_number(b"1759276800.25"), Some(BASE + 250_000_000));
        assert_eq!(parse_number(b"nan"), None);
        assert_eq!(parse_number(b"true"), None);
    }

    #[test]
    fn display_is_utc() {
        assert_eq!(clock_utc(BASE + 240_000_000), "00:00:00.240");
        assert_eq!(datetime_utc(BASE), "2025-10-01 00:00:00.000 UTC");
    }
}
