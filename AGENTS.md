# AGENTS.md — Okilum

## Purpose and layout

Okilum is a local-first desktop Markdown knowledge environment (Rust, GPUI).
Read `docs/PRD.md` before product-shaping changes; it is the product contract.
This repository holds product code. The four-candidate spike sandbox is evidence,
not a source for bulk imports; porting requires an issue and a stated reason.
Keep design decisions in `docs/` or the project brain, not only in commit messages.

- `crates/` — the Rust workspace (`okilum-shell` desktop app, `okilum-core`, `okilum-gpui`, sync, write authority, T3, HTTP and packaging crates).
- `docs/` — design and engineering documents (`docs/agent-checks.md`: the check suites; `docs/upstream/`: GPUI patch records).
- `scripts/` — CI, release, signing and vendor tooling (`scripts/agent/gh-check.sh`).
- `.github/workflows/`, `.forgejo/workflows/` — CI on GitHub (agent checks, native suites) and Forgejo (releases, publishing).
- `tests/`, `fixtures/`, `web/`, `inbox/`, `experiments/`, `design/`, `specs/`.

## Commands

| Step | Command | Where | Time |
|---|---|---|---|
| Merge check | `cd ~/src/dev-pulse && python3 -m devctl gate okilum <pr>` (READY/WAIT) | maestro | seconds |
| Fast check (optional, not a merge gate) | `H=github scripts/agent/gh-check.sh --fast` on the exact HEAD | GitHub, `agent-checks` | 5–15 min, median ~6.5 (16 green runs before 2026-11-21) |
| Full set | `scripts/agent/gh-check.sh` (no `--fast`) + the Forgejo full gate | GitHub pool shards, Forgejo | not measured here; see `docs/agent-checks.md` |
| Format | `cargo fmt` | any host, the only Cargo command allowed on maestro | — |
| Local hardware checks | `okilum-slot run` (build slots) | build-slot pool | not measured |
| One-line shell rebuild | `scripts/verify reader-basic --measure-incremental` (build portion) | Linux rig build slot, 3 jobs | **55.553 s** |
| Launch to first frame | same run, GPUI `ui_scale/first_frame` callback | headless sway, 1366×768 | **0.714 s** process wall (callback: 0.688 s) |
| Reader acceptance, unchanged source | `scripts/verify reader-basic` | same warm slot, build + rig + OCR + screenshot | **7.059 s** in slot; **11.617 s** command wall, including queue/source/evidence transfer |

Measured 2026-11-23 on `dd6c156d7`, same Linux host/session: a temporary
`std::hint::black_box(());` line in `okilum-shell/src/ui_zoom.rs::init` was
compiled, then the source restored. The pool uses `CARGO_INCREMENTAL=0`:
“incremental” here means a warm dependency build, not rustc's incremental mode.
The unchanged-source check spent 1.535 s in vendor verification + Cargo; its
first frame took 0.564 s. The first run encountered a cold/oversized target;
these warm measurements exclude that automatic pool cleanup.
After restoring the changed line, the ordinary verify command took 70.853 s
end to end (60.286 s vendor + rebuild, 65.756 s total in the slot).

### Native acceptance

```bash
scripts/verify reader-basic
scripts/verify hr-ornament
scripts/verify ui-zoom-step
```

`scripts/verify` builds incrementally: it passes `--env CARGO_INCREMENTAL=1
--env BUILD_SLOT_SCCACHE=off` to `okilum-slot run` (sccache refuses
incremental, so it is off for these runs only). Measured 2026-11-23/24 in the
pool: a warm one-line `okilum-shell` rebuild takes ~72 s non-incremental and
~17 s incremental (verify on okilum-dev, same slot, one-line change: build step 30 s including vendor setup and the Cargo no-op checks). The first incremental build in a slot is slower (cold,
166–220 s on okilum-dev; the first slot ended at 19.96 GB, over the 16 GiB cap) and adds about 4.5–5 GiB to that slot's target (cap
16 GiB on okilum-dev, 20 GiB elsewhere); a target over the cap is emptied, so
the next run there is cold. Slots are assigned by the pool, so consecutive runs
can land on different slots and each pays the cold build once. Only verify
runs are affected: CI, full checks and release/beta builds stay
non-incremental, and pool config/`BASE_ENV` are untouched. Opt out with
`OKILUM_VERIFY_INCREMENTAL=0`.

`scripts/verify` reserves 6 GiB per run (`okilum-slot run --mem 6`) instead of
the project default 9 GiB, so two verifies fit okilum-dev's 12 GiB budget and
run concurrently. Measured: 44 verify runs over 3 days peaked at 5.2 GiB anon.
Override with `OKILUM_VERIFY_MEM_GB=<GiB>`.

Each command streams the working tree (including an uncommitted acceptance
file) into `okilum-slot`, builds the app alone in its shared target, starts an
isolated 1366×768 Wayland rig, and returns 0/1 with PNGs, `result.json`, source
identity and the slot receipt under a unique `~/work/qa-evidence/verify-*`.
That directory is scratch: preserve wanted evidence before its daily cleanup.
The default is the existing `okilum-dev` rig host; `OKILUM_VERIFY_HOST` selects
another pool host with sway, wtype, wlrctl, grim, dbus-launch, ImageMagick,
Tesseract English and Python PyYAML. `--binary` skips building for bisect on a
Linux rig host. `--output` in pool mode must stay under `~/work/qa-evidence`.
Steps use stable ids for evidence, keyboard keysyms or fixed pixel coordinates;
text checks OCR the screenshot, pixel checks count a stated colour/region,
and zoom checks combine actual window scale with a screenshot difference.
There is no app-side automation protocol or new test framework.

For external deep links, `outside_note` creates `outside/External note.md`
beside the synthetic vault. A step with `open_url: okilum://file/{outside_file}`
launches the same binary with that URL argument in the isolated environment,
exercising single-instance forwarding. The placeholder is percent-encoded,
including its space; consent is handled with ordinary keyboard/click steps.

Day-0 regression proof (2026-11-23): `hr-ornament` on `180ae5779` (parent of
the #3252 fix merge) built/launched and read both paragraphs, then exited 1:
0 pink pixels in the centre region. The same scenario on `3feba9093` exited 0
with 28 pixels. Only the harness was added to the old worktree; its ignored
generated Windows vendor tree was regenerated for the pre-#3257 bootstrap.
`ui-zoom-step` observed 1 → 1.05 → 1 and 0 changed pixels after reset.

A Mac variant needs a permitted Mac build host plus a WindowServer session,
screen capture and CGEvent/Accessibility input, replacing the Wayland `Rig`
and translating keysyms/coordinates at a fixed scale. The YAML assertion model
can stay the same. This Linux work does not build or run on Oleg's M4.

### How to add a scenario

1. Before implementing a UI change, write its initial state → action → visible result.
2. Copy the closest `acceptance/*.yaml` into a file named after the new scenario id.
3. Give each step a stable, unique `id`; it becomes the screenshot filename.
4. Use a synthetic `note`, explicit appearance/preferences, and a note-pane `region`.
5. Set `ready_text` to fixture content that must be visible before actions start.
6. Use `click: [x, y]`, `keys`/`mods` and optional `wait` at the fixed 1366×768 size.
7. Assert visible `text`, bounded colour `pixels`, `ui_scale`, or `changed_from`.
8. Run the new scenario before the fix and confirm a visible assertion fails, not the build.
9. Implement the fix, run it green, and inspect the saved PNGs plus `result.json`.

Large fixtures and timing (no big files in git):

- `generate:` is a list written into the isolated vault before launch:
  `{path, kind: markdown, mib|bytes, first_line, last_line}` (exact size, `# first_line`
  heading first, `last_line` last) or `{path, kind: json, mib|bytes, valid: true|false,
  error_at: end|middle, single_line}` (one array, exact size; invalid = trailing comma at
  the end or a bare word in the middle). `open: <path>` launches that note instead of
  `scenario.md` (`note:` is then optional).
- A step with `max_ms: N` and a `text`/`pixels` condition is timed from the input event
  to the first screenshot that satisfies the condition. Frames are grabbed back to back
  and read afterwards, so OCR adds no latency. `result.json` has `ms` (when that grab
  finished, an upper bound; it is the gated value) and `ms_lower` (when it started).
  The condition must be false before the action, otherwise the step fails.
- `probe:` inside a timed step: `{after_ms, keys|click, mods, text|pixels, region, max_ms}`
  issues a second input `after_ms` after the first, while the long operation runs, and
  requires its visible effect within `max_ms` of the probe (`probe.ms` in the result).
  Choose a probe whose condition differs from the main one.

Fixture vaults and file ages:

- `vault: <repo-relative dir>` copies that directory (for example
  `tests/fixtures/verify-vault-smoke`) into the isolated vault before launch. It combines
  with `note:`, `generate:` and `open:` (`open:` names a vault-relative note).
- `mtime: {<vault-relative path>: <days_ago>}` sets file mtimes after the copy and
  generation. The runner stats each file, fails on a mismatch, prints a `verify: mtime` line
  and records `vault_mtimes` in `result.json`.

Word geometry:

- `layout:` on a step is a list of `{a, b, axis, min, max, more_than, less_than}` rules
  over Tesseract word boxes (`a`/`b` are single words, first exact match). The gap is
  `top(b) - top(a)` on `axis: y` (default) or `left(b) - right(a)` on `axis: x`, in
  screenshot pixels. `more_than`/`less_than: {a, b, axis, by}` compare it with another
  pair's gap, so a rule like "one empty line more than a paragraph break" does not hard-code
  font metrics. `result.json` records every gap (`acceptance/markdown-breaks.yaml`).

10. Run affected scenarios before pushing; during the direct-session experiment this own smoke replaces the daytime PR/CI gate.

## Merge gate

Rule as of 23.11 (pilot to 28.11 15:00; project area `AGENTS.md`, Oleg 21.11 and 23.11):

- By day there is no CI wait. A PR merges as soon as
  `cd ~/src/dev-pulse && python3 -m devctl gate okilum <pr>` says READY. The
  gate owns the logic (mergeable head, risk classes, OLEG-OK, night-red hold);
  do not restate or second-guess it here.
- The session that wrote the PR merges it itself, at the exact head
  (`head_commit_id`), then confirms the install: M4 Sprint on the Mac via
  `devctl m4tick`, and idunn. There is no merge lane and no manager-only merge.
- Risk classes (data loss, install/update, signing, security; devctl
  `risk_paths` / `risk_labels`) need one review PASS by another model family
  first. Updater/velopack changes need Oleg's word (`OLEG-OK <head>` on the PR).
- Never merge a PR whose latest review verdict is «fix first».
- Full CI (`gh-check.sh` without `--fast` plus the Forgejo full gate) and the
  review of the day's merges run only in the night window 03:00–05:00 Israel;
  findings become issues. See `docs/agent-checks.md`.
- Use local build slots only for GitHub outages or checks requiring local
  hardware. Never run Cargo on maestro except `cargo fmt`.

## Hard rules

### Product boundaries

- **The v0 reader remains read-only.** The AI Brain writing phase follows
  `docs/ai-brain-poc.md` and `docs/ai-brain-contracts.md`. Use lossless,
  revision-aware source APIs; MCP `read_note` is rendered input. Do not widen
  the reader protocol or migrate unrelated notes.
- **Derived data must be rebuildable** from canonical files. Durable dispatch
  intents, external identities, acknowledgements and replay cursors belong
  outside the deletable index directory.
- **Keep the product under test isolated.** POC/alpha test goals must not manage
  Okilum's own development or migrate daily work. This is a product boundary,
  not an approval step for development.
- **Quality gates do not move.** Replace a failing library or write the component.
  Development cost only breaks ties between passing options.
- **Surface ambiguity.** Unresolved identity or conflicting facts stay visible;
  never silently pick a winner.

### Vendored `gpui-kit`

- Keep patches as diffs, applied idempotently by the vendor script. Fresh clones
  are unpatched: verify application before trusting a build.
- **Core GPUI (`gpui-pre*`) patches are allowed with Oleg's OK.** If an approach needs one, keep the approach and bring it to Oleg through the manager with its cost: what changes, upkeep on GPUI upgrades, and alternatives. Once approved, ship it as its own small PR through the checksum-pinned vendor patch mechanism (as in `scripts/gpui-pre-windows-setup.py`), with tests and manager review; write the patch to be upstreamable (minimal, no Okilum-specific hooks). Every proposal includes an upstream plan: identify the actual owning repository/crate and publisher with source citations, assess whether the change is general enough to submit, and plan submission where possible. The `gpui-pre*` snapshots are published by [Jason Lee (`huacnlee`)](https://crates.io/api/v1/crates/gpui-pre/owners); their [package metadata](https://docs.rs/crate/gpui-pre/0.3.3/source/Cargo.toml.orig) identifies [zed-industries/zed](https://github.com/zed-industries/zed) as the source (`crates/gpui`, `crates/gpui_platform` and the OS backend crates), so distinguish the snapshot publisher from the code owner. Opening any upstream issue or PR requires Oleg's separate approval for that patch.
- Exception (Oleg, 2026-11-12 09:05 Israel): the separately reviewed `gpui-pre-windows` DirectX atlas-upload vendor patch is allowed; core, editor, shaping, thresholds and stack reserve stay unchanged.
- Approved (Oleg, 2026-11-13, #2825): the two cold first-window macOS patches, `gpui-pre-macos` (no `tabGroup` query without tabbing) and `gpui-pre-apple` (Metal library/pipelines built once off the main thread), pinned by `scripts/gpui-pre-apple-setup.py`; see `docs/upstream/gpui-pre-macos-first-window.md`.
- Approved (Oleg, 2026-11-17, #1615): external drops of text, HTML, RTF, URLs and image data, `gpui-pre` (`ExternalData`, `FileDropEvent::DataEntered`/`DataSubmit`) and `gpui-pre-macos` (data drag types, contents read at the drop), macOS first; see `docs/upstream/gpui-external-data-drops.md`.
- Approved (Oleg, 2026-11-12 approach, implementation started 2026-11-22, #2848): application UI scale on `gpui-pre` (`Window::set_ui_scale`, logical viewport and pointer coordinates divided by the zoom, outgoing native rectangles multiplied back); see `docs/upstream/gpui-pre-ui-scale.md`.
- Approved (Oleg, 2026-11-24, #3261): `gpui-pre-linux` Wayland `set_client_inset` also sends the new xdg window geometry, so a floating client-decorated window does not keep the old inset's outline after a UI zoom change; see `docs/upstream/gpui-pre-linux-client-inset-geometry.md`.
- Keep the patch set small and upstream it to `longbridge/gpui-kit`.

### Tests and measurements

- idunn is used interactively by Oleg: keep there only what needs idunn, and run
  every build/test as `flock ~/work/okilum-idunn-build.lock nice -n 19 ionice -c3 …`.
- A regression test must fail without the fix. Probes need controls that exercise
  the measured path, so an unfired test cannot look like success.
- Compare timings on the same hardware in the same session.

### Scratch & cleanup (Oleg, 2026-11-20)

idunn reached 92% full on 2026-11-20, about 400 GB of it agent leftovers (per-copy `target/`, binaries in evidence, unbounded ship rollbacks, root-owned pcaps). `~/.local/bin/idunn-reaper` (daily 04:30) enforces these rules on idunn; on other hosts follow them yourself.

- Scratch lives only in `~/work/<project>-<issue|lane>/`, evidence only in `~/work/qa-evidence/<topic>/`. Never use `~/src`, `~/Developer`, `~/.cache`, `~/.local/share` or `~/.local/state` as scratch; the `~/src/toroid-staged/*`, `~/src/toroid-verify-*` and `~/src/axe3-*` copies are legacy and are reaped after 7 idle days.
- Rust builds go through the shared build-slot pool (`okilum-slot` or `build-slot run --project okilum`) or one shared `CARGO_TARGET_DIR`; no per-copy `target/`.
- Evidence stores a binary's sha256 or receipt, never the AppImage, package or test binary itself.
- At the end of a lane its owner deletes `target/` and binaries and keeps logs and JSON. Something needed longer gets a `.keep-until YYYY-MM-DD` file in its folder, at most 7 days ahead.
- Sprint and review copies (`~/work/okilum-<issue>`) build through the pool, not with their own `CARGO_TARGET_DIR`: on 2026-11-20 about 200 GB of such targets were left behind after merge. Build scripts that create per-build directories (for example `~/work/bm-native-<sha8>`) delete them when the build ends.
- Toroid lanes on the same host follow the same rules (ship rollbacks kept to 3, pcaps deleted after a queue); see the toroid repository's `AGENTS.md`.
- Reaper thresholds: qa-evidence entries 72 h idle (non-text files over 50 MB after 24 h), `~/work` entries 7 days (cargo targets 3 days), legacy `~/src` scratch 7 days (pcaps 2 days), targets under `~/src`, `~/dev`, `~/Developer` 14 days; halved when `/home` has less than 60 GB free. Idle means the newest mtime in the whole tree. A checkout with uncommitted, unpushed or stashed work, or without a remote, is reported and kept (only its `target/` goes).

### Conventions

- Code, comments, commits and PR text in English.
- Rust edition/toolchain pinned; commit `Cargo.lock`.
- The pin tracks the latest stable Rust. On each stable release, the bugs-main
  lane opens the toolchain bump PR the same day. The pin provides reproducibility:
  CI and every development host use the same compiler through rustup; it is not
  a reason to stay on an older release (Oleg's decision, 2026-11-12).
- No secrets in the repository; use Infisical references.

## Public-repo hygiene

The repository is public (Forgejo, mirrored to GitHub). Commit no secrets, personal absolute paths, or new internal addresses; use Infisical references.

## Process

Working process lives in the private project area `Dev/Areas/okilum/AGENTS.md` («Okilum — how we work») of the management vault; it wins on process. Linked documents define product behavior and engineering checks, not additional process or approval gates. `CLAUDE.md` only imports this file.
