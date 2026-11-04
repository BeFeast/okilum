//! Test-only foreground allocation accounting. This observes the old String
//! path too, rather than trusting annotations on the replacement path.
use crate::NATIVE_ALLOCATION;
use std::alloc::{GlobalAlloc, Layout};
use std::cell::Cell;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Work {
    pub bytes: usize,
    pub largest: usize,
    pub frees: usize,
    pub inspected: usize,
    pub nanos: u128,
}
thread_local! {
    static WORK: Cell<Option<Work>> = const { Cell::new(None) };
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}
// A whole App update includes native input and GPUI test-executor bookkeeping.
// Scope app editor/persistence entry points to match this issue's layer. The
// same scopes are overlaid on origin/main; actual allocations are intercepted.
pub(crate) struct Scope(std::time::Instant);
pub(crate) fn scope() -> Scope {
    DEPTH.with(|d| d.set(d.get() + 1));
    Scope(std::time::Instant::now())
}
impl Drop for Scope {
    fn drop(&mut self) {
        let outermost = DEPTH.with(|d| {
            d.set(d.get() - 1);
            d.get() == 0
        });
        if outermost {
            WORK.with(|slot| {
                if let Some(mut work) = slot.get() {
                    work.nanos += self.0.elapsed().as_nanos();
                    slot.set(Some(work));
                }
            });
        }
    }
}
pub(crate) fn inspect(bytes: usize) {
    if DEPTH.with(|d| d.get()) == 0 {
        return;
    }
    WORK.with(|slot| {
        if let Some(mut work) = slot.get() {
            work.inspected += bytes;
            slot.set(Some(work));
        }
    });
}
struct Meter;
#[global_allocator]
static ALLOCATOR: Meter = Meter;
fn charge(bytes: usize) {
    if !DEPTH.try_with(|d| d.get() > 0).unwrap_or(false) {
        return;
    }
    let _ = WORK.try_with(|slot| {
        if let Some(mut work) = slot.get() {
            work.bytes += bytes;
            work.largest = work.largest.max(bytes);
            slot.set(Some(work));
        }
    });
}
unsafe impl GlobalAlloc for Meter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        charge(layout.size());
        unsafe { NATIVE_ALLOCATION.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        charge(layout.size());
        unsafe { NATIVE_ALLOCATION.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if DEPTH.try_with(|d| d.get() > 0).unwrap_or(false) {
            let _ = WORK.try_with(|slot| {
                if let Some(mut work) = slot.get() {
                    work.frees += 1;
                    slot.set(Some(work));
                }
            });
        }
        unsafe { NATIVE_ALLOCATION.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        charge(size);
        unsafe { NATIVE_ALLOCATION.realloc(ptr, layout, size) }
    }
}
pub(crate) fn reset() {
    WORK.with(|s| s.set(Some(Work::default())));
}
pub(crate) fn take() -> Work {
    WORK.with(|s| s.replace(None).unwrap())
}
