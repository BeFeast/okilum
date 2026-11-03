//! Save chrome follows the editor without invalidating the Reader per key.
use super::*;
pub(super) struct SaveStatus {
    reader: WeakEntity<Reader>,
    _subscriptions: [Subscription; 2],
}
impl SaveStatus {
    pub(super) fn new(
        reader: Entity<Reader>,
        input: &Entity<EditorState>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            reader: reader.downgrade(),
            _subscriptions: [
                cx.observe(input, |_, _, cx| cx.notify()),
                cx.observe(&reader, |_, _, cx| cx.notify()),
            ],
        }
    }
}
impl Render for SaveStatus {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.reader
            .upgrade()
            .map(|reader| reader.read(cx).save_status_element(cx))
            .unwrap_or_else(|| div().into_any_element())
    }
}
