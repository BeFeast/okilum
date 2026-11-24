//! Read Gardener's sealed path map after a verified switch to a successor vault.
use super::{moves, State};
use crate::reader_ui_state::Layout;
use anyhow::{ensure, Context as _, Result};
use okilum_core::vault_link_identity;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};

pub const ARCHIVE_MARKER: &str = ".brain-read-only";
const MAP: &str = "@views/path-map.jsonl";
const DESCRIPTOR: &str = ".okilum/vault.json";
const MAX_MAP_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveShortcuts {
    pub root: PathBuf,
    pub vault_id: String,
    pub recent: Vec<(String, u64)>,
    pub pinned: Vec<String>,
}

#[derive(Deserialize)]
struct Descriptor {
    lifecycle: Archive,
}

#[derive(Deserialize)]
struct Archive {
    state: String,
    read_only: bool,
    successor: Successor,
}

#[derive(Deserialize)]
struct Successor {
    name: String,
    vault_id: String,
}

#[derive(Deserialize)]
struct Row {
    old_path: String,
    new_path: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Cutover {
    pub archive: PathBuf,
    pub grove: PathBuf,
    archive_id: String,
    paths: BTreeMap<String, String>,
}

fn metadata_file(root: &Path, path: &str) -> Option<File> {
    moves::contained(root, path)
        .then(|| File::open(root.join(path)).ok())
        .flatten()
}

fn metadata_identity(root: &Path) -> Option<String> {
    let marker = root.join(vault_link_identity::FILE);
    let canonical = marker.canonicalize().ok()?;
    if canonical != marker {
        return None;
    }
    vault_link_identity::read(root).ok().flatten()
}

fn archive_descriptor(root: &Path) -> Option<Archive> {
    metadata_file(root, ARCHIVE_MARKER)?;
    // This exact service file is deliberately absent from ordinary inventory.
    let path = root.join(DESCRIPTOR);
    if path.canonicalize().ok()? != path || !fs::symlink_metadata(&path).ok()?.is_file() {
        return None;
    }
    let Descriptor { lifecycle } =
        serde_json::from_reader(File::open(path).ok()?.take(64 * 1024)).ok()?;
    (lifecycle.state == "archive" && lifecycle.read_only).then_some(lifecycle)
}

fn verified_roots(
    archive: &Path,
    successor: &Successor,
    known_roots: &[PathBuf],
) -> BTreeSet<PathBuf> {
    let named_sibling = archive
        .parent()
        .filter(|_| moves::valid_path(&successor.name) && !successor.name.contains('/'))
        .map(|parent| parent.join(&successor.name));
    known_roots
        .iter()
        .cloned()
        .chain(named_sibling)
        .filter_map(|root| root.canonicalize().ok())
        .filter(|root| {
            root != archive && metadata_identity(root).as_deref() == Some(&successor.vault_id)
        })
        .collect()
}

fn read_map(grove: &Path) -> Result<BTreeMap<String, String>> {
    let file = metadata_file(grove, MAP).context("The sealed Grove map is unavailable")?;
    ensure!(
        file.metadata()?.len() <= MAX_MAP_BYTES,
        "The sealed Grove map is too large"
    );
    let mut claimed = BTreeMap::<String, BTreeSet<Option<String>>>::new();
    let mut sources = BTreeMap::<String, BTreeSet<String>>::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        let row: Row = serde_json::from_str(&line)?;
        ensure!(
            moves::relative_path(&row.old_path),
            "Invalid archive-relative path"
        );
        if let Some(target) = &row.new_path {
            ensure!(moves::relative_path(target), "Invalid Grove-relative path");
            sources
                .entry(target.clone())
                .or_default()
                .insert(row.old_path.clone());
        }
        claimed
            .entry(row.old_path)
            .or_default()
            .insert(row.new_path);
    }
    Ok(claimed
        .into_iter()
        .filter_map(|(old, targets)| {
            if targets.len() != 1 {
                return None;
            }
            let target = targets.into_iter().next().flatten()?;
            (sources[&target].len() == 1 && moves::contained(grove, &target))
                .then_some((old, target))
        })
        .collect())
}

impl Cutover {
    pub fn from_archive(archive: &Path, known_roots: &[PathBuf]) -> Option<Self> {
        let descriptor = archive_descriptor(archive)?;
        let archive_id = metadata_identity(archive)?;
        if archive_id == descriptor.successor.vault_id {
            return None;
        }
        let receivers = verified_roots(archive, &descriptor.successor, known_roots);
        if receivers.len() != 1 {
            return None;
        }
        let grove = receivers.into_iter().next()?;
        let paths = read_map(&grove)
            .inspect_err(|error| {
                diagnostic_error!("Cannot read sealed Grove map: {error:#}");
            })
            .ok()?;
        Some(Self {
            archive: archive.to_owned(),
            grove,
            archive_id,
            paths,
        })
    }

    pub fn into_grove(grove: &Path, known_roots: &[PathBuf]) -> Option<Self> {
        let id = metadata_identity(grove)?;
        let archives: BTreeSet<_> = known_roots
            .iter()
            .filter_map(|root| root.canonicalize().ok())
            .filter(|root| {
                archive_descriptor(root)
                    .is_some_and(|descriptor| descriptor.successor.vault_id == id)
            })
            .collect();
        if archives.len() != 1 {
            return None;
        }
        let archive = archives.first()?;
        let candidates = known_roots
            .iter()
            .cloned()
            .chain([grove.to_owned()])
            .collect::<Vec<_>>();
        Self::from_archive(archive, &candidates).filter(|cutover| cutover.grove == grove)
    }

    pub fn target(&self, source: &str) -> Option<&str> {
        self.paths.get(source).map(String::as_str)
    }

    fn folder(&self, source: &str) -> Option<String> {
        if let Some(target) = self
            .target(source)
            .filter(|target| self.grove.join(target).is_dir())
        {
            return Some(target.to_owned());
        }
        let source_index = format!("{source}/@index.md");
        let target = self.target(&source_index)?;
        Some(target.rsplit_once('/')?.0.to_owned())
    }

    /// Grove's existing entries take precedence. Import a sealed snapshot once
    /// so a later restart cannot reinstate an intentionally unpinned shortcut.
    pub fn import_sidebar(&self, old: &State, grove: &mut State) -> bool {
        if grove.archive_shortcuts.is_some() {
            return false;
        }
        let mut unresolved = ArchiveShortcuts {
            root: self.archive.clone(),
            vault_id: self.archive_id.clone(),
            recent: Vec::new(),
            pinned: Vec::new(),
        };
        for (path, at) in old
            .recent
            .iter()
            .filter(|(path, _)| moves::valid_path(path))
        {
            if let Some(target) = self.target(path) {
                if !grove.recent.iter().any(|(current, _)| current == target) {
                    grove.recent.push((target.to_owned(), *at));
                }
            } else {
                unresolved.recent.push((path.clone(), *at));
            }
        }
        for path in old.pinned.iter().filter(|path| moves::valid_path(path)) {
            if let Some(target) = self
                .target(path)
                .map(str::to_owned)
                .or_else(|| self.folder(path))
            {
                if !grove.is_pinned(&target) {
                    grove.pinned.push(target);
                }
            } else {
                unresolved.pinned.push(path.clone());
            }
        }
        for path in &old.recent_projects {
            if let Some(target) = self
                .folder(path)
                .filter(|target| !grove.recent_projects.contains(target))
            {
                grove.recent_projects.push(target);
            }
        }
        grove.nav_all_vault.extend(
            old.nav_all_vault
                .iter()
                .filter_map(|path| self.folder(path)),
        );
        grove.archive_shortcuts = Some(unresolved);
        true
    }

    pub fn import_layout(&self, mut saved: Layout) -> Layout {
        saved.note = self.target(&saved.note).unwrap_or_default().to_owned();
        let mut history = Vec::new();
        let mut history_index = 0;
        for (index, mut visit) in saved.history.into_iter().enumerate() {
            if let Some(target) = self.target(&visit.note) {
                visit.note = target.to_owned();
                if index == saved.history_index {
                    history_index = history.len();
                }
                history.push(visit);
            }
        }
        saved.history = history;
        saved.history_index = history_index;
        saved.project = saved.project.as_ref().and_then(|path| {
            self.folder(&okilum_core::vault::note_path(path))
                .map(PathBuf::from)
        });
        saved.folders = saved
            .folders
            .iter()
            .filter_map(|path| self.folder(path))
            .collect();
        saved.tree_cursor = saved.tree_cursor.as_deref().and_then(|path| {
            self.target(path)
                .map(str::to_owned)
                .or_else(|| self.folder(path))
        });
        saved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARCHIVE_ID: &str = "550e8400-e29b-41d4-a716-446655440001";
    const GROVE_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

    fn write(root: &Path, path: &str, bytes: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path().canonicalize().unwrap();
        let archive = base.join("brain");
        let grove = base.join("grove");
        for (root, id) in [(&archive, ARCHIVE_ID), (&grove, GROVE_ID)] {
            write(
                root,
                vault_link_identity::FILE,
                &serde_json::json!({"version": 1, "id": id}).to_string(),
            );
        }
        write(&archive, ARCHIVE_MARKER, "Archived");
        write(
            &archive,
            DESCRIPTOR,
            &serde_json::json!({
                "lifecycle": {"state": "archive", "read_only": true,
                    "successor": {"name": "grove", "vault_id": GROVE_ID}}
            })
            .to_string(),
        );
        write(&archive, "Legacy/A.md", "Archive bytes");
        write(&grove, "state/dev/example/A.md", "Grove bytes");
        write(&grove, "state/dev/example/@index.md", "Project");
        write(&grove, MAP, concat!(
            "{\"old_path\":\"Legacy/A.md\",\"new_path\":\"state/dev/example/A.md\",\"aliases\":[\"alias\"]}\n",
            "{\"old_path\":\"Legacy/@index.md\",\"new_path\":\"state/dev/example/@index.md\"}\n",
            "{\"old_path\":\"Legacy/Retired.md\",\"new_path\":null}\n"
        ));
        (fixture, archive, grove)
    }

    #[test]
    fn sealed_copy_imports_shortcuts_once_and_preserves_archive_provenance() {
        let (_fixture, archive, grove) = fixture();
        let cutover = Cutover::into_grove(&grove, std::slice::from_ref(&archive)).unwrap();
        assert_eq!(
            cutover.target("Legacy/A.md"),
            Some("state/dev/example/A.md")
        );
        assert!(
            cutover.target("alias").is_none(),
            "saved paths use exact identity"
        );
        let old = State {
            recent: vec![("Legacy/A.md".into(), 20), ("Legacy/Retired.md".into(), 10)],
            pinned: vec!["Legacy/A.md".into()],
            recent_projects: vec!["Legacy".into()],
            nav_all_vault: BTreeSet::from(["Legacy".into()]),
            ..Default::default()
        };
        let mut new = State::default();
        assert!(cutover.import_sidebar(&old, &mut new));
        assert_eq!(new.recent, [("state/dev/example/A.md".into(), 20)]);
        assert_eq!(new.pinned, ["state/dev/example/A.md"]);
        assert_eq!(new.recent_projects, ["state/dev/example"]);
        assert_eq!(
            new.nav_all_vault,
            BTreeSet::from(["state/dev/example".into()])
        );
        assert_eq!(
            new.archive_shortcuts.as_ref().unwrap().recent,
            [("Legacy/Retired.md".into(), 10)]
        );
        new.pinned.clear();
        assert!(!cutover.import_sidebar(&old, &mut new));
        assert!(new.pinned.is_empty());
        assert_eq!(
            fs::read_to_string(archive.join("Legacy/A.md")).unwrap(),
            "Archive bytes"
        );
    }

    #[test]
    fn never_uses_a_namesake_duplicate_identity_or_ambiguous_map_target() {
        let (fixture, archive, grove) = fixture();
        let namesake = fixture.path().join("second-grove");
        write(
            &namesake,
            vault_link_identity::FILE,
            &serde_json::json!({"id": GROVE_ID}).to_string(),
        );
        assert!(Cutover::from_archive(&archive, &[namesake]).is_none());
        write(
            &grove,
            MAP,
            concat!(
                "{\"old_path\":\"Legacy/A.md\",\"new_path\":\"state/dev/example/A.md\"}\n",
                "{\"old_path\":\"Legacy/B.md\",\"new_path\":\"state/dev/example/A.md\"}\n"
            ),
        );
        assert!(Cutover::from_archive(&archive, &[])
            .unwrap()
            .target("Legacy/A.md")
            .is_none());
        write(
            &grove,
            vault_link_identity::FILE,
            &serde_json::json!({"id": ARCHIVE_ID}).to_string(),
        );
        assert!(Cutover::from_archive(&archive, &[]).is_none());
    }
}
