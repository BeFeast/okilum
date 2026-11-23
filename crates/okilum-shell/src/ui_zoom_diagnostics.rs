//! Render-scale diagnostics for the whole-UI zoom (#3244).
//!
//! After a restart with a restored UI zoom, resetting it sometimes left the
//! whole UI blurry on macOS. This records what each side believes: the scale
//! GPUI renders at, the UI zoom, the native window and screen backing scales,
//! whether the window had a screen when read (GPUI's Mac backend assumes 2.0
//! when it has none), the window frame, and the layer's contents scale, size
//! and drawable size. It is read-only and never changes a scale.
use super::*;
use serde::Serialize;

/// One reading of a window's render scale. Flattened into feedback metadata,
/// so every field name is a metadata key.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct Snapshot {
    /// GPUI's device scale: OS display scale × UI zoom.
    pub render_scale: f32,
    pub ui_zoom: f32,
    /// GPUI's logical viewport in UI pixels.
    pub viewport: String,
    pub display: Option<String>,
    pub screen_backing_scale: Option<f32>,
    pub window_backing_scale: Option<f32>,
    /// `false` when `[NSWindow screen]` was nil at the time of reading.
    pub window_on_screen: Option<bool>,
    /// Native outer frame in screen points (macOS) or GPUI window bounds.
    pub window_frame: Option<String>,
    pub layer_contents_scale: Option<f32>,
    pub layer_size: Option<String>,
    pub drawable_size: Option<String>,
    /// Whether the drawable is the layer size × contents scale; when it is
    /// not, the compositor stretches the rendered frame.
    pub drawable_matches_layer: Option<bool>,
}

pub(crate) fn snapshot(window: &Window, cx: &App) -> Snapshot {
    let viewport = window.viewport_size();
    let mut snapshot = Snapshot {
        render_scale: window.scale_factor(),
        ui_zoom: window.ui_scale(),
        viewport: size_text(
            f32::from(viewport.width) as f64,
            f32::from(viewport.height) as f64,
        ),
        display: window
            .display(cx)
            .map(|display| format!("{:?}", display.id())),
        ..Default::default()
    };
    #[cfg(target_os = "macos")]
    native::read(window, &mut snapshot);
    #[cfg(not(target_os = "macos"))]
    {
        let bounds = window.bounds();
        snapshot.window_frame = Some(frame_text(
            f32::from(bounds.left()) as f64,
            f32::from(bounds.top()) as f64,
            f32::from(bounds.size.width) as f64,
            f32::from(bounds.size.height) as f64,
        ));
    }
    snapshot
}

/// Log one `ui_scale` line. `bounds` readings are logged only when a scale,
/// the display, the screen presence or the drawable match changed, so plain
/// resizes and activations do not fill the log.
pub(crate) fn note(reason: &'static str, window: &Window, cx: &mut App) {
    let snapshot = snapshot(window, cx);
    let id = window.window_handle().window_id();
    let key = scale_key(&snapshot);
    let open = cx.windows();
    let logged = cx.default_global::<Logged>();
    logged
        .0
        .retain(|window, _| open.iter().any(|handle| handle.window_id() == *window));
    if reason == "bounds" && logged.0.get(&id) == Some(&key) {
        return;
    }
    logged.0.insert(id, key);
    if let Some(trace) = reader_diagnostics::trace(cx) {
        trace.event_now(
            "ui_scale",
            serde_json::json!({
                "reason": reason,
                "window": format!("{id:?}"),
                "scale": snapshot,
            }),
        );
    }
}

/// The last logged scale-relevant reading per window.
#[derive(Default)]
struct Logged(std::collections::HashMap<gpui::WindowId, Snapshot>);
impl Global for Logged {}

fn scale_key(snapshot: &Snapshot) -> Snapshot {
    Snapshot {
        viewport: String::new(),
        window_frame: None,
        layer_size: None,
        drawable_size: None,
        ..snapshot.clone()
    }
}

fn size_text(width: f64, height: f64) -> String {
    format!("{width}×{height}")
}

fn frame_text(x: f64, y: f64, width: f64, height: f64) -> String {
    format!("{x},{y} {}", size_text(width, height))
}

#[cfg(any(target_os = "macos", test))]
fn drawable_matches(layer: (f64, f64), scale: f64, drawable: (f64, f64)) -> bool {
    (layer.0 * scale - drawable.0).abs() <= 1. && (layer.1 * scale - drawable.1).abs() <= 1.
}

#[cfg(target_os = "macos")]
mod native {
    use super::Snapshot;
    use gpui::Window;
    use objc2::{
        msg_send,
        runtime::{AnyObject, Bool},
        sel,
    };
    use objc2_foundation::{NSRect, NSSize};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    pub(super) fn read(window: &Window, snapshot: &mut Snapshot) {
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return;
        };
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return;
        };
        // SAFETY: synchronous AppKit/Core Animation getters on GPUI's UI
        // thread. The live handle owns the NSView and its NSWindow for this
        // call; CGFloat/CGRect/CGSize match the native return types, and
        // drawableSize is only sent to a layer that responds to it.
        unsafe {
            let view = &*handle.ns_view.as_ptr().cast::<AnyObject>();
            let native: *mut AnyObject = msg_send![view, window];
            if let Some(native) = native.as_ref() {
                let scale: f64 = msg_send![native, backingScaleFactor];
                snapshot.window_backing_scale = Some(scale as f32);
                let frame: NSRect = msg_send![native, frame];
                snapshot.window_frame = Some(super::frame_text(
                    frame.origin.x,
                    frame.origin.y,
                    frame.size.width,
                    frame.size.height,
                ));
                let screen: *mut AnyObject = msg_send![native, screen];
                snapshot.window_on_screen = Some(!screen.is_null());
                if let Some(screen) = screen.as_ref() {
                    let scale: f64 = msg_send![screen, backingScaleFactor];
                    snapshot.screen_backing_scale = Some(scale as f32);
                }
            }
            let layer: *mut AnyObject = msg_send![view, layer];
            if let Some(layer) = layer.as_ref() {
                let scale: f64 = msg_send![layer, contentsScale];
                snapshot.layer_contents_scale = Some(scale as f32);
                let bounds: NSRect = msg_send![layer, bounds];
                let size = (bounds.size.width, bounds.size.height);
                snapshot.layer_size = Some(super::size_text(size.0, size.1));
                let drawable: Bool = msg_send![layer, respondsToSelector: sel!(drawableSize)];
                if drawable.as_bool() {
                    let drawable: NSSize = msg_send![layer, drawableSize];
                    let drawable = (drawable.width, drawable.height);
                    snapshot.drawable_size = Some(super::size_text(drawable.0, drawable.1));
                    snapshot.drawable_matches_layer =
                        Some(super::drawable_matches(size, scale, drawable));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    struct Content;
    impl Render for Content {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full()
        }
    }

    #[test]
    fn drawable_match_detects_a_stretched_frame() {
        assert!(drawable_matches((800., 600.), 2., (1600., 1200.)));
        assert!(drawable_matches((800.5, 600.), 1., (801., 600.)));
        // A 1× drawable on a 2× layer is stretched by the compositor: blurry.
        assert!(!drawable_matches((800., 600.), 2., (800., 600.)));
        // A 2× drawable shown at 1× is scaled down.
        assert!(!drawable_matches((800., 600.), 1., (1600., 1200.)));
    }

    /// Restore, zoom and reset each leave one `ui_scale` line with GPUI's
    /// render scale; repeated identical bounds readings do not.
    #[gpui::test]
    fn zoom_restore_and_reset_are_logged_with_the_render_scale(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(
            state.join("reader-ui.json"),
            br#"{"version":1,"ui_zoom":1.5}"#,
        )
        .unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            cx.set_global(reader_diagnostics::LaunchTrace(
                reader_diagnostics::Trace::new(Some(state.clone()), None),
            ));
            reader_ui_state::install(&state, cx);
            crate::ui_zoom::init(cx);
        });
        let (_, visual) = cx.add_window_view(|window, cx| {
            let content = cx.new(|_| Content);
            Root::new(content, window, cx)
        });
        visual.run_until_parked();
        let os = visual.update(|window, _| window.scale_factor() / window.ui_scale());
        visual.update(|window, cx| {
            note("bounds", window, cx);
            note("bounds", window, cx);
            reader_ui_state::set_ui_zoom(1., cx);
        });
        visual.run_until_parked();
        let lines = reader_diagnostics::logged(&state, "ui_scale");
        let reasons: Vec<_> = lines
            .iter()
            .map(|line| line["details"]["reason"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            reasons.first().map(String::as_str),
            Some("open"),
            "{reasons:?}"
        );
        assert_eq!(
            reasons.last().map(String::as_str),
            Some("zoom"),
            "{reasons:?}"
        );
        assert!(
            !reasons.iter().any(|reason| reason == "bounds"),
            "identical bounds readings are not logged: {reasons:?}"
        );
        let scale = |line: &serde_json::Value, key: &str| line["details"]["scale"][key].as_f64();
        let open = &lines[0];
        assert_eq!(scale(open, "ui_zoom"), Some(1.5));
        assert_eq!(scale(open, "render_scale"), Some((os * 1.5) as f64));
        let reset = lines.last().unwrap();
        assert_eq!(scale(reset, "ui_zoom"), Some(1.));
        assert_eq!(scale(reset, "render_scale"), Some(os as f64));
        // A changed scale is logged even from a bounds callback.
        visual.update(|window, cx| {
            window.set_scale_factor(os * 2.);
            note("bounds", window, cx);
        });
        let lines = reader_diagnostics::logged(&state, "ui_scale");
        let last = lines.last().unwrap();
        assert_eq!(last["details"]["reason"], "bounds");
        assert_eq!(scale(last, "render_scale"), Some((os * 2.) as f64));
    }
}
