use crate::prose::prose_spans;
use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

pub mod warm;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Convert an OS relative path into the canonical slash-separated note identity.
/// Only the platform separator changes; a literal Unix backslash stays literal.
pub fn note_path(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

/// Canonical vault root. On Windows a share root (`\\server\share`, or a mapped
/// drive resolving to one) canonicalizes to a bare UNC prefix without a root
/// directory, so `strip_prefix` of every descendant kept a leading separator and
/// all vault paths started with `/` (#922). Other roots are unchanged.
pub fn canonical_root(path: &Path) -> std::io::Result<PathBuf> {
    Ok(with_root_directory(path.canonicalize()?))
}

/// Give a bare Windows path prefix its root directory; identity elsewhere.
pub fn with_root_directory(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        use std::path::Component;
        let mut components = path.components();
        if matches!(components.next(), Some(Component::Prefix(_))) && components.next().is_none() {
            let mut rooted = path.into_os_string();
            rooted.push("\\");
            return PathBuf::from(rooted);
        }
    }
    path
}

/// Human-readable paths and error chains; keep verbatim prefixes for filesystem I/O.
pub fn display_path(path: &Path) -> String {
    display_error(&path.to_string_lossy())
}

pub fn display_error(message: &str) -> String {
    #[cfg(windows)]
    {
        display_windows_error(message)
    }
    #[cfg(not(windows))]
    {
        message.to_owned()
    }
}

#[cfg(any(windows, test))]
fn display_windows_error(message: &str) -> String {
    message.replace(r"\\?\UNC\", r"\\").replace(r"\\?\", "")
}

/// Keep the OS cause and explain names that Windows cannot open over SMB.
pub fn read_error(path: &Path, error: &std::io::Error) -> String {
    #[cfg(windows)]
    if error.raw_os_error() == Some(123) {
        return windows_name_error(&error.to_string(), long_windows_name(path).as_deref());
    }
    let _ = path;
    error.to_string()
}

#[cfg(any(windows, test))]
fn windows_name_error(cause: &str, original: Option<&Path>) -> String {
    let original = original
        .map(|path| format!(" Original name: {}.", display_path(path)))
        .unwrap_or_default();
    format!("{cause} Name not valid on Windows (possibly an SMB short alias).{original} Rename the original file on the server or another platform.")
}

#[cfg(windows)]
fn long_windows_name(path: &Path) -> Option<PathBuf> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetLongPathNameW;
    let input: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    // Some SMB servers expose only the mangled alias. Recover a long name only
    // when the OS actually supplies one; do not infer it from an 8.3 basename.
    let needed = unsafe { GetLongPathNameW(input.as_ptr(), std::ptr::null_mut(), 0) };
    if needed == 0 || needed > 32768 {
        return None;
    }
    let mut output = vec![0; needed as usize];
    let written = unsafe { GetLongPathNameW(input.as_ptr(), output.as_mut_ptr(), needed) };
    if written == 0 || written >= needed {
        return None;
    }
    let long = PathBuf::from(std::ffi::OsString::from_wide(&output[..written as usize]));
    (long.file_name() != path.file_name()).then_some(long)
}

#[cfg(test)]
mod root_tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_share_roots_gain_a_root_directory_so_vault_paths_are_relative() {
        // canonicalize() returns the verbatim form, which has no root directory.
        for bare in [r"\\?\UNC\10.10.0.35\qa516w"] {
            let root = with_root_directory(PathBuf::from(bare));
            assert_eq!(root.as_os_str(), &*format!("{bare}\\"));
            let note = root.join("notes").join("a.md");
            assert_eq!(note_path(note.strip_prefix(&root).unwrap()), "notes/a.md");
            // Positive control: the bare prefix is what produced "/notes/a.md".
            let bare = PathBuf::from(bare);
            assert_eq!(note_path(note.strip_prefix(&bare).unwrap()), "/notes/a.md");
        }
        for rooted in [
            r"\\10.10.0.35\qa516w",
            r"\\?\C:\vault",
            r"\\?\UNC\server\share\vault",
            r"C:\",
        ] {
            assert_eq!(
                with_root_directory(PathBuf::from(rooted)),
                PathBuf::from(rooted)
            );
        }
    }

    #[test]
    fn ordinary_roots_are_unchanged() {
        let root = std::env::temp_dir();
        assert_eq!(with_root_directory(root.clone()), root);
        assert_eq!(canonical_root(&root).unwrap(), root.canonicalize().unwrap());
    }
}

#[cfg(test)]
mod display_tests {
    #[test]
    fn windows_invalid_name_explains_alias_and_only_reports_supplied_original() {
        let cause = "The filename syntax is incorrect. (os error 123)";
        let unknown = super::windows_name_error(cause, None);
        assert!(unknown.contains("os error 123") && unknown.contains("Name not valid on Windows"));
        assert!(unknown.contains("SMB short alias") && !unknown.contains("Original name:"));
        let known = super::windows_name_error(
            cause,
            Some(std::path::Path::new("Эскалация переговоров?.md")),
        );
        assert!(known.contains("Original name: Эскалация переговоров?.md"));
    }

    #[test]
    fn windows_error_paths_hide_prefixes_and_other_platforms_preserve_literal_names() {
        let error = r"Prepare \\?\C:\vault: Read \\?\UNC\server\share\note.md: Access is denied. (os error 5)";
        assert_eq!(
            super::display_windows_error(error),
            r"Prepare C:\vault: Read \\server\share\note.md: Access is denied. (os error 5)"
        );
        #[cfg(not(windows))]
        {
            let literal = r"weird\\?\file.md";
            assert_eq!(super::display_path(std::path::Path::new(literal)), literal);
            assert_eq!(super::display_error(error), error);
        }
    }
}

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".obsidian",
    ".trash",
    ".stfolder",
    ".stversions",
    ".tessera-index",
    "node_modules",
];
/// Exact Windows preimage spelling reserved for native safe-save recovery.
/// Keep the parser portable: Syncthing can bring these files to Unix clients.
pub fn windows_preimage_name(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .and_then(|s| s.strip_prefix(".tessera-save-"))
        .and_then(|s| s.strip_suffix(".previous"))
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok_and(|u| u.to_string() == id))
}

/// Service directories are excluded from every inventory consumer. Visibility
/// of ordinary dot/underscore paths belongs only to the browsing tree.
pub fn service_path(path: &Path) -> bool {
    path.components().any(|component| {
        let name = component.as_os_str();
        name.as_encoded_bytes().starts_with(b".tessera-save-")
            || name.as_encoded_bytes().starts_with(b"._")
            || name.to_str().is_some_and(|name| SKIP_DIRS.contains(&name))
    })
}

/// iCloud placeholders must not be opened just to populate derived data.
/// SF_DATALESS identifies evicted files that retain their ordinary filename.
pub(crate) fn cloud_placeholder(path: &Path) -> bool {
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("icloud"))
    {
        return true;
    }
    #[cfg(target_os = "macos")]
    {
        const SF_DATALESS: u32 = 0x4000_0000;
        if rustix::fs::stat(path).is_ok_and(|stat| stat.st_flags & SF_DATALESS != 0) {
            return true;
        }
    }
    false
}

pub(crate) fn read_source(path: &Path) -> std::io::Result<String> {
    if cloud_placeholder(path) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "iCloud placeholder is not downloaded",
        ));
    }
    std::fs::read_to_string(path)
}

/// Scan-admitted browser metadata. Identity is the complete root-relative path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    Directory,
    Markdown,
    Attachment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultEntry {
    pub path: String,
    pub kind: EntryKind,
}

/// A skipped entry, retained so a partial inventory is never presented as complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnreadableEntry {
    pub path: PathBuf,
    pub operation: &'static str,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Note {
    /// Path relative to vault root, with extension ("Dev/Areas/x/_index.md").
    pub path: String,
    /// Display title: file stem.
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Backlink {
    pub path: String,
    pub title: String,
    /// The line of the source note that contains the link.
    pub context: String,
    /// The target of the wikilink that produced this backlink, as written in
    /// the source (`[[target|alias]]` records `target`), so the link can be
    /// found again inside `context` without re-resolving anything.
    pub link: String,
    /// The link that produced this backlink named several notes, and this is
    /// one of them. It is not established that the author meant *this* note.
    pub ambiguous: bool,
    /// The frontmatter key when the link sits in the source's properties
    /// (`related: [[x]]`), so the target can show the relation (#386).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub property: Option<String>,
}

impl Backlink {
    /// The context line with inline Markdown stripped for display (#22):
    /// emphasis markers, wikilink brackets (alias wins), link syntax (text
    /// wins), and leading block markers. Code spans are kept verbatim.
    pub fn context_plain(&self) -> String {
        crate::render::strip_inline_markdown(&self.context)
    }

    /// `context_plain` plus the byte range, inside the returned string, of the
    /// display text of the wikilink that produced this backlink (#38): the
    /// first `[[...]]` on the line whose target is `self.link`. `None` when
    /// that link is not on the line (a context truncated at 240 chars can
    /// lose it).
    pub fn context_plain_with_link(&self) -> (String, Option<Range<usize>>) {
        crate::render::strip_inline_markdown_tracking(&self.context, Some(&self.link))
    }
}

/// What a wikilink target names.
///
/// The third case is the point. A link that names several notes used to be
/// resolved to whichever candidate happened to sort first, which is a guess the
/// reader cannot see and cannot correct. Making it a variant forces every call
/// site to decide what to do about it rather than silently inheriting a winner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Resolution {
    /// Exactly one note answers to this target.
    Resolved { path: String },
    /// Several notes answer to it and nothing in the link says which.
    /// Candidates are sorted by path, so the order is stable across runs.
    Ambiguous { candidates: Vec<String> },
    /// No note answers to it.
    Unresolved,
}

/// Borrowed resolution avoids allocating all candidates for bounded derived queries.
pub enum ResolutionRef<'a> {
    Resolved(&'a str),
    Ambiguous(&'a [String]),
    Unresolved,
}

/// Visit prose wikilinks in original byte order. Returning false stops scanning.
/// The target excludes display aliases and heading/block fragments, as in backlinks.
pub fn visit_wikilinks(text: &str, mut visit: impl FnMut(&str, Range<usize>) -> bool) {
    let re = wikilink_re();
    for span in prose_spans(text) {
        for cap in re.captures_iter(&text[span.clone()]) {
            let whole = cap.get(0).unwrap();
            if !visit(
                cap[1].trim(),
                span.start + whole.start()..span.start + whole.end(),
            ) {
                return;
            }
        }
    }
}

impl Resolution {
    /// The resolved path, or `None` for ambiguous and unresolved alike.
    ///
    /// For call sites that genuinely have nothing to do with an ambiguous link.
    /// Reaching for this to "just get a path" is how the old behaviour worked;
    /// prefer matching on the variants.
    pub fn path(&self) -> Option<&str> {
        match self {
            Resolution::Resolved { path } => Some(path),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct Vault {
    /// Explicit quick-view scope: direct document links can be checked without a recursive inventory.
    pub single_file: bool,
    /// No entries were skipped during enumeration or source reads.
    pub inventory_complete: bool,
    /// Enumeration has finished, including a usable partial inventory.
    pub inventory_scanned: bool,
    pub unreadable: Vec<UnreadableEntry>,
    pub root: PathBuf,
    pub notes: Vec<Note>,
    pub entries: Vec<VaultEntry>,
    /// Every segment-boundary suffix of a note's path (lowercased, without the
    /// `.md`) -> the notes carrying that suffix.
    ///
    /// A wikilink is a suffix of a path: `[[alpha]]`, `[[notes/alpha]]` and
    /// `[[a/b/notes/alpha]]` are progressively more qualified names for the same
    /// note, and each resolves exactly when it names one note and no other. The
    /// previous index was keyed on the bare stem alone, which had two
    /// consequences: colliding stems silently kept the shortest path, and a
    /// path-qualified link was reduced to its last segment before lookup, so
    /// `[[c/note]]` could resolve to a note with no `c/` anywhere in its path.
    suffix_map: HashMap<String, Vec<String>>,
    /// lowercase full rel path without .md -> rel path
    path_map: HashMap<String, String>,
    /// lowercase asset filename -> absolute path
    asset_map: HashMap<String, PathBuf>,
    /// target rel path -> backlinks
    backlink_map: HashMap<String, Vec<Backlink>>,
    // Metadata facts include skipped symlinks/non-directories, so an occupied
    // local path cannot redirect to a root/suffix namesake during graph build.
    occupied_paths: HashSet<String>,
    non_directory_paths: HashSet<String>,
    symlink_paths: HashSet<String>,
    pub(crate) graph_root: Option<PathBuf>,
    pub(crate) graph_os_paths: std::sync::Arc<std::sync::Mutex<HashMap<String, Option<String>>>>,
}

impl Vault {
    pub fn scan(root: &Path) -> Result<Vault> {
        Self::scan_with(root, &mut |_, _| Ok(()))
    }

    /// Cooperative scan. The callback runs between entries and source reads.
    pub fn scan_with(
        root: &Path,
        checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
    ) -> Result<Vault> {
        let mut vault = Self::scan_metadata_with(root, checkpoint)?;
        let mut unreadable = Vec::new();
        vault.build_backlinks_from(checkpoint, |path| {
            let path = root.join(path);
            match read_source(&path) {
                Ok(source) => Some(source),
                Err(error) => {
                    unreadable.push(UnreadableEntry {
                        path,
                        operation: "read note",
                        error: error.to_string(),
                    });
                    None
                }
            }
        })?;
        vault.unreadable.extend(unreadable);
        vault.finish_scan_report();
        Ok(vault)
    }

    /// Collect each source once, retaining the exact bytes used for backlinks
    /// and the Reader's content-addressed search snapshot.
    pub fn scan_snapshot_with(
        root: &Path,
        checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
    ) -> Result<(Vault, HashMap<String, Vec<u8>>)> {
        Self::scan_snapshot_read_with(root, checkpoint, &mut |path| {
            read_source(path).map(String::into_bytes)
        })
    }

    fn scan_snapshot_read_with(
        root: &Path,
        checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
        read: &mut impl FnMut(&Path) -> std::io::Result<Vec<u8>>,
    ) -> Result<(Vault, HashMap<String, Vec<u8>>)> {
        let mut vault = Self::scan_metadata_with(root, checkpoint)?;
        let mut sources = HashMap::with_capacity(vault.notes.len());
        for (count, note) in vault.notes.iter().enumerate() {
            checkpoint("Reading notes", count)?;
            let path = root.join(&note.path);
            let result = if cloud_placeholder(&path) {
                Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "iCloud placeholder is not downloaded",
                ))
            } else {
                read(&path)
            };
            match result {
                Ok(bytes) => match std::str::from_utf8(&bytes) {
                    Ok(_) => {
                        sources.insert(note.path.clone(), bytes);
                    }
                    Err(error) => vault.unreadable.push(UnreadableEntry {
                        path,
                        operation: "decode note",
                        error: error.to_string(),
                    }),
                },
                Err(error) => vault.unreadable.push(UnreadableEntry {
                    error: read_error(&path, &error),
                    path,
                    operation: "read note",
                }),
            }
        }
        vault.finish_scan_report();
        vault.build_backlinks_from(checkpoint, |path| {
            sources
                .get(path)
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        })?;
        Ok((vault, sources))
    }

    /// Build the shared resolver from directory metadata without opening note
    /// contents. Root-bounded services supply their own source reader instead
    /// of using the desktop reader's filesystem access for backlinks.
    pub fn scan_metadata(root: &Path) -> Result<Vault> {
        Self::scan_metadata_with(root, &mut |_, _| Ok(()))
    }

    pub(crate) fn scan_metadata_with(
        root: &Path,
        checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
    ) -> Result<Vault> {
        checkpoint("Discovering notes", 0)?;
        // A bad root is a failed open, not an empty successful inventory.
        std::fs::read_dir(root)
            .with_context(|| format!("Read vault directory {}", root.display()))?;
        Self::scan_metadata_entries(
            root,
            checkpoint,
            WalkDir::new(root).into_iter().filter_entry(|e| {
                e.depth() == 0 || !service_path(e.path().strip_prefix(root).unwrap_or(e.path()))
            }),
        )
    }

    fn scan_metadata_entries(
        root: &Path,
        checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
        walker: impl IntoIterator<Item = walkdir::Result<walkdir::DirEntry>>,
    ) -> Result<Vault> {
        let mut inventory_complete = true;
        let mut unreadable = Vec::new();
        let mut notes = Vec::new();
        let mut entries = Vec::new();
        let mut suffix_map: HashMap<String, Vec<String>> = HashMap::new();
        let mut path_map = HashMap::new();
        let mut asset_map = HashMap::new();
        let mut occupied_paths = HashSet::new();
        let mut non_directory_paths = HashSet::new();
        let mut symlink_paths = HashSet::new();

        for entry in walker {
            checkpoint("Discovering notes", notes.len())?;
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    inventory_complete = false;
                    unreadable.push(UnreadableEntry {
                        path: error.path().unwrap_or(root).to_path_buf(),
                        operation: "enumerate entry",
                        error: error.to_string(),
                    });
                    continue;
                }
            };
            let path = entry.path();
            let relative = path.strip_prefix(root)?;
            if service_path(relative) {
                continue;
            }
            let rel = note_path(relative);
            if !rel.is_empty() {
                occupied_paths.insert(rel.to_lowercase());
                if !entry.file_type().is_dir() {
                    non_directory_paths.insert(rel.to_lowercase());
                }
                if entry.file_type().is_symlink() {
                    symlink_paths.insert(rel.to_lowercase());
                }
            }
            if cloud_placeholder(path) {
                unreadable.push(UnreadableEntry {
                    path: path.to_path_buf(),
                    operation: "read note",
                    error: "iCloud placeholder is not downloaded".into(),
                });
                inventory_complete = false;
                continue;
            }
            if entry.file_type().is_dir() && !rel.is_empty() {
                entries.push(VaultEntry {
                    path: rel,
                    kind: EntryKind::Directory,
                });
                continue;
            }
            if !entry.file_type().is_file() {
                continue;
            }
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if ext == "md" {
                entries.push(VaultEntry {
                    path: rel.clone(),
                    kind: EntryKind::Markdown,
                });
                let stem = path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let key = rel.to_lowercase();
                let key = key.trim_end_matches(".md");
                // Index every suffix, not just the stem: "a/b/note" is reachable
                // as "note", "b/note" and "a/b/note", and each of those is a
                // legitimate way to write the link.
                let segments: Vec<&str> = key.split('/').collect();
                for i in 0..segments.len() {
                    suffix_map
                        .entry(segments[i..].join("/"))
                        .or_default()
                        .push(rel.clone());
                }
                path_map.insert(key.to_string(), rel.clone());
                notes.push(Note {
                    path: rel,
                    title: stem,
                });
            } else {
                entries.push(VaultEntry {
                    path: rel,
                    kind: EntryKind::Attachment,
                });
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                asset_map.entry(name).or_insert_with(|| path.to_path_buf());
            }
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        notes.sort_by(|a, b| a.path.cmp(&b.path));
        // Stable candidate order: an ambiguity report that reshuffles between
        // runs is not a report anyone can act on.
        for candidates in suffix_map.values_mut() {
            candidates.sort();
            candidates.dedup();
        }

        Ok(Vault {
            single_file: false,
            inventory_complete,
            inventory_scanned: true,
            unreadable,
            root: root.to_path_buf(),
            notes,
            entries,
            suffix_map,
            path_map,
            asset_map,
            backlink_map: HashMap::new(),
            occupied_paths,
            non_directory_paths,
            symlink_paths,
            graph_root: None,
            graph_os_paths: Default::default(),
        })
    }

    fn finish_scan_report(&mut self) {
        self.unreadable.sort_by(|a, b| a.path.cmp(&b.path));
        self.unreadable.dedup_by(|a, b| a.path == b.path);
        self.inventory_complete &= self.unreadable.is_empty();
    }

    /// Apply a validated regular-note identity without a directory walk.
    fn set_note_identity(&mut self, path: &str, present: bool) {
        self.entries.retain(|entry| entry.path != path);
        self.notes.retain(|note| note.path != path);
        self.path_map.retain(|_, value| value != path);
        self.suffix_map.retain(|_, values| {
            values.retain(|value| value != path);
            !values.is_empty()
        });
        self.occupied_paths.remove(&path.to_lowercase());
        self.non_directory_paths.remove(&path.to_lowercase());
        self.symlink_paths.remove(&path.to_lowercase());
        if present {
            self.occupied_paths.insert(path.to_lowercase());
            self.non_directory_paths.insert(path.to_lowercase());
            self.entries.push(VaultEntry {
                path: path.into(),
                kind: EntryKind::Markdown,
            });
            self.notes.push(Note {
                path: path.into(),
                title: Path::new(path)
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            });
            let lower = path.to_lowercase();
            let key = lower.trim_end_matches(".md");
            self.path_map.insert(key.into(), path.into());
            let parts: Vec<_> = key.split('/').collect();
            for i in 0..parts.len() {
                let values = self.suffix_map.entry(parts[i..].join("/")).or_default();
                values.push(path.into());
                values.sort();
                values.dedup();
            }
            self.entries.sort_by(|a, b| a.path.cmp(&b.path));
            self.notes.sort_by(|a, b| a.path.cmp(&b.path));
        }
    }

    fn refresh_backlinks_from(
        &mut self,
        selected: &std::collections::BTreeSet<String>,
        checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
        source: impl FnMut(&str) -> Option<String>,
    ) -> Result<()> {
        let mut retained = std::mem::take(&mut self.backlink_map);
        for incoming in retained.values_mut() {
            incoming.retain(|link| !selected.contains(&link.path));
        }
        let identities: HashSet<_> = self.notes.iter().map(|note| &note.path).collect();
        retained.retain(|target, incoming| identities.contains(target) && !incoming.is_empty());
        let result = self.build_backlinks_selected(Some(selected), checkpoint, source);
        if result.is_ok() {
            for (target, incoming) in std::mem::take(&mut self.backlink_map) {
                let links = retained.entry(target).or_default();
                links.extend(incoming);
                links.sort_by(|a, b| a.path.cmp(&b.path));
            }
        }
        self.backlink_map = retained;
        result
    }

    /// Build only note identity from an already accepted inventory; performs no I/O.
    pub fn from_note_paths(paths: impl IntoIterator<Item = String>) -> Self {
        let mut vault = Self {
            single_file: false,
            inventory_complete: false,
            inventory_scanned: false,
            unreadable: Vec::new(),
            root: PathBuf::new(),
            notes: Vec::new(),
            entries: Vec::new(),
            suffix_map: HashMap::new(),
            path_map: HashMap::new(),
            asset_map: HashMap::new(),
            backlink_map: HashMap::new(),
            occupied_paths: HashSet::new(),
            non_directory_paths: HashSet::new(),
            symlink_paths: HashSet::new(),
            graph_root: None,
            graph_os_paths: Default::default(),
        };
        for path in paths {
            if service_path(Path::new(&path)) {
                continue;
            }
            let key = path.to_lowercase();
            let key = key.trim_end_matches(".md");
            let segments: Vec<_> = key.split('/').collect();
            for i in 0..segments.len() {
                vault
                    .suffix_map
                    .entry(segments[i..].join("/"))
                    .or_default()
                    .push(path.clone());
            }
            vault.path_map.insert(key.to_owned(), path.clone());
            vault.entries.push(VaultEntry {
                path: path.clone(),
                kind: EntryKind::Markdown,
            });
            vault.notes.push(Note {
                title: Self::title_of(&path),
                path,
            });
        }
        vault.notes.sort_by(|a, b| a.path.cmp(&b.path));
        for candidates in vault.suffix_map.values_mut() {
            candidates.sort();
            candidates.dedup();
        }
        vault
    }

    /// Publish a successfully created canonical note before watcher delivery.
    /// Updates only identity; the watcher still owns derived content/backlinks.
    pub fn register_created_note(&mut self, path: &str) {
        self.set_note_identity(&note_path(Path::new(path)), true);
    }

    /// Find a readable document without collecting or sorting the inventory.
    pub fn discover_document(
        root: &Path,
        checkpoint: &mut impl FnMut() -> Result<()>,
    ) -> Result<Option<String>> {
        for entry in WalkDir::new(root).into_iter().filter_entry(|e| {
            e.depth() == 0 || !service_path(e.path().strip_prefix(root).unwrap_or(e.path()))
        }) {
            checkpoint()?;
            let Ok(entry) = entry else {
                continue;
            };
            if entry.file_type().is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("md"))
                && read_source(entry.path()).is_ok()
            {
                return Ok(Some(note_path(entry.path().strip_prefix(root)?)));
            }
        }
        Ok(None)
    }

    /// The first note in scan order, or `None` for an empty vault.
    ///
    /// Used when the shell is started without `--note`: there is no
    /// vault-specific default note to fall back on, and an empty window is a
    /// worse first impression than an arbitrary real one.
    pub fn first_note(&self) -> Option<String> {
        self.notes.first().map(|n| n.path.clone())
    }

    pub fn read_note(&self, rel: &str) -> Result<String> {
        anyhow::ensure!(!service_path(Path::new(rel)), "Service files are not notes");
        read_source(&self.root.join(rel)).with_context(|| format!("read {rel}"))
    }

    /// Resolve a wikilink target ("Note", "Dir/Note", "Note#Heading", with any
    /// "|alias" already split off) against the vault.
    ///
    /// Identity is the path from the vault root, and a link is a suffix of one:
    /// it resolves exactly when it names one note and no other. Where several
    /// notes answer, the answer is [`Resolution::Ambiguous`] and the caller
    /// decides — this never picks a winner on the reader's behalf.
    pub fn resolve(&self, target: &str) -> Resolution {
        self.resolve_from(target, "")
    }

    /// Resolve `target` as written inside `from_note` (a rel path; pass `""`
    /// when there is no source note).
    ///
    /// Only relative targets — those starting with `./` or `../` — use the
    /// source note, and for them it is not a hint but the whole meaning:
    /// `[[../ops/signoff]]` names a sibling directory of the *linking* note and
    /// says nothing on its own. The vault has hundreds of these. Resolving them
    /// by suffix instead would answer with whichever unrelated note happened to
    /// end in `ops/signoff`.
    pub fn resolve_from(&self, target: &str, from_note: &str) -> Resolution {
        match self.resolve_from_ref(target, from_note) {
            ResolutionRef::Resolved(path) => Resolution::Resolved {
                path: path.to_owned(),
            },
            ResolutionRef::Ambiguous(candidates) => Resolution::Ambiguous {
                candidates: candidates.to_vec(),
            },
            ResolutionRef::Unresolved => Resolution::Unresolved,
        }
    }

    /// Same identity rules as `resolve_from`, with borrowed candidate storage.
    pub fn resolve_from_ref(&self, target: &str, from_note: &str) -> ResolutionRef<'_> {
        let t = target.split(['#', '^']).next().unwrap_or("").trim();
        let key = t.to_lowercase();
        let key = key.trim_end_matches(".md").trim_matches('/');
        if key.is_empty() {
            return ResolutionRef::Unresolved;
        }
        if key.starts_with("./") || key.starts_with("../") {
            return self
                .walk_relative(key, from_note)
                .and_then(|abs| self.path_map.get(&abs))
                .map_or(ResolutionRef::Unresolved, |p| ResolutionRef::Resolved(p));
        }
        if let Some(path) = self.path_map.get(key) {
            return ResolutionRef::Resolved(path);
        }
        match self.suffix_map.get(key) {
            None => ResolutionRef::Unresolved,
            Some(c) if c.len() == 1 => ResolutionRef::Resolved(&c[0]),
            Some(c) => ResolutionRef::Ambiguous(c),
        }
    }

    /// Ordinary Markdown is relative-first; existing but unreadable/invalid
    /// local entries must never redirect to a root or suffix namesake.
    pub fn resolve_markdown(&self, path: &str, from: &str) -> Resolution {
        let Some(path) = crate::document_links::markdown_path(self, path) else {
            return Resolution::Unresolved;
        };
        let path = path.as_ref();
        let key = path.to_lowercase();
        let key = key.strip_suffix(".md").unwrap_or(&key);
        let exact = |key: &str| {
            // The wiki index strips repeated .md suffixes. Narrow to that
            // bucket, then retain exactly one filename extension for Markdown.
            let mut candidates: Vec<_> = self
                .suffix_map
                .get(key.trim_end_matches(".md"))
                .into_iter()
                .flatten()
                .filter(|path| path.to_lowercase().strip_suffix(".md") == Some(key))
                .cloned()
                .collect();
            candidates.sort();
            match candidates.len() {
                0 => Resolution::Unresolved,
                1 => Resolution::Resolved {
                    path: candidates.remove(0),
                },
                _ => Resolution::Ambiguous { candidates },
            }
        };
        if self.single_file {
            let candidate = if path.starts_with('/') {
                self.root.join(path.trim_start_matches('/'))
            } else {
                self.root
                    .join(Path::new(from).parent().unwrap_or(Path::new("")))
                    .join(path)
            };
            if candidate
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
            {
                if let Ok(canonical) = candidate.canonicalize() {
                    if canonical.is_file() && canonical.starts_with(&self.root) {
                        return Resolution::Resolved {
                            path: note_path(canonical.strip_prefix(&self.root).unwrap()),
                        };
                    }
                }
            }
        }
        if path.starts_with('/') {
            return self
                .walk_relative(key.trim_start_matches('/'), "")
                .map_or(Resolution::Unresolved, |key| exact(&key));
        }
        let Some(relative) = self.walk_relative(key, from) else {
            return Resolution::Unresolved;
        };
        let local = exact(&relative);
        if local != Resolution::Unresolved {
            return local;
        }
        // A provisional snapshot cannot establish lexical absence. Do not
        // probe cloud-backed paths before the first document is published, or
        // redirect to a root/suffix namesake while local occupancy is unknown.
        if !self.inventory_complete {
            return Resolution::Unresolved;
        }
        // The complete graph uses metadata captured by enumeration, including
        // dangling links and non-directory ancestors, rather than probing per link.
        let local_occupied = if self.graph_root.is_some() {
            self.indexed_occupied(&format!("{relative}.md"))
        } else {
            let candidate = self
                .root
                .join(Path::new(from).parent().unwrap_or(Path::new("")))
                .join(path);
            !matches!(std::fs::symlink_metadata(candidate), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
        };
        if local_occupied {
            return Resolution::Unresolved;
        }
        if path.starts_with("./") || path.starts_with("../") {
            return Resolution::Unresolved;
        }
        let root = exact(key);
        if root != Resolution::Unresolved {
            return root;
        }
        let root_occupied = if self.graph_root.is_some() {
            self.indexed_occupied(&format!("{key}.md"))
        } else {
            !matches!(std::fs::symlink_metadata(self.root.join(path)), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
        };
        if root_occupied {
            return Resolution::Unresolved;
        }
        let suffix = format!("/{key}");
        let candidates: Vec<_> = self
            .suffix_map
            .get(key.trim_end_matches(".md"))
            .into_iter()
            .flatten()
            .filter(|path| {
                path.to_lowercase()
                    .strip_suffix(".md")
                    .is_some_and(|p| p == key || p.ends_with(&suffix))
            })
            .cloned()
            .collect();
        match candidates.len() {
            0 => Resolution::Unresolved,
            1 => Resolution::Resolved {
                path: candidates[0].clone(),
            },
            _ => Resolution::Ambiguous { candidates },
        }
    }

    pub(crate) fn graph_alias_path(&self, path: &Path) -> bool {
        path.components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
            || path
                .ancestors()
                .any(|part| self.symlink_paths.contains(&note_path(part).to_lowercase()))
    }

    fn indexed_occupied(&self, path: &str) -> bool {
        self.occupied_paths.contains(path)
            || Path::new(path)
                .ancestors()
                .skip(1)
                .any(|parent| self.non_directory_paths.contains(&note_path(parent)))
    }

    /// Apply a `./` `../` target to the directory of `from_note`, yielding a
    /// lowercased vault-relative key, or `None` if it walks above the root.
    fn walk_relative(&self, key: &str, from_note: &str) -> Option<String> {
        let from = from_note.to_lowercase();
        let mut parts: Vec<&str> = from
            .trim_end_matches(".md")
            .split('/')
            .filter(|p| !p.is_empty())
            .collect();
        parts.pop(); // the note itself; links are relative to its directory
        for seg in key.split('/') {
            match seg {
                "" | "." => {}
                ".." => {
                    // A link that climbs past the vault root names nothing here.
                    parts.pop()?;
                }
                other => parts.push(other),
            }
        }
        if parts.is_empty() {
            return None;
        }
        Some(parts.join("/"))
    }

    /// Every wikilink target written in a note, in document order, unresolved.
    ///
    /// Exposed so the index records what the resolver decided rather than
    /// re-deriving link resolution with a second regex that would be free to
    /// disagree with this one.
    ///
    /// A `[[link]]` inside inline code, a fence or an indented block is an
    /// example, not a link, and is not reported (#20).
    pub fn outbound_links(&self, rel: &str) -> Vec<String> {
        let Ok(text) = self.read_note(rel) else {
            return Vec::new();
        };
        Self::outbound_links_in(&text)
    }

    /// Extract from a source snapshot without reopening its canonical file.
    pub fn outbound_links_in(text: &str) -> Vec<String> {
        let re = wikilink_re();
        prose_spans(text)
            .into_iter()
            .flat_map(|span| {
                re.captures_iter(&text[span])
                    .map(|c| c[1].trim().to_string())
                    .collect::<Vec<_>>()
            })
            .filter(|t| !t.is_empty())
            .collect()
    }

    /// Resolve an image/asset reference (wikilink embed or relative md path) to an absolute path.
    pub fn resolve_asset(&self, target: &str, note_rel: &str) -> Option<PathBuf> {
        // Prefer a real filename, including literal #/% characters, before
        // treating a suffix as Obsidian fragment syntax.
        let target = target.trim();
        self.resolve_asset_path(target, note_rel).or_else(|| {
            let path = target.split(['#', '^']).next().unwrap_or("").trim();
            (path != target)
                .then(|| self.resolve_asset_path(path, note_rel))
                .flatten()
        })
    }

    fn resolve_asset_path(&self, t: &str, note_rel: &str) -> Option<PathBuf> {
        if t.is_empty() {
            return None;
        }
        if !self.inventory_complete && !self.single_file {
            let find = |key: &str| {
                self.entries
                    .iter()
                    .find(|entry| {
                        entry.kind == EntryKind::Attachment && entry.path.eq_ignore_ascii_case(key)
                    })
                    .map(|entry| self.root.join(&entry.path))
            };
            let path = Path::new(t);
            if path.is_absolute() {
                return find(&note_path(path.strip_prefix(&self.root).ok()?));
            }
            let local = self.walk_relative(&t.to_lowercase(), note_rel)?;
            if let Some(path) = find(&local) {
                return Some(path);
            }
            if t.starts_with("./") || t.starts_with("../") {
                return None;
            }
            find(t).or_else(|| {
                self.asset_map
                    .get(&t.rsplit('/').next()?.to_lowercase())
                    .cloned()
            })
        } else {
            // relative to note dir
            if let Some(dir) = Path::new(note_rel).parent() {
                let cand = self.root.join(dir).join(t);
                if cand.is_file() {
                    return Some(cand);
                }
            }
            // relative to root
            let cand = self.root.join(t);
            if cand.is_file() {
                return Some(cand);
            }
            // bare filename lookup
            let name = t.rsplit('/').next().unwrap_or(t).to_lowercase();
            self.asset_map.get(&name).cloned()
        }
    }

    /// Stable identity of the metadata used by document-link resolution.
    /// Include occupied paths omitted from visible entries (symlinks, special
    /// files and non-directory ancestors), since they can block fallback.
    /// This reads only the captured inventory, never canonical file contents.
    pub fn link_inventory_revision(&self) -> String {
        use sha2::{Digest, Sha256};
        let sorted = |paths: &HashSet<String>| {
            let mut paths: Vec<_> = paths.iter().cloned().collect();
            paths.sort();
            paths
        };
        let bytes = serde_json::to_vec(&(
            &self.entries,
            &self.unreadable,
            self.inventory_complete,
            sorted(&self.occupied_paths),
            sorted(&self.non_directory_paths),
            sorted(&self.symlink_paths),
        ))
        .expect("serializable link inventory");
        format!("{:x}", Sha256::digest(bytes))
    }

    /// Backlinks to `rel`, grouped by source note (path order) and in
    /// source-line order within each note. Uncapped (#25).
    pub fn backlinks(&self, rel: &str) -> Vec<Backlink> {
        self.backlink_map.get(rel).cloned().unwrap_or_default()
    }

    pub fn note_title(&self, rel: &str) -> String {
        Self::title_of(rel)
    }

    /// The display title of a note at `rel`: its file stem.
    pub fn title_of(rel: &str) -> String {
        Path::new(rel)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    }

    /// Incoming references retain wiki/property extraction and use the shared
    /// document parser/resolver for Markdown links, including Obsidian paths.
    /// Code examples are excluded from both grammars.
    fn build_backlinks_from(
        &mut self,
        checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
        mut source: impl FnMut(&str) -> Option<String>,
    ) -> Result<()> {
        self.build_backlinks_selected(None, checkpoint, &mut source)
    }

    fn build_backlinks_selected(
        &mut self,
        selected: Option<&std::collections::BTreeSet<String>>,
        checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
        mut source: impl FnMut(&str) -> Option<String>,
    ) -> Result<()> {
        // Resolve graph identities against one metadata inventory and canonical
        // root. Full UI/file actions retain their explicit filesystem checks.
        let mut resolver = self.clone();
        resolver.graph_root = Some(canonical_root(&self.root)?);
        resolver.graph_os_paths = Default::default();
        let link_re = wikilink_re();
        let mut map: HashMap<String, Vec<Backlink>> = HashMap::new();
        for (count, note) in self
            .notes
            .iter()
            .filter(|note| selected.is_none_or(|paths| paths.contains(&note.path)))
            .enumerate()
        {
            checkpoint("Preparing backlinks", count)?;
            let Some(text) = source(&note.path) else {
                continue;
            };
            let frontmatter_len = text.len() - crate::render::without_frontmatter(&text).len();
            let mut occurrences = Vec::new();
            for span in prose_spans(&text) {
                for cap in link_re.captures_iter(&text[span.clone()]) {
                    occurrences.push((
                        span.start + cap.get(0).unwrap().start(),
                        cap[1].trim().to_owned(),
                        true,
                    ));
                }
            }
            // Inline destinations or reference definitions are necessary for
            // local Markdown links. Wiki-only notes keep the cheap graph path.
            if text.contains("](") || text.contains("]:") {
                occurrences.extend(
                    crate::document_links::parse_in_vault(&text, &resolver, &note.path)
                        .into_iter()
                        .filter(|link| !link.wiki)
                        .map(|link| (link.range.start, link.target, false)),
                );
            }
            occurrences.sort_by_key(|(at, _, _)| *at);
            for (at, target, wiki) in occurrences {
                let line = line_around(&text, at);
                // An ambiguous link is recorded against EVERY candidate,
                // marked. The alternatives are both dishonest: giving it to
                // one candidate is the "duplicate stems steal backlinks"
                // bug, and dropping it hides a link that really was written.
                let targets: Vec<(String, bool)> = match if wiki {
                    self.resolve_from(&target, &note.path)
                } else {
                    let resolved =
                        crate::document_links::resolve(&target, false, &resolver, &note.path);
                    match resolved.status {
                        "resolved" => Resolution::Resolved {
                            path: resolved.candidates[0].clone(),
                        },
                        "ambiguous" => Resolution::Ambiguous {
                            candidates: resolved.candidates,
                        },
                        _ => Resolution::Unresolved,
                    }
                } {
                    Resolution::Resolved { path } => vec![(path, false)],
                    Resolution::Ambiguous { candidates } => {
                        candidates.into_iter().map(|p| (p, true)).collect()
                    }
                    Resolution::Unresolved => Vec::new(),
                };
                for (resolved, ambiguous) in targets {
                    if resolved == note.path {
                        continue;
                    }
                    let entry = map.entry(resolved).or_default();
                    let ctx: String = line.trim().chars().take(240).collect();
                    // Entries for one source note are contiguous at the
                    // tail (notes are walked in order), so the duplicate
                    // check only has to look at that tail. Scanning the
                    // whole list was what made the old 50-cap look like a
                    // performance necessity on hub notes.
                    let existing = entry
                        .iter()
                        .rev()
                        .take_while(|b| b.path == note.path)
                        .position(|b| b.context == ctx)
                        .map(|k| entry.len() - 1 - k);
                    match existing {
                        // One backlink per (source note, line). If the same
                        // line also names this note unambiguously, that
                        // settles it: keep the entry and drop the warning,
                        // or the reader is told a link is uncertain when a
                        // precise one sits right beside it.
                        Some(i) => entry[i].ambiguous &= ambiguous,
                        // No per-target cap (#25). The old `entry.len() < 50`
                        // guard silently made `Backlinks (50)` read as the
                        // truth on hub notes; measured on an 8k-note vault,
                        // dropping it did not move `Vault::scan` (see the
                        // PR for numbers), so there is nothing to surface.
                        None => entry.push(Backlink {
                            path: note.path.clone(),
                            title: note.title.clone(),
                            context: ctx,
                            link: target.to_string(),
                            ambiguous,
                            property: (at < frontmatter_len)
                                .then(|| property_key(&text[..at]))
                                .flatten(),
                        }),
                    }
                }
            }
        }
        // Grouped by source note, in source-line order within a note (#22).
        // `notes` is walked in path order and lines in file order, so the
        // push order already is that; the sort makes it a contract rather
        // than an accident of the walk.
        for links in map.values_mut() {
            links.sort_by(|a, b| a.path.cmp(&b.path));
        }
        self.backlink_map = map;
        Ok(())
    }
}

/// The wikilink shape the link graph and the index scan for: target, optional
/// `#heading` / `^block`, optional `|alias`.
fn wikilink_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\[\[([^\]\[|#^]+)(?:[#^][^\]\[|]*)?(?:\|[^\]\[]*)?\]\]").unwrap()
    })
}

/// The frontmatter key owning the link that starts after `before`: the last
/// unindented `key:` line at or above it (list items sit under their key).
fn property_key(before: &str) -> Option<String> {
    before.lines().rev().find_map(|line| {
        // Indented items belong to the key above; comments name nothing.
        if line.starts_with([' ', '\t', '-', '#']) || line.starts_with("---") {
            return None;
        }
        let (key, _) = line.split_once(':')?;
        let key = key
            .trim()
            .trim_start_matches('\u{feff}')
            .trim_matches(['"', '\'']);
        (!key.is_empty()).then(|| key.to_owned())
    })
}

/// The full source line containing byte offset `at`, without its newline.
fn line_around(text: &str, at: usize) -> &str {
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let end = text[at..].find('\n').map_or(text.len(), |i| at + i);
    text[start..end].trim_end_matches('\r')
}

#[cfg(test)]
mod context_link_tests {
    use super::Backlink;

    fn bl(context: &str, link: &str) -> Backlink {
        Backlink {
            path: "src.md".into(),
            title: "src".into(),
            context: context.into(),
            link: link.into(),
            ambiguous: false,
            property: None,
        }
    }

    fn check(context: &str, link: &str, plain: &str, shown: Option<&str>) {
        let (text, range) = bl(context, link).context_plain_with_link();
        assert_eq!(text, plain, "context {context:?}");
        assert_eq!(
            range.clone().map(|r| &text[r]),
            shown,
            "context {context:?} -> {text:?} {range:?}"
        );
    }

    #[test]
    fn link_at_start_middle_and_end() {
        check(
            "[[target]] opens the line",
            "target",
            "target opens the line",
            Some("target"),
        );
        check(
            "see [[target]] here",
            "target",
            "see target here",
            Some("target"),
        );
        check(
            "ends with [[target]]",
            "target",
            "ends with target",
            Some("target"),
        );
    }

    #[test]
    fn alias_range_is_the_alias() {
        check(
            "see [[target|the alias]] here",
            "target",
            "see the alias here",
            Some("the alias"),
        );
    }

    #[test]
    fn heading_and_block_refs_are_the_same_target() {
        check(
            "see [[target#Heading]] here",
            "target",
            "see target here",
            Some("target"),
        );
        check(
            "see [[target^blk|T]] here",
            "target",
            "see T here",
            Some("T"),
        );
    }

    #[test]
    fn link_inside_bold_maps_past_the_stripped_markers() {
        check(
            "**bold [[target]] text** tail",
            "target",
            "bold target text tail",
            Some("target"),
        );
        check("> - **also** [[target|T]]", "target", "also T", Some("T"));
    }

    #[test]
    fn only_the_matching_link_is_marked() {
        check(
            "[[other]] and then [[target]]",
            "target",
            "other and then target",
            Some("target"),
        );
        // Two hits: the first one is the range.
        check(
            "[[target]] / [[target|again]]",
            "target",
            "target / again",
            Some("target"),
        );
    }

    #[test]
    fn no_matching_link_is_none() {
        check(
            "plain prose without a link",
            "target",
            "plain prose without a link",
            None,
        );
        check("[[other]] only", "target", "other only", None);
        check(
            "[[targeted]] is not target",
            "target",
            "targeted is not target",
            None,
        );
    }

    #[test]
    fn link_inside_markdown_link_text_is_still_found() {
        check(
            "[go [[target]]](https://x)",
            "target",
            "go target",
            Some("target"),
        );
    }

    #[test]
    fn plain_and_with_link_agree() {
        let b = bl("- *x* `[[target]]` and [[target|T]]", "target");
        let (text, range) = b.context_plain_with_link();
        assert_eq!(text, b.context_plain());
        // The one in the code span is verbatim, not a link.
        assert_eq!(range.map(|r| &text[r]), Some("T"));
    }
}

#[cfg(test)]
mod browser_inventory_tests {
    use super::*;

    #[test]
    fn unreadable_source_does_not_discard_readable_notes_or_backlinks() {
        let root = std::env::temp_dir().join(format!("tessera-partial-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        for (name, source) in [
            ("blocked.md", "private"),
            ("good.md", "[[target]]"),
            ("target.md", "target"),
        ] {
            std::fs::write(root.join(name), source).unwrap();
        }
        let (vault, sources) =
            Vault::scan_snapshot_read_with(&root, &mut |_, _| Ok(()), &mut |path| {
                if path.ends_with("blocked.md") {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "Access is denied",
                    ))
                } else {
                    std::fs::read(path)
                }
            })
            .unwrap();
        assert!(vault.inventory_scanned);
        assert!(!vault.inventory_complete);
        assert_eq!(sources.len(), 2);
        assert_eq!(sources["good.md"], b"[[target]]");
        assert!(!sources.contains_key("blocked.md"));
        assert_eq!(vault.unreadable.len(), 1);
        assert_eq!(vault.unreadable[0].path, root.join("blocked.md"));
        assert_eq!(vault.unreadable[0].operation, "read note");
        assert!(vault.unreadable[0].error.contains("Access is denied"));
        assert_eq!(vault.backlinks("target.md")[0].path, "good.md");
        // Keep known identities: an unreadable duplicate must not turn into a
        // different apparently unique link target.
        assert_eq!(vault.resolve("blocked").path(), Some("blocked.md"));
        assert_eq!(std::fs::read(root.join("blocked.md")).unwrap(), b"private");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn enumeration_error_keeps_its_path_and_continues_to_healthy_entries() {
        let root =
            std::env::temp_dir().join(format!("tessera-enumeration-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("Readable")).unwrap();
        std::fs::write(root.join("Readable/note.md"), "readable").unwrap();
        let missing = root.join("Unavailable");
        let failure = WalkDir::new(&missing).into_iter().next().unwrap();
        assert!(
            failure.is_err(),
            "The error-injection positive control must fire"
        );
        let entries = std::iter::once(failure).chain(WalkDir::new(&root));
        let vault = Vault::scan_metadata_entries(&root, &mut |_, _| Ok(()), entries).unwrap();
        assert!(vault.inventory_scanned);
        assert!(!vault.inventory_complete);
        assert_eq!(vault.notes.len(), 1);
        assert_eq!(vault.notes[0].path, "Readable/note.md");
        assert_eq!(vault.unreadable[0].path, missing);
        assert_eq!(vault.unreadable[0].operation, "enumerate entry");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn root_errors_and_cancellation_remain_fatal_and_pathful() {
        let root = std::env::temp_dir().join(format!("tessera-root-{}", uuid::Uuid::new_v4()));
        let error = Vault::scan(&root).err().expect("Missing root must fail");
        assert!(error.to_string().contains(&root.display().to_string()));
        std::fs::create_dir_all(&root).unwrap();
        let error = Vault::scan_snapshot_with(&root, &mut |phase, _| {
            if phase == "Reading notes" {
                anyhow::bail!("cancelled");
            }
            Ok(())
        });
        assert!(error.is_ok(), "Empty positive control completes");
        std::fs::write(root.join("note.md"), "body").unwrap();
        assert!(Vault::scan_snapshot_with(&root, &mut |phase, _| {
            if phase == "Reading notes" {
                anyhow::bail!("cancelled");
            }
            Ok(())
        })
        .is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selected_hidden_root_is_read_but_nested_service_directories_are_excluded() {
        let temp = tempfile::tempdir().unwrap();
        for name in [".vault", ".git"] {
            let root = temp.path().join(name);
            std::fs::create_dir_all(root.join(".git")).unwrap();
            std::fs::create_dir_all(root.join("node_modules")).unwrap();
            std::fs::write(root.join("note.md"), "# Visible").unwrap();
            std::fs::write(root.join("diagram.svg"), "fixture").unwrap();
            std::fs::write(root.join(".git/ignored.md"), "hidden service data").unwrap();
            std::fs::write(root.join("node_modules/ignored.md"), "generated").unwrap();
            let vault = Vault::scan(&root).unwrap();
            assert_eq!(vault.notes.len(), 1);
            assert_eq!(vault.notes[0].path, "note.md");
            assert!(vault
                .entries
                .iter()
                .any(|entry| entry.path == "diagram.svg"));
            assert!(!vault
                .entries
                .iter()
                .any(|entry| entry.path.starts_with(".git")
                    || entry.path.starts_with("node_modules")));
            assert_eq!(
                Vault::discover_document(&root, &mut || Ok(())).unwrap(),
                Some("note.md".into())
            );
        }
    }

    #[test]
    fn metadata_preserves_empty_folders_duplicates_and_scan_boundaries() {
        let root = std::env::temp_dir().join(format!("tessera-tree-{}", uuid::Uuid::new_v4()));
        for dir in [
            "Dev/Areas/Empty",
            "Projects/Unicode З",
            "Arbitrary",
            ".hidden",
            "node_modules",
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for file in [
            "Dev/Areas/duplicate.md",
            "Projects/Unicode З/duplicate.md",
            "Dev/Areas/image.png",
            "Arbitrary/image.png",
            "Arbitrary/manual.pdf",
            ".hidden/secret.md",
            "node_modules/generated.md",
            "Arbitrary/ignored.txt",
            "Arbitrary/photo.heic",
            "Arbitrary/slides.pptx",
            "Arbitrary/archive.zip",
            "Arbitrary/LICENSE",
        ] {
            std::fs::write(root.join(file), b"# fixture").unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("Arbitrary"), root.join("shortcut")).unwrap();
        let v = Vault::scan_metadata(&root).unwrap();
        let entries: HashMap<_, _> = v
            .entries
            .iter()
            .map(|e| (e.path.as_str(), e.kind))
            .collect();
        assert_eq!(entries["Dev/Areas/Empty"], EntryKind::Directory);
        assert_eq!(entries["Dev/Areas/image.png"], EntryKind::Attachment);
        assert_eq!(entries["Arbitrary/image.png"], EntryKind::Attachment);
        assert_eq!(entries["Arbitrary/manual.pdf"], EntryKind::Attachment);
        assert_eq!(v.notes.len(), 3);
        for file in ["photo.heic", "slides.pptx", "archive.zip", "LICENSE"] {
            assert_eq!(
                entries[format!("Arbitrary/{file}").as_str()],
                EntryKind::Attachment
            );
        }
        assert_eq!(entries["Arbitrary/ignored.txt"], EntryKind::Attachment);
        assert_eq!(entries[".hidden/secret.md"], EntryKind::Markdown);
        assert!(!entries
            .keys()
            .any(|p| p.starts_with("node_modules") || p.starts_with("shortcut")));
        assert!(matches!(
            v.resolve("duplicate"),
            Resolution::Ambiguous { .. }
        ));
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod property_backlink_tests {
    use super::*;

    #[test]
    fn frontmatter_links_carry_their_key_and_body_links_do_not() {
        let root = std::env::temp_dir().join(format!("tessera-386-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("a.md"),
            "---\ntype: note\nrelated:\n# TODO: verify\n  - \"[[b]]\"\n\"project\": \"[[c]]\"\n---\nBody mentions [[b]].\n",
        )
        .unwrap();
        std::fs::write(root.join("b.md"), "# B\n").unwrap();
        std::fs::write(root.join("c.md"), "# C\n").unwrap();
        let vault = Vault::scan(&root).unwrap();
        let to_b: Vec<Option<String>> = vault
            .backlinks("b.md")
            .into_iter()
            .map(|b| b.property)
            .collect();
        assert_eq!(to_b, [Some("related".to_string()), None]);
        let to_c: Vec<Option<String>> = vault
            .backlinks("c.md")
            .into_iter()
            .map(|b| b.property)
            .collect();
        assert_eq!(to_c, [Some("project".to_string())]);
        let _ = std::fs::remove_dir_all(root);
    }
}

#[cfg(test)]
mod preimage_visibility_tests {
    use super::*;
    use std::fs;
    #[test]
    fn windows_history_preimages_are_service_files_on_every_platform() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir(root.join("nested")).unwrap();
        let filename = format!(".tessera-save-{}.previous", uuid::Uuid::new_v4());
        let hidden = root.join("nested").join(&filename);
        fs::write(&hidden, "source preimage").unwrap();
        fs::write(root.join("nested/Visible.md"), "# positive control").unwrap();
        let service_names = [
            ".tessera-save-user.previous",
            ".tessera-save-icf3uR",
            ".tessera-save-legacy.md",
            ".tessera-save-proposed.prepared",
            ".tessera-save-conflict.raced",
        ];
        for name in service_names {
            fs::write(root.join("nested").join(name), "protected recovery bytes").unwrap();
            assert!(service_path(Path::new(&format!("nested/{name}"))));
        }
        fs::write(
            root.join("nested/.ordinary.md"),
            "# ordinary hidden control",
        )
        .unwrap();
        assert!(service_path(Path::new(&format!("nested/{filename}"))));
        assert!(!windows_preimage_name(std::ffi::OsStr::new(
            ".tessera-save-user.previous"
        )));
        assert!(!service_path(Path::new("nested/.ordinary.md")));
        assert!(!windows_preimage_name(std::ffi::OsStr::new(
            &filename.replace('-', "")
        )));
        let vault = Vault::scan(root).unwrap();
        assert!(vault.notes.iter().any(|n| n.path == "nested/Visible.md"));
        assert!(vault
            .entries
            .iter()
            .any(|e| e.path == "nested/.ordinary.md"));
        assert!(vault
            .entries
            .iter()
            .all(|entry| !service_path(Path::new(&entry.path))));
        assert!(vault
            .notes
            .iter()
            .all(|note| !service_path(Path::new(&note.path))));
        for name in service_names {
            assert!(
                fs::read(root.join("nested").join(name)).is_ok(),
                "inventory preserves recovery bytes"
            );
        }
        assert!(!vault.entries.iter().any(|e| e.path.ends_with(&filename)));
        assert!(
            fs::read(&hidden).is_ok(),
            "inventory never deletes recovery"
        );
        // Older cached inventories may still contain this attachment. Quick Open
        // must suppress it before offering file-name matches as well.
        let mut cached = vec![
            VaultEntry {
                path: format!("nested/{filename}"),
                kind: EntryKind::Attachment,
            },
            VaultEntry {
                path: "nested/.ordinary.md".into(),
                kind: EntryKind::Markdown,
            },
        ];
        cached.extend(service_names.into_iter().map(|name| VaultEntry {
            path: format!("nested/{name}"),
            kind: EntryKind::Attachment,
        }));
        let palette = crate::quick_open::inventory(vault.notes.clone(), &cached);
        assert!(palette.iter().any(|n| n.path == "nested/Visible.md"));
        assert!(palette.iter().any(|n| n.path == "nested/.ordinary.md"));
        assert!(palette
            .iter()
            .all(|note| !service_path(Path::new(&note.path))));
        assert!(!palette.iter().any(|n| n.path.ends_with(&filename)));
    }
}
