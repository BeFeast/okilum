//! Settings-local mitigation for macOS 27.2's WindowServer drag handoff.
//! Native coordinates are always absolute Cocoa screen points; window-local
//! mouse coordinates would feed movement back into the next delta and jump.

#[derive(Clone, Copy, Debug, PartialEq)]
struct Point {
    x: f64,
    y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    origin: Point,
    width: f64,
    height: f64,
}

impl Rect {
    fn contains(self, point: Point) -> bool {
        point.x >= self.origin.x
            && point.x < self.origin.x + self.width
            && point.y >= self.origin.y
            && point.y < self.origin.y + self.height
    }
}

#[derive(Clone, Copy, Debug)]
struct Anchor {
    mouse: Point,
    frame: Rect,
}

impl Anchor {
    fn origin(self, mouse: Point, visible: Rect) -> Option<Point> {
        if ![
            self.mouse.x,
            self.mouse.y,
            self.frame.origin.x,
            self.frame.origin.y,
            self.frame.width,
            self.frame.height,
            mouse.x,
            mouse.y,
            visible.origin.x,
            visible.origin.y,
            visible.width,
            visible.height,
        ]
        .into_iter()
        .all(f64::is_finite)
            || self.frame.width <= 0.
            || self.frame.height <= 0.
            || visible.width < 80.
            || visible.height < 34.
        {
            return None;
        }
        // Keep at least an 80pt caption segment and its full height reachable.
        // The selected display is the one under the actual screen pointer.
        Some(Point {
            x: (self.frame.origin.x + mouse.x - self.mouse.x).clamp(
                visible.origin.x + 80. - self.frame.width,
                visible.origin.x + visible.width - 80.,
            ),
            y: (self.frame.origin.y + mouse.y - self.mouse.y).clamp(
                visible.origin.y + 34. - self.frame.height,
                visible.origin.y + visible.height - self.frame.height,
            ),
        })
    }
}

fn affected_version(major: isize, minor: isize) -> bool {
    major == 27 && minor == 2
}

#[derive(Default)]
struct Drag {
    anchor: std::cell::Cell<Option<Anchor>>,
    last_origin: std::cell::Cell<Option<Point>>,
}

impl Drag {
    fn begin(&self, anchor: Anchor) {
        self.anchor.set(Some(anchor));
        self.last_origin.set(Some(anchor.frame.origin));
    }
    fn cancel(&self) {
        self.anchor.set(None);
        self.last_origin.set(None);
    }
    fn active(&self) -> bool {
        self.anchor.get().is_some()
    }
    fn applied(&self, actual_origin: Point) {
        if self.active() {
            self.last_origin.set(Some(actual_origin));
        }
    }
    fn step(
        &self,
        owned_and_active: bool,
        left_pressed: bool,
        frame: Rect,
        mouse: Point,
        visible: Option<Rect>,
    ) -> Option<Point> {
        let anchor = self.anchor.get()?;
        let origin = (owned_and_active
            && left_pressed
            && self.last_origin.get() == Some(frame.origin)
            && frame.width == anchor.frame.width
            && frame.height == anchor.frame.height)
            .then(|| visible.and_then(|visible| anchor.origin(mouse, visible)))
            .flatten();
        if origin.is_none() {
            self.cancel();
        }
        origin
    }
}

#[cfg(target_os = "macos")]
pub(super) use native::NativeDrag;

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use block2::RcBlock;
    use objc2::{
        class, msg_send,
        rc::Retained,
        runtime::{AnyObject, Bool},
    };
    use objc2_foundation::{NSPoint, NSProcessInfo, NSRect};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::{cell::RefCell, rc::Rc};

    #[derive(Clone)]
    pub(in super::super) struct NativeDrag(Rc<Inner>);

    struct Inner {
        window: Retained<AnyObject>,
        drag: Drag,
        monitor: RefCell<Option<Retained<AnyObject>>>,
    }

    fn point(p: NSPoint) -> Point {
        Point { x: p.x, y: p.y }
    }
    fn rect(r: NSRect) -> Rect {
        Rect {
            origin: point(r.origin),
            width: r.size.width,
            height: r.size.height,
        }
    }

    impl NativeDrag {
        pub(in super::super) fn install(window: &gpui::Window) -> Option<Self> {
            let version = NSProcessInfo::processInfo().operatingSystemVersion();
            if !affected_version(version.majorVersion, version.minorVersion) {
                return None;
            }
            let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else {
                return None;
            };
            // SAFETY: invoked on the GPUI UI thread with its live native view.
            // Retaining the NSWindow makes the local monitor independent of the
            // view's teardown order; the monitor is removed with the last owner.
            unsafe {
                let raw: *mut AnyObject =
                    msg_send![&*handle.ns_view.as_ptr().cast::<AnyObject>(), window];
                let window = Retained::retain(raw)?;
                let inner = Rc::new(Inner {
                    window,
                    drag: Drag::default(),
                    monitor: RefCell::new(None),
                });
                let weak = Rc::downgrade(&inner);
                let block = RcBlock::new(move |event: *mut AnyObject| -> *mut AnyObject {
                    if event.is_null() {
                        return event;
                    }
                    weak.upgrade().map_or(event, |inner| inner.event(&*event))
                });
                // App-local only, not a global input monitor/event tap. It
                // observes new presses/up to retire a lost drag and consumes
                // only drags begun in THIS window's blank Settings caption.
                let mask = (1_usize << 1) | (1_usize << 2) | (1_usize << 6);
                let token: *mut AnyObject = msg_send![class!(NSEvent), addLocalMonitorForEventsMatchingMask: mask, handler: &*block];
                *inner.monitor.borrow_mut() = Some(Retained::retain(token)?);
                Some(Self(inner))
            }
        }

        pub(in super::super) fn begin(&self) -> bool {
            // SAFETY: UI-thread, original synchronous mouse-down callback.
            // No AppKit move or nested event loop is started while GPUI holds
            // the app borrow. Subsequent movement happens before GPUI dispatch.
            unsafe {
                let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
                let event: *mut AnyObject = msg_send![app, currentEvent];
                if event.is_null() {
                    return false;
                }
                let kind: usize = msg_send![event, type];
                let window: *mut AnyObject = msg_send![event, window];
                if kind != 1 || window != Retained::as_ptr(&self.0.window).cast_mut() {
                    return false;
                }
                let local: NSPoint = msg_send![event, locationInWindow];
                let mouse: NSPoint = msg_send![&*self.0.window, convertPointToScreen: local];
                let frame: NSRect = msg_send![&*self.0.window, frame];
                self.0.drag.begin(Anchor {
                    mouse: point(mouse),
                    frame: rect(frame),
                });
                true
            }
        }
    }

    impl Inner {
        unsafe fn event(&self, event: &AnyObject) -> *mut AnyObject {
            let original = (event as *const AnyObject).cast_mut();
            // SAFETY: Cocoa supplies NSEvents on the UI thread to this monitor.
            unsafe {
                let kind: usize = msg_send![event, type];
                if kind == 1 || kind == 2 {
                    self.drag.cancel();
                    return original;
                }
                if !self.drag.active() {
                    return original;
                }
                let owner: *mut AnyObject = msg_send![event, window];
                let visible: Bool = msg_send![&*self.window, isVisible];
                let key: Bool = msg_send![&*self.window, isKeyWindow];
                let buttons: usize = msg_send![class!(NSEvent), pressedMouseButtons];
                if owner != Retained::as_ptr(&self.window).cast_mut()
                    || !visible.as_bool()
                    || !key.as_bool()
                    || buttons & 1 == 0
                {
                    self.drag.cancel();
                    return original;
                }
                let frame: NSRect = msg_send![&*self.window, frame];
                let mouse: NSPoint = msg_send![class!(NSEvent), mouseLocation];
                let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
                let count: usize = msg_send![screens, count];
                let mut visible_frame = None;
                for index in 0..count {
                    let screen: *mut AnyObject = msg_send![screens, objectAtIndex: index];
                    let bounds: NSRect = msg_send![screen, frame];
                    if rect(bounds).contains(point(mouse)) {
                        let visible: NSRect = msg_send![screen, visibleFrame];
                        visible_frame = Some(rect(visible));
                        break;
                    }
                }
                let Some(origin) =
                    self.drag
                        .step(true, true, rect(frame), point(mouse), visible_frame)
                else {
                    return original;
                };
                // Absolute start-frame + current screen pointer; never add a
                // previous move delta or re-use an old window-local position.
                let _: () =
                    msg_send![&*self.window, setFrameOrigin: NSPoint::new(origin.x, origin.y)];
                // Acknowledge AppKit's actual frame, including pixel alignment.
                // A later unrelated geometry change cancels the old gesture.
                let actual: NSRect = msg_send![&*self.window, frame];
                self.drag.applied(point(actual.origin));
                std::ptr::null_mut()
            }
        }
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            self.drag.cancel();
            if let Some(monitor) = self.monitor.get_mut().take() {
                // SAFETY: matching token from our app-local NSEvent monitor,
                // removed on the UI thread before releasing the owned window.
                unsafe {
                    let _: () = msg_send![class!(NSEvent), removeMonitor: &*monitor];
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn display() -> Rect {
        Rect {
            origin: Point { x: 0., y: 0. },
            width: 1600.,
            height: 870.,
        }
    }
    fn anchor() -> Anchor {
        Anchor {
            mouse: Point { x: 800., y: 700. },
            frame: Rect {
                origin: Point { x: 410., y: 200. },
                width: 780.,
                height: 520.,
            },
        }
    }

    #[test]
    fn issue2770_manual_drag_is_limited_to_affected_os_family() {
        for (major, minor, expected) in [
            (26, 2, false),
            (27, 0, false),
            (27, 1, false),
            (27, 2, true),
            (27, 3, false),
            (28, 0, false),
        ] {
            assert_eq!(affected_version(major, minor), expected);
        }
    }

    #[test]
    fn issue2770_absolute_screen_anchor_never_accumulates_frame_or_local_deltas() {
        let start = anchor();
        for delta in [15., 30., 45., 60., 60., 45., 0., -60., 0.] {
            assert_eq!(
                start.origin(
                    Point {
                        x: 800. + delta,
                        y: 700.
                    },
                    display()
                ),
                Some(Point {
                    x: 410. + delta,
                    y: 200.
                })
            );
        }
        assert_eq!(
            start.origin(Point { x: 800., y: 640. }, display()),
            Some(Point { x: 410., y: 140. })
        );
    }

    #[test]
    fn issue2770_manual_drag_retains_reachable_caption_and_menu_bar_clearance() {
        let start = anchor();
        assert_eq!(
            start.origin(
                Point {
                    x: -2000.,
                    y: 4000.
                },
                display()
            ),
            Some(Point { x: -700., y: 350. })
        );
        assert_eq!(
            start.origin(
                Point {
                    x: 4000.,
                    y: -2000.
                },
                display()
            ),
            Some(Point { x: 1520., y: -486. })
        );
        let left_display = Rect {
            origin: Point { x: -1920., y: 100. },
            width: 1920.,
            height: 1080.,
        };
        assert!(left_display.contains(Point { x: -100., y: 700. }));
        assert_eq!(
            start.origin(Point { x: -100., y: 700. }, left_display),
            Some(Point { x: -490., y: 200. })
        );
        assert!(!left_display.contains(Point { x: 0., y: 700. }));
    }

    #[test]
    fn issue2770_invalid_native_geometry_never_produces_a_move() {
        let start = anchor();
        assert!(start
            .origin(
                Point {
                    x: f64::NAN,
                    y: 700.
                },
                display()
            )
            .is_none());
        assert!(start
            .origin(
                Point { x: 800., y: 700. },
                Rect {
                    height: 0.,
                    ..display()
                }
            )
            .is_none());
    }

    #[test]
    fn issue2770_manual_drag_does_not_restart_after_up_wrong_window_or_lost_button() {
        let drag = Drag::default();
        let mouse = Point { x: 830., y: 700. };
        assert!(!drag.active());
        assert!(drag
            .step(true, true, anchor().frame, mouse, Some(display()))
            .is_none());
        for (owned, pressed) in [(false, true), (true, false)] {
            drag.begin(anchor());
            assert!(drag.active());
            assert!(drag
                .step(owned, pressed, anchor().frame, mouse, Some(display()))
                .is_none());
            assert!(!drag.active());
            assert!(drag
                .step(true, true, anchor().frame, mouse, Some(display()))
                .is_none());
        }
        drag.begin(anchor());
        assert_eq!(
            drag.step(true, true, anchor().frame, mouse, Some(display())),
            Some(Point { x: 440., y: 200. })
        );
        drag.cancel();
        assert!(drag
            .step(true, true, anchor().frame, mouse, Some(display()))
            .is_none());
    }

    #[test]
    fn issue2770_manual_drag_retires_changed_size_or_unknown_display() {
        let drag = Drag::default();
        let mouse = Point { x: 830., y: 700. };
        drag.begin(anchor());
        let changed = Rect {
            width: 900.,
            ..anchor().frame
        };
        assert!(drag
            .step(true, true, changed, mouse, Some(display()))
            .is_none());
        assert!(!drag.active());
        drag.begin(anchor());
        assert!(drag.step(true, true, anchor().frame, mouse, None).is_none());
        assert!(!drag.active());
    }

    #[test]
    fn issue2770_manual_drag_never_overwrites_an_external_window_move() {
        let drag = Drag::default();
        let mouse = Point { x: 830., y: 700. };
        drag.begin(anchor());
        let moved = drag
            .step(true, true, anchor().frame, mouse, Some(display()))
            .unwrap();
        drag.applied(moved);
        let current = Rect {
            origin: moved,
            ..anchor().frame
        };
        assert_eq!(
            drag.step(true, true, current, mouse, Some(display())),
            Some(moved)
        );
        let external = Rect {
            origin: Point {
                x: moved.x + 25.,
                ..moved
            },
            ..current
        };
        assert!(drag
            .step(true, true, external, mouse, Some(display()))
            .is_none());
        assert!(!drag.active());
    }
}
