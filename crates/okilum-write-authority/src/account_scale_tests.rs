//! How the cost of opening a vault scope grows with the entries already in the
//! account directory (#3272). Records are never pruned, so a long-lived
//! account (or a test host) only ever adds to them. A new folder's
//! registration reads each of them once per registry.lock hold, not twice
//! (registry_reads.rs).
//!
//! Run on a pool host, in release as shipped:
//! `cargo test -p okilum-write-authority --release account_scale -- --ignored --nocapture`
//! With `ACCOUNT_SCALE_BUDGET_MS` set, the first open of a new folder at the
//! largest size must stay within it (release only).
use super::registry_reads::ENABLED;
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
        "entries | first open of a new folder: two full scans (pre-#3272) | one read per hold | reopen + admit of a known folder"
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
        ENABLED.with(|e| e.set(false));
        let full = average(5, |_| {
            Authority::open_in(&account, &new_folder()?).map(drop)
        });
        ENABLED.with(|e| e.set(true));
        let full = full?;
        let reused = average(5, |_| {
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
            "{entries:>7} | {:>10.2} ms | {:>10.2} ms | {:>8.3} ms",
            ms(full),
            ms(reused),
            ms(warm)
        );
        last = Some((size, full, reused));
    }
    if let (Some(budget), Some((size, full, reused))) = (budget, last) {
        if cfg!(debug_assertions) {
            println!("debug build: the {budget} ms budget applies to release only");
        } else {
            assert!(
                ms(reused) <= budget,
                "first open at {size} entries: {:.2} ms > {budget} ms",
                ms(reused)
            );
        }
        // Control: the measured path is the reuse, not a cheap account.
        if size >= 1000 {
            assert!(reused < full, "reuse {reused:?} vs two full scans {full:?}");
        }
    }
    Ok(())
}

/// How long a registration of a new folder can keep another registration or
/// rebind waiting on `registry.lock` (#3272 review): another thread takes and
/// releases the lock every millisecond while folders are registered, and the
/// longest wait it saw is reported. Uses only what main has, so the same
/// function measures main.
#[test]
#[ignore = "measurement; run explicitly with --ignored --nocapture"]
fn account_scale_registry_lock_wait() -> Result<()> {
    let size: usize = std::env::var("ACCOUNT_SCALE_LOCK_SIZE")
        .ok()
        .map_or(10_000, |size| size.parse().expect("record count"));
    let temp = tempfile::tempdir()?;
    let account = temp.path().join("account");
    let folders = temp.path().join("folders");
    fs::create_dir(&folders)?;
    private_dir(&account)?;
    fill(&account, &folders, size)?;
    // Warm the page cache.
    for warm in 0..2 {
        let root = temp.path().join(format!("warm-{warm}"));
        fs::create_dir(&root)?;
        Authority::open_in(&account, &root)?;
    }
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let registering = {
        let (account, base, done) = (account.clone(), temp.path().to_owned(), done.clone());
        std::thread::spawn(move || -> Result<Duration> {
            let runs = 10;
            let start = Instant::now();
            for run in 0..runs {
                let root = base.join(format!("lock-{run}"));
                fs::create_dir(&root)?;
                Authority::open_in(&account, &root)?;
            }
            done.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(start.elapsed() / runs)
        })
    };
    let lock = file(&account.join("registry.lock"), true)?;
    let mut waits = Vec::new();
    while !done.load(std::sync::atomic::Ordering::SeqCst) {
        let start = Instant::now();
        lock.lock()?;
        waits.push(start.elapsed());
        lock.unlock()?;
        std::thread::sleep(Duration::from_millis(1));
    }
    let registration = registering.join().expect("registering thread")?;
    waits.sort();
    let longest = waits.last().copied().unwrap_or_default();
    let p95 = waits
        .get(waits.len() * 95 / 100)
        .copied()
        .unwrap_or_default();
    println!(
        "{size} records | registration {:.2} ms | registry.lock wait of another registrant: longest {:.2} ms, p95 {:.2} ms over {} probes",
        ms(registration),
        ms(longest),
        ms(p95),
        waits.len()
    );
    Ok(())
}
