# Markdown conformance: CommonMark and GFM spec tests (#650)

> **Historical document.** Written before Tessera was renamed Okilum (2026-10-09). Names, paths and links are kept as they were then.

Status: harness landed, one bug fixed, deviations documented. No UI changes.

## TL;DR

- Every example of the **CommonMark 0.31.2** spec (652) and the **GFM 0.29**
  spec (672) now runs in `tessera-core` against both Markdown parses Tessera
  has, with the input Tessera actually gives each of them.
- **Comrak** (HTML reader mode and all core analysis) passes 644/652
  CommonMark and 654/672 GFM examples. **markdown-rs** (the default reader)
  passes 644/652 and 653/672.
- **Every remaining failure is explained.** All CommonMark failures are
  intentional Tessera choices: front matter, wikilinks, embeds, bare-URL
  autolinks. The GFM-only failures add 9 examples where the 2019 GFM spec
  predates a CommonMark change both parsers follow, one raw-HTML filter
  Tessera leaves off, and one upstream markdown-rs choice (no `ftp://`
  autolinks).
- **One real bug found and fixed** (CommonMark 216/510, GFM 185/518). Comrak
  reports wrong source ranges for some links. The reader spliced its link
  rewrite into those ranges and corrupted the note as displayed. Before the
  fix the reader passed 642/652 and 651/672.

## What is measured

Test: `crates/tessera-core/tests/markdown_conformance.rs`. Spec data:
`crates/tessera-core/tests/fixtures/markdown-spec/`. The notices and
provenance of that data (CC BY-SA 4.0) are in `NOTICE.md` in the same
directory.

| Engine | Input given to the parser | Parser and options | Used by |
|---|---|---|---|
| `comrak` | `render::preprocess` (Obsidian embeds) | Comrak 0.47, `render::comrak_options()` | HTML reader mode; links, tasks, outline IR, export, source classification |
| `reader` | `render::reader_document_from_source` (front matter, embeds, link and image rewriting, `==highlights==`) | markdown-rs 1.0.0, `ParseOptions::gfm()` as in gpui-component's Markdown view | Default reader |

- The output is compared after HTML normalisation in the style of cmark's
  `normalize.py`. Attribute order, `<br />` against `<br>`, and whitespace
  around block tags do not count as differences.
- `normalization_ignores_form_but_not_content` is the positive control: it
  checks that real differences (text, whitespace inside `<pre>`, heading
  level, `href`) still fail the comparison.
- `asset_uri_remapping_keeps_image_structure_checks` accepts the Reader
  asset identity mapping while rejecting altered alt text, titles, nesting
  and arbitrary image URL changes. This accounts for the shared adjacent
  image-identity change already on main.
- `harness_detects_a_known_deviation` checks that a known deviation is
  detected and named in both engines.
- GFM examples run with the extensions their fence names, as cmark-gfm's own
  test runner does. The two task list examples are tagged `disabled`
  upstream only because cmark cannot normalise checkbox attributes. Here they
  run with the task list extension.

Not covered:

- How gpui-component draws the markdown-rs tree. The harness compiles that
  tree to HTML, so it measures the parse, not the GPUI rendering.
- Comrak's AST link rewriting and syntax highlighting. Both run after the
  parse and change URLs and code markup on purpose.
- The reader rewrites link destinations in the *source*, before the parse.
  It turns them into `tessera://` URLs, by design. An example whose output
  differs only in such `href`/`src` values (including opaque `tessera-asset://`
  identities) counts as passing and is listed
  in the "resolved URLs" column. The parse around the link still has to
  match.

### Failure causes

The harness classifies every failure instead of only counting it:

- `deviation:<knob>`: the parser passes the example with the spec's own
  options. Putting that one Tessera choice back to the spec value also makes
  it pass.
- `deviation:combined`: the parser passes with the spec's options, but no
  single choice explains the failure.
- `parser`: the parser fails with the spec's own options as well.
- `spec-drift`: a GFM 0.29 example whose CommonMark core changed by 0.31.2.
  The parser passes the 0.31.2 example with the same input.

`known-failures.txt` pins every failing example and its cause. A new failure,
a new pass and a changed cause all fail the test, so the list changes only on
purpose.

To regenerate the report and the list:

```sh
TESSERA_CONFORMANCE_REPORT=1 cargo test -p tessera-core \
  --test markdown_conformance -- --nocapture
```

Add `TESSERA_CONFORMANCE_DEBUG=1` to print the input, expected output and
actual output of every failure.

## Results per section

The tables show the state after the fix in this change. A bare count means
every example in the section passes. "Resolved URLs" counts reader passes
whose link URLs were rewritten to `tessera://` (see above).

#### commonmark

| Section | Examples | Comrak | Reader | Reader, resolved URLs |
|---|---:|---:|---:|---:|
| Tabs | 11 | 11 | 11 |  |
| Backslash escapes | 13 | 13 | 13 | 2 |
| Entity and numeric character references | 17 | 17 | 17 | 2 |
| Precedence | 1 | 1 | 1 |  |
| Thematic breaks | 19 | 19 | 19 |  |
| ATX headings | 18 | 18 | 18 |  |
| Setext headings | 27 | 26 (96%) | 25 (93%) |  |
| Indented code blocks | 12 | 12 | 12 |  |
| Fenced code blocks | 29 | 29 | 29 |  |
| HTML blocks | 44 | 44 | 44 |  |
| Link reference definitions | 27 | 27 | 27 | 16 |
| Paragraphs | 8 | 8 | 8 |  |
| Blank lines | 1 | 1 | 1 |  |
| Block quotes | 25 | 25 | 25 |  |
| List items | 48 | 48 | 48 |  |
| Lists | 26 | 26 | 26 |  |
| Inlines | 1 | 1 | 1 |  |
| Code spans | 22 | 22 | 22 |  |
| Emphasis and strong emphasis | 132 | 132 | 132 | 6 |
| Links | 90 | 89 (99%) | 89 (99%) | 66 |
| Images | 22 | 21 (95%) | 21 (95%) | 20 |
| Autolinks | 19 | 14 (74%) | 15 (79%) | 4 |
| Raw HTML | 20 | 20 | 20 |  |
| Hard line breaks | 15 | 15 | 15 |  |
| Soft line breaks | 2 | 2 | 2 |  |
| Textual content | 3 | 3 | 3 |  |
| **Total** | **652** | **644 (98.8%)** | **644 (98.8%)** | 116 |

#### gfm

| Section | Examples | Comrak | Reader | Reader, resolved URLs |
|---|---:|---:|---:|---:|
| Tabs | 11 | 11 | 11 |  |
| Precedence | 1 | 1 | 1 |  |
| Thematic breaks | 19 | 19 | 19 |  |
| ATX headings | 18 | 18 | 18 |  |
| Setext headings | 27 | 26 (96%) | 25 (93%) |  |
| Indented code blocks | 12 | 12 | 12 |  |
| Fenced code blocks | 29 | 29 | 29 |  |
| HTML blocks | 43 | 43 | 43 |  |
| Link reference definitions | 28 | 28 | 28 | 16 |
| Paragraphs | 8 | 8 | 8 |  |
| Blank lines | 1 | 1 | 1 |  |
| Tables (extension) | 8 | 8 | 8 |  |
| Block quotes | 25 | 25 | 25 |  |
| List items | 48 | 48 | 48 |  |
| Task list items (extension) | 2 | 2 | 2 |  |
| Lists | 26 | 26 | 26 |  |
| Inlines | 1 | 1 | 1 |  |
| Backslash escapes | 13 | 13 | 13 | 2 |
| Entity and numeric character references | 17 | 17 | 17 | 2 |
| Code spans | 22 | 22 | 22 |  |
| Emphasis and strong emphasis | 131 | 122 (93%) | 122 (93%) | 6 |
| Strikethrough (extension) | 2 | 2 | 2 |  |
| Links | 87 | 86 (99%) | 86 (99%) | 64 |
| Images | 22 | 21 (95%) | 21 (95%) | 20 |
| Autolinks | 19 | 14 (74%) | 15 (79%) | 4 |
| Autolinks (extension) | 11 | 11 | 10 (91%) |  |
| Raw HTML | 20 | 20 | 20 |  |
| Disallowed Raw HTML (extension) | 1 | 0 (0%) | 0 (0%) |  |
| Hard line breaks | 15 | 15 | 15 |  |
| Soft line breaks | 2 | 2 | 2 |  |
| Textual content | 3 | 3 | 3 |  |
| **Total** | **672** | **654 (97.3%)** | **653 (97.2%)** | 114 |

## Intentional deviations

Each deviation below is pinned, with its cause, in `known-failures.txt`.

| Choice | Examples | Engines | Why |
|---|---|---|---|
| A leading `---` block is YAML front matter | CM 96, 98; GFM 66, 68 | comrak (96 only), reader | Obsidian vaults keep note properties there. A note that starts with a thematic break and a setext heading is far rarer than one that starts with properties. |
| `[[...]]` is a wikilink | CM 559; GFM 567 | both | Obsidian link syntax, the core of note identity. |
| `![[...]]` is an embed | CM 590; GFM 598 | both | Obsidian embed syntax. |
| GFM autolink literals are on in every note | CM 602, 606, 608, 611, 612; GFM 610, 614, 616, 619, 620 | both (the reader passes CM 606 / GFM 614) | Bare URLs and email addresses are links on GitHub and in Obsidian. The spec runs these CommonMark examples without the extension. |
| Link and image destinations are resolved into internal URLs | 116 CommonMark and 114 GFM examples (passing, listed separately) | reader | Navigation is resolved against the vault (`document_links`); missing adjacent assets use `tessera-asset://unavailable`. The parse around the URL is unchanged. |
| GFM "disallowed raw HTML" (tagfilter) is off | GFM 652 | both | See the open question below. |

### Not Tessera's choice

- **spec-drift, GFM 398, 426, 434–436, 473–475, 477** (both engines). These
  are GFM 0.29 (2019) emphasis examples that a later CommonMark release
  changed. Comrak and markdown-rs both follow CommonMark 0.31.2 and pass the
  current versions of the same inputs in the CommonMark suite. GFM 0.29 is
  still the latest published GFM spec.
- **parser, GFM 628** (reader only). markdown-rs does not turn `ftp://`
  literals into links; Comrak does. This is upstream behaviour of a
  dependency of the vendored gpui-component and is left as is.

## Fixed: link rewrite spliced into a wrong range

Comrak 0.47 reports wrong inline source positions in two cases. Comrak 0.56
(the latest release on 2026-10-06) does the same, checked with a standalone
probe.

1. If a link's title or closing parenthesis is on a later line, the link ends
   at its destination. Later inlines in the same paragraph lose a line.
2. If a paragraph opens with a link reference definition, its inlines are
   placed as if the definition were not there.

`document_links::parse` turned those positions into byte ranges, and the
reader's `rewrite_source_links` spliced the resolved link into them. Some
examples:

- `[r]: /u` followed by `para [a](x.md)` became
  `[r]: [x.md](tessera://…)a](x.md)` in the reader.
- `[link](   /uri` followed by `  "title"  )` lost its title and showed
  `"title"  )` as text.

The fix is in `document_links::checked_link_end`:

- A range is trusted only when it has the shape of its link.
- A short inline link is extended to the closing parenthesis at which its
  text, parsed on its own, is the same link.
- Any other link keeps its target, so backlinks and link state still count
  it. It is marked `exact_range: false`, and the source rewrite leaves it as
  authored. Ambiguity is surfaced, not guessed.

Regression test: `misreported_comrak_ranges_are_repaired_or_skipped_never_spliced`
in `tests/document_links.rs`. It fails without the fix.

Residual effects:

- A link that follows a multi-line link in the same paragraph, or that sits
  in a paragraph that opens with a definition, stays unresolved in the reader
  (shown as written) instead of being corrupted.
- `tessera-shell` (`prepared_links`, `reader_editor`) still uses
  `ParsedLink::range` for markers and click hit-testing without checking
  `exact_range`. This was out of scope here (no UI changes) and is worth a
  follow-up.
- The note-move rewriter (`link_rewrite`) already checks the authored text
  of each range, so a misplaced link is not rewritten on disk.

## Open question: tagfilter

GFM's tagfilter escapes `<title>`, `<textarea>`, `<style>`, `<xmp>`,
`<iframe>`, `<noembed>`, `<noframes>`, `<script>` and `<plaintext>`. In the HTML
reader mode, html5ever would otherwise read everything after a stray
`<textarea>` or `<plaintext>` as that element's text, so the rest of the note
disappears. Turning it on in `comrak_options()` was measured:

- GFM: +1 (652).
- CommonMark: −6 (HTML blocks 170–173, 176, 178).
- Notes with `<style>` or `<script>` blocks would then show the code as
  literal text. Today gpui-component's HTML view drops them.

The default reader is unaffected: it never compiles the markdown-rs tree to
HTML. Because the trade-off changes what some notes show, the option is left
off and the decision goes to the product owner.

## How the spec data was produced

- `commonmark-0.31.2.json`: the CommonMark project's published `spec.json`,
  unmodified.
- `gfm-0.29.json`: `python3 scripts/markdown-spec-json.py spec.txt` on
  cmark-gfm's `test/spec.txt` at `27d942c8b0a6`. The script mirrors cmark-gfm's
  `spec_tests.py --dump-tests` and keeps `disabled` examples with their tag
  instead of dropping them.
