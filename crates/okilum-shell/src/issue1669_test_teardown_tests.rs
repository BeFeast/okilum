//! #1669: every `#[gpui::test]` must end the test app it builds.
//!
//! gpui-pre's own macro never reaches `TestAppContext::quit`, so a window
//! opened through `add_window_view` kept its app, its views and every file
//! they held until the test binary exited. `gpui` here is okilum-gpui, whose
//! `#[gpui::test]` adds that call. The probes below are ordinary
//! `#[gpui::test]` functions; the drivers call them in sequence and count
//! what is still alive afterwards. The control runs the same probe under
//! gpui-pre's unchanged macro and must still see the leak, so a probe that
//! measures nothing cannot pass. GPUI's entity leak detector must keep
//! failing a test whose app gpui-pre's teardown already dropped, and since
//! #3039 also one whose app had a window.
use gpui::{
    AnyEntity, AppContext as _, Context, Empty, IntoElement, Render, TestAppContext, Window,
};
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::fs::File;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

/// Open/close cycles per wrapped probe; the suite leaked one app per test.
const CYCLES: usize = 16;

struct Probe {
    file: PathBuf,
    apps: Vec<Weak<dyn Any>>,
    released: Rc<Cell<usize>>,
}

thread_local! {
    static PROBE: RefCell<Option<Probe>> = const { RefCell::new(None) };
    /// An entity handle kept outside its app: what the leak detector reports.
    static HELD_OUTSIDE: RefCell<Option<AnyEntity>> = const { RefCell::new(None) };
}

/// Window root that holds an open descriptor for as long as it lives.
struct DescriptorView {
    _file: Option<File>,
    released: Option<Rc<Cell<usize>>>,
}

impl Drop for DescriptorView {
    fn drop(&mut self) {
        if let Some(released) = &self.released {
            released.set(released.get() + 1);
        }
    }
}

impl Render for DescriptorView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// Opens a window the way shell fixtures do. Outside a driver (libtest also
/// runs the probes on their own) it records nothing and opens no file.
fn open_probe_window(cx: &mut TestAppContext) {
    let recorded = PROBE.with(|probe| {
        probe.borrow().as_ref().map(|probe| {
            (
                File::open(&probe.file).expect("open probe file"),
                probe.released.clone(),
            )
        })
    });
    let (file, released) = recorded.unzip();
    cx.add_window_view(|_, _| DescriptorView {
        _file: file,
        released,
    });
    let app: Rc<dyn Any> = cx.app.clone();
    let app = Rc::downgrade(&app);
    PROBE.with(|probe| {
        if let Some(probe) = probe.borrow_mut().as_mut() {
            probe.apps.push(app);
        }
    });
}

#[gpui::test]
fn issue1669_probe_wrapped_sync(cx: &mut TestAppContext) {
    open_probe_window(cx);
}

#[gpui::test]
async fn issue1669_probe_wrapped_async(cx: &mut TestAppContext) {
    open_probe_window(cx);
    cx.run_until_parked();
}

/// A window-less app: gpui-pre's teardown dropped it and ran the detector.
#[gpui::test]
fn issue1669_probe_windowless_leak(cx: &mut TestAppContext) {
    if PROBE.with(|probe| probe.borrow().is_some()) {
        let entity = cx.new(|_| ());
        HELD_OUTSIDE.with(|held| *held.borrow_mut() = Some(entity.into_any()));
    }
}

/// An app with a window: gpui-pre's teardown never dropped it, so the
/// detector never ran for it before #3039.
#[gpui::test]
fn issue1669_probe_windowed_leak(cx: &mut TestAppContext) {
    if PROBE.with(|probe| probe.borrow().is_some()) {
        open_probe_window(cx);
        let entity = cx.new(|_| ());
        HELD_OUTSIDE.with(|held| *held.borrow_mut() = Some(entity.into_any()));
    }
}

#[gpui::upstream_test]
fn issue1669_probe_upstream_macro(cx: &mut TestAppContext) {
    open_probe_window(cx);
}

/// Installs a probe, runs `body`, and returns what it observed.
fn observe(body: impl FnOnce()) -> (Probe, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("issue1669-probe");
    std::fs::write(&file, b"probe").unwrap();
    PROBE.with(|probe| {
        *probe.borrow_mut() = Some(Probe {
            file: file.canonicalize().unwrap(),
            apps: Vec::new(),
            released: Rc::new(Cell::new(0)),
        })
    });
    body();
    let probe = PROBE.with(|probe| probe.borrow_mut().take()).unwrap();
    (probe, directory)
}

fn live_apps(probe: &Probe) -> usize {
    probe
        .apps
        .iter()
        .filter(|app| app.upgrade().is_some())
        .count()
}

/// Descriptors of this process that still point at the probe file.
#[cfg(target_os = "linux")]
fn open_descriptors(probe: &Probe) -> usize {
    std::fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
        .filter(|target| *target == probe.file)
        .count()
}

#[test]
fn issue1669_gpui_test_ends_each_app_and_closes_its_descriptors() {
    let (probe, _directory) = observe(|| {
        for _ in 0..CYCLES {
            issue1669_probe_wrapped_sync();
            issue1669_probe_wrapped_async();
        }
    });
    assert_eq!(probe.apps.len(), 2 * CYCLES, "every probe opened a window");
    assert_eq!(live_apps(&probe), 0, "test apps outlived their tests");
    assert_eq!(probe.released.get(), 2 * CYCLES, "window roots not dropped");
    #[cfg(target_os = "linux")]
    assert_eq!(open_descriptors(&probe), 0, "probe descriptors still open");
}

#[test]
fn issue1669_upstream_gpui_test_macro_still_retains_the_app() {
    let (probe, _directory) = observe(issue1669_probe_upstream_macro);
    assert_eq!(probe.apps.len(), 1, "the control opened its window");
    // gpui-pre's teardown must still leak here; otherwise this probe does not
    // measure the path the wrapper repairs. The one leaked app is the price.
    assert_eq!(live_apps(&probe), 1, "control app was freed");
    assert_eq!(probe.released.get(), 0, "control window root was dropped");
    #[cfg(target_os = "linux")]
    assert_eq!(open_descriptors(&probe), 1, "control descriptor closed");
}

/// Runs a probe that keeps an entity handle past its app and requires the
/// leak detector to fail it.
fn assert_detector_fails(probe: fn()) -> Probe {
    let (probe_state, _directory) = observe(|| {
        let outcome = std::panic::catch_unwind(probe);
        HELD_OUTSIDE.with(|held| held.borrow_mut().take());
        let payload = outcome.expect_err("leaked entity handle passed");
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or_default();
        assert!(message.contains("leaked handles"), "{message}");
    });
    probe_state
}

#[test]
fn issue1669_leak_detector_still_fails_apps_it_already_checked() {
    assert_detector_fails(issue1669_probe_windowless_leak);
}

#[test]
fn issue1669_leak_detector_fails_apps_with_windows() {
    let probe = assert_detector_fails(issue1669_probe_windowed_leak);
    // The probe must have measured an app that had a window.
    assert_eq!(probe.apps.len(), 1, "the probe opened its window");
    assert_eq!(probe.released.get(), 1, "window root not dropped");
}
