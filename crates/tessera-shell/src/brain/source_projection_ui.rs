//! Managed source presentation and latest-only background classification.
use super::source_projection::CachedProvider;
use super::*;
use gpui_component::input::projection::{
    ProjectionProvider, SourceMutation, SourceMutationReason, SourceSnapshot, SourceStamp,
};

#[derive(Clone, PartialEq)]
struct Binding {
    entity: EntityId,
    workspace: Value,
    path: String,
    stamp: SourceStamp,
}
#[derive(Default)]
pub(super) struct SourceProjectionUi {
    pub live: bool,
    in_flight: bool,
    queued: bool,
    accepted: Option<(Binding, Arc<CachedProvider>)>,
    observed: Option<SourceStamp>,
    pub clipboard_error: Option<String>,
}
impl SourceProjectionUi {
    pub(super) fn pending(&self) -> (bool, bool) {
        (self.in_flight, self.queued)
    }
}

impl BrainView {
    pub(super) fn source_readonly(&self) -> bool {
        !self.source_editable
            || self.source_loading
            || self.busy
            || self.editor_closing()
            || self.editor_guarded_save()
    }
    pub(super) fn sync_source_policy(&self, cx: &mut App) {
        if self.source.managed().is_some() {
            self.source.set_readonly(self.source_readonly(), cx);
        }
    }
    pub(super) fn managed_source_changed(
        &mut self,
        event: SourceMutation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.source_find_changed(event, window, cx);
        if event.reason == SourceMutationReason::Reset
            || self.source.stamp(cx) != Some(event.stamp)
            || self.source_projection.observed == Some(event.stamp)
        {
            return;
        }
        self.source_projection.observed = Some(event.stamp);
        self.editor_input_changed();
        self.schedule_preview(window, cx);
        self.editor_protect(window, cx);
        cx.notify();
    }
    fn projection_snapshot(&self, cx: &App) -> Option<(Binding, SourceSnapshot)> {
        let editor = self.source.managed()?.read(cx);
        if !self.source_editable || editor.text().len() > tessera_core::source_classifier::MAX_BYTES
        {
            return None;
        }
        let binding = Binding {
            entity: self.source.entity_id(),
            workspace: self.expected_workspace.clone()?,
            path: self.source_snapshot.as_ref()?["path"].as_str()?.to_owned(),
            stamp: editor.source_stamp(),
        };
        Some((
            binding,
            SourceSnapshot {
                stamp: editor.source_stamp(),
                text: Arc::from(editor.value().as_ref()),
            },
        ))
    }
    pub(super) fn schedule_source_projection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.source.managed().cloned() else {
            return;
        };
        let Some((binding, source)) = self.projection_snapshot(cx) else {
            self.source_projection.accepted = None;
            editor.update(cx, |state, cx| state.set_projection_provider(None, cx));
            return;
        };
        if self.source_projection.in_flight {
            self.source_projection.queued = true;
            return;
        }
        self.source_projection.in_flight = true;
        cx.spawn_in(window, async move |this, cx| {
            let provider = cx
                .background_executor()
                .spawn(async move { Arc::new(CachedProvider::classify(source)) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.source_projection.in_flight = false;
                if let Some((current, snapshot)) = this.projection_snapshot(cx) {
                    if current == binding && snapshot.text == provider.source().text {
                        this.source_projection.accepted = Some((binding, provider.clone()));
                        if this.source_projection.live && !this.source_find.active() {
                            editor.update(cx, |state, cx| {
                                state.set_projection_provider(Some(provider), cx)
                            });
                        }
                    }
                }
                if std::mem::take(&mut this.source_projection.queued) {
                    this.schedule_source_projection(window, cx);
                }
                this.restore_source_navigation(window, cx);
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn current_source_links(
        &self,
        cx: &App,
    ) -> Option<Vec<tessera_core::source_classifier::NoteLink>> {
        if self.source_projection.in_flight || self.source_projection.queued {
            return None;
        }
        let (binding, snapshot) = self.projection_snapshot(cx)?;
        let (accepted, provider) = self.source_projection.accepted.as_ref()?;
        (accepted == &binding && provider.source().text == snapshot.text)
            .then(|| provider.note_links().to_vec())
    }
    pub(super) fn source_projection_ready(&self) -> bool {
        !self.source_projection.in_flight && !self.source_projection.queued
    }
    pub(super) fn toggle_source_projection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.source.managed().is_none() || self.source_find.active() {
            return;
        }
        self.cancel_open_link_chooser();
        self.source_projection.live = !self.source_projection.live;
        self.apply_source_projection(cx);
        self.source.focus(window, cx);
        cx.notify();
    }
    pub(super) fn apply_source_projection(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.source.managed().cloned() else {
            return;
        };
        let provider = if self.source_projection.live && !self.source_find.active() {
            self.projection_snapshot(cx)
                .and_then(|(current, snapshot)| {
                    self.source_projection
                        .accepted
                        .as_ref()
                        .filter(|(binding, provider)| {
                            *binding == current && provider.source().text == snapshot.text
                        })
                        .map(|(_, provider)| provider.clone() as Arc<dyn ProjectionProvider>)
                })
        } else {
            None
        };
        editor.update(cx, |state, cx| state.set_projection_provider(provider, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use sha2::{Digest, Sha256};

    #[gpui::test]
    fn latest_bound_classification_and_oversize_fallback_preserve_exact_source(
        cx: &mut TestAppContext,
    ) {
        let dir = std::env::temp_dir().join(format!("tessera-projection-ui-{}", uuid()));
        let workspace = json!({"brain_id":uuid(),"root":"/isolated/projection-ui","records_dir":"records","managed":true});
        let store = editor_recovery::EditorRecovery::at(dir.clone(), &workspace).unwrap();
        let snapshot = |value: &str| json!({"schema":SCHEMA,"brain_id":workspace["brain_id"],"path":"notes/source.md","revision":format!("sha256:{:x}",Sha256::digest(value.as_bytes())),"content_base64":STANDARD.encode(value),"media_type":"text/markdown"});
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&workspace, &store, window, cx)
        });
        cx.run_until_parked();
        let entity = view.update_in(cx, |v, window, cx| {
            assert!(!v.source_projection.live);
            v.load_source(snapshot("# first\r\n"), window, cx);
            assert!(v.source_projection.in_flight);
            v.load_source(snapshot("# second\r\n"), window, cx);
            v.load_source(snapshot("# first\r\n"), window, cx);
            v.source_snapshot.as_mut().unwrap()["path"] = json!("notes/latest.md");
            v.schedule_source_projection(window, cx);
            assert!(v.source_projection.queued);
            v.toggle_source_projection(window, cx);
            v.source.entity_id()
        });
        cx.run_until_parked();
        view.update_in(cx, |v, _, cx| {
            assert!(!v.source_projection.in_flight && !v.source_projection.queued);
            let (binding, provider) = v.source_projection.accepted.as_ref().unwrap();
            assert_eq!(binding.entity, entity);
            assert_eq!(binding.path, "notes/latest.md");
            assert_eq!(Some(binding.stamp), v.source.stamp(cx));
            assert_eq!(provider.source().text.as_ref(), v.source.value(cx).as_ref());
            assert!(
                store.list().unwrap().drafts.is_empty(),
                "presentation never protects source"
            );
        });
        for len in [65536, 65537, 256 * 1024] {
            let value = format!("\u{feff}# {}\r\n", "x".repeat(len - 7));
            assert_eq!(value.len(), len);
            view.update_in(cx, |v, window, cx| {
                v.load_source(snapshot(&value), window, cx);
                assert_eq!(v.source.entity_id(), entity);
                assert!(v.source_editable);
                assert_eq!(v.source.value(cx).as_bytes(), value.as_bytes());
                assert_eq!(v.projection_snapshot(cx).is_some(), len <= 65536);
                v.toggle_source_projection(window, cx);
                v.toggle_source_projection(window, cx);
            });
            cx.run_until_parked();
            view.update_in(cx, |v, _, cx| {
                assert_eq!(v.source_projection.accepted.is_some(), len <= 65536);
                assert_eq!(v.source.value(cx).as_bytes(), value.as_bytes());
                let request = source_write(
                    v.source_snapshot.as_ref().unwrap(),
                    &v.source.value(cx),
                    &uuid(),
                );
                assert_eq!(
                    STANDARD
                        .decode(text(&request["request"]["content_base64"]))
                        .unwrap(),
                    value.as_bytes()
                );
                assert!(store.list().unwrap().drafts.is_empty());
            });
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
