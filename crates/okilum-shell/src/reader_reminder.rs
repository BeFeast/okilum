//! Remind me on <date>: a selected date becomes a Tasks line in the reminders
//! note (#724). The note is plain Markdown; nothing here is a private database.
use super::*;
use gpui_component::input::EditorState;
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::native_menu::NativeMenu;
use gpui_component::{notification::Notification, WindowExt};
#[cfg(any(unix, windows))]
use okilum_core::reminder_append::write::{self, Receipt};
use okilum_core::{reminder_context, reminder_dates, reminder_task};

gpui::actions!(reader_reminder, [RemindOnSelection]);

/// The default reminders note; Settings can choose another per vault.
#[cfg(test)]
pub(super) const NOTE: &str = reader_reminder_settings::DEFAULT_NOTE;

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
                this.add_reminder(selection, date, None, window, cx)
            });
        }),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    GoToDefinition,
    ShowCodeActions,
    Cut,
    Copy,
    Paste,
    SelectAll,
    Remind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Entry {
    Item {
        label: String,
        disabled: bool,
        command: Command,
    },
    Separator,
}

/// The source editor's context menu as plain data. `Editor::context_menu`
/// replaces the built-in menu, so its items are reproduced from the same
/// capabilities; the reminder entry follows when the selection is a whole date.
fn editor_entries(
    capabilities: &gpui_base::input::InputContextMenuCapabilities,
    clipboard: bool,
    date: Option<time::Date>,
) -> Vec<Entry> {
    let item = |label: &str, disabled, command| Entry::Item {
        label: label.into(),
        disabled,
        command,
    };
    let enabled = !capabilities.is_disabled();
    let editable = enabled && !capabilities.is_readonly();
    let mut entries = Vec::new();
    // Language-server items appear only where a server provides them; notes
    // never have one, so the menu shows text actions only (#1010).
    if capabilities.is_code_editor() && capabilities.has_definition() {
        entries.push(item("Go to Definition", !enabled, Command::GoToDefinition));
    }
    if capabilities.is_code_editor() && capabilities.has_code_actions() {
        entries.push(item(
            "Show Code Actions",
            !editable,
            Command::ShowCodeActions,
        ));
    }
    if !entries.is_empty() {
        entries.push(Entry::Separator);
    }
    entries.push(item(
        "Cut",
        !(editable && capabilities.is_copyable()),
        Command::Cut,
    ));
    entries.push(item("Copy", !capabilities.is_copyable(), Command::Copy));
    entries.push(item("Paste", !(editable && clipboard), Command::Paste));
    entries.push(Entry::Separator);
    entries.push(item("Select All", false, Command::SelectAll));
    if let Some(date) = date {
        entries.push(Entry::Separator);
        entries.push(item(
            &format!("Remind me on {}", date_label(date)),
            false,
            Command::Remind,
        ));
    }
    entries
}

#[cfg(test)]
thread_local! {
    /// How many times the real builder ran: a right-click test must prove the
    /// callback was reached, or "no panic" would prove nothing.
    pub(super) static MENU_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A selection longer than this cannot be a date, so it is never read out of
/// the document.
const MAX_DATE_BYTES: usize = 96;

/// Plain `Copy` data on purpose: the builder reads it with `Cell::get` and holds
/// no borrow. Keep it that way; a `RefCell` borrow held across the menu's
/// construction is the kind of re-entrancy that caused #955.
#[derive(Clone, Copy, Default)]
struct Facts {
    capabilities: gpui_base::input::InputContextMenuCapabilities,
    date: Option<time::Date>,
}

/// What the source editor's menu needs to know about the editor, kept current
/// outside the editor's own updates. `Editor::context_menu` runs its builder
/// while the editor state is being updated (the vendor defers it with
/// `defer_in`), where reading that state panics (#955). Observers run after an
/// update, when reading is allowed, so the builder only reads this snapshot.
#[derive(Clone, Default)]
pub(super) struct MenuFacts(std::rc::Rc<std::cell::Cell<Facts>>);

impl MenuFacts {
    #[cfg(test)]
    pub(super) fn date(&self) -> Option<time::Date> {
        self.0.get().date
    }

    pub(super) fn watch(
        input: &Entity<EditorState>,
        cx: &mut Context<Reader>,
    ) -> (Self, Subscription) {
        let facts = Self::default();
        facts.refresh(input, cx);
        let mirror = facts.clone();
        let subscription = cx.observe(input, move |_, input, cx| mirror.refresh(&input, cx));
        (facts, subscription)
    }

    fn refresh(&self, input: &Entity<EditorState>, cx: &App) {
        let state = input.read(cx);
        #[cfg(any(unix, windows))]
        let date = {
            let range = state.selected_range();
            (!range.is_empty() && range.len() <= MAX_DATE_BYTES)
                .then(|| reminder_dates::parse(&state.selected_text().to_string(), today()))
                .flatten()
        };
        #[cfg(not(any(unix, windows)))]
        let date = None;
        self.0.set(Facts {
            capabilities: state.context_menu_capabilities(),
            date,
        });
    }
}

pub(super) fn editor_menu(menu: NativeMenu, facts: &MenuFacts, cx: &App) -> NativeMenu {
    use gpui_base::input::{Copy, Cut, GoToDefinition, Paste, SelectAll, ToggleCodeActions};
    #[cfg(test)]
    MENU_BUILDS.with(|builds| builds.set(builds.get() + 1));
    let Facts { capabilities, date } = facts.0.get();
    editor_entries(&capabilities, cx.read_from_clipboard().is_some(), date)
        .into_iter()
        .fold(menu, |menu, entry| match entry {
            Entry::Separator => menu.separator(),
            Entry::Item {
                label,
                disabled,
                command,
            } => {
                let action: Box<dyn gpui::Action> = match command {
                    Command::GoToDefinition => Box::new(GoToDefinition),
                    Command::ShowCodeActions => Box::new(ToggleCodeActions),
                    Command::Cut => Box::new(Cut),
                    Command::Copy => Box::new(Copy),
                    Command::Paste => Box::new(Paste),
                    Command::SelectAll => Box::new(SelectAll),
                    Command::Remind => Box::new(RemindOnSelection),
                };
                menu.menu_with_disabled(label, disabled, action)
            }
        })
}

/// The editor's selection and its date, when the whole selection is one.
#[cfg(any(unix, windows))]
fn selected_date(state: &Entity<EditorState>, cx: &App) -> Option<(String, time::Date)> {
    let selection = state.read(cx).selected_text().to_string();
    let date = reminder_dates::parse(&selection, today())?;
    Some((selection, date))
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
    /// A link hovered in this document; one left over from a document that
    /// navigation replaced does not count.
    pub(super) fn pointer_on_link(&self) -> bool {
        self.pointer_link
            .as_ref()
            .is_some_and(|(_, generation)| *generation == self.navigation.preparation_generation)
    }

    /// The hovered link when text is selected: the only case where the
    /// TextView's own link click does not run (#943).
    pub(super) fn hovered_link_with_selection(&self, cx: &App) -> Option<String> {
        if !self.pointer_on_link() || self.content.read(cx).selected_text().is_empty() {
            return None;
        }
        self.pointer_link.as_ref().map(|(url, _)| url.clone())
    }

    pub(super) fn reminder_for_selection(&self, cx: &App) -> Option<(String, time::Date)> {
        // A right-click on a link belongs to the link's own menu (#943).
        if self.single_file
            || self.file_preview.is_some()
            || self.current_rel.is_empty()
            || self.pointer_on_link()
        {
            return None;
        }
        let selection = self.content.read(cx).selected_text();
        let date = reminder_dates::parse(&selection, today())?;
        Some((selection, date))
    }

    /// The `RemindOnSelection` action from the source editor's menu.
    pub(super) fn remind_on_editor_selection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.single_file || self.current_rel.is_empty() {
            return;
        }
        let Some(editing) = self.editing.as_ref() else {
            return;
        };
        let Some((selection, date)) = selected_date(editing.input(), cx) else {
            return;
        };
        let text = editing.input().read(cx).value().to_string();
        self.add_reminder(selection, date, Some(text), window, cx);
    }

    /// `source` is the text the selection was made in when it differs from the
    /// saved note (the source editor holds unsaved edits).
    pub(super) fn add_reminder(
        &mut self,
        selection: String,
        date: time::Date,
        source: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drafts) = self
            .session_directory
            .as_ref()
            .map(|state| state.join("editor-drafts"))
        else {
            reader_toast::transient("Reminders need Okilum's app storage", window, cx);
            return;
        };
        let source = source.as_deref().unwrap_or_else(|| {
            self.note_canonical_source
                .as_deref()
                .unwrap_or(&self.note_source)
        });
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
        let note = self.reminder_prefs.note.clone();
        let task = cx.background_executor().spawn({
            let (root, drafts) = (root.clone(), drafts.clone());
            async move { write::add(&root, &drafts, &note, &line) }
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
        let temp = std::env::temp_dir().join(format!("okilum-remind-{}", uuid::Uuid::new_v4()));
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
            v.add_reminder(selection, date, None, window, cx)
        });
        visual.run_until_parked();
        let note = root.join(NOTE);
        let expected = "- [ ] 2026-11-01 [[start.md]] 📅 2026-11-01\n";
        assert_eq!(std::fs::read_to_string(&note).unwrap(), expected);
        assert_eq!(
            okilum_core::tasks::parse(NOTE, expected).len(),
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

    /// #943: with a date selected, a right-click on a link opens only the
    /// link's menu; the reminder entry stays with the selection.
    #[gpui::test]
    fn right_click_on_a_link_opens_one_menu_while_a_date_is_selected(cx: &mut TestAppContext) {
        let temp = std::env::temp_dir().join(format!("okilum-remind-{}", uuid::Uuid::new_v4()));
        let (root, state) = (temp.join("vault"), temp.join("state"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(root.join("other.md"), "# Other\n").unwrap();
        std::fs::write(
            root.join("start.md"),
            "2026-11-01\n\n[Other note](other.md) and more words after the link.\n",
        )
        .unwrap();
        let (reader, visual) = mount(cx, &root, &state);
        let bounds = reader.read_with(visual, |v, cx| v.content.read(cx).bounds());
        let start = bounds.origin + point(px(2.), px(10.));
        let end = bounds.origin + point(px(400.), px(10.));
        visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        let selected = reader.read_with(visual, |v, cx| v.content.read(cx).selected_text());
        assert_eq!(selected.trim(), "2026-11-01", "drag precondition");
        // Find the link by real hover events, not by guessed coordinates.
        let link = (20..200).step_by(4).find_map(|y| {
            let at = bounds.origin + point(px(20.), px(y as f32));
            visual.simulate_mouse_move(at, None, Modifiers::default());
            visual.run_until_parked();
            reader
                .read_with(visual, |v, _| v.pointer_on_link())
                .then_some(at)
        });
        let link = link.expect("positive control: hovering the link is reported");
        visual.simulate_mouse_down(link, MouseButton::Right, Modifiers::default());
        visual.simulate_mouse_up(link, MouseButton::Right, Modifiers::default());
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("remind-me-item").is_none(),
            "no reminder menu for a right-click on a link"
        );
        assert!(
            reader.read_with(visual, |v, _| v.file_menu.is_some()),
            "the link's own menu opened"
        );
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        // A link left under the pointer by navigation is stale (review).
        reader.update(visual, |v, _| {
            v.navigation.preparation_generation =
                v.navigation.preparation_generation.wrapping_add(1)
        });
        assert!(!reader.read_with(visual, |v, _| v.pointer_on_link()));
        reader.update(visual, |v, _| {
            v.navigation.preparation_generation =
                v.navigation.preparation_generation.wrapping_sub(1)
        });
        // Positive control: on the selection the reminder is still offered.
        let inside = bounds.origin + point(px(20.), px(10.));
        visual.simulate_mouse_move(inside, None, Modifiers::default());
        visual.run_until_parked();
        visual.simulate_mouse_down(inside, MouseButton::Right, Modifiers::default());
        visual.simulate_mouse_up(inside, MouseButton::Right, Modifiers::default());
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("remind-me-item").is_some(),
            "the selection still offers the reminder"
        );
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[gpui::test]
    fn add_reminder_writes_to_the_configured_note_not_the_default(cx: &mut TestAppContext) {
        let temp = std::env::temp_dir().join(format!("okilum-remind-{}", uuid::Uuid::new_v4()));
        let (root, state) = (temp.join("vault"), temp.join("state"));
        std::fs::create_dir_all(root.join("Inbox")).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(root.join("start.md"), "2026-11-01\n").unwrap();
        let (reader, visual) = mount(cx, &root, &state);
        reader.update(visual, |v, cx| {
            let mut prefs = v.reminder_prefs.clone();
            prefs.note = "Inbox/Remind.md".into();
            v.set_reminder_prefs(prefs, cx);
        });
        reader.update_in(visual, |v, window, cx| {
            v.add_reminder(
                "2026-11-01".into(),
                time::macros::date!(2026 - 11 - 01),
                None,
                window,
                cx,
            )
        });
        visual.run_until_parked();
        let line = std::fs::read_to_string(root.join("Inbox/Remind.md")).unwrap();
        assert!(
            line.starts_with("- [ ] 2026-11-01 [[start.md]] 📅 2026-11-01"),
            "{line}"
        );
        assert!(!root.join(NOTE).exists(), "the default note is not created");
        std::fs::remove_dir_all(temp).unwrap();
    }

    /// #955: Editor::context_menu runs its builder while the editor's own state
    /// is being updated (the vendor defers it with `defer_in`), so the builder
    /// must not read that state. Right-click it for real, once per app: the menu
    /// stays open afterwards and would swallow a second click.
    fn right_click_in_the_source_editor(
        cx: &mut TestAppContext,
        range: std::ops::Range<usize>,
        date: Option<time::Date>,
    ) {
        let temp = std::env::temp_dir().join(format!("okilum-remind-{}", uuid::Uuid::new_v4()));
        let (root, state) = (temp.join("vault"), temp.join("state"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(root.join("start.md"), "Winter 2026-11-01 time\n").unwrap();
        let (reader, visual) = mount(cx, &root, &state);
        visual.simulate_resize(size(px(1400.), px(960.)));
        reader.update_in(visual, |v, window, cx| v.toggle_source(window, cx));
        visual.run_until_parked();
        let input = reader.read_with(visual, |v, _| v.editing.as_ref().unwrap().input().clone());
        let bounds = input.read_with(visual, |i, _| i.input_bounds());
        assert!(
            bounds.size.width > px(100.),
            "the editor is laid out: {bounds:?}"
        );
        input.update(visual, |i, cx| i.set_selected_range(range, cx));
        visual.run_until_parked();
        let mirrored = reader.read_with(visual, |v, _| {
            v.editing.as_ref().unwrap().menu_facts().date()
        });
        assert_eq!(mirrored, date, "the snapshot follows the selection");
        let before = MENU_BUILDS.with(|builds| builds.get());
        let at = bounds.origin + point(px(40.), px(12.));
        visual.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
        visual.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            MENU_BUILDS.with(|builds| builds.get()) - before,
            1,
            "positive control: the real builder ran for the right-click"
        );
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[gpui::test]
    fn right_click_on_a_selected_date_reaches_the_menu_builder_without_panicking(
        cx: &mut TestAppContext,
    ) {
        right_click_in_the_source_editor(cx, 7..17, Some(time::macros::date!(2026 - 11 - 01)));
    }

    #[gpui::test]
    fn right_click_with_nothing_selected_reaches_the_menu_builder_without_panicking(
        cx: &mut TestAppContext,
    ) {
        right_click_in_the_source_editor(cx, 0..0, None);
    }

    #[gpui::test]
    fn source_editor_action_writes_the_selected_date_and_leaves_the_note_alone(
        cx: &mut TestAppContext,
    ) {
        let temp = std::env::temp_dir().join(format!("okilum-remind-{}", uuid::Uuid::new_v4()));
        let (root, state) = (temp.join("vault"), temp.join("state"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        let note = "Winter time starts 2026-11-01 in Israel.\n";
        std::fs::write(root.join("start.md"), note).unwrap();
        let (reader, visual) = mount(cx, &root, &state);
        reader.update_in(visual, |v, window, cx| v.toggle_source(window, cx));
        visual.run_until_parked();
        let input = reader.read_with(visual, |v, _| v.editing.as_ref().unwrap().input().clone());

        // The menu builder sees the same selection the action will use.
        let offered = |visual: &mut VisualTestContext, range: std::ops::Range<usize>| {
            input.update(visual, |i, cx| i.set_selected_range(range, cx));
            visual.update(|_, cx| selected_date(&input, cx))
        };
        assert!(offered(visual, 0..6).is_none(), "plain words offer nothing");
        let (selection, date) = offered(visual, 19..29).expect("a whole date is offered");
        assert_eq!(
            (selection.as_str(), date),
            ("2026-11-01", time::macros::date!(2026 - 11 - 01))
        );

        // Dispatching the menu's action writes one task and touches nothing else.
        input.update_in(visual, |i, window, cx| i.focus(window, cx));
        visual.dispatch_action(RemindOnSelection);
        visual.run_until_parked();
        assert_eq!(
            std::fs::read_to_string(root.join(NOTE)).unwrap(),
            "- [ ] Winter time starts 2026-11-01 in Israel. [[start.md]] 📅 2026-11-01\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("start.md")).unwrap(),
            note
        );
        assert!(reader.read_with(visual, |v, cx| !v.source_is_dirty(cx)));

        // Without a date selection the same action does nothing.
        std::fs::remove_file(root.join(NOTE)).unwrap();
        input.update(visual, |i, cx| i.set_selected_range(0..6, cx));
        visual.dispatch_action(RemindOnSelection);
        visual.run_until_parked();
        assert!(!root.join(NOTE).exists());
        std::fs::remove_dir_all(temp).unwrap();
    }
}

#[cfg(test)]
mod menu_tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui_base::input::InputContextMenuCapabilities as Capabilities;

    fn labels(entries: &[Entry]) -> Vec<(String, bool)> {
        entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Item {
                    label, disabled, ..
                } => Some((label.clone(), *disabled)),
                Entry::Separator => None,
            })
            .collect()
    }

    #[test]
    fn builtin_items_follow_the_editor_capabilities() {
        // A note editor is a code editor without a language server (#1010).
        let editor = Capabilities::new().code_editor(true).selection(true);
        let plain = labels(&editor_entries(&editor, true, None));
        let names: Vec<_> = plain.iter().map(|(label, _)| label.as_str()).collect();
        assert_eq!(names, ["Cut", "Copy", "Paste", "Select All"]);
        assert_ne!(
            editor_entries(&editor, true, None).first(),
            Some(&Entry::Separator),
            "no leading separator without server items"
        );
        // Positive control: a server's capabilities bring the items back.
        let served = editor.go_to_definition(true).code_actions(true);
        let served = labels(&editor_entries(&served, true, None));
        let names: Vec<_> = served.iter().map(|(label, _)| label.as_str()).collect();
        assert_eq!(
            names,
            [
                "Go to Definition",
                "Show Code Actions",
                "Cut",
                "Copy",
                "Paste",
                "Select All"
            ]
        );
        assert!(served.contains(&("Go to Definition".into(), false)));
        assert!(plain.contains(&("Copy".into(), false)));
        assert!(plain.contains(&("Cut".into(), false)));
        assert!(plain.contains(&("Paste".into(), false)));

        // No selection or clipboard disables the matching items; read-only
        // rejects what would change the text.
        let idle = labels(&editor_entries(
            &Capabilities::new().code_editor(true),
            false,
            None,
        ));
        for name in ["Cut", "Copy", "Paste"] {
            assert!(idle.contains(&(name.into(), true)), "{name} disabled");
        }
        let readonly = Capabilities::new().selection(true).readonly(true);
        let readonly = labels(&editor_entries(&readonly, true, None));
        assert!(readonly.contains(&("Cut".into(), true)));
        assert!(readonly.contains(&("Paste".into(), true)));
        assert!(readonly.contains(&("Copy".into(), false)));
        assert!(!readonly
            .iter()
            .any(|(label, _)| label == "Go to Definition"));
    }

    #[test]
    fn a_date_adds_exactly_one_reminder_entry_at_the_end() {
        let editor = Capabilities::new().code_editor(true).selection(true);
        let without = editor_entries(&editor, true, None);
        let with = editor_entries(&editor, true, Some(time::macros::date!(2026 - 11 - 01)));
        assert_eq!(with[..without.len()], without[..]);
        assert_eq!(with.len(), without.len() + 2);
        assert_eq!(with[with.len() - 2], Entry::Separator);
        assert_eq!(
            with.last(),
            Some(&Entry::Item {
                label: "Remind me on Sun, 1 Nov 2026".into(),
                disabled: false,
                command: Command::Remind,
            })
        );
    }
}
