//! Corpus-scale latency for the bands in `docs/SEARCH-ACCEPTANCE.md`.
//!
//! Ignored by default: it needs a real vault, which no CI has. Run it against
//! one explicitly:
//!
//! ```sh
//! OKILUM_BENCH_VAULT=/path/to/vault \
//!   cargo test -p okilum-core --test search_bench -- --ignored --nocapture
//! ```
//!
//! The fixture-sized numbers in `search_acceptance.rs` are a regression floor.
//! These are the ones the bands are judged on.

use std::path::{Path, PathBuf};
use std::time::Instant;

use okilum_core::{Searcher, Vault};

fn vault_path() -> PathBuf {
    PathBuf::from(
        std::env::var("OKILUM_BENCH_VAULT")
            .expect("set OKILUM_BENCH_VAULT to a real vault to run this"),
    )
}

fn percentile(mut xs: Vec<f64>, p: f64) -> f64 {
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    xs[((xs.len() as f64 - 1.0) * p).round() as usize]
}

fn sample(searcher: &Searcher, query: &str, runs: usize) -> Vec<f64> {
    let _ = searcher.search(query, 30); // warm; the first query is not the band
    (0..runs)
        .map(|_| {
            let t = Instant::now();
            let _ = searcher.search(query, 30);
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect()
}

fn verdict(value: f64, pass: f64, fail: f64) -> &'static str {
    if value < pass {
        "PASS"
    } else if value < fail {
        "GRAY"
    } else {
        "FAIL"
    }
}

#[test]
#[ignore = "needs a real vault via OKILUM_BENCH_VAULT"]
fn corpus_scale_latency() {
    let root = vault_path();
    let index = std::env::temp_dir().join("okilum-bench-index");
    let _ = std::fs::remove_dir_all(&index);

    let t = Instant::now();
    let vault = Vault::scan(Path::new(&root)).unwrap();
    let scan_ms = t.elapsed().as_secs_f64() * 1000.0;

    let t = Instant::now();
    let _built = Searcher::build(&vault, &index).unwrap();
    let build_s = t.elapsed().as_secs_f64();

    let t = Instant::now();
    let opened = Searcher::open(&index).unwrap();
    let open_ms = t.elapsed().as_secs_f64() * 1000.0;

    let single = sample(&opened, "maestro", 300);
    let l1 = percentile(single.clone(), 0.50);
    let l2 = percentile(single, 0.99);
    let l3 = percentile(sample(&opened, "\"read only\"", 300), 0.50);
    // A term that hits a large share of the corpus, so the collector does real
    // work rather than short-circuiting on a handful of postings.
    let l4 = percentile(sample(&opened, "the", 300), 0.50);

    println!(
        "\ncorpus: {} notes, scanned in {scan_ms:.0} ms",
        vault.notes.len()
    );
    println!("{:<38} {:>10}  band", "measure", "value");
    println!("{:-<60}", "");
    println!(
        "{:<38} {:>7.2} ms  {}",
        "L1 single-term p50",
        l1,
        verdict(l1, 15.0, 50.0)
    );
    println!(
        "{:<38} {:>7.2} ms  {}",
        "L2 single-term p99",
        l2,
        verdict(l2, 50.0, 150.0)
    );
    println!(
        "{:<38} {:>7.2} ms  {}",
        "L3 phrase p50",
        l3,
        verdict(l3, 25.0, 75.0)
    );
    println!(
        "{:<38} {:>7.2} ms  {}",
        "L4 common-term p50",
        l4,
        verdict(l4, 40.0, 120.0)
    );
    println!(
        "{:<38} {:>7.2} ms  {}",
        "L5 index open",
        open_ms,
        verdict(open_ms, 50.0, 200.0)
    );
    println!(
        "{:<38} {:>7.2} s   {}",
        "L6 full rebuild",
        build_s,
        verdict(build_s, 30.0, 120.0)
    );
    println!();

    // Fail the run only on a Fail band, so a Gray result is reported rather than
    // hidden behind a red test.
    for (name, v, fail) in [
        ("L1", l1, 50.0),
        ("L2", l2, 150.0),
        ("L3", l3, 75.0),
        ("L4", l4, 120.0),
        ("L5", open_ms, 200.0),
        ("L6", build_s * 1000.0, 120_000.0),
    ] {
        assert!(v < fail, "{name} landed in the Fail band: {v:.2}");
    }
}

/// #2094 speed budget: query latency on a synthetic 10,000-note vault.
///
/// Unlike `corpus_scale_latency`, this needs no private vault, so hosted CI can
/// run it on a Stable candidate. It writes the vault to disk, scans and indexes
/// it through the product path, then times the Search palette's call: ownership
/// over every note applied before the top-100 cut (`search_in_paths`), fully
/// materialized ranked rows with snippets. No UI paint is included.
///
/// Report-only: the test fails only when a positive control fails (wrong hit
/// counts, missing snippets). Budgets are judged by `scripts/perf/speed`, which
/// reads the JSON written to `OKILUM_SPEED_SEARCH_REPORT`.
///
/// ```sh
/// OKILUM_SPEED_SEARCH_REPORT=search.json cargo test --release -p okilum-core \
///   --test search_bench synthetic_10k -- --ignored --nocapture --test-threads=1
/// ```
#[test]
#[ignore = "speed budget probe; run by scripts/perf/speed in the stable-speed workflow"]
fn synthetic_10k_query_latency() {
    const NOTES: usize = 10_000;
    const LIMIT: usize = 100;
    const RUNS: usize = 30;
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("vault");
    for folder in ["archive", "notes"] {
        std::fs::create_dir_all(root.join(folder)).unwrap();
    }
    let mut source_bytes = 0usize;
    for i in 0..NOTES {
        let folder = if i % 2 == 0 { "archive" } else { "notes" };
        // One note in a hundred is ~65 KiB, so snippet work sees long bodies.
        let prose = "Ordinary synthetic prose with unrelated tokens. ".repeat(if i % 100 == 0 {
            1400
        } else {
            35
        });
        let text = format!(
            "# Document {i:05}\n\n{prose}\n\ncommonmarker adjacent phrase שלום\n\n{}\n\nunique{i:05}\n",
            "More ordinary source text. ".repeat(35)
        );
        source_bytes += text.len();
        std::fs::write(root.join(folder).join(format!("note-{i:05}.md")), text).unwrap();
    }

    let t = Instant::now();
    let vault = Vault::scan(&root).unwrap();
    let scan_ms = t.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(
        vault.notes.len(),
        NOTES,
        "the synthetic vault was not fully scanned"
    );
    let index = work.path().join("index");
    let t = Instant::now();
    // Merges finish before timing, so no background merge competes with queries.
    Searcher::build(&vault, &index)
        .unwrap()
        .finish_build()
        .unwrap();
    let build_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let searcher = Searcher::open(&index).unwrap();
    let open_ms = t.elapsed().as_secs_f64() * 1000.0;
    let mut owned: Vec<String> = (0..NOTES)
        .map(|i| {
            let folder = if i % 2 == 0 { "archive" } else { "notes" };
            format!("{folder}/note-{i:05}.md")
        })
        .collect();
    owned.sort();

    let mut queries = Vec::new();
    for (query, expected) in [
        ("commonmarker", LIMIT),
        ("adjacent phrase", LIMIT),
        ("שלום", LIMIT),
        ("path:archive commonmarker", LIMIT),
        ("unique09999", 1),
    ] {
        let mut times = Vec::with_capacity(RUNS + 1);
        for _ in 0..=RUNS {
            let t = Instant::now();
            let hits = searcher.search_in_paths(query, LIMIT, &owned).unwrap();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            // Positive controls: the timed call did the palette's real work.
            assert_eq!(hits.len(), expected, "{query}: wrong hit count");
            assert!(
                hits.iter()
                    .all(|h| h.display_snippet.is_some() && !h.snippet_html.is_empty()),
                "{query}: a row has no materialized snippet"
            );
            if query.starts_with("path:") {
                assert!(hits.iter().all(|h| h.path.starts_with("archive/")));
            }
            if expected == 1 {
                assert_eq!(hits[0].path, "notes/note-09999.md");
            }
        }
        let first_ms = times.remove(0);
        println!(
            "SPEED_SEARCH query={query:?} first_ms={first_ms:.3} p50_ms={:.3} max_ms={:.3}",
            percentile(times.clone(), 0.5),
            percentile(times.clone(), 1.0)
        );
        queries.push(serde_json::json!({
            "query": query, "hits": expected, "first_ms": first_ms, "samples_ms": times,
        }));
    }
    let report = serde_json::json!({
        "schema": 1,
        "boundary": "query_to_materialized_rows (search_in_paths, all notes owned, limit 100)",
        "notes": NOTES, "source_bytes": source_bytes, "limit": LIMIT, "runs": RUNS,
        "scan_ms": scan_ms, "build_ms": build_ms, "open_ms": open_ms,
        "queries": queries,
    });
    println!("SPEED_SEARCH notes={NOTES} source_bytes={source_bytes} scan_ms={scan_ms:.0} build_ms={build_ms:.0} open_ms={open_ms:.1}");
    if let Some(path) = std::env::var_os("OKILUM_SPEED_SEARCH_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}
