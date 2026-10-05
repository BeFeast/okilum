//! Display helpers for read-only note properties (#386,
//! docs/design/reader.md §Properties). Parsing lives in
//! `tessera_core::properties`; this module only formats.
use tessera_core::properties::{self, Property, PropertyValue};

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Days since the Unix epoch for a civil date (proleptic Gregorian).
fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "3 Oct 2026, 18:40 · yesterday": absolute first, relative only when near.
pub fn date_label(year: i32, month: u8, day: u8, time: Option<(u8, u8)>, today: i64) -> String {
    let month_name = MONTHS[usize::from(month.clamp(1, 12)) - 1];
    let mut label = format!("{day} {month_name} {year}");
    if let Some((hour, minute)) = time {
        label.push_str(&format!(", {hour:02}:{minute:02}"));
    }
    let delta = today - days_from_civil(year, month, day);
    let relative = match delta {
        0 => Some("today".to_owned()),
        1 => Some("yesterday".to_owned()),
        2..=6 => Some(format!("{delta} days ago")),
        -1 => Some("tomorrow".to_owned()),
        -6..=-2 => Some(format!("in {} days", -delta)),
        _ => None,
    };
    if let Some(relative) = relative {
        label.push_str(" · ");
        label.push_str(&relative);
    }
    label
}

/// Short date for the summary line: "3 Oct".
pub fn short_date(month: u8, day: u8) -> String {
    format!("{day} {}", MONTHS[usize::from(month.clamp(1, 12)) - 1])
}

static LOCAL_OFFSET_SECS: std::sync::OnceLock<i64> = std::sync::OnceLock::new();

/// `time` can only read the local offset while the process is single
/// threaded, so `main` calls this before the app starts its threads.
pub fn init_local_offset() {
    let offset = time::UtcOffset::current_local_offset()
        .map(|o| i64::from(o.whole_seconds()))
        .unwrap_or_default();
    let _ = LOCAL_OFFSET_SECS.set(offset);
}

pub fn today() -> i64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    (secs + LOCAL_OFFSET_SECS.get().copied().unwrap_or_default()).div_euclid(86_400)
}

fn find<'a>(props: &'a [Property], keys: &[&str]) -> Option<&'a PropertyValue> {
    props
        .iter()
        .find(|p| keys.iter().any(|k| p.key.eq_ignore_ascii_case(k)))
        .map(|p| &p.value)
}

fn plain(value: &PropertyValue) -> Option<String> {
    match value {
        PropertyValue::Text(t) | PropertyValue::Number(t) => Some(t.clone()),
        PropertyValue::Link { label, .. } => Some(label.clone()),
        PropertyValue::List(items) => items.first().and_then(plain),
        _ => None,
    }
}

/// Collapsed one-line summary: «Note · Active · updated 3 Oct · 3 relations · #a #b».
pub fn summary(props: &[Property]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(kind) = find(props, &["type", "kind"]).and_then(plain) {
        parts.push(kind);
    }
    if let Some(status) = find(props, &["status", "state"]).and_then(plain) {
        parts.push(status);
    }
    if let Some(PropertyValue::Date { month, day, .. }) = find(
        props,
        &["updated", "modified", "date_modified", "last_modified"],
    ) {
        parts.push(format!("updated {}", short_date(*month, *day)));
    }
    let relations = properties::links(props).len();
    if relations > 0 {
        parts.push(format!(
            "{relations} {}",
            if relations == 1 {
                "relation"
            } else {
                "relations"
            }
        ));
    }
    if let Some(value) = find(props, &["tags", "tag"]) {
        let tags: Vec<String> = match value {
            PropertyValue::List(items) => items.iter().filter_map(plain).take(3).collect(),
            other => plain(other).into_iter().collect(),
        };
        if !tags.is_empty() {
            parts.push(
                tags.iter()
                    .map(|t| format!("#{}", t.trim_start_matches('#')))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
    }
    if parts.is_empty() {
        let shown = props.iter().filter(|p| !p.hidden()).count();
        parts.push(format!(
            "{shown} {}",
            if shown == 1 { "property" } else { "properties" }
        ));
    }
    parts.join(" · ")
}

/// Keys whose values read best as chips (status-like or list-like).
pub fn chip_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "type" | "kind" | "status" | "state" | "tags" | "tag" | "aliases"
    )
}

pub fn status_key(key: &str) -> bool {
    matches!(key.to_ascii_lowercase().as_str(), "status" | "state")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_absolute_with_near_relative() {
        let today = days_from_civil(2026, 10, 4);
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(
            date_label(2026, 10, 3, Some((18, 40)), today),
            "3 Oct 2026, 18:40 · yesterday"
        );
        assert_eq!(
            date_label(2026, 9, 28, None, today),
            "28 Sep 2026 · 6 days ago"
        );
        assert_eq!(date_label(2026, 9, 1, None, today), "1 Sep 2026");
    }

    #[test]
    fn summary_reads_like_the_mockup() {
        let props = properties::parse(
            "type: Note\nstatus: Active\nupdated: 2026-10-03\ntags: [cafe, launch]\n\
             project: \"[[Cafe]]\"\nrelated: [\"[[A]]\", \"[[B]]\"]\n_id: x\n",
        )
        .unwrap();
        assert_eq!(
            summary(&props),
            "Note · Active · updated 3 Oct · 3 relations · #cafe #launch"
        );
        let bare = properties::parse("owner: Anya\n_id: x\n").unwrap();
        assert_eq!(
            summary(&bare),
            "1 property",
            "positive control: hidden keys not counted"
        );
    }
}
