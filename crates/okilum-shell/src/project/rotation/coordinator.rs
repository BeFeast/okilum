//! Native reconciliation adapter for frozen slot23 APIs ce1ed704d/5f7c3657e.
//! The configured policy surface retains this Entity and invalidates it before
//! hide/rebind/feature disable. Persistence uses its canonical identity lock.
use super::{
    panel::ReconciliationPanel, protocol::Execution, Action, Checkpoint, Owner, PendingReview,
    PolicyStore, Requested,
};
use crate::project::{
    drafts::{Barrier, ComposerSnapshot, DraftCheckpoint},
    *,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// The configured policy surface supplies its canonical rotation owner (including policy/lifecycle
/// revision). None revokes access. This callback is always read on the UI thread.
pub(in crate::project) type OwnerResolver = fn(&ProjectView, &App) -> Option<Owner>;
#[allow(dead_code, reason = "event payload; no subscriber reads it yet")]
#[derive(Clone)]
pub(in crate::project) struct Resolved {
    pub action: Action,
    pub checkpoint: Checkpoint,
}
impl EventEmitter<Resolved> for Coordinator {}

pub(in crate::project) struct Coordinator {
    view: WeakEntity<ProjectView>,
    authority: Arc<super::policy_owner::Store>,
    owner: OwnerResolver,
    flow: Execution,
    panel: Entity<ReconciliationPanel>,
    _subscription: Subscription,
    valid: Arc<AtomicBool>,
    resolved: bool,
}
impl Drop for Coordinator {
    fn drop(&mut self) {
        self.valid.store(false, Ordering::Release);
    }
}
impl Coordinator {
    pub(in crate::project) fn revocation_token(&self) -> Arc<AtomicBool> {
        self.valid.clone()
    }

    /// checkpoint was loaded by the owner off the UI thread. The native draft
    /// is captured here, not supplied by an asynchronous preview callback.
    pub fn mount(
        view: &Entity<ProjectView>,
        store: Arc<PolicyStore>,
        checkpoint: Checkpoint,
        exact_source: Option<String>,
        owner: OwnerResolver,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<Entity<Self>, String> {
        let (snapshot, authority) = view.update(cx, |view, cx| {
            live_owner(view, owner, cx)
                .filter(|live| live == &checkpoint.owner)
                .ok_or("Rotation target is no longer active")?;
            let authority = view
                .rotation_policy
                .authority_for(&checkpoint.owner, &store)?;
            Ok::<_, String>((view.composer_snapshot(cx), authority))
        })?;
        let review = PendingReview::open(checkpoint, exact_source, snapshot.clone())?;
        let panel = cx.new(|_| ReconciliationPanel::new(&review));
        let weak = view.downgrade();
        Ok(cx.new(|cx| {
            let subscription = cx.subscribe_in(
                &panel,
                window,
                |this: &mut Self, _, event: &Requested, window, cx| {
                    this.request(event.clone(), window, cx)
                },
            );
            Self {
                view: weak,
                authority,
                owner,
                flow: Execution::new(review, snapshot),
                panel,
                _subscription: subscription,
                valid: Arc::new(AtomicBool::new(true)),
                resolved: false,
            }
        }))
    }
    /// Required mount callback handoff: call before losing the owner surface,
    /// not just after an asynchronous completion reaches another project.
    pub fn invalidate(&mut self, cx: &mut Context<Self>) {
        self.valid.store(false, Ordering::Release);
        self.panel.update(cx, |panel, cx| panel.invalidate(cx));
        cx.notify();
    }
    fn fail(&mut self, error: String, cx: &mut Context<Self>) {
        self.valid.store(false, Ordering::Release);
        let expected = self.flow.review.checkpoint().clone();
        self.panel
            .update(cx, |panel, cx| panel.failed(&expected, error, cx));
        cx.notify();
    }
    fn current(&self, cx: &mut Context<Self>) -> Result<(Owner, ComposerSnapshot), String> {
        if !self.valid.load(Ordering::Acquire) || self.resolved {
            return Err("Rotation reconciliation is no longer active".into());
        }
        let view = self.view.upgrade().ok_or("Project view closed")?;
        view.update(cx, |view, cx| {
            let owner =
                live_owner(view, self.owner, cx).ok_or("Rotation target is no longer active")?;
            Ok((owner, view.composer_snapshot(cx)))
        })
    }
    fn request(&mut self, event: Requested, window: &mut Window, cx: &mut Context<Self>) {
        let started = self
            .current(cx)
            .and_then(|(owner, current)| self.flow.start(&event, &owner, &current));
        if let Err(error) = started {
            self.fail(error, cx);
            return;
        }
        let authority = self.authority.clone();
        let valid = self.valid.clone();
        let expected = event.expected.clone();
        cx.spawn_in(window, async move |this, cx| {
            // Every disk boundary compares the entire captured record, not only
            // its revision. A reused directory cannot authorize another target.
            let disk_expected = expected.clone();
            let disk_authority = authority.clone();
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    disk_authority.with_current(&disk_expected.owner, |policy| {
                        load_exact(policy, &disk_expected)
                    })
                })
                .await;
            let outcome: Result<Checkpoint, String> = async {
                let loaded = loaded?;
                let next = if event.action == Action::Resume {
                    let (receipt, barrier) = this
                        .update_in(cx, |this, window, cx| this.apply(&loaded, window, cx))
                        .map_err(|e| e.to_string())??;
                    match barrier.await {
                        Ok(Ok(())) => (),
                        Ok(Err(error)) => {
                            return Err(format!(
                                "Draft could not be saved: {error}. Claim preserved."
                            ))
                        }
                        Err(_) => return Err("Draft writer stopped; claim preserved".into()),
                    }
                    let disk_expected = expected.clone();
                    let disk_authority = authority.clone();
                    let fresh = cx
                        .background_executor()
                        .spawn(async move {
                            disk_authority.with_current(&disk_expected.owner, |policy| {
                                load_exact(policy, &disk_expected)
                            })
                        })
                        .await?;
                    this.update_in(cx, |this, _, cx| this.after_barrier(&fresh, &receipt, cx))
                        .map_err(|e| e.to_string())??
                } else {
                    this.update_in(cx, |this, _, cx| {
                        let (owner, snapshot) = this.current(cx)?;
                        this.flow
                            .resolve(&loaded, &owner, &snapshot, event.action, unix_now()?)
                    })
                    .map_err(|e| e.to_string())??
                };
                let disk_expected = expected.clone();
                cx.background_executor()
                    .spawn(async move {
                        if !valid.load(Ordering::Acquire) {
                            return Err("Rotation target invalidated before acknowledgement".into());
                        }
                        authority.with_current(&disk_expected.owner, |policy| {
                            load_exact(policy, &disk_expected)?;
                            if !valid.load(Ordering::Acquire) {
                                return Err(
                                    "Rotation target invalidated before acknowledgement".into()
                                );
                            }
                            policy.save(disk_expected.revision, next)
                        })
                    })
                    .await
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                match outcome {
                    Ok(checkpoint) if this.valid.load(Ordering::Acquire) => {
                        this.resolved = true;
                        cx.emit(Resolved {
                            action: event.action,
                            checkpoint,
                        });
                        cx.notify();
                    }
                    Ok(_) => (), // old UI completion cannot touch a replacement
                    Err(error) => this.fail(error, cx),
                }
            });
        })
        .detach();
    }
    fn apply(
        &mut self,
        loaded: &Checkpoint,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(DraftCheckpoint, Barrier), String> {
        let (owner, current) = self.current(cx)?;
        let request = self.flow.resume(loaded, &owner, &current)?;
        let view = self.view.upgrade().ok_or("Project view closed")?;
        let owner_resolver = self.owner;
        view.update(cx, |view, cx| {
            // Compare, replace and capture the barrier without an async yield.
            let owner = live_owner(view, owner_resolver, cx)
                .ok_or("Rotation target changed before draft application")?;
            let before = view.composer_snapshot(cx);
            request.check(&owner, &before)?;
            if before.draft != request.replacement {
                view.composer.update(cx, |composer, cx| {
                    composer.replace_typed_draft(request.replacement.text.clone(), None, window, cx)
                })?;
            }
            view.draft_changed(window, cx);
            self.flow
                .applied(&request, &owner, view.composer_snapshot(cx))?;
            view.draft_durable_barrier(cx)
        })
    }
    fn after_barrier(
        &mut self,
        fresh: &Checkpoint,
        receipt: &DraftCheckpoint,
        cx: &mut Context<Self>,
    ) -> Result<Checkpoint, String> {
        let (owner, current) = self.current(cx)?;
        let view = self.view.upgrade().ok_or("Project view closed")?;
        let matches = view.update(cx, |view, cx| view.draft_checkpoint_matches(receipt, cx));
        self.flow.persisted(fresh, &owner, &current, true, matches)
    }
}
impl Render for Coordinator {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().when(!self.resolved, |panel| panel.child(self.panel.clone()))
    }
}
fn live_owner(view: &ProjectView, resolve: OwnerResolver, cx: &App) -> Option<Owner> {
    if !view.active || view.executors_open || !view.draft_ready() {
        return None;
    }
    let owner = resolve(view, cx)?;
    let binding = view.binding.as_ref()?;
    if view.environment.as_ref()?.origin() != owner.origin
        || binding.manager.project_id.as_ref()? != &owner.project
        || binding.manager.thread_id.as_ref()? != &owner.manager
        || view.chat.as_ref()?.thread_id != owner.manager
    {
        return None;
    }
    Some(owner)
}
fn load_exact(store: &PolicyStore, expected: &Checkpoint) -> Result<Checkpoint, String> {
    let loaded = store.load()?.ok_or("Rotation claim disappeared")?;
    if &loaded != expected {
        return Err("Rotation claim changed; reopen reconciliation".into());
    }
    Ok(loaded)
}
fn unix_now() -> Result<i64, String> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?;
    i64::try_from(elapsed.as_secs()).map_err(|_| "Clock is outside supported range".into())
}

#[cfg(test)]
#[path = "coordinator_tests.rs"]
mod tests;
