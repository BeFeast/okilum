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
| Fast gate (before merge) | `H=github scripts/agent/gh-check.sh --fast` on the exact HEAD | GitHub, `agent-checks` | 5–15 min, median ~6.5 (16 green runs before 2026-11-21) |
| Full set | `scripts/agent/gh-check.sh` (no `--fast`) + the Forgejo full gate | GitHub pool shards, Forgejo | not measured here; see `docs/agent-checks.md` |
| Format | `cargo fmt` | any host, the only Cargo command allowed on maestro | — |
| Local hardware checks | `okilum-slot run` (build slots) | build-slot pool | not measured |

## Merge gate

- Before merge (Oleg 13.11 ~14:20, ~15:10): `H=github scripts/agent/gh-check.sh
  --fast` on the exact HEAD: formatting, script checks and workspace clippy on
  Linux; no Rust test suites, no Windows/macOS run. The full set (`gh-check.sh` without `--fast`
  and the Forgejo full gate) runs once a day before Stable and gates Stable
  only; see `docs/agent-checks.md`. Use local build slots only for
  GitHub outages or checks requiring local hardware. Never run Cargo on maestro
  except `cargo fmt`.

Merging (manager, 13.11):

- A lane merges only PRs it authored itself. Green agent checks are not a
  merge permission.
- Never merge a PR whose latest review verdict is «fix first», or one with no
  review when a review was requested.
- PRs from manager-delegated agents (toolbar #2806–#2814, #2817, #2825/#2845,
  #2837, #2847, updater #2855, and any PR the manager names) are merged by the
  manager only.

- Until 28.11 15:00 (pilot, project area): merge on the green fast gate after the install check on the target machine; per-PR review only for data loss, install/update, signing and security changes; updater/velopack behaviour changes need Oleg's word. Full CI and a cross-family review of the day's merges run at night.

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
- Approved (Oleg, 2026-11-12 approach, implementation started 2026-11-22, #2848): application UI scale on `gpui-pre` (`App::set_ui_scale`, logical viewport and pointer coordinates divided by the zoom, outgoing native rectangles multiplied back); see `docs/upstream/gpui-ui-scale.md`.
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
