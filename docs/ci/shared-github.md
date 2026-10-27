# Shared gate routing and bounded hosted measurement

`ci / shared` remains the required Forgejo context. `check-shared.sh` is unchanged
and owned by Tests. The coordinator reads `OKILUM_SHARED_LANE` once per CI run:

| Value | Required execution | Independent measurement |
| --- | --- | --- |
| unset / local / unknown | local | none |
| measure | local | hosted, only within the bounded window |
| hosted | hosted | none |

Fork PRs always use local and receive no bridge credentials. A failed hosted gate
never starts a second local gate automatically. In measure mode a hosted failure
is reported by the independent job but cannot change the local required verdict.

Hosted uses free `ubuntu-24.04`, the same Rust/profile/linker flags, pinned vendor
and system dependencies, the isolated sprint-report regression, raised nofile
limit and `tini -s -- bash scripts/ci/check-shared.sh`. Only source-pinned unique
`forgejo-shared-pr/**` refs trigger it. Main uses a run-ID ref owner, so the mirror
main push cannot create a duplicate shared run. App secrets stay on Forgejo;
GitHub jobs receive only their normal ephemeral read-only repository token.

## Measurement and activation

Before enabling measurement, record the full SHA and initialize these Forgejo
repository variables:

- `OKILUM_SHARED_MEASURE_UNTIL`: UTC epoch at most 24 hours in the future.
- `OKILUM_SHARED_MEASURE_FIRST_RUN`: first eligible Forgejo repository run number.
- `OKILUM_SHARED_LANE`: `measure`.

The inline coordinator enables measurement only for run numbers from FIRST_RUN
through FIRST_RUN+9 inclusive and before UNTIL. Repository-wide numbering can
consume that range sooner; it can never enable more than ten ordinary CI runs.
Invalid, missing, expired, or more-than-24h-future bounds disable measurement.
The variable may remain `measure` after expiry, but hosted measurement then stays
off and local remains required. Set it back to `local` when collecting the receipt.
No dispatch, empty commit or rerun is needed to create observations.

Record local and hosted queue+run for the SAME full source SHA from the ordinary
CI job timestamps and bridge log/native run. Include all queue waiting, full
suite receipts, sample count and p50/p90 in the PR. A one-run p90 is descriptive
only. Activation requires reviewed green exact-head CI and manager acceptance
of hosted queue+run at roughly 20 minutes or less. The baseline supplied for
planning was local p50 24 / p90 38 minutes; same-SHA measurements supersede it.
If hosted is slower, keep local active and propose a budgeted cache follow-up.
Rollback after activation is `OKILUM_SHARED_LANE=local` for subsequent runs.

## Cache receipt and initial budget

The single pre-change inventory on 27 October found 9,000,924,982 bytes (8.38 GiB),
three main entries: one macOS native 4,100,211,540 bytes and two Linux baselines
2,457,010,253 / 2,443,703,189 bytes. The reported second macOS duplicate was already
absent, so nothing was deleted. Receipt:
`/home/god/work/qa-evidence/github-shared-cache-before.json` (operator evidence).

Initial shared measurement is cold and adds ZERO cache saves. The existing Linux
archive includes target output with different flags; it is not a compatible shared
cache. A follow-up may propose GitHub Actions per-object sccache with PR reads,
main-only writes and LRU, or a bounded main-only target cache, with an explicit
repo-wide budget under 10 GiB. No LAN cache credentials are exposed to GitHub.
