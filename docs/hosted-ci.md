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

## Reconcile a successful exact native run without another compiler

A completed native workflow can be independently observed with
`github-macos.py reconcile`. It performs exactly three GETs, retains raw API
responses in a new exclusive directory, and never reports statuses, writes
`GITHUB_OUTPUT`, pushes, dispatches, retries or cancels. The current retained
GitHub39604244715/job119236245615 evidence passes the original native predicate;
the historical jobs response seen by Forgejo9765 was not retained, so the
historical mismatch cause remains UNKNOWN. Do not classify it as product failure
or infer a transport/cache cause from current visibility.

### Trusted CI consumer and authoritative source

The manual-only `.forgejo/workflows/ci-reconcile.yml` is the supported consumer.
It executes reviewed **main tooling**, while `source` explicitly identifies the
**unchanged PR head**. These are separate identities. The existing Forgejo
commit-status API reports for `/statuses/<source>`, not for the tooling SHA.
The target URL is the NEW reconciliation execution, never the old failed job.
Original9765 jobs, outputs and failed status records remain immutable history.
This is machine-evaluated CI reporting, not an interactive green-status command.

Only first-attempt trusted main/repository/job execution is accepted. The
workflow checks out its exact tooling SHA without persisted credentials, runs
on the existing bridge, and has contents/actions read plus statuses write. It
uses the existing workflow token and approved GitHub mirror credential; it never
sends Forgejo credentials to GitHub. No runner/lane/secret change is needed.
Token policy/API denial is STOP, not a request for broader credentials.

Before reporting it independently verifies:

- The current open, non-draft same-repository PR still has the exact head/base.
- Own live CI run, manual inputs, SHA and one first-attempt running job match
  this execution; current main still equals its tooling SHA.
- Existing branch protection still requires exactly `ci / macos (pull_request)`
  and `ci / check (pull_request)`; policy changes refuse pending explicit review.
- The original CI run belongs to that PR/source/event and all original jobs and
  latest contexts still match its exact execution. Linux/native-scope/hosted
  bridge/Windows/performance passed, local lane skipped, and only macos/check
  failed. Missing/extra/rerun/cancelled/pending/failed other gates refuse.
- GitHub repository/source/original PR+Forgejo run+attempt+unique ref/registered
  workflow/run/job/native step and execution timestamps match. The workflow and
  exact compile/native step actually completed successfully on attempt1.

The full proof is uploaded BEFORE reporting. A second fresh full verification
then requires the exact own evidence artifact to exist and checks the current
PR before each status POST. Only the two protected contexts are appended with
success, both linked to this execution, followed by exact readback. Other
statuses, old job/output records and branch policy are untouched. `GITHUB_OUTPUT`
is not patched or fabricated. The diagnostic final upload may fail without
masking a retained proof or a completed gate decision.

### Exact proposed live operation — separate manager GO required

After this implementation is reviewed/merged and exact main is green, freeze
that main SHA separately from the product head. First verify source/head and
no conflicting publisher/reconciliation once. The ONE approved operation would
POST the following to the existing Forgejo workflow-dispatch API:

```text
POST /api/v1/repos/BeFeast/okilum/actions/workflows/ci-reconcile.yml/dispatches
```

```json
{"ref":"main","inputs":{"source":"db413780a600d67fdf1374fbb6b468f8cfdae8dd","pr":"1456","forgejo_run":"15277","github_run":"39604244715","github_job":"119236245615"}}
```

Use the existing approved Actions API credential and a JSON body file. API15277
is UI9765. No source push, PR update, unchanged-head native rerun, direct GitHub
dispatch, manual status CLI or output edit is part of this operation. One bridge
job has a5min timeout and bounded API requests; no compiler, app/native/GUI,
M4/host/runner/service/key/lane/signing/publication operations. Existing app-owned
completion/watch only; no additional polling service. This dispatch and its
status/artifact writes have NOT been executed by code preparation or review.

**Effects and failure boundary:** two new success-status records on the verified
product SHA plus evidence artifacts for the new execution. GitHub reads only;
no original run mutation. Status pair is sequential, not atomic. Validation or
upload failure writes no success. Before each mutation the consumer fsyncs a
journal entry with exact SHA/context/target/request; responses or sanitized
exception classes and actual status readbacks are retained in `reporting.jsonl`.
An accepted POST with a lost response is explicitly UNCONFIRMED, not failed or
retried. `report-stop.json` and the journal survive exceptions for the final
diagnostic upload. Durable attempted intent remains even if response persistence
subsequently fails; storage failure before the intent blocks the mutation.

The status API offers no conditional update here. A rollback GET then POST could
overwrite another publisher between those operations. Therefore **there are no
compensating status writes**: on reporting/readback uncertainty, retain one
bounded observational readback or its unavailability, STOP and report the actual
attempted/confirmed/unconfirmed checkpoint for manager disposition. Never claim
an own-target rollback guarantee. At most two status POST attempts; no retry.
Original failures remain inspectable, while one or both new statuses might have
been accepted despite a transport error. That outcome is not claimed as verified
recovery. No generic lock/service or interactive manual green is introduced.

Rollback before dispatch is no execution/revert of this route. A code revert
prevents future use but cannot erase an appended result. Post-mutation correction
has no safe compare-and-swap interface in this implementation and needs a new
concrete authority after examining the retained execution/history. Do not
blindly publish compensating error/green statuses, delete/reset history, or
rerun the native compiler.

The retained fixture data and finite controls exercise wrong PR/source/ref/run/
job/attempt/workflow/step/time, changed policy/main/source, other gate failure,
missing/ambiguous status/artifact, evidence failure, accepted-but-response-lost
POST, durable mutation journal, failed readback and concurrent publication.
Fixtures model the new CI execution explicitly; they are not a live recovery
receipt or proof that workflow-token permissions will succeed at runtime.
