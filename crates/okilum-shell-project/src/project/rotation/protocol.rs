//! The coordinator's pure transition guards; no GPUI, disk, timers or dispatch.
use super::{Action, Checkpoint, ComposerSnapshot, Owner, PendingReview, Requested, ResumeDraft};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Review,
    Validating,
    Applying,
    WaitingForBarrier,
    Resolving,
}
pub(super) struct Execution {
    pub review: PendingReview,
    phase: Phase,
    baseline: ComposerSnapshot,
}
impl Execution {
    pub fn new(review: PendingReview, baseline: ComposerSnapshot) -> Self {
        Self {
            review,
            phase: Phase::Review,
            baseline,
        }
    }
    pub fn start(
        &mut self,
        event: &Requested,
        owner: &Owner,
        current: &ComposerSnapshot,
    ) -> Result<(), String> {
        if self.phase != Phase::Review {
            return Err("Reconciliation is already running; reopen after a failure".into());
        }
        self.review.validate_loaded(&event.expected, owner)?;
        self.check_baseline(current)?;
        self.phase = Phase::Validating;
        Ok(())
    }
    fn check_baseline(&self, current: &ComposerSnapshot) -> Result<(), String> {
        if current != &self.baseline {
            return Err("Composer changed; authored draft preserved".into());
        }
        Ok(())
    }
    pub fn resume(
        &mut self,
        loaded: &Checkpoint,
        owner: &Owner,
        current: &ComposerSnapshot,
    ) -> Result<ResumeDraft, String> {
        if self.phase != Phase::Validating {
            return Err("Unexpected rotation resume".into());
        }
        self.check_baseline(current)?;
        let request = self.review.resume_loaded(loaded, owner, current)?;
        self.phase = Phase::Applying;
        Ok(request)
    }
    pub fn applied(
        &mut self,
        request: &ResumeDraft,
        owner: &Owner,
        current: ComposerSnapshot,
    ) -> Result<(), String> {
        if self.phase != Phase::Applying {
            return Err("Unexpected rotation draft application".into());
        }
        self.review.record_applied(request, owner, current)?;
        self.phase = Phase::WaitingForBarrier;
        Ok(())
    }
    pub fn persisted(
        &mut self,
        loaded: &Checkpoint,
        owner: &Owner,
        current: &ComposerSnapshot,
        barrier_succeeded: bool,
        receipt_matches: bool,
    ) -> Result<Checkpoint, String> {
        if self.phase != Phase::WaitingForBarrier || !barrier_succeeded || !receipt_matches {
            return Err("Durable draft barrier failed or became stale; claim preserved".into());
        }
        let next = self.review.acknowledgement_loaded(loaded, owner, current)?;
        self.phase = Phase::Resolving;
        Ok(next)
    }
    pub fn resolve(
        &mut self,
        loaded: &Checkpoint,
        owner: &Owner,
        current: &ComposerSnapshot,
        action: Action,
        now: i64,
    ) -> Result<Checkpoint, String> {
        if self.phase != Phase::Validating {
            return Err("Unexpected claim resolution".into());
        }
        self.check_baseline(current)?;
        let through = match action {
            Action::Defer => Some(now),
            Action::Cancel => None,
            Action::Resume => return Err("Resume requires a durable draft barrier".into()),
        };
        let next = self.review.resolve_loaded(loaded, owner, through)?;
        self.phase = Phase::Resolving;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use okilum_t3::{
        chat_drafts::Draft,
        rotation::{Policy, Slot},
    };
    fn fixture() -> (Execution, Requested, Owner, ComposerSnapshot) {
        let owner = Owner {
            origin: "fixture".into(),
            project: "p".into(),
            manager: "m".into(),
            revision: 7,
        };
        let checkpoint = Checkpoint {
            revision: 2,
            owner: owner.clone(),
            policy: Policy::new(true, "Asia/Jerusalem", ["08:30", "20:30"]).unwrap(),
            through: 100,
            pending: Some(Slot {
                day: 0,
                minute: 510,
                utc: Some(99),
            }),
        };
        let current = ComposerSnapshot {
            draft: Draft::default(),
            edit_revision: 10,
            has_authored_metadata: false,
            typed_metadata: None,
        };
        let review = PendingReview::open(
            checkpoint.clone(),
            Some("exact rotation source\n".into()),
            current.clone(),
        )
        .unwrap();
        (
            Execution::new(review, current.clone()),
            Requested {
                action: Action::Resume,
                expected: checkpoint,
            },
            owner,
            current,
        )
    }
    #[test]
    fn actual_flow_needs_barrier_full_snapshot_and_exact_claim_before_ack_plan() {
        let (mut flow, event, owner, mut current) = fixture();
        flow.start(&event, &owner, &current).unwrap();
        assert!(flow.start(&event, &owner, &current).is_err());
        assert!(flow
            .resolve(&event.expected, &owner, &current, Action::Resume, 100)
            .is_err());
        let request = flow.resume(&event.expected, &owner, &current).unwrap();
        request.check(&owner, &current).unwrap();
        current.draft = request.replacement.clone();
        current.edit_revision += 1;
        flow.applied(&request, &owner, current.clone()).unwrap();
        assert!(flow
            .persisted(&event.expected, &owner, &current, false, true)
            .is_err());
        assert!(flow
            .persisted(&event.expected, &owner, &current, true, false)
            .is_err());
        let mut changed = current.clone();
        changed.edit_revision += 1;
        assert!(flow
            .persisted(&event.expected, &owner, &changed, true, true)
            .is_err());
        let mut changed_claim = event.expected.clone();
        changed_claim.through += 1;
        assert!(flow
            .persisted(&changed_claim, &owner, &current, true, true)
            .is_err());
        let mut stale_owner = owner.clone();
        stale_owner.revision += 1;
        assert!(flow
            .persisted(&event.expected, &stale_owner, &current, true, true)
            .is_err());
        let next = flow
            .persisted(&event.expected, &owner, &current, true, true)
            .unwrap();
        assert!(next.pending.is_none());
        assert_eq!(next.revision, event.expected.revision); // Store assigns the CAS revision.
        assert!(flow
            .persisted(&event.expected, &owner, &current, true, true)
            .is_err());
        assert!(event.expected.pending.is_some()); // planning alone never clears disk.
    }
    #[test]
    fn async_preparation_rejects_owner_and_metadata_only_edits_before_apply() {
        let (mut flow, event, owner, current) = fixture();
        flow.start(&event, &owner, &current).unwrap();
        let mut changed = current.clone();
        changed.has_authored_metadata = true;
        changed.edit_revision += 1;
        assert!(flow.resume(&event.expected, &owner, &changed).is_err());
        let mut stale = owner.clone();
        stale.manager = "other".into();
        assert!(flow.resume(&event.expected, &stale, &current).is_err());
        assert!(flow.resume(&event.expected, &owner, &current).is_ok());
        assert!(flow.resume(&event.expected, &owner, &current).is_err());
    }
    #[test]
    fn explicit_defer_and_cancel_require_fresh_snapshot_and_never_replace_draft() {
        for action in [Action::Defer, Action::Cancel] {
            let (mut flow, mut event, owner, current) = fixture();
            event.action = action;
            flow.start(&event, &owner, &current).unwrap();
            let mut edited = current.clone();
            edited.draft.text = "authored during disk load".into();
            edited.edit_revision += 1;
            assert!(flow
                .resolve(&event.expected, &owner, &edited, action, 500)
                .is_err());
            let next = flow
                .resolve(&event.expected, &owner, &current, action, 500)
                .unwrap();
            assert!(next.pending.is_none());
            assert_eq!(
                next.through,
                if action == Action::Defer { 500 } else { 100 }
            );
            assert!(current.draft.text.is_empty());
            assert!(flow.start(&event, &owner, &current).is_err());
        }
    }
}
