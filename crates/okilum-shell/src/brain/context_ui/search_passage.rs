//! A search passage retains the existing form entities; fingerprints only reject stale work.
use super::*;

#[derive(Clone)]
pub(in crate::brain) struct ContextOrigin {
    pub endpoint: SocketAddr,
    pub workspace: Value,
    pub goal: String,
    pub collection: Collection,
    entities: [EntityId; 3],
    fingerprint: String,
}
fn fingerprint(state: &GoalContext, cx: &App) -> String {
    // A comparison token, never an authoritative form snapshot to restore.
    let bytes = serde_json::to_vec(&json!({
        "query":state.query.read(cx).value(),"scope":state.scope.read(cx).value(),
        "mode":state.mode,"scope_mode":state.scope_mode,"search":state.search,"search_request":state.search_request,
        "guidance":state.guidance.read(cx).value(),"chosen":state.chosen,"pinned":state.pinned,
        "packet":state.packet,"selection_changed":state.selection_changed,"pending_text":state.pending_text,
        "export":state.export,"saved_path":state.saved_path,"brief":state.brief,"brief_error":state.brief_error
    })).unwrap();
    okilum_core::decision_reuse::revision(&bytes)
}
impl BrainView {
    pub(in crate::brain) fn context_search_input_changed(&mut self, entity: EntityId) {
        for state in self.context_ui.goals.values_mut() {
            if state.query.entity_id() == entity || state.scope.entity_id() == entity {
                state.search_request = Value::Null;
            }
        }
        self.cancel_context_passage_input();
    }
    pub(in crate::brain) fn context_passage_guard(&self, cx: &App) -> Result<(), String> {
        if self.busy
            || self.context_form_locked()
            || self.context_ui.running(&self.goal_id())
            || self.adoption.active
            || self.context_ui.proposals.contains_key(&self.goal_id())
            || self.adoption.pending.iter().any(|p| {
                self.expected_workspace.as_ref() == Some(&p.workspace)
                    && p.request["destination"] == "context"
                    && p.request["goal_id"] == self.goal_id()
            })
        {
            return Err(
                "Finish the current Context operation before opening a source passage.".into(),
            );
        }
        if self.source_loading
            || self.dirty(cx)
            || self.editor_closing()
            || !self.editor_can_begin_criteria()
            || self.source_conflict.is_some()
            || self.pending_source_write.is_some()
            || self.pending_navigation.is_some()
            || self.source_find.active()
            || self.note_link.active()
        {
            return Err(
                "Finish Source edits, Find, links or recovery before opening a Context passage."
                    .into(),
            );
        }
        if self.show_capture
            || self.discussion_note.active()
            || self.goal_criteria.active
            || self.discussion_decision.active
            || self.decision_reuse.active
            || matches!(self.collection, Collection::Inbox | Collection::Attention)
            || self.source.managed().is_none()
            || self.expected_workspace.is_none()
            || self.goal_id().is_empty()
        {
            return Err("Open current Context in a managed goal before choosing a passage.".into());
        }
        Ok(())
    }
    pub(in crate::brain) fn context_inputs_composing(
        &self,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let Some(state) = self.context_ui.goals.get(&self.goal_id()) else {
            return true;
        };
        state
            .query
            .update(cx, |s, cx| s.marked_text_range(window, cx).is_some())
            || state
                .scope
                .update(cx, |s, cx| s.marked_text_range(window, cx).is_some())
            || state
                .guidance
                .update(cx, |s, cx| s.marked_text_range(window, cx).is_some())
    }
    pub(in crate::brain) fn context_origin_retained(
        &self,
        origin: &ContextOrigin,
        _: &App,
    ) -> bool {
        self.endpoint == origin.endpoint
            && self.expected_workspace.as_ref() == Some(&origin.workspace)
            && self.goal_id() == origin.goal
            && self.collection == origin.collection
            && self.context_ui.goals.get(&origin.goal).is_some_and(|s| {
                [
                    s.query.entity_id(),
                    s.scope.entity_id(),
                    s.guidance.entity_id(),
                ] == origin.entities
            })
    }
    pub(in crate::brain) fn context_origin_matches(
        &self,
        origin: &ContextOrigin,
        cx: &App,
    ) -> bool {
        self.surface == Surface::Context
            && self.context_origin_retained(origin, cx)
            && self.context_passage_guard(cx).is_ok()
            && self
                .context_ui
                .goals
                .get(&origin.goal)
                .is_some_and(|s| fingerprint(s, cx) == origin.fingerprint)
    }
    pub(in crate::brain) fn open_context_search_passage(
        &mut self,
        citation: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cancel_context_passage_input();
        let result = (|| {
            self.context_passage_guard(cx)?;
            if self.surface != Surface::Context || self.source_navigation_pending() {
                return Err(
                    "Finish the current navigation before opening a Context passage.".into(),
                );
            }
            if self.context_inputs_composing(window, cx)
                || self.source.link_selection(window, cx).is_none()
            {
                return Err("Finish text composition before opening a source passage.".into());
            }
            let state = self
                .context_ui
                .goals
                .get(&self.goal_id())
                .ok_or("Open Context first.")?;
            let request = &state.search_request;
            if !request.is_object()
                || request["query"] != state.query.read(cx).value().trim()
                || text(&request["scope"]["path_prefix"]) != state.scope.read(cx).value().trim()
                || request["mode"] != state.mode
                || request["scope"]["mode"] != state.scope_mode
                || request["scope"]["goal_id"] != self.goal_id()
                || !array(&state.search["hits"]).contains(&citation)
                || state.search["index"]["generation"]
                    .as_str()
                    .is_none_or(str::is_empty)
            {
                return Err("These results no longer match this Context search. Find sources again before opening a passage.".into());
            }
            let path = text(&citation["path"]);
            let revision = text(&citation["revision"]);
            let start = citation["start_line"]
                .as_u64()
                .and_then(|v| usize::try_from(v).ok())
                .unwrap_or(0);
            let end = citation["end_line"]
                .as_u64()
                .and_then(|v| usize::try_from(v).ok())
                .unwrap_or(0);
            if path.is_empty()
                || path.len() > 4096
                || path.starts_with('/')
                || path.contains(['\\', '\0'])
                || path.split('/').any(|s| s.is_empty() || s.starts_with('.'))
                || revision.is_empty()
                || revision.len() > 256
                || start == 0
                || end < start
                || text(&citation["citation_id"]).is_empty()
            {
                return Err(
                    "This search hit has invalid source identity or lines. Find sources again."
                        .into(),
                );
            }
            let origin = ContextOrigin {
                endpoint: self.endpoint,
                workspace: self.expected_workspace.clone().unwrap(),
                goal: self.goal_id(),
                collection: self.collection,
                entities: [
                    state.query.entity_id(),
                    state.scope.entity_id(),
                    state.guidance.entity_id(),
                ],
                fingerprint: fingerprint(state, cx),
            };
            Ok((origin, path, revision, start, end))
        })();
        match result {
            Ok((origin, path, revision, start, end)) => {
                self.begin_context_passage(origin, path, revision, start, end, window, cx)
            }
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    };
    struct Server {
        endpoint: SocketAddr,
        source: Arc<Mutex<Value>>,
        search: Arc<Mutex<Value>>,
        stop: Arc<AtomicBool>,
        worker: Option<std::thread::JoinHandle<Vec<Value>>>,
    }
    impl Server {
        fn new(source: Value, hit: Value) -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let endpoint = listener.local_addr().unwrap();
            let source = Arc::new(Mutex::new(source));
            let search = Arc::new(Mutex::new(
                json!({"hits":[hit],"index":{"status":"ready","generation":"g299"},"mode_requested":"lexical","mode_used":"lexical","warnings":[]}),
            ));
            let stop = Arc::new(AtomicBool::new(false));
            let (disk, response, stopped) = (source.clone(), search.clone(), stop.clone());
            let worker = std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + Duration::from_secs(30);
                let mut calls = vec![];
                while !stopped.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
                    let (mut stream, _) = match listener.accept() {
                        Ok(v) => v,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                            continue;
                        }
                        Err(e) => panic!("{e}"),
                    };
                    let mut line = String::new();
                    BufReader::new(stream.try_clone().unwrap())
                        .read_line(&mut line)
                        .unwrap();
                    let request: Value = serde_json::from_str(&line).unwrap();
                    let data = match request["op"].as_str().unwrap() {
                        "source_read" => disk.lock().unwrap().clone(),
                        "brain_search" => response.lock().unwrap().clone(),
                        "source_preview" => json!({"markdown":"Preview","assets":[]}),
                        op => panic!("unexpected operation {op}"),
                    };
                    writeln!(
                        stream,
                        "{}",
                        json!({"schema":request["schema"],"id":request["id"],"ok":true,"data":data})
                    )
                    .unwrap();
                    calls.push(request);
                }
                calls
            });
            Self {
                endpoint,
                source,
                search,
                stop,
                worker: Some(worker),
            }
        }
        fn finish(mut self) -> Vec<Value> {
            self.stop.store(true, Ordering::Relaxed);
            self.worker.take().unwrap().join().unwrap()
        }
    }
    fn source(workspace: &Value, path: &str, raw: &str) -> Value {
        json!({"schema":SCHEMA,"brain_id":workspace["brain_id"],"path":path,"revision":okilum_core::decision_reuse::revision(raw.as_bytes()),"content_base64":STANDARD.encode(raw),"media_type":"text/markdown"})
    }
    fn setup(
        cx: &mut TestAppContext,
    ) -> (
        Entity<BrainView>,
        &mut VisualTestContext,
        PathBuf,
        Value,
        Value,
        String,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let workspace = json!({"brain_id":uuid(),"root":"/isolated/passage299","records_dir":"records","managed":true});
        let directory = std::env::temp_dir().join(format!("passage299-{}", uuid()));
        let store = editor_recovery::EditorRecovery::at(directory.clone(), &workspace).unwrap();
        let (view, visual) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&workspace, &store, window, cx)
        });
        visual.run_until_parked();
        let raw = format!(
            "\u{feff}# Source\r\n{}## Passage 😀299\r\n{}",
            "Earlier source line\r\n".repeat(70),
            "Following source line\r\n".repeat(50)
        );
        let target = source(&workspace, "notes/target.md", &raw);
        let hit = json!({"citation_id":"hit299","path":target["path"],"revision":target["revision"],"start_line":72,"end_line":73,"excerpt":"## Passage 😀299\r\nFollowing source line\r\n","metadata":{},"lexical_score":1.0});
        view.update_in(visual,|v,window,cx|{
            v.busy=false;v.surface=Surface::Context;v.collection=Collection::Sources;v.snapshot=json!({"goal":{"id":uuid()}});v.ensure_context(window,cx);
            let state=v.context_ui.goals.get_mut(&v.goal_id()).unwrap();
            state.query.update(cx,|s,cx|s.set_value("Passage299",window,cx));state.mode="lexical".into();
            state.guidance.update(cx,|s,cx|s.set_value("Unsent guidance 😀299",window,cx));
            state.chosen.insert("manual".into(),json!({"citation_id":"manual","path":"notes/manual.md","revision":"manual","start_line":1,"end_line":1,"excerpt":"Retained manual","metadata":{}}));state.pinned.insert("manual".into());
            state.packet=json!({"id":"retained","revision":"packet","text":"Old saved guidance","scope":{"mode":"project","exclude_paths":["private.md"]}});
            state.pending_text=Some("Earlier pending text".into());state.selection_changed=true;
        });
        visual.run_until_parked();
        (view, visual, directory, target, hit, raw)
    }
    fn search(view: &Entity<BrainView>, visual: &mut VisualTestContext, server: &Server) {
        view.update_in(visual, |v, window, cx| {
            v.endpoint = server.endpoint;
            v.search_context(window, cx);
        });
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            let s = &v.context_ui.goals[&v.goal_id()];
            assert!(
                s.search_request.is_object(),
                "real search request provenance installed"
            );
            assert_eq!(s.search_request["query"], "Passage299");
            assert!(!v.busy);
            cx.notify();
        });
        visual.run_until_parked();
    }
    #[gpui::test]
    fn context_passage_actual_search_card_rpc_return_keeps_form_entities_and_same_note(
        cx: &mut TestAppContext,
    ) {
        let (view, visual, directory, target, hit, raw) = setup(cx);
        let server = Server::new(target.clone(), hit.clone());
        for prior in ["none", "different", "same"] {
            view.update_in(visual, |v, window, cx| {
                v.surface = Surface::Context;
                if prior != "none" {
                    let old = if prior == "same" {
                        target.clone()
                    } else {
                        source(
                            v.expected_workspace.as_ref().unwrap(),
                            "notes/old.md",
                            "# Old\r\n",
                        )
                    };
                    v.load_source(old, window, cx);
                    v.source_projection.live = true;
                    v.apply_source_projection(cx);
                }
            });
            visual.run_until_parked();
            search(&view, visual, &server);
            let (form, entities, stamp) = view.update_in(visual, |v, _, cx| {
                let s = &v.context_ui.goals[&v.goal_id()];
                (
                    fingerprint(s, cx),
                    [
                        s.query.entity_id(),
                        s.scope.entity_id(),
                        s.guidance.entity_id(),
                    ],
                    v.source.stamp(cx),
                )
            });
            let button = visual
                .debug_bounds("context-open-passage-0")
                .expect("actual current hit card action painted");
            visual.simulate_click(button.center(), Modifiers::default());
            visual.run_until_parked();
            // Exercise the actual Source paint hook used by the native after-paint landing.
            visual.draw(point(px(0.), px(0.)), size(px(1200.), px(800.)), |_, _| {
                view.clone().into_any_element()
            });
            visual.run_until_parked();
            view.update_in(visual,|v,_,cx| {
                assert_eq!(v.source_snapshot.as_ref(),Some(&target),"{prior}: {:?}",v.error);
                assert!(v.surface==Surface::Source);assert!(v.context_return_available(cx));assert!(v.source_back_path().is_none());
                let input=v.source.managed().unwrap().read(cx);let at=raw.find("## Passage").unwrap();
                assert_eq!(input.selected_range(),at..at,"exact CRLF/BOM/emoji source offset");
                let (caret,line)=input.cursor_layout().expect("actual source caret layout");let bounds=input.input_bounds();let visible=caret.top()+input.scroll_offset().y;
                assert!(visible>=bounds.top() && visible<bounds.top()+line*4.,"{prior}: first cited line at {visible:?}, bounds={bounds:?}, line={line:?}, scroll={:?}",input.scroll_offset());
                assert_eq!(v.source_projection.live,prior!="none");
                if prior=="same"{assert_eq!(v.source.stamp(cx),stamp,"same-note read retains editor revision/Undo");}
            });
            let button = visual
                .debug_bounds("context-return")
                .expect("actual Return action painted");
            visual.simulate_click(button.center(), Modifiers::default());
            visual.run_until_parked();
            view.update_in(visual, |v, _, cx| {
                assert!(v.surface == Surface::Context);
                assert!(!v.context_return_available(cx));
                let s = &v.context_ui.goals[&v.goal_id()];
                assert_eq!(fingerprint(s, cx), form);
                assert_eq!(
                    [
                        s.query.entity_id(),
                        s.scope.entity_id(),
                        s.guidance.entity_id()
                    ],
                    entities
                );
            });
        }
        let calls = server.finish();
        assert_eq!(
            calls.iter().filter(|r| r["op"] == "brain_search").count(),
            3
        );
        assert_eq!(calls.iter().filter(|r| r["op"] == "source_read").count(), 3);
        assert!(calls
            .iter()
            .filter(|r| r["op"] == "brain_search")
            .all(|r| r.get("_context_search_id").is_none()));
        assert!(calls.iter().all(|r| matches!(
            r["op"].as_str(),
            Some("brain_search" | "source_read" | "source_preview")
        )));
        if directory.exists() {
            std::fs::remove_dir_all(directory).unwrap();
        }
    }
    #[gpui::test]
    fn context_passage_failed_late_reads_and_single_return_replacement(cx: &mut TestAppContext) {
        let (view, visual, directory, target, hit, _) = setup(cx);
        let server = Server::new(target.clone(), hit.clone());
        let (a, b) = view.update_in(visual, |v, window, cx| {
            v.endpoint = server.endpoint;
            v.surface = Surface::Source;
            let a = source(
                v.expected_workspace.as_ref().unwrap(),
                "notes/a.md",
                "# A\n",
            );
            let b = source(
                v.expected_workspace.as_ref().unwrap(),
                "notes/b.md",
                "# B\n",
            );
            v.load_source(a.clone(), window, cx);
            *server.source.lock().unwrap() = b.clone();
            v.open_source_link("notes/b.md".into(), window, cx);
            (a, b)
        });
        visual.run_until_parked();
        view.update_in(visual, |v, _, _| {
            assert_eq!(v.source_back_path(), Some("notes/a.md"))
        });
        let owner = view.update_in(visual, |v, _, _| {
            (
                v.endpoint,
                v.expected_workspace.clone(),
                v.snapshot.clone(),
                v.collection,
            )
        });
        for reason in [
            "revision",
            "lines",
            "query",
            "scope",
            "mode",
            "guidance",
            "pins",
            "search",
            "same_search",
            "superseded",
            "pending_text",
            "selection",
            "endpoint",
            "workspace",
            "goal",
            "collection",
            "departure",
        ] {
            view.update_in(visual, |v, window, cx| {
                v.surface = Surface::Context;
                let goal = v.goal_id();
                let c = v.context_ui.goals.get_mut(&goal).unwrap();
                c.query
                    .update(cx, |s, cx| s.set_value("Passage299", window, cx));
                c.scope.update(cx, |s, cx| s.set_value("", window, cx));
                c.mode = "lexical".into();
                *server.source.lock().unwrap() = target.clone();
                server.search.lock().unwrap()["hits"] = json!([hit.clone()]);
            });
            visual.run_until_parked();
            search(&view, visual, &server);
            view.update_in(visual, |v, window, cx| {
                if reason == "revision" {
                    *server.source.lock().unwrap() = source(
                        v.expected_workspace.as_ref().unwrap(),
                        "notes/target.md",
                        "Changed\n",
                    );
                }
                let mut chosen_hit = hit.clone();
                if reason == "lines" {
                    chosen_hit["end_line"] = json!(5000);
                    v.context_ui.goals.get_mut(&v.goal_id()).unwrap().search["hits"] =
                        json!([chosen_hit.clone()]);
                }
                v.open_context_search_passage(chosen_hit, window, cx);
                assert!(
                    v.source_navigation_pending(),
                    "{reason}: the intended request boundary must be reached: {:?}",
                    v.error
                );
                let goal = v.goal_id();
                let c = v.context_ui.goals.get_mut(&goal).unwrap();
                match reason {
                    "query" => c
                        .query
                        .update(cx, |s, cx| s.set_value("Changed query", window, cx)),
                    "scope" => c.scope.update(cx, |s, cx| s.set_value("other", window, cx)),
                    "mode" => c.mode = "semantic".into(),
                    "guidance" => c
                        .guidance
                        .update(cx, |s, cx| s.set_value("New guidance", window, cx)),
                    "pins" => {
                        c.pinned.insert("new-pin".into());
                    }
                    "search" => c.search["index"]["generation"] = json!("replacement"),
                    "same_search" => c.search_request["_context_search_id"] = json!(uuid()),
                    "superseded" => v.open_source_link("notes/new-choice.md".into(), window, cx),
                    "pending_text" => c.pending_text = Some("New pending text".into()),
                    "selection" => c.selection_changed = !c.selection_changed,
                    "endpoint" => v.endpoint = "127.0.0.1:1".parse().unwrap(),
                    "workspace" => {
                        v.expected_workspace.as_mut().unwrap()["brain_id"] = json!(uuid())
                    }
                    "goal" => v.snapshot["goal"]["id"] = json!(uuid()),
                    "collection" => v.collection = Collection::Goals,
                    "departure" => v.surface = Surface::Source,
                    _ => {}
                }
            });
            visual.run_until_parked();
            view.update_in(visual, |v, _, _| {
                assert!(
                    v.surface
                        == if reason == "departure" {
                            Surface::Source
                        } else {
                            Surface::Context
                        },
                    "{reason}"
                );
                assert_eq!(v.source_snapshot.as_ref(), Some(&b), "{reason}");
                assert!(!v.source_navigation_pending());
                v.endpoint = owner.0;
                v.expected_workspace = owner.1.clone();
                v.snapshot = owner.2.clone();
                v.collection = owner.3;
                v.surface = Surface::Source;
                if !matches!(
                    reason,
                    "endpoint" | "workspace" | "goal" | "collection" | "departure"
                ) {
                    assert_eq!(
                        v.source_back_path(),
                        Some("notes/a.md"),
                        "{reason}: rejected read retains previous Source Back"
                    );
                }
            });
        }
        // Success installs a single Context return after owner changes invalidated old history.
        view.update_in(visual, |v, window, cx| {
            v.surface = Surface::Context;
            let goal = v.goal_id();
            let c = v.context_ui.goals.get_mut(&goal).unwrap();
            c.query
                .update(cx, |s, cx| s.set_value("Passage299", window, cx));
            c.scope.update(cx, |s, cx| s.set_value("", window, cx));
            c.mode = "lexical".into();
            *server.source.lock().unwrap() = target.clone();
        });
        visual.run_until_parked();
        search(&view, visual, &server);
        view.update_in(visual, |v, window, cx| {
            v.open_context_search_passage(hit.clone(), window, cx)
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert!(v.context_return_available(cx));
            assert!(v.source_back_path().is_none());
            *server.source.lock().unwrap() = a.clone();
            v.open_source_link("notes/a.md".into(), window, cx);
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert!(!v.context_return_available(cx));
            assert_eq!(v.source_back_path(), Some("notes/target.md"));
            *server.source.lock().unwrap() = target.clone();
            v.back_source(window, cx);
        });
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            assert!(!v.context_return_available(cx));
            assert!(v.source_back_path().is_none());
            assert_eq!(v.source_snapshot.as_ref(), Some(&target));
        });
        let calls = server.finish();
        assert_eq!(
            calls.iter().filter(|r| r["op"] == "source_read").count(),
            21,
            "all failed-read probes reached the real service plus source history reads"
        );
        if directory.exists() {
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[gpui::test]
    fn context_passage_preflight_guards_issue_no_reads_and_allow_unsaved_guidance(
        cx: &mut TestAppContext,
    ) {
        let (view, visual, directory, target, hit, _) = setup(cx);
        let server = Server::new(target.clone(), hit.clone());
        view.update_in(visual, |v, window, cx| {
            v.load_source(target.clone(), window, cx);
        });
        visual.run_until_parked();
        search(&view, visual, &server);
        for reason in [
            "busy",
            "loading",
            "adoption",
            "export",
            "conflict",
            "write",
            "dirty",
            "find",
            "query_ime",
            "scope_ime",
            "guidance_ime",
            "source_ime",
            "stale_query",
            "stale_scope",
            "stale_mode",
        ] {
            view.update_in(visual, |v, window, cx| {
                let goal = v.goal_id();
                let state = v.context_ui.goals.get_mut(&goal).unwrap();
                let prior_request = state.search_request.clone();
                match reason {
                    "busy" => v.busy = true,
                    "loading" => v.source_loading = true,
                    "adoption" => v.adoption.active = true,
                    "export" => state.export = json!({"status":"running"}),
                    "conflict" => v.source_conflict = Some(json!({"conflict_id":"test"})),
                    "write" => v.pending_source_write = Some(json!({"op":"source_write"})),
                    "dirty" => v.source.reset("Unsaved Source edits", window, cx),
                    "find" => {
                        v.surface = Surface::Source;
                        v.open_source_find(window, cx);
                        assert!(v.source_find.active());
                        v.surface = Surface::Context;
                    }
                    "query_ime" => state.query.update(cx, |s, cx| {
                        s.replace_and_mark_text_in_range(None, "中", Some(0..1), window, cx)
                    }),
                    "scope_ime" => state.scope.update(cx, |s, cx| {
                        s.replace_and_mark_text_in_range(None, "中", Some(0..1), window, cx)
                    }),
                    "guidance_ime" => state.guidance.update(cx, |s, cx| {
                        s.replace_and_mark_text_in_range(None, "中", Some(0..1), window, cx)
                    }),
                    "source_ime" => {
                        v.sync_source_policy(cx);
                        v.source.managed().unwrap().update(cx, |s, cx| {
                            s.replace_and_mark_text_in_range(None, "中", Some(0..1), window, cx)
                        });
                    }
                    "stale_query" => state.search_request["query"] = json!("Earlier query"),
                    "stale_scope" => state.search_request["scope"]["path_prefix"] = json!("other"),
                    "stale_mode" => state.search_request["mode"] = json!("semantic"),
                    _ => unreachable!(),
                }
                let before = v.source.value(cx);
                let stamp = v.source.stamp(cx);
                v.open_context_search_passage(hit.clone(), window, cx);
                assert!(!v.source_navigation_pending(), "{reason}: {:?}", v.error);
                assert!(v.surface == Surface::Context, "{reason}");
                assert_eq!(v.source.value(cx), before, "{reason}");
                assert_eq!(v.source.stamp(cx), stamp, "{reason}");
                v.busy = false;
                v.source_loading = false;
                v.adoption.active = false;
                v.source_conflict = None;
                v.pending_source_write = None;
                if reason == "find" {
                    v.close_source_find(window, cx);
                }
                if matches!(reason, "dirty" | "source_ime") {
                    v.load_source(target.clone(), window, cx);
                }
                let state = v.context_ui.goals.get_mut(&goal).unwrap();
                state.export = Value::Null;
                state.search_request = prior_request;
                if reason == "query_ime" {
                    state
                        .query
                        .update(cx, |s, cx| s.set_value("Passage299", window, cx));
                }
                if reason == "scope_ime" {
                    state.scope.update(cx, |s, cx| s.set_value("", window, cx));
                }
                if reason == "guidance_ime" {
                    state
                        .guidance
                        .update(cx, |s, cx| s.set_value("Unsent guidance 😀299", window, cx));
                }
            });
            visual.run_until_parked();
            // Input changes invalidate result provenance, so the next attempt gets a real search.
            search(&view, visual, &server);
        }
        view.update_in(visual, |v, window, cx| {
            v.open_context_search_passage(hit.clone(), window, cx)
        });
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            assert!(
                v.context_return_available(cx),
                "positive control after all guards: {:?}",
                v.error
            )
        });
        let calls = server.finish();
        assert_eq!(
            calls.iter().filter(|r| r["op"] == "source_read").count(),
            1,
            "only the final positive control may read"
        );
        if directory.exists() {
            std::fs::remove_dir_all(directory).unwrap();
        }
    }
}
