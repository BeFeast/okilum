//! Explicit, discardable merge previews bound to one protected editor generation.
use super::super::merge_preview::{preview_merge, MergePreview};
use super::*;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq)]
struct Binding {
    workspace: Value,
    draft: Draft,
}
#[derive(Default)]
pub(super) struct MergeUi {
    sequence: u64,
    binding: Option<Binding>,
    result: Option<Result<MergePreview, String>>,
    preview: Option<Entity<TextareaState>>,
    flight: bool,
    adopting: bool,
}
fn snapshot_bytes(value: &Value, binding: &Binding) -> Result<Option<Vec<u8>>, String> {
    if value.is_null() {
        return Ok(None);
    }
    if value["brain_id"] != binding.workspace["brain_id"]
        || value["path"] != binding.draft.base["path"]
    {
        return Err("Merge source does not match this workspace and note.".into());
    }
    let bytes = STANDARD
        .decode(text(&value["content_base64"]))
        .map_err(|_| "Invalid merge source bytes.")?;
    if value["revision"] != json!(format!("sha256:{:x}", Sha256::digest(&bytes))) {
        return Err("Merge source revision does not match its exact bytes.".into());
    }
    Ok(Some(bytes))
}
fn calculate(binding: &Binding) -> Result<MergePreview, String> {
    let conflict = binding
        .draft
        .conflict
        .as_ref()
        .ok_or("No retained conflict is available.")?;
    let base = snapshot_bytes(conflict_base(Some(&binding.draft), conflict), binding)?;
    let current = snapshot_bytes(&conflict["current"], binding)?;
    Ok(preview_merge(
        base.as_deref(),
        current.as_deref(),
        binding.draft.text.as_bytes(),
    ))
}
impl BrainView {
    pub(super) fn merge_adoption_pending(&self) -> bool {
        self.editor.merge.adopting
    }
    fn matches_merge(&self, binding: &Binding, cx: &App) -> bool {
        self.expected_workspace.as_ref() == Some(&binding.workspace)
            && self.editor.active.as_ref() == Some(&binding.draft)
            && self.source_snapshot.as_ref() == Some(&binding.draft.base)
            && self.source_conflict == binding.draft.conflict
            && self.source.value(cx).as_ref() == binding.draft.text
    }
    fn merge_binding(&self, cx: &App) -> Result<Binding, String> {
        if self.editor_closing()
            || self.busy
            || self.editor.flight
            || !self.editor.confirmed
            || self.editor.error.is_some()
            || !self.source_editable
        {
            return Err(
                "Wait for the current draft to be Protected before previewing a merge.".into(),
            );
        }
        let draft = self
            .editor
            .active
            .clone()
            .ok_or("No protected draft is available.")?;
        if draft.pending_save.is_some() {
            return Err("Recover the uncertain Save before previewing another draft.".into());
        }
        if draft.conflict.is_none() {
            return Err("No retained source conflict is available.".into());
        }
        let binding = Binding {
            workspace: self
                .expected_workspace
                .clone()
                .ok_or("No bound workspace.")?,
            draft,
        };
        if !self.matches_merge(&binding, cx) {
            return Err("The editor changed. Protect the latest text before previewing.".into());
        }
        Ok(binding)
    }
    pub(super) fn merge_check(&mut self, cx: &App) {
        if self
            .editor
            .merge
            .binding
            .as_ref()
            .is_some_and(|binding| !self.matches_merge(binding, cx))
        {
            self.editor.merge.sequence += 1;
            self.editor.merge.binding = None;
            self.editor.merge.result = Some(Err(
                "The editor or conflict changed. Preview the latest protected draft again.".into(),
            ));
            self.editor.merge.preview = None;
            self.editor.merge.flight = false;
        }
    }
    fn merge_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let binding = match self.merge_binding(cx) {
            Ok(binding) => binding,
            Err(error) => {
                self.editor.merge.result = Some(Err(error));
                cx.notify();
                return;
            }
        };
        self.editor.merge.sequence += 1;
        let sequence = self.editor.merge.sequence;
        self.editor.merge.binding = Some(binding.clone());
        self.editor.merge.result = None;
        self.editor.merge.preview = None;
        self.editor.merge.flight = true;
        cx.spawn_in(window, async move |this, cx| {
            let calculate_binding = binding.clone();
            let result = cx
                .background_executor()
                .spawn(async move { calculate(&calculate_binding) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.editor.merge.sequence != sequence || !this.matches_merge(&binding, cx) {
                    this.merge_check(cx);
                    return;
                }
                this.editor.merge.flight = false;
                if let Ok(MergePreview::Ready { text, .. }) = &result {
                    this.editor.merge.preview = Some(cx.new(|cx| {
                        let mut input = TextareaState::new(window, cx).rows(8).soft_wrap(false);
                        input.set_value(text.clone(), window, cx);
                        input
                    }));
                }
                this.editor.merge.result = Some(result);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn merge_adopt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(binding) = self.editor.merge.binding.clone() else {
            return;
        };
        if self.merge_binding(cx).ok().as_ref() != Some(&binding) {
            self.merge_check(cx);
            return;
        }
        let Some(Ok(MergePreview::Ready { text, .. })) = &self.editor.merge.result else {
            return;
        };
        let candidate = text.clone();
        let store = self.editor_store();
        self.editor.merge.adopting = true;
        self.editor.flight = true;
        self.source_loading = true;
        self.sync_source_policy(cx);
        cx.spawn_in(window,async move |this,cx| {
            let expected=binding.draft.clone();
            let result=cx.background_executor().spawn(async move {store?.update(&expected,&candidate)}).await;
            let _=this.update_in(cx,|this,window,cx| {
                this.editor.flight=false;
                this.source_loading=false; this.sync_source_policy(cx);
                match result {
                    Ok(draft) if this.matches_merge(&binding,cx)=>{
                        this.editor_close_adopted(&binding.draft,&draft,cx);
                        this.source.reset(draft.text.clone(), window, cx);
                        this.editor.active=Some(draft);
                        this.editor.confirmed=true;
                        this.editor.merge=MergeUi::default();
                        this.notice="Merged draft protected. Review it, then Save resolved draft.".into();
                        this.schedule_preview(window,cx);
                    }
                    Ok(_)=>{this.editor.error=Some("The editor changed during merge adoption. Its visible text is retained; recheck recovery before continuing.".into());this.editor.confirmed=false;this.merge_check(cx);}
                    Err(error)=>{this.editor.error=Some(format!("Merged draft could not be protected: {error}"));this.editor.confirmed=false;}
                }
                this.editor.merge.adopting=false;
                this.editor_protect(window,cx);
                cx.notify();
            });
        }).detach();
        cx.notify();
    }
    pub(super) fn merge_panel(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.editor_enabled() || self.source_conflict.is_none() {
            return None;
        }
        self.merge_check(cx);
        let disabled = self.merge_binding(cx).is_err() || self.editor.merge.flight;
        let mut panel = v_flex().gap_2().child(
            super::super::super::brand::control("preview-merged-draft", cx)
                .label(if self.editor.merge.flight {
                    "Preparing merge preview…"
                } else {
                    "Preview merged draft"
                })
                .disabled(disabled)
                .on_click(cx.listener(|this, _, window, cx| this.merge_start(window, cx))),
        );
        if let Some(result) = &self.editor.merge.result {
            match result {
                Ok(MergePreview::Ready { .. }) => {
                    panel=panel.child(div().text_sm().child("Independent text edits can be combined. Review this preview; it has not replaced your draft or saved the note."));
                    if let Some(preview) = &self.editor.merge.preview {
                        panel = panel.child(
                            Textarea::new(preview)
                                .readonly(true)
                                .h(px(180.))
                                .flex_shrink_0(),
                        );
                    }
                    panel = panel.child(
                        super::super::super::brand::control("use-merged-draft", cx)
                            .label("Use merged draft")
                            .disabled(disabled)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.merge_adopt(window, cx)),
                            ),
                    );
                }
                Ok(MergePreview::Manual { reason }) => {
                    panel = panel.child(div().text_sm().child(reason.message()))
                }
                Err(error) => panel = panel.child(div().text_sm().child(error.clone())),
            }
        }
        Some(panel.into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use std::path::PathBuf;
    const ORIGINAL: &str = "\u{feff}# Exact note\r\nChoice: blue\r\nTools: ruler\r\n";
    const PROPOSED: &str = "\u{feff}# Exact note\r\nChoice: green\r\nTools: ruler\r\n";
    const CURRENT: &str = "\u{feff}# Exact note\r\nChoice: blue\r\nTools: tape\r\n";
    const MERGED: &str = "\u{feff}# Exact note\r\nChoice: green\r\nTools: tape\r\n";
    fn fixture() -> (PathBuf, Binding, EditorRecovery) {
        let dir = std::env::temp_dir().join(format!("okilum-merge-ui-{}", uuid()));
        let workspace = json!({"brain_id":uuid(),"root":"/isolated/merge-ui","records_dir":"records","managed":true});
        let snapshot = |text: &str| json!({"schema":SCHEMA,"brain_id":workspace["brain_id"],"path":"notes/exact.md","revision":format!("sha256:{:x}",Sha256::digest(text.as_bytes())),"content_base64":STANDARD.encode(text.as_bytes()),"media_type":"text/markdown"});
        let base = snapshot(ORIGINAL);
        let current = snapshot(CURRENT);
        let store = EditorRecovery::at(dir.clone(), &workspace).unwrap();
        let draft = store.create(&base, PROPOSED).unwrap();
        let request = source_write(&base, PROPOSED, &uuid());
        let retained = store.retain_save(&draft, &request).unwrap();
        let conflict = json!({"base":base,"current":current,"proposed":snapshot(PROPOSED),"conflict":{"conflict_id":request["request"]["operation_id"],"path":"notes/exact.md","expected_revision":base["revision"],"current_revision":current["revision"],"reason":"stale_revision"}});
        let draft = store
            .record_conflict(&retained.id, &request, &conflict)
            .unwrap();
        (dir, Binding { workspace, draft }, store)
    }
    fn install(
        v: &mut BrainView,
        binding: &Binding,
        store: &EditorRecovery,
        window: &mut Window,
        cx: &mut Context<BrainView>,
    ) {
        v.expected_workspace = Some(binding.workspace.clone());
        v.editor.store = Some(store.clone());
        v.load_source(binding.draft.base.clone(), window, cx);
        v.source.reset(binding.draft.text.clone(), window, cx);
        v.editor.active = Some(binding.draft.clone());
        v.editor.confirmed = true;
        v.busy = false;
        v.show_source_conflict(binding.draft.conflict.clone().unwrap(), window, cx);
    }

    #[gpui::test]
    fn automatic_child_conflict_preview_uses_true_visible_base(cx: &mut TestAppContext) {
        let (dir, old_binding, store) = fixture();
        store.discard(&old_binding.draft).unwrap();
        let snapshot = |value: &str| {
            let mut source = old_binding.draft.base.clone();
            source["content_base64"] = json!(STANDARD.encode(value));
            source["revision"] = json!(format!("sha256:{:x}", Sha256::digest(value.as_bytes())));
            source
        };
        let conflict_for = |wire: &Value, current: &str| {
            let mut proposed = wire["base"].clone();
            let bytes = STANDARD
                .decode(text(&wire["request"]["content_base64"]))
                .unwrap();
            proposed["content_base64"] = wire["request"]["content_base64"].clone();
            proposed["revision"] = json!(format!("sha256:{:x}", Sha256::digest(bytes)));
            json!({"base":wire["base"],"proposed":proposed,"current":snapshot(current),"conflict":{
                "conflict_id":wire["request"]["operation_id"],"path":wire["request"]["path"],
                "expected_revision":wire["request"]["expected_revision"],"current_revision":snapshot(current)["revision"],"reason":"stale_revision"}})
        };
        let original = store.create(&snapshot("A\nB\nC\n"), "X\nB\nC\n").unwrap();
        let request = source_write(&original.base, &original.text, &uuid());
        let mut calls = 0;
        let result = save_attempt_with_mode(&store, &original, &request, SaveMode::Fresh, |wire| {
            calls += 1;
            Ok(
                json!({"source_conflict":conflict_for(&wire, if calls == 1 {"A\nY\nC\n"} else {"A\nY\nZ\n"})}),
            )
        });
        assert_eq!(calls, 2);
        let saved = result.result.unwrap();
        assert!(!saved.written);
        // The actual callback preserves the visible original proposal, then
        // protection replaces only the locally retained candidate text.
        let draft = store.update(&saved.draft.unwrap(), &original.text).unwrap();
        assert_eq!(draft.base, snapshot("A\nB\nC\n"));
        assert_eq!(
            draft.conflict.as_ref().unwrap()["base"],
            snapshot("A\nY\nC\n")
        );
        let binding = Binding {
            workspace: old_binding.workspace.clone(),
            draft,
        };
        assert!(
            matches!(calculate(&binding).unwrap(), MergePreview::Ready {text, ..} if text == "X\nY\nZ\n")
        );
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&old_binding.workspace, &store, window, cx)
        });
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| {
            install(v, &binding, &store, window, cx);
            assert_eq!(v.source_base.read(cx).value().as_ref(), "A\nB\nC\n");
            assert_eq!(
                v.source_conflict.as_ref().unwrap()["base"],
                snapshot("A\nY\nC\n")
            );
            v.merge_start(window, cx);
        });
        cx.run_until_parked();
        view.update_in(cx, |v, _, cx| {
            assert_eq!(
                v.editor
                    .merge
                    .preview
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .value()
                    .as_ref(),
                "X\nY\nZ\n"
            );
            assert_eq!(v.source.value(cx).as_ref(), "X\nB\nC\n");
        });
        // A later deliberate resolution explicitly chooses its current CAS base.
        let manual = source_write(
            &binding.draft.conflict.as_ref().unwrap()["current"],
            &binding.draft.text,
            &uuid(),
        );
        let pending = store.retain_save(&binding.draft, &manual).unwrap();
        let manual_conflict = conflict_for(&manual, "A\nY+\nZ\n");
        let retained = store
            .record_conflict(&pending.id, &manual, &manual_conflict)
            .unwrap();
        assert_eq!(
            conflict_base(Some(&retained), &manual_conflict),
            &manual["base"]
        );
        assert_ne!(manual["base"], retained.base);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[gpui::test]
    fn preview_is_separate_and_adoption_waits_for_exact_durability(cx: &mut TestAppContext) {
        let (dir, binding, store) = fixture();
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new_managed_test(&binding.workspace, &store, window, cx);
            v.busy = true;
            v
        });
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| {
            install(v, &binding, &store, window, cx);
            v.merge_start(window, cx);
            assert_eq!(v.source.value(cx).as_ref(), PROPOSED);
            assert!(v.editor.merge.flight);
        });
        cx.run_until_parked();
        assert_eq!(
            store.list().unwrap().drafts[0],
            binding.draft,
            "computation never adopts or saves"
        );
        view.update_in(cx,|v,window,cx| {
            assert!(matches!(&v.editor.merge.result,Some(Ok(MergePreview::Ready{text,..})) if text==MERGED));
            assert_eq!(v.source.value(cx).as_ref(),PROPOSED);
            v.merge_adopt(window,cx);
            assert!(v.editor.flight && v.source_loading);
            assert_eq!(v.source.value(cx).as_ref(),PROPOSED,"visible draft cannot change before durability acknowledgement");
            v.resolve_source(window,cx);
            assert!(v.editor.flight && v.source_loading,"resolution cannot release the adoption freeze");
            let request=source_write(&binding.draft.conflict.as_ref().unwrap()["current"],PROPOSED,&uuid());
            v.editor_save(request,window,cx);
            assert!(v.editor.flight && v.source_loading,"refused Save cannot release another job's freeze");
            assert!(v.editor.save.is_none());
        });
        cx.run_until_parked();
        let durable = store.list().unwrap().drafts.pop().unwrap();
        assert_eq!(durable.text, MERGED);
        assert_eq!(durable.conflict, binding.draft.conflict);
        assert_eq!(durable.base, binding.draft.base);
        assert!(durable.pending_save.is_none());
        view.update_in(cx, |v, _, cx| {
            assert_eq!(v.source.value(cx).as_ref(), MERGED);
            assert!(v.editor.confirmed);
            assert!(!v.editor.flight);
            assert!(v.editor.save.is_none());
            assert!(v.pending_source_write.is_none());
        });
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[gpui::test]
    fn changed_input_invalidates_late_preview_and_pending_save_blocks_replacement(
        cx: &mut TestAppContext,
    ) {
        let (dir, binding, store) = fixture();
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new_managed_test(&binding.workspace, &store, window, cx);
            v.busy = true;
            v
        });
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| {
            install(v, &binding, &store, window, cx);
            v.merge_start(window, cx);
            v.source_conflict.as_mut().unwrap()["current"]["revision"] = json!("sha256:changed");
            v.merge_check(cx);
        });
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| {
            assert!(v.editor.merge.preview.is_none());
            assert!(v.editor.merge.binding.is_none());
            assert!(!v.editor.merge.flight);
            v.merge_adopt(window, cx);
        });
        assert_eq!(store.list().unwrap().drafts[0], binding.draft);
        let request = source_write(
            &binding.draft.conflict.as_ref().unwrap()["current"],
            PROPOSED,
            &uuid(),
        );
        let pending = store.retain_save(&binding.draft, &request).unwrap();
        view.update_in(cx, |v, window, cx| {
            install(
                v,
                &Binding {
                    workspace: binding.workspace.clone(),
                    draft: pending.clone(),
                },
                &store,
                window,
                cx,
            );
            assert!(v.merge_binding(cx).unwrap_err().contains("uncertain Save"));
            v.merge_start(window, cx);
            assert!(v.editor.merge.binding.is_none());
            assert!(v.editor.save.is_none());
        });
        assert_eq!(store.list().unwrap().drafts[0], pending);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[gpui::test]
    fn another_window_generation_prevents_candidate_adoption_without_losing_visible_draft(
        cx: &mut TestAppContext,
    ) {
        let (dir, binding, store) = fixture();
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new_managed_test(&binding.workspace, &store, window, cx);
            v.busy = true;
            v
        });
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| {
            install(v, &binding, &store, window, cx);
            v.merge_start(window, cx);
        });
        cx.run_until_parked();
        let newer = store
            .update(&binding.draft, "Newer text from another window\r\n")
            .unwrap();
        view.update_in(cx, |v, window, cx| {
            v.merge_adopt(window, cx);
            assert_eq!(v.source.value(cx).as_ref(), PROPOSED);
        });
        cx.run_until_parked();
        view.update_in(cx, |v, _, cx| {
            assert_eq!(v.source.value(cx).as_ref(), PROPOSED);
            assert!(v.editor.error.is_some());
            assert!(!v.editor.confirmed);
            assert!(!v.source_loading);
        });
        assert_eq!(store.list().unwrap().drafts[0], newer);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[gpui::test]
    fn close_started_during_explicit_adoption_waits_for_the_adopted_generation(
        cx: &mut TestAppContext,
    ) {
        let (dir, binding, store) = fixture();
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut v = BrainView::new_managed_test(&binding.workspace, &store, window, cx);
            v.busy = true;
            v
        });
        view.update_in(cx, |v, window, cx| {
            install(v, &binding, &store, window, cx);
            v.merge_start(window, cx);
        });
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| {
            v.merge_adopt(window, cx);
            assert!(v.editor.flight && v.merge_adoption_pending());
            assert_eq!(v.source.value(cx).as_ref(), PROPOSED);
            assert!(!v.editor_request_close(window, cx));
            assert!(v.editor_closing());
        });
        assert_eq!(cx.windows().len(), 1);
        cx.run_until_parked();
        assert!(cx.windows().is_empty());
        let durable = store.list().unwrap().drafts.pop().unwrap();
        assert_eq!(durable.text, MERGED);
        assert_eq!(durable.conflict, binding.draft.conflict);
        assert!(durable.pending_save.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
