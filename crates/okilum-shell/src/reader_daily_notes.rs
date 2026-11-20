//! Daily notes (#3182), after Obsidian's core Daily notes plugin.
//!
//! «Today's note» opens today's note, or creates it in the daily notes folder
//! from the daily template. Where the settings come from, in order:
//! the choice made in Okilum (stored with app state per vault, never in the
//! vault); the vault's `.obsidian/daily-notes.json`, used as is; otherwise
//! nothing is set up and the first press asks, in a small dialog with the
//! folder (`Daily`) and a template from the templates folder prefilled.
//! A daily note never goes into the vault root.
use super::*;
use anyhow::Context as _;
use gpui_component::{
    input::Input,
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
    WindowExt as _,
};
use okilum_core::daily_notes::{Settings as DailySettings, Today};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Origin {
    /// Chosen in Okilum.
    Okilum,
    /// Read from `.obsidian/daily-notes.json`.
    Obsidian,
    /// Nothing set anywhere yet: the first press asks.
    Default,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Prefs {
    pub settings: DailySettings,
    pub origin: Origin,
}

/// What the vault offers as daily templates, read with the preference.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Templates {
    /// Vault-relative Markdown files of the templates folder (or `Templates/`).
    pub candidates: Vec<String>,
    /// The one plainly meant for daily notes, prefilled when asking.
    pub suggested: Option<String>,
}

/// The first-press dialog's choices (#3182). An entity of its own: the
/// dialog layer is drawn inside the Reader's render, so its builder reads this,
/// never the Reader.
pub(super) struct Setup {
    root: PathBuf,
    folder: Entity<InputState>,
    template: Option<String>,
    candidates: Vec<String>,
    error: Option<String>,
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

/// The vault-relative form of a picked folder, or `None` outside the vault,
/// through a symlink, or for the vault itself: daily notes never go there.
fn folder_in_vault(root: &Path, chosen: &Path) -> Option<String> {
    let root = root.canonicalize().ok()?;
    let chosen = chosen.canonicalize().ok()?;
    let relative = chosen.strip_prefix(&root).ok()?;
    let folder = relative
        .components()
        .map(|part| part.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?
        .join("/");
    (!folder.is_empty() && chosen.is_dir() && !okilum_core::vault::service_path(relative))
        .then_some(folder)
}

/// A folder typed in the dialog: inside the vault, not the vault itself, not
/// app storage. Leading and trailing `/` are dropped, as in Obsidian.
fn typed_folder(text: &str) -> Result<String, String> {
    let folder = text.trim().trim_matches('/');
    if folder.is_empty() {
        return Err("Enter a folder for daily notes; they never go into the vault root.".into());
    }
    // The raw segments, not `Path::components`, which would quietly fold
    // `a//b` into `a/b` and drop `.`.
    let parts: Vec<&str> = folder.split('/').map(str::trim).collect();
    let valid = parts.iter().all(|part| {
        !part.is_empty()
            && *part != "."
            && *part != ".."
            && !part.chars().any(|c| c.is_control() || c == '\\')
    }) && !okilum_core::vault::service_path(Path::new(folder));
    if valid {
        Ok(parts.join("/"))
    } else {
        Err("Enter a folder inside the vault, such as Daily or Journal/Days.".into())
    }
}

fn template_label(template: &str) -> String {
    let template = template.trim().trim_matches('/');
    if template.is_empty() {
        return "None".into();
    }
    let template = template.strip_suffix(".md").unwrap_or(template);
    template.to_owned()
}

/// The values «Today's note» uses, as one line for Settings.
pub(super) fn effective_summary(
    prefs: &Prefs,
    templates: &Templates,
    now: time::OffsetDateTime,
) -> String {
    let settings = effective_settings(prefs, templates);
    let path = settings
        .relative_path(now)
        .map(|path| okilum_core::vault::note_path(&path))
        .unwrap_or_else(|error| format!("{error:#}"));
    let template = template_label(&settings.template);
    match prefs.origin {
        Origin::Okilum => format!("Today's note: {path} · template: {template}"),
        Origin::Obsidian => format!(
            "Today's note: {path} · template: {template} · from Obsidian's Daily notes settings"
        ),
        Origin::Default => format!(
            "Not set up yet. The first Today's note asks, proposing {path} · template: {template}"
        ),
    }
}

/// The settings in effect; while nothing is set up, what the dialog proposes.
fn effective_settings(prefs: &Prefs, templates: &Templates) -> DailySettings {
    let mut settings = prefs.settings.clone();
    if prefs.origin == Origin::Default && settings.template.trim().is_empty() {
        settings.template = templates.suggested.clone().unwrap_or_default();
    }
    settings
}

/// Where «Other file in this vault…» puts its choice.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PickFor {
    Setup,
    Template,
    Entry,
}

const NO_TEMPLATE: &str = "None: just the date as title";
const NO_ENTRY_TEMPLATE: &str = "None: a ## HH:MM heading";

/// A template chosen in a dropdown (`None`: no template).
type PickTemplate = Rc<dyn Fn(Option<String>, &mut Window, &mut App)>;
/// «Other file in this vault…»: the native picker.
type OtherTemplate = Rc<dyn Fn(&mut Window, &mut App)>;

/// A template dropdown: None, the templates folder's notes, another file.
fn template_menu(
    menu: PopupMenu,
    none_label: &'static str,
    current: &str,
    candidates: &[String],
    pick: PickTemplate,
    other: OtherTemplate,
) -> PopupMenu {
    let current = current.trim();
    let none = pick.clone();
    let mut menu = menu.item(
        PopupMenuItem::new(none_label)
            .checked(current.is_empty())
            .on_click(move |_, window, cx| none(None, window, cx)),
    );
    if !candidates.is_empty() {
        menu = menu.separator();
    }
    for candidate in candidates {
        let pick = pick.clone();
        let chosen = candidate.clone();
        menu = menu.item(
            PopupMenuItem::new(template_label(candidate))
                .checked(current == candidate)
                .on_click(move |_, window, cx| pick(Some(chosen.clone()), window, cx)),
        );
    }
    menu.separator().item(
        PopupMenuItem::new("Other file in this vault…")
            .on_click(move |_, window, cx| other(window, cx)),
    )
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
        self.daily_templates = self
            .creation_templates()
            .map(|catalog| {
                let candidates =
                    okilum_core::daily_notes::template_candidates(&self.vault_root, &catalog);
                Templates {
                    suggested: okilum_core::daily_notes::suggested_template(&candidates),
                    candidates,
                }
            })
            .unwrap_or_default();
        self.daily_notes = Some((self.vault_root.clone(), loaded.clone()));
        loaded
    }

    fn set_daily_settings(&mut self, settings: DailySettings, cx: &mut Context<Self>) -> bool {
        let Some(state) = self.session_directory.clone() else {
            return false;
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
        self.daily_notes_error.is_none()
    }

    /// The settings to change from Settings: those in effect (what the
    /// dialog would propose while nothing is set up), or the defaults when the
    /// stored preference cannot be read (choosing again repairs it).
    fn daily_settings(&mut self) -> DailySettings {
        self.ensure_daily_prefs();
        match &self.daily_notes {
            Some((_, Ok(prefs))) => effective_settings(prefs, &self.daily_templates),
            _ => DailySettings::default(),
        }
    }

    fn set_daily_format(&mut self, format: &str, cx: &mut Context<Self>) {
        let mut settings = self.daily_settings();
        settings.format = format.into();
        self.set_daily_settings(settings, cx);
    }

    fn set_daily_template(&mut self, template: Option<String>, cx: &mut Context<Self>) {
        let mut settings = self.daily_settings();
        settings.template = template.unwrap_or_default();
        if let Err(error) = settings.template_parts() {
            self.daily_notes_error = Some(format!("{error:#}"));
            cx.notify();
            return;
        }
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
                        this.daily_notes_error =
                            Some("Choose a folder inside this vault, not the vault itself.".into());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// A template outside the list: any Markdown note in this vault, picked
    /// natively, for the open dialog, the daily template or the entry template.
    fn choose_daily_template_file(&mut self, target: PickFor, cx: &mut Context<Self>) {
        let setup = target == PickFor::Setup;
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
                    let error = "Choose a Markdown note inside this vault.".to_owned();
                    match this.daily_setup.clone().filter(|_| setup) {
                        Some(dialog) => dialog.update(cx, |dialog, cx| {
                            dialog.error = Some(error);
                            cx.notify();
                        }),
                        None => this.daily_notes_error = Some(error),
                    }
                    cx.notify();
                    return;
                };
                match target {
                    PickFor::Setup => this.set_setup_template(Some(note), cx),
                    PickFor::Template => this.set_daily_template(Some(note), cx),
                    PickFor::Entry => this.set_daily_entry(Some(note), cx),
                }
            });
        })
        .detach();
    }

    fn set_setup_template(&mut self, template: Option<String>, cx: &mut Context<Self>) {
        if let Some(setup) = self.daily_setup.clone() {
            let check = DailySettings {
                template: template.clone().unwrap_or_default(),
                ..Default::default()
            };
            let checked = check.template_parts();
            setup.update(cx, |setup, cx| {
                match checked {
                    Ok(_) => {
                        setup.template = template;
                        setup.error = None;
                    }
                    Err(error) => setup.error = Some(format!("{error:#}")),
                }
                cx.notify();
            });
            cx.notify();
        }
    }

    /// Nothing set up yet: ask where, prefilled, instead of writing anything.
    fn begin_daily_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let folder = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(okilum_core::daily_notes::DEFAULT_FOLDER)
                .default_value(okilum_core::daily_notes::DEFAULT_FOLDER)
        });
        folder.update(cx, |input, cx| input.focus(window, cx));
        let setup = cx.new(|_| Setup {
            root: self.vault_root.clone(),
            folder,
            template: self.daily_templates.suggested.clone(),
            candidates: self.daily_templates.candidates.clone(),
            error: None,
        });
        self.daily_setup = Some(setup.clone());
        let reader = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            setup_dialog(dialog, &setup, reader.clone(), cx)
        });
        cx.notify();
    }

    /// The dialog's Create: save the choice, then make and open today's note.
    pub(super) fn confirm_daily_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(setup) = self.daily_setup.clone() else {
            return;
        };
        let (root, input, template) = {
            let setup = setup.read(cx);
            (
                setup.root.clone(),
                setup.folder.clone(),
                setup.template.clone().unwrap_or_default(),
            )
        };
        if root != self.vault_root {
            self.daily_setup = None;
            window.close_dialog(cx);
            return;
        }
        let folder = typed_folder(&input.read(cx).value());
        let settings = folder.and_then(|folder| {
            let settings = DailySettings {
                folder,
                format: okilum_core::daily_notes::DEFAULT_FORMAT.into(),
                template,
                entry: String::new(),
            };
            settings
                .relative_path(now())
                .and(settings.template_parts())
                .map_err(|error| format!("{error:#}"))?;
            Ok(settings)
        });
        let saved = match settings {
            Ok(settings) => {
                if self.set_daily_settings(settings, cx) {
                    Ok(())
                } else {
                    Err(self
                        .daily_notes_error
                        .clone()
                        .unwrap_or_else(|| "No preference storage is available.".into()))
                }
            }
            Err(error) => Err(error),
        };
        if let Err(error) = saved {
            setup.update(cx, |setup, cx| {
                setup.error = Some(error);
                cx.notify();
            });
            cx.notify();
            return;
        }
        self.daily_setup = None;
        window.close_dialog(cx);
        self.open_daily_note(window, cx);
    }

    pub(super) fn cancel_daily_setup(&mut self, cx: &mut Context<Self>) {
        if self.daily_setup.take().is_some() {
            cx.notify();
        }
    }

    /// «Today's note»: open today's note, or create it from the template.
    pub(super) fn open_daily_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.creation.is_some()
            || self.renaming.is_some()
            || self.trash_pending
            || self.note_move_pending
            || self.daily_setup.is_some()
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
        let prefs = match self.reload_daily_prefs() {
            Ok(prefs) => prefs,
            Err(error) => {
                reader_toast::error(format!("Cannot open today's note: {error}"), window, cx);
                return;
            }
        };
        if prefs.origin == Origin::Default {
            self.trace_file_operation("daily_note", "setup", "", None, cx);
            self.begin_daily_setup(window, cx);
            return;
        }
        let root = self.vault_root.clone();
        let result = (|| -> anyhow::Result<Today> {
            let state = self
                .session_directory
                .clone()
                .context("No draft recovery storage")?;
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
        if result.is_ok() {
            // From Inbox, Projects or Logs the note must come on screen,
            // as `open_note` does; Quick Look draws over the editor.
            if self.project_navigation.active.is_none() {
                self.close_project(window, cx);
            }
            self.project_navigation.section = reader_project_navigation::Section::Reader;
            self.tree_preview.close();
        }
        match result {
            Ok(Today::Existing(rel)) => self.append_daily_entry(rel, &prefs.settings, window, cx),
            Ok(Today::Created { rel, source }) => {
                self.trace_file_operation("daily_note", "created", &rel, None, cx);
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

impl Reader {
    /// A later press the same day (#3182): append a timed entry at the end of
    /// today's note and put the cursor under it. The note template is not
    /// applied again. An open editor saves and closes first, so the append
    /// goes through the note's own editor without a second lock holder.
    fn append_daily_entry(
        &mut self,
        rel: String,
        settings: &DailySettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pane.editing.is_some() && !self.leave_source(cx) {
            reader_toast::error(
                "Cannot add to today's note: resolve the current note's save first",
                window,
                cx,
            );
            return;
        }
        let Some(state) = self.session_directory.clone() else {
            return;
        };
        match okilum_core::daily_notes::append_entry(
            &self.vault_root,
            &rel,
            settings,
            now(),
            &state.join("editor-drafts"),
        ) {
            Ok((source, cursor)) => {
                self.trace_file_operation("daily_note", "entry", &rel, None, cx);
                self.show_written_note(rel, &source, cursor, window, cx);
            }
            Err(error) => {
                let error = format!("{error:#}");
                self.trace_file_operation("daily_note", "failed", &rel, Some(&error), cx);
                reader_toast::error(format!("Cannot add to today's note: {error}"), window, cx);
                self.open_note(&rel, None, window, cx);
            }
        }
    }

    fn set_daily_entry(&mut self, entry: Option<String>, cx: &mut Context<Self>) {
        let mut settings = self.daily_settings();
        settings.entry = entry.unwrap_or_default();
        if let Err(error) = settings.entry_parts() {
            self.daily_notes_error = Some(format!("{error:#}"));
            cx.notify();
            return;
        }
        self.set_daily_settings(settings, cx);
    }
}

/// The first-press dialog: folder and template, prefilled; Create or Cancel.
fn setup_dialog(
    dialog: gpui_component::dialog::Dialog,
    setup: &Entity<Setup>,
    reader: WeakEntity<Reader>,
    cx: &mut App,
) -> gpui_component::dialog::Dialog {
    let setup = setup.read(cx);
    let muted = brand::palette(cx).text_muted;
    let danger = brand::palette(cx).danger;
    let folder = setup.folder.clone();
    let typed = folder.read(cx).value().to_string();
    let preview = match typed_folder(&typed).and_then(|folder| {
        DailySettings {
            folder,
            ..Default::default()
        }
        .relative_path(now())
        .map_err(|error| format!("{error:#}"))
    }) {
        Ok(path) => format!("Today: {}", okilum_core::vault::note_path(&path)),
        Err(error) => error,
    };
    let template = setup.template.clone().unwrap_or_default();
    let candidates = setup.candidates.clone();
    let error = setup.error.clone();
    let pick_reader = reader.clone();
    let pick: PickTemplate = Rc::new(move |template, window, cx| {
        let _ = pick_reader.update(cx, |this, cx| this.set_setup_template(template, cx));
        window.refresh();
    });
    let other_reader = reader.clone();
    let other: OtherTemplate = Rc::new(move |_, cx| {
        let _ = other_reader.update(cx, |this, cx| {
            this.choose_daily_template_file(PickFor::Setup, cx)
        });
    });
    let (create, cancel, close, create_button) =
        (reader.clone(), reader.clone(), reader.clone(), reader);
    let row = |label: &'static str, control: AnyElement| {
        h_flex()
            .gap_3()
            .items_center()
            .child(div().w(px(80.)).flex_none().text_sm().child(label))
            .child(div().flex_1().min_w_0().child(control))
    };
    dialog
        .title("Set up daily notes")
        .width(px(460.))
        .child(
            v_flex()
                .gap_3()
                .debug_selector(|| "daily-setup".into())
                .child(div().text_sm().child(
                    "Today's note goes into a folder of its own, created from a template. \
                     You can change both later in Settings → Vault.",
                ))
                .child(row(
                    "Folder",
                    div()
                        .debug_selector(|| "daily-setup-folder".into())
                        .child(Input::new(&folder).small())
                        .into_any_element(),
                ))
                .child(row(
                    "Template",
                    Button::new("daily-setup-template")
                        .debug_selector(|| "daily-setup-template".into())
                        .outline()
                        .small()
                        .label(template_label(&template))
                        .dropdown_caret(true)
                        .dropdown_menu(move |menu, _, _| {
                            template_menu(
                                menu,
                                NO_TEMPLATE,
                                &template,
                                &candidates,
                                pick.clone(),
                                other.clone(),
                            )
                        })
                        .into_any_element(),
                ))
                .child(
                    div()
                        .debug_selector(|| "daily-setup-preview".into())
                        .text_sm()
                        .text_color(muted)
                        .child(preview),
                )
                .children(error.map(|error| {
                    div()
                        .debug_selector(|| "daily-setup-error".into())
                        .text_sm()
                        .text_color(danger)
                        .child(error)
                })),
        )
        // Enter in the folder field creates, as the button does.
        .on_ok(move |_, window, cx| {
            let _ = create.update(cx, |this, cx| this.confirm_daily_setup(window, cx));
            false
        })
        .on_close(move |_, _, cx| {
            let _ = close.update(cx, |this, cx| this.cancel_daily_setup(cx));
        })
        .footer(
            h_flex()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("daily-setup-cancel")
                        .debug_selector(|| "daily-setup-cancel".into())
                        .ghost()
                        .label("Cancel")
                        .on_click(move |_, window, cx| {
                            let _ = cancel.update(cx, |this, cx| this.cancel_daily_setup(cx));
                            window.close_dialog(cx);
                        }),
                )
                .child({
                    let create = create_button;
                    Button::new("daily-setup-create")
                        .debug_selector(|| "daily-setup-create".into())
                        .primary()
                        .label("Create today's note")
                        .on_click(move |_, window, cx| {
                            let _ =
                                create.update(cx, |this, cx| this.confirm_daily_setup(window, cx));
                        })
                }),
        )
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
    let templates = state.daily_templates.clone();
    let today = now();
    let summary = prefs
        .as_ref()
        .map(|prefs| effective_summary(prefs, &templates, today));
    let settings = prefs
        .as_ref()
        .map(|prefs| effective_settings(prefs, &templates))
        .unwrap_or_default();
    let from_obsidian = prefs
        .as_ref()
        .is_some_and(|prefs| prefs.origin == Origin::Obsidian);
    let folder = settings
        .folder()
        .map(str::to_owned)
        .unwrap_or_else(|error| format!("{error:#}"));
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
    let template = settings.template.clone();
    let candidates = templates.candidates.clone();
    let entry = settings.entry.clone();
    let entry_candidates = templates.candidates.clone();
    let entry_reader = reader.downgrade();
    let pick_entry: PickTemplate = Rc::new(move |entry, _, cx| {
        let _ = entry_reader.update(cx, |this, cx| this.set_daily_entry(entry, cx));
    });
    let other_entry_reader = reader.downgrade();
    let other_entry: OtherTemplate = Rc::new(move |_, cx| {
        let _ = other_entry_reader.update(cx, |this, cx| {
            this.choose_daily_template_file(PickFor::Entry, cx)
        });
    });
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
    let (folder_reader, format_reader) = (reader.clone(), reader.clone());
    let pick_reader = reader.downgrade();
    let pick: PickTemplate = Rc::new(move |template, _, cx| {
        let _ = pick_reader.update(cx, |this, cx| this.set_daily_template(template, cx));
    });
    let other_reader = reader.downgrade();
    let other: OtherTemplate = Rc::new(move |_, cx| {
        let _ = other_reader.update(cx, |this, cx| {
            this.choose_daily_template_file(PickFor::Template, cx)
        });
    });
    v_flex()
        .gap_2()
        .debug_selector(|| "settings-daily-notes".into())
        .child(setting_row(
            "Daily notes folder",
            if from_obsidian {
                "Where Today's note goes. Taken from Obsidian's Daily notes settings."
            } else {
                "Where Today's note goes; created on first use."
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
            "From the templates folder; {{date}}, {{date:FORMAT}}, {{time}} and {{title}} are filled in.",
            Button::new("settings-daily-template")
                .debug_selector(|| "settings-daily-template".into())
                .outline()
                .small()
                .label(template_label(&template))
                .dropdown_caret(true)
                .disabled(!storage)
                .dropdown_menu(move |menu, _, _| {
                    template_menu(menu, NO_TEMPLATE, &template, &candidates, pick.clone(), other.clone())
                }),
            cx,
        ))
        .child(setting_row(
            "Later entries",
            "Each later press the same day adds this at the end of the note, with the cursor under it.",
            Button::new("settings-daily-entry")
                .debug_selector(|| "settings-daily-entry".into())
                .outline()
                .small()
                .label(if entry.trim().is_empty() {
                    "## HH:MM".to_owned()
                } else {
                    template_label(&entry)
                })
                .dropdown_caret(true)
                .disabled(!storage)
                .dropdown_menu(move |menu, _, _| {
                    template_menu(
                        menu,
                        NO_ENTRY_TEMPLATE,
                        &entry,
                        &entry_candidates,
                        pick_entry.clone(),
                        other_entry.clone(),
                    )
                }),
            cx,
        ))
        .children(summary.map(|summary| {
            div()
                .debug_selector(|| "settings-daily-effective".into())
                .text_sm()
                .text_color(muted)
                .child(summary)
        }))
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
            entry: String::new(),
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
    fn folders_must_be_inside_the_vault_and_never_the_vault_itself() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir_all(root.join("Daily/2026")).unwrap();
        std::fs::create_dir_all(dir.path().join("outside")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("outside"), root.join("Link")).unwrap();
        assert_eq!(folder_in_vault(&root, &root), None, "never the vault root");
        assert_eq!(
            folder_in_vault(&root, &root.join("Daily/2026")).as_deref(),
            Some("Daily/2026")
        );
        assert_eq!(folder_in_vault(&root, &dir.path().join("outside")), None);
        assert_eq!(folder_in_vault(&root, &root.join("Link")), None);
        assert_eq!(typed_folder(" /Daily/ ").as_deref(), Ok("Daily"));
        assert_eq!(typed_folder("Journal/Days").as_deref(), Ok("Journal/Days"));
        for bad in ["", "/", "  ", "../out", "a//b", "./Daily"] {
            assert!(typed_folder(bad).is_err(), "{bad:?}");
        }
        assert_eq!(template_label(""), "None");
        assert_eq!(template_label("Templates/Daily.md"), "Templates/Daily");
    }

    #[test]
    fn settings_say_what_todays_note_will_use_and_where_it_comes_from() {
        let now = time::macros::datetime!(2026-11-20 08:30 +02:00);
        let templates = Templates {
            candidates: vec!["Templates/Daily.md".into(), "Templates/Meeting.md".into()],
            suggested: Some("Templates/Daily.md".into()),
        };
        let prefs = |origin, folder: &str, template: &str| Prefs {
            settings: DailySettings {
                folder: folder.into(),
                format: String::new(),
                template: template.into(),
                entry: String::new(),
            },
            origin,
        };
        assert_eq!(
            effective_summary(&prefs(Origin::Default, "", ""), &templates, now),
            "Not set up yet. The first Today's note asks, proposing Daily/2026-11-20.md · template: Templates/Daily"
        );
        assert_eq!(
            effective_summary(
                &prefs(Origin::Obsidian, "Journal/", "Templates/Meeting"),
                &templates,
                now
            ),
            "Today's note: Journal/2026-11-20.md · template: Templates/Meeting · from Obsidian's Daily notes settings"
        );
        assert_eq!(
            effective_summary(&prefs(Origin::Okilum, "", ""), &templates, now),
            "Today's note: Daily/2026-11-20.md · template: None"
        );
    }

    fn open_vault<'a>(
        cx: &'a mut TestAppContext,
        temp: &tempfile::TempDir,
    ) -> (Entity<Reader>, &'a mut VisualTestContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            crate::test_init(cx);
            bind_keys(cx);
        });
        let root = temp.path().join("vault");
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Start.md".into()),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(temp.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        visual.simulate_resize(size(px(1200.), px(800.)));
        visual.run_until_parked();
        (entity.unwrap(), visual)
    }

    fn vault_with_daily_template() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(root.join("Templates")).unwrap();
        std::fs::write(root.join("Start.md"), "# Start\n").unwrap();
        std::fs::write(
            root.join("Templates/Daily.md"),
            "# {{title}}\n{{date:dddd, D MMMM YYYY}}\n\n## Plan\n",
        )
        .unwrap();
        std::fs::write(root.join("Templates/Meeting.md"), "# Meeting\n").unwrap();
        temp
    }

    fn root_entries(root: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// #3182: with nothing set up, the first press asks (folder Daily and the
    /// daily template prefilled) and writes nothing; Create saves the choice,
    /// creates Daily/<date>.md from the template and opens it; the next press
    /// opens it without asking.
    #[gpui::test]
    fn first_press_asks_with_prefilled_folder_and_template_then_creates_in_daily(
        cx: &mut TestAppContext,
    ) {
        let temp = vault_with_daily_template();
        let root = temp.path().join("vault");
        let before = root_entries(&root);
        let (reader, visual) = open_vault(cx, &temp);
        reader.update_in(visual, |r, window, cx| r.open_daily_note(window, cx));
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("daily-setup").is_some(),
            "the dialog asks"
        );
        assert!(visual.debug_bounds("daily-setup-create").is_some());
        reader.read_with(visual, |r, cx| {
            let setup = r.daily_setup.as_ref().expect("setup is open").read(cx);
            assert_eq!(setup.folder.read(cx).value().to_string(), "Daily");
            assert_eq!(setup.template.as_deref(), Some("Templates/Daily.md"));
            assert_eq!(
                setup.candidates,
                ["Templates/Daily.md", "Templates/Meeting.md"]
            );
        });
        assert_eq!(root_entries(&root), before, "asking writes nothing");
        let create = visual.debug_bounds("daily-setup-create").unwrap();
        visual.simulate_click(create.center(), Modifiers::default());
        visual.run_until_parked();
        let now = now();
        let date = DailySettings::default().name(now).unwrap();
        let rel = format!("Daily/{date}.md");
        let long = DailySettings {
            format: "dddd, D MMMM YYYY".into(),
            ..Default::default()
        }
        .name(now)
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join(&rel)).unwrap(),
            format!("# {date}\n{long}\n\n## Plan\n"),
            "created from the template with Obsidian variables"
        );
        assert!(
            visual.debug_bounds("daily-setup").is_none(),
            "dialog closed"
        );
        reader.read_with(visual, |r, _| {
            assert!(r.daily_setup.is_none());
            assert_eq!(r.pane.current_rel, rel);
            assert!(r.pane.editing.is_some());
            let state = r.session_directory.clone().unwrap();
            let saved = load(&state, &r.vault_root).unwrap();
            assert_eq!(saved.origin, Origin::Okilum);
            assert_eq!(saved.settings.folder, "Daily");
            assert_eq!(saved.settings.template, "Templates/Daily.md");
        });
        let mut after = before.clone();
        after.push("Daily".into());
        after.sort();
        assert_eq!(root_entries(&root), after, "nothing in the vault root");
        reader.update_in(visual, |r, window, cx| {
            r.open_note("Start.md", None, window, cx)
        });
        visual.run_until_parked();
        // Second press the same day: no question, no second template; a
        // `## HH:MM` entry at the end, the cursor on the line under it.
        let before = hhmm(super::now());
        reader.update_in(visual, |r, window, cx| r.open_daily_note(window, cx));
        visual.run_until_parked();
        let after = hhmm(super::now());
        assert!(
            visual.debug_bounds("daily-setup").is_none(),
            "asks only once"
        );
        let text = std::fs::read_to_string(root.join(&rel)).unwrap();
        let base = format!("# {date}\n{long}\n\n## Plan\n");
        assert!(
            text == format!("{base}\n## {before}\n") || text == format!("{base}\n## {after}\n"),
            "the entry is appended once, the template not again: {text:?}"
        );
        reader.read_with(visual, |r, cx| {
            assert_eq!(r.pane.current_rel, rel);
            let editing = r.pane.editing.as_ref().expect("open for writing");
            assert_eq!(
                editing.input().read(cx).cursor(),
                text.len(),
                "under the heading"
            );
            assert_eq!(r.source_text(cx).as_deref(), Some(text.as_str()));
        });
    }

    fn hhmm(at: time::OffsetDateTime) -> String {
        format!("{:02}:{:02}", at.hour(), at.minute())
    }

    /// Cancel and a folder that is the vault root create nothing.
    #[gpui::test]
    fn setup_cancel_and_root_folder_create_nothing(cx: &mut TestAppContext) {
        let temp = vault_with_daily_template();
        let root = temp.path().join("vault");
        let before = root_entries(&root);
        let (reader, visual) = open_vault(cx, &temp);
        reader.update_in(visual, |r, window, cx| r.open_daily_note(window, cx));
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            let folder = r.daily_setup.as_ref().unwrap().read(cx).folder.clone();
            folder.update(cx, |input, cx| input.set_value("/", window, cx));
            r.confirm_daily_setup(window, cx);
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("daily-setup-error").is_some());
        reader.read_with(visual, |r, cx| {
            let setup = r.daily_setup.as_ref().expect("still asking").read(cx);
            assert!(setup
                .error
                .as_deref()
                .unwrap()
                .contains("never go into the vault root"));
        });
        let cancel = visual.debug_bounds("daily-setup-cancel").unwrap();
        visual.simulate_click(cancel.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("daily-setup").is_none());
        reader.read_with(visual, |r, _| {
            assert!(r.daily_setup.is_none());
            let state = r.session_directory.clone().unwrap();
            assert_eq!(load(&state, &r.vault_root).unwrap().origin, Origin::Default);
        });
        assert_eq!(root_entries(&root), before, "nothing created");
    }

    /// A vault set up in Obsidian works as it is: no question, its folder,
    /// name format and template.
    #[gpui::test]
    fn obsidian_daily_notes_settings_are_used_without_asking(cx: &mut TestAppContext) {
        let temp = vault_with_daily_template();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(root.join(".obsidian")).unwrap();
        std::fs::write(
            root.join(".obsidian/daily-notes.json"),
            r#"{"folder":"Journal/","format":"YYYY/YYYY-MM-DD","template":"Templates/Daily"}"#,
        )
        .unwrap();
        let (reader, visual) = open_vault(cx, &temp);
        reader.update_in(visual, |r, window, cx| r.open_daily_note(window, cx));
        visual.run_until_parked();
        assert!(visual.debug_bounds("daily-setup").is_none(), "no question");
        let now = now();
        let date = DailySettings::default().name(now).unwrap();
        let rel = format!("Journal/{}/{date}.md", now.year());
        let source = std::fs::read_to_string(root.join(&rel)).unwrap();
        assert!(source.starts_with(&format!("# {date}\n")), "{source}");
        assert!(source.ends_with("## Plan\n"), "{source}");
        reader.read_with(visual, |r, _| assert_eq!(r.pane.current_rel, rel));
    }
}
