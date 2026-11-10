//! Opening a rendered Live Preview block from the keyboard, and returning to
//! it (#1642). A rendered table, image, callout or code block shows its source
//! once the caret or selection touches it; besides arrows and a click:
//!
//! - F2 opens the rendered block next to the caret (only blank space between
//!   them) with its whole source selected. With no such block F2 renames the
//!   note, as before.
//! - Esc with the selection inside a block's source draws the block rendered
//!   again: the caret returns to where F2 or Enter found it, else to the line
//!   after the block. The block keeps a focus ring while the caret stays there,
//!   and Enter or F2 opens it again. Any other key or click drops the focus,
//!   and Enter is a new line again.
//! - Click and double-click preserve the exact source offset of the rendered
//!   element; only keyboard entry selects the whole block (vendor patch 0119).
//!
//! The keys read only the remembered focus until they act: Enter, the typing
//! key, does no work that grows with the note.
use super::*;
use gpui_component::input::projection::SourceStamp;
use gpui_component::input::Enter;
use std::ops::Range;

gpui::actions!(okilum, [OpenRenderedBlock]);

/// A rendered block the keyboard returned to: Enter or F2 opens it while the
/// caret stays at `caret` in the same source revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BlockFocus {
    pub stamp: SourceStamp,
    pub block: Range<usize>,
    pub caret: usize,
}

/// A block opened from the keyboard, and where the caret was: Esc returns it
/// there while the source is unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OpenedBlock {
    pub stamp: SourceStamp,
    pub block: Range<usize>,
    pub caret: usize,
}

/// The rendered block next to a collapsed caret at `caret`: one that does
/// not touch it, with only whitespace between them. The nearer one wins; at
/// an equal distance, the one below.
#[cfg(test)]
pub(super) fn block_next_to(
    text: &str,
    blocks: &[Range<usize>],
    caret: usize,
) -> Option<Range<usize>> {
    block_next_to_where(blocks, caret, |range| {
        text.get(range)
            .is_some_and(|gap| gap.chars().all(char::is_whitespace))
    })
}

fn block_next_to_source(
    source: &SourceSnapshot,
    blocks: &[Range<usize>],
    caret: usize,
) -> Option<Range<usize>> {
    block_next_to_where(blocks, caret, |range| {
        source.text.is_char_boundary(range.start)
            && source.text.is_char_boundary(range.end)
            && source
                .text
                .rope()
                .slice(range)
                .chars()
                .all(char::is_whitespace)
    })
}

fn block_next_to_where(
    blocks: &[Range<usize>],
    caret: usize,
    blank: impl Fn(Range<usize>) -> bool,
) -> Option<Range<usize>> {
    // Check whitespace only for the nearest candidate on either side. A farther
    // candidate has the same gap as a prefix/suffix, so cannot rescue a failed
    // check. In particular, a long blank gap is never rescanned per block.
    let below = blocks
        .iter()
        .filter(|block| block.start > caret)
        .min_by_key(|block| block.start)
        .filter(|block| blank(caret..block.start));
    let above = blocks
        .iter()
        .filter(|block| block.end < caret)
        .max_by_key(|block| block.end)
        .filter(|block| blank(block.end..caret));
    match (above, below) {
        (Some(above), Some(below)) if caret - above.end < below.start - caret => Some(above),
        (_, Some(below)) => Some(below),
        (above, None) => above,
    }
    .cloned()
}

/// Where Esc leaves the caret so that `block` is drawn rendered: the start of
/// the line after it, else the end of the line before it. None when the block
/// is the whole note.
pub(super) fn caret_outside(text: &str, block: &Range<usize>) -> Option<usize> {
    if let Some(newline) = text.get(block.end..).and_then(|rest| rest.find('\n')) {
        return Some(block.end + newline + 1);
    }
    let before = text.get(..block.start)?;
    let end = before.strip_suffix('\n')?;
    Some(end.strip_suffix('\r').unwrap_or(end).len())
}

/// Current-source return caret without flattening the note. Only the next
/// line break and the at most two bytes before the block are inspected.
fn caret_outside_source(source: &SourceSnapshot, block: &Range<usize>) -> Option<usize> {
    if !source.text.is_char_boundary(block.start) || !source.text.is_char_boundary(block.end) {
        return None;
    }
    let rope = source.text.rope();
    if let Some(newline) = rope.slice(block.end..).bytes().position(|b| b == b'\n') {
        return Some(block.end + newline + 1);
    }
    let before = block.start.checked_sub(1)?;
    if rope.get_byte(before) != Some(b'\n') {
        return None;
    }
    Some(
        before - usize::from(before.checked_sub(1).and_then(|at| rope.get_byte(at)) == Some(b'\r')),
    )
}

/// The rendered block whose source holds all of `selection`.
pub(super) fn block_holding(
    blocks: &[Range<usize>],
    selection: &Range<usize>,
) -> Option<Range<usize>> {
    blocks
        .iter()
        .find(|block| block.start <= selection.start && selection.end <= block.end)
        .cloned()
}

impl Reader {
    /// The note editor in Live Preview with its rendered block ranges for the
    /// current source, or None outside Live Preview.
    fn live_blocks_now(
        &self,
        cx: &App,
    ) -> Option<(Entity<EditorState>, SourceSnapshot, Vec<Range<usize>>)> {
        let editing = self.editing.as_ref()?;
        if !editing.live_preview.enabled || !editing.markdown {
            return None;
        }
        let input = editing.input.clone();
        let state = input.read(cx);
        // The native input owns an immutable Rope revision, including edits
        // not yet adopted by background classification. Sharing it is O(1).
        let source = state.source_snapshot();
        let blocks = editing.live_preview.rendered_blocks(&source);
        Some((input, source, blocks))
    }

    /// The block focus, when it still holds: the caret has not moved and the
    /// source has not changed since Esc.
    pub(super) fn valid_block_focus(&self, cx: &App) -> Option<BlockFocus> {
        let editing = self.editing.as_ref()?;
        let focus = editing.live_preview.block_focus.clone()?;
        let state = editing.input.read(cx);
        (editing.live_preview.enabled
            && !state.ime_settling(cx)
            && state.source_stamp() == focus.stamp
            && state.selected_range() == (focus.caret..focus.caret))
            .then_some(focus)
    }

    /// Show `block`'s source with all of it selected, caret at its start, and
    /// remember where the caret was for Esc.
    fn open_rendered_block(&mut self, block: Range<usize>, caret: usize, cx: &mut Context<Self>) {
        let Some(editing) = &mut self.editing else {
            return;
        };
        let input = editing.input.clone();
        let stamp = input.read(cx).source_stamp();
        editing.live_preview.block_focus = None;
        editing.live_preview.opened_block = Some(OpenedBlock {
            stamp,
            block: block.clone(),
            caret,
        });
        // A reversed range: the caret, and the view, stay at the block start.
        input.update(cx, |state, cx| {
            state.set_selected_range_preserving_view(block.end..block.start, block.start, cx)
        });
        cx.notify();
    }

    /// F2 in the note editor: open the focused rendered block, else the one
    /// next to the caret; otherwise the key renames the note as before.
    fn open_block_from_keyboard(&mut self, cx: &mut Context<Self>) {
        if !self.ime_allows(super::ime_guard::Trigger::Enter, cx) {
            return;
        }
        if let Some(focus) = self.valid_block_focus(cx) {
            self.open_rendered_block(focus.block, focus.caret, cx);
            return;
        }
        // Reject selections before snapshot or block-list work. F2 then
        // retains its ordinary Rename note fallback.
        let selection = self
            .editing
            .as_ref()
            .map(|e| e.input.read(cx).selected_range());
        let target = selection.filter(Range::is_empty).and_then(|selection| {
            self.live_blocks_now(cx).and_then(|(_, source, blocks)| {
                block_next_to_source(&source, &blocks, selection.start)
                    .map(|block| (block, selection.start))
            })
        });
        match target {
            Some((block, caret)) => self.open_rendered_block(block, caret, cx),
            None => cx.propagate(),
        }
    }

    /// Enter on a block Esc just returned to opens it again; any other Enter
    /// is the editor's. Reads only the remembered focus.
    pub(super) fn enter_focused_block(&mut self, action: &Enter, cx: &mut Context<Self>) {
        if action.secondary || action.shift {
            return;
        }
        if let Some(focus) = self.valid_block_focus(cx) {
            self.open_rendered_block(focus.block, focus.caret, cx);
            cx.stop_propagation();
        }
    }

    /// Esc with the selection inside a rendered block's source: draw the block
    /// rendered again and focus it. False when there is no such block, so Esc
    /// does what it did before.
    pub(crate) fn close_rendered_block(&mut self, cx: &mut Context<Self>) -> bool {
        // Esc belongs to the input method until its commit has settled. Consume
        // it here rather than falling through to closing the note editor.
        if self
            .editing
            .as_ref()
            .is_some_and(|e| e.input.read(cx).ime_settling(cx))
        {
            return true;
        }
        // F2/Enter already proved this block and remembered the return caret.
        // In the same immutable revision, Esc needs neither a source snapshot
        // nor another block lookup. A changed source always takes the checked
        // current-source path below.
        let remembered = self.editing.as_ref().and_then(|editing| {
            if !editing.live_preview.enabled || !editing.markdown {
                return None;
            }
            let opened = editing.live_preview.opened_block.as_ref()?;
            let state = editing.input.read(cx);
            let selection = state.selected_range();
            (opened.stamp == state.source_stamp()
                && opened.block.start <= selection.start
                && selection.end <= opened.block.end
                && (opened.caret < opened.block.start || opened.caret > opened.block.end))
                .then(|| (editing.input.clone(), opened.clone()))
        });
        if let Some((input, opened)) = remembered {
            let editing = self.editing.as_mut().unwrap();
            let anchor = opened.block.start;
            editing.live_preview.opened_block = None;
            editing.live_preview.block_focus = Some(BlockFocus {
                stamp: opened.stamp,
                block: opened.block,
                caret: opened.caret,
            });
            input.update(cx, |state, cx| {
                state.set_selected_range_preserving_view(opened.caret..opened.caret, anchor, cx)
            });
            cx.notify();
            return true;
        }
        let Some((input, source, blocks)) = self.live_blocks_now(cx) else {
            return false;
        };
        let (stamp, selection) = {
            let state = input.read(cx);
            (state.source_stamp(), state.selected_range())
        };
        let Some(block) = block_holding(&blocks, &selection) else {
            return false;
        };
        let Some(editing) = &mut self.editing else {
            return false;
        };
        let opened = editing.live_preview.opened_block.take();
        let caret = opened
            .filter(|opened| opened.stamp == stamp && opened.block == block)
            .map(|opened| opened.caret)
            .filter(|&caret| caret < block.start || caret > block.end)
            .or_else(|| caret_outside_source(&source, &block));
        let Some(caret) = caret else {
            return false;
        };
        let anchor = block.start;
        editing.live_preview.block_focus = Some(BlockFocus {
            stamp,
            block,
            caret,
        });
        input.update(cx, |state, cx| {
            state.set_selected_range_preserving_view(caret..caret, anchor, cx)
        });
        cx.notify();
        true
    }

    /// The block keys on the note editor's column: Enter is captured before
    /// the code, list and input keys, and falls through unless a block is
    /// focused.
    pub(super) fn block_entry_actions(&self, column: Div, cx: &mut Context<Self>) -> Div {
        column
            .capture_action(
                cx.listener(|this, action: &Enter, _, cx| this.enter_focused_block(action, cx)),
            )
            .on_action(
                cx.listener(|this, _: &OpenRenderedBlock, _, cx| this.open_block_from_keyboard(cx)),
            )
    }
}

/// Source bytes copied by explicit block commands, including native value().
/// Typing probes count projection snapshots separately from durable recovery.
#[cfg(test)]
pub(super) fn take_source_work_bytes() -> usize {
    gpui_component::input::projection::take_flattened_bytes()
        + gpui_component::input::projection::take_materialized_value_bytes()
        + super::take_source_status_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn the_block_next_to_the_caret_is_across_blank_space_only() {
        let text = "para\n\n| a |\n|---|\n\nmiddle\n\n![[x.png]]\n\nend";
        let table = text.find('|').unwrap()..text.find("|---|").unwrap() + 5;
        let image = text.find("![[").unwrap()..text.find("]]").unwrap() + 2;
        let blocks = [table.clone(), image.clone()];
        let at = |needle: &str| text.find(needle).unwrap();
        // End of the paragraph above, the blank line, the line after.
        assert_eq!(block_next_to(text, &blocks, 4), Some(table.clone()));
        assert_eq!(block_next_to(text, &blocks, 5), Some(table.clone()));
        assert_eq!(
            block_next_to(text, &blocks, table.end + 1),
            Some(table.clone())
        );
        // Text between the caret and a block: not next to it.
        assert_eq!(block_next_to(text, &blocks, 2), None);
        assert_eq!(block_next_to(text, &blocks, at("middle") + 3), None);
        // At the start of `middle` the table is above across a blank line.
        assert_eq!(
            block_next_to(text, &blocks, at("middle")),
            Some(table.clone())
        );
        assert_eq!(
            block_next_to(text, &blocks, at("middle") + 6),
            Some(image.clone())
        );
        assert_eq!(block_next_to(text, &blocks, at("end")), Some(image.clone()));
        // A caret on a block's edge already shows its source.
        assert_eq!(block_next_to(text, &blocks, table.start), None);
        assert_eq!(block_next_to(text, &blocks, image.end), None);
        // Equal distance: the block below.
        let both = "| a |\n\n![[x.png]]";
        let blocks = [0..5, 7..both.len()];
        assert_eq!(block_next_to(both, &blocks, 6), Some(7..both.len()));
    }

    #[test]
    fn adjacency_keeps_unicode_boundaries_ties_and_unsorted_ranges() {
        let text = "α\r\n\u{2003}\n| a |\n\n![[x.png]]\n";
        let table = text.find('|').unwrap()..text.find('|').unwrap() + 5;
        let image = text.find('!').unwrap()..text.len() - 1;
        let blocks = [image.clone(), table.clone()];
        assert_eq!(block_next_to(text, &blocks, "α".len()), Some(table.clone()));
        assert_eq!(block_next_to(text, &blocks, 1), None);
        assert_eq!(block_next_to(text, &blocks, table.end + 1), Some(image));
        assert_eq!(block_next_to(text, &blocks, usize::MAX), None);
    }

    /// Real Reader/provider/input/layout path, with synthetic files only. This
    /// measures the admitted projected size, never labels raw fallback as LP.
    #[gpui::test]
    fn projected_block_entry_checks_current_source_and_preserves_undo(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let mut note = "Intro\n\n| Name | Count |\n|---|---|\n| pear | 12 |\n\n".to_owned();
        while note.len() < 60 * 1024 {
            note.push_str("**Bold** and ordinary prose with a Unicode α character for exact source coordinates.\n\n");
        }
        std::fs::write(root.join("note.md"), &note).unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("note.md")),
                        index_dir: Some(directory.path().join("index")),
                        session_directory: Some(directory.path().join("state")),
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
        let draw = |visual: &mut VisualTestContext| {
            for _ in 0..3 {
                visual.run_until_parked();
                visual.update(|window, cx| window.draw(cx).clear(cx));
            }
            visual.run_until_parked();
        };
        draw(visual);
        reader.update_in(visual, |r, window, cx| {
            r.toggle_source(window, cx);
            if !r.editing.as_ref().unwrap().live_preview.enabled {
                r.toggle_live_preview(window, cx);
            }
        });
        draw(visual);
        let input = reader.read_with(visual, |r, _| r.editing.as_ref().unwrap().input.clone());
        input.update_in(visual, |state, window, cx| {
            state.enable_layout_diagnostics();
            state.focus(window, cx);
            state.set_selected_range(5..5, cx);
        });
        draw(visual);
        let old = reader.read_with(visual, |r, cx| {
            let (_, source, blocks) = r.live_blocks_now(cx).unwrap();
            assert!(!blocks.is_empty(), "positive control: rendered table");
            assert_eq!(source.text.as_ref(), note);
            source
        });
        let table = note.find('|').unwrap()..note.find("| 12 |").unwrap() + 6;
        take_source_work_bytes();
        reader.update_in(visual, |r, _, cx| r.open_block_from_keyboard(cx));
        assert_eq!(
            take_source_work_bytes(),
            0,
            "first F2 shares the native immutable Rope"
        );
        draw(visual);
        input.read_with(visual, |state, _| {
            assert_eq!(state.selected_range(), table);
            assert_eq!(state.value().as_ref(), note);
        });
        reader.update_in(visual, |r, _, cx| {
            take_source_work_bytes();
            assert!(r.close_rendered_block(cx));
            assert_eq!(
                take_source_work_bytes(),
                0,
                "unchanged Esc returns without flattening source"
            );
        });
        draw(visual);
        // A real edit before classification adopts it must never read old bytes.
        reader.update_in(visual, |r, window, cx| {
            input.update(cx, |state, cx| {
                state.replace_text_in_range(Some(0..0), "x", window, cx);
            });
            take_source_work_bytes();
            let (_, current, _) = r.live_blocks_now(cx).unwrap();
            assert_eq!(
                take_source_work_bytes(),
                0,
                "changed revisions also share current Rope bytes"
            );
            assert_ne!(current.stamp, old.stamp);
            assert_eq!(current.text.rope().to_string(), format!("x{note}"));
            assert_eq!(old.text.rope().to_string(), note);
            let _old_input = gpui_component::input::projection::SourceText::from(Arc::<str>::from(
                current.text.rope().to_string(),
            ));
            assert!(
                take_source_work_bytes() >= note.len(),
                "positive control: old contiguous snapshot construction is observed"
            );
        });
        draw(visual);
        take_source_work_bytes();
        input.read_with(visual, |state, _| {
            let _legacy_value = state.value();
        });
        assert!(
            take_source_work_bytes() >= note.len(),
            "positive control: native value() materialization is also observed"
        );
        input.read_with(visual, |state, _| {
            let counts = state.layout_diagnostics().unwrap();
            assert!(counts.projection_composes > 0, "actual projected work");
            let frames: Vec<_> = state
                .take_layout_frames()
                .into_iter()
                .filter(|f| f.edit)
                .collect();
            assert!(!frames.is_empty(), "actual edit frames positive control");
            assert!(
                frames.iter().all(|f| !f.raw),
                "raw fallback is not evidence"
            );
            for frame in frames {
                diagnostic_error!(
                    "OK38 bytes={} raw={} projection_composes={} prepaint_ms={} reshape_ms={}",
                    note.len(),
                    frame.raw,
                    counts.projection_composes,
                    frame.prepaint_ms,
                    frame.reshape_ms
                );
            }
        });
        #[cfg(target_os = "macos")]
        visual.simulate_keystrokes("cmd-z");
        #[cfg(not(target_os = "macos"))]
        visual.simulate_keystrokes("ctrl-z");
        draw(visual);
        input.read_with(visual, |state, _| assert_eq!(state.value().as_ref(), note));
        assert_eq!(std::fs::read_to_string(root.join("note.md")).unwrap(), note);
    }

    #[test]
    fn esc_leaves_the_caret_just_outside_the_block() {
        let text = "a\r\n| a |\r\n|---|\r\nb";
        let block = 3..text.find("|---|").unwrap() + 5;
        assert_eq!(caret_outside(text, &block), Some(text.len() - 1));
        // The block ends the note: the end of the line before it.
        let last = "top\r\n![[x.png]]";
        assert_eq!(caret_outside(last, &(5..last.len())), Some(3));
        assert_eq!(caret_outside("![[x.png]]", &(0..10)), None);
        let blocks = [3..9, 12..20];
        assert_eq!(block_holding(&blocks, &(3..9)), Some(3..9));
        assert_eq!(block_holding(&blocks, &(9..9)), Some(3..9));
        assert_eq!(block_holding(&blocks, &(8..12)), None);
        assert_eq!(block_holding(&blocks, &(10..10)), None);
    }
}
