//! Explicit native QA scope. This is separate from the account service: neither
//! a Reader profile nor an arbitrary helper --state argument selects it.
use crate::private;
use anyhow::{ensure, Context, Result};
use okilum_write_authority::filesystem_identity::Identity;
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
                identity: crate::managed_launch::identity(&root)?,
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
                && crate::managed_launch::identity(&self.0.root)? == self.0.identity,
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
    pub fn require_controller(&self, state: &Path, configs: &[PathBuf]) -> Result<()> {
        self.verify()?;
        ensure!(
            state == self.0.root.join("SyncDirect"),
            "QA direct state is outside its selected profile"
        );
        self.require_configurations(configs)
    }
    pub fn require_configurations(&self, configs: &[PathBuf]) -> Result<()> {
        self.verify()?;
        let expected = self.state().join("config/config.xml");
        ensure!(
            configs.is_empty() || configs == [expected],
            "QA selection cannot adopt an account or external runtime"
        );
        Ok(())
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

/// The isolated path reuses the ordinary revision-bound registration dispatch,
/// but starts the verified bundled helper directly instead of registering the
/// account's launchd service. Only that helper may consume the one-use claim.
#[cfg(target_os = "macos")]
pub(crate) struct NativeLaunchPort {
    profile: Profile,
    channel: Option<std::os::unix::net::UnixStream>,
}
#[cfg(target_os = "macos")]
impl NativeLaunchPort {
    pub(crate) fn new(profile: Profile) -> Self {
        Self {
            profile,
            channel: None,
        }
    }
}
#[cfg(target_os = "macos")]
impl super::registration::RegistrationPort for NativeLaunchPort {
    fn status(&mut self, deadline: std::time::Instant) -> Result<super::Status> {
        use crate::sidecar::{authority::launch::Phase, store::UnixStore};
        self.profile.verify()?;
        let store = UnixStore::open_existing(&self.profile.state())?;
        let tx = store.begin(deadline)?;
        let saved = tx.current()?.context("QA launch journal missing")?;
        Ok(
            match saved
                .launch_registration()
                .context("QA launch intent missing")?
                .phase()
            {
                // Dispatch consumes permission before RegistrationGuard's final
                // native status check. It is not itself a native registration.
                // A reopened port cannot redispatch a spent journal even when
                // no child was started: dispatch_launch still requires Ready.
                Phase::Ready | Phase::Dispatched if self.channel.is_none() => {
                    super::Status::NotRegistered
                }
                _ => super::Status::Enabled,
            },
        )
    }
    fn register(&mut self, deadline: std::time::Instant) -> Result<()> {
        use crate::sidecar::{authority::launch::Phase, store::UnixStore, Intent};
        use std::{
            os::{
                fd::AsRawFd,
                unix::{fs::OpenOptionsExt, net::UnixStream, process::CommandExt},
            },
            process::{Command, Stdio},
        };
        self.profile.verify()?;
        ensure!(
            std::time::Instant::now() < deadline,
            "QA dispatch deadline elapsed"
        );
        let store = UnixStore::open_existing(&self.profile.state())?;
        let binding = {
            let tx = store.begin(deadline)?;
            let saved = tx.current()?.context("QA launch journal missing")?;
            ensure!(
                saved.intent() == Intent::Enabled
                    && saved.stop().is_none()
                    && saved.update().is_none()
                    && saved
                        .launch_registration()
                        .is_some_and(|launch| launch.phase() == Phase::Dispatched),
                "QA launch requires the dispatched one-use permission"
            );
            saved.binding().clone()
        };
        ensure!(
            Path::new(&binding.state_directory) == self.profile.state(),
            "QA binding changed"
        );
        let (parent, child) = UnixStream::pair()?;
        let descriptor = child.as_raw_fd();
        let log = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.profile.state().join("isolated-supervisor.log"))?;
        let mut command = Command::new(&binding.supervisor);
        command
            .arg("--state")
            .arg(self.profile.state())
            .arg("--instance")
            .arg(binding.instance.to_string())
            .env("OKILUM_SYNC_QA_PARENT_FD", "3")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log));
        // Only this child inherits the launch channel. All other descriptors
        // retain CLOEXEC, including the app side of the socket pair.
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(descriptor, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        self.channel = Some(parent);
        Ok(())
    }
}

/// Validate the explicit helper root AND the inherited signed parent before
/// opening its journal or consuming launch permission. Ordinary launchd starts
/// never call this function and continue using the account-derived location.
#[cfg(target_os = "macos")]
pub fn authorize_helper(
    state: &Path,
    app: &Path,
    app_requirement: crate::sidecar::supervisor::ipc::code_requirement::CodeRequirement,
) -> Result<Profile> {
    use crate::sidecar::supervisor::ipc::macos_peer::SignedPeer;
    use std::os::{fd::FromRawFd, unix::net::UnixStream};
    let descriptor: i32 = std::env::var("OKILUM_SYNC_QA_PARENT_FD")
        .context("QA helper requires its signed parent launch channel")?
        .parse()?;
    ensure!(descriptor == 3, "unexpected QA parent descriptor");
    let stream = unsafe { UnixStream::from_raw_fd(descriptor) };
    SignedPeer::new(app_requirement, app)?.verify_parent(&stream)?;
    Profile::open(state, &super::account_state_directory()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    #[test]
    fn dispatched_permission_passes_native_preflight_without_rearming() -> Result<()> {
        use super::super::{registration::RegistrationPort, Status};
        use crate::sidecar::{authority::Envelope, store::UnixStore, Binding};
        use std::time::{Duration, Instant};

        let temporary = tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)?;
        let root = temporary.path().join("qa");
        private::directory(&root)?;
        let profile = Profile::prepare(
            &root,
            &root.join("home"),
            &root.join("reader"),
            &temporary.path().join("account"),
        )?;
        private::directory(&profile.state())?;
        let store = UnixStore::open_existing(&profile.state())?;
        let binding = Binding {
            installation: Uuid::new_v4(),
            instance: Uuid::new_v4(),
            owner: rustix::process::geteuid().as_raw().to_string(),
            supervisor: "/usr/bin/true".into(),
            state_directory: profile.state().to_str().unwrap().into(),
            device_identity: "fixture-device".into(),
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        store
            .begin(deadline)?
            .commit(Envelope::first_with_launch(binding.clone()))?;
        let mut port = NativeLaunchPort::new(profile.clone());
        assert_eq!(port.status(deadline)?, Status::NotRegistered);
        let generation = store
            .dispatch_launch(&binding, deadline)?
            .execute(|generation| {
                // This is RegistrationGuard's exact post-dispatch preflight, before
                // it may invoke the one native effect. The old port reported Enabled.
                assert_eq!(
                    port.status(deadline)?,
                    Status::NotRegistered,
                    "dispatch permission was mistaken for native registration"
                );
                assert!(store.dispatch_launch(&binding, deadline).is_err());
                let mut reopened = NativeLaunchPort::new(profile.clone());
                assert_eq!(reopened.status(deadline)?, Status::NotRegistered);
                // A disposable child exercises the real port's spawn/channel effect;
                // signed parent/helper authentication has separate native controls.
                port.register(deadline)?;
                assert!(port.channel.is_some());
                assert_eq!(port.status(deadline)?, Status::Enabled);
                Ok(generation)
            })?;
        assert_eq!(store.claim_launch(&binding, deadline)?, generation);
        assert_eq!(
            NativeLaunchPort::new(profile).status(deadline)?,
            Status::Enabled
        );
        assert!(store.claim_launch(&binding, deadline).is_err());
        assert!(store.dispatch_launch(&binding, deadline).is_err());
        Ok(())
    }

    #[test]
    fn separate_profile_reopens_without_touching_the_account_or_its_files() -> Result<()> {
        let temporary = tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)?;
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
        profile.require_controller(&root.join("SyncDirect"), &[])?;
        profile.require_controller(
            &root.join("SyncDirect"),
            &[profile.state().join("config/config.xml")],
        )?;
        assert!(profile
            .require_controller(&account.join("SyncDirect"), &[])
            .is_err());
        assert!(profile
            .require_controller(
                &root.join("SyncDirect"),
                &[account.join("config/config.xml")]
            )
            .is_err());
        assert!(profile.require_vault(&root.join("reader-state")).is_err());
        Ok(())
    }
    #[test]
    fn overlapping_account_controls_and_redirected_profile_paths_are_refused() -> Result<()> {
        let temporary = tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)?;
        let root = temporary.path();
        assert!(Profile::prepare(
            root,
            &root.join("home"),
            &root.join("state"),
            &root.join("account")
        )
        .is_err());
        let outside = tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)?;
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
        let temporary = tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)?;
        let root = temporary.path();
        let account = tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)?;
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
        let temporary = tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)?;
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
