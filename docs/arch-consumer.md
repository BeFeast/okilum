# Arch source selection and package retry

The Arch packaging consumer never dispatches a producer or compiles a fallback.
`.forgejo/workflows/linux-release.yml` first checks out its own revision and runs
`scripts/arch/select-source.py` before checking out the selected product source.
The selected revision must have exactly the same Git objects (including modes)
under `scripts/arch/` and for the Linux consumer, its transport dependency and
payload validator. Historical revisions with missing or different tooling are
explicitly unsupported by that workflow revision, including pre-schema Linux
helpers that ignore `resolve` and dispatch a build. This comparison reads Git
objects only; it does not execute the selected helper to discover support.

The selected full commit remains unchanged. The workflow checks the resulting
checkout against the selected SHA before resolution, and payload consumption
continues to validate that source and its production configuration digest. A
historical source with different tooling fails without source substitution.
Both checkouts retain full history. In particular, a selected synthetic fork
merge must expose its actual declared base/head parents to the fork validator;
a depth-one second fetch can hide them even after an initial full checkout.

Every trusted step that independently authenticates hosted bytes receives the
existing GitHub artifact read credential: `github.token` as `GH_TOKEN` in the
hosted Arch resolver and packaging steps, and `OKILUM_GITHUB_MIRROR` as
`MIRROR_TOKEN` in the trusted Forgejo resolver, packaging and protected Linux
publication steps. The unprivileged `runuser` child retains that step environment.
Forgejo's API token is not a GitHub artifact credential. Fork expressions remain
empty and their separate unsigned producer never receives the mirror secret.
Artifact storage redirects remain credential-free.

Before unsigned receipt resolution or `makepkg`, `scripts/arch/reuse-package.py`
looks up the immutable archive by the structurally validated receipt's original
producer build number. Those unsigned fields locate bytes; the package signature
establishes authority before its embedded receipt is trusted. This does not depend on a
source catalog descriptor: the archive can already exist when feed/catalog
publication was interrupted. An existing archive must have its original signed
package, matching archive checksum, full source/configuration, embedded producer
receipt, artifact identities and executable bytes. Reuse carries the exact package,
signature and original embedded receipt into the packaging artifact. A fresh
hosted download may advance verification timestamps, but cannot change any of
those immutable identities. No new package counter is assigned.

The caller uses trusted receipt field validation only after the helper has proved
signed reuse; neither environment nor receipt flags select it. Static source,
configuration and executable digest checks and executed ELF/build-info inspection
remain before success. Thus a supplied prebuilt can recover a signed build archive
after hosted expiry even when its source descriptor has not yet been published.
The upstream resolver still needs hosted evidence or a source descriptor to
discover an otherwise unknown build; this does not add build discovery.

Only an entirely absent build archive with independently authenticated unsigned
receipt authority permits `makepkg`. A partial archive needs
explicit recovery. An existing local package with different metadata is rejected;
immutable archive contents are never overwritten to accommodate repackaging.
Both protected `publication.production_build()` and downstream `publish.execute()`
use the same signature-first helper before unsigned hosted receipt resolution.
For a known build, exact archived package bytes and embedded receipt/source/config,
all immutable artifact fields and binary must agree with the signed package and
archive manifest. The standalone publication receipt must equal the embedded
original. A wholly absent archive still requires independent unsigned authority;
partial or invalid archives fail without replacement signing or writes.
An exact-package retry retains the original signature and can finish the feed
and catalog after hosted expiry without a source descriptor. Feed monotonicity and the
Linux-only promotion/signing contract remain in their existing owners.

The reuse helper consumes the existing `github-linux.py` read-only interfaces
`public_read(key)`, `verify_signature(package_path, signature_path)` and `payload`
(the strict receipt/package validator). It does not call discovery, `request`,
`produce`, `push_head` or any compiler. Fork producer routing is a separate concern;
these checks do not grant a fork signing credentials or bypass package acceptance.
