//! Explicit native QA scope. This is separate from the account service: neither
//! a Reader profile nor an arbitrary helper --state argument selects it.
use crate::private;
use anyhow::{ensure, Context, Result};
use okilum_write_authority::filesystem_identity::{self, Identity};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Component, Path, PathBuf},
    sync::OnceLock,
};
use uuid::Uuid;

const MARKER: &str = "isolated-sync-profile.json";
static SELECTED: OnceLock<Profile> = OnceLock::new();

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    schema: u32,
    id: Uuid,
    root: PathBuf,
    identity: Identity,
    reader_home: PathBuf,
    reader_state: PathBuf,
}

#[derive(Clone)]
pub struct Profile(Marker);

fn plain(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute()
            && path
                .components()
                .all(|part| matches!(part, Component::RootDir | Component::Normal(_))),
        "plain absolute QA path required"
    );
    Ok(())
}
fn canonical_location(path: &Path) -> Result<PathBuf> {
    plain(path)?;
    let mut ancestor = path;
    while !ancestor.try_exists()? {
        ancestor = ancestor
            .parent()
            .context("QA path has no existing ancestor")?;
    }
    Ok(ancestor.canonicalize()?.join(path.strip_prefix(ancestor)?))
}
fn descendant(root: &Path, path: &Path) -> Result<()> {
    plain(path)?;
    let relative = path
        .strip_prefix(root)
        .context("QA path is outside its profile")?;
    ensure!(
        !relative.as_os_str().is_empty(),
        "QA control directory cannot be the profile root"
    );
    let mut current = root.to_owned();
    for part in relative.components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "QA control path contains a link or non-directory"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
fn controls(root: &Path, home: &Path, state: &Path) -> Result<()> {
    descendant(root, home)?;
    descendant(root, state)?;
    ensure!(
        !home.starts_with(state) && !state.starts_with(home),
        "QA Reader HOME and state must be separate"
    );
    for reserved in [
        root.join("Sync"),
        root.join("SyncDirect"),
        root.join("vaults"),
    ] {
        descendant(root, &reserved)?;
        ensure!(
            [home, state]
                .iter()
                .all(|path| !path.starts_with(&reserved) && !reserved.starts_with(path)),
            "QA Reader controls overlap Sync state or synthetic vaults"
        );
    }
    Ok(())
}
impl Profile {
    /// Explicit app launch only. The root already exists, is private and cannot
    /// overlap the real account control plane. No service is registered here.
    pub fn prepare(
        root: &Path,
        reader_home: &Path,
        reader_state: &Path,
        account: &Path,
    ) -> Result<Self> {
        plain(root)?;
        plain(account)?;
        ensure!(
            !fs::symlink_metadata(root)?.file_type().is_symlink(),
            "QA root is a symlink"
        );
        let root = root.canonicalize()?;
        let account = canonical_location(account)?;
        let reader_home = canonical_location(reader_home)?;
        let reader_state = canonical_location(reader_state)?;
        ensure!(
            !root.starts_with(&account) && !account.starts_with(&root),
            "QA root overlaps account Sync state"
        );
        private::directory(&root)?;
        controls(&root, &reader_home, &reader_state)?;
        let _lock = private::lock(&root)?;
        let path = root.join(MARKER);
        let marker = if path.try_exists()? {
            let saved: Marker = serde_json::from_slice(&private::read(&path)?)?;
            ensure!(
                saved.reader_home == reader_home && saved.reader_state == reader_state,
                "QA Reader profile selection changed"
            );
            saved
        } else {
            let saved = Marker {
                schema: 1,
                id: Uuid::new_v4(),
                identity: filesystem_identity::read(&root)?,
                root: root.clone(),
                reader_home: reader_home.to_owned(),
                reader_state: reader_state.to_owned(),
            };
            private::write(&path, &serde_json::to_vec(&saved)?)?;
            saved
        };
        let profile = Self(marker);
        ensure!(profile.0.root == root, "QA profile root binding changed");
        profile.verify()?;
        Ok(profile)
    }
    /// The helper opens the app's existing marker; it never creates an isolated
    /// profile or infers permission from a missing account service.
    pub fn open(state: &Path, account: &Path) -> Result<Self> {
        plain(state)?;
        ensure!(
            state.file_name().is_some_and(|name| name == "Sync"),
            "isolated helper state name differs"
        );
        let root = state.parent().context("QA root missing")?;
        let account = canonical_location(account)?;
        let marker: Marker = serde_json::from_slice(&private::read(&root.join(MARKER))?)?;
        ensure!(
            marker.root == root && !root.starts_with(&account) && !account.starts_with(root),
            "isolated helper root differs or overlaps account state"
        );
        let profile = Self(marker);
        profile.verify()?;
        Ok(profile)
    }
    pub fn verify(&self) -> Result<()> {
        ensure!(
            self.0.schema == 1
                && !self.0.id.is_nil()
                && filesystem_identity::read(&self.0.root)? == self.0.identity,
            "QA profile directory was replaced"
        );
        private::directory(&self.0.root)?;
        controls(&self.0.root, &self.0.reader_home, &self.0.reader_state)?;
        let current: Marker = serde_json::from_slice(&private::read(&self.0.root.join(MARKER))?)?;
        ensure!(
            serde_json::to_vec(&current)? == serde_json::to_vec(&self.0)?,
            "QA profile marker changed"
        );
        Ok(())
    }
    pub fn state(&self) -> PathBuf {
        self.0.root.join("Sync")
    }
    pub fn root(&self) -> &Path {
        &self.0.root
    }
    pub fn require_vault(&self, path: &Path) -> Result<()> {
        self.verify()?;
        let canonical = path.canonicalize()?;
        let vaults = self.0.root.join("vaults");
        ensure!(
            canonical.starts_with(&vaults) && canonical != vaults,
            "isolated Sync accepts only synthetic folders under its vaults directory"
        );
        ensure!(
            !canonical.starts_with(&self.0.reader_home)
                && !canonical.starts_with(&self.0.reader_state),
            "synthetic vault overlaps Reader control state"
        );
        Ok(())
    }
}

pub fn select(profile: Profile) -> Result<()> {
    profile.verify()?;
    if let Some(current) = SELECTED.get() {
        ensure!(
            current.0.id == profile.0.id && current.0.root == profile.0.root,
            "Sync profile cannot change during a process lifetime"
        );
    } else {
        SELECTED
            .set(profile)
            .map_err(|_| anyhow::anyhow!("Sync profile selected concurrently"))?;
    }
    Ok(())
}
pub fn selected() -> Result<Option<&'static Profile>> {
    let profile = SELECTED.get();
    if let Some(profile) = profile {
        profile.verify()?;
    }
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn separate_profile_reopens_without_touching_the_account_or_its_files() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("qa");
        private::directory(&root)?;
        let account = temporary.path().join("account");
        fs::create_dir(&account)?;
        fs::write(account.join("sentinel"), b"existing account")?;
        let profile = Profile::prepare(
            &root,
            &root.join("home"),
            &root.join("reader-state"),
            &account,
        )?;
        let reopened = Profile::open(&profile.state(), &account)?;
        assert_eq!(profile.0.id, reopened.0.id);
        assert_eq!(fs::read(account.join("sentinel"))?, b"existing account");
        assert_eq!(fs::read_dir(&account)?.count(), 1);
        assert!(
            !profile.state().exists(),
            "scope selection does not prepare or launch a daemon"
        );
        fs::create_dir(root.join("vaults"))?;
        fs::create_dir(root.join("vaults/here"))?;
        profile.require_vault(&root.join("vaults/here"))?;
        assert!(profile.require_vault(&account).is_err());
        Ok(())
    }
    #[test]
    fn overlapping_account_controls_and_redirected_profile_paths_are_refused() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path();
        assert!(Profile::prepare(
            root,
            &root.join("home"),
            &root.join("state"),
            &root.join("account")
        )
        .is_err());
        let outside = tempfile::tempdir()?;
        std::os::unix::fs::symlink(outside.path(), root.join("home"))?;
        assert!(Profile::prepare(
            root,
            &root.join("home"),
            &root.join("state"),
            outside.path()
        )
        .is_err());
        assert!(!root.join(MARKER).exists());
        Ok(())
    }
    #[test]
    fn reader_state_cannot_be_enrolled_as_a_synthetic_vault_or_helper_home() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path();
        let account = tempfile::tempdir()?;
        for state in [
            root.join("Sync"),
            root.join("SyncDirect"),
            root.join("vaults/state"),
        ] {
            assert!(Profile::prepare(root, &root.join("home"), &state, account.path()).is_err());
            assert!(!root.join(MARKER).exists());
        }
        Ok(())
    }
    #[test]
    fn an_existing_marker_cannot_adopt_a_replaced_root() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("qa");
        private::directory(&root)?;
        let account = temporary.path().join("account");
        let profile = Profile::prepare(&root, &root.join("home"), &root.join("state"), &account)?;
        fs::rename(&root, temporary.path().join("retained-original"))?;
        private::directory(&root)?;
        fs::copy(
            temporary.path().join("retained-original").join(MARKER),
            root.join(MARKER),
        )?;
        assert!(profile.verify().is_err());
        assert!(Profile::open(&profile.state(), &account).is_err());
        Ok(())
    }
}
