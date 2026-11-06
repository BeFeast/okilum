# macOS checks before merge

macOS validation runs on **GitHub-hosted arm64 runners**, through
`forgejo-macos / macos`. Do not run CI on the corporate M4 or SSH to a Mac.
Forgejo CI does not run on PRs. The scheduled/manual `ci` workflow
can bridge trusted same-repository heads
to unique `forgejo-pr/<number>-<sha>-...` refs on the GitHub mirror and reports
the result back to Forgejo. No Forgejo credentials reach the hosted runner.

For an implementation PR, explicitly run the exact committed head on the hosted
lane. From a clean worktree, with the existing host `gh` login:

```bash
PR_NUMBER=2524 # replace with your Forgejo PR number
macos_ref="forgejo-pr/$PR_NUMBER-$(git rev-parse HEAD)-$(date +%Y%m%d%H%M%S)"
git -c credential.helper= -c 'credential.helper=!gh auth git-credential' \
  push https://github.com/BeFeast/okilum.git "HEAD:refs/heads/$macos_ref"
gh run list -R BeFeast/okilum --branch "$macos_ref" --workflow forgejo-macos.yml
```

Open that GitHub Actions run and verify its SHA equals the PR head. A skipped
`macos` job is not evidence; use a completed successful native job. Superseded
PR runs are cancelled, so use the replacement for the current head. The bridge
is implemented in `scripts/ci/github-macos.py`, and the hosted workflow is
`.github/workflows/forgejo-macos.yml`. It has no main trigger: the main beta is
signed by `forgejo-macos-shipping` on every push. The full acceptance workflow exempts root/docs
Markdown-only PRs from macOS.

`scripts/ci/check-macos.sh` verifies the vendor patches, compiles the production
Reader and **every workspace test target** in release mode, then runs selected
native regressions. This includes all ten portable native Manager modules
(`attachments`, `composer`, `discuss_handoff`, `drafts`, `feed`,
`links`, `message_types`, `native`, `rotation`, `threads`) and the shared
ProjectView, models and connections tests. Each filter must list at least one
test before executing; each suite reports its passed/failed/ignored counts.
A failing suite remains a failure even if later independent suites pass.
The three `manager_ttfm_` regressions also run as an explicit non-empty group;
they live under `project::feed::` and `project::native::`, so both module filters
cover them too. `project::ttfm` contains the measurement implementation only.
Link the exact-head hosted run and native Manager counts in the PR.

The shell build script defines `cfg(native_manager)` on Linux and macOS only.
This is compilation coverage, separate from the platform's runtime panel choice:
macOS still mounts WKWebView until #2522. Its retained ProjectView does not
implicitly restore a native token or start a native stream. Explicit native
selection (`ProjectView::threads.enabled`) uses the shared pairing/Keychain
lifecycle; #2522 will enable it for the mounted Mac panel. The hosted lane runs
the credential-store double and actual pairing exchange target-OS regression
from #2519, without requiring an unlocked login Keychain. Linux keeps its
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
