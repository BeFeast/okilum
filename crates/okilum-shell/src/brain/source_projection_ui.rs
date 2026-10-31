//! Managed source presentation and latest-only background classification.
use super::source_projection::CachedProvider;
use super::*;
use crate::source_presentation::navigation_inventory as nav;
use gpui_component::input::projection::{
    ProjectionProvider, SourceMutation, SourceMutationReason, SourceSnapshot, SourceStamp,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

#[derive(Clone, PartialEq)]
struct Binding {
    entity: EntityId,
    workspace: Value,
    path: String,
    stamp: SourceStamp,
}
struct Navigation {
    entity: EntityId,
    owner: nav::Owner,
    session: RefCell<nav::Session>,
}
impl Navigation {
    fn new(entity: EntityId) -> Self {
        let owner = nav::Owner::new(entity);
        Self {
            entity,
            session: RefCell::new(nav::Session::new(owner.clone())),
            owner,
        }
    }
    fn pending(&self, key: &nav::RequestKey, input: Option<&nav::Input>) -> bool {
        self.session
            .try_borrow()
            .is_ok_and(|session| session.is_pending(key, input))
    }
}
struct Queued {
    binding: Binding,
    input: nav::Input,
    request: nav::Request,
}
struct Flight {
    key: nav::RequestKey,
    finished: Cell<bool>,
}
/// Created BEFORE spawning: drop/cancellation before first poll settles only the
/// captured physical flight, even if its weak view/window can no longer update.
struct FlightGuard(Rc<Flight>);
impl FlightGuard {
    fn finish(&self) {
        self.0.finished.set(true);
    }
}
impl Drop for FlightGuard {
    fn drop(&mut self) {
        self.finish();
    }
}
#[derive(Default)]
pub(super) struct SourceProjectionUi {
    pub live: bool,
    running: Option<Rc<Flight>>,
    foreground: Option<gpui::Task<()>>,
    queued: Option<Queued>,
    navigation: Option<Navigation>,
    accepted: Option<(Binding, Arc<CachedProvider>)>,
    observed: Option<SourceStamp>,
    pub clipboard_error: Option<String>,
    #[cfg(test)]
    worker_reports: Vec<okilum_core::open_diagnostics::Report>,
}
impl SourceProjectionUi {
    pub(super) fn pending(&self) -> (bool, bool) {
        (
            self.running
                .as_ref()
                .is_some_and(|flight| !flight.finished.get()),
            self.queued.is_some(),
        )
    }
    fn invalidate_navigation(&mut self) {
        if let Some(nav) = &mut self.navigation {
            let _ = nav.session.get_mut().begin(None);
        }
        self.queued = None;
        self.accepted = None;
    }
    fn ensure_owner(&mut self, entity: Option<EntityId>) {
        if self.navigation.as_ref().map(|nav| nav.entity) == entity {
            return;
        }
        self.invalidate_navigation();
        // Retired jobs retain their own marker/guard. Never reuse it for a new owner.
        self.foreground = None;
        self.running = None;
        self.navigation = entity.map(Navigation::new);
    }
    fn owns(&self, flight: &Rc<Flight>) -> bool {
        self.running
            .as_ref()
            .is_some_and(|current| Rc::ptr_eq(current, flight))
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
        if !self.source_editable || editor.text().len() > okilum_core::source_classifier::MAX_BYTES
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
    fn navigation_snapshot(&self, cx: &App) -> Option<(Binding, nav::Input)> {
        let (binding, source) = self.projection_snapshot(cx)?;
        let nav = self.source_projection.navigation.as_ref()?;
        if nav.entity != binding.entity {
            return None;
        }
        let input = nav::Input {
            binding: nav::Binding {
                owner: nav.owner.clone(),
                workspace: binding.workspace.clone(),
                path: binding.path.clone(),
            },
            source,
        };
        Some((binding, input))
    }
    pub(super) fn invalidate_source_navigation_inventory(&mut self) {
        self.source_projection.invalidate_navigation();
    }
    pub(super) fn schedule_source_projection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = self.source.managed().map(|_| self.source.entity_id());
        self.source_projection.ensure_owner(entity);
        let captured = self.navigation_snapshot(cx);
        // Supersede BEFORE unavailable/running returns; a denied attempt has no
        // replacement job but must still revoke the old navigation admission.
        let request = self.source_projection.navigation.as_mut().and_then(|nav| {
            nav.session
                .get_mut()
                .begin(captured.as_ref().map(|(_, input)| input.clone()))
                .ok()
        });
        self.source_projection.queued = None;
        let Some(((binding, input), request)) = captured.zip(request) else {
            self.source_projection.accepted = None;
            if let Some(editor) = self.source.managed().cloned() {
                editor.update(cx, |state, cx| state.set_projection_provider(None, cx));
            }
            return;
        };
        // Ordinary valid edits KEEP the selected legacy provider while it remaps.
        self.source_projection.queued = Some(Queued {
            binding,
            input,
            request,
        });
        self.start_source_projection(window, cx);
    }
    fn start_source_projection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.source_projection.pending().0 {
            return;
        }
        if let Some(task) = self.source_projection.foreground.take() {
            task.detach();
        }
        self.source_projection.running = None; // A canceled/settled owned flight cannot stay busy.
        let Some(job) = self.source_projection.queued.take() else {
            return;
        };
        let live = self.navigation_snapshot(cx);
        let key = job.request.key();
        let pending = self
            .source_projection
            .navigation
            .as_ref()
            .is_some_and(|nav| nav.pending(&key, live.as_ref().map(|(_, input)| input)));
        if !pending {
            // A foreground live-query refusal may have canceled this queued job.
            // Recapture ONCE as a real new attempt, not by adopting obsolete work.
            self.schedule_source_projection(window, cx);
            return;
        }
        let Some(editor) = self.source.managed().cloned() else {
            return;
        };
        let flight = Rc::new(Flight {
            key,
            finished: Cell::new(false),
        });
        let guard = FlightGuard(flight.clone());
        self.source_projection.running = Some(flight.clone());
        let Queued {
            binding,
            input,
            request,
        } = job;
        self.source_projection.foreground = Some(cx.spawn_in(window, {
            let module_access =
                crate::reader_features::access(crate::reader_features::Feature::SecondBrain, cx);
            async move |this, cx| {
                okilum_t3::scope_network(module_access, async move {
                    // Only immutable owned input/request cross to the worker. The Rc
                    // marker, guard, native entity and UI context stay foreground.
                    let produced = cx
                        .background_executor()
                        .spawn(okilum_t3::scope_current_network(async move {
                            #[cfg(test)]
                            {
                                okilum_core::open_diagnostics::capture(|| {
                                    CachedProvider::classify_navigation(request, input)
                                })
                            }
                            #[cfg(not(test))]
                            {
                                CachedProvider::classify_navigation(request, input)
                            }
                        }))
                        .await;
                    #[cfg(test)]
                    let (produced, report) = produced;
                    guard.finish();
                    let _ = this.update_in(cx, |this, window, cx| {
                        if !this.source_projection.owns(&flight) {
                            return;
                        }
                        if let Some(task) = this.source_projection.foreground.take() {
                            task.detach();
                        }
                        this.source_projection.running = None;
                        #[cfg(test)]
                        this.source_projection.worker_reports.push(report);
                        let live = this.navigation_snapshot(cx);
                        let pending =
                            this.source_projection
                                .navigation
                                .as_ref()
                                .is_some_and(|nav| {
                                    nav.pending(&flight.key, live.as_ref().map(|(_, input)| input))
                                });
                        // Do NOT call destructive adopt on a stale job: the newer queued
                        // request must remain pending and retain its own admission chance.
                        if pending {
                            match (produced, live) {
                                (Ok((provider, completion)), Some((current, input)))
                                    if current == binding =>
                                {
                                    let admitted =
                                        this.source_projection.navigation.as_mut().and_then(
                                            |nav| {
                                                nav.session
                                                    .get_mut()
                                                    .adopt(completion, Some(&input))
                                                    .ok()
                                            },
                                        );
                                    if admitted.is_some() {
                                        let provider = Arc::new(provider);
                                        this.source_projection.accepted =
                                            Some((current, provider.clone()));
                                        if this.source_projection.live && !this.source_find.active()
                                        {
                                            editor.update(cx, |state, cx| {
                                                state.set_projection_provider(Some(provider), cx)
                                            });
                                        }
                                    }
                                }
                                _ => this.invalidate_source_navigation_inventory(),
                            }
                        }
                        this.start_source_projection(window, cx);
                        if this.source_projection_ready() {
                            this.restore_source_navigation(window, cx);
                        }
                        cx.notify();
                    });
                })
                .await
            }
        }));
    }
    pub(super) fn current_source_links(
        &self,
        cx: &App,
    ) -> Option<Vec<okilum_core::source_classifier::NoteLink>> {
        let live = self.navigation_snapshot(cx);
        let nav = self.source_projection.navigation.as_ref()?;
        // Do the fail-closed live check even when physical work is pending.
        let inventory = nav
            .session
            .try_borrow_mut()
            .ok()?
            .current(live.as_ref().map(|(_, input)| input))?;
        if self.source_projection.pending() != (false, false) {
            return None;
        }
        Some(inventory.view(&live?.1).ok()?.legacy_links().to_vec())
    }
    pub(super) fn source_projection_ready(&self) -> bool {
        self.source_projection.pending() == (false, false)
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
        let dir = std::env::temp_dir().join(format!("okilum-projection-ui-{}", uuid()));
        let workspace = json!({"brain_id":uuid(),"root":"/isolated/projection-ui","records_dir":"records","managed":true});
        let store = editor_recovery::EditorRecovery::at(dir.clone(), &workspace).unwrap();
        let snapshot = |value: &str| json!({"schema":SCHEMA,"brain_id":workspace["brain_id"],"path":"notes/source.md","revision":format!("sha256:{:x}",Sha256::digest(value.as_bytes())),"content_base64":STANDARD.encode(value),"media_type":"text/markdown"});
        cx.update(crate::test_init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&workspace, &store, window, cx)
        });
        cx.run_until_parked();
        let entity = view.update_in(cx, |v, window, cx| {
            assert!(!v.source_projection.live);
            v.load_source(snapshot("# first\r\n"), window, cx);
            assert!(v.source_projection.pending().0);
            v.load_source(snapshot("# second\r\n"), window, cx);
            v.load_source(snapshot("# first\r\n"), window, cx);
            v.source_snapshot.as_mut().unwrap()["path"] = json!("notes/latest.md");
            v.schedule_source_projection(window, cx);
            assert!(v.source_projection.pending().1);
            v.toggle_source_projection(window, cx);
            v.source.entity_id()
        });
        cx.run_until_parked();
        view.update_in(cx, |v, _, cx| {
            assert_eq!(v.source_projection.pending(), (false, false));
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

    fn data(workspace: &Value, value: &str, path: &str) -> Value {
        json!({"schema":SCHEMA,"brain_id":workspace["brain_id"],"path":path,
            "revision":format!("sha256:{:x}",Sha256::digest(value.as_bytes())),
            "content_base64":STANDARD.encode(value),"media_type":"text/markdown"})
    }
    fn fixture(
        cx: &mut TestAppContext,
    ) -> (
        Value,
        tempfile::TempDir,
        Entity<BrainView>,
        &mut gpui::VisualTestContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = json!({"brain_id":uuid(),"root":"/isolated/projection-consumer","records_dir":"records","managed":true});
        let store = editor_recovery::EditorRecovery::at(dir.path().to_owned(), &workspace).unwrap();
        cx.update(crate::test_init);
        let (view, visual) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&workspace, &store, window, cx)
        });
        (workspace, dir, view, visual)
    }

    #[gpui::test]
    fn real_worker_preserves_latest_queued_admission_and_counts_one_navigation_producer(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _dir, view, visual) = fixture(cx);
        let first = "[first](first.md 'title')\n";
        let second = "[second](second.md)\n";
        let latest = "[latest](latest.md) and [[note|alias]]\n";
        view.update_in(visual, |v, window, cx| {
            v.load_source(data(&workspace, first, "notes/first.md"), window, cx);
            assert_eq!(v.source_projection.pending(), (true, false));
            v.load_source(data(&workspace, second, "notes/second.md"), window, cx);
            v.load_source(data(&workspace, latest, "notes/latest.md"), window, cx);
            v.expected_workspace.as_mut().unwrap()["view_context"] = json!({"version":2});
            v.schedule_source_projection(window, cx);
            assert_eq!(v.source_projection.pending(), (true, true));
            assert!(v.current_source_links(cx).is_none());
        });
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            assert_eq!(v.source_projection.pending(), (false, false));
            assert_eq!(
                v.source_projection.accepted.as_ref().unwrap().0.path,
                "notes/latest.md"
            );
            let expected = CachedProvider::classify(SourceSnapshot {
                stamp: v.source.stamp(cx).unwrap(),
                text: Arc::from(latest),
            });
            assert_eq!(v.current_source_links(cx).unwrap(), expected.note_links());
            let reports = &v.source_projection.worker_reports;
            assert_eq!(
                reports.len(),
                2,
                "old physical job plus newest queue; no disconnected parse worker"
            );
            assert_eq!(reports[0].counts["link_parse_bytes"], first.len() as u64);
            assert_eq!(reports[1].counts["link_parse_bytes"], latest.len() as u64);
            assert!(reports.iter().all(|r| r.phases["parse"].calls > 0));
            let (_, report) = okilum_core::open_diagnostics::capture(|| {
                assert!(v.current_source_links(cx).is_some());
                assert!(v.current_source_links(cx).is_some());
            });
            assert!(!report.counts.contains_key("link_parse_bytes"));
            assert!(!report.phases.contains_key("parse"));
        });
    }

    #[gpui::test]
    fn refused_then_restored_actual_input_does_not_resurrect_a_pending_worker(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _dir, view, visual) = fixture(cx);
        let text = "[first](first.md)\n";
        view.update_in(visual, |v, window, cx| {
            v.load_source(data(&workspace, text, "notes/first.md"), window, cx);
            v.source_editable = false;
            v.schedule_source_projection(window, cx); // Real refused consumer attempt.
            v.source_editable = true; // Exact same stamp/bytes/path, with no new scheduling input.
            assert!(v.current_source_links(cx).is_none());
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert!(v.source_projection.accepted.is_none());
            assert!(
                v.current_source_links(cx).is_none(),
                "old completion cannot undo an intervening refusal"
            );
            v.schedule_source_projection(window, cx);
        });
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            assert_eq!(v.current_source_links(cx).unwrap()[0].target, "first.md")
        });
    }

    #[gpui::test]
    fn current_consumer_live_path_workspace_and_source_checks_are_fail_closed(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _dir, view, visual) = fixture(cx);
        let text = "[first](first.md)\n";
        view.update_in(visual, |v, window, cx| {
            v.load_source(data(&workspace, text, "notes/first.md"), window, cx)
        });
        visual.run_until_parked();
        for changed in ["path", "workspace", "bytes"] {
            view.update_in(visual, |v, window, cx| {
                let old_workspace = v.expected_workspace.clone();
                let old_snapshot = v.source_snapshot.clone();
                assert!(v.current_source_links(cx).is_some());
                match changed {
                    "path" => {
                        v.source_snapshot.as_mut().unwrap()["path"] = json!("notes/foreign.md")
                    }
                    "workspace" => {
                        v.expected_workspace.as_mut().unwrap()["identity_detail"] = json!("changed")
                    }
                    _ => v.source.reset("[other](other.md)\n", window, cx),
                }
                assert!(v.current_source_links(cx).is_none());
                v.expected_workspace = old_workspace;
                v.source_snapshot = old_snapshot;
                if changed == "bytes" {
                    v.source.reset(text, window, cx);
                }
                assert!(
                    v.current_source_links(cx).is_none(),
                    "live-query refusal persists after restoring visible data"
                );
                v.schedule_source_projection(window, cx);
            });
            visual.run_until_parked();
        }
    }

    #[gpui::test]
    fn actual_goal_source_clear_revokes_inventory_before_late_completion(cx: &mut TestAppContext) {
        let (workspace, _dir, view, visual) = fixture(cx);
        view.update_in(visual, |v, window, cx| {
            v.load_source(data(&workspace, "[first](first.md)\n", "notes/first.md"), window, cx);
            assert!(v.source_projection.pending().0);
            let reply = json!({"goal":{"id":"different","title":"Different"},"goals":[{"id":"different"}],
                "selected_source_paths":[],"selected_conversation_id":null});
            v.finish_batch(vec![("snapshot".into(), Ok(reply))], false, v.request_generation, window, cx);
            assert!(v.source_snapshot.is_none());
            assert!(v.source_projection.queued.is_none());
            assert!(v.current_source_links(cx).is_none());
        });
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            assert!(v.source_projection.accepted.is_none());
            assert!(v.current_source_links(cx).is_none());
            assert!(v.source_projection_ready());
        });
    }

    #[gpui::test]
    fn closing_before_poll_settles_only_the_original_flight_and_reopened_owner_completes(
        cx: &mut TestAppContext,
    ) {
        let (workspace, dir, view, visual) = fixture(cx);
        let text = "[first](first.md)\n";
        let old_flight = view.update_in(visual, |v, window, cx| {
            v.load_source(data(&workspace, text, "notes/first.md"), window, cx);
            v.source_projection.running.as_ref().unwrap().clone()
        });
        assert!(!old_flight.finished.get());
        // No executor drain between creating the physical flight and closing its owner.
        visual.update(|window, _| window.remove_window());
        drop(view);
        let store =
            editor_recovery::EditorRecovery::at(dir.path().join("reopened"), &workspace).unwrap();
        let (reopened, visual) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&workspace, &store, window, cx)
        });
        let new_flight = reopened.update_in(visual, |v, window, cx| {
            v.load_source(data(&workspace, text, "notes/first.md"), window, cx);
            v.source_projection.running.as_ref().unwrap().clone()
        });
        assert!(!Rc::ptr_eq(&old_flight, &new_flight));
        visual.run_until_parked();
        assert!(
            old_flight.finished.get(),
            "drop/weak-update failure settles the captured old marker"
        );
        assert!(new_flight.finished.get());
        reopened.update_in(visual, |v, _, cx| {
            assert_eq!(v.source_projection.pending(), (false, false));
            assert_eq!(v.current_source_links(cx).unwrap()[0].target, "first.md");
        });
    }

    fn same_projection(
        actual: &dyn gpui_component::input::projection::SourceProjection,
        expected: &dyn gpui_component::input::projection::SourceProjection,
    ) {
        use gpui_component::input::projection::{ConcealBias, ProjectedByte, SourceByte};
        assert_eq!(actual.text(), expected.text());
        assert_eq!(actual.styles(), expected.styles());
        assert_eq!(actual.line_scales(), expected.line_scales());
        assert_eq!(actual.blocks(), expected.blocks());
        assert_eq!(
            format!("{:?}", actual.markers()),
            format!("{:?}", expected.markers())
        );
        for byte in 0..=actual.source().text.len() {
            for bias in [ConcealBias::Left, ConcealBias::Right] {
                assert_eq!(
                    actual.to_projected(SourceByte(byte), bias),
                    expected.to_projected(SourceByte(byte), bias)
                );
            }
        }
        for byte in 0..=actual.text().len() {
            for bias in [ConcealBias::Left, ConcealBias::Right] {
                assert_eq!(
                    actual.to_source(ProjectedByte(byte), bias),
                    expected.to_source(ProjectedByte(byte), bias)
                );
            }
        }
        for marker in actual.markers() {
            assert_eq!(
                actual.marker_scope_is_raw(&marker.scope),
                expected.marker_scope_is_raw(&marker.scope)
            );
        }
    }

    #[gpui::test]
    fn actual_unpainted_inputs_keep_legacy_projection_metadata_and_current_bytes(
        cx: &mut TestAppContext,
    ) {
        use gpui_component::input::projection::{
            ActiveSource, ProjectionProvider as _, SourceByte,
        };
        let (workspace, _dir, view, visual) = fixture(cx);
        let original = "# Heading\n\n| a | b |\n| - | - |\n| c | d |\n\n> Quote\n\n**tail** [link](target.md)\n";
        view.update_in(visual, |v, window, cx| {
            v.load_source(data(&workspace, original, "notes/current.md"), window, cx);
            v.toggle_source_projection(window, cx);
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            let selected = v.source_projection.accepted.as_ref().unwrap().1.clone();
            let expected = CachedProvider::classify(selected.source().clone());
            for (inserted, value) in [
                ("x", original.replace("tail", "tailx")),
                ("y", original.replace("tail", "tailxy")),
            ] {
                let before = v.source.value(cx);
                let at = before.find("** [link]").unwrap();
                v.source.managed().unwrap().update(cx, |input, cx| {
                    input.set_selected_range(at..at, cx);
                    input.replace(inserted, window, cx);
                });
                v.schedule_source_projection(window, cx);
                assert!(
                    Arc::ptr_eq(&v.source_projection.accepted.as_ref().unwrap().1, &selected),
                    "valid edits keep the legacy provider selected"
                );
                let source = SourceSnapshot {
                    stamp: v.source.stamp(cx).unwrap(),
                    text: Arc::from(value.clone()),
                };
                let active = ActiveSource {
                    anchor: SourceByte(value.len()),
                    head: SourceByte(value.len()),
                    ..Default::default()
                };
                let actual = selected.compose(&source, &active).unwrap();
                let expected = expected.compose(&source, &active).unwrap();
                same_projection(actual.as_ref(), expected.as_ref());
                assert!(
                    !actual.blocks().is_empty(),
                    "rendered blocks above dirty text remain rendered"
                );
                assert_eq!(v.source.value(cx).as_ref(), value);
                assert!(v.current_source_links(cx).is_none());
            }
        });
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            assert!(v.current_source_links(cx).is_some())
        });
    }
}
