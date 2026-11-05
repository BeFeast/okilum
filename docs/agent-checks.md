# Agent build/test checks on GitHub

Start from main containing this workflow and helper; older worker branches must
rebase onto that main once before using GitHub checks. Commit the worktree, then
run from that worktree:

```bash
H=github scripts/agent/gh-check.sh
```

The helper pushes **only committed HEAD** to `agent-checks/<local-branch>` in
the public `BeFeast/okilum` mirror, using `gh auth git-credential`. Install `gh`,
authenticate with write access to the mirror (`repo` and `workflow` scopes for
a classic token), and keep the credential in the host's existing `gh` credential
store/keyring. No token is copied into Git URLs, source, the vault, or Actions.
The push uses an explicit lease so another worker's concurrent update is refused.
Use distinct local branch names for independent workers. The helper rejects a
dirty worktree and detached HEAD. Everything pushed is public: source and tests
must contain only public code and synthetic fixtures, never private/corporate data.

Only `agent-checks.yml` listens to this namespace. It creates no GitHub PR, tags,
release, signing or publication work. Forgejo remains primary; its existing main
and release-tag mirror is unchanged. Push the implementation branch to Forgejo
and open its PR there as usual. Before merge, require gh-check green on this
exact HEAD, then merge the PR yourself. Any later commit needs a new check.
Forgejo CI does not run on PRs; main has no required status checks. Forgejo
runs nightly and on workflow_dispatch; hourly Beta and pre-Stable certification
remain source-pinned. Main pushes only mirror source or publish releases.
Ordinary fast manual retries on one ref supersede older manual runs; each
certification dispatch has its own concurrency group and is never cancelled.

The standard public `ubuntu-24.04` runner has 4 vCPUs. This worker lane omits
dev/test debug symbols to keep the complete desktop and test targets within the
hosted runner's disk budget; assertions, lint and every suite remain enabled.
Release and native acceptance profiles are independent. The job records its CPU,
memory and exact source SHA, verifies the pinned Rust toolchain and the complete
gpui-kit/comrak patch trees, fetches public dependencies with the workspace lock,
then runs the scheduled shared gate's exact lint command,
`cargo clippy --workspace --all-targets --locked -- -D warnings`, builds the
Linux desktop/sync supervisor and runs the full `okilum-core`
suite and `scripts/ci/linux-rust-tests.sh` with Cargo offline. These are the same
Rust suites called by full Linux acceptance (`scripts/ci/check-linux.sh`); the
nightly acceptance packaging checks retain their existing scope. The agent job
also runs complete discovery of `scripts/ci`, `scripts/releases` and
`scripts/updater`, and the helper requires that step to pass. AppImage process fixtures run serially so a
concurrent fork cannot temporarily retain another fixture's installation lock
before exec; all lifecycle assertions and test cases remain enabled. Both test shells raise their soft nofile limit via `test-nofile.sh`.
The Linux Rust suites retain their existing conditional vendor workspace tests.

Every worker run also requires `windows-recovery` on a GitHub-hosted Windows
runner (#2551). It runs the existing native `okilum-core` `windows_` suite,
including file publication, editor recovery and protected history. Discovery
must contain the unacknowledged-save regression before execution, so a missing
Windows configuration cannot pass with zero tests. The helper requires both
jobs and their required steps; native test failures remain check failures.
Forgejo PR CI and native release/signing policy are unchanged.

Every job that discovers these Python suites, including local/hosted full gates
and the fast and Sync fixture lanes, runs `scripts/ci/setup-python-tests.sh`
first. It creates a fresh venv, installs `scripts/ci/requirements.txt` and adds
its executables to the Actions `GITHUB_PATH`. `PyYAML==6.0.3` is explicit;
host site-packages and pip configuration are excluded. Keeping the full YAML
parser preserves workflow expressions, flow collections, block scalars and
the existing BaseLoader/safe_load semantics without maintaining a partial
parser in our tests. A guard rejects missing or mismatched dependencies with
the installation command. Setup failure stops the job before discovery.

For local Python-only checks, start with a fresh venv (no host PyYAML), install
the same requirements and run all three suites:

```bash
test_env=$(mktemp -d)
python3 -m venv "$test_env"
"$test_env/bin/python3" -I -m pip --isolated install -r scripts/ci/requirements.txt
"$test_env/bin/python3" -I scripts/ci/python_dependencies.py
for suite in ci releases updater; do
  "$test_env/bin/python3" -I -m unittest discover -s "scripts/$suite" -p 'test_*.py'
done
```

Debian/Ubuntu jobs install `python3-venv` explicitly. Job-created environments
are temporary; Actions owns their lifetime.

Rust downloads, verified generated vendor trees and `target/` are cached with the toolchain, dependency/manifests,
vendor patches and workflow configuration in the key. Each source SHA can save a
fresh immutable archive; later commits restore a compatible archive from the same
branch. This namespace is separate from release caches. GitHub's repository cache
quota/eviction policy still applies; a missing cache is an ordinary cold build.
Job credentials have `contents: read` only, checkout does not persist credentials,
and no repository/organization secrets or private cache services are referenced.
Concurrent checks of a branch cancel the older run.

The helper blocks on **this SHA, workflow, branch and event**, prints the run URL,
per-step timestamps/conclusions and the last 100 failed log lines. Exit codes:
`0` pass, `1` code/check failure, `75` no verdict (API/auth, cancellation, timeout,
setup failure, missing checks, OOM/disk exhaustion). Retry infrastructure once.
Its bounded discovery waits and `gh run watch` run inside one helper process;
do not add thread polling or launch Cargo on the orchestration host.
Watch refreshes once per minute to leave API quota for concurrent workers.

```bash
# Repeat the identical source to measure warm cache (new attempt, same run).
scripts/agent/gh-check.sh --rerun
# Controlled cold/warm comparison on one runner VM (separate benchmark ref).
scripts/agent/gh-check.sh --benchmark --receipt /path/to/evidence.json
# Explicit dispatch on the pushed branch; workflow must already exist on main.
scripts/agent/gh-check.sh --dispatch
```

The total helper wait ceiling is two hours (`--timeout SECONDS`); the job ceiling
is 100 minutes. For GitHub outages or checks needing local hardware, use the
build-slot fallback recipe in the project operations notes.

`--benchmark` pushes to the sibling `agent-checks-benchmark/<sha>/<branch>` namespace
and repeats the build, core suite and Linux suites using the already warm
Cargo downloads/target on the same 4-vCPU VM. Each pass has separately timed steps;
each new head gets a cold branch cache scope. `--rerun` instead measures a new
hosted job including archive restoration; its VM hardware may differ.

For source-pinned editor publication measurements and a real main negative
control on the same hosted VM, supply the public main SHA explicitly:

```bash
H=github scripts/agent/gh-check.sh --dispatch --publication-baseline <full-main-sha>
```

This also runs the normal complete checks. The helper requires the comparison
step to succeed; missing/skipped comparison work is not a passing verdict.
The baseline compile must pass before its failing regression counts as evidence.
