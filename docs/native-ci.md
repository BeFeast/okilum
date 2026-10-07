# Native pull-request gate

Forgejo remains the review and protected-branch authority. For a trusted
same-repository PR that changes more than documentation, `ci / macos` now uses
GitHub's standard public-repository `macos-15` arm64 runner. Main release builds,
signing, notarization and publication stay on the M4. External GitHub pull
requests still use the public `.github/workflows/ci.yml` workflow.

## Delivery and result

After Linux succeeds, the Forgejo `macos-github` job checks out the exact PR head
and pushes only that commit to
`forgejo-pr/<number>/<head-sha>-<forgejo-run-id>-<attempt>` on `BeFeast/tessera`.
The `TESSERA_GITHUB_MIRROR` secret needs Contents and Workflows read/write and
permission to read Actions run/job results. The token stays in the Forgejo job;
GitHub receives neither a Forgejo token nor signing secrets. This is the explicit
exception to the main-and-release-tag mirror policy: trusted PR source is public.
Fork code is not sent to the M4 or pushed with the mirror credential.

The push starts `.github/workflows/forgejo-macos.yml`. It runs the existing
`scripts/ci/check-macos.sh`: production compilation, test compilation and the
quick native regressions, including the real clipboard and Quick Look tests.
The architecture assertion and every test-filter positive control remain active.
Native thumbnail evidence is uploaded to the GitHub run even when tests fail.

The bridge waits for the exact branch, head SHA, push event and workflow path.
Success also requires the `macos` job and its native build/test step to have
actually succeeded; a skipped or missing step cannot pass. Forgejo's final
`macos` job returns the required `ci / macos` status through its normal Actions
integration, and `ci / check` still requires that gate. The GitHub run URL is in
the bridge log. There is no cross-host callback credential or external status
that can race a newer head.

## Fallback and cleanup

Missing credentials, an API/push outage, runner startup failure, or eight minutes
without a running GitHub job fail the hosted gate. No automatic M4 fallback is
used. Cancellation, skipped/neutral results, execution timeouts and unknown
bridge failures also fail closed. The explicit owner-approved local lane below
checks out the same PR-head SHA; it does not authorize shell access to M4.

Closing or merging a Forgejo PR runs `github-macos-cleanup` from trusted main.
It deletes only refs under that PR's `refs/heads/forgejo-pr/<number>/` prefix;
main, tags and other PR refs are untouched. If GitHub is unavailable during
cleanup, rerun that failed cleanup job after recovery. Unique attempt refs avoid
force pushes and make stale results ineligible for a later attempt.

## Validation

`python3 -m unittest discover -s scripts/ci -p 'test_*.py'` checks exact-head
matching, executed-step evidence, failed/queued/hung runs, API outages, cleanup
boundaries and the Forgejo success/fallback/failure matrix. Workflow edits also
need YAML/actionlint validation and a real hosted run before accepting this gate
as operational. A green local run does not prove the GitHub integration.

### Temporary owner-approved local lane

`TESSERA_MACOS_LANE=local` selects the trusted M4 native job directly after Linux.
The GitHub bridge is skipped, so it occupies no `light` runner while that lane is
selected. Unset the variable or set `hosted` to restore hosted-only checks; remote
unavailability never silently falls back to M4. Both lanes retain the same native
script and fail-closed aggregate checks. Fork PRs cannot run on M4. This switch
does not change the shared mirror secret, mirroring or release workflows.

Enable local only for an explicitly approved window, with a scheduled reset to
`hosted` before morning. Existing PR heads must incorporate this workflow before
the switch affects them; already-running jobs are not migrated or cancelled.

### Hosted bridge capacity and shared cache

`macos-github` waits for its exact remote invocation and must not share a scarce
build/control slot. Register a dedicated runner with label `bridge` (baldr, capacity 8), then set the Forgejo repository variable
`MACOS_BRIDGE_RUNNER=bridge`. Until provisioning is approved and complete, the
variable is unset and the workflow retains `light`. The bridge lane only checks
out trusted PR heads, pushes them and polls GitHub; it does not compile. Keep the
existing publication lane separate. Existing runs keep their assigned runner;
do not cancel or retry them merely to migrate labels.

GitHub caches are scoped to a branch and the default branch. Unique
`forgejo-pr/...` refs cannot restore caches saved by sibling refs. The hosted
workflow therefore also runs on mirrored `main` to populate the native cache.
Only `main` saves it; PRs restore it and always execute the native gate. A main
run skips compilation when that exact dependency/toolchain/patch key is already
cached. The first baseline fill is cold. Cache eviction remains safe: it causes
a rebuild, never a skipped PR gate. No LAN cache credentials go to GitHub.

Measure remote queue time separately from native step time. Two simultaneous
observed macOS jobs are not proof of the account concurrency quota; confirm that
quota in GitHub organization settings before requesting a plan change. Moving
bridge waiters off `light` does not increase GitHub execution capacity.
