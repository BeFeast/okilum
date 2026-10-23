//! A pasted image becomes a file in the vault's attachment folder and an
//! Obsidian embed at the caret (#1602). Platform-neutral.
//!
//! Folder: `.obsidian/app.json` `attachmentFolderPath` — `/` is the vault
//! root, `./` the note's folder, `./sub` a folder under the note's folder, any
//! other value a vault-relative folder. Absent (or unusable): `attachments/`
//! at the vault root. Name: `Pasted image YYYYMMDDHHMMSS.png`, `-1`, `-2`, …
//! when taken. The file is written to a temporary file in that folder and
//! linked into place without replacing anything, before the embed is inserted.

use super::offer::{ImageKind, PastedImage, MAX_IMAGE_BYTES};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

/// Where pasted images go when the vault does not say.
pub const DEFAULT_FOLDER: &str = "attachments";
/// Names tried in one folder before giving up.
const MAX_SUFFIX: usize = 999;

#[derive(Debug)]
pub enum SaveError {
    TooLarge,
    /// The TIFF could not be converted to PNG.
    Convert,
    Folder(io::Error),
    Write(io::Error),
    NoFreeName,
}

impl SaveError {
    /// What the toast says; nothing was inserted.
    pub fn user_text(&self) -> String {
        match self {
            Self::TooLarge => "The image is too large to paste (64 MB at most).".into(),
            Self::Convert => "The image couldn’t be converted to PNG, so it wasn’t pasted.".into(),
            Self::Folder(error) => {
                format!(
                    "Couldn’t create the attachment folder, so the image wasn’t pasted: {error}"
                )
            }
            Self::Write(error) => {
                format!("Couldn’t save the pasted image, so nothing was inserted: {error}")
            }
            Self::NoFreeName => {
                "Couldn’t find a free name for the pasted image, so nothing was inserted.".into()
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedImage {
    pub path: PathBuf,
    pub file_name: String,
    /// `![[Pasted image ….png]]`, inserted at the caret.
    pub embed: String,
}

/// The folder a pasted image is saved in, for `note_rel` (vault-relative,
/// `/`-separated). Never outside the vault.
pub fn attachment_folder(vault_root: &Path, note_rel: &str) -> PathBuf {
    let setting = std::fs::read(vault_root.join(".obsidian").join("app.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|app| app.get("attachmentFolderPath")?.as_str().map(str::to_owned));
    let note_dir: Vec<&str> = {
        let mut parts: Vec<&str> = note_rel.split('/').filter(|p| !p.is_empty()).collect();
        parts.pop();
        parts
    };
    let relative = setting
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|setting| folder_from_setting(setting, &note_dir))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_FOLDER));
    vault_root.join(relative)
}

/// The vault-relative folder for one `attachmentFolderPath` value, or `None`
/// when it would leave the vault.
fn folder_from_setting(setting: &str, note_dir: &[&str]) -> Option<PathBuf> {
    let setting = setting.replace('\\', "/");
    let (base, rest): (Vec<&str>, &str) = if setting == "." || setting == "./" {
        (note_dir.to_vec(), "")
    } else if let Some(rest) = setting.strip_prefix("./") {
        (note_dir.to_vec(), rest)
    } else {
        (Vec::new(), setting.trim_start_matches('/'))
    };
    let mut parts: Vec<String> = base.into_iter().map(str::to_owned).collect();
    for component in Path::new(rest).components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_owned()),
            Component::CurDir => {}
            // `..` and absolute paths could leave the vault.
            _ => return None,
        }
    }
    Some(parts.iter().collect())
}

/// `Pasted image 20261023153000.png`, `Pasted image 20261023153000-1.png`, …
pub fn pasted_name(stamp: &str, suffix: usize, extension: &str) -> String {
    if suffix == 0 {
        format!("Pasted image {stamp}.{extension}")
    } else {
        format!("Pasted image {stamp}-{suffix}.{extension}")
    }
}

/// Obsidian's embed for a file name.
pub fn embed(file_name: &str) -> String {
    format!("![[{file_name}]]")
}

/// The local time stamp Obsidian puts in pasted image names.
pub fn stamp_now() -> String {
    chrono::Local::now().format("%Y%m%d%H%M%S").to_string()
}

/// PNG for a TIFF (macOS screenshots and copied images); other kinds as given.
pub fn normalize(image: &PastedImage) -> Result<(Vec<u8>, ImageKind), SaveError> {
    if image.bytes.len() > MAX_IMAGE_BYTES {
        return Err(SaveError::TooLarge);
    }
    if image.kind != ImageKind::Tiff {
        return Ok((image.bytes.clone(), image.kind));
    }
    let decoded = image::load_from_memory_with_format(&image.bytes, image::ImageFormat::Tiff)
        .map_err(|_| SaveError::Convert)?;
    let mut png = Vec::new();
    decoded
        .write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|_| SaveError::Convert)?;
    Ok((png, ImageKind::Png))
}

/// Save `image` for the note `note_rel` and return the embed to insert. The
/// file is complete on disk before this returns; an existing file is never
/// replaced.
pub fn save_pasted_image(
    vault_root: &Path,
    note_rel: &str,
    image: &PastedImage,
    stamp: &str,
) -> Result<SavedImage, SaveError> {
    let (bytes, kind) = normalize(image)?;
    let folder = attachment_folder(vault_root, note_rel);
    std::fs::create_dir_all(&folder).map_err(SaveError::Folder)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".okilum-paste-")
        .suffix(".tmp")
        .tempfile_in(&folder)
        .map_err(SaveError::Write)?;
    temporary.write_all(&bytes).map_err(SaveError::Write)?;
    temporary.as_file().sync_all().map_err(SaveError::Write)?;
    for suffix in 0..=MAX_SUFFIX {
        let file_name = pasted_name(stamp, suffix, kind.extension());
        let path = folder.join(&file_name);
        if path.exists() {
            continue;
        }
        match temporary.persist_noclobber(&path) {
            Ok(_) => {
                sync_folder(&folder);
                return Ok(SavedImage {
                    path,
                    embed: embed(&file_name),
                    file_name,
                });
            }
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                temporary = error.file;
            }
            Err(error) => return Err(SaveError::Write(error.error)),
        }
    }
    Err(SaveError::NoFreeName)
}

fn sync_folder(folder: &Path) {
    #[cfg(unix)]
    if let Ok(dir) = std::fs::File::open(folder) {
        let _ = dir.sync_all();
    }
    #[cfg(not(unix))]
    let _ = folder;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> PastedImage {
        PastedImage {
            bytes: b"\x89PNG\r\n\x1a\nfixture".to_vec(),
            kind: ImageKind::Png,
            source: None,
        }
    }

    fn app_json(root: &Path, setting: Option<&str>) {
        std::fs::create_dir_all(root.join(".obsidian")).unwrap();
        let json = match setting {
            Some(value) => {
                serde_json::json!({ "attachmentFolderPath": value, "livePreview": false })
            }
            None => serde_json::json!({ "livePreview": false }),
        };
        std::fs::write(root.join(".obsidian/app.json"), json.to_string()).unwrap();
    }

    #[test]
    fn image_becomes_a_file_and_an_obsidian_embed() {
        let vault = tempfile::tempdir().unwrap();
        let saved =
            save_pasted_image(vault.path(), "Daily/today.md", &png(), "20261023153000").unwrap();
        assert_eq!(saved.file_name, "Pasted image 20261023153000.png");
        assert_eq!(saved.embed, "![[Pasted image 20261023153000.png]]");
        assert_eq!(
            saved.path,
            vault
                .path()
                .join("attachments/Pasted image 20261023153000.png")
        );
        assert_eq!(std::fs::read(&saved.path).unwrap(), png().bytes);
        // No temporary file is left behind.
        let names: Vec<_> = std::fs::read_dir(vault.path().join("attachments"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["Pasted image 20261023153000.png"]);
    }

    #[test]
    fn names_are_unique_and_nothing_is_overwritten() {
        let vault = tempfile::tempdir().unwrap();
        let folder = vault.path().join("attachments");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("Pasted image 20261023153000.png"), b"keep").unwrap();
        let first = save_pasted_image(vault.path(), "n.md", &png(), "20261023153000").unwrap();
        let second = save_pasted_image(vault.path(), "n.md", &png(), "20261023153000").unwrap();
        assert_eq!(first.file_name, "Pasted image 20261023153000-1.png");
        assert_eq!(second.file_name, "Pasted image 20261023153000-2.png");
        assert_eq!(
            std::fs::read(folder.join("Pasted image 20261023153000.png")).unwrap(),
            b"keep"
        );
    }

    #[test]
    fn attachment_folder_follows_app_json() {
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path();
        let note = "Projects/Alpha/plan.md";
        // Absent app.json, absent key, empty value: vault-root attachments/.
        assert_eq!(attachment_folder(root, note), root.join("attachments"));
        app_json(root, None);
        assert_eq!(attachment_folder(root, note), root.join("attachments"));
        for (setting, expected) in [
            ("", root.join("attachments")),
            ("/", root.to_path_buf()),
            ("./", root.join("Projects/Alpha")),
            (".", root.join("Projects/Alpha")),
            ("./assets", root.join("Projects/Alpha/assets")),
            ("./assets/img", root.join("Projects/Alpha/assets/img")),
            ("_Assets/Attachments", root.join("_Assets/Attachments")),
            ("/_Assets/Attachments/", root.join("_Assets/Attachments")),
            // Never outside the vault.
            ("../outside", root.join("attachments")),
            ("./../../../x", root.join("attachments")),
        ] {
            app_json(root, Some(setting));
            assert_eq!(attachment_folder(root, note), expected, "{setting:?}");
        }
        // A note at the vault root with "./" saves next to it, at the root.
        app_json(root, Some("./"));
        assert_eq!(attachment_folder(root, "top.md"), root.to_path_buf());
        // Unreadable JSON falls back to the default.
        std::fs::write(root.join(".obsidian/app.json"), b"{not json").unwrap();
        assert_eq!(attachment_folder(root, note), root.join("attachments"));
    }

    #[test]
    fn saved_in_the_configured_folder() {
        let vault = tempfile::tempdir().unwrap();
        app_json(vault.path(), Some("./img"));
        let saved =
            save_pasted_image(vault.path(), "Notes/a.md", &png(), "20260101000000").unwrap();
        assert_eq!(
            saved.path,
            vault
                .path()
                .join("Notes/img/Pasted image 20260101000000.png")
        );
    }

    #[test]
    fn tiff_is_saved_as_png() {
        let mut tiff = Vec::new();
        image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut io::Cursor::new(&mut tiff), image::ImageFormat::Tiff)
            .unwrap();
        let vault = tempfile::tempdir().unwrap();
        let image = PastedImage {
            bytes: tiff,
            kind: ImageKind::Tiff,
            source: None,
        };
        let saved = save_pasted_image(vault.path(), "n.md", &image, "20260101000000").unwrap();
        assert_eq!(saved.embed, "![[Pasted image 20260101000000.png]]");
        let bytes = std::fs::read(&saved.path).unwrap();
        assert!(bytes.starts_with(b"\x89PNG"));
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!(decoded.get_pixel(1, 1), &image::Rgba([10, 20, 30, 255]));
    }

    #[test]
    fn failure_writes_nothing() {
        let vault = tempfile::tempdir().unwrap();
        // A file where the folder should be: no folder, no image, no embed.
        std::fs::write(vault.path().join("attachments"), b"file").unwrap();
        assert!(matches!(
            save_pasted_image(vault.path(), "n.md", &png(), "20260101000000"),
            Err(SaveError::Folder(_))
        ));
        let broken = PastedImage {
            bytes: b"not a tiff".to_vec(),
            kind: ImageKind::Tiff,
            source: None,
        };
        let other = tempfile::tempdir().unwrap();
        assert!(matches!(
            save_pasted_image(other.path(), "n.md", &broken, "20260101000000"),
            Err(SaveError::Convert)
        ));
        assert!(!other.path().join("attachments").exists());
    }
}
