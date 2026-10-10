# AGENTS.md — Okilum

Canonical agent instructions for this repository. Read `docs/PRD.md` before any
product-shaping change; it is the approved product contract.

## What this repo is

The Okilum product code. It is **not** the spike sandbox and not the project brain.

- **Project brain** (PM tracking, design decisions, handovers) lives in Oleg's vault at
  `Dev/Areas/okilum/`. Design *decisions* go there or into `docs/`, never buried in a
  commit message.
- **Spike sandbox** — the four-candidate framework experiment — is evidence only. Do
  not copy it wholesale into this repo. Porting anything out of it is a reviewed,
  issue-tracked step with a stated reason, not a bulk import.

## Hard rules

- **The v0 reader remains read-only.** The approved AI Brain POC is a distinct
  writing phase: follow `docs/ai-brain-poc.md` and `docs/ai-brain-contracts.md`.
  Source edits use a lossless, revision-aware API; existing MCP `read_note` is
  rendered input, not the original source. Do not silently enable writes in the
  existing reader protocol or migrate unrelated notes.
- **Derived data must be rebuildable** from canonical files. Durable operational
  state (dispatch intents, external identities, acknowledgements, replay cursors)
  is not a disposable cache and must not live under the deletable index directory.
- **Develop Okilum externally in T3 Code.** Planning, execution control, review,
  debugging and recovery stay outside the product under test. The POC may drive
  an isolated test goal; it must not take over its own development or daily work.
- **Quality gates do not move.** A library that fails an acceptance gate is replaced,
  or the component is written from scratch. Development cost is a tiebreaker between
  passing options — never a reason to lower a bar.
- **Ambiguity is surfaced, not guessed.** This is the rule behind the note-identity
  model and it generalises: when Okilum cannot resolve something, it says so rather
  than picking a winner the user cannot see.

## Vendored `gpui-kit`

The GPUI shell builds against a vendored clone with a small patch set. Rules:

- Patches live as diffs and are re-applied idempotently by the vendor patch script.
  **A fresh clone arrives unpatched** — verify the patches are actually applied before
  trusting a build. This has already produced a full session of misleading evidence.
- **gpui core stays unpatched.** A repair that requires patching gpui core is a
  failure of that approach, not a licence to patch it.
- Keep the patch set small and upstream it to `longbridge/gpui-kit`. The
  cumulative diff is a number worth watching, not an accident.

## Evidence and measurement

This project has repeatedly been misled by its own instruments. Two standing rules:

- **A probe that reports an absence needs a positive control.** "Nothing happened" is
  only evidence if the run can prove it would have detected something happening. Probe
  runs that cannot distinguish "the feature is broken" from "the test never fired"
  are not evidence.
- **Compare on one machine.** Timings are not portable between hosts. A baseline and
  its comparison must come from the same hardware, in the same session, or the delta
  means nothing.

## Red main

Main is not protected against merging an outdated branch, so a merge can turn main
red after its own PR was green. CI on main reports a red Linux gate: every merge
since the last green main gets a comment, and the issue "main is red" stays open
until main is green again.

- **Whoever merged the commit that turned main red reverts it or lands a fix within
  20 minutes.** Check the suspects list in "main is red"; if your merge is on it,
  find out within those 20 minutes whether it is yours.
- Do not merge onto a red main except the revert or the fix.

## Conventions

- Code, comments, commit messages, PR titles and bodies in English.
- Rust, edition and toolchain pinned; `Cargo.lock` committed.
- No secrets in the repo — Infisical references only.
