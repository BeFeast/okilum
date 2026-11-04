# Content revisions on coarse and remote filesystems (#2383)

Size, mtime, inode and fractional ctime do not prove unchanged content. On the
truenas ZFS slot, two same-size writes in one scheduler tick returned identical
mtime and ctime, even without explicitly restoring mtime. Main reused stale warm
sources, missed an editor refresh, omitted a newly introduced link referrer and
accepted the old rename preview. The real-filesystem integration test reproduces
these paths; deterministic unit tests emulate colliding metadata on every host.

## Verification policy

`SourceRevision::read` includes a SHA-256 fingerprint on ZFS, network, FUSE,
overlay and unknown filesystems, and whenever ctime is less than two seconds old
(or in the future). Only Linux ext4, XFS, Btrfs and tmpfs can use old native
revisions without reading content. A new local write advances ctime into the
verification window even when its timestamp is rounded. Other platforms verify
content conservatively. Filesystem-probe failures also require verification.
This does not infer granularity from the presence of fractional timestamp digits.

Fingerprinting uses a 64 KiB buffer, reads at most the observed size plus one
byte, rejects size/metadata races, and never intentionally hydrates a cloud
placeholder. Normal warm reconciliation fingerprints each covered source once,
retains unchanged source bytes and reuses the graph. Cold/changed sources also
verify after the source read, and only retain a stamp matching the bytes actually
read. Old caches without fingerprints revalidate naturally. This is derived data;
no new canonical files or operational state are introduced.

The fingerprint is verification evidence, not part of the file's identity. When
an unchanged local revision ages out of the vulnerable tick, its native metadata
can reuse a saved fingerprinted source without decoding it again. In the other
direction, a current content check still requires an identical saved fingerprint;
an old cache without one must revalidate. Changed metadata or a changed fingerprint
always prevents reuse. Fresh-process startup tests cover this transition as well
as changed-source and topology-delta relaunches (#2427).

The same rule applies while establishing a stamp (#2540). A cold source read,
primary-note checkpoint, or source-bank load can start inside the two-second
window and finish outside it. Compare the native metadata independently of
fingerprint presence, require every observed fingerprint to match the bytes
actually read, and retain any verified fingerprint in the accepted stamp.
Elapsed time alone cannot leave an unchanged source without a stamp and force
`invalidated_revision` reads on the next launch. A thread-scoped injectable clock
tests the 1.999 s to 2 s transition without sleeps; coarse-filesystem checks still
require hashes and reject same-size restored-mtime edits and racing reads.

A source replaced during fingerprint verification returns a transient
`WouldBlock` revision error. Generation collection defers that run just as it does
when its explicit hint recheck changes; it never treats an unstable hint as an
empty reference set. Invalid/unreadable hints still fail closed.

There is no periodic vault-wide hash scan. Startup/full reconciliation and link
preview/validation do bounded work over their existing inventories; incremental
watcher updates keep their existing path scope. Editor refresh fingerprints only
its open note, through the pinned regular-file handle, against its exact base.
Known metadata mismatches proceed directly to the existing content read. Warm
reuse diagnostics report initial fingerprint count and bytes separately from
source decoding; cold/changed sources have an additional verification read.
Link preview `files_read` also counts decoded sources, not fingerprint IO.

Filesystem reads still observe the OS client's view. This does not bypass NFS/SMB
server/client caching, and an unavailable watcher cannot announce a remote write;
reconciliation verifies the content available when it runs.

## Save and conflict safety

The fingerprint only prevents an unsafe early return from refresh or revision
validation. It never authorizes a save or advances a conflict base. Save still
compares exact current bytes against the original base before atomic exchange;
displaced versions and recovery are retained by the existing transaction path.
Dirty text remains a draft and a differing external version remains a conflict.
A clean refresh first persists recovery, then updates visible text and the base
together. A failed recovery write leaves both unchanged. `Keep mine` continues to
require the exact reviewed bytes. Handing an autosave stamp back to the editor
cannot bypass content verification on a coarse filesystem.

Tests cover identical stat keys, clean refresh, dirty conflicts, failed recovery,
stale `Keep mine`, warm backlinks, cache migration, a racing source read and
rename preview validation. Native ctime-only optimization tests probe 32 immediate
same-size restored-mtime writes first, printing an explicit SKIP reason when even
one write is unobservable. Safety/fallback tests do not skip.

## Measurement

`cargo test -p okilum-core --test coarse_ctime warm_index_fingerprint_profile -- --ignored --nocapture`
measures a 4,000-note, 33,632,000-byte corpus. Run baseline and fixed code in the
same build-host session. Wait three seconds before sampling so the local stat-only
case is measured as well as the conservative ZFS case. See the PR for measured
results and slot receipts; source-decoding read counts alone are not an IO measure.
