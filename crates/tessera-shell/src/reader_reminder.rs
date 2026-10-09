//! Remind me on <date>: a selected date becomes a Tasks line in the reminders
//! note (#724). The note is plain Markdown; nothing here is a private database.
use super::*;
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::{notification::Notification, WindowExt};
#[cfg(any(unix, windows))]
use tessera_core::reminder_append::write::{self, Receipt};
use tessera_core::{reminder_context, reminder_dates, reminder_task};

/// Vault-relative reminders note. Settings will make this configurable.
pub(super) const NOTE: &str = "Reminders.md";

struct ReminderToast;

fn today() -> time::Date {
    time::OffsetDateTime::now_local()
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
        .date()
}

/// `Sun, 1 Nov 2026`: the resolved date, so the user sees what a year-less
/// selection became before choosing it.
pub(super) fn date_label(date: time::Date) -> String {
    const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{}, {} {} {}",
        DAYS[date.weekday().number_days_from_monday() as usize],
        date.day(),
        MONTHS[date.month() as usize - 1],
        date.year()
    )
}

/// Add the reminder entry to a context menu when the selection is a whole date.
#[cfg(any(unix, windows))]
pub(super) fn menu(menu: PopupMenu, reader: &WeakEntity<Reader>, cx: &App) -> PopupMenu {
    let Some(view) = reader.upgrade() else {
        return menu;
    };
    let Some((selection, date)) = view.read(cx).reminder_for_selection(cx) else {
        return menu;
    };
    let reader = reader.clone();
    let label = format!("Remind me on {}", date_label(date));
    menu.item(
        PopupMenuItem::element(move |_, _| {
            div()
                .debug_selector(|| "remind-me-item".into())
                .child(label.clone())
        })
        .on_click(move |_, window, cx| {
            let selection = selection.clone();
            let _ = reader.update(cx, |this, cx| {
                this.add_reminder(selection, date, window, cx)
            });
        }),
    )
}

#[cfg(not(any(unix, windows)))]
pub(super) fn menu(menu: PopupMenu, _: &WeakEntity<Reader>, _: &App) -> PopupMenu {
    menu
}

#[cfg(any(unix, windows))]
impl Reader {
    /// The selected text and its date, when the whole selection is one. The
    /// action needs a vault note to link back to: not single-file mode, and not
    /// an attachment preview.
    pub(super) fn reminder_for_selection(&self, cx: &App) -> Option<(String, time::Date)> {
        if self.single_file || self.file_preview.is_some() || self.current_rel.is_empty() {
            return None;
        }
        let selection = self.content.read(cx).selected_text();
        let date = reminder_dates::parse(&selection, today())?;
        Some((selection, date))
    }

    pub(super) fn add_reminder(
        &mut self,
        selection: String,
        date: time::Date,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drafts) = self
            .session_directory
            .as_ref()
            .map(|state| state.join("editor-drafts"))
        else {
            reader_toast::transient("Reminders need Tessera's app storage", window, cx);
            return;
        };
        let source = self
            .note_canonical_source
            .as_deref()
            .unwrap_or(&self.note_source);
        let found = reminder_context::locate(source, &selection);
        // Without a verified sentence the reminder names the note, not a guess.
        let sentence = found.sentence.unwrap_or_else(|| {
            if self.current_title.is_empty() {
                reader_move::display_name(&self.current_rel)
            } else {
                self.current_title.clone()
            }
        });
        let line = match reminder_task::format(
            &sentence,
            &self.current_rel,
            found.heading.as_deref(),
            date,
        ) {
            Ok(line) => line,
            Err(message) => {
                reader_toast::transient(message, window, cx);
                return;
            }
        };
        let root = self.vault_root.clone();
        let task = cx.background_executor().spawn({
            let (root, drafts) = (root.clone(), drafts.clone());
            async move { write::add(&root, &drafts, NOTE, &line) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(receipt) => this.announce_reminder(receipt, root, drafts, date, window, cx),
                Err(error) => {
                    reader_toast::transient(format!("Reminder not added: {error:#}"), window, cx)
                }
            });
        })
        .detach();
    }

    fn announce_reminder(
        &mut self,
        receipt: Receipt,
        root: PathBuf,
        drafts: PathBuf,
        date: time::Date,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let receipt = Arc::new(receipt);
        self.reminder_undo = Some(receipt.clone());
        let reader = cx.weak_entity();
        let announced = receipt.clone();
        window.push_notification(
            Notification::new()
                .id::<ReminderToast>()
                .message(format!("Reminder added for {}", date_label(date)))
                .placement(Anchor::BottomRight)
                .py_2()
                .autohide(false)
                .action(move |_, _, cx| {
                    let (reader, announced) = (reader.clone(), announced.clone());
                    let (root, drafts) = (root.clone(), drafts.clone());
                    reader_icon_button("undo-reminder", IconName::Undo2, "Undo", cx)
                        .debug_selector(|| "undo-reminder".into())
                        .on_click(move |_, window, cx| {
                            let _ = reader.update(cx, |this, cx| {
                                this.undo_reminder(&announced, &root, &drafts, window, cx)
                            });
                        })
                }),
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(8)).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this
                    .reminder_undo
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &receipt))
                {
                    window.remove_notification::<ReminderToast>(cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn undo_reminder(
        &mut self,
        announced: &Arc<Receipt>,
        root: &Path,
        drafts: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Only the most recent toast may undo; an older one is already stale.
        if !self
            .reminder_undo
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, announced))
        {
            return;
        }
        self.reminder_undo = None;
        window.remove_notification::<ReminderToast>(cx);
        let (receipt, root, drafts) = (announced.clone(), root.to_owned(), drafts.to_owned());
        let task = cx
            .background_executor()
            .spawn(async move { receipt.undo(&root, &drafts) });
        cx.spawn_in(window, async move |_, cx| {
            let result = task.await;
            let _ = cx.update(|window, cx| match result {
                Ok(()) => reader_toast::transient("Reminder removed", window, cx),
                Err(error) => reader_toast::transient(
                    format!("Cannot undo: {error:#}. The reminders note was kept."),
                    window,
                    cx,
                ),
            });
        })
        .detach();
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    // No glob import: it would bring gpui's `test` macro over the built-in one.
    use super::date_label;
    use time::macros::date;

    #[test]
    fn label_shows_weekday_month_and_year() {
        assert_eq!(date_label(date!(2026 - 11 - 01)), "Sun, 1 Nov 2026");
        assert_eq!(date_label(date!(2027 - 01 - 31)), "Sun, 31 Jan 2027");
        assert_eq!(date_label(date!(2026 - 12 - 07)), "Mon, 7 Dec 2026");
    }
}

#[cfg(all(test, any(unix, windows)))]
mod visual_tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn mount<'a>(
        cx: &'a mut TestAppContext,
        root: &Path,
        state: &Path,
    ) -> (Entity<Reader>, &'a mut VisualTestContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.to_owned()),
                        note: Some("start.md".into()),
                        session_directory: Some(state.to_owned()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        visual.run_until_parked();
        (reader.unwrap(), visual)
    }

    #[gpui::test]
    fn selected_date_survives_right_click_and_becomes_one_undoable_task(cx: &mut TestAppContext) {
        let temp = std::env::temp_dir().join(format!("tessera-remind-{}", uuid::Uuid::new_v4()));
        let (root, state) = (temp.join("vault"), temp.join("state"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(
            root.join("start.md"),
            "2026-11-01\n\nA second paragraph that is not a date.\n",
        )
        .unwrap();
        let (reader, visual) = mount(cx, &root, &state);

        let bounds = reader.read_with(visual, |v, cx| v.content.read(cx).bounds());
        // A selection that is more than a date must not offer the action. The
        // positive case below proves this probe would see the item if present.
        reader.update(visual, |v, cx| {
            v.content.update(cx, |content, cx| content.select_all(cx))
        });
        let at = bounds.origin + point(px(20.), px(10.));
        visual.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
        visual.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("remind-me-item").is_none());
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();

        // Positive control: a real drag selects exactly the date line.
        let start = bounds.origin + point(px(2.), px(10.));
        let end = bounds.origin + point(px(400.), px(10.));
        visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        let selected = reader.read_with(visual, |v, cx| v.content.read(cx).selected_text());
        assert_eq!(selected.trim(), "2026-11-01", "drag precondition");

        // The right click that opens the menu must not discard the selection.
        let inside = bounds.origin + point(px(20.), px(10.));
        visual.simulate_mouse_down(inside, MouseButton::Right, Modifiers::default());
        visual.simulate_mouse_up(inside, MouseButton::Right, Modifiers::default());
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("remind-me-item").is_some(),
            "the context menu offers the reminder for a selected date"
        );
        let offered = reader.read_with(visual, |v, cx| v.reminder_for_selection(cx));
        let (selection, date) = offered.expect("the date selection offers a reminder");
        assert_eq!(selection.trim(), "2026-11-01");
        assert_eq!(date, time::macros::date!(2026 - 11 - 01));

        reader.update_in(visual, |v, window, cx| {
            v.add_reminder(selection, date, window, cx)
        });
        visual.run_until_parked();
        let note = root.join(NOTE);
        let expected = "- [ ] 2026-11-01 [[start.md]] 📅 2026-11-01\n";
        assert_eq!(std::fs::read_to_string(&note).unwrap(), expected);
        assert_eq!(
            tessera_core::tasks::parse(NOTE, expected).len(),
            1,
            "the line is an ordinary Tasks item"
        );

        let receipt = reader
            .read_with(visual, |v, _| v.reminder_undo.clone())
            .expect("a successful save issues an Undo receipt");
        reader.update_in(visual, |v, window, cx| {
            v.undo_reminder(&receipt, &root, &state.join("editor-drafts"), window, cx)
        });
        visual.run_until_parked();
        assert_eq!(std::fs::read_to_string(&note).unwrap(), "");
        assert!(reader.read_with(visual, |v, _| v.reminder_undo.is_none()));

        // Negative control: a selection that is more than a date offers nothing.
        reader.update(visual, |v, cx| {
            v.content.update(cx, |content, cx| content.select_all(cx))
        });
        visual.run_until_parked();
        let everything = reader.read_with(visual, |v, cx| v.content.read(cx).selected_text());
        assert!(everything.contains("second paragraph"), "{everything:?}");
        assert!(reader
            .read_with(visual, |v, cx| v.reminder_for_selection(cx))
            .is_none());
        std::fs::remove_dir_all(temp).unwrap();
    }
}
