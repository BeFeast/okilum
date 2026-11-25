fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
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
    // Warm the page cache (and, with the index, the index file).
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
