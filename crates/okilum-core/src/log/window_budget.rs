//! Reservations shared by immutable log windows and background readers.
//!
//! Reserve before allocating, retain the reservation with the snapshot, and
//! fail closed when old filters/exports still hold the available budget.
//! Dropping a task handle alone is not proof its worker released a snapshot.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct MemoryBudget(Arc<Pool>);

#[derive(Debug)]
struct Pool {
    limit: usize,
    used: AtomicUsize,
}

/// The owner moves this into its immutable snapshot; sharing that snapshot
/// through Arc does not reserve twice or release early.
#[derive(Debug)]
pub struct Reservation {
    pool: Arc<Pool>,
    bytes: usize,
}

impl MemoryBudget {
    pub fn new(limit: usize) -> Self {
        Self(Arc::new(Pool {
            limit,
            used: AtomicUsize::new(0),
        }))
    }

    pub fn reserve(&self, bytes: usize) -> Option<Reservation> {
        if bytes == 0 || bytes > self.0.limit {
            return None;
        }
        self.0
            .used
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|&next| next <= self.0.limit)
            })
            .ok()?;
        Some(Reservation {
            pool: self.0.clone(),
            bytes,
        })
    }

    pub fn used(&self) -> usize {
        self.0.used.load(Ordering::Acquire)
    }

    pub fn limit(&self) -> usize {
        self.0.limit
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.pool.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
