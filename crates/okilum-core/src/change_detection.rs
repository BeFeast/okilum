//! Stat timestamps are hints, not content revisions. Coarse/network filesystems
//! and the current timestamp tick require bounded content verification.
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path, time::SystemTime};

pub(crate) fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub(crate) fn hash_file(path: &Path, size: u64) -> std::io::Result<[u8; 32]> {
    // Never hydrate a cloud placeholder merely to validate a derived revision.
    if crate::vault::cloud_placeholder(path) {
        return Err(std::io::Error::other(
            "Cloud placeholder has no local revision",
        ));
    }
    #[cfg(unix)]
    let file = File::from(rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?);
    #[cfg(not(unix))]
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("Not a regular source file"));
    }
    hash_reader(file, size)
}

pub(crate) fn hash_reader(file: impl Read, size: u64) -> std::io::Result<[u8; 32]> {
    let mut file = file.take(size.saturating_add(1));
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut count = 0u64;
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        count += n as u64;
        hash.update(&buffer[..n]);
    }
    if count != size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "File changed during revision check",
        ));
    }
    Ok(hash.finalize().into())
}

pub(crate) fn needs_content_check(path: &Path, changed: SystemTime) -> bool {
    #[cfg(test)]
    if FORCE_CONTENT.with(|value| value.get()) {
        return true;
    }
    #[cfg(test)]
    if let Some(now) = NATIVE_CTIME_CLOCK.with(|clock| clock.borrow_mut().as_mut().map(|now| now()))
    {
        return now
            .duration_since(changed)
            .map_or(true, |age| age.as_secs() < 2);
    }
    // Even a local filesystem may round ctime to a scheduler tick. No write
    // observed in the current (conservatively two-second) tick is stat-only.
    if changed.elapsed().map_or(true, |age| age.as_secs() < 2) {
        return true;
    }
    !known_local_filesystem(path)
}

fn known_local_filesystem(path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        // ZFS, NFS, SMB, FUSE, overlay and unknown/failed probes all verify bytes.
        // A local old ctime can be reused: a subsequent write advances it to now.
        rustix::fs::statfs(path).is_ok_and(|stat| {
            matches!(
                stat.f_type as u64,
                0xef53 | 0x58465342 | 0x9123683e | 0x01021994
            )
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        false
    }
}

#[cfg(test)]
thread_local! {
    static FORCE_CONTENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static NATIVE_CTIME_CLOCK: std::cell::RefCell<Option<Box<dyn FnMut() -> SystemTime>>> =
        const { std::cell::RefCell::new(None) };
}

/// Emulate the known-local filesystem policy with an injectable clock. This
/// thread-scoped hook exercises real metadata and fingerprints without sleeps
/// or dependence on the test host's filesystem. Coarse-content checks win.
#[cfg(test)]
pub(crate) fn with_native_ctime_clock<T>(
    now: impl FnMut() -> SystemTime + 'static,
    run: impl FnOnce() -> T,
) -> T {
    struct Reset(Option<Box<dyn FnMut() -> SystemTime>>);
    impl Drop for Reset {
        fn drop(&mut self) {
            NATIVE_CTIME_CLOCK.with(|clock| *clock.borrow_mut() = self.0.take());
        }
    }
    let _reset = Reset(NATIVE_CTIME_CLOCK.with(|clock| clock.replace(Some(Box::new(now)))));
    run()
}

/// Test-only emulation of a filesystem which returns one ctime tick for all
/// writes. Actual metadata equality is separately reproduced on the ZFS slot.
#[inline]
pub(crate) fn change_time(seconds: i64, nanos: i64) -> (i64, i64) {
    #[cfg(test)]
    if FORCE_CONTENT.with(|value| value.get()) {
        return (1_700_000_000, 0);
    }
    (seconds, nanos)
}

/// Exercise the conservative filesystem path on local test hosts too. Scoped to
/// this test thread; never modifies process environment or another test's policy.
#[cfg(test)]
pub(crate) fn with_content_checks<T>(run: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            FORCE_CONTENT.with(|value| value.set(self.0));
        }
    }
    let _reset = Reset(FORCE_CONTENT.with(|value| value.replace(true)));
    run()
}

/// A positive control for tests specifically asserting native ctime changes.
/// Native timing tests may skip; content-fallback/safety tests must always run.
#[cfg(test)]
pub(crate) fn native_ctime_changes(root: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mut probe = tempfile::NamedTempFile::new_in(root).unwrap();
        use std::io::{Seek, Write};
        probe.write_all(b"old!").unwrap();
        let mut minimum_step = i128::MAX;
        for _ in 0..32 {
            let before = probe.as_file().metadata().unwrap();
            probe.as_file_mut().rewind().unwrap();
            probe.write_all(b"new!").unwrap();
            probe
                .as_file()
                .set_modified(before.modified().unwrap())
                .unwrap();
            let after = probe.as_file().metadata().unwrap();
            let step = (after.ctime() as i128 - before.ctime() as i128) * 1_000_000_000
                + (after.ctime_nsec() as i128 - before.ctime_nsec() as i128);
            let observed = step != 0;
            minimum_step = minimum_step.min(step.abs());
            if !observed {
                diagnostic_error!("SKIP native ctime test: same-size write with restored mtime is unobservable in this filesystem tick (ctime={}.{:09}); content-fallback tests still run", before.ctime(), before.ctime_nsec());
            }
            if !observed {
                return false;
            }
        }
        diagnostic_error!("CTIME_PROBE: 32/32 same-size restored-mtime writes observed; minimum ctime step={minimum_step}ns");
        true
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        true
    }
}
