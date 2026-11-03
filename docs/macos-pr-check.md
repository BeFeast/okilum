# macOS checks before merge

macOS PR checks run on **GitHub-hosted arm64 runners**, through
`forgejo-macos / macos`. Do not run CI on the corporate M4 or SSH to a Mac.
The Forgejo `ci` workflow bridges trusted same-repository PR heads to unique
`forgejo-pr/<number>-<sha>-...` refs on the GitHub mirror. It reports the hosted
result back to Forgejo; no Forgejo credentials reach the hosted runner.

Open a same-repository PR with the committed head pushed to Forgejo. Follow its
`ci / macos-github` job to the exact-head GitHub Actions run and check that
`forgejo-macos / macos` executed successfully (a skipped job is not evidence).
If a run is superseded, use the replacement run for the current PR head. The
bridge is implemented in `scripts/ci/github-macos.py`; the hosted workflow is
`.github/workflows/forgejo-macos.yml`. Main's supplementary native checks follow
the hourly beta cadence. Root/docs Markdown-only PRs are exempt from the PR lane.

`scripts/ci/check-macos.sh` verifies the vendor patches, compiles the production
Reader and **every workspace test target** in release mode, then runs selected
native regressions. This includes all eleven portable native Manager modules
(`attachments`, `composer`, `discuss_handoff`, `drafts`, `express_session`, `feed`,
`links`, `message_types`, `native`, `rotation`, `threads`) and the shared
ProjectView, models and connections tests. Each filter must list at least one
test before executing; each suite reports its passed/failed/ignored counts.
A failing suite remains a failure even if later independent suites pass.
Link the exact-head hosted run and native Manager counts in the PR.

The shell build script defines `cfg(native_manager)` on Linux and macOS only.
This is compilation coverage, separate from the platform's runtime panel choice:
macOS still mounts WKWebView until #2522. Its retained ProjectView does not
implicitly restore a native token or start a native stream. Linux keeps its
native Manager, and Windows keeps its existing path.

Linux cross-compilation cannot exercise the macOS Objective-C bridges, SDK,
Sparkle, APFS semantics or AppKit pasteboards. Hosted native checks use the same
release profile and shared dependency cache as the main lane. They do not sign
or publish a shipping app. The isolated Sparkle fixture uses its own test identity.

## Portable filesystem fixtures

`cfg(unix)` is not proof that a filename can be created on every Unix filesystem.
Use valid Unicode filenames for portable fixtures. Test malformed file contents
separately from malformed filenames. Gate deliberately Linux-specific filename
fixtures with `cfg(target_os = "linux")`, keeping a meaningful macOS fixture and
assertions enabled. Never swallow arbitrary fixture-creation errors to make a
test pass. New platform filesystem regressions belong in the native smoke list
in `scripts/ci/check-macos.sh` as well as ordinary Linux tests. Startup/index
performance tests and product code are unchanged by this gate.
