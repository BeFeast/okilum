//! Revocation shared with blocking work, before its result reaches the UI.
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

#[derive(Default)]
pub(super) struct Epoch(Arc<AtomicU64>);

#[derive(Clone)]
pub(super) struct Ticket {
    epoch: Arc<AtomicU64>,
    value: u64,
}

impl Epoch {
    pub(super) fn invalidate(&self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }

    pub(super) fn begin(&self) -> Ticket {
        self.invalidate();
        self.ticket()
    }

    pub(super) fn ticket(&self) -> Ticket {
        Ticket {
            epoch: self.0.clone(),
            value: self.0.load(Ordering::Acquire),
        }
    }

    #[cfg(test)]
    pub(super) fn current(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }
}

impl Ticket {
    pub(super) fn is_current(&self) -> bool {
        self.epoch.load(Ordering::Acquire) == self.value
    }

    pub(super) fn check(&self) -> Result<(), Superseded> {
        if self.is_current() {
            Ok(())
        } else {
            Err(Superseded)
        }
    }

    pub(super) fn run<T>(&self, work: impl FnOnce() -> T) -> Result<T, Superseded> {
        self.check()?;
        Ok(work())
    }
}

#[derive(Debug)]
pub(super) struct Superseded;
impl std::fmt::Display for Superseded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sync operation was superseded")
    }
}
impl std::error::Error for Superseded {}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use std::cell::Cell;

    #[test]
    fn queued_work_is_revoked_before_effects_and_reenable_does_not_revive_it(
    ) -> Result<(), Superseded> {
        let epoch = Epoch::default();
        let old = epoch.begin();
        let effects = Cell::new(0);
        old.run(|| {
            effects.set(effects.get() + 1);
        })?;
        assert_eq!(effects.get(), 1, "positive control: admitted work executes");
        epoch.invalidate(); // suspend, before the queued blocking worker starts
        let fresh = epoch.begin(); // re-enable before that old worker is scheduled
        assert!(old
            .run(|| {
                effects.set(effects.get() + 1);
            })
            .is_err());
        assert_eq!(
            effects.get(),
            1,
            "stale work must perform no discovery or IO"
        );
        fresh.run(|| {
            effects.set(effects.get() + 1);
        })?;
        assert_eq!(effects.get(), 2);
        assert!(!old.is_current());
        Ok(())
    }

    #[test]
    fn a_ticket_already_on_another_worker_observes_suspend_before_io() {
        let epoch = Epoch::default();
        let ticket = epoch.begin();
        let rendezvous = std::sync::Barrier::new(2);
        let effects = AtomicU64::new(0);
        std::thread::scope(|threads| {
            threads.spawn(|| {
                rendezvous.wait();
                rendezvous.wait();
                assert!(ticket
                    .run(|| effects.fetch_add(1, Ordering::AcqRel))
                    .is_err());
            });
            rendezvous.wait(); // the old work has reached the blocking executor
            epoch.invalidate();
            let fresh = epoch.begin();
            assert!(fresh.is_current());
            rendezvous.wait();
        });
        assert_eq!(effects.load(Ordering::Acquire), 0);
        assert!(!ticket.is_current());
        assert_eq!(epoch.current(), 3);
    }
}
