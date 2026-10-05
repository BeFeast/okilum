# macOS checks before merge

`ci / check` is the required aggregate gate: Linux must pass, then code PRs
must pass a native M4 job. Only root Markdown files and Markdown under `docs/`
are exempt; mixed changes and unknown paths require macOS. Main keeps its
existing macOS release checks. Superseded PR runs are cancelled; main releases
are not. Native work starts only after Linux passes, avoiding wasted M4 builds.

The native job compiles Reader production and test targets in release mode and
runs the APFS/invalid-filename regression, four file-editor locking/recovery
regressions, and isolated native pasteboard tests. Every test filter must match
at least one test. No signing, notarization, package or upload happens on PRs.
This is a small platform smoke suite, not the complete macOS test suite.

## Why native, and runner cost

Linux `cargo check --target aarch64-apple-darwin` cannot check this project with
just rustup: the shell build script invokes `xcrun` for its Objective-C bridge
and needs the macOS SDK and Sparkle. Cross-checking also cannot reproduce APFS
`EILSEQ`. It would not cover both observed main-release failures.

On 2026-10-05, the last 12 successful releases took 4:35–5:35. Build 6490 spent
2:14 in its test stage. Budget roughly 3–5 additional M4 minutes per warm code
PR, to be checked against the first native run; cold dependencies cost more.
The job has a 20-minute cap and shares the release toolchain/profile/target
cache, avoiding a second debug cache. With one runner slot, PRs and releases
queue serially; no job interrupts a release. At ten code PRs/hour, the estimate
adds 30–50 runner-minutes/hour; combined with releases, ten merges/hour
would exceed one runner’s capacity. Avoid stacking superseded revisions. Documentation changes consume no M4 slot.

The host has a signing keychain, so fork PRs never execute on it. A maintainer
must review and copy approved changes to a same-repository branch; a code PR
whose native job is skipped does not pass the aggregate gate. The workflow
lives under `.forgejo/`; GitHub mirrors do not automatically execute it.

## Portable filesystem fixtures

`cfg(unix)` is not proof that a filename can be created on every Unix filesystem.
Use valid Unicode filenames for portable fixtures. Test malformed file contents
separately from malformed filenames. Gate deliberately Linux-specific filename
fixtures with `cfg(target_os = "linux")`, keeping a meaningful macOS fixture and
assertions enabled. Never swallow arbitrary fixture-creation errors to make a
test pass. New platform filesystem regressions belong in the native smoke list
in `scripts/ci/check-macos.sh` as well as ordinary Linux tests. Startup/index
performance tests and product code are unchanged by this gate.
