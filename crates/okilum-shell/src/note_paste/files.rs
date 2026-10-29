//! Files pasted or dropped into a note (#1611). Platform-neutral.
//!
//! A file already inside the vault is linked where it is. Any other file is
//! copied into the attachment folder (`.obsidian/app.json`, see
//! [`super::attachments`]) under its own name, `-1`, `-2` when taken, never
//! over an existing file; an image from a clipboard cache or temporary folder
//! (Raycast's history, screenshot thumbnails) is named like a pasted image.
//! The copy runs off the UI thread and the link is inserted only once the
//! file is complete on disk. Folders are not pasted.

use super::attachments::{self, embed, is_clipboard_cache, is_image_extension};
use super::offer::VIDEO_EXTENSIONS;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

/// Default for Settings → Editor → "Ask before copying files larger than".
pub const DEFAULT_ASK_ABOVE_MB: u32 = 100;
/// Names tried in one folder before giving up.
const MAX_SUFFIX: usize = 999;
/// Chunk size of a copy across volumes, between progress reports.
const CHUNK: usize = 1024 * 1024;

const AUDIO_EXTENSIONS: [&str; 8] = ["mp3", "wav", "m4a", "ogg", "flac", "aac", "webm", "3gp"];
const EMBED_EXTENSIONS: [&str; 1] = ["pdf"];
/// Video Obsidian embeds that the Feedback window's video slot does not take.
const MORE_VIDEO_EXTENSIONS: [&str; 2] = ["mkv", "ogv"];
/// Longest file name in bytes (APFS and ext4 allow 255), leaving room for a
/// `-999` suffix.
const MAX_NAME_BYTES: usize = 240;
/// Vault entries a name scan looks at before giving up (#1611).
const MAX_SCAN_ENTRIES: usize = 500_000;
/// A hidden temporary copy older than this was left by an interrupted copy.
const STALE_TEMPORARY: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// What one pasted file becomes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileAction {
    /// Inside the vault: linked in place, nothing copied.
    Link { text: String },
    /// Outside the vault: copied as `file_name` (or the next free variant).
    Copy { file_name: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileItem {
    pub source: PathBuf,
    pub size: u64,
    pub action: FileAction,
}

/// What a paste of file URLs does, decided on the UI thread from metadata
/// only; the copying happens later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePlan {
    pub image_options: super::image_import::Options,
    pub items: Vec<FileItem>,
    /// The attachment folder copies go to.
    pub folder: PathBuf,
    /// The vault, for the name scan that keeps a copy's link unambiguous.
    pub vault_root: PathBuf,
    /// The attachment folder relative to the vault, `/`-separated (`""`: the
    /// vault root).
    pub folder_rel: String,
    /// Folders on the clipboard: never pasted.
    pub folders: usize,
    /// Files that were gone by the time of the paste.
    pub missing: usize,
    /// Bytes to copy into the vault.
    pub copy_bytes: u64,
    /// Ask first: a file to copy is above the threshold.
    pub ask: bool,
}

impl FilePlan {
    pub fn copies(&self) -> usize {
        self.items
            .iter()
            .filter(|item| matches!(item.action, FileAction::Copy { .. }))
            .count()
    }

    /// What to tell the user about what was left out, if anything.
    pub fn skipped_text(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.folders == 1 {
            parts.push("A folder can’t be pasted into a note.".to_string());
        } else if self.folders > 1 {
            parts.push(format!(
                "{} folders can’t be pasted into a note.",
                self.folders
            ));
        }
        if self.missing > 0 {
            parts.push(format!(
                "{} copied file{} no longer exist{}.",
                self.missing,
                if self.missing == 1 { "" } else { "s" },
                if self.missing == 1 { "s" } else { "" }
            ));
        }
        (!parts.is_empty()).then(|| parts.join(" "))
    }
}

/// The plan for `paths` pasted into the note `note_rel`. `ask_above` is the
/// size above which a copy needs confirmation (`None`: never ask).
pub fn plan(
    paths: &[PathBuf],
    vault_root: &Path,
    note_rel: &str,
    stamp: &str,
    ask_above: Option<u64>,
) -> FilePlan {
    let folder = attachments::attachment_folder(vault_root, note_rel);
    let vault = std::fs::canonicalize(vault_root).unwrap_or_else(|_| vault_root.to_path_buf());
    let folder_rel = folder
        .strip_prefix(vault_root)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let mut plan = FilePlan {
        image_options: super::image_import::Options::disabled(),
        items: Vec::new(),
        folder,
        vault_root: vault_root.to_path_buf(),
        folder_rel: slash_path(&folder_rel),
        folders: 0,
        missing: 0,
        copy_bytes: 0,
        ask: false,
    };
    for path in paths {
        let Ok(metadata) = std::fs::metadata(path) else {
            plan.missing += 1;
            continue;
        };
        if metadata.is_dir() {
            plan.folders += 1;
            continue;
        }
        let size = metadata.len();
        let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        if let Ok(rel) = real.strip_prefix(&vault) {
            if !hidden(rel) {
                let text = vault_link(rel);
                plan.items.push(FileItem {
                    source: path.clone(),
                    size,
                    action: FileAction::Link { text },
                });
                continue;
            }
        }
        let file_name = copy_name(path, stamp);
        plan.copy_bytes += size;
        if ask_above.is_some_and(|limit| size > limit) {
            plan.ask = true;
        }
        plan.items.push(FileItem {
            source: path.clone(),
            size,
            action: FileAction::Copy { file_name },
        });
    }
    plan
}

/// A vault path under a dot folder (`.obsidian`, `.trash`) is not a note's
/// attachment; such a file is copied like an outside one.
fn hidden(rel: &Path) -> bool {
    rel.components().any(|c| match c {
        Component::Normal(part) => part.to_string_lossy().starts_with('.'),
        _ => false,
    })
}

/// The name a copied file gets: its own, or `Pasted image <stamp>.<ext>` for
/// an image out of a clipboard cache or temporary folder (#1602: Raycast's
/// `file-8F3A….png` means nothing in a note). A cached file without an
/// extension is named by its content. Taken names get `-1`, `-2` when the
/// file is placed.
fn copy_name(path: &Path, stamp: &str) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Pasted file".into());
    let extension = path.extension().and_then(|e| e.to_str()).map(str::to_owned);
    let image_extension = match &extension {
        Some(extension) => is_image_extension(extension).then(|| extension.clone()),
        None => sniff(path).map(|kind| kind.extension().to_owned()),
    };
    match image_extension {
        Some(image) if is_clipboard_cache(path) => attachments::pasted_name(stamp, 0, &image),
        Some(image) if extension.is_none() => sanitize_name(&format!("{name}.{image}")),
        _ => sanitize_name(&name),
    }
}

/// The image kind of a file's first bytes.
fn sniff(path: &Path) -> Option<super::offer::ImageKind> {
    let mut head = [0u8; 16];
    let read = std::fs::File::open(path)
        .and_then(|mut file| file.read(&mut head))
        .unwrap_or(0);
    attachments::sniff_image(&head[..read])
}

/// A file name safe to create: no separators or control characters, no
/// leading dot, at most 120 characters with the extension kept.
pub fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            // `[ ] # ^ |` are wikilink syntax: `![[a #1.pdf]]` would name a
            // heading, not the file.
            '/' | '\\' | ':' | '\0' | '[' | ']' | '#' | '^' | '|' => '-',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim().to_string();
    let cleaned = if cleaned.is_empty() {
        "Pasted file".to_string()
    } else {
        cleaned
    };
    if cleaned.chars().count() <= 120 && cleaned.len() <= MAX_NAME_BYTES {
        return cleaned;
    }
    let (stem, extension) = split_name(&cleaned);
    let keep = 120usize
        .saturating_sub(extension.chars().count() + 1)
        .max(1);
    // Characters, then bytes: 120 CJK characters are 360 bytes.
    let byte_budget = MAX_NAME_BYTES.saturating_sub(extension.len() + 1).max(4);
    let mut stem: String = stem.chars().take(keep).collect();
    while stem.len() > byte_budget {
        stem.pop();
    }
    let stem = stem.trim_end().to_string();
    if extension.is_empty() {
        stem
    } else {
        format!("{stem}.{extension}")
    }
}

fn split_name(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < name.len() && name.len() - dot <= 12 => {
            (&name[..dot], &name[dot + 1..])
        }
        _ => (name, ""),
    }
}

/// `name.ext`, `name-1.ext`, `name-2.ext`, …
pub fn numbered(name: &str, suffix: usize) -> String {
    if suffix == 0 {
        return name.to_string();
    }
    match split_name(name) {
        (stem, "") => format!("{stem}-{suffix}"),
        (stem, extension) => format!("{stem}-{suffix}.{extension}"),
    }
}

/// The kinds Obsidian shows inline: `![[…]]`; anything else is a link.
pub fn embeds_inline(name: &str) -> bool {
    let extension = match split_name(name) {
        (_, "") => return false,
        (_, extension) => extension.to_ascii_lowercase(),
    };
    is_image_extension(&extension)
        || VIDEO_EXTENSIONS.contains(&extension.as_str())
        || MORE_VIDEO_EXTENSIONS.contains(&extension.as_str())
        || AUDIO_EXTENSIONS.contains(&extension.as_str())
        || EMBED_EXTENSIONS.contains(&extension.as_str())
}

/// The wikilink for a file the note can resolve by `target` (a name or a
/// vault-relative path): `![[x.png]]`, `[[x.zip]]`, `[[Note]]` for a note.
pub fn wikilink(target: &str) -> String {
    if let Some(note) = target
        .strip_suffix(".md")
        .or_else(|| target.strip_suffix(".MD"))
    {
        return format!("[[{note}]]");
    }
    if embeds_inline(target) {
        embed(target)
    } else {
        format!("[[{target}]]")
    }
}

/// The link to a vault file by its vault-relative path, which is never
/// ambiguous (#1611: a name alone can resolve to another file of that name
/// elsewhere in the vault). A path holding wikilink syntax (`[ ] # ^ |`) is a
/// Markdown link instead, percent-encoded.
fn vault_link(rel: &Path) -> String {
    let target = slash_path(rel);
    if !has_link_syntax(&target) {
        return wikilink(&target);
    }
    let name = rel
        .file_name()
        .map(|n| n.to_string_lossy().replace(['[', ']'], ""))
        .unwrap_or_default();
    let encoded: String = target
        .chars()
        .map(|c| match c {
            ' ' => "%20".to_string(),
            '#' => "%23".to_string(),
            '%' => "%25".to_string(),
            '^' => "%5E".to_string(),
            '|' => "%7C".to_string(),
            '[' => "%5B".to_string(),
            ']' => "%5D".to_string(),
            '(' => "%28".to_string(),
            ')' => "%29".to_string(),
            '<' => "%3C".to_string(),
            '>' => "%3E".to_string(),
            c => c.to_string(),
        })
        .collect();
    if embeds_inline(&target) {
        format!("![{name}]({encoded})")
    } else {
        format!("[{name}]({encoded})")
    }
}

/// Characters a wikilink target cannot hold.
fn has_link_syntax(target: &str) -> bool {
    target.contains(['[', ']', '#', '^', '|'])
}

/// A relative path with `/` separators.
pub(super) fn slash_path(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Every file name in the vault, lowercased (dot folders skipped), taken
/// before files are added to it (#1611). Blocking: background only.
pub fn vault_names(vault_root: &Path) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    let mut folders = vec![vault_root.to_path_buf()];
    let mut seen = 0usize;
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_SCAN_ENTRIES {
                return names;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => folders.push(entry.path()),
                Ok(_) => {
                    names.insert(name.to_lowercase());
                }
                Err(_) => {}
            }
        }
    }
    names
}

/// The link target for `name`, just placed in the attachment folder
/// `folder_rel`: the name alone, or the vault-relative path when the vault
/// already had a file of that name elsewhere (`existing`: from
/// [`vault_names`] taken before the file was added).
pub fn link_target(
    name: &str,
    folder_rel: &str,
    existing: &std::collections::HashSet<String>,
) -> String {
    if folder_rel.is_empty() || !existing.contains(&name.to_lowercase()) {
        name.to_string()
    } else {
        format!("{folder_rel}/{name}")
    }
}

/// A Markdown link to a file left outside the vault ("Link to original
/// file"): `![name](<file:///…>)` for media, `[name](<file:///…>)` otherwise.
pub fn original_link(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into())
        .replace(['[', ']'], "");
    let url = url::Url::from_file_path(path)
        .map(|url| url.to_string())
        .unwrap_or_else(|_| path.to_string_lossy().into_owned());
    let bang = if embeds_inline(&name) { "!" } else { "" };
    format!("{bang}[{name}](<{url}>)")
}

/// The links for a finished plan, in clipboard order: `copied` holds the
/// link targets of the copies (see [`link_target`]), in order.
pub fn links(plan: &FilePlan, copied: &[Option<String>]) -> Vec<String> {
    let mut copies = copied.iter();
    plan.items
        .iter()
        .filter_map(|item| match &item.action {
            FileAction::Link { text } => Some(text.clone()),
            FileAction::Copy { .. } => copies.next().and_then(|name| name.as_deref()).map(wikilink),
        })
        .collect()
}

#[derive(Debug)]
pub enum CopyError {
    Folder(io::Error),
    Read(io::Error),
    Write(io::Error),
    NoFreeName,
}

impl CopyError {
    pub fn user_text(&self, name: &str) -> String {
        match self {
            Self::Folder(error) => {
                format!("Couldn’t create the attachment folder for “{name}”: {error}")
            }
            Self::Read(error) => format!("Couldn’t read “{name}”: {error}"),
            Self::Write(error) => format!("Couldn’t copy “{name}” into the vault: {error}"),
            Self::NoFreeName => format!("Couldn’t find a free name for “{name}”."),
        }
    }
}

/// Copy `source` into `folder` as `wanted` (or the next free variant) and
/// return the final name. The data goes to a hidden temporary name first and
/// is linked into place without replacing anything. On the same volume the
/// copy is the file system's own (an APFS clone); across volumes it runs in
/// chunks, reporting the bytes done to `progress`.
pub fn copy_into(
    source: &Path,
    folder: &Path,
    wanted: &str,
    progress: &mut dyn FnMut(u64),
) -> Result<String, CopyError> {
    std::fs::create_dir_all(folder).map_err(CopyError::Folder)?;
    remove_stale_temporaries(folder);
    let temporary = folder.join(format!(".okilum-paste-{}.tmp", uuid::Uuid::new_v4()));
    let result = copy_to_temporary(source, folder, &temporary, progress)
        .and_then(|()| place(&temporary, folder, wanted));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Write `bytes` into `folder` as `wanted` (or the next free variant) and
/// return the final name: a hidden temporary file first, synced, then linked
/// into place without replacing anything (#1611: downloaded images).
pub fn write_into(folder: &Path, wanted: &str, bytes: &[u8]) -> Result<String, CopyError> {
    std::fs::create_dir_all(folder).map_err(CopyError::Folder)?;
    let temporary = folder.join(format!(".okilum-paste-{}.tmp", uuid::Uuid::new_v4()));
    let written = (|| -> Result<(), CopyError> {
        let mut writer = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(CopyError::Write)?;
        writer.write_all(bytes).map_err(CopyError::Write)?;
        writer.sync_all().map_err(CopyError::Write)
    })();
    let result = written.and_then(|()| place(&temporary, folder, wanted));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Remove hidden temporary copies an interrupted copy (Quit, a crash) left
/// in `folder`; once per folder per run, only ones older than
/// [`STALE_TEMPORARY`] so a copy in progress is never touched.
fn remove_stale_temporaries(folder: &Path) {
    static SWEPT: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
    {
        let Ok(mut swept) = SWEPT.lock() else {
            return;
        };
        if swept.iter().any(|done| done == folder) {
            return;
        }
        swept.push(folder.to_path_buf());
    }
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with(".okilum-paste-") && name.ends_with(".tmp")) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > STALE_TEMPORARY);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn copy_to_temporary(
    source: &Path,
    folder: &Path,
    temporary: &Path,
    progress: &mut dyn FnMut(u64),
) -> Result<(), CopyError> {
    if same_volume(source, folder) {
        let bytes = std::fs::copy(source, temporary).map_err(CopyError::Write)?;
        progress(bytes);
        return Ok(());
    }
    let mut reader = std::fs::File::open(source).map_err(CopyError::Read)?;
    let mut writer = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary)
        .map_err(CopyError::Write)?;
    let mut buffer = vec![0u8; CHUNK];
    let mut done = 0u64;
    loop {
        let read = reader.read(&mut buffer).map_err(CopyError::Read)?;
        if read == 0 {
            break;
        }
        writer
            .write_all(&buffer[..read])
            .map_err(CopyError::Write)?;
        done += read as u64;
        progress(done);
    }
    writer.sync_all().map_err(CopyError::Write)?;
    Ok(())
}

#[cfg(unix)]
fn same_volume(source: &Path, folder: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(source), std::fs::metadata(folder)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_volume(_: &Path, _: &Path) -> bool {
    true
}

/// Move the finished temporary file to the first free name.
fn place(temporary: &Path, folder: &Path, wanted: &str) -> Result<String, CopyError> {
    for suffix in 0..=MAX_SUFFIX {
        let name = numbered(wanted, suffix);
        let path = folder.join(&name);
        if path.exists() {
            continue;
        }
        match std::fs::hard_link(temporary, &path) {
            Ok(()) => {
                let _ = std::fs::remove_file(temporary);
                sync_folder(folder);
                return Ok(name);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                // No hard links here (FAT, some network shares): reserve the
                // name, then replace only that reservation.
                match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                {
                    Ok(_) => {
                        std::fs::rename(temporary, &path).map_err(CopyError::Write)?;
                        sync_folder(folder);
                        return Ok(name);
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(CopyError::Write(error)),
                }
            }
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

/// Human size for a confirmation: `1.2 GB`, `340 MB`.
pub fn size_text(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let mb = bytes as f64 / MB;
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else if mb >= 10.0 {
        format!("{mb:.0} MB")
    } else {
        format!("{mb:.1} MB")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn outside_files_are_copied_under_their_own_names() {
        let vault = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let doc = outside.path().join("Report Q3.pdf");
        write(&doc, b"%PDF");
        let plan = plan(
            std::slice::from_ref(&doc),
            vault.path(),
            "Notes/a.md",
            "20261023120000",
            None,
        );
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.copy_bytes, 4);
        assert!(!plan.ask);
        assert_eq!(plan.folder, vault.path().join("attachments"));
        let FileAction::Copy { file_name } = &plan.items[0].action else {
            panic!("{plan:?}");
        };
        // A temp-dir PDF keeps its name; only images get pasted-image names.
        assert_eq!(file_name, "Report Q3.pdf");
        let name = copy_into(&doc, &plan.folder, file_name, &mut |_| {}).unwrap();
        assert_eq!(name, "Report Q3.pdf");
        let again = copy_into(&doc, &plan.folder, file_name, &mut |_| {}).unwrap();
        assert_eq!(again, "Report Q3-1.pdf");
        assert_eq!(
            std::fs::read(plan.folder.join("Report Q3.pdf")).unwrap(),
            b"%PDF"
        );
        // No temporary file is left.
        let names: Vec<_> = std::fs::read_dir(&plan.folder)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(
            names.iter().all(|n| !n.starts_with(".okilum-paste-")),
            "{names:?}"
        );
        assert_eq!(
            links(&plan, &[Some(again)]),
            ["![[Report Q3-1.pdf]]".to_string()]
        );
    }

    #[test]
    fn vault_files_are_linked_not_copied() {
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path();
        write(&root.join("attachments/shot.png"), b"png");
        write(&root.join("Projects/spec.pdf"), b"pdf");
        write(&root.join("Projects/Plan.md"), b"# plan");
        write(&root.join("Notes/here.zip"), b"zip");
        let paths = [
            root.join("attachments/shot.png"),
            root.join("Projects/spec.pdf"),
            root.join("Projects/Plan.md"),
            root.join("Notes/here.zip"),
        ];
        let plan = plan(&paths, root, "Notes/a.md", "x", Some(0));
        let texts: Vec<_> = plan
            .items
            .iter()
            .map(|item| match &item.action {
                FileAction::Link { text } => text.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            texts,
            [
                "![[attachments/shot.png]]",
                "![[Projects/spec.pdf]]",
                "[[Projects/Plan]]",
                "[[Notes/here.zip]]"
            ]
        );
        assert!(!plan.ask, "nothing is copied, nothing to confirm");
        assert!(!root.join("attachments/shot-1.png").exists());
    }

    #[test]
    fn folders_and_missing_files_are_skipped_and_said() {
        let vault = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let folder = outside.path().join("dir");
        std::fs::create_dir_all(&folder).unwrap();
        let plan = plan(
            &[folder, outside.path().join("gone.txt")],
            vault.path(),
            "a.md",
            "x",
            None,
        );
        assert!(plan.items.is_empty());
        assert_eq!(
            plan.skipped_text().unwrap(),
            "A folder can’t be pasted into a note. 1 copied file no longer exists."
        );
    }

    #[test]
    fn large_files_ask_first() {
        let vault = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let big = outside.path().join("movie.mov");
        write(&big, &[0u8; 2048]);
        assert!(
            plan(
                std::slice::from_ref(&big),
                vault.path(),
                "a.md",
                "x",
                Some(1024)
            )
            .ask
        );
        assert!(
            !plan(
                std::slice::from_ref(&big),
                vault.path(),
                "a.md",
                "x",
                Some(4096)
            )
            .ask
        );
        assert!(!plan(&[big], vault.path(), "a.md", "x", None).ask);
    }

    #[test]
    fn clipboard_cache_images_get_pasted_image_names() {
        for path in [
            "/Users/o/Library/Application Support/com.raycast.macos/clipboard/file-8F3A.png",
            "/var/folders/xy/T/TemporaryItems/NSIRD_screencaptureui/Screenshot.png",
            "/private/var/folders/xy/T/x.jpeg",
        ] {
            assert!(is_clipboard_cache(Path::new(path)), "{path}");
            let name = copy_name(Path::new(path), "20261023120000");
            assert!(name.starts_with("Pasted image 20261023120000."), "{name}");
        }
        assert_eq!(
            copy_name(Path::new("/var/folders/a.png"), "S"),
            "Pasted image S.png"
        );
        // A cached PDF keeps its name; only images get pasted-image names.
        assert_eq!(copy_name(Path::new("/var/folders/a.pdf"), "S"), "a.pdf");
        assert!(!is_clipboard_cache(Path::new("/Users/o/Desktop/Shot.png")));
        assert_eq!(
            copy_name(Path::new("/Users/o/Desktop/Shot.png"), "S"),
            "Shot.png"
        );
    }

    /// #1602 Raycast: a cached file without an extension is named by its
    /// content; two cached images in one paste get `-1` when placed.
    #[test]
    fn cached_files_are_named_by_content_and_numbered_when_placed() {
        let vault = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let cache = outside.path().join("com.raycast.macos/clipboard");
        write(&cache.join("file-A"), b"\xff\xd8\xff\xe0jpeg");
        write(&cache.join("file-B.png"), b"\x89PNG\r\n\x1a\nb");
        write(&cache.join("file-C.png"), b"\x89PNG\r\n\x1a\nc");
        let paths = [
            cache.join("file-A"),
            cache.join("file-B.png"),
            cache.join("file-C.png"),
        ];
        let plan = plan(&paths, vault.path(), "n.md", "20261023193000", None);
        let mut copied = Vec::new();
        for item in &plan.items {
            let FileAction::Copy { file_name } = &item.action else {
                panic!("{item:?}");
            };
            copied.push(Some(
                copy_into(&item.source, &plan.folder, file_name, &mut |_| {}).unwrap(),
            ));
        }
        assert_eq!(
            links(&plan, &copied),
            [
                "![[Pasted image 20261023193000.jpg]]",
                "![[Pasted image 20261023193000.png]]",
                "![[Pasted image 20261023193000-1.png]]"
            ]
        );
        assert_eq!(
            std::fs::read(plan.folder.join("Pasted image 20261023193000-1.png")).unwrap(),
            b"\x89PNG\r\n\x1a\nc"
        );
    }

    #[test]
    fn names_are_sanitized_and_numbered() {
        assert_eq!(sanitize_name(".hidden"), "hidden");
        assert_eq!(sanitize_name("a/b:c"), "a-b-c");
        assert_eq!(sanitize_name("  "), "Pasted file");
        assert_eq!(
            sanitize_name("Invoice #42 [v2] a|b^c.pdf"),
            "Invoice -42 -v2- a-b-c.pdf"
        );
        let cjk = format!("{}.png", "画".repeat(110));
        let cut = sanitize_name(&cjk);
        assert!(cut.len() <= MAX_NAME_BYTES, "{}", cut.len());
        assert!(cut.ends_with(".png"));
        let long = format!("{}.png", "x".repeat(200));
        let cut = sanitize_name(&long);
        assert_eq!(cut.chars().count(), 120);
        assert!(cut.ends_with(".png"));
        assert_eq!(numbered("a.tar.gz", 2), "a.tar-2.gz");
        assert_eq!(numbered("README", 1), "README-1");
        assert_eq!(numbered("x.png", 0), "x.png");
    }

    #[test]
    fn embed_syntax_follows_the_kind() {
        assert_eq!(wikilink("a.PNG"), "![[a.PNG]]");
        assert_eq!(wikilink("clip.mov"), "![[clip.mov]]");
        assert_eq!(wikilink("voice.m4a"), "![[voice.m4a]]");
        assert_eq!(wikilink("doc.pdf"), "![[doc.pdf]]");
        assert_eq!(wikilink("data.csv"), "[[data.csv]]");
        assert_eq!(wikilink("Note.md"), "[[Note]]");
        assert_eq!(wikilink("Makefile"), "[[Makefile]]");
        assert_eq!(wikilink("clip.mkv"), "![[clip.mkv]]");
        let link = original_link(Path::new("/Volumes/Big Disk/movie [cut].mov"));
        assert_eq!(
            link,
            "![movie cut.mov](<file:///Volumes/Big%20Disk/movie%20[cut].mov>)"
        );
    }

    #[test]
    fn vault_files_with_link_syntax_get_markdown_links() {
        assert_eq!(
            vault_link(Path::new("Docs/Invoice #42.pdf")),
            "![Invoice #42.pdf](Docs/Invoice%20%2342.pdf)"
        );
        assert_eq!(vault_link(Path::new("a|b.zip")), "[a|b.zip](a%7Cb.zip)");
    }

    #[test]
    fn a_name_taken_elsewhere_in_the_vault_is_linked_by_path() {
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path();
        write(&root.join("Projects/README.md"), b"x");
        write(&root.join(".obsidian/logo.png"), b"x");
        let names = vault_names(root);
        assert!(names.contains("readme.md"));
        assert!(!names.contains("logo.png"));
        assert_eq!(
            link_target("README.md", "attachments", &names),
            "attachments/README.md"
        );
        assert_eq!(wikilink("attachments/README.md"), "[[attachments/README]]");
        assert_eq!(link_target("logo.png", "attachments", &names), "logo.png");
        assert_eq!(link_target("README.md", "", &names), "README.md");
    }
}
