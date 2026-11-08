//! Additive review/recovery inside layout A. A preview owns no mutation hold;
//! an applying worker remains owned when its modal closes.
use super::*;
use crate::reader_sync_merge::SyncMergePreview;
use okilum_sync_controller::{
    direct::inventory::merge::PendingMerge,
    merge_preview::{Backend, Plan},
};

pub(super) struct Prepared {
    pub plan: Plan,
    pub backend: Box<dyn Backend>,
    pub path: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{size, Modifiers, TestAppContext};
    use gpui_component::Root;
    use okilum_sync_controller::merge_preview::{Error, Snapshot};
    use std::{
        collections::BTreeMap,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    struct Writes(Arc<AtomicUsize>);
    impl Backend for Writes {
        fn apply_if_unchanged(
            &mut self,
            _: &Snapshot,
            _: &Snapshot,
            files: &BTreeMap<String, Vec<u8>>,
        ) -> Result<(), Error> {
            assert_eq!(
                files.len(),
                9,
                "the confirmed plan must reach the measured backend"
            );
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    fn prepared(writes: Arc<AtomicUsize>) -> Prepared {
        let snapshot = |scope: &str, only: &str, conflict: &[u8]| Snapshot {
            scope: scope.into(),
            revision: "revision".into(),
            files: [
                ("same.md".into(), b"same".to_vec()),
                ("different.md".into(), conflict.to_vec()),
            ]
            .into_iter()
            .chain((0..3).map(|index| (format!("{only}-{index}.md"), only.as_bytes().to_vec())))
            .collect(),
        };
        let plan = Plan::preview(
            snapshot("here", "here", b"local"),
            snapshot("there", "there", b"remote"),
        )
        .unwrap();
        assert!(plan.unrelated_warning());
        Prepared {
            plan,
            backend: Box::new(Writes(writes)),
            path: PathBuf::from("/isolated-first-sync"),
        }
    }
    #[gpui::test]
    fn issue2043_settings_preview_cancel_and_explicit_unrelated_consent(cx: &mut TestAppContext) {
        let directory = Arc::new(tempfile::tempdir().unwrap());
        let controller = Controller::open(directory.path().join("controller"), vec![]).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            reader_features::install(directory.path(), cx);
            reader_features::set_enabled(reader_features::Feature::Sync, true, cx);
            cx.set_global(TestWorker(Arc::new(move |_| {
                let _keep = &directory;
                Ok(super::super::Output {
                    first_sync: Default::default(),
                    overview: Default::default(),
                    connection_error: None,
                    controller: controller.clone(),
                    snapshot: controller.snapshot()?,
                    inspected: None,
                    selected_destination: None,
                    runtimes: vec![],
                    vaults: vec![],
                })
            })));
        });
        let writes = Arc::new(AtomicUsize::new(0));
        let (root, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = SyncSettings::idle();
                view.inputs(window, cx);
                view.publish_first_sync(
                    Output {
                        preview: Some(prepared(writes.clone())),
                        pending: vec![],
                    },
                    cx,
                );
                view
            });
            Root::new(view, window, cx)
        });
        let view = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<SyncSettings>().unwrap()
        });
        visual.simulate_resize(size(px(1000.), px(1400.)));
        visual.run_until_parked();
        for selector in [
            "sync-merge-same-count",
            "sync-merge-here-count",
            "sync-merge-there-count",
            "sync-merge-conflict-count",
        ] {
            assert!(
                visual.debug_bounds(selector).is_some(),
                "missing review category {selector}"
            );
        }
        let cancel = visual.debug_bounds("sync-cancel-first-preview").unwrap();
        visual.simulate_click(cancel.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            writes.load(Ordering::SeqCst),
            0,
            "closing a preview must never publish"
        );
        assert!(!view.read_with(visual, |view, _| view.has_first_sync_preview()));
        view.update(visual, |view, cx| {
            view.publish_first_sync(
                Output {
                    preview: Some(prepared(writes.clone())),
                    pending: vec![],
                },
                cx,
            )
        });
        visual.run_until_parked();
        let apply = visual.debug_bounds("sync-merge-apply").unwrap();
        visual.simulate_click(apply.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            writes.load(Ordering::SeqCst),
            0,
            "unrelated vaults require acknowledgement"
        );
        let acknowledge = visual.debug_bounds("sync-merge-ack").unwrap();
        visual.simulate_click(acknowledge.center(), Modifiers::default());
        visual.run_until_parked();
        let apply = visual.debug_bounds("sync-merge-apply").unwrap();
        visual.simulate_click(apply.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            writes.load(Ordering::SeqCst),
            1,
            "positive control must execute the real confirmation worker"
        );
        assert!(!view.read_with(visual, |view, _| view.has_first_sync_preview()));
    }
}

#[derive(Default)]
pub(super) struct Output {
    pub preview: Option<Prepared>,
    pub pending: Vec<PendingMerge>,
}
#[derive(Default)]
pub(super) struct State {
    pub view: Option<Entity<SyncMergePreview>>,
    changes: Option<Subscription>,
    pub pending: Vec<PendingMerge>,
    destination: Option<PathBuf>,
    observed: bool,
}
impl SyncSettings {
    pub(super) fn has_first_sync_preview(&self) -> bool {
        self.first_sync.view.is_some()
    }
    pub(super) fn first_sync_applying(&self, cx: &App) -> bool {
        self.first_sync
            .view
            .as_ref()
            .is_some_and(|view| view.read(cx).applying())
    }
    pub(super) fn start_destination(&mut self, operation: Operation, cx: &mut Context<Self>) {
        let operation = match operation {
            Operation::Attach(peer, vault, path) => Operation::ReviewDestination(peer, vault, path),
            operation => operation,
        };
        self.run(operation, cx);
    }
    pub(super) fn close_first_sync_preview(&mut self, cx: &mut Context<Self>) {
        if self.first_sync_applying(cx) {
            return;
        }
        if let Some(view) = self.first_sync.view.take() {
            view.update(cx, |view, cx| view.close(cx));
        }
        self.first_sync.changes = None;
        self.first_sync.destination = None;
        self.first_sync.observed = false;
    }
    pub(super) fn publish_first_sync(&mut self, output: Output, cx: &mut Context<Self>) {
        self.first_sync.pending = output.pending;
        let Some(Prepared {
            plan,
            backend,
            path,
        }) = output.preview
        else {
            return;
        };
        self.close_first_sync_preview(cx);
        let view = cx.new(|_| SyncMergePreview::new_tracked(plan, backend, Default::default()));
        self.first_sync.destination = Some(path);
        self.first_sync.observed = false;
        self.first_sync.changes = Some(cx.observe(&view, |this, view, cx| {
            let Some(succeeded) = view.read(cx).outcome() else {
                return;
            };
            if this.first_sync.observed {
                return;
            }
            this.first_sync.observed = true;
            if succeeded {
                this.awaiting_open = this.first_sync.destination.take();
                this.first_sync.view = None;
                if this.presentation.wizard.is_some() {
                    this.presentation.wizard = Some(presentation::Step::Progress);
                }
            }
            this.run(Operation::Read, cx);
        }));
        self.first_sync.view = Some(view);
        self.presentation.wizard = Some(presentation::Step::Merge);
    }
    pub(super) fn render_first_sync_recovery(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut body = v_flex().w_full().gap_3();
        for (index, pending) in self.first_sync.pending.iter().enumerate() {
            if pending.remote_side {
                let peer = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.peers.iter().find(|p| p.id == pending.peer))
                    .map(|p| p.name.as_str())
                    .unwrap_or("the other computer");
                body=body.child(format!("First Sync for {} is waiting for {peer}. Keep Okilum open and recover or finish the merge on that computer. Both originals are retained.",pending.path.display()));
                continue;
            }
            let operation = pending.operation;
            let path = pending.path.clone();
            body=body.child(v_flex().w_full().gap_2()
                .child(format!("First Sync needs attention: {}",pending.path.display()))
                .child("Both originals are retained. Recover the saved merge, or cancel if publication has not started. Keep both computers online.")
                .child(h_flex().gap_2()
                    .child(Button::new(("sync-first-recover",index)).label("Recover first sync")
                        .disabled(self.busy||self.first_sync_applying(cx))
                        .on_click(cx.listener(move|this,_,_,cx|{
                            this.close_first_sync_preview(cx);this.awaiting_open=Some(path.clone());
                            this.run(Operation::RecoverFirstSync(operation),cx);
                        })))
                    .child(Button::new(("sync-first-cancel",index)).ghost().label("Cancel before merge")
                        .disabled(self.busy||self.first_sync_applying(cx))
                        .on_click(cx.listener(move|this,_,_,cx|{
                            this.close_first_sync_preview(cx);this.presentation.transfers.clear();
                            this.run(Operation::CancelFirstSync(operation),cx);
                        })))));
        }
        body.into_any_element()
    }
}
