# Reader CI image candidates

The manual `reader-ci-image` workflow builds on the existing `heavy` lane and
checks that the host is Baldr. It does not change runner labels or the active
Reader CI image. Requires the root `rust-toolchain.toml` from #710.

Inputs are `scripts/ci/reader-image/{Dockerfile,base-image.txt,packages.txt}` and
the root toolchain file. The base digest was read from Baldr's existing
`ghcr.io/catthehacker/ubuntu:act-24.04` image on 2026-10-07. Rust's exact version
comes only from the toolchain file; missing/floating versions, new components,
and cross targets fail closed until the recipe is reviewed. `rustfmt` and
`clippy` are installed. Node/actions, Python, git and the original HOME layout
are inherited. The Docker context is an explicit three-file allowlist, not the
checkout. No app source, vault, target, cache or credentials enter that context.

Run the workflow on a reviewed commit with `publish=false` first. Set
`publish=true` to push a unique candidate tag to
`registry.oklabs.uk/okilum-ci-reader`. The receipt records the source revision,
base digest, toolchain hash, local image ID and, after publication, repository
digest. Detailed toolchain and installed-package versions are inside the image
at `/opt/okilum-ci/`. Candidates never move `latest` or an active tag.

Registry preflight found no existing registry credential in Infisical
services/prod and anonymous `/v2/` GET succeeded. Anonymous **push has not yet
been verified**. The script uses the builder's existing Docker authentication
if provisioned; if push requires absent credentials, stop and ask manager.
Do not create credentials, change registry authentication or print credentials.

## Acceptance before switching ci.yml

On **each** Baldr and Sindri, compare existing versus candidate on the same
commit, toolchain, profile and cache conditions, at least three pairs. Record
queue, cold/warm pull, setup, fmt/clippy/tests and total durations separately.
Do not delete shared runner caches or active images to simulate cold runs; use
an isolated cache/context. Check checkout and cache actions in `job.container`,
HOME and Cargo cache paths, vendor verification, native-scope output and the
entire current Linux gate. Verify missing/unreachable sccache remains fail-open.
The offline smoke in the build script does not replace these acceptance gates.

Only after canaries pass and warm total time improves, propose a separate
reviewed change to use the published **digest**, removing redundant setup and
including image provenance in the compatibility cache key. Keep the current and
previous digests retained in the registry; confirm GC policy with HomeLab.
Rollback restores the previous job image/setup. Toolchain/dependency/security
refreshes create new candidates and repeat validation; they never silently
replace CI. Automated rebuild triggers are deferred until the manual path passes.
