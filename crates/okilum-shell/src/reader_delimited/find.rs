//! Loaded-row Find: debounce on the UI executor, projection on a worker.
use super::*;
use std::sync::atomic::Ordering;

impl TablePreview {
    pub(crate) fn save_presentation(&self) -> Saved {
        Saved {
            query: self.query.clone(),
            sensitive: self.case_sensitive,
            sort: self.sort,
            anchor: self.anchor.and_then(|c| self.source_cell(c)),
            end: self.end.and_then(|c| self.source_cell(c)),
            hit_index: self.hit_index,
            horizontal: self.horizontal.offset(),
        }
    }
    pub(crate) fn restore_presentation(
        &mut self,
        saved: Saved,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.query = saved.query.clone();
        self.case_sensitive = saved.sensitive;
        reader_ui_state::remember_find_case_sensitive(saved.sensitive, cx);
        self.sort = saved.sort;
        self.filter.as_ref().unwrap().update(cx, |input, cx| {
            input.set_value(saved.query.clone(), window, cx)
        });
        self.restored = Some(saved);
        self.refilter(cx);
    }

    /// Hide the table: stop an in-flight scan; later requests (a finished
    /// Load more rows, a header toggle) wait until `resume_work`.
    pub(crate) fn suspend_work(&mut self) {
        self.suspended = true;
        if self.projection_task.is_some() && self.updating {
            self.cancel_projection();
            self.resume_pending = true;
        }
    }
    /// Show the table: the latest pending projection runs exactly once.
    pub(crate) fn resume_work(&mut self, cx: &mut Context<Self>) {
        self.suspended = false;
        if std::mem::take(&mut self.resume_pending) {
            self.refilter(cx);
        }
    }

    pub(super) fn cancel_projection(&mut self) {
        self.projection_cancel.store(true, Ordering::Relaxed);
        self.projection_task = None;
        self.projection_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.revision += 1;
    }

    pub(super) fn schedule_filter(&mut self, cx: &mut Context<Self>) {
        self.cancel_projection();
        self.updating = true;
        let revision = self.revision;
        self.projection_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(60))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.revision == revision {
                    this.refilter(cx);
                }
            });
        }));
        cx.notify();
    }

    pub(super) fn refilter(&mut self, cx: &mut Context<Self>) {
        self.cancel_projection();
        if self.suspended {
            self.resume_pending = true;
            return;
        }
        self.projection_all = self.all;
        let revision = self.revision;
        let Some(table) = self.table.clone() else {
            return;
        };
        let old_selection = self
            .anchor
            .zip(self.end)
            .and_then(|(a, b)| Some((self.source_cell(a)?, self.source_cell(b)?)));
        let (query, sort, start, sensitive) = (
            self.query.clone(),
            self.sort,
            usize::from(self.header),
            self.case_sensitive,
        );
        let count = if self.all { usize::MAX } else { INITIAL_ROWS };
        let cancel = self.projection_cancel.clone();
        self.updating = true;
        self.anchor = None;
        self.end = None;
        #[cfg(test)]
        {
            self.projections += 1;
        }
        let task = cx.background_executor().spawn(async move {
            okilum_core::delimited::project_rows(
                &table, start, count, &query, sort, sensitive, &cancel,
            )
        });
        self.projection_task = Some(cx.spawn(async move |this, cx| {
            let Some(projected) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.revision != revision {
                    return;
                }
                this.order = projected.rows;
                this.hits = projected
                    .matches
                    .into_iter()
                    .map(|(row, column)| Cell {
                        row: row + start,
                        column,
                    })
                    .collect();
                this.hit_index = 0;
                this.updating = false;
                if let Some(saved) = this.restored.take() {
                    this.hit_index = saved.hit_index.min(this.hits.len().saturating_sub(1));
                    this.anchor = saved.anchor.and_then(|c| this.display_cell(c));
                    this.end = saved.end.and_then(|c| this.display_cell(c));
                    if let Some(cell) = this.end {
                        this.reveal_cell(cell);
                    }
                    this.horizontal.set_offset(saved.horizontal);
                } else if let Some(source) = this.reveal_source.take() {
                    if let Some(cell) = this.display_cell(source) {
                        this.anchor = Some(cell);
                        this.end = Some(cell);
                        this.reveal_cell(cell);
                    }
                } else if let Some(cell) = this.hits.first().copied() {
                    this.anchor = Some(cell);
                    this.end = Some(cell);
                    this.reveal_cell(cell);
                } else if this.query.is_empty() {
                    if let Some((a, b)) = old_selection {
                        this.anchor = this.display_cell(a);
                        this.end = this.display_cell(b);
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(super) fn source_cell(&self, cell: Cell) -> Option<Cell> {
        let start = usize::from(self.header);
        let row = if self.header && cell.row == 0 {
            0
        } else {
            *self.order.get(cell.row.checked_sub(start)?)?
        };
        Some(Cell {
            row,
            column: cell.column,
        })
    }
    pub(super) fn display_cell(&self, cell: Cell) -> Option<Cell> {
        let row = if self.header && cell.row == 0 {
            0
        } else {
            self.order.iter().position(|&r| r == cell.row)? + usize::from(self.header)
        };
        Some(Cell {
            row,
            column: cell.column,
        })
    }
    pub(super) fn reveal_cell(&self, cell: Cell) {
        self.scroll.scroll_to_item(
            cell.row.saturating_sub(usize::from(self.header)),
            ScrollStrategy::Top,
        );
        let width = f32::from(self.pane_bounds.get().size.width).max(COLUMN_WIDTH);
        let left = GUTTER + cell.column as f32 * COLUMN_WIDTH;
        let right = left + COLUMN_WIDTH;
        let offset = -f32::from(self.horizontal.offset().x);
        let next = if left < offset {
            left
        } else if right > offset + width {
            right - width
        } else {
            offset
        };
        self.horizontal.set_offset(point(px(-next.max(0.)), px(0.)));
    }
    pub(super) fn step_hit(
        &mut self,
        direction: i32,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.updating || self.hits.is_empty() {
            return;
        }
        self.hit_index = (self.hit_index as i64 + i64::from(direction))
            .rem_euclid(self.hits.len() as i64) as usize;
        let cell = self.hits[self.hit_index];
        self.anchor = Some(cell);
        self.end = Some(cell);
        self.reveal_cell(cell);
        cx.notify();
    }

    pub(crate) fn find_target(
        &self,
        owner: Entity<Self>,
        live: bool,
        _cx: &App,
    ) -> reader_find_field::Target {
        let case = owner.clone();
        let step = owner.clone();
        let close = owner;
        reader_find_field::Target {
            input: self.filter.clone().unwrap(),
            focus_override: None,
            scope: "Find in loaded rows".into(),
            count: if live && !self.query.is_empty() {
                Some(if self.updating {
                    "Searching…".into()
                } else {
                    format!(
                        "{} of {}",
                        if self.hits.is_empty() {
                            0
                        } else {
                            self.hit_index + 1
                        },
                        self.hits.len()
                    )
                    .into()
                })
            } else {
                None
            },
            match_case: Some((
                self.case_sensitive,
                Rc::new(move |_, cx| {
                    case.update(cx, |this, cx| {
                        this.case_sensitive = !this.case_sensitive;
                        reader_ui_state::remember_find_case_sensitive(this.case_sensitive, cx);
                        this.refilter(cx);
                    });
                }),
            )),
            step: Some(Rc::new(move |direction, window, cx| {
                step.update(cx, |this, cx| this.step_hit(direction, window, cx))
            })),
            steps_visible: true,
            close: Rc::new(move |window, cx| close.read(cx).focus.clone().focus(window, cx)),
            large_file: self.file_bytes.is_some_and(|bytes| bytes > 4 * 1024 * 1024),
        }
    }
}
