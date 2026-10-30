//! Root-specific last-document history for document-first opening (#338).
//! App-owned state, separate from notes and disposable indexes.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

/// An absolute `OKILUM_STATE_DIR` gives a QA instance its own state, instance
/// lock, drafts, diagnostics and presentation config, so it neither forwards to
/// nor shares files with the user's running Reader. Windows has no XDG override.
pub(crate) fn isolated_state_directory() -> Option<PathBuf> {
    isolated_from(std::env::var_os("OKILUM_STATE_DIR"))
}

fn isolated_from(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|path| path.is_absolute())
}

/// App-owned Reader state location, outside any vault or index directory.
/// Tests use `TestSessionDirectory` instead.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn state_directory() -> Result<PathBuf> {
    if let Some(directory) = isolated_state_directory() {
        return Ok(directory);
    }
    #[cfg(target_os = "macos")]
    let directory = std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support/com.befeast.okilum"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let directory = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|base| base.join("okilum"))
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state/okilum"))
        });
    #[cfg(windows)]
    let directory = dirs::data_local_dir().map(|base| base.join("okilum"));
    directory
        .filter(|p| p.is_absolute())
        .context("No absolute Reader state directory is available")
}

/// Tests point Readers at a temporary state directory instead of the user's.
#[cfg(test)]
pub(crate) struct TestSessionDirectory(pub PathBuf);
#[cfg(test)]
impl gpui::Global for TestSessionDirectory {}

const SCHEMA: u32 = 1;
const MAX_BYTES: u64 = 1024 * 1024;
// File name kept from earlier builds so existing history survives an update.
const FILE: &str = "update-session.json";

#[derive(Debug)]
struct InvalidHistory;

/// Another writer of this process holds the history lock for a moment;
/// the caller retries rather than reporting it (#1325).
#[derive(Debug)]
pub(crate) struct Busy;
impl std::fmt::Display for Busy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Reader state is busy; retry without losing the existing history")
    }
}
impl std::error::Error for Busy {}
impl std::fmt::Display for InvalidHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Reading history is corrupt or unsupported")
    }
}
impl std::error::Error for InvalidHistory {}

fn validate_document(root: &Path, document: &str) -> Result<()> {
    if !root.is_absolute() {
        bail!("Reader root must be absolute");
    }
    if !Path::new(document)
        .components()
        .all(|p| matches!(p, Component::Normal(_)))
    {
        bail!("Reader document must be a relative path without traversal");
    }
    Ok(())
}

/// Older builds stored extra restart fields in the same file; serde ignores them.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct ReadingHistory {
    schema: u32,
    #[serde(default)]
    last_documents: BTreeMap<PathBuf, String>,
    #[serde(default)]
    quick_roots: std::collections::BTreeSet<PathBuf>,
    #[serde(default)]
    recent_roots: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    hidden_roots: std::collections::BTreeSet<PathBuf>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    last_opened: BTreeMap<PathBuf, u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_root: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KnownVault {
    pub root: PathBuf,
    pub last_opened: Option<u64>,
    pub is_default: bool,
}

// Closing a descriptor alone does not release a Unix flock if a concurrent
// fork inherited its open file description. End the writer's critical section
// explicitly so a child waiting to exec cannot keep later history writes busy.
struct HistoryLock(fs::File);
impl Drop for HistoryLock {
    fn drop(&mut self) {
        if let Err(error) = self.0.unlock() {
            eprintln!("Cannot release Reader history lock: {error}");
        }
    }
}

impl ReadingHistory {
    fn lock<'a>(directory: &Path, roots: impl Iterator<Item = &'a Path>) -> Result<HistoryLock> {
        let mut roots = roots.map(Path::to_path_buf).collect::<Vec<_>>();
        match Self::load(directory) {
            Ok(Some(previous)) => roots.extend(previous.last_documents.into_keys()),
            Ok(None) => {}
            Err(error) if error.is::<InvalidHistory>() => {}
            Err(error) => return Err(error),
        }
        Self::outside_notes(directory, roots.iter().map(PathBuf::as_path))?;
        fs::create_dir_all(directory)?;
        let path = directory.join("reader-session.lock");
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            if !metadata.is_file() {
                bail!("Invalid Reader state lock");
            }
        }
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(path)?;
        lock.try_lock().map_err(|_| Busy)?;
        Ok(HistoryLock(lock))
    }
    fn outside_notes<'a>(directory: &Path, roots: impl Iterator<Item = &'a Path>) -> Result<()> {
        let ancestor = directory
            .ancestors()
            .find(|p| p.exists())
            .context("State path has no existing ancestor")?;
        let resolved = ancestor
            .canonicalize()?
            .join(directory.strip_prefix(ancestor)?);
        for root in roots {
            let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
            if directory.starts_with(root) || resolved.starts_with(canonical) {
                bail!("Reader state must be outside canonical notes");
            }
        }
        Ok(())
    }
    fn persist(&self, directory: &Path) -> Result<()> {
        Self::outside_notes(directory, self.last_documents.keys().map(PathBuf::as_path))?;
        fs::create_dir_all(directory)?;
        let destination = directory.join(FILE);
        let temp = directory.join(format!(".reader-history-{}", uuid::Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut opts = fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut file = opts.open(&temp)?;
            let bytes = serde_json::to_vec(self)?;
            if bytes.len() as u64 > MAX_BYTES {
                bail!("Reading history is too large");
            }
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temp, &destination)?;
            #[cfg(unix)]
            fs::File::open(directory)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result.context("Cannot save reading history")
    }
    fn load(directory: &Path) -> Result<Option<Self>> {
        let path = directory.join(FILE);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if !metadata.is_file() {
            bail!("Invalid reading history file type");
        }
        if metadata.len() > MAX_BYTES {
            return Err(InvalidHistory.into());
        }
        let mut history: Self =
            serde_json::from_slice(&fs::read(path)?).map_err(|_| InvalidHistory)?;
        if history.schema != SCHEMA {
            return Err(InvalidHistory.into());
        }
        for (root, document) in &history.last_documents {
            validate_document(root, document).map_err(|_| InvalidHistory)?;
        }
        // Recency is a hint. A stray row must not invalidate otherwise usable
        // per-root documents or prevent opening a folder through the picker.
        history
            .recent_roots
            .retain(|root| history.last_documents.contains_key(root));
        history
            .hidden_roots
            .retain(|root| history.last_documents.contains_key(root));
        history
            .last_opened
            .retain(|root, _| history.last_documents.contains_key(root));
        if history.default_root.as_ref().is_some_and(|root| {
            !history.last_documents.contains_key(root)
                || history.hidden_roots.contains(root)
                || history.quick_roots.contains(root)
        }) {
            history.default_root = None;
        }
        Ok(Some(history))
    }
    /// #338 supplies a validated, already published usable document, not a pending
    /// request or full-index completion. Callers surface I/O failures explicitly.
    #[cfg(test)]
    pub fn record_usable_document(directory: &Path, root: &Path, document: &str) -> Result<()> {
        validate_document(root, document)?;
        if document.is_empty() {
            return Ok(());
        }
        Self::record_document_mode(directory, root, document, false)
    }

    #[cfg(test)]
    pub fn record_document_mode(
        directory: &Path,
        root: &Path,
        document: &str,
        single_file: bool,
    ) -> Result<()> {
        Self::record_document_opened(directory, root, document, single_file, None)
    }

    /// The session worker supplies a timestamp only on a usable vault opening,
    /// not on each note change or a candidate that never published.
    pub fn record_document_opened(
        directory: &Path,
        root: &Path,
        document: &str,
        single_file: bool,
        opened_at: Option<u64>,
    ) -> Result<()> {
        validate_document(root, document)?;
        let _lock = Self::lock(directory, std::iter::once(root))?;
        let mut state = Self::load(directory)?.unwrap_or(Self {
            schema: SCHEMA,
            last_documents: BTreeMap::new(),
            quick_roots: Default::default(),
            recent_roots: Vec::new(),
            hidden_roots: Default::default(),
            last_opened: Default::default(),
            default_root: None,
        });
        state.hidden_roots.remove(root);
        if single_file {
            state.quick_roots.insert(root.to_owned());
        } else {
            state.quick_roots.remove(root);
        }
        state
            .last_documents
            .insert(root.to_path_buf(), document.into());
        state.recent_roots.retain(|old| old != root);
        state.recent_roots.insert(0, root.to_path_buf());
        if !single_file {
            if let Some(opened_at) = opened_at {
                let previous = state.last_opened.entry(root.to_path_buf()).or_default();
                *previous = (*previous).max(opened_at);
            }
        }
        state.persist(directory)
    }
    /// Older history has no ordering: only a single root can be restored without guessing.
    pub fn quick_document(directory: &Path, root: &Path) -> Result<Option<String>> {
        Ok(Self::load(directory)?.and_then(|state| {
            if state.quick_roots.contains(root) {
                state.last_documents.get(root).cloned()
            } else {
                None
            }
        }))
    }

    pub fn startup_roots(directory: &Path) -> Result<(Option<PathBuf>, Vec<PathBuf>)> {
        let Some(state) = Self::load(directory)? else {
            return Ok((None, Vec::new()));
        };
        let roots = state.listed_roots();
        let last = state
            .recent_roots
            .iter()
            .find(|root| !state.hidden_roots.contains(*root))
            .cloned()
            .or_else(|| (roots.len() == 1).then(|| roots[0].clone()));
        Ok((last, roots))
    }

    fn listed_roots(&self) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        for root in self.recent_roots.iter().chain(self.last_documents.keys()) {
            if !self.hidden_roots.contains(root) && !roots.contains(root) {
                roots.push(root.clone());
            }
        }
        roots
    }

    /// Known vaults, including paths that are temporarily absent/unmounted.
    /// Quick-viewer folders do not become vaults merely by opening one file.
    pub fn known_vault_roots(directory: &Path) -> Result<Vec<PathBuf>> {
        Ok(Self::known_vaults(directory)?
            .into_iter()
            .map(|vault| vault.root)
            .collect())
    }

    pub fn known_vaults(directory: &Path) -> Result<Vec<KnownVault>> {
        let Some(state) = Self::load(directory)? else {
            return Ok(Vec::new());
        };
        Ok(state
            .listed_roots()
            .into_iter()
            .filter(|root| !state.quick_roots.contains(root))
            .map(|root| KnownVault {
                last_opened: state.last_opened.get(&root).copied(),
                is_default: state.default_root.as_ref() == Some(&root),
                root,
            })
            .collect())
    }

    pub fn default_vault(directory: &Path) -> Result<Option<PathBuf>> {
        Ok(Self::load(directory)?.and_then(|state| state.default_root))
    }

    pub fn set_default_vault(directory: &Path, root: Option<&Path>) -> Result<()> {
        let _lock = Self::lock(directory, std::iter::empty())?;
        let Some(mut state) = Self::load(directory)? else {
            if root.is_none() {
                return Ok(());
            }
            bail!("Open this vault successfully before making it the default");
        };
        if let Some(root) = root {
            if !state.listed_roots().iter().any(|known| known == root)
                || state.quick_roots.contains(root)
            {
                bail!("Choose a known full vault as the default");
            }
        }
        state.default_root = root.map(Path::to_path_buf);
        state.persist(directory)
    }

    /// Hide only the list entry; retain the folder and its last reading place.
    /// Publishing a usable document there again makes it recent/visible again.
    pub fn remove_from_list(directory: &Path, root: &Path) -> Result<()> {
        let _lock = Self::lock(directory, std::iter::empty())?;
        let Some(mut state) = Self::load(directory)? else {
            return Ok(());
        };
        if state.last_documents.contains_key(root) {
            state.hidden_roots.insert(root.to_owned());
            if state.default_root.as_deref() == Some(root) {
                state.default_root = None;
            }
            state.persist(directory)?;
        }
        Ok(())
    }

    /// Remove a vault from the start screen's list and from restore (#1368).
    /// Its folder is not touched; opening it again adds it back.
    pub fn forget_root(directory: &Path, root: &Path) -> Result<()> {
        let _lock = Self::lock(directory, std::iter::empty())?;
        let Some(mut state) = Self::load(directory)? else {
            return Ok(());
        };
        state.last_documents.remove(root);
        state.quick_roots.remove(root);
        state.hidden_roots.remove(root);
        state.last_opened.remove(root);
        if state.default_root.as_deref() == Some(root) {
            state.default_root = None;
        }
        state.recent_roots.retain(|old| old != root);
        state.persist(directory)
    }

    /// Recover only invalid content, never permission/I/O errors or symlinks.
    /// The same lock as writers prevents archiving a concurrently repaired file.
    pub fn last_document_or_recover(
        directory: &Path,
        root: &Path,
    ) -> Result<(Option<String>, Option<String>)> {
        match Self::last_document(directory, root) {
            Ok(document) => return Ok((document, None)),
            Err(error) if error.is::<InvalidHistory>() => {}
            Err(error) => return Err(error),
        }
        let _lock = Self::lock(directory, std::iter::once(root))?;
        match Self::last_document(directory, root) {
            Ok(document) => Ok((document, None)),
            Err(error) if error.is::<InvalidHistory>() => {
                let name = format!("update-session.invalid-{}.json", uuid::Uuid::new_v4());
                fs::rename(directory.join(FILE), directory.join(&name))
                    .context("Cannot set aside invalid reading history")?;
                // The rename has already preserved the file. A directory-sync
                // failure must not turn successful recovery into a failed open.
                #[cfg(unix)]
                if let Err(error) = fs::File::open(directory).and_then(|file| file.sync_all()) {
                    eprintln!("Cannot sync recovered reading history directory: {error}");
                }
                Ok((
                    None,
                    Some(format!(
                        "Reading history could not be used and was saved as {name}."
                    )),
                ))
            }
            Err(error) => Err(error),
        }
    }

    /// A history hint, never a validated OpenIntent. #338 validates existence and
    /// eligibility in the existing Reader opener before choosing it.
    pub fn last_document(directory: &Path, root: &Path) -> Result<Option<String>> {
        Ok(Self::load(directory)?.and_then(|state| state.last_documents.get(root).cloned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn isolated_state_directory_requires_an_absolute_path() {
        let absolute = std::env::temp_dir().join("okilum-qa-state");
        assert_eq!(
            isolated_from(Some(absolute.clone().into_os_string())),
            Some(absolute)
        );
        assert_eq!(isolated_from(Some("relative/state".into())), None);
        assert_eq!(isolated_from(Some("".into())), None);
        assert_eq!(isolated_from(None), None);
    }

    #[test]
    fn known_vaults_keep_unmounted_paths_but_not_quick_viewer_folders() {
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let available = fixture.path().join("vault");
        let missing = fixture.path().join("unmounted");
        let quick = fixture.path().join("quick");
        fs::create_dir(&available).unwrap();
        ReadingHistory::record_document_mode(&state, &available, "note.md", false).unwrap();
        ReadingHistory::record_document_mode(&state, &missing, "offline.md", false).unwrap();
        ReadingHistory::record_document_mode(&state, &quick, "file.md", true).unwrap();
        let before = fs::read(state.join(FILE)).unwrap();
        assert_eq!(
            ReadingHistory::known_vault_roots(&state).unwrap(),
            [missing.clone(), available]
        );
        assert!(!missing.exists(), "the absent mount is not created");
        assert_eq!(
            fs::read(state.join(FILE)).unwrap(),
            before,
            "listing is read-only"
        );
        assert!(
            !serde_json::from_slice::<serde_json::Value>(&before)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("hidden_roots"),
            "ordinary history serialization is unchanged"
        );
    }

    #[test]
    fn removing_a_known_vault_preserves_bytes_and_reading_place_and_reopen_readds_it() {
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let vault = fixture.path().join("vault");
        let other = fixture.path().join("other");
        fs::create_dir(&vault).unwrap();
        let bytes = b"# Synthetic note\r\n\xff\0";
        fs::write(vault.join("note.md"), bytes).unwrap();
        ReadingHistory::record_usable_document(&state, &other, "other.md").unwrap();
        ReadingHistory::record_usable_document(&state, &vault, "note.md").unwrap();
        assert_eq!(
            ReadingHistory::known_vault_roots(&state).unwrap(),
            [vault.clone(), other.clone()]
        );
        ReadingHistory::remove_from_list(&state, &vault).unwrap();
        assert_eq!(
            ReadingHistory::known_vault_roots(&state).unwrap(),
            [other.clone()]
        );
        assert_eq!(
            ReadingHistory::startup_roots(&state).unwrap(),
            (Some(other.clone()), vec![other])
        );
        assert_eq!(
            ReadingHistory::last_document(&state, &vault)
                .unwrap()
                .as_deref(),
            Some("note.md")
        );
        assert_eq!(fs::read(vault.join("note.md")).unwrap(), bytes);
        assert_eq!(
            fs::read_dir(&vault).unwrap().count(),
            1,
            "no sidecars or directory operations"
        );
        // The durable suppression survives a load; using the vault again re-adds it.
        ReadingHistory::remove_from_list(&state, &vault).unwrap();
        ReadingHistory::record_usable_document(&state, &vault, "note.md").unwrap();
        assert_eq!(
            ReadingHistory::known_vault_roots(&state).unwrap().first(),
            Some(&vault)
        );
        assert_eq!(fs::read(vault.join("note.md")).unwrap(), bytes);
    }

    #[test]
    fn changing_invalid_or_busy_history_preserves_the_list_and_default() {
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let vault = fixture.path().join("vault");
        ReadingHistory::record_usable_document(&state, &vault, "note.md").unwrap();
        let before = fs::read(state.join(FILE)).unwrap();
        let lock = ReadingHistory::lock(&state, std::iter::empty()).unwrap();
        assert!(ReadingHistory::remove_from_list(&state, &vault)
            .unwrap_err()
            .is::<Busy>());
        assert!(ReadingHistory::set_default_vault(&state, Some(&vault))
            .unwrap_err()
            .is::<Busy>());
        assert_eq!(fs::read(state.join(FILE)).unwrap(), before);
        drop(lock);
        fs::write(state.join(FILE), b"invalid JSON").unwrap();
        assert!(ReadingHistory::remove_from_list(&state, &vault).is_err());
        assert!(ReadingHistory::set_default_vault(&state, Some(&vault)).is_err());
        assert_eq!(fs::read(state.join(FILE)).unwrap(), b"invalid JSON");
    }

    #[test]
    fn vault_settings_preserve_legacy_dates_and_only_record_successful_openings() {
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let legacy = fixture.path().join("legacy");
        let opened = fixture.path().join("opened");
        ReadingHistory::record_document_mode(&state, &legacy, "old.md", false).unwrap();
        ReadingHistory::record_document_opened(&state, &opened, "first.md", false, Some(100))
            .unwrap();
        ReadingHistory::record_document_mode(&state, &opened, "second.md", false).unwrap();
        let rows = ReadingHistory::known_vaults(&state).unwrap();
        assert_eq!(
            rows[0].last_opened,
            Some(100),
            "note navigation is not another opening"
        );
        assert_eq!(
            rows[1].last_opened, None,
            "legacy records have no invented timestamp"
        );
        ReadingHistory::record_document_opened(&state, &opened, "second.md", false, Some(200))
            .unwrap();
        assert_eq!(
            ReadingHistory::known_vaults(&state).unwrap()[0].last_opened,
            Some(200)
        );
        assert!(!opened.exists(), "history recording never creates a vault");
    }

    #[test]
    fn default_vault_roundtrips_and_removal_clears_only_the_preference_and_list_entry() {
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let offline = fixture.path().join("unmounted");
        let quick = fixture.path().join("quick");
        ReadingHistory::record_document_mode(&state, &offline, "note.md", false).unwrap();
        ReadingHistory::record_document_mode(&state, &quick, "file.md", true).unwrap();
        ReadingHistory::set_default_vault(&state, Some(&offline)).unwrap();
        assert_eq!(
            ReadingHistory::default_vault(&state).unwrap(),
            Some(offline.clone())
        );
        assert!(ReadingHistory::known_vaults(&state).unwrap()[0].is_default);
        let before = fs::read(state.join(FILE)).unwrap();
        for invalid in [&quick, &fixture.path().join("unknown")] {
            assert!(ReadingHistory::set_default_vault(&state, Some(invalid)).is_err());
            assert_eq!(fs::read(state.join(FILE)).unwrap(), before);
        }
        ReadingHistory::remove_from_list(&state, &offline).unwrap();
        assert_eq!(ReadingHistory::default_vault(&state).unwrap(), None);
        assert!(ReadingHistory::known_vaults(&state).unwrap().is_empty());
        assert_eq!(
            ReadingHistory::last_document(&state, &offline)
                .unwrap()
                .as_deref(),
            Some("note.md")
        );
        assert!(
            !offline.exists(),
            "an unmounted default is retained until explicit removal, never created"
        );
        ReadingHistory::record_document_mode(&state, &offline, "note.md", false).unwrap();
        assert!(
            !ReadingHistory::known_vaults(&state).unwrap()[0].is_default,
            "reopening does not silently restore a removed preference"
        );
    }

    #[test]
    fn forgetting_a_vault_keeps_the_others_and_its_folder() {
        // #1368: the start screen removes a vault from the list.
        let f =
            Fixture(std::env::temp_dir().join(format!("okilum-forget-{}", uuid::Uuid::new_v4())));
        let state = f.0.join("state");
        let one = f.0.join("one");
        let two = f.0.join("two");
        for root in [&one, &two] {
            fs::create_dir_all(root).unwrap();
        }
        ReadingHistory::record_usable_document(&state, &one, "a.md").unwrap();
        ReadingHistory::record_document_mode(&state, &two, "b.md", true).unwrap();
        ReadingHistory::forget_root(&state, &two).unwrap();
        assert_eq!(
            ReadingHistory::startup_roots(&state).unwrap(),
            (Some(one.clone()), vec![one.clone()])
        );
        assert_eq!(ReadingHistory::last_document(&state, &two).unwrap(), None);
        assert_eq!(ReadingHistory::quick_document(&state, &two).unwrap(), None);
        assert!(two.is_dir(), "the folder itself is left alone");
        // A vault that is not listed, or no history at all, is not an error.
        ReadingHistory::forget_root(&state, &two).unwrap();
        ReadingHistory::forget_root(&f.0.join("empty-state"), &one).unwrap();
    }

    #[test]
    fn history_roundtrip_refuses_notes_and_reads_older_files() {
        let f =
            Fixture(std::env::temp_dir().join(format!("okilum-history-{}", uuid::Uuid::new_v4())));
        let vault = f.0.join("vault");
        let state = f.0.join("state");
        fs::create_dir_all(&vault).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::write(vault.join("Заметка.md"), b"# Exact bytes\r\n").unwrap();
        // A file written by an earlier build with restart fields still loads.
        let legacy = serde_json::json!({
            "schema": 1,
            "readers": [],
            "pending_restart": false,
            "last_documents": { vault.to_str().unwrap(): "Заметка.md" },
        });
        fs::write(state.join(FILE), serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert_eq!(
            ReadingHistory::last_document(&state, &vault)
                .unwrap()
                .as_deref(),
            Some("Заметка.md")
        );
        ReadingHistory::record_usable_document(&state, &vault, "next.md").unwrap();
        assert_eq!(
            ReadingHistory::last_document(&state, &vault)
                .unwrap()
                .as_deref(),
            Some("next.md")
        );
        assert!(ReadingHistory::record_usable_document(&state, &vault, "../out.md").is_err());
        assert!(ReadingHistory::record_usable_document(&vault, &vault, "a.md").is_err());
        assert_eq!(
            fs::read(vault.join("Заметка.md")).unwrap(),
            b"# Exact bytes\r\n"
        );
        fs::write(state.join(FILE), b"{\"schema\":2}").unwrap();
        assert!(ReadingHistory::last_document(&state, &vault).is_err());
    }
    #[test]
    fn startup_tracks_last_published_root_without_guessing_legacy_order() {
        let f = Fixture(
            std::env::temp_dir().join(format!("okilum-startup-history-{}", uuid::Uuid::new_v4())),
        );
        let state = f.0.join("state");
        let a = f.0.join("a");
        let b = f.0.join("b");
        fs::create_dir_all(&state).unwrap();
        assert_eq!(
            ReadingHistory::startup_roots(&state).unwrap(),
            (None, vec![])
        );
        let legacy =
            serde_json::json!({"schema": 1, "last_documents": {a.to_str().unwrap(): "a.md"}});
        fs::write(state.join(FILE), serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert_eq!(
            ReadingHistory::startup_roots(&state).unwrap(),
            (Some(a.clone()), vec![a.clone()])
        );
        let mut legacy = legacy;
        legacy["last_documents"][b.to_str().unwrap()] = "b.md".into();
        fs::write(state.join(FILE), serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert_eq!(
            ReadingHistory::startup_roots(&state).unwrap(),
            (None, vec![a.clone(), b.clone()])
        );
        ReadingHistory::record_usable_document(&state, &b, "latest.md").unwrap();
        assert_eq!(
            ReadingHistory::startup_roots(&state).unwrap(),
            (Some(b.clone()), vec![b.clone(), a.clone()])
        );
        ReadingHistory::record_usable_document(&state, &a, "next.md").unwrap();
        assert_eq!(
            ReadingHistory::startup_roots(&state).unwrap(),
            (Some(a.clone()), vec![a, b.clone()])
        );
        assert_eq!(
            ReadingHistory::last_document(&state, &b)
                .unwrap()
                .as_deref(),
            Some("latest.md")
        );
    }

    #[test]
    fn stray_recency_row_does_not_block_valid_document_history() {
        let f = Fixture(
            std::env::temp_dir().join(format!("okilum-history-stray-{}", uuid::Uuid::new_v4())),
        );
        let state = f.0.join("state");
        let root = f.0.join("vault");
        fs::create_dir_all(&state).unwrap();
        let data = serde_json::json!({
            "schema": 1,
            "last_documents": { root.to_str().unwrap(): "last.md" },
            "recent_roots": [f.0.join("stray"), root.clone()]
        });
        fs::write(state.join(FILE), serde_json::to_vec(&data).unwrap()).unwrap();
        assert_eq!(
            ReadingHistory::last_document(&state, &root)
                .unwrap()
                .as_deref(),
            Some("last.md")
        );
        assert_eq!(
            ReadingHistory::startup_roots(&state).unwrap(),
            (Some(root.clone()), vec![root])
        );
    }

    #[test]
    fn invalid_history_is_preserved_once_and_can_be_replaced() {
        for bytes in [
            b"not json".as_slice(),
            b"{\"schema\":999}",
            b"{\"schema\":1,\"last_documents\":{\"relative\":\"../escape.md\"}}",
        ] {
            let f = Fixture(
                std::env::temp_dir().join(format!("okilum-recover-{}", uuid::Uuid::new_v4())),
            );
            let state = f.0.join("state");
            let root = f.0.join("vault");
            fs::create_dir_all(&state).unwrap();
            fs::create_dir_all(&root).unwrap();
            fs::write(state.join(FILE), bytes).unwrap();
            // A writer that arrives before recovery must refuse, not overwrite.
            assert!(ReadingHistory::record_usable_document(&state, &root, "new.md").is_err());
            assert_eq!(fs::read(state.join(FILE)).unwrap(), bytes);
            let (document, notice) =
                ReadingHistory::last_document_or_recover(&state, &root).unwrap();
            assert!(document.is_none());
            assert!(notice.unwrap().contains("saved as update-session.invalid-"));
            assert!(!state.join(FILE).exists());
            let backups = || {
                fs::read_dir(&state)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .filter(|path| {
                        path.file_name()
                            .unwrap()
                            .to_string_lossy()
                            .starts_with("update-session.invalid-")
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(backups().len(), 1);
            assert_eq!(fs::read(&backups()[0]).unwrap(), bytes);
            assert_eq!(
                ReadingHistory::last_document_or_recover(&state, &root).unwrap(),
                (None, None)
            );
            ReadingHistory::record_usable_document(&state, &root, "first.md").unwrap();
            assert_eq!(
                ReadingHistory::last_document(&state, &root)
                    .unwrap()
                    .as_deref(),
                Some("first.md")
            );
            assert_eq!(backups().len(), 1);
            assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        }
    }

    #[test]
    fn recovery_respects_writer_lock_and_vault_boundary() {
        let f = Fixture(
            std::env::temp_dir().join(format!("okilum-recovery-lock-{}", uuid::Uuid::new_v4())),
        );
        let state = f.0.join("state");
        let root = f.0.join("vault");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&root).unwrap();
        fs::write(state.join(FILE), b"broken").unwrap();
        let lock = ReadingHistory::lock(&state, std::iter::once(root.as_path())).unwrap();
        assert!(ReadingHistory::last_document_or_recover(&state, &root).is_err());
        assert_eq!(fs::read(state.join(FILE)).unwrap(), b"broken");
        drop(lock);
        fs::write(root.join(FILE), b"broken").unwrap();
        assert!(ReadingHistory::last_document_or_recover(&root, &root).is_err());
        assert_eq!(fs::read(root.join(FILE)).unwrap(), b"broken");
    }

    #[cfg(unix)]
    #[test]
    fn completed_writer_releases_lock_even_with_an_inherited_descriptor() {
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let vault = fixture.path().join("vault");
        fs::create_dir_all(&vault).unwrap();
        ReadingHistory::record_usable_document(&state, &vault, "first.md").unwrap();
        let before = fs::read(state.join(FILE)).unwrap();
        let writer = ReadingHistory::lock(&state, std::iter::once(vault.as_path())).unwrap();
        // dup and fork share the same open file description on Unix. Holding
        // this descriptor models a child paused between fork and exec.
        let inherited = writer.0.try_clone().unwrap();
        let busy = ReadingHistory::record_usable_document(&state, &vault, "second.md").unwrap_err();
        assert!(
            busy.is::<Busy>(),
            "positive control: the writer excludes another writer"
        );
        assert_eq!(fs::read(state.join(FILE)).unwrap(), before);
        drop(writer);
        ReadingHistory::record_usable_document(&state, &vault, "second.md").unwrap();
        assert_eq!(
            ReadingHistory::last_document(&state, &vault)
                .unwrap()
                .as_deref(),
            Some("second.md")
        );
        drop(inherited);
    }

    #[test]
    fn busy_lock_preserves_last_document() {
        let f =
            Fixture(std::env::temp_dir().join(format!("okilum-history-{}", uuid::Uuid::new_v4())));
        let vault = f.0.join("vault");
        let state = f.0.join("state");
        fs::create_dir_all(&vault).unwrap();
        ReadingHistory::record_usable_document(&state, &vault, "first.md").unwrap();
        let before = fs::read(state.join(FILE)).unwrap();
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(state.join("reader-session.lock"))
            .unwrap();
        lock.lock().unwrap();
        assert!(ReadingHistory::record_usable_document(&state, &vault, "second.md").is_err());
        assert_eq!(fs::read(state.join(FILE)).unwrap(), before);
        lock.unlock().unwrap();
        ReadingHistory::record_usable_document(&state, &vault, "second.md").unwrap();
        assert_eq!(
            ReadingHistory::last_document(&state, &vault)
                .unwrap()
                .as_deref(),
            Some("second.md")
        );
    }
}
