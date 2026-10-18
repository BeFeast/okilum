#[path = "../src/log/window_budget.rs"]
mod window_budget;

use std::sync::{Arc, Barrier};
use window_budget::MemoryBudget;

#[test]
fn retained_background_snapshots_cannot_bypass_the_budget() {
    let pool = MemoryBudget::new(96);
    let snapshot = Arc::new(pool.reserve(64).unwrap());
    let exporter = snapshot.clone();
    let filter = snapshot.clone();
    assert_eq!(pool.used(), 64);
    drop(snapshot);
    assert!(
        pool.reserve(64).is_none(),
        "negative control: an old worker really retains its budget"
    );
    let next = pool.reserve(32).unwrap();
    assert_eq!(pool.used(), pool.limit());
    assert!(pool.reserve(1).is_none());
    drop(exporter);
    drop(filter);
    assert_eq!(pool.used(), 32);
    drop(next);
    assert_eq!(pool.used(), 0);
    assert!(pool.reserve(96).is_some());
    assert!(pool.reserve(usize::MAX).is_none());
    assert!(pool.reserve(0).is_none());
}

#[test]
fn competing_workers_never_overbook_or_double_release() {
    let pool = MemoryBudget::new(64);
    let ready = Arc::new(Barrier::new(17));
    let release = Arc::new(Barrier::new(17));
    std::thread::scope(|scope| {
        for _ in 0..16 {
            let (pool, ready, release) = (pool.clone(), ready.clone(), release.clone());
            scope.spawn(move || {
                let held = pool.reserve(16);
                ready.wait();
                assert_eq!(pool.used(), 64);
                release.wait();
                drop(held);
            });
        }
        ready.wait();
        assert_eq!(pool.used(), 64);
        assert!(pool.reserve(1).is_none());
        release.wait();
    });
    assert_eq!(pool.used(), 0);
}
