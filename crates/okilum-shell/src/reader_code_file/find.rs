//! The single shared field selects a whole-snapshot or bounded editor adapter.
use super::*;
use std::rc::Rc;

impl CodePreview {
    fn large_find(&self) -> bool {
        self.paged
            || self
                .raw_document
                .as_ref()
                .is_some_and(|doc| doc.pages.len() > 1)
            || self.source_bytes.is_some_and(|bytes| bytes > EDIT_BYTES)
    }
    pub(super) fn whole_find(&self) -> bool {
        self.view == View::Tree || self.paged
    }

    pub(crate) fn find_target(&self, entity: Entity<Self>, cx: &App) -> reader_find_field::Target {
        if let Some(editor) = self.editor_find.as_ref().filter(|_| !self.whole_find()) {
            let mut target = reader_find_field::editor::EditorTarget::target(editor, cx);
            target.large_file = self.large_find();
            target.scope = if target.large_file {
                "Find in whole file"
            } else {
                "Find"
            }
            .into();
            return target;
        }
        let mut target = self.find_descriptor(entity, cx);
        target.count = Some(
            if self.find.read(cx).value().is_empty() {
                String::new()
            } else if self.searching {
                "Searching…".into()
            } else if self.matches.is_empty() {
                "No matches".into()
            } else {
                format!("{} of {}", self.match_index + 1, self.matches.len())
            }
            .into(),
        );
        target
    }

    /// Cached slots must never read the query Input or the editor matcher.
    /// The live sibling resolves count/case/results from find_target instead.
    pub(super) fn find_descriptor(
        &self,
        entity: Entity<Self>,
        _cx: &App,
    ) -> reader_find_field::Target {
        let case = entity.clone();
        let step = entity.clone();
        let close = entity.clone();
        reader_find_field::Target {
            input: self.find.clone(),
            focus_override: None,
            scope: if self.large_find() {
                "Find in whole file"
            } else {
                "Find"
            }
            .into(),
            count: None,
            match_case: Some((
                self.case_sensitive.get(),
                Rc::new(move |w, cx| {
                    case.update(cx, |s, cx| {
                        if !s.whole_find() {
                            if let Some(editor) = &s.editor_find {
                                editor.update(cx, |e, cx| {
                                    e.set_case_sensitive(!s.case_sensitive.get(), cx)
                                });
                            }
                            return;
                        }
                        s.case_sensitive.set(!s.case_sensitive.get());
                        reader_ui_state::remember_find_case_sensitive(s.case_sensitive.get(), cx);
                        s.search(w, cx);
                    })
                }),
            )),
            step: Some(Rc::new(move |d, w, cx| {
                step.update(cx, |s, cx| {
                    if s.whole_find() {
                        s.step_match(d, w, cx);
                    } else if let Some(editor) = &s.editor_find {
                        editor.update(cx, |e, cx| e.step(d, cx));
                    }
                })
            })),
            steps_visible: true,
            close: Rc::new(move |w, cx| {
                close.update(cx, |s, cx| {
                    if s.whole_find() {
                        s.focus.focus(w, cx);
                    } else if let Some(input) = s.input() {
                        input.update(cx, |i, cx| i.focus(w, cx));
                    }
                })
            }),
            large_file: self.large_find(),
        }
    }

    pub(super) fn search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if reader_ui_state::installed(cx) {
            self.case_sensitive
                .set(reader_ui_state::find_case_sensitive(cx));
        }
        let placeholder = if self.large_find() {
            "Find in whole file"
        } else {
            "Find"
        };
        self.find
            .update(cx, |s, cx| s.set_placeholder(placeholder, window, cx));
        self.search_revision += 1;
        let revision = self.search_revision;
        let whole = self.whole_find();
        if let Some(editor) = &self.editor_find {
            editor.update(cx, |editor, cx| {
                editor.set_active(!whole && self.view != View::Table && self.read_active, cx);
                if !whole {
                    editor.set_case_sensitive(self.case_sensitive.get(), cx);
                }
            });
        }
        if !whole {
            return;
        }
        let Some(doc) = self.document.clone() else {
            return;
        };
        let query = self.find.read(cx).value().to_string();
        let key = (
            self.document_revision,
            query.clone(),
            self.case_sensitive.get(),
        );
        if self.search_key.as_ref() == Some(&key) && !self.searching {
            self.show_match(window, cx);
            return;
        }
        self.search_key = Some(key);
        if query.is_empty() {
            self.structure.finish_find(&doc);
            if let Some(input) = self.input() {
                input.update(cx, |s, cx| {
                    s.close_search(cx);
                    s.set_external_search_results(
                        String::new(),
                        !self.case_sensitive.get(),
                        vec![],
                        0,
                        cx,
                    );
                });
            }
        }
        self.match_pattern = None;
        self.matches.clear();
        self.match_rows.clear();
        self.scrollbar_marks.clear();
        self.structure.match_node = None;
        self.searching = !query.is_empty();
        let sensitive = self.case_sensitive.get();
        let task = cx.background_executor().spawn(async move {
            let pattern = (!query.is_empty()).then(|| {
                regex::RegexBuilder::new(&regex::escape(&query))
                    .case_insensitive(!sensitive)
                    .build()
                    .unwrap()
            });
            let matches = if sensitive {
                doc.find_case(&query, true)
            } else {
                doc.find(&query)
            };
            let mut rows = std::collections::HashMap::new();
            for hit in &matches {
                if let Some(row) = doc.row_at(hit) {
                    rows.entry(row).or_insert_with(|| hit.clone());
                }
            }
            let marks = sample_marks(&matches, &doc);
            (matches, rows, marks, pattern)
        });
        cx.spawn_in(window, async move |this, cx| {
            let (matches, rows, marks, pattern) = task.await;
            let _ = this.update_in(cx, |s, window, cx| {
                if s.search_revision != revision {
                    return;
                }
                s.searching = false;
                s.matches = matches;
                s.match_pattern = pattern;
                s.match_rows = rows;
                s.scrollbar_marks = marks;
                s.match_index = 0;
                s.show_match(window, cx);
                s.find.update(cx, |_, cx| cx.notify());
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn step_match(
        &mut self,
        direction: i32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.matches.is_empty() {
            return;
        }
        self.match_index = (self.match_index as i64 + direction as i64)
            .rem_euclid(self.matches.len() as i64) as usize;
        self.show_match(window, cx);
        self.find.update(cx, |_, cx| cx.notify());
    }

    pub(super) fn show_match(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(range) = self.matches.get(self.match_index).cloned() else {
            return;
        };
        let Some(doc) = &self.document else {
            return;
        };
        if self.view == View::Tree {
            self.structure.reveal(&range, doc);
            cx.notify();
            return;
        }
        self.show_page(doc.page_at(range.start), window, cx);
        let page = self.document.as_ref().unwrap().pages[self.page].clone();
        let ranges: Vec<_> = self
            .matches
            .iter()
            .filter(|r| r.start < page.end && r.end > page.start)
            .map(|r| r.start.saturating_sub(page.start)..r.end.min(page.end) - page.start)
            .collect();
        let current = ranges
            .iter()
            .position(|r| r.start == range.start.saturating_sub(page.start))
            .unwrap_or(0);
        let query = self.find.read(cx).value().to_string();
        if let State::Content(input) = &self.state {
            input.update(cx, |s, cx| {
                s.set_external_search_results(
                    query,
                    !self.case_sensitive.get(),
                    ranges,
                    current,
                    cx,
                );
                s.set_selected_range(
                    range.start - page.start..range.end.min(page.end) - page.start,
                    cx,
                );
            });
        }
    }
}

/// Evenly sample endpoints as well as the middle; never allocate >1,000 marks.
pub(super) fn sample_marks(matches: &[std::ops::Range<usize>], doc: &Document) -> Vec<usize> {
    let count = matches.len().min(1000);
    (0..count)
        .map(|i| {
            let at = if count < 2 {
                0
            } else {
                i * (matches.len() - 1) / (count - 1)
            };
            doc.row_at(&matches[at]).unwrap_or(0)
        })
        .collect()
}
