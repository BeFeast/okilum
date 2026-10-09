//! Per-vault reminder preferences (#724): which note receives reminders and
//! when date-only reminders notify. Stored with app state, never in the vault;
//! an unreadable or invalid file means the defaults, field by field.
use super::*;
use time::Time;

pub(super) const DEFAULT_NOTE: &str = "Reminders.md";
const STEP_MINUTES: i32 = 30;

fn minutes(at: Time) -> i32 {
    i32::from(at.hour()) * 60 + i32::from(at.minute())
}

/// Quiet hours are fixed policy and hold every notification, so a time inside
/// them would only be a promise the app does not keep: the choices are the
/// waking hours, from the end of quiet hours to the last half-hour before them.
fn waking_range() -> (i32, i32) {
    let policy = tessera_core::reminder_schedule::Policy::default();
    (
        minutes(policy.quiet_until),
        minutes(policy.quiet_from) - STEP_MINUTES,
    )
}

fn usable_time(at: Time) -> bool {
    let (earliest, latest) = waking_range();
    (earliest..=latest).contains(&minutes(at))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Preferences {
    /// Vault-relative Markdown note.
    pub note: String,
    pub notify_at: Time,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            note: DEFAULT_NOTE.into(),
            notify_at: Time::from_hms(9, 0, 0).unwrap(),
        }
    }
}

fn path(state: &Path, root: &Path) -> PathBuf {
    state
        .join("reminder-settings")
        .join(format!("{}.json", reader_reminder_notify::key(root)))
}

/// A note the writer and the index can address: vault-relative, normal
/// components only, Markdown, not an application file. The spelling must be the
/// canonical `a/b.md` the index uses as its key: `a//b.md` or `a\\b.md` would
/// name the same file yet never match its tasks.
pub(super) fn valid_note(note: &str) -> bool {
    let relative = Path::new(note);
    let parts: Option<Vec<&str>> = relative
        .components()
        .map(|part| match part {
            std::path::Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect();
    parts.is_some_and(|parts| parts.join("/") == note)
        && relative
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        && !tessera_core::vault::service_path(relative)
}

pub(super) fn parse_time(text: &str) -> Option<Time> {
    let (hours, minutes) = text.split_once(':')?;
    Time::from_hms(hours.parse().ok()?, minutes.parse().ok()?, 0).ok()
}

pub(super) fn time_label(at: Time) -> String {
    format!("{:02}:{:02}", at.hour(), at.minute())
}

/// Move by half-hours within the waking hours; the ends stop instead of
/// wrapping, so "later" can never become "earlier".
pub(super) fn step(at: Time, direction: i32) -> Time {
    let (earliest, latest) = waking_range();
    let next = (minutes(at) + direction * STEP_MINUTES).clamp(earliest, latest);
    Time::from_hms((next / 60) as u8, (next % 60) as u8, 0).unwrap()
}

pub(super) fn load(state: &Path, root: &Path) -> Preferences {
    let mut preferences = Preferences::default();
    let Ok(bytes) = std::fs::read(path(state, root)) else {
        return preferences;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return preferences;
    };
    if let Some(note) = value["note"].as_str().filter(|note| valid_note(note)) {
        preferences.note = note.into();
    }
    if let Some(at) = value["notify_at"]
        .as_str()
        .and_then(parse_time)
        .filter(|at| usable_time(*at))
    {
        preferences.notify_at = at;
    }
    preferences
}

pub(super) fn save(state: &Path, root: &Path, preferences: &Preferences) -> std::io::Result<()> {
    let json = serde_json::json!({
        "note": preferences.note,
        "notify_at": time_label(preferences.notify_at),
    });
    reader_reminder_notify::store(&path(state, root), &json.to_string())
}

#[cfg(any(unix, windows))]
impl Reader {
    /// Apply and persist new preferences. A different note is re-imported
    /// silently, so choosing a note full of old tasks never sounds like news.
    pub(super) fn set_reminder_prefs(&mut self, preferences: Preferences, cx: &mut Context<Self>) {
        if preferences == self.reminder_prefs {
            // Choosing what is already set resolves an earlier complaint too.
            if self.reminder_prefs_error.take().is_some() {
                cx.notify();
            }
            return;
        }
        let Some(state) = self.session_directory.clone() else {
            self.reminder_prefs_error = Some("No preference storage is available.".into());
            cx.notify();
            return;
        };
        if let Err(error) = save(&state, &self.vault_root, &preferences) {
            self.reminder_prefs_error = Some(format!("Could not save: {error}"));
            cx.notify();
            return;
        }
        self.reminder_prefs_error = None;
        if preferences.note != self.reminder_prefs.note {
            self.reminder_notifier.note_changed();
        }
        self.reminder_prefs = preferences;
        self.reminder_tick(cx);
        cx.notify();
    }

    fn change_reminder_time(&mut self, direction: i32, cx: &mut Context<Self>) {
        let mut preferences = self.reminder_prefs.clone();
        preferences.notify_at = step(preferences.notify_at, direction);
        self.set_reminder_prefs(preferences, cx);
    }

    fn choose_reminder_note(&mut self, cx: &mut Context<Self>) {
        let root = self.vault_root.clone();
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a note in this vault for reminders".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picker.await else {
                return;
            };
            let Some(chosen) = paths.into_iter().next() else {
                return;
            };
            let note = note_in_vault(&root, &chosen);
            let _ = this.update(cx, |this, cx| {
                if this.vault_root != root {
                    return;
                }
                match note {
                    Some(note) => {
                        let mut preferences = this.reminder_prefs.clone();
                        preferences.note = note;
                        this.set_reminder_prefs(preferences, cx);
                    }
                    None => {
                        this.reminder_prefs_error =
                            Some("Choose a Markdown note inside this vault.".into());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }
}

/// The vault-relative form of a picked file, or `None` when it is outside the
/// vault, reached through a symlink, or not a usable Markdown note.
#[cfg(any(unix, windows))]
fn note_in_vault(root: &Path, chosen: &Path) -> Option<String> {
    let root = root.canonicalize().ok()?;
    let chosen = chosen.canonicalize().ok()?;
    let relative = chosen.strip_prefix(&root).ok()?;
    let note = relative
        .components()
        .map(|part| part.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?
        .join("/");
    (valid_note(&note) && chosen.is_file()).then_some(note)
}

/// Settings rows for reminders. Calls straight into the reader: the preferences
/// belong to the vault window, not to the Settings view.
#[cfg(any(unix, windows))]
pub(super) fn controls(reader: &Entity<Reader>, cx: &App) -> AnyElement {
    use reader_settings::setting_row;
    let state = reader.read(cx);
    let preferences = state.reminder_prefs.clone();
    let storage = state.session_directory.is_some();
    let name = Path::new(&preferences.note)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let at = preferences.notify_at;
    let (earlier, later) = (reader.clone(), reader.clone());
    let chooser = reader.clone();
    v_flex()
        .gap_2()
        .child(setting_row(
            "Reminder time",
            "Date-only reminders notify then, never between 21:00 and 09:00.",
            h_flex()
                .gap_2()
                .child(
                    Button::new("reminder-earlier")
                        .debug_selector(|| "reminder-earlier".into())
                        .ghost()
                        .icon(IconName::Minus)
                        .accessibility_label("Earlier")
                        .tooltip("Earlier")
                        .disabled(!storage || at == step(at, -1))
                        .on_click(move |_, _, cx| {
                            earlier.update(cx, |reader, cx| reader.change_reminder_time(-1, cx))
                        }),
                )
                .child(
                    div()
                        .debug_selector(|| "reminder-time".into())
                        .w(px(48.))
                        .text_center()
                        .child(time_label(at)),
                )
                .child(
                    Button::new("reminder-later")
                        .debug_selector(|| "reminder-later".into())
                        .ghost()
                        .icon(IconName::Plus)
                        .accessibility_label("Later")
                        .tooltip("Later")
                        .disabled(!storage || at == step(at, 1))
                        .on_click(move |_, _, cx| {
                            later.update(cx, |reader, cx| reader.change_reminder_time(1, cx))
                        }),
                ),
            cx,
        ))
        .child(setting_row(
            "Reminders note",
            "Where “Remind me on…” adds tasks.",
            h_flex()
                .gap_2()
                .child(
                    div()
                        .max_w(px(160.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_sm()
                        .text_color(brand::palette(cx).text_muted)
                        .child(name),
                )
                .child(
                    Button::new("reminder-note")
                        .debug_selector(|| "reminder-note".into())
                        .ghost()
                        .icon(IconName::FolderOpen)
                        .accessibility_label("Change reminders note")
                        .tooltip("Change reminders note")
                        .disabled(!storage)
                        .on_click(move |_, _, cx| {
                            chooser.update(cx, |reader, cx| reader.choose_reminder_note(cx))
                        }),
                ),
            cx,
        ))
        .children(state.reminder_prefs_error.as_ref().map(|error| {
            div()
                .text_sm()
                .text_color(brand::palette(cx).danger)
                .child(error.clone())
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn temp() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tessera-remind-prefs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn notes_are_vault_relative_markdown_and_never_application_files() {
        for good in ["Reminders.md", "Inbox/Reminders.md", "Мои задачи.MD"] {
            assert!(valid_note(good), "{good}");
        }
        for bad in [
            "",
            "../Reminders.md",
            "/Reminders.md",
            "a//b.md",
            "Reminders.txt",
            "Reminders",
            "./x.md/..",
        ] {
            assert!(!valid_note(bad), "{bad}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn picked_files_must_be_real_markdown_notes_inside_the_vault() {
        let dir = temp();
        let (root, outside) = (dir.join("vault"), dir.join("outside"));
        std::fs::create_dir_all(root.join("Inbox")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("Inbox/Remind.md"), "").unwrap();
        std::fs::write(root.join("data.txt"), "").unwrap();
        std::fs::write(outside.join("Away.md"), "").unwrap();
        std::os::unix::fs::symlink(outside.join("Away.md"), root.join("Link.md")).unwrap();
        assert_eq!(
            note_in_vault(&root, &root.join("Inbox/Remind.md")).as_deref(),
            Some("Inbox/Remind.md")
        );
        for bad in [
            root.join("data.txt"),
            root.join("Inbox"),
            outside.join("Away.md"),
            root.join("Link.md"),
            root.join("missing.md"),
        ] {
            assert_eq!(note_in_vault(&root, &bad), None, "{}", bad.display());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn times_parse_strictly_and_steps_stop_at_the_ends() {
        let t = |h, m| Time::from_hms(h, m, 0).unwrap();
        assert_eq!(parse_time("09:30"), Some(t(9, 30)));
        assert_eq!(parse_time("9:05"), Some(t(9, 5)));
        for bad in ["", "24:00", "09:60", "nine", "09", "09:00:00", "-1:00"] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
        assert_eq!(step(t(9, 0), 1), t(9, 30));
        // Quiet hours (21:00-09:00) are not offered: the ends hold.
        assert_eq!(step(t(9, 0), -1), t(9, 0));
        assert_eq!(step(t(20, 30), 1), t(20, 30));
        assert_eq!(step(t(20, 0), 1), t(20, 30));
        assert!(usable_time(t(9, 0)) && usable_time(t(20, 30)));
        assert!(!usable_time(t(8, 59)) && !usable_time(t(21, 0)) && !usable_time(t(3, 0)));
        assert_eq!(time_label(t(7, 5)), "07:05");
    }

    #[test]
    fn missing_or_damaged_files_give_defaults_field_by_field() {
        let (state, root) = (temp(), PathBuf::from("/vault"));
        assert_eq!(load(&state, &root), Preferences::default());
        let wanted = Preferences {
            note: "Inbox/Remind.md".into(),
            notify_at: Time::from_hms(17, 30, 0).unwrap(),
        };
        save(&state, &root, &wanted).unwrap();
        assert_eq!(load(&state, &root), wanted);
        // One bad field falls back alone; the other stays.
        let file = path(&state, &root);
        std::fs::write(&file, r#"{"note":"../escape.md","notify_at":"16:30"}"#).unwrap();
        let loaded = load(&state, &root);
        assert_eq!(loaded.note, DEFAULT_NOTE);
        assert_eq!(loaded.notify_at, Time::from_hms(16, 30, 0).unwrap());
        // A time inside quiet hours is not a promise the app keeps: default.
        std::fs::write(&file, r#"{"note":"Remind.md","notify_at":"06:30"}"#).unwrap();
        let loaded = load(&state, &root);
        assert_eq!(
            (loaded.note.as_str(), loaded.notify_at),
            ("Remind.md", Preferences::default().notify_at)
        );
        std::fs::write(&file, "not json").unwrap();
        assert_eq!(load(&state, &root), Preferences::default());
        // Settings are per vault.
        assert_eq!(load(&state, Path::new("/other")), Preferences::default());
        std::fs::remove_dir_all(state).unwrap();
    }
}
