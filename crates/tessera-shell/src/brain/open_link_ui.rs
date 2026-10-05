//! Explicit managed-source link action using accepted classifier and preview proof.
use super::*;
use gpui_component::input::projection::SourceStamp;
use source_input::LinkSelection;
use tessera_core::source_classifier::NoteLink;

#[derive(Clone, PartialEq)]
pub(super) struct PreviewOwner {
    endpoint: SocketAddr,
    workspace: Value,
    goal: String,
    collection: Collection,
    entity: EntityId,
    source: Value,
    stamp: SourceStamp,
    surface: Surface,
    capture: bool,
    epoch: u64,
}
struct Proof {
    owner: PreviewOwner,
    generation: u64,
}
struct Chooser {
    generation: u64,
    owner: PreviewOwner,
    preview_generation: u64,
    selection: LinkSelection,
    live: bool,
    candidates: Vec<Value>,
    heading: Option<String>,
}
#[derive(Default)]
pub(super) struct OpenLinkUi {
    epoch: u64,
    generation: u64,
    proof: Option<Proof>,
    chooser: Option<Chooser>,
}

fn link_target(link: &NoteLink) -> Result<(String, Option<String>), &'static str> {
    match tessera_core::document_links::destination(&link.target, link.wiki) {
        tessera_core::document_links::Destination::Note { heading, .. } => Ok((
            if link.wiki {
                link.target.trim().into()
            } else {
                link.target.clone()
            },
            heading,
        )),
        tessera_core::document_links::Destination::External(_) => Ok((link.target.clone(), None)),
        tessera_core::document_links::Destination::Unsupported(reason) => Err(reason),
    }
}

fn selected_link(links: &[NoteLink], selection: &LinkSelection) -> Result<NoteLink, &'static str> {
    let mut matches = links.iter().filter(|link| {
        link.range.start <= selection.bytes.start
            && selection.bytes.start < link.range.end
            && selection.bytes.end <= link.range.end
    });
    let link = matches
        .next()
        .ok_or("Place the caret inside one supported note link, or select text within it.")?;
    if matches.next().is_some() {
        return Err("Select exactly one note link.");
    }
    Ok(link.clone())
}
fn resolved_row<'a>(
    preview: &'a Value,
    source: &Value,
    raw: &str,
    target: &str,
    wiki: bool,
) -> Result<&'a Value, &'static str> {
    if preview["path"] != source["path"]
        || preview["revision"] != source["revision"]
        || preview["preview_revision"] != note_link::digest(raw.as_bytes())
    {
        return Err("The note preview is stale. Wait for the current preview or reload the saved note, then open the link again.");
    }
    if preview["document_links_version"] != 1 {
        return Err("This backend does not support consistent document links. Update the backend to open this link; source is unchanged.");
    }
    let links = preview["links"]
        .as_array()
        .ok_or("This note preview has no link information.")?;
    let mut matches = links
        .iter()
        .filter(|row| row["authored_target"] == target && row["wiki"] == wiki);
    let row = matches
        .next()
        .ok_or("This link has no matching preview result. Use Source and its rendered preview.")?;
    if matches.next().is_some() {
        return Err("The note preview has conflicting link results.");
    }
    if !row["url"].is_string() || row["candidates"].as_array().is_none() {
        return Err("The note preview link result is incomplete.");
    }
    Ok(row)
}

impl BrainView {
    pub(super) fn open_link_preview_owner(&self, cx: &App) -> Option<PreviewOwner> {
        Some(PreviewOwner {
            endpoint: self.endpoint,
            workspace: self.expected_workspace.clone()?,
            goal: self.goal_id(),
            collection: self.collection,
            entity: self.source.managed()?.entity_id(),
            source: self.source_snapshot.clone()?,
            stamp: self.source.stamp(cx)?,
            surface: self.surface,
            capture: self.show_capture,
            epoch: self.open_link.epoch,
        })
    }
    pub(super) fn cancel_open_link_chooser(&mut self) {
        self.open_link.chooser = None;
        self.open_link.generation = self.open_link.generation.wrapping_add(1);
    }
    pub(super) fn clear_open_link_preview(&mut self) {
        self.open_link.epoch = self.open_link.epoch.wrapping_add(1);
        self.open_link.proof = None;
        self.open_link.chooser = None;
    }
    pub(super) fn accept_open_link_preview(
        &mut self,
        owner: Option<PreviewOwner>,
        generation: u64,
        cx: &App,
    ) {
        self.open_link.proof = owner
            .filter(|owner| self.open_link_preview_owner(cx).as_ref() == Some(owner))
            .map(|owner| Proof { owner, generation });
    }
    fn open_link_ready(&self, cx: &App) -> Result<(), &'static str> {
        if self.source.managed().is_none()
            || self.surface != Surface::Source
            || self.show_capture
            || matches!(self.collection, Collection::Inbox | Collection::Attention)
        {
            return Err("Open a managed Source note first.");
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
        {
            return Err(
                "Finish Find or the current draft/recovery first, then open the link again.",
            );
        }
        if self.source.value(cx).len() > tessera_core::source_classifier::MAX_BYTES {
            return Err("Open link at caret supports notes up to 64 KiB. Use Source and its rendered preview.");
        }
        if self.preview_loading || self.preview_error.is_some() {
            return Err("Wait for the current note preview, then open the link again.");
        }
        let proof = self
            .open_link
            .proof
            .as_ref()
            .ok_or("Reload the saved note to refresh its preview, then open the link again.")?;
        if proof.generation != self.preview_generation
            || self.open_link_preview_owner(cx).as_ref() != Some(&proof.owner)
        {
            return Err("The note preview belongs to an earlier source. Open the link again after refreshing it.");
        }
        Ok(())
    }
    pub(super) fn open_link_at_caret(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_link.chooser = None;
        self.open_link.generation = self.open_link.generation.wrapping_add(1);
        let result = (|| {
            self.open_link_ready(cx)?;
            let selection = self
                .source
                .link_selection(window, cx)
                .ok_or("Finish text composition before opening a note link.")?;
            let links = self
                .current_source_links(cx)
                .ok_or("Wait for the current source classification, then open the link again.")?;
            let link = selected_link(&links, &selection)?;
            let (target, heading) = link_target(&link)?;
            let source = self.source_snapshot.as_ref().unwrap();
            let row = resolved_row(
                &self.preview,
                source,
                &self.source.value(cx),
                &target,
                link.wiki,
            )?
            .clone();
            Ok::<_, &'static str>((selection, row, heading))
        })();
        let (selection, row, heading) = match result {
            Ok(value) => value,
            Err(error) => {
                self.error = Some(error.into());
                cx.notify();
                return;
            }
        };
        let candidates = array(&row["candidates"]);
        if candidates
            .iter()
            .any(|c| c["path"].as_str().is_none_or(str::is_empty))
        {
            self.error = Some("The linked-note candidates are incomplete.".into());
        } else if row["status"] == "external" {
            if let Some(url) = row["url"]
                .as_str()
                .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
            {
                cx.open_url(url);
            }
        } else if (row["status"] == "resolved" || row["status"] == "resolved_heading")
            && candidates.len() == 1
        {
            self.error = None;
            self.open_source_link_at(text(&candidates[0]["path"]), heading, window, cx);
        } else if (row["status"] == "ambiguous" || row["status"] == "ambiguous_heading")
            && !candidates.is_empty()
        {
            self.error = None;
            self.open_link.chooser = Some(Chooser {
                generation: self.open_link.generation,
                owner: self.open_link_preview_owner(cx).unwrap(),
                preview_generation: self.preview_generation,
                selection,
                live: self.source_projection.live,
                candidates,
                heading,
            });
        } else {
            self.error = Some(
                "This note link has no unique readable destination in the current workspace."
                    .into(),
            );
        }
        cx.notify();
    }
    fn open_link_chooser_matches(
        &self,
        chooser: &Chooser,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.open_link_ready(cx).is_ok()
            && self.current_source_links(cx).is_some()
            && self.open_link_preview_owner(cx).as_ref() == Some(&chooser.owner)
            && self.preview_generation == chooser.preview_generation
            && self.source_projection.live == chooser.live
            && self.source.link_selection(window, cx).as_ref() == Some(&chooser.selection)
    }
    pub(super) fn sync_open_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .open_link
            .chooser
            .as_ref()
            .is_some_and(|chooser| !self.open_link_chooser_matches(chooser, window, cx))
        {
            self.open_link.chooser = None;
        }
        if self
            .open_link
            .proof
            .as_ref()
            .is_some_and(|p| self.open_link_preview_owner(cx).as_ref() != Some(&p.owner))
        {
            self.open_link.proof = None;
        }
    }
    fn choose_open_link(
        &mut self,
        generation: u64,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .open_link
            .chooser
            .as_ref()
            .is_none_or(|c| c.generation != generation)
        {
            return;
        }
        let Some(chooser) = self.open_link.chooser.take() else {
            return;
        };
        if !self.open_link_chooser_matches(&chooser, window, cx) {
            cx.notify();
            return;
        }
        if let Some(candidate) = chooser.candidates.get(index) {
            self.open_source_link_at(text(&candidate["path"]), chooser.heading, window, cx);
        }
        cx.notify();
    }
    pub(super) fn open_link_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let chooser = self.open_link.chooser.as_ref()?;
        let generation = chooser.generation;
        let mut panel = v_flex()
            .id("source-open-link-chooser")
            .debug_selector(|| "source-open-link-chooser".into())
            .p_3()
            .gap_2()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Choose linked note"),
            );
        for (index, candidate) in chooser.candidates.iter().enumerate() {
            panel = panel.child(
                super::super::brand::control(("source-open-link-choice", index), cx)
                    .label(format!(
                        "{} · {}",
                        text(&candidate["title"]),
                        text(&candidate["path"])
                    ))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose_open_link(generation, index, window, cx)
                    })),
            );
        }
        Some(
            panel
                .child(
                    super::super::brand::control("source-open-link-cancel", cx)
                        .debug_selector(|| "source-open-link-cancel".into())
                        .label("Cancel")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.open_link.generation != generation {
                                return;
                            }
                            this.cancel_open_link_chooser();
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::EntityInputHandler;

    fn setup(
        cx: &mut TestAppContext,
    ) -> (
        Entity<BrainView>,
        &mut VisualTestContext,
        std::path::PathBuf,
    ) {
        cx.update(gpui_component::init);
        let workspace = json!({"brain_id":uuid(),"root":"/isolated/open-link285","records_dir":"records","managed":true});
        let directory = std::env::temp_dir().join(format!("open-link285-{}", uuid()));
        let store = editor_recovery::EditorRecovery::at(directory.clone(), &workspace).unwrap();
        let (view, visual) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&workspace, &store, window, cx)
        });
        visual.run_until_parked();
        view.update_in(visual, |v,window,cx| {
            v.busy=false; v.surface=Surface::Source; v.collection=Collection::Sources;
            v.snapshot=json!({"goal":{"id":uuid()}});
            let raw="\u{feff}# Source\r\n\r\n[[shared|😀 alias]]\r\n";
            let source=json!({"schema":SCHEMA,"brain_id":workspace["brain_id"],"path":"notes/a.md","revision":note_link::digest(raw.as_bytes()),"content_base64":STANDARD.encode(raw),"media_type":"text/markdown"});
            v.load_source(source,window,cx);
            v.source_projection.live=true;
            v.schedule_source_projection(window,cx);
            v.source.managed().unwrap().update(cx,|s,cx| {
                let start=raw.find("😀").unwrap(); s.set_selected_range((start+"😀 alias".len())..start,cx);
            });
        });
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| ready(v, cx));
        (view, visual, directory)
    }
    fn ready(v: &mut BrainView, cx: &mut Context<BrainView>) {
        v.preview_loading = false;
        v.preview_error = None;
        let source = v.source_snapshot.as_ref().unwrap();
        v.preview = json!({"document_links_version":1,"path":source["path"],"revision":source["revision"],"preview_revision":note_link::digest(v.source.value(cx).as_bytes()),"links":[{"wiki":true,"target":"shared","authored_target":"shared","url":"tessera://ambiguous/shared","status":"ambiguous","candidates":[{"path":"left/shared.md","title":"Left"},{"path":"right/shared.md","title":"Right"}]}]});
        v.accept_open_link_preview(v.open_link_preview_owner(cx), v.preview_generation, cx);
        assert!(v.open_link_ready(cx).is_ok());
        assert_eq!(v.current_source_links(cx).unwrap().len(), 1);
        v.apply_source_projection(cx);
        cx.notify();
    }
    fn cleanup(directory: std::path::PathBuf) {
        if directory.exists() {
            std::fs::remove_dir_all(directory).unwrap();
        }
    }
    #[test]
    fn open_link_half_open_selection_and_preview_metadata_are_exact() {
        let link = NoteLink {
            range: 3..20,
            label: 8..12,
            target: " dir/Note.md ".into(),
            wiki: true,
        };
        let selection = |start, end| LinkSelection {
            bytes: start..end,
            utf16: 0..0,
            reversed: false,
            text: String::new(),
        };
        for (start, end) in [(3, 3), (19, 19), (4, 20), (3, 20)] {
            assert!(selected_link(std::slice::from_ref(&link), &selection(start, end)).is_ok());
        }
        for (start, end) in [(2, 3), (20, 20), (3, 21)] {
            assert!(selected_link(std::slice::from_ref(&link), &selection(start, end)).is_err());
        }
        assert_eq!(link_target(&link).unwrap(), ("dir/Note.md".into(), None));
        for target in ["x#", "x^block"] {
            let mut bad = link.clone();
            bad.target = target.into();
            assert!(link_target(&bad).is_err());
        }
        let mut heading = link.clone();
        heading.target = "dir/Note.md#Decision: C#".into();
        assert_eq!(
            link_target(&heading).unwrap(),
            (
                "dir/Note.md#Decision: C#".into(),
                Some("Decision: C#".into())
            )
        );
        let mut md = link;
        md.wiki = false;
        md.target = "../space%20name.md".into();
        assert_eq!(
            link_target(&md).unwrap(),
            ("../space%20name.md".into(), None)
        );
        let source = json!({"path":"a.md","revision":"r1"});
        let preview = json!({"document_links_version":1,"path":"a.md","revision":"r1","preview_revision":note_link::digest(b"raw"),"links":[{"wiki":true,"target":"x","authored_target":"x","url":"tessera://x","candidates":[]}]});
        assert!(resolved_row(&preview, &source, "raw", "x", true).is_ok());
        for (pointer, value) in [
            ("/path", json!("b.md")),
            ("/revision", json!("r2")),
            ("/preview_revision", json!("stale")),
        ] {
            let mut stale = preview.clone();
            *stale.pointer_mut(pointer).unwrap() = value;
            assert!(resolved_row(&stale, &source, "raw", "x", true).is_err());
        }
    }
    #[gpui::test]
    fn open_link_actual_lp_button_shows_owned_chooser_and_cancel(cx: &mut TestAppContext) {
        let (view, visual, directory) = setup(cx);
        visual.run_until_parked();
        let button = visual
            .debug_bounds("source-open-link")
            .expect("actual toolbar action is painted");
        visual.simulate_click(button.center(), Modifiers::default());
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert!(v.source_projection.live);
            assert!(
                v.open_link.chooser.is_some(),
                "actual action admitted reversed Unicode selection"
            );
            assert!(v.source.link_selection(window, cx).unwrap().reversed);
            assert!(!v.source_navigation_pending());
            assert!(!v.dirty(cx));
        });
        assert!(
            visual.debug_bounds("source-open-link-chooser").is_some(),
            "chooser painted while rendered preview is hidden in LP"
        );
        let cancel = visual.debug_bounds("source-open-link-cancel").unwrap();
        visual.simulate_click(cancel.center(), Modifiers::default());
        visual.run_until_parked();
        view.update_in(visual, |v, _, _| {
            assert!(v.open_link.chooser.is_none());
            assert!(!v.source_navigation_pending());
        });
        cleanup(directory);
    }
    #[gpui::test]
    fn open_link_stale_chooser_selection_mode_and_departure_cannot_navigate(
        cx: &mut TestAppContext,
    ) {
        let (view, visual, directory) = setup(cx);
        view.update_in(visual, |v, window, cx| {
            v.open_link_at_caret(window, cx);
            assert!(v.open_link.chooser.is_some());
            let old_generation = v.open_link.generation;
            v.open_link_at_caret(window, cx);
            v.choose_open_link(old_generation, 0, window, cx);
            assert!(
                v.open_link.chooser.is_some(),
                "stale row cannot consume a newer chooser"
            );
            assert!(!v.source_navigation_pending());
            v.source
                .managed()
                .unwrap()
                .update(cx, |s, cx| s.set_selected_range(0..0, cx));
            v.choose_open_link(v.open_link.generation, 0, window, cx);
            assert!(!v.source_navigation_pending());
            assert!(v.open_link.chooser.is_none());
            let start = v.source.value(cx).find("shared").unwrap();
            v.source
                .managed()
                .unwrap()
                .update(cx, |s, cx| s.set_selected_range(start..start, cx));
            v.open_link_at_caret(window, cx);
            assert!(v.open_link.chooser.is_some());
            v.toggle_source_projection(window, cx);
            v.choose_open_link(v.open_link.generation, 0, window, cx);
            assert!(!v.source_navigation_pending());
            v.open_link_at_caret(window, cx);
            assert!(v.open_link.chooser.is_some());
            v.begin_capture(window, cx);
            assert!(v.open_link.chooser.is_none());
            v.show_capture = false;
            v.choose_open_link(v.open_link.generation, 0, window, cx);
            assert!(!v.source_navigation_pending());
            let departed_owner = v.open_link_preview_owner(cx);
            ready(v, cx);
            for owner_case in 0..3 {
                v.open_link_at_caret(window, cx);
                assert!(v.open_link.chooser.is_some());
                let owner = v.open_link_preview_owner(cx).unwrap();
                match owner_case {
                    0 => v.snapshot["goal"]["id"] = json!("other"),
                    1 => v.expected_workspace.as_mut().unwrap()["brain_id"] = json!(uuid()),
                    _ => v.endpoint = "127.0.0.1:2".parse().unwrap(),
                }
                v.choose_open_link(v.open_link.generation, 0, window, cx);
                assert!(!v.source_navigation_pending());
                v.snapshot["goal"]["id"] = json!(owner.goal);
                v.expected_workspace = Some(owner.workspace);
                v.endpoint = owner.endpoint;
                ready(v, cx);
            }
            v.open_link_at_caret(window, cx);
            assert!(v.open_link.chooser.is_some());
            v.select_collection(Collection::Goals, window, cx);
            v.select_collection(Collection::Sources, window, cx);
            v.accept_open_link_preview(departed_owner, v.preview_generation, cx);
            assert!(
                v.open_link.proof.is_none(),
                "leave/return epoch rejects late matching preview"
            );
            assert!(v.open_link.chooser.is_none());
            v.choose_open_link(v.open_link.generation, 0, window, cx);
            assert!(!v.source_navigation_pending());
        });
        cleanup(directory);
    }
    #[gpui::test]
    fn open_link_dirty_ime_and_late_preview_never_open_or_write(cx: &mut TestAppContext) {
        let (view, visual, directory) = setup(cx);
        view.update_in(visual, |v, window, cx| {
            assert_ne!(
                v.capabilities["source_write"], true,
                "read-only source is eligible"
            );
            let old = v.open_link_preview_owner(cx);
            v.preview_generation += 1;
            v.open_link_at_caret(window, cx);
            assert!(!v.source_navigation_pending());
            assert!(v.open_link.chooser.is_none());
            v.accept_open_link_preview(old, v.preview_generation - 1, cx);
            assert!(v.open_link_ready(cx).is_err());
            ready(v, cx);
            v.capabilities["source_write"] = json!(true);
            v.source.set_readonly(false, cx);
            v.source.managed().unwrap().update(cx, |s, cx| {
                s.replace_and_mark_text_in_range(None, "中", Some(0..1), window, cx)
            });
            assert!(v.source.link_selection(window, cx).is_none());
            let raw = v.source.value(cx);
            v.open_link_at_caret(window, cx);
            assert!(v.open_link.chooser.is_none());
            assert!(!v.source_navigation_pending());
            assert_eq!(v.source.value(cx), raw);
            assert!(v.dirty(cx));
        });
        cleanup(directory);
    }
    #[gpui::test]
    fn source_return_keeps_open_link_ready_on_idempotent_source_tab(cx: &mut TestAppContext) {
        let (view, visual, directory) = setup(cx);
        view.update_in(visual, |v, window, cx| {
            let before = v.source.link_selection(window, cx);
            let stamp = v.source.stamp(cx);
            let generation = v.preview_generation;
            v.note_navigate_surface(Surface::Source, window, cx);
            assert_eq!(v.source.stamp(cx), stamp);
            assert_eq!(v.source.link_selection(window, cx), before);
            assert_eq!(
                v.preview_generation, generation,
                "idempotent tab does not issue another preview"
            );
            assert!(
                v.open_link_ready(cx).is_ok(),
                "same Source tab must preserve current owned proof"
            );
            v.open_link_at_caret(window, cx);
            assert!(v.open_link.chooser.is_some());
        });
        cleanup(directory);
    }
    #[gpui::test]
    fn prepared_missing_rendered_click_retains_managed_selection_history_and_notices(
        cx: &mut TestAppContext,
    ) {
        let (view, visual, directory) = setup(cx);
        view.update_in(visual, |v, window, cx| {
            let selection = v.source.link_selection(window, cx);
            let stamp = v.source.stamp(cx);
            let path = v.source_snapshot.clone();
            v.error = Some("Existing notice".into());
            let generation = v.preview_generation;
            v.preview["prepared_links_version"] = json!(1);
            for status in ["missing_document", "missing_heading"] {
                v.preview["links"] = json!([{"url":"tessera://known-missing", "status":"unresolved", "candidates":[], "prepared":{"status":status,"reason":"Missing destination","target_revision":null}}]);
                v.preview_link("tessera://known-missing", window, cx);
                assert_eq!(v.error.as_deref(), Some("Existing notice"));
                assert_eq!(v.source.link_selection(window, cx), selection);
                assert_eq!(v.source.stamp(cx), stamp);
                assert_eq!(v.source_snapshot, path);
                assert_eq!(v.preview_generation, generation);
                assert!(!v.source_navigation_pending());
            }
            // Unknown is not silently dead: the existing operation/error path
            // remains observable when authoritative evidence is unavailable.
            v.preview["links"][0]["prepared"]["status"] = json!("unknown");
            v.preview_link("tessera://known-missing", window, cx);
            assert_ne!(v.error.as_deref(), Some("Existing notice"));
        });
        cleanup(directory);
    }

    #[gpui::test]
    fn rendered_click_keeps_grammar_identity_and_refuses_conflicting_rows(cx: &mut TestAppContext) {
        let (view, visual, directory) = setup(cx);
        view.update_in(visual, |v, window, cx| {
            let wiki = json!({"url":"tessera://ambiguous/same.md%23Landing","status":"ambiguous_heading","heading":"Landing","candidates":[{"path":"notes/same.md"},{"path":"notes/Same.md"},{"path":"other/same.md"}]});
            let md = json!({"url":"tessera://ambiguous-markdown/same.md%23Landing","status":"ambiguous_heading","heading":"Landing","candidates":[{"path":"notes/Same.md"},{"path":"notes/same.md"}]});
            v.preview["links"] = json!([wiki, md]);
            v.preview_link("tessera://ambiguous-markdown/same.md%23Landing", window, cx);
            assert_eq!(v.link_candidates.len(), 2);
            assert!(v.link_candidates.iter().all(|c| c["path"].as_str().unwrap().starts_with("notes/") && c["heading"] == "Landing"));
            v.preview_link("tessera://ambiguous/same.md%23Landing", window, cx);
            assert_eq!(v.link_candidates.len(), 3, "wiki positive control");
            v.link_candidates.clear();
            v.preview["links"][1]["url"] = json!("tessera://ambiguous/same.md%23Landing");
            v.preview_link("tessera://ambiguous/same.md%23Landing", window, cx);
            assert!(v.error.as_ref().unwrap().contains("conflicting"));
            assert!(v.link_candidates.is_empty());
            assert!(!v.source_navigation_pending());
            v.preview["document_links_version"] = Value::Null;
            v.preview_link("tessera://ambiguous/same.md%23Landing", window, cx);
            assert!(v.error.as_ref().unwrap().contains("backend"));
        });
        cleanup(directory);
    }
    #[gpui::test]
    fn explicit_action_old_backend_refuses_then_versioned_positive_control(
        cx: &mut TestAppContext,
    ) {
        let (view, visual, directory) = setup(cx);
        view.update_in(visual, |v, window, cx| {
            let source = v.source_snapshot.clone();
            let before = v.source.value(cx);
            v.preview["document_links_version"] = Value::Null;
            v.open_link_at_caret(window, cx);
            assert!(v.error.as_ref().unwrap().contains("backend"));
            assert!(v.open_link.chooser.is_none());
            assert!(!v.source_navigation_pending());
            assert_eq!(v.source_snapshot, source);
            assert_eq!(v.source.value(cx), before);
            v.preview["document_links_version"] = json!(1);
            v.open_link_at_caret(window, cx);
            assert!(
                v.open_link.chooser.is_some(),
                "current metadata actually reaches navigation choice"
            );
        });
        cleanup(directory);
    }
}
