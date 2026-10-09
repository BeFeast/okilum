> Historical pilot record. On 2026-10-08 the owner made hosted Linux permanent
> and disabled automatic expiry. Current executor entrypoint and fallback:
> [Hosted CI](hosted-ci.md). The rollout instructions below describe the original pilot.

# One-day GitHub-hosted Linux PR pilot (pending owner enablement)

Scope: full Linux PR gate only. Windows packaging stays local for this pilot;
main, release, publish and signing routes stay local. Existing public GitHub CI
on mirror main is unchanged. `OKILUM_LINUX_LANE=hosted` opts Forgejo PR heads into
GitHub ubuntu-24.04; `pr-<number>` selects exactly one canary PR; unset/local selects the current local compiler job. Only
trusted same-repository PR heads can use the mirror credential. Fork Linux jobs
keep the existing local path; native fork policy is unchanged.

The Forgejo `linux` job becomes a bridge aggregate over linux-local/linux-github.
It accepts exactly one successful executed lane; hosted additionally needs its
verified result. It never reads the mutable lane variable. Existing ci/check and
macos aggregate keep requiring linux=success. Both lanes use check-linux.sh with
the complete existing fmt/clippy/script/unit/Reader feature-boundary commands;
this is not the deferred Fast PR selector.

The bridge pushes the exact checkout SHA to a unique forgejo-linux-pr ref using
the existing Forgejo-side mirror credential. The GitHub workflow is triggered by
that ref, not by an eventually synchronized main. Match full SHA, unique ref,
push event and exact workflow path; require exactly one successful linux job with
its full gate step executed. Missing workflow/head, timeout, API failure, skipped
step or ambiguous runs fail closed. No statuses are copied from another SHA.
Delete only the invocation's temporary ref afterward. Existing macOS transport
behavior is preserved through default parameters.

GitHub receives no Forgejo/signing/S3 secrets; contents permission is read-only,
checkout does not persist its temporary credential, and no S3 sccache is enabled.
Restore only the existing public-ci main Cargo/target cache with its exact key.
Do not write per-PR caches. On 08 October the repo held 7 caches totaling
10,698,033,550 bytes (~9.96 GiB), so this pilot adds zero cache entries rather than
churning the shared 10 GiB budget. A miss or restore error builds cold. The hosted
job uses the existing public-ci compile settings to consume that baseline; the
local lane retains its mold RUSTFLAGS and LAN sccache settings.

GitHub API confirms BeFeast Free. Published standard-runner limits are 20 total
concurrent jobs, including at most 5 macOS, shared with other org repositories:
https://docs.github.com/en/actions/reference/limits . There is no reserved Linux
capacity claim. Bridge capacity is also shared with macOS/aggregators; compiler
work runs only on GitHub. Queue deadline 15 minutes, execution deadline 60 minutes,
bridge timeout 80 minutes. Excess demand reports failure instead of falling back
to a misleading successful local/old run. Observe queue saturation during pilot.

Enablement after explicit owner OK: merge the reviewed patch, rebase participating
PRs (workflows/scripts come from their head), start with one exact-head canary,
set the variable to `pr-<number>` for that canary. After its success, record UTC
start/end (24 hours), then set the variable to hosted. Rollback: set
OKILUM_LINUX_LANE=local; newly evaluated lanes go local, in-flight exact-head
aggregates retain their actual lane results. No active main publication is cancelled.
Old PR heads without this patch cannot participate and must be reported separately.

Measurement: retain the existing throughput collector/analyzer and its task-grain
wait semantics. Before/after report local linux-local executed tasks and slot-hours;
bridge waiting time is not compiler time. For each hosted URL capture GitHub run
created_at plus linux job started_at/completed_at, exact head and conclusion.
Report remote start-wait and runtime separately from Forgejo bridge queue and
end-to-end latency. Compare equal observation windows and completed PR jobs; list
pending/running jobs separately to avoid survivor bias. Report moved job count and
avoided local compiler demand, not a claim that the physical pool gained runners.

Pre-pilot successful-completion baseline from the saved 08 October dataset is in
~/.cache/okilum-qa/github-linux-pilot/baseline.json. No hosted pilot run or lane
variable change has been performed. Cache/runner speed benefits remain hypotheses.
