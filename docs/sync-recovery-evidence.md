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
disk-full recovery, conflict/case collisions, Windows/macOS lifecycle or login.
Those remain separate acceptance work. Native Settings QA uses the published Linux
beta and the manager's muninn QA session. CT119 rollout still requires its own
backup/rollback plan and explicit daytime approval. No live rollout is implied.
