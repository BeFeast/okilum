//! How the cost of opening a vault scope grows with the entries already in the
//! account directory (#3272). Records are never pruned, so a long-lived
//! account (or a test host) only ever adds to them; the registration index
//! (registry_index.rs) keeps a new folder's registration from reading them.
//!
//! Run on a pool host, in release as shipped:
//! `cargo test -p okilum-write-authority --release account_scale -- --ignored --nocapture`
//! With `ACCOUNT_SCALE_BUDGET_MS` set, the indexed first open of a new folder
//! at the largest size must stay within it (release only).
use super::registry_index::{self, ENABLED};
use super::*;
use std::time::{Duration, Instant};

/// Registered scopes whose folders still exist, as an account accumulates them.
fn fill(account: &Path, folders: &Path, count: usize) -> Result<()> {
    let sample = identity(folders)?;
    for index in 0..count {
        let root = folders.join(format!("folder-{index}"));
        fs::create_dir(&root)?;
        let binding = Binding {
            schema: BINDING_SCHEMA,
            root: root.clone(),
            root_identity: identity(&root)?,
            epoch: uuid::Uuid::new_v4().to_string(),
            directory: sample.clone(),
            admission: sample.clone(),
            writers: sample.clone(),
            owner: sample.clone(),
        };
        persist(
            &account.join(format!("{}.json", key(&root))),
            &binding,
            true,
        )?;
        fs::create_dir(account.join(key(&root)))?;
    }
    Ok(())
}

fn average(runs: u32, mut work: impl FnMut(u32) -> Result<()>) -> Result<Duration> {
    let start = Instant::now();
    for run in 0..runs {
        work(run)?;
    }
    Ok(start.elapsed() / runs)
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}

#[test]
#[ignore = "measurement; run explicitly with --ignored --nocapture"]
fn account_scale_open_cost_by_entry_count() -> Result<()> {
    let sizes = std::env::var("ACCOUNT_SCALE_SIZES").unwrap_or_else(|_| "0,100,1000,10000".into());
    let budget = std::env::var("ACCOUNT_SCALE_BUDGET_MS")
        .ok()
        .map(|ms| ms.parse::<f64>().expect("budget in ms"));
    let temp = tempfile::tempdir()?;
    let account = temp.path().join("account");
    let folders = temp.path().join("folders");
    fs::create_dir(&folders)?;
    private_dir(&account)?;
    let mut have = 0;
    let mut probe = 0;
    let mut new_folder = || -> Result<PathBuf> {
        probe += 1;
        let root = temp.path().join(format!("new-{probe}"));
        fs::create_dir(&root)?;
        Ok(root)
    };
    println!(
        "entries | first open of a new folder: full scan (pre-#3272) | index rebuilt | index | reopen + admit of a known folder"
    );
    let mut last = None;
    for size in sizes.split(',').map(|s| s.trim().parse::<usize>().unwrap()) {
        // Entries are appended in steps, so the account only grows.
        let batch = folders.join(format!("batch-{size}"));
        fs::create_dir(&batch)?;
        fill(&account, &batch, size - have)?;
        have = size;
        let known = temp.path().join(format!("known-{size}"));
        fs::create_dir(&known)?;
        Authority::open_in(&account, &known)?;
        // Records are cached once their last change is SETTLE old, as an
        // account's records are by the time a folder is opened.
        std::thread::sleep(registry_index::SETTLE + Duration::from_millis(100));
        ENABLED.with(|e| e.set(false));
        let full = average(5, |_| {
            Authority::open_in(&account, &new_folder()?).map(drop)
        });
        ENABLED.with(|e| e.set(true));
        let full = full?;
        let _ = fs::remove_file(account.join(registry_index::FILE));
        let rebuilt = average(1, |_| {
            Authority::open_in(&account, &new_folder()?).map(drop)
        })?;
        let indexed = average(5, |_| {
            Authority::open_in(&account, &new_folder()?).map(drop)
        })?;
        let warm = average(50, |_| {
            let authority = Authority::open_in(&account, &known)?;
            drop(authority.admit()?);
            Ok(())
        })?;
        let entries = fs::read_dir(&account)?
            .filter(|e| {
                e.as_ref()
                    .is_ok_and(|e| e.path().extension().is_some_and(|x| x == "json"))
            })
            .count();
        println!(
            "{entries:>7} | {:>10.2} ms | {:>10.2} ms | {:>10.2} ms | {:>8.3} ms",
            ms(full),
            ms(rebuilt),
            ms(indexed),
            ms(warm)
        );
        last = Some((size, full, indexed));
    }
    if let (Some(budget), Some((size, full, indexed))) = (budget, last) {
        if cfg!(debug_assertions) {
            println!("debug build: the {budget} ms budget applies to release only");
        } else {
            assert!(
                ms(indexed) <= budget,
                "indexed first open at {size} entries: {:.2} ms > {budget} ms",
                ms(indexed)
            );
        }
        // Control: the measured path is the index, not a cheap account.
        if size >= 1000 {
            assert!(indexed < full, "index {indexed:?} vs full scan {full:?}");
        }
    }
    Ok(())
}
