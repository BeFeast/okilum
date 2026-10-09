//! Notification policy for reminders (#724) as a pure function of the current
//! reminders, a durable ledger and the local time. No clock, no IO, no OS.
//!
//! Policy (manager decision, 2026-10-08):
//! - a date-only reminder notifies at `notify_at` (default 09:00) on its day;
//! - a reminder first seen for today after `notify_at` does not notify, it
//!   shows in Today only;
//! - everything that becomes due in one evaluation is one notification, never a
//!   batch; missed (overdue) reminders are counted, not listed one by one;
//! - nothing notifies during quiet hours (default 21:00-09:00): it waits and is
//!   combined into the first notification after they end.
//!
//! The ledger is delivery bookkeeping, not note content: it holds opaque
//! reminder ids and times, lives outside the disposable index, and a lost or
//! unreadable ledger is rebuilt silently (the next evaluation re-baselines)
//! rather than replaying old reminders.
use crate::tasks::Task;
use anyhow::{ensure, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::LazyLock;
use time::{Date, Duration, PrimitiveDateTime, Time};

const LEDGER_VERSION: u32 = 1;
/// Entries for reminders that are no longer in the note are kept this long, so a
/// transient gap (index rebuild, sync in progress) cannot replay old reminders.
const RETAIN_ABSENT_DAYS: i64 = 90;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub notify_at: Time,
    pub quiet_from: Time,
    pub quiet_until: Time,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            notify_at: Time::from_hms(9, 0, 0).unwrap(),
            quiet_from: Time::from_hms(21, 0, 0).unwrap(),
            quiet_until: Time::from_hms(9, 0, 0).unwrap(),
        }
    }
}

impl Policy {
    pub fn is_quiet(&self, at: Time) -> bool {
        match self.quiet_from.cmp(&self.quiet_until) {
            std::cmp::Ordering::Equal => false,
            std::cmp::Ordering::Less => self.quiet_from <= at && at < self.quiet_until,
            std::cmp::Ordering::Greater => at >= self.quiet_from || at < self.quiet_until,
        }
    }

    /// The first moment at or after `at` that is outside quiet hours.
    fn after_quiet(&self, at: PrimitiveDateTime) -> PrimitiveDateTime {
        if !self.is_quiet(at.time()) {
            return at;
        }
        let today = at.date().with_time(self.quiet_until);
        if today > at {
            today
        } else {
            today + Duration::days(1)
        }
    }
}

/// Tasks metadata markers: the visible text ends where the first one starts.
const METADATA: [char; 11] = [
    '📅', '⏳', '🛫', '✅', '❌', '➕', '⏫', '🔼', '🔽', '⏬', '🔺',
];
static WIKI_ALIAS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[[^\]|]*\|([^\]]*)\]\]").unwrap());
static WIKI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[\[([^\]]*)\]\]").unwrap());
static TRAILING_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*\[\[[^\]]*\]\]\s*$").unwrap());

/// What a notification says: the task's own words, without the Tasks metadata,
/// the trailing backlink the reminder writer adds, or Markdown escapes.
fn spoken(raw: &str) -> String {
    let head = raw.split(METADATA).next().unwrap_or(raw);
    let head = TRAILING_LINK.replace(head, "");
    let head = WIKI_ALIAS.replace_all(&head, "$1");
    let head = WIKI.replace_all(&head, "$1");
    let mut out = String::new();
    let mut chars = head.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            c => out.push(c),
        }
    }
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.is_empty() {
        raw.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        out
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reminder {
    pub id: String,
    pub text: String,
    pub due: Date,
}

/// Unchecked tasks with a due date. The id covers note, text and date, so a
/// rescheduled or reworded reminder is a new one and a moved line is the same.
pub fn reminders(tasks: &[Task]) -> Vec<Reminder> {
    let mut seen = std::collections::BTreeSet::new();
    tasks
        .iter()
        .filter(|task| !task.checked)
        .filter_map(|task| {
            let due = task.due?;
            let mut hash = Sha256::new();
            for part in [task.path.as_str(), task.text.as_str(), &due.to_string()] {
                hash.update(part.as_bytes());
                hash.update([0]);
            }
            let id: String = hash
                .finalize()
                .iter()
                .take(8)
                .map(|b| format!("{b:02x}"))
                .collect();
            seen.insert(id.clone()).then(|| Reminder {
                id,
                text: spoken(task.display.as_deref().unwrap_or(&task.text)),
                due,
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum State {
    Pending,
    Delivered,
    /// Never notifies: part of the first import, or created after its time.
    Silent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    /// Local wall-clock seconds when first seen, used only for retention.
    seen: i64,
    state: State,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    version: u32,
    baselined: bool,
    entries: BTreeMap<String, Entry>,
}

impl Default for Ledger {
    fn default() -> Self {
        Self {
            version: LEDGER_VERSION,
            baselined: false,
            entries: BTreeMap::new(),
        }
    }
}

impl Ledger {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("ledger is plain data")
    }

    pub fn from_json(json: &str) -> Result<Self> {
        let ledger: Self = serde_json::from_str(json)?;
        ensure!(
            ledger.version == LEDGER_VERSION,
            "Unsupported reminder ledger"
        );
        Ok(ledger)
    }
}

/// One notification for everything that became due together.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub today: Vec<Reminder>,
    pub overdue: Vec<Reminder>,
}

impl Notice {
    pub fn title(&self) -> String {
        match (self.overdue.len(), self.today.len()) {
            (0, 0) => String::new(),
            (0, 1) => self.today[0].text.clone(),
            (0, n) => format!("{n} reminders today"),
            (1, 0) => "1 task overdue".into(),
            (n, 0) => format!("{n} tasks overdue"),
            (o, t) => format!("{} reminders need attention", o + t),
        }
    }

    pub fn body(&self) -> String {
        match (self.overdue.len(), self.today.len()) {
            (0, 1) => "Due today".into(),
            (o, t) if o > 0 && t > 0 => format!("{o} overdue, {t} today"),
            _ => {
                let all: Vec<_> = self.today.iter().chain(&self.overdue).collect();
                match all.as_slice() {
                    [] => String::new(),
                    [only] => only.text.clone(),
                    [first, rest @ ..] => format!("{} and {} more", first.text, rest.len()),
                }
            }
        }
    }
}

fn seconds(at: PrimitiveDateTime) -> i64 {
    at.assume_utc().unix_timestamp()
}

/// Decide what to notify now and update the ledger. Returns at most one notice;
/// call it at start-up, on wake-ups from `next_wake`, and when the note changes.
pub fn evaluate(
    reminders: &[Reminder],
    ledger: &mut Ledger,
    now: PrimitiveDateTime,
    policy: &Policy,
) -> Option<Notice> {
    let first_run = !ledger.baselined;
    ledger.baselined = true;
    for reminder in reminders {
        if ledger.entries.contains_key(&reminder.id) {
            continue;
        }
        let state = if first_run || (reminder.due == now.date() && now.time() >= policy.notify_at) {
            State::Silent
        } else {
            State::Pending
        };
        ledger.entries.insert(
            reminder.id.clone(),
            Entry {
                seen: seconds(now),
                state,
            },
        );
    }
    let horizon = seconds(now) - RETAIN_ABSENT_DAYS * 86_400;
    ledger.entries.retain(|id, entry| {
        entry.seen >= horizon || reminders.iter().any(|reminder| &reminder.id == id)
    });
    if policy.is_quiet(now.time()) {
        return None;
    }
    let mut notice = Notice {
        today: vec![],
        overdue: vec![],
    };
    for reminder in reminders {
        let Some(entry) = ledger.entries.get_mut(&reminder.id) else {
            continue;
        };
        if entry.state != State::Pending || reminder.due.with_time(policy.notify_at) > now {
            continue;
        }
        entry.state = State::Delivered;
        if reminder.due == now.date() {
            notice.today.push(reminder.clone());
        } else {
            notice.overdue.push(reminder.clone());
        }
    }
    (!notice.today.is_empty() || !notice.overdue.is_empty()).then_some(notice)
}

/// When to evaluate next: the earliest pending reminder, moved out of quiet
/// hours. `None` when nothing is waiting. Call after `evaluate`.
pub fn next_wake(
    reminders: &[Reminder],
    ledger: &Ledger,
    now: PrimitiveDateTime,
    policy: &Policy,
) -> Option<PrimitiveDateTime> {
    reminders
        .iter()
        .filter(|reminder| {
            ledger
                .entries
                .get(&reminder.id)
                .is_none_or(|entry| entry.state == State::Pending)
        })
        .map(|reminder| {
            let at = reminder.due.with_time(policy.notify_at).max(now);
            policy.after_quiet(at)
        })
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::{date, datetime};

    fn r(text: &str, due: Date) -> Reminder {
        Reminder {
            id: format!("{text}|{due}"),
            text: text.into(),
            due,
        }
    }

    /// A ledger that has already done its silent first import.
    fn started(policy: &Policy) -> Ledger {
        let mut ledger = Ledger::default();
        assert!(evaluate(&[], &mut ledger, datetime!(2026-10-01 12:00), policy).is_none());
        ledger
    }

    #[test]
    fn first_run_imports_everything_silently_and_stays_silent() {
        let policy = Policy::default();
        let mut ledger = Ledger::default();
        let old = [
            r("old overdue", date!(2026 - 09 - 01)),
            r("today", date!(2026 - 10 - 09)),
            r("tomorrow", date!(2026 - 10 - 10)),
        ];
        // Positive control for the silence: the same data after the first run
        // is a normal reminder set, and a later evaluation still says nothing
        // for the imported items.
        assert!(evaluate(&old, &mut ledger, datetime!(2026-10-09 10:00), &policy).is_none());
        assert!(evaluate(&old, &mut ledger, datetime!(2026-10-10 09:30), &policy).is_none());
        let fresh = [r("added later", date!(2026 - 10 - 11))];
        assert!(evaluate(&fresh, &mut ledger, datetime!(2026-10-10 09:31), &policy).is_none());
        let notice = evaluate(&fresh, &mut ledger, datetime!(2026-10-11 09:00), &policy);
        assert_eq!(notice.unwrap().title(), "added later");
    }

    #[test]
    fn notifies_once_at_the_configured_time_and_not_before() {
        let policy = Policy::default();
        let mut ledger = started(&policy);
        let list = [r("Pay rent", date!(2026 - 11 - 01))];
        assert!(evaluate(&list, &mut ledger, datetime!(2026-10-31 12:00), &policy).is_none());
        assert!(evaluate(&list, &mut ledger, datetime!(2026-11-01 08:59), &policy).is_none());
        let notice = evaluate(&list, &mut ledger, datetime!(2026-11-01 09:00), &policy).unwrap();
        assert_eq!(
            (notice.title(), notice.body()),
            ("Pay rent".into(), "Due today".into())
        );
        assert!(evaluate(&list, &mut ledger, datetime!(2026-11-01 09:01), &policy).is_none());
        assert!(evaluate(&list, &mut ledger, datetime!(2026-11-02 09:00), &policy).is_none());
    }

    #[test]
    fn a_reminder_created_for_today_after_the_time_is_silent() {
        let policy = Policy::default();
        let mut ledger = started(&policy);
        let list = [r("late add", date!(2026 - 10 - 09))];
        assert!(evaluate(&list, &mut ledger, datetime!(2026-10-09 10:30), &policy).is_none());
        assert!(evaluate(&list, &mut ledger, datetime!(2026-10-10 09:00), &policy).is_none());
        // Same day but before the time still fires at the time.
        let mut ledger = started(&policy);
        let early = [r("early add", date!(2026 - 10 - 09))];
        assert!(evaluate(&early, &mut ledger, datetime!(2026-10-09 08:00), &policy).is_none());
        assert!(evaluate(&early, &mut ledger, datetime!(2026-10-09 09:00), &policy).is_some());
    }

    #[test]
    fn missed_reminders_are_one_summary_not_a_batch() {
        let policy = Policy::default();
        let mut ledger = started(&policy);
        let list = [
            r("a", date!(2026 - 10 - 05)),
            r("b", date!(2026 - 10 - 06)),
            r("c", date!(2026 - 10 - 07)),
        ];
        assert!(evaluate(&list, &mut ledger, datetime!(2026-10-02 12:00), &policy).is_none());
        let notice = evaluate(&list, &mut ledger, datetime!(2026-10-09 10:00), &policy).unwrap();
        assert_eq!(notice.title(), "3 tasks overdue");
        assert_eq!(notice.body(), "a and 2 more");
        assert!(evaluate(&list, &mut ledger, datetime!(2026-10-09 10:05), &policy).is_none());

        let mut ledger = started(&policy);
        let mixed = [
            r("old", date!(2026 - 10 - 05)),
            r("now", date!(2026 - 10 - 09)),
        ];
        assert!(evaluate(&mixed, &mut ledger, datetime!(2026-10-02 12:00), &policy).is_none());
        let notice = evaluate(&mixed, &mut ledger, datetime!(2026-10-09 09:00), &policy).unwrap();
        assert_eq!(notice.title(), "2 reminders need attention");
        assert_eq!(notice.body(), "1 overdue, 1 today");
        let one = Notice {
            today: vec![],
            overdue: vec![r("only", date!(2026 - 10 - 05))],
        };
        assert_eq!(
            (one.title(), one.body()),
            ("1 task overdue".into(), "only".into())
        );
    }

    #[test]
    fn quiet_hours_defer_and_combine_never_drop() {
        let policy = Policy {
            notify_at: Time::from_hms(7, 0, 0).unwrap(),
            ..Policy::default()
        };
        let mut ledger = started(&policy);
        let list = [
            r("early bird", date!(2026 - 10 - 10)),
            r("missed", date!(2026 - 10 - 08)),
        ];
        assert!(evaluate(&list, &mut ledger, datetime!(2026-10-07 12:00), &policy).is_none());
        // 07:00 is inside 21:00-09:00: nothing, and the wake-up moves to 09:00.
        assert!(evaluate(&list, &mut ledger, datetime!(2026-10-10 07:00), &policy).is_none());
        assert_eq!(
            next_wake(&list, &ledger, datetime!(2026-10-10 07:00), &policy),
            Some(datetime!(2026-10-10 09:00))
        );
        // Launching at night changes nothing until the morning.
        assert!(evaluate(&list, &mut ledger, datetime!(2026-10-10 23:30), &policy).is_none());
        let notice = evaluate(&list, &mut ledger, datetime!(2026-10-11 09:00), &policy).unwrap();
        assert_eq!(notice.title(), "2 tasks overdue");
    }

    #[test]
    fn quiet_window_edges_and_degenerate_bounds() {
        let p = Policy::default();
        let t = |h, m| Time::from_hms(h, m, 0).unwrap();
        assert!(p.is_quiet(t(21, 0)) && p.is_quiet(t(0, 0)) && p.is_quiet(t(8, 59)));
        assert!(!p.is_quiet(t(9, 0)) && !p.is_quiet(t(20, 59)));
        let same = Policy {
            quiet_from: t(9, 0),
            quiet_until: t(9, 0),
            ..p
        };
        assert!(!same.is_quiet(t(3, 0)));
        let daytime = Policy {
            quiet_from: t(13, 0),
            quiet_until: t(14, 0),
            ..p
        };
        assert!(
            daytime.is_quiet(t(13, 30))
                && !daytime.is_quiet(t(14, 0))
                && !daytime.is_quiet(t(2, 0))
        );
    }

    #[test]
    fn completed_removed_and_rescheduled_reminders_are_cancelled() {
        let policy = Policy::default();
        let mut ledger = started(&policy);
        let original = [
            r("task", date!(2026 - 10 - 20)),
            r("kept", date!(2026 - 10 - 20)),
        ];
        assert!(evaluate(&original, &mut ledger, datetime!(2026-10-10 12:00), &policy).is_none());
        // "task" was checked off (no longer an unchecked reminder) and "kept"
        // was rescheduled: only the new date counts.
        let now = [r("kept", date!(2026 - 10 - 22))];
        assert!(evaluate(&now, &mut ledger, datetime!(2026-10-20 09:00), &policy).is_none());
        let notice = evaluate(&now, &mut ledger, datetime!(2026-10-22 09:00), &policy).unwrap();
        assert_eq!(notice.title(), "kept");
        // Rescheduled to today after the time: silent, like any late add.
        let again = [r("kept", date!(2026 - 10 - 23))];
        assert!(evaluate(&again, &mut ledger, datetime!(2026-10-23 11:00), &policy).is_none());
    }

    #[test]
    fn a_transient_gap_does_not_replay_delivered_reminders() {
        let policy = Policy::default();
        let mut ledger = started(&policy);
        let list = [r("once", date!(2026 - 10 - 10))];
        assert!(evaluate(&list, &mut ledger, datetime!(2026-10-09 12:00), &policy).is_none());
        assert!(evaluate(&list, &mut ledger, datetime!(2026-10-10 09:00), &policy).is_some());
        // The index is rebuilding and the note momentarily yields nothing.
        assert!(evaluate(&[], &mut ledger, datetime!(2026-10-10 09:05), &policy).is_none());
        assert!(evaluate(&list, &mut ledger, datetime!(2026-10-10 09:10), &policy).is_none());
        // Long-absent entries are eventually forgotten.
        assert!(evaluate(&[], &mut ledger, datetime!(2027-02-01 12:00), &policy).is_none());
        assert!(ledger.entries.is_empty());
    }

    #[test]
    fn ledger_round_trips_and_refuses_foreign_data() {
        let policy = Policy::default();
        let mut ledger = started(&policy);
        let list = [r("x", date!(2026 - 10 - 12))];
        evaluate(&list, &mut ledger, datetime!(2026-10-10 12:00), &policy);
        let json = ledger.to_json();
        assert_eq!(Ledger::from_json(&json).unwrap(), ledger);
        assert!(!json.contains("task text"), "ids and times only");
        assert!(Ledger::from_json("not json").is_err());
        assert!(Ledger::from_json(&json.replace("\"version\":1", "\"version\":9")).is_err());
    }

    #[test]
    fn next_wake_picks_the_earliest_pending_and_skips_done_ones() {
        let policy = Policy::default();
        let mut ledger = started(&policy);
        let list = [
            r("later", date!(2026 - 10 - 20)),
            r("sooner", date!(2026 - 10 - 15)),
        ];
        let now = datetime!(2026-10-10 12:00);
        evaluate(&list, &mut ledger, now, &policy);
        assert_eq!(
            next_wake(&list, &ledger, now, &policy),
            Some(datetime!(2026-10-15 09:00))
        );
        evaluate(&list, &mut ledger, datetime!(2026-10-15 09:00), &policy);
        assert_eq!(
            next_wake(&list, &ledger, datetime!(2026-10-15 09:00), &policy),
            Some(datetime!(2026-10-20 09:00))
        );
        assert_eq!(next_wake(&[], &ledger, now, &policy), None);
    }

    #[test]
    fn an_empty_notice_has_no_text_instead_of_panicking() {
        let empty = Notice {
            today: vec![],
            overdue: vec![],
        };
        assert_eq!(
            (empty.title(), empty.body()),
            (String::new(), String::new())
        );
    }

    #[test]
    fn notification_text_is_the_task_words_only() {
        let line = crate::reminder_task::format(
            "Review [x](evil) *a* & more",
            "Notes/Start.md",
            Some("Winter time"),
            date!(2026 - 11 - 01),
        )
        .unwrap();
        let task = crate::tasks::parse("Reminders.md", &line);
        assert_eq!(reminders(&task)[0].text, "Review [x](evil) *a* & more");
        for (raw, spoken_text) in [
            (
                "Call [[People/Dana|Dana]] now 📅 2026-11-02",
                "Call Dana now",
            ),
            ("Pay rent [[Bills.md#May]] 📅 2026-11-02", "Pay rent"),
            ("Plain ⏫ 📅 2026-11-02", "Plain"),
            ("📅 2026-11-02", "📅 2026-11-02"),
        ] {
            assert_eq!(spoken(raw), spoken_text, "{raw}");
        }
    }

    #[test]
    fn reminders_from_tasks_ignore_checked_undated_and_duplicates() {
        let source = "- [ ] a 📅 2026-11-01\n- [x] b 📅 2026-11-01\n- [ ] c\n- [ ] a 📅 2026-11-01\n- [ ] a 📅 2026-11-02\n";
        let list = reminders(&crate::tasks::parse("Reminders.md", source));
        assert_eq!(list.len(), 2);
        assert_ne!(list[0].id, list[1].id);
        // Moving the line does not change the id.
        let moved = reminders(&crate::tasks::parse(
            "Reminders.md",
            &format!("\n\n{source}"),
        ));
        assert_eq!(moved[0].id, list[0].id);
        // The same text in another note is a different reminder.
        let other = reminders(&crate::tasks::parse("Other.md", source));
        assert_ne!(other[0].id, list[0].id);
    }
}
