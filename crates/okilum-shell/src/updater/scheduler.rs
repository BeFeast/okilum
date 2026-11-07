//! Automatic discovery policy, independent of the executor and wall clock.
use std::time::Duration;

pub(super) const MIN_INTERVAL: Duration = Duration::from_secs(28 * 60);
pub(super) const MAX_INTERVAL: Duration = Duration::from_secs(30 * 60);
const THROTTLE: Duration = Duration::from_secs(5 * 60);

/// Spread requests over two minutes without exceeding the discovery deadline.
pub(super) fn interval(entropy: u64) -> Duration {
    MIN_INTERVAL
        + Duration::from_secs(entropy % (MAX_INTERVAL - MIN_INTERVAL).as_secs().saturating_add(1))
}

#[derive(Clone, Copy)]
pub(super) enum Trigger {
    Interval,
    Focus,
    /// Explicit checks/channel changes retain their existing immediate behavior.
    Manual,
    Download,
}

impl Trigger {
    pub(super) fn manual(self) -> bool {
        matches!(self, Self::Manual | Self::Download)
    }
}

pub(super) struct Schedule {
    last_started: Option<Duration>,
    next_due: Duration,
}

impl Schedule {
    pub(super) fn new(now: Duration, interval: Duration) -> Self {
        Self {
            last_started: None,
            next_due: now + interval,
        }
    }

    /// `now` is an injected monotonic clock. `start` atomically claims the
    /// platform's single-flight guard and returns false if it is busy. A
    /// rejected attempt must not postpone a due check or erase its last error.
    /// `enabled` is the policy gate; callers allow explicit actions in Off mode.
    pub(super) fn run(
        &mut self,
        now: Duration,
        trigger: Trigger,
        enabled: bool,
        interval: Duration,
        start: impl FnOnce() -> bool,
    ) -> bool {
        if !enabled {
            return false;
        }
        if !trigger.manual()
            && (self
                .last_started
                .is_some_and(|last| now.saturating_sub(last) < THROTTLE)
                || (matches!(trigger, Trigger::Interval) && now < self.next_due))
        {
            return false;
        }
        if !start() {
            return false;
        }
        self.last_started = Some(now);
        self.next_due = now + interval;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use std::cell::Cell;

    const ZERO: Duration = Duration::ZERO;
    const PERIOD: Duration = Duration::from_secs(30 * 60);

    #[test]
    fn intervals_cover_both_jitter_bounds_and_stay_near_thirty_minutes() {
        assert_eq!(interval(0), MIN_INTERVAL);
        assert_eq!(interval(120), MAX_INTERVAL);
        for entropy in (0..1000).chain([u64::MAX]) {
            assert!((MIN_INTERVAL..=MAX_INTERVAL).contains(&interval(entropy)));
        }
        for period in [MIN_INTERVAL, PERIOD, MAX_INTERVAL] {
            let mut schedule = Schedule::new(ZERO, period);
            assert!(!schedule.run(
                period - Duration::from_nanos(1),
                Trigger::Interval,
                true,
                period,
                || panic!("early check")
            ));
            assert!(schedule.run(period, Trigger::Interval, true, period, || true));
            assert!(!schedule.run(
                period + period - Duration::from_nanos(1),
                Trigger::Interval,
                true,
                period,
                || panic!("early recheck")
            ));
            assert!(schedule.run(period + period, Trigger::Interval, true, period, || true));
        }
    }

    #[test]
    fn every_entropy_allows_automatic_discovery_within_thirty_minutes() {
        let deadline = Duration::from_secs(30 * 60);
        // Includes values that selected delays beyond thirty minutes before
        // the deadline was bounded, plus the full random input's upper edge.
        for entropy in (0..=240).chain([u64::MAX]) {
            let period = interval(entropy);
            let mut schedule = Schedule::new(ZERO, period);
            assert!(schedule.run(ZERO, Trigger::Manual, true, period, || true));
            assert!(
                schedule.run(deadline, Trigger::Interval, true, period, || true),
                "automatic discovery missed the deadline for entropy {entropy}"
            );
        }
    }

    #[test]
    fn focus_is_immediate_but_all_automatic_triggers_share_the_five_minute_throttle() {
        let mut schedule = Schedule::new(ZERO, PERIOD);
        assert!(schedule.run(ZERO, Trigger::Manual, true, PERIOD, || true));
        assert!(!schedule.run(
            THROTTLE - Duration::from_nanos(1),
            Trigger::Focus,
            true,
            PERIOD,
            || panic!("focus storm")
        ));
        assert!(schedule.run(THROTTLE, Trigger::Focus, true, PERIOD, || true));
        assert!(
            !schedule.run(THROTTLE, Trigger::Interval, true, PERIOD, || panic!(
                "duplicate tick"
            ))
        );
        assert!(
            !schedule.run(THROTTLE, Trigger::Focus, true, PERIOD, || panic!(
                "second window"
            ))
        );
        assert!(
            !schedule.run(PERIOD, Trigger::Interval, true, PERIOD, || panic!(
                "focus reset deadline"
            ))
        );
        assert!(schedule.run(PERIOD + THROTTLE, Trigger::Interval, true, PERIOD, || true));
    }

    #[test]
    fn long_check_cannot_double_start_and_busy_does_not_consume_a_due_tick() {
        let mut schedule = Schedule::new(ZERO, PERIOD);
        let busy = Cell::new(false);
        let starts = Cell::new(0);
        let start = || {
            if busy.replace(true) {
                return false;
            }
            starts.set(starts.get() + 1);
            true
        };
        assert!(schedule.run(ZERO, Trigger::Manual, true, PERIOD, start));
        assert!(!schedule.run(PERIOD, Trigger::Interval, true, PERIOD, start));
        assert!(!schedule.run(PERIOD, Trigger::Focus, true, PERIOD, start));
        assert!(!schedule.run(PERIOD, Trigger::Manual, true, PERIOD, start));
        assert_eq!(starts.get(), 1);
        busy.set(false); // Failed or successful completion releases the guard.
        assert!(schedule.run(PERIOD, Trigger::Interval, true, PERIOD, start));
        assert_eq!(starts.get(), 2);
    }

    #[test]
    fn failures_retry_on_next_scheduled_tick_and_mode_gate_starts_nothing() {
        let mut schedule = Schedule::new(ZERO, PERIOD);
        assert!(schedule.run(ZERO, Trigger::Manual, true, PERIOD, || true));
        // An accepted check fails; there is no success-only rescheduling/backoff.
        assert!(
            !schedule.run(PERIOD, Trigger::Interval, false, PERIOD, || panic!(
                "disabled"
            ))
        );
        assert!(schedule.run(PERIOD, Trigger::Interval, true, PERIOD, || true));
        assert!(schedule.run(PERIOD, Trigger::Manual, true, PERIOD, || true));
        assert!(
            !schedule.run(PERIOD, Trigger::Focus, true, PERIOD, || panic!(
                "manual also throttles focus"
            ))
        );
    }
}
