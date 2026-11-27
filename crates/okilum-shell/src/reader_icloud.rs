//! Reader glue and integration tests for iCloud hydration (#3234).
pub(crate) use okilum_shell_platform::reader_icloud::*;
use std::path::{Path, PathBuf};

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

#[cfg(test)]
mod tests {
    use super::*;

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
            crate::test_init(cx);
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
}
