//! Several writer scopes on volumes without a volume UUID (#3204).
//!
//! On macOS a network or FUSE volume (NFS, SMB, macFUSE) is identified by a
//! digest of its mount source spelling (#3202), so one export mounted twice
//! (by IP address and by hostname) shows one folder under two paths and two
//! digests. Paths therefore cannot relate such scopes. Three rules keep one
//! fencing scope per physical folder:
//!
//! - Same folder: `Identity::mount_alias_candidate` (filesystem type, inode,
//!   birth time) matches a folder under every spelling.
//! - Enclosure: each UUID-less scope records the identities of its strict
//!   ancestors on its own mount (`ancestry.json` in its private directory) when
//!   it is registered. That record outlives the mount, so a parent opened
//!   through another mount finds the child even when the child's mount is gone,
//!   and child handles resolve the parent scope at admission.
//! - Stale binding: a UUID-less binding whose path no longer reaches its folder
//!   (the mount is gone, or holds another folder) is retired, when it is idle,
//!   once its folder is opened through another path. A hold or a live writer
//!   keeps it and refuses the new path.
//!
//! Other platforms and native volumes keep no record and are unaffected.
use super::*;

const ANCESTRY: &str = "ancestry.json";
/// Scopes may enclose scopes; a chain longer than this is a corrupt registry.
pub(super) const MAX_SCOPE_HOPS: usize = 64;

/// The identity names its volume by mount source (macOS volume without UUID).
pub(super) fn mount_source(identity: &Identity) -> bool {
    #[cfg(target_os = "macos")]
    {
        identity.is_mount_source()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = identity;
        false
    }
}

/// The strict ancestors of a UUID-less root on its own mount, innermost first,
/// from the identities of the root and all its ancestors.
pub(super) fn mount_ancestry(chain: &[Identity]) -> Vec<Identity> {
    #[cfg(target_os = "macos")]
    if let Some((root, ancestors)) = chain.split_first() {
        return ancestors
            .iter()
            .take_while(|ancestor| root.same_mount(ancestor))
            .cloned()
            .collect();
    }
    let _ = chain;
    Vec::new()
}

impl Authority {
    /// Record the mount ancestry of a newly registered UUID-less scope.
    pub(super) fn record_ancestry(directory: &Path, chain: &[Identity]) -> Result<()> {
        if chain.first().is_some_and(mount_source) {
            persist(&directory.join(ANCESTRY), &mount_ancestry(chain), false)?;
        }
        Ok(())
    }

    /// The recorded mount ancestry; empty for native scopes. `None` when a
    /// UUID-less scope registered before #3204 has no record and its root
    /// cannot be read to write one now: its enclosure is then unknown.
    pub(super) fn ancestry(&self) -> Result<Option<Vec<Identity>>> {
        if !mount_source(&self.binding.root_identity) {
            return Ok(Some(Vec::new()));
        }
        let path = self.directory.join(ANCESTRY);
        if path.try_exists()? {
            return read(&path).map(Some);
        }
        let Ok(chain) = self
            .binding
            .root
            .ancestors()
            .map(identity)
            .collect::<Result<Vec<_>>>()
        else {
            return Ok(None);
        };
        if chain[0] != self.binding.root_identity {
            return Ok(None);
        }
        let ancestry = mount_ancestry(&chain);
        if let Err(error) = persist(&path, &ancestry, true) {
            // Another process recorded it first.
            return if path.try_exists()? {
                read(&path).map(Some)
            } else {
                Err(error)
            };
        }
        Ok(Some(ancestry))
    }

    /// The outermost registered scope that physically encloses this UUID-less
    /// scope through another path (a second mount of the export). Admission
    /// resolves it like a path ancestor, so one scope fences both paths.
    pub(super) fn alias_enclosing(&self) -> Result<Option<Self>> {
        if !mount_source(&self.binding.root_identity) {
            return Ok(None);
        }
        let record = self.directory.join(ANCESTRY);
        if !record.try_exists()? {
            return Ok(None);
        }
        let ancestry: Vec<Identity> = read(&record)?;
        if ancestry.is_empty() {
            return Ok(None);
        }
        let mut outermost: Option<(usize, Self)> = None;
        for entry in fs::read_dir(&self.account)? {
            let path = entry?.path();
            if path == self.binding_path || path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let binding: Binding = read(&path)?;
            let Some(depth) = ancestry.iter().rposition(|ancestor| {
                physical_candidate(std::slice::from_ref(ancestor), &binding.root_identity)
            }) else {
                continue;
            };
            if outermost.as_ref().is_none_or(|(known, _)| depth > *known) {
                outermost = Some((
                    depth,
                    Self {
                        account: self.account.clone(),
                        directory: self.account.join(key(&binding.root)),
                        binding_path: path,
                        binding,
                    },
                ));
            }
        }
        Ok(outermost.map(|(_, scope)| scope))
    }

    /// Whether `self` physically encloses `root` through another path. `None`:
    /// unknown, because `self` predates the ancestry record and is unreadable.
    pub(super) fn encloses_by_alias(&self, root: &Identity) -> Result<Option<bool>> {
        if !mount_source(&self.binding.root_identity) || !mount_source(root) {
            return Ok(Some(false));
        }
        Ok(self
            .ancestry()?
            .map(|ancestry| physical_candidate(&ancestry, root)))
    }

    /// The binding's path no longer reaches its folder: the mount is gone or
    /// the path now holds another folder. Other errors (a stalled server) are
    /// not proof and stay errors.
    pub(super) fn unmounted(&self) -> Result<bool> {
        match identity(&self.binding.root) {
            Ok(current) => Ok(!physical_candidate(&[current], &self.binding.root_identity)),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(true)
            }
            Err(error) => Err(error.context(format!(
                "Cannot tell whether {} is still mounted",
                self.binding.root.display()
            ))),
        }
    }

    /// Retire an idle UUID-less binding whose path no longer reaches its folder,
    /// because that folder is being opened through another path. Callers hold
    /// `registry.lock`. A hold or a live writer keeps the binding and refuses.
    /// Handles of the retired binding fail closed: their record is gone.
    pub(super) fn retire_unmounted(&self) -> Result<()> {
        let admission = file(&self.directory.join("admission.lock"), false)?;
        admission
            .lock()
            .context("Cannot serialize local writer admission")?;
        self.validate_binding()?;
        ensure!(
            self.state()?.phase == Phase::Open,
            "{} has an unfinished writer hold and its network volume is not mounted there; mount it again and finish or recover the hold",
            self.binding.root.display()
        );
        let owner = file(&self.directory.join("owner.lock"), false)?;
        owner
            .try_lock()
            .context("A local writer hold is still running on the unmounted path; retry")?;
        let writers = file(&self.directory.join("writers.lock"), false)?;
        writers
            .try_lock()
            .context("A writer still uses the unmounted path; retry after it finishes")?;
        fs::rename(
            &self.binding_path,
            self.binding_path.with_extension("retired"),
        )?;
        #[cfg(unix)]
        File::open(&self.account)?.sync_all()?;
        Ok(())
    }

    /// An existing UUID-less scope whose alias-enclosing scope was registered
    /// through a mount that is gone: retire that idle parent so this scope can
    /// admit writers again. A held parent keeps fencing (admission refuses).
    pub(super) fn retire_unmounted_enclosure(&self) -> Result<()> {
        let Some(parent) = self.alias_enclosing()? else {
            return Ok(());
        };
        if !mount_source(&parent.binding.root_identity) || !parent.unmounted()? {
            return Ok(());
        }
        let registry = file(&self.account.join("registry.lock"), true)?;
        registry
            .lock()
            .context("Cannot serialize vault authority registration")?;
        // Re-read under the registry lock: another opener may have retired it.
        if read::<Binding>(&parent.binding_path).ok().as_ref() == Some(&parent.binding) {
            parent.retire_unmounted()?;
        }
        Ok(())
    }
}
