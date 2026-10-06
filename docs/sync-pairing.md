# Desktop pairing and hub adapter (#586)

This opt-in Inbox extension is separate from passkey enrollment and from the
native Settings controller. It does not start a desktop daemon or register a
service. The adapter and all integration evidence use isolated synthetic vaults.
Installing an adapter or changing a working hub requires its own approved rollout.

## Pairing protocol

A desktop persists a random request UUID, its Syncthing device ID and display name,
and **two independent 256-bit secrets**: a verifier and a future grant credential,
encoded as 64 lowercase hex characters. It sends only their SHA-256 hex digests to
`POST /api/v1/sync/desktop/start`:

```json
{"id":"<request UUID>","device_id":"<Syncthing ID>","name":"My computer",
 "verifier_challenge":"<SHA-256(verifier)>","grant_challenge":"<SHA-256(grant secret)>"}
```

The response includes an approval URL with a `#sync=<UUID>` fragment, an eight
character comparison code, and a ten-minute expiry. The desktop shows the code and
opens the system browser. Exact start retries preserve the request and expiry;
changed content with the same ID conflicts. Unknown fields are rejected. Pending
requests are bounded (128 pending, 10,000 durable total) and new requests rate limited; durable grants are capped at
100 registrations per owner, including tombstones.

The browser uses the existing HTTPS-origin WebAuthn login. Approval shows the
computer's name, full device ID, comparison code, and the operator-configured
vault choices. The owner must compare with the requesting desktop and explicitly
approve with fresh passkey confirmation. Browser approval accepts only request ID,
code and an allowed vault UUID; it cannot supply owner, filesystem path, folder ID,
REST endpoint or transport address. Reject cancels the request. No browser session
is issued to the desktop. Device ID is a Syncthing certificate identity, not a
passkey identity; actual transfer still requires its private key.

`POST /api/v1/sync/desktop/exchange` takes `{id, verifier, grant_secret}` in its JSON
body. The service compares both original challenges. Before approval it returns
`registration: null`; after approval it atomically creates one registration and
stores only the grant's hash. Exact retries return the same registration without
minting or returning another credential. Exchange expires with the request, even
after service restart. The separate grant remains durable. Browser sessions and
unfinished WebAuthn ceremonies still require fresh login after restart.

Native routes reject browser Origin and Cookie headers. They do not enable CORS.
`POST /api/v1/sync/desktop/status` and `/remove` use `Authorization: Bearer <grant>`;
the credential selects only its own owner/vault/device registration. Never put it
in a URL, diagnostic log or command argument. No grant endpoint can execute an
arbitrary hub operation. The browser has authenticated list/remove endpoints,
with fresh passkey confirmation required for removal. The computer list includes
unexpired requests awaiting approval or desktop exchange; a Review action opens
the separate approval view. Expired, cancelled and exchanged requests leave the
pending list.

States are `provisioning`, `hub_ready`, `removal_pending`, `revoked`.
`hub_ready` means only that the hub share is configured, **not** that a desktop has
received the vault. Only this state exposes folder ID, public hub ID/address and
ignore policy for the later native controller. A revoked grant can only read its
own removal receipt or repeat Remove; connection parameters are withheld. Source
files and REST keys are never returned. Passkey revocation does not automatically
revoke independent sync registrations; use Remove for those registrations.

## Authority and durable state

The Inbox operator explicitly enables `--sync-config <private JSON file>`:

```json
{"owner_id":"<existing Inbox owner UUID>","vaults":[{
 "id":"<vault UUID>","name":"Test vault","folder_id":"test-folder",
 "hub_device_id":"<hub Syncthing ID>","hub_address":"tcp://127.0.0.1:22440",
 "ignores":["/.tessera-index","/worktrees"],
 "adapter_socket":"/private/adapter/hub.sock"
}]}
```

There is no default mapping, real-vault discovery or socket mount. Invalid Sync
configuration disables only Sync, preserving Inbox login and capture. The mapping
must match the existing owner; durable scope bindings reject reinterpretation of
an existing vault UUID's folder/hub/policy. Label changes are allowed; a different
hub or folder needs a new vault UUID. Requests,
grants and requested removal state live in the private Inbox SQLite database
(schema 12), outside vaults and indexes. Secrets are hashed. Expired requests and
revocation tombstones are durable records, not caches; no automatic tombstone
expiry is implemented. Fresh owner approval can create a new registration after
confirmed removal; old credentials and tombstones cannot override it.

An asynchronous worker reconciles saved registrations every five seconds. Intent
commits before external work. A result from an old Add cannot overwrite a newer
Remove. Offline/unavailable/conflicting hub configuration remains pending with a
safe error; restart retries the same registration. A configured vault must remain
available while its registrations need reconciliation.

## Host adapter

`tessera-sync-hub --config <private JSON file>` is a separate Unix process. Its
configuration contains owner UUID, vault UUID, folder ID and expected folder path,
hub device ID, literal loopback REST address, private REST-key file, private data
directory and socket path. It alone reads the REST key. The socket has mode 0600
inside a 0700 directory; service access is an OS deployment capability, not an
unauthenticated TCP proxy. Only one adapter process may own its data directory.

The bounded newline-JSON socket protocol accepts only `{owner_id, vault_id,
registration_id, device_id, action}`, with actions `add`, `remove`, `status`.
Unknown fields, wrong scope or replayed registration IDs with different devices
are rejected. Every hub mutation verifies hub identity, **1.29.5** version and
configured folder path. Existing external devices/shares are not adopted by Add.

The adapter journals ownership before device creation/share addition, allowing a
restart between REST calls. New devices are not introducers and do not auto-accept
folders. Their `dynamic` address does not enable discovery/relay/NAT: transport is
configured separately by the operator, and the client can initiate a static
connection to the hub. Adapter code never changes those global transport options.
Only the configured folder's device array is patched; other folder and device
settings are retained. It serializes its own mutations and rechecks immediately
before writing. Syncthing REST offers no compare-and-swap, so concurrent external
administrative writes still require coordination.

Remove persists a terminal tombstone before touching the hub. A removal received
before any owned Add does not acquire authority over an external device/share.
Owned shares are removed again after re-introduction, including after restart.
Global device records are retained to avoid damaging other roles/folders. This
is eventual removal from **this hub**, with a possible re-introduction window;
other peer connections and already copied files may remain. It is not lost-device
fleet revocation. A terminal registration stays revoked. Re-enrollment requires a new request,
new grant secret and fresh owner approval after removal completes. Native cooperative shutdown is slice 3.

## Validation and deployment boundary

Normal Inbox tests exercise WebAuthn-backed approval, wrong credentials/origin/
owner/vault, expiry, duplicate/conflicting requests, restart and stale Add results.
The ignored `sync_hub` test runs an unchanged pinned Syncthing 1.29.5 in a temporary
home with loopback-only listeners and discovery/relay/NAT/upgrade disabled. It
checks interrupted Add, REST read-back, offline Remove/restart, re-introduction,
external configuration preservation, scope binding and the actual private socket
transport. All owned children are stopped; the allowed share and live endpoint
are positive controls before absence assertions.

```sh
# On the authorized isolated development host, with the build wrapper:
cd inbox
cargo fmt --check
cargo fmt --manifest-path ../crates/tessera-sync/Cargo.toml -p tessera-sync --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
TESSERA_SYNC_HUB=/path/to/pinned/syncthing-1.29.5 \
  cargo test --locked -p tessera-inboxd --test sync_hub -- --ignored
```

Real browser/passkey UX is verified in a separate QA session against the sandbox
approval screen; test authenticators do not replace that check. No native Settings
UI, OS service lifecycle, production socket mount or working-vault enrollment is
part of this slice. Any Inbox deployment must preserve existing owner/credentials
and use backup → deploy → login check → automatic rollback; none is required to
exercise the independent development sandbox.
