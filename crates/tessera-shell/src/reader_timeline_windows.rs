//! Keep the Windows diagnostic read-only until its safe-save/history backend exists.
use super::*;

pub(super) struct Timeline {
    pub(super) selected: Option<usize>,
}

impl Reader {
    pub(super) fn active_timeline(&self) -> Option<&Timeline> {
        self.timeline.as_ref()
    }
    pub(super) fn back_from_timeline(&mut self, _: &mut Window, _: &mut Context<Self>) {}
    pub(super) fn step_timeline(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) {}
    pub(super) fn render_timeline(&self, _: &mut Context<Self>) -> AnyElement {
        unreachable!("The Windows diagnostic has no source history")
    }
    pub(super) fn render_timeline_preview(
        &self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<AnyElement> {
        None
    }
}
