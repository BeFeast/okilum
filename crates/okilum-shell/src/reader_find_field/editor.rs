//! The external editor target. This stage exercises it only through native
//! component tests; later code/read and edit stages connect it to their header.
use gpui::*;
use gpui_component::input::projection::SourceMutation;
use gpui_component::input::{EditorState, InputEvent, InputState, RopeExt as _};

pub(super) struct EditorTarget {
    source: Entity<EditorState>,
    pub query: Entity<InputState>,
    case_sensitive: bool,
    _subscriptions: [Subscription; 2],
}

impl EditorTarget {
    pub(super) fn new(
        source: Entity<EditorState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Find"));
        let changed = cx.subscribe(&query, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Change => this.apply_query(cx),
            InputEvent::PressEnter { shift, .. } => this.step(if *shift { -1 } else { 1 }, cx),
            InputEvent::Focus | InputEvent::Blur => cx.notify(),
        });
        let mutated = cx.subscribe(&source, |this, _, _: &SourceMutation, cx| {
            // Cursor, hover and layout notifications never rescan the source.
            // Only an actual source mutation refreshes an existing query.
            if !this.query.read(cx).value().is_empty() {
                this.apply_query(cx);
            }
        });
        Self {
            source,
            query,
            case_sensitive: false,
            _subscriptions: [changed, mutated],
        }
    }

    fn apply_query(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().to_string();
        let case_insensitive = !self.case_sensitive;
        self.source.update(cx, |source, cx| {
            source.set_search_query(query.clone(), case_insensitive, cx);
            if query.is_empty() {
                source.close_search(cx);
            } else {
                source.set_external_search_presentation(true, cx);
            }
        });
        cx.notify();
    }

    pub(super) fn set_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        // Programmatic set_value does not emit InputEvent::Change.
        self.query
            .update(cx, |input, cx| input.set_value(query, window, cx));
        self.apply_query(cx);
    }

    pub(super) fn set_case_sensitive(&mut self, sensitive: bool, cx: &mut Context<Self>) {
        self.case_sensitive = sensitive;
        self.apply_query(cx);
    }

    /// Zero-based current match and total; the field supplies human numbering.
    pub(super) fn count(&self, cx: &App) -> (usize, usize) {
        let source = self.source.read(cx);
        let matcher = &source.search_session().matcher;
        (
            matcher.current_match_index(),
            matcher.matched_ranges().len(),
        )
    }

    pub(super) fn step(&mut self, direction: i32, cx: &mut Context<Self>) {
        self.source.update(cx, |source, cx| {
            let range = if direction < 0 {
                source.previous_search_match(cx)
            } else {
                source.next_search_match(cx)
            };
            if let Some(range) = range {
                let position = source.text().offset_to_position(range.start);
                source.unfold_at(position, cx);
            }
        });
        cx.notify();
    }

    pub(super) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.query.update(cx, |input, cx| input.focus(window, cx));
    }

    pub(super) fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_query("", window, cx);
        self.source
            .update(cx, |source, cx| source.focus(window, cx));
    }
}
