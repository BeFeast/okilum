# Sync recovery: isolated CT141 evidence

First recovery increment for #589. The `owned_folder_and_external_replica_keep_their_boundaries`
controller integration test passes against Syncthing hub 1.29.5 and client 2.1.6
on CT141. Each run generates fresh config, certificates, device IDs and synthetic
vaults in a private temporary directory. All addresses are loopback, with global
and local discovery, relay and NAT disabled. RAII stops the fixture children.
Neither persistent CT141 daemons nor production CT119 are used.

The extended test observes an authenticated connected hub before stopping it,
then observes disconnection. It creates a note and deletes a previously transferred
note while the hub is down, restarts the client and hub, and confirms both changes
arrive. Both device IDs and certificate bytes survive restart. An unrelated folder's
configuration and the exclusion of derived index content remain intact.

It next moves the existing `.stfolder` marker outside the client vault, observes
an explicit folder error, then restores the marker and confirms a new hub note
transfers. The observed error and subsequent real transfer are the positive controls;
absence of a file alone is not the assertion of safety. Existing assertions cover
explicit reuse, durable promotion, offline local removal and cleanup boundaries.

The next increment creates concurrent edits from a shared base while the hub is
stopped. Both replicas retain both contents (main note plus Syncthing conflict copy)
after reconnect, and Okilum's bounded conflict inventory finds the actual copy
without changing either version. A per-folder 100% minimum-free-space setting then
blocks a real incoming file. The controller reports Needs attention with the actual
`insufficient space` error; restoring the prior reserve permits the queued transfer.
This is Syncthing's space-reserve admission check, not an OS ENOSPC simulation.

The combined recovery run passed in 24.64 seconds. The failed attempt to require
an actionable space explanation exposed #742: `insufficient space in folder` fell
back to generic diagnostics. It was fixed separately in #747, including placement
of the explanation immediately below Needs attention. Native case collisions and
actual OS disk-full recovery remain open.

## Linux case variants and rename

`linux_case_variants_and_case_only_rename_preserve_content` passed on CT141 in
8.83 seconds with fresh loopback hub 1.29.5 and client 2.1.6. A successful first
transfer establishes the positive control; the client then receives both `Case.md`
and `case.md` with distinct checked contents. Renaming one to a non-colliding name
and renaming the other only by case both propagate without losing either content.

An initial attempt to require a case-conflict error failed: the Linux receiver
accepted both names even with `caseSensitiveFS=false`. That option does not emulate
a case-insensitive filesystem. This Linux result is not evidence for collision
rejection on default macOS/Windows volumes. Native acceptance remains required on
an isolated case-insensitive volume: transfer the first spelling, introduce the
second with different content, observe the explicit collision error and preserve
the original, then rename the second and verify both contents after recovery.
Do not run that destructive fixture on a personal vault.

## Reproduce on CT141 only

```sh
mkdir -p "$HOME/.cache/okilum-qa/589/scratch"
export TMPDIR="$HOME/.cache/okilum-qa/589/scratch"
export OKILUM_SYNC_HUB="$HOME/.cache/okilum-sync-fixture/syncthing-linux-amd64-v1.29.5/syncthing"
export OKILUM_SYNC_CLIENT="$HOME/.cache/okilum-sync-fixture/syncthing-linux-amd64-v2.1.6/syncthing"
~/bin/okilum-build cargo test -p okilum-sync-controller --test folder \
  owned_folder_and_external_replica_keep_their_boundaries -- --ignored --nocapture --test-threads=1
```

The first extended run passed in 11.21 seconds, with crate fmt/clippy also passing.
The log is retained in CT141's issue cache under `589/recovery.log`.

## OS disk-full gate: environment blocked

The 100% reserve test above is not an OS `ENOSPC` test. An attempt on CT141 to
mount a separate 16 MiB tmpfs at the issue-cache `small-volume` directory was
rejected by the container (`mount: tmpfs already mounted on /dev/shm`).
`findmnt -T` confirmed the directory still belongs to the root ext4 filesystem;
no private filesystem was mounted. `/dev/fuse` is absent, so a userspace filesystem
is not available either. The empty mountpoint was removed. Container settings,
shared `/dev/shm`, the root filesystem capacity and production data were unchanged.

A real disk-full acceptance run needs an isolated quota-limited volume from the
infrastructure owner or a disposable native test host. Proposed procedure:

1. Keep the client config/database outside a private 16 MiB vault volume. Use
   fresh hub/client identities with loopback addresses and discovery/relay disabled.
2. Transfer and verify a small canary. Disable only this fixture folder's
   `minDiskFree` admission threshold so it cannot mask the filesystem error.
3. Allocate non-sparse filler on that private volume; send a file larger than its
   remaining capacity from the fixture hub. Require a real `no space left on
   device` error from Syncthing, Needs attention, and intact canary bytes. Missing
   incoming content alone is not sufficient evidence.
4. Delete only the known filler, retry the folder, and require complete incoming
   bytes plus intact canary. Stop the two owned daemons, unmount only the private
   volume, and remove its fixture directory. Keep API keys/certificates private.

Do not substitute process file-size limits (`EFBIG`), permission denial (`EACCES`),
or the Syncthing free-space reserve for this acceptance criterion. No permission
to alter CT141 mount policy or any production hub is implied by this plan.

## Remaining gates

This fixture injects approved pairing and readiness receipts. It does not prove
browser/passkey → HTTPS service → native Settings, service-side revoke reconciliation,
OS disk-full recovery, cross-platform case collisions, Windows/macOS lifecycle or login.
Those remain separate acceptance work. Native Settings QA uses the published Linux
beta and the manager's muninn QA session. CT119 rollout still requires its own
backup/rollback plan and explicit daytime approval. No live rollout is implied.
