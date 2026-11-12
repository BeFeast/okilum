//! Revision-bound editor Find. Source edits schedule one debounced worker;
//! the keystroke path never flattens a Rope, builds a matcher or scans text.
use gpui::*;
use gpui_component::input::projection::SourceMutation;
use gpui_component::input::{EditorState, InputEvent, InputState, RopeExt as _};
use std::time::Duration;

pub(crate) struct EditorTarget {
    source: Entity<EditorState>,
    pub query: Entity<InputState>,
    case_sensitive: std::rc::Rc<std::cell::Cell<bool>>,
    active: bool,
    generation: u64,
    task: Option<Task<()>>,
    _subscriptions: [Subscription; 2],
}

pub(crate) fn literal_matches(
    text: &str,
    query: &str,
    sensitive: bool,
) -> Vec<std::ops::Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    regex::RegexBuilder::new(&regex::escape(query))
        .case_insensitive(!sensitive)
        .build()
        .map(|re| re.find_iter(text).map(|hit| hit.range()).collect())
        .unwrap_or_default()
}

impl EditorTarget {
    #[cfg(test)]
    pub(crate) fn new(
        source: Entity<EditorState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Find"));
        Self::with_query(
            source,
            query,
            std::rc::Rc::new(std::cell::Cell::new(
                crate::reader_ui_state::find_case_sensitive(cx),
            )),
            cx,
        )
    }

    pub(crate) fn with_query(
        source: Entity<EditorState>,
        query: Entity<InputState>,
        case_sensitive: std::rc::Rc<std::cell::Cell<bool>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let changed = cx.subscribe(&query, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Change => this.apply_query(cx),
            InputEvent::PressEnter { shift, .. } if this.active => {
                this.step(if *shift { -1 } else { 1 }, cx)
            }
            _ => {}
        });
        let mutated = cx.subscribe(&source, |this, _, _: &SourceMutation, cx| {
            if this.active && !this.query.read(cx).value().is_empty() {
                this.apply_query(cx);
            }
        });
        Self {
            source,
            query,
            case_sensitive,
            active: true,
            generation: 0,
            task: None,
            _subscriptions: [changed, mutated],
        }
    }

    pub(crate) fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.active == active {
            return;
        }
        self.active = active;
        self.generation += 1;
        self.task = None;
        if active {
            self.apply_query(cx);
        }
    }

    pub(crate) fn target(entity: &Entity<Self>, cx: &App) -> super::Target {
        let state = entity.read(cx);
        let (current, total) = state.count(cx);
        let count = if state.query.read(cx).value().is_empty() {
            String::new()
        } else if total == 0 {
            "No matches".into()
        } else {
            format!("{} of {total}", current + 1)
        };
        let case = entity.clone();
        let step = entity.clone();
        let close = entity.clone();
        super::Target {
            input: state.query.clone(),
            focus_override: None,
            scope: "Find".into(),
            count: Some(count.into()),
            match_case: Some((
                state.case_sensitive.get(),
                std::rc::Rc::new(move |_, cx| {
                    case.update(cx, |s, cx| {
                        s.set_case_sensitive(!s.case_sensitive.get(), cx)
                    })
                }),
            )),
            step: Some(std::rc::Rc::new(move |d, _, cx| {
                step.update(cx, |s, cx| s.step(d, cx))
            })),
            steps_visible: true,
            close: std::rc::Rc::new(move |w, cx| close.update(cx, |s, cx| s.clear(w, cx))),
            large_file: false,
        }
    }

    fn apply_query(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.task = None;
        if !self.active {
            return;
        }
        let query = self.query.read(cx).value().to_string();
        if query.is_empty() {
            self.source.update(cx, |s, cx| {
                // Capture the match-return anchor before retiring marks.
                s.close_search(cx);
                s.set_external_search_results(
                    String::new(),
                    !self.case_sensitive.get(),
                    vec![],
                    0,
                    cx,
                );
            });
            return;
        }
        // Reserve external presentation immediately. A source/view handoff
        // must not let the toolkit's hidden search panel reclaim field focus
        // while the replacement worker is pending. This does no matching.
        self.source
            .update(cx, |s, cx| s.set_external_search_presentation(true, cx));
        let generation = self.generation;
        let sensitive = self.case_sensitive.get();
        self.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(60))
                .await;
            let Ok((text, stamp)) = this.read_with(cx, |s, cx| {
                let source = s.source.read(cx);
                (source.text().clone(), source.source_stamp())
            }) else {
                return;
            };
            let work = cx.background_executor().spawn(async move {
                let ranges = literal_matches(&text.to_string(), &query, sensitive);
                (query, ranges)
            });
            let (query, ranges) = work.await;
            let _ = this.update(cx, |s, cx| {
                if s.generation != generation
                    || !s.active
                    || s.source.read(cx).source_stamp() != stamp
                {
                    return;
                }
                s.source.update(cx, |source, cx| {
                    let first = ranges.first().cloned();
                    source.set_external_search_results(query, !sensitive, ranges, 0, cx);
                    if let Some(range) = first {
                        Self::reveal(source, range, cx);
                    }
                });
                // Dirty the live sibling field, never Reader or cached slots.
                s.query.update(cx, |_, cx| cx.notify());
                cx.notify();
            });
        }));
    }

    #[cfg(test)]
    pub(crate) fn set_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.query
            .update(cx, |i, cx| i.set_value(query, window, cx));
        self.apply_query(cx);
    }
    pub(crate) fn set_case_sensitive(&mut self, sensitive: bool, cx: &mut Context<Self>) {
        if self.case_sensitive.get() == sensitive {
            return;
        }
        self.case_sensitive.set(sensitive);
        crate::reader_ui_state::remember_find_case_sensitive(sensitive, cx);
        self.apply_query(cx);
    }
    pub(crate) fn count(&self, cx: &App) -> (usize, usize) {
        let m = &self.source.read(cx).search_session().matcher;
        (m.current_match_index(), m.matched_ranges().len())
    }
    fn reveal(
        source: &mut EditorState,
        range: std::ops::Range<usize>,
        cx: &mut Context<EditorState>,
    ) {
        let position = source.text().offset_to_position(range.start);
        source.unfold_at(position, cx);
        source.set_selected_range(range, cx);
    }
    pub(crate) fn step(&mut self, direction: i32, cx: &mut Context<Self>) {
        self.source.update(cx, |s, cx| {
            let range = if direction < 0 {
                s.previous_search_match(cx)
            } else {
                s.next_search_match(cx)
            };
            if let Some(range) = range {
                Self::reveal(s, range, cx);
            }
        });
        self.query.update(cx, |_, cx| cx.notify());
    }
    #[cfg(test)]
    pub(crate) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.query.update(cx, |i, cx| i.focus(window, cx));
    }
    pub(crate) fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.query.update(cx, |i, cx| i.set_value("", window, cx));
        self.apply_query(cx);
        self.source.update(cx, |s, cx| s.focus(window, cx));
    }
}
