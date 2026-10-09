//! Desktop notifications for reminders (#724). The reminders note stays the
//! source of truth; this only decides when to tell the user, through the pure
//! policy in `okilum_core::reminder_schedule`, and records what it already
//! delivered in a ledger outside the vault and outside the disposable index.
//!
//! One window per vault acts (the owner); other windows of the same vault stay
//! quiet, so a duplicate window never doubles a notification. The scheduler
//! reads the already-published task index: no second vault scan or watcher.
use super::*;
use gpui_component::{notification::Notification, WindowExt as _};
use okilum_core::reminder_schedule::{self as schedule, Ledger, Policy};
use std::collections::HashMap;

/// Identifies a vault's reminder notification to gpui-component, which owns the
/// delivery: a repeat for the same vault replaces the earlier one.
struct ReminderNotice;
/// A sleeping machine or a changed clock must not leave a stale long timer.
const MAX_WAIT: Duration = Duration::from_secs(15 * 60);
const MIN_WAIT: Duration = Duration::from_secs(1);
const RETRY: Duration = Duration::from_secs(5 * 60);

#[derive(Default)]
pub(super) struct Notifier {
    /// `None` until read from disk. Replaced only after a successful write.
    ledger: Option<Ledger>,
    busy: bool,
    /// An evaluation was requested while a read or write was in flight.
    again: bool,
    /// The watched note changed: re-import silently on the next evaluation.
    rebaseline: bool,
    generation: u64,
}

impl Notifier {
    /// The watched note changed: the next evaluation re-imports silently.
    pub(super) fn note_changed(&mut self) {
        self.rebaseline = true;
    }
}

/// Vault root to the window that delivers its notifications.
#[derive(Default)]
struct Owners(HashMap<PathBuf, WeakEntity<Reader>>);
impl gpui::Global for Owners {}

#[cfg(test)]
thread_local! {
    pub(super) static TEST_NOW: std::cell::Cell<Option<time::PrimitiveDateTime>> =
        const { std::cell::Cell::new(None) };
}

fn local_now() -> time::PrimitiveDateTime {
    #[cfg(test)]
    if let Some(now) = TEST_NOW.with(|now| now.get()) {
        return now;
    }
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    time::PrimitiveDateTime::new(now.date(), now.time())
}

pub(super) fn key(root: &Path) -> String {
    let keyed = reader_sidebar::State::path(Path::new(""), root);
    keyed
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Delivery bookkeeping lives with app state, never inside the vault.
fn ledger_path(state: &Path, root: &Path) -> PathBuf {
    state.join("reminders").join(format!("{}.json", key(root)))
}

fn load(path: &Path) -> Ledger {
    match std::fs::read_to_string(path) {
        Ok(json) => Ledger::from_json(&json).unwrap_or_else(|error| {
            // Rebuilt silently by the next evaluation: never replay old reminders.
            eprintln!("Reminder ledger ignored: {error:#}");
            Ledger::default()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ledger::default(),
        Err(error) => {
            eprintln!("Reminder ledger unreadable: {error:#}");
            Ledger::default()
        }
    }
}

pub(super) fn store(path: &Path, json: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let directory = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(directory)?;
    let temporary = directory.join(format!(".ledger-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Name the app to the notification centre. Platforms drop notifications until
/// the identity is set (Windows AppUserModelID, the name shown to the user), so
/// it is set here, once, before any window posts one.
///
/// This registers no response handler, on purpose: gpui keeps a single one and
/// gpui-component owns it, so a handler installed here before
/// `gpui_component::init` is silently replaced and a click does nothing (#961).
/// The click is routed by the notification itself, see `deliver`.
pub(super) fn install(cx: &mut App) {
    cx.set_app_identity("com.befeast.okilum", "Okilum");
}

#[cfg(any(unix, windows))]
impl Reader {
    /// Evaluate reminders now and arm the next evaluation. Cheap when nothing
    /// changed; safe to call whenever the task index is published.
    pub(super) fn reminder_tick(&mut self, cx: &mut Context<Self>) {
        if self.single_file || self.incremental_initializing {
            return;
        }
        if self.reminder_notifier.busy {
            self.reminder_notifier.again = true;
            return;
        }
        // Without a published index an empty note would be indistinguishable
        // from a note not read yet, and the first import must see the real one.
        let (Some(state), Some(index)) = (self.session_directory.clone(), self.tasks_index.clone())
        else {
            return;
        };
        if !self.claim_reminders(cx) {
            self.arm_reminder_wake(None, local_now(), cx);
            return;
        }
        let path = ledger_path(&state, &self.vault_root);
        let Some(ledger) = self.reminder_notifier.ledger.clone() else {
            self.reminder_notifier.busy = true;
            let loaded = cx.background_executor().spawn({
                let path = path.clone();
                async move { load(&path) }
            });
            cx.spawn(async move |this, cx| {
                let ledger = loaded.await;
                let _ = this.update(cx, |this, cx| {
                    this.reminder_notifier.busy = false;
                    this.reminder_notifier.again = false;
                    this.reminder_notifier.ledger = Some(ledger);
                    this.reminder_tick(cx);
                });
            })
            .detach();
            return;
        };
        let reminders = schedule::reminders(&index.note_tasks(&self.reminder_prefs.note));
        let policy = Policy {
            notify_at: self.reminder_prefs.notify_at,
            ..Policy::default()
        };
        let now = local_now();
        let mut next = ledger.clone();
        if std::mem::take(&mut self.reminder_notifier.rebaseline) {
            next.rebaseline();
        }
        let notice = schedule::evaluate(&reminders, &mut next, now, &policy);
        let wake = schedule::next_wake(&reminders, &next, now, &policy);
        if next == ledger {
            self.arm_reminder_wake(wake, now, cx);
            return;
        }
        // The ledger is made durable before anything is shown: a notification
        // that cannot be recorded would repeat after every restart.
        self.reminder_notifier.busy = true;
        let json = next.to_json();
        let written = cx
            .background_executor()
            .spawn(async move { store(&path, &json) });
        let missed = notice.as_ref().map_or(0, |notice| notice.overdue.len());
        let notification = notice.map(|notice| {
            (
                SharedString::from(key(&self.vault_root)),
                SharedString::from(notice.title()),
                SharedString::from(notice.body()),
            )
        });
        cx.spawn(async move |this, cx| {
            let result = written.await;
            let _ = this.update(cx, |this, cx| {
                this.reminder_notifier.busy = false;
                match result {
                    Ok(()) => {
                        this.reminder_notifier.ledger = Some(next);
                        if let Some((key, title, body)) = notification {
                            this.deliver(key, title, body, cx);
                        }
                        // The same news inside the app, for when the system
                        // notification is denied or went by unseen.
                        if missed > 0 {
                            this.announce_missed_reminders(missed, cx);
                        }
                        this.arm_reminder_wake(wake, now, cx);
                        if std::mem::take(&mut this.reminder_notifier.again) {
                            this.reminder_tick(cx);
                        }
                    }
                    Err(error) => {
                        eprintln!("Reminder ledger not saved, nothing was shown: {error:#}");
                        this.arm_reminder_wake(Some(now + time::Duration::minutes(5)), now, cx);
                    }
                }
            });
        })
        .detach();
    }

    /// Post the reminder to the system notification centre through the window
    /// that owns the vault's reminders. Clicking it brings that window forward
    /// and opens the reminders note (#961); gpui-component dispatches the click
    /// back to this window, so the click needs no handler of ours. Only the owner
    /// window gets here: `reminder_tick` returns before delivering unless
    /// `claim_reminders` made this window the owner.
    fn deliver(
        &self,
        key: SharedString,
        title: SharedString,
        body: SharedString,
        cx: &mut Context<Self>,
    ) {
        let reader = cx.weak_entity();
        let notification = Notification::new()
            .id1::<ReminderNotice>(key)
            .title(title)
            .message(body)
            .system()
            .on_click(move |_, window, cx| {
                let _ = reader.update(cx, |reader, cx| reader.show_reminders_view(window, cx));
            });
        let _ = self.reader_window.update(cx, |_, window, cx| {
            window.push_notification(notification, cx)
        });
    }

    /// The reminders note seen through the existing native Tasks view (#919),
    /// opened on its Overdue chip. Navigating to another note ends it.
    pub(super) fn show_reminders_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let note = self.reminder_prefs.note.clone();
        if self.current_rel != note || self.file_preview.is_some() {
            self.open_note(&note, None, window, cx);
        }
        self.reminders_view = true;
        cx.notify();
    }

    /// The Tasks source for the view above, while it is active for the open note.
    pub(super) fn reminders_lens(&self) -> Option<Arc<str>> {
        if !self.reminders_view || self.current_rel != self.reminder_prefs.note {
            return None;
        }
        schedule::reminders_dashboard(&self.reminder_prefs.note)
            .map(|source| Arc::from(source.as_str()))
    }

    fn announce_missed_reminders(&mut self, missed: usize, cx: &mut Context<Self>) {
        let message = if missed == 1 {
            "1 missed reminder".to_owned()
        } else {
            format!("{missed} missed reminders")
        };
        let reader = cx.weak_entity();
        let window = self.reader_window;
        let _ = window.update(cx, |_, window, cx| {
            reader_toast::push(
                Notification::new().message(message).action(move |_, _, _| {
                    let reader = reader.clone();
                    Button::new("missed-reminders-show")
                        .debug_selector(|| "missed-reminders-show".into())
                        .ghost()
                        .small()
                        .label("Show")
                        .on_click(move |_, window, cx| {
                            let _ =
                                reader.update(cx, |this, cx| this.show_reminders_view(window, cx));
                        })
                }),
                Some(Duration::from_secs(8)),
                window,
                cx,
            );
        });
    }

    /// The first window of a vault to ask becomes its owner; a closed owner is
    /// replaced by the next window that evaluates.
    fn claim_reminders(&self, cx: &mut Context<Self>) -> bool {
        let me = cx.weak_entity();
        let owners = cx.default_global::<Owners>();
        match owners
            .0
            .get(&self.vault_root)
            .and_then(|owner| owner.upgrade())
        {
            Some(owner) if owner.entity_id() != me.entity_id() => false,
            _ => {
                owners.0.insert(self.vault_root.clone(), me);
                true
            }
        }
    }

    fn arm_reminder_wake(
        &mut self,
        wake: Option<time::PrimitiveDateTime>,
        now: time::PrimitiveDateTime,
        cx: &mut Context<Self>,
    ) {
        self.reminder_notifier.generation += 1;
        let generation = self.reminder_notifier.generation;
        let delay = match wake {
            Some(wake) => Duration::try_from(wake - now).unwrap_or(MIN_WAIT),
            // Nothing pending here; a non-owner re-checks in case it must take over.
            None => RETRY,
        }
        .clamp(MIN_WAIT, MAX_WAIT);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                if this.reminder_notifier.generation == generation {
                    this.reminder_tick(cx);
                }
            });
        })
        .detach();
    }
}

/// Reminders exist only where the guarded writer does.
#[cfg(not(any(unix, windows)))]
impl Reader {
    pub(super) fn reminders_lens(&self) -> Option<Arc<str>> {
        None
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::SystemNotificationResponse;
    use okilum_core::reminder_append::write;
    use time::macros::datetime;

    fn at(now: time::PrimitiveDateTime) {
        TEST_NOW.with(|cell| cell.set(Some(now)));
    }

    fn mount<'a>(
        cx: &'a mut TestAppContext,
        root: &Path,
        state: &Path,
    ) -> (Entity<Reader>, &'a mut VisualTestContext) {
        cx.update(|cx| {
            // The order of the application: gpui-component's own response handler
            // is registered after ours, which is what lost the click (#961).
            install(cx);
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

    /// Open one more window on the vault; its context is independent of `cx`.
    fn open(
        cx: &mut TestAppContext,
        root: &Path,
        state: &Path,
    ) -> (Entity<Reader>, VisualTestContext) {
        let mut reader = None;
        let window = cx.add_window(|window, cx| {
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
        let visual = VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        (reader.unwrap(), visual)
    }

    /// GPUI's one-shot toast animation uses wall time, unlike the test executor's
    /// lifetime clock: let it finish before looking at the toast's bounds.
    fn settle_toasts(visual: &mut VisualTestContext) {
        std::thread::sleep(Duration::from_millis(450));
        visual.update(|window, _| window.refresh());
        visual.executor().advance_clock(Duration::from_millis(400));
        visual.run_until_parked();
    }

    fn tick(reader: &Entity<Reader>, visual: &mut VisualTestContext) {
        reader.update(visual, |reader, cx| reader.reminder_tick(cx));
        visual.run_until_parked();
    }

    /// Reflect an external edit of the reminders note in the task index.
    fn reindex(reader: &Entity<Reader>, visual: &mut VisualTestContext) {
        reader.update_in(visual, |reader, window, cx| {
            let mut changes = okilum_core::Changes::default();
            changes.changed.insert(reader_reminder::NOTE.into());
            reader.apply_vault_changes(changes, window, cx);
        });
        visual.run_until_parked();
    }

    #[gpui::test]
    fn first_import_is_silent_then_one_notification_at_nine_and_never_again(
        cx: &mut TestAppContext,
    ) {
        let temp = std::env::temp_dir().join(format!("okilum-notify-{}", uuid::Uuid::new_v4()));
        let (root, state) = (temp.join("vault"), temp.join("state"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(root.join("start.md"), "# Start\n").unwrap();
        std::fs::write(
            root.join(reader_reminder::NOTE),
            "- [ ] Old overdue [[start.md]] 📅 2026-09-01\n- [ ] Existing [[start.md]] 📅 2026-11-02\n",
        )
        .unwrap();
        at(datetime!(2026-10-01 12:00));
        let (reader, visual) = mount(cx, &root, &state);
        tick(&reader, visual);

        // The first evaluation imports what is already there without a word.
        assert!(visual.shown_system_notifications().is_empty());
        let ledger_file = ledger_path(&state, &root);
        let saved = std::fs::read_to_string(&ledger_file).expect("ledger is written");
        assert!(!saved.contains("Old overdue") && !saved.contains("start.md"));

        // A reminder added afterwards is new: it fires, once, at its time.
        let drafts = state.join("editor-drafts");
        let line = "- [ ] Call Dana [[start.md]] 📅 2026-11-02\n";
        write::add(&root, &drafts, reader_reminder::NOTE, line).unwrap();
        reindex(&reader, visual);
        at(datetime!(2026-11-02 08:59));
        tick(&reader, visual);
        assert!(
            visual.shown_system_notifications().is_empty(),
            "not before 09:00"
        );
        at(datetime!(2026-11-02 09:00));
        tick(&reader, visual);
        let shown = visual.shown_system_notifications();
        assert_eq!(shown.len(), 1, "positive control: a notification is shown");
        assert_eq!(shown[0].title.as_ref(), "Call Dana");
        assert!(
            shown[0].tag.contains(&key(&root)),
            "one notification per vault: {}",
            shown[0].tag
        );
        at(datetime!(2026-11-02 09:30));
        tick(&reader, visual);
        assert_eq!(
            visual.shown_system_notifications().len(),
            1,
            "never repeated"
        );
        // A reminder due today is not a missed one: no in-app "Show" toast.
        settle_toasts(visual);
        assert!(visual.debug_bounds("missed-reminders-show").is_none());

        // Delivery is recorded before and independently of the window: what a
        // restarted app reads is exactly what this one believes.
        let on_disk = load(&ledger_file);
        assert_eq!(
            Some(&on_disk),
            reader
                .read_with(visual, |r, _| r.reminder_notifier.ledger.clone())
                .as_ref()
        );
        let mut replay = on_disk.clone();
        let reminders = schedule::reminders(&okilum_core::tasks::parse(
            reader_reminder::NOTE,
            &std::fs::read_to_string(root.join(reader_reminder::NOTE)).unwrap(),
        ));
        assert!(schedule::evaluate(
            &reminders,
            &mut replay,
            datetime!(2026-11-02 10:00),
            &Policy::default()
        )
        .is_none());

        // Clicking the notification opens the reminders note in the owner window.
        visual.simulate_system_notification_response(SystemNotificationResponse {
            tag: shown[0].tag.clone(),
            action_id: None,
        });
        visual.run_until_parked();
        assert_eq!(
            reader.read_with(visual, |r, _| r.current_rel.clone()),
            reader_reminder::NOTE
        );
        assert!(
            reader.read_with(visual, |r, _| r.reminders_lens().is_some()),
            "the click shows the reminders note in the native Tasks view"
        );
        TEST_NOW.with(|cell| cell.set(None));
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[gpui::test]
    fn configured_time_and_note_are_used_and_switching_notes_is_silent(cx: &mut TestAppContext) {
        use reader_reminder_settings::{load as load_prefs, Preferences};
        let temp = std::env::temp_dir().join(format!("okilum-notify-{}", uuid::Uuid::new_v4()));
        let (root, state) = (temp.join("vault"), temp.join("state"));
        std::fs::create_dir_all(root.join("Inbox")).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(root.join("start.md"), "# Start\n").unwrap();
        // A note that already holds an overdue task, to be chosen later.
        std::fs::write(
            root.join("Old.md"),
            "- [ ] Ancient [[start.md]] 📅 2026-09-01\n",
        )
        .unwrap();
        at(datetime!(2026-10-01 12:00));
        let (reader, visual) = mount(cx, &root, &state);
        tick(&reader, visual);

        let chosen = Preferences {
            note: "Inbox/Remind.md".into(),
            notify_at: time::Time::from_hms(17, 30, 0).unwrap(),
        };
        reader.update(visual, |r, cx| r.set_reminder_prefs(chosen.clone(), cx));
        visual.run_until_parked();
        assert_eq!(load_prefs(&state, &root), chosen, "persisted per vault");

        let drafts = state.join("editor-drafts");
        write::add(
            &root,
            &drafts,
            &chosen.note,
            "- [ ] Call Dana [[start.md]] 📅 2026-11-02\n",
        )
        .unwrap();
        assert!(
            !root.join(reader_reminder::NOTE).exists(),
            "default note untouched"
        );
        reader.update_in(visual, |r, window, cx| {
            let mut changes = okilum_core::Changes::default();
            changes.changed.insert(chosen.note.clone());
            r.apply_vault_changes(changes, window, cx);
        });
        visual.run_until_parked();
        at(datetime!(2026-11-02 09:00));
        tick(&reader, visual);
        assert!(
            visual.shown_system_notifications().is_empty(),
            "09:00 is no longer the time"
        );
        at(datetime!(2026-11-02 17:29));
        tick(&reader, visual);
        assert!(visual.shown_system_notifications().is_empty());
        at(datetime!(2026-11-02 17:30));
        tick(&reader, visual);
        let shown = visual.shown_system_notifications();
        assert_eq!(
            shown.len(),
            1,
            "positive control: the chosen note and time fire"
        );
        assert_eq!(shown[0].title.as_ref(), "Call Dana");

        // Choosing a note full of old tasks must not sound like news.
        at(datetime!(2026-11-02 18:00));
        let switched = Preferences {
            note: "Old.md".into(),
            ..chosen
        };
        reader.update(visual, |r, cx| r.set_reminder_prefs(switched, cx));
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            let mut changes = okilum_core::Changes::default();
            changes.changed.insert("Old.md".into());
            r.apply_vault_changes(changes, window, cx);
        });
        visual.run_until_parked();
        at(datetime!(2026-11-03 09:00));
        tick(&reader, visual);
        assert_eq!(
            visual.shown_system_notifications().len(),
            1,
            "no overdue summary appeared"
        );
        TEST_NOW.with(|cell| cell.set(None));
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[gpui::test]
    fn the_reminders_view_lists_only_this_notes_overdue_tasks_and_ends_on_navigation(
        cx: &mut TestAppContext,
    ) {
        use okilum_core::typed_view::layout;
        let temp = std::env::temp_dir().join(format!("okilum-notify-{}", uuid::Uuid::new_v4()));
        let (root, state) = (temp.join("vault"), temp.join("state"));
        std::fs::create_dir_all(root.join("Sub")).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(root.join("start.md"), "# Start\n").unwrap();
        std::fs::write(
            root.join(reader_reminder::NOTE),
            "- [ ] Late one [[start.md]] 📅 2020-01-01\n- [ ] Far future [[start.md]] 📅 2099-01-01\n- [x] Done one [[start.md]] 📅 2020-01-02\n",
        )
        .unwrap();
        // The same file name elsewhere, with more overdue tasks: a substring
        // filter would let these in and add rows.
        std::fs::write(
            root.join("Sub").join(reader_reminder::NOTE),
            "- [ ] Elsewhere A 📅 2020-01-01\n- [ ] Elsewhere B 📅 2020-01-01\n",
        )
        .unwrap();
        at(datetime!(2026-10-01 12:00));
        let (reader, visual) = mount(cx, &root, &state);
        visual.simulate_resize(size(px(1400.), px(960.)));
        reader.update_in(visual, |r, window, cx| r.show_reminders_view(window, cx));
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("native-tasks-dashboard").is_some(),
            "positive control: the existing native view is shown"
        );
        let source = schedule::reminders_dashboard(reader_reminder::NOTE).unwrap();
        let offset = layout::parse(&source, reader_tasks::today(), layout::Defaults::default())
            .unwrap()
            .sections[0]
            .source_line;
        let row = |ix: usize| -> &'static str {
            Box::leak(format!("task-title-{offset}-{ix}").into_boxed_str())
        };
        assert!(
            visual.debug_bounds(row(0)).is_some(),
            "the overdue task is listed"
        );
        assert!(
            visual.debug_bounds(row(1)).is_none(),
            "no future, done or other-folder task is listed"
        );

        // Any way of leaving the note ends the view, history included; coming
        // back shows the plain note, not the view.
        reader.update_in(visual, |r, window, cx| r.history_move(-1, window, cx));
        visual.run_until_parked();
        assert_eq!(
            reader.read_with(visual, |r, _| r.current_rel.clone()),
            "start.md"
        );
        assert!(visual.debug_bounds("native-tasks-dashboard").is_none());
        assert!(!reader.read_with(visual, |r, _| r.reminders_view));
        reader.update_in(visual, |r, window, cx| r.history_move(1, window, cx));
        visual.run_until_parked();
        assert_eq!(
            reader.read_with(visual, |r, _| r.current_rel.clone()),
            reader_reminder::NOTE,
            "positive control: the reminders note is open again"
        );
        assert!(visual.debug_bounds("native-tasks-dashboard").is_none());
        TEST_NOW.with(|cell| cell.set(None));
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[gpui::test]
    fn missed_reminders_get_one_toast_whose_show_opens_the_view(cx: &mut TestAppContext) {
        let temp = std::env::temp_dir().join(format!("okilum-notify-{}", uuid::Uuid::new_v4()));
        let (root, state) = (temp.join("vault"), temp.join("state"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(root.join("start.md"), "# Start\n").unwrap();
        at(datetime!(2026-10-01 12:00));
        let (reader, visual) = mount(cx, &root, &state);
        visual.simulate_resize(size(px(1400.), px(960.)));
        tick(&reader, visual);
        let drafts = state.join("editor-drafts");
        write::add(
            &root,
            &drafts,
            reader_reminder::NOTE,
            "- [ ] Slept through [[start.md]] 📅 2026-10-05\n",
        )
        .unwrap();
        reindex(&reader, visual);
        // The app was not running when it came due; it is found overdue now.
        at(datetime!(2026-10-06 09:00));
        tick(&reader, visual);
        let shown = visual.shown_system_notifications();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].title.as_ref(), "1 task overdue");
        settle_toasts(visual);
        let show = visual
            .debug_bounds("missed-reminders-show")
            .expect("the in-app toast offers Show");
        assert!(reader.read_with(visual, |r, _| r.reminders_lens().is_none()));
        visual.simulate_mouse_move(show.center(), None, Modifiers::default());
        visual.simulate_click(show.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(
            reader.read_with(visual, |r, _| r.reminders_lens().is_some()),
            "Show opens the reminders note in the native Tasks view"
        );
        TEST_NOW.with(|cell| cell.set(None));
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[gpui::test]
    fn quiet_hours_hold_back_and_a_second_window_never_doubles_a_notification(
        cx: &mut TestAppContext,
    ) {
        let temp = std::env::temp_dir().join(format!("okilum-notify-{}", uuid::Uuid::new_v4()));
        let (root, state) = (temp.join("vault"), temp.join("state"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(root.join("start.md"), "# Start\n").unwrap();
        at(datetime!(2026-10-01 12:00));
        cx.update(|cx| {
            install(cx);
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let (first, mut first_visual) = open(cx, &root, &state);
        let visual = &mut first_visual;
        tick(&first, visual);
        let drafts = state.join("editor-drafts");
        write::add(
            &root,
            &drafts,
            reader_reminder::NOTE,
            "- [ ] Night owl [[start.md]] 📅 2026-10-05\n",
        )
        .unwrap();
        reindex(&first, visual);

        // A second window of the same vault must not act while the first lives.
        let (second, mut second_visual) = open(cx, &root, &state);
        let second_visual = &mut second_visual;

        at(datetime!(2026-10-05 08:00));
        tick(&first, visual);
        assert!(
            visual.shown_system_notifications().is_empty(),
            "quiet hours hold it back"
        );
        at(datetime!(2026-10-05 21:30));
        tick(&first, visual);
        assert!(
            visual.shown_system_notifications().is_empty(),
            "still quiet at night"
        );
        // 09:00 next day: due date has passed, so it is delivered as overdue.
        at(datetime!(2026-10-06 09:00));
        tick(&second, second_visual);
        reindex(&second, second_visual);
        tick(&second, second_visual);
        assert!(
            visual.shown_system_notifications().is_empty(),
            "the non-owner window stays quiet"
        );
        tick(&first, visual);
        let shown = visual.shown_system_notifications();
        assert_eq!(shown.len(), 1, "the owner delivers once");
        assert_eq!(shown[0].title.as_ref(), "1 task overdue");
        assert_eq!(shown[0].body.as_ref(), "Night owl");
        TEST_NOW.with(|cell| cell.set(None));
        std::fs::remove_dir_all(temp).unwrap();
    }
}
