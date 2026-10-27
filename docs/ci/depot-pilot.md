# Inactive Depot macOS pilot: setup and cost sheet

Preparation only. No Team purchase, Depot account/trial, App installation,
Depot job, token creation or runner activation is authorized by merging this PR.
The public mirror, free required shared lane and all signing remain unchanged.

## Proposed paid request

**GitHub Team for BeFeast (four cached seats, $16/month at $4/seat/month) + one
14-day Depot Developer macOS pilot, with a $500 Depot spending ceiling.** Confirm
the checkout price/taxes and current billable seats before paying. Cached pricing
mentions the $4 introductory first-12-month rate; renewal may differ.

| Item | Planning amount | Basis |
| --- | --- | --- |
| GitHub Team | $16/month for 4 seats | Cached 27 Oct org/pricing research; not a new account read |
| Depot Developer | $20/month | Vendor pricing, one user; not Startup |
| 14-day macOS execution | about $360 | Cached volume and ~4.5–6 min/run estimate at $0.08/min; unmeasured Depot speed |
| Conservative total including both monthly fees | about $396 | No included credits or prorating assumed; taxes excluded |
| Recommended Depot-only ceiling | at least $500 | Reserve above ~$380 compute+plan estimate; Team billed separately |
| Guard warning / routing stop | $300 / $450 at a $500 cap | 60% alert; 90% stop leaves room for in-flight work |

Sources: cached `depot-research-20261027/{opus-report,astra-report}.md`, not a new
run-history audit; [Depot pricing](https://depot.dev/pricing),
[UsageService](https://depot.dev/docs/api/sdk-reference#usage-service),
[GitHub Actions usage caps](https://depot.dev/changelog/2024-07-15-github-actions-usage-caps).
The existing cached research measured four BeFeast seats. ~$360 is an estimate,
not a quote or a speed guarantee. Team raises standard total concurrency, not
GitHub's five-Mac sub-cap; the Depot runner pool is separate.

## Exact owner setup, only after the paid GO

1. In GitHub organization **BeFeast → Settings → Billing & licensing → Plans**,
   select Team, confirm the four-seat/$16 monthly total or report any difference,
   and complete the approved purchase. Do not make the public mirror private.
2. Create a dedicated **BeFeast Depot organization**, choose **Developer**
   ($20/month, one user), and connect its GitHub App to **Only select repositories
   → BeFeast/okilum**. Do not install it on all repositories. Allow that public
   repository in the runner group and keep approval for all outside collaborators.
   Use `depot-macos-15` (M2 arm64, macOS 15), not `latest`/macOS 26.
3. In Depot org **Settings → Current usage**, set the GitHub Actions usage cap
   before enabling any job. Prefer the approved $500 total ceiling if exposed by
   the UI; if it exposes minutes, cap at no more than 6,000 billable units and
   confirm the current unit/multiplier semantics before enabling. At the maximum
   $0.08 rate this is at most $480 usage plus $20 Developer. Do not subtract
   included credits. The guard below is a second conservative stop, not a
   replacement for Depot's own enforced cap. No other Depot workloads are part
   of this dedicated pilot; disable if non-pilot usage appears.
4. The guard must be able to **unset GitHub repository variables**. The existing
   bridge App has Contents/Workflows/Actions write and Metadata read; these do
   **not** authorize the Variables API. Before activation, grant that App
   repository **Variables: write** for `okilum` only and approve the installation
   permission update. The normal bridge still requests its original three
   permissions; only the disabled pilot guard requests Variables write as well.
   No permission/account change is performed by this PR.
5. Generate a Depot **organization API token** for the read-only UsageService
   query, through the owner's secure secret request. Store it ONLY as Forgejo
   repo secret `OKILUM_DEPOT_USAGE_TOKEN`. If Depot cannot issue a narrowly scoped
   token, disclose its actual scope before accepting it. It is never sent to a
   GitHub runner. Existing App credentials stay on Forgejo.
6. Set Forgejo repo variables `OKILUM_DEPOT_PILOT_START` and
   `OKILUM_DEPOT_PILOT_END` to UTC epoch seconds at most 14 days apart, and
   `OKILUM_DEPOT_PILOT_CAP_USD=500` (or the explicitly approved higher ceiling).
   Set `OKILUM_DEPOT_PILOT_ENABLED=true` only once the guard is reviewed and its
   permissions/usage readback have been verified. Let the ordinary 5-minute tick
   validate the guard first; no workflow dispatch is needed.
7. After manager activation GO, set **GitHub** repo variables
   `OKILUM_DEPOT_MACOS_RUNNER=depot-macos-15` and
   `OKILUM_DEPOT_MACOS_SHADOW=true`. Both absent is the shipped default and skips
   the shadow before any nonexistent runner label can queue. Pair ordinary
   unsigned PR gate results; shadow is not required, saves no duplicate cache,
   and receives no signing identity, LAN secret or Forgejo/App credential.
8. Switching the real macOS lane later is separate manager GO:
   **GitHub** `OKILUM_GITHUB_MACOS_RUNNER=depot-macos-15`. Unset restores the
   free `macos-15` label. Do not change `OKILUM_MACOS_LANE`, the local M4 route,
   shipping/signing, feeds or required acceptance gates for this pilot.

## Guard behavior and limits

The Forgejo job is skipped unless `OKILUM_DEPOT_PILOT_ENABLED=true`. On trusted
main it makes one Depot HTTP-JSON `UsageService/GetUsage` read for the pilot period
and bounded operational reads of **currently queued** runs of only the two
macOS workflows. It does not repeat the historical run audit. No account API,
purchase, workflow dispatch, rerun or new producer is implemented.

The light budget check uses the larger of Depot-reported elapsed/billed minutes
at $0.08/min plus the entire $20 monthly fee. It ignores credits and emits a
workflow `::warning::` at 60%. This is a conservative gross bound, not an invoice;
unknown runner/resource usage, malformed/stale period evidence or API outage
causes fallback. Actual response shape and billing-unit interpretation still
need an authenticated read-only validation after secure setup; offline fixtures
are not live Depot proof.

At 90%, pilot expiry, invalid usage evidence, or a **Depot-labeled macOS job**
queued more than 600 seconds, the guard deletes the three GitHub pilot/route
variables. New required jobs select free `macos-15`; shadow skips. The 5-minute
schedule detects the 10-minute threshold on its next successful tick (up to about
5 additional minutes plus scheduler/API delay). It does not cancel, rerun or
turn an existing queued/failed gate green. A current pending gate remains visible
for manager disposition. Paid execution remains protected by Depot's hard cap.

A Variables permission/token failure means the guard cannot confirm fallback:
it fails with an operator-action message, never claims success. Do not enable
Depot until this path is proven. Warning annotations/guard logs are the 60%
notification mechanism; no undocumented vendor notification threshold is assumed.
For an owner inbox notification, subscribe to this guard workflow's alerts.

Stop the pilot by unsetting the GitHub route/shadow/runner variables and setting
Forgejo `OKILUM_DEPOT_PILOT_ENABLED=false`. No running work is silently restarted.
