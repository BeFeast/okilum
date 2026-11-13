//! The single shared field selects a whole-snapshot or bounded editor adapter.
use super::*;
use std::rc::Rc;

impl CodePreview {
    pub(super) fn cancel_search(&mut self) {
        self.search_cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.search_task = None;
        self.search_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    }

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
        if self.view == View::Table {
            if let Some(table) = &self.table {
                return table.read(cx).find_target(table.clone(), true, cx);
            }
        }
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
            } else if self.searching
                || (self.document.is_none() && !matches!(self.state, State::Message(_)))
            {
                "Searching…".into()
            } else if let Some(error) = &self.search_error {
                error.to_string()
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
        cx: &App,
    ) -> reader_find_field::Target {
        if self.view == View::Table {
            if let Some(table) = &self.table {
                return table.read(cx).find_target(table.clone(), false, cx);
            }
        }
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
        let whole = self.whole_find();
        if let Some(editor) = &self.editor_find {
            editor.update(cx, |editor, cx| {
                editor.set_active(!whole && self.view != View::Table && self.read_active, cx);
                if !whole {
                    editor.set_case_sensitive(self.case_sensitive.get(), cx);
                }
            });
        }
        if !whole || !self.read_active {
            self.cancel_search();
            if self.searching {
                self.search_key = None;
                self.searching = false;
            }
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
        if self.search_key.as_ref() == Some(&key) {
            if !self.searching {
                self.show_match(window, cx);
            }
            return;
        }
        self.cancel_search();
        self.search_revision += 1;
        let revision = self.search_revision;
        self.search_key = Some(key);
        self.search_error = None;
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
        self.find.update(cx, |_, cx| cx.notify());
        cx.notify();
        if query.is_empty() {
            return;
        }
        let sensitive = self.case_sensitive.get();
        let cancelled = self.search_cancel.clone();
        #[cfg(test)]
        let scans = self.search_scans.clone();
        #[cfg(test)]
        let started = self.search_scans_started.clone();
        self.search_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(60))
                .await;
            let task = cx.background_executor().spawn(async move {
                #[cfg(test)]
                started.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let result = scan_snapshot(&doc, &query, sensitive, &cancelled);
                #[cfg(test)]
                if result.is_some() {
                    scans.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                result
            });
            let Some(result) = task.await else {
                return;
            };
            let _ = this.update_in(cx, |s, window, cx| {
                if s.search_revision != revision || !s.whole_find() || !s.read_active {
                    return;
                }
                #[cfg(test)]
                {
                    s.search_installs += 1;
                }
                s.searching = false;
                s.matches = result.matches;
                s.match_pattern = result.pattern;
                s.match_rows = result.rows;
                s.scrollbar_marks = result.marks;
                s.search_error = result.error;
                s.match_index = 0;
                s.show_match(window, cx);
                s.finish_presentation_restore(window, cx);
                s.find.update(cx, |_, cx| cx.notify());
                cx.notify();
            });
        }));
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
        self.install_page_matches(cx);
        if let State::Content(input) = &self.state {
            input.update(cx, |s, cx| {
                s.set_selected_range(
                    range.start - page.start..range.end.min(page.end) - page.start,
                    cx,
                )
            });
        }
    }

    pub(super) fn install_page_matches(&self, cx: &mut App) {
        let Some(doc) = &self.document else {
            return;
        };
        let page = doc.pages[self.page].clone();
        let span = page_matches(&self.matches, &page);
        let current = self.match_index.saturating_sub(span.start);
        let ranges = self.matches[span]
            .iter()
            .map(|r| r.start.saturating_sub(page.start)..r.end.min(page.end) - page.start)
            .collect();
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

/// Matches are sorted and disjoint. Only the visible page slice is visited on
/// Enter, independently of the number of hits in the complete document.
fn page_matches(
    matches: &[std::ops::Range<usize>],
    page: &std::ops::Range<usize>,
) -> std::ops::Range<usize> {
    let lo = matches.partition_point(|r| {
        page_probe();
        r.end <= page.start
    });
    let hi = matches.partition_point(|r| {
        page_probe();
        r.start < page.end
    });
    lo..hi
}

pub(super) struct ScanResult {
    pub(super) matches: Vec<std::ops::Range<usize>>,
    rows: std::collections::HashMap<usize, std::ops::Range<usize>>,
    marks: Vec<usize>,
    pattern: Option<regex::Regex>,
    error: Option<SharedString>,
}

pub(super) fn scan_snapshot(
    doc: &Document,
    query: &str,
    sensitive: bool,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Option<ScanResult> {
    scan_snapshot_with_progress(doc, query, sensitive, cancelled, || {})
}

pub(super) fn scan_snapshot_with_progress(
    doc: &Document,
    query: &str,
    sensitive: bool,
    cancelled: &std::sync::atomic::AtomicBool,
    mut progress: impl FnMut(),
) -> Option<ScanResult> {
    use std::sync::atomic::Ordering;
    if cancelled.load(Ordering::Relaxed) {
        return None;
    }
    let compiled = regex::RegexBuilder::new(&regex::escape(query))
        .case_insensitive(!sensitive)
        .build();
    let pattern = match compiled {
        Ok(pattern) => pattern,
        Err(_) => {
            return Some(ScanResult {
                matches: vec![],
                rows: Default::default(),
                marks: vec![],
                pattern: None,
                error: Some("Query is too long".into()),
            });
        }
    };
    let mut matches = Vec::new();
    let mut rows = std::collections::HashMap::new();
    for hit in pattern.find_iter(&doc.text) {
        progress();
        if cancelled.load(Ordering::Relaxed) {
            return None;
        }
        let range = hit.range();
        if let Some(row) = doc.row_at(&range) {
            rows.entry(row).or_insert_with(|| range.clone());
        }
        matches.push(range);
    }
    if cancelled.load(Ordering::Relaxed) {
        return None;
    }
    let marks = sample_marks(&matches, doc);
    Some(ScanResult {
        matches,
        rows,
        marks,
        pattern: Some(pattern),
        error: None,
    })
}

#[cfg(test)]
thread_local! { pub(super) static PAGE_PROBES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[inline]
fn page_probe() {
    #[cfg(test)]
    PAGE_PROBES.with(|probes| probes.set(probes.get() + 1));
}
