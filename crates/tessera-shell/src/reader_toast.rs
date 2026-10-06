//! Reader feedback shares one bottom overlay and explicit lifetimes.
use super::*;
use gpui_component::{notification::Notification, WindowExt};
use std::sync::atomic::{AtomicU64, Ordering};

struct Toast;
struct LinkNotice;
static NEXT_TOAST: AtomicU64 = AtomicU64::new(1);

pub(super) fn transient(message: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
    push(
        Notification::new().message(message),
        Some(Duration::from_secs(4)),
        window,
        cx,
    );
}

pub(super) fn error(message: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
    push(Notification::new().message(message), None, window, cx);
}

pub(super) fn push(
    notification: Notification,
    lifetime: Option<Duration>,
    window: &mut Window,
    cx: &mut App,
) {
    let key = ("reader-toast", NEXT_TOAST.fetch_add(1, Ordering::Relaxed));
    window.push_notification(
        notification
            .id1::<Toast>(key)
            .placement(Anchor::BottomRight)
            .py_2()
            .autohide(false),
        cx,
    );
    if let Some(lifetime) = lifetime {
        let handle = window.window_handle();
        cx.spawn(async move |cx| {
            cx.background_executor().timer(lifetime).await;
            let _ = cx.update_window(handle, |_, window, cx| {
                window.remove_notification1::<Toast>(key, cx);
            });
        })
        .detach();
    }
}

pub(super) fn dismiss(window: &mut Window, cx: &mut App) -> bool {
    let notice = window.notifications(cx).last().cloned();
    if let Some(notice) = notice {
        notice.update(cx, |notice, cx| notice.dismiss(window, cx));
        true
    } else {
        false
    }
}

impl Reader {
    /// Existing error producers retain their state; presentation never takes a layout row.
    pub(super) fn sync_notice_toast(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let notice = self
            .link_notice
            .clone()
            .filter(|_| self.link_choices.is_empty());
        if notice == self.displayed_notice {
            return;
        }
        self.displayed_notice = notice.clone();
        self.notice_generation = self.notice_generation.wrapping_add(1);
        let generation = self.notice_generation;
        let reader = cx.weak_entity();
        window.defer(cx, move |window, cx| {
            let _ = reader.update(cx, |this, cx| {
                if this.notice_generation != generation {
                    return;
                }
                window.remove_notification::<LinkNotice>(cx);
                if let Some(message) = notice {
                    let reader = cx.weak_entity();
                    let dismissed = message.clone();
                    window.push_notification(
                        Notification::new()
                            .id::<LinkNotice>()
                            .message(message)
                            .placement(Anchor::BottomRight)
                            .py_2()
                            .autohide(false)
                            .on_close(move |_, cx| {
                                let _ = reader.update(cx, |this, cx| {
                                    if this.notice_generation == generation
                                        && this.link_notice.as_ref() == Some(&dismissed)
                                    {
                                        this.link_notice = None;
                                        cx.notify();
                                    }
                                });
                            }),
                        cx,
                    );
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::TestAppContext;

    #[gpui::test]
    fn notices_overlay_without_reflow_expire_independently_and_errors_require_dismissal(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("start.md"), "# Stable document\nBody").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("start.md")),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(temp.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        let before = visual.debug_bounds("reader-document").unwrap();
        visual.update(|window, cx| {
            push(
                Notification::new().content(|_, _, _| {
                    div()
                        .debug_selector(|| "toast-positive-control".into())
                        .child("Moved to Folder")
                        .into_any_element()
                }),
                Some(Duration::from_secs(4)),
                window,
                cx,
            )
        });
        visual.run_until_parked();
        let toast = visual
            .debug_bounds("toast-positive-control")
            .expect("real visible toast");
        visual.update(|window, _| assert!(toast.center().y > window.viewport_size().height / 2.));
        assert_eq!(visual.debug_bounds("reader-document").unwrap(), before);
        visual.executor().advance_clock(Duration::from_secs(3));
        visual.run_until_parked();
        assert!(visual.debug_bounds("toast-positive-control").is_some());
        visual.update(|window, cx| transient("Created Second.md", window, cx));
        visual.executor().advance_clock(Duration::from_secs(2));
        visual.run_until_parked();
        assert!(visual.debug_bounds("toast-positive-control").is_none());
        visual.update(|window, cx| {
            assert_eq!(
                window.notifications(cx).len(),
                1,
                "old timer leaves new toast alone"
            )
        });
        visual.executor().advance_clock(Duration::from_secs(3));
        visual.run_until_parked();
        visual.update(|window, cx| assert!(window.notifications(cx).is_empty()));
        reader.update_in(visual, |r, _, cx| {
            r.link_notice = Some("Cannot move: destination exists".into());
            cx.notify();
        });
        visual.run_until_parked();
        visual.update(|window, cx| assert_eq!(window.notifications(cx).len(), 1));
        assert_eq!(visual.debug_bounds("reader-document").unwrap(), before);
        visual.executor().advance_clock(Duration::from_secs(20));
        visual.run_until_parked();
        reader.read_with(visual, |r, _| assert!(r.link_notice.is_some()));
        reader.update_in(visual, |r, window, cx| r.focus_handle.focus(window, cx));
        visual.simulate_keystrokes("escape");
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        reader.read_with(visual, |r, _| assert!(r.link_notice.is_none()));
        visual.update(|window, cx| assert!(window.notifications(cx).is_empty()));
        assert_eq!(visual.debug_bounds("reader-document").unwrap(), before);
    }
}
