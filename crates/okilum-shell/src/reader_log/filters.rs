//! Generic point-and-click Log filters. Pickers own their rendering and inputs:
//! drawing a popup never reads (and rebinds) the owning LogView entity.
use super::*;
use gpui_component::{
    checkbox::Checkbox,
    menu::{PopupMenu, PopupMenuItem},
    popover::Popover,
};
use okilum_core::log::chips::{
    self, ColumnCountProvider, ColumnCounts, CountSource, FilterChip, FilterExecution, FilterMode,
    FilterValue, LoadedRowCountProvider,
};

pub(super) struct FilterUi {
    pub chips: Vec<FilterChip>,
    pub advanced: Vec<Expr>,
    pub provider: Arc<dyn ColumnCountProvider>,
    pub server: bool,
    pub(super) navigating: bool,
    picker: Rc<std::cell::RefCell<Option<Entity<ValuePicker>>>>,
    subscription: Option<Subscription>,
    cell_menu: Option<CellMenu>,
}
impl Default for FilterUi {
    fn default() -> Self {
        Self {
            chips: vec![],
            advanced: vec![],
            provider: Arc::new(LoadedRowCountProvider),
            server: false,
            navigating: false,
            picker: Rc::default(),
            subscription: None,
            cell_menu: None,
        }
    }
}

/// The owning view releases both the popup and its subscription on dismissal.
/// Menu callbacks capture only weak view handles, so removing the view also
/// releases an open menu instead of retaining an element-state reference cycle.
struct CellMenu {
    menu: Entity<PopupMenu>,
    position: Point<Pixels>,
    _dismiss: Subscription,
}

struct ApplyChip {
    index: Option<usize>,
    chip: FilterChip,
}
pub(super) struct ValuePicker {
    index: Option<usize>,
    chip: FilterChip,
    counts: Option<Result<Arc<ColumnCounts>, String>>,
    visible: Arc<Vec<usize>>,
    _searching: Option<Task<()>>,
    search: Entity<InputState>,
    needle: String,
    _search: Subscription,
    _count: Option<Task<()>>,
    allow_server: bool,
    count_server: bool,
    provider: Arc<dyn ColumnCountProvider>,
    snapshots: Arc<Vec<(Snapshot, String)>>,
}
impl EventEmitter<ApplyChip> for ValuePicker {}
impl Render for ValuePicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.chip.mode;
        let enabled = self.chip.enabled;
        let execution = self.chip.execution;
        let count_server = self.count_server;
        let rows = uniform_list(
            "log-filter-values",
            self.visible.len(),
            cx.processor(|this, range: Range<usize>, _, cx| {
                let Some(Ok(counts)) = &this.counts else {
                    return vec![];
                };
                range
                    .filter_map(|position| {
                        let at = *this.visible.get(position)?;
                        let item = counts.values.get(at)?;
                        let value = item.value.clone();
                        let checked = this.chip.values.contains(&value);
                        Some(
                            h_flex()
                                .h(px(28.))
                                .gap_2()
                                .child(
                                    Checkbox::new(("log-filter-value", at))
                                        .debug_selector(move || format!("log-filter-value-{at}"))
                                        .checked(checked)
                                        .label(item.value.label())
                                        .on_click(cx.listener(
                                            move |this, checked: &bool, _, cx| {
                                                if *checked {
                                                    if !this.chip.values.contains(&value) {
                                                        this.chip.values.push(value.clone());
                                                    }
                                                } else {
                                                    this.chip.values.retain(|v| v != &value);
                                                }
                                                cx.notify();
                                            },
                                        )),
                                )
                                .child(
                                    div()
                                        .debug_selector(move || format!("log-filter-count-{at}"))
                                        .text_color(cx.theme().muted_foreground)
                                        .child(item.count.to_string()),
                                )
                                .into_any_element(),
                        )
                    })
                    .collect()
            }),
        )
        .h(px((self.visible.len().clamp(1, 8) * 28) as f32))
        .w_full();
        v_flex()
            .debug_selector(|| "log-value-picker".into())
            .w(px(360.))
            .max_w_full()
            .p_3()
            .gap_2()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.chip.field.to_string()),
            )
            .child(
                div()
                    .debug_selector(|| "log-count-scope".into())
                    .w_full()
                    .min_w_0()
                    .flex_shrink_0()
                    .whitespace_normal()
                    .text_sm()
                    .child(match &self.counts {
                        None => "Counting values…".into(),
                        Some(Ok(counts)) => counts.label.clone(),
                        Some(Err(error)) => format!("Could not count values: {error}"),
                    }),
            )
            .when(self.allow_server, |body| {
                body.child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("log-count-server")
                                .debug_selector(|| "log-count-server".into())
                                .small()
                                .label("Kibana counts")
                                .selected(count_server)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.count_server = true;
                                    this.reload_counts(cx);
                                })),
                        )
                        .child(
                            Button::new("log-count-loaded")
                                .debug_selector(|| "log-count-loaded".into())
                                .small()
                                .label("Loaded row counts")
                                .selected(!count_server)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.count_server = false;
                                    this.reload_counts(cx);
                                })),
                        ),
                )
            })
            .child(
                div()
                    .debug_selector(|| "log-value-search".into())
                    .child(Input::new(&self.search).small()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("log-mode-include")
                            .debug_selector(|| "log-mode-include".into())
                            .small()
                            .label("Include")
                            .selected(mode == FilterMode::Include)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.chip.mode = FilterMode::Include;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("log-mode-exclude")
                            .debug_selector(|| "log-mode-exclude".into())
                            .small()
                            .label("Exclude")
                            .selected(mode == FilterMode::Exclude)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.chip.mode = FilterMode::Exclude;
                                cx.notify();
                            })),
                    ),
            )
            .child(rows)
            .when(self.counts.is_some() && self.visible.is_empty(), |view| {
                view.child("No matching values")
            })
            .when(self.allow_server, |body| {
                body.child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("log-execution-server")
                                .debug_selector(|| "log-execution-server".into())
                                .small()
                                .label("Apply in Kibana")
                                .selected(execution == FilterExecution::Server)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.chip.execution = FilterExecution::Server;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("log-execution-local")
                                .debug_selector(|| "log-execution-local".into())
                                .small()
                                .label("Apply to loaded rows")
                                .selected(execution == FilterExecution::Local)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.chip.execution = FilterExecution::Local;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .child(
                Checkbox::new("log-chip-enabled")
                    .debug_selector(|| "log-chip-enabled".into())
                    .label("Enabled")
                    .checked(enabled)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.chip.enabled = *checked;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("log-filter-apply")
                    .debug_selector(|| "log-filter-apply".into())
                    .small()
                    .label("Apply filter")
                    .disabled(self.chip.values.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.emit(ApplyChip {
                            index: this.index,
                            chip: this.chip.clone(),
                        });
                    })),
            )
    }
}

impl ValuePicker {
    fn reload_counts(&mut self, cx: &mut Context<Self>) {
        self.counts = None;
        self.visible = Arc::default();
        self._searching = None;
        let field = self.chip.field.clone();
        let selections = self.chip.values.clone();
        let snapshots = self.snapshots.clone();
        let provider: Arc<dyn ColumnCountProvider> = if self.allow_server && !self.count_server {
            Arc::new(LoadedRowCountProvider)
        } else {
            self.provider.clone()
        };
        self._count = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let sources: Vec<_> = snapshots
                        .iter()
                        .map(|(snapshot, name)| {
                            let (index, bytes) = snapshot.parts();
                            CountSource { index, bytes, name }
                        })
                        .collect();
                    provider.count(&field, &sources).map(|mut counts| {
                        for value in selections
                            .into_iter()
                            .chain(std::iter::once(FilterValue::Missing))
                        {
                            if !counts.values.iter().any(|item| item.value == value) {
                                counts.values.push(chips::ValueCount { value, count: 0 });
                            }
                        }
                        Arc::new(counts)
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.counts = Some(result);
                this.search_values(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn search_values(&mut self, cx: &mut Context<Self>) {
        let Some(Ok(counts)) = &self.counts else {
            return;
        };
        let counts = counts.clone();
        let needle = self.needle.clone();
        self._searching = Some(cx.spawn(async move |this, cx| {
            let expected = needle.clone();
            let visible = cx
                .background_executor()
                .spawn(async move {
                    let needle = needle.to_lowercase();
                    counts
                        .values
                        .iter()
                        .enumerate()
                        .filter(|(_, item)| item.value.label().to_lowercase().contains(&needle))
                        .map(|(at, _)| at)
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.needle == expected {
                    this.visible = Arc::new(visible);
                    cx.notify();
                }
            });
        }));
    }
}

impl LogView {
    pub(crate) fn enable_server_filters(&mut self, cx: &mut Context<Self>) {
        if self.filter_ui.server {
            return;
        }
        self.filter_ui.server = true;
        let query = self.local_chip_query();
        self.update_filter(
            |filter| filter.query = (!query.is_empty()).then_some(query),
            cx,
        );
    }
    pub(super) fn local_chip_query(&self) -> Query {
        let chips = self
            .filter_ui
            .chips
            .iter()
            .filter(|chip| !self.filter_ui.server || chip.execution == FilterExecution::Local)
            .cloned()
            .collect::<Vec<_>>();
        chips::combined_query(&chips, &self.filter_ui.advanced)
    }
    pub(super) fn emit_chip_change(&self, cx: &mut Context<Self>) {
        if self.filter_ui.server && !self.filter_ui.navigating {
            cx.emit(FilterChipsChanged(self.filter_ui.chips.clone()));
        }
    }
    pub(crate) fn reset_remote_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let server = self.filter_ui.server;
        self.filter_ui.server = false;
        self.investigation = Default::default();
        self.clear_filters(window, cx);
        self.filter_ui.server = server;
    }
    /// Source adapters install their server-count adapter here.
    #[allow(dead_code)]
    pub(crate) fn set_count_provider(
        &mut self,
        provider: Arc<dyn ColumnCountProvider>,
        cx: &mut Context<Self>,
    ) {
        self.filter_ui.provider = provider;
        cx.notify();
    }
    pub(super) fn filter_for_source(&self, source: usize) -> LogFilter {
        let mut filter = self.investigation_filter();
        filter.query = filter
            .query
            .as_ref()
            .map(|q| q.for_source(&self.source_name(source)));
        filter
    }
    pub(super) fn sync_chips(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter_ui.server {
            for chip in &mut self.filter_ui.chips {
                if chip.execution == FilterExecution::Server {
                    chip.field = self.filter_ui.provider.canonical_field(&chip.field);
                }
            }
        }
        let query = chips::combined_query(&self.filter_ui.chips, &self.filter_ui.advanced);
        let text = query.to_string();
        self.query_input
            .update(cx, |input, cx| input.set_value(text.clone(), window, cx));
        self.query_text = text;
        self.query_error = None;
        let local = self.local_chip_query();
        self.update_filter(
            |filter| filter.query = (!local.is_empty()).then_some(local),
            cx,
        );
        self.emit_chip_change(cx);
        self.remember_query(cx);
        cx.notify();
    }
    pub(super) fn add_chip(
        &mut self,
        mut chip: FilterChip,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.filter_ui.server {
            chip.execution = FilterExecution::Server;
        }
        if !self.filter_ui.chips.contains(&chip) {
            self.filter_ui.chips.push(chip);
        }
        self.sync_chips(window, cx);
    }
    fn open_value_picker(
        &mut self,
        field: FieldRef,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let allow_server = self.filter_ui.server;
        let field = if allow_server {
            self.filter_ui.provider.canonical_field(&field)
        } else {
            field
        };
        let chip = index
            .and_then(|i| self.filter_ui.chips.get(i).cloned())
            .unwrap_or_else(|| {
                let mut chip = FilterChip::new(field.clone(), vec![], FilterMode::Include);
                if allow_server {
                    chip.execution = FilterExecution::Server;
                }
                chip
            });
        let snapshots: Vec<_> = self
            .enabled_sources()
            .into_iter()
            .filter_map(|s| Some((self.snapshot_of(s)?, self.source_name(s))))
            .collect();
        let provider = self.filter_ui.provider.clone();
        let picker = cx.new(|cx| {
            let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search values…"));
            let subscription = cx.subscribe(&search, |this: &mut ValuePicker, input, event, cx| {
                if matches!(event, InputEvent::Change) {
                    this.needle = input.read(cx).value().to_string();
                    this.search_values(cx);
                    cx.notify();
                }
            });
            let mut picker = ValuePicker {
                index,
                chip,
                counts: None,
                visible: Arc::default(),
                _searching: None,
                search,
                needle: String::new(),
                _search: subscription,
                _count: None,
                allow_server,
                count_server: allow_server,
                provider,
                snapshots: Arc::new(snapshots),
            };
            picker.reload_counts(cx);
            picker
        });
        self.filter_ui.subscription =
            Some(
                cx.subscribe_in(&picker, window, |this, _, event: &ApplyChip, window, cx| {
                    if let Some(index) = event.index {
                        if let Some(chip) = this.filter_ui.chips.get_mut(index) {
                            *chip = event.chip.clone();
                        }
                    } else if !this.filter_ui.chips.contains(&event.chip) {
                        this.filter_ui.chips.push(event.chip.clone());
                    }
                    this.sync_chips(window, cx);
                }),
            );
        *self.filter_ui.picker.borrow_mut() = Some(picker);
        cx.notify();
    }
    pub(super) fn filter_popover(
        &self,
        id: SharedString,
        label: String,
        field: FieldRef,
        index: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let owner = cx.entity().downgrade();
        let selector = id.to_string();
        let picker = self.filter_ui.picker.clone();
        Popover::new(id.clone())
            .trigger(
                Button::new(id)
                    .debug_selector(move || selector.clone())
                    .ghost()
                    .xsmall()
                    .max_w_full()
                    .truncate()
                    .label(label)
                    .tooltip("Filter column values"),
            )
            .on_open_change(move |open, window, cx| {
                if *open {
                    let _ = owner.update(cx, |this, cx| {
                        this.open_value_picker(field.clone(), index, window, cx)
                    });
                }
            })
            .content(move |_, _, _| div().children(picker.borrow().clone()))
            .into_any_element()
    }
    pub(super) fn render_chip_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.filter_ui.chips.is_empty() {
            return None;
        }
        Some(
            h_flex()
                .debug_selector(|| "log-chip-bar".into())
                .min_w_0()
                .overflow_x_scrollbar()
                .gap_2()
                .children(self.filter_ui.chips.iter().enumerate().map(|(at, chip)| {
                    h_flex()
                        .gap_1()
                        .when(!chip.enabled, |chip| chip.opacity(0.5))
                        .when(self.filter_ui.server, |row| {
                            row.child(
                                Button::new(("log-chip-execution", at))
                                    .debug_selector(move || format!("log-chip-execution-{at}"))
                                    .small()
                                    .label(if chip.execution == FilterExecution::Server {
                                        "Kibana"
                                    } else {
                                        "Loaded rows"
                                    })
                                    .selected(chip.execution == FilterExecution::Server)
                                    .tooltip("Switch where this chip runs")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if let Some(chip) = this.filter_ui.chips.get_mut(at) {
                                            chip.execution =
                                                if chip.execution == FilterExecution::Server {
                                                    FilterExecution::Local
                                                } else {
                                                    FilterExecution::Server
                                                };
                                            this.sync_chips(window, cx);
                                        }
                                    })),
                            )
                        })
                        .child(self.filter_popover(
                            format!("log-chip-{at}").into(),
                            chip.label(),
                            chip.field.clone(),
                            Some(at),
                            cx,
                        ))
                        .child(
                            Button::new(("log-chip-remove", at))
                                .debug_selector(move || format!("log-chip-remove-{at}"))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .tooltip("Remove filter")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.filter_ui.chips.remove(at);
                                    this.sync_chips(window, cx);
                                })),
                        )
                }))
                .into_any_element(),
        )
    }
    fn cell_menu(
        menu: PopupMenu,
        owner: WeakEntity<Self>,
        target: (FieldRef, Option<String>),
        value: Arc<FilterValue>,
        row: (usize, bool, bool),
        request: Option<correlation::RequestId>,
    ) -> PopupMenu {
        let (field, column) = target;
        let include_owner = owner.clone();
        let include_field = field.clone();
        let include_value = value.clone();
        let exclude_owner = owner.clone();
        let exclude_field = field.clone();
        let exclude_value = value.clone();
        let text = value.text().to_string();
        let mut menu = menu
            .item(
                PopupMenuItem::new("Include this value").on_click(move |_, window, cx| {
                    let _ = include_owner.update(cx, |this, cx| {
                        this.add_chip(
                            FilterChip::new(
                                include_field.clone(),
                                vec![include_value.as_ref().clone()],
                                FilterMode::Include,
                            ),
                            window,
                            cx,
                        )
                    });
                }),
            )
            .item(
                PopupMenuItem::new("Exclude this value").on_click(move |_, window, cx| {
                    let _ = exclude_owner.update(cx, |this, cx| {
                        this.add_chip(
                            FilterChip::new(
                                exclude_field.clone(),
                                vec![exclude_value.as_ref().clone()],
                                FilterMode::Exclude,
                            ),
                            window,
                            cx,
                        )
                    });
                }),
            )
            .item(PopupMenuItem::new("Copy value").on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
            }));
        if let Some(key) = column {
            menu = Self::field_actions(
                menu,
                owner.clone(),
                key.clone(),
                value.as_ref().clone(),
                request,
            );
            let column_owner = owner.clone();
            menu = menu.item(
                PopupMenuItem::new("Show as a column").on_click(move |_, _, cx| {
                    let _ = column_owner.update(cx, |this, cx| {
                        if !this.columns(cx).contains(&key) {
                            this.toggle_column(&key, cx);
                        }
                    });
                }),
            );
        }
        let (index, marked, any_marks) = row;
        let item =
            |label: &str, run: fn(&mut LogView, usize, &mut Window, &mut Context<LogView>)| {
                let owner = owner.clone();
                PopupMenuItem::new(label.to_owned()).on_click(move |_, window, cx| {
                    let _ = owner.update(cx, |this, cx| run(this, index, window, cx));
                })
            };
        menu.separator()
            .item(item(
                if marked { "Remove mark" } else { "Mark row" },
                |this, index, _, cx| this.toggle_mark_at(index, cx),
            ))
            .item(item("Next mark", |this, index, window, cx| {
                this.step_mark_from(index, true, window, cx)
            }))
            .item(item("Previous mark", |this, index, window, cx| {
                this.step_mark_from(index, false, window, cx)
            }))
            .item(
                item("Clear all marks", |this, _, _, cx| this.clear_marks(cx)).disabled(!any_marks),
            )
    }
    fn open_cell_menu(
        &mut self,
        target: (FieldRef, Option<String>),
        value: Arc<FilterValue>,
        row: (usize, bool, bool),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let request = self
            .locate(row.0)
            .and_then(|(source, index)| self.source(source)?.record(index))
            .and_then(|record| {
                correlation::ids_with_field(&record, self.field_choices(cx).correlation.as_deref())
                    .into_iter()
                    .find(|id| Some(&id.key) == target.1.as_ref())
            });
        let owner = cx.entity().downgrade();
        let previous_focus = window.focused(cx);
        let menu = PopupMenu::build(window, cx, move |menu, _, _| {
            Self::cell_menu(menu, owner, target, value, row, request)
        });
        let menu_focus = menu.focus_handle(cx);
        menu_focus.focus(window, cx);
        let dismiss = cx.subscribe_in(
            &menu,
            window,
            move |this, _, _: &DismissEvent, window, cx| {
                this.filter_ui.cell_menu = None;
                if menu_focus.contains_focused(window, cx) {
                    if let Some(previous) = &previous_focus {
                        previous.focus(window, cx);
                    }
                }
                cx.notify();
            },
        );
        self.filter_ui.cell_menu = Some(CellMenu {
            menu,
            position: window.mouse_position(),
            _dismiss: dismiss,
        });
        cx.notify();
    }

    pub(super) fn render_cell_menu(&self) -> Option<AnyElement> {
        let menu = self.filter_ui.cell_menu.as_ref()?;
        Some(
            deferred(
                anchored()
                    .position(menu.position)
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .debug_selector(|| "log-cell-menu".into())
                            .occlude()
                            .child(menu.menu.clone()),
                    ),
            )
            .with_priority(gpui_base::POPUP_PRIORITY)
            .into_any_element(),
        )
    }

    #[cfg(test)]
    pub(super) fn cell_menu_handle(&self) -> Option<WeakEntity<PopupMenu>> {
        self.filter_ui
            .cell_menu
            .as_ref()
            .map(|menu| menu.menu.downgrade())
    }

    /// A visible small menu button makes right-click actions discoverable. Both
    /// paths share the same action builder and typed value captured from the row.
    pub(super) fn filterable_cell(
        &self,
        content: AnyElement,
        selector: String,
        target: (FieldRef, Option<String>),
        value: FilterValue,
        index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let marked = self
            .locate(index)
            .is_some_and(|entry| self.marks.contains(&entry));
        let row = (index, marked, !self.marks.is_empty());
        let value = Arc::new(value);
        let id = SharedString::from(format!("{selector}-actions"));
        let button_selector = id.to_string();
        let button_target = target.clone();
        let button_value = value.clone();
        h_flex()
            .id(id.clone())
            .group(id.clone())
            .relative()
            .debug_selector(move || selector.clone())
            .gap_1()
            .min_w_0()
            .w_full()
            .h(px(ROW_HEIGHT))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.open_cell_menu(target.clone(), value.clone(), row, window, cx);
                }),
            )
            .child(div().flex_1().min_w_0().truncate().child(content))
            .child(
                Button::new(id.clone())
                    .debug_selector(move || button_selector.clone())
                    .ghost()
                    .xsmall()
                    .absolute()
                    .top_0()
                    .right_0()
                    .opacity(0.)
                    .group_hover(id.clone(), |style| style.opacity(1.))
                    .focus(|style| style.opacity(1.))
                    .icon(IconName::Ellipsis)
                    .tooltip("Cell actions: include, exclude or copy value")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_cell_menu(
                            button_target.clone(),
                            button_value.clone(),
                            row,
                            window,
                            cx,
                        );
                    })),
            )
            .into_any_element()
    }
}

/// Ephemeral state handed to a replacement view only for the same source.
#[derive(Clone)]
pub(crate) struct FilterViewState {
    filter: LogFilter,
    chips: Vec<FilterChip>,
    advanced: Vec<Expr>,
    query_text: String,
    query_error: Option<(usize, String)>,
}
impl LogView {
    pub(crate) fn filter_view_state(&self) -> FilterViewState {
        FilterViewState {
            filter: self.filter.clone(),
            chips: self.filter_ui.chips.clone(),
            advanced: self.filter_ui.advanced.clone(),
            query_text: self.query_text.clone(),
            query_error: self.query_error.clone(),
        }
    }
    pub(crate) fn restore_filter_view(
        &mut self,
        state: FilterViewState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.filter_ui.chips = state.chips;
        self.filter_ui.advanced = state.advanced;
        self.query_text = state.query_text;
        self.query_error = state.query_error;
        self.text_input.update(cx, |input, cx| {
            input.set_value(state.filter.text.clone(), window, cx)
        });
        self.query_input.update(cx, |input, cx| {
            input.set_value(self.query_text.clone(), window, cx)
        });
        self.filter = state.filter;
        if self.filter_ui.server {
            let local = self.local_chip_query();
            self.filter.query = (!local.is_empty()).then_some(local);
            self.emit_chip_change(cx);
        }
        self.refilter(cx);
        cx.notify();
    }
}

impl LogView {
    /// Source adapters can inspect the typed chip contract without reading UI
    /// controls. An enabled remote adapter owns Server execution; those chips
    /// are excluded from the local matcher.
    #[allow(dead_code)]
    pub(crate) fn filter_chips(&self) -> &[FilterChip] {
        &self.filter_ui.chips
    }
}

#[derive(Clone)]
pub(crate) struct FilterChipsChanged(pub Vec<FilterChip>);
impl EventEmitter<FilterChipsChanged> for LogView {}

#[cfg(test)]
impl LogView {
    pub(super) fn filter_picker_for_test(&self) -> Option<Entity<ValuePicker>> {
        self.filter_ui.picker.borrow().clone()
    }
}
#[cfg(test)]
impl ValuePicker {
    pub(super) fn counts_for_test(&self) -> Option<Result<Arc<ColumnCounts>, String>> {
        self.counts.clone()
    }
}
