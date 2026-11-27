use crate::project::*;
use ::core::prelude::v1::test;
use okilum_t3::rotation::{Checkpoint, Owner, PolicyStore, TzifResolver};
use std::sync::atomic::Ordering;

// The explicit test policy supplies its own revision; neither a saved claim
// nor the native connection counter can grant that authority.
fn policy_owner(view: &ProjectView, cx: &App) -> Option<Owner> {
    super::policy_owner::canonical_owner(view, cx)
}

fn pending(store: &PolicyStore, owner: Owner) -> Checkpoint {
    let checkpoint = store.load().unwrap().unwrap();
    store
        .claim(
            checkpoint.revision,
            &owner,
            checkpoint.through + 22 * 3600,
            &TzifResolver::new("/usr/share/zoneinfo"),
            None,
        )
        .unwrap()
}

#[gpui::test]
fn rotation_1473_close_and_connection_change_revoke_mounted_claim_without_touching_draft(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        crate::test_init(cx);
        bind_keys(cx);
    });
    let state = tempfile::tempdir().unwrap();
    let mut store = None;
    let mut owner = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let project = cx.new(|cx| {
            discuss_handoff::native_tests::view_with_draft(
                state.path(),
                "Authored draft 🧠\n",
                window,
                cx,
            )
        });
        owner = Some(project.clone());
        let composer = project.read(cx).composer.clone();
        Root::new(composer, window, cx)
    });
    let project = owner.unwrap();
    visual.run_until_parked();
    let (claim, before) = project.update_in(visual, |view, window, cx| {
        view.draft_handoff_surface_visible(true, cx);
        let policy = super::policy_owner::tests::seed(view, cx);
        store = Some(policy.clone());
        let claim = pending(&policy, policy_owner(view, cx).unwrap());
        let before = view.composer_snapshot(cx);
        view.mount_pending_rotation(
            policy,
            Some("Reviewed source\n".into()),
            policy_owner,
            window,
            cx,
        );
        (claim, before)
    });
    let store = store.unwrap();
    visual.run_until_parked();
    let old = project.update_in(visual, |view, _, cx| {
        assert!(
            view.rotation.is_some(),
            "positive control: real coordinator mounted"
        );
        let valid = view.rotation_validity.as_ref().unwrap().clone();
        let epoch = view.rotation_epoch;
        let authority = view
            .rotation_policy
            .authority_for(&claim.owner, &store)
            .unwrap();
        let authority_before = authority.load().unwrap().unwrap().record;
        view.draft_handoff_surface_visible(true, cx);
        assert!(valid.load(Ordering::Acquire));
        assert_eq!(
            view.rotation_epoch, epoch,
            "identical repaint preserves the lease"
        );
        assert_eq!(
            authority.load().unwrap().unwrap().record,
            authority_before,
            "same visibility cannot allocate policy authority"
        );
        view.draft_handoff_surface_visible(false, cx);
        assert!(
            !valid.load(Ordering::Acquire),
            "close revokes before the next paint"
        );
        assert!(view.rotation.is_none());
        assert_eq!(view.rotation_epoch, epoch + 1);
        assert!(view.composer_snapshot(cx) == before);
        assert_eq!(store.load().unwrap().unwrap(), claim);
        valid
    });
    project.update_in(visual, |view, window, cx| {
        view.draft_handoff_surface_visible(true, cx);
        view.mount_pending_rotation(store.clone(), None, policy_owner, window, cx);
    });
    visual.run_until_parked();
    project.update_in(visual, |view, _, cx| {
        let fresh = view.rotation_validity.as_ref().unwrap().clone();
        assert!(
            fresh.load(Ordering::Acquire),
            "positive control: reopened coordinator is live"
        );
        assert!(!old.load(Ordering::Acquire));
        view.draft_handoff_connection_changing();
        assert!(!fresh.load(Ordering::Acquire));
        assert!(ProjectView::rotation_owner(view, cx).is_none());
        assert!(view.composer_snapshot(cx) == before);
        assert_eq!(store.load().unwrap().unwrap(), claim);
        assert!(
            view.records.is_empty(),
            "reconciliation never prepares or sends a message"
        );
    });
}

#[gpui::test]
fn rotation_1473_held_mount_refuses_a_closed_surface_and_fresh_mount_still_works(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    cx.update(crate::test_init);
    let state = tempfile::tempdir().unwrap();
    let mut store = None;
    let mut owner = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let project = cx.new(|cx| discuss_handoff::native_tests::view(state.path(), window, cx));
        owner = Some(project.clone());
        let composer = project.read(cx).composer.clone();
        Root::new(composer, window, cx)
    });
    let project = owner.unwrap();
    visual.run_until_parked();
    let claim = project.update_in(visual, |view, window, cx| {
        view.draft_handoff_surface_visible(true, cx);
        let policy = super::policy_owner::tests::seed(view, cx);
        store = Some(policy.clone());
        let claim = pending(&policy, policy_owner(view, cx).unwrap());
        view.mount_pending_rotation(
            policy,
            Some("Reviewed source\n".into()),
            policy_owner,
            window,
            cx,
        );
        // Same UI update: close before the queued disk read can publish a mount.
        view.draft_handoff_surface_visible(false, cx);
        claim
    });
    let store = store.unwrap();
    visual.run_until_parked();
    project.update_in(visual, |view, window, cx| {
        assert!(view.rotation.is_none());
        assert!(view.rotation_validity.is_none());
        assert_eq!(store.load().unwrap().unwrap(), claim);
        view.draft_handoff_surface_visible(true, cx);
        view.mount_pending_rotation(store.clone(), None, policy_owner, window, cx);
    });
    visual.run_until_parked();
    project.update_in(visual, |view, _, cx| {
        assert!(
            view.rotation.is_some(),
            "positive control: current visible owner mounts"
        );
        let owner = ProjectView::rotation_owner(view, cx).unwrap();
        assert_eq!(
            owner.revision, claim.owner.revision,
            "policy authority remains independent of connection epoch"
        );
        view.native.connection_epoch += 1;
        assert!(ProjectView::rotation_owner(view, cx).is_none());
        assert_eq!(store.load().unwrap().unwrap(), claim);
        assert!(view.records.is_empty());
    });
}
