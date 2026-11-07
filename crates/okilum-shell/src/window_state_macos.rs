//! App-side Mac mode restoration. AppKit Zoom is a toggle, not an idempotent
//! maximize request: zooming an already full-size restored frame shrinks it.
use super::*;

pub(super) fn maximized(window: &Window) -> bool {
    use objc2::{
        msg_send,
        runtime::{AnyObject, Bool},
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let native = (|| {
        let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw()
        else {
            return None;
        };
        // SAFETY: synchronous UI-thread reads from the live GPUI NSView/NSWindow,
        // following the same borrowed-handle pattern as owned tool windows.
        unsafe {
            let view = &*handle.ns_view.as_ptr().cast::<AnyObject>();
            let native: *mut AnyObject = msg_send![view, window];
            let native = native.as_ref()?;
            let zoomed: Bool = msg_send![native, isZoomed];
            Some(zoomed.as_bool())
        }
    })();
    // Native intent keeps intermediate Zoom geometry out of normal history;
    // the geometric fallback also covers a user filling the visible frame.
    native.unwrap_or(false) || window.is_maximized()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct NormalFrame {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}
impl NormalFrame {
    fn from_frame(frame: &Frame) -> Self {
        Self {
            x: frame.x,
            y: frame.y,
            width: frame.width,
            height: frame.height,
        }
    }

    fn fitted(&self, visible: Bounds<Pixels>) -> Option<Bounds<Pixels>> {
        let frame = Frame {
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
            display: None,
            fullscreen: false,
            maximized: false,
            normal: None,
        };
        frame.valid().then(|| frame.fitted(visible))
    }
}

pub(super) fn remember_normal(frame: &mut Frame, previous: Option<&Frame>) {
    // Fullscreen's native frame no longer says whether its predecessor was
    // zoomed. Preserve that independent mode and the last known normal frame.
    if frame.fullscreen {
        frame.maximized = previous.is_some_and(|old| old.maximized);
    }
    frame.normal = if !frame.fullscreen && !frame.maximized {
        Some(NormalFrame::from_frame(frame))
    } else {
        previous.and_then(|old| {
            old.normal.clone().or_else(|| {
                (!old.fullscreen && !old.maximized && old.valid())
                    .then(|| NormalFrame::from_frame(old))
            })
        })
    };
}

pub(super) fn opening_bounds(frame: &Frame, visible: Bounds<Pixels>) -> WindowBounds {
    let initial = if frame.fullscreen || frame.maximized {
        frame
            .normal
            .as_ref()
            .and_then(|normal| normal.fitted(visible))
            .unwrap_or_else(|| frame.fitted(visible))
    } else {
        frame.fitted(visible)
    };
    // Prime AppKit with the normal frame, then restore modes after mapping.
    // Legacy zoomed frames have no normal history: retain their full geometry
    // and do not invent a different normal frame or toggle an already zoomed one.
    WindowBounds::Windowed(initial)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    Wait,
    Zoom,
    Fullscreen,
    Complete,
}

pub(super) struct Restore {
    fullscreen: bool,
    maximized: bool,
    started: bool,
    zoom_requested: bool,
    fullscreen_requested: bool,
}
impl Restore {
    fn new(frame: &Frame) -> Self {
        Self {
            fullscreen: frame.fullscreen,
            maximized: frame.maximized,
            started: false,
            zoom_requested: false,
            fullscreen_requested: false,
        }
    }

    fn advance(&mut self, fullscreen: bool, maximized: bool) -> Step {
        if !self.started {
            return Step::Wait;
        }
        if fullscreen {
            return if self.fullscreen {
                Step::Complete
            } else {
                Step::Wait
            };
        }
        if self.maximized && !maximized {
            if self.zoom_requested {
                return Step::Wait;
            }
            self.zoom_requested = true;
            return Step::Zoom;
        }
        if self.fullscreen {
            if self.fullscreen_requested {
                return Step::Wait;
            }
            self.fullscreen_requested = true;
            return Step::Fullscreen;
        }
        Step::Complete
    }
}

pub(super) fn record_allowed(key: &str, window: &Window, cx: &mut App) -> bool {
    let step = cx
        .global_mut::<State>()
        .restoring
        .get_mut(key)
        .map(|restore| restore.advance(window.is_fullscreen(), maximized(window)));
    match step {
        None => true,
        Some(Step::Complete) => {
            cx.global_mut::<State>().restoring.remove(key);
            true
        }
        Some(Step::Zoom) => {
            window.zoom_window();
            false
        }
        Some(Step::Fullscreen) => {
            window.toggle_fullscreen();
            false
        }
        Some(Step::Wait) => false,
    }
}

pub(super) fn track(key: &str, restore: bool, window: &mut Window, cx: &mut App) {
    let frame = super::super::reader_ui_state::window_frame(key, cx)
        .or_else(|| cx.global::<State>().saved.frames.get(key).cloned())
        .or_else(|| super::super::reader_ui_state::inherited_window_frame(key, cx))
        .filter(|frame| restore && frame.valid() && (frame.fullscreen || frame.maximized));
    let Some(frame) = frame else { return };
    cx.global_mut::<State>()
        .restoring
        .insert(key.to_owned(), Restore::new(&frame));
    let first_key = key.to_owned();
    window.on_next_frame(move |window, cx| {
        if let Some(restore) = cx.global_mut::<State>().restoring.get_mut(&first_key) {
            restore.started = true;
        }
        super::record(&first_key, window, cx);
    });
    let timeout_key = key.to_owned();
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(std::time::Duration::from_secs(5))
            .await;
        cx.update(|cx| {
            if cx
                .global_mut::<State>()
                .restoring
                .remove(&timeout_key)
                .is_some()
            {
                // Preserve the old durable intent rather than replacing it
                // with a transient startup frame. Later user changes can save.
                eprintln!("Mac window mode restoration did not settle within five seconds");
            }
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> Frame {
        Frame {
            x: 0.,
            y: 30.,
            width: 1600.,
            height: 870.,
            display: Some("secondary-display".into()),
            fullscreen: false,
            maximized: true,
            normal: None,
        }
    }

    fn visible() -> Bounds<Pixels> {
        Bounds::new(point(px(0.), px(30.)), size(px(1600.), px(870.)))
    }

    fn normal() -> Frame {
        Frame {
            x: 100.,
            y: 100.,
            width: 900.,
            height: 650.,
            maximized: false,
            ..frame()
        }
    }

    #[test]
    fn window_state_2708_normal_history_survives_zoom_fullscreen_and_serialization() {
        let mut regular = normal();
        remember_normal(&mut regular, None);
        let mut zoomed = frame();
        remember_normal(&mut zoomed, Some(&regular));
        assert_eq!(zoomed.normal, regular.normal);
        let mut fullscreen = Frame {
            fullscreen: true,
            maximized: false,
            ..frame()
        };
        remember_normal(&mut fullscreen, Some(&zoomed));
        assert!(fullscreen.fullscreen && fullscreen.maximized);
        assert_eq!(fullscreen.normal, regular.normal);
        let restored: Frame =
            serde_json::from_slice(&serde_json::to_vec(&fullscreen).unwrap()).unwrap();
        assert_eq!(restored, fullscreen);
        assert_eq!(restored.normal.as_ref().unwrap().width, 900.);
        let mut unzoomed = normal();
        remember_normal(&mut unzoomed, Some(&restored));
        assert!(!unzoomed.fullscreen && !unzoomed.maximized);
        assert_eq!(unzoomed.normal, regular.normal);
    }

    #[test]
    fn window_state_2708_zoom_primes_normal_frame_without_an_early_native_toggle() {
        let regular = normal();
        let mut zoomed = frame();
        remember_normal(&mut zoomed, Some(&regular));
        // GPUI unconditionally calls native Zoom for WindowBounds::Maximized.
        // AppKit must first see the normal frame; the mapped transition owns
        // the sole Zoom request, retaining a real native unzoom destination.
        assert_eq!(
            opening_bounds(&zoomed, visible()),
            WindowBounds::Windowed(regular.fitted(visible()))
        );
        let mut restore = Restore::new(&zoomed);
        assert_eq!(restore.advance(false, false), Step::Wait);
        restore.started = true;
        assert_eq!(restore.advance(false, false), Step::Zoom);
        assert_eq!(restore.advance(false, false), Step::Wait);
        assert_eq!(restore.advance(false, true), Step::Complete);
    }

    #[test]
    fn window_state_2708_legacy_zoom_retains_full_geometry_without_double_zoom() {
        let mut legacy = serde_json::to_value(frame()).unwrap();
        legacy.as_object_mut().unwrap().remove("normal");
        let legacy: Frame = serde_json::from_value(legacy).unwrap();
        assert!(legacy.normal.is_none());
        assert_eq!(
            opening_bounds(&legacy, visible()),
            WindowBounds::Windowed(visible())
        );
        let mut restore = Restore::new(&legacy);
        restore.started = true;
        assert_eq!(restore.advance(false, true), Step::Complete);
        let mut still_zoomed = legacy.clone();
        remember_normal(&mut still_zoomed, Some(&legacy));
        assert!(
            still_zoomed.normal.is_none(),
            "unknown history stays unknown"
        );
    }

    #[test]
    fn window_state_2708_zoom_and_fullscreen_requests_are_ordered_and_not_repeated() {
        let frame = Frame {
            fullscreen: true,
            ..frame()
        };
        let mut restore = Restore::new(&frame);
        assert_eq!(restore.advance(false, false), Step::Wait);
        restore.started = true;
        assert_eq!(restore.advance(false, false), Step::Zoom);
        assert_eq!(restore.advance(false, false), Step::Wait);
        assert_eq!(restore.advance(false, true), Step::Fullscreen);
        assert_eq!(restore.advance(false, true), Step::Wait);
        assert_eq!(restore.advance(true, false), Step::Complete);
    }

    #[test]
    fn window_state_2708_normal_and_retina_display_coordinates_remain_logical_points() {
        let regular = normal();
        let mut zoomed = frame();
        remember_normal(&mut zoomed, Some(&regular));
        // Different displays/Dock/menu insets are already in GPUI's paired,
        // display-relative points. Retina pixels must not divide these again.
        for area in [
            visible(),
            Bounds::new(point(px(70.), px(24.)), size(px(1370.), px(876.))),
            Bounds::new(point(px(0.), px(24.)), size(px(720.), px(450.))),
        ] {
            let opening = opening_bounds(&zoomed, area).get_bounds();
            assert_eq!(opening, regular.fitted(area));
            assert!(opening.left() >= area.left() && opening.right() <= area.right());
            assert!(opening.top() >= area.top() && opening.bottom() <= area.bottom());
        }
        assert_eq!(
            opening_bounds(&regular, visible()).get_bounds(),
            regular.fitted(visible())
        );
    }

    #[test]
    fn window_state_2708_fullscreen_normal_and_invalid_optional_history_are_safe() {
        let mut fullscreen = Frame {
            fullscreen: true,
            ..normal()
        };
        assert!(matches!(
            opening_bounds(&fullscreen, visible()),
            WindowBounds::Windowed(_)
        ));
        let mut restore = Restore::new(&fullscreen);
        restore.started = true;
        assert_eq!(restore.advance(false, false), Step::Fullscreen);
        assert_eq!(restore.advance(false, false), Step::Wait);
        assert_eq!(restore.advance(true, false), Step::Complete);
        fullscreen.normal = Some(NormalFrame {
            width: -1.,
            ..NormalFrame::from_frame(&normal())
        });
        assert_eq!(
            opening_bounds(&fullscreen, visible()).get_bounds(),
            fullscreen.fitted(visible())
        );
    }
}
