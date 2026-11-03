//! Cooperative local canonical-writer admission. This is NOT transport quiescence
//! or protection from external editors. Durable holds never release on Drop.
//! Admission/hold transitions are nonblocking: drain polling belongs on a worker.
//! First registration serializes account metadata on the vault-opening worker.
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Identity(u64, u64);
fn identity(path: &Path) -> Result<Identity> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        !meta.file_type().is_symlink(),
        "Writer authority path is a symlink"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(Identity(meta.dev(), meta.ino()))
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
    Ok(serde_json::from_slice(&bytes)
        .context("Invalid writer authority; explicit recovery required")?)
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
fn key(root: &Path) -> String {
    // Native path encoding preserves non-UTF8 filenames; canonicalize folds aliases.
    format!("{:x}", Sha256::digest(root.as_os_str().as_encoded_bytes()))
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
impl Authority {
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
        private_dir(account)?;
        let account = account.canonicalize()?;
        ensure!(
            !account.starts_with(&root),
            "Writer authority must be outside the vault"
        );
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
        // An old single-folder authority cannot silently become a different
        // vault authority. Surface this ambiguity instead of bypassing its hold.
        let ancestor_identities = root.ancestors().map(identity).collect::<Result<Vec<_>>>()?;
        for entry in fs::read_dir(&account)? {
            let p = entry?.path();
            if p.extension().is_some_and(|e| e == "json") {
                let b: Binding = read(&p)?;
                // Canonical path aliases such as bind mounts can still name
                // the same physical root. Never mint a second authority for it.
                if ancestor_identities.contains(&b.root_identity) {
                    let a = Self {
                        account: account.clone(),
                        binding_path: p,
                        directory: account.join(key(&b.root)),
                        binding: b,
                    };
                    a.validate()?;
                    return Ok(a);
                }
                if b.root.starts_with(&root) && b.root != root && b.root.exists() {
                    bail!("An enclosed folder already has writer authority; reopen that folder or explicitly reconcile vault scopes");
                }
            }
        }
        let directory = account.join(key(&root));
        ensure!(
            !directory.try_exists()?,
            "Incomplete writer authority initialization; explicit recovery required"
        );
        private_dir(&directory)?;
        for name in ["admission.lock", "writers.lock", "owner.lock"] {
            file(&directory.join(name), true)?.sync_all()?;
        }
        let binding = Binding {
            schema: 1,
            root: root.clone(),
            root_identity: identity(&root)?,
            epoch: uuid::Uuid::new_v4().to_string(),
            directory: identity(&directory)?,
            admission: identity(&directory.join("admission.lock"))?,
            writers: identity(&directory.join("writers.lock"))?,
            owner: identity(&directory.join("owner.lock"))?,
        };
        persist(
            &directory.join("state.json"),
            &Snapshot {
                epoch: binding.epoch.clone(),
                revision: 0,
                operation_id: None,
                phase: Phase::Open,
            },
            true,
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
        ensure!(
            self.binding.schema == 1,
            "Unsupported local writer authority"
        );
        ensure!(
            self.binding.root.canonicalize()? == self.binding.root
                && identity(&self.binding.root)? == self.binding.root_identity,
            "Vault root replaced; local writer admission refused"
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
    fn locked(&self) -> Result<File> {
        self.validate()?;
        let f = file(&self.directory.join("admission.lock"), false)?;
        exclusive(&f)?;
        self.validate()?;
        Ok(f)
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
        self.validate()?;
        persist(&self.directory.join("state.json"), s, false)
    }
    /// No waiting. Dropped/crashed holders leave admission closed. A fresh reader
    /// additionally records RecoveryPending when the owner lock has disappeared.
    pub fn snapshot(&self) -> Result<Snapshot> {
        let _l = self.locked()?;
        let mut s = self.state()?;
        if matches!(s.phase, Phase::Draining | Phase::Held) {
            let owner = file(&self.directory.join("owner.lock"), false)?;
            if owner.try_lock().is_ok() {
                s.phase = Phase::RecoveryPending;
                s.revision = next(s.revision)?;
                self.save(&s)?;
            }
        }
        Ok(s)
    }
    pub fn admit(&self) -> Result<WriterLease> {
        if let Some(lease) = ACTIVE.with(|a| {
            a.borrow()
                .iter()
                .find(|l| l.0.authority.binding == self.binding)
                .cloned()
        }) {
            lease.validate()?;
            return Ok(lease);
        }
        let _l = self.locked()?;
        let s = self.state()?;
        ensure!(s.phase == Phase::Open, "Canonical publication deferred by local writer hold {:?} (operation {:?}); draft and recovery must be retained", s.phase, s.operation_id);
        let writers = file(&self.directory.join("writers.lock"), false)?;
        writers
            .try_lock_shared()
            .context("Local writer drain is active; retain draft and retry")?;
        self.validate()?;
        Ok(WriterLease(Arc::new(LeaseInner {
            authority: self.clone(),
            _writers: writers,
        })))
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
        let _l = self.locked()?;
        let mut s = self.state()?;
        ensure!(
            s.phase == Phase::Open,
            "An unfinished local writer hold already exists"
        );
        let owner = file(&self.directory.join("owner.lock"), false)?;
        exclusive(&owner)?;
        s.phase = Phase::Draining;
        s.operation_id = Some(operation_id.into());
        s.revision = next(s.revision)?;
        self.save(&s)?;
        Ok(Hold {
            authority: self.clone(),
            token: token(&s)?,
            _owner: owner,
        })
    }
    /// Explicit recovery adopts the exact observed state after the former holder
    /// has died/closed. It does not grant transport ownership or resume anything.
    pub fn recover(&self, observed: &Snapshot) -> Result<Hold> {
        let _l = self.locked()?;
        let mut s = self.state()?;
        ensure!(
            &s == observed && s.phase != Phase::Open,
            "Stale recovery observation refused"
        );
        let owner = file(&self.directory.join("owner.lock"), false)?;
        exclusive(&owner)?;
        s.phase = Phase::RecoveryPending;
        s.revision = next(s.revision)?;
        self.save(&s)?;
        Ok(Hold {
            authority: self.clone(),
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
    pub fn token(&self) -> &Token {
        &self.token
    }
    pub fn try_hold(&mut self, expected: &Token) -> Result<bool> {
        let _l = self.authority.locked()?;
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
        let _l = self.authority.locked()?;
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
}
#[derive(Clone)]
pub struct WriterLease(Arc<LeaseInner>);
thread_local! { static ACTIVE: RefCell<Vec<WriterLease>> = const { RefCell::new(Vec::new()) }; }
impl WriterLease {
    pub fn validate(&self) -> Result<()> {
        self.0.authority.validate()
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
    Authority::for_root(root)?.admit()?.scope(body)
}
pub fn with_path<T>(path: &Path, body: impl FnOnce() -> Result<T>) -> Result<T> {
    Authority::for_path(path)?.admit()?.scope(body)
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
        ACTIVE.with(|a| a.borrow_mut().push(self.clone()));
        Ok(OperationGuard {
            _lease: self.clone(),
            _thread: std::marker::PhantomData,
        })
    }
}
pub fn operation(root: &Path) -> Result<OperationGuard> {
    Authority::for_root(root)?.admit()?.enter()
}
pub fn operation_for_path(path: &Path) -> Result<OperationGuard> {
    Authority::for_path(path)?.admit()?.enter()
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
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Authority) {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir(t.path().join("vault")).unwrap();
        let a = Authority::open_in(&t.path().join("account"), &t.path().join("vault")).unwrap();
        (t, a)
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
