//! Retire writer scopes that can no longer fence anything (#3272).
//!
//! Every folder ever registered keeps `<key>.json` and `<key>/` in the account
//! directory, and a new registration reads all of them under `registry.lock`.
//! A registration therefore retires, under that same lock and bounded per call,
//! scopes that meet every condition below. Each one is checked while the scope's
//! admission lock is held, so no admission, hold, re-attach or durable-binding
//! request for it can run concurrently:
//!
//! * the scope was created by a version that knows about pruning and never
//!   handed out a [`DurableBinding`](crate::DurableBinding): its `prunable`
//!   marker still exists. Sync keeps durable bindings of absent roots and
//!   admits them without the root (#2582); those scopes, and every scope
//!   written before this marker existed, are kept forever;
//! * its record and lock files are exactly the registered ones (current schema);
//! * the root path no longer exists, and the nearest existing ancestor is on the
//!   volume the root was registered on: the folder was deleted or moved on a
//!   mounted volume. A root on an unmounted or detached volume is kept;
//! * its state is `Open` (no unfinished hold, no recovery pending), and nobody
//!   holds its writers lock (no live lease) or its owner lock (no hold owner).
//!
//! A retired scope fences nothing the registry would otherwise enforce: an
//! `Open` scope with a missing root never blocks promotion of an enclosing
//! folder or registration of a physical alias. Authorities and leases minted
//! for it stay refused: their binding record is gone, and a later registration
//! at the same path gets a new epoch and new lock files.
//!
//! Retiring renames `<key>.json` to `<key>.pruned` (one atomic step in one
//! directory, made durable before anything else is removed), then deletes the
//! scope directory and finally the tombstone. A crash at any point leaves either
//! the complete scope or a tombstone that the next registration finishes.
use super::*;

/// Scopes retired by one registration; the rest wait for the next one.
pub(super) const PER_REGISTRATION: usize = 1024;
/// Present while a scope may be retired; removed for good by `durable_binding`.
pub(super) const PRUNABLE: &str = "prunable";
const TOMBSTONE: &str = "pruned";

pub(super) fn tombstone(account: &Path, root: &Path) -> PathBuf {
    account.join(format!("{}.{TOMBSTONE}", key(root)))
}
pub(super) fn is_tombstone(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == TOMBSTONE)
}

/// The root is gone, and its nearest existing ancestor is a plain directory on
/// the volume the root was registered on. Every other outcome keeps the scope.
pub(super) fn root_deleted(binding: &Binding) -> bool {
    match fs::symlink_metadata(&binding.root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        _ => return false,
    }
    for ancestor in binding.root.ancestors().skip(1) {
        match fs::symlink_metadata(ancestor) {
            Ok(meta) => {
                return meta.is_dir()
                    && identity(ancestor)
                        .is_ok_and(|current| same_volume(&current, &binding.root_identity));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return false,
        }
    }
    false
}

#[cfg(target_os = "linux")]
fn same_volume(a: &Identity, b: &Identity) -> bool {
    match (a.3, b.3) {
        (Some(x), Some(y)) => x == y && a.4 == b.4,
        _ => false,
    }
}
#[cfg(target_os = "macos")]
fn same_volume(a: &Identity, b: &Identity) -> bool {
    matches!((a.3, b.3), (Some(x), Some(y)) if x == y)
}
#[cfg(windows)]
fn same_volume(a: &Identity, b: &Identity) -> bool {
    use filesystem_identity::FileIdentity::Full;
    matches!(
        (a.3, b.3),
        (Some(Full { volume: x, .. }), Some(Full { volume: y, .. })) if x == y
    )
}
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn same_volume(_: &Identity, _: &Identity) -> bool {
    false // no stable volume identity: never retire
}

/// Remove the `prunable` marker before a durable binding leaves the process.
/// Called with the scope's admission lock held.
pub(super) fn pin(directory: &Path) -> Result<()> {
    match fs::remove_file(directory.join(PRUNABLE)) {
        Ok(()) => {
            #[cfg(unix)]
            File::open(directory)?.sync_all()?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Under `registry.lock`: retire up to [`PER_REGISTRATION`] candidates (records
/// whose root was missing during the registration scan) and finish tombstones.
/// Never fails the registration; a scope that cannot be proven idle stays.
pub(super) fn run(
    account: &Path,
    candidates: Vec<(PathBuf, Binding)>,
    mut tombstones: Vec<PathBuf>,
) {
    for (binding_path, binding) in candidates.into_iter().take(PER_REGISTRATION) {
        let root = binding.root.clone();
        if retire(account, &binding_path, binding).unwrap_or(false) {
            tombstones.push(tombstone(account, &root));
        }
    }
    if tombstones.is_empty() {
        return;
    }
    // The renames must be durable before any lock file disappears: a record
    // that came back after a crash would name deleted objects.
    #[cfg(unix)]
    if File::open(account).and_then(|d| d.sync_all()).is_err() {
        return;
    }
    for tombstone in tombstones.into_iter().take(PER_REGISTRATION) {
        let _ = finish(account, &tombstone);
    }
}

fn retire(account: &Path, binding_path: &Path, binding: Binding) -> Result<bool> {
    let scope = Authority {
        requested: None,
        account: account.to_owned(),
        binding_path: binding_path.to_owned(),
        directory: account.join(key(&binding.root)),
        binding,
    };
    let admission = file(&scope.directory.join("admission.lock"), false)?;
    if admission.try_lock().is_err() {
        return Ok(false);
    }
    scope.validate_binding()?;
    let marker = fs::symlink_metadata(scope.directory.join(PRUNABLE));
    if !marker.is_ok_and(|m| m.is_file()) || !root_deleted(&scope.binding) {
        return Ok(false);
    }
    if scope.state()?.phase != Phase::Open {
        return Ok(false);
    }
    let writers = file(&scope.directory.join("writers.lock"), false)?;
    let owner = file(&scope.directory.join("owner.lock"), false)?;
    if writers.try_lock().is_err() || owner.try_lock().is_err() {
        return Ok(false);
    }
    fs::rename(binding_path, tombstone(account, &scope.binding.root))?;
    Ok(true)
}

/// Delete an unpublished scope directory, then its tombstone.
fn finish(account: &Path, tombstone: &Path) -> Result<()> {
    let stem = tombstone
        .file_stem()
        .context("Writer scope tombstone has no name")?;
    ensure!(
        !account.join(stem).with_extension("json").try_exists()?,
        "Writer scope tombstone has a published record"
    );
    remove_scope(&account.join(stem))?;
    fs::remove_file(tombstone)?;
    Ok(())
}

/// Remove a scope directory that has no published record: lock files, state,
/// marker and stray temporary files. Anything else stops the removal.
pub(super) fn remove_scope(directory: &Path) -> Result<()> {
    let meta = match fs::symlink_metadata(directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        meta => meta?,
    };
    ensure!(meta.is_dir(), "Writer scope is not a directory");
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        ensure!(
            entry.file_type()?.is_file(),
            "Unexpected object in a retired writer scope"
        );
        fs::remove_file(entry.path())?;
    }
    fs::remove_dir(directory)?;
    Ok(())
}
