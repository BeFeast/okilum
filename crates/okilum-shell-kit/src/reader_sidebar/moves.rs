//! Reconcile app-owned paths after an external vault move.
use super::{
    identity::{FileId, Identity},
    State,
};
use crate::reader_ui_state::Layout;
use okilum_core::Vault;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::Path,
};

pub(super) fn valid_path(path: &str) -> bool {
    relative_path(path) && !okilum_core::vault::service_path(Path::new(path))
}

pub(super) fn relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':'])
        && !path.chars().any(char::is_control)
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

fn native_ids(root: &Path) -> bool {
    !okilum_core::watch::is_network_root(root) && !okilum_core::watch::is_remote_fuse_root(root)
}

pub(super) fn contained(root: &Path, relative: &str) -> bool {
    valid_path(relative)
        && fs::symlink_metadata(root.join(relative))
            .is_ok_and(|meta| meta.is_file() || meta.is_dir())
        && fs::canonicalize(root.join(relative)).is_ok_and(|path| {
            path.strip_prefix(root)
                .is_ok_and(|path| okilum_core::vault::note_path(path) == relative)
        })
}

#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub import: Option<Box<(super::cutover::Cutover, State)>>,
    pub handoff: Option<Box<super::cutover::Cutover>>,
    moved: BTreeMap<String, String>,
    uncertain: BTreeSet<String>,
    present: BTreeSet<String>,
}

impl Plan {
    pub fn target(&self, path: &str) -> Option<&str> {
        self.moved.get(path).map(String::as_str)
    }

    pub fn uncertain(&self, path: &str) -> bool {
        self.uncertain.contains(path)
    }

    pub fn remap(&self, path: &mut String) {
        if let Some(target) = self.target(path) {
            *path = target.to_owned();
        }
    }

    pub fn sidebar(&self, state: &mut State) {
        for (path, _) in &mut state.recent {
            self.remap(path);
        }
        let mut seen = BTreeSet::new();
        state.recent.retain(|(path, _)| seen.insert(path.clone()));
        for path in &mut state.pinned {
            self.remap(path);
        }
        seen.clear();
        state.pinned.retain(|path| seen.insert(path.clone()));
        state
            .moved_uncertain
            .retain(|path| !self.moved.contains_key(path) && !self.present.contains(path));
        for path in &self.uncertain {
            if !state.is_pinned(path) && !state.recent.iter().any(|(old, _)| old == path) {
                state.recent.insert(0, (path.clone(), 0));
            }
        }
        state.moved_uncertain.extend(self.uncertain.iter().cloned());
    }

    pub fn layout(&self, saved: &mut Layout) {
        self.remap(&mut saved.note);
        for visit in &mut saved.history {
            self.remap(&mut visit.note);
        }
    }
}

pub fn resolve(
    root: &Path,
    directory: Option<&Path>,
    extra: impl IntoIterator<Item = String>,
    inventory: Option<&Vault>,
) -> Plan {
    if !root.is_dir() {
        return Plan::default();
    }
    let saved = directory
        .map(|directory| State::load(&State::path(directory, root)))
        .unwrap_or_default();
    let refs: BTreeSet<_> = saved
        .recent
        .iter()
        .map(|(path, _)| path.clone())
        .chain(saved.pinned.iter().cloned())
        .chain(saved.moved_uncertain.iter().cloned())
        .chain(extra)
        .filter(|path| valid_path(path))
        .collect();

    let mut plan = Plan::default();
    let mut missing = Vec::new();
    for path in refs {
        match fs::symlink_metadata(root.join(&path)) {
            Ok(_) => {
                plan.present.insert(path);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => missing.push(path),
            Err(_) => {}
        }
    }
    if missing.is_empty() {
        return plan;
    }
    let identities: BTreeMap<_, _> = missing
        .into_iter()
        .filter_map(|path| {
            let identity = saved.path_identities.get(&path)?;
            Some((path, identity))
        })
        .collect();
    if !identities.is_empty() {
        match inventory {
            Some(vault) => match_identities(&mut plan, vault, &identities),
            None => {
                if let Ok(vault) = Vault::scan_metadata(root) {
                    match_identities(&mut plan, &vault, &identities);
                }
            }
        }
    }
    // Several saved sources converging on one target are unresolved even
    // when the individual lookups were unique.
    let mut claims: BTreeMap<String, usize> = BTreeMap::new();
    for target in plan.moved.values() {
        *claims.entry(target.clone()).or_default() += 1;
    }
    plan.moved.retain(|source, target| {
        if claims[target] == 1 {
            return true;
        }
        plan.uncertain.insert(source.clone());
        false
    });
    plan
}

fn match_identities(plan: &mut Plan, vault: &Vault, missing: &BTreeMap<String, &Identity>) {
    if !vault.inventory_complete {
        return;
    }
    let ids: BTreeSet<_> = missing
        .values()
        .filter_map(|identity| identity.id.as_ref())
        .collect();
    let sizes: BTreeSet<_> = missing
        .values()
        .filter_map(|identity| identity.hash.and(identity.size))
        .collect();
    let native = !ids.is_empty() && native_ids(&vault.root);
    let mut by_id: BTreeMap<FileId, Vec<&str>> = BTreeMap::new();
    let mut by_hash: BTreeMap<[u8; 32], Vec<&str>> = BTreeMap::new();
    let mut incomplete_hashes = BTreeSet::new();
    for entry in &vault.entries {
        let rel = &entry.path;
        let path = vault.root.join(rel);
        let Some(meta) = Identity::metadata(&path, native) else {
            // A changing inventory cannot establish uniqueness.
            return;
        };
        if let Some(id) = meta.id.filter(|id| ids.contains(id)) {
            by_id.entry(id).or_default().push(rel);
        }
        if let Some(size) = meta.size.filter(|size| sizes.contains(size)) {
            if let Some(hash) =
                Identity::read(&path, native, None).and_then(|identity| identity.hash)
            {
                by_hash.entry(hash).or_default().push(rel);
            } else {
                incomplete_hashes.insert(size);
            }
        }
    }
    for (old, identity) in missing {
        let same_id = identity.id.as_ref().and_then(|id| by_id.get(id));
        let matches = same_id.or_else(|| {
            identity
                .size
                .filter(|size| !incomplete_hashes.contains(size))?;
            by_hash.get(&identity.hash?)
        });
        match matches.map(Vec::as_slice) {
            Some([path]) if contained(&vault.root, path) => {
                plan.moved.insert(old.clone(), (*path).to_owned());
            }
            Some(_) => {
                plan.uncertain.insert(old.clone());
            }
            None => {}
        }
    }
}

pub fn save_sidebar(
    mut state: State,
    path: &Path,
    root: &Path,
    extra: Vec<String>,
    sequence: u64,
    latest: &std::sync::atomic::AtomicU64,
) -> anyhow::Result<()> {
    if latest.load(std::sync::atomic::Ordering::SeqCst) != sequence {
        return Ok(());
    }
    let previous = State::load(path).path_identities;
    let native = native_ids(root);
    state.path_identities = state
        .recent
        .iter()
        .map(|(path, _)| path.clone())
        .chain(state.pinned.iter().cloned())
        .chain(state.moved_uncertain.iter().cloned())
        .chain(extra)
        .filter(|rel| valid_path(rel))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|rel| {
            let old = previous.get(&rel);
            let current = contained(root, &rel)
                .then(|| Identity::read(&root.join(&rel), native, old))
                .flatten();
            current.or_else(|| old.cloned()).map(|id| (rel, id))
        })
        .collect();
    state.save_ordered(path, sequence, latest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn copied_note_uses_hash_and_duplicate_copies_stay_unresolved() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let directory = temp.path().join("app-state");
        write(&root, "old.md", "Hash witness");
        let root = root.canonicalize().unwrap();
        let state = State {
            pinned: vec!["old.md".into()],
            path_identities: BTreeMap::from([(
                "old.md".into(),
                Identity::read(&root.join("old.md"), false, None).unwrap(),
            )]),
            ..Default::default()
        };
        state.save(&State::path(&directory, &root)).unwrap();
        write(&root, "state/first.md", "Hash witness");
        fs::remove_file(root.join("old.md")).unwrap();

        let plan = resolve(&root, Some(&directory), [], None);
        assert_eq!(plan.target("old.md"), Some("state/first.md"));
        write(&root, "state/second.md", "Hash witness");
        let plan = resolve(&root, Some(&directory), [], None);
        let mut sidebar = state;
        plan.sidebar(&mut sidebar);
        assert!(plan.uncertain("old.md"));
        assert_eq!(sidebar.pinned, ["old.md"]);
        assert!(sidebar.moved_uncertain.contains("old.md"));
    }

    #[test]
    fn stable_file_id_survives_in_place_link_rewrite() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let directory = temp.path().join("app-state");
        write(&root, "old.md", "[Note](../old-target.md)");
        let root = root.canonicalize().unwrap();
        let identity = Identity::read(&root.join("old.md"), native_ids(&root), None).unwrap();
        if identity.id.is_none() {
            return;
        }
        let state = State {
            pinned: vec!["old.md".into()],
            path_identities: BTreeMap::from([("old.md".into(), identity)]),
            ..Default::default()
        };
        state.save(&State::path(&directory, &root)).unwrap();
        fs::rename(root.join("old.md"), root.join("new.md")).unwrap();
        write(&root, "new.md", "[Note](../../log/new-target.md)");
        let plan = resolve(&root, Some(&directory), [], None);
        assert_eq!(plan.target("old.md"), Some("new.md"));
    }
}
