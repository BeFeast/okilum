//! Fresh Save may retain and send one child; recovery never recomputes a merge.
use super::*;
use crate::brain::editor_recovery::AutoResolution;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SaveMode {
    Fresh,
    Recovery,
}

pub(super) struct Saved {
    pub draft: Option<Draft>,
    pub written: bool,
    pub submitted: Value,
    pub automatic: bool,
    pub adopt_from: Option<String>,
    pub manual_reason: Option<String>,
}
pub(super) struct SaveAttempt {
    pub id: String,
    pub result: Result<Saved, String>,
    pub inventory: Result<RecoveryList, String>,
}

fn finish(
    store: &EditorRecovery,
    retained: &Draft,
    request: &Value,
    response: Value,
    adopt_from: Option<String>,
    manual_reason: Option<String>,
) -> Result<Saved, String> {
    let automatic = retained.is_automatic_request(request);
    let (draft, written) = if let Some(conflict) = response.get("source_conflict") {
        (
            Some(store.record_conflict(&retained.id, request, conflict)?),
            false,
        )
    } else if automatic {
        (
            Some(store.acknowledge_auto_resolution(&retained.id, request, &response)?),
            true,
        )
    } else {
        (store.acknowledge(&retained.id, request, &response)?, true)
    };
    Ok(Saved {
        draft,
        written,
        submitted: request.clone(),
        automatic,
        adopt_from,
        manual_reason,
    })
}

pub(super) fn save_attempt_with_mode(
    store: &EditorRecovery,
    draft: &Draft,
    request: &Value,
    mode: SaveMode,
    mut rpc: impl FnMut(Value) -> Result<Value, String>,
) -> SaveAttempt {
    let result = store.retain_save(draft, request).and_then(|retained| {
        let response = rpc(request.clone())?;
        if request["op"] == "source_write"
            && mode == SaveMode::Fresh
            && retained.conflict.is_none()
            && !retained.is_automatic_request(request)
        {
            if let Some(conflict) = response.get("source_conflict") {
                let reason = match store.retain_auto_resolution(&retained, request, conflict) {
                    Ok(AutoResolution::Ready {
                        draft,
                        request: child,
                    }) => {
                        let response = rpc(child.clone())?;
                        return finish(
                            store,
                            &draft,
                            &child,
                            response,
                            Some(retained.text.clone()),
                            None,
                        );
                    }
                    Ok(AutoResolution::Manual { reason }) => reason.message().to_owned(),
                    Err(error) => error,
                };
                // If child publication became uncertain, this original-conflict
                // transition refuses the now-different pending request. No send.
                return finish(store, &retained, request, response, None, Some(reason));
            }
        }
        let adopt_from = if retained.is_automatic_request(request)
            && STANDARD
                .decode(text(&request["request"]["content_base64"]))
                .ok()
                .as_deref()
                == Some(retained.text.as_bytes())
        {
            Some(retained.text.clone())
        } else {
            None
        };
        finish(store, &retained, request, response, adopt_from, None)
    });
    SaveAttempt {
        id: draft.id.clone(),
        result,
        inventory: store.list(),
    }
}

#[cfg(test)]
pub(super) fn save_attempt(
    store: &EditorRecovery,
    draft: &Draft,
    request: &Value,
    rpc: impl FnOnce(Value) -> Result<Value, String>,
) -> SaveAttempt {
    let mut rpc = Some(rpc);
    save_attempt_with_mode(store, draft, request, SaveMode::Recovery, |request| {
        rpc.take().unwrap()(request)
    })
}

impl BrainView {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn editor_apply_auto_receipt(
        &mut self,
        acknowledged: Draft,
        child: &Value,
        original: &Value,
        adopt_from: Option<&str>,
        input_epoch: u64,
        native_source: Option<gpui_component::input::projection::SourceStamp>,
        inventory_error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let candidate = decode_source(&acknowledged.conflict.as_ref().unwrap()["current"]).unwrap();
        let current_record = self.editor.records.iter().find(|d| d.id == acknowledged.id);
        let record_changed = current_record.is_some_and(|d| d != &acknowledged);
        let base = acknowledged.conflict.as_ref().unwrap()["current"].clone();
        let unchanged = self.source_snapshot.as_ref() == Some(&acknowledged.base)
            && self.editor.input_epoch == input_epoch
            && self.source.stamp(cx) == native_source
            && adopt_from == Some(self.source.value(cx).as_ref())
            && acknowledged.text == candidate
            && current_record == Some(&acknowledged)
            && inventory_error.is_none();
        self.editor.active = Some(acknowledged.clone());
        self.editor.confirmed = true;
        self.editor.error = inventory_error;
        self.error = None;
        self.pending_source_write = None;
        let close_matches =
            !unchanged || self.editor_close_auto_saved(original, child, &base, &candidate, cx);
        if !unchanged || !close_matches {
            // Its old loaded base is intentional: newer unreconciled text must
            // not acquire the merged CAS revision and silently undo remote edits.
            self.show_source_conflict(acknowledged.conflict.clone().unwrap(), window, cx);
            self.notice="Earlier edits were merged and saved. Newer text remains based on the original version; compare the saved version before resolving.".into();
            if record_changed {
                self.editor.error=Some("A newer recovery generation remains protected. Recheck it before resolving this visible draft.".into());
            }
            self.editor_protect(window, cx);
            return;
        }

        self.source_original = candidate.clone();
        self.source_snapshot = Some(base);
        self.source_conflict = None;
        self.source.reset(candidate, window, cx);
        self.notice = "Independent edits merged and saved.".into();
        self.schedule_preview(window, cx);
        // Adopt first: any subsequent typing is now based on the visible merged
        // version. Retire only the exact acknowledged local generation.
        let store = self.editor_store();
        self.editor.flight = true;
        cx.spawn_in(window,async move |this,cx| {
            let expected=acknowledged.clone();
            let result=cx.background_executor().spawn(async move {store?.discard(&expected)}).await;
            let _=this.update_in(cx,|this,window,cx| {
                this.editor.flight=false;
                match result {
                    Ok(())=>{
                        if this.editor.active.as_ref()==Some(&acknowledged) {this.editor.active=None;}
                        this.editor.records.retain(|d|d.id!=acknowledged.id);
                    }
                    Err(error)=>{this.editor.error=Some(format!("Saved bytes are retained, but recovery retirement needs recheck: {error}"));this.editor.confirmed=false;}
                }
                this.editor_protect(window,cx);
                if this.navigation_after_source && !this.dirty(cx) {this.continue_navigation(window,cx);}
                cx.notify();
            });
        }).detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use sha2::{Digest, Sha256};

    struct Fixture {
        dir: std::path::PathBuf,
        workspace: Value,
        store: EditorRecovery,
        draft: Draft,
        original: Value,
        conflict: Value,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("okilum-auto-ui-{}", uuid()));
            let workspace = json!({"brain_id":uuid(),"root":"/isolated/auto-ui","records_dir":"records","managed":true});
            let store = EditorRecovery::at(dir.clone(), &workspace).unwrap();
            let snapshot = |s: &str| json!({"schema":SCHEMA,"brain_id":workspace["brain_id"],"path":"notes/source.md","revision":format!("sha256:{:x}",Sha256::digest(s.as_bytes())),"content_base64":STANDARD.encode(s),"media_type":"text/markdown"});
            let draft = store.create(&snapshot("A\nB\n"), "X\nB\n").unwrap();
            let original = source_write(&draft.base, &draft.text, &uuid());
            let conflict = json!({"base":draft.base,"current":snapshot("A\nY\n"),"proposed":snapshot("X\nB\n"),"conflict":{"conflict_id":original["request"]["operation_id"],"path":draft.base["path"],"expected_revision":draft.base["revision"],"current_revision":snapshot("A\nY\n")["revision"],"reason":"stale_revision"}});
            Self {
                dir,
                workspace,
                store,
                draft,
                original,
                conflict,
            }
        }
        fn ack(&self) -> Saved {
            let mut calls = 0;
            let result = save_attempt_with_mode(
                &self.store,
                &self.draft,
                &self.original,
                SaveMode::Fresh,
                |wire| {
                    calls += 1;
                    assert_eq!(
                        self.store.list().unwrap().drafts[0].pending_save,
                        Some(wire.clone())
                    );
                    if calls == 1 {
                        Ok(json!({"source_conflict":self.conflict}))
                    } else {
                        Ok(receipt(&wire))
                    }
                },
            );
            assert_eq!(calls, 2);
            result.result.unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
    fn receipt(wire: &Value) -> Value {
        let r = &wire["request"];
        json!({"operation_id":r["operation_id"],"path":r["path"],"previous_revision":r["expected_revision"],"revision":format!("sha256:{:x}",Sha256::digest(STANDARD.decode(text(&r["content_base64"])).unwrap())),"outcome":"written"})
    }
    #[test]
    fn fresh_save_sends_exactly_one_retained_child() {
        let f = Fixture::new();
        let saved = f.ack();
        assert!(saved.written && saved.automatic);
        assert_eq!(saved.adopt_from.as_deref(), Some("X\nB\n"));
        assert_eq!(saved.draft.unwrap().text, "X\nY\n");
    }
    #[test]
    fn lost_original_reply_retry_does_not_plan_child() {
        let f = Fixture::new();
        let first =
            save_attempt_with_mode(&f.store, &f.draft, &f.original, SaveMode::Fresh, |_| {
                Err("lost".into())
            });
        assert!(first.result.is_err());
        let pending = first.inventory.unwrap().drafts.pop().unwrap();
        let mut calls = 0;
        let retry = save_attempt_with_mode(
            &f.store,
            &pending,
            &f.original,
            SaveMode::Recovery,
            |wire| {
                calls += 1;
                assert_eq!(wire, f.original);
                Ok(json!({"source_conflict":f.conflict}))
            },
        );
        assert_eq!(calls, 1);
        assert!(!retry.result.unwrap().written);
        assert!(!f.store.list().unwrap().drafts[0].automatic_format);
    }
    #[test]
    fn lost_child_reply_retries_exact_child_despite_newer_text() {
        let f = Fixture::new();
        let mut calls = 0;
        let first =
            save_attempt_with_mode(&f.store, &f.draft, &f.original, SaveMode::Fresh, |_| {
                calls += 1;
                if calls == 1 {
                    Ok(json!({"source_conflict":f.conflict}))
                } else {
                    Err("lost child ACK".into())
                }
            });
        assert!(first.result.is_err());
        assert_eq!(calls, 2);
        let pending = first.inventory.unwrap().drafts.pop().unwrap();
        let child = pending.pending_save.clone().unwrap();
        let newer = f.store.update(&pending, "X+\nB\n").unwrap();
        calls = 0;
        let retry = save_attempt_with_mode(&f.store, &newer, &child, SaveMode::Recovery, |wire| {
            calls += 1;
            assert_eq!(wire, child);
            Ok(receipt(&wire))
        });
        assert_eq!(calls, 1);
        let saved = retry.result.unwrap();
        assert!(saved.automatic);
        assert!(saved.adopt_from.is_none());
        let d = saved.draft.unwrap();
        assert_eq!(d.base, f.draft.base);
        assert_eq!(d.text, "X+\nB\n");
    }
    #[test]
    fn second_conflict_is_manual_without_third_rpc() {
        let f = Fixture::new();
        let mut calls = 0;
        let result =
            save_attempt_with_mode(&f.store, &f.draft, &f.original, SaveMode::Fresh, |wire| {
                calls += 1;
                let mut conflict = f.conflict.clone();
                if calls == 2 {
                    conflict["conflict"]["expected_revision"] =
                        wire["request"]["expected_revision"].clone();
                    conflict["conflict"]["conflict_id"] = wire["request"]["operation_id"].clone();
                    conflict["base"] = wire["base"].clone();
                    conflict["proposed"]["content_base64"] =
                        wire["request"]["content_base64"].clone();
                    conflict["proposed"]["revision"] = receipt(&wire)["revision"].clone();
                }
                Ok(json!({"source_conflict":conflict}))
            });
        assert_eq!(calls, 2);
        let saved = result.result.unwrap();
        assert!(!saved.written);
        assert!(saved.draft.unwrap().pending_save.is_none());
    }
    #[test]
    fn newer_generation_during_original_rpc_prevents_child_send() {
        let f = Fixture::new();
        let mut calls = 0;
        let result =
            save_attempt_with_mode(&f.store, &f.draft, &f.original, SaveMode::Fresh, |_| {
                calls += 1;
                let retained = f.store.list().unwrap().drafts.pop().unwrap();
                f.store.update(&retained, "X+\nB\n").unwrap();
                Ok(json!({"source_conflict":f.conflict}))
            });
        assert_eq!(calls, 1);
        let saved = result.result.unwrap();
        assert!(!saved.written);
        assert!(saved.manual_reason.is_some());
        assert_eq!(saved.draft.unwrap().text, "X+\nB\n");
    }
    #[test]
    fn uncertain_child_publication_never_sends_child() {
        let f = Fixture::new();
        let mut calls = 0;
        let result =
            save_attempt_with_mode(&f.store, &f.draft, &f.original, SaveMode::Fresh, |_| {
                calls += 1;
                f.store
                    .fail_after_publish
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(json!({"source_conflict":f.conflict}))
            });
        assert_eq!(calls, 1);
        assert!(result.result.is_err());
        let retained = result.inventory.unwrap().drafts.pop().unwrap();
        assert!(retained.automatic_format);
        assert!(retained.pending_save.is_some());
        // Publication happened, but its failed durability ACK did not authorize RPC.
        assert_eq!(retained.text, "X\nY\n");
    }
    fn install(
        v: &mut BrainView,
        f: &Fixture,
        saved: &Saved,
        window: &mut Window,
        cx: &mut Context<BrainView>,
    ) {
        v.editor.flight = true;
        v.expected_workspace = Some(f.workspace.clone());
        v.editor.store = Some(f.store.clone());
        v.load_source(f.draft.base.clone(), window, cx);
        v.source.reset(f.draft.text.clone(), window, cx);
        v.editor.active = Some(f.draft.clone());
        v.editor.records = vec![saved.draft.clone().unwrap()];
        v.editor.confirmed = true;
        v.editor.flight = false;
        v.busy = false;
    }
    fn apply(
        v: &mut BrainView,
        f: &Fixture,
        saved: &Saved,
        epoch: u64,
        window: &mut Window,
        cx: &mut Context<BrainView>,
    ) {
        v.editor_apply_auto_receipt(
            saved.draft.clone().unwrap(),
            &saved.submitted,
            &f.original,
            saved.adopt_from.as_deref(),
            epoch,
            v.source.stamp(cx),
            None,
            window,
            cx,
        );
    }
    #[gpui::test]
    fn native_aba_before_subscription_delivery_refuses_receipt_but_presentation_allows_it(
        cx: &mut TestAppContext,
    ) {
        use gpui::EntityInputHandler;
        cx.update(gpui_component::init);
        for mutate in [true, false] {
            let f = Fixture::new();
            let saved = f.ack();
            let (view, cx) = cx.add_window_view(|window, cx| {
                BrainView::new_managed_test(&f.workspace, &f.store, window, cx)
            });
            cx.run_until_parked();
            view.update_in(cx, |v, window, cx| {
                install(v, &f, &saved, window, cx);
                v.sync_source_policy(cx);
                let epoch = v.editor.input_epoch;
                let captured = v.source.stamp(cx);
                if mutate {
                    v.source.managed().unwrap().update(cx, |state, cx| {
                        state.replace_text_in_range(Some(0..1), "Z", window, cx);
                        state.replace_text_in_range(Some(0..1), "X", window, cx);
                    });
                    assert_ne!(v.source.stamp(cx), captured);
                } else {
                    v.toggle_source_projection(window, cx);
                    v.toggle_source_projection(window, cx);
                    assert_eq!(v.source.stamp(cx), captured);
                }
                assert_eq!(v.source.value(cx).as_ref(), f.draft.text);
                assert_eq!(v.editor.input_epoch, epoch, "callbacks have not run");
                v.editor_apply_auto_receipt(
                    saved.draft.clone().unwrap(),
                    &saved.submitted,
                    &f.original,
                    saved.adopt_from.as_deref(),
                    epoch,
                    captured,
                    None,
                    window,
                    cx,
                );
                if mutate {
                    assert_eq!(v.source_snapshot, Some(f.draft.base.clone()));
                    assert_eq!(v.source.value(cx).as_ref(), f.draft.text);
                    assert!(v.source_conflict.is_some());
                } else {
                    assert_eq!(v.source.value(cx).as_ref(), "X\nY\n");
                    assert!(v.source_conflict.is_none());
                }
            });
            cx.run_until_parked();
            assert_eq!(f.store.list().unwrap().drafts.is_empty(), !mutate);
        }
    }
    #[gpui::test]
    fn clean_receipt_adopts_merged_bytes_before_retirement(cx: &mut TestAppContext) {
        let f = Fixture::new();
        let saved = f.ack();
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&f.workspace, &f.store, window, cx)
        });
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| {
            install(v, &f, &saved, window, cx);
            let epoch = v.editor.input_epoch;
            apply(v, &f, &saved, epoch, window, cx);
            assert_eq!(v.source.value(cx).as_ref(), "X\nY\n");
            assert!(v.source_conflict.is_none());
            assert!(v.editor.flight);
        });
        cx.run_until_parked();
        assert!(f.store.list().unwrap().drafts.is_empty());
        view.update_in(cx, |v, _, _| assert!(v.editor.active.is_none()));
    }
    #[gpui::test]
    fn late_visible_text_keeps_true_base_and_blocks_next_ordinary_save(cx: &mut TestAppContext) {
        let f = Fixture::new();
        let saved = f.ack();
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&f.workspace, &f.store, window, cx)
        });
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| {
            install(v, &f, &saved, window, cx);
            let epoch = v.editor.input_epoch;
            v.source.reset("X+\nB\n", window, cx);
            v.editor_input_changed();
            apply(v, &f, &saved, epoch, window, cx);
            assert_eq!(v.source.value(cx).as_ref(), "X+\nB\n");
            assert_eq!(v.source_snapshot, Some(f.draft.base.clone()));
            assert_eq!(
                decode_source(&v.source_conflict.as_ref().unwrap()["current"]).unwrap(),
                "X\nY\n"
            );
            v.save_source(window, cx);
            assert!(v.editor.save.is_none());
            assert!(v.editor.sending.is_none());
            assert!(v.pending_source_write.is_none());
        });
        cx.run_until_parked();
        let d = f.store.list().unwrap().drafts.pop().unwrap();
        assert_eq!(d.text, "X+\nB\n");
        assert_eq!(d.base, f.draft.base);
        // Positive control: the same action queues a Save once the conflict is cleared.
        view.update_in(cx, |v, window, cx| {
            v.source_conflict = None;
            v.editor.flight = true;
            v.save_source(window, cx);
            assert!(v.editor.save.is_some());
            v.editor.save = None;
        });
    }
    #[gpui::test]
    fn typing_after_adoption_uses_merged_base(cx: &mut TestAppContext) {
        let f = Fixture::new();
        let saved = f.ack();
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&f.workspace, &f.store, window, cx)
        });
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| {
            install(v, &f, &saved, window, cx);
            let epoch = v.editor.input_epoch;
            apply(v, &f, &saved, epoch, window, cx);
            v.source.reset("X+\nY\n", window, cx);
            v.editor_input_changed();
            v.editor_protect(window, cx);
        });
        cx.run_until_parked();
        let d = f.store.list().unwrap().drafts.pop().unwrap();
        assert_eq!(d.text, "X+\nY\n");
        assert_eq!(decode_source(&d.base).unwrap(), "X\nY\n");
    }
    #[gpui::test]
    fn changed_epoch_or_durable_generation_refuses_late_adoption(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        for durable in [false, true] {
            let f = Fixture::new();
            let saved = f.ack();
            let (view, cx) = cx.add_window_view(|window, cx| {
                BrainView::new_managed_test(&f.workspace, &f.store, window, cx)
            });
            cx.run_until_parked();
            view.update_in(cx, |v, window, cx| {
                install(v, &f, &saved, window, cx);
                let epoch = v.editor.input_epoch;
                if durable {
                    let newer = f
                        .store
                        .update(saved.draft.as_ref().unwrap(), "Another window\nB\n")
                        .unwrap();
                    v.editor.records = vec![newer];
                } else {
                    v.editor_input_changed();
                }
                apply(v, &f, &saved, epoch, window, cx);
                assert_eq!(v.source_snapshot, Some(f.draft.base.clone()));
                assert_eq!(v.source.value(cx).as_ref(), "X\nB\n");
                assert!(v.source_conflict.is_some());
                if durable {
                    assert!(v.editor.error.is_some());
                }
                v.save_source(window, cx);
                assert!(v.editor.save.is_none());
            });
            cx.run_until_parked();
            let d = f.store.list().unwrap().drafts.pop().unwrap();
            assert_eq!(d.base, f.draft.base);
            assert_eq!(
                d.text,
                if durable {
                    "Another window\nB\n"
                } else {
                    "X\nB\n"
                }
            );
        }
    }
    #[gpui::test]
    fn close_during_auto_save_waits_for_merged_adoption_and_retirement(cx: &mut TestAppContext) {
        let f = Fixture::new();
        let saved = f.ack();
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&f.workspace, &f.store, window, cx)
        });
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| {
            install(v, &f, &saved, window, cx);
            v.editor.sending = Some(f.original.clone());
            v.editor.flight = true;
            v.busy = true;
            assert!(!v.editor_request_close(window, cx));
            assert!(v.editor_closing());
            v.editor.flight = false;
            v.busy = false;
            v.editor.sending = None;
            let epoch = v.editor.input_epoch;
            apply(v, &f, &saved, epoch, window, cx);
            assert_eq!(v.source.value(cx).as_ref(), "X\nY\n");
            assert!(v.editor.flight);
        });
        cx.run_until_parked();
        assert!(f.store.list().unwrap().drafts.is_empty());
    }
    #[gpui::test]
    fn restart_pending_child_reconfirms_and_retries_without_conflict_refresh(
        cx: &mut TestAppContext,
    ) {
        let f = Fixture::new();
        let mut calls = 0;
        let lost = save_attempt_with_mode(&f.store, &f.draft, &f.original, SaveMode::Fresh, |_| {
            calls += 1;
            if calls == 1 {
                Ok(json!({"source_conflict": f.conflict}))
            } else {
                Err("Child committed; reply lost before restart".into())
            }
        });
        assert!(lost.result.is_err());
        let pending = lost.inventory.unwrap().drafts.pop().unwrap();
        let child = pending.pending_save.clone().unwrap();
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&f.workspace, &f.store, window, cx)
        });
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| {
            v.expected_workspace = Some(f.workspace.clone());
            v.editor.store = Some(f.store.clone());
            v.load_source(f.draft.base.clone(), window, cx);
            v.busy = false;
            v.editor_restore(pending.clone(), window, cx);
        });
        cx.run_until_parked();
        view.update_in(cx, |v, _, cx| {
            assert!(
                v.editor.error.is_none(),
                "pending recovery must not try source_conflict refresh: {:?}",
                v.editor.error
            );
            assert!(v.editor.confirmed);
            assert!(!v.editor.flight);
            assert_eq!(v.editor.active.as_ref(), Some(&pending));
            assert_eq!(v.source.value(cx).as_ref(), pending.text);
        });
        assert_eq!(f.store.list().unwrap().drafts, vec![pending.clone()]);
        calls = 0;
        let retried =
            save_attempt_with_mode(&f.store, &pending, &child, SaveMode::Recovery, |wire| {
                calls += 1;
                assert_eq!(wire, child);
                Ok(receipt(&wire))
            });
        assert_eq!(calls, 1);
        assert!(retried.result.unwrap().written);
    }

    #[gpui::test]
    fn ordinary_restored_conflict_still_requires_refresh(cx: &mut TestAppContext) {
        let f = Fixture::new();
        let result =
            save_attempt_with_mode(&f.store, &f.draft, &f.original, SaveMode::Recovery, |_| {
                Ok(json!({"source_conflict": f.conflict}))
            });
        let conflict = result.result.unwrap().draft.unwrap();
        assert!(conflict.pending_save.is_none());
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&f.workspace, &f.store, window, cx)
        });
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| {
            v.expected_workspace = Some(f.workspace.clone());
            v.editor.store = Some(f.store.clone());
            v.load_source(f.draft.base.clone(), window, cx);
            v.busy = false;
            v.editor_restore(conflict.clone(), window, cx);
        });
        cx.run_until_parked();
        view.update_in(cx, |v, _, _| {
            // Positive control: the unavailable endpoint makes the ordinary
            // conflict's required refresh observable, rather than silently skipped.
            assert!(v
                .editor
                .error
                .as_ref()
                .unwrap()
                .starts_with("Conflict refresh failed:"));
            assert!(!v.editor.confirmed);
        });
        assert_eq!(f.store.list().unwrap().drafts, vec![conflict]);
    }
}
