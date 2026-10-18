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

For Linux QA, select `lane=linux-binary`; `baseline=true` separately pins and
resolves main. It waits for the existing required producer or reuses its accepted
artifact. It cannot compile or dispatch a replacement for a missing payload. The
Forgejo job saves the exact executable, production receipt, JSONL, and (while
hosted) checksummed tar as `linux-production-receipt-<job>`. Native QA still uses
the existing owner/lock; downloading is not QA completion.

To resolve from an exact checked-out source on a QA machine, provide the existing
mirror credential as `MIRROR_TOKEN` and run:

```sh
PR_HEAD_SHA="$(git rev-parse HEAD)" python3 scripts/ci/github-linux.py resolve
```

This discovers the full SHA/config identity, verifies the successful full gate,
producer/run attempt, artifact lifetime/linkage, immutable ZIP digest, outer tar
checksum, complete internal checksums and production manifest before extracting.
Saved output retains the original `artifact.zip`. Every resume fetches accepted
API metadata again and hashes that ZIP against the API digest, then derives the
manifest and compares every payload member through its checked inner tar. A
legacy output without the ZIP redownloads it. Edited receipt/tar checksums alone
cannot authorize changed executable bytes.

The bridge validates ELF identity and the producer's executed build-info receipt
without launching the app. Arch repeats build-info/ELF/runtime validation before
packaging. Run native QA on the verified `dist/linux-production/payload/okilum`
through the existing `/tmp/okilum-gui-qa.lock`, preserving time spent waiting.
The old `forgejo-linux-binary` workflow is a compatibility consumer with no Cargo.

The identity is full source SHA + `x86_64-unknown-linux-gnu` + release/default
`brain` + no harness/test-support + a configuration digest covering the pinned
inputs and producer code. **The lifetime one-production-compilation guarantee
applies only to this canonical trusted artifact used for QA/release.** It requires
the durable identity claim/tombstone below. A lost artifact, expired/deleted run
history or cancelled job never authorizes recompilation or a hidden Cargo
fallback. It is not a global once-per-SHA promise for every CI configuration.
The app-only Cargo invocation follows the unchanged
full gate. Build version is **5000 + originating Forgejo run number**, carried in
the temporary ref; GitHub run numbers and later packaging runs never renumber it.
PR-head bytes cannot be relabelled as merge-SHA bytes. Repeated accepted requests
reuse evidence; active requests wait. Simultaneous cold requests are serialized
by the existing workflow's SHA lock and recheck acceptance before compiling.
Commit timestamps do not control first-build eligibility: old and future-dated
never-attempted source commits use the same path. The bridge uses its existing
mirror credential to create an annotated Git tag under
`refs/tags/okilum-production-claims/<artifact-name>`. The artifact name includes
full SHA, target, profile and configuration. The tag points at the source commit
and records the exact hosted run ID/attempt/ref and original Forgejo identity.
It is durable operational state: never delete this namespace with temporary
invocation refs, artifacts, CI history or caches.

The bridge creates this claim only while the hosted workflow lock holder is
executing the production step after its successful full gate. The hosted process
waits up to three minutes for the claim, using only its existing read-only token.
No extra hosted secrets or write permissions are needed. Ref creation is atomic;
competing bridge observers read the winner rather than updating it. A lost create
response is reconciled by reading the same ref. Only an explicit API 404 means
no claim; permission errors and outages never authorize compilation.

The first run attempt matching that claim may invoke the shipping compiler once.
Actions step execution is once per run attempt; an exclusive invocation marker in `RUNNER_TEMP`
additionally rejects a process restart even with a different output directory. A GitHub rerun
on a fresh runner has a higher attempt and cannot compile. Another run cannot use
the claim, even if the earlier run's API history and artifact have disappeared.
Accepted hosted or signed archive bytes are resolved first, including on reruns.
If a claimed identity has no valid bytes, explicit recovery is required. There
is no automatic claim deletion, transfer or cache-miss compiler fallback.

A crash before claim creation has not authorized compilation. A crash after
claim creation, including before Cargo starts or during upload, conservatively
leaves a tombstone; it can strand an identity, never silently rebuild it. Recovery
is an operator-authorized exception outside the normal request helper. GitHub
may cancel excess pending runs. A completed/cancelled run with **no jobs** is
ignored during discovery; success with missing jobs is still invalid, and an
actual production step retains its no-recompile tombstone.

Hosted production artifacts expire after three days. Once packaged, the existing
signed Arch archive retains the exact executable and original producer receipt;
the resolver can verify/reuse those bytes after hosted expiry. Legacy archives
remain valid for their existing promotion path; they cannot masquerade as the new
full-source schema. Unarchived expired PR artifacts are unavailable.

`delivery.jsonl` and `receipt.json` are outside Cargo/index caches and saved as
workflow evidence. They retain request/observed queue times, API creation/job/step
times, production build start/end, verification and the original build identity.
QA and Stable availability remain null until their owners measure them. Append
measured QA or availability facts through the same helper, for example:

```sh
PR_HEAD_SHA=<full-sha> OKILUM_ARTIFACT_DIR=<existing-evidence-directory> \
  python3 scripts/ci/github-linux.py record --event qa_waiting \
  --facts '{"occurred_at":"<actual-UTC-time>","reason":"existing native QA slot"}'
```

Use `task_started`, `forgejo_queued`, `qa_requested`, `qa_waiting`, `qa_started`,
`qa_completed`, `promotion_waiting_for_go`, and `available_verified` as applicable;
record known actual timestamps in `--facts`, retaining null for unknowns. Upload
is not Stable availability. The approximate 60-minute delivery target includes
all queue/QA/GO waiting and has not been established by these implementation tests.

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

Trusted same-repository PRs and actual main merge commits now use the same hosted
Linux producer. `OKILUM_LINUX_LANE` no longer sends trusted shipping identities to
a second compiler lane. Required `ci/linux` and `ci/check` names are unchanged;
main-health follows the Linux aggregate and reads failed hosted/local lane logs.
Forks keep the isolated local full gate, never receive a mirror credential and
cannot produce trusted publication artifacts. `OKILUM_PR_LANE=local` still selects
local supplemental jobs; trusted Arch jobs resolve the same production bytes.
Signing, publication locks and promotion entrypoints are unchanged.

The organization budget is up to 20 simultaneous standard jobs, shared across
repositories and macOS/Windows, with a separate macOS cap. This is a capacity
limit, not a promise of immediate starts. Windows packaging uses a short Linux
icon job followed by one `windows-latest` compiler job. Other selected lanes
use one job each. Branch pushes run Linux only; request additional lanes when
needed. Linux/Brain consume the existing main cache; new lanes do not save
large per-branch target caches into the shared 10 GiB quota. Missing dependency caches are normal for the canonical producer; missing
production artifacts do not permit a consumer build. Capacity incidents are reported, not retried in a loop.

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

## Receipt and fork consumer interfaces (#1431)

`validate_receipt(receipt, source, configuration)` and
`package_receipt(path, source, build)` authenticate unsigned local receipts by
resolving the original accepted hosted artifact or a signed archive independently.
They use static ELF/digest inspection before any native execution. Public reads
can be anonymous, but the hosted artifact ZIP requires an existing
`MIRROR_TOKEN`/`GH_TOKEN`. Both resolver and unprivileged packaging steps receive
that credential on trusted workflows, as does protected initial Linux publication.
The Forgejo API token cannot replace it. Fork steps receive no mirror credential.
Authorization is never forwarded to artifact storage redirects.

The optional `trusted_envelope=True` skips only that independent fetch. It is for
the resolver after API ZIP digest verification, or after the caller has verified
the package signature using the configured key. It must never be inferred from
editable receipt fields, tar checksums, a file's local existence, or fork CI
success. Package/manifest/source/build/binary checks still apply.

The canonical `build.sh` first calls `reuse-package.py`, which validates the signed
package with this option **after** `verify_signature`, compares its canonical
manifest and immutable receipt fields with the selected prebuilt bytes, and
retains the exact original package, signature and embedded receipt. Only the
helper's verified reuse result permits the caller's trusted field validation;
no environment variable or receipt flag grants that authority. Static source,
configuration and binary-digest checks, followed by executed ELF/build-info
inspection, still precede success. Reuse skips makepkg and re-signing. This permits
recovery with a supplied prebuilt after hosted expiry, even before source
descriptor publication. Discovery of an unknown build when both the descriptor
and hosted payload are absent remains unsupported. An entirely absent archive
requires independent unsigned receipt authority before initial packaging; partial
or invalid archives fail closed. Parsing unsigned archive keys is not authorization.
Protected `production_build()` and downstream `publish.execute()` use the same
signature-first authority for known-build retries. They require the exact original
signed package/receipt and matching archive fields before trusting its envelope,
then retain its signature without replacement signing. Thus the existing feed and
catalog path can finish after hosted expiry with a missing descriptor. Absent
archives still require independent hosted authority; immutable-content rejection,
feed monotonicity and the Linux-only promotion contract remain unchanged.

Structural fixture tests may inject an external authority or explicitly mock
`authenticate_receipt`; they are not provenance proof. Dedicated whole-entrypoint
controls use the real canonical receipt authority against immutable ZIP/API
fixtures, a signature double accepting only exact archived bytes, expiry/missing
descriptor controls and counted compiler/packager stubs. These establish local
control flow and provenance comparisons, not live transport, cryptographic or
native acceptance. No fixture test may contact live APIs.

**Manager-accepted fork boundary (#1431):** fork validation is a separate isolated,
unsigned compatibility check, with deduplication only inside the retained local
workspace/run. Local repeat invocations reuse retained bytes; interrupted attempts
cannot silently compile again there. A new independent first-attempt fork workflow
for the same SHA/configuration may compile again. This is an explicit limitation,
not the canonical lifetime guarantee, and #1431 adds no trusted fork coordinator.

Forks receive no mirror credentials or signing/publication secrets, create no
canonical claims, and cannot become trusted QA/release artifacts. Their
`okilum-untrusted-fork-v1` schema and local checksums establish consistency only;
`trusted_envelope=True` cannot relabel them as canonical. Protected publication
checks Forgejo run metadata and rejects fork/PR/non-main runs before artifact
reading, signing or writes, even with a forged canonical receipt. Required full
checks, package installation, ldd, X11 and smoke validation remain intact.

Validation distinguishes the two guarantees: canonical counting-compiler,
durable-claim/history-loss/expiry/no-artifact controls remain, while fork controls
count one local compilation and explicitly allow one per independent run. Full
required CI, the live claim handshake/upload/download and Ubuntu-to-supported-Arch
unchanged-binary install/ldd/X11 acceptance remain delivery gates; Python/stub
results alone do not complete #1431 acceptance. `public_read` and
`verify_signature` interfaces are unchanged.
