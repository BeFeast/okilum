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
//! - Enclosure: a new UUID-less root is disjoint from an existing UUID-less
//!   scope only when that is proved: the scope's folder is reached now (through
//!   its own path, or through the new root's mount at the same path below the
//!   mount root) and both lie below one physical mount root, so each one's
//!   ancestor chain shows any enclosure. Otherwise (nested or other exports, a
//!   folder that cannot be reached) the new root is refused, except for two
//!   NFS exports that the export witness proves disjoint (#3211). A scope inside the
//!   new root is promoted; its record of ancestors (`ancestry.json`) is then
//!   rewritten from the chain read now, and its handles use it to resolve the
//!   enclosing scope at admission. The record never proves disjointness.
//! - Stale binding: a UUID-less binding whose path no longer reaches its folder
//!   (the mount is gone, or holds another folder) is retired, when it is idle,
//!   once its folder is opened through another path. A hold or a live writer
//!   keeps it and refuses the new path.
//!
//! - Two exports (#3211): mounts of different exports share no mount root, so
//!   their chains cannot show enclosure. On NFS the server assigns fileid and
//!   so the export witness reads fileids live: neither root's fileid is on
//!   the other's chain, and a bounded walk of each folder finds no directory
//!   with a fileid of the other's chain. Birth time is ignored there (NFSv3
//!   reports none, NFSv4 the server's). Anything the walk cannot read, a
//!   directory not on NFS, another filesystem type, or a folder too large to
//!   walk stays refused.
//!
//! Other platforms and native volumes keep no record and are unaffected.
use super::*;

const ANCESTRY: &str = "ancestry.json";
/// A retired binding, moved into its own scope directory. One rename both
/// unpublishes the binding and marks the directory for setting aside.
const RETIRED: &str = "retired-binding.json";
/// Scopes may enclose scopes; a chain longer than this is a corrupt registry.
pub(super) const MAX_SCOPE_HOPS: usize = 64;
/// The export witness walks at most this many directories per folder; a
/// larger folder cannot be proved disjoint and stays refused (#3211).
pub(super) const MAX_WITNESS_DIRECTORIES: usize = 100_000;

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

/// The folder and its same-mount ancestors, innermost first.
fn mount_chain(chain: &[Identity]) -> Vec<Identity> {
    let mut own = chain[..1].to_vec();
    own.extend(mount_ancestry(chain));
    own
}

/// Export witness (#3211): no directory in or below `folder` has the NFS
/// fileid of one of `chain`. `None`: the walk could not read everything, met
/// a directory not on NFS, or met its bound, so nothing is proved. Symbolic
/// links are not followed.
fn walk_avoids(folder: &Path, chain: &[Identity]) -> Option<bool> {
    let mut pending = vec![folder.to_path_buf()];
    let mut seen = 0;
    while let Some(directory) = pending.pop() {
        seen += 1;
        if seen > MAX_WITNESS_DIRECTORIES {
            return None;
        }
        let current = identity(&directory).ok()?;
        if !nfs(&current) {
            return None;
        }
        let current = std::slice::from_ref(&current);
        if chain.iter().any(|stored| fileid_overlap(current, stored)) {
            return Some(false);
        }
        for entry in fs::read_dir(&directory).ok()? {
            let entry = entry.ok()?;
            if entry.file_type().ok()?.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Some(true)
}

/// One of `current` has the NFS fileid of `stored`, whatever the birth times
/// (#3212 review: one folder over NFSv3 and NFSv4 has two birth times).
fn fileid_overlap(current: &[Identity], stored: &Identity) -> bool {
    #[cfg(target_os = "macos")]
    {
        current
            .iter()
            .any(|identity| identity.nfs_fileid_match(stored))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (current, stored);
        false
    }
}

/// Two folders on mounts with different mount roots (two exports, #3211).
/// Their chains end at their mount roots, so an export nested in the other is
/// invisible to them; it shows up only below the wider folder. Both folders
/// are on NFS, whose fileid the server assigns under every mount, and they
/// are disjoint only when neither root's fileid is on the other's chain and
/// neither folder contains a directory with a fileid of the other's chain.
/// Birth time never excludes a match. Everything else is `Unknown`.
fn export_witness(
    folder: &Path,
    chain: &[Identity],
    root: &Path,
    root_chain: &[Identity],
) -> Overlap {
    if !nfs(&chain[0]) || !nfs(&root_chain[0]) {
        return Overlap::Unknown;
    }
    let (own, other) = (mount_chain(chain), mount_chain(root_chain));
    if fileid_overlap(&own, &root_chain[0]) || fileid_overlap(&other, &chain[0]) {
        return Overlap::Unknown;
    }
    match (walk_avoids(folder, &other), walk_avoids(root, &own)) {
        (Some(true), Some(true)) => Overlap::Disjoint,
        _ => Overlap::Unknown,
    }
}

/// The identity is on an NFS mount (#3211).
fn nfs(identity: &Identity) -> bool {
    #[cfg(target_os = "macos")]
    {
        identity.is_filesystem(b"nfs")
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = identity;
        false
    }
}

/// How an existing UUID-less scope relates to a folder being registered.
pub(super) enum Overlap {
    Disjoint,
    /// The scope lies inside the folder; its ancestry as read now.
    Encloses(Vec<Identity>),
    /// Overlap cannot be ruled out: refuse.
    Unknown,
}

impl Authority {
    /// Record the mount ancestry of a newly registered UUID-less scope.
    pub(super) fn record_ancestry(directory: &Path, chain: &[Identity]) -> Result<()> {
        if chain.first().is_some_and(mount_source) {
            persist(&directory.join(ANCESTRY), &mount_ancestry(chain), false)?;
        }
        Ok(())
    }

    /// The recorded mount ancestry, if any. It routes admission to an
    /// enclosing scope; it is never proof that two scopes are disjoint.
    pub(super) fn recorded_ancestry(&self) -> Result<Option<Vec<Identity>>> {
        let path = self.directory.join(ANCESTRY);
        if !path.try_exists()? {
            return Ok(None);
        }
        read(&path).map(Some)
    }

    /// Replace the record with ancestry read now (a promoted child, #3204).
    /// Callers hold this scope's admission lock.
    pub(super) fn rewrite_ancestry(&self, ancestry: Vec<Identity>) -> Result<()> {
        persist(&self.directory.join(ANCESTRY), &ancestry, false)
    }

    /// The current identities of this UUID-less scope's folder and all its
    /// ancestors, read through its own path or, when that no longer reaches
    /// the folder, through `mount` (the mount root of the folder being opened)
    /// at the same path below the mount root, with the path that reached it.
    /// `None`: neither reaches it.
    fn current_chain(&self, mount: &Path) -> Result<Option<(PathBuf, Vec<Identity>)>> {
        let reach = |path: &Path| -> Option<(PathBuf, Vec<Identity>)> {
            let chain = path
                .ancestors()
                .map(identity)
                .collect::<Result<Vec<_>>>()
                .ok()?;
            physical_candidate(std::slice::from_ref(&chain[0]), &self.binding.root_identity)
                .then(|| (path.to_path_buf(), chain))
        };
        if let Some(chain) = reach(&self.binding.root) {
            return Ok(Some(chain));
        }
        // The record's length places the folder below its old mount root. A
        // stale or wrong record only fails to reach the folder.
        let Some(recorded) = self.recorded_ancestry()? else {
            return Ok(None);
        };
        let Some(own_mount) = self.binding.root.ancestors().nth(recorded.len()) else {
            return Ok(None);
        };
        let Ok(below) = self.binding.root.strip_prefix(own_mount) else {
            return Ok(None);
        };
        Ok(reach(&mount.join(below)))
    }

    /// How this scope relates to a new UUID-less `root`, given the identities
    /// of `root` and all its ancestors (`root_chain`). Disjointness needs proof
    /// (#3204): this folder's ancestry is read now, and both folders lie below
    /// one physical mount root, so each chain shows any enclosure. Mounts of
    /// two exports are disjoint only by the export witness (#3211); a nested
    /// export, or a folder that cannot be reached now, is `Unknown`. The
    /// stored record never proves disjointness.
    pub(super) fn overlap(&self, root: &Path, root_chain: &[Identity]) -> Result<Overlap> {
        if !mount_source(&self.binding.root_identity) || !mount_source(&root_chain[0]) {
            return Ok(Overlap::Disjoint);
        }
        let root_ancestry = mount_ancestry(root_chain);
        let root_top = root_ancestry.last().unwrap_or(&root_chain[0]);
        let mount = root
            .ancestors()
            .nth(root_ancestry.len())
            .context("Writer root has no mount root")?;
        let Some((folder, chain)) = self.current_chain(mount)? else {
            return Ok(Overlap::Unknown);
        };
        let ancestry = mount_ancestry(&chain);
        let top = ancestry.last().unwrap_or(&chain[0]);
        if !physical_candidate(std::slice::from_ref(top), root_top) {
            return Ok(export_witness(&folder, &chain, root, root_chain));
        }
        if physical_candidate(&ancestry, &root_chain[0]) {
            return Ok(Overlap::Encloses(ancestry));
        }
        Ok(Overlap::Disjoint)
    }

    /// The outermost registered scope that physically encloses this UUID-less
    /// scope through another path (a second mount of the export). Admission
    /// resolves it like a path ancestor, so one scope fences both paths.
    pub(super) fn alias_enclosing(&self) -> Result<Option<Self>> {
        if !mount_source(&self.binding.root_identity) {
            return Ok(None);
        }
        let Some(ancestry) = self.recorded_ancestry()? else {
            return Ok(None);
        };
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
    /// Handles of the retired binding fail closed: their record is gone, and
    /// a later registration at this path sets the directory aside and creates
    /// new lock files and a new epoch (`set_aside_retired`).
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
        fs::rename(&self.binding_path, self.directory.join(RETIRED))?;
        #[cfg(unix)]
        File::open(&self.account)?.sync_all()?;
        Ok(())
    }

    /// Registration of a path whose scope directory belongs to a retired
    /// binding: move that directory aside so the path starts a fresh scope.
    /// Callers hold `registry.lock` and have found no binding for the path.
    /// Stale handles stay fenced: the new directory and lock files have other
    /// identities and the new state another epoch.
    pub(super) fn set_aside_retired(directory: &Path) -> Result<()> {
        let marker = directory.join(RETIRED);
        if !marker.try_exists()? {
            return Ok(());
        }
        let retired: Binding = read(&marker)?;
        ensure!(
            uuid::Uuid::parse_str(&retired.epoch).is_ok(),
            "Invalid retired writer authority; explicit recovery required"
        );
        let name = directory
            .file_name()
            .context("Writer authority directory has no name")?
            .to_string_lossy()
            .into_owned();
        fs::rename(
            directory,
            directory.with_file_name(format!("{name}.retired-{}", retired.epoch)),
        )?;
        #[cfg(unix)]
        File::open(directory.parent().context("Authority parent missing")?)?.sync_all()?;
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
