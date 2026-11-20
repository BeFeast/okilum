//! Daily notes (#3182): «Today's note» opens today's note, or creates it in
//! the chosen folder from the chosen template. The choice is stored with app
//! state per vault, never in the vault. Until one is made in Settings → Vault,
//! Obsidian's Daily notes settings (`.obsidian/daily-notes.json`) apply.
use super::*;
use anyhow::Context as _;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use okilum_core::daily_notes::{Settings as DailySettings, Today};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Origin {
    /// Chosen in Okilum's Settings.
    Okilum,
    /// Read from `.obsidian/daily-notes.json`.
    Obsidian,
    /// Nothing chosen anywhere: the vault root, `YYYY-MM-DD`, no template.
    Default,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Prefs {
    pub settings: DailySettings,
    pub origin: Origin,
}

/// The name formats Settings offers. A format read from Obsidian that is not
/// one of them is kept and shown as it is.
pub(super) const FORMATS: [&str; 4] = [
    "YYYY-MM-DD",
    "YYYY/MM/YYYY-MM-DD",
    "YYYY/MM/DD/YYYY-MM-DD",
    "DD.MM.YYYY",
];

fn path(state: &Path, root: &Path) -> PathBuf {
    state
        .join("daily-notes")
        .join(format!("{}.json", reader_reminder_notify::key(root)))
}

pub(super) fn load(state: &Path, root: &Path) -> anyhow::Result<Prefs> {
    match std::fs::read(path(state, root)) {
        Ok(bytes) => {
            let settings =
                serde_json::from_slice(&bytes).context("Invalid daily notes preference")?;
            return Ok(Prefs {
                settings,
                origin: Origin::Okilum,
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("Cannot read the daily notes preference"),
    }
    Ok(match DailySettings::from_obsidian(root)? {
        Some(settings) => Prefs {
            settings,
            origin: Origin::Obsidian,
        },
        None => Prefs {
            settings: DailySettings::default(),
            origin: Origin::Default,
        },
    })
}

pub(super) fn save(state: &Path, root: &Path, settings: &DailySettings) -> std::io::Result<()> {
    let json = serde_json::to_string(settings).map_err(std::io::Error::other)?;
    reader_reminder_notify::store(&path(state, root), &json)
}

pub(super) fn now() -> time::OffsetDateTime {
    time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc())
}

/// The vault-relative form of a picked folder (`""` for the vault root), or
/// `None` outside the vault or through a symlink.
fn folder_in_vault(root: &Path, chosen: &Path) -> Option<String> {
    let root = root.canonicalize().ok()?;
    let chosen = chosen.canonicalize().ok()?;
    let relative = chosen.strip_prefix(&root).ok()?;
    let folder = relative
        .components()
        .map(|part| part.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?
        .join("/");
    (chosen.is_dir() && !okilum_core::vault::service_path(relative)).then_some(folder)
}

fn template_label(template: &str) -> String {
    let template = template.trim().trim_matches('/');
    if template.is_empty() {
        return "None".into();
    }
    Path::new(template)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| template.to_owned())
}

impl Reader {
    /// Settings shows the same preference «Today's note» uses; read once per
    /// vault and again after each change.
    pub(super) fn ensure_daily_prefs(&mut self) {
        if self
            .daily_notes
            .as_ref()
            .is_some_and(|(root, _)| root == &self.vault_root)
        {
            return;
        }
        let _ = self.reload_daily_prefs();
    }

    fn reload_daily_prefs(&mut self) -> Result<Prefs, String> {
        let loaded = match &self.session_directory {
            Some(state) => load(state, &self.vault_root).map_err(|error| format!("{error:#}")),
            None => Err("No preference storage is available.".into()),
        };
        self.daily_notes = Some((self.vault_root.clone(), loaded.clone()));
        loaded
    }

    fn set_daily_settings(&mut self, settings: DailySettings, cx: &mut Context<Self>) {
        let Some(state) = self.session_directory.clone() else {
            return;
        };
        self.daily_notes_error = save(&state, &self.vault_root, &settings)
            .err()
            .map(|error| format!("Could not save: {error}"));
        if self.daily_notes_error.is_none() {
            self.daily_notes = Some((
                self.vault_root.clone(),
                Ok(Prefs {
                    settings,
                    origin: Origin::Okilum,
                }),
            ));
        }
        cx.notify();
    }

    /// The settings to change: the current ones, or the defaults when the
    /// stored preference cannot be read (choosing again repairs it).
    fn daily_settings(&mut self) -> DailySettings {
        self.ensure_daily_prefs();
        match &self.daily_notes {
            Some((_, Ok(prefs))) => prefs.settings.clone(),
            _ => DailySettings::default(),
        }
    }

    fn set_daily_format(&mut self, format: &str, cx: &mut Context<Self>) {
        let mut settings = self.daily_settings();
        settings.format = format.into();
        self.set_daily_settings(settings, cx);
    }

    fn clear_daily_template(&mut self, cx: &mut Context<Self>) {
        let mut settings = self.daily_settings();
        settings.template.clear();
        self.set_daily_settings(settings, cx);
    }

    fn choose_daily_folder(&mut self, cx: &mut Context<Self>) {
        let root = self.vault_root.clone();
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose the daily notes folder in this vault".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picker.await else {
                return;
            };
            let Some(chosen) = paths.into_iter().next() else {
                return;
            };
            let folder = folder_in_vault(&root, &chosen);
            let _ = this.update(cx, |this, cx| {
                if this.vault_root != root {
                    return;
                }
                match folder {
                    Some(folder) => {
                        let mut settings = this.daily_settings();
                        settings.folder = folder;
                        this.set_daily_settings(settings, cx);
                    }
                    None => {
                        this.daily_notes_error = Some("Choose a folder inside this vault.".into());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn choose_daily_template(&mut self, cx: &mut Context<Self>) {
        let root = self.vault_root.clone();
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose the daily note template in this vault".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picker.await else {
                return;
            };
            let Some(chosen) = paths.into_iter().next() else {
                return;
            };
            let note = reader_reminder_settings::note_in_vault(&root, &chosen);
            let _ = this.update(cx, |this, cx| {
                if this.vault_root != root {
                    return;
                }
                let Some(note) = note else {
                    this.daily_notes_error =
                        Some("Choose a Markdown note inside this vault.".into());
                    cx.notify();
                    return;
                };
                let mut settings = this.daily_settings();
                settings.template = note;
                if let Err(error) = settings.template_parts() {
                    this.daily_notes_error = Some(format!("{error:#}"));
                    cx.notify();
                    return;
                }
                this.set_daily_settings(settings, cx);
            });
        })
        .detach();
    }

    /// «Today's note»: open today's note, or create it from the template.
    pub(super) fn open_daily_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.creation.is_some()
            || self.renaming.is_some()
            || self.trash_pending
            || self.note_move_pending
        {
            self.file_operation_blocked(
                "daily_note",
                "",
                "Finish the current file operation first.",
                cx,
            );
            return;
        }
        if self.loading.as_ref().is_some_and(|l| l.active) || self.session_directory.is_none() {
            self.file_operation_blocked(
                "daily_note",
                "",
                "Wait for the vault and draft recovery storage before creating files.",
                cx,
            );
            return;
        }
        // An untitled draft's folder is Okilum's own storage, not a vault:
        // a dated note there would be stranded in app state, and the draft
        // would lose its Save As (#1528). Also after browsing to another file
        // of that folder, so the check is on the root, not the file on screen.
        if self
            .session_directory
            .as_deref()
            .and_then(|state| reader_untitled::draft_folder(state, &self.vault_root))
            .is_some()
        {
            self.file_operation_blocked(
                "daily_note",
                "",
                "Today's note needs a vault. Open a folder first.",
                cx,
            );
            return;
        }
        if self.leaving_deleted_note(None, cx) {
            self.ask_leaving_deleted_note(
                Box::new(|this, window, cx| this.open_daily_note(window, cx)),
                window,
                cx,
            );
            return;
        }
        let root = self.vault_root.clone();
        let result = (|| -> anyhow::Result<Today> {
            let state = self
                .session_directory
                .clone()
                .context("No draft recovery storage")?;
            let prefs = self.reload_daily_prefs().map_err(anyhow::Error::msg)?;
            let templates = self.creation_templates()?;
            anyhow::ensure!(
                self.save_source_for_navigation(cx),
                "Resolve the current note's save before opening another"
            );
            okilum_core::daily_notes::open_or_create(
                &root,
                &prefs.settings,
                &templates,
                now(),
                &state.join("editor-drafts"),
            )
        })();
        match result {
            Ok(Today::Existing(rel)) => {
                self.trace_file_operation("daily_note", "opened", &rel, None, cx);
                self.open_note(&rel, None, window, cx);
            }
            Ok(Today::Created { rel, source }) => {
                self.trace_file_operation("daily_note", "created", &rel, None, cx);
                // From Inbox, Projects or Logs the note must come on screen,
                // as `open_note` does for an existing one.
                if self.project_navigation.active.is_none() {
                    self.close_project(window, cx);
                }
                self.project_navigation.section = reader_project_navigation::Section::Reader;
                // Quick Look draws over the editor; close it like opening does.
                self.tree_preview.close();
                self.show_created_note(rel, source, window, cx);
            }
            Err(error) => {
                let error = format!("{error:#}");
                self.trace_file_operation("daily_note", "failed", "", Some(&error), cx);
                reader_toast::error(format!("Cannot open today's note: {error}"), window, cx);
            }
        }
        cx.notify();
    }
}

/// Settings → Vault rows for daily notes. They call straight into the reader:
/// the preference belongs to the vault window, like the reminders note.
pub(super) fn controls(reader: &Entity<Reader>, cx: &App) -> AnyElement {
    use reader_settings::setting_row;
    let state = reader.read(cx);
    let storage = state.session_directory.is_some();
    let (prefs, load_error) = match &state.daily_notes {
        Some((root, Ok(prefs))) if root == &state.vault_root => (Some(prefs.clone()), None),
        Some((root, Err(error))) if root == &state.vault_root => (None, Some(error.clone())),
        _ => (None, None),
    };
    let settings = prefs
        .as_ref()
        .map(|prefs| prefs.settings.clone())
        .unwrap_or_default();
    let from_obsidian = prefs
        .as_ref()
        .is_some_and(|prefs| prefs.origin == Origin::Obsidian);
    let folder = match settings.folder.trim().trim_matches('/') {
        "" => "Vault root".to_owned(),
        folder => folder.to_owned(),
    };
    let today = now();
    let example = move |format: &str| {
        DailySettings {
            format: format.into(),
            ..Default::default()
        }
        .name(today)
        .unwrap_or_else(|_| format.to_owned())
    };
    let current = settings.format().to_owned();
    let mut formats: Vec<String> = FORMATS.iter().map(|f| (*f).to_owned()).collect();
    if !formats.contains(&current) {
        formats.push(current.clone());
    }
    let has_template = !settings.template.trim().is_empty();
    let muted = brand::palette(cx).text_muted;
    let value = |text: String| {
        div()
            .max_w(px(160.))
            .overflow_hidden()
            .text_ellipsis()
            .whitespace_nowrap()
            .text_sm()
            .text_color(muted)
            .child(text)
    };
    let (folder_reader, format_reader, template_reader, clear_reader) = (
        reader.clone(),
        reader.clone(),
        reader.clone(),
        reader.clone(),
    );
    v_flex()
        .gap_2()
        .debug_selector(|| "settings-daily-notes".into())
        .child(setting_row(
            "Daily notes folder",
            if from_obsidian {
                "Where Today's note goes. Taken from Obsidian's Daily notes settings."
            } else {
                "Where Today's note goes."
            },
            h_flex()
                .gap_2()
                .child(value(folder.clone()).debug_selector(|| "settings-daily-folder".into()))
                .child(
                    Button::new("settings-daily-folder-change")
                        .debug_selector(|| "settings-daily-folder-change".into())
                        .ghost()
                        .icon(IconName::FolderOpen)
                        .accessibility_label("Change daily notes folder")
                        .tooltip(format!("Change daily notes folder ({folder})"))
                        .disabled(!storage)
                        .on_click(move |_, _, cx| {
                            folder_reader.update(cx, |reader, cx| reader.choose_daily_folder(cx))
                        }),
                ),
            cx,
        ))
        .child(setting_row(
            "Daily note name",
            "A / makes a folder, such as one per year and month.",
            Button::new("settings-daily-format")
                .debug_selector(|| "settings-daily-format".into())
                .outline()
                .small()
                .label(example(&current))
                .dropdown_caret(true)
                .disabled(!storage)
                .dropdown_menu(move |menu, _, _| {
                    let mut menu = menu;
                    for format in formats.clone() {
                        let reader = format_reader.clone();
                        let label = format!("{format}  →  {}", example(&format));
                        menu = menu.item(
                            PopupMenuItem::new(label)
                                .checked(format == current)
                                .on_click(move |_, _, cx| {
                                    reader.update(cx, |reader, cx| {
                                        reader.set_daily_format(&format, cx)
                                    })
                                }),
                        );
                    }
                    menu
                }),
            cx,
        ))
        .child(setting_row(
            "Daily note template",
            "Applied when Today's note is created. None: just the date as its title.",
            h_flex()
                .gap_2()
                .child(
                    value(template_label(&settings.template))
                        .debug_selector(|| "settings-daily-template".into()),
                )
                .when(has_template, |row| {
                    row.child(
                        Button::new("settings-daily-template-clear")
                            .debug_selector(|| "settings-daily-template-clear".into())
                            .ghost()
                            .icon(IconName::Close)
                            .accessibility_label("No daily note template")
                            .tooltip("No daily note template")
                            .disabled(!storage)
                            .on_click(move |_, _, cx| {
                                clear_reader
                                    .update(cx, |reader, cx| reader.clear_daily_template(cx))
                            }),
                    )
                })
                .child(
                    Button::new("settings-daily-template-change")
                        .debug_selector(|| "settings-daily-template-change".into())
                        .ghost()
                        .icon(IconName::FolderOpen)
                        .accessibility_label("Choose daily note template")
                        .tooltip("Choose daily note template")
                        .disabled(!storage)
                        .on_click(move |_, _, cx| {
                            template_reader
                                .update(cx, |reader, cx| reader.choose_daily_template(cx))
                        }),
                ),
            cx,
        ))
        .children(
            load_error
                .or_else(|| state.daily_notes_error.clone())
                .map(|error| {
                    div()
                        .text_sm()
                        .text_color(brand::palette(cx).danger)
                        .child(error)
                }),
        )
        .into_any_element()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn okilum_choice_wins_over_obsidian_and_stays_out_of_the_vault() {
        let dir = tempfile::tempdir().unwrap();
        let (root, state) = (dir.path().join("vault"), dir.path().join("state"));
        std::fs::create_dir_all(root.join(".obsidian")).unwrap();
        let defaults = load(&state, &root).unwrap();
        assert_eq!(defaults.origin, Origin::Default);
        assert_eq!(defaults.settings, DailySettings::default());
        std::fs::write(
            root.join(".obsidian/daily-notes.json"),
            r#"{"folder":"Daily","format":"YYYY/YYYY-MM-DD"}"#,
        )
        .unwrap();
        let obsidian = load(&state, &root).unwrap();
        assert_eq!(obsidian.origin, Origin::Obsidian);
        assert_eq!(obsidian.settings.folder, "Daily");
        let chosen = DailySettings {
            folder: "Journal".into(),
            format: FORMATS[1].into(),
            template: "Templates/Day.md".into(),
        };
        save(&state, &root, &chosen).unwrap();
        let loaded = load(&state, &root).unwrap();
        assert_eq!(loaded.origin, Origin::Okilum);
        assert_eq!(loaded.settings, chosen);
        assert_eq!(
            std::fs::read_to_string(root.join(".obsidian/daily-notes.json")).unwrap(),
            r#"{"folder":"Daily","format":"YYYY/YYYY-MM-DD"}"#
        );
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            1,
            "only .obsidian: nothing written into the vault"
        );
        // Another vault has its own choice.
        let other = dir.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        assert_eq!(load(&state, &other).unwrap().origin, Origin::Default);
    }

    #[test]
    fn picked_folders_must_be_inside_the_vault() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir_all(root.join("Daily/2026")).unwrap();
        std::fs::create_dir_all(dir.path().join("outside")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("outside"), root.join("Link")).unwrap();
        assert_eq!(folder_in_vault(&root, &root).as_deref(), Some(""));
        assert_eq!(
            folder_in_vault(&root, &root.join("Daily/2026")).as_deref(),
            Some("Daily/2026")
        );
        assert_eq!(folder_in_vault(&root, &dir.path().join("outside")), None);
        assert_eq!(folder_in_vault(&root, &root.join("Link")), None);
        assert_eq!(template_label(""), "None");
        assert_eq!(template_label("Templates/Daily.md"), "Daily");
        assert_eq!(template_label("Templates/Daily"), "Daily");
    }
}
