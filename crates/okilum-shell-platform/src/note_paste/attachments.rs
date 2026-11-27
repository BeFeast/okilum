//! A pasted image becomes a file in the vault's attachment folder and an
//! Obsidian embed at the caret (#1602). Platform-neutral.
//!
//! Folder: `.obsidian/app.json` `attachmentFolderPath` — `/` is the vault
//! root, `./` the note's folder, `./sub` a folder under the note's folder, any
//! other value a vault-relative folder. Absent (or unusable): the typed vault
//! folder profile supplies the image and file defaults. Name: `Pasted image YYYYMMDDHHMMSS.png`, `-1`, `-2`, …
//! when taken. The file is written to a temporary file in that folder and
//! linked into place without replacing anything, before the embed is inserted.

use super::offer::{ImageKind, PastedImage, MAX_IMAGE_BYTES};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentKind {
    Image,
    File,
}

impl AttachmentKind {
    pub fn of_path(path: &Path) -> Self {
        use std::io::Read as _;
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(is_image_extension)
        {
            return Self::Image;
        }
        // Do not open directories, devices or FIFOs just to classify a paste.
        if !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
            return Self::File;
        }
        let mut head = [0; 16];
        if std::fs::File::open(path)
            .and_then(|mut file| file.read(&mut head))
            .is_ok_and(|len| sniff_image(&head[..len]).is_some())
        {
            Self::Image
        } else {
            Self::File
        }
    }
}
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
    /// A copied file couldn't be copied into the vault.
    CopyFile(String, io::Error),
    Webp(String),
}

impl SaveError {
    /// What the toast says; nothing was inserted.
    pub fn user_text(&self) -> String {
        match self {
            Self::Webp(error) => format!("The image wasn’t inserted: {error}"),
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
            Self::CopyFile(name, error) => {
                format!("Couldn’t copy “{name}” into the vault, so nothing was inserted: {error}")
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
    pub paths: Vec<PathBuf>,
}

/// The folder a pasted image is saved in, for `note_rel` (vault-relative,
/// `/`-separated). Never outside the vault.
pub fn attachment_folder(
    vault_root: &Path,
    note_rel: &str,
    kind: AttachmentKind,
) -> io::Result<PathBuf> {
    use okilum_core::vault_descriptor::LayoutState;

    let mut note_dir = note_rel.split('/').collect::<Vec<_>>();
    if !relative_parts(note_rel) {
        return Err(io::Error::other("Choose a note inside the vault"));
    }
    note_dir.pop();
    let setting = okilum_core::vault_conventions::obsidian_attachment_setting(vault_root);
    let configured = setting
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|setting| folder_from_setting(setting, &note_dir));
    let relative = match LayoutState::load(vault_root) {
        LayoutState::Managed(layout) => {
            let configured = configured
                .map(|path| {
                    let folder = vault_root.join(path);
                    validate_folder(vault_root, &folder).map(|()| folder)
                })
                .transpose();
            if let Ok(Some(folder)) = configured {
                return Ok(folder);
            }
            if let Some(path) = layout.attachments.filter(|path| relative_parts(path)) {
                PathBuf::from(path)
            } else {
                return Err(configured.err().unwrap_or_else(|| {
                    io::Error::other("This managed vault has no allowed attachment folder. Check the vault layout or Obsidian attachment setting.")
                }));
            }
        }
        LayoutState::Unsupported => return Err(unsupported_layout()),
        LayoutState::Legacy => configured.unwrap_or_else(|| {
            let folders = okilum_core::vault_conventions::defaults();
            let typed = Path::new(folders.legacy_attachment_folder(kind == AttachmentKind::Image));
            let legacy_root_exists = typed
                .parent()
                .is_some_and(|parent| vault_root.join(parent).is_dir());
            if !legacy_root_exists && vault_root.join("attachments").is_dir() {
                PathBuf::from("attachments")
            } else {
                typed.to_path_buf()
            }
        }),
    };
    let folder = vault_root.join(relative);
    validate_folder(vault_root, &folder)?;
    Ok(folder)
}

fn unsupported_layout() -> io::Error {
    io::Error::other("Attachment writes are blocked because this vault layout is unsupported")
}

fn relative_parts(path: &str) -> bool {
    !path.contains(['\\', ':'])
        && !path.chars().any(char::is_control)
        && path.split('/').all(|part| {
            !part.is_empty()
                && !matches!(part, "." | "..")
                && matches!(
                    Path::new(part).components().next(),
                    Some(Component::Normal(_))
                )
        })
}

/// Obsidian uses / for the vault root, ./ for the note folder and ./sub for
/// a sibling folder. All remaining path parts must be real relative names.
fn folder_from_setting(setting: &str, note_dir: &[&str]) -> Option<PathBuf> {
    let setting = setting.replace('\\', "/");
    let mut folder = PathBuf::new();
    let rest = if setting == "." {
        folder.extend(note_dir);
        ""
    } else if let Some(rest) = setting.strip_prefix("./") {
        folder.extend(note_dir);
        rest
    } else {
        setting.trim_start_matches('/')
    };
    let rest = rest.trim_end_matches('/');
    if !rest.is_empty() {
        if !relative_parts(rest) {
            return None;
        }
        folder.push(rest);
    }
    Some(folder)
}

fn allowed(layout: &okilum_core::vault_descriptor::Layout, path: &Path) -> bool {
    let path = super::files::slash_path(path).to_lowercase();
    [
        layout.roots.log.as_str(),
        layout.roots.views.as_str(),
        layout.roots.knowledge.as_str(),
        "@Schema",
        okilum_core::vault_descriptor::DIRECTORY,
    ]
    .into_iter()
    .all(|protected| {
        let protected = protected.to_lowercase();
        path != protected
            && !path
                .strip_prefix(&protected)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// Before creating anything and again before publishing a temporary file,
/// verify both the live layout policy and the existing path on disk.
pub fn validate_folder(root: &Path, folder: &Path) -> io::Result<()> {
    use okilum_core::vault_descriptor::LayoutState;

    let relative = folder
        .strip_prefix(root)
        .map_err(|_| io::Error::other("Choose an attachment folder inside the vault"))?;
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(io::Error::other(
            "Choose an attachment folder inside the vault",
        ));
    }
    let canonical_root = root.canonicalize()?;
    for path in folder.ancestors().take_while(|path| path.starts_with(root)) {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(io::Error::other(
                    "Attachment folders must be real directories inside the vault",
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let existing = folder
        .ancestors()
        .find(|path| path.is_dir())
        .ok_or_else(|| io::Error::other("Attachment folder has no existing parent"))?;
    let canonical = existing.canonicalize()?;
    let canonical_relative = canonical
        .strip_prefix(&canonical_root)
        .map_err(|_| io::Error::other("Attachment folder leaves the vault"))?;
    let planned = canonical_relative.join(folder.strip_prefix(existing).map_err(io::Error::other)?);
    match LayoutState::load(root) {
        LayoutState::Managed(layout)
            if !allowed(&layout, relative) || !allowed(&layout, &planned) =>
        {
            return Err(io::Error::other(
                "Attachments cannot be saved in this managed folder",
            ));
        }
        LayoutState::Unsupported => return Err(unsupported_layout()),
        _ => {}
    }
    Ok(())
}

pub fn admit_folder(
    root: &Path,
    folder: &Path,
) -> anyhow::Result<okilum_write_authority::WriterLease> {
    validate_folder(root, folder)?;
    // Register the open vault scope before borrowing its lock for a child.
    okilum_write_authority::Authority::for_root(root)?;
    okilum_write_authority::admit_directory(folder)
}

pub fn prepare_folder(root: &Path, folder: &Path) -> io::Result<()> {
    validate_folder(root, folder)?;
    let relative = folder.strip_prefix(root).map_err(io::Error::other)?;
    okilum_core::note_files::create_folders(root, relative, false).map_err(io::Error::other)?;
    validate_folder(root, folder)
}

pub fn validate_name(name: &str) -> io::Result<()> {
    if !relative_parts(name) || name.contains('/') {
        return Err(io::Error::other("Choose an attachment name without a path"));
    }
    Ok(())
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
    save_pasted_image_with(
        vault_root,
        note_rel,
        image,
        stamp,
        super::image_import::Options::disabled(),
    )
}

pub fn save_pasted_image_with(
    vault_root: &Path,
    note_rel: &str,
    image: &PastedImage,
    stamp: &str,
    options: super::image_import::Options,
) -> Result<SavedImage, SaveError> {
    let _admission = okilum_write_authority::operation(vault_root)
        .map_err(|e| SaveError::Write(io::Error::other(e)))?;
    let folder = attachment_folder(vault_root, note_rel, AttachmentKind::Image)
        .map_err(SaveError::Folder)?;
    let _destination = okilum_write_authority::operation_for_directory(&folder)
        .map_err(|e| SaveError::Write(io::Error::other(e)))?;
    if options.enabled {
        let wanted = pasted_name(stamp, 0, image.kind.extension());
        let saved = super::image_import::save(vault_root, &folder, &wanted, &image.bytes, options)
            .map_err(SaveError::Webp)?;
        let path = folder.join(&saved.name);
        let target = path
            .strip_prefix(vault_root)
            .map(super::files::slash_path)
            .unwrap_or_else(|_| saved.name.clone());
        return Ok(SavedImage {
            path,
            embed: embed(&target),
            file_name: saved.name,
            paths: saved.paths,
        });
    }
    let (bytes, kind) = normalize(image)?;
    prepare_folder(vault_root, &folder).map_err(SaveError::Folder)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".okilum-paste-")
        .suffix(".tmp")
        .tempfile_in(&folder)
        .map_err(SaveError::Write)?;
    temporary.write_all(&bytes).map_err(SaveError::Write)?;
    temporary.as_file().sync_all().map_err(SaveError::Write)?;
    for suffix in 0..=MAX_SUFFIX {
        let file_name = pasted_name(stamp, suffix, kind.extension());
        validate_name(&file_name).map_err(SaveError::Write)?;
        let path = folder.join(&file_name);
        if path.exists() {
            continue;
        }
        validate_folder(vault_root, &folder).map_err(SaveError::Folder)?;
        let _publication = okilum_write_authority::operation_for_directory(&folder)
            .map_err(|e| SaveError::Write(io::Error::other(e)))?;
        match temporary.persist_noclobber(&path) {
            Ok(_) => {
                sync_folder(&folder);
                return Ok(SavedImage {
                    paths: vec![path.clone()],
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

/// Files larger than this are linked where they are instead of copied.
pub const MAX_COPY_BYTES: u64 = 100 * 1024 * 1024;

/// Extensions embedded with `![[…]]`; anything else is linked with `[[…]]`.
pub const IMAGE_EXTENSIONS: [&str; 13] = [
    "png", "jpg", "jpeg", "gif", "webp", "heic", "heif", "tif", "tiff", "bmp", "svg", "avif", "ico",
];

pub fn is_image_extension(extension: &str) -> bool {
    IMAGE_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
}

/// The image kind of a file's first bytes, for a cached file without an
/// extension.
pub fn sniff_image(head: &[u8]) -> Option<ImageKind> {
    Some(if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        ImageKind::Png
    } else if head.starts_with(&[0xff, 0xd8, 0xff]) {
        ImageKind::Jpeg
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        ImageKind::Gif
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        ImageKind::Webp
    } else if head.starts_with(b"II*\0") || head.starts_with(b"MM\0*") {
        ImageKind::Tiff
    } else if head.starts_with(b"BM") {
        ImageKind::Bmp
    } else if head.len() >= 12
        && &head[4..8] == b"ftyp"
        && matches!(
            &head[8..12],
            b"heic" | b"heix" | b"mif1" | b"msf1" | b"heif"
        )
    {
        ImageKind::Heic
    } else {
        return None;
    })
}

/// Whether a copied file lives in a clipboard manager's cache or a temporary
/// folder (its name means nothing; it gets a `Pasted image …` name).
pub fn is_clipboard_cache(path: &Path) -> bool {
    let text = path.to_string_lossy().replace('\\', "/");
    text.contains("/com.raycast.macos/")
        || text.contains("/TemporaryItems/")
        || text.starts_with("/var/folders/")
        || text.starts_with("/private/var/folders/")
        || text.starts_with("/tmp/")
        || text.starts_with("/private/tmp/")
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SavedFiles {
    /// Inserted at the caret, one link per line.
    pub text: String,
    pub copied: usize,
    pub copied_bytes: u64,
    pub in_vault: usize,
    /// Too large to copy: linked at their original place.
    pub linked_outside: Vec<String>,
}

/// Copy pasted files into the note's attachment folder (never replacing a
/// file) and return their links: `![[name]]` for images, `[[name]]` for other
/// files, a vault-relative link for a file already in the vault, a `file://`
/// link for a file over [`MAX_COPY_BYTES`]. Every linked file exists when
/// this returns.
pub fn save_pasted_files(
    vault_root: &Path,
    note_rel: &str,
    files: &[PathBuf],
    stamp: &str,
) -> Result<SavedFiles, SaveError> {
    save_pasted_files_with(vault_root, note_rel, files, stamp, &is_clipboard_cache)
}

pub fn save_pasted_files_with(
    vault_root: &Path,
    note_rel: &str,
    files: &[PathBuf],
    stamp: &str,
    is_cache: &dyn Fn(&Path) -> bool,
) -> Result<SavedFiles, SaveError> {
    let mut saved = SavedFiles::default();
    let mut links = Vec::new();
    let root = std::fs::canonicalize(vault_root).unwrap_or_else(|_| vault_root.to_path_buf());
    for source in files {
        let display = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| source.to_string_lossy().into_owned());
        let canonical = std::fs::canonicalize(source).unwrap_or_else(|_| source.clone());
        let extension = source
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_owned);
        // Already in the vault: link it where it is.
        if let Ok(relative) = canonical.strip_prefix(&root) {
            let relative = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if !relative.is_empty() {
                let image = extension.as_deref().is_some_and(is_image_extension);
                links.push(if image {
                    embed(&relative)
                } else {
                    format!("[[{relative}]]")
                });
                saved.in_vault += 1;
                continue;
            }
        }
        let metadata =
            std::fs::metadata(source).map_err(|e| SaveError::CopyFile(display.clone(), e))?;
        // Only regular files are copied: a FIFO or device would block or never end.
        if !metadata.is_file() || metadata.len() > MAX_COPY_BYTES {
            let url = url::Url::from_file_path(&canonical)
                .map(|u| u.to_string())
                .unwrap_or_else(|_| canonical.to_string_lossy().into_owned());
            links.push(format!("[{}]({url})", display.replace(['[', ']'], "")));
            saved.linked_outside.push(display);
            continue;
        }
        // A cached file without an extension: name it by its content.
        let extension = match extension {
            Some(extension) => Some(extension),
            None => {
                let mut head = [0u8; 16];
                let read = std::fs::File::open(source)
                    .and_then(|mut f| io::Read::read(&mut f, &mut head))
                    .unwrap_or(0);
                sniff_image(&head[..read]).map(|kind| kind.extension().to_owned())
            }
        };
        let image = extension.as_deref().is_some_and(is_image_extension);
        let cached = is_cache(source);
        let stem = if image && cached {
            format!("Pasted image {stamp}")
        } else {
            source
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| display.clone())
        };
        let folder = attachment_folder(vault_root, note_rel, AttachmentKind::of_path(source))
            .map_err(SaveError::Folder)?;
        let (file_name, bytes) =
            copy_noclobber(vault_root, source, &folder, &stem, extension.as_deref()).map_err(
                |error| match error {
                    CopyError::Folder(e) => SaveError::Folder(e),
                    CopyError::Io(e) => SaveError::CopyFile(display.clone(), e),
                    CopyError::NoFreeName => SaveError::NoFreeName,
                },
            )?;
        links.push(if image {
            embed(&file_name)
        } else {
            format!("[[{file_name}]]")
        });
        saved.copied += 1;
        saved.copied_bytes += bytes;
    }
    saved.text = links.join("\n");
    Ok(saved)
}

enum CopyError {
    Folder(io::Error),
    Io(io::Error),
    NoFreeName,
}

/// `stem.ext`, `stem-1.ext`, … — the first free one, written completely
/// before it gets its name.
fn copy_noclobber(
    vault_root: &Path,
    source: &Path,
    folder: &Path,
    stem: &str,
    extension: Option<&str>,
) -> Result<(String, u64), CopyError> {
    let _admission = admit_folder(vault_root, folder)
        .and_then(|lease| lease.enter())
        .map_err(|e| CopyError::Io(io::Error::other(e)))?;
    prepare_folder(vault_root, folder).map_err(CopyError::Folder)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".okilum-paste-")
        .suffix(".tmp")
        .tempfile_in(folder)
        .map_err(CopyError::Io)?;
    let mut input = std::fs::File::open(source).map_err(CopyError::Io)?;
    let bytes = io::copy(&mut input, temporary.as_file_mut()).map_err(CopyError::Io)?;
    temporary.as_file().sync_all().map_err(CopyError::Io)?;
    for suffix in 0..=MAX_SUFFIX {
        let base = if suffix == 0 {
            stem.to_owned()
        } else {
            format!("{stem}-{suffix}")
        };
        let file_name = match extension {
            Some(extension) if !extension.is_empty() => format!("{base}.{extension}"),
            _ => base,
        };
        validate_name(&file_name).map_err(CopyError::Io)?;
        let path = folder.join(&file_name);
        if path.exists() {
            continue;
        }
        validate_folder(vault_root, folder).map_err(CopyError::Folder)?;
        let _publication = okilum_write_authority::operation_for_directory(folder)
            .map_err(|e| CopyError::Io(io::Error::other(e)))?;
        match temporary.persist_noclobber(&path) {
            Ok(_) => {
                sync_folder(folder);
                return Ok((file_name, bytes));
            }
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                temporary = error.file;
            }
            Err(error) => return Err(CopyError::Io(error.error)),
        }
    }
    Err(CopyError::NoFreeName)
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
        std::fs::create_dir(vault.path().join("attachments")).unwrap();
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
    fn migration_review_managed_paste_refuses_protected_and_unsupported_destinations() {
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path();
        let fixture =
            include_bytes!("../../../../tests/fixtures/lifecycle-vault/.okilum/vault.json");
        let path = root.join(".okilum/vault.json");
        std::fs::create_dir(root.join(".okilum")).unwrap();
        std::fs::write(&path, fixture).unwrap();
        let note = "log/health/sleep/decisions/note.md";

        for setting in [
            None,
            Some("./"),
            Some("log/new"),
            Some("@views"),
            Some("@Schema"),
            Some(".okilum"),
            Some("knowledge/images"),
        ] {
            app_json(root, setting);
            assert!(
                save_pasted_image(root, note, &png(), "protected").is_err(),
                "{setting:?}"
            );
        }
        assert!(!root.join("log").exists());
        assert!(!root.join("@views").exists());
        assert!(!root.join("@Schema").exists());
        assert!(!root.join("knowledge").exists());
        assert_eq!(std::fs::read_dir(root.join(".okilum")).unwrap().count(), 1);

        let mut descriptor: serde_json::Value = serde_json::from_slice(fixture).unwrap();
        descriptor["layout"]["attachments"] = serde_json::json!("sources/files/pasted");
        std::fs::write(&path, serde_json::to_vec(&descriptor).unwrap()).unwrap();
        let saved = save_pasted_image(root, note, &png(), "fallback").unwrap();
        assert_eq!(
            saved.path.parent().unwrap(),
            root.join("sources/files/pasted")
        );

        app_json(root, Some("state/media"));
        let saved = save_pasted_image(root, note, &png(), "allowed").unwrap();
        assert_eq!(saved.path.parent().unwrap(), root.join("state/media"));

        descriptor["layout"]["schema_version"] = serde_json::json!(2);
        std::fs::write(path, serde_json::to_vec(&descriptor).unwrap()).unwrap();
        assert!(save_pasted_image(root, note, &png(), "unsupported").is_err());
        assert!(!root
            .join("state/media/Pasted image unsupported.png")
            .exists());
    }

    #[test]
    fn migration_review_paste_refuses_note_path_traversal() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        app_json(&root, Some("./"));
        assert!(save_pasted_image(&root, "../outside/note.md", &png(), "outside").is_err());
        assert!(!temp.path().join("outside").exists());
        assert!(save_pasted_image(&root, "/absolute/note.md", &png(), "absolute").is_err());
        assert!(save_pasted_image(&root, "notes/note.md", &png(), "inside").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn migration_review_paste_refuses_a_symlinked_folder_or_ancestor() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
        for setting in ["linked", "linked/subfolder"] {
            app_json(&root, Some(setting));
            assert!(
                save_pasted_image(&root, "note.md", &png(), "outside").is_err(),
                "{setting}"
            );
        }
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    }

    #[test]
    fn attachment_folder_follows_app_json() {
        let vault = tempfile::tempdir().unwrap();
        std::fs::create_dir(vault.path().join("attachments")).unwrap();
        let root = vault.path();
        let note = "Projects/Alpha/plan.md";
        // Absent app.json, absent key, empty value: vault-root attachments/.
        assert_eq!(
            attachment_folder(root, note, AttachmentKind::Image).unwrap(),
            root.join("attachments")
        );
        app_json(root, None);
        assert_eq!(
            attachment_folder(root, note, AttachmentKind::Image).unwrap(),
            root.join("attachments")
        );
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
            assert_eq!(
                attachment_folder(root, note, AttachmentKind::Image).unwrap(),
                expected,
                "{setting:?}"
            );
        }
        // A note at the vault root with "./" saves next to it, at the root.
        app_json(root, Some("./"));
        assert_eq!(
            attachment_folder(root, "top.md", AttachmentKind::Image).unwrap(),
            root.to_path_buf()
        );
        // Unreadable JSON falls back to the default.
        std::fs::write(root.join(".obsidian/app.json"), b"{not json").unwrap();
        assert_eq!(
            attachment_folder(root, note, AttachmentKind::Image).unwrap(),
            root.join("attachments")
        );
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

    /// #1602 Raycast: a cached clipboard image becomes `Pasted image …`, a
    /// named file keeps its name, nothing is overwritten.
    #[test]
    fn pasted_files_are_copied_and_linked() {
        let vault = tempfile::tempdir().unwrap();
        std::fs::create_dir(vault.path().join("attachments")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let cache = outside.path().join("com.raycast.macos/clipboard");
        std::fs::create_dir_all(&cache).unwrap();
        let cached_png = cache.join("file-ABC.png");
        std::fs::write(&cached_png, b"\x89PNG\r\n\x1a\nraycast").unwrap();
        let cached_bare = cache.join("file-DEF");
        std::fs::write(&cached_bare, b"\xff\xd8\xff\xe0jpeg").unwrap();
        let named = outside.path().join("Report Q3.pdf");
        std::fs::write(&named, b"%PDF-1.7").unwrap();
        let photo = outside.path().join("photo.JPG");
        std::fs::write(&photo, b"\xff\xd8\xffjpeg").unwrap();
        let is_cache = |p: &Path| p.to_string_lossy().contains("com.raycast.macos");
        let files = vec![cached_png.clone(), cached_bare, named.clone(), photo];
        let saved = save_pasted_files_with(
            vault.path(),
            "Daily/n.md",
            &files,
            "20261023193000",
            &is_cache,
        )
        .unwrap();
        assert_eq!(
            saved.text,
            "![[Pasted image 20261023193000.png]]\n![[Pasted image 20261023193000.jpg]]\n[[Report Q3.pdf]]\n![[photo.JPG]]"
        );
        assert_eq!(saved.copied, 4);
        let folder = vault.path().join("attachments");
        assert_eq!(
            std::fs::read(folder.join("Pasted image 20261023193000.png")).unwrap(),
            b"\x89PNG\r\n\x1a\nraycast"
        );
        assert_eq!(
            std::fs::read(folder.join("Report Q3.pdf")).unwrap(),
            b"%PDF-1.7"
        );
        // The same paste again: new names, the first copies untouched.
        let again = save_pasted_files_with(
            vault.path(),
            "n.md",
            &[cached_png, named],
            "20261023193000",
            &is_cache,
        )
        .unwrap();
        assert_eq!(
            again.text,
            "![[Pasted image 20261023193000-1.png]]\n[[Report Q3-1.pdf]]"
        );
        // No temporary file is left behind.
        assert!(std::fs::read_dir(&folder).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".okilum-paste-")));
    }

    #[test]
    fn a_file_already_in_the_vault_is_linked_not_copied() {
        let vault = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(vault.path().join("img")).unwrap();
        let inside = vault.path().join("img/a b.png");
        std::fs::write(&inside, b"\x89PNG").unwrap();
        let doc = vault.path().join("Notes.md");
        std::fs::write(&doc, b"# x").unwrap();
        let saved =
            save_pasted_files_with(vault.path(), "n.md", &[inside, doc], "s", &|_| true).unwrap();
        assert_eq!(saved.text, "![[img/a b.png]]\n[[Notes.md]]");
        assert_eq!((saved.copied, saved.in_vault), (0, 2));
        assert!(!vault.path().join("attachments").exists());
    }

    #[test]
    fn a_directory_is_linked_where_it_is() {
        let vault = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let dir = outside.path().join("Folder");
        std::fs::create_dir_all(&dir).unwrap();
        let saved = save_pasted_files_with(
            vault.path(),
            "n.md",
            std::slice::from_ref(&dir),
            "s",
            &|_| false,
        )
        .unwrap();
        assert!(saved.text.starts_with("[Folder](file://"), "{}", saved.text);
        assert_eq!(saved.linked_outside, ["Folder"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_device_is_linked_not_read() {
        let vault = tempfile::tempdir().unwrap();
        let saved = save_pasted_files_with(
            vault.path(),
            "n.md",
            &[PathBuf::from("/dev/null")],
            "s",
            &|_| false,
        )
        .unwrap();
        assert_eq!(saved.linked_outside, ["null"]);
        assert_eq!(saved.copied, 0);
        assert!(!vault.path().join("attachments").exists());
    }

    #[test]
    fn clipboard_cache_folders_and_sniffing() {
        for path in [
            "/Users/o/Library/Application Support/com.raycast.macos/clipboard/file-1.png",
            "/private/var/folders/xy/T/TemporaryItems/NSIRD_screencaptureui/shot.png",
            "/var/folders/xy/T/x.png",
            "/tmp/x.png",
        ] {
            assert!(is_clipboard_cache(Path::new(path)), "{path}");
        }
        assert!(!is_clipboard_cache(Path::new("/Users/o/Desktop/x.png")));
        assert_eq!(sniff_image(b"\x89PNG\r\n\x1a\n"), Some(ImageKind::Png));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 "), Some(ImageKind::Webp));
        assert_eq!(sniff_image(b"%PDF"), None);
        assert!(is_image_extension("SVG") && !is_image_extension("pdf"));
    }

    #[test]
    fn failure_writes_nothing() {
        let vault = tempfile::tempdir().unwrap();
        app_json(vault.path(), Some("attachments"));
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

#[cfg(test)]
mod underscore_tests {
    use super::*;

    #[test]
    fn issue2847_legacy_attachment_folder_is_not_a_default() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("_Assets/Attachments")).unwrap();
        for (kind, expected) in [
            (AttachmentKind::Image, "@Assets/Images"),
            (AttachmentKind::File, "@Assets/Files"),
        ] {
            assert_eq!(
                attachment_folder(root.path(), "note.md", kind).unwrap(),
                root.path().join(expected)
            );
            assert!(!root.path().join(expected).exists());
        }
        std::fs::create_dir(root.path().join("attachments")).unwrap();
        assert_eq!(
            attachment_folder(root.path(), "note.md", AttachmentKind::File).unwrap(),
            root.path().join("attachments")
        );
        std::fs::create_dir(root.path().join(".obsidian")).unwrap();
        std::fs::write(
            root.path().join(".obsidian/app.json"),
            r#"{"attachmentFolderPath":"_Assets/Attachments"}"#,
        )
        .unwrap();
        assert_eq!(
            attachment_folder(root.path(), "note.md", AttachmentKind::File).unwrap(),
            root.path().join("_Assets/Attachments")
        );
    }
}
