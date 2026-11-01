//! One bounded selection pass followed by optional event-driven invalidation.
use okilum_core::log::virtual_log::{
    watcher::{Change, VirtualLogWatcher},
    Definition, Selection,
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

pub(super) enum Frame {
    Progress {
        checked: usize,
        bytes: u64,
    },
    Ready {
        selection: Selection,
        elapsed: Duration,
        watching: bool,
    },
    Invalidated(String, bool),
    Failed(String),
    Stopped,
}
pub(super) struct Worker {
    cancel: Arc<AtomicBool>,
    wake: mpsc::SyncSender<()>,
}
impl Worker {
    pub fn start(
        vault: PathBuf,
        path: PathBuf,
        definition: Definition,
        watch: bool,
    ) -> (Self, async_channel::Receiver<Frame>) {
        let cancel = Arc::new(AtomicBool::new(false));
        let stopped = cancel.clone();
        let (wake, notifications) = mpsc::sync_channel(1);
        let on_change = wake.clone();
        let (sender, receiver) = async_channel::bounded(2);
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<()> {
                if stopped.load(Ordering::Acquire) {
                    let _ = sender.send_blocking(Frame::Stopped);
                    return Ok(());
                }
                let now = chrono::Utc::now().timestamp_nanos_opt().ok_or_else(|| {
                    anyhow::anyhow!("Current timestamp is outside the supported range")
                })?;
                let plan = definition.plan(&vault, &path, now)?;
                let watcher = if watch {
                    Some(VirtualLogWatcher::start(plan.root(), move || {
                        let _ = on_change.try_send(());
                    })?)
                } else {
                    None
                };
                let started = Instant::now();
                let selection = plan.select(|checked, bytes| {
                    let _ = sender.try_send(Frame::Progress { checked, bytes });
                });
                // The existing backend has a bounded pass, not a cancellable I/O
                // API. Never publish a late pass after Stop or a new generation.
                if stopped.load(Ordering::Acquire) {
                    let _ = sender.send_blocking(Frame::Stopped);
                    return Ok(());
                }
                if sender
                    .send_blocking(Frame::Ready {
                        selection,
                        elapsed: started.elapsed(),
                        watching: watch,
                    })
                    .is_err()
                {
                    return Ok(());
                }
                if let Some(watcher) = watcher {
                    loop {
                        if stopped.load(Ordering::Acquire) {
                            let _ = sender.send_blocking(Frame::Stopped);
                            break;
                        }
                        let changes = watcher.drain(); // Includes events during enumeration/selection.
                        if let Some((message, error)) = invalidation(&changes) {
                            let _ = sender.send_blocking(Frame::Invalidated(message, error));
                            break;
                        }
                        if notifications.recv().is_err() {
                            break;
                        }
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                let _ = sender.send_blocking(Frame::Failed(error.to_string()));
            }
        });
        (Self { cancel, wake }, receiver)
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
        let _ = self.wake.try_send(());
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn invalidation(changes: &[Change]) -> Option<(String, bool)> {
    if let Some(error) = changes.iter().find_map(|change| match change {
        Change::Incomplete(error) => Some(error),
        _ => None,
    }) {
        return Some((format!("Watch coverage is incomplete: {error}. Loaded snapshot retained; Rescan explicitly."), true));
    }
    if changes
        .iter()
        .any(|change| matches!(change, Change::Removed(_)))
    {
        return Some((
            "Source files were removed. Loaded snapshot retained; Rescan to update membership."
                .into(),
            false,
        ));
    }
    (!changes.is_empty()).then(|| ("Source files changed. Loaded snapshot retained; Rescan to update membership and records.".into(), false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn removals_and_watch_loss_require_explicit_rescan_without_claiming_current_results() {
        assert!(invalidation(&[]).is_none());
        assert!(invalidation(&[Change::Changed(vec!["new.log".into()])])
            .unwrap()
            .0
            .contains("changed"));
        assert!(invalidation(&[Change::Removed(vec!["old.log".into()])])
            .unwrap()
            .0
            .contains("snapshot retained"));
        let (message, error) = invalidation(&[
            Change::Removed(vec![]),
            Change::Incomplete("overflow fixture".into()),
        ])
        .unwrap();
        assert!(error && message.contains("overflow fixture"));
    }
}
