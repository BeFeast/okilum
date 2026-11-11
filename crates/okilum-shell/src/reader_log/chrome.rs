//! Log presentation shared by Reader and standalone Log hosts.
use super::*;
use gpui_component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use reader_viewer_chrome::{BuildMenu, ViewerAction, ViewerChrome};

impl LogView {
    fn saved_menu(
        &self,
        view: WeakEntity<Self>,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let names: Vec<String> = reader_ui_state::saved_log_filters(cx)
            .sorted()
            .iter()
            .map(|query| query.name.clone())
            .collect();
        let can_save = self.data().is_some();

        let mut menu = menu;
        for name in &names {
            let (view, name) = (view.clone(), name.clone());
            menu = menu.item(
                PopupMenuItem::new(name.clone()).on_click(move |_, window, cx| {
                    let _ = view.update(cx, |this, cx| this.apply_saved(&name, window, cx));
                }),
            );
        }
        if names.is_empty() {
            menu = menu.item(PopupMenuItem::new("No saved views yet").disabled(true));
        }
        let save_view = view.clone();
        menu = menu.separator().item(
            PopupMenuItem::new("Save current view…")
                .disabled(!can_save)
                .on_click(move |_, window, cx| {
                    let _ = save_view.update(cx, |this, cx| this.begin_saving(window, cx));
                }),
        );
        if !names.is_empty() {
            let (view, names) = (view.clone(), names.clone());
            menu = menu.submenu("Delete", window, cx, move |menu, _, _| {
                let mut menu = menu;
                for name in &names {
                    let (view, name) = (view.clone(), name.clone());
                    menu = menu.item(PopupMenuItem::new(name.clone()).on_click(
                        move |_, window, cx| {
                            reader_ui_state::delete_log_filter(&name, cx);
                            reader_toast::transient(format!("Deleted “{name}”"), window, cx);
                            let _ = view.update(cx, |_, cx| cx.notify());
                        },
                    ));
                }
                menu
            });
        }
        menu
    }
}

impl LogView {
    fn export_menu(
        &self,
        view: WeakEntity<Self>,
        menu: PopupMenu,
        _window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let shown = self.visible_len();
        let selected = self.selection().map_or(0, |rows| rows.len());
        let marked = self.marked_rows().len();
        let csv = ExportFormat::Csv(self.csv_columns(cx));
        let raw_values = self.csv_raw_values;

        let mut menu = menu;
        let mut scopes = vec![(ExportScope::Shown, shown, "shown")];
        if selected > 1 {
            scopes.push((ExportScope::Selected, selected, "selected"));
        }
        if marked > 0 {
            scopes.push((ExportScope::Marked, marked, "marked"));
        }
        for (at, (scope, count, what)) in scopes.into_iter().enumerate() {
            if at > 0 {
                menu = menu.separator();
            }
            for (format, how) in [
                (ExportFormat::Raw, "raw lines"),
                (ExportFormat::JsonLines, "JSON lines"),
                (csv.clone(), "CSV"),
            ] {
                let view = view.clone();
                menu = menu.item(
                    PopupMenuItem::new(format!(
                        "Export {} {what} as {how}…",
                        grouped(count as u64)
                    ))
                    .disabled(count == 0)
                    .on_click(move |_, window, cx| {
                        let format = format.clone();
                        let _ =
                            view.update(cx, |this, cx| this.export_rows(scope, format, window, cx));
                    }),
                );
            }
        }
        // A spreadsheet runs a cell that starts with = + - @ as a
        // formula; CSV cells get an apostrophe there unless this is on.
        let view = view.clone();
        menu.separator().item(
            PopupMenuItem::new("Raw values in CSV")
                .checked(raw_values)
                .on_click(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| this.set_csv_raw_values(!raw_values, cx));
                }),
        )
    }
}

impl LogView {
    fn files_menu(
        &self,
        view: WeakEntity<Self>,
        menu: PopupMenu,
        _window: &mut Window,
        _cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let sources: Vec<(usize, String, bool)> = (0..self.source_count())
            .map(|source| {
                (
                    source,
                    self.source_name(source),
                    self.source_enabled(source),
                )
            })
            .collect();
        let several = sources.len() > 1;
        let hidden = sources.iter().any(|(_, _, enabled)| !enabled);

        let mut menu = menu;
        for (source, name, enabled) in &sources {
            let (view, source, enabled) = (view.clone(), *source, *enabled);
            menu = menu.item(PopupMenuItem::new(name.clone()).checked(enabled).on_click(
                move |_, _, cx| {
                    let _ =
                        view.update(cx, |this, cx| this.set_source_enabled(source, !enabled, cx));
                },
            ));
        }
        let view = view.clone();
        menu.separator()
            .item(PopupMenuItem::new("Add file…").on_click(move |_, _, cx| {
                let _ = view.update(cx, |this, cx| this.prompt_add_files(cx));
            }))
    }
}

impl LogView {
    fn columns_menu(
        &self,
        view: WeakEntity<Self>,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let shown = self.columns(cx);

        let names = self.field_names();
        let mut menu = menu;
        for name in names {
            let (view, checked) = (view.clone(), shown.contains(&name));
            menu = menu.item(PopupMenuItem::new(name.clone()).checked(checked).on_click(
                move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| this.toggle_column(&name, cx));
                },
            ));
        }
        let fields = self.string_fields();
        let selected = self.message_field(cx);
        let owner = view.clone();
        let label = selected.map_or_else(
            || "Choose message field".to_owned(),
            |key| format!("Message: {key}"),
        );
        menu.separator()
            .submenu(label, window, cx, move |mut menu, _, _| {
                let clear = owner.clone();
                menu = menu.item(PopupMenuItem::new("Default message field").on_click(
                    move |_, _, cx| {
                        let _ = clear.update(cx, |this, cx| this.choose_field(None, true, cx));
                    },
                ));
                for field in &fields {
                    let (owner, field) = (owner.clone(), field.clone());
                    menu =
                        menu.item(PopupMenuItem::new(field.clone()).on_click(move |_, _, cx| {
                            let _ = owner.update(cx, |this, cx| {
                                this.choose_field(Some(field.clone()), true, cx)
                            });
                        }));
                }
                menu
            })
    }
}

impl LogView {
    fn render_query_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let danger = brand::palette(cx).danger;
        let view = cx.entity().downgrade();
        let error = self.query_error.clone();
        let query = div()
            .flex_1()
            .min_w(px(QUERY_MIN_WIDTH))
            .h(px(BAR_CONTROL_HEIGHT))
            .flex()
            .items_center()
            .rounded(px(6.))
            .bg(palette.surface_raised)
            .debug_selector(|| "log-query".into())
            .child(
                Input::new(&self.query_input)
                    .appearance(false)
                    .cleanable(true)
                    .small()
                    .when_some(error, |input, (column, message)| {
                        // The error sits inside the field, so the bar keeps
                        // its height; the rows keep the last valid query.
                        input.suffix(
                            div()
                                .id("log-query-error")
                                .debug_selector(|| "log-query-error".into())
                                .max_w(px(QUERY_ERROR_MAX_WIDTH))
                                .truncate()
                                .text_size(px(12.))
                                .text_color(danger)
                                .child(format!("Column {column}: {message}"))
                                .tooltip(move |window, cx| {
                                    gpui_component::tooltip::Tooltip::new(format!(
                                        "Column {column}: {message}"
                                    ))
                                    .build(window, cx)
                                }),
                        )
                    }),
            );
        let history = self
            .history_key()
            .map(|key| reader_ui_state::log_queries(&key, cx))
            .filter(|queries| !queries.is_empty())
            .map(|queries| {
                let view = view.clone();
                bar_button("log-query-history", false)
                    .icon(Icon::default().path(brand::READER_CLOCK_ICON))
                    .tooltip("Recent queries for this log")
                    .debug_selector(|| "log-query-history".into())
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                        let mut menu = menu;
                        for query in &queries {
                            let view = view.clone();
                            let text = query.clone();
                            menu = menu.item(PopupMenuItem::new(query.clone()).on_click(
                                move |_, window, cx| {
                                    let text = text.clone();
                                    let _ = view.update(cx, |this, cx| {
                                        this.replace_query(text, window, cx);
                                        this.remember_query(cx);
                                    });
                                },
                            ));
                        }
                        menu
                    })
            });

        h_flex()
            .flex_1()
            .min_w_0()
            .gap_1()
            .child(query)
            .children(history)
            .when(
                !self.source_locked && !matches!(&self.state, State::Remote(_) | State::Stream(_)),
                |row| row.child(self.regex_button(cx)),
            )
            .into_any_element()
    }
}

#[derive(Clone, Copy)]
pub(super) enum MenuKind {
    Level,
    Time,
    Saved,
    Columns,
    Export,
    Files,
}

pub(super) struct CommandMenu {
    menu: Entity<PopupMenu>,
    position: Point<Pixels>,
    _dismiss: Subscription,
}

fn menu_builder(owner: WeakEntity<LogView>, kind: MenuKind) -> BuildMenu {
    Rc::new(move |menu, window, cx| {
        let Some(view) = owner.upgrade() else {
            return menu;
        };
        view.read(cx).menu(kind, owner.clone(), menu, window, cx)
    })
}

pub(crate) fn describe(view: &Entity<LogView>, cx: &App) -> ViewerChrome {
    view.read(cx).describe(view.downgrade(), cx)
}

impl LogView {
    pub(crate) fn without_header(mut self) -> Self {
        self.hosted_header = true;
        self
    }

    pub(super) fn render_own_header(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let chrome = self.describe(cx.entity().downgrade(), cx);
        reader_viewer_chrome::render_compact_header(
            &chrome,
            window.viewport_size().width.as_f32(),
            window,
            cx,
        )
    }

    fn menu(
        &self,
        kind: MenuKind,
        owner: WeakEntity<Self>,
        mut menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        match kind {
            MenuKind::Saved => self.saved_menu(owner, menu, window, cx),
            MenuKind::Columns => self.columns_menu(owner, menu, window, cx),
            MenuKind::Export => self.export_menu(owner, menu, window, cx),
            MenuKind::Files => self.files_menu(owner, menu, window, cx),
            MenuKind::Level => {
                for (min, label) in LEVEL_CHOICES {
                    let owner = owner.clone();
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .checked(self.filter.level.min == min)
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |this, cx| {
                                    this.update_filter(|filter| filter.level.min = min, cx)
                                });
                            }),
                    );
                }
                let without = self.filter.level.without_level;
                menu.separator().item(
                    PopupMenuItem::new("Include lines without a level")
                        .checked(without)
                        .on_click(move |_, _, cx| {
                            let _ = owner.update(cx, |this, cx| {
                                this.update_filter(
                                    |filter| filter.level.without_level = !without,
                                    cx,
                                )
                            });
                        }),
                )
            }
            MenuKind::Time => {
                for time in TimeWindow::ALL {
                    let owner = owner.clone();
                    menu = menu.item(
                        PopupMenuItem::new(time.label())
                            .checked(self.filter.time == time)
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |this, cx| {
                                    this.update_filter(|filter| filter.time = time, cx)
                                });
                            }),
                    );
                }
                menu
            }
        }
    }

    fn menu_action(
        &self,
        owner: WeakEntity<Self>,
        kind: MenuKind,
        id: &'static str,
        icon: Icon,
        label: impl Into<SharedString>,
        cx: &App,
    ) -> ViewerAction {
        let invoke = owner.clone();
        let mut action = ViewerAction::new(
            id,
            icon,
            label,
            Rc::new(move |window, cx| {
                let _ = invoke.update(cx, |this, cx| this.show_menu(kind, window, cx));
            }),
        );
        action.menu = Some(menu_builder(owner, kind));
        action.tooltip = match kind {
            MenuKind::Level => reader_shortcuts::hint("Levels shown", &OpenLogLevels, cx),
            MenuKind::Time => reader_shortcuts::hint(
                "Time range, ending at the newest record",
                &OpenLogTimeRange,
                cx,
            ),
            MenuKind::Saved => reader_shortcuts::hint("Saved views", &OpenLogSavedViews, cx),
            MenuKind::Columns => reader_shortcuts::hint("Columns", &OpenLogColumns, cx),
            MenuKind::Export => reader_shortcuts::hint("Export rows", &OpenLogExport, cx),
            MenuKind::Files => reader_shortcuts::hint("Files in this view", &OpenLogFiles, cx),
        };
        action
    }

    fn describe(&self, owner: WeakEntity<Self>, cx: &App) -> ViewerChrome {
        let mut level = self.menu_action(
            owner.clone(),
            MenuKind::Level,
            "log-level",
            Icon::default().path(brand::READER_FILTER_ICON),
            level_label(self.filter.level),
            cx,
        );
        level.labelled = true;
        level.label_by_default = true;
        level.caret = true;
        level.minimum_width = 40.;
        level.never_dropped = true;
        level.collapse_priority = 12;
        level.filled = self.filter.level.is_active();
        level.toggled = level.filled;
        let mut actions = vec![level];
        let timestamped = (0..self.source_count()).any(|source| {
            self.source(source)
                .is_some_and(|data| data.index().stats().timestamped > 0)
        });
        if timestamped {
            let mut time = self.menu_action(
                owner.clone(),
                MenuKind::Time,
                "log-time",
                Icon::default().path(brand::READER_CLOCK_ICON),
                self.filter.time.label(),
                cx,
            );
            time.labelled = true;
            time.label_by_default = true;
            time.caret = true;
            time.minimum_width = 40.;
            time.never_dropped = true;
            time.collapse_priority = 11;
            time.filled = self.filter.time != TimeWindow::All;
            time.toggled = time.filled;
            actions.push(time);
        }
        if !self.source_locked
            && !self.compressed()
            && !matches!(&self.state, State::Remote(_) | State::Stream(_))
        {
            let target = owner.clone();
            let mut follow = ViewerAction::new(
                "log-follow",
                Icon::new(IconName::ArrowDown),
                if self.following() {
                    "Following"
                } else {
                    "Follow"
                },
                Rc::new(move |_, cx| {
                    let _ = target.update(cx, |this, cx| this.toggle_follow(cx));
                }),
            )
            .with_action(ToggleLogFollow, cx);
            follow.labelled = true;
            follow.label_by_default = true;
            follow.caret = true;
            follow.minimum_width = 40.;
            follow.never_dropped = true;
            follow.collapse_priority = 10;
            follow.filled = self.following();
            follow.toggled = follow.filled;
            actions.push(follow);
        }
        let target = owner.clone();
        let mut query = ViewerAction::new(
            "log-query-toggle",
            Icon::new(IconName::SquareTerminal),
            "Query",
            Rc::new(move |window, cx| {
                let _ = target.update(cx, |this, cx| this.toggle_query(window, cx));
            }),
        )
        .with_action(ToggleLogQuery, cx);
        query.never_dropped = true;
        query.toggled = self.secondary_visible();
        actions.push(query);
        for (kind, id, icon, label, priority, allowed) in [
            (
                MenuKind::Saved,
                "log-saved",
                Icon::new(IconName::Star),
                "Saved views",
                4,
                true,
            ),
            (
                MenuKind::Columns,
                "log-columns-menu",
                Icon::new(IconName::LayoutDashboard),
                "Columns",
                3,
                true,
            ),
            (
                MenuKind::Export,
                "log-export",
                Icon::default().path(brand::READER_DOWNLOAD_ICON),
                "Export rows",
                2,
                !matches!(&self.state, State::Remote(_)),
            ),
            (
                MenuKind::Files,
                "log-files",
                Icon::new(IconName::Folder),
                "Files",
                1,
                !self.source_locked && !matches!(&self.state, State::Remote(_) | State::Stream(_)),
            ),
        ] {
            if allowed {
                let mut action = self.menu_action(owner.clone(), kind, id, icon, label, cx);
                action.priority = priority;
                actions.push(action);
            }
        }
        let close_owner = owner.clone();
        let find = reader_find_field::Target {
            input: self.text_input.clone(),
            focus_override: None,
            scope: "Find in records".into(),
            count: None,
            match_case: None,
            step: None,
            steps_visible: false,
            large_file: false,
            close: Rc::new(move |window, cx| {
                let _ = close_owner.update(cx, |this, cx| this.focus.focus(window, cx));
            }),
        };
        let raw_owner = owner.clone();
        let mut raw = ViewerAction::new(
            "log-copy-raw",
            Icon::new(IconName::Copy),
            "Copy raw line",
            Rc::new(move |window, cx| {
                let _ = raw_owner.update(cx, |this, cx| this.copy_raw(window, cx));
            }),
        )
        .with_action(CopyRawLine, cx);
        raw.enabled = self.selection().is_some();
        raw.disabled_reason = (!raw.enabled).then(|| "Select a record first".into());
        ViewerChrome {
            overflow_order: reader_viewer_chrome::OverflowOrder {
                reveal: 5,
                width: 20,
                maximize: 21,
                open_with: 20,
                copy: 0,
                find_narrow: 30,
                find_icon: 31,
                viewer_text: 0,
            },
            find: Some(find),
            actions,
            copy_actions: vec![raw],
            copy_in_menu: true,
            ..Default::default()
        }
    }

    pub(super) fn show_menu(&mut self, kind: MenuKind, window: &mut Window, cx: &mut Context<Self>) {
        let owner = cx.entity().downgrade();
        let focus = self.focus.clone();
        let menu = PopupMenu::build(window, cx, |menu, window, cx| {
            self.menu(kind, owner, menu.action_context(focus.clone()), window, cx)
        });
        menu.focus_handle(cx).focus(window, cx);
        let dismiss = cx.subscribe_in(&menu, window, |this, _, _: &DismissEvent, window, cx| {
            this.command_menu = None;
            this.focus.focus(window, cx);
            cx.notify();
        });
        self.command_menu = Some(CommandMenu {
            menu,
            position: window.mouse_position(),
            _dismiss: dismiss,
        });
        cx.notify();
    }

    pub(super) fn render_command_menu(&self) -> Option<AnyElement> {
        let menu = self.command_menu.as_ref()?;
        Some(
            deferred(
                anchored()
                    .position(menu.position)
                    .snap_to_window_with_margin(px(8.))
                    .child(div().occlude().child(menu.menu.clone())),
            )
            .with_priority(gpui_base::POPUP_PRIORITY)
            .into_any_element(),
        )
    }

    fn secondary_visible(&self) -> bool {
        self.query_open
            || !self.query_text.trim().is_empty()
            || !self.filter_ui.chips.is_empty()
            || self.regex_editor.is_some()
            || self.regex_error.is_some()
            || !self.investigation.frames.is_empty()
            || self.investigation.request.is_some()
    }

    pub(super) fn toggle_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.query_open = !self.query_open;
        if self.query_open {
            self.query_input
                .update(cx, |input, cx| input.focus(window, cx));
        } else {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    pub(super) fn render_secondary(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.secondary_visible() {
            return None;
        }
        let narrow = window.viewport_size().width < px(900.);
        let owner = cx.entity().downgrade();
        let chips = if narrow && !self.filter_ui.chips.is_empty() {
            let count = self.filter_ui.chips.len();
            Some(
                gpui_component::popover::Popover::new("log-chip-overflow")
                    .trigger(
                        Button::new("log-chip-count")
                            .ghost()
                            .small()
                            .label(format!("{count} filters"))
                            .debug_selector(|| "log-chip-count".into()),
                    )
                    .content(move |_, _, cx| {
                        div().w(px(420.)).max_w_full().children(
                            owner
                                .update(cx, |this, cx| this.render_chip_bar(cx))
                                .ok()
                                .flatten(),
                        )
                    })
                    .into_any_element(),
            )
        } else {
            self.render_chip_bar(cx)
        };
        Some(
            h_flex()
                .id("log-query-row")
                .debug_selector(|| "log-query-row".into())
                .flex_none()
                .h(px(40.))
                .px_3()
                .gap_1()
                .min_w_0()
                .overflow_hidden()
                .children((!self.investigation.frames.is_empty()).then(|| {
                    bar_button("log-request-back", true)
                        .label("Back")
                        .icon(IconName::ArrowLeft)
                        .tooltip(reader_shortcuts::hint(
                            "Back to the previous filter",
                            &InvestigationBack,
                            cx,
                        ))
                        .debug_selector(|| "log-request-back".into())
                        .on_click(cx.listener(|this, _, window, cx| this.request_back(window, cx)))
                }))
                .children(self.render_investigation(cx))
                .when(self.regex_editor.is_none(), |row| {
                    row.child(self.render_query_controls(cx)).children(chips)
                })
                .children(self.render_regex_editor(cx))
                .child(
                    bar_button("log-chips-clear", false)
                        .label("Clear all")
                        .debug_selector(|| "log-chips-clear".into())
                        .tooltip(reader_shortcuts::hint(
                            "Clear filters",
                            &ClearLogFilters,
                            cx,
                        ))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.clear_filters(window, cx)),
                        ),
                )
                .children(self.regex_error.clone().map(|error| {
                    div()
                        .debug_selector(|| "log-regex-error".into())
                        .text_color(brand::palette(cx).danger)
                        .truncate()
                        .child(error)
                }))
                .into_any_element(),
        )
    }
}
