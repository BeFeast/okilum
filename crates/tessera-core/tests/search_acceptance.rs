//! The acceptance gate on the search engine — see `docs/SEARCH-ACCEPTANCE.md`.
//!
//! These are the specification, not a description of what the incumbent happens
//! to do. Capabilities that do not hold today are marked `#[ignore = "GAP: …"]`
//! so they stay visible, run on demand (`cargo test -- --ignored`), and turn
//! green the day someone closes them.
//!
//! The bands in the doc were committed before anything was measured against
//! them. Keep it that way when revising: a band chosen after seeing the numbers
//! describes an engine instead of judging one.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use tessera_core::{Searcher, Vault};

struct Fixture {
    root: PathBuf,
    index: PathBuf,
    searcher: Searcher,
    _vault: Vault,
}

/// Cargo runs these in parallel threads of ONE process, so a fixture directory
/// keyed on the process id is shared by every test — they then race, wiping each
/// other's vault mid-build. Hand out a unique directory per fixture instead.
static FIXTURE_SEQ: AtomicUsize = AtomicUsize::new(0);

impl Fixture {
    fn new(name: &str, files: &[(&str, &str)]) -> Fixture {
        let seq = FIXTURE_SEQ.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!(
            "tessera-search-{name}-{}-{seq}",
            std::process::id()
        ));
        let root = base.join("vault");
        let index = base.join("index");
        let _ = fs::remove_dir_all(&base);
        for (rel, body) in files {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, body).unwrap();
        }
        let vault = Vault::scan(Path::new(&root)).unwrap();
        let searcher = Searcher::build(&vault, &index).unwrap();
        Fixture {
            root,
            index,
            searcher,
            _vault: vault,
        }
    }

    fn paths(&self, query: &str) -> Vec<String> {
        self.searcher
            .search(query, 30)
            .unwrap_or_default()
            .into_iter()
            .map(|h| h.path)
            .collect()
    }
}

/// A small corpus with deliberately separated vocabulary, so a hit is
/// attributable to exactly one note.
fn corpus() -> Fixture {
    Fixture::new(
        "corpus",
        &[
            (
                "notes/alpha.md",
                "---\ntitle: Alpha\ntags: [widget]\ndate: 2026-03-01\n---\n\n# Alpha\n\n\
                 The zarquon device is unique to this note.\n\n\
                 ## Installation notes\n\nA quick brown fox runs daily.\n\n\
                 See [[notes/beta]] for the sequel.\n",
            ),
            (
                "notes/beta.md",
                "---\ntitle: Beta\ntags: [gadget]\ndate: 2026-07-15\n---\n\n# Beta\n\n\
                 This note mentions brown separately and fox separately, never adjacent.\n\n\
                 ## Deployment\n\nThe run completed.\n",
            ),
            (
                // Deliberately tagged something ELSE while mentioning "widget"
                // in prose: that is the only way C6 can tell a tag query from a
                // word query. The first version of this fixture tagged it
                // `widget` and then asserted it must not match `tag:widget` —
                // the data contradicted the spec, and the test was right to fail.
                "archive/gamma.md",
                "---\ntitle: Gamma\ntags: [archived]\ndate: 2025-01-09\n---\n\n# Gamma\n\n\
                 Archived material. Mentions widget as ordinary prose, not as a tag.\n",
            ),
            (
                "notes/russian.md",
                "---\ntitle: Настройка\n---\n\n# Настройка сервера\n\n\
                 Конфигурация выполняется через systemd. Перезапуск обязателен.\n",
            ),
            (
                "notes/zarquon-title.md",
                "# Zarquon\n\nBody text without the special term.\n",
            ),
        ],
    )
}

// ---------------------------------------------------------------- correctness

#[test]
fn c1_exact_term_finds_and_ranks_its_note() {
    let f = corpus();
    let hits = f.paths("zarquon");
    assert!(!hits.is_empty(), "a unique term must find something");
    assert!(
        hits.contains(&"notes/alpha.md".to_string()),
        "the note containing the term must be in the results: {hits:?}"
    );
}

#[test]
fn c2_phrase_matches_adjacency_not_co_occurrence() {
    let f = corpus();
    let hits = f.paths("\"brown fox\"");
    assert_eq!(
        hits,
        vec!["notes/alpha.md"],
        "beta.md has both words but never adjacent, so a phrase query must exclude it"
    );
}

#[test]
fn c3_boolean_operators_select_the_right_subset() {
    let f = corpus();
    assert!(
        f.paths("zarquon AND fox")
            .contains(&"notes/alpha.md".to_string()),
        "AND over two terms in the same note"
    );
    assert!(
        f.paths("zarquon AND deployment").is_empty(),
        "AND across notes must match nothing"
    );
    let or = f.paths("zarquon OR deployment");
    assert!(
        or.contains(&"notes/alpha.md".to_string()) && or.contains(&"notes/beta.md".to_string()),
        "OR must union both: {or:?}"
    );
    let neg = f.paths("run -deployment");
    assert!(
        !neg.contains(&"notes/beta.md".to_string()),
        "negation must exclude beta.md: {neg:?}"
    );
}

#[test]
fn c4_field_scoping_distinguishes_title_from_body() {
    let f = corpus();
    let in_title = f.paths("title:zarquon");
    assert!(
        in_title.contains(&"notes/zarquon-title.md".to_string()),
        "the note whose TITLE is Zarquon must match: {in_title:?}"
    );
    assert!(
        !in_title.contains(&"notes/alpha.md".to_string()),
        "alpha.md has the term in its body only, so a title-scoped query must not match it"
    );
}

#[test]
fn c5_a_query_can_be_restricted_to_a_subtree() {
    let f = corpus();
    let hits = f.paths("path:archive widget");
    assert_eq!(
        hits,
        vec!["archive/gamma.md"],
        "restricting to archive/ must exclude the notes/ hits"
    );
}

#[test]
fn c6_tag_search_is_not_prose_search() {
    let f = corpus();
    let tagged = f.paths("tag:widget");
    assert!(
        tagged.contains(&"notes/alpha.md".to_string()),
        "alpha.md carries the widget tag"
    );
    assert!(
        !tagged.contains(&"archive/gamma.md".to_string()),
        "gamma.md only mentions the word as prose and must not match a tag query"
    );
}

#[test]
fn c7_notes_can_be_filtered_by_frontmatter_date() {
    let f = corpus();
    // Bare dates on purpose: this is what a reader types. The query layer
    // widens them to the RFC 3339 form tantivy stores.
    let hits = f.paths("date:[2026-01-01 TO 2026-12-31]");
    assert!(
        hits.contains(&"notes/alpha.md".to_string()) && hits.contains(&"notes/beta.md".to_string()),
        "both 2026 notes must be in range: {hits:?}"
    );
    assert!(
        !hits.contains(&"archive/gamma.md".to_string()),
        "the 2025 note must be out of range"
    );
}

#[test]
fn c8_a_single_character_typo_still_finds_the_note() {
    let f = corpus();
    let hits = f.paths("zarqon");
    assert!(
        hits.contains(&"notes/alpha.md".to_string()),
        "one missing character must not lose the note: {hits:?}"
    );
}

#[test]
fn c9_stemming_relates_inflections() {
    let f = corpus();
    // beta.md says "The run completed"; alpha.md says "runs daily".
    let hits = f.paths("running");
    assert!(
        !hits.is_empty(),
        "an inflected query must reach its stem: {hits:?}"
    );
}

#[test]
fn c10_russian_is_searchable_and_case_insensitive() {
    let f = corpus();
    assert!(
        f.paths("конфигурация")
            .contains(&"notes/russian.md".to_string()),
        "a Russian term must find its note"
    );
    assert!(
        f.paths("КОНФИГУРАЦИЯ")
            .contains(&"notes/russian.md".to_string()),
        "Russian matching must be case-insensitive"
    );
    assert!(
        f.paths("перезапуска")
            .contains(&"notes/russian.md".to_string()),
        "an inflected Russian form must reach the note that says перезапуск"
    );
}

#[test]
fn c11_a_hit_can_explain_itself() {
    let f = corpus();
    let explanation = f
        .searcher
        .explain("zarquon", "notes/alpha.md")
        .expect("explain must not error")
        .expect("alpha.md is a hit for zarquon, so there must be an explanation");
    assert!(
        explanation.to_lowercase().contains("zarquon"),
        "an explanation must name the term that matched: {explanation}"
    );
    // And a non-hit explains as "nothing", not as an error.
    let none = f
        .searcher
        .explain("zarquon", "notes/beta.md")
        .expect("explain must not error");
    assert!(none.is_none(), "beta.md does not contain zarquon: {none:?}");
}

#[test]
fn c12_snippets_come_from_the_matching_region_and_mark_the_match() {
    let f = corpus();
    let hits = f.searcher.search("zarquon", 5).unwrap();
    let hit = hits
        .iter()
        .find(|h| h.path == "notes/alpha.md")
        .expect("alpha.md must be a hit");
    assert!(
        hit.snippet_html.contains("<b>"),
        "the match must be marked: {}",
        hit.snippet_html
    );
    assert!(
        hit.snippet_html.to_lowercase().contains("zarquon"),
        "the snippet must contain the match, not the head of the note: {}",
        hit.snippet_html
    );
}

#[test]
fn c13_a_query_can_be_restricted_to_headings() {
    let f = corpus();
    let hits = f.paths("heading:installation");
    assert_eq!(
        hits,
        vec!["notes/alpha.md"],
        "\"Installation notes\" is a heading in alpha.md and nowhere else"
    );
    // `notes/zarquon-title.md` has "# Zarquon" as its H1, so heading:zarquon
    // finds exactly that one and NOT alpha.md, where the term is body prose.
    // The first version asserted the result was empty, which the fixture itself
    // contradicted.
    assert_eq!(
        f.paths("heading:zarquon"),
        vec!["notes/zarquon-title.md"],
        "a heading query must find the note whose heading it is, and only that one"
    );
}

#[test]
fn a_heading_after_a_fence_containing_a_tilde_line_is_indexed() {
    // #24. The facet extractor once flipped its fence state on any ``` or ~~~
    // line, so a `~~~` inside a backtick fence "closed" it: the code line below
    // was indexed as a heading and the real heading after the fence was lost.
    let f = Fixture::new(
        "heading-after-tilde-in-fence",
        &[
            (
                "notes/fenced.md",
                "# Fenced\n\n```\n~~~\n# fakeheading inside code\n```\n\n## Realheading after\n",
            ),
            (
                "notes/other.md",
                "# Other\n\nMentions realheading and fakeheading in prose.\n",
            ),
        ],
    );
    assert_eq!(
        f.paths("heading:realheading"),
        vec!["notes/fenced.md"],
        "the heading after the fence must be indexed as a heading"
    );
    assert!(
        f.paths("heading:fakeheading").is_empty(),
        "a heading marker inside the fence must not be indexed as a heading"
    );
}

#[test]
fn c14_frontmatter_is_indexed_as_fields() {
    let f = corpus();
    // The first version of this test only checked that the right note came back,
    // and it PASSED — for the wrong reason. There is no frontmatter field, so the
    // query failed to parse, fell through the escape() fallback, and became a bag
    // of words that matched because the raw frontmatter text is indexed as body.
    //
    // A field query is only real if it EXCLUDES. Ask for a value no note carries:
    // a genuine field lookup returns nothing, a bag of words returns everything
    // that merely contains the word "title".
    let nonsense = f.paths("frontmatter.title:zzzz-no-note-has-this");
    assert!(
        nonsense.is_empty(),
        "a frontmatter field query for a value nobody has must return nothing, \
         not every note that happens to contain the word 'title': {nonsense:?}"
    );

    let hits = f.paths("frontmatter.title:Настройка");
    assert!(
        hits.contains(&"notes/russian.md".to_string()),
        "a frontmatter value must be addressable as a field: {hits:?}"
    );
}

#[test]
fn an_unparseable_query_must_not_degrade_into_a_bag_of_words() {
    // Found while checking whether C14's pass was real. `escape()` strips every
    // non-alphanumeric character and retries, so a query naming a field that does
    // not exist silently becomes a word soup and returns confident nonsense.
    //
    // The reader cannot tell the difference between "your filter matched these"
    // and "your filter was discarded". Same failure as guessing an ambiguous
    // link: the honest answer is to say the query was not understood.
    let f = corpus();
    let bogus = f.paths("nosuchfield:zarquon");
    assert!(
        bogus.is_empty(),
        "a query against an unknown field must not silently match on the words \
         it was made of: {bogus:?}"
    );
}

#[test]
fn c15_notes_can_be_found_by_what_they_link_to() {
    let f = corpus();
    let hits = f.paths("links_to:notes/beta.md");
    assert_eq!(
        hits,
        vec!["notes/alpha.md"],
        "alpha.md is the only note linking to beta.md"
    );
}

#[test]
fn a_link_written_inside_code_is_not_a_links_to_term() {
    // #20. The index records what the resolver decided, and the resolver does
    // not see inside code spans or blocks.
    let f = Fixture::new(
        "links-in-code",
        &[
            ("notes/beta.md", "# Beta\n"),
            (
                "example.md",
                "```\n[[beta]]\n```\n\nAnd `[[beta]]` inline.\n",
            ),
            ("real.md", "Really links to [[beta]].\n"),
        ],
    );
    assert_eq!(f.paths("links_to:notes/beta.md"), vec!["real.md"]);
}

#[test]
fn c16_editing_one_note_updates_the_index_without_a_full_rebuild() {
    let f = corpus();
    assert!(
        f.paths("supercalifragilistic").is_empty(),
        "term absent to begin with"
    );

    fs::write(
        f.root.join("notes/beta.md"),
        "# Beta\n\nNow contains supercalifragilistic.\n",
    )
    .unwrap();

    // The vault the update reads from must reflect the file on disk; rescanning
    // is the caller's job, not update_note's (see #6).
    let vault = Vault::scan(&f.root).unwrap();
    // "Without a full rebuild" is a structural claim, so assert the structure:
    // a rebuild recreates the index directory, an update leaves it in place.
    // The wall-clock bound below used to be the whole test, and it flaked on
    // the shared CI runner (413 ms under two parallel jobs); a directory
    // identity check cannot flake. Verified by mutation: forcing update_note
    // to recreate the directory fails this assertion.
    let dir_before = std::fs::metadata(&f.index).unwrap();
    let t = Instant::now();
    f.searcher
        .update_note(&vault, "notes/beta.md")
        .expect("the index must accept a single-note update");
    let elapsed = t.elapsed();
    let dir_after = std::fs::metadata(&f.index).unwrap();
    assert_eq!(
        (dir_before.ino(), dir_before.created().ok()),
        (dir_after.ino(), dir_after.created().ok()),
        "the index directory was recreated: that is a rebuild, not an update"
    );

    assert!(
        f.paths("supercalifragilistic")
            .contains(&"notes/beta.md".to_string()),
        "the edit must be visible without rebuilding"
    );
    // And the OLD content is gone — a replace, not an append.
    assert!(
        !f.paths("deployment").contains(&"notes/beta.md".to_string()),
        "the previous body must no longer match after the update"
    );
    // Latency stays as a loose sanity band (a rebuild of this fixture is
    // seconds, an update is milliseconds), wide enough for a loaded runner.
    assert!(
        elapsed.as_secs() < 5,
        "a one-note update took rebuild-scale time: {elapsed:?}"
    );
}

#[test]
fn c16b_deleting_a_note_removes_it_from_the_index() {
    let f = corpus();
    assert!(f.paths("zarquon").contains(&"notes/alpha.md".to_string()));
    fs::remove_file(f.root.join("notes/alpha.md")).unwrap();
    f.searcher
        .remove_note("notes/alpha.md")
        .expect("remove must succeed");
    assert!(
        !f.paths("zarquon").contains(&"notes/alpha.md".to_string()),
        "a removed note must not be a hit"
    );
}

#[test]
fn c17_a_full_rebuild_reproduces_the_same_results() {
    let f = corpus();
    let before = f.paths("zarquon");
    let vault = Vault::scan(&f.root).unwrap();
    let rebuilt = Searcher::build(&vault, &f.index).unwrap();
    let after: Vec<String> = rebuilt
        .search("zarquon", 30)
        .unwrap()
        .into_iter()
        .map(|h| h.path)
        .collect();
    assert_eq!(before, after, "a rebuild from scratch must be reproducible");
}

// -------------------------------------------------------------------- latency
//
// Bands live in docs/SEARCH-ACCEPTANCE.md and were fixed before measurement.
// These run against the fixture corpus, which is far smaller than the target
// vault, so they are a regression floor rather than the real verdict — the
// corpus-scale numbers are produced by the harness in the results commit and
// recorded in the doc.

fn percentile(mut xs: Vec<f64>, p: f64) -> f64 {
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let ix = ((xs.len() as f64 - 1.0) * p).round() as usize;
    xs[ix]
}

fn latency_ms(f: &Fixture, query: &str, runs: usize) -> Vec<f64> {
    // one untimed pass so the first-query cost does not land in the sample
    let _ = f.searcher.search(query, 30);
    (0..runs)
        .map(|_| {
            let t = Instant::now();
            let _ = f.searcher.search(query, 30);
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect()
}

#[test]
fn l1_l2_single_term_latency() {
    let f = corpus();
    let s = latency_ms(&f, "zarquon", 200);
    let p50 = percentile(s.clone(), 0.50);
    let p99 = percentile(s, 0.99);
    println!("L1 p50={p50:.3} ms  L2 p99={p99:.3} ms");
    assert!(
        p50 < 50.0,
        "L1 single-term p50 in the Fail band: {p50:.3} ms"
    );
    assert!(
        p99 < 150.0,
        "L2 single-term p99 in the Fail band: {p99:.3} ms"
    );
}

#[test]
fn l3_phrase_latency() {
    let f = corpus();
    let p50 = percentile(latency_ms(&f, "\"brown fox\"", 200), 0.50);
    println!("L3 phrase p50={p50:.3} ms");
    assert!(p50 < 75.0, "L3 phrase p50 in the Fail band: {p50:.3} ms");
}

#[test]
fn l5_index_open_is_cheap() {
    let f = corpus();
    let t = Instant::now();
    let _ = Searcher::open(&f.index).expect("index must open");
    let elapsed = t.elapsed().as_secs_f64() * 1000.0;
    println!("L5 index open={elapsed:.3} ms");
    assert!(
        elapsed < 200.0,
        "L5 index open in the Fail band: {elapsed:.3} ms"
    );
}
