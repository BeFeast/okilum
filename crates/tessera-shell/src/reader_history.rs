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

/// App-owned Reader state location, outside any vault or index directory.
/// Tests use `TestSessionDirectory` instead.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn state_directory() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    let directory = std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support/uk.oklabs.tessera"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let directory = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|base| base.join("tessera"))
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state/tessera"))
        });
    #[cfg(windows)]
    let directory = dirs::data_local_dir().map(|base| base.join("tessera"));
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
    recent_roots: Vec<PathBuf>,
}
impl ReadingHistory {
    fn lock<'a>(directory: &Path, roots: impl Iterator<Item = &'a Path>) -> Result<fs::File> {
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
        lock.try_lock()
            .context("Reader state is busy; retry without losing the existing history")?;
        Ok(lock)
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
        Ok(Some(history))
    }
    /// #338 supplies a validated, already published usable document, not a pending
    /// request or full-index completion. Callers surface I/O failures explicitly.
    pub fn record_usable_document(directory: &Path, root: &Path, document: &str) -> Result<()> {
        validate_document(root, document)?;
        if document.is_empty() {
            return Ok(());
        }
        Self::record_selection(directory, root, document)
    }

    /// An explicit empty selection is different from having no saved selection.
    pub fn record_empty_vault(directory: &Path, root: &Path) -> Result<()> {
        validate_document(root, "")?;
        Self::record_selection(directory, root, "")
    }

    fn record_selection(directory: &Path, root: &Path, document: &str) -> Result<()> {
        let _lock = Self::lock(directory, std::iter::once(root))?;
        let mut state = Self::load(directory)?.unwrap_or(Self {
            schema: SCHEMA,
            last_documents: BTreeMap::new(),
            recent_roots: Vec::new(),
        });
        state
            .last_documents
            .insert(root.to_path_buf(), document.into());
        state.recent_roots.retain(|old| old != root);
        state.recent_roots.insert(0, root.to_path_buf());
        state.persist(directory)
    }
    /// Older history has no ordering: only a single root can be restored without guessing.
    pub fn startup_roots(directory: &Path) -> Result<(Option<PathBuf>, Vec<PathBuf>)> {
        let Some(state) = Self::load(directory)? else {
            return Ok((None, Vec::new()));
        };
        let last = state.recent_roots.first().cloned().or_else(|| {
            (state.last_documents.len() == 1)
                .then(|| state.last_documents.keys().next().unwrap().clone())
        });
        let mut roots = state.recent_roots;
        for root in state.last_documents.into_keys() {
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        Ok((last, roots))
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
    fn history_roundtrip_refuses_notes_and_reads_older_files() {
        let f =
            Fixture(std::env::temp_dir().join(format!("tessera-history-{}", uuid::Uuid::new_v4())));
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
            std::env::temp_dir().join(format!("tessera-startup-history-{}", uuid::Uuid::new_v4())),
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
            std::env::temp_dir().join(format!("tessera-history-stray-{}", uuid::Uuid::new_v4())),
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
                std::env::temp_dir().join(format!("tessera-recover-{}", uuid::Uuid::new_v4())),
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
            std::env::temp_dir().join(format!("tessera-recovery-lock-{}", uuid::Uuid::new_v4())),
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

    #[test]
    fn busy_lock_preserves_last_document() {
        let f =
            Fixture(std::env::temp_dir().join(format!("tessera-history-{}", uuid::Uuid::new_v4())));
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
