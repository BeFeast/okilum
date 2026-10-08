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
after reconnect, and Tessera's bounded conflict inventory finds the actual copy
without changing either version. A per-folder 100% minimum-free-space setting then
blocks a real incoming file. The controller reports Needs attention with the actual
`insufficient space` error; restoring the prior reserve permits the queued transfer.
This is Syncthing's space-reserve admission check, not an OS ENOSPC simulation.

The combined recovery run passed in 24.64 seconds. The failed attempt to require
an actionable space explanation exposed #742: `insufficient space in folder` falls
back to generic diagnostics. That presentation defect remains separate; transport
recovery passing does not waive it. Case collisions and actual OS disk-full recovery
also remain open.

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
mkdir -p "$HOME/.cache/tessera-qa/589/scratch"
export TMPDIR="$HOME/.cache/tessera-qa/589/scratch"
export TESSERA_SYNC_HUB="$HOME/.cache/tessera-sync-fixture/syncthing-linux-amd64-v1.29.5/syncthing"
export TESSERA_SYNC_CLIENT="$HOME/.cache/tessera-sync-fixture/syncthing-linux-amd64-v2.1.6/syncthing"
~/bin/tessera-build cargo test -p tessera-sync-controller --test folder \
  owned_folder_and_external_replica_keep_their_boundaries -- --ignored --nocapture --test-threads=1
```

The first extended run passed in 11.21 seconds, with crate fmt/clippy also passing.
The log is retained in CT141's issue cache under `589/recovery.log`.

## Remaining gates

This fixture injects approved pairing and readiness receipts. It does not prove
browser/passkey → HTTPS service → native Settings, service-side revoke reconciliation,
OS disk-full recovery, cross-platform case collisions, Windows/macOS lifecycle or login.
Those remain separate acceptance work. Native Settings QA uses the published Linux
beta and the manager's muninn QA session. CT119 rollout still requires its own
backup/rollback plan and explicit daytime approval. No live rollout is implied.
