//! Settings «Show the open note in the file tree» (#1762): whether opening a
//! note moves the folder tree to it. Off by default; Reveal in sidebar
//! (#1756) does the same on request.
//!
//! With it on, the tree opens the folders above the note and selects it,
//! but it never takes keyboard focus or opens a closed sidebar, it waits
//! while the user scrolls the tree, it only scrolls when the row is off
//! screen, and it does not open a large closed folder: that folder is
//! selected instead. With it off, the note's row is selected only if it is
//! already listed, and the tree does not scroll.
use super::*;

/// A closed folder with more entries than this is not opened by auto-reveal.
pub(crate) const AUTO_REVEAL_MAX_CHILDREN: usize = 200;
/// How long after the last wheel scroll over the tree auto-reveal waits.
pub(crate) const TREE_WHEEL_QUIET: Duration = Duration::from_millis(1200);

impl Reader {
    /// Render-time inputs of `sync_tree`: the app preference and whether the
    /// user is scrolling the tree right now.
    pub(crate) fn prepare_tree_follow(&mut self, cx: &mut Context<Self>) {
        let enabled = reader_ui_state::tree_auto_reveal(cx);
        if enabled != self.tree_auto_reveal {
            self.tree_auto_reveal = enabled;
            // Switching it on shows the open note at once.
            if enabled {
                self.tree_revealed.clear();
            }
        }
        if let Some(at) = self.tree_wheel_at {
            if cx.background_executor().now().saturating_duration_since(at) >= TREE_WHEEL_QUIET {
                self.tree_wheel_at = None;
            }
        }
    }

    /// After `sync_tree`: a reveal that waited for the tree to go quiet runs
    /// once it has.
    pub(crate) fn schedule_tree_follow(&mut self, cx: &mut Context<Self>) {
        if !self.tree_reveal_waiting {
            self.tree_reveal_retry = None;
            return;
        }
        if self.tree_reveal_retry.is_some() {
            return;
        }
        let Some(at) = self.tree_wheel_at else {
            return;
        };
        let executor = cx.background_executor().clone();
        let wait = TREE_WHEEL_QUIET.saturating_sub(executor.now().saturating_duration_since(at));
        self.tree_reveal_retry = Some(cx.spawn(async move |this, cx| {
            executor.timer(wait).await;
            let _ = this.update(cx, |this, cx| {
                this.tree_reveal_retry = None;
                cx.notify();
            });
        }));
    }

    /// A wheel scroll over the tree, seen in the capture phase so the list's
    /// own scrolling is untouched.
    pub(crate) fn tree_wheel_scrolled(&mut self, cx: &mut Context<Self>) {
        self.tree_wheel_at = Some(cx.background_executor().now());
        // The wait restarts from this scroll.
        self.tree_reveal_retry = None;
    }

    /// `sync_tree` for a newly open note (not a rename or creation row).
    pub(crate) fn follow_open_note_in_tree(&mut self, note: &str) {
        if !self.tree_auto_reveal {
            self.tree_reveal_waiting = false;
            self.tree_revealed = note.to_owned();
            // A folder that stands for its index note (#1512) is selected
            // if it is listed; nothing opens and nothing scrolls.
            let row = self
                .tree_index_folder(note)
                .unwrap_or_else(|| note.to_owned());
            self.tree.select_if_listed(&row);
            return;
        }
        if self.tree_wheel_at.is_some() {
            // `tree_revealed` stays behind, so a later sync tries again.
            self.tree_reveal_waiting = true;
            return;
        }
        self.tree_reveal_waiting = false;
        self.tree_revealed = note.to_owned();
        let ix = self.tree.auto_reveal(note, AUTO_REVEAL_MAX_CHILDREN);
        let ix = if self.tree.cursor.as_deref() == Some(note) {
            self.select_index_folder(note, ix)
        } else {
            ix
        };
        if let Some(ix) = ix {
            if !self.tree_row_on_screen(ix) {
                self.scroll_tree_to(ix);
            }
        }
    }

    /// Whether tree row `ix` is fully inside the tree viewport as last laid
    /// out. A tree that was never laid out shows nothing.
    fn tree_row_on_screen(&self, ix: usize) -> bool {
        let scroll = self.tree_scroll.0.borrow();
        let height = f32::from(scroll.base_handle.bounds().size.height);
        let top = -f32::from(scroll.base_handle.offset().y);
        let row_top = TREE_ROW_HEIGHT * ix as f32;
        height > 0. && row_top >= top && row_top + TREE_ROW_HEIGHT <= top + height
    }
}

#[cfg(test)]
#[path = "reader_tree_follow/tests.rs"]
mod tests;
