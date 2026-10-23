# M4 action-host removal (#1557 subset)

This slice removes external action fetches from `ci/native-scope` (the prerequisite
of `ci/macos-local`), `ci/macos-local`, all three `macos-release` jobs, and
`release-publish/publish`. The remaining OS lanes are a later #1557 slice.
`sprint.yml` is untouched; its plain-git checkout at
`3028bcd2efc7c93d236ad00ec2adcf2d01dba608` is the reference pattern.

## Source and trust

- Source fetches use `${CHECKOUT_SERVER_URL%/}/${CHECKOUT_REPOSITORY}.git`.
  Both values come through step env from `github.server_url` and
  `github.repository`; no context expression is interpolated into shell source.
  The runner's existing origin-scoped authentication is inherited. These public
  repositories need no new token/helper/rewrite or persisted authorization header.
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
commit. Two inline bootstraps fetch this full SHA through
`${ACTION_SERVER_URL%/}/BeFeast/actions-upload-artifact.git`, verify HEAD, then
invoke the fixed local `uses: ./.forgejo-local-actions/upload-artifact` action.
The bootstrap immediately precedes upload and replaces stale action contents.
The diagnostic bootstrap is `always()` and non-blocking like its upload; the
publication bootstrap remains required and main-only. There is no remote `uses`
clone during job preparation and no unsupported dynamic expression in `uses`.
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

## Runner context and remaining network dependencies

Run 10816 (API 16612) failed before executing any Mac shell step: job 38170,
task 24191 could not resolve `git.oklabs.uk` while preparing the remote upload
action. Native-scope job 38163 completed its inline checkout successfully.
The manager confirmed the M4 runner instance is `http://10.10.0.27:3000`;
the existing M4 handover documents the same registered endpoint. A read-only LAN
Git advertisement request returned 200 with no redirect to a public hostname.

Runner v13.0.0 source (commit `1a633ad51320631293dfcac99755b13659efe784`)
confirms `internal/app/run/runner.go` derives `GITHUB_SERVER_URL` and
`ACTIONS_RUNTIME_URL` from the registered instance address. Its
`act/runner/run_context.go` uses that server URL for the GitHub context, and
`act/runner/step_action_local.go` loads local actions during the main step,
after the preceding bootstrap. Its remote-action auth uses an origin-scoped
extraHeader; no global rewrite or TLS exception is introduced by this patch.

This removes public-host DNS from the scoped source/action bootstrap, not from
all build and publication operations. Existing genuine dependencies remain:

- gpui-kit fetch: `github.com/longbridge/gpui-kit`, exact revision in
  `vendor-setup.sh`; vendor patches and verification are unchanged.
- Sparkle 2.10.0 archive: `github.com/sparkle-project/Sparkle/releases/download/…`,
  plus GitHub's release CDN redirects; the archive/closure hashes are checked.
- rustup toolchain/target downloads: rustup's configured distribution server
  (normally `static.rust-lang.org`); Cargo registries/git sources as locked;
  conditional sccache binary download from `github.com/mozilla/sccache`.
- Apple notarization and stapling use Apple's services through `notarytool` and
  `stapler`; no proxy or replacement endpoint is invented.
- `coalesce.py` checks public release markers at `updates.befeast.com`;
  publication writes to configured `R2_ENDPOINT` and emits public update URLs at
  `updates.befeast.com`. These are genuine release destinations.
- Forgejo API dispatch/publication uses the existing env `FORGEJO_URL` from
  `github.server_url`; archive download URLs come from that API. Artifact
  uploads use the runner's LAN `ACTIONS_RUNTIME_URL` and returned resource URLs.
  The hosted shipping bridge retains its existing GitHub API/mirror dependency.

No host DNS, server/service configuration, signing credential, Stable grant or
artifact destination was changed. Exact-head M4 CI and independent review are
required again after the LAN fix; approval of `5e22b2b7` does not approve it.

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
missing/invalid source failures, credential/config persistence checks, origin-scoped header matching, repository
namespace selection, shell-injection resistance, and pinned local-action bootstrap. Existing
native routing and release/publication regressions remain in the hosted suites.
A green hosted run and real artifact transport remain unproven until normal PR
CI completes; this patch does not claim completion of full #1557 acceptance.
