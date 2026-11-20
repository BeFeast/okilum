//! Daily notes (#3182): one note per day, in a chosen folder, named by a
//! date format and created from an optional template. The settings mirror
//! Obsidian's core Daily notes plugin (`.obsidian/daily-notes.json`), so a
//! vault that used it keeps its folder, name format and template.
use crate::note_templates::{self, Catalog};
use anyhow::{bail, ensure, Context, Result};
use std::path::{Component, Path, PathBuf};
use time::OffsetDateTime;

/// Obsidian's default name: `2026-11-20.md`.
pub const DEFAULT_FORMAT: &str = "YYYY-MM-DD";
/// Where daily notes go when no folder is set, in Okilum or in Obsidian's
/// settings. Never the vault root (#3182): created on first use.
pub const DEFAULT_FOLDER: &str = "Daily";
/// What a later press the same day appends when no entry template is set:
/// a heading with the local time (#3182).
pub const DEFAULT_ENTRY: &str = "## {{time:HH:mm}}\n";
/// Offered for templates when the vault's templates folder is not there.
pub const FALLBACK_TEMPLATES: &str = "Templates";

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Vault-relative folder; empty is [`DEFAULT_FOLDER`].
    pub folder: String,
    /// Moment-style name; a `/` in it makes subfolders (`YYYY/MM/YYYY-MM-DD`).
    /// Empty is [`DEFAULT_FORMAT`].
    pub format: String,
    /// Vault-relative template note, `.md` optional as in Obsidian; empty is
    /// no template. Applied only when the day's note is created.
    pub template: String,
    /// Vault-relative entry template: what each later press the same day
    /// appends. Empty is [`DEFAULT_ENTRY`]. Okilum only; Obsidian has none.
    pub entry: String,
}

/// What «Today's note» found or made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Today {
    /// Today's note is already there; it is opened, never rewritten.
    Existing(String),
    /// Created now with this source.
    Created { rel: String, source: String },
}

/// `/Daily/` and `Daily` are the same folder in Obsidian's settings file.
fn trimmed(path: &str) -> &str {
    path.trim().trim_matches('/')
}

fn inside_vault(path: &str) -> bool {
    Path::new(path)
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
}

impl Settings {
    /// The settings Obsidian's Daily notes plugin left in this vault, if any.
    /// Read only: Okilum keeps its own choice outside the vault.
    pub fn from_obsidian(root: &Path) -> Result<Option<Self>> {
        let Some(folder) = note_templates::open_folder(root, Path::new(".obsidian"))? else {
            return Ok(None);
        };
        note_templates::read_file(&folder, "daily-notes.json")?
            .map(|text| {
                serde_json::from_str::<Self>(&text).context("Invalid .obsidian/daily-notes.json")
            })
            .transpose()
    }

    pub fn format(&self) -> &str {
        match self.format.trim() {
            "" => DEFAULT_FORMAT,
            format => format,
        }
    }

    /// Vault-relative folder; [`DEFAULT_FOLDER`] when none is set, so a
    /// daily note never lands in the vault root.
    pub fn folder(&self) -> Result<&str> {
        let folder = match trimmed(&self.folder) {
            "" => DEFAULT_FOLDER,
            folder => folder,
        };
        ensure!(
            inside_vault(folder),
            "Choose a daily notes folder inside the vault"
        );
        Ok(folder)
    }

    /// The note's name for `now`, as the format spells it (may hold `/`).
    pub fn name(&self, now: OffsetDateTime) -> Result<String> {
        let format = self.format();
        note_templates::moment(format, now).with_context(|| {
            format!("The daily note format «{format}» is not supported. Use YYYY, MM, DD and separators")
        })
    }

    /// Where today's note lives: `folder/name.md`.
    pub fn relative_path(&self, now: OffsetDateTime) -> Result<PathBuf> {
        crate::note_files::typed_path(Path::new(self.folder()?), &self.name(now)?, false)
    }

    /// The template as (folder, file name), `None` without one.
    pub fn template_parts(&self) -> Result<Option<(String, String)>> {
        note_parts(&self.template, "daily note template")
    }

    /// The entry template as (folder, file name), `None` for the default.
    pub fn entry_parts(&self) -> Result<Option<(String, String)>> {
        note_parts(&self.entry, "entry template")
    }
}

/// A template note setting as (folder, file name); `.md` optional.
fn note_parts(value: &str, what: &str) -> Result<Option<(String, String)>> {
    {
        let template = trimmed(value);
        if template.is_empty() {
            return Ok(None);
        }
        let mut template = template.to_owned();
        if !Path::new(&template)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        {
            template.push_str(".md");
        }
        // A template at the vault root would make the whole vault its
        // templates folder, and every note in it a template.
        let (folder, file) = template
            .rsplit_once('/')
            .with_context(|| format!("Put the {what} in a folder, not in the vault root"))?;
        ensure!(
            inside_vault(folder) && !file.is_empty(),
            "Choose a {what} inside the vault"
        );
        Ok(Some((folder.to_owned(), file.to_owned())))
    }
}

/// Template notes to offer for daily notes, vault-relative: the Markdown
/// files of the vault's templates folder (Settings, `.obsidian/templates.json`
/// or the default) and, when that has none, of `Templates/`.
pub fn template_candidates(root: &Path, templates: &Catalog) -> Vec<String> {
    let mut folder = templates.folder.clone();
    let mut files = templates.files.clone();
    if files.is_empty() && folder != Path::new(FALLBACK_TEMPLATES) {
        if let Ok(fallback) = Catalog::load_with_folder(root, Some(FALLBACK_TEMPLATES)) {
            folder = fallback.folder;
            files = fallback.files;
        }
    }
    files
        .into_iter()
        .map(|file| crate::vault::note_path(&folder.join(file)))
        .collect()
}

/// The candidate that is plainly a daily template: `Daily.md`,
/// `Daily Note.md`, … first, then any name with «daily» in it.
pub fn suggested_template(candidates: &[String]) -> Option<String> {
    let stem = |path: &str| {
        Path::new(path)
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    candidates
        .iter()
        .find(|path| {
            matches!(
                stem(path).as_str(),
                "daily" | "daily note" | "daily notes" | "day"
            )
        })
        .or_else(|| candidates.iter().find(|path| stem(path).contains("daily")))
        .cloned()
}

/// A later press the same day (#3182): append the entry (the entry template,
/// or `## HH:MM`) at the end of today's note through the note's own editor,
/// so a change on disk at the same moment is refused, never overwritten.
/// Returns the new source and where typing starts: the line under the
/// entry's first line. The note keeps its line endings.
pub fn append_entry(
    root: &Path,
    rel: &str,
    settings: &Settings,
    now: OffsetDateTime,
    drafts: &Path,
) -> Result<(String, usize)> {
    let title = Path::new(rel)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .context("Use a UTF-8 note name")?;
    let mut entry = match settings.entry_parts()? {
        Some((folder, file)) => {
            let catalog = Catalog::load_with_folder(root, Some(&folder))?;
            ensure!(
                catalog.files.iter().any(|name| name == &file),
                "The entry template «{}» is not in this vault. Choose it in Settings → Vault",
                settings.entry
            );
            catalog.source(root, Some(&file), title, now)?
        }
        None => note_templates::expand(DEFAULT_ENTRY, title, now, DEFAULT_FORMAT, "HH:mm"),
    };
    let mut editor = crate::file_editor::FileEditor::open(&root.join(rel), drafts)?;
    let mut source = editor.text().to_owned();
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    entry = entry.replace("\r\n", "\n");
    if !entry.ends_with('\n') {
        entry.push('\n');
    }
    let entry = entry.replace('\n', newline);
    if !source.is_empty() {
        if !source.ends_with(newline) {
            source.push_str(newline);
        }
        if !source.ends_with(&newline.repeat(2)) {
            source.push_str(newline);
        }
    }
    let cursor = source.len()
        + entry
            .find(newline)
            .map_or(entry.len(), |end| end + newline.len());
    source.push_str(&entry);
    editor.set_text(source.clone())?;
    match editor.save()? {
        crate::file_editor::Save::Saved => Ok((source, cursor)),
        crate::file_editor::Save::Conflict => {
            bail!("Today's note changed on disk at the same moment; nothing was added. Try again")
        }
    }
}

/// Open today's note, or create it. Without a daily template the note is
/// just its title, as any new note; `templates` is the vault's templates
/// catalog, which keeps the note out of the templates folder.
pub fn open_or_create(
    root: &Path,
    settings: &Settings,
    templates: &Catalog,
    now: OffsetDateTime,
    drafts: &Path,
) -> Result<Today> {
    let relative = settings.relative_path(now)?;
    let rel = crate::vault::note_path(&relative);
    let path = root.join(&relative);
    if path.is_file() {
        return Ok(Today::Existing(rel));
    }
    match std::fs::symlink_metadata(&path) {
        Ok(_) => bail!("{rel} is already there and is not a note"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("Cannot check {rel}")),
    }
    let (catalog, selected) = match settings.template_parts()? {
        Some((folder, file)) => {
            let catalog = Catalog::load_with_folder(root, Some(&folder))?;
            ensure!(
                catalog.files.iter().any(|name| name == &file),
                "The daily note template «{}» is not in this vault. Choose it in Settings → Vault",
                settings.template
            );
            (catalog, Some(file))
        }
        None => (templates.clone(), None),
    };
    let source = crate::note_files::create_from_template(
        root,
        &relative,
        now,
        drafts,
        &catalog,
        selected.as_deref(),
    )?;
    Ok(Today::Created { rel, source })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    const NOW: OffsetDateTime = time::macros::datetime!(2026-11-20 08:30 +02:00);

    fn create(root: &Path, settings: &Settings) -> Result<Today> {
        let drafts = root.parent().unwrap().join("drafts");
        std::fs::create_dir_all(&drafts)?;
        open_or_create(root, settings, &Catalog::load(root)?, NOW, &drafts)
    }

    #[test]
    fn todays_path_uses_folder_and_format() {
        let settings = |folder: &str, format: &str| Settings {
            folder: folder.into(),
            format: format.into(),
            template: String::new(),
            entry: String::new(),
        };
        let path = |s: Settings| s.relative_path(NOW).unwrap();
        // Nothing set: the Daily folder, never the vault root (#3182).
        assert_eq!(path(settings("", "")), Path::new("Daily/2026-11-20.md"));
        assert_eq!(path(settings("/", "")), Path::new("Daily/2026-11-20.md"));
        assert_eq!(
            path(settings("/Daily/", "")),
            Path::new("Daily/2026-11-20.md")
        );
        assert_eq!(
            path(settings("Journal", "YYYY/MM/YYYY-MM-DD")),
            Path::new("Journal/2026/11/2026-11-20.md")
        );
        assert_eq!(
            path(settings("", "DD.MM.YYYY")),
            Path::new("Daily/20.11.2026.md")
        );
        assert_eq!(
            path(settings("Journal", "YYYY/MMMM/dddd D")),
            Path::new("Journal/2026/November/Friday 20.md")
        );
        assert_eq!(path(settings("", "dddd")), Path::new("Daily/Friday.md"));
        assert!(settings("", "Do").relative_path(NOW).is_err());
        assert!(settings("../out", "").relative_path(NOW).is_err());
    }

    #[test]
    fn obsidian_settings_are_read_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir_all(root.join(".obsidian")).unwrap();
        assert_eq!(Settings::from_obsidian(&root).unwrap(), None);
        let json = r#"{"folder":"Daily/","format":"YYYY/YYYY-MM-DD","template":"Templates/Day","autorun":true}"#;
        std::fs::write(root.join(".obsidian/daily-notes.json"), json).unwrap();
        let settings = Settings::from_obsidian(&root).unwrap().unwrap();
        assert_eq!(settings.folder, "Daily/");
        assert_eq!(settings.format(), "YYYY/YYYY-MM-DD");
        assert_eq!(
            settings.template_parts().unwrap(),
            Some(("Templates".into(), "Day.md".into()))
        );
        assert_eq!(
            std::fs::read_to_string(root.join(".obsidian/daily-notes.json")).unwrap(),
            json
        );
        let root_template = Settings {
            template: "Day".into(),
            ..Default::default()
        };
        assert!(root_template.template_parts().is_err());
    }

    #[test]
    fn todays_note_is_created_once_from_its_template_and_then_opened() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir_all(root.join("Templates")).unwrap();
        std::fs::write(
            root.join("Templates/Daily.md"),
            "# {{title}}\n\n{{date:DD.MM.YYYY}}\n\n## Tasks\n",
        )
        .unwrap();
        let settings = Settings {
            folder: "Daily".into(),
            format: "YYYY/MM/YYYY-MM-DD".into(),
            template: "Templates/Daily".into(),
            entry: String::new(),
        };
        let created = create(&root, &settings).unwrap();
        let rel = "Daily/2026/11/2026-11-20.md";
        let source = "# 2026-11-20\n\n20.11.2026\n\n## Tasks\n";
        assert_eq!(
            created,
            Today::Created {
                rel: rel.into(),
                source: source.into()
            }
        );
        assert_eq!(std::fs::read_to_string(root.join(rel)).unwrap(), source);
        // The second press opens what the person wrote; nothing is rewritten.
        std::fs::write(root.join(rel), "my day").unwrap();
        assert_eq!(
            create(&root, &settings).unwrap(),
            Today::Existing(rel.into())
        );
        assert_eq!(std::fs::read_to_string(root.join(rel)).unwrap(), "my day");
    }

    #[test]
    fn no_template_gives_the_title_and_a_missing_template_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        let missing = Settings {
            template: "Templates/Gone.md".into(),
            ..Default::default()
        };
        let error = create(&root, &missing).unwrap_err().to_string();
        assert!(error.contains("Templates/Gone.md"), "{error}");
        assert!(!root.join("Daily").exists(), "nothing is created");
        let plain = create(&root, &Settings::default()).unwrap();
        assert_eq!(
            plain,
            Today::Created {
                rel: "Daily/2026-11-20.md".into(),
                source: "# 2026-11-20\n".into()
            }
        );
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            1,
            "only the Daily folder: nothing in the vault root"
        );
        // A folder where the note should be is not opened as a note.
        let blocked = Settings {
            format: "[x]".into(),
            ..Default::default()
        };
        std::fs::create_dir_all(root.join("Daily/x.md")).unwrap();
        assert!(create(&root, &blocked).is_err());
    }

    #[test]
    fn a_later_press_appends_a_timed_entry_and_the_template_applies_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir_all(root.join("Templates")).unwrap();
        std::fs::write(root.join("Templates/Daily.md"), "# {{title}}\n\n## Plan\n").unwrap();
        let drafts = dir.path().join("drafts");
        std::fs::create_dir_all(&drafts).unwrap();
        let settings = Settings {
            template: "Templates/Daily".into(),
            ..Default::default()
        };
        let Today::Created { rel, .. } = create(&root, &settings).unwrap() else {
            panic!("first press creates");
        };
        assert_eq!(rel, "Daily/2026-11-20.md");
        // Second press: the template is not applied again; an entry is added.
        assert_eq!(
            create(&root, &settings).unwrap(),
            Today::Existing(rel.clone())
        );
        let at = time::macros::datetime!(2026-11-20 14:05 +02:00);
        let (source, cursor) = append_entry(&root, &rel, &settings, at, &drafts).unwrap();
        assert_eq!(source, "# 2026-11-20\n\n## Plan\n\n## 14:05\n");
        assert_eq!(cursor, source.len(), "the empty line under the heading");
        assert_eq!(std::fs::read_to_string(root.join(&rel)).unwrap(), source);
        // A note ending without a newline, and one with CRLF, stay tidy.
        std::fs::write(root.join(&rel), "a\r\nb").unwrap();
        let later = time::macros::datetime!(2026-11-20 18:30 +02:00);
        let (source, cursor) = append_entry(&root, &rel, &settings, later, &drafts).unwrap();
        assert_eq!(source, "a\r\nb\r\n\r\n## 18:30\r\n");
        assert_eq!(cursor, source.len());
        // An entry template: expanded, typing starts under its first line.
        std::fs::write(
            root.join("Templates/Entry.md"),
            "### {{time:HH:mm}} {{title}}\n- \n",
        )
        .unwrap();
        let custom = Settings {
            entry: "Templates/Entry".into(),
            ..settings.clone()
        };
        std::fs::write(root.join(&rel), "x\n").unwrap();
        let (source, cursor) = append_entry(&root, &rel, &custom, at, &drafts).unwrap();
        assert_eq!(source, "x\n\n### 14:05 2026-11-20\n- \n");
        assert_eq!(&source[cursor..], "- \n");
        let gone = Settings {
            entry: "Templates/Gone".into(),
            ..settings
        };
        assert!(append_entry(&root, &rel, &gone, at, &drafts).is_err());
        assert_eq!(
            std::fs::read_to_string(root.join(&rel)).unwrap(),
            source,
            "nothing added"
        );
    }

    #[test]
    fn templates_are_offered_from_the_templates_folder_or_templates() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir_all(root.join("Templates")).unwrap();
        std::fs::write(root.join("Templates/Meeting.md"), "").unwrap();
        std::fs::write(root.join("Templates/Daily Note.md"), "").unwrap();
        std::fs::write(root.join("Templates/notes.txt"), "").unwrap();
        // The default templates folder is not there: Templates/ is offered.
        let candidates = template_candidates(&root, &Catalog::load(&root).unwrap());
        assert_eq!(
            candidates,
            ["Templates/Daily Note.md", "Templates/Meeting.md"]
        );
        assert_eq!(
            suggested_template(&candidates).as_deref(),
            Some("Templates/Daily Note.md")
        );
        // Obsidian's templates folder wins when it has templates.
        std::fs::create_dir_all(root.join(".obsidian")).unwrap();
        std::fs::create_dir_all(root.join("_meta/tpl")).unwrap();
        std::fs::write(root.join("_meta/tpl/My daily plan.md"), "").unwrap();
        std::fs::write(
            root.join(".obsidian/templates.json"),
            r#"{"folder":"_meta/tpl"}"#,
        )
        .unwrap();
        let candidates = template_candidates(&root, &Catalog::load(&root).unwrap());
        assert_eq!(candidates, ["_meta/tpl/My daily plan.md"]);
        assert_eq!(
            suggested_template(&candidates).as_deref(),
            Some("_meta/tpl/My daily plan.md")
        );
        assert_eq!(suggested_template(&["Templates/Meeting.md".into()]), None);
    }
}
