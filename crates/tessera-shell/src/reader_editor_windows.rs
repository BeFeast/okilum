//! Windows diagnostic is read-only until native safe-save is implemented.
use super::*;

// Uninhabited: the Reader can never enter a writable state on Windows.
pub(super) enum Editing {}

impl Reader {
    pub(super) fn open_source_find(&mut self, _: &mut Context<Self>) {}

    pub(super) fn restore_source_position(&self, _: [f32; 2], _: &Window, _: &mut Context<Self>) {}

    pub(super) fn source_highlighting_pending(&self, _: &App) -> bool {
        false
    }

    pub(super) fn source_scroll_offset(&self, _: &App) -> Option<Point<Pixels>> {
        None
    }
    pub(super) fn scroll_source_by(&mut self, _: Pixels, _: &mut Context<Self>) -> bool {
        false
    }

    pub(super) fn source_history(&mut self, _: bool, _: &mut Window, cx: &mut Context<Self>) {
        self.link_notice =
            Some("Source history is not available in the read-only Windows diagnostic.".into());
        cx.notify();
    }
    pub(super) fn recover_link_moves(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.link_notice =
            Some("Link move recovery is not available in the read-only Windows diagnostic.".into());
        cx.notify();
    }
    pub(super) fn rename_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_source(window, cx);
    }
    pub(super) fn new_folder(
        &mut self,
        _: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_source(window, cx);
    }
    pub(super) fn new_note(
        &mut self,
        _: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_source(window, cx);
    }
    pub(super) fn discover_source_recovery(&mut self, _: &mut Context<Self>) {}
    pub(super) fn install_source_lifecycle(&mut self, _: &mut Window, _: &mut Context<Self>) {}
    pub(super) fn toggle_source(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.link_notice = Some("Editing is not available on Windows yet".into());
        cx.notify();
    }
    pub(super) fn request_source_save(&mut self, _: &mut Context<Self>) {}
    pub(super) fn refresh_source_from_disk(&mut self, _: &mut Window, _: &mut Context<Self>) {}
    pub(super) fn save_source(&mut self, _: &mut Context<Self>) -> bool {
        true
    }
    pub(super) fn leave_source(&mut self, _: &mut Context<Self>) -> bool {
        true
    }
    pub(super) fn render_source(&self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        unreachable!("Windows diagnostic cannot enter source editing")
    }
}
