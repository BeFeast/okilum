# M4 action-host removal (#1557 subset)

This slice removes external action fetches from `ci/native-scope` (the prerequisite
of `ci/macos-local`), `ci/macos-local`, all three `macos-release` jobs, and
`release-publish/publish`. The remaining OS lanes are a later #1557 slice.
`sprint.yml` is untouched; its plain-git checkout at
`3028bcd2efc7c93d236ad00ec2adcf2d01dba608` is the reference pattern.

## Source and trust

- Public source fetches use `https://git.oklabs.uk/${repository}.git`, without a
  token, credential helper, URL rewrite, or persisted authorization header.
- `macos-local` retains the exact PR **head** SHA. `native-scope` retains the
  default checkout identity (`github.sha`, including the event merge SHA when
  supplied). Its base-SHA fetch and native selection logic are unchanged.
- Release jobs retain `github.sha`, not a moving main tip or an independently
  selected product source. Full-history jobs retain all branches/tags and
  `origin/main` for ancestry/provenance validation, including on a previously
  shallow workspace. Every checkout verifies HEAD and cleans stale outputs.
- The source has no `.gitmodules`; the old checkout did not request submodules.
  No recursive submodule fetch is added.
- The pinned gpui-kit clone and cumulative vendor patch verification remain in
  both Mac entry scripts. Rust setup, Sparkle fetching/verification, and Cargo
  dependencies are unchanged; this slice removes action-host dependencies, not
  all external build inputs.

## Artifact mirror

With explicit authorization, the public pull mirror
<https://git.oklabs.uk/BeFeast/actions-upload-artifact> was created from
<https://github.com/actions/upload-artifact.git>. Both upstream GitHub and the
previous Forgejo action host resolve `v3` to
`ff15f0306b3f739f7b6fd43fb5d26cd321bd4de5`; the local mirror serves that identical
commit. Workflow consumers pin this full SHA rather than the moving tag.
The upstream MIT license is retained (copyright GitHub, Inc. and contributors).

At that commit, `action.yml` runs bundled `dist/index.js` with Node 16. It has no
composite/nested actions, pre/post installer, submodule, package-install step, or
runtime action/code download. The artifact client uses `ACTIONS_RUNTIME_URL`,
`ACTIONS_RUNTIME_TOKEN` and upload resource URLs returned by that service. Those
are the existing runner/service artifact endpoints, not an external action host.
No server configuration or services were changed.

Both existing uploads keep their names, paths, conditions, error behavior and
retention. The signed `macos-publication` stage and protected dispatch are
unchanged. Publication already downloads archives through the Forgejo API in
`publication.py`; no download action needs mirroring. Source/run eligibility,
Sparkle signatures, notarization and publication secrets/permissions are unchanged.
The temporary Mac Beta grant through 2026-10-26 03:00 UTC and Stable behavior are
unchanged.

## Review and verification

Prepared in an isolated worktree and rebased onto main
`23c022e6a15d194d7ab4cf9e28bc21ca4e41fb5a` after #1571 merged. The original
prepared commit `4282b6e9` and parent worktree remain backed up. Submit one
canonical PR with normal CI; independent review and merge belong to the parent
and manager. The sccache change `33a0e8b7` is separate and is not included.

Local verification is limited to Python AST, YAML structure, `bash -n`, and diff
checks. Structural comparison preserves every workflow field and step other than
checkout replacements and upload action URLs. No tests, builds, containers or
artifacts were executed locally.

`test_m4_checkout.py` is collected by the existing hosted CI Python suite. It runs
the actual inline checkout scripts against disposable Git fixtures with distinct
base/head/merge commits, stale shallow workspaces, full-history ancestry checks,
missing/invalid source failures, and credential/config persistence checks. Existing
native routing and release/publication regressions remain in the hosted suites.
A green hosted run and real artifact transport remain unproven until normal PR
CI completes; this patch does not claim completion of full #1557 acceptance.
