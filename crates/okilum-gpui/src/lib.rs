//! GPUI as okilum-shell names it (`gpui = { package = "okilum-gpui" }`).
//!
//! Everything is gpui-pre, re-exported unchanged, with one exception:
//! `#[gpui::test]` (#1669). gpui-pre 0.3.3's test macro ends each test with
//! `cx.update(|cx| cx.quit())`, which reaches `App::quit` and the test
//! platform's no-op `quit`, never `TestAppContext::quit`. That method is
//! what frees the `VisualTestContext`s that `add_window_view` and
//! `add_empty_window` leak on purpose until the test ends. Without it each
//! test app, its windows and everything they own (Readers, vault watchers,
//! index files, threads) live until the test process exits; the full shell
//! suite accumulated more than a thousand descriptors that way.
//!
//! The replacement attribute keeps gpui-pre's macro and adds, for every
//! `&mut TestAppContext` argument, a guard that settles pending work and
//! calls `TestAppContext::quit` when the test body returns. GPUI itself is
//! not patched. A panicking test keeps its app: quitting during unwinding
//! could panic again and abort the whole test binary.
//!
//! GPUI's entity leak detector runs when a test app is dropped. Under
//! gpui-pre's teardown it therefore ran only for apps without a
//! `VisualTestContext`. Since #3039 it runs for every test app: freeing an
//! app with windows makes a handle that outlives the app (an `Rc` cycle, a
//! thread-local) fail the test that created it.

pub use gpui::*;

/// `#[gpui::test]` that also ends each `TestAppContext` it hands the test.
#[cfg(feature = "test-support")]
pub use okilum_gpui_test_macro::test;

/// gpui-pre's own test macro, for controls that need its exact teardown.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use gpui::test as upstream_test;

#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod __test_support {
    use gpui::TestAppContext;

    /// Ends a test app when the test body that received it returns.
    pub struct QuitOnTestEnd(TestAppContext);

    impl QuitOnTestEnd {
        pub fn new(cx: &TestAppContext) -> Self {
            Self(cx.clone())
        }
    }

    impl Drop for QuitOnTestEnd {
        fn drop(&mut self) {
            if std::thread::panicking() {
                return;
            }
            // Same order as GPUI's own teardown: let queued work finish while
            // windows still exist, then free the test contexts and shut down.
            self.0.run_until_parked();
            self.0.quit();
        }
    }
}
