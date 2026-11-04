//! Outline clicks target the visible document surface, including Source/Live.
use super::*;
use gpui_component::input::EditorState;
use okilum_core::document_links::HeadingEntry;

mod draft;
mod incremental;
pub(super) use draft::DraftOutline;

const STALE_HEADING: &str = "The outline is updating. Click the heading again when it refreshes.";

/// Held outline publication: source text plus its release and resume signals.
#[cfg(test)]
type OutlinePublicationHold = (
    String,
    futures::channel::oneshot::Sender<()>,
    futures::channel::oneshot::Receiver<()>,
);

#[cfg(test)]
thread_local! {
    static OUTLINE_PUBLICATION_HOLD: std::cell::RefCell<Option<OutlinePublicationHold>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
async fn hold_outline_publication(source: &str) {
    let hold = OUTLINE_PUBLICATION_HOLD.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot
            .as_ref()
            .is_some_and(|(expected, _, _)| expected == source)
        {
            slot.take()
        } else {
            None
        }
    });
    if let Some((_, reached, release)) = hold {
        let _ = reached.send(());
        let _ = release.await;
    }
}

impl Reader {
    pub(super) fn visible_outline(&self) -> &[HeadingEntry] {
        if self.editing.is_some() {
            &self.draft_outline.entries
        } else {
            &self.outline
        }
    }

    pub(super) fn refresh_draft_outline(&mut self, cx: &mut Context<Self>) {
        self.draft_outline.pending = true;
        // One parser in flight, with no queue of intermediate drafts. Every
        // completion either publishes its exact revision or reads the latest.
        if self.draft_outline.worker.is_some() {
            return;
        }
        let mut cache = self.draft_outline.cache.take().unwrap_or_default();
        self.draft_outline.worker = Some(cx.spawn(async move |this, cx| loop {
            let Ok(Some((input, stamp, source))) = this.update(cx, |this, cx| {
                #[cfg(test)]
                let _work = crate::reader_editor::work::scope();
                let Some(editing) = &this.editing else {
                    this.draft_outline.worker = None;
                    return None;
                };
                let input = editing.input().clone();
                let state = input.read(cx);
                Some((
                    input.clone(),
                    state.source_stamp(),
                    state.source_snapshot().text,
                ))
            }) else {
                return;
            };
            let (snapshot, next_cache) = cx
                .background_executor()
                .spawn(async move {
                    let snapshot = DraftOutline::with_cache(&source, &mut cache);
                    (snapshot, cache)
                })
                .await;
            #[cfg(test)]
            hold_outline_publication(&snapshot.source).await;
            cache = next_cache;
            let Ok(done) = this.update(cx, |this, cx| {
                if this
                    .editing
                    .as_ref()
                    .is_none_or(|editing| editing.input() != &input)
                    || input.read(cx).source_stamp() != stamp
                {
                    return false;
                }
                let labels_changed = this.draft_outline.signature != snapshot.signature;
                this.draft_outline.signature = snapshot.signature;
                this.draft_outline.cache = Some(std::mem::take(&mut cache));
                let retired = (
                    std::mem::replace(&mut this.draft_outline.source, snapshot.source),
                    std::mem::replace(&mut this.draft_outline.entries, snapshot.entries),
                    std::mem::replace(&mut this.draft_outline.lines, snapshot.lines),
                );
                cx.background_executor()
                    .spawn(async move {
                        drop(retired);
                    })
                    .detach();
                this.draft_outline.pending = false;
                this.draft_outline.worker = None;
                if this.outline_list.item_count() != this.draft_outline.entries.len() {
                    this.outline_list.reset_with_uniform_height(
                        this.draft_outline.entries.len(),
                        px(OUTLINE_ROW_HEIGHT),
                    );
                }
                if labels_changed {
                    cx.notify();
                }
                true
            }) else {
                return;
            };
            if done {
                return;
            }
        }));
    }

    pub(super) fn open_outline_heading(
        &mut self,
        heading: HeadingEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editing) = &self.editing else {
            self.scroll_to_block(heading.target.block, cx);
            self.content
                .read(cx)
                .focus_handle()
                .clone()
                .focus(window, cx);
            return;
        };
        let input = editing.input().clone();
        let current = input.read(cx).value();
        let line = self.draft_outline.line_for(&current, &heading);
        let untouched = (
            input.read(cx).source_stamp(),
            input.read(cx).selected_range(),
        );
        let root = self.vault_root.clone();
        let rel = self.current_rel.clone();
        self.cancel_pending_landing();
        let request = self.navigation.landing_generation;
        input.read(cx).focus_handle(cx).focus(window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = line;
            let _ = this.update_in_own_window(cx, |this, window, cx| {
                if this.navigation.landing_generation != request
                    || this.vault_root != root
                    || this.current_rel != rel
                    || this
                        .editing
                        .as_ref()
                        .is_none_or(|editing| editing.input() != &input)
                    || (
                        input.read(cx).source_stamp(),
                        input.read(cx).selected_range(),
                    ) != untouched
                {
                    return;
                }
                let line = match result {
                    Ok(line) => line,
                    Err(reason) => {
                        this.link_notice = Some(reason.into());
                        cx.notify();
                        return;
                    }
                };
                this.link_notice = None;
                let after = input.update(cx, |input, cx| {
                    place(input, line, window, cx);
                    (input.source_stamp(), input.selected_range())
                });
                // A caret can scroll only after layout. A subsequent click,
                // note/mode change, typed edit or caret move cancels this handoff.
                let reader = cx.weak_entity();
                window.on_next_frame(move |window, cx| {
                    let _ = reader.update(cx, |reader, cx| {
                        if reader.navigation.landing_generation != request
                            || reader.vault_root != root
                            || reader.current_rel != rel
                            || reader
                                .editing
                                .as_ref()
                                .is_none_or(|editing| editing.input() != &input)
                            || (
                                input.read(cx).source_stamp(),
                                input.read(cx).selected_range(),
                            ) != after
                        {
                            return;
                        }
                        input.update(cx, |input, cx| place(input, line, window, cx));
                    });
                });
                cx.notify();
            });
        })
        .detach();
    }
}

fn place(input: &mut EditorState, line: u32, window: &mut Window, cx: &mut Context<EditorState>) {
    input.set_cursor_position(
        gpui_component::input::Position { line, character: 0 },
        window,
        cx,
    );
    input.scroll_line_to_top(line as usize, cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn draft_outline_tracks_added_renamed_removed_and_duplicate_headings() {
        let old = "---\ntitle: Note\n---\n# Old\n\nBody\n";
        let before = DraftOutline::new(old);
        assert_eq!(before.entries.len(), 1, "inventory positive control");
        let draft = old.replace("# Old", "## QA heading\n\n# Renamed\n\n# Same\n\n# Same");
        let after = DraftOutline::new(&draft);
        assert_eq!(
            after
                .entries
                .iter()
                .map(|h| h.text.as_str())
                .collect::<Vec<_>>(),
            ["QA heading", "Renamed", "Same", "Same"]
        );
        assert_eq!(after.current(0), None);
        assert_eq!(after.current(3), Some(0));
        assert_eq!(after.current(9), Some(3));
        for (index, heading) in after.entries.iter().enumerate() {
            assert_eq!(
                after.line_for(&draft, heading),
                Ok(after.lines[index] as u32)
            );
        }
        let removed = DraftOutline::new("Body only\n");
        assert!(removed.entries.is_empty());
        assert_eq!(removed.current(0), None);
    }

    fn edit_and_save_refresh_outline(cx: &mut TestAppContext, live: bool) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let path = root.join("note.md");
        let original = (0..20)
            .map(|index| format!("## Heading {index}\n\nBody\n\n"))
            .collect::<String>();
        std::fs::write(&path, &original).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("note.md".into()),
                        session_directory: Some(fixture.path().join("state")),
                        index_dir: Some(fixture.path().join("index")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.simulate_resize(size(px(1400.), px(960.)));
        settle(visual);
        reader.update_in(visual, |reader, window, cx| {
            assert_eq!(reader.outline.len(), 20, "loaded note positive control");
            reader.panels.open(reader_layout::Panel::Backlinks);
            cx.notify();
            if live {
                reader.open_live_preview(window, cx);
            } else {
                reader.toggle_source(window, cx);
            }
        });
        settle(visual);
        let draft = format!("## QA heading\n\n{original}");
        reader.update_in(visual, |reader, window, cx| {
            reader
                .editing
                .as_ref()
                .unwrap()
                .set_value(&draft, window, cx);
        });
        settle(visual);
        reader.update_in(visual, |reader, _, cx| {
            assert_eq!(reader.visible_outline().len(), 21);
            assert_eq!(reader.visible_outline()[0].text, "QA heading");
            assert_eq!(reader.outline_list.item_count(), 21);
            assert!(reader.save_source(cx), "explicit save succeeds");
            assert!(reader.editing.is_some(), "Source remains open");
            assert_eq!(reader.visible_outline()[0].text, "QA heading");
        });
        assert_eq!(std::fs::read_to_string(&path).unwrap(), draft);
        settle(visual);
        let row = visual
            .debug_bounds("reader-outline-row-0")
            .expect("new heading is drawn");
        visual.simulate_click(row.center(), Modifiers::default());
        settle(visual);
        reader.read_with(visual, |reader, cx| {
            assert!(reader.link_notice.is_none());
            let input = reader.editing.as_ref().unwrap().input().read(cx);
            assert_eq!(input.cursor_position().line, 0);
            assert_eq!(reader.current_section(cx), Some(0));
        });
    }

    #[gpui::test]
    fn source_edit_and_save_refresh_outline_without_leaving_source(cx: &mut TestAppContext) {
        edit_and_save_refresh_outline(cx, false);
    }

    #[gpui::test]
    fn live_edit_and_save_refresh_outline_without_leaving_live(cx: &mut TestAppContext) {
        edit_and_save_refresh_outline(cx, true);
    }

    struct MountedDraft {
        reader: Entity<Reader>,
        input: Entity<EditorState>,
        file: std::path::PathBuf,
        original: String,
        _directory: tempfile::TempDir,
    }

    fn mount_draft(cx: &mut TestAppContext, live: bool) -> (MountedDraft, &mut VisualTestContext) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let original = format!(
            "# Existing\r\n\r\n{}## Tail 🎯\r\n\r\n{}",
            "Body Ω paragraph.\r\n\r\n".repeat(80),
            "After target Ω paragraph.\r\n\r\n".repeat(80)
        );
        let file = root.join("note.md");
        std::fs::write(&file, &original).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root),
                        note: Some("note.md".into()),
                        session_directory: Some(directory.path().join("state")),
                        index_dir: Some(directory.path().join("index")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.simulate_resize(size(px(1400.), px(760.)));
        settle(visual);
        reader.update_in(visual, |reader, window, cx| {
            reader.panels.open(reader_layout::Panel::Backlinks);
            if live {
                reader.open_live_preview(window, cx);
            } else {
                reader.toggle_source(window, cx);
            }
            cx.notify();
        });
        settle(visual);
        let input = reader.update_in(visual, |reader, window, cx| {
            assert_eq!(
                reader.live_preview_shown(),
                live,
                "requested surface mounted"
            );
            let input = reader.editing.as_ref().unwrap().input().clone();
            input.update(cx, |input, cx| input.set_selected_range(0..0, cx));
            input.read(cx).focus_handle(cx).focus(window, cx);
            input
        });
        (
            MountedDraft {
                reader,
                input,
                file,
                original,
                _directory: directory,
            },
            visual,
        )
    }

    fn assert_draft(
        fixture: &MountedDraft,
        visual: &mut VisualTestContext,
        source: &str,
        headings: &[&str],
    ) {
        fixture.reader.read_with(visual, |reader, cx| {
            assert_eq!(fixture.input.read(cx).value().as_ref(), source);
            assert_eq!(
                reader.draft_outline.source, source,
                "exact accepted input snapshot"
            );
            assert!(!reader.draft_outline.pending);
            assert!(reader.draft_outline.worker.is_none(), "worker completed");
            assert_eq!(
                reader
                    .visible_outline()
                    .iter()
                    .map(|h| h.text.as_str())
                    .collect::<Vec<_>>(),
                headings
            );
            assert_eq!(reader.outline_list.item_count(), headings.len());
            assert_eq!(
                reader.outline.len(),
                2,
                "saved Reader inventory remains distinct"
            );
        });
        assert_eq!(
            std::fs::read_to_string(&fixture.file).unwrap(),
            fixture.original,
            "outline refresh does not require a save or an autosave clock advance"
        );
    }

    fn click_draft_tail(fixture: &MountedDraft, visual: &mut VisualTestContext) {
        let (index, line) = fixture.reader.read_with(visual, |reader, _| {
            let index = reader.visible_outline().len() - 1;
            assert_eq!(reader.visible_outline()[index].text, "Tail 🎯");
            (index, reader.draft_outline.lines[index] as u32)
        });
        let selector = match index {
            1 => "reader-outline-row-1",
            2 => "reader-outline-row-2",
            3 => "reader-outline-row-3",
            _ => panic!("unexpected fixture heading index {index}"),
        };
        let row = visual
            .debug_bounds(selector)
            .expect("current draft heading row is painted");
        visual.simulate_click(row.center(), Modifiers::default());
        settle(visual);
        fixture.reader.read_with(visual, |reader, cx| {
            let input = fixture.input.read(cx);
            assert_eq!(input.cursor_position().line, line);
            assert_eq!(input.top_visible_line(), Some(line as usize));
            assert!(
                input.scroll_offset().y < px(-100.),
                "visible editor actually scrolled"
            );
            assert_eq!(reader.current_section(cx), Some(index));
            assert!(reader.link_notice.is_none());
            assert_eq!(
                reader
                    .content
                    .read(cx)
                    .list_state()
                    .logical_scroll_top()
                    .item_ix,
                0,
                "hidden saved Reader is not the click destination"
            );
        });
    }

    fn draft_edit_click_undo_redo(cx: &mut TestAppContext, live: bool) {
        let (fixture, visual) = mount_draft(cx, live);
        assert_draft(
            &fixture,
            visual,
            &fixture.original,
            &["Existing", "Tail 🎯"],
        );
        let edited = format!("## Draft Ω\r\n\r\n{}", fixture.original);
        // One text-input transaction: simulate_input splits its string into
        // individual keystrokes, whose undo grouping is a separate contract.
        fixture.input.update_in(visual, |input, window, cx| {
            input.replace_text_in_range(Some(0..0), "## Draft Ω\r\n\r\n", window, cx);
        });
        settle(visual);
        assert_draft(
            &fixture,
            visual,
            &edited,
            &["Draft Ω", "Existing", "Tail 🎯"],
        );
        click_draft_tail(&fixture, visual);
        visual.dispatch_action(gpui_component::input::Undo);
        settle(visual);
        assert_draft(
            &fixture,
            visual,
            &fixture.original,
            &["Existing", "Tail 🎯"],
        );
        click_draft_tail(&fixture, visual);
        visual.dispatch_action(gpui_component::input::Redo);
        settle(visual);
        assert_draft(
            &fixture,
            visual,
            &edited,
            &["Draft Ω", "Existing", "Tail 🎯"],
        );
        click_draft_tail(&fixture, visual);
    }

    #[gpui::test]
    fn draft_1320_source_refresh_click_undo_redo(cx: &mut TestAppContext) {
        draft_edit_click_undo_redo(cx, false);
    }

    #[gpui::test]
    fn draft_1320_live_refresh_click_undo_redo(cx: &mut TestAppContext) {
        draft_edit_click_undo_redo(cx, true);
    }

    fn hold_publication(
        source: &str,
    ) -> (
        futures::channel::oneshot::Receiver<()>,
        futures::channel::oneshot::Sender<()>,
    ) {
        let (reached, waiting) = futures::channel::oneshot::channel();
        let (release, held) = futures::channel::oneshot::channel();
        OUTLINE_PUBLICATION_HOLD.with(|slot| {
            assert!(slot.borrow().is_none(), "independent publication hold");
            *slot.borrow_mut() = Some((source.to_owned(), reached, held));
        });
        (waiting, release)
    }

    fn draft_coalescing_and_stale_rows(cx: &mut TestAppContext, live: bool) {
        let (fixture, visual) = mount_draft(cx, live);
        let intermediate = format!("## Intermediate\r\n\r\n{}", fixture.original);
        let (mut first_reached, first_release) = hold_publication(&intermediate);
        visual.simulate_input("## Intermediate\r\n\r\n");
        settle(visual);
        assert_eq!(
            first_reached.try_recv().unwrap(),
            Some(()),
            "positive control: real parser completed and publication is held"
        );
        let selected = fixture
            .input
            .read_with(visual, |input, _| input.selected_range());
        let stale_row = visual
            .debug_bounds("reader-outline-row-1")
            .expect("previous snapshot row remains painted while refreshing");
        visual.simulate_click(stale_row.center(), Modifiers::default());
        settle(visual);
        fixture.reader.read_with(visual, |reader, cx| {
            assert_eq!(reader.link_notice.as_deref(), Some(STALE_HEADING));
            assert_eq!(
                fixture.input.read(cx).selected_range(),
                selected,
                "stale painted row cannot move the caret"
            );
        });
        fixture
            .input
            .update(visual, |input, cx| input.set_selected_range(0..0, cx));
        visual.simulate_input("# Latest\r\n\r\n");
        let latest = format!("# Latest\r\n\r\n{intermediate}");
        let (mut latest_reached, latest_release) = hold_publication(&latest);
        first_release.send(()).unwrap();
        settle(visual);
        assert_eq!(
            latest_reached.try_recv().unwrap(),
            Some(()),
            "coalesced worker parsed the latest input after rejecting the held result"
        );
        fixture.reader.read_with(visual, |reader, _| {
            assert_eq!(
                reader.draft_outline.source, fixture.original,
                "obsolete parser result was never installed before the latest publication"
            );
            assert!(reader.draft_outline.pending);
            assert!(reader.draft_outline.worker.is_some());
            assert_eq!(reader.visible_outline().len(), 2);
        });
        latest_release.send(()).unwrap();
        settle(visual);
        assert_draft(
            &fixture,
            visual,
            &latest,
            &["Latest", "Intermediate", "Existing", "Tail 🎯"],
        );
        // Queue a valid landing, then move the caret before that task runs.
        // The deferred handoff must not reclaim the user's newer selection.
        fixture.reader.update_in(visual, |reader, window, cx| {
            let heading = reader.visible_outline().last().unwrap().clone();
            assert_ne!(
                fixture.input.read(cx).selected_range(),
                0..0,
                "positive control: the user's new caret differs from the captured selection"
            );
            reader.open_outline_heading(heading, window, cx);
            fixture
                .input
                .update(cx, |input, cx| input.set_selected_range(0..0, cx));
        });
        settle(visual);
        fixture
            .input
            .read_with(visual, |input, _| assert_eq!(input.selected_range(), 0..0));
        click_draft_tail(&fixture, visual);
    }

    #[gpui::test]
    fn draft_1320_source_coalesces_and_rejects_stale_rows(cx: &mut TestAppContext) {
        draft_coalescing_and_stale_rows(cx, false);
    }

    #[gpui::test]
    fn draft_1320_live_coalesces_and_rejects_stale_rows(cx: &mut TestAppContext) {
        draft_coalescing_and_stale_rows(cx, true);
    }

    #[derive(Clone, Copy, Debug)]
    enum Mode {
        Read,
        Source,
        Live,
    }

    fn settle(visual: &mut gpui::VisualTestContext) {
        visual.run_until_parked();
        // Deliver the finite layout handoff just as a native display does.
        for _ in 0..2 {
            visual.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
            visual.run_until_parked();
        }
    }

    fn click_heading(cx: &mut TestAppContext, mode: Mode) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let original = format!(
            "---\r\ntitle: Synthetic Ω\r\n---\r\n# Top\r\n\r\n{}Цель 🎯\r\n-------\r\n\r\n{}## End\r\n\r\n{}",
            "A paragraph before the target.\r\n\r\n".repeat(80),
            "A paragraph after the target.\r\n\r\n".repeat(80),
            "Ending paragraph.\r\n\r\n".repeat(30)
        );
        let path = root.join("note.md");
        std::fs::write(&path, &original).unwrap();
        let target_offset = original.find("Цель 🎯").unwrap();
        let target_line = original[..target_offset]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count() as u32;
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("note.md".into()),
                        session_directory: Some(fixture.path().join("state")),
                        index_dir: Some(fixture.path().join("index")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.simulate_resize(size(px(1400.), px(960.)));
        settle(visual);
        let target_block = reader.update_in(visual, |reader, window, cx| {
            assert_eq!(
                reader.outline.len(),
                3,
                "outline inventory positive control"
            );
            assert_eq!(reader.outline[1].text, "Цель 🎯");
            reader.panels.open(reader_layout::Panel::Backlinks);
            match mode {
                Mode::Read => {}
                Mode::Source => reader.toggle_source(window, cx),
                Mode::Live => reader.open_live_preview(window, cx),
            }
            cx.notify();
            reader.outline[1].target.block
        });
        settle(visual);
        let input = reader.read_with(visual, |reader, _| {
            reader
                .editing
                .as_ref()
                .map(|editing| editing.input().clone())
        });
        reader.read_with(visual, |reader, _| {
            assert_eq!(
                input.is_some(),
                !matches!(mode, Mode::Read),
                "positive control: requested editing mode opened"
            );
            assert_eq!(
                reader.live_preview_shown(),
                matches!(mode, Mode::Live),
                "positive control: actual Live Preview state"
            );
        });
        if let Some(input) = &input {
            input.update_in(visual, |input, window, cx| place(input, 0, window, cx));
            settle(visual);
            input.read_with(visual, |input, _| {
                assert_eq!(input.cursor_position().line, 0);
                assert!(
                    input.scroll_offset().y >= px(-1.),
                    "positive control: editor starts at the top"
                );
            });
        }
        let row = visual
            .debug_bounds("reader-outline-row-1")
            .expect("real outline row is drawn");
        visual.simulate_click(row.center(), Modifiers::default());
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(300));
        settle(visual);
        reader.update_in(visual, |reader, window, cx| {
            assert_eq!(reader.current_rel, "note.md");
            assert!(
                reader.link_notice.is_none(),
                "{mode:?}: {:?}",
                reader.link_notice
            );
            if let Some(input) = &input {
                let input = input.read(cx);
                assert_eq!(
                    input.cursor_position().line,
                    target_line,
                    "{mode:?}: authored CRLF/frontmatter heading line"
                );
                assert_eq!(input.cursor_position().character, 0);
                assert!(
                    input.scroll_offset().y < px(-100.),
                    "{mode:?}: visible editor must actually scroll"
                );
                assert_eq!(
                    input.top_visible_line(),
                    Some(target_line as usize),
                    "{mode:?}: heading reaches the top of the visible editor"
                );
                assert!(input.focus_handle(cx).is_focused(window));
                assert_eq!(input.value().as_ref(), original);
                assert_eq!(
                    reader
                        .content
                        .read(cx)
                        .list_state()
                        .logical_scroll_top()
                        .item_ix,
                    0,
                    "{mode:?}: the hidden Reader is not the navigation target"
                );
            } else {
                assert_eq!(
                    reader
                        .content
                        .read(cx)
                        .list_state()
                        .logical_scroll_top()
                        .item_ix,
                    target_block,
                    "Read retains its block landing"
                );
                assert!(reader.editing.is_none());
            }
        });
        if let Some(input) = &input {
            // Both jobs begin only after this update returns. The older result
            // must not win even if background completion order changes.
            reader.update_in(visual, |reader, window, cx| {
                reader.open_outline_heading(reader.visible_outline()[0].clone(), window, cx);
                reader.open_outline_heading(reader.visible_outline()[2].clone(), window, cx);
            });
            settle(visual);
            let end_line = original[..original.find("## End").unwrap()]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count();
            input.read_with(visual, |input, _| {
                assert_eq!(input.cursor_position().line as usize, end_line);
                assert_eq!(input.top_visible_line(), Some(end_line));
            });
            // A user caret move while parsing is pending cancels the landing.
            reader.update_in(visual, |reader, window, cx| {
                reader.open_outline_heading(reader.visible_outline()[1].clone(), window, cx);
                input.update(cx, |input, cx| place(input, 0, window, cx));
            });
            settle(visual);
            input.read_with(visual, |input, _| {
                assert_eq!(input.cursor_position().line, 0);
                assert_eq!(input.top_visible_line(), Some(0));
                assert_eq!(input.value().as_ref(), original);
            });
        }
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            original,
            "navigation does not edit the note"
        );
    }

    #[gpui::test]
    fn outline_pointer_click_scrolls_source_and_places_caret(cx: &mut TestAppContext) {
        click_heading(cx, Mode::Source);
    }
    #[gpui::test]
    fn outline_pointer_click_scrolls_live_preview_and_places_caret(cx: &mut TestAppContext) {
        click_heading(cx, Mode::Live);
    }
    #[gpui::test]
    fn outline_pointer_click_preserves_read_navigation(cx: &mut TestAppContext) {
        click_heading(cx, Mode::Read);
    }
}
