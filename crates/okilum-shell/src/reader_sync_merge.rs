//! First-sync preview composition. Parent supplies authenticated, complete
//! snapshots and a serialized recovery-capable adapter. Recovery review IO runs
//! on the background executor; neither constructor authorizes a canonical write.
#![allow(
    dead_code,
    reason = "first-sync preview owner not wired into Reader yet"
)]
use gpui::{prelude::*, *};
use gpui_component::{button::Button, h_flex, v_flex, Disableable};
use okilum_sync_controller::merge_preview::{
    completion::{Returned, Work},
    Backend, Error, Kind, Plan,
};

pub(crate) struct SyncMergePreview {
    plan: Option<Returned<std::sync::Arc<Plan>>>,
    backend: Option<Returned<Box<dyn Backend>>>,
    completion: Work,
    acknowledged: bool,
    page: usize,
    busy: bool,
    finished: bool,
    stopped: bool,
    message: Option<&'static str>,
    #[cfg(unix)]
    recovery: Option<RecoveryFlow>,
}
impl SyncMergePreview {
    pub(crate) fn outcome(&self) -> Option<bool> {
        if self.finished {
            Some(true)
        } else if self.stopped && !self.busy {
            Some(false)
        } else {
            None
        }
    }
    pub(crate) fn applying(&self) -> bool {
        self.busy
    }
    /// Legacy factories remain opaque until their owner adopts the shared lease.
    pub(crate) fn new(plan: Plan, backend: Box<dyn Backend>) -> Self {
        Self::new_tracked(plan, backend, Work::unknown())
    }
    /// The factory's queued/output lease must cover the incoming backend until
    /// this constructor reserves its idle ownership. Use the mount's SAME Work.
    pub(crate) fn new_tracked(plan: Plan, backend: Box<dyn Backend>, completion: Work) -> Self {
        let plan = completion
            .reserve()
            .map(|lease| lease.owned(std::sync::Arc::new(plan)));
        let backend = completion.reserve().map(|lease| lease.owned(backend));
        let stopped = backend.is_none();
        Self {
            plan,
            backend,
            completion,
            acknowledged: false,
            page: 0,
            busy: false,
            finished: false,
            stopped,
            message: None,
            #[cfg(unix)]
            recovery: None,
        }
    }
    pub(crate) fn completion(&self) -> Work {
        self.completion.clone()
    }
    /// Close admission first, then release idle resources. Queued jobs, futures
    /// and returned outputs keep independent leases until their actual cleanup.
    /// This is cancellation, NOT a whole-completion or readback acknowledgement.
    pub(crate) fn close(&mut self, cx: &mut Context<Self>) {
        self.completion.close();
        self.cleanup_idle();
        self.stopped = true;
        self.busy = self.completion.snapshot().pending != 0;
        cx.notify();
    }
    fn cleanup_idle(&mut self) {
        drop(self.plan.take());
        #[cfg(unix)]
        if let Some(flow) = self.recovery.as_mut() {
            drop(flow.preview.take());
            // Retained display-only metadata owns no session, file or task.
            drop(flow.session.take());
        }
        drop(self.backend.take());
    }
    fn apply(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.finished || self.stopped || self.completion.snapshot().closed {
            return;
        }
        let Some(plan) = self.plan.as_ref().map(Returned::as_ref) else {
            return;
        };
        let Ok(consent) = plan.consent(self.acknowledged) else {
            return;
        };
        // Reserve BEFORE taking the idle backend or enqueueing either future.
        let Some(reservation) = self.completion.reserve() else {
            return;
        };
        let Some(backend) = self.backend.take() else {
            return;
        };
        let plan = plan.clone();
        let work = self.completion.clone();
        let queued = backend.into_queued(reservation, |backend| (plan, consent, backend, work));
        self.busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            queued.join(|(plan, consent, backend, work)| async move {
                let Some(reservation) = work.reserve() else { return; };
                let worker = work.clone();
                let background = reservation.queued((plan, consent, backend, worker));
                let result = cx.background_executor().spawn(background.job(|(plan, consent, mut backend, worker)| async move {
                    if worker.snapshot().closed { return Err(Error::Backend("Merge admission closed".into())); }
                    plan.apply(consent, backend.as_mut())
                })).await;
                result.consume(|result| {
                    let _ = this.update(cx, |this, cx| {
                        if work.snapshot().closed { return; }
                        this.busy = false;
                        this.finished = result.is_ok();
                        this.stopped = result.is_err();
                        this.message = Some(match result {
                            Ok(()) => "Both copies now contain all files. Conflict versions are kept separately.",
                            Err(Error::Stale) => "Files or connection changed. Refresh the preview before continuing.",
                            Err(_) => "Merge stopped. Originals must be recovered before a new preview. Sync has not been enabled.",
                        });
                        cx.notify();
                    });
                });
            }).await;
        }).detach();
    }
}
impl Render for SyncMergePreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(unix)]
        if self.recovery.is_some() {
            return self.render_recovery(cx);
        }
        let Some(plan) = self.plan.as_ref().map(Returned::as_ref) else {
            return div().into_any_element();
        };
        let mut content = v_flex()
            .gap_2()
            .child("Review first Sync")
            .child("All files are kept. Different versions are stored separately for resolution.")
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .debug_selector(|| "sync-merge-same-count".into())
                            .child(format!("Same: {}", plan.count(Kind::Equal))),
                    )
                    .child(
                        div()
                            .debug_selector(|| "sync-merge-here-count".into())
                            .child(format!("New here: {}", plan.count(Kind::OnlyHere))),
                    )
                    .child(
                        div()
                            .debug_selector(|| "sync-merge-there-count".into())
                            .child(format!("New there: {}", plan.count(Kind::OnlyThere))),
                    )
                    .child(
                        div()
                            .debug_selector(|| "sync-merge-conflict-count".into())
                            .child(format!("Conflicting: {}", plan.count(Kind::Different))),
                    ),
            );
        if plan.unrelated_warning() {
            content = content
                .child("Fewer than half of paths overlap. These may be unrelated vaults.")
                .child(
                    Button::new("sync-merge-ack")
                        .debug_selector(|| "sync-merge-ack".into())
                        .label(if self.acknowledged {
                            "Different vaults acknowledged"
                        } else {
                            "I understand these may be different vaults"
                        })
                        .disabled(self.busy || self.finished || self.stopped)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.acknowledged = !this.acknowledged;
                            cx.notify();
                        })),
                );
        }
        // Every category is inspectable, with bounded rendering even for a
        // large inventory. Pagination does not change the revision-bound plan.
        let page_size = 50;
        let pages = plan.entries().len().div_ceil(page_size);
        let page = self.page.min(pages.saturating_sub(1));
        for entry in plan.entries().iter().skip(page * page_size).take(page_size) {
            let kind = match entry.kind {
                Kind::Equal => "Same",
                Kind::OnlyHere => "New here",
                Kind::OnlyThere => "New there",
                Kind::Different => "Conflicting",
            };
            content = content.child(match &entry.conflict_path {
                Some(conflict) => format!("{kind}: {} → {conflict}", entry.path),
                None => format!("{kind}: {}", entry.path),
            });
        }
        content = content.child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("sync-merge-previous")
                        .label("Previous files")
                        .disabled(page == 0)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.page = this.page.saturating_sub(1);
                            cx.notify();
                        })),
                )
                .child(format!("Page {} of {pages}", page + 1))
                .child(
                    Button::new("sync-merge-next")
                        .label("Next files")
                        .disabled(page + 1 >= pages)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.page = (this.page + 1).min(pages.saturating_sub(1));
                            cx.notify();
                        })),
                ),
        );
        content = content.child(
            Button::new("sync-merge-apply")
                .debug_selector(|| "sync-merge-apply".into())
                .label(if self.busy {
                    "Merging…"
                } else {
                    "Merge and keep both versions"
                })
                .disabled(
                    self.busy
                        || self.finished
                        || self.stopped
                        || (plan.unrelated_warning() && !self.acknowledged),
                )
                .on_click(cx.listener(|this, _, _, cx| this.apply(cx))),
        );
        if let Some(message) = self.message {
            content = content.child(message);
        }
        content.into_any_element()
    }
}

#[cfg(unix)]
use okilum_sync_controller::merge_preview::filesystem::{FilesystemSession, RecoveryPreview};
#[cfg(unix)]
use std::{path::PathBuf, sync::Arc};

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum RecoveryPhase {
    Loading,
    Review,
    Recovering,
    Recovered,
    Failed,
    Cancelled,
}
#[cfg(unix)]
struct RecoveryDetails {
    operation_identity: (u64, u64),
    digest: String,
    counts: [usize; 2],
    bytes: [usize; 2],
    remaining: [usize; 2],
    revisions: [String; 2],
    conflicts: usize,
    conflict_paths: Vec<String>,
}
#[cfg(unix)]
impl RecoveryDetails {
    /// Hashing and inventory summaries belong on the background executor too.
    fn of(preview: &RecoveryPreview) -> Self {
        Self {
            operation_identity: preview.operation_identity(),
            digest: preview.originals_digest().into(),
            counts: preview.original_counts(),
            bytes: preview.original_bytes(),
            remaining: preview.remaining_counts(),
            revisions: preview.revision_fingerprints(),
            conflicts: preview.plan().count(Kind::Different),
            conflict_paths: preview
                .plan()
                .entries()
                .iter()
                .filter_map(|entry| {
                    entry
                        .conflict_path
                        .as_ref()
                        .map(|target| format!("{} → {}", entry.path, target))
                })
                .take(50)
                .collect(),
        }
    }
}
#[cfg(unix)]
struct RecoveryFlow {
    operation: PathBuf,
    originals: PathBuf,
    roots: [PathBuf; 2],
    phase: RecoveryPhase,
    session: Option<Returned<FilesystemSession>>,
    preview: Option<Returned<Arc<RecoveryPreview>>>,
    details: Option<RecoveryDetails>,
}

#[cfg(unix)]
impl SyncMergePreview {
    /// Compose the result of background inspection without re-opening roots or
    /// losing the session's mutation guard. The parent's queued/output lease
    /// must cover `prepared` until this constructor reserves its idle ownership.
    /// A live direct controller cannot supply an offline session safely.
    pub(crate) fn prepared_tracked(
        prepared: okilum_sync_controller::merge_preview::filesystem::PreparedSession,
        completion: Work,
        cx: &mut Context<Self>,
    ) -> Self {
        use okilum_sync_controller::merge_preview::{filesystem::PreparedSession, GuardedBackend};
        match prepared {
            PreparedSession::Fresh { plan, session } => {
                Self::new_tracked(*plan, Box::new(GuardedBackend(session)), completion)
            }
            PreparedSession::Recovery(session) => {
                Self::pending_recovery_tracked(session, completion, cx)
            }
        }
    }
    /// Settings mount: `cx.new(|cx| SyncMergePreview::pending_recovery(session, cx))`.
    /// Parent supplies an already-authorized OFFLINE session and preserves its
    /// quiescence until background work returns, even if this entity is closed. No discovery, stop/start, selection
    /// change or live Sync is performed here. The local gate excludes only its
    /// cooperating users; global exclusion still requires every writer to share it.
    /// Construction schedules a read-only review, never recovery or fresh apply.
    pub(crate) fn pending_recovery(session: FilesystemSession, cx: &mut Context<Self>) -> Self {
        Self::pending_recovery_tracked(session, Work::unknown(), cx)
    }
    /// The incoming session is covered by the factory's lease until we reserve
    /// the constructor preview BEFORE queueing. Retain the mount's shared Work.
    pub(crate) fn pending_recovery_tracked(
        session: FilesystemSession,
        completion: Work,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut flow = RecoveryFlow {
            operation: session.operation_directory().to_owned(),
            originals: session.originals_path(),
            roots: session.roots().map(PathBuf::from),
            phase: RecoveryPhase::Loading,
            session: None,
            preview: None,
            details: None,
        };
        let scheduled = Self::load_recovery_preview(session, completion.clone(), cx);
        if !scheduled {
            flow.phase = RecoveryPhase::Cancelled;
        }
        Self {
            plan: None,
            backend: None,
            completion,
            acknowledged: false,
            page: 0,
            busy: scheduled,
            finished: false,
            stopped: !scheduled,
            message: None,
            recovery: Some(flow),
        }
    }
    fn load_recovery_preview(
        session: FilesystemSession,
        work: Work,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(reservation) = work.reserve() else {
            return false;
        };
        let queued = reservation.queued((session, work));
        Self::queue_recovery_preview(queued, cx);
        true
    }
    fn queue_recovery_preview(
        queued: okilum_sync_controller::merge_preview::completion::Queued<(
            FilesystemSession,
            Work,
        )>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            queued
                .join(|(session, work)| async move {
                    let Some(reservation) = work.reserve() else {
                        return;
                    };
                    let worker = work.clone();
                    let background = reservation.queued((session, worker));
                    let result = cx
                        .background_executor()
                        .spawn(background.job(|(mut session, worker)| async move {
                            let result = if worker.snapshot().closed {
                                Err(Error::Backend("Recovery preview admission closed".into()))
                            } else {
                                session.preview_recovery().map(|preview| {
                                    let details = RecoveryDetails::of(&preview);
                                    (Arc::new(preview), details)
                                })
                            };
                            (session, result)
                        }))
                        .await;
                    result.consume(|(session, result)| {
                        let _ = this.update(cx, |this, cx| {
                            let Some(flow) = this.recovery.as_mut() else {
                                return;
                            };
                            if work.snapshot().closed || flow.phase != RecoveryPhase::Loading {
                                return;
                            }
                            // Overlap worker output and the new idle ownership lease.
                            let Some(idle) = work.reserve() else {
                                return;
                            };
                            flow.session = Some(idle.owned(session));
                            this.busy = false;
                            match result {
                                Ok((preview, details)) => {
                                    let Some(preview_owner) = work.reserve() else {
                                        return;
                                    };
                                    flow.preview = Some(preview_owner.owned(preview));
                                    flow.details = Some(details);
                                    flow.phase = RecoveryPhase::Review;
                                }
                                Err(error) => {
                                    flow.phase = RecoveryPhase::Failed;
                                    this.stopped = true;
                                    this.message = Some(recovery_message(&error));
                                }
                            }
                            cx.notify();
                        });
                    });
                })
                .await;
        })
        .detach();
    }
    fn recover(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.recovery.as_mut() else {
            return;
        };
        if flow.phase != RecoveryPhase::Review || self.completion.snapshot().closed {
            return;
        }
        let Some(preview) = flow.preview.as_ref().map(|owner| owner.as_ref().clone()) else {
            return;
        };
        let Some(reservation) = self.completion.reserve() else {
            return;
        };
        let Some(session) = flow.session.take() else {
            return;
        };
        let work = self.completion.clone();
        let queued = session.into_queued(reservation, |session| (session, preview, work));
        flow.phase = RecoveryPhase::Recovering;
        self.busy = true;
        self.message = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            queued.join(|(session, preview, work)| async move {
                let Some(reservation) = work.reserve() else { return; };
                let worker = work.clone();
                let background = reservation.queued((session, preview, worker));
                let result = cx.background_executor().spawn(background.job(|(mut session, preview, worker)| async move {
                    let result = if worker.snapshot().closed {
                        Err(Error::Backend("Recovery admission closed".into()))
                    } else { session.recover_previewed(&preview) };
                    (session, result)
                })).await;
                result.consume(|(session, result)| {
                    let _ = this.update(cx, |this, cx| {
                        if work.snapshot().closed { return; }
                        let Some(flow) = this.recovery.as_mut() else { return; };
                        this.busy = false; this.finished = result.is_ok(); this.stopped = result.is_err();
                        match result {
                            Ok(()) => {
                                flow.phase = RecoveryPhase::Recovered;
                                this.message = Some("Recovery complete. Both offline copies contain the planned files. Originals are retained at the location above. Sync has not been enabled.");
                                // Result lease remains until this session is dropped.
                                drop(session);
                            }
                            Err(error) => {
                                let Some(idle) = work.reserve() else { return; };
                                flow.session = Some(idle.owned(session));
                                flow.phase = RecoveryPhase::Failed;
                                this.message = Some(recovery_message(&error));
                            }
                        }
                        cx.notify();
                    });
                });
            }).await;
        }).detach();
    }
    fn cancel_recovery(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.recovery.as_mut() else {
            return;
        };
        if !matches!(
            flow.phase,
            RecoveryPhase::Loading | RecoveryPhase::Review | RecoveryPhase::Failed
        ) {
            return;
        }
        flow.phase = RecoveryPhase::Cancelled;
        self.close(cx);
        self.message = Some("Recovery cancelled. Admission is closed; queued or admitted work and cleanup may still be running. Originals are retained. Already completed writes are not rolled back.");
        cx.notify();
    }
    fn refresh_recovery(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.recovery.as_mut() else {
            return;
        };
        if flow.phase != RecoveryPhase::Failed || self.completion.snapshot().closed {
            return;
        }
        let Some(reservation) = self.completion.reserve() else {
            return;
        };
        let Some(session) = flow.session.take() else {
            return;
        };
        let work = self.completion.clone();
        let queued = session.into_queued(reservation, |session| (session, work));
        flow.phase = RecoveryPhase::Loading;
        flow.preview = None;
        flow.details = None;
        self.busy = true;
        self.stopped = false;
        self.message = None;
        Self::queue_recovery_preview(queued, cx);
        cx.notify();
    }
    fn render_recovery(&self, cx: &mut Context<Self>) -> AnyElement {
        let flow = self.recovery.as_ref().expect("recovery flow");
        let mut content = v_flex().gap_2()
            .child("Interrupted first Sync")
            .child(format!("Operation: {:?}", flow.operation))
            .child(format!("Retained originals: {:?}", flow.originals))
            .child(format!("Here: {:?}", flow.roots[0]))
            .child(format!("There: {:?}", flow.roots[1]))
            .child("Recover finishes the original merge and keeps both conflict versions. Cancel retains the pending operation and originals.")
            .child("Both copies must remain offline. The local gate covers cooperating writers only; other writers must stay stopped.");
        if flow.phase == RecoveryPhase::Loading {
            content = content.child("Reading the pending operation and checking both copies…");
        }
        if let Some(details) = &flow.details {
            content = content
                .child(format!(
                    "Operation generation: {}:{}",
                    details.operation_identity.0, details.operation_identity.1
                ))
                .child(format!("Originals SHA-256: {}", details.digest))
                .child(format!(
                    "Originals here: {} files, {} bytes; there: {} files, {} bytes",
                    details.counts[0], details.bytes[0], details.counts[1], details.bytes[1]
                ))
                .child(format!(
                    "Preview revisions — here: {}; there: {}",
                    details.revisions[0], details.revisions[1]
                ))
                .child(format!(
                    "This preview: {} files remaining here, {} there; {} conflicts retained",
                    details.remaining[0], details.remaining[1], details.conflicts
                ));
            for path in &details.conflict_paths {
                content = content.child(path.clone());
            }
            if details.conflicts > 50 {
                content = content
                    .child("Showing the first 50 conflict paths; counts include every file.");
            }
        }
        content = content.child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("sync-merge-recover")
                        .label(if flow.phase == RecoveryPhase::Recovering {
                            "Recovering…"
                        } else {
                            "Recover"
                        })
                        .disabled(flow.phase != RecoveryPhase::Review)
                        .on_click(cx.listener(|this, _, _, cx| this.recover(cx))),
                )
                .child(
                    Button::new("sync-merge-cancel-recovery")
                        .label("Cancel")
                        .disabled(!matches!(
                            flow.phase,
                            RecoveryPhase::Loading | RecoveryPhase::Review | RecoveryPhase::Failed
                        ))
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_recovery(cx))),
                ),
        );
        if flow.phase == RecoveryPhase::Failed {
            content = content.child(
                Button::new("sync-merge-refresh-recovery")
                    .label("Preview again")
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_recovery(cx))),
            );
        }
        if let Some(message) = self.message {
            content = content.child(message);
        }
        content.into_any_element()
    }
}
#[cfg(unix)]
fn recovery_message(error: &Error) -> &'static str {
    match error {
        Error::Stale => "Files, revisions or the operation changed. Recovery refused this preview. Preview again before confirming. Originals are retained at the location above.",
        _ => "Recovery stopped: the pending operation or files could not be validated or written. Keep the originals at the location above for recovery. Sync has not been enabled.",
    }
}

impl Drop for SyncMergePreview {
    fn drop(&mut self) {
        // Closing an entity does not cancel detached workers or returned outputs.
        self.completion.close();
        self.cleanup_idle();
        // Display metadata is not a filesystem or background-work resource.
    }
}
