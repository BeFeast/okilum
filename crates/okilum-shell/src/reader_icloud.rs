//! Explicit user-requested hydration only. Inventory/indexing never calls this module.
use std::path::{Path, PathBuf};
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::Instant;

pub(crate) const DOWNLOADING: &str = "Downloading from iCloud…";
pub(crate) const KEEP_DOWNLOADED: &str =
    "To keep files available, select this folder in Finder and choose Keep Downloaded.";
const SF_DATALESS: u32 = 0x4000_0000;

/// Request submission is independent of inventory refreshes. Dropping the owning
/// load (vault switch/window close) stops submission of the remaining requests.
pub(crate) struct Batch {
    pub active: bool,
    pub identity: std::sync::Arc<()>,
    pub cancel: super::reader_loading::Cancellation,
}
impl Default for Batch {
    fn default() -> Self {
        Self {
            active: true,
            identity: Default::default(),
            cancel: Default::default(),
        }
    }
}
impl Drop for Batch {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[derive(Default, Debug, PartialEq, Eq)]
struct BatchResult {
    requested: usize,
    failed: usize,
}

/// Submit each known placeholder once, without reading source bytes or waiting
/// for one file to finish before asking iCloud for the next. FSEvents supplies
/// materialization updates through the normal metadata-only inventory path.
fn request_all(
    paths: Vec<PathBuf>,
    mut check: impl FnMut() -> anyhow::Result<()>,
    mut request: impl FnMut(&Path) -> anyhow::Result<()>,
) -> anyhow::Result<BatchResult> {
    let mut result = BatchResult::default();
    let paths: std::collections::BTreeSet<_> = paths.iter().map(|p| logical_path(p)).collect();
    for path in paths {
        check()?;
        match request(&path) {
            Ok(()) => result.requested += 1,
            Err(_) => result.failed += 1,
        }
    }
    Ok(result)
}

impl super::Reader {
    #[cfg(target_os = "macos")]
    pub(super) fn download_all_icloud(&mut self, cx: &mut gpui::Context<Self>) {
        self.download_all_icloud_using(cx, |path| {
            require_worker(objc2::MainThreadMarker::new().is_some())?;
            request_download(path)
        });
    }

    fn download_all_icloud_using(
        &mut self,
        cx: &mut gpui::Context<Self>,
        request: impl FnMut(&Path) -> anyhow::Result<()> + Send + 'static,
    ) {
        let Some(load) = self.loading.as_mut() else {
            return;
        };
        if load
            .cloud_download
            .as_ref()
            .is_some_and(|batch| batch.active)
            || self.vault.cloud_placeholders.is_empty()
        {
            return;
        }
        let batch = Batch::default();
        let identity = batch.identity.clone();
        let cancel = batch.cancel.clone();
        load.cloud_download = Some(batch);
        let root = self.vault_root.clone();
        let paths = self.vault.cloud_placeholders.clone();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move {
                request_all(paths, || cancel.check(), request)
            }).await;
            let _ = this.update(cx, |this, cx| {
                let Some(batch) = this.loading.as_mut().and_then(|load| load.cloud_download.as_mut()) else { return };
                if this.vault_root != root || !std::sync::Arc::ptr_eq(&batch.identity, &identity) {
                    return;
                }
                batch.active = false;
                if let Ok(result) = result {
                    let message = if result.failed == 0 {
                        format!("Downloads requested from iCloud. {KEEP_DOWNLOADED}")
                    } else {
                        format!("Couldn’t request {} iCloud download{}. Check your connection and iCloud account, then try Download all again. {KEEP_DOWNLOADED}", result.failed, if result.failed == 1 { "" } else { "s" })
                    };
                    this.link_notice = Some(message.into());
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }
}

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
    okilum_core::vault::cloud_document_path(path)
}

/// Preserve vault-vs-file intent. Only a legacy placeholder rename changes
/// the explicit path; a plain folder/empty selection never requests hydration.
pub(crate) fn prepare_explicit_open(
    open_path: &mut Option<PathBuf>,
    root: Option<&Path>,
    note: Option<&str>,
    materialize: impl FnOnce(&Path) -> anyhow::Result<PathBuf>,
) -> anyhow::Result<()> {
    let path = open_path
        .clone()
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| {
            root.zip(note.filter(|note| !note.is_empty()))
                .map(|(root, note)| root.join(note))
        });
    if let Some(path) = path {
        let ready = materialize(&path)?;
        if ready != path {
            *open_path = Some(ready);
        }
    }
    Ok(())
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

#[cfg(target_os = "macos")]
fn request_download(path: &Path) -> anyhow::Result<()> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};
    require_worker(objc2::MainThreadMarker::new().is_some())?;
    objc2::rc::autoreleasepool(|_| {
        let path = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("The iCloud path is not valid UTF-8"))?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        NSFileManager::defaultManager()
            .startDownloadingUbiquitousItemAtURL_error(&url)
            .map_err(|e| anyhow::anyhow!("{e}"))
    })
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
            request_download(&path)
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
    fn bulk_requests_normalize_stubs_once_and_continue_after_one_failure() {
        let mut requested = Vec::new();
        let paths = [
            "vault/.a.md.icloud",
            "vault/a.md",
            "vault/b.md",
            "vault/c.md",
        ];
        let result = request_all(
            paths.map(PathBuf::from).into(),
            || Ok(()),
            |path| {
                requested.push(path.to_path_buf());
                anyhow::ensure!(path != Path::new("vault/b.md"), "offline");
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            requested,
            ["vault/a.md", "vault/b.md", "vault/c.md"].map(PathBuf::from)
        );
        assert_eq!(
            result,
            BatchResult {
                requested: 2,
                failed: 1
            }
        );
    }

    #[test]
    fn bulk_cancellation_stops_remaining_requests_and_owner_drop_cancels() {
        let batch = Batch::default();
        let cancel = batch.cancel.clone();
        let mut count = 0;
        let result = request_all(
            vec!["a.md".into(), "b.md".into()],
            || cancel.check(),
            |_| {
                count += 1;
                cancel.cancel();
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(count, 1, "first request proves cancellation was exercised");
        let batch = Batch::default();
        let cancel = batch.cancel.clone();
        assert!(cancel.check().is_ok());
        drop(batch);
        assert!(cancel.check().is_err());
        let result = request_all(
            Vec::new(),
            || panic!("empty batch"),
            |_| panic!("empty request"),
        )
        .unwrap();
        assert_eq!(result, BatchResult::default());
    }

    #[gpui::test]
    fn bulk_download_requires_explicit_action_and_blocks_duplicate_submission(
        cx: &mut gpui::TestAppContext,
    ) {
        use super::super::*;
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(fixture.path().join("local.md"), "# Local").unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(fixture.path().into()),
                        cache_base_override: Some(fixture.path().join("cache")),
                        session_directory: Some(fixture.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        let count = Arc::new(AtomicUsize::new(0));
        let path = fixture.path().join(".pending.md.icloud");
        reader.update_in(visual, |r, _, cx| {
            Arc::make_mut(&mut r.vault)
                .cloud_placeholders
                .push(path.clone());
            cx.notify();
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(r.loading.as_ref().unwrap().cloud_download.is_none())
        });
        reader.update_in(visual, |r, _, cx| {
            let count = count.clone();
            let expected = logical_path(&path);
            r.download_all_icloud_using(cx, move |path| {
                assert_eq!(path, expected);
                count.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
            assert!(
                r.loading
                    .as_ref()
                    .unwrap()
                    .cloud_download
                    .as_ref()
                    .unwrap()
                    .active
            );
            r.download_all_icloud_using(cx, |_| panic!("duplicate batch"));
        });
        visual.run_until_parked();
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "positive control: the explicit action submits a request"
        );
        reader.read_with(visual, |r, _| {
            assert!(
                !r.loading
                    .as_ref()
                    .unwrap()
                    .cloud_download
                    .as_ref()
                    .unwrap()
                    .active
            );
            assert!(r.link_notice.as_ref().unwrap().contains("Keep Downloaded"));
            assert_eq!(
                r.vault.cloud_placeholders,
                vec![path],
                "submission is not materialization"
            );
        });
    }

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
    fn first_open_preserves_vault_note_intent_and_ignores_folder_only_selection() {
        let root = Path::new("vault");
        let mut open_path = None;
        prepare_explicit_open(&mut open_path, Some(root), Some("note.md"), |path| {
            assert_eq!(path, root.join("note.md"));
            Ok(path.to_path_buf())
        })
        .unwrap();
        assert!(open_path.is_none(), "local vault+note stays a vault intent");
        for (root, note) in [(Some(root), None), (Some(root), Some("")), (None, None)] {
            prepare_explicit_open(&mut open_path, root, note, |_| {
                panic!("no explicit document")
            })
            .unwrap();
            assert!(open_path.is_none());
        }
    }

    #[test]
    fn first_open_remaps_only_legacy_placeholder_names_and_retains_explicit_paths() {
        let mut open_path = Some(PathBuf::from("vault/.note.md.icloud"));
        prepare_explicit_open(&mut open_path, None, None, |path| Ok(logical_path(path))).unwrap();
        assert_eq!(open_path, Some(PathBuf::from("vault/note.md")));
        prepare_explicit_open(&mut open_path, None, None, |path| Ok(path.to_path_buf())).unwrap();
        assert_eq!(open_path, Some(PathBuf::from("vault/note.md")));
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
