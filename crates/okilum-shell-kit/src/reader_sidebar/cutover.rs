//! Read Gardener's path map after verifying the Brain archive and its successor.
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
const BRAIN_ID: &str = "cc480ea3-0b98-4783-960f-3a64fa4c274c";
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
    #[serde(default)]
    action: String,
}

#[derive(Clone, Debug)]
struct Mapping {
    target: Option<String>,
    opaque: bool,
}

#[derive(Clone, Debug)]
pub struct Cutover {
    pub archive: PathBuf,
    pub grove: PathBuf,
    paths: BTreeMap<String, Mapping>,
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
    if metadata_identity(root).as_deref() != Some(BRAIN_ID) {
        return None;
    }
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

fn read_map(grove: &Path) -> Result<BTreeMap<String, Mapping>> {
    let file = metadata_file(grove, MAP).context("The Grove path map is unavailable")?;
    ensure!(
        file.metadata()?.len() <= MAX_MAP_BYTES,
        "The Grove path map is too large"
    );
    let mut claimed = BTreeMap::<String, BTreeSet<(Option<String>, bool)>>::new();
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
        claimed.entry(row.old_path).or_default().insert((
            row.new_path,
            matches!(row.action.as_str(), "keep-nested" | "keep-unit"),
        ));
    }
    Ok(claimed
        .into_iter()
        .map(|(old, claims)| {
            let opaque = claims.iter().any(|(_, opaque)| *opaque);
            let target = if claims.len() == 1 {
                claims
                    .into_iter()
                    .next()
                    .and_then(|(target, _)| target)
                    .filter(|target| {
                        sources[target].len() == 1
                            && moves::contained(grove, target)
                            && (!opaque || grove.join(target).is_dir())
                    })
            } else {
                None
            };
            (old, Mapping { target, opaque })
        })
        .collect())
}

impl Cutover {
    pub fn from_archive(archive: &Path, known_roots: &[PathBuf]) -> Option<Self> {
        let descriptor = archive_descriptor(archive)?;
        if BRAIN_ID == descriptor.successor.vault_id {
            return None;
        }
        let receivers = verified_roots(archive, &descriptor.successor, known_roots);
        if receivers.len() != 1 {
            return None;
        }
        let grove = receivers.into_iter().next()?;
        let paths = read_map(&grove)
            .inspect_err(|error| {
                diagnostic_error!("Cannot read Grove path map: {error:#}");
            })
            .ok()?;
        Some(Self {
            archive: archive.to_owned(),
            grove,
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

    /// Restore saved paths by exact old path or by an opaque directory ancestor.
    pub fn target(&self, source: &str) -> Option<String> {
        if let Some(mapping) = self.paths.get(source) {
            return mapping.target.clone();
        }
        self.opaque_target(source)
    }

    fn opaque_target(&self, source: &str) -> Option<String> {
        if !moves::valid_path(source) {
            return None;
        }
        let mut ancestor = source;
        while let Some((parent, _)) = ancestor.rsplit_once('/') {
            if let Some(mapping) = self.paths.get(parent).filter(|mapping| mapping.opaque) {
                let target = format!("{}{}", mapping.target.as_deref()?, &source[parent.len()..]);
                return moves::contained(&self.grove, &target).then_some(target);
            }
            ancestor = parent;
        }
        None
    }

    fn folder(&self, source: &str) -> Option<String> {
        if let Some(target) = self
            .target(source)
            .filter(|target| self.grove.join(target).is_dir())
        {
            return Some(target);
        }
        let source_index = format!("{source}/@index.md");
        let target = self.target(&source_index)?;
        Some(target.rsplit_once('/')?.0.to_owned())
    }

    /// Grove's existing entries take precedence. After the first import, only
    /// shortcuts still stored as unresolved can migrate into Grove.
    pub fn import_sidebar(&self, old: &State, grove: &mut State) -> bool {
        if let Some(unresolved) = &mut grove.archive_shortcuts {
            if unresolved.vault_id != BRAIN_ID || unresolved.root != self.archive {
                return false;
            }
            let previous_count = unresolved.recent.len() + unresolved.pinned.len();
            // Older builds stored descendants of opaque roots as unresolved.
            let resolve = |path: &str| {
                if self.paths.contains_key(path) {
                    return None;
                }
                self.opaque_target(path)
            };
            unresolved.recent.retain(|(path, at)| {
                let Some(target) = resolve(path) else {
                    return true;
                };
                if !grove.recent.iter().any(|(current, _)| current == &target) {
                    grove.recent.push((target, *at));
                }
                false
            });
            unresolved.pinned.retain(|path| {
                let Some(target) = resolve(path) else {
                    return true;
                };
                if !grove.pinned.contains(&target) {
                    grove.pinned.push(target);
                }
                false
            });
            return previous_count != unresolved.recent.len() + unresolved.pinned.len();
        }
        let mut unresolved = ArchiveShortcuts {
            root: self.archive.clone(),
            vault_id: BRAIN_ID.into(),
            recent: Vec::new(),
            pinned: Vec::new(),
        };
        for (path, at) in old
            .recent
            .iter()
            .filter(|(path, _)| moves::valid_path(path))
        {
            if let Some(target) = self.target(path) {
                if !grove.recent.iter().any(|(current, _)| current == &target) {
                    grove.recent.push((target, *at));
                }
            } else {
                unresolved.recent.push((path.clone(), *at));
            }
        }
        for path in old.pinned.iter().filter(|path| moves::valid_path(path)) {
            if let Some(target) = self.target(path).or_else(|| self.folder(path)) {
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
        saved.note = self.target(&saved.note).unwrap_or_default();
        let mut history = Vec::new();
        let mut history_index = 0;
        for (index, mut visit) in saved.history.into_iter().enumerate() {
            if let Some(target) = self.target(&visit.note) {
                visit.note = target;
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
        saved.tree_cursor = saved
            .tree_cursor
            .as_deref()
            .and_then(|path| self.target(path).or_else(|| self.folder(path)));
        saved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        for (root, id) in [(&archive, BRAIN_ID), (&grove, GROVE_ID)] {
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
        write(&archive, "Legacy/Retired.md", "Retained note");
        write(&grove, "state/dev/example/A.md", "Grove bytes");
        write(&grove, "state/dev/example/@index.md", "Project");
        write(&grove, "vaults/Nested/notes/pin.md", "Nested note");
        write(&grove, "units/repo-seed/docs/start.md", "Unit note");
        // Exists in Grove to catch an accidental raw string prefix replacement.
        write(&grove, "vaults/Nested2/x.md", "Unrelated note");
        write(
            &grove,
            MAP,
            r#"{"old_path":"Legacy/A.md","new_path":"state/dev/example/A.md","action":"move","rule":"example","reason":"Example move","aliases":["Alias.md"]}
{"old_path":"Legacy/@index.md","new_path":"state/dev/example/@index.md","action":"move","rule":"example","reason":"Example move","aliases":[]}
{"old_path":"Legacy/Retired.md","new_path":null,"action":"retire","rule":"example","reason":"Example retirement","aliases":[]}
{"old_path":"Obsidian Vaults","new_path":"vaults/Nested","action":"keep-nested","rule":"opaque-tree","reason":"Preserve the foreign tree and its internal structure byte-for-byte.","aliases":[]}
{"old_path":"repo-seed","new_path":"units/repo-seed","action":"keep-unit","rule":"opaque-tree","reason":"Preserve the foreign tree and its internal structure byte-for-byte.","aliases":[]}
"#,
        );
        (fixture, archive, grove)
    }

    #[test]
    fn imports_shortcuts_once_and_preserves_archive_provenance() {
        let (_fixture, archive, grove) = fixture();
        let cutover = Cutover::into_grove(&grove, std::slice::from_ref(&archive)).unwrap();
        assert_eq!(
            cutover.target("Legacy/A.md").as_deref(),
            Some("state/dev/example/A.md")
        );
        assert!(
            cutover.target("Alias.md").is_none(),
            "saved paths use exact identity"
        );
        let old = State {
            recent: vec![
                ("Legacy/A.md".into(), 20),
                ("Obsidian Vaults/notes/pin.md".into(), 15),
                ("Legacy/Retired.md".into(), 10),
            ],
            pinned: vec![
                "Legacy/A.md".into(),
                "Obsidian Vaults/notes/pin.md".into(),
                "Obsidian Vaults2/x.md".into(),
                "Alias.md".into(),
            ],
            recent_projects: vec!["Legacy".into()],
            nav_all_vault: BTreeSet::from(["Legacy".into()]),
            ..Default::default()
        };
        let mut new = State::default();
        assert!(cutover.import_sidebar(&old, &mut new));
        assert_eq!(
            new.recent,
            [
                ("state/dev/example/A.md".into(), 20),
                ("vaults/Nested/notes/pin.md".into(), 15),
            ]
        );
        assert_eq!(
            new.pinned,
            ["state/dev/example/A.md", "vaults/Nested/notes/pin.md"]
        );
        assert_eq!(new.recent_projects, ["state/dev/example"]);
        assert_eq!(
            new.nav_all_vault,
            BTreeSet::from(["state/dev/example".into()])
        );
        let archived = new.archive_shortcuts.as_ref().unwrap();
        assert_eq!(archived.recent, [("Legacy/Retired.md".into(), 10)]);
        assert_eq!(archived.pinned, ["Obsidian Vaults2/x.md", "Alias.md"]);
        new.pinned.clear();
        assert!(!cutover.import_sidebar(&old, &mut new));
        assert!(new.pinned.is_empty());
        assert_eq!(
            fs::read_to_string(archive.join("Legacy/A.md")).unwrap(),
            "Archive bytes"
        );
    }

    #[test]
    fn restores_saved_paths_inside_keep_unit() {
        let (_fixture, archive, _) = fixture();
        let cutover = Cutover::from_archive(&archive, &[]).unwrap();
        let saved = Layout {
            note: "repo-seed/docs/start.md".into(),
            tree_cursor: Some("repo-seed/docs".into()),
            ..Default::default()
        };
        let restored = cutover.import_layout(saved);
        assert_eq!(restored.note, "units/repo-seed/docs/start.md");
        assert_eq!(
            restored.tree_cursor.as_deref(),
            Some("units/repo-seed/docs")
        );
        assert!(cutover.target("repo-seed/docs/missing.md").is_none());
        assert!(cutover.target("repo-seed/../vaults/Nested2/x.md").is_none());
    }

    #[test]
    fn promotes_opaque_shortcuts_left_unresolved_by_an_earlier_import() {
        let (_fixture, archive, _) = fixture();
        let cutover = Cutover::from_archive(&archive, &[]).unwrap();
        let nested = "Obsidian Vaults/notes/pin.md";
        let mut grove = State {
            archive_shortcuts: Some(ArchiveShortcuts {
                root: archive,
                vault_id: BRAIN_ID.into(),
                recent: vec![(nested.into(), 15), ("Legacy/Retired.md".into(), 10)],
                pinned: vec![nested.into()],
            }),
            ..Default::default()
        };
        let old = State {
            pinned: vec!["Legacy/A.md".into(), nested.into()],
            ..Default::default()
        };
        assert!(cutover.import_sidebar(&old, &mut grove));
        assert_eq!(grove.recent, [("vaults/Nested/notes/pin.md".into(), 15)]);
        assert_eq!(grove.pinned, ["vaults/Nested/notes/pin.md"]);
        let unresolved = grove.archive_shortcuts.as_ref().unwrap();
        assert_eq!(unresolved.recent, [("Legacy/Retired.md".into(), 10)]);
        assert!(unresolved.pinned.is_empty());
        grove.pinned.clear();
        assert!(!cutover.import_sidebar(&old, &mut grove));
        assert!(grove.pinned.is_empty());
    }

    #[test]
    fn requires_the_brain_identity_and_archive_marker() {
        let (_fixture, archive, grove) = fixture();
        fs::remove_file(archive.join(ARCHIVE_MARKER)).unwrap();
        assert!(Cutover::from_archive(&archive, &[]).is_none());
        write(&archive, ARCHIVE_MARKER, "Archived");
        write(
            &archive,
            vault_link_identity::FILE,
            r#"{"version":1,"id":"550e8400-e29b-41d4-a716-446655440001"}"#,
        );
        assert!(Cutover::from_archive(&archive, &[]).is_none());
        assert!(Cutover::into_grove(&grove, &[archive]).is_none());
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
            &serde_json::json!({"id": BRAIN_ID}).to_string(),
        );
        assert!(Cutover::from_archive(&archive, &[]).is_none());
    }
}
