//! Explicit, generation-bound incoming references for saved managed sources.
use super::*;
use open_link_ui::PreviewOwner;

#[derive(Default)]
pub(super) struct IncomingReferencesUi {
    owner: Option<PreviewOwner>,
    epoch: u64,
    expanded: bool,
    loading: bool,
    page: Option<Value>,
    error: Option<String>,
}
fn valid_path(path: &str) -> bool {
    !path.is_empty() && path.len() <= 4096 && !path.contains(['\\', '\0'])
        && !std::path::Path::new(path).is_absolute()
        && std::path::Path::new(path).components().all(|part| {
            matches!(part, std::path::Component::Normal(n) if !n.to_string_lossy().starts_with('.'))
        })
}
fn page_rows(page: &Value) -> &[Value] {
    page["rows"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
}
fn row_key(row: &Value) -> (String, u64) {
    (
        text(&row["path"]),
        row["start_line"].as_u64().unwrap_or_default(),
    )
}
fn validate_page(page: Value, source: &Value, previous: Option<&Value>) -> Result<Value, String> {
    let bad = || {
        "Incoming references returned incomplete or stale evidence. Refresh to try again."
            .to_owned()
    };
    if page.to_string().len() > 128 * 1024
        || page["target"]["path"] != source["path"]
        || page["target"]["revision"] != source["revision"]
        || page["index"]["status"] != "ready"
        || page["index"]["generation"]
            .as_str()
            .is_none_or(str::is_empty)
        || page["freshness"] != "current_at_read"
        || page["rows"].as_array().is_none_or(|rows| rows.len() > 10)
        || page["warnings"].as_array().is_none()
        || (!page["next_cursor"].is_null()
            && page["next_cursor"]
                .as_str()
                .is_none_or(|s| s.is_empty() || s.len() > 4096))
    {
        return Err(bad());
    }
    if page_rows(&page).is_empty() && page["next_cursor"].is_string() {
        return Err(bad());
    }
    if let Some(previous) = previous {
        if previous["index"]["generation"] != page["index"]["generation"]
            || previous["target"] != page["target"]
            || (!page_rows(&page).is_empty()
                && page_rows(previous)
                    .last()
                    .is_some_and(|last| row_key(last) >= row_key(&page_rows(&page)[0])))
            || (!page["next_cursor"].is_null() && page["next_cursor"] == previous["next_cursor"])
        {
            return Err(
                "The index changed while paging. Refresh incoming references from the first page."
                    .into(),
            );
        }
    }
    let mut previous_key = None;
    for row in page_rows(&page) {
        let start = row["start_line"].as_u64().unwrap_or_default();
        if !row["path"].as_str().is_some_and(valid_path)
            || row["path"] == source["path"]
            || row["revision"].as_str().is_none_or(str::is_empty)
            || start == 0
            || start > 1024 * 1024
            || row["end_line"].as_u64() != Some(start)
            || row["title"].as_str().is_none_or(|s| s.len() > 256)
            || row["excerpt"].as_str().is_none_or(|s| s.len() > 1024)
            || row["link"].as_str().is_none_or(|s| s.len() > 512)
            || row["ambiguous"].as_bool().is_none()
            || row["excerpt_truncated"].as_bool().is_none()
            || row["link_truncated"].as_bool().is_none()
            || previous_key.as_ref().is_some_and(|p| p >= &row_key(row))
        {
            return Err(bad());
        }
        previous_key = Some(row_key(row));
    }
    Ok(page)
}
impl BrainView {
    fn sync_incoming_references(&mut self, cx: &App) {
        let owner = self.open_link_preview_owner(cx);
        if owner != self.incoming_references.owner {
            let epoch = self.incoming_references.epoch.wrapping_add(1);
            self.incoming_references = IncomingReferencesUi {
                owner,
                epoch,
                ..Default::default()
            };
        }
    }
    fn incoming_ready(&self, window: &mut Window, cx: &mut App) -> Result<(), String> {
        if self.capabilities["source_backlinks"] != true {
            return Err("Incoming references are unavailable on this backend.".into());
        }
        if self.source.managed().is_none()
            || self.surface != Surface::Source
            || self.show_capture
            || matches!(self.collection, Collection::Inbox | Collection::Attention)
            || self.source_snapshot.is_none()
        {
            return Err("Open a saved managed Source note first.".into());
        }
        if self.busy
            || self.source_loading
            || self.dirty(cx)
            || !self.editor_can_begin_criteria()
            || self.source_conflict.is_some()
            || self.pending_navigation.is_some()
            || self.pending_source_write.is_some()
            || self.source_find.active()
            || self.note_link.active()
            || self.source_selection_context_active()
            || self.discussion_note.active()
            || self.goal_criteria.active
            || self.discussion_decision.active
            || self.decision_reuse.active
            || self.source_navigation_pending()
            || self.source.link_selection(window, cx).is_none()
        {
            return Err("Finish the current draft, recovery, Find or composition before using incoming references.".into());
        }
        Ok(())
    }
    fn load_incoming_references(
        &mut self,
        next: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_incoming_references(cx);
        if let Err(error) = self.incoming_ready(window, cx) {
            self.incoming_references.error = Some(error);
            cx.notify();
            return;
        }
        if self.incoming_references.loading {
            return;
        }
        let previous = next
            .then(|| self.incoming_references.page.clone())
            .flatten();
        let cursor = previous
            .as_ref()
            .and_then(|p| p["next_cursor"].as_str())
            .map(str::to_owned);
        if next && cursor.is_none() {
            return;
        }
        let source = self.source_snapshot.clone().unwrap();
        let owner = self.incoming_references.owner.clone().unwrap();
        let endpoint = self.endpoint;
        let workspace = self.expected_workspace.clone().unwrap();
        let request = json!({"op":"source_backlinks","path":source["path"],"expected_revision":source["revision"],"scope":{"goal_id":self.goal_id(),"mode":"project"},"limit":10,"cursor":cursor});
        self.incoming_references.epoch = self.incoming_references.epoch.wrapping_add(1);
        let epoch = self.incoming_references.epoch;
        self.incoming_references.loading = true;
        self.incoming_references.error = None;
        cx.spawn_in(window, async move |view, cx| {
            let reply = cx
                .background_executor()
                .spawn(async move {
                    let reply = rpc_guarded(endpoint, request, Some(&workspace))?;
                    validate_page(reply, &source, previous.as_ref())
                })
                .await;
            let _ = view.update_in(cx, |this, _, cx| {
                if this.incoming_references.epoch != epoch
                    || this.open_link_preview_owner(cx).as_ref() != Some(&owner)
                {
                    return;
                }
                this.incoming_references.loading = false;
                match reply {
                    Ok(page) => this.incoming_references.page = Some(page),
                    Err(error) => this.incoming_references.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn open_incoming_passage(&mut self, row: Value, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.open_link_preview_owner(cx);
        if self.incoming_references.owner != current
            || self.incoming_references.loading
            || self.incoming_references.error.is_some()
            || !self
                .incoming_references
                .page
                .as_ref()
                .is_some_and(|p| page_rows(p).contains(&row))
        {
            self.error = Some(
                "This reference belongs to an earlier page. Refresh incoming references.".into(),
            );
            cx.notify();
            return;
        }
        if let Err(error) = self.incoming_ready(window, cx) {
            self.error = Some(error);
            cx.notify();
            return;
        }
        self.open_source_passage(
            text(&row["path"]),
            text(&row["revision"]),
            row["start_line"].as_u64().unwrap() as usize,
            row["end_line"].as_u64().unwrap() as usize,
            window,
            cx,
        );
    }
    pub(super) fn incoming_passage_stale(&mut self) {
        self.incoming_references.error = Some(
            "This source passage changed. Refresh the first page before opening a reference."
                .into(),
        );
    }
    pub(super) fn incoming_references_panel(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.sync_incoming_references(cx);
        if self.source.managed().is_none() || self.source_snapshot.is_none() {
            return None;
        }
        let mut panel = v_flex().id("incoming-references-panel").gap_2().child(
            Button::new("incoming-references-toggle")
                .label(if self.incoming_references.expanded {
                    "Hide incoming references"
                } else {
                    "Incoming references"
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.incoming_references.expanded = !this.incoming_references.expanded;
                    cx.notify();
                })),
        );
        if !self.incoming_references.expanded {
            return Some(panel.into_any_element());
        }
        panel = panel.child(
            div()
                .text_sm()
                .child("Project-visible saved sources · body wikilinks"),
        );
        if self.capabilities["source_backlinks"] != true {
            return Some(
                panel
                    .child("Incoming references are unavailable on this backend.")
                    .into_any_element(),
            );
        }
        let loading = self.incoming_references.loading;
        panel = panel
            .child(
                h_flex().gap_2().child(
                    Button::new("incoming-references-load")
                        .debug_selector(|| "incoming-references-load".into())
                        .label(if self.incoming_references.page.is_some() {
                            "Refresh first page"
                        } else {
                            "Load incoming references"
                        })
                        .disabled(loading)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.load_incoming_references(false, window, cx)
                        })),
                ),
            )
            .when(loading, |p| p.child("Loading incoming references…"));
        if let Some(error) = &self.incoming_references.error {
            panel = panel.child(div().text_sm().child(error.clone()));
        }
        if let Some(page) = &self.incoming_references.page {
            let disabled = loading || self.incoming_references.error.is_some();
            if page_rows(page).is_empty() && !disabled {
                panel = panel.child(
                    "No incoming body-wikilink references in this scope at the indexed revision.",
                );
            }
            for (i, row) in page_rows(page).iter().enumerate() {
                let selected = row.clone();
                let title = row["title"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(row["path"].as_str().unwrap());
                panel = panel.child(
                    v_flex()
                        .gap_1()
                        .min_w_0()
                        .child(div().text_sm().child(title.to_owned()))
                        .child(div().text_xs().child(format!(
                            "{} · line {}",
                            text(&row["path"]),
                            row["start_line"]
                        )))
                        .child(div().text_sm().child(text(&row["excerpt"])))
                        .when(row["excerpt_truncated"] == true, |p| {
                            p.child(div().text_xs().child("Excerpt shortened"))
                        })
                        .when(row["ambiguous"] == true, |p| {
                            p.child(div().text_sm().child("May refer to this note"))
                        })
                        .child(
                            Button::new(("incoming-reference-open", i))
                                .debug_selector(move || format!("incoming-reference-open-{i}"))
                                .label("Open source passage")
                                .disabled(disabled)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_incoming_passage(selected.clone(), window, cx)
                                })),
                        ),
                );
            }
            for warning in array(&page["warnings"]).iter().take(4) {
                if let Some(warning) = warning.as_str() {
                    panel = panel.child(div().text_xs().child(warning.to_owned()));
                }
            }
            if page["next_cursor"].is_string() {
                panel = panel.child(
                    Button::new("incoming-references-next")
                        .debug_selector(|| "incoming-references-next".into())
                        .label("Next page")
                        .disabled(disabled)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.load_incoming_references(true, window, cx)
                        })),
                );
            }
        }
        Some(panel.into_any_element())
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

    fn snapshot(workspace: &Value, path: &str, raw: &str) -> Value {
        json!({"schema":SCHEMA,"brain_id":workspace["brain_id"],"path":path,
            "revision":tessera_core::decision_reuse::revision(raw.as_bytes()),
            "content_base64":STANDARD.encode(raw),"media_type":"text/markdown"})
    }
    fn row(source: &Value, line: usize) -> Value {
        json!({"path":source["path"],"title":"Referencing note","revision":source["revision"],
            "start_line":line,"end_line":line,"excerpt":"Original [[source|label]] passage",
            "link":"source","ambiguous":true,"excerpt_truncated":false,"link_truncated":false})
    }
    fn page(target: &Value, rows: Vec<Value>, next: Option<&str>) -> Value {
        json!({"target":{"path":target["path"],"revision":target["revision"]},"index":{"status":"ready","generation":"g1","observed_at":"2026-09-09T00:00:00Z","semantic_status":"unavailable"},"freshness":"current_at_read","rows":rows,"next_cursor":next,"warnings":[]})
    }
    #[test]
    fn incoming_page_validates_identity_generation_order_and_bounds_without_semantic_dependency() {
        let a = json!({"path":"a.md","revision":"ra"});
        let b = json!({"path":"b.md","revision":"rb"});
        let c = json!({"path":"c.md","revision":"rc"});
        let first = page(&a, vec![row(&b, 2)], Some("next"));
        assert!(validate_page(first.clone(), &a, None).is_ok());
        let mut next = page(&a, vec![row(&c, 3)], None);
        next["index"]["observed_at"] = json!("later same generation");
        assert!(validate_page(next.clone(), &a, Some(&first)).is_ok());
        for change in [
            "target",
            "revision",
            "generation",
            "status",
            "rowpath",
            "lines",
            "excerpt",
            "duplicate",
            "order",
            "cursor",
        ] {
            let mut bad = next.clone();
            match change {
                "target" => bad["target"]["path"] = json!("other.md"),
                "revision" => bad["target"]["revision"] = json!("old"),
                "generation" => bad["index"]["generation"] = json!("g2"),
                "status" => bad["index"]["status"] = json!("stale"),
                "rowpath" => bad["rows"][0]["path"] = json!("../escape.md"),
                "lines" => bad["rows"][0]["start_line"] = json!(0),
                "excerpt" => bad["rows"][0]["excerpt"] = json!("x".repeat(1025)),
                "duplicate" => bad["rows"] = json!([row(&c, 3), row(&c, 3)]),
                "order" => bad["rows"] = json!([row(&b, 1)]),
                "cursor" => bad["next_cursor"] = first["next_cursor"].clone(),
                _ => unreachable!(),
            }
            assert!(validate_page(bad, &a, Some(&first)).is_err(), "{change}");
        }
        let empty = page(&a, vec![], None);
        assert!(validate_page(empty, &a, None).is_ok());
    }
    struct Server {
        sources: BTreeMap<String, Value>,
        first: Value,
        next: Value,
        requests: Vec<Value>,
    }
    fn server(
        state: Arc<Mutex<Server>>,
        workspace: Value,
    ) -> (SocketAddr, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let task = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            while !stopped.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(v) => v,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["expected_workspace"], workspace);
                let mut state = state.lock().unwrap();
                let data = match request["op"].as_str().unwrap() {
                    "source_backlinks" => {
                        if request["cursor"].is_null() {
                            state.first.clone()
                        } else {
                            state.next.clone()
                        }
                    }
                    "source_read" => state.sources[request["path"].as_str().unwrap()].clone(),
                    "source_preview" => {
                        let s = &state.sources[request["path"].as_str().unwrap()];
                        json!({"path":s["path"],"revision":s["revision"],"preview_revision":s["revision"],"markdown":decode_source(s).unwrap(),"assets":[],"links":[]})
                    }
                    "brain_index_status" => json!({"status":"ready","generation":"g1"}),
                    op => panic!("Forbidden or unexpected operation {op}"),
                };
                state.requests.push(request.clone());
                writeln!(
                    stream,
                    "{}",
                    json!({"schema":request["schema"],"id":request["id"],"ok":true,"data":data})
                )
                .unwrap();
            }
        });
        (endpoint, stop, task)
    }
    #[gpui::test]
    fn incoming_actual_buttons_page_stale_passage_and_back_preserve_source(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let workspace = json!({"brain_id":uuid(),"root":"/isolated/incoming297","records_dir":"records","managed":true});
        let dir = std::env::temp_dir().join(format!("incoming297-{}", uuid()));
        let store = editor_recovery::EditorRecovery::at(dir.clone(), &workspace).unwrap();
        let a = snapshot(
            &workspace,
            "notes/source.md",
            "# Current\r\nSaved target\r\n",
        );
        let raw = format!(
            "# Referrer\r\n{}Original [[source|label]] passage\r\n{}",
            "Before\r\n".repeat(60),
            "Following\r\n".repeat(30)
        );
        let b = snapshot(&workspace, "notes/referrer.md", &raw);
        let z = snapshot(&workspace, "notes/z.md", "# Z\n[[source]]\n");
        let state = Arc::new(Mutex::new(Server {
            sources: BTreeMap::from([
                (text(&a["path"]), a.clone()),
                (text(&b["path"]), b.clone()),
                (text(&z["path"]), z.clone()),
            ]),
            first: page(&a, vec![row(&b, 62)], Some("next")),
            next: page(&a, vec![row(&z, 2)], None),
            requests: vec![],
        }));
        let (endpoint, stop, task) = server(state.clone(), workspace.clone());
        let (view, visual) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&workspace, &store, window, cx)
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            v.endpoint = endpoint;
            v.busy = false;
            v.surface = Surface::Source;
            v.collection = Collection::Sources;
            v.snapshot = json!({"goal":{"id":uuid()}});
            v.capabilities["source_backlinks"] = json!(true);
            v.load_source(a.clone(), window, cx);
            v.source_projection.live = true;
            v.schedule_source_projection(window, cx);
            v.sync_incoming_references(cx);
            v.incoming_references.expanded = true;
        });
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(250));
        visual.run_until_parked();
        assert!(
            state
                .lock()
                .unwrap()
                .requests
                .iter()
                .all(|r| r["op"] != "source_backlinks"),
            "render does not query references"
        );
        let click = |visual: &mut VisualTestContext, selector: &'static str| {
            let button = visual
                .debug_bounds(selector)
                .expect("actual control painted");
            visual.simulate_click(button.center(), Modifiers::default());
            visual.run_until_parked();
        };
        click(visual, "incoming-references-load");
        view.update_in(visual, |v, _, _| {
            assert!(
                v.incoming_references.error.is_none(),
                "{:?}",
                v.incoming_references.error
            );
            assert_eq!(
                page_rows(v.incoming_references.page.as_ref().unwrap())[0]["ambiguous"],
                true
            );
        });
        click(visual, "incoming-references-next");
        view.update_in(visual, |v, _, _| {
            assert_eq!(
                page_rows(v.incoming_references.page.as_ref().unwrap()).len(),
                1
            );
            assert_eq!(
                page_rows(v.incoming_references.page.as_ref().unwrap())[0]["path"],
                z["path"]
            );
        });
        click(visual, "incoming-references-load");
        state.lock().unwrap().sources.insert(
            text(&b["path"]),
            snapshot(&workspace, "notes/referrer.md", "# Changed\n"),
        );
        click(visual, "incoming-reference-open-0");
        view.update_in(visual, |v, _, _| {
            assert_eq!(v.source_snapshot.as_ref(), Some(&a));
            assert!(v.source_back_path().is_none());
            assert!(v.error.as_ref().unwrap().contains("changed"));
        });
        state
            .lock()
            .unwrap()
            .sources
            .insert(text(&b["path"]), b.clone());
        click(visual, "incoming-references-load");
        click(visual, "incoming-reference-open-0");
        visual.executor().advance_clock(Duration::from_millis(250));
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert_eq!(v.source_snapshot.as_ref(), Some(&b), "{:?}", v.error);
            let offset = raw.find("Original [[").unwrap();
            assert_eq!(
                v.source.managed().unwrap().read(cx).selected_range(),
                offset..offset
            );
            assert!(v.source_projection.live);
            assert_eq!(v.source_back_path(), Some("notes/source.md"));
            v.back_source(window, cx);
        });
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(250));
        visual.run_until_parked();
        view.update_in(visual, |v, _, _| {
            assert_eq!(v.source_snapshot.as_ref(), Some(&a));
            assert!(v.source_back_path().is_none());
        });
        // A request that was explicitly started may finish after the user leaves
        // its owner. Its page must not attach to a newer goal or edited source.
        for change in ["goal", "edit", "epoch"] {
            view.update_in(visual, |v, window, cx| {
                v.load_source(a.clone(), window, cx);
                v.surface = Surface::Source;
                v.sync_incoming_references(cx);
                v.incoming_references.expanded = true;
                v.load_incoming_references(false, window, cx);
                assert!(
                    v.incoming_references.loading,
                    "positive control: explicit request began"
                );
                match change {
                    "goal" => v.snapshot["goal"]["id"] = json!(uuid()),
                    "edit" => v.source.reset("New unsaved local draft", window, cx),
                    "epoch" => {
                        v.incoming_references.epoch += 1;
                        v.incoming_references.loading = false;
                    }
                    _ => unreachable!(),
                }
            });
            visual.run_until_parked();
            view.update_in(visual, |v, _, _| {
                assert!(v.incoming_references.page.is_none(), "{change}");
            });
        }
        view.update_in(visual, |v, window, cx| {
            v.load_source(a.clone(), window, cx);
            v.sync_incoming_references(cx);
            v.capabilities["source_backlinks"] = json!(false);
            v.load_incoming_references(false, window, cx);
            assert!(v
                .incoming_references
                .error
                .as_ref()
                .unwrap()
                .contains("unavailable"));
            assert!(!v.incoming_references.loading);
        });
        stop.store(true, Ordering::Relaxed);
        task.join().unwrap();
        let state = state.lock().unwrap();
        assert_eq!(
            state
                .requests
                .iter()
                .filter(|r| r["op"] == "source_backlinks")
                .count(),
            7
        );
        assert_eq!(
            state
                .requests
                .iter()
                .filter(|r| r["op"] == "source_read")
                .count(),
            3
        );
        if dir.exists() {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
