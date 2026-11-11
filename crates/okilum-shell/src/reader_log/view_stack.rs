//! Investigation navigation, source-scoped field choices and saved-view context.
use super::*;
use okilum_core::log::saved_query::{FieldChoices, RemoteGuided};
use okilum_core::remote_log::live::Loaded;

#[derive(Clone)]
pub(crate) enum Navigation {
    Follow(correlation::RequestId),
    Back(Arc<Loaded>, Option<Box<RemoteGuided>>),
    Saved(Box<SavedQuery>),
}
impl EventEmitter<Navigation> for LogView {}

#[derive(Default)]
pub(crate) struct Investigation {
    pub(super) frames: Vec<ViewFrame>,
    pub request: Option<correlation::RequestId>,
    pub remote: Option<Arc<Loaded>>,
    pub guided: Option<RemoteGuided>,
    pub live: bool,
    pub last_update: Option<String>,
    restore: Option<Viewport>,
    fields: std::cell::RefCell<Option<(u64, Vec<String>)>>,
}
pub(super) struct ViewFrame {
    filters: filters::FilterViewState,
    columns: Vec<String>,
    widths: Vec<(String, f32)>,
    fields: FieldChoices,
    viewport: Viewport,
    request: Option<correlation::RequestId>,
    remote: Option<Arc<Loaded>>,
    guided: Option<RemoteGuided>,
}
struct Viewport {
    selected: Option<usize>,
    anchor: Option<usize>,
    offset: Point<Pixels>,
    pinned: bool,
}
impl LogView {
    pub(super) fn field_choices(&self, cx: &App) -> FieldChoices {
        self.history_key()
            .map(|key| reader_ui_state::log_fields(&key, cx))
            .unwrap_or_default()
    }
    pub(crate) fn choose_field(
        &mut self,
        key: Option<String>,
        message: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(source) = self.history_key() else {
            return;
        };
        let mut fields = self.field_choices(cx);
        if message {
            fields.message = key;
        } else {
            fields.correlation = key;
        }
        reader_ui_state::set_log_fields(&source, fields, cx);
        cx.notify();
    }
    pub(super) fn field_actions(
        mut menu: gpui_component::menu::PopupMenu,
        owner: WeakEntity<Self>,
        key: String,
        value: FilterValue,
        request: Option<correlation::RequestId>,
    ) -> gpui_component::menu::PopupMenu {
        use gpui_component::menu::PopupMenuItem;
        if let Some(request) = request {
            let owner = owner.clone();
            menu = menu.item(PopupMenuItem::new("Follow this request").on_click(
                move |_, window, cx| {
                    let _ = owner.update(cx, |this, cx| {
                        this.follow_request(request.clone(), window, cx)
                    });
                },
            ));
        }
        if matches!(
            value,
            FilterValue::Value {
                kind: ValueKind::String,
                ..
            }
        ) {
            let message_owner = owner.clone();
            let message_key = key.clone();
            menu = menu.item(PopupMenuItem::new("Use as message field").on_click(
                move |_, _, cx| {
                    let _ = message_owner.update(cx, |this, cx| {
                        this.choose_field(Some(message_key.clone()), true, cx)
                    });
                },
            ));
        }
        menu.item(
            PopupMenuItem::new("Use as correlation field").on_click(move |_, _, cx| {
                let _ = owner.update(cx, |this, cx| {
                    this.choose_field(Some(key.clone()), false, cx)
                });
            }),
        )
    }
    pub(crate) fn message_field(&self, cx: &App) -> Option<String> {
        self.field_choices(cx).message.or_else(|| {
            self.string_fields()
                .into_iter()
                .find(|key| key == "message")
        })
    }
    fn string_fields(&self) -> Vec<String> {
        if let Some((revision, fields)) = self.investigation.fields.borrow().as_ref() {
            if *revision == self.window_revision {
                return fields.clone();
            }
        }
        let mut fields = std::collections::BTreeSet::new();
        if let State::Remote(data) = &self.state {
            fields.extend(
                data.fields
                    .iter()
                    .filter(|field| {
                        field
                            .types
                            .contains(&okilum_core::remote_log::result::FieldType::String)
                    })
                    .map(|field| field.path.clone()),
            );
        }
        for source in self.enabled_sources() {
            let Some(data) = self.source(source) else {
                continue;
            };
            for index in 0..data.index().len().min(COLUMN_MENU_SAMPLE) {
                if let Some(record) = data.record(index) {
                    fields.extend(
                        record
                            .fields
                            .into_iter()
                            .filter(|field| {
                                field.kind == ValueKind::String && !field.key.starts_with("_event.")
                            })
                            .map(|field| field.key),
                    );
                }
            }
        }

        let fields: Vec<_> = fields.into_iter().collect();
        *self.investigation.fields.borrow_mut() = Some((self.window_revision, fields.clone()));
        fields
    }
    pub(super) fn request_id(&self, record: &Record, cx: &App) -> Option<correlation::RequestId> {
        correlation::ids_with_field(record, self.field_choices(cx).correlation.as_deref())
            .into_iter()
            .next()
    }
    pub(crate) fn follow_request(
        &mut self,
        request: correlation::RequestId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let columns = self.columns(cx);
        self.investigation.frames.push(ViewFrame {
            filters: self.filter_view_state(),
            widths: columns
                .iter()
                .map(|key| (key.clone(), self.width_of(key, cx)))
                .collect(),
            columns,
            fields: self.field_choices(cx),
            viewport: Viewport {
                selected: self.selected,
                anchor: self.anchor,
                offset: self.scroll.0.borrow().base_handle.offset(),
                pinned: self.follow.as_ref().is_some_and(|follow| follow.pinned),
            },
            request: self.investigation.request.clone(),
            remote: self.investigation.remote.clone(),
            guided: self.investigation.guided.clone(),
        });
        self.investigation.request = Some(request.clone());
        self.selected = None;
        self.anchor = None;
        if let Some(follow) = &mut self.follow {
            follow.pinned = false;
        }
        // A request spans levels and messages; the parent filters belong to Back.
        let time = self.filter.time;
        self.filter = LogFilter {
            time,
            ..Default::default()
        };
        self.filter_ui.chips.clear();
        self.filter_ui.advanced.clear();
        self.text_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.filter_ui.navigating = true;
        self.replace_query(
            if matches!(self.state, State::Remote(_)) {
                String::new()
            } else {
                correlation::query_with_field(&request.value, Some(&request.key)).to_string()
            },
            window,
            cx,
        );
        self.filter_ui.navigating = false;
        if matches!(self.state, State::Remote(_)) {
            cx.emit(Navigation::Follow(request));
        }
        cx.notify();
    }
    pub(super) fn request_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(frame) = self.investigation.frames.pop() else {
            return;
        };
        self.investigation.request = frame.request;
        self.investigation.guided = frame.guided;
        if let Some(remote) = frame.remote {
            self.state = State::Remote(remote.window.clone());
            self.investigation.remote = Some(remote.clone());
            self.investigation.live = false;
            cx.emit(Navigation::Back(
                remote,
                self.investigation.guided.clone().map(Box::new),
            ));
        }
        if let Some(key) = self.history_key() {
            reader_ui_state::set_log_columns(&key, frame.columns, cx);
            reader_ui_state::set_log_fields(&key, frame.fields, cx);
            for (column, width) in frame.widths {
                reader_ui_state::set_log_column_width(&key, &column, width, cx);
            }
        }
        // Cancel a pending child filter before restoring the parent viewport.
        self.generations.next();
        self._filtering = None;
        self.filtering = false;
        self.filter_running = None;
        self.filter_again = false;
        self.investigation.restore = Some(frame.viewport);
        self.filter_ui.navigating = true;
        self.restore_filter_view(frame.filters, window, cx);
        self.filter_ui.navigating = false;
        self.focus.focus(window, cx);
    }
    pub(super) fn restore_viewport(&mut self) {
        if self.filtering {
            return;
        }
        let Some(viewport) = self.investigation.restore.take() else {
            return;
        };
        self.selected = viewport
            .selected
            .filter(|&index| self.row_of(index).is_some());
        self.anchor = viewport
            .anchor
            .filter(|&index| self.row_of(index).is_some());
        if let Some(follow) = &mut self.follow {
            follow.pinned = viewport.pinned;
        }
        let mut scroll = self.scroll.0.borrow_mut();
        scroll.deferred_scroll_to_item = None;
        scroll.base_handle.set_offset(viewport.offset);
    }
    /// The drill-down's identity is an independent scope, including when its
    /// editable query is cleared or replaced while live is running.
    pub(super) fn investigation_filter(&self) -> LogFilter {
        let mut filter = self.filter.clone();
        if let Some(request) = &self.investigation.request {
            let confirmed_server_scope = self.investigation.remote.as_ref().is_some_and(|remote| {
                remote.source.discover.filters.iter().any(|filter| {
                    filter.get("query")
                        == Some(&correlation::server_query(&request.value, &request.key))
                })
            });
            if confirmed_server_scope {
                return filter;
            }
            let scope = correlation::query_with_field(&request.value, Some(&request.key));
            let mut terms: Vec<_> = filter
                .query
                .iter()
                .map(|query| query.expr().clone())
                .collect();
            let present = terms.iter().any(|expr| {
                expr == &scope || matches!(expr, Expr::And(items) if items.contains(&scope))
            });
            if !present {
                terms.push(scope);
            }
            filter.query = Some(chips::combined_query(&[], &terms));
        }
        filter
    }
    pub(super) fn sort_request_rows(&mut self) {
        if self.investigation.request.is_none() {
            return;
        }
        let mut rows = self.rows.as_ref().map_or_else(
            || (0..self.total() as u32).collect(),
            |rows| rows.as_ref().clone(),
        );
        rows.sort_by_key(|&index| {
            self.locate(index as usize)
                .and_then(|(source, index)| self.source(source)?.index().get(index))
                .and_then(|entry| entry.timestamp())
                .unwrap_or(i64::MAX)
        });
        self.rows = Some(Arc::new(rows));
    }
    pub(crate) fn remote_context(
        &mut self,
        loaded: Arc<Loaded>,
        live: bool,
        updated: bool,
        cx: &mut Context<Self>,
    ) {
        self.investigation.remote = Some(loaded);
        self.investigation.live = live;
        if updated {
            self.investigation.last_update = Some(timestamp::clock_utc(super::unix_now()));
        }
        cx.notify();
    }
    pub(crate) fn investigation_label(&self) -> Option<String> {
        let request = self.investigation.request.as_ref()?;
        let count = self.visible_len();
        let id = short_id(&request.value);
        if self.filtering {
            return Some(format!("Filtering loaded rows for id {id}…"));
        }
        Some(match &self.investigation.remote {
            Some(loaded)
                if !loaded.source.discover.filters.iter().any(|filter| {
                    filter.get("query")
                        == Some(&correlation::server_query(&request.value, &request.key))
                }) =>
            {
                format!("Loaded rows: {count} events for id {id} · Kibana request has not loaded; see connection status")
            }
            Some(loaded) => format!(
                "Kibana: {count} events for id {} in {} → {} · {}",
                id,
                loaded.range.from,
                loaded.range.to,
                loaded.quality.label(loaded.window.events.len())
            ),
            None => format!("Loaded rows: {count} events for id {id}"),
        })
    }
    pub(super) fn render_investigation(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let label = self.investigation_label()?;
        let breadcrumb = std::iter::once("All rows".to_owned())
            .chain(
                self.investigation
                    .frames
                    .iter()
                    .filter_map(|frame| frame.request.as_ref())
                    .map(|id| format!("request {}", short_id(&id.value))),
            )
            .chain(
                self.investigation
                    .request
                    .iter()
                    .map(|id| format!("request {}", short_id(&id.value))),
            )
            .collect::<Vec<_>>()
            .join(" › ");
        Some(
            h_flex()
                .flex_none()
                .min_w_0()
                .max_w(px(240.))
                .gap_2()
                .text_sm()
                .child(
                    div()
                        .debug_selector(|| "log-breadcrumb".into())
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(breadcrumb),
                )
                .child(
                    div()
                        .debug_selector(|| "log-request-count".into())
                        .max_w(px(120.))
                        .min_w_0()
                        .truncate()
                        .child(label),
                )
                .text_color(cx.theme().muted_foreground)
                .into_any_element(),
        )
    }
    pub(super) fn restore_saved_source(
        &mut self,
        saved: &SavedQuery,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if saved.remote_source.is_some() && !matches!(self.state, State::Remote(_)) {
            reader_toast::error(
                "Open saved Kibana views from the Kibana log window",
                window,
                cx,
            );
            return false;
        }
        let Some(path) = saved
            .file
            .as_ref()
            .filter(|path| self.history_key().as_ref() != Some(path))
        else {
            return true;
        };
        if self.source_locked || matches!(self.state, State::Remote(_) | State::Stream(_)) {
            reader_toast::error("Open saved file views from a local Log view", window, cx);
            return false;
        }
        self._follow_open = None;
        let path = PathBuf::from(path);
        let name = saved.name.clone();
        self._indexing = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    LogFile::open(&path).map(|file| LogDocument {
                        rel: path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                        file: Arc::new(file),
                        elapsed: Duration::ZERO,
                    })
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(document) => {
                    this.follow = None;
                    this.extra.clear();
                    this.merge = None;
                    this.primary_enabled = true;
                    this.state = State::Ready(document);
                    this.selected = None;
                    this.anchor = None;
                    this.marks.clear();
                    this.rows = None;
                    this.generations.next();
                    this._filtering = None;
                    this.filtering = false;
                    this.filter_running = None;
                    this.filter_again = false;
                    this.apply_saved(&name, window, cx);
                    this.refilter(cx);
                }
                Err(error) => {
                    reader_toast::error(format!("Cannot open saved source: {error:#}"), window, cx)
                }
            });
        }));
        false
    }
    pub(super) fn capture_investigation(&self, query: &mut SavedQuery, cx: &App) {
        let fields = self.field_choices(cx);
        query.message_field = fields.message;
        query.correlation_field = fields.correlation;
        query.request = self.investigation.request.clone();
        if let Some(remote) = &self.investigation.remote {
            query.remote_source = Some(remote.source.clone());
            query.remote_range = Some(remote.range.clone());
            query.remote_guided = self.investigation.guided.clone();
        } else if !self.source_locked && !matches!(self.state, State::Remote(_) | State::Stream(_))
        {
            query.file = self.history_key();
        }
    }
    pub(super) fn restore_investigation(&mut self, saved: &SavedQuery, cx: &mut Context<Self>) {
        self.investigation.frames.clear();
        self.investigation.request = saved.request.clone();
        let fields_key = saved
            .remote_source
            .as_ref()
            .map(|source| format!("kibana-{}.jsonl", source.id))
            .or_else(|| self.history_key());
        if let Some(key) = fields_key {
            reader_ui_state::set_log_fields(
                &key,
                FieldChoices {
                    message: saved.message_field.clone(),
                    correlation: saved.correlation_field.clone(),
                },
                cx,
            );
        }
        if saved.remote_source.is_some() {
            cx.emit(Navigation::Saved(Box::new(saved.clone())));
        }
    }
}

/// Breadcrumbs stay bounded while queries, copy and saved state keep the full id.
fn short_id(value: &str) -> String {
    let mut chars = value.chars();
    let mut shown: String = chars
        .by_ref()
        .take(64)
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    if chars.next().is_some() {
        shown.push('…');
    }
    shown
}
