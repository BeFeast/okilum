# Run a commit in CI

Heavy PR checks run on standard GitHub-hosted runners in the public
`BeFeast/okilum` mirror. Executors do not need to compile on maestro. Source
and logs must be suitable for that public repository; no signing or private
cache credentials go to GitHub.

1. Rebase on current Forgejo main (the commit must contain these workflows).
2. Push your exact commit to Forgejo as `ci/<issue>-<purpose>`. The `commit-ci`
   workflow automatically runs the full Linux gate and reports its result on
   that commit. Alternatively push a normal branch and open a draft PR: the
   existing PR gates run, including path-selected platform release checks.
3. For Windows tests or unsigned packaging, manually run Forgejo **commit-ci**
   on the same branch, selecting `native-core` (core `windows_` tests),
   `native-sync` (sync-controller `sidecar::` tests), `windows-release`, `arch`,
   `brain`, or `inbox`. `linux` is the default. Native tests use default features,
   `--nocapture --test-threads=1` on Windows 2022. For a different package/filter,
   owners use exact-source `windows-native.yml` dispatch; CI owner handles lane failures.

For a runnable Linux QA build, dispatch `commit-ci` with `lane=linux-binary` (`baseline=true` also builds a pinned main snapshot); on muninn download its `linux-binary-<sha>` artifact with `gh run download <run> -R BeFeast/okilum -n linux-binary-<sha>`, verify `sha256sum -c *.tar.zst.sha256`, unpack with `tar --zstd -xf <archive>`, enter the payload directory, verify `sha256sum -c SHA256SUMS` and `SOURCE_SHA`, then run `(flock -w 2700 9 && xvfb-run -a ./okilum --vault /path/to/qa-vault 9>&-) 9>/tmp/okilum-gui-qa.lock` (omit `xvfb-run -a` for a visible session); artifacts expire after three days and require compatible Linux x86_64 runtime libraries.

4. Read the linked GitHub run in the Forgejo job log. Check the run SHA and
   named tests; a green job with zero matches is not acceptance evidence.
   Unsigned release artifacts are available on that GitHub run for three days.
   A newer source commit needs its own evidence when the tested crate,
   dependencies, lockfile, toolchain or workflow changed.

The bridge pushes an isolated temporary ref and accepts only that exact SHA,
workflow, job and executed acceptance step. Missing mirror source, skipped
steps, capacity timeouts and network errors fail closed. The ref is deleted
when the bridge finishes; an interrupted coordinator can leave a ref for CI
owner cleanup. There is no automatic dispatch retry. Branch CI is evidence,
not a replacement for the unchanged required `ci/check` PR aggregate.

Linux PRs use `OKILUM_LINUX_LANE=hosted` permanently. To explicitly fall back,
set that variable to `local`; set `OKILUM_PR_LANE=local` for the supplemental
Brain/Inbox/Arch/Windows PR jobs. Neither changes running jobs. Same-repository
PRs use hosted bridges; untrusted forks keep the isolated local fallback and
never execute on the bridge host with a mirror token. Existing local main,
nightly, manual release, publish and signing workflows remain unchanged.

The organization budget is up to 20 simultaneous standard jobs, shared across
repositories and macOS/Windows, with a separate macOS cap. This is a capacity
limit, not a promise of immediate starts. Windows packaging uses a short Linux
icon job followed by one `windows-latest` compiler job. Other selected lanes
use one job each. Branch pushes run Linux only; request additional lanes when
needed. Linux/Brain consume the existing main cache; new lanes do not save
large per-branch target caches into the shared 10 GiB quota. Missing caches
fall back to cold builds. Capacity incidents are reported, not retried in a loop.

## Logs of failed jobs

Hosted Linux dependency setup uses `scripts/ci/apt-bounded.sh` on both build
runners and in the Ubuntu AppImage smoke container. Each apt invocation has a
four-minute deadline plus a 15-second kill grace, 30-second HTTP/HTTPS inactivity
timeouts, two download retries and a 60-second dpkg lock wait. Failed index
downloads fail the step rather than silently using stale indexes. Output remains
visible for diagnosis; the two build dependency steps also have a ten-minute
workflow deadline. No compiler or acceptance command is retried or skipped.

Incident evidence (2026-10-22): GitHub run `39767622990`, `linux-checks` job
`119786892717`, entered **Install system libraries** at 11:02:51 UTC and was
cancelled at 12:05:34 UTC, with no apt output between those events. Its old
`-qq` commands do not distinguish update/install, mirror/network or lock stalls.
Run `39772117886` had already reached **Run full Linux gate**; its live job log
returned `404 BlobNotFound`, so an apt cause for that run is not established.
The bounded helper prevents another hour-long apt stall and provides diagnostic
output; it does not establish a mirror/network root cause.

Run `39781817542` subsequently passed the full Linux gate and built the AppImage
on Ubuntu 22.04. In the clean Ubuntu 24.04 container, update fetched 33.6 MB in
19 seconds, but installation requested 87.7 MB / 104 packages and HTTP archive
downloads progressed around 80 KB/s before the four-minute bound returned 124.
Runtime execution, Fedora and signed installation were not reached.

The smoke container now uses `scripts/ci/ubuntu-smoke-apt.sh`: official
`https://archive.ubuntu.com/ubuntu`, then one explicit fallback to
`https://security.ubuntu.com/ubuntu`. Both serve Noble, Noble updates and Noble
security; read-only metadata checks confirmed the base and security InRelease
paths. This is availability evidence, not hosted throughput evidence. The normal
hosted run tests these routes with a bounded signed index update (60 seconds)
and verified package download (150 seconds) per candidate, logging the selected
host and failures. There are at most two candidates; final installation retains
the shared four-minute bound and uses `--no-download`. A 12-minute total deadline
plus 15-second kill grace bounds the whole provisioning stage, including retries.
The runner's one-hour ceiling stays unchanged.

The clean image receives only the hosted runner's CA bundle as a read-only file
for HTTPS bootstrap; no TLS or apt signature/hash verification is disabled.
Sources explicitly retain the Ubuntu archive keyring and Noble amd64 suites.
Completed deb downloads can be reused within this fresh container on fallback;
there is no cross-run cache or restored installed runtime. Ubuntu/Fedora native
smoke and signed installation remain required. Offline fake-apt tests cover
primary success, download timeout fallback and fail-closed exhaustion in the
normal hosted Linux gate; no local tests or artifact execution were performed.

Separate read-only infrastructure evidence: baldr's
`forgejo-runner-bridge.service` accepted Forgejo task `22335` (job `34758`, API
run `15757`, web run `10092`). Its actual ExecStart config is
`/var/lib/forgejo-runner-bridge/config.yaml`, with `runner.timeout: 1h` and
`runner.shutdown_timeout: 1h`. The extra service's separate
`/var/lib/forgejo-runner-bridge-extra/config.yaml` has both values `130m`.
Forgejo 16.0.5 in loki CT102 has no explicit timeout override in `[actions]`
of `/etc/forgejo/app.ini`. No live configuration was changed.

The primary bridge's one-hour ceiling remains unchanged; this CI-only fix does
not authorize a live runner change.
An existing bridge rerun still creates a new GitHub ref/build; this patch does
not add receipt replay or permit older-source evidence for a new head.

Logs of this repository's jobs are public; no token is needed. The usual trap is
the run number: the number in a run's web URL (`/actions/runs/6123`) is not the
API run id, so `GET /actions/runs/6123/jobs` reads another run or nothing.

- **One run:** `python3 scripts/ci/ci-logs.py get <run URL or number> [job]`
  prints the failed jobs' logs (or the named job). It resolves the number first.
- **Every failure, already saved (maestro):** `~/.cache/okilum-qa/ci-logs/`.
  A user timer (`okilum-ci-logs.timer`, every 5 minutes) runs
  `ci-logs.py collect` into it from `~/.cache/okilum-qa/tools/ci-logs.py`:
  - `<run>-<job>.log` is the Forgejo job log;
  - `<run>-<job>.github.log` holds the failed steps of a hosted lane's GitHub run
    (`gh run view --log-failed`);
  - `index.tsv` has one line per failed job: time, run, job, workflow, event,
    branch, sha, summary, links. The summary names failing tests or the first
    compiler error. It also says when the job only aggregates others (`linux`,
    `macos`, `check`), ran on GitHub, or produced no build or test output
    (likely infrastructure).

  Files are kept for 30 days. Start with `column -t -s$'\t' index.tsv | tail`.

## Mac shipping artifacts (delivery #1445)

The trusted `macos-release.yml` selector now honors `OKILUM_MACOS_LANE`.
`local` keeps the existing keychain/sign/notarize/beta-publication path. `hosted`
never falls back to the local Mac: scheduled ticks stop before selecting a
compiler; a separately authorized manual run on `main` can request `source=<full
main ancestor SHA>` and hands one artifact job to GitHub's arm64 runner. The
bridge uses the existing mirror credential only on Forgejo; GitHub receives no
Forgejo/private cache/signing secrets. Required CI remains unchanged.

Product source and reviewed workflow SHA are distinct. The bridge checks both,
pins the mirror workflow SHA, retains the original Forgejo `5000+run_number`
version, and accepts only its exact workflow/ref/run/attempt/job/artifact. The
hosted job checks out the product source and executes its existing
`build-macos-ci.sh`, which performs Developer ID signing, notarization, stapling
and Gatekeeper checks for the app ZIP and DMG. Provenance records both SHAs and
the actual app plist/source/tree/version. No fabricated source marker, CI cache
export, signing bypass or #1431 claim reuse is involved.

**Runtime prerequisite:** separately approved signing inputs must be provisioned
before an artifact dispatch. Empty/missing credentials, wrong certificate SHA256,
team or private identity, unavailable notary authentication and any partial setup
fail before Cargo. Code/CI/merge do not authorize reading/exporting the existing
key-holder identity, creating GitHub secrets or dispatching a build.

The setup step consumes only `OKILUM_MAC_DEVELOPER_ID_P12` (base64 PKCS#12),
`OKILUM_MAC_DEVELOPER_ID_PASSWORD`, `OKILUM_MAC_NOTARY_APPLE_ID`,
`OKILUM_MAC_NOTARY_PASSWORD`, `OKILUM_MAC_NOTARY_TEAM`, plus the frozen public
certificate pin `OKILUM_MAC_CERT_SHA256` (repository variable). No credential is
present in the repository; these bindings remain unprovisioned until separately
approved. No new account/token/identity is requested by the code.

`scripts/releases/macos-keychain.py` creates a unique mode0700 directory under
`RUNNER_TEMP/okilum-signing-<run>-<attempt>/`, imports into its own
`shipping.keychain-db`, matches certificate SHA256 to the one private-key identity
and team, and stores the existing notary credential only there. It captures the
original default/search-list and restores/verifies them immediately after
`create-keychain` (which may add itself), and again during cleanup. Every signing,
identity lookup and notary command uses the explicit ephemeral keychain; no
persistent login-keychain lookup/fallback. Secret command argv/stdout/errors are
never printed. Only sanitized ready/failed state is logged.

An `always()` cleanup copied outside the product checkout deletes only the exact
operation keychain/files and restores/verifies baseline before artifact upload.
Partial import/notary failure is cleaned; unknown/foreign/aliased residue stops
rather than removing it. Private keychain/P12/env never enter caches/artifacts.
The four reviewed delivery scripts are retained before checkout of an older
product source: app/Rust source remains that exact tree, while provenance records
workflow SHA and delivery-script hashes. No tracked product-source overlay or
fabricated source marker is used.

Live operation/rollback (separate approval): first freeze the existing identity
SHA256/team/exportability and approved existing notary credential; provision only
the five named secrets and public pin, then one normal source-pinned artifact job.
No private material is copied by preparation/tests. Rollback deletes only secrets
created by that recorded operation and its private transient copies; preserve the
original key-holder login identity/profile and encrypted backup. No rotate/revoke,
M4 SSH, lane/runner/service changes or Stable mutation. Revert the integration to
rollback code; never dispatch the old local path during workday as fallback.

On success the authenticated GitHub run provides `macos-shipping-<source>-<build>`:
only the signed/notarized app ZIP, first-install DMG and `provenance.json`. Download
with the existing GitHub artifact UI/API, verify the API artifact digest and asset
SHA256s, and mount the DMG/drag Okilum.app to Applications (or unpack the app ZIP).
This is QA distribution: **no Stable or beta feed mutation and no automatic
promotion completion** in the hosted route. Native product acceptance stays
separate. Existing local shipping beta publication is unchanged.

Rollback before provisioning/build: revert #1445; no keychain, secret, service,
runner or feed rollback is needed. Do not flip the lane or dispatch the legacy
local path during workday as a rollback. A failed/ambiguous hosted run is a stop,
not permission for rerun, fallback, a second compilation or a new version claim.

## Portable Linux AppImage (issue #1470, slice 1)

The required hosted Linux workflow builds one unsigned x86_64 AppImage on
Ubuntu 22.04, independently of the existing Arch package. Its final `linux`
job requires both the unchanged full Linux checks and AppImage compatibility
checks. QA must reuse this artifact rather than dispatch `linux-binary` or
compile another release. No release/publish/signing workflow is changed.

The GitHub run retains `linux-appimage-<full-source-sha>` for three days:
`Okilum-x86_64.AppImage`, `SHA256SUMS`, `provenance.json` and the bundled-library
inventory. Upload happens before smoke testing; an available artifact is not a
claim that smoke passed. Verify the download's checksum and provenance source
before running it. The same artifact is extracted without FUSE and checked for
ELF dependency resolution, a real visible X11 Reader window, and clean Ctrl+Q
exit in fresh Ubuntu 24.04 and Fedora 44 containers. Evidence logs are uploaded
as `linux-appimage-evidence-<full-source-sha>`, including on smoke failure.

The build uses pinned/checksummed linuxdeploy, appimagetool and Type-2 runtime.
linuxdeploy bundles its non-system dependency closure and the existing desktop
entry/icon; glibc and the host GPU driver remain system libraries. Mesa software
Vulkan in CI proves packaged startup, not hardware rendering or Wayland support.
The container dependencies are runtime libraries/fonts/display tooling, without
the product source's build dependencies. The visible-window probe has a visible
X11 positive control; the missing-library detector also checks its failure path.

This slice does not install desktop integration, publish beta or Stable assets,
add signatures, or enable in-app self-update. Those need separate changes and
native install/update acceptance before merge. Later publishing should reuse the
same bytes in the existing Linux path with immutable `builds/<n>/` assets and
`beta/Okilum-x86_64.AppImage`; later self-update must verify existing GPG trust,
respect beta/stable selection and replace a writable AppImage atomically without
root. Until implemented, this unsigned CI artifact is for explicit QA only.

### Slice 2: exact-main reuse and beta signing

The same `forgejo-linux.yml` now also runs on public mirror `main` pushes, so
merge source has its own AppImage rather than a relabelled PR artifact. The
hosted AppImage job additionally verifies disposable-key signing/install/tamper
rejection on the actual payload. No live signing credentials reach GitHub.
The existing Linux bridge resolves completed exact-source acceptance and transfers
one original archive plus evidence; it never rebuilds or dispatches a replacement.
Missing/expired/ambiguous evidence fails closed. The existing protected Linux
publisher signs and publishes beta using the same key/R2/lock. See
[AppImage beta publication](linux-releases.md#appimage-beta-publication-1470-slice-2)
for numbering, paths, retry semantics and remaining live verification gates.
