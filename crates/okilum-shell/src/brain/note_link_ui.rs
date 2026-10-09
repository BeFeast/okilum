//! A local chooser; the existing backend preview proves the chosen destination.
use super::source_input::LinkSelection;
use super::*;
use gpui_component::input::projection::SourceStamp;

#[derive(Clone, PartialEq)]
struct Owner {
    endpoint: SocketAddr,
    workspace: Value,
    goal: String,
    entity: EntityId,
    path: String,
    revision: Value,
    stamp: SourceStamp,
    sources: Vec<Value>,
}
struct Picker {
    owner: Owner,
    selection: LinkSelection,
    query: Entity<InputState>,
    _query_subscription: Subscription,
    pending: bool,
    error: Option<String>,
}
#[derive(Default)]
pub(super) struct NoteLinkUi {
    generation: u64,
    picker: Option<Box<Picker>>,
}
impl NoteLinkUi {
    pub(super) fn active(&self) -> bool {
        self.picker.is_some()
    }
}
impl BrainView {
    pub(super) fn note_link_enabled(&self) -> bool {
        self.source.managed().is_some()
            && !self.source_find.active()
            && self.expected_workspace.is_some()
            && self.capabilities["source_write"] == true
            && !self.source_readonly()
            && self.source_conflict.is_none()
            && self.pending_source_write.is_none()
            && self.pending_navigation.is_none()
            && self.editor_can_begin_criteria()
            && self.surface == Surface::Source
    }
    fn note_link_owner(&self, cx: &App) -> Option<Owner> {
        let source = self.source_snapshot.as_ref()?;
        Some(Owner {
            endpoint: self.endpoint,
            workspace: self.expected_workspace.clone()?,
            goal: self.goal_id(),
            entity: self.source.entity_id(),
            path: source["path"].as_str()?.to_owned(),
            revision: source["revision"].clone(),
            stamp: self.source.stamp(cx)?,
            sources: self.source_list.clone(),
        })
    }
    pub(super) fn open_note_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.note_link_enabled() {
            return;
        }
        let Some(owner) = self.note_link_owner(cx) else {
            return;
        };
        let Some(selection) = self.source.link_selection(window, cx) else {
            self.error =
                Some("Finish the current text composition before inserting a note link.".into());
            cx.notify();
            return;
        };
        if let Err(error) = note_link::insertion("note.md", &selection.text) {
            self.error = Some(error);
            cx.notify();
            return;
        }
        let query =
            cx.new(|cx| InputState::new(window, cx).placeholder("Find a note by title or path"));
        let query_subscription = cx.subscribe(&query, |_, _, _: &InputEvent, cx| cx.notify());
        self.note_link.generation = self.note_link.generation.wrapping_add(1);
        self.note_link.picker = Some(Box::new(Picker {
            owner,
            selection,
            query: query.clone(),
            _query_subscription: query_subscription,
            pending: false,
            error: None,
        }));
        self.error = None;
        query.focus_handle(cx).focus(window, cx);
        cx.notify();
    }
    fn note_link_matches(
        &self,
        owner: &Owner,
        selection: &LinkSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.note_link_enabled()
            && self.note_link_owner(cx).as_ref() == Some(owner)
            && self.source.link_selection(window, cx).as_ref() == Some(selection)
    }
    fn cancel_note_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.note_link.generation = self.note_link.generation.wrapping_add(1);
        if let Some(picker) = self.note_link.picker.take() {
            if self.note_link_matches(&picker.owner, &picker.selection, window, cx) {
                self.source.focus(window, cx);
            }
        }
        cx.notify();
    }
    fn choose_note_link(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = &self.note_link.picker else {
            return;
        };
        if picker.pending {
            return;
        }
        let owner = picker.owner.clone();
        let selection = picker.selection.clone();
        let request = (|| {
            if !self.note_link_matches(&owner, &selection, window, cx) {
                return Err("The source, selection or workspace changed. Cancel and open Insert note link again.".to_owned());
            }
            let target = note_link::target(&path, &owner.sources)?;
            let insertion = note_link::insertion(&target, &selection.text)?;
            Ok((target, insertion))
        })();
        let (target, insertion) = match request {
            Ok(request) => request,
            Err(error) => {
                self.note_link.picker.as_mut().unwrap().error = Some(error);
                cx.notify();
                return;
            }
        };
        let generation = self.note_link.generation;
        let picker = self.note_link.picker.as_mut().unwrap();
        picker.pending = true;
        picker.error = None;
        cx.spawn_in(window, async move |this, cx| {
            let probe_owner = owner.clone(); let probe_target = target.clone();
            let result = cx.background_executor().spawn(async move {
                let reply = rpc_guarded(probe_owner.endpoint,
                    json!({"op":"source_preview","path":probe_owner.path,"content_base64":STANDARD.encode(note_link::probe(&probe_target))}), Some(&probe_owner.workspace))?;
                note_link::verify(&reply, &probe_owner.path, &probe_owner.revision, &probe_target)
            }).await;
            let _ = this.update_in(cx, |this, window, cx| this.finish_note_link(generation, &owner, &selection, &insertion, result, window, cx));
        }).detach();
        cx.notify();
    }
    #[allow(clippy::too_many_arguments)]
    fn finish_note_link(
        &mut self,
        generation: u64,
        owner: &Owner,
        selection: &LinkSelection,
        insertion: &str,
        result: Result<(), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.note_link.generation != generation || self.note_link.picker.is_none() {
            return;
        }
        let result = result.and_then(|()| {
            if !self.note_link_matches(owner, selection, window, cx) {
                return Err("The source, selection or workspace changed while checking the link. Your draft is unchanged; cancel and choose again.".into());
            }
            self.sync_source_policy(cx);
            if !self.source.replace_link_selection(selection, insertion, window, cx) {
                return Err("The source selection is no longer available. Your draft is unchanged.".into());
            }
            Ok(())
        });
        match result {
            Ok(()) => {
                self.note_link.picker = None;
                self.note_link.generation = self.note_link.generation.wrapping_add(1);
                self.source.focus(window, cx);
                self.notice = "Note link inserted in your draft. Save when ready.".into();
            }
            Err(error) => {
                let picker = self.note_link.picker.as_mut().unwrap();
                picker.pending = false;
                picker.error = Some(error);
            }
        }
        cx.notify();
    }
    pub(super) fn note_link_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picker = self.note_link.picker.as_ref()?;
        let query = picker.query.read(cx).value().to_lowercase();
        let mut rows = v_flex()
            .id("note-link-results")
            .max_h(px(180.))
            .overflow_y_scroll()
            .gap_1();
        let mut count = 0;
        for source in picker
            .owner
            .sources
            .iter()
            .filter(|s| {
                text(&s["title"]).to_lowercase().contains(&query)
                    || text(&s["path"]).to_lowercase().contains(&query)
            })
            .take(100)
        {
            let path = text(&source["path"]);
            let selected = path.clone();
            count += 1;
            rows = rows.child(
                super::super::brand::control(SharedString::from(format!("note-link-{path}")), cx)
                    .ghost()
                    .w_full()
                    .h_auto()
                    .py_3()
                    .justify_start()
                    .disabled(picker.pending)
                    .child(
                        v_flex()
                            .flex_1()
                            .w_full()
                            .items_start()
                            .text_left()
                            .gap_1()
                            .min_w_0()
                            .child(div().whitespace_normal().child(text(&source["title"])))
                            .child(div().text_xs().whitespace_normal().child(path)),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose_note_link(selected.clone(), window, cx)
                    })),
            );
        }
        if count == 0 {
            rows = rows.child(
                div()
                    .text_sm()
                    .child("No matching notes. Refine the search or refresh Sources."),
            );
        }
        Some(
            v_flex()
                .gap_2()
                .p_3()
                .rounded(px(8.))
                .border_1()
                .border_color(cx.theme().border)
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child("Insert note link"))
                        .child(Button::new("cancel-note-link").label("Cancel").on_click(
                            cx.listener(|this, _, window, cx| this.cancel_note_link(window, cx)),
                        )),
                )
                .child(Input::new(&picker.query))
                .child(rows)
                .when(picker.pending, |p| {
                    p.child(div().text_sm().child("Checking destination…"))
                })
                .when_some(picker.error.clone(), |p, error| {
                    p.child(div().text_sm().child(error))
                })
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::TestAppContext;

    #[gpui::test]
    fn note_link_late_response_cannot_edit_changed_owner_selection_or_cancelled_chooser(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let directory = std::env::temp_dir().join(format!("note-link271-{}", uuid()));
        let workspace = json!({"brain_id":uuid(),"root":"/isolated/note-link","records_dir":"records","managed":true});
        let store = editor_recovery::EditorRecovery::at(directory.clone(), &workspace).unwrap();
        let (view, visual) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&workspace, &store, window, cx)
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            v.busy = false;
            v.surface = Surface::Source;
            v.snapshot = json!({"goal":{"id":uuid()}});
            v.capabilities = json!({"source_write":true});
            v.source_editable = true;
            v.source_loading = false;
            v.source_snapshot = Some(json!({"path":"a.md","revision":"r1"}));
            v.source_list = vec![json!({"path":"b.md","title":"Target"})];
            v.source.reset("Keep 😀 text", window, cx);
            v.sync_source_policy(cx);
            assert!(v.note_link_enabled());
            for changed in 0..6 {
                v.open_note_link(window, cx);
                let generation = v.note_link.generation;
                let picker = v.note_link.picker.as_ref().unwrap();
                let owner = picker.owner.clone();
                let selection = picker.selection.clone();
                let before = v.source.value(cx);
                match changed {
                    0 => v.snapshot["goal"]["id"] = json!(uuid()),
                    1 => v.source_snapshot.as_mut().unwrap()["path"] = json!("changed.md"),
                    2 => v.expected_workspace.as_mut().unwrap()["brain_id"] = json!(uuid()),
                    3 => v.source_list.push(json!({"path":"another.md"})),
                    4 => v
                        .source
                        .managed()
                        .unwrap()
                        .update(cx, |s, cx| s.set_selected_range(0..4, cx)),
                    _ => v.cancel_note_link(window, cx),
                }
                v.finish_note_link(generation, &owner, &selection, "BAD", Ok(()), window, cx);
                assert_eq!(v.source.value(cx), before, "late case {changed}");
                v.cancel_note_link(window, cx);
            }
            // Positive control: the same current owner/selection permits one
            // replacement after destination proof, without a Save request.
            v.open_note_link(window, cx);
            let picker = v.note_link.picker.as_ref().unwrap();
            let owner = picker.owner.clone();
            let selection = picker.selection.clone();
            v.finish_note_link(
                v.note_link.generation,
                &owner,
                &selection,
                "[[b.md|Keep]]",
                Ok(()),
                window,
                cx,
            );
            assert_eq!(v.source.value(cx).as_ref(), "[[b.md|Keep]] 😀 text");
            assert!(v.note_link.picker.is_none());
            assert!(v.pending_source_write.is_none());
        });
        visual.run_until_parked();
        let _ = std::fs::remove_dir_all(directory);
    }
}
