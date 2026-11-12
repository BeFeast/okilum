# AGENTS.md — Okilum

Working process: **Okilum — how we work (v3, 02.11)**, supplied by the task
owner, is authoritative. This file adds repository-specific constraints;
linked documents define product behavior and engineering checks, not additional
process or approval gates. `CLAUDE.md` only imports this file.

Read `docs/PRD.md` before product-shaping changes; it is the product contract.
This repository holds product code. The four-candidate spike sandbox is evidence,
not a source for bulk imports; porting requires an issue and a stated reason.
Keep design decisions in `docs/` or the project brain, not only in commit messages.

## Product boundaries

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

## Vendored `gpui-kit`

- Keep patches as diffs, applied idempotently by the vendor script. Fresh clones
  are unpatched: verify application before trusting a build.
- **gpui core stays unpatched.** An approach requiring a core patch fails.
- Exception (Oleg, 2026-11-12 09:05 Israel): the separately reviewed `gpui-pre-windows` DirectX atlas-upload vendor patch is allowed; core, editor, shaping, thresholds and stack reserve stay unchanged.
- Keep the patch set small and upstream it to `longbridge/gpui-kit`.

## Tests and measurements

- Agent builds/tests default to `H=github scripts/agent/gh-check.sh` from a
  committed worktree; see `docs/agent-checks.md`. Use local build slots only for
  GitHub outages or checks requiring local hardware. Never run Cargo on maestro
  except `cargo fmt`.
- A regression test must fail without the fix. Probes need controls that exercise
  the measured path, so an unfired test cannot look like success.
- Compare timings on the same hardware in the same session.

## Conventions

- Code, comments, commits and PR text in English.
- Rust edition/toolchain pinned; commit `Cargo.lock`.
- The pin tracks the latest stable Rust. On each stable release, the bugs-main
  lane opens the toolchain bump PR the same day. The pin provides reproducibility:
  CI and every development host use the same compiler through rustup; it is not
  a reason to stay on an older release (Oleg's decision, 2026-11-12).
- No secrets in the repository; use Infisical references.
