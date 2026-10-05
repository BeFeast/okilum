//! Preview exact saved source lines before staging one existing Context citation.
use super::*;
use crate::brain::source_input::LinkSelection;
use gpui_component::input::projection::SourceStamp;

#[derive(Clone)]
struct Owner {
    endpoint: SocketAddr,
    collection: Collection,
    workspace: Value,
    goal: String,
    source: Value,
    entity: EntityId,
    stamp: SourceStamp,
    selection: LinkSelection,
    form: Option<Value>,
}
#[derive(Clone)]
struct Prepared {
    form: Value,
    packet: Value,
    citation: Value,
}
struct Preview {
    owner: Owner,
    prepared: Option<Prepared>,
    excerpt: Option<Entity<TextareaState>>,
    details: bool,
    pending: bool,
    error: Option<String>,
}
#[derive(Default)]
pub(super) struct SelectionUi {
    generation: u64,
    preview: Option<Box<Preview>>,
}

fn read_saved(owner: &Owner) -> Result<Value, String> {
    let current = rpc_guarded(
        owner.endpoint,
        json!({"op":"source_read","path":owner.source["path"]}),
        Some(&owner.workspace),
    )?;
    if current != owner.source {
        return Err(
            "Source changed on disk. Reload and inspect it, then select lines again.".into(),
        );
    }
    if owner.form.as_ref().is_some_and(|f| f["packet"].is_object()) {
        return Ok(Value::Null);
    }
    let reply = rpc_guarded(
        owner.endpoint,
        json!({"op":"context_get","goal_id":owner.goal}),
        Some(&owner.workspace),
    )?;
    reply
        .get("packet")
        .filter(|p| p.is_null() || p.is_object())
        .cloned()
        .ok_or_else(|| {
            "Saved Context response is incomplete. Open Context to inspect it first.".into()
        })
}
fn prepare(owner: &Owner) -> Result<Prepared, String> {
    // Reject unsupported sources before showing a preview or staging anything.
    let scope = json!({"goal_id":owner.goal,"mode":"project","path_prefix":null});
    source_context::selected_lines(
        &owner.source,
        &owner.workspace,
        &owner.goal,
        &scope,
        owner.selection.bytes.clone(),
    )?;
    let packet = read_saved(owner)?;
    let form = source_context::hydrate(owner.form.as_ref(), &packet, &owner.goal)?;
    let citation = source_context::selected_lines(
        &owner.source,
        &owner.workspace,
        &owner.goal,
        &source_context::form_scope(&form, &owner.goal),
        owner.selection.bytes.clone(),
    )?;
    source_context::preflight(&citation, &array(&form["chosen"]), &text(&form["guidance"]))?;
    Ok(Prepared {
        form,
        packet,
        citation,
    })
}

impl BrainView {
    pub(crate) fn source_selection_context_active(&self) -> bool {
        self.context_ui.selection_preview.preview.is_some()
    }
    fn selection_context_matches(
        &self,
        owner: &Owner,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.surface == Surface::Source
            && !self.show_capture
            && self.collection == owner.collection
            && !self.busy
            && self.source_context_ready(cx).is_ok()
            && self.endpoint == owner.endpoint
            && self.expected_workspace.as_ref() == Some(&owner.workspace)
            && self.goal_id() == owner.goal
            && self.source_snapshot.as_ref() == Some(&owner.source)
            && self.source.entity_id() == owner.entity
            && self.source.stamp(cx).as_ref() == Some(&owner.stamp)
            && self.source_context_form(&owner.goal, cx) == owner.form
            && self.source.link_selection(window, cx).as_ref() == Some(&owner.selection)
    }
    pub(crate) fn cancel_source_selection_context(&mut self, cx: &mut Context<Self>) {
        self.context_ui.selection_preview.generation =
            self.context_ui.selection_preview.generation.wrapping_add(1);
        self.context_ui.selection_preview.preview = None;
        cx.notify();
    }
    pub(crate) fn open_source_selection_context(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.surface != Surface::Source || self.show_capture {
            return;
        }
        let owner = (|| {
            self.source_context_ready(cx)?;
            let selection = self
                .source
                .link_selection(window, cx)
                .ok_or("Finish text composition before using selected lines in Context.")?;
            if selection.bytes.is_empty() {
                return Err("Select nonempty Source text first.".into());
            }
            Ok(Owner {
                endpoint: self.endpoint,
                collection: self.collection,
                workspace: self
                    .expected_workspace
                    .clone()
                    .ok_or("Choose a managed workspace.")?,
                goal: self.goal_id(),
                source: self
                    .source_snapshot
                    .clone()
                    .ok_or("Choose a saved Source note first.")?,
                entity: self.source.entity_id(),
                stamp: self
                    .source
                    .stamp(cx)
                    .ok_or("Source selection is unavailable.")?,
                selection,
                form: self.source_context_form(&self.goal_id(), cx),
            })
        })();
        let owner = match owner {
            Ok(o) => o,
            Err(e) => {
                self.error = Some(e);
                cx.notify();
                return;
            }
        };
        self.cancel_source_selection_context(cx);
        let generation = self.context_ui.selection_preview.generation;
        self.context_ui.selection_preview.preview = Some(Box::new(Preview {
            owner: owner.clone(),
            prepared: None,
            excerpt: None,
            details: false,
            pending: true,
            error: None,
        }));
        self.error = None;
        cx.spawn_in(window, async move |this, cx| {
            let captured = owner.clone();
            let result = cx
                .background_executor()
                .spawn(async move { prepare(&captured) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.context_ui.selection_preview.generation != generation {
                    return;
                }
                if !this.selection_context_matches(&owner, window, cx) {
                    this.cancel_source_selection_context(cx);
                    return;
                }
                let excerpt = result.as_ref().ok().map(|p| {
                    cx.new(|cx| {
                        let mut state = TextareaState::new(window, cx).rows(6);
                        state.set_value(text(&p.citation["excerpt"]), window, cx);
                        state
                    })
                });
                if let Some(preview) = this.context_ui.selection_preview.preview.as_mut() {
                    preview.pending = false;
                    preview.excerpt = excerpt;
                    match result {
                        Ok(p) => preview.prepared = Some(p),
                        Err(e) => preview.error = Some(e),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn add_source_selection_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preview) = &self.context_ui.selection_preview.preview else {
            return;
        };
        if preview.pending {
            return;
        }
        let Some(prepared) = preview.prepared.clone() else {
            return;
        };
        let owner = preview.owner.clone();
        if !self.selection_context_matches(&owner, window, cx) {
            self.cancel_source_selection_context(cx);
            self.error=Some("The source, selection or Context changed. Select lines and inspect a fresh preview.".into());
            return;
        }
        let generation = self.context_ui.selection_preview.generation;
        let preview = self.context_ui.selection_preview.preview.as_mut().unwrap();
        preview.pending = true;
        preview.error = None;
        cx.spawn_in(window, async move |this, cx| {
            let captured = owner.clone();
            let prior_packet = prepared.packet.clone();
            let result = cx.background_executor().spawn(async move {
                if read_saved(&captured)? != prior_packet {
                    return Err("Saved Context changed. Cancel and inspect a fresh selection preview.".to_owned());
                }
                Ok(())
            }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.context_ui.selection_preview.generation != generation { return; }
                if !this.selection_context_matches(&owner, window, cx) {
                    this.cancel_source_selection_context(cx);
                    return;
                }
                let result = result.and_then(|()| source_context::preflight(&prepared.citation, &array(&prepared.form["chosen"]), &text(&prepared.form["guidance"])));
                match result {
                    Ok(action) => {
                        this.cancel_source_selection_context(cx);
                        this.apply_source_context(&owner.goal, prepared.form, prepared.citation, action, window, cx);
                        this.notice = if action == source_context::Addition::Add {
                            "Added the saved source lines to Context. Build and review before use."
                        } else {
                            "Already included in Context. Remove the existing source explicitly before replacing its passage."
                        }.into();
                    }
                    Err(e) => {
                        if let Some(preview) = this.context_ui.selection_preview.preview.as_mut() {
                            preview.pending=false;
                            preview.error=Some(e);
                        }
                    }
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }
    pub(crate) fn sync_source_selection_context(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let invalid = self
            .context_ui
            .selection_preview
            .preview
            .as_ref()
            .is_some_and(|p| !self.selection_context_matches(&p.owner, window, cx));
        if invalid {
            self.cancel_source_selection_context(cx);
        }
    }
    pub(crate) fn source_selection_context_panel(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let preview = self.context_ui.selection_preview.preview.as_ref()?;
        let mut panel = v_flex()
            .gap_2()
            .p_3()
            .rounded(px(8.))
            .border_1()
            .border_color(super::super::super::brand::palette(cx).border)
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Use selected lines in Context"),
            )
            .child(div().text_sm().child(
                "Context includes complete source lines. Inspect the passage before adding it.",
            ));
        if let Some(prepared) = &preview.prepared {
            let citation = &prepared.citation;
            panel = panel
                .child(div().text_sm().child(format!(
                    "{} · {}",
                    text(&citation["path"]),
                    text(&citation["locator"])
                )))
                .child(div().text_xs().child(
                    if text(&citation["excerpt"]) != preview.owner.selection.text {
                        "The preview extends your selection to complete source-line boundaries."
                    } else {
                        "The selection already contains complete source lines."
                    },
                ));
            if let Some(excerpt) = &preview.excerpt {
                panel = panel.child(Textarea::new(excerpt).readonly(true).h(px(150.)));
            }
            panel = panel.child(
                super::super::super::brand::control("source-selection-details", cx)
                    .ghost()
                    .label("Details")
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(p) = this.context_ui.selection_preview.preview.as_mut() {
                            p.details = !p.details;
                        }
                        cx.notify();
                    })),
            );
            if preview.details {
                panel = panel.child(div().text_xs().child(text(&citation["revision"])));
            }
        }
        if preview.pending {
            panel = panel.child(
                div()
                    .text_sm()
                    .child("Checking the saved source and Context…"),
            );
        }
        if let Some(error) = &preview.error {
            panel = panel.child(div().text_sm().child(error.clone()));
        }
        Some(
            panel
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            super::super::super::brand::control("source-selection-add", cx)
                                .label("Add to Context")
                                .primary()
                                .disabled(preview.pending || preview.prepared.is_none())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.add_source_selection_context(window, cx)
                                })),
                        )
                        .child(
                            super::super::super::brand::control("source-selection-cancel", cx)
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cancel_source_selection_context(cx)
                                })),
                        ),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{
        citation, packet, source_context_changing_test_server, source_context_test_server,
        source_context_test_source,
    };
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::TestAppContext;
    use std::sync::atomic::Ordering;

    #[gpui::test]
    fn source_selection_context_preview_cancel_add_preserves_projected_source_and_form(
        cx: &mut TestAppContext,
    ) {
        preview_cancel_add(cx);
    }
    fn preview_cancel_add(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let goal = uuid();
        let workspace = json!({"brain_id":uuid(),"root":"/isolated/source-selection","records_dir":"records","managed":true});
        let directory = std::env::temp_dir().join(format!("source-selection281-{}", uuid()));
        let store = editor_recovery::EditorRecovery::at(directory.clone(), &workspace).unwrap();
        let raw=format!("---\r\ntype: Note\r\nverification: unverified\r\n---\r\n# Heading\r\nBefore **😀План** after\r\n{}", "ordinary line\r\n".repeat(3000));
        let mut source = source_context_test_source(&workspace);
        source["content_base64"] = json!(STANDARD.encode(&raw));
        source["revision"] = json!(tessera_core::decision_reuse::revision(raw.as_bytes()));
        let saved = packet(&goal, true);
        let (endpoint, stop, server) = source_context_test_server(source.clone(), saved.clone());
        let (view, visual) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&workspace, &store, window, cx)
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            v.endpoint = endpoint;
            v.busy = false;
            v.surface = Surface::Source;
            v.snapshot = json!({"goal":{"id":goal}});
            v.selected_goal_id = Some(goal.clone());
            v.capabilities["reviewed_context"] = json!(true);
            v.load_source(source.clone(), window, cx);
            v.ensure_context(window, cx);
            let state = v.context_ui.goals.get_mut(&goal).unwrap();
            state
                .guidance
                .update(cx, |i, cx| i.set_value("Keep unsent guidance", window, cx));
            state
                .query
                .update(cx, |i, cx| i.set_value("Keep query", window, cx));
            state.chosen.insert("local".into(), citation("local"));
            state.pinned.insert("local".into());
            state.selection_changed = true;
            v.toggle_source_projection(window, cx);
        });
        visual.run_until_parked();
        let (original_form, selection) = view.update_in(visual, |v, window, cx| {
            assert!(v.source_projection.live);
            let start = raw.find("😀План").unwrap();
            let end = start + "😀План".len();
            v.source
                .managed()
                .unwrap()
                .update(cx, |s, cx| s.set_selected_range(end..start, cx));
            let selection = v.source.link_selection(window, cx).unwrap();
            assert!(selection.reversed);
            assert_eq!(selection.text, "😀План");
            let form = v.source_context_form(&goal, cx);
            v.open_source_selection_context(window, cx);
            (form, selection)
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            let preview = v
                .context_ui
                .selection_preview
                .preview
                .as_ref()
                .expect("preview retained");
            assert!(preview.error.is_none(), "{:?}", preview.error);
            assert_eq!(
                preview.prepared.as_ref().unwrap().citation["excerpt"],
                "Before **😀План** after\r\n"
            );
            assert_eq!(v.source_context_form(&goal, cx), original_form);
            assert_eq!(v.source.link_selection(window, cx), Some(selection.clone()));
            v.cancel_source_selection_context(cx);
            assert_eq!(v.source_context_form(&goal, cx), original_form);
            assert_eq!(v.source.value(cx).as_ref(), raw);
            assert!(v.source_projection.live);
            v.open_source_selection_context(window, cx);
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            v.add_source_selection_context(window, cx)
        });
        visual.run_until_parked();
        let added_form = view.update_in(visual, |v, window, cx| {
            assert!(v.context_ui.selection_preview.preview.is_none());
            assert!(v.error.is_none(), "{:?}", v.error);
            assert!(v.surface == Surface::Context);
            let state = &v.context_ui.goals[&goal];
            assert_eq!(
                state.guidance.read(cx).value().as_ref(),
                "Keep unsent guidance"
            );
            assert_eq!(state.query.read(cx).value().as_ref(), "Keep query");
            assert!(state.pinned.contains("local"));
            assert_eq!(state.packet, saved);
            let actual = state
                .chosen
                .values()
                .find(|c| c["path"] == source["path"])
                .unwrap();
            assert_eq!(actual["start_line"], 6);
            assert_eq!(actual["end_line"], 6);
            assert_eq!(actual["metadata"]["verification"], "unverified");
            assert_eq!(v.source.value(cx).as_ref(), raw);
            assert_eq!(v.source.link_selection(window, cx), Some(selection.clone()));
            assert!(v.source_projection.live);
            let form = v.source_context_form(&goal, cx);
            v.surface = Surface::Source;
            v.open_source_selection_context(window, cx);
            form
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            v.add_source_selection_context(window, cx)
        });
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            assert_eq!(v.source_context_form(&goal, cx), added_form);
            assert!(v.notice.starts_with("Already included"));
        });
        stop.store(true, Ordering::Relaxed);
        let requests = server.join().unwrap();
        assert_eq!(
            requests.iter().filter(|r| r["op"] == "source_read").count(),
            5
        );
        assert_eq!(
            requests.iter().filter(|r| r["op"] == "context_get").count(),
            3
        );
        assert!(requests
            .iter()
            .all(|r| r["expected_workspace"] == workspace));
        assert!(!directory.exists());
    }

    #[gpui::test]
    fn source_selection_context_canceled_late_and_changed_owners_never_stage(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let goal = uuid();
        let workspace = json!({"brain_id":uuid(),"root":"/isolated/source-selection","records_dir":"records","managed":true});
        let directory = std::env::temp_dir().join(format!("source-selection281-{}", uuid()));
        let store = editor_recovery::EditorRecovery::at(directory.clone(), &workspace).unwrap();
        let source = source_context_test_source(&workspace);
        let (endpoint, stop, server) = source_context_test_server(source.clone(), Value::Null);
        let (view, visual) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&workspace, &store, window, cx)
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            v.endpoint = endpoint;
            v.busy = false;
            v.surface = Surface::Source;
            v.snapshot = json!({"goal":{"id":goal}});
            v.selected_goal_id = Some(goal.clone());
            v.capabilities["reviewed_context"] = json!(true);
            v.load_source(source.clone(), window, cx);
        });
        visual.run_until_parked();
        for change in 0..6 {
            view.update_in(visual, |v, window, cx| {
                v.surface = Surface::Source;
                v.selected_goal_id = Some(goal.clone());
                v.snapshot["goal"]["id"] = json!(goal);
                v.source_snapshot = Some(source.clone());
                v.endpoint = endpoint;
                v.source
                    .managed()
                    .unwrap()
                    .update(cx, |s, cx| s.set_selected_range(0..3, cx));
                v.open_source_selection_context(window, cx);
                assert!(v.context_ui.selection_preview.preview.is_some());
                match change {
                    0 => v.cancel_source_selection_context(cx),
                    1 => v
                        .source
                        .managed()
                        .unwrap()
                        .update(cx, |s, cx| s.set_selected_range(4..5, cx)),
                    2 => v.snapshot["goal"]["id"] = json!(uuid()),
                    3 => v.source_snapshot.as_mut().unwrap()["revision"] = json!("changed"),
                    4 => v.surface = Surface::Details,
                    _ => v.endpoint = "127.0.0.1:9".parse().unwrap(),
                }
            });
            visual.run_until_parked();
            view.update_in(visual, |v, _, cx| {
                assert!(v.context_ui.selection_preview.preview.is_none());
                assert!(v.source_context_form(&goal, cx).is_none());
            });
        }
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
        assert!(!directory.exists());
    }
    #[gpui::test]
    fn source_selection_context_add_rechecks_saved_source_packet_and_local_draft(
        cx: &mut TestAppContext,
    ) {
        changed_saved_inputs(cx);
    }
    fn changed_saved_inputs(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let goal = uuid();
        let workspace = json!({"brain_id":uuid(),"root":"/isolated/source-selection","records_dir":"records","managed":true});
        let directory = std::env::temp_dir().join(format!("source-selection281-{}", uuid()));
        let store = editor_recovery::EditorRecovery::at(directory.clone(), &workspace).unwrap();
        let source = source_context_test_source(&workspace);
        let disk = Arc::new(std::sync::Mutex::new(source.clone()));
        let saved = Arc::new(std::sync::Mutex::new(Value::Null));
        let (endpoint, stop, server) =
            source_context_changing_test_server(disk.clone(), saved.clone());
        let (view, visual) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&workspace, &store, window, cx)
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            v.endpoint = endpoint;
            v.busy = false;
            v.surface = Surface::Source;
            v.snapshot = json!({"goal":{"id":goal}});
            v.selected_goal_id = Some(goal.clone());
            v.capabilities["reviewed_context"] = json!(true);
            v.load_source(source.clone(), window, cx);
        });
        visual.run_until_parked();
        for changed in 0..3 {
            *disk.lock().unwrap() = source.clone();
            *saved.lock().unwrap() = Value::Null;
            view.update_in(visual, |v, window, cx| {
                v.source
                    .managed()
                    .unwrap()
                    .update(cx, |s, cx| s.set_selected_range(44..49, cx));
                v.open_source_selection_context(window, cx);
            });
            visual.run_until_parked();
            view.update_in(visual, |v, window, cx| {
                let preview = v.context_ui.selection_preview.preview.as_ref().unwrap();
                assert!(preview.prepared.is_some(), "{:?}", preview.error);
                match changed {
                    0 => disk.lock().unwrap()["revision"] = json!("changed-on-disk"),
                    1 => *saved.lock().unwrap() = packet(&goal, true),
                    _ => {
                        v.ensure_context(window, cx);
                        v.context_ui
                            .goals
                            .get_mut(&goal)
                            .unwrap()
                            .guidance
                            .update(cx, |i, cx| i.set_value("New local guidance", window, cx));
                    }
                }
                v.add_source_selection_context(window, cx);
            });
            visual.run_until_parked();
            view.update_in(visual, |v, _, cx| {
                if changed < 2 {
                    let preview = v.context_ui.selection_preview.preview.as_ref().unwrap();
                    assert!(preview.error.as_ref().unwrap().contains("changed"));
                    assert!(v.source_context_form(&goal, cx).is_none());
                } else {
                    assert!(v.context_ui.selection_preview.preview.is_none());
                    assert_eq!(
                        v.source_context_form(&goal, cx).unwrap()["guidance"],
                        "New local guidance"
                    );
                    assert!(v.context_ui.goals[&goal].chosen.is_empty());
                }
                v.cancel_source_selection_context(cx);
            });
        }
        for capture in [true, false] {
            *disk.lock().unwrap() = source.clone();
            *saved.lock().unwrap() = Value::Null;
            let before = view.update_in(visual, |v, window, cx| {
                v.show_capture = false;
                v.collection = Collection::Sources;
                v.surface = Surface::Source;
                v.source
                    .managed()
                    .unwrap()
                    .update(cx, |s, cx| s.set_selected_range(44..49, cx));
                let before = v.source_context_form(&goal, cx);
                v.open_source_selection_context(window, cx);
                before
            });
            visual.run_until_parked();
            view.update_in(visual, |v, window, cx| {
                assert!(v
                    .context_ui
                    .selection_preview
                    .preview
                    .as_ref()
                    .unwrap()
                    .prepared
                    .is_some());
                v.add_source_selection_context(window, cx);
                assert!(
                    v.context_ui
                        .selection_preview
                        .preview
                        .as_ref()
                        .unwrap()
                        .pending
                );
                if capture {
                    v.begin_capture(window, cx);
                } else {
                    v.select_collection(Collection::Goals, window, cx);
                }
            });
            visual.run_until_parked();
            view.update_in(visual, |v, _, cx| {
                assert!(v.context_ui.selection_preview.preview.is_none());
                assert_eq!(v.source_context_form(&goal, cx), before);
                assert!(v.surface == Surface::Source);
                if capture {
                    assert!(v.show_capture);
                } else {
                    assert!(v.collection == Collection::Goals);
                }
            });
        }
        view.update_in(visual, |v, window, cx| {
            v.source.managed().unwrap().update(cx, |s, cx| {
                s.replace_and_mark_text_in_range(None, "中", Some(0..1), window, cx)
            });
            let composing = v.source.value(cx);
            v.open_source_selection_context(window, cx);
            assert!(v.context_ui.selection_preview.preview.is_none());
            assert_eq!(v.source.value(cx), composing);
        });
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
    }
}
