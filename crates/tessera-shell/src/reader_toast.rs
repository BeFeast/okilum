//! Reader feedback shares one bottom overlay and explicit lifetimes.
use super::*;
use gpui_component::{notification::Notification, WindowExt};
use std::sync::atomic::{AtomicU64, Ordering};

struct Toast;
struct LinkNotice;
struct HistoryNotice;
static NEXT_TOAST: AtomicU64 = AtomicU64::new(1);
static NEXT_NOTICE: AtomicU64 = AtomicU64::new(1);

/// One occurrence of a persistent reader notice.
///
/// Every producer assignment mints a fresh occurrence, so two notices with the
/// same text published by different operations stay distinguishable. Equality
/// includes the occurrence: closing an older toast never clears a newer
/// occurrence of the same message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Notice {
    text: String,
    occurrence: u64,
}

impl Notice {
    pub(super) fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            occurrence: NEXT_NOTICE.fetch_add(1, Ordering::Relaxed),
        }
    }
}

impl std::ops::Deref for Notice {
    type Target = str;
    fn deref(&self) -> &str {
        &self.text
    }
}

impl std::fmt::Display for Notice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl From<String> for Notice {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl From<&str> for Notice {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

/// Scroll-only space: keep the last line reachable above the notification stack.
/// This never takes a layout row or changes the document viewport.
pub(super) fn bottom_space(window: &Window, cx: &App) -> Pixels {
    let height = window.viewport_size().height;
    if Root::read(window, cx)
        .notification
        .read(cx)
        .notifications()
        .is_empty()
    {
        reader_bottom_space(height)
    } else {
        reader_bottom_space(height).max(height * 0.65)
    }
}

pub(super) fn transient(message: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
    let _ = push(
        Notification::new().message(message),
        Some(Duration::from_secs(4)),
        window,
        cx,
    );
}

pub(super) fn missing_file(path: String, window: &mut Window, cx: &mut App) {
    let _ = push(
        Notification::new()
            .message("File not found")
            .action(move |_, _, cx| {
                let path = path.clone();
                reader_icon_button("copy-missing-file-path", IconName::Copy, "Copy path", cx)
                    .debug_selector(|| "copy-missing-file-path".into())
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(path.clone()))
                    })
            }),
        Some(Duration::from_secs(4)),
        window,
        cx,
    );
}

pub(super) fn error(message: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
    let key = ("reader-error", NEXT_TOAST.fetch_add(1, Ordering::Relaxed));
    let message = message.into();
    window.push_notification(
        Notification::new()
            .id1::<Toast>(key)
            .content(move |_, _, _| {
                div()
                    .debug_selector(|| "reader-error-toast".into())
                    .text_sm()
                    .child(message.clone())
                    .into_any_element()
            })
            .placement(Anchor::BottomRight)
            .py_2()
            .timeout(Duration::from_secs(8)),
        cx,
    );
}

pub(super) type ToastKey = (&'static str, u64);

pub(super) fn remove(key: ToastKey, window: &mut Window, cx: &mut App) {
    window.remove_notification1::<Toast>(key, cx);
}

pub(super) fn push(
    notification: Notification,
    lifetime: Option<Duration>,
    window: &mut Window,
    cx: &mut App,
) -> ToastKey {
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
    key
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
    fn sync_recovery_toast(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let recovery = (self.recovery_offer && !self.recovery_dismissed && self.editing.is_none())
            .then(|| {
                (
                    self.selected_file().to_owned(),
                    self.navigation.preparation_generation,
                )
            });
        if recovery != self.displayed_recovery {
            self.displayed_recovery = recovery.clone();
            // Editing with the restored draft (or another note) makes the
            // offer moot: it must not outlive «Unsaved changes restored».
            if let Some(key) = self.recovery_toast.take() {
                remove(key, window, cx);
            }
            self.recovery_toast_generation = self.recovery_toast_generation.wrapping_add(1);
            let queued = self.recovery_toast_generation;
            if let Some((path, generation)) = recovery {
                let reader = cx.weak_entity();
                let message = if self.recovery_startup {
                    "Unsaved edits from your previous session are available."
                } else {
                    "Unsaved edits are available."
                };
                let current = reader.clone();
                let offered = (path.clone(), generation);
                window.defer(cx, move |window, cx| {
                    // Startup can enter editing between this render and the
                    // deferred push; only show the offer if it still applies.
                    let still_offered = current
                        .read_with(cx, |this, _| {
                            this.recovery_toast_generation == queued
                                && this.displayed_recovery.as_ref() == Some(&offered)
                                && this.editing.is_none()
                        })
                        .unwrap_or(false);
                    if !still_offered {
                        return;
                    }
                    let key = push(
                        Notification::new()
                            .message(message)
                            .content(move |_, _, _| {
                                let reader = reader.clone();
                                let path = path.clone();
                                Button::new("restore-unsaved-edits")
                                    .debug_selector(|| "restore-unsaved-edits".into())
                                    .small()
                                    .label("Restore unsaved edits")
                                    .on_click(move |_, window, cx| {
                                        let _ = reader.update(cx, |this, cx| {
                                            if this.selected_file() == path
                                                && this.navigation.preparation_generation
                                                    == generation
                                                && this.recovery_offer
                                                && this.editing.is_none()
                                            {
                                                this.toggle_source(window, cx);
                                            }
                                        });
                                    })
                                    .into_any_element()
                            }),
                        Some(Duration::from_secs(4)),
                        window,
                        cx,
                    );
                    let _ = current.update(cx, |this, _| this.recovery_toast = Some(key));
                });
            }
        }
        #[cfg(any(unix, windows))]
        let history = self
            .active_timeline()
            .and_then(|t| t.message.clone().map(|m| (t.id, t.selection, m)));
        #[cfg(not(any(unix, windows)))]
        let history: Option<(uuid::Uuid, uuid::Uuid, String)> = None;
        if history != self.displayed_history_notice {
            self.displayed_history_notice = history.clone();
            self.history_notice_generation = self.history_notice_generation.wrapping_add(1);
            let generation = self.history_notice_generation;
            let reader = cx.weak_entity();
            window.defer(cx, move |window, cx| {
                let _ = reader.update(cx, |this, cx| {
                    if this.history_notice_generation != generation {
                        return;
                    }
                    window.remove_notification::<HistoryNotice>(cx);
                    if let Some((id, selection, message)) = history {
                        let reader = cx.weak_entity();
                        window.push_notification(
                            Notification::new()
                                .id::<HistoryNotice>()
                                .message(message.clone())
                                .placement(Anchor::BottomRight)
                                .py_2()
                                .autohide(false)
                                .on_close(move |_, cx| {
                                    let _ = reader.update(cx, |this, cx| {
                                        if this.history_notice_generation != generation {
                                            return;
                                        }
                                        #[cfg(any(unix, windows))]
                                        if let Some(t) = this.timeline.as_mut().filter(|t| {
                                            t.id == id
                                                && t.selection == selection
                                                && t.message.as_ref() == Some(&message)
                                        }) {
                                            t.message = None;
                                        }
                                        #[cfg(not(any(unix, windows)))]
                                        let _ = (id, selection, &message);
                                        this.displayed_history_notice = None;
                                        cx.notify();
                                    });
                                }),
                            cx,
                        );
                    }
                });
            });
        }
    }

    /// Existing error producers retain their state; presentation never takes a layout row.
    pub(super) fn sync_notice_toast(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        crate::updater::ready_notice(window, cx);
        self.sync_recovery_toast(window, cx);
        let notice = self.link_notice.clone();
        let choices = self.link_choices.clone();
        if notice == self.displayed_notice && choices == self.displayed_choices {
            return;
        }
        self.displayed_choices = choices.clone();
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
                    let actions_reader = reader.clone();
                    window.push_notification(
                        Notification::new()
                            .id::<LinkNotice>()
                            .message(message.text)
                            .content(move |_, _, _| {
                                let reader = actions_reader.clone();
                                v_flex()
                                    .gap_1()
                                    .children(choices.iter().enumerate().map(
                                        |(i, (path, heading))| {
                                            let reader = reader.clone();
                                            let path = path.clone();
                                            let heading = heading.clone();
                                            Button::new(("reader-link-choice", i))
                                                .small()
                                                .label(path.clone())
                                                .on_click(move |_, window, cx| {
                                                    let _ = reader.update(cx, |this, cx| {
                                                        if this.notice_generation == generation {
                                                            this.open_note_at(
                                                                &path,
                                                                None,
                                                                heading.as_deref(),
                                                                window,
                                                                cx,
                                                            );
                                                        }
                                                    });
                                                })
                                        },
                                    ))
                                    .into_any_element()
                            })
                            .placement(Anchor::BottomRight)
                            .py_2()
                            .autohide(false)
                            .on_close(move |_, cx| {
                                let _ = reader.update(cx, |this, cx| {
                                    if this.notice_generation == generation
                                        && this.link_notice.as_ref() == Some(&dismissed)
                                    {
                                        this.link_notice = None;
                                        this.link_choices.clear();
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
    fn local_file_links_missing_click_copies_path_without_navigation_or_reflow(
        cx: &mut TestAppContext,
    ) {
        use tessera_core::document_links::prepared::LinkStatus;
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("start.md"),
            "[Missing JSON](missing.json)\n\nStable paragraph",
        )
        .unwrap();
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
        let url = reader.read_with(visual, |r, _| {
            let (url, state) = r
                .prepared_links
                .iter()
                .find(|(_, s)| s.status == LinkStatus::MissingFile)
                .expect("background missing-file evidence");
            assert_eq!(state.reason, "File not found");
            let p = prepared_links::presentation(url, &r.prepared_links);
            assert!(!p.inert && p.style.color.is_some() && p.style.underline.is_some());
            assert!(p
                .tooltip
                .as_deref()
                .unwrap()
                .starts_with("File not found\n"));
            url.clone()
        });
        let before = visual.debug_bounds("reader-document").unwrap();
        let weak = reader.downgrade();
        visual.update(|window, cx| handle_link(&weak, &url, window, cx));
        visual.run_until_parked();
        visual.update(|window, cx| assert_eq!(window.notifications(cx).len(), 1));
        reader.read_with(visual, |r, _| {
            assert_eq!(r.current_rel, "start.md");
            assert!(r.file_menu.is_none() && r.file_preview.is_none());
        });
        assert_eq!(visual.debug_bounds("reader-document").unwrap(), before);
        let copy = visual
            .debug_bounds("copy-missing-file-path")
            .expect("copy action positive control")
            .center();
        visual.simulate_click(copy, gpui::Modifiers::default());
        visual.run_until_parked();
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                root.canonicalize()
                    .unwrap()
                    .join("missing.json")
                    .to_string_lossy()
            )
        });
        visual.executor().advance_clock(Duration::from_secs(5));
        visual.run_until_parked();
        visual.update(|window, cx| assert!(window.notifications(cx).is_empty()));
    }

    /// #930: the «Restore unsaved edits» offer disappears as soon as the
    /// note is being edited (startup restores the draft into the editor), and
    /// a push deferred past that moment never shows up.
    #[gpui::test]
    fn recovery_offer_goes_away_when_editing_starts(cx: &mut TestAppContext) {
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
        let restore_shown = |visual: &mut VisualTestContext| {
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.debug_bounds("restore-unsaved-edits").is_some()
        };
        let offer = |visual: &mut VisualTestContext| {
            reader.update_in(visual, |r, _, cx| {
                r.recovery_startup = true;
                r.recovery_checked = true;
                r.recovery_dismissed = false;
                r.recovery_offer = true;
                cx.notify();
            });
        };
        // Shown, then editing starts: the offer is removed at once, long
        // before its 4 s lifetime.
        offer(visual);
        visual.run_until_parked();
        assert!(
            restore_shown(visual),
            "positive control: the offer is shown"
        );
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        reader.read_with(visual, |r, _| assert!(r.editing.is_some()));
        // Let the removed notice finish its exit transition; still far
        // below the offer's own 4 s lifetime.
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        assert!(!restore_shown(visual), "offer removed when editing starts");
        // Back to Reader with a fresh offer whose push is deferred, and
        // editing starts in the same update: it must never appear.
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_secs(5));
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            r.recovery_offer = true;
            r.recovery_dismissed = false;
            r.sync_notice_toast(window, cx);
            r.toggle_source(window, cx);
        });
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        assert!(
            !restore_shown(visual),
            "a deferred offer is dropped once editing"
        );
        // A → B → A before the deferred push: only the last one may show.
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_secs(5));
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            r.recovery_offer = true;
            r.sync_notice_toast(window, cx);
            r.recovery_offer = false;
            r.sync_notice_toast(window, cx);
            r.recovery_offer = true;
            r.sync_notice_toast(window, cx);
        });
        visual.run_until_parked();
        visual.update(|window, cx| {
            assert_eq!(window.notifications(cx).len(), 1, "one offer, tracked")
        });
        reader.read_with(visual, |r, _| assert!(r.recovery_toast.is_some()));
    }

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
        // Errors have eight seconds of reading time; hovering pauses expiry.
        visual.update(|window, cx| error("Could not create the note", window, cx));
        visual.run_until_parked();
        visual.update(|window, cx| assert_eq!(window.notifications(cx).len(), 1));
        visual.executor().advance_clock(Duration::from_secs(7));
        visual.run_until_parked();
        let error_bounds = visual.debug_bounds("reader-error-toast").unwrap();
        visual.simulate_mouse_move(error_bounds.center(), None, gpui::Modifiers::default());
        visual.run_until_parked();
        // Let the stack sample its hover state before advancing the long interval.
        visual.executor().advance_clock(Duration::from_millis(100));
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_secs(20));
        visual.run_until_parked();
        visual.update(|window, cx| assert_eq!(window.notifications(cx).len(), 1));
        visual.simulate_mouse_move(point(px(10.), px(10.)), None, gpui::Modifiers::default());
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(100));
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_secs(2));
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(300));
        visual.run_until_parked();
        visual.update(|window, cx| assert!(window.notifications(cx).is_empty()));
        // Unclean launch without a newer draft is silent; an actual recovery offer
        // appears in the same overlay without changing document geometry.
        reader.update_in(visual, |r, _, cx| {
            r.recovery_startup = true;
            r.recovery_checked = true;
            r.recovery_offer = false;
            cx.notify();
        });
        visual.run_until_parked();
        visual.update(|window, cx| assert!(window.notifications(cx).is_empty()));
        assert_eq!(visual.debug_bounds("reader-document").unwrap(), before);
        reader.update_in(visual, |r, _, cx| {
            r.recovery_offer = true;
            cx.notify();
        });
        visual.run_until_parked();
        visual.update(|window, cx| assert_eq!(window.notifications(cx).len(), 1));
        assert_eq!(visual.debug_bounds("reader-document").unwrap(), before);
        visual.executor().advance_clock(Duration::from_secs(5));
        visual.run_until_parked();
        visual.update(|window, cx| assert!(window.notifications(cx).is_empty()));
        reader.update_in(visual, |r, _, cx| {
            r.recovery_offer = false;
            r.link_choices = vec![("start.md".into(), None)];
            r.link_notice = Some("Choose the matching note".into());
            cx.notify();
        });
        visual.run_until_parked();
        assert_eq!(visual.debug_bounds("reader-document").unwrap(), before);
        visual.update(|window, cx| assert_eq!(window.notifications(cx).len(), 1));
        reader.update_in(visual, |r, _, cx| {
            r.link_choices.clear();
            cx.notify();
        });
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

    #[gpui::test]
    fn closing_a_notice_keeps_a_new_occurrence_of_the_same_message(cx: &mut TestAppContext) {
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
        let message = "Cannot move: destination exists";

        // Positive control: an undisturbed dismissal clears the notice, so the
        // close callback demonstrably runs within the clock advanced below.
        reader.update_in(visual, |r, _, cx| {
            r.link_notice = Some(message.into());
            cx.notify();
        });
        visual.run_until_parked();
        visual.update(|window, cx| assert_eq!(window.notifications(cx).len(), 1));
        visual.update(|window, cx| assert!(dismiss(window, cx)));
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        reader.read_with(visual, |r, _| assert!(r.link_notice.is_none()));
        visual.update(|window, cx| assert!(window.notifications(cx).is_empty()));

        // Display A, start dismissing it, publish a new occurrence of A while
        // the old close transition is still running, then finish that transition.
        reader.update_in(visual, |r, _, cx| {
            r.link_notice = Some(message.into());
            cx.notify();
        });
        visual.run_until_parked();
        let first = reader.read_with(visual, |r, _| r.link_notice.clone().unwrap());
        visual.update(|window, cx| assert!(dismiss(window, cx)));
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert_eq!(r.link_notice.as_ref(), Some(&first))
        });
        let second = Notice::from(message);
        assert_eq!(&*second, &*first, "same message text");
        assert_ne!(second, first, "distinct occurrence");
        reader.update_in(visual, |r, _, cx| {
            r.link_notice = Some(second.clone());
            cx.notify();
        });
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert_eq!(
                r.link_notice.as_ref(),
                Some(&second),
                "new occurrence remains"
            )
        });
        visual.update(|window, cx| assert_eq!(window.notifications(cx).len(), 1));

        // The new occurrence still dismisses normally.
        visual.update(|window, cx| assert!(dismiss(window, cx)));
        visual.executor().advance_clock(Duration::from_secs(1));
        visual.run_until_parked();
        reader.read_with(visual, |r, _| assert!(r.link_notice.is_none()));
        visual.update(|window, cx| assert!(window.notifications(cx).is_empty()));
    }
}
