//! Guarded requests keep their identity across every source recovery boundary.
use super::*;
use ::core::prelude::v1::test;
use sha2::{Digest, Sha256};

pub(super) fn fixture() -> (std::path::PathBuf, Value, Value, EditorRecovery, Value) {
    let (dir, workspace, _, store) = super::tests::fixture();
    let goal = uuid();
    let original=format!("---\nschema: ai-brain/v1\nrecord_type: goal\nbrain_id: {}\nid: {goal}\ntitle: Original\ncriteria:\n- id: C1\n  description: Original proof\n  requires_human: true\n---\n# Full body\n",text(&workspace["brain_id"]));
    let base = json!({"schema":SCHEMA,"brain_id":workspace["brain_id"],"path":format!("records/goal-{goal}.md"),"revision":format!("sha256:{:x}",Sha256::digest(original.as_bytes())),"content_base64":STANDARD.encode(&original),"media_type":"text/markdown"});
    let typed = serde_json::from_value(base.clone()).unwrap();
    let mut rows = tessera_core::goal_criteria::inspect(&typed, &goal).unwrap();
    rows[0].description = "Revised proof".into();
    let proposed = tessera_core::goal_criteria::transform(&typed, &goal, &rows).unwrap();
    let mut request = source_write(&base, &proposed, &uuid());
    request["op"] = json!("goal_criteria_write");
    request["goal_id"] = json!(goal);
    (dir, workspace, base, store, request)
}
#[test]
fn criteria_lost_reply_reopens_original_guarded_save_without_downgrade() {
    let (dir, workspace, base, store, request) = fixture();
    let proposed = String::from_utf8(
        STANDARD
            .decode(text(&request["request"]["content_base64"]))
            .unwrap(),
    )
    .unwrap();
    let draft = store.create(&base, &proposed).unwrap();
    let result = save_attempt_with_mode(&store, &draft, &request, SaveMode::Fresh, |wire| {
        assert_eq!(wire, request);
        Err("Lost acknowledgement".into())
    });
    assert!(result.result.is_err());
    let reopened = EditorRecovery::at(dir.clone(), &workspace).unwrap();
    let saved = reopened.list().unwrap().drafts.remove(0);
    assert_eq!(saved.pending_save, Some(request.clone()));
    assert!(
        reopened.discard(&saved).is_err(),
        "An unresolved send must remain protected"
    );
    assert_eq!(reopened.list().unwrap().drafts, vec![saved.clone()]);
    let mut downgraded = request.clone();
    downgraded["op"] = json!("source_write");
    downgraded.as_object_mut().unwrap().remove("goal_id");
    assert!(reopened.retain_save(&saved, &downgraded).is_err());
    let retry = save_attempt(&reopened, &saved, &request, |wire| {
        assert_eq!(wire, request);
        Ok(super::tests::receipt(&request))
    });
    assert!(retry.result.unwrap().written);
    assert!(reopened.list().unwrap().drafts.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn criteria_conflict_retains_guarded_envelope_and_never_sends_merge_child() {
    let (dir, workspace, base, store, request) = fixture();
    let proposed = String::from_utf8(
        STANDARD
            .decode(text(&request["request"]["content_base64"]))
            .unwrap(),
    )
    .unwrap();
    let draft = store.create(&base, &proposed).unwrap();
    let mut current = base.clone();
    let changed = format!("{}\nExternal body\n", decode_source(&base).unwrap());
    current["content_base64"] = json!(STANDARD.encode(&changed));
    current["revision"] = json!(format!("sha256:{:x}", Sha256::digest(changed.as_bytes())));
    let mut candidate = base.clone();
    candidate["content_base64"] = request["request"]["content_base64"].clone();
    candidate["revision"] = json!(format!("sha256:{:x}", Sha256::digest(proposed.as_bytes())));
    let conflict = json!({"conflict":{"conflict_id":request["request"]["operation_id"],"path":base["path"],"expected_revision":base["revision"],"current_revision":current["revision"],"reason":"changed"},"base":base,"current":current,"proposed":candidate});
    let mut calls = 0;
    let result = save_attempt_with_mode(&store, &draft, &request, SaveMode::Fresh, |wire| {
        calls += 1;
        assert_eq!(wire, request);
        Ok(json!({"source_conflict":conflict}))
    });
    assert!(!result.result.unwrap().written);
    assert_eq!(calls, 1);
    let reopened = EditorRecovery::at(dir.clone(), &workspace).unwrap();
    let draft = reopened.list().unwrap().drafts.remove(0);
    assert_eq!(draft.pending_save, Some(request.clone()));
    assert_eq!(draft.conflict, Some(conflict));
    // A stale window cannot discard the newer, positively terminal generation.
    assert!(reopened
        .discard(&Draft {
            generation: draft.generation - 1,
            ..draft.clone()
        })
        .is_err());
    let path = dir
        .join(text(&workspace["brain_id"]))
        .join(format!("{}.json", draft.id));
    let exact = std::fs::read(&path).unwrap();
    for field in ["conflict_id", "expected_revision"] {
        let mut corrupt: Value = serde_json::from_slice(&exact).unwrap();
        corrupt["conflict"]["conflict"][field] = json!(uuid());
        std::fs::write(&path, serde_json::to_vec(&corrupt).unwrap()).unwrap();
        let mismatched = reopened.list().unwrap().drafts.remove(0);
        assert!(
            reopened.discard(&mismatched).is_err(),
            "Mismatched {field} cannot establish terminality"
        );
        assert!(path.exists());
    }
    std::fs::write(&path, &exact).unwrap();
    reopened.discard(&draft).unwrap();
    assert!(reopened.list().unwrap().drafts.is_empty());
    // A fresh review now uses the current saved base and receives a new identity.
    let current = &draft.conflict.as_ref().unwrap()["current"];
    let typed = serde_json::from_value(current.clone()).unwrap();
    let goal = text(&request["goal_id"]);
    let mut rows = tessera_core::goal_criteria::inspect(&typed, &goal).unwrap();
    rows[0].description = "Fresh reviewed proof".into();
    let proposed = tessera_core::goal_criteria::transform(&typed, &goal, &rows).unwrap();
    let fresh = reopened.create(current, &proposed).unwrap();
    assert_ne!(fresh.id, draft.id);
    let mut next = source_write(current, &proposed, &uuid());
    next["op"] = json!("goal_criteria_write");
    next["goal_id"] = json!(goal);
    assert_ne!(
        next["request"]["operation_id"],
        request["request"]["operation_id"]
    );
    assert_eq!(
        reopened.retain_save(&fresh, &next).unwrap().pending_save,
        Some(next)
    );
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn criteria_wrong_owner_and_unknown_envelope_are_refused_without_deleting_draft() {
    let (dir, _, base, store, request) = fixture();
    let proposed = String::from_utf8(
        STANDARD
            .decode(text(&request["request"]["content_base64"]))
            .unwrap(),
    )
    .unwrap();
    let draft = store.create(&base, &proposed).unwrap();
    for (key, value) in [
        ("goal_id", json!(uuid())),
        ("unexpected", json!(true)),
        ("op", json!("unsupported_future_write")),
    ] {
        let mut bad = request.clone();
        bad[key] = value;
        assert!(store.retain_save(&draft, &bad).is_err());
        assert_eq!(store.list().unwrap().drafts, vec![draft.clone()]);
    }
    std::fs::remove_dir_all(dir).unwrap();
}
#[gpui::test]
fn criteria_pretransport_failure_preserves_guard_and_blocks_generic_source_save(
    cx: &mut TestAppContext,
) {
    let (dir, workspace, base, store, request) = fixture();
    cx.update(gpui_component::init);
    let (view, cx) = cx
        .add_window_view(|window, cx| BrainView::new_managed_test(&workspace, &store, window, cx));
    cx.run_until_parked();
    view.update_in(cx, |v, window, cx| {
        v.busy = false;
        v.load_source(base.clone(), window, cx);
        let proposed = String::from_utf8(
            STANDARD
                .decode(text(&request["request"]["content_base64"]))
                .unwrap(),
        )
        .unwrap();
        v.source.reset(proposed, window, cx);
        // A concurrent recovery generation makes local retention fail before RPC.
        let stale = store.create(&base, &v.source.value(cx)).unwrap();
        store
            .update(&stale, "Another window owns this generation")
            .unwrap();
        v.editor.active = Some(stale);
        v.editor.confirmed = false;
        v.editor.error = None;
        v.editor_save(request.clone(), window, cx);
    });
    cx.run_until_parked();
    view.update_in(cx, |v, window, cx| {
        assert_eq!(v.editor.guarded_retry, Some(request.clone()));
        assert!(v.editor_guarded_save());
        assert!(v.source_readonly());
        assert!(!v.editor_can_begin_criteria());
        v.save_source(window, cx);
        assert!(v.editor.save.is_none());
        assert_eq!(v.editor.guarded_retry, Some(request));
        assert!(!v.editor_request_close(window, cx));
        // Even after recovery is readable/protected again, the in-memory
        // command cannot be abandoned as a generic durable source draft.
        v.editor.closing = None;
        v.editor.error = None;
        let current = store.list().unwrap().drafts.remove(0);
        let updated = store.update(&current, &v.source.value(cx)).unwrap();
        v.editor.active = Some(updated.clone());
        v.editor.confirmed = true;
        assert!(v.editor_save_owner_matches(&workspace, &updated.id));
        assert!(!v.editor_save_owner_matches(&workspace, &uuid()));
        let mut other = workspace.clone();
        other["brain_id"] = json!(uuid());
        assert!(!v.editor_save_owner_matches(&other, &updated.id));
        assert!(!v.editor_request_close(window, cx));
    });
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
fn criteria_late_save_failure_does_not_attach_to_another_visible_owner(cx: &mut TestAppContext) {
    let (dir, workspace, base, store, request) = fixture();
    cx.update(gpui_component::init);
    let (view, cx) = cx
        .add_window_view(|window, cx| BrainView::new_managed_test(&workspace, &store, window, cx));
    cx.run_until_parked();
    view.update_in(cx, |v, window, cx| {
        v.busy = false;
        v.load_source(base.clone(), window, cx);
        let proposed = String::from_utf8(
            STANDARD
                .decode(text(&request["request"]["content_base64"]))
                .unwrap(),
        )
        .unwrap();
        v.source.reset(proposed, window, cx);
        let draft = store.create(&base, &v.source.value(cx)).unwrap();
        v.editor.active = Some(draft);
        v.editor.confirmed = true;
        v.editor.error = None;
        v.editor.save = Some(request.clone());
        v.editor_send(window, cx);
        // Force the exceptional late-owner branch before the worker's callback.
        v.expected_workspace.as_mut().unwrap()["brain_id"] = json!(uuid());
        v.editor.active = None;
        v.editor.flight = false;
        v.editor.sending = None;
        v.busy = false;
        v.source.reset("Other owner's visible source", window, cx);
    });
    cx.run_until_parked();
    view.update_in(cx, |v, _, cx| {
        assert!(v.editor.guarded_retry.is_none());
        assert!(v.editor.active.is_none());
        assert!(!v.editor.flight);
        assert_eq!(v.source.value(cx).as_ref(), "Other owner's visible source");
        assert!(v.error.as_deref().unwrap().contains("late Save"));
    });
    assert_eq!(store.list().unwrap().drafts[0].pending_save, Some(request));
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
fn criteria_guarded_shortcut_and_navigation_do_not_start_generic_loading(cx: &mut TestAppContext) {
    let (dir, workspace, base, store, request) = fixture();
    cx.update(gpui_component::init);
    let (view, cx) = cx
        .add_window_view(|window, cx| BrainView::new_managed_test(&workspace, &store, window, cx));
    cx.run_until_parked();
    view.update_in(cx, |v, window, cx| {
        v.busy = false;
        v.load_source(base.clone(), window, cx);
        let proposed = String::from_utf8(
            STANDARD
                .decode(text(&request["request"]["content_base64"]))
                .unwrap(),
        )
        .unwrap();
        v.source.reset(proposed, window, cx);
        let draft = store.create(&base, &v.source.value(cx)).unwrap();
        v.editor.active = Some(store.retain_save(&draft, &request).unwrap());
        v.editor.confirmed = true;
        v.editor.error = None;
        v.capabilities["source_write"] = json!(true);
        v.collection = Collection::Sources;
        v.surface = Surface::Source;
        v.focus.focus(window, cx);
        cx.notify();
    });
    cx.run_until_parked();
    cx.dispatch_action(SaveDraft);
    cx.run_until_parked();
    view.update_in(cx, |v, window, cx| {
        assert!(!v.source_loading);
        assert!(!v.busy);
        assert!(v.editor.save.is_none());
        assert_eq!(
            v.editor.active.as_ref().unwrap().pending_save,
            Some(request.clone())
        );
        v.save_and_continue(window, cx);
        assert!(!v.navigation_after_source);
        assert!(!v.source_loading);
        // Isolate the other guarded state: local retention failed before a
        // pending command was published. No pending_save can mask this guard.
        let pending = v.editor.active.as_ref().unwrap();
        store
            .acknowledge(&pending.id, &request, &super::tests::receipt(&request))
            .unwrap();
        v.editor.active = Some(store.create(&base, &v.source.value(cx)).unwrap());
        assert!(v.editor.active.as_ref().unwrap().pending_save.is_none());
        v.editor.guarded_retry = Some(request.clone());
        v.save_and_continue(window, cx);
        assert!(!v.navigation_after_source);
        assert!(!v.source_loading);
    });
    cx.dispatch_action(SaveDraft);
    cx.run_until_parked();
    view.update_in(cx, |v, _, cx| {
        assert!(!v.source_loading);
        assert!(v.editor.save.is_none());
        assert_eq!(v.editor.guarded_retry, Some(request.clone()));
        assert!(v.editor.active.as_ref().unwrap().pending_save.is_none());
        assert!(store.list().unwrap().drafts[0].pending_save.is_none());
        v.editor.guarded_retry = None;
        v.editor.error = None;
        cx.notify();
    });
    // Positive control: the same rendered SaveDraft action actually sends an
    // ordinary Source save once the guarded operation has been acknowledged.
    cx.run_until_parked();
    cx.dispatch_action(SaveDraft);
    cx.run_until_parked();
    let drafts = store.list().unwrap().drafts;
    assert_eq!(drafts.len(), 1);
    assert_eq!(
        drafts[0].pending_save.as_ref().unwrap()["op"],
        "source_write"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
