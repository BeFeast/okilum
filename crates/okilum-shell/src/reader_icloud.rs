//! Explicit-open hydration only. Inventory/indexing never calls this module.
use std::path::{Path, PathBuf};
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::Instant;

pub(crate) const DOWNLOADING: &str = "Downloading from iCloud…";
const SF_DATALESS: u32 = 0x4000_0000;

#[derive(Debug)]
pub(crate) struct DownloadError(pub String);
impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for DownloadError {}

/// Legacy iCloud stubs are `.name.ext.icloud`; the document keeps `name.ext`.
/// Pure name handling is safe on the UI thread; metadata and Cocoa are not.
pub(crate) fn logical_path(path: &Path) -> PathBuf {
    let name = path.file_name().and_then(|s| s.to_str());
    match name
        .and_then(|s| s.strip_prefix('.'))
        .and_then(|s| s.strip_suffix(".icloud"))
    {
        Some(name) if !name.is_empty() && name != "." && name != ".." => path.with_file_name(name),
        _ => path.to_path_buf(),
    }
}

fn placeholder_name(path: &Path) -> PathBuf {
    let mut name = std::ffi::OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(".icloud");
    path.with_file_name(name)
}

fn dataless(flags: u32) -> bool {
    flags & SF_DATALESS != 0
}

fn pending_state(flags: Option<u32>, stub_exists: bool, requested: bool) -> bool {
    match flags {
        Some(flags) => dataless(flags),
        // The stub may disappear before the materialized pathname arrives.
        None => stub_exists || requested,
    }
}

fn require_worker(is_main_thread: bool) -> anyhow::Result<()> {
    anyhow::ensure!(
        !is_main_thread,
        "iCloud hydration requires a background worker"
    );
    Ok(())
}

/// Dependency-injected polling keeps timeout, cancellation and controls testable
/// without iCloud. Called exclusively by the Reader's background executor.
fn wait_for_download(
    mut pending: impl FnMut() -> anyhow::Result<bool>,
    request: impl FnOnce() -> anyhow::Result<()>,
    mut progress: impl FnMut() -> anyhow::Result<()>,
    mut elapsed: impl FnMut() -> Duration,
    mut pause: impl FnMut(),
    timeout: Duration,
) -> anyhow::Result<()> {
    if !pending()? {
        return Ok(());
    }
    progress()?;
    request().map_err(|e| DownloadError(format!("Couldn’t download from iCloud. Check your connection and iCloud account, then try again. {e}")))?;
    loop {
        progress()?;
        if !pending()? {
            return Ok(());
        }
        if elapsed() >= timeout {
            return Err(DownloadError("iCloud download timed out. Check your connection and iCloud account, then try again.".into()).into());
        }
        pause();
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn materialize(
    path: &Path,
    progress: impl FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<PathBuf> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};
    use std::os::macos::fs::MetadataExt;
    // Fail before even stat/Cocoa if a future caller moves this onto the UI.
    require_worker(objc2::MainThreadMarker::new().is_some())?;
    let path = logical_path(path);
    let started = Instant::now();
    let requested = std::cell::Cell::new(false);
    // User-facing default, configurable for slower/offline iCloud accounts.
    let timeout = std::env::var("OKILUM_ICLOUD_DOWNLOAD_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(60);
    wait_for_download(
        || match std::fs::metadata(&path) {
            Ok(meta) => {
                Ok(meta.is_file() && pending_state(Some(meta.st_flags()), false, requested.get()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(pending_state(
                None,
                placeholder_name(&path).try_exists()?,
                requested.get(),
            )),
            Err(e) => Err(e.into()),
        },
        || {
            requested.set(true);
            objc2::rc::autoreleasepool(|_| {
                let path = path
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("The iCloud path is not valid UTF-8"))?;
                let url = NSURL::fileURLWithPath(&NSString::from_str(path));
                NSFileManager::defaultManager()
                    .startDownloadingUbiquitousItemAtURL_error(&url)
                    .map_err(|e| anyhow::anyhow!("{e}"))
            })
        },
        progress,
        || started.elapsed(),
        || std::thread::sleep(Duration::from_millis(250)),
        Duration::from_secs(timeout),
    )?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn detects_legacy_names_and_dataless_flag_without_reading_bytes() {
        assert_eq!(
            logical_path(Path::new("folder/.Заметка.md.icloud")),
            Path::new("folder/Заметка.md")
        );
        for name in ["note.md", "note.icloud", ".icloud", "..icloud"] {
            assert_eq!(logical_path(Path::new(name)), Path::new(name));
        }
        assert_eq!(
            placeholder_name(Path::new("folder/note.md")),
            Path::new("folder/.note.md.icloud")
        );
        assert!(dataless(SF_DATALESS | 1));
        assert!(!dataless(1));
        assert!(pending_state(None, true, false));
        assert!(pending_state(Some(SF_DATALESS), false, false));
        assert!(
            pending_state(None, false, true),
            "stub removal is not readiness"
        );
        assert!(!pending_state(None, false, false));
        assert!(
            !pending_state(Some(0), true, true),
            "materialized file is ready"
        );
    }

    #[test]
    fn explicit_download_reports_progress_and_waits_until_materialized() {
        let checks = Cell::new(0);
        let requested = Cell::new(false);
        let progress = Cell::new(0);
        wait_for_download(
            || {
                checks.set(checks.get() + 1);
                Ok(checks.get() < 4)
            },
            || {
                requested.set(true);
                Ok(())
            },
            || {
                progress.set(progress.get() + 1);
                Ok(())
            },
            || Duration::ZERO,
            || {},
            Duration::from_secs(60),
        )
        .unwrap();
        assert!(requested.get());
        assert_eq!(checks.get(), 4);
        assert!(progress.get() >= 2);
        assert_eq!(DOWNLOADING, "Downloading from iCloud…");
    }

    #[test]
    fn ui_thread_is_rejected_before_any_filesystem_or_download_work() {
        assert!(require_worker(true).is_err());
        assert!(require_worker(false).is_ok());
    }

    #[test]
    fn local_file_never_requests_download_or_displays_progress() {
        wait_for_download(
            || Ok(false),
            || panic!("local download"),
            || panic!("local progress"),
            || Duration::ZERO,
            || panic!("local wait"),
            Duration::from_secs(60),
        )
        .unwrap();
    }

    #[test]
    fn offline_timeout_and_cancel_are_distinct_and_never_report_ready() {
        let offline = wait_for_download(
            || Ok(true),
            || anyhow::bail!("offline"),
            || Ok(()),
            || Duration::ZERO,
            || {},
            Duration::from_secs(60),
        )
        .unwrap_err();
        assert!(offline.downcast_ref::<DownloadError>().is_some());
        assert!(offline.to_string().contains("offline"));
        let timeout = wait_for_download(
            || Ok(true),
            || Ok(()),
            || Ok(()),
            || Duration::from_secs(60),
            || panic!("must not wait past deadline"),
            Duration::from_secs(60),
        )
        .unwrap_err();
        assert!(timeout.to_string().contains("timed out"));
        let cancelled = wait_for_download(
            || Ok(true),
            || panic!("cancelled request"),
            || anyhow::bail!("cancelled"),
            || Duration::ZERO,
            || {},
            Duration::from_secs(60),
        )
        .unwrap_err();
        assert_eq!(cancelled.to_string(), "cancelled");
    }
}
