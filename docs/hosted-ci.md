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

**Current blocker:** standard hosted runners have no approved Developer ID
private-key identity or notary profile. Preflight checks that existing capability
before Cargo and fails closed if unavailable. This PR imports no keychain, binds
no signing secret and does not provision credentials. Do not dispatch an artifact
build until an approved signing route exists. Providing a PKCS#12 private key and
notary credential to an ephemeral GitHub runner would create a new trust boundary;
it requires a separate concrete reviewed provisioning/cleanup plan and approval.
No private material is moved by this code. Alternatively an explicitly approved
signing service could be designed; none is assumed or contacted here.

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
