//! AppKit frame/state adapter for GPUI 0.3.3. Never used by Linux or Windows.
use super::Frame;
use gpui::{Bounds, Pixels, WindowBounds, point, px, size};

/// AppKit frames are logical points, not backing pixels. Preserve fractional
/// normal frames exactly; only shrink/move when the current work area requires it.
pub(super) fn normal_bounds(frame: &Frame, visible: Bounds<Pixels>) -> Bounds<Pixels> {
    let width = px(frame.width).min(visible.size.width);
    let height = px(frame.height).min(visible.size.height);
    Bounds::new(
        point(
            px(frame.x).clamp(visible.left(), visible.right() - width),
            px(frame.y).clamp(visible.top(), visible.bottom() - height),
        ),
        size(width, height),
    )
}

fn plan(frame: &Frame, visible: Bounds<Pixels>) -> WindowBounds {
    let normal = normal_bounds(frame, visible);
    if frame.fullscreen {
        WindowBounds::Fullscreen(normal)
    } else if frame.maximized {
        WindowBounds::Maximized(normal)
    } else {
        WindowBounds::Windowed(normal)
    }
}

/// GPUI's Mac backend reports the current zoomed frame as Windowed. Retain the
/// last normal frame separately from the native mode so unzoom still works.
pub(super) fn capture(mut current: Frame, previous: Option<&Frame>) -> Frame {
    if current.fullscreen || current.maximized {
        if let Some(previous) = previous.filter(|frame| frame.valid()) {
            current.x = previous.x;
            current.y = previous.y;
            current.width = previous.width;
            current.height = previous.height;
            if current.fullscreen {
                current.maximized = previous.maximized;
            }
        }
    }
    current
}

// The same screen-local, top-left point space as GPUI MacDisplay::visible_bounds.
// AppKit uses global, bottom-left coordinates. Never involve backingScaleFactor.
fn from_appkit(frame: Bounds<Pixels>, screen: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        point(
            frame.left() - screen.left(),
            screen.bottom() - frame.bottom(),
        ),
        frame.size,
    )
}

fn to_appkit(frame: Bounds<Pixels>, screen: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        point(
            screen.left() + frame.left(),
            screen.bottom() - frame.bottom(),
        ),
        frame.size,
    )
}

fn fills_visible(frame: Bounds<Pixels>, visible: Bounds<Pixels>) -> bool {
    // Accommodate AppKit's point rounding without classifying a near-full-size
    // normal window (the original bug's right/bottom margins) as maximized.
    [
        frame.left() - visible.left(),
        frame.top() - visible.top(),
        frame.right() - visible.right(),
        frame.bottom() - visible.bottom(),
    ]
    .iter()
    .all(|delta| f32::from(*delta).abs() <= 1.)
}

#[cfg(target_os = "macos")]
pub(super) use native::{restore, snapshot};

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use gpui::{App, Window};
    use objc2::{
        msg_send,
        runtime::{AnyObject, Bool},
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    fn with_window<T>(window: &Window, f: impl FnOnce(&AnyObject) -> T) -> Option<T> {
        let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw()
        else {
            return None;
        };
        // SAFETY: synchronous access on GPUI's UI thread. The live handle owns
        // the NSView/NSWindow throughout this call; no pointer escapes.
        unsafe {
            let native: *mut AnyObject =
                msg_send![&*handle.ns_view.as_ptr().cast::<AnyObject>(), window];
            native.as_ref().map(f)
        }
    }

    fn bounds(rect: NSRect) -> Bounds<Pixels> {
        Bounds::new(
            point(px(rect.origin.x as f32), px(rect.origin.y as f32)),
            size(px(rect.size.width as f32), px(rect.size.height as f32)),
        )
    }

    fn rect(bounds: Bounds<Pixels>) -> NSRect {
        NSRect::new(
            NSPoint::new(
                f32::from(bounds.left()) as f64,
                f32::from(bounds.top()) as f64,
            ),
            NSSize::new(
                f32::from(bounds.size.width) as f64,
                f32::from(bounds.size.height) as f64,
            ),
        )
    }

    pub(crate) fn snapshot(window: &Window, cx: &App) -> Option<Frame> {
        let display = window
            .display(cx)
            .and_then(|d| d.uuid().ok())
            .map(|u| u.to_string());
        // SAFETY: AppKit object methods on the UI thread; NSRect/Bool match the
        // native method signatures. No global display or Retina pixel conversion.
        with_window(window, |native| unsafe {
            let screen: *mut AnyObject = msg_send![native, screen];
            let screen = screen.as_ref()?;
            let screen_frame: NSRect = msg_send![screen, frame];
            let visible: NSRect = msg_send![screen, visibleFrame];
            let outer: NSRect = msg_send![native, frame];
            let zoomed: Bool = msg_send![native, isZoomed];
            let fullscreen = window.is_fullscreen();
            let outer = from_appkit(bounds(outer), bounds(screen_frame));
            let visible = from_appkit(bounds(visible), bounds(screen_frame));
            Some(Frame {
                x: outer.left().into(),
                y: outer.top().into(),
                width: outer.size.width.into(),
                height: outer.size.height.into(),
                display,
                fullscreen,
                maximized: !fullscreen && (zoomed.as_bool() || fills_visible(outer, visible)),
            })
        })
        .flatten()
    }

    pub(crate) fn restore(frame: &Frame, window: &Window, cx: &App) {
        let Some(display) = window.display(cx) else {
            return;
        };
        let state = plan(frame, display.visible_bounds());
        // SAFETY: synchronous UI-thread messages to the live NSWindow. Set the
        // *outer* frame, avoiding GPUI's content-rect initialization and incorrect
        // secondary-screen y conversion. Zoom first gives AppKit its normal frame;
        // then fill the current visibleFrame, including changed Dock/menu insets.
        let _ = with_window(window, |native| unsafe {
            let screen: *mut AnyObject = msg_send![native, screen];
            let Some(screen) = screen.as_ref() else {
                return;
            };
            let screen_frame: NSRect = msg_send![screen, frame];
            let normal = rect(to_appkit(state.get_bounds(), bounds(screen_frame)));
            let _: () = msg_send![native, setFrame: normal, display: Bool::YES];
            if frame.maximized {
                let zoomed: Bool = msg_send![native, isZoomed];
                if !zoomed.as_bool() {
                    let _: () = msg_send![native, zoom: std::ptr::null::<AnyObject>()];
                }
                let visible: NSRect = msg_send![screen, visibleFrame];
                let _: () = msg_send![native, setFrame: visible, display: Bool::YES];
            }
            if matches!(state, WindowBounds::Fullscreen(_)) && !window.is_fullscreen() {
                let _: () = msg_send![native, toggleFullScreen: std::ptr::null::<AnyObject>()];
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normal() -> Frame {
        Frame {
            x: 81.5,
            y: 62.5,
            width: 1100.5,
            height: 700.5,
            display: Some("retina".into()),
            fullscreen: false,
            maximized: false,
        }
    }
    fn visible() -> Bounds<Pixels> {
        Bounds::new(point(px(0.), px(30.)), size(px(1600.), px(830.)))
    }

    #[test]
    fn zoom_and_fullscreen_roundtrip_preserves_the_normal_frame_and_mode() {
        let normal = normal();
        for (fullscreen, maximized) in [(false, true), (true, false)] {
            let zoomed = capture(
                Frame {
                    x: 0.,
                    y: 30.,
                    width: 1600.,
                    height: 830.,
                    fullscreen,
                    maximized,
                    ..normal.clone()
                },
                Some(&normal),
            );
            assert_eq!(
                (zoomed.x, zoomed.y, zoomed.width, zoomed.height),
                (normal.x, normal.y, normal.width, normal.height)
            );
            let saved: Frame =
                serde_json::from_slice(&serde_json::to_vec(&zoomed).unwrap()).unwrap();
            assert_eq!(saved.fullscreen, fullscreen);
            assert_eq!(saved.maximized, maximized);
            let restored = plan(&saved, visible());
            assert_eq!(restored.get_bounds(), normal_bounds(&normal, visible()));
            assert_eq!(matches!(restored, WindowBounds::Fullscreen(_)), fullscreen);
            assert_eq!(matches!(restored, WindowBounds::Maximized(_)), maximized);
            // Repeated zoomed snapshots must not overwrite normal geometry.
            assert_eq!(capture(zoomed.clone(), Some(&saved)), saved);
        }
        let resized = Frame {
            x: 90.,
            width: 950.,
            ..normal.clone()
        };
        assert_eq!(capture(resized.clone(), Some(&normal)), resized);
        assert!(matches!(
            plan(&resized, visible()),
            WindowBounds::Windowed(_)
        ));
        let fullscreen = capture(
            Frame {
                fullscreen: true,
                ..normal.clone()
            },
            Some(&Frame {
                maximized: true,
                ..normal
            }),
        );
        assert!(fullscreen.fullscreen && fullscreen.maximized);
    }

    #[test]
    fn normal_frame_is_exact_in_logical_points_even_below_default_minimum() {
        let frame = Frame {
            width: 500.5,
            height: 300.5,
            ..normal()
        };
        assert_eq!(
            normal_bounds(&frame, visible()),
            Bounds::new(point(px(81.5), px(62.5)), size(px(500.5), px(300.5)))
        );
    }

    #[test]
    fn changed_visible_frame_clamps_normal_but_zoom_fills_the_new_work_area() {
        let screen = Bounds::new(point(px(60.), px(24.)), size(px(1220.), px(700.)));
        let frame = Frame {
            x: -200.,
            y: 1200.,
            width: 1800.,
            height: 900.,
            ..normal()
        };
        assert_eq!(normal_bounds(&frame, screen), screen);
        let frame = Frame {
            x: 1100.,
            y: 700.,
            width: 700.,
            height: 450.,
            ..normal()
        };
        assert_eq!(
            normal_bounds(&frame, screen),
            Bounds::new(point(px(580.), px(274.)), size(px(700.), px(450.)))
        );
        assert!(fills_visible(screen, screen));
        assert!(!fills_visible(
            Bounds::new(
                screen.origin,
                size(screen.size.width - px(20.), screen.size.height - px(20.))
            ),
            screen
        ));
    }

    #[test]
    fn appkit_coordinates_roundtrip_on_retina_and_displays_above_and_left() {
        let local = normal_bounds(&normal(), visible());
        for screen in [
            Bounds::new(point(px(0.), px(0.)), size(px(1600.), px(900.))),
            Bounds::new(point(px(-1600.), px(900.)), size(px(1600.), px(900.))),
            Bounds::new(point(px(1600.), px(-900.)), size(px(1600.), px(900.))),
        ] {
            assert_eq!(from_appkit(to_appkit(local, screen), screen), local);
            let work = to_appkit(visible(), screen);
            assert_eq!(work.size, visible().size, "never scale points by Retina 2x");
            assert_eq!(from_appkit(work, screen), visible());
        }
    }
}
