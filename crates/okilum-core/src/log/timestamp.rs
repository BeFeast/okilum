//! Record timestamps as Unix nanoseconds. Parsing is strict: a value that is
//! not clearly a timestamp is left unparsed and shown as written.

use ::time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};

/// RFC 3339 and its common relaxations: a space or lower-case `t` separator,
/// a comma before the fraction, `±HHMM`/`±HH` offsets. A value without an
/// offset is read as UTC, which displays it exactly as written. Two fixed
/// text-log shapes are accepted as well: glog's `yyyymmdd hh:mm:ss.uuuuuu`
/// and the access-log `dd/Mon/yyyy:hh:mm:ss ±hhmm`. Shapes without a year
/// (syslog's `Oct 10 06:00:01`, glog's old `mmdd`) are not timestamps.
pub fn parse_text(text: &[u8]) -> Option<i64> {
    let b = text.trim_ascii();
    if let Some(ms) = dotnet_date(b) {
        return Some(ms);
    }
    // A number written as a string takes the epoch path at any length, e.g.
    // quill's 19-digit nanoseconds (#1157); RFC 3339 always has a ':'.
    if b.len() < 17 || is_number(b) {
        return number_text(b);
    }
    match (b[2], b[8]) {
        (b'/', _) => access_log(b),
        (_, b' ') => glog(b),
        _ => rfc3339(b),
    }
}

/// .NET's JSON date, as PowerShell's `ConvertTo-Json` writes `DateTime`:
/// `/Date(1791612051256)/`, milliseconds since the epoch, sometimes with a
/// `+0300` that only records the writer's zone.
fn dotnet_date(b: &[u8]) -> Option<i64> {
    let inner = b.strip_prefix(b"/Date(")?.strip_suffix(b")/")?;
    let digits = inner.len()
        - inner
            .iter()
            .rev()
            .position(|c| matches!(c, b'+' | b'-'))
            .filter(|&at| at == 4 && inner.len() > 5)
            .map_or(0, |_| 5);
    let ms: i64 = std::str::from_utf8(&inner[..digits]).ok()?.parse().ok()?;
    ms.checked_mul(1_000_000)
}

fn rfc3339(b: &[u8]) -> Option<i64> {
    if b.len() < 19 {
        return None;
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
    let (nanos, pos) = fraction(b, 19)?;
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
    assemble([year, month, day, hour, minute, second], nanos, offset)
}

/// glog: `20261010 06:04:00.000279`, local time without an offset, read as
/// UTC like any offset-less value.
fn glog(b: &[u8]) -> Option<i64> {
    if b[11] != b':' || b[14] != b':' {
        return None;
    }
    let (nanos, pos) = fraction(b, 17)?;
    if pos != b.len() {
        return None;
    }
    let field = |range: std::ops::Range<usize>| digits(&b[range]);
    assemble(
        [
            field(0..4)?,
            field(4..6)?,
            field(6..8)?,
            field(9..11)?,
            field(12..14)?,
            field(15..17)?,
        ],
        nanos,
        UtcOffset::UTC,
    )
}

/// Apache/nginx `%t`: `10/Oct/2026:06:00:01 +0000`.
fn access_log(b: &[u8]) -> Option<i64> {
    let &[d1, d2, b'/', m1, m2, m3, b'/', y1, y2, y3, y4, b':', h1, h2, b':', n1, n2, b':', s1, s2, b' ', sign @ (b'+' | b'-'), o1, o2, o3, o4] =
        b
    else {
        return None;
    };
    const MONTHS: [&[u8; 3]; 12] = [
        b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov",
        b"Dec",
    ];
    let month = MONTHS.iter().position(|m| **m == [m1, m2, m3])? as u32 + 1;
    let sign = if sign == b'-' { -1 } else { 1 };
    let offset = UtcOffset::from_hms(
        sign * digits(&[o1, o2])? as i8,
        sign * digits(&[o3, o4])? as i8,
        0,
    )
    .ok()?;
    assemble(
        [
            digits(&[y1, y2, y3, y4])?,
            month,
            digits(&[d1, d2])?,
            digits(&[h1, h2])?,
            digits(&[n1, n2])?,
            digits(&[s1, s2])?,
        ],
        0,
        offset,
    )
}

/// An optional `.`/`,` fraction at `pos`: nanoseconds and the position after
/// it. Digits past nanoseconds are consumed; a separator without digits is
/// not a timestamp.
fn fraction(b: &[u8], mut pos: usize) -> Option<(u32, usize)> {
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
    Some((nanos, pos))
}

fn assemble(
    [year, month, day, hour, minute, second]: [u32; 6],
    nanos: u32,
    offset: UtcOffset,
) -> Option<i64> {
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

fn is_number(b: &[u8]) -> bool {
    !b.is_empty()
        && b.iter()
            .all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'-'))
}

fn number_text(b: &[u8]) -> Option<i64> {
    if is_number(b) {
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
    fn dotnet_json_dates() {
        assert_eq!(parse_text(b"/Date(1759276800000)/"), Some(BASE));
        assert_eq!(
            parse_text(b"/Date(1759276800250+0300)/"),
            Some(BASE + 250_000_000)
        );
        assert_eq!(parse_text(b"/Date(-1000)/"), Some(-1_000_000_000));
        assert_eq!(parse_text(b"/Date(soon)/"), None);
    }

    #[test]
    fn text_log_shapes() {
        assert_eq!(
            parse_text(b"20251001 00:00:00.000279"),
            Some(BASE + 279_000)
        );
        assert_eq!(parse_text(b"20251001 00:00:00"), Some(BASE));
        assert_eq!(parse_text(b"20251001 00:00:00.x"), None);
        assert_eq!(parse_text(b"01/Oct/2025:02:00:00 +0200"), Some(BASE));
        assert_eq!(parse_text(b"30/Sep/2025:23:00:00 -0100"), Some(BASE));
        assert_eq!(parse_text(b"01/Okt/2025:00:00:00 +0000"), None);
        // No year: shown as written, never guessed.
        assert_eq!(parse_text(b"Oct  1 00:00:00"), None);
        assert_eq!(parse_text(b"1001 00:00:00.000279"), None);
    }

    #[test]
    fn epoch_strings_of_any_length() {
        // quill's JsonFileSink: Unix nanoseconds as a 19-digit string (#1157).
        assert_eq!(
            parse_text(b"1791612242497259677"),
            Some(1_791_612_242_497_259_677)
        );
        assert_eq!(parse_text(b"1759276800000000000"), Some(BASE));
        assert_eq!(parse_text(b"1759276800000000.5"), Some(BASE + 500));
        // Same magnitude rules as numbers; still strict about other text.
        assert_eq!(parse_text(b"1759276800000"), Some(BASE));
        assert_eq!(parse_text(b"99999999999999999999999"), None);
        assert_eq!(parse_text(b"2025-10-01-00-00-00-000"), None);
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
