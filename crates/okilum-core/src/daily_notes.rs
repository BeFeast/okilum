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

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Vault-relative folder; empty is the vault root.
    pub folder: String,
    /// Moment-style name; a `/` in it makes subfolders (`YYYY/MM/YYYY-MM-DD`).
    /// Empty is [`DEFAULT_FORMAT`].
    pub format: String,
    /// Vault-relative template note, `.md` optional as in Obsidian; empty is
    /// no template.
    pub template: String,
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

    /// Vault-relative folder, `""` for the vault root.
    pub fn folder(&self) -> Result<&str> {
        let folder = trimmed(&self.folder);
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
        let template = trimmed(&self.template);
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
            .context("Put the daily note template in a folder, not in the vault root")?;
        ensure!(
            inside_vault(folder) && !file.is_empty(),
            "Choose a daily note template inside the vault"
        );
        Ok(Some((folder.to_owned(), file.to_owned())))
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
        };
        let path = |s: Settings| s.relative_path(NOW).unwrap();
        assert_eq!(path(settings("", "")), Path::new("2026-11-20.md"));
        assert_eq!(
            path(settings("/Daily/", "")),
            Path::new("Daily/2026-11-20.md")
        );
        assert_eq!(
            path(settings("Journal", "YYYY/MM/YYYY-MM-DD")),
            Path::new("Journal/2026/11/2026-11-20.md")
        );
        assert_eq!(path(settings("", "DD.MM.YYYY")), Path::new("20.11.2026.md"));
        assert!(settings("", "dddd").relative_path(NOW).is_err());
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
        let plain = create(&root, &Settings::default()).unwrap();
        assert_eq!(
            plain,
            Today::Created {
                rel: "2026-11-20.md".into(),
                source: "# 2026-11-20\n".into()
            }
        );
        let missing = Settings {
            folder: "Daily".into(),
            template: "Templates/Gone.md".into(),
            ..Default::default()
        };
        let error = create(&root, &missing).unwrap_err().to_string();
        assert!(error.contains("Templates/Gone.md"), "{error}");
        assert!(!root.join("Daily").exists());
        // A folder where the note should be is not opened as a note.
        std::fs::create_dir_all(root.join("Daily/2026-11-20.md")).unwrap();
        let blocked = Settings {
            folder: "Daily".into(),
            ..Default::default()
        };
        assert!(create(&root, &blocked).is_err());
    }
}
