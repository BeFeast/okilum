//! Corpus-scale latency for the bands in `docs/SEARCH-ACCEPTANCE.md`.
//!
//! Ignored by default: it needs a real vault, which no CI has. Run it against
//! one explicitly:
//!
//! ```sh
//! TESSERA_BENCH_VAULT=/path/to/vault \
//!   cargo test -p tessera-core --test search_bench -- --ignored --nocapture
//! ```
//!
//! The fixture-sized numbers in `search_acceptance.rs` are a regression floor.
//! These are the ones the bands are judged on.

use std::path::{Path, PathBuf};
use std::time::Instant;

use tessera_core::{Searcher, Vault};

fn vault_path() -> PathBuf {
    PathBuf::from(
        std::env::var("TESSERA_BENCH_VAULT")
            .expect("set TESSERA_BENCH_VAULT to a real vault to run this"),
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
#[ignore = "needs a real vault via TESSERA_BENCH_VAULT"]
fn corpus_scale_latency() {
    let root = vault_path();
    let index = std::env::temp_dir().join("tessera-bench-index");
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
