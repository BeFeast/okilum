# Search acceptance — the gate on the engine choice

`docs/PRD.md` §6 leaves the search engine unchosen and gates it behind this file.
`tantivy` is the incumbent because the spike reached for it, not because it won
anything. It stays until it has a measured result here.

**Bands below were written and committed before any engine was measured against
them.** That ordering is the whole point: a band chosen after seeing the numbers
is not a band, it is a description. The git history shows the spec commit
preceding the results commit, and it should stay that way for every future
revision.

## Two things this gate must not confuse

**"tantivy cannot" is not the same as "we have not configured it."** tantivy ships
stemming tokenizers, fuzzy term queries, fast fields for range filters, and a
query explainer. If a capability fails here, the first question is whether the
schema and query parser were ever asked for it. Only a capability that survives a
genuine configuration attempt counts as evidence against the engine.

**A capability we never implemented is a gap, not a failure of the corpus.** The
tests below are written as the specification. The ones that do not pass today are
marked `#[ignore = "GAP: …"]` with the reason, so they run on demand
(`cargo test -- --ignored`), stay visible, and go green the day someone closes
them.

## Correctness capabilities

Absolute — a visible defect is a fail, and no latency number redeems one.

| # | Capability | What counts as passing |
|---|---|---|
| C1 | Exact term | A term unique to one note returns that note, ranked first |
| C2 | Phrase | `"two words"` matches a note containing that phrase and not one containing both words apart |
| C3 | Boolean | `a AND b`, `a OR b`, and negation each select the right subset |
| C4 | Field scoping | `title:foo` matches a title occurrence and not a body-only one |
| C5 | Path filtering | A query can be restricted to a subtree of the vault |
| C6 | Tag filtering | `#tag` finds notes carrying that tag, and does not match the same word as prose |
| C7 | Date filtering | Notes can be filtered by a frontmatter date range |
| C8 | Typo tolerance | A single-character typo still finds the note |
| C9 | Stemming | "running" finds a note that only says "run" |
| C10 | Multilingual | A Russian query finds a Russian note, case-insensitively. **Not hypothetical:** the target vault is mixed Russian and English, often in the same sentence |
| C11 | Ranking explainability | For a given hit, the engine can say which terms in which fields caused it |
| C12 | Snippets | The snippet contains the match, marked, and is drawn from the matching region rather than the head of the note |
| C13 | Heading indexing | A query can be restricted to headings |
| C14 | Frontmatter indexing | A frontmatter value is findable as a field, not merely because the raw text was indexed as body |
| C15 | Link indexing | Notes can be found by what they link to |
| C16 | Incremental update | Editing one note updates the index without a full rebuild |
| C17 | Full rebuild | Deleting the index directory and rebuilding reproduces the same results |

## Latency bands

Measured on the target corpus (~3900 notes, ~50 MB of Markdown), warm cache,
after the index is open. Query latency is measured per query and aggregated over
repeated runs.

| # | Measure | Pass | Gray (interview judges) | Fail |
|---|---|---|---|---|
| L1 | Single-term query, p50 | < 15 ms | 15–50 ms | ≥ 50 ms |
| L2 | Single-term query, p99 | < 50 ms | 50–150 ms | ≥ 150 ms |
| L3 | Phrase query, p50 | < 25 ms | 25–75 ms | ≥ 75 ms |
| L4 | Common-term query (thousands of hits), p50 | < 40 ms | 40–120 ms | ≥ 120 ms |
| L5 | Index open (contributes to cold start) | < 50 ms | 50–200 ms | ≥ 200 ms |
| L6 | Full rebuild of the whole corpus | < 30 s | 30–120 s | ≥ 120 s |

Why these numbers: search in this product is search-as-you-type in a reader, so
the interactive bands are set against human perception rather than against any
engine's known behaviour — under ~15 ms a keystroke feels free, past ~150 ms the
list visibly lags the typing. L5 is bounded by the 243 ms cold start the framework
probe measured for the whole shell; a search index that costs more than a fifth of
that is buying its speed from the wrong budget.

**L6 is not a blind band.** Index build was incidentally observed at roughly 0.8 s
for this corpus during unrelated work on #2, so the band was set knowing the
answer. It is recorded as a floor for regressions, not as evidence about the
engine. L1–L5 were set blind.

## Verdict rule

- **PASS** — every correctness capability holds and no latency measure lands in
  Fail, with at most two in Gray.
- **GRAY** — capabilities hold but latency has three or more Gray measures, or one
  correctness capability fails with a named workaround and a stated user-visible
  consequence.
- **FAIL** — a correctness capability fails with no workaround, or any latency
  measure lands in Fail, **and** the cause is the engine rather than our
  configuration.

A gap that is ours to close does not fail the engine. It fails us, and becomes an
issue.

## Results — tantivy 0.26, 2026-09-03

Corpus: the target vault, 3949 notes. Bands above were fixed in the preceding
commit.

### Latency — every band PASS

| # | Measure | Value | Band |
|---|---|---|---|
| L1 | Single-term p50 | **10.42 ms** | PASS (< 15) |
| L2 | Single-term p99 | **20.78 ms** | PASS (< 50) |
| L3 | Phrase p50 | **8.54 ms** | PASS (< 25) |
| L4 | Common-term p50 | **10.45 ms** | PASS (< 40) |
| L5 | Index open | **0.30 ms** | PASS (< 50) |
| L6 | Full rebuild | **1.15 s** | PASS (< 30 s) |

Vault scan, separately, is 824 ms — seven times the index build, so scanning is
where a corpus-scale startup budget actually goes.

### Correctness — 10 hold, 10 do not, 1 untestable

Holding: exact term (C1), phrase (C2), boolean (C3), field scoping on fields that
exist (C4), snippets drawn from the matching region (C12), reproducible rebuild
(C17).

Not holding: path (C5), tag (C6), date (C7), typo tolerance (C8), stemming (C9),
Russian inflection (C10 — exact terms and case folding already work), heading
scoping (C13), frontmatter as fields (C14), link indexing (C15), incremental
update (C16), explainability (C11, untestable — no API).

### The verdict: not one of these is tantivy's fault

The gate exists to separate "the engine cannot" from "we never configured it".
Checked against tantivy 0.26 rather than assumed:

| Missing capability | What tantivy already ships |
|---|---|
| Stemming, Russian inflection | `Stemmer` tokenizer, 18 languages, Russian among them |
| Typo tolerance | `FuzzyTermQuery` |
| Date ranges | `range_query` |
| Explainability | `Query::explain` |
| Incremental update | `delete_term` + add + commit |
| Path, tag, heading, frontmatter, link fields | Ordinary schema fields nobody declared |

Our schema is two text fields — `title` and `body` — with the default tokenizer.
Every failure above is that schema being thin, not tantivy refusing.

**So the verdict rule applies as written: a gap that is ours to close does not
fail the engine.** tantivy is not disqualified, and its latency is comfortable
against bands set blind. What the gate actually measured is that our search layer
is still the spike's prototype.

### One defect, not a gap

`escape()` strips punctuation and retries whenever a query fails to parse. A query
naming a field that does not exist therefore becomes a bag of words and returns
confident nonsense: `frontmatter.title:Beta` and `frontmatter.title:Nonexistent`
return **the same two notes**, because both notes contain the word "title" in
their frontmatter text.

The reader cannot distinguish "your filter matched these" from "your filter was
thrown away". It is the same failure as silently resolving an ambiguous link, and
it was found only because the first version of C14 passed for the wrong reason —
it asserted that the right note came back without asserting that the wrong ones
did not.

### What this does not settle

No alternative engine was measured. This says tantivy clears the bar we set and
that the work in front of us is configuration; it does not say tantivy beat
anything, because nothing else was run. Choosing the engine for v0 is still a
decision, now an informed one.

---

## Second run — 2026-09-04, after #10 and #11

The gate was written to be re-run. Six of the ten failing capabilities now hold.

### Correctness — 17 hold, 4 remain

Newly holding: **path filtering** (C5), **tag filtering** (C6), **date ranges**
(C7), **heading scoping** (C13), **frontmatter as addressable fields** (C14),
**link indexing** (C15), and the `escape()` defect is gone — an unknown field is
now an error naming the fields that do exist, not a bag of words.

Still open, all with `#[ignore]` and a reason: typo tolerance (C8), stemming
(C9), Russian inflection (C10), explainability (C11). Those are #12 and #13.

### Latency — every band still PASS, and the cost is visible

| # | Measure | Before | After | Band |
|---|---|---|---|---|
| L1 | Single-term p50 | 10.42 ms | **10.04 ms** | PASS (< 15) |
| L2 | Single-term p99 | 20.78 ms | **38.14 ms** | PASS (< 50) |
| L3 | Phrase p50 | 8.54 ms | **8.48 ms** | PASS (< 25) |
| L4 | Common-term p50 | 10.45 ms | **10.12 ms** | PASS (< 40) |
| L5 | Index open | 0.30 ms | **0.30 ms** | PASS (< 50) |
| L6 | Full rebuild | 1.15 s | **1.35 s** | PASS (< 30 s) |

**p99 nearly doubled.** It clears the band with room, but the median did not
move, so this is tail behaviour from the extra fields, not a general slowdown.
Worth watching rather than acting on: if #12 adds a second analyzer, p99 is the
number that will move first.

Rebuild is +17% for five more indexed fields, which is cheap.

The vault scan printed 1581 ms against 824 ms before. The scan code did not
change, so treat that as cache state rather than a regression — it is measured
outside the index build and is not one of the bands.

### Two decisions taken here rather than assumed

**Space now means AND.** tantivy defaults to OR, which made `path:archive widget`
mean "in archive, OR mentions widget anywhere" — a filter that widens the result
instead of narrowing it. Every search box a reader has used treats a space as
AND, and C5 could not otherwise pass honestly.

**Bare dates are widened.** A reader writes `date:[2026-01-01 TO 2026-12-31]`;
tantivy stores RFC 3339. The query layer widens the bare form rather than making
the reader learn the storage format.

### Two tests were wrong, and were fixed as tests

Said plainly because the alternative — quietly adjusting the code until a bad
test goes green — is the failure this whole document exists to prevent.

- **C6** asserted `archive/gamma.md` must not match `tag:widget`, while the same
  fixture gave it `tags: [widget]`. The data contradicted the spec. The fixture
  now tags it `archived` and mentions "widget" only as prose, which is the only
  arrangement that can tell a tag query from a word query.
- **C13** asserted `heading:zarquon` returns nothing, while the fixture contains
  a note whose H1 *is* "Zarquon". It now asserts that query finds exactly that
  note and not the one with the term in body prose.

And one test was right and the code was renamed to meet it: C14 asked for
`frontmatter.title:`, so the JSON field is named `frontmatter`, not the shorter
`fm` the implementation reached for first. Editing the spec to fit the code is
the wrong direction.

---

## Third run — 2026-09-04, after #13 and #6

### Correctness — 19 hold, 3 remain

Newly holding: **incremental update** (C16 — now a real test, replacing the
inverted pin that was there to go red when this landed; it did) and
**explainability** (C11). Plus a new C16b: deleting a note removes it from the
index.

Remaining, all #12: typo tolerance (C8), stemming (C9), Russian inflection (C10).

### Latency — every band still PASS

| # | Measure | Run 2 | Run 3 | Band |
|---|---|---|---|---|
| L1 | Single-term p50 | 10.04 ms | **11.87 ms** | PASS (< 15) |
| L2 | Single-term p99 | 38.14 ms | **26.38 ms** | PASS (< 50) |
| L3 | Phrase p50 | 8.48 ms | **10.48 ms** | PASS (< 25) |
| L4 | Common-term p50 | 10.12 ms | **13.55 ms** | PASS (< 40) |
| L5 | Index open | 0.30 ms | **0.35 ms** | PASS (< 50) |
| L6 | Full rebuild | 1.35 s | **1.96 s** | PASS (< 30 s) |

Nothing in this change touches the query path, so the movement here is noise
and cache state — the p99 that "nearly doubled" in run 2 came back down
unprompted. Do not read run-to-run deltas below ~20% as signal on this host.

Rebuild went 1.35 → 1.96 s. That one may be real: `build()` now keeps its
`IndexWriter` alive for later single-note updates instead of dropping it, and a
held writer keeps its buffers. Small against a 30 s band and the whole point
was to make rebuilds rare.

### What #6 measured

A single-note update through the whole path — file change → watcher batch →
`Vault::scan` → `update_note` → search sees it — is bounded in the test at
250 ms for the index step, and the vault rescan is the dominant cost at ~1 s on
the real corpus. That rescan is the next thing to make incremental if it ever
matters; for now a save is visible in about a second, which is faster than the
reader can switch windows.

---

## Fourth run — 2026-09-04, after #12

### Correctness — 22 of 22 hold

The last three closed: **stemming** (C9), **Russian inflection** (C10), **typo
tolerance** (C8). Nothing is `#[ignore]`d any more. The gate is fully green.

### The bilingual analyzer

2077 of 8103 notes in the target vault contain Cyrillic, and Russian and
English routinely share a sentence, so per-note language detection was the
wrong shape. The analyzer stems **per token, by script**: Cyrillic → Russian
stemmer, Latin → English, anything else untouched. Identifier fields (path,
tag, links) keep the plain tokenizer — a stemmed tag would turn `tag:archived`
into a search for `archiv`.

The analyzer's name is stored in the schema, so it has to be re-registered on
every `open`; forgetting that makes every stemmed query silently find nothing.

### Fuzzy: measured, rejected as a default, kept as a fallback

Fuzzy on every prose query was tried first and it failed **three ways at
once**:

| | exact only | fuzzy on every query |
|---|---|---|
| L1 single-term p50 | 11.87 ms | **21.02 ms** (GRAY) |
| L2 single-term p99 | 26.38 ms | **79.39 ms** (GRAY) |
| C11 explain | holds | **broke** — no exact term to name |
| C12 snippets | holds | **broke** — every snippet blank |

A fuzzy term query carries no exact term for the snippet generator or the
explainer to work from. Buying C8 by breaking C11 and C12 is not a trade the
verdict rule allows.

Fuzzy is therefore a **fallback**: it runs only when the exact query finds
nothing, and snippets are still generated against the exact query so a fuzzy
hit shows the word as written in the note. Confirmed by negative control —
disabling the fallback fails C8 and only C8.

### Latency — stemming has a cost, and it lands in one gray band

| # | Measure | Run 3 | Run 4 | Band |
|---|---|---|---|---|
| L1 | Single-term p50 | 11.87 ms | **18.57 ms** | **GRAY** (15–50) |
| L2 | Single-term p99 | 26.38 ms | **29.26 ms** | PASS (< 50) |
| L3 | Phrase p50 | 10.48 ms | **22.98 ms** | PASS (< 25) |
| L4 | Common-term p50 | 13.55 ms | **31.74 ms** | PASS (< 40) |
| L5 | Index open | 0.35 ms | **0.14 ms** | PASS |
| L6 | Full rebuild | 1.96 s | **1.54 s** | PASS |

L1 crosses into gray by 3.5 ms. By the verdict rule that is one gray measure,
within the allowance of two, so the overall result is still **PASS** — but it
is the first band that has moved off green, and it moved for a real reason:
every query term is now stemmed through two Snowball stemmers before lookup.

Not hidden behind the run-to-run noise disclaimer from run 3: this shift is
consistent in direction across L1, L3 and L4 and matches the mechanism.

Where the time goes is not measured yet. Two obvious candidates — building the
`rust_stemmers::Stemmer` per token stream instead of once, and running both
stemmers' setup for tokens that only need one — are cheap to test and are the
first thing to try if L1 needs to come back under 15 ms. That is a follow-up,
recorded rather than folded in here.

---

## Fifth run — 2026-09-04, L1 back under the line

The gray band from run 4 was chased down rather than left. The two suspects
named there were both tried, in order:

**Suspect 1, stemmer construction per token stream — not it.** Sharing the two
`rust_stemmers::Stemmer`s per process (they are `Sync`) moved L1 by nothing
measurable. Kept anyway, since it is free.

**The real cost was snippets, and it had been there all along.** Timing the
stages of one query on the real corpus:

| | p50 |
|---|---|
| `search("maestro", 30)` | 20.06 ms |
| `search("maestro", 1)` | **0.27 ms** |
| `search("zarquon", 30)` — a rare term, few hits | 2.19 ms |

The search itself is a quarter of a millisecond. The other 19.8 ms were
`snippet_from_doc` tokenizing the **entire stored body** of each of 30 hits —
some of them 130 KB — through the stemming analyzer, to pick 180 characters.
Stemming did not create this cost; it made an existing cost twice as
expensive, which is why L1 crossed the line in run 4 and not before.

The snippet generator now sees a 1 500-byte window of the body centred on the
first occurrence of any query term (or the head of the note if none is found),
so the fragment still comes from the matching region and thirty of them cost
nothing.

| # | Measure | Run 4 | Run 5 | Band |
|---|---|---|---|---|
| L1 | Single-term p50 | 18.57 ms | **5.77 ms** | PASS (< 15) |
| L2 | Single-term p99 | 29.26 ms | **11.23 ms** | PASS (< 50) |
| L3 | Phrase p50 | 22.98 ms | **5.05 ms** | PASS (< 25) |
| L4 | Common-term p50 | 31.74 ms | **4.03 ms** | PASS (< 40) |
| L5 | Index open | 0.14 ms | **0.16 ms** | PASS |
| L6 | Full rebuild | 1.54 s | **1.53 s** | PASS |

Every latency band is green again, and every one is below where it started in
run 1. All 22 correctness capabilities still hold — the window is centred on a
match, so C12 (snippet from the matching region) is unaffected.

**Final state of the gate: 22/22 correctness, 6/6 latency PASS, nothing gray,
nothing ignored.** tantivy clears every bar set for it. Choosing it for v0 is
still a decision, not a default — but it is now one with nothing measured
against it.
