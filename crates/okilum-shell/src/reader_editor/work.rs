//! Test-only foreground allocation accounting. This observes the old String
//! path too, rather than trusting annotations on the replacement path.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Work {
    pub bytes: usize,
    pub largest: usize,
    pub frees: usize,
}
thread_local! { static WORK: Cell<Option<Work>> = const { Cell::new(None) }; }
struct Meter;
#[global_allocator]
static ALLOCATOR: Meter = Meter;
fn charge(bytes: usize) {
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
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        charge(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let _ = WORK.try_with(|slot| {
            if let Some(mut work) = slot.get() {
                work.frees += 1;
                slot.set(Some(work));
            }
        });
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        charge(size);
        unsafe { System.realloc(ptr, layout, size) }
    }
}
pub(super) fn reset() {
    WORK.with(|s| s.set(Some(Work::default())));
}
pub(super) fn take() -> Work {
    WORK.with(|s| s.replace(None).unwrap())
}
