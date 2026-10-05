//! Literal Find over the current managed buffer; no source writes or search index.
use super::*;
use gpui_base::input::SearchMatcher;
use gpui_component::input::{
    projection::{SourceMutation, SourceMutationReason, SourceStamp},
    Rope, TextDecoration, TextDecorationCollection,
};
use std::ops::Range;

const MAX_QUERY_BYTES: usize = 2048;
const MAX_SOURCE_BYTES: usize = editor_recovery::MAX_TEXT_BYTES;

#[derive(Clone, PartialEq)]
struct Owner {
    endpoint: SocketAddr,
    workspace: Value,
    entity: EntityId,
    path: String,
}
#[derive(Clone, PartialEq, Debug)]
struct Cursor {
    bytes: Range<usize>,
    reversed: bool,
}
impl Cursor {
    fn range(&self) -> Range<usize> {
        if self.reversed {
            self.bytes.end..self.bytes.start
        } else {
            self.bytes.clone()
        }
    }
}
struct Panel {
    owner: Owner,
    initial_stamp: SourceStamp,
    original_cursor: Cursor,
    original_scroll: Point<Pixels>,
    restore_cursor: bool,
    selected_by_find: Cursor,
    query: Entity<InputState>,
    _subscription: Subscription,
    stamp: SourceStamp,
    query_text: String,
    ranges: Vec<Range<usize>>,
    current: Option<usize>,
    pending: bool,
    message: Option<&'static str>,
}
#[derive(Default)]
pub(super) struct SourceFind {
    panel: Option<Panel>,
    generation: u64,
    in_flight: bool,
    // One reusable collection per editor; the vendor retains collection IDs until drop.
    highlight: Option<TextDecorationCollection>,
}
impl SourceFind {
    pub(super) fn active(&self) -> bool {
        self.panel.is_some()
    }
}

/// Construct and consume the non-Send matcher entirely inside the worker.
/// Unwrap its range vector after dropping the matcher to avoid a second full copy.
fn search_ranges(source: Rope, query: &str) -> Vec<Range<usize>> {
    let mut matcher = SearchMatcher::new();
    matcher.update(&source);
    matcher.update_query(query, false);
    let ranges = matcher.matched_ranges();
    drop(matcher);
    std::rc::Rc::try_unwrap(ranges).expect("matcher has released its ranges")
}

impl BrainView {
    pub(super) fn source_find_enabled(&self) -> bool {
        self.source.managed().is_some()
            && self.source_snapshot.is_some()
            && self.expected_workspace.is_some()
            && self.surface == Surface::Source
            && !self.source_loading
            && !self.busy
            && !self.editor_closing()
            && !self.editor_guarded_save()
            && !self.editor_find_blocked()
            && !self.discussion_note.active()
            && !self.goal_criteria.active
            && !self.discussion_decision.active
            && !self.decision_reuse.active
            && (self.pending_navigation.is_none()
                || matches!(
                    self.pending_navigation,
                    Some(PendingNavigation::SourceBack | PendingNavigation::SourceLink(..))
                ))
            && self.pending_source_write.is_none()
            && self.source_conflict.is_none()
    }
    fn find_owner(&self) -> Option<Owner> {
        self.source_find_enabled().then(|| Owner {
            endpoint: self.endpoint,
            workspace: self.expected_workspace.clone().unwrap(),
            entity: self.source.entity_id(),
            path: text(&self.source_snapshot.as_ref().unwrap()["path"]),
        })
    }
    fn find_cursor(&self, window: &mut Window, cx: &mut App) -> Option<Cursor> {
        self.source.managed()?.update(cx, |state, cx| {
            if state.marked_text_range(window, cx).is_some() {
                return None;
            }
            Some(Cursor {
                bytes: state.selected_range(),
                reversed: state.selected_text_range(false, window, cx)?.reversed,
            })
        })
    }
    fn find_query_matches(panel: &Panel, cx: &App) -> bool {
        let text = panel.query.read(cx).text();
        text.len() <= MAX_QUERY_BYTES && *text == panel.query_text
    }
    fn find_composing(&self, window: &mut Window, cx: &mut App) -> bool {
        self.find_cursor(window, cx).is_none()
            || self.source_find.panel.as_ref().is_some_and(|p| {
                p.query
                    .update(cx, |q, cx| q.marked_text_range(window, cx).is_some())
            })
    }
    pub(super) fn source_find_focus(&self, window: &Window, cx: &App) -> bool {
        self.source
            .managed()
            .is_some_and(|s| s.focus_handle(cx).is_focused(window))
            || self
                .source_find
                .panel
                .as_ref()
                .is_some_and(|p| p.query.focus_handle(cx).is_focused(window))
    }
    pub(super) fn open_source_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_source_find(window, cx);
        let Some(owner) = self.find_owner() else {
            return;
        };
        if self.find_composing(window, cx) {
            self.notice = "Finish text composition before opening Find.".into();
            cx.notify();
            return;
        }
        if let Some(panel) = &self.source_find.panel {
            panel.query.update(cx, |q, cx| {
                q.focus(window, cx);
                q.select_all(window, cx);
            });
            return;
        }
        let Some(cursor) = self.find_cursor(window, cx) else {
            return;
        };
        let editor = self.source.managed().unwrap().read(cx);
        let stamp = editor.source_stamp();
        let scroll = editor.scroll_offset();
        let query_text = if cursor.bytes.len() <= MAX_QUERY_BYTES {
            editor.selected_text().to_string()
        } else {
            String::new()
        };
        let query =
            cx.new(|cx| InputState::new(window, cx).placeholder("Find in note · case-sensitive"));
        query.update(cx, |q, cx| q.set_value(query_text, window, cx));
        let subscription = cx.subscribe_in(
            &query,
            window,
            |this, query, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change)
                    && this
                        .source_find
                        .panel
                        .as_ref()
                        .is_some_and(|p| p.query.entity_id() == query.entity_id())
                {
                    this.refresh_source_find(window, cx);
                }
            },
        );
        if self.source_find.highlight.is_none() {
            self.source_find.highlight = Some(
                self.source
                    .managed()
                    .unwrap()
                    .update(cx, |s, cx| s.create_decorations_collection(Vec::new(), cx)),
            );
        }
        self.source_find.panel = Some(Panel {
            owner,
            initial_stamp: stamp,
            original_cursor: cursor.clone(),
            original_scroll: scroll,
            restore_cursor: true,
            selected_by_find: cursor.clone(),
            query: query.clone(),
            _subscription: subscription,
            stamp,
            query_text: String::new(),
            ranges: Vec::new(),
            current: None,
            pending: false,
            message: None,
        });
        self.apply_source_projection(cx);
        self.refresh_source_find(window, cx);
        if cursor.bytes.len() > MAX_QUERY_BYTES {
            self.source_find.panel.as_mut().unwrap().message =
                Some("Selection exceeds 2048 UTF-8 bytes; enter a shorter query.");
        }
        query.update(cx, |q, cx| {
            q.focus(window, cx);
            q.select_all(window, cx);
        });
        cx.notify();
    }
    pub(super) fn sync_source_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.source_find.active() {
            return;
        }
        if self
            .source_find
            .panel
            .as_ref()
            .is_some_and(|p| self.find_owner().as_ref() != Some(&p.owner))
        {
            self.invalidate_source_find(cx);
        }
        let cursor = self.find_cursor(window, cx);
        let stamp = self.source.stamp(cx);
        if let Some(panel) = &mut self.source_find.panel {
            if stamp != Some(panel.initial_stamp)
                || cursor
                    .as_ref()
                    .is_some_and(|cursor| cursor != &panel.selected_by_find)
            {
                panel.restore_cursor = false;
            }
        }
        if self
            .source_find
            .panel
            .as_ref()
            .is_some_and(|panel| stamp != Some(panel.stamp))
        {
            self.refresh_source_find(window, cx);
        }
    }
    fn clear_find_highlight(&self, cx: &mut App) {
        if let Some(highlight) = &self.source_find.highlight {
            highlight.clear(cx);
        }
    }
    pub(super) fn invalidate_source_find(&mut self, cx: &mut Context<Self>) {
        if self.source_find.panel.take().is_some() {
            self.clear_find_highlight(cx);
            self.source_find.generation = self.source_find.generation.wrapping_add(1);
            self.apply_source_projection(cx);
            cx.notify();
        }
    }
    pub(super) fn source_find_changed(
        &mut self,
        event: SourceMutation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.source_find.active() {
            return;
        }
        if event.reason == SourceMutationReason::Reset {
            if self
                .source_find
                .panel
                .as_ref()
                .is_some_and(|panel| event.stamp.generation > panel.initial_stamp.generation)
            {
                self.invalidate_source_find(cx);
            }
        } else if self.source.stamp(cx) == Some(event.stamp) {
            self.refresh_source_find(window, cx);
        }
    }
    fn refresh_source_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_find_highlight(cx);
        let Some(panel) = &mut self.source_find.panel else {
            return;
        };
        self.source_find.generation = self.source_find.generation.wrapping_add(1);
        panel.ranges = Vec::new();
        panel.current = None;
        let editor = self.source.managed().unwrap().read(cx);
        panel.stamp = editor.source_stamp();
        let query = panel.query.read(cx).text();
        let query_too_long = query.len() > MAX_QUERY_BYTES;
        panel.query_text = if query_too_long {
            String::new()
        } else {
            query.to_string()
        };
        panel.message = if editor.text().len() > MAX_SOURCE_BYTES {
            Some("Find supports notes up to 8 MiB; this note was not searched.")
        } else if query_too_long {
            Some("Find supports queries up to 2048 UTF-8 bytes; shorten the query.")
        } else if panel.query_text.is_empty() {
            Some("Enter text to find · case-sensitive")
        } else {
            None
        };
        panel.pending = panel.message.is_none();
        self.start_source_find_worker(window, cx);
        cx.notify();
    }
    fn start_source_find_worker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.source_find.in_flight {
            return;
        }
        let Some(panel) = &self.source_find.panel else {
            return;
        };
        if !panel.pending {
            return;
        }
        let owner = panel.owner.clone();
        let stamp = panel.stamp;
        let query = panel.query_text.clone();
        let generation = self.source_find.generation;
        let source = self.source.managed().unwrap().read(cx).text().clone();
        self.source_find.in_flight = true;
        cx.spawn_in(window, async move |this, cx| {
            let worker_query = query.clone();
            let ranges = cx
                .background_executor()
                .spawn(async move { search_ranges(source, &worker_query) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.source_find.in_flight = false;
                this.sync_source_find(window, cx);
                if this.source_find.generation == generation && this.source.stamp(cx) == Some(stamp)
                {
                    if let Some(panel) = &mut this.source_find.panel {
                        if panel.owner == owner
                            && panel.stamp == stamp
                            && panel.query_text == query
                            && Self::find_query_matches(panel, cx)
                        {
                            panel.ranges = ranges;
                            panel.pending = false;
                        }
                    }
                }
                this.start_source_find_worker(window, cx);
                cx.notify();
            });
        })
        .detach();
    }
    fn step_source_find(&mut self, previous: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_source_find(window, cx);
        if self.find_composing(window, cx) {
            return;
        }
        let stamp = self.source.stamp(cx);
        let Some(panel) = &mut self.source_find.panel else {
            return;
        };
        if panel.pending
            || stamp != Some(panel.stamp)
            || panel.ranges.is_empty()
            || !Self::find_query_matches(panel, cx)
        {
            return;
        }
        let count = panel.ranges.len();
        let index = match (panel.current, previous) {
            (Some(i), false) => (i + 1) % count,
            (Some(i), true) => (i + count - 1) % count,
            (None, false) => 0,
            (None, true) => count - 1,
        };
        let range = panel.ranges[index].clone();
        panel.current = Some(index);
        panel.selected_by_find = Cursor {
            bytes: range.clone(),
            reversed: false,
        };
        self.source
            .managed()
            .unwrap()
            .update(cx, |s, cx| s.set_selected_range(range.clone(), cx));
        if let Some(highlight) = &self.source_find.highlight {
            highlight.set(
                vec![TextDecoration::new(
                    range,
                    HighlightStyle {
                        background_color: Some(cx.theme().selection),
                        ..Default::default()
                    },
                )],
                cx,
            );
        }
        cx.notify();
    }
    pub(super) fn close_source_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_source_find(window, cx);
        if self.find_composing(window, cx) {
            self.notice = "Finish text composition before closing Find.".into();
            cx.notify();
            return;
        }
        let cursor = self.find_cursor(window, cx);
        let Some(panel) = self.source_find.panel.take() else {
            return;
        };
        self.source_find.generation = self.source_find.generation.wrapping_add(1);
        self.clear_find_highlight(cx);
        self.apply_source_projection(cx);
        if panel.restore_cursor
            && self.source.stamp(cx) == Some(panel.initial_stamp)
            && cursor.as_ref() == Some(&panel.selected_by_find)
        {
            self.source.managed().unwrap().update(cx, |s, cx| {
                s.set_selected_range(panel.original_cursor.range(), cx);
                s.set_scroll_offset(panel.original_scroll, cx);
            });
        }
        self.source.focus(window, cx);
        cx.notify();
    }
    pub(super) fn source_find_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let panel = self.source_find.panel.as_ref()?;
        let status = if let Some(message) = panel.message {
            message.to_string()
        } else if panel.pending {
            "Searching…".into()
        } else if panel.ranges.is_empty() {
            "0 matches".into()
        } else {
            format!(
                "{}/{} · case-sensitive",
                panel.current.map_or(0, |i| i + 1),
                panel.ranges.len()
            )
        };
        let disabled = panel.pending || panel.ranges.is_empty();
        Some(
            v_flex()
                .id("managed-note-find")
                .gap_2()
                .capture_action(cx.listener(
                    |this, action: &gpui_component::input::Enter, window, cx| {
                        this.step_source_find(action.shift, window, cx);
                        cx.stop_propagation();
                    },
                ))
                .child(
                    h_flex()
                        .gap_2()
                        .child(Input::new(&panel.query).flex_1())
                        .child(
                            Button::new("find-previous")
                                .label("Previous")
                                .disabled(disabled)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.step_source_find(true, window, cx)
                                })),
                        )
                        .child(
                            Button::new("find-next")
                                .label("Next")
                                .disabled(disabled)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.step_source_find(false, window, cx)
                                })),
                        )
                        .child(Button::new("find-close").label("Close").on_click(
                            cx.listener(|this, _, window, cx| this.close_source_find(window, cx)),
                        )),
                )
                .child(div().text_xs().child(status))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests;
