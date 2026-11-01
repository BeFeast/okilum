# Bundled Syncthing 2.1.6

Only the packaging builder downloads this pinned runtime. Settings and the
supervisor never download or search PATH for it. `BundleGuard::bundled_runtime`
returns only `Contents/MacOS/syncthing` after outer resource-seal, dedicated nested
signature and signed-byte SHA256 checks. Preparation must retain the returned
provenance bytes and exact digest alongside any private copy/Selection; this API
does not start Syncthing or complete Enable.

## Upstream provenance

- Release: https://github.com/syncthing/syncthing/releases/tag/v2.1.6
- Checksum list: https://github.com/syncthing/syncthing/releases/download/v2.1.6/sha256sum.txt.asc
- Published key: https://syncthing.net/release-key.txt
- Verification procedure: https://syncthing.net/security/#release-signatures
- Pinned primary fingerprint: `FBA2E162F2F44657B38F0309E5665F9BD5970C47`.

The committed published key includes its encryption subkey. The checksum file
has a valid signature by this primary key and a legacy signature whose key
`37C84554E7E0A261E4F76E1ED26E6ED000654A3E` is no longer in the published key file.
Per manager disposition (30 October), verify GPG status records: pinned VALIDSIG
is required, BADSIG/expired/revoked signatures fail, and only that legacy
NO_PUBKEY is expected. No keyserver, new trust or user GPG keyring is used.

`bundle.py verify-upstream` validates the pinned key, signed list and both archive
SHA256 constants. `stage --app APP --arch arm64|amd64` additionally checks the
selected archive digest and thin Mach-O CPU before writing the app. It extracts
only the fixed runtime and upstream LICENSE/AUTHORS/README, not archive paths.
Source is available at the exact v2.1.6 tag and release source tarball recorded in
provenance. Syncthing is MPL-2.0, unmodified; no embedded source modifications.

The common supervisor bundle helper signs this binary before the outer app,
using the existing Developer ID/keychain, hardened runtime and timestamp path;
normal app notarization seals it. The retained app provenance contains both the
upstream and post-signing digest, signature identifier/team and verifier output.
The Intel artifact-only QA route uses the matching amd64 archive and the same
signing operation. Native `check-supervisor-bundle.sh` exercises arm64 staging and
ad-hoc nested signing; release Developer ID/notarization and complete Enable
require separate native delivery receipts. Ad-hoc QA is never accepted by the
production `bundled_runtime` lookup (`signed=false`).
