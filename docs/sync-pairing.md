# Pair computers directly

The current Linux setup is [Sync between your computers](sync-enrollment.md).
On the computer with the notes, open **Share with another computer…**, share the
vault with **Share with my computers**, and select **Add computer** → **Copy
pairing URL**. On the new computer, choose **Get notes from another computer**,
paste the URL and select **Connect**. Confirm the expected computer on the old
one, then choose a vault and an empty local folder on the new one. First receive
and bidirectional Sync start automatically.

The pairing URL is private, usable for one computer and expires after ten minutes.
On macOS, opening Sync settings and creating this URL use only Okilum's local
pairing identity and listener. They do not start Syncthing or wait for Login Items
approval. Runtime preparation remains required when connecting to receive notes.
Repeated **Add computer** reuses an active invitation, preserving a pending
confirmation; expired, cancelled or unbound invitations are replaced. The LAN
address comes from active interfaces, with default routing used only as a hint,
so a VPN or missing Internet route cannot select a tunnel instead of Ethernet/Wi-Fi.
Action failures record the redacted complete cause chain as `sync_action_failed`
in the app diagnostic log. Unclassified failures show their short underlying
cause in Settings. Explicitly disabled Sync remains disabled.

Keep both computers on the same LAN for first pairing and keep the old computer's
Okilum open during confirmation and vault selection. The URL is not a filesystem
path. If it expires, create a new one on the old computer. A paired computer can
choose further authorized vaults without another invitation. A non-empty new
destination remains refused until [#2043](https://git.oklabs.uk/BeFeast/okilum/issues/2043).

## What confirmation authorizes

The new computer supplies its signed name, application identity and selected
Syncthing device identity. The old computer shows the requesting identities
before approval. A different requester cannot replace that request silently.
After confirmation, the new computer gets a private catalog of authorized
vaults. No unauthenticated request can list that catalog, and no account or
Okilum service participates.

The URL pins the old computer's application public key and carries a random
invitation secret. Do not post it publicly or save it inside a vault. Approval
creates a separate durable paired credential; expiry of the invitation does not
unpair the computers. The original requester may retry within the invitation's
lifetime to recover a lost approval response, but cannot use it to approve a
second computer. Syncthing separately authenticates file transfer with its device
certificates. Public discovery/relays help later transport; they do not carry the
first pairing handshake or authorize vaults.

Implementation and bounds are in
[the direct-pairing engineering notes](sync-direct-pairing.md),
[`direct/mod.rs`](../crates/okilum-sync-controller/src/direct/mod.rs) and
[`direct/wire.rs`](../crates/okilum-sync-controller/src/direct/wire.rs), checked
against `origin/main` at `547a8c4c96be464a4d66921447c6c93e5ad90162`.

macOS is **coming** in [#2489](https://git.oklabs.uk/BeFeast/okilum/issues/2489);
Windows is **coming** in [#2490](https://git.oklabs.uk/BeFeast/okilum/issues/2490).
See [remaining work](sync-enrollment.md#removal-ignores-and-remaining-work) for
non-empty first merge, device-list/removal, conflict/history and server limits.

## Superseded service pairing

The former #586 hub adapter and browser/passkey enrollment are gone from the
desktop user flow ([#2049](https://git.oklabs.uk/BeFeast/okilum/issues/2049)).
They are not steps for connecting computers. Their code remains for independent
consumers in [Inbox Sync](../inbox/crates/okilum-inboxd/src/sync.rs),
[the hub adapter](../inbox/crates/okilum-inboxd/src/sync_hub.rs), and
[legacy controller enrollment](../crates/okilum-sync-controller/src/enrollment.rs).
The [older Linux controller notes](sync-linux-controller.md) and
[service readiness notes](sync-readiness.md) describe that historical implementation,
not the current desktop setup.
