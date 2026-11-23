# Views fixture vault

A small synthetic vault for declarative views (#3262). `tests/views_fixture.rs`
loads every file here, so the fixture cannot rot before the `view` module uses it.
Golden outputs (later tasks) run at a fixed `as_of`; dates below are chosen
around 2026-11-24.

Layout:

- Lifecycle layout (26 notes): `state/<domain>/<context>/…`,
  `log/<domain>/<context>/<kind>/<date>-<slug>.md`, `knowledge/<practice>.md`,
  `sources/<channel>/<yyyy>/<mm>/<dd>/…`, `inbox/…`.
- Old (PARA) layout (3 notes): `Projects/`, `Areas/`, `Resources/`. They get no
  path-derived fields (`layer`, `context`, `kind`, `channel`); the gardener
  reports them.
- `@views/`: the 7 example views and `changes.jsonl` (gardener change log rows).
- `@Schema/view.schema.json`: the Okilum view schema, draft v0.
- `@Schema/fields.yaml`: copy of the Gardener assistant's field contract
  (version 1-draft), with two comment lines changed to drop a host name and a
  local address. TODO: resync this copy when the contract is frozen in the
  vault `@Schema/`.
- `@Schema/tests/view-invalid/`: 9 views the schema must reject. They sit under
  `@Schema/`, which view queries exclude, so they never become rows and a
  directory validation of `@views/` stays clean.

Deliberate edge cases:

- `knowledge/sleep-hygiene.md` has `area: health`, the lower-case alias of Health.
  Exactly 4 notes are in Health.
- `state/dev/okilum/roadmap.md` and `state/dev/tasker/roadmap.md` share a name;
  `state/dev/common/conventions.md` links `[[roadmap]]`, which is ambiguous.
- `state/homelab/backups/runbook.md` stores `layer: knowledge` on a `state/`
  path: the path wins and the conflict is a finding.
- `state/dev/okilum/questions.md` and `state/dev/tasker/questions.md` hold tasks
  for `@views/questions.md`; only one of them has a `thread`.
- `state/dev/okilum/index.md` has the «Write view schema» task and the Now /
  Next / Blockers sections the Okilum and agent-context views read.
