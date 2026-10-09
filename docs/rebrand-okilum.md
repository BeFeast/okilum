# Tessera → Okilum: mechanical rename and the rename night (#970)

Stage 0 of the rebrand. Owner decision of 2026-10-09 ~14:55 Israel time: rename
**now**, without a last Tessera bridge release and without data migration.
Okilum starts with clean, new data folders; the Tessera channels are frozen as
they are. This document is the contract for the one mechanical rename PR, the
release channels and the rename runbook. The first Okilum and its update chain
is #971 (Gate D).

## The rename PR is generated, not hand-edited

`scripts/rebrand/okilum_rename.py` renames a clean checkout of the exact main
being renamed. It is deterministic and idempotent: running it twice changes
nothing, and it fails when a `tessera` line is left for no recorded reason. On
the night the PR is regenerated on the frozen main, so it cannot go stale while
other work merges.

| Changes | To |
|---|---|
| crates, packages, modules, imports, binaries, paths (`tessera-*`, `tessera_*`) | `okilum-*`, `okilum_*` |
| display name `Tessera` | `Okilum` |
| repository `BeFeast/tessera` | `BeFeast/okilum` |
| bundle ID `uk.oklabs.tessera` (+ `.intel-qa`) | `com.befeast.okilum` (+ `.intel-qa`) |
| Windows packId `BeFeast.Tessera` | `BeFeast.Okilum` |
| env vars `TESSERA_*` | `OKILUM_*` (runtime ones read the legacy name too, below) |
| workflows, build and packaging scripts, current docs | renamed with the code |

Kept on purpose (legacy compatibility; listed in the generated report):

- **Persisted schema ids and namespaces** (`tessera-…/vN`, `tessera/…/vN`).
  Renaming them changes readers and deterministic ids. New schema versions are
  separate, explicit migrations.
- **Recovery and file markers** (`.tessera-save-…`, `.tessera-source-…`,
  `.tessera-index`) and **internal URL forms** (`tessera://`, `tessera-asset://`).
  Users do not see them (owner decision 5); renaming them needs a reader for both
  forms, so it is a separate, later change.
- **Sync identities** (launchd label, Windows scheduled task and pipe, systemd
  units): moved by the sync owner with a stop/handover, never by text replace.
- **Sparkle beta preference key** (`TesseraReceiveBetaBuilds`): Okilum has a new
  bundle ID, so this key starts empty anyway; renamed with the channel work.
- **Brand assets** (`crates/okilum-shell/assets/brand/`): pinned by a hash
  manifest to the brand repository. The new Okilum symbol is imported with
  `scripts/brand-assets.py` as its own change, not by renaming text.
- **History and fixtures**: `docs/archive`, `docs/research`, `docs/upstream`,
  `experiments`, `fixtures`, test fixture trees, vendor patches. Fixtures inside
  renamed crates move with the crate but keep their contents (legacy formats are
  test input).

All `TESSERA_*` variables become `OKILUM_*` without a fallback: Okilum starts
clean, and the old names are build-time or test-only except a few developer
overrides (vault, state and index directory), which simply use the new name.

### Order

1. While the rename runs, other executors do not push (manager holds them).
2. The rename PR is regenerated on the current main, merged once the required
   Linux check is green; macOS runs but does not block during stage 0.
3. Repositories are renamed, then the mirror, tokens and bridge follow.

## Release channels after the rename

The operating model has two channels; Okilum starts with them, so no existing
user is moved between channels.

| Channel | Who | When it updates | How |
|---|---|---|---|
| **internal** | QA agent (muninn), executors | every merge to main | today's automatic per-merge publication (Linux each merge; macOS/Windows coalesced) writes the `internal` feeds |
| **beta** | Oleg | once a day | manager checks the internal build on muninn against the stage list, then dispatches `releases.yml` with that build to promote it to `beta` on all platforms |
| **stable** | everyone else | explicit owner approval | unchanged |

Old Tessera feeds are frozen as they are: no new builds, nothing removed.

## Rename runbook (fast path, 2026-10-09)

The CI executor runs the steps; the manager holds other executors and receives
one message per milestone. Each step leaves a short receipt in #970.

| Step | Done when |
|---|---|
| **Required checks for stage 0.** `main` requires only `ci / linux`; macOS still runs and reports. | Restored to `ci / check` + `ci / macos` once the first Okilum macOS build is green. |
| **Freeze.** Other executors do not push; scheduled publication is left running for Tessera until the rename merges (its feeds then freeze). | Manager confirms the hold. |
| **Regenerate and merge.** Run `okilum_rename.py` on current main, open the PR, merge on green `ci / linux`. | Merged; the residual report has 0 unexplained lines. |
| **Rename repositories.** Forgejo `BeFeast/tessera` → `BeFeast/okilum`, then GitHub. Never recreate `tessera` (it would break redirects). | Old web, git (HTTPS/SSH), API and release-asset URLs checked one by one. |
| **Mirror, tokens, bridge.** Secret `OKILUM_GITHUB_MIRROR` (same token value), lane variables `OKILUM_*_LANE`, workflow repository comparisons; dispatch a hosted run. | A Forgejo PR starts the GitHub run for its exact head. |
| **First Okilum builds.** Linux (Arch `okilum`), Windows (`BeFeast.Okilum`), macOS (`com.befeast.okilum`, same Developer ID and notary profile) into the new `internal` feeds; promote one to `beta`. | Signatures verify; installers carry the new identities; artifacts match the merged source. |
| **Verify.** Old issue/PR/release links redirect; a clean muninn install of the internal build starts with empty Okilum folders. | Gate C receipts in #970. |

If the macOS build is red after the merge, fixing it is the stage's P0.
Rollback before the repository rename: revert the rename PR. After it, roll
forward; never publish a half-renamed build.
