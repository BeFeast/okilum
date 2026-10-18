//! Time between two records, or from a record to now (#1385), at the
//! precision the log writes: a log with milliseconds gives milliseconds, one
//! with whole seconds gives seconds. No digit is shown that the source did not
//! write.

use super::{timestamp, Record, Role, ValueKind};

/// A record's time: Unix nanoseconds and how many digits of a second the
/// source spells (0–9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub nanos: i64,
    pub digits: u8,
}

impl Stamp {
    pub fn new(nanos: i64, digits: u8) -> Self {
        Self {
            nanos,
            digits: digits.min(9),
        }
    }
}

impl Record {
    /// The time field as a `Stamp`, when the record has a parseable one.
    pub fn stamp(&self) -> Option<Stamp> {
        let field = self.role(Role::Time)?;
        let bytes = field.value.as_bytes();
        let (nanos, digits) = match field.kind {
            ValueKind::String => (
                timestamp::parse_text(bytes)?,
                timestamp::digits_text(bytes)?,
            ),
            ValueKind::Number => (
                timestamp::parse_number(bytes)?,
                timestamp::digits_number(bytes)?,
            ),
            _ => return None,
        };
        Some(Stamp::new(nanos, digits))
    }
}

/// `to` minus `from`, signed: `+1.234 s`, `−2 min 03.5 s`. Shown at the
/// coarser precision of the two, where the difference is exact.
pub fn between(from: Stamp, to: Stamp) -> String {
    let diff = i128::from(to.nanos) - i128::from(from.nanos);
    let digits = from.digits.min(to.digits);
    let text = duration(diff.unsigned_abs(), digits);
    match diff.signum() {
        1 => format!("+{text}"),
        -1 => format!("\u{2212}{text}"),
        _ => text,
    }
}

/// From the record to `now`: `12.345 s ago`, or `in 3 s` for a record time
/// still ahead of this computer's clock. At most milliseconds: finer digits
/// of the clock would only say when the frame was drawn.
pub fn since(stamp: Stamp, now: i64) -> String {
    let diff = i128::from(now) - i128::from(stamp.nanos);
    let text = duration(diff.unsigned_abs(), stamp.digits.min(3));
    if diff < 0 {
        format!("in {text}")
    } else {
        format!("{text} ago")
    }
}

/// `nanos` with `digits` decimals of a second, cut (never rounded up):
/// `4.567 s`, `3 min 04.567 s`, `1 h 02 min 03 s`, `2 d 1 h 02 min 03 s`.
fn duration(nanos: u128, digits: u8) -> String {
    let secs = nanos / 1_000_000_000;
    let unit = 10u128.pow(9 - u32::from(digits));
    let fraction = nanos % 1_000_000_000 / unit;
    let (days, hours, minutes, seconds) =
        (secs / 86_400, secs / 3_600 % 24, secs / 60 % 60, secs % 60);
    let width = if secs >= 60 { 2 } else { 1 };
    let mut second = format!("{seconds:0width$}");
    if digits > 0 {
        second.push_str(&format!(".{fraction:0w$}", w = usize::from(digits)));
    }
    match (days, hours, secs >= 60) {
        (0, 0, false) => format!("{second} s"),
        (0, 0, true) => format!("{minutes} min {second} s"),
        (0, h, _) => format!("{h} h {minutes:02} min {second} s"),
        (d, h, _) => format!("{d} d {h} h {minutes:02} min {second} s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::Format;

    const BASE: i64 = 1_759_276_800_000_000_000; // 2025-10-01T00:00:00Z

    fn stamp(line: &str) -> Stamp {
        Record::parse(line.as_bytes(), Format::JsonLines)
            .unwrap()
            .stamp()
            .unwrap()
    }

    #[test]
    fn precision_of_each_unit() {
        // Seconds, milliseconds, microseconds and nanoseconds, as written.
        let s = (stamp(r#"{"ts":1759276800}"#), stamp(r#"{"ts":1759276805}"#));
        assert_eq!(between(s.0, s.1), "+5 s");
        let ms = (
            stamp(r#"{"time":"2025-10-01T00:00:00.120Z"}"#),
            stamp(r#"{"time":"2025-10-01T00:00:01.354Z"}"#),
        );
        assert_eq!(between(ms.0, ms.1), "+1.234 s");
        let us = (
            stamp(r#"{"ts":1759276800000001}"#),
            stamp(r#"{"ts":1759276800250010}"#),
        );
        assert_eq!(between(us.0, us.1), "+0.250009 s");
        let ns = (
            stamp(r#"{"ts":"1759276800000000001"}"#),
            stamp(r#"{"ts":"1759276800000000250"}"#),
        );
        assert_eq!(between(ns.0, ns.1), "+0.000000249 s");
        // A pair of different precisions: the coarser one, exact there.
        assert_eq!(between(s.0, ms.1), "+1 s");
        assert_eq!(between(ms.1, ms.0), "\u{2212}1.234 s");
        assert_eq!(between(ms.0, ms.0), "0.000 s");
    }

    #[test]
    fn long_differences_read_as_minutes_hours_and_days() {
        let at = |nanos| Stamp::new(nanos, 3);
        let s = 1_000_000_000i64;
        assert_eq!(
            between(at(BASE), at(BASE + 184 * s + 567_000_000)),
            "+3 min 04.567 s"
        );
        assert_eq!(
            between(at(BASE), at(BASE + 3_723 * s)),
            "+1 h 02 min 03.000 s"
        );
        assert_eq!(
            between(
                Stamp::new(BASE, 0),
                Stamp::new(BASE + 2 * 86_400 * s + 3_723 * s, 0)
            ),
            "+2 d 1 h 02 min 03 s"
        );
    }

    #[test]
    fn since_counts_to_now_and_cuts_at_milliseconds() {
        let ns = Stamp::new(BASE, 9);
        assert_eq!(since(ns, BASE + 12_345_678_901), "12.345 s ago");
        assert_eq!(
            since(Stamp::new(BASE, 0), BASE + 12_999_999_999),
            "12 s ago"
        );
        assert_eq!(
            since(Stamp::new(BASE, 3), BASE - 3_000_000_000),
            "in 3.000 s"
        );
    }

    #[test]
    fn a_record_without_a_time_field_has_no_stamp() {
        let record = Record::parse(br#"{"msg":"no time here"}"#, Format::JsonLines).unwrap();
        assert_eq!(record.stamp(), None);
        // Syslog writes no year: not a timestamp, so no stamp either.
        let syslog = Record::parse(b"Oct  1 00:00:00 host app[1]: hello", Format::Syslog);
        assert_eq!(syslog.and_then(|r| r.stamp()), None);
    }
}
