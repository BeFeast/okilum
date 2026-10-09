//! Desktop notifications for reminders (#724). The reminders note stays the
//! source of truth; this only decides when to tell the user, through the pure
//! policy in `okilum_core::reminder_schedule`, and records what it already
//! delivered in a ledger outside the vault and outside the disposable index.
//!
//! One window per vault acts (the owner); other windows of the same vault stay
//! quiet, so a duplicate window never doubles a notification. The scheduler
//! reads the already-published task index: no second vault scan or watcher.
use super::*;
use gpui::SystemNotification;
use okilum_core::reminder_schedule::{self as schedule, Ledger, Policy};
use std::collections::HashMap;

const TAG: &str = "okilum.reminders";
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

fn tag(root: &Path) -> String {
    format!("{TAG}.{}", key(root))
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

/// Name the app to the notification centre, then route a click on a reminder
/// notification to the window that owns it. Platforms drop notifications until
/// the identity is set (Windows AppUserModelID, the name shown to the user), so
/// it is set here, once, before any window posts one.
pub(super) fn install(cx: &mut App) {
    cx.set_app_identity("com.befeast.okilum", "Okilum");
    cx.on_system_notification_response(|response, cx| {
        if !response.tag.starts_with(TAG) {
            return;
        }
        let owner = cx.try_global::<Owners>().and_then(|owners| {
            owners
                .0
                .iter()
                .find(|(root, _)| tag(root) == response.tag.as_ref())
                .and_then(|(_, owner)| owner.upgrade())
        });
        let Some(owner) = owner else {
            return;
        };
        let window = owner.read(cx).reader_window;
        let _ = window.update(cx, |_, window, cx| {
            window.activate_window();
            owner.update(cx, |reader, cx| {
                let note = reader.reminder_prefs.note.clone();
                reader.open_note(&note, None, window, cx)
            });
        });
    });
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
        let notification = notice.map(|notice| SystemNotification {
            tag: tag(&self.vault_root).into(),
            title: notice.title().into(),
            body: notice.body().into(),
            actions: Vec::new(),
        });
        cx.spawn(async move |this, cx| {
            let result = written.await;
            let _ = this.update(cx, |this, cx| {
                this.reminder_notifier.busy = false;
                match result {
                    Ok(()) => {
                        this.reminder_notifier.ledger = Some(next);
                        if let Some(notification) = notification {
                            cx.show_system_notification(notification);
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
            gpui_component::init(cx);
            bind_keys(cx);
            install(cx);
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
        assert_eq!(shown[0].tag.as_ref(), tag(&root));
        at(datetime!(2026-11-02 09:30));
        tick(&reader, visual);
        assert_eq!(
            visual.shown_system_notifications().len(),
            1,
            "never repeated"
        );

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
            gpui_component::init(cx);
            bind_keys(cx);
            install(cx);
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
