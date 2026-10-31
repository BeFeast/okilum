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
//! - A double-click on a rendered block selects its whole source (vendor patch
//!   0105).
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
pub(super) fn block_next_to(
    text: &str,
    blocks: &[Range<usize>],
    caret: usize,
) -> Option<Range<usize>> {
    let blank = |range: Range<usize>| {
        text.get(range)
            .is_some_and(|gap| gap.chars().all(char::is_whitespace))
    };
    let below = blocks
        .iter()
        .filter(|block| block.start > caret && blank(caret..block.start))
        .min_by_key(|block| block.start);
    let above = blocks
        .iter()
        .filter(|block| block.end < caret && blank(block.end..caret))
        .max_by_key(|block| block.end);
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
    ) -> Option<(Entity<EditorState>, String, Vec<Range<usize>>)> {
        let editing = self.editing.as_ref()?;
        if !editing.live_preview.enabled || !editing.markdown {
            return None;
        }
        let input = editing.input.clone();
        let (stamp, text) = {
            let state = input.read(cx);
            (state.source_stamp(), state.value().to_string())
        };
        let blocks = editing.live_preview.rendered_blocks(&SourceSnapshot {
            stamp,
            text: Arc::from(text.as_str()),
        });
        Some((input, text, blocks))
    }

    /// The block focus, when it still holds: the caret has not moved and the
    /// source has not changed since Esc.
    pub(super) fn valid_block_focus(&self, cx: &App) -> Option<BlockFocus> {
        let editing = self.editing.as_ref()?;
        let focus = editing.live_preview.block_focus.clone()?;
        let state = editing.input.read(cx);
        (editing.live_preview.enabled
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
            state.set_selected_range(block.end..block.start, cx)
        });
        cx.notify();
    }

    /// F2 in the note editor: open the focused rendered block, else the one
    /// next to the caret; otherwise the key renames the note as before.
    fn open_block_from_keyboard(&mut self, cx: &mut Context<Self>) {
        if let Some(focus) = self.valid_block_focus(cx) {
            self.open_rendered_block(focus.block, focus.caret, cx);
            return;
        }
        let target = self.live_blocks_now(cx).and_then(|(input, text, blocks)| {
            let selection = input.read(cx).selected_range();
            selection
                .is_empty()
                .then(|| block_next_to(&text, &blocks, selection.start))
                .flatten()
                .map(|block| (block, selection.start))
        });
        match target {
            Some((block, caret)) => self.open_rendered_block(block, caret, cx),
            None => cx.propagate(),
        }
    }

    /// Enter on a block Esc just returned to opens it again; any other Enter
    /// is the editor's. Reads only the remembered focus.
    fn enter_focused_block(&mut self, action: &Enter, cx: &mut Context<Self>) {
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
        let Some((input, text, blocks)) = self.live_blocks_now(cx) else {
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
            .or_else(|| caret_outside(&text, &block));
        let Some(caret) = caret else {
            return false;
        };
        editing.live_preview.block_focus = Some(BlockFocus {
            stamp,
            block,
            caret,
        });
        input.update(cx, |state, cx| state.set_selected_range(caret..caret, cx));
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
