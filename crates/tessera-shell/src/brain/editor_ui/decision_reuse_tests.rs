//! Reuse attempts use the existing guarded outbox, including same-base fresh review.
use super::*;
use ::core::prelude::v1::test;
use tessera_core::decision_reuse::{self as reuse, Disposition};
fn fixture() -> (std::path::PathBuf, Value, Value, EditorRecovery, Value) {
    let (dir, workspace, _, store) = super::tests::fixture();
    let goal = uuid();
    let id = uuid();
    let raw=format!("---\nschema: ai-brain/v1\nrecord_type: discussion-decision\nbrain_id: {}\nid: {id}\ngoal_id: {goal}\nactor_id: local:oleg\n---\n Keep exact λ\n",text(&workspace["brain_id"]));
    let base = json!({"schema":SCHEMA,"brain_id":workspace["brain_id"],"path":format!("records/discussion-decision-{id}.md"),"revision":reuse::revision(raw.as_bytes()),"content_base64":STANDARD.encode(raw),"media_type":"text/markdown"});
    let p = Disposition::new(
        uuid(),
        "local:oleg".into(),
        "2026-09-08T16:00:00Z".into(),
        text(&base["revision"]),
    );
    let proposed = reuse::transform(
        &serde_json::from_value(base.clone()).unwrap(),
        &goal,
        &id,
        &p,
    )
    .unwrap();
    let mut request = source_write(&base, &proposed, &p.operation_id);
    request["op"] = json!("discussion_decision_reuse_write");
    request["goal_id"] = json!(goal);
    request["decision_id"] = json!(id);
    (dir, workspace, base, store, request)
}
#[test]
fn reuse_lost_ack_retains_guarded_request_and_terminal_conflict_allows_fresh_review() {
    let (dir, workspace, base, store, request) = fixture();
    let proposed=decode_source(&json!({"schema":SCHEMA,"path":base["path"],"brain_id":base["brain_id"],"revision":"unused","content_base64":request["request"]["content_base64"]})).unwrap();
    let draft = store.create(&base, &proposed).unwrap();
    let attempt = save_attempt_with_mode(&store, &draft, &request, SaveMode::Fresh, |wire| {
        assert_eq!(wire, request);
        Err("Lost ACK".into())
    });
    assert!(attempt.result.is_err());
    let reopened = EditorRecovery::at(dir.clone(), &workspace).unwrap();
    let pending = reopened.list().unwrap().drafts.remove(0);
    assert_eq!(pending.pending_save, Some(request.clone()));
    assert!(reopened.discard(&pending).is_err());
    let mut current = base.clone();
    current["content_base64"] = json!(STANDARD.encode("conflicting"));
    current["revision"] = json!(reuse::revision(b"conflicting"));
    let mut proposal = base.clone();
    proposal["content_base64"] = request["request"]["content_base64"].clone();
    proposal["revision"] = json!(reuse::revision(proposed.as_bytes()));
    let conflict = json!({"conflict":{"conflict_id":request["request"]["operation_id"],"path":base["path"],"expected_revision":base["revision"],"current_revision":current["revision"],"reason":"stale_revision"},"base":base,"proposed":proposal,"current":current});
    let mut calls = 0;
    let result =
        save_attempt_with_mode(&reopened, &pending, &request, SaveMode::Recovery, |wire| {
            calls += 1;
            assert_eq!(wire, request);
            Ok(json!({"source_conflict":conflict}))
        });
    assert_eq!(calls, 1);
    assert!(!result.result.unwrap().written);
    let terminal = reopened.list().unwrap().drafts.remove(0);
    assert!(terminal.has_terminal_guarded_conflict());
    assert!(!terminal.has_terminal_criteria_conflict());
    reopened.discard(&terminal).unwrap();
    assert!(reopened.list().unwrap().drafts.is_empty());
    let fresh = store.create(&base, &proposed).unwrap();
    assert_ne!(fresh.id, draft.id);
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
fn reuse_explicit_action_cancels_old_navigation_and_close(cx: &mut TestAppContext) {
    let (dir, workspace, _, store, _) = fixture();
    cx.update(gpui_component::init);
    let (view, cx) = cx
        .add_window_view(|window, cx| BrainView::new_managed_test(&workspace, &store, window, cx));
    cx.run_until_parked();
    view.update_in(cx, |v, window, cx| {
        v.busy = false;
        let goal = v.goal_id();
        v.decision_reuse.active = true;
        assert!(v.reuse_guard_navigation(PendingNavigation::Goal(uuid()), cx));
        assert!(v.reuse_request_close(cx));
        // Both Include and Open original use this exact departure path before
        // dispatching their action; the old goal/close must never execute.
        v.reuse_leave_for_action(window, cx);
        assert!(!v.decision_reuse.active);
        assert_eq!(v.goal_id(), goal);
        assert!(!v.busy);
        assert!(!v.source_loading);
        assert!(!v.editor_closing());
        assert!(v.pending_navigation.is_none());
        v.decision_reuse.active = true;
        assert!(v.reuse_guard_navigation(PendingNavigation::Source("records/old.md".into()), cx));
        v.reuse_keep_reviewing(cx);
        assert!(v.decision_reuse.active);
        v.reuse_leave_for_action(window, cx);
        assert!(!v.busy);
        assert!(!v.source_loading);
    });
    assert!(
        !dir.exists(),
        "Navigation alone must not create a recovery draft"
    );
}
