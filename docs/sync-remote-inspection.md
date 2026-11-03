# Read-only remote vault inspection (#2505)

`Controller::preview_remote_vault(peer, vault, local_root)` compares an explicitly
authorized remote vault with a real, non-empty local directory. Both controllers
must select the checksum-pinned managed Linux runtime. The destination can remain
an ordinary directory: inspection does not enroll it. The returned `Preview`
exposes the existing `merge_preview::Plan` counts and complete path lists.
`read_remote_preview_file` retrieves exact binary bytes from that revision;
`validate_remote_preview` rechecks both sides. Run these blocking methods outside
the UI thread.

This is observation, not mutation ownership. A successful read proves the bound
revision was unchanged during its validation interval, not that an external
editor cannot change it later. #2504 supplies writer fencing/quiescence; #2506
must revalidate both revisions under that ownership before any publication.
There is no apply endpoint here. Non-empty direct attachment stays refused, and
#2043 retains the complete Settings/first-merge user journey.

## Authorization and exposure

The existing encrypted, Ed25519-authenticated approved-peer channel carries a
separate `Inspect` operation. A temporary invitation cannot invoke it. The
paired signer must match the approved persistent identity and the vault must
remain in that peer's explicit allowed catalog, backed by the same attached
replica/root identity. Paused folders and unpromoted receiveonly replicas are
refused on admission and every validation. The local directory must be non-empty
and disjoint in both directions from controller state and managed preparation. Inspection does not acquire a Syncthing share grant.
Authorized peers can see that vault's canonical relative filenames, sizes,
SHA-256 hashes and content. The root binding is an opaque HMAC-SHA256 scoped
to the peer and a random controller-lifetime secret; canonical absolute paths,
OS usernames and raw root device/inode identities stay local. No caller-supplied path is opened
on the server: content requests index the validated immutable manifest.

Inspection bypasses both directions of transport-address reconciliation. It
rejects a request or response trying to add that exchange, so an inspection
cannot PATCH a device, change a folder, save the pairing database or create a
destination marker/ignore file. Runtime operations are authenticated GETs only.
Admission verifies the managed preparation and pinned binary; subsequent reads
verify its selected configuration, certificate, device and live start time.
Every admission still streams the complete executable through SHA-256. The
shared package verifier uses `ring`'s compiled backend: unoptimized software
hashing on CPUs without SHA extensions can exceed the channel's three-second
receive timeout before the opening response is ready. Package pins, admission
checks and channel limits remain unchanged.

## Revisions, completeness and bounds

A header binds the peer capability, both persistent signing identities, vault ID,
opaque canonical-root/open-directory identity binding, daemon identity,
complete runtime configuration hash, controller generation, daemon start time,
and canonical ignore policy. Its revision includes regular-file and directory
identity, size, mode, link count, mtime, ctime and the content manifest. The manifest binds
every complete relative path, byte length and content hash.

Descriptor-relative `openat` traversal refuses symlinks, hard links, special or
unknown entry kinds, mounted filesystems, parent traversal, non-UTF-8 names and
unsupported paths. An open root descriptor prevents inode reuse from making a
replacement root look unchanged. Complete metadata passes bracket content
collection; subsequent page/chunk operations validate that same complete tree and
runtime binding once before slicing immutable cached data. Explicit validation
reads the complete bytes once. Scans and REST calls hold neither the controller
database nor cache lock; authorization is rechecked before returning a response. Cached immutable
bytes ensure different generations cannot be assembled into a successful read.
Reading atime is not part of the revision.

Full byte revalidation also brackets public preview/content operations and
explicit validation. Metadata is a fast per-chunk change detector, not the sole
freshness proof: even a same-size edit on a coarse timestamp filesystem must
fail the final complete byte comparison. Unit tests model that metadata
collision explicitly.

The supported ignore policy is the canonical policy from `direct/source.rs`,
plus root Syncthing metadata. An unenrolled destination without `.stignore`
uses those same canonical rules without writing the file. Custom patterns,
includes/negations, nested runtime metadata and unexpected live runtime ignore
rules fail explicitly; they are not approximated. Root `.stignore` metadata is
part of the revision even though its bytes are not canonical merge content.

The implementation refuses more than 16,384 files, 64 MiB aggregate content,
32,768 visited canonical entries, 64 directory levels or 1,024 bytes per relative
path. Traversal has a two-second per-pass budget. These are error boundaries,
not successful truncation. At most four snapshots (256 MiB cached content), two
per peer, are retained by (peer, vault, revision). Admission reserves capacity
before collection; full caches refuse new vaults rather than evict another peer.
Unchanged revisions reuse their capability while installing the newly collected,
fully validated snapshot and its admission deadline; changed revisions replace
only that peer/vault. Reads never renew a deadline. One request per peer and four
inspection requests globally may run concurrently. Up to 16 bounded connection
workers keep slow inspections off the listener; excess connections are closed
without a queue. Capabilities expire
120 seconds after the latest fully validated opening, never survive controller
restart, and the listener reaps expired allocations even without a new inspection
request. In-flight ownership and full byte validation have bounded additional
temporary allocations.

Pages contain at most four entries and content chunks at most 4,096 original
bytes. Hex encoding stays below the existing 256 KiB authenticated wire-frame
limit, including its nested JSON overhead. Every response echoes the complete
header and exact page/file/byte offset; every content response also binds the
whole-file SHA-256. The collector requires exact page lengths, sorted unique
paths, the complete advertised count/byte total and manifest digest. It refuses
missing, duplicated, reordered, truncated or substituted pages/chunks. A final
whole-file hash and both complete revisions are checked before returning bytes.

## Acceptance evidence

`scripts/ci/sync-source-sharing.sh` explicitly runs the inventory unit tests as
part of `direct::` and the named real-runtime acceptance test
`source_sharing::remote_inventory::managed_remote_inventory_exact_plan_binary_chunks_staleness_and_zero_writes`.
The runner requires every expected test name and a non-zero exact pass count.
GitHub agent checks run it before the desktop build; Forgejo's existing managed
source-sharing job runs the same script twice in parallel.

The acceptance test uses two disposable prepared, checksum-pinned Syncthing
2.1.6 runtimes and non-empty roots. It checks the exact four Plan categories and
all paths, a 300,007-byte binary across frame bounds, edits on either side,
concurrent unchanged snapshot capabilities, paused/receiveonly refusals,
state/preparation overlap and empty-local-directory refusals, unsafe paths, unapproved peer/vault, symlinks,
ignore changes, runtime restarts on either side and a successful authorized read
as positive control. It hashes
all canonical/adjacent files and compares both runtime configurations, on-disk
configuration bytes and both pairing databases before/after inspection. It also
checks no destination folder or source share grant appeared and direct
non-empty attachment remains refused.

Fixture `Drop` kills and waits on daemon children on success/failure. The test
also explicitly stops/reaps both at completion. Linux parent-death signals kill
fixture daemons if their owning test process is cancelled, and hosted acceptance
runs under a subreaper. Unit controls separately reject mixed controller/runtime
bindings, root/policy substitution, missing pages, reordered chunks, unsafe tree
entries, oversize inventories and frames that would exceed the wire budget.

Hardening regressions (#2512) also use an authenticated wire request with a
deterministic delayed inspection boundary. The real-runtime acceptance also
completes concurrent previews and exact binary reads through two separately
approved signers without invalidating either capability. A positive control
proves that inspection reached the boundary, then a second peer inspection, actual pairing
redemption and controller/cache lock acquisition must finish while it is held.
Cache tests cover peer/vault isolation, admission reservations, limits and
expired-allocation release. All four response kinds are encrypted and decoded
in a wire control before asserting that paths/usernames/raw root identity are
absent. Fixture daemons retain explicit kill-and-wait cleanup.
