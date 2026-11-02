# Compatible publication on network vaults (#1915)

Some macOS NFS/SMB and Linux NFS/CIFS mounts reject exclusive rename or
exchange with ENOTSUP, EOPNOTSUPP, EINVAL or ENOSYS. Okilum tries the native
operation first and remembers unsupported flags separately for each open vault
and device, across its folders and editors. A nested mount has its own device
entry. Single-file viewers use their containing folder. The cache is process
local; reopening the app probes again. Directory handles pin scope identities,
so an unmounted/recreated vault cannot inherit a recycled inode's capability.
One stderr diagnostic per vault/device announces compatible publication.
Ordinary permission, I/O and timeout errors do not downgrade capabilities.

Creation and file move/Trash publication retain the existing exclusive hard-link
fallback: link the complete source to an absent destination, then remove the
source name. An occupied destination, including a dangling symlink, wins.
Filesystems without hard-link support fail safely; writing a public file in
place or checking a destination before an overwriting rename is not a safe
substitute. Directory moves retain their existing checked plain-rename fallback
and its documented race with an external empty destination directory.

## Save boundary

An immediate revision check followed by plain rename is insufficient: another
writer can replace the note between those operations. Compatible saves instead:

1. Persist the dirty draft (or exact-byte transaction), write the proposal to an
   exclusive same-folder temporary, preserve permissions, and sync its bytes and
   directory. Persist original History and a recovery intent naming both private
   entries, their identities, exact hashes and bound ancestors.
2. Prove hard-link support while the public note is still present; recheck exact
   current bytes, independent of stat timestamps. Remove that private preflight
   alias and rename the actual public entry into the private preimage name.
3. Sync the captured file and directory, then compare **captured bytes** with the
   approved base. A newer replacement is restored with create-only linkat and
   reported as Conflict. A racing public occupant is never overwritten.
4. Link the complete prepared proposal to the absent public name. EEXIST preserves
   the external occupant and reports Conflict after checked cleanup of private
   aliases (the base and dirty proposal remain durable). Uncertain outcomes or a
   changed captured preimage retain all recovery instead. Sync the new public link before removing the prepared alias. Recheck the displaced
   bytes before archiving/acknowledging; late old-fd writes retain both versions
   and report that attention is needed. Advance the editor base only after durable
   completion. Successful saves leave no `.okilum-save-*` entries.

Each public creation is atomic and create-only; the **whole replacement is not
an atomic namespace transaction**. Between capture and link, readers can briefly
observe ENOENT. This is the explicit cost of avoiding an overwriting rename on a
filesystem without exchange/conditional replacement. Local supported filesystems
keep the existing exchange path. No advisory lock is claimed to exclude unrelated
editors, and OS-client network caching is not bypassed.

## Interruption and recovery

Before capture, the existing public source remains and both original History and
the proposed draft are durable. After capture, the actual displaced inode remains
in the recorded private name, even if it was a newer external replacement. A crash
in this interval can leave the public name absent, but neither version is partially
written or discarded. After publication, the public proposal and captured preimage
remain; a lost RPC reply or failed sync/acknowledgement retains recovery and never
triggers a blind rollback. A failed raw-byte rollback also refuses plain rename
when exchange is unsupported.

The existing Recover notes/History surfaces retain snapshots and unexpected
preimages, including records whose public note is missing, for saving a recovered
copy. The core inspection API additionally represents `SourceMissing` and offers
explicit `restore_missing_source`: it validates the inspection's captured bytes,
identity, ancestors and journal, then links the captured source back **only if the
public name is absent**. Reinspect and Keep current to clean owned aliases;
proposed content is never silently adopted. This core operation has no new desktop
button. Merely inspecting recovery performs no writes. Current bytes are persisted
before explicit cleanup, and cleanup cannot discard a dirty draft.

Tests inject refused flags and publication-boundary replacements, exercise both
ordinary text and exact-byte recovery, and cover missing-name restoration and a
concurrent creator winning that restoration. Real appliance/OS acceptance is
reported separately in the PR; ZFS alone is not NFS/SMB evidence.
