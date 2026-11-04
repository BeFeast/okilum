//! Exact, draft-only rotation handoff. Parent wires this into the selected
//! project's composer; no send, launch, registry edit or scheduled task occurs.
use okilum_t3::chat_drafts::{Attachment, Draft};
use okilum_t3::rotation::{Blocker, Checkpoint, Owner, PolicyStore, TzifResolver};

#[derive(Clone)]
#[cfg_attr(not(test), allow(dead_code, reason = "exercised only by tests"))]
pub(super) struct Handoff {
    owner: Owner,
    captured: Draft<Attachment>,
    replacement: Draft<Attachment>,
}
#[cfg_attr(not(test), allow(dead_code, reason = "exercised only by tests"))]
impl Handoff {
    pub(super) fn capture(
        owner: Owner,
        current: Draft<Attachment>,
        exact_text: String,
    ) -> Result<Self, &'static str> {
        if exact_text.is_empty() {
            return Err("Rotation draft is empty");
        }
        // Never overwrite an existing composer, including attachment-only work.
        if !current.text.is_empty() || !current.attachments.is_empty() {
            return Err("Keep or clear the current draft before preparing rotation");
        }
        Ok(Self {
            owner,
            captured: current,
            replacement: Draft {
                text: exact_text,
                attachments: Vec::new(),
            },
        })
    }
    /// Compare again at application time, after any asynchronous preview work.
    pub(super) fn apply(
        self,
        owner: &Owner,
        current: &Draft<Attachment>,
    ) -> Result<Draft<Attachment>, &'static str> {
        if &self.owner != owner {
            return Err("The selected project or manager changed");
        }
        if current != &self.captured {
            return Err("The composer changed; draft preserved");
        }
        Ok(self.replacement)
    }
}

/// Prepare a recovery draft only after its exact owner/window claim is durable.
/// Parent applies the handoff, flushes the existing draft writer, and ONLY then
/// calls PolicyStore::acknowledge(checkpoint.revision, owner). A crash or a stale
/// composer leaves the pending claim visible for reconciliation after restart.
#[allow(clippy::too_many_arguments)]
#[cfg_attr(not(test), allow(dead_code, reason = "exercised only by tests"))]
pub(super) fn prepare_recovery(
    store: &PolicyStore,
    expected_revision: u64,
    owner: &Owner,
    current: Draft<Attachment>,
    exact_text: String,
    now: i64,
    resolver: &TzifResolver,
    blocker: Option<Blocker>,
) -> Result<(Handoff, Checkpoint), String> {
    let handoff = Handoff::capture(owner.clone(), current, exact_text)?;
    let claim = store.claim(expected_revision, owner, now, resolver, blocker)?;
    Ok((handoff, claim))
}

#[path = "rotation/panel.rs"]
pub(super) mod panel;
pub(super) mod policy_owner;

// Frozen slot23 snapshot includes the complete retained typed metadata.
pub(super) use super::drafts::ComposerSnapshot;
#[path = "rotation/coordinator.rs"]
pub(super) mod coordinator;
#[path = "rotation/protocol.rs"]
mod protocol;

#[cfg(test)]
#[path = "rotation/lifecycle_tests.rs"]
mod lifecycle_tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Resume,
    Defer,
    Cancel,
}
#[derive(Clone)]
pub(super) struct Requested {
    pub action: Action,
    pub expected: Checkpoint,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DraftState {
    SourceUnavailable,
    Empty,
    AlreadyPrepared,
    AuthoredDraftPreserved,
}
/// No transport is held here. Resume prepares a local draft only. The pending
/// claim survives until the parent confirms that exact draft's durable flush.
pub(super) struct PendingReview {
    checkpoint: Checkpoint,
    source: Option<String>,
    captured: ComposerSnapshot,
    resume_issued: bool,
    applied: Option<ComposerSnapshot>,
}
pub(super) struct ResumeDraft {
    owner: Owner,
    expected: ComposerSnapshot,
    pub replacement: Draft<Attachment>,
}
impl ResumeDraft {
    /// Compare immediately before applying to the slot23 composer. A typed
    /// metadata edit or an ABA text edit is a conflict even if text is equal.
    pub fn check(&self, owner: &Owner, current: &ComposerSnapshot) -> Result<(), &'static str> {
        if owner != &self.owner {
            return Err("Rotation target changed; draft preserved");
        }
        if current != &self.expected {
            return Err("Composer changed; authored draft preserved");
        }
        Ok(())
    }
}
impl PendingReview {
    pub fn open(
        checkpoint: Checkpoint,
        exact_source: Option<String>,
        captured: ComposerSnapshot,
    ) -> Result<Self, &'static str> {
        if checkpoint.pending.is_none() {
            return Err("No pending rotation claim");
        }
        Ok(Self {
            checkpoint,
            source: exact_source.filter(|s| !s.is_empty()),
            captured,
            resume_issued: false,
            applied: None,
        })
    }
    pub fn checkpoint(&self) -> &Checkpoint {
        &self.checkpoint
    }
    pub fn resume_text(&self) -> Option<&str> {
        self.source.as_deref()
    }
    pub fn draft_state(&self) -> DraftState {
        let Some(text) = &self.source else {
            return DraftState::SourceUnavailable;
        };
        if self.captured.has_authored_metadata || !self.captured.draft.attachments.is_empty() {
            DraftState::AuthoredDraftPreserved
        } else if &self.captured.draft.text == text {
            DraftState::AlreadyPrepared
        } else if self.captured.draft.text.is_empty() {
            DraftState::Empty
        } else {
            DraftState::AuthoredDraftPreserved
        }
    }
    fn validate_loaded(&self, loaded: &Checkpoint, owner: &Owner) -> Result<(), String> {
        if owner != &self.checkpoint.owner {
            return Err("Rotation target changed; reopen reconciliation".into());
        }
        if loaded != &self.checkpoint {
            return Err("Rotation claim changed; reopen reconciliation".into());
        }
        Ok(())
    }
    #[cfg_attr(not(test), allow(dead_code, reason = "exercised only by tests"))]
    pub fn resume(
        &mut self,
        store: &PolicyStore,
        owner: &Owner,
        current: &ComposerSnapshot,
    ) -> Result<ResumeDraft, String> {
        let loaded = store.load()?.ok_or("Rotation claim disappeared")?;
        self.resume_loaded(&loaded, owner, current)
    }
    fn resume_loaded(
        &mut self,
        loaded: &Checkpoint,
        owner: &Owner,
        current: &ComposerSnapshot,
    ) -> Result<ResumeDraft, String> {
        self.validate_loaded(loaded, owner)?;
        if !self.checkpoint.policy.enabled() {
            return Err("Rotation is disabled".into());
        }
        if self.resume_issued {
            return Err("Draft resume already in progress; no duplicate preparation".into());
        }
        if current != &self.captured {
            return Err("Composer changed; authored draft preserved".into());
        }
        if !matches!(
            self.draft_state(),
            DraftState::Empty | DraftState::AlreadyPrepared
        ) {
            return Err(
                "Keep the authored draft or restore the exact rotation source before resuming"
                    .into(),
            );
        }
        self.resume_issued = true;
        Ok(ResumeDraft {
            owner: owner.clone(),
            expected: current.clone(),
            replacement: Draft {
                text: self.source.clone().expect("checked source"),
                attachments: Vec::new(),
            },
        })
    }
    /// Invoke after compare-and-apply in the same UI update. This does not ack
    /// the durable claim; a successful durable draft barrier is still required.
    pub fn record_applied(
        &mut self,
        request: &ResumeDraft,
        owner: &Owner,
        actual: ComposerSnapshot,
    ) -> Result<(), &'static str> {
        if owner != &self.checkpoint.owner
            || &request.owner != owner
            || self.source.as_deref() != Some(request.replacement.text.as_str())
            || !request.replacement.attachments.is_empty()
            || (actual.draft != request.expected.draft
                && actual.edit_revision <= request.expected.edit_revision)
            || !self.resume_issued
            || self.applied.is_some()
            || request.expected != self.captured
            || actual.draft != request.replacement
            || actual.has_authored_metadata
            || actual.edit_revision < request.expected.edit_revision
        {
            return Err("Rotation draft application did not match the reviewed request");
        }
        self.applied = Some(actual);
        Ok(())
    }
    /// Only call on the existing durable composer writer's successful barrier,
    /// with a fresh current snapshot. draft_flush alone is NOT an acknowledgement.
    #[cfg_attr(not(test), allow(dead_code, reason = "exercised only by tests"))]
    pub fn acknowledge_persisted(
        &self,
        store: &PolicyStore,
        owner: &Owner,
        current: &ComposerSnapshot,
    ) -> Result<Checkpoint, String> {
        let loaded = store.load()?.ok_or("Rotation claim disappeared")?;
        let next = self.acknowledgement_loaded(&loaded, owner, current)?;
        store.save(self.checkpoint.revision, next)
    }
    fn acknowledgement_loaded(
        &self,
        loaded: &Checkpoint,
        owner: &Owner,
        current: &ComposerSnapshot,
    ) -> Result<Checkpoint, String> {
        self.validate_loaded(loaded, owner)?;
        if self.applied.as_ref() != Some(current) {
            return Err("Stale draft acknowledgement; pending claim preserved".into());
        }
        let mut next = self.checkpoint.clone();
        next.pending = None;
        Ok(next)
    }
    #[cfg_attr(not(test), allow(dead_code, reason = "exercised only by tests"))]
    pub fn defer(
        &self,
        store: &PolicyStore,
        owner: &Owner,
        now: i64,
    ) -> Result<Checkpoint, String> {
        let loaded = store.load()?.ok_or("Rotation claim disappeared")?;
        let next = self.resolve_loaded(&loaded, owner, Some(now))?;
        store.save(self.checkpoint.revision, next)
    }
    #[cfg_attr(not(test), allow(dead_code, reason = "exercised only by tests"))]
    pub fn cancel(&self, store: &PolicyStore, owner: &Owner) -> Result<Checkpoint, String> {
        let loaded = store.load()?.ok_or("Rotation claim disappeared")?;
        let next = self.resolve_loaded(&loaded, owner, None)?;
        store.save(self.checkpoint.revision, next)
    }
    fn resolve_loaded(
        &self,
        loaded: &Checkpoint,
        owner: &Owner,
        defer_through: Option<i64>,
    ) -> Result<Checkpoint, String> {
        self.validate_loaded(loaded, owner)?;
        if self.resume_issued {
            return Err("Wait for draft persistence before resolving".into());
        }
        let mut next = self.checkpoint.clone();
        next.pending = None;
        if let Some(now) = defer_through {
            if now < self.checkpoint.through {
                return Err("Clock moved behind recovery cursor".into());
            }
            next.through = now;
        }
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    fn owner() -> Owner {
        Owner {
            origin: "fixture".into(),
            project: "p".into(),
            manager: "m".into(),
            revision: 1,
        }
    }
    #[test]
    fn recovery_claim_is_durable_and_busy_composer_does_not_claim() {
        use okilum_t3::rotation::{Policy, Recovery};
        let path = std::env::temp_dir().join(format!("rotation-shell-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        let store = PolicyStore::new(&path);
        let resolver = TzifResolver::new("/usr/share/zoneinfo");
        let owner = owner();
        let start = 20758 * 86400;
        store
            .configure(
                0,
                owner.clone(),
                Policy::new(true, "Asia/Jerusalem", ["08:30", "20:30"]).unwrap(),
                start,
                &resolver,
            )
            .unwrap();
        let busy = Draft {
            text: "user draft".into(),
            attachments: vec![],
        };
        let now = start + 22 * 3600;
        assert!(prepare_recovery(
            &store,
            1,
            &owner,
            busy,
            "exact request".into(),
            now,
            &resolver,
            None
        )
        .is_err());
        assert_eq!(store.load().unwrap().unwrap().revision, 1);
        let (handoff, checkpoint) = prepare_recovery(
            &store,
            1,
            &owner,
            Draft::default(),
            "exact request\n".into(),
            now,
            &resolver,
            None,
        )
        .unwrap();
        assert_eq!(
            handoff.apply(&owner, &Draft::default()).unwrap().text,
            "exact request\n"
        );
        assert!(matches!(
            store
                .load()
                .unwrap()
                .unwrap()
                .recover(&owner, now, &resolver, None)
                .unwrap(),
            Recovery::NeedsReconciliation(_)
        ));
        store.acknowledge(checkpoint.revision, &owner).unwrap();
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn exact_handoff_preserves_edits_and_rejects_stale_selection() {
        let text = "Rotation preview\n🧠  keep spaces  \n";
        let o = owner();
        let handoff = || Handoff::capture(o.clone(), Draft::default(), text.into()).unwrap();
        let changed = Draft {
            text: "new user edit".into(),
            attachments: vec![],
        };
        assert!(handoff().apply(&o, &changed).is_err());
        let mut stale = o.clone();
        stale.revision += 1;
        assert!(handoff().apply(&stale, &Draft::default()).is_err());
        assert_eq!(handoff().apply(&o, &Draft::default()).unwrap().text, text);
        assert!(Handoff::capture(o, changed, text.into()).is_err());
    }
}

/// Gregorian civil date from a Unix day, using wide arithmetic for untrusted
/// persisted input. Display only; timezone resolution remains in okilum-t3.
pub(super) fn calendar_day(day: i64) -> String {
    let z = i128::from(day) + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let date = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + i128::from(month <= 2);
    format!("{year:04}-{month:02}-{date:02}")
}

#[cfg(test)]
mod reconciliation_tests {
    use super::*;
    use ::core::prelude::v1::test;
    use okilum_t3::rotation::{Policy, Recovery};
    struct Fixture {
        path: std::path::PathBuf,
        store: PolicyStore,
        owner: Owner,
        checkpoint: Checkpoint,
    }
    impl Fixture {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("rotation-review-{label}-{}", std::process::id()));
            std::fs::create_dir(&path).unwrap();
            let store = PolicyStore::new(&path);
            let owner = Owner {
                origin: "fixture".into(),
                project: "project".into(),
                manager: "manager".into(),
                revision: 1,
            };
            let resolver = TzifResolver::new("/usr/share/zoneinfo");
            let start = 20758 * 86400;
            store
                .configure(
                    0,
                    owner.clone(),
                    Policy::new(true, "Asia/Jerusalem", ["08:30", "20:30"]).unwrap(),
                    start,
                    &resolver,
                )
                .unwrap();
            let checkpoint = store
                .claim(1, &owner, start + 22 * 3600, &resolver, None)
                .unwrap();
            Self {
                path,
                store,
                owner,
                checkpoint,
            }
        }
        fn review(&self, source: Option<&str>, snapshot: ComposerSnapshot) -> PendingReview {
            PendingReview::open(self.checkpoint.clone(), source.map(str::to_owned), snapshot)
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
    fn snapshot(text: &str, edit_revision: u64) -> ComposerSnapshot {
        ComposerSnapshot {
            draft: Draft {
                text: text.into(),
                attachments: vec![],
            },
            edit_revision,
            has_authored_metadata: false,
            typed_metadata: None,
        }
    }
    #[test]
    fn resume_is_draft_only_and_ack_requires_exact_persisted_snapshot() {
        let f = Fixture::new("resume");
        let initial = snapshot("", 10);
        let mut review = f.review(Some("Exact source\n"), initial.clone());
        let request = review.resume(&f.store, &f.owner, &initial).unwrap();
        assert!(review.resume(&f.store, &f.owner, &initial).is_err());
        assert!(review
            .acknowledge_persisted(&f.store, &f.owner, &initial)
            .is_err());
        request.check(&f.owner, &initial).unwrap();
        let applied = snapshot("Exact source\n", 11);
        review
            .record_applied(&request, &f.owner, applied.clone())
            .unwrap();
        let mut typed_edit = applied.clone();
        typed_edit.has_authored_metadata = true;
        typed_edit.edit_revision += 1;
        assert!(review
            .acknowledge_persisted(&f.store, &f.owner, &typed_edit)
            .is_err());
        assert!(f.store.load().unwrap().unwrap().pending.is_some());
        let saved = review
            .acknowledge_persisted(&f.store, &f.owner, &applied)
            .unwrap();
        assert!(saved.pending.is_none());
        assert!(review
            .acknowledge_persisted(&f.store, &f.owner, &applied)
            .is_err());
    }
    #[test]
    fn authored_draft_unknown_source_and_stale_target_are_preserved() {
        let f = Fixture::new("stale");
        let authored = snapshot("user draft", 5);
        let mut review = f.review(Some("rotation"), authored.clone());
        assert_eq!(review.draft_state(), DraftState::AuthoredDraftPreserved);
        assert!(review.resume(&f.store, &f.owner, &authored).is_err());
        let empty = snapshot("", 6);
        let mut missing = f.review(None, empty.clone());
        assert_eq!(missing.draft_state(), DraftState::SourceUnavailable);
        assert!(missing.resume(&f.store, &f.owner, &empty).is_err());
        let mut review = f.review(Some("rotation"), empty.clone());
        let mut stale = f.owner.clone();
        stale.manager = "replacement".into();
        assert!(review.resume(&f.store, &stale, &empty).is_err());
        assert!(review.cancel(&f.store, &stale).is_err());
        assert!(review
            .defer(&f.store, &stale, f.checkpoint.through)
            .is_err());
        let request = review.resume(&f.store, &f.owner, &empty).unwrap();
        assert!(request.check(&stale, &empty).is_err());
        let aba = snapshot("", 8);
        assert!(request.check(&f.owner, &aba).is_err());
        assert!(f.store.load().unwrap().unwrap() == f.checkpoint);
    }
    #[test]
    fn restart_retains_existing_exact_draft_without_duplicate_and_cancel_is_revision_aware() {
        let f = Fixture::new("restart");
        let existing = snapshot("rotation", 4);
        let mut review = f.review(Some("rotation"), existing.clone());
        assert_eq!(review.draft_state(), DraftState::AlreadyPrepared);
        let request = review.resume(&f.store, &f.owner, &existing).unwrap();
        assert!(request.replacement == existing.draft);
        review
            .record_applied(&request, &f.owner, existing.clone())
            .unwrap();
        // No successful draft barrier: reopening still sees pending review.
        let reloaded = f.store.load().unwrap().unwrap();
        assert!(reloaded.pending.is_some());
        let cancel = PendingReview::open(reloaded, None, existing.clone()).unwrap();
        let cancelled = cancel.cancel(&f.store, &f.owner).unwrap();
        assert!(cancelled.pending.is_none());
        assert!(cancelled.policy.enabled());
        assert!(review
            .acknowledge_persisted(&f.store, &f.owner, &existing)
            .is_err());
        assert!(cancel.cancel(&f.store, &f.owner).is_err());
        assert_eq!(existing.draft.text, "rotation");
    }
    #[test]
    fn explicit_defer_survives_restart_and_cannot_retry_old_window() {
        assert_eq!(calendar_day(20758), "2026-11-01");
        assert_eq!(calendar_day(0), "1970-01-01");
        assert_eq!(calendar_day(-1), "1969-12-31");
        let f = Fixture::new("defer");
        let authored = snapshot("keep authored text", 3);
        let review = f.review(None, authored.clone());
        let now = f.checkpoint.through + 86400;
        let deferred = review.defer(&f.store, &f.owner, now).unwrap();
        let resolver = TzifResolver::new("/usr/share/zoneinfo");
        assert_eq!(f.store.load().unwrap().unwrap(), deferred);
        assert_eq!(
            deferred.recover(&f.owner, now, &resolver, None).unwrap(),
            Recovery::NoMissedWindow
        );
        assert!(matches!(
            deferred
                .recover(&f.owner, now + 86400, &resolver, None)
                .unwrap(),
            Recovery::Offer(_)
        ));
        assert_eq!(authored.draft.text, "keep authored text");
    }
}
