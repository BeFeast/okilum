//! Ordinary-note templates compatible with Obsidian's template folder/variables.
use anyhow::{ensure, Context, Result};
#[cfg(unix)]
use rustix::{
    fd::OwnedFd,
    fs::{open, openat, Dir, Mode, OFlags},
};
#[cfg(unix)]
use std::fs::File;
use std::{
    io::Read,
    path::{Component, Path, PathBuf},
};
#[cfg(windows)]
type OwnedFd = crate::windows_files::Directory;
use time::OffsetDateTime;

pub const DEFAULT_FOLDER: &str = "_Assets/Templates";
/// A new note without a chosen template is just its title: front matter the
/// person did not write makes a first note look like a config file (#1352).
/// A `Note.md` in the templates folder still applies by default.
const DEFAULT_SOURCE: &str = "# {{title}}\n";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalog {
    pub folder: PathBuf,
    pub files: Vec<String>,
    /// Chosen in Settings or `.obsidian/templates.json`, not the default.
    chosen: bool,
    exists: bool,
    date_format: String,
    time_format: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    #[serde(default)]
    folder: String,
    #[serde(default = "default_date")]
    date_format: String,
    #[serde(default = "default_time")]
    time_format: String,
}
fn default_date() -> String {
    "YYYY-MM-DD".into()
}
fn default_time() -> String {
    "HH:mm".into()
}

fn open_folder(root: &Path, relative: &Path) -> Result<Option<OwnedFd>> {
    ensure!(
        relative
            .components()
            .all(|p| matches!(p, Component::Normal(_))),
        "Templates folder must be inside the vault"
    );
    #[cfg(windows)]
    {
        let mut folder = crate::windows_files::Directory::open(root)?;
        for part in relative.components() {
            let path = folder.path().join(part.as_os_str());
            folder = match crate::windows_files::Directory::open(&path) {
                Ok(folder) => folder,
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
                {
                    return Ok(None)
                }
                Err(error) => {
                    return Err(error).context("Templates require real folders, not symbolic links")
                }
            };
        }
        Ok(Some(folder))
    }
    #[cfg(unix)]
    {
        let mut fd = open(
            root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        for part in relative.components() {
            fd = match openat(
                &fd,
                part.as_os_str(),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(fd) => fd,
                Err(rustix::io::Errno::NOENT) => return Ok(None),
                Err(error) => {
                    return Err(error).context("Templates require real folders, not symbolic links")
                }
            };
        }
        Ok(Some(fd))
    }
}

fn read_file(folder: &OwnedFd, name: &str) -> Result<Option<String>> {
    #[cfg(windows)]
    let file = match folder.open_file(std::ffi::OsStr::new(name)) {
        Ok(file) => file,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(None)
        }
        Err(error) => return Err(error).with_context(|| format!("Cannot read {name}")),
    };
    #[cfg(unix)]
    let file = {
        let fd = match openat(
            folder,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(error) => return Err(error).with_context(|| format!("Cannot read {name}")),
        };
        File::from(fd)
    };
    ensure!(
        file.metadata()?.is_file(),
        "{name} must be a regular UTF-8 file"
    );
    let mut text = String::new();
    file.take(1024 * 1024 + 1)
        .read_to_string(&mut text)
        .with_context(|| format!("Cannot read {name} as UTF-8"))?;
    ensure!(text.len() <= 1024 * 1024, "{name} exceeds 1 MiB");
    Ok(Some(text))
}

impl Catalog {
    pub fn load(root: &Path) -> Result<Self> {
        Self::load_with_folder(root, None)
    }

    pub fn load_with_folder(root: &Path, override_folder: Option<&str>) -> Result<Self> {
        let settings = match open_folder(root, Path::new(".obsidian"))? {
            Some(fd) => read_file(&fd, "templates.json")?
                .map(|text| {
                    serde_json::from_str::<Settings>(&text)
                        .context("Invalid .obsidian/templates.json")
                })
                .transpose()?,
            None => None,
        };
        let settings = settings.unwrap_or(Settings {
            folder: String::new(),
            date_format: default_date(),
            time_format: default_time(),
        });
        let (folder, chosen) = if let Some(folder) = override_folder {
            (PathBuf::from(folder), true)
        } else if settings.folder.is_empty() {
            (PathBuf::from(DEFAULT_FOLDER), false)
        } else {
            (PathBuf::from(&settings.folder), true)
        };
        let mut files = Vec::new();
        let opened = open_folder(root, &folder)?;
        let exists = opened.is_some();
        if let Some(fd) = opened {
            #[cfg(windows)]
            for item in std::fs::read_dir(fd.path())? {
                let item = item?;
                let Some(name) = item.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if name.starts_with("._")
                    || !Path::new(&name)
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
                {
                    continue;
                }
                match fd.open_file(std::ffi::OsStr::new(&name)) {
                    Ok(_) => files.push(name),
                    Err(error) if name.eq_ignore_ascii_case("Note.md") => {
                        return Err(error)
                            .context("Note.md must be a regular resident template file")
                    }
                    Err(_) => {}
                }
            }
            #[cfg(unix)]
            for item in Dir::read_from(&fd)? {
                let item = item?;
                let Some(name) = item.file_name().to_str().ok() else {
                    continue;
                };
                if name == "Note.md"
                    || (cfg!(any(target_os = "macos", windows))
                        && name.eq_ignore_ascii_case("Note.md"))
                {
                    ensure!(
                        item.file_type() == rustix::fs::FileType::RegularFile,
                        "Note.md must be a regular template file, not a symbolic link"
                    );
                }
                if !name.starts_with("._")
                    && Path::new(name)
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
                    && item.file_type() == rustix::fs::FileType::RegularFile
                {
                    files.push(name.to_owned());
                }
            }
        }
        files.sort();
        Ok(Self {
            folder,
            files,
            chosen,
            exists,
            date_format: settings.date_format,
            time_format: settings.time_format,
        })
    }

    /// The same catalog read again: the same folder, wherever it was chosen.
    /// Reloading from `.obsidian/templates.json` alone lost a folder chosen
    /// in Settings, so every New note failed as «Templates changed» (#1394).
    pub fn reload(&self, root: &Path) -> Result<Self> {
        let folder = self
            .folder
            .to_str()
            .context("Templates folder must be a UTF-8 path")?;
        Self::load_with_folder(root, Some(folder)).map(|current| Self {
            chosen: self.chosen,
            ..current
        })
    }

    /// A templates folder was chosen but is not in the vault: new notes are
    /// created without a template, and the person is told why (#1394). The
    /// default folder missing is the normal case and says nothing.
    pub fn missing(&self) -> bool {
        self.chosen && !self.exists
    }

    pub fn default_file(&self) -> Option<String> {
        self.files
            .iter()
            .find(|name| {
                *name == "Note.md"
                    || (cfg!(any(target_os = "macos", windows))
                        && name.eq_ignore_ascii_case("Note.md"))
            })
            .cloned()
    }

    pub fn contains_target(&self, target: &Path) -> bool {
        target.starts_with(&self.folder)
            || (cfg!(any(target_os = "macos", windows))
                && target
                    .to_string_lossy()
                    .to_lowercase()
                    .starts_with(&(self.folder.to_string_lossy().to_lowercase() + "/")))
    }

    pub fn source(
        &self,
        root: &Path,
        selected: Option<&str>,
        title: &str,
        now: OffsetDateTime,
    ) -> Result<String> {
        let source = if let Some(selected) = selected {
            ensure!(
                self.files.iter().any(|file| file == selected),
                "Choose a template from the templates folder"
            );
            let fd =
                open_folder(root, &self.folder)?.context("The templates folder disappeared")?;
            read_file(&fd, selected)?.context("The selected template disappeared")?
        } else {
            DEFAULT_SOURCE.into()
        };
        Ok(expand(
            &source,
            title,
            now,
            &self.date_format,
            &self.time_format,
        ))
    }
}

/// Replace only recognized variables. Never rescan the inserted title.
fn expand(source: &str, title: &str, now: OffsetDateTime, date: &str, time: &str) -> String {
    let mut out = String::new();
    let mut rest = source;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start + 2..].find("}}").map(|n| start + 2 + n) else {
            out.push_str(&rest[start..]);
            return out;
        };
        let var = rest[start + 2..end].trim();
        let replacement = match var {
            "title" => Some(title.to_owned()),
            "date" => moment(date, now),
            "time" => moment(time, now),
            _ => var
                .strip_prefix("date:")
                .or_else(|| var.strip_prefix("time:"))
                .and_then(|f| moment(f, now)),
        };
        out.push_str(replacement.as_deref().unwrap_or(&rest[start..end + 2]));
        rest = &rest[end + 2..];
    }
    out.push_str(rest);
    out
}

/// Supported Moment tokens are explicit, so unsupported formats remain literal.
fn moment(mut format: &str, now: OffsetDateTime) -> Option<String> {
    let mut out = String::new();
    while !format.is_empty() {
        if let Some(literal) = format.strip_prefix('[') {
            let end = literal.find(']')?;
            out.push_str(&literal[..end]);
            format = &literal[end + 1..];
            continue;
        }
        let tokens = [
            ("YYYY", format!("{:04}", now.year())),
            ("MM", format!("{:02}", u8::from(now.month()))),
            ("DD", format!("{:02}", now.day())),
            ("HH", format!("{:02}", now.hour())),
            ("mm", format!("{:02}", now.minute())),
            ("ss", format!("{:02}", now.second())),
        ];
        if let Some((token, value)) = tokens.into_iter().find(|(t, _)| format.starts_with(t)) {
            out.push_str(&value);
            format = &format[token.len()..];
        } else {
            let ch = format.chars().next()?;
            if ch.is_ascii_alphabetic() {
                return None;
            }
            out.push(ch);
            format = &format[ch.len_utf8()..];
        }
    }
    Some(out)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn obsidian_variables_are_one_pass_and_unknowns_lossless() {
        let now = time::macros::datetime!(2026-10-05 09:07:03 +02:00);
        assert_eq!(expand("\u{feff}# {{title}}\r\n{{date}} {{time}} {{date:DD/MM/YYYY}} {{time:HH-mm-ss}} {{date:[on] YYYY}} {{unknown}} {{date:dddd}}", "{{date}} 🧠", now, "YYYY-MM-DD", "HH:mm"),
            "\u{feff}# {{date}} 🧠\r\n2026-10-05 09:07 05/10/2026 09-07-03 on 2026 {{unknown}} {{date:dddd}}");
    }
    #[test]
    fn config_folder_defaults_and_safety() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let default = Catalog::load(root).unwrap();
        assert_eq!(default.folder, Path::new(DEFAULT_FOLDER));
        assert_eq!(default.default_file(), None);
        std::fs::create_dir_all(root.join("My Templates")).unwrap();
        std::fs::create_dir(root.join(".obsidian")).unwrap();
        std::fs::write(
            root.join(".obsidian/templates.json"),
            r#"{"folder":"My Templates","dateFormat":"DD/MM/YYYY","timeFormat":"HH-mm"}"#,
        )
        .unwrap();
        std::fs::write(root.join("My Templates/Note.md"), "{{date}} {{time}}").unwrap();
        std::fs::write(root.join("My Templates/Meeting.md"), "# {{title}}").unwrap();
        let catalog = Catalog::load(root).unwrap();
        assert_eq!(catalog.default_file().as_deref(), Some("Note.md"));
        assert_eq!(catalog.files, ["Meeting.md", "Note.md"]);
        std::fs::create_dir(root.join("Chosen Templates")).unwrap();
        std::fs::write(root.join("Chosen Templates/Note.md"), "Chosen {{date}}").unwrap();
        let chosen = Catalog::load_with_folder(root, Some("Chosen Templates")).unwrap();
        assert_eq!(chosen.folder, Path::new("Chosen Templates"));
        assert_eq!(chosen.files, ["Note.md"]);
        assert!(Catalog::load_with_folder(root, Some("../outside")).is_err());
        let now = time::macros::datetime!(2026-10-05 09:07 UTC);
        assert_eq!(
            chosen.source(root, Some("Note.md"), "Title", now).unwrap(),
            "Chosen 05/10/2026"
        );
        assert_eq!(
            catalog.source(root, Some("Note.md"), "Title", now).unwrap(),
            "05/10/2026 09-07"
        );
        assert!(catalog
            .source(root, Some("../escape.md"), "Title", now)
            .is_err());
        std::fs::write(
            root.join(".obsidian/templates.json"),
            r#"{"folder":"../outside"}"#,
        )
        .unwrap();
        assert!(Catalog::load(root).is_err());
        std::fs::remove_file(root.join(".obsidian/templates.json")).unwrap();
        std::os::unix::fs::symlink(dir.path(), root.join("_Assets")).unwrap();
        assert!(Catalog::load(root).is_err());
    }
}
