# Artifact-only native macOS verification

`macos-native-verify.yml` is a manual, first-attempt, trusted-main workflow. It
does not change macOS release/preview policy or lane variables. A manager must
coordinate its M4 slot after higher-priority saving/recovery gates. The nightly
03:05 beta has priority: admission refuses starts between 00:50 and 09:00 Israel,
leaving a full 90-minute job before the existing 02:20 night guard.

Dispatch inputs are `verify_only=true`, `source=<full current green PR SHA>` and
`pr=<same-repository open PR number>`, ref `main`. The coordinator reads only
the PR and required commit statuses, checks exact source identity, then checks
out that product source separately from trusted main tools. It uses the existing
build/sign/notarize/staple packaging script; it does not register or launch an app.
It runs only the fixed bundled Syncthing `--version` command, never `serve`.

Receipt checks: Info.plist source SHA, notarization Accepted, staple validation,
outer deep/strict codesign, Gatekeeper assessment, nested Syncthing identifier,
matching Developer ID Team, hardened runtime and timestamp, pinned upstream
archive digest, exact sealed post-signing digest, version2.1.6. The original and
signed digests differ because codesigning modifies the Mach-O payload. Both are
retained in the bundle provenance; no claim equates them.

Output is a Forgejo artifact containing ZIP/DMG, notarization and native verify
JSON. CI downloads the artifact/receipt into its existing `qa-evidence` directory
after the run. This is not a release and cannot update a feed: the workflow has
one `verify-only` job, no publisher/dispatch step, no feed-writing credentials,
and no access to the Sparkle private update key. Regression tests check the closed
job/step graph, `verify_only=false` admission refusal, read-only API paths, and
the full coordinator's single build invocation without publication credentials.

No automatic reruns. Any failure/infra blocker requires manager disposition.
