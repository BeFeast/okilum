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
without a running GitHub job select the existing M4 gate. Cancellation of a queued GitHub run is best-effort before fallback (the optional
Actions write permission allows cancellation; it is not needed to run the gate). A compile/test failure, cancellation,
skipped/neutral result, or execution timeout fails the gate instead of retrying
on another machine. Unknown bridge failures also fail closed. The same PR-head
SHA is checked out for the M4 fallback. This does not authorize shell access to
the M4.

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
as operational. A green M4 fallback proves the PR, not the GitHub integration.
