//! How the cost of opening a vault scope grows with the entries already in the
//! account directory (#3272). Nothing prunes those entries, so a long-lived
//! account (or a test host) only ever adds to them.
//!
//! Run on a pool host, in release as shipped:
//! `cargo test -p okilum-write-authority --release account_scale -- --ignored --nocapture`
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

#[test]
#[ignore = "measurement; run explicitly with --ignored --nocapture"]
fn account_scale_open_cost_by_entry_count() -> Result<()> {
    let sizes = std::env::var("ACCOUNT_SCALE_SIZES").unwrap_or_else(|_| "0,100,1000,10000".into());
    let temp = tempfile::tempdir()?;
    let account = temp.path().join("account");
    let folders = temp.path().join("folders");
    fs::create_dir(&folders)?;
    private_dir(&account)?;
    let mut have = 0;
    let mut probe = 0;
    println!("entries | first open of a new folder | reopen + admit of a known folder");
    for size in sizes.split(',').map(|s| s.trim().parse::<usize>().unwrap()) {
        // Entries are appended in steps, so the account only grows.
        let batch = folders.join(format!("batch-{size}"));
        fs::create_dir(&batch)?;
        fill(&account, &batch, size - have)?;
        have = size;
        let known = temp.path().join(format!("known-{size}"));
        fs::create_dir(&known)?;
        Authority::open_in(&account, &known)?;
        let cold = average(5, |_| {
            probe += 1;
            let root = temp.path().join(format!("new-{probe}"));
            fs::create_dir(&root)?;
            Authority::open_in(&account, &root).map(drop)
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
            "{entries:>7} | {:>10.2} ms | {:>8.3} ms",
            cold.as_secs_f64() * 1e3,
            warm.as_secs_f64() * 1e3
        );
    }
    Ok(())
}
