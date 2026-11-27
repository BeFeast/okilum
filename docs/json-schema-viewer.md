# Local JSON Schema inspection (#2994)

The Reader can explicitly route custom project extensions to its existing JSON
viewer and separately associate local JSON Schemas. This is a read-only
inspection layer over Raw, Formatted, Tree and applicable array Table views.
Source editing, Save and Undo remain explicit; schema inspection never writes,
applies defaults, repairs data or changes OS default-app associations.

## Project configuration and precedence

Create `.okilum/viewers.json` in a project directory inside the Reader's root:

```json
{
  "version": 1,
  "json": [{ "pattern": "jobs/*.samplejob" }],
  "schemas": [
    { "pattern": "jobs/*.samplejob", "schema": "schemas/general.schema.json" },
    { "pattern": "jobs/demo.samplejob", "schema": "schemas/job.schema.json" }
  ]
}
```

`.samplejob` is synthetic; no real project extension is assumed. Patterns are
case-sensitive globs relative to the project directory, using `/` separators;
`*` matches within a directory and `**` crosses directories. An extension rule
is a pattern such as `**/*.samplejob` (also matches at the project root).
Absolute paths, `..`, backslashes and URI paths are refused. There is no content
sniffing. The nearest ancestor configuration, stopping at the Reader root,
replaces any parent configuration. Within each array, the last matching rule
wins. The two arrays are independent: a schema rule alone never selects JSON.
Empty arrays opt out of inherited associations. Config errors are reported
rather than silently taking an earlier rule.

Schema association precedence is deterministic:

1. An explicit matching rule in the nearest ancestor `.okilum/viewers.json`
   `schemas` array (last matching rule wins).
2. Otherwise, a standard in-file top-level `"$schema": "./path/to/schema.json"`
   local relative path in the JSON document, resolved relative to the document's
   directory and strictly bounded inside the project/vault root. Remote URIs are
   never fetched.

Source previews honor `json` routing, as do otherwise generic attachment
cards and text previews. Dedicated CSV/TSV, media, PDF, Canvas, drawing, HAR,
archive and Log viewers retain their existing priority. JSONC keeps its tolerant viewer
unless explicitly routed to strict JSON. No system file association is changed.
Open a project directory as the Reader root, or browse it within a vault; a
single-file window cannot search outside its root for a project configuration.

Reopen the file after changing routing/configuration. The **Schema** action
shows the selected schema filename and verdict, with its full path in the
tooltip and details. **Reload schema** takes a new local schema snapshot for
the same document snapshot. Schema work is independent of ordinary JSON parsing;
Raw/Formatted/Tree/Table continue working when a schema is unavailable.

## Supported schema language

Every schema file must explicitly declare:

```json
"$schema": "https://json-schema.org/draft/2020-12/schema"
```

The supported Draft 2020-12 subset is:

- `type` (single or union: object, array, string, number, integer, boolean, null),
  `properties`, `required`, `items` (one schema for every item), boolean schemas,
  boolean `additionalProperties`, `enum`, `const`;
- `minimum`, `maximum`, `minItems`, `maxItems`, `minLength`, `maxLength`
  (string lengths count Unicode scalar values);
- `title`, `description`, `$comment`, `default`, `examples` as annotations;
  defaults/examples are never applied;
- `$defs` / `definitions` and local `$ref`: same-file or project-contained
  relative file paths (including `../` segments whose canonical target stays
  inside the project/vault root), with a literal RFC 6901 JSON Pointer fragment.
  Ref siblings also apply;
- basic composition via `allOf`, `anyOf`, and `oneOf` (bounded by the same
  step, depth, and time limits).

Remote references/catalogs, `$id`, anchors, percent-encoded reference fragments,
recursion, `format`, `pattern`, conditional keywords and all other keywords are
unsupported. They make the schema unavailable, including when found in an unused
definition. A schema-valued `additionalProperties` is also unsupported. Files
and symlinks must resolve inside the project. Missing/malformed schema files,
invalid keyword shapes, unsupported drafts and unresolved refs explain the
fallback. There is no remote schema download.

Schema validation reports exact JSON Pointers (`~0` for `~`, `~1` for `/`),
including array indices. Missing required properties name their absent path;
the containing object is their source location. Tree and Table hover show the
declared title, description, type, required/optional status and enum without
changing the actual key or value. The details dialog lists all captured
violations and the first 100 declared field projections (including absent
properties); other projected fields remain available through Tree/Table hover.
Unknown properties stay visible with an explicit absence of schema metadata.
Syntax diagnostics remain separate from schema validation. Duplicate document
properties produce an ambiguous/incomplete verdict, never a silent last-value
choice. Numeric equality accepts `1` and `1.0`; values outside
±(2^53−1) make schema validation incomplete instead of rounding large integers.

## Limits and snapshot identity

All config/schema I/O, parsing, projection and validation run on workers.
Configuration: 64 KiB, 128 rules, 512 bytes per pattern, at most 128 ancestors.
Schema: 256 KiB/file, 16 files, 2 MiB total, 4096 expanded nodes, depth 32,
two seconds of resolution. Title: 2048 bytes; description/enum: 8192 bytes each.
Document validation: 4 MiB, 100000 evaluation steps, depth 64, 100 violations,
10000 field projections, 4 MiB of projected field text, two seconds of evaluation.
Bounds are deliberately lower than the ordinary viewer limits. Resource limits
never produce a valid verdict. The existing 64 MiB preview and JSON editing
limits are unchanged. Tree explanation text is capped at 4 MiB with a notice;
ordinary tree rows and keys remain available beyond that cap.

A new selection owns a new preview; worker routing checks that selection's
identity. Schema refresh results additionally check generation and document
revision. A result cannot replace a newer schema request or source snapshot.
Reports describe the opened source/schema snapshots; reopen after an external
source change. This first version does not add a schema-file watcher.

## Synthetic walkthrough and acceptance boundary

Open [the fixture directory](fixtures/json-schema-project) in the Reader, then
`jobs/demo.samplejob`. The second schema association must select
`schemas/job.schema.json`, rather than `general.schema.json`. Raw, Formatted and
Tree remain available. Hover `name`, expand `steps` and hover `state`/`attempts`:
these explanations come only from the local schemas. `custom` and `unexplained`
retain their values without invented descriptions. Schema details must name
four violations: `/owner`, `/steps/1/state`, `/steps/1/attempts` and
`/steps/2/state`. `jobs/general.samplejob` uses the first association and is valid.

For array Table, associate a synthetic array document with a schema whose root
is `type: array` and whose `items` declares the row properties. Table cell hover
uses the original source row even after sorting/filtering; column headers show
an example path from the first source row. Nested cells retain their existing
JSON text; Tree exposes the nested paths.

Focused core tests cover precedence, refs, metadata, constraints, ambiguity and
fallback. GPUI tests exercise custom-extension routing, all four views, source
preservation/editing, and stale refresh/selection controls. These are source and
synthetic test evidence, not installed native acceptance. The first containing
beta and native acceptance are recorded by the delivery lane after landing;
this change itself does not claim a beta/build number or install anything.
