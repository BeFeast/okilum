//! Cooperative local canonical-writer admission. This is NOT transport quiescence
//! or protection from external editors. Durable holds never release on Drop.
//! Short admission metadata locks serialize ordinary writers; drain polling is nonblocking.
//! First registration serializes account metadata on the vault-opening worker.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Identity(u64, u64, #[serde(default)] Option<u64>);
#[cfg(target_os = "macos")]
#[path = "identity_macos.rs"]
pub mod native_identity;
#[cfg(target_os = "macos")]
use native_identity::Identity;
#[cfg(target_os = "linux")]
pub mod identity_linux;
#[cfg(target_os = "linux")]
pub use identity_linux as filesystem_identity;
#[cfg(target_os = "linux")]
use identity_linux::Identity;
#[cfg(target_os = "macos")]
pub use native_identity as filesystem_identity;
#[cfg(all(test, target_os = "linux"))]
mod identity_linux_tests;
#[cfg(all(test, target_os = "macos"))]
mod identity_macos_tests;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod migration;

const BINDING_SCHEMA: u32 = if cfg!(any(target_os = "macos", target_os = "linux")) {
    2
} else {
    1
};

fn identity(path: &Path) -> Result<Identity> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        !meta.file_type().is_symlink(),
        "Writer authority path is a symlink"
    );
    #[cfg(target_os = "macos")]
    {
        native_identity::read(path)
    }
    #[cfg(target_os = "linux")]
    {
        identity_linux::read(path)
    }
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(Identity(
            meta.dev(),
            meta.ino(),
            meta.created()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|time| u64::try_from(time.as_nanos()).ok()),
        ))
    }
    #[cfg(windows)]
    {
        use std::os::windows::{
            fs::{MetadataExt, OpenOptionsExt},
            io::AsRawHandle,
        };
        use windows_sys::Win32::Storage::FileSystem::*;
        ensure!(
            meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
            "Reparse point in writer authority"
        );
        let f = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        ensure!(
            unsafe { GetFileInformationByHandle(f.as_raw_handle(), &mut info) } != 0,
            "Cannot verify writer authority identity"
        );
        Ok(Identity(
            info.dwVolumeSerialNumber as u64,
            ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
            Some(
                ((info.ftCreationTime.dwHighDateTime as u64) << 32)
                    | info.ftCreationTime.dwLowDateTime as u64,
            ),
        ))
    }
}

/// One account-wide location, shared by Reader, core writers and Sync. QA's
/// OKILUM_STATE_DIR is intentionally ignored: a second window cannot bypass a hold
/// by selecting different drafts/index/profile directories.
pub fn account_directory() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(|p| PathBuf::from(p).join("Library/Application Support/com.befeast.okilum"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|p| p.join("okilum"))
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/state/okilum")));
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("okilum"));
    let base = base
        .filter(|p| p.is_absolute())
        .context("No absolute account state directory for local writer authority")?;
    Ok(base.join("local-writers-v1"))
}
fn private_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let mut b = fs::DirBuilder::new();
        b.recursive(true).mode(0o700);
        b.create(path)?;
        let m = fs::symlink_metadata(path)?;
        ensure!(
            m.is_dir()
                && m.uid() == rustix::process::getuid().as_raw()
                && m.permissions().mode() & 0o077 == 0,
            "Writer authority directory must be private to this account"
        );
    }
    #[cfg(windows)]
    {
        fs::create_dir_all(path)?;
    }
    identity(path)?;
    Ok(())
}
fn file(path: &Path, create: bool) -> Result<File> {
    let mut o = OpenOptions::new();
    o.read(true).write(true).create(create).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let f = o.open(path)?;
    ensure!(
        f.metadata()?.is_file(),
        "Writer authority lock is not a file"
    );
    Ok(f)
}
fn exclusive(f: &File) -> Result<()> {
    f.try_lock().map_err(|e| {
        anyhow::anyhow!(
            "Local writer authority is busy; retry on a worker without discarding drafts: {e}"
        )
    })
}
fn read<T: for<'a> Deserialize<'a>>(p: &Path) -> Result<T> {
    ensure!(
        fs::symlink_metadata(p)?.is_file(),
        "Writer authority record is not a regular file"
    );
    let mut bytes = Vec::new();
    File::open(p)?.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 64 * 1024,
        "Writer authority record is oversized"
    );
    serde_json::from_slice(&bytes).context("Invalid writer authority; explicit recovery required")
}
fn persist(p: &Path, v: &impl Serialize, create: bool) -> Result<()> {
    let parent = p.parent().context("Authority parent missing")?;
    let mut t = tempfile::NamedTempFile::new_in(parent)?;
    t.write_all(&serde_json::to_vec(v)?)?;
    t.as_file().sync_all()?;
    if create {
        t.persist_noclobber(p).map_err(|e| e.error)?;
    } else {
        t.persist(p).map_err(|e| e.error)?;
    }
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}
/// The folder at a bound vault path is no longer the folder that was registered,
/// for example because Sync re-received it. Admission stays refused until the
/// user explicitly re-attaches it with [`Authority::rebind_replaced_root`].
#[derive(Debug)]
pub struct RootReplaced {
    root: PathBuf,
}
impl std::fmt::Display for RootReplaced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Vault root replaced; local writer admission refused")
    }
}
impl std::error::Error for RootReplaced {}
/// The bound vault root of a [`RootReplaced`] failure anywhere in `error`'s chain.
pub fn replaced_root(error: &anyhow::Error) -> Option<PathBuf> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<RootReplaced>())
        .map(|replaced| replaced.root.clone())
}
/// Only a plain folder at its own canonical path, owned by this account, can be
/// re-attached: never a symlink, a junction, or a path through a symlinked parent.
fn plain_folder(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir() && path.canonicalize()? == path,
        "Vault folder is a link or resolves elsewhere; re-attach refused"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            meta.uid() == rustix::process::getuid().as_raw(),
            "Vault folder belongs to another account; re-attach refused"
        );
    }
    identity(path)?;
    Ok(())
}
fn key(root: &Path) -> String {
    // Native path encoding preserves non-UTF8 filenames; canonicalize folds aliases.
    format!("{:x}", Sha256::digest(root.as_os_str().as_encoded_bytes()))
}
// Legacy Unix records have no filesystem ID. Treat a matching inode/birthtime as
// a candidate when looking for another scope: an unconverted or missing root
// must still fence a held physical alias, never authorize a second authority.
fn physical_candidate(current: &[Identity], stored: &Identity) -> bool {
    current.iter().any(|identity| {
        if identity == stored {
            return true;
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if stored.3.is_none() && stored.legacy_matches(identity) {
            return true;
        }
        false
    })
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Binding {
    schema: u32,
    root: PathBuf,
    root_identity: Identity,
    epoch: String,
    directory: Identity,
    admission: Identity,
    writers: Identity,
    owner: Identity,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Phase {
    Open,
    Draining,
    Held,
    RecoveryPending,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Snapshot {
    pub epoch: String,
    pub revision: u64,
    pub operation_id: Option<String>,
    pub phase: Phase,
}
/// Serialized fencing token; every transition consumes a revision. Replays,
/// another operation, and a replaced authority cannot complete/release a hold.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Token {
    pub epoch: String,
    pub revision: u64,
    pub operation_id: String,
}
#[derive(Clone, Debug)]
pub struct Authority {
    account: PathBuf,
    binding_path: PathBuf,
    directory: PathBuf,
    binding: Binding,
}
/// Durable operational reference for autonomous publishers. Reopening never
/// registers a root or repairs a missing/replaced authority. Keep outside the
/// vault and disposable indexes, alongside the consumer's own identity binding.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DurableBinding {
    account: PathBuf,
    binding: Binding,
}
impl DurableBinding {
    pub fn open(&self) -> Result<Authority> {
        let authority = Authority {
            account: self.account.clone(),
            binding_path: self
                .account
                .join(format!("{}.json", key(&self.binding.root))),
            directory: self.account.join(key(&self.binding.root)),
            binding: self.binding.clone(),
        };
        authority.validate()?;
        Ok(authority)
    }
}
impl Authority {
    pub fn durable_binding(&self) -> Result<DurableBinding> {
        let (authority, _lock) = self.locked()?;
        Ok(DurableBinding {
            account: authority.account,
            binding: authority.binding,
        })
    }
    /// Register/open an actual vault before opening editors. Nested publishers
    /// resolve to the existing enclosing authority rather than inventing one.
    pub fn for_root(root: &Path) -> Result<Self> {
        Self::open_in(&account_directory()?, root)
    }
    pub fn for_path(path: &Path) -> Result<Self> {
        Self::for_root(
            path.parent()
                .context("Canonical destination has no parent")?,
        )
    }
    fn open_in(account: &Path, root: &Path) -> Result<Self> {
        let root = root.canonicalize()?;
        ensure!(root.is_dir(), "Writer authority root is not a directory");
        ensure!(
            account.is_absolute()
                && !account
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir)),
            "Account authority needs an absolute path without parent traversal"
        );
        // Resolve the existing prefix before mkdir: an invalid account location
        // must not create operational files inside canonical data, even on error.
        let existing = account
            .ancestors()
            .find(|path| path.exists())
            .context("Account authority has no existing ancestor")?;
        let planned = existing
            .canonicalize()?
            .join(account.strip_prefix(existing)?);
        ensure!(
            !planned.starts_with(&root),
            "Writer authority must be outside the vault"
        );
        private_dir(account)?;
        let account = account.canonicalize()?;
        ensure!(
            !account.starts_with(&root),
            "Writer authority must be outside the vault"
        );
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        Self::migrate_legacy_bindings(&account, &root)?;
        if let Some(a) = Self::existing(&account, &root)? {
            return Ok(a);
        }
        let registry = file(&account.join("registry.lock"), true)?;
        registry
            .lock()
            .context("Cannot serialize vault authority registration")?;
        if let Some(a) = Self::existing(&account, &root)? {
            return Ok(a);
        }
        // Promote idle quick-file scopes under the registry lock. Keep their
        // records: existing handles resolve the new enclosing authority at admission.
        let mut enclosed = Vec::new();
        let ancestor_identities = root.ancestors().map(identity).collect::<Result<Vec<_>>>()?;
        for entry in fs::read_dir(&account)? {
            let p = entry?.path();
            if p.extension().is_some_and(|e| e == "json") {
                let b: Binding = read(&p)?;
                // Canonical path aliases such as bind mounts can still name
                // the same physical root. Never mint a second authority for it.
                if physical_candidate(&ancestor_identities, &b.root_identity) {
                    let a = Self {
                        account: account.clone(),
                        binding_path: p.clone(),
                        directory: account.join(key(&b.root)),
                        binding: b.clone(),
                    };
                    if identity(&b.root)
                        .is_ok_and(|current| physical_candidate(&[current], &b.root_identity))
                    {
                        a.validate()?;
                        return Ok(a);
                    }
                    // A removed inactive vault's inode may be reused for an
                    // unrelated new folder. An unfinished hold still fences a
                    // moved physical root even after its old path disappears.
                    ensure!(a.state()?.phase == Phase::Open,
                        "Physical vault identity has an unfinished hold at another root; explicit recovery required");
                }
                if b.root.starts_with(&root) && b.root != root {
                    let child = Self {
                        account: account.clone(),
                        binding_path: p.clone(),
                        directory: account.join(key(&b.root)),
                        binding: b,
                    };
                    let admission = file(&child.directory.join("admission.lock"), false)?;
                    admission.lock()?;
                    // A temporarily moved/deleted child root still owns its
                    // durable hold and outstanding leases. Missing paths never
                    // authorize an enclosing scope to bypass those fences.
                    if child.root().try_exists()? {
                        child.validate()?;
                    } else {
                        child.validate_binding()?;
                    }
                    ensure!(child.state()?.phase == Phase::Open,
                        "Enclosed folder has an unfinished writer hold; release or recover it before opening the enclosing vault");
                    let writers = file(&child.directory.join("writers.lock"), false)?;
                    writers.try_lock().context("Enclosed folder is publishing; retry opening the enclosing vault after it finishes")?;
                    enclosed.push((admission, writers));
                }
            }
        }
        let directory = account.join(key(&root));
        // No binding was published, so no caller can have acquired this scope.
        // Resume interrupted initialization under registry.lock, preserving any
        // unfinished durable hold rather than resetting operational state.
        private_dir(&directory)?;
        let state_path = directory.join("state.json");
        if state_path.try_exists()? {
            let initial: Snapshot = read(&state_path)?;
            ensure!(
                initial.phase == Phase::Open
                    && initial.revision == 0
                    && initial.operation_id.is_none()
                    && !initial.epoch.is_empty(),
                "Unbound authority is not an interrupted initial Open state; recovery required"
            );
        }
        for name in ["admission.lock", "writers.lock", "owner.lock"] {
            file(&directory.join(name), true)?.sync_all()?;
        }
        let binding = Binding {
            schema: BINDING_SCHEMA,
            root: root.clone(),
            root_identity: identity(&root)?,
            epoch: uuid::Uuid::new_v4().to_string(),
            directory: identity(&directory)?,
            admission: identity(&directory.join("admission.lock"))?,
            writers: identity(&directory.join("writers.lock"))?,
            owner: identity(&directory.join("owner.lock"))?,
        };
        persist(
            &state_path,
            &Snapshot {
                epoch: binding.epoch.clone(),
                revision: 0,
                operation_id: None,
                phase: Phase::Open,
            },
            !state_path.try_exists()?,
        )?;
        let binding_path = account.join(format!("{}.json", key(&root)));
        persist(&binding_path, &binding, true)?;
        Ok(Self {
            account,
            binding_path,
            directory,
            binding,
        })
    }
    fn existing(account: &Path, root: &Path) -> Result<Option<Self>> {
        for ancestor in root.ancestors().collect::<Vec<_>>().into_iter().rev() {
            let binding_path = account.join(format!("{}.json", key(ancestor)));
            if binding_path.try_exists()? {
                let binding: Binding = read(&binding_path)?;
                ensure!(
                    binding.root == ancestor,
                    "Writer authority root binding mismatch"
                );
                let a = Self {
                    directory: account.join(key(ancestor)),
                    account: account.to_owned(),
                    binding_path,
                    binding,
                };
                a.validate()?;
                return Ok(Some(a));
            }
        }
        Ok(None)
    }
    pub fn root(&self) -> &Path {
        &self.binding.root
    }
    fn validate(&self) -> Result<()> {
        #[cfg(test)]
        VALIDATIONS.with(|count| count.set(count.get() + 1));
        if self.binding.root.canonicalize()? != self.binding.root
            || identity(&self.binding.root)? != self.binding.root_identity
        {
            return Err(RootReplaced {
                root: self.binding.root.clone(),
            }
            .into());
        }
        self.validate_binding()
    }
    /// Explicit user recovery after [`RootReplaced`] (Retry): re-admit the folder
    /// now at the bound path when it was re-created in place. Fresh lookups never
    /// do this on their own. Only the recorded root identity changes: the epoch
    /// and any unfinished hold are kept, so a hold still closes admission and
    /// authorities or leases minted for the old folder stay refused.
    pub fn rebind_replaced_root(root: &Path) -> Result<Self> {
        Self::rebind_in(&account_directory()?, root)
    }
    fn rebind_in(account: &Path, root: &Path) -> Result<Self> {
        // Check before open_in: a symlinked path must not even register its target.
        plain_folder(root)?;
        let bound = match Self::open_in(account, root) {
            Ok(authority) => return Ok(authority),
            Err(error) => replaced_root(&error).ok_or(error)?,
        };
        let account = account.canonicalize()?;
        let registry = file(&account.join("registry.lock"), false)?;
        registry
            .lock()
            .context("Cannot serialize vault authority registration")?;
        let binding_path = account.join(format!("{}.json", key(&bound)));
        let binding: Binding = read(&binding_path)?;
        ensure!(
            binding.root == bound,
            "Writer authority root binding mismatch"
        );
        let old = Self {
            directory: account.join(key(&bound)),
            account,
            binding_path,
            binding,
        };
        let admission = file(&old.directory.join("admission.lock"), false)?;
        admission
            .lock()
            .context("Cannot serialize local writer admission")?;
        old.validate_binding()?;
        plain_folder(&bound)?;
        let owner = file(&old.directory.join("owner.lock"), false)?;
        owner
            .try_lock()
            .context("A local writer hold is still running; re-attach refused")?;
        let writers = file(&old.directory.join("writers.lock"), false)?;
        writers
            .try_lock()
            .context("A writer still uses the replaced vault folder; retry after it finishes")?;
        // Never mint a second authority for one physical folder, as open_in.
        let ancestors = bound
            .ancestors()
            .map(identity)
            .collect::<Result<Vec<_>>>()?;
        for entry in fs::read_dir(&old.account)? {
            let p = entry?.path();
            if p == old.binding_path || p.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let b: Binding = read(&p)?;
            if physical_candidate(&ancestors, &b.root_identity) {
                ensure!(
                    !identity(&b.root)
                        .is_ok_and(|current| physical_candidate(&[current], &b.root_identity)),
                    "Another vault authority owns this folder; re-attach refused"
                );
                let other = Self {
                    account: old.account.clone(),
                    directory: old.account.join(key(&b.root)),
                    binding_path: p,
                    binding: b,
                };
                ensure!(other.state()?.phase == Phase::Open,
                    "Physical vault identity has an unfinished hold at another root; explicit recovery required");
            }
        }
        let binding = Binding {
            root_identity: identity(&bound)?,
            ..old.binding.clone()
        };
        persist(&old.binding_path, &binding, false)?;
        let rebound = Self { binding, ..old };
        rebound.validate()?;
        Ok(rebound)
    }
    fn validate_binding(&self) -> Result<()> {
        ensure!(
            self.binding.schema == BINDING_SCHEMA,
            "Unsupported local writer authority"
        );
        ensure!(
            read::<Binding>(&self.binding_path)? == self.binding,
            "Writer authority replaced; stale authority refused"
        );
        ensure!(
            identity(&self.directory)? == self.binding.directory
                && identity(&self.directory.join("admission.lock"))? == self.binding.admission
                && identity(&self.directory.join("writers.lock"))? == self.binding.writers
                && identity(&self.directory.join("owner.lock"))? == self.binding.owner,
            "Writer authority identity replaced; recovery required"
        );
        ensure!(
            !self.account.starts_with(&self.binding.root),
            "Authority is inside canonical vault"
        );
        Ok(())
    }
    // Read the outermost published scope without validating each candidate.
    // The selected scope is validated exactly once, under its admission lock.
    fn enclosing(&self) -> Result<Option<Self>> {
        for ancestor in self
            .root()
            .ancestors()
            .skip(1)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            let binding_path = self.account.join(format!("{}.json", key(ancestor)));
            if binding_path.try_exists()? {
                let binding: Binding = read(&binding_path)?;
                ensure!(binding.root == ancestor, "Writer scope binding mismatch");
                return Ok(Some(Self {
                    account: self.account.clone(),
                    directory: self.account.join(key(ancestor)),
                    binding_path,
                    binding,
                }));
            }
        }
        Ok(None)
    }
    fn locked(&self) -> Result<(Self, File)> {
        let mut authority = self.enclosing()?.unwrap_or_else(|| self.clone());
        loop {
            let f = file(&authority.directory.join("admission.lock"), false)?;
            // This protects only bounded metadata work, never a writer's whole
            // lifetime. Contention is normal and must not become a failed save.
            f.lock()
                .context("Cannot serialize local writer admission")?;
            if let Some(parent) = authority.enclosing()? {
                drop(f);
                authority = parent;
                continue;
            }
            authority.validate()?;
            return Ok((authority, f));
        }
    }
    fn state(&self) -> Result<Snapshot> {
        let s: Snapshot = read(&self.directory.join("state.json"))?;
        ensure!(
            s.epoch == self.binding.epoch && (s.phase == Phase::Open) == s.operation_id.is_none(),
            "Invalid/replaced writer state; recovery required"
        );
        Ok(s)
    }
    fn save(&self, s: &Snapshot) -> Result<()> {
        persist(&self.directory.join("state.json"), s, false)
    }
    /// Never waits for writers. Dropped/crashed holders leave admission closed. A fresh reader
    /// additionally records RecoveryPending when the owner lock has disappeared.
    pub fn snapshot(&self) -> Result<Snapshot> {
        let (authority, _l) = self.locked()?;
        let mut s = authority.state()?;
        if matches!(s.phase, Phase::Draining | Phase::Held) {
            let owner = file(&authority.directory.join("owner.lock"), false)?;
            if owner.try_lock().is_ok() {
                s.phase = Phase::RecoveryPending;
                s.revision = next(s.revision)?;
                authority.save(&s)?;
            }
        }
        Ok(s)
    }
    pub fn admit(&self) -> Result<WriterLease> {
        if let Some(lease) = ACTIVE.with(|a| {
            a.borrow()
                .iter()
                .find(|l| {
                    l.0.authority.binding == self.binding
                        || (l.0.authority.account == self.account
                            && self.root().starts_with(l.0.authority.root()))
                })
                .cloned()
        }) {
            lease.validate()?;
            return Ok(lease);
        }
        let (authority, _l) = self.locked()?;
        let s = authority.state()?;
        ensure!(s.phase == Phase::Open, "Canonical publication deferred by local writer hold {:?} (operation {:?}); draft and recovery must be retained", s.phase, s.operation_id);
        let writers = file(&authority.directory.join("writers.lock"), false)?;
        writers
            .try_lock_shared()
            .context("Local writer drain is active; retain draft and retry")?;

        Ok(WriterLease(Arc::new(LeaseInner {
            authority: authority.clone(),
            _writers: writers,
            revision: s.revision,
        })))
    }
    /// Admit and enter one immediate synchronous operation. Delayed jobs use
    /// admit() and enter() separately, revalidating when publication resumes.
    pub fn operation(&self) -> Result<OperationGuard> {
        Ok(self.admit()?.enter_validated())
    }
    /// Close admission durably BEFORE testing/draining existing writers. Caller
    /// retains Hold and polls try_hold on a worker; there is no UI-thread wait.
    pub fn begin_drain(&self, operation_id: &str) -> Result<Hold> {
        ensure!(
            !operation_id.is_empty()
                && operation_id.len() <= 128
                && !operation_id.chars().any(char::is_control),
            "Invalid hold operation identity"
        );
        let (authority, _l) = self.locked()?;
        let mut s = authority.state()?;
        ensure!(
            s.phase == Phase::Open,
            "An unfinished local writer hold already exists"
        );
        let owner = file(&authority.directory.join("owner.lock"), false)?;
        exclusive(&owner)?;
        s.phase = Phase::Draining;
        s.operation_id = Some(operation_id.into());
        s.revision = next(s.revision)?;
        authority.save(&s)?;
        Ok(Hold {
            authority: authority.clone(),
            token: token(&s)?,
            _owner: owner,
        })
    }
    /// Explicit recovery adopts the exact observed state after the former holder
    /// has died/closed. It does not grant transport ownership or resume anything.
    pub fn recover(&self, observed: &Snapshot) -> Result<Hold> {
        let (authority, _l) = self.locked()?;
        let mut s = authority.state()?;
        ensure!(
            &s == observed && s.phase != Phase::Open,
            "Stale recovery observation refused"
        );
        let owner = file(&authority.directory.join("owner.lock"), false)?;
        exclusive(&owner)?;
        s.phase = Phase::RecoveryPending;
        s.revision = next(s.revision)?;
        authority.save(&s)?;
        Ok(Hold {
            authority: authority.clone(),
            token: token(&s)?,
            _owner: owner,
        })
    }
}
fn next(revision: u64) -> Result<u64> {
    revision
        .checked_add(1)
        .context("Writer authority revision exhausted")
}
fn token(s: &Snapshot) -> Result<Token> {
    Ok(Token {
        epoch: s.epoch.clone(),
        revision: s.revision,
        operation_id: s.operation_id.clone().context("No hold operation")?,
    })
}
/// Local writers drained only. Never an ExclusiveSession or daemon receipt.
pub struct Hold {
    authority: Authority,
    token: Token,
    _owner: File,
}
impl Hold {
    pub fn root(&self) -> &Path {
        self.authority.root()
    }
    /// Validate an already drained local ownership capability. Transport
    /// adapters must separately prove that their own publishers have stopped.
    pub fn validate_held(&self, expected: &Token) -> Result<()> {
        let (_authority, _lock) = self.authority.locked()?;
        let state = self.authority.state()?;
        ensure!(
            state.phase == Phase::Held && expected == &self.token && token(&state)? == *expected,
            "Local writer hold is stale or not drained"
        );
        Ok(())
    }
    pub fn token(&self) -> &Token {
        &self.token
    }
    pub fn try_hold(&mut self, expected: &Token) -> Result<bool> {
        let (_authority, _l) = self.authority.locked()?;
        let mut s = self.authority.state()?;
        ensure!(
            expected == &self.token && token(&s)? == *expected,
            "Stale/replayed hold token refused"
        );
        if s.phase == Phase::Held {
            return Ok(true);
        }
        let writers = file(&self.authority.directory.join("writers.lock"), false)?;
        match writers.try_lock() {
            Err(std::fs::TryLockError::WouldBlock) => return Ok(false),
            Err(e) => return Err(e.into()),
            Ok(()) => {}
        }
        s.phase = Phase::Held;
        s.revision = next(s.revision)?;
        self.authority.save(&s)?;
        self.token = token(&s)?;
        Ok(true)
    }
    /// Explicit release/cancel after the owner has reconciled its operation.
    /// Refuses while a previously admitted operation/rollback is still alive.
    pub fn release(self, expected: &Token) -> Result<()> {
        let (_authority, _l) = self.authority.locked()?;
        let mut s = self.authority.state()?;
        ensure!(
            expected == &self.token && token(&s)? == *expected,
            "Stale/replayed release refused"
        );
        let writers = file(&self.authority.directory.join("writers.lock"), false)?;
        exclusive(&writers)?;
        s.phase = Phase::Open;
        s.operation_id = None;
        s.revision = next(s.revision)?;
        self.authority.save(&s)
    }
}
struct LeaseInner {
    authority: Authority,
    _writers: File,
    revision: u64,
}
#[derive(Clone)]
pub struct WriterLease(Arc<LeaseInner>);
thread_local! { static ACTIVE: RefCell<Vec<WriterLease>> = const { RefCell::new(Vec::new()) }; }
impl WriterLease {
    pub fn validate(&self) -> Result<()> {
        self.0.authority.validate()?;
        let state = self.0.authority.state()?;
        ensure!(
            state.revision >= self.0.revision && state.phase != Phase::Held,
            "Writer lease state replaced or rolled back; publication refused"
        );
        Ok(())
    }
    /// Explicit operation inheritance: nested synchronous writers borrow this
    /// admission, including rollback after drain closes. Delayed jobs retain a
    /// clone and enter a scope on their worker, never acquire a competing lease.
    pub fn scope<T>(&self, body: impl FnOnce() -> Result<T>) -> Result<T> {
        self.validate()?;
        struct Pop;
        impl Drop for Pop {
            fn drop(&mut self) {
                ACTIVE.with(|a| {
                    a.borrow_mut().pop();
                });
            }
        }
        ACTIVE.with(|a| a.borrow_mut().push(self.clone()));
        let _pop = Pop;
        body()
    }
}
pub fn with_root<T>(root: &Path, body: impl FnOnce() -> Result<T>) -> Result<T> {
    let _operation = operation(root)?;
    body()
}
pub fn with_path<T>(path: &Path, body: impl FnOnce() -> Result<T>) -> Result<T> {
    let _operation = operation_for_path(path)?;
    body()
}

/// Synchronous operation scope. It is intentionally !Send: transfer a cloned
/// WriterLease into a delayed job, then call enter on that worker instead.
pub struct OperationGuard {
    _lease: WriterLease,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl Drop for OperationGuard {
    fn drop(&mut self) {
        ACTIVE.with(|a| {
            a.borrow_mut().pop();
        });
    }
}
impl WriterLease {
    pub fn enter(&self) -> Result<OperationGuard> {
        self.validate()?;
        Ok(self.enter_validated())
    }
    fn enter_validated(&self) -> OperationGuard {
        ACTIVE.with(|a| a.borrow_mut().push(self.clone()));
        OperationGuard {
            _lease: self.clone(),
            _thread: std::marker::PhantomData,
        }
    }
}
pub fn operation(root: &Path) -> Result<OperationGuard> {
    Authority::for_root(root)?.operation()
}
pub fn operation_for_path(path: &Path) -> Result<OperationGuard> {
    Authority::for_path(path)?.operation()
}

/// Resolve a destination folder before create_dir_all. Never create canonical
/// scaffolding before admission; all missing suffixes remain under this scope.
pub fn operation_for_directory(directory: &Path) -> Result<OperationGuard> {
    let existing = directory
        .ancestors()
        .find(|p| p.is_dir())
        .context("Destination has no existing parent")?;
    operation(existing)
}

#[cfg(test)]
thread_local! { static VALIDATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Authority) {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir(t.path().join("vault")).unwrap();
        let a = Authority::open_in(&t.path().join("account"), &t.path().join("vault")).unwrap();
        (t, a)
    }
    #[test]
    fn local_writer_admission_durable_sync_binding_never_recreates_missing_authority() -> Result<()>
    {
        let (_temp, authority) = fixture();
        let binding = authority.durable_binding()?;
        let encoded = serde_json::to_vec(&binding)?;
        let reopened: DurableBinding = serde_json::from_slice(&encoded)?;
        let lease = reopened.open()?.admit()?;
        drop(lease);
        fs::remove_file(&authority.binding_path)?;
        assert!(reopened.open().is_err());
        assert!(!authority.binding_path.exists());
        Ok(())
    }
    #[test]
    fn local_writer_admission_review_parallel_normal_writers_all_succeed() {
        let (_t, a) = fixture();
        let barrier = Arc::new(std::sync::Barrier::new(16));
        std::thread::scope(|scope| {
            let mut jobs = vec![];
            for index in 0..16 {
                let a = &a;
                let barrier = &barrier;
                jobs.push(scope.spawn(move || {
                    barrier.wait();
                    for iteration in 0..16 {
                        let _operation = a.operation().unwrap();
                        fs::write(a.root().join(format!("{index}-{iteration}.md")), b"saved")
                            .unwrap();
                    }
                }));
            }
            for job in jobs {
                job.join().unwrap();
            }
        });
        assert_eq!(fs::read_dir(a.root()).unwrap().count(), 256);
        let hold = a.begin_drain("positive-hold-control").unwrap();
        assert!(a.operation().is_err());
        let token = hold.token().clone();
        hold.release(&token).unwrap();
        a.operation().unwrap();
    }
    #[test]
    fn local_writer_admission_review_synchronous_operation_validates_once() {
        let (_t, a) = fixture();
        VALIDATIONS.with(|count| count.set(0));
        let operation = a.operation().unwrap();
        assert_eq!(VALIDATIONS.with(|count| count.get()), 1);
        drop(operation);
        // Delayed publications still validate when their worker resumes.
        let lease = a.admit().unwrap();
        VALIDATIONS.with(|count| count.set(0));
        let _operation = lease.enter().unwrap();
        assert_eq!(VALIDATIONS.with(|count| count.get()), 1);
    }
    #[test]
    fn local_writer_admission_review_subfolder_first_promotes_and_old_handles_follow_root() {
        let (t, _) = fixture();
        let root = t.path().join("quick-vault");
        let sub = root.join("Sub");
        let sibling = root.join("Other");
        fs::create_dir_all(&sub).unwrap();
        fs::create_dir(&sibling).unwrap();
        let account = t.path().join("quick-account");
        let first = Authority::open_in(&account, &sub).unwrap();
        let other = Authority::open_in(&account, &sibling).unwrap();
        let lease = first.admit().unwrap();
        assert!(
            Authority::open_in(&account, &root).is_err(),
            "live lease fences promotion"
        );
        let moved_sub = t.path().join("moved-sub");
        fs::rename(&sub, &moved_sub).unwrap();
        assert!(
            Authority::open_in(&account, &root).is_err(),
            "a moved root's live lease also fences promotion"
        );
        fs::rename(&moved_sub, &sub).unwrap();
        drop(lease);
        let hold = other.begin_drain("subfolder-hold").unwrap();
        assert!(
            Authority::open_in(&account, &root).is_err(),
            "hold cannot be bypassed by promotion"
        );
        let moved_other = t.path().join("moved-other");
        fs::rename(&sibling, &moved_other).unwrap();
        assert!(
            Authority::open_in(&account, &root).is_err(),
            "a missing child path cannot bypass its durable hold"
        );
        fs::rename(&moved_other, &sibling).unwrap();
        let token = hold.token().clone();
        hold.release(&token).unwrap();
        let vault = Authority::open_in(&account, &root).unwrap();
        assert_eq!(
            Authority::open_in(&account, &sub).unwrap().root(),
            vault.root()
        );
        assert_eq!(
            Authority::open_in(&account, &sibling).unwrap().root(),
            vault.root()
        );
        first.operation().unwrap();
        other.operation().unwrap();
        let hold = vault.begin_drain("whole-vault").unwrap();
        for authority in [&first, &other, &vault] {
            assert!(authority.admit().is_err());
        }
        let token = hold.token().clone();
        hold.release(&token).unwrap();
        let _operation = vault.operation().unwrap();
        // An editor that predates promotion can join its parent's rollback.
        first.operation().unwrap();
        other.operation().unwrap();
    }
    #[test]
    fn local_writer_admission_review_interrupted_initialization_is_resumable() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("vault");
        fs::create_dir(&root).unwrap();
        let account = t.path().join("account");
        let directory = account.join(key(&root.canonicalize().unwrap()));
        private_dir(&directory).unwrap();
        file(&directory.join("admission.lock"), true).unwrap();
        let a = Authority::open_in(&account, &root).unwrap();
        a.operation().unwrap();
        assert_eq!(a.snapshot().unwrap().phase, Phase::Open);
        // Crash after state.json but before the binding was published.
        fs::remove_file(&a.binding_path).unwrap();
        let recovered = Authority::open_in(&account, &root).unwrap();
        assert_ne!(recovered.binding.epoch, a.binding.epoch);
        recovered.operation().unwrap();
        assert!(a.admit().is_err(), "the interrupted epoch cannot publish");
        // A binding lost after a real hold is operational damage, not initial
        // setup. Never erase its fencing revisions as initialization recovery.
        let hold = recovered.begin_drain("completed-hold").unwrap();
        let token = hold.token().clone();
        hold.release(&token).unwrap();
        fs::remove_file(&recovered.binding_path).unwrap();
        let before = fs::read(recovered.directory.join("state.json")).unwrap();
        assert!(Authority::open_in(&account, &root).is_err());
        assert_eq!(
            fs::read(recovered.directory.join("state.json")).unwrap(),
            before
        );
    }
    #[test]
    fn local_writer_admission_drain_closes_before_existing_lease_and_nested_rollback() {
        let (_t, a) = fixture();
        let lease = a.admit().unwrap();
        let mut hold = a.begin_drain("merge-1").unwrap();
        assert!(a.admit().is_err());
        let token = hold.token().clone();
        assert!(!hold.try_hold(&token).unwrap());
        // Nested production helpers must be able to complete/roll back while
        // admission is closed, using the SAME lease, never a competing flock.
        lease
            .scope(|| {
                let nested = a.admit()?;
                nested.validate()
            })
            .unwrap();
        drop(lease);
        assert!(hold.try_hold(&token).unwrap());
        assert!(hold.try_hold(&token).is_err());
        assert_eq!(a.snapshot().unwrap().phase, Phase::Held);
        let token = hold.token().clone();
        hold.release(&token).unwrap();
        a.admit().unwrap();
    }
    #[test]
    fn local_writer_admission_restart_requires_explicit_fenced_recovery() {
        let (_t, a) = fixture();
        let hold = a.begin_drain("lost-holder").unwrap();
        let stale = hold.token().clone();
        drop(hold);
        let restarted = Authority::open_in(&a.account, a.root()).unwrap();
        assert!(restarted.admit().is_err());
        let observed = restarted.snapshot().unwrap();
        assert_eq!(observed.phase, Phase::RecoveryPending);
        let mut recovered = restarted.recover(&observed).unwrap();
        assert!(restarted.recover(&observed).is_err());
        assert!(recovered.try_hold(&stale).is_err());
        let token = recovered.token().clone();
        assert!(recovered.try_hold(&token).unwrap());
        let token = recovered.token().clone();
        recovered.release(&token).unwrap();
        assert!(restarted.recover(&observed).is_err());
        restarted.admit().unwrap();
    }
    #[test]
    fn local_writer_admission_replaced_authority_root_and_state_fail_closed() {
        let (_t, a) = fixture();
        let lease = a.admit().unwrap();
        let original = a.directory.join("writers.lock");
        fs::rename(&original, a.directory.join("old-writers.lock")).unwrap();
        file(&original, true).unwrap();
        assert!(a.admit().is_err());
        assert!(lease.validate().is_err());
        assert!(Authority::open_in(&a.account, a.root()).is_err());
        let (_t, a) = fixture();
        fs::rename(a.root(), a.root().with_extension("old")).unwrap();
        fs::create_dir(a.root()).unwrap();
        assert!(a.admit().is_err());
        assert!(Authority::open_in(&a.account, a.root()).is_err());
        let (_t, a) = fixture();
        let mut s = a.snapshot().unwrap();
        s.epoch = "substitute".into();
        persist(&a.directory.join("state.json"), &s, false).unwrap();
        assert!(a.admit().is_err());
    }
    #[test]
    fn local_writer_admission_unrelated_vaults_and_aliases_use_correct_authority() {
        let (t, a) = fixture();
        let hold = a.begin_drain("first").unwrap();
        let nested = a.root().join("nested");
        fs::create_dir(&nested).unwrap();
        let b = Authority::open_in(&a.account, &nested).unwrap();
        assert_eq!(a.binding, b.binding);
        assert!(b.admit().is_err());
        let other = t.path().join("other");
        fs::create_dir(&other).unwrap();
        let b = Authority::open_in(&a.account, &other).unwrap();
        b.admit().unwrap();
        let token = hold.token().clone();
        hold.release(&token).unwrap();
    }
}

impl std::fmt::Debug for WriterLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriterLease").finish_non_exhaustive()
    }
}
/// Keep the admission alive through a delayed publication/rollback even when its
/// destination folder has not been created yet.
pub fn admit_directory(directory: &Path) -> Result<WriterLease> {
    let existing = directory
        .ancestors()
        .find(|p| p.is_dir())
        .context("Destination has no existing parent")?;
    Authority::for_root(existing)?.admit()
}

#[cfg(test)]
mod root_relocation_tests {
    use super::*;
    #[test]
    fn local_writer_admission_moved_held_root_cannot_mint_new_authority() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("vault");
        fs::create_dir(&root).unwrap();
        let account = t.path().join("account");
        let a = Authority::open_in(&account, &root).unwrap();
        let h = a.begin_drain("move-held-root").unwrap();
        let moved = t.path().join("moved");
        fs::rename(&root, &moved).unwrap();
        assert!(Authority::open_in(&account, &moved).is_err());
        fs::rename(&moved, &root).unwrap();
        let token = h.token().clone();
        h.release(&token).unwrap();
        fs::rename(&root, &moved).unwrap();
        let fresh = Authority::open_in(&account, &moved).unwrap();
        assert_ne!(fresh.binding.epoch, a.binding.epoch);
        fresh.admit().unwrap();
    }
}

#[cfg(test)]
mod account_location_tests {
    use super::*;
    #[test]
    fn local_writer_admission_rejects_in_vault_state_before_creating_any_directory() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("vault");
        fs::create_dir(&root).unwrap();
        assert!(Authority::open_in(&root.join("private/account"), &root).is_err());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        #[cfg(unix)]
        {
            let alias = t.path().join("alias");
            std::os::unix::fs::symlink(&root, &alias).unwrap();
            assert!(Authority::open_in(&alias.join("private/account"), &root).is_err());
            assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        }
        let allowed = Authority::open_in(&t.path().join("private/account"), &root).unwrap();
        allowed.admit().unwrap();
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    }
}

#[cfg(test)]
mod root_rebind_tests {
    use super::*;
    /// A vault at `base/vault`, bound once, with its account outside the vault.
    fn bound() -> (tempfile::TempDir, PathBuf, Authority) {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().canonicalize().unwrap().join("base");
        fs::create_dir_all(base.join("vault")).unwrap();
        let account = t.path().join("account");
        let a = Authority::open_in(&account, &base.join("vault")).unwrap();
        (t, account, a)
    }
    /// Sync re-receives the vault: a new folder at the same path.
    fn recreate(root: &Path, aside: &str) -> PathBuf {
        let aside = root.with_file_name(aside);
        fs::rename(root, &aside).unwrap();
        fs::create_dir(root).unwrap();
        fs::write(root.join("note.md"), b"re-received").unwrap();
        aside
    }
    #[test]
    fn local_writer_admission_recreated_root_reattaches_only_by_explicit_rebind() {
        let (_t, account, a) = bound();
        let root = a.root().to_path_buf();
        recreate(&root, "vault.previous");
        let error = Authority::open_in(&account, &root)
            .unwrap_err()
            .context("Prepare vault");
        assert_eq!(replaced_root(&error), Some(root.clone()));
        assert!(
            Authority::open_in(&account, &root).is_err(),
            "a fresh lookup never accepts the swapped folder by itself"
        );
        let rebound = Authority::rebind_in(&account, &root).unwrap();
        assert_eq!(rebound.binding.epoch, a.binding.epoch);
        {
            let _operation = Authority::open_in(&account, &root)
                .unwrap()
                .operation()
                .unwrap();
            fs::write(root.join("saved.md"), b"saved").unwrap();
        }
        assert!(
            a.admit().is_err(),
            "authorities minted for the old folder stay fenced"
        );
        assert_eq!(
            Authority::rebind_in(&account, &root).unwrap().binding,
            rebound.binding,
            "re-attaching a healthy vault changes nothing"
        );
        // The old folder may also be gone entirely.
        let aside = recreate(&root, "vault.again");
        fs::remove_dir_all(aside).unwrap();
        assert!(Authority::open_in(&account, &root).is_err());
        Authority::rebind_in(&account, &root)
            .unwrap()
            .operation()
            .unwrap();
    }
    #[test]
    fn local_writer_admission_rebind_waits_for_writers_and_never_bypasses_a_hold() {
        let (_t, account, a) = bound();
        let root = a.root().to_path_buf();
        let lease = a.admit().unwrap();
        recreate(&root, "vault.previous");
        assert!(
            Authority::rebind_in(&account, &root).is_err(),
            "a live writer still uses the old folder"
        );
        drop(lease);
        Authority::rebind_in(&account, &root).unwrap();

        let (_t, account, a) = bound();
        let root = a.root().to_path_buf();
        let hold = a.begin_drain("sync-merge").unwrap();
        recreate(&root, "vault.previous");
        assert!(
            Authority::rebind_in(&account, &root).is_err(),
            "a running hold owner is never disturbed"
        );
        // The holder exits without releasing: its durable hold remains.
        drop(hold);
        let rebound = Authority::rebind_in(&account, &root).unwrap();
        assert!(
            rebound.admit().is_err(),
            "re-attaching never reopens admission closed by a hold"
        );
        let observed = rebound.snapshot().unwrap();
        assert_eq!(observed.phase, Phase::RecoveryPending);
        let recovered = rebound.recover(&observed).unwrap();
        let token = recovered.token().clone();
        recovered.release(&token).unwrap();
        rebound.admit().unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn local_writer_admission_rebind_refuses_symlink_swaps() {
        let (_t, account, a) = bound();
        let root = a.root().to_path_buf();
        let base = root.parent().unwrap().to_path_buf();
        let binding = fs::read(&a.binding_path).unwrap();
        let registered = || fs::read_dir(&a.account).unwrap().count();
        let before = registered();
        // The vault path itself becomes a symlink to another folder.
        let elsewhere = base.with_file_name("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        fs::rename(&root, base.join("vault.previous")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &root).unwrap();
        assert!(Authority::rebind_in(&account, &root).is_err());
        // A plain folder reached through a symlinked parent.
        fs::rename(&base, base.with_file_name("base.previous")).unwrap();
        fs::create_dir_all(elsewhere.join("vault")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &base).unwrap();
        assert!(root.is_dir());
        assert!(Authority::rebind_in(&account, &root).is_err());
        assert_eq!(registered(), before, "a symlinked path registers nothing");
        assert_eq!(fs::read(&a.binding_path).unwrap(), binding);
        assert!(a.admit().is_err());
    }
}
