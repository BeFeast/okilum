use super::*;

pub(super) const READER_HEADER_HEIGHT: f32 = 46.;
pub(super) const READER_SIDE_PADDING: f32 = 40.;

/// Left gutter of the note text, the same in Reader, Source and Live Preview
/// so text does not move between them. It holds a revealed heading marker
/// (`##`) in Live Preview (#1034): 48 px, narrowing linearly to 24 px at the
/// 600 px minimum window so narrow windows keep their text width.
pub(super) fn note_gutter(window: &Window) -> Pixels {
    let width = f32::from(window.viewport_size().width);
    px((24. + (width - 600.) * 24. / 400.).clamp(24., 48.))
}

/// Reader window title bar: the native traffic lights are vertically centred
/// on the 46px Reader header (#365). The toolkit default centres them on its
/// 34px bar (y = 9 for a 16px button group).
pub(crate) fn reader_titlebar_options() -> TitlebarOptions {
    const TRAFFIC_LIGHT_GROUP_HEIGHT: f32 = 16.;
    TitlebarOptions {
        traffic_light_position: Some(point(
            px(9.),
            px((READER_HEADER_HEIGHT - TRAFFIC_LIGHT_GROUP_HEIGHT) / 2.),
        )),
        ..TitleBar::title_bar_options()
    }
}

pub(super) const READER_SLIDERS_ICON: &str = "icons/sliders.svg";

/// A run of header controls. The standalone header lives inside the window's
/// TitleBar, which starts a window move on press and zooms on double-click;
/// the controls handle their press first, then keep it from reaching the bar.
pub(super) fn header_controls() -> Div {
    h_flex()
        .flex_none()
        .gap(px(2.))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// Icon-only Reader control (docs/design/reader.md §Icons): 28px, ghost, tooltip.
pub(super) fn reader_icon_button(
    id: impl Into<ElementId>,
    icon: impl Into<Icon>,
    tooltip: impl Into<SharedString>,
    _cx: &App,
) -> Button {
    Button::new(id)
        .ghost()
        .icon(icon)
        .tooltip(tooltip)
        .w(px(28.))
        .h(px(28.))
        .rounded(px(6.))
}

/// More (⋯): open actions and appearance. Themes are picked in Settings (#349).
pub(super) fn reader_more_menu(
    vault: PathBuf,
    _selected: String,
    show_hidden: bool,
    mode_label: &'static str,
    reader: WeakEntity<Reader>,
    cx: &App,
) -> impl IntoElement {
    use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
    use gpui_component::ThemeMode;
    let current = cx.try_global::<AppearancePreference>().and_then(|p| p.0);
    reader_icon_button("reader-more", IconName::Ellipsis, "More", cx)
        .debug_selector(|| "reader-more".into())
        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, window, cx| {
            let menu = menu
                .scrollable(true)
                .max_h((window.viewport_size().height - px(80.)).max(px(40.)));
            let appearance = |label: &'static str, mode: Option<ThemeMode>| {
                let vault = vault.clone();
                PopupMenuItem::new(label)
                    .checked(current == mode)
                    .on_click(move |_, window, cx| set_appearance(mode, Some(&vault), window, cx))
            };
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            let settings_reader_for_sync = reader.clone();
            let settings_reader = reader.clone();
            let wrap_reader = reader.clone();
            let reader = reader.clone();
            // `.action` only shows the bound key; the click handler still runs.
            let app_section = |label, section| {
                let reader = reader.clone();
                PopupMenuItem::new(label).on_click(move |_, window, cx| {
                    let _ =
                        reader.update(cx, |this, cx| this.show_app_section(section, window, cx));
                })
            };
            menu.item(app_section(
                "Reader",
                reader_project_navigation::Section::Reader,
            ))
            .item(app_section(
                "Inbox",
                reader_project_navigation::Section::Inbox,
            ))
            .when(
                reader_features::enabled(reader_features::Feature::Agents, cx),
                |menu| {
                    menu.item(app_section(
                        "Projects",
                        reader_project_navigation::Section::Projects,
                    ))
                },
            )
            .item(app_section(
                "Logs",
                reader_project_navigation::Section::Logs,
            ))
            .when(
                cfg!(all(unix, feature = "brain"))
                    && reader_features::enabled(reader_features::Feature::SecondBrain, cx),
                |menu| {
                    #[cfg(all(unix, feature = "brain"))]
                    let menu = menu.item(
                        PopupMenuItem::new("Second brain…")
                            .on_click(|_, _, cx| reader_features::open_second_brain(cx)),
                    );
                    menu
                },
            )
            .separator()
            .item(
                PopupMenuItem::new("Settings…")
                    .action(Box::new(reader_settings::OpenSettings))
                    .on_click(move |_, _, cx| {
                        reader_settings::show(Some(settings_reader.clone()), cx);
                    }),
            )
            .menu(
                "Project service storage…",
                Box::new(reader_service_storage::OpenServiceStorage),
            )
            .map(|menu| {
                #[cfg(any(target_os = "linux", target_os = "macos", windows))]
                let menu = menu.item(reader_settings::share_sync_menu_item(
                    settings_reader_for_sync.clone(),
                ));
                menu
            })
            .menu("Keyboard shortcuts", Box::new(ToggleShortcutSheet))
            .menu("Maximize note", Box::new(MaximizeNote))
            .separator()
            .when(cfg!(any(unix, windows)), |menu| {
                menu.menu("New note…", Box::new(NewNote))
                    .menu("New untitled file", Box::new(NewUntitled))
                    .menu("New Folder", Box::new(NewFolder))
                    .menu("New note from template…", Box::new(NewFromTemplate))
                    .menu("Today's note", Box::new(OpenDailyNote))
                    .item(reader_capture::menu_item(reader.clone(), vault.clone(), cx))
                    .menu("Recover notes…", Box::new(RecoverUnsavedNotes))
                    .menu("Recover link moves…", Box::new(RecoverLinkMoves))
            })
            .separator()
            .menu("Recent files…", Box::new(ShowRecentFiles))
            .menu("Open file…", Box::new(reader_open::OpenFile))
            .menu("Open folder…", Box::new(reader_open::OpenFolder))
            .menu(mode_label, Box::new(SwitchWindowMode))
            .menu("New Window", Box::new(reader_open::NewWindow))
            .separator()
            .item(
                PopupMenuItem::new(HIDDEN_FILES_MENU)
                    .checked(show_hidden)
                    .action(Box::new(ToggleHiddenFiles))
                    .on_click(move |_, _, cx| {
                        let _ = reader.update(cx, |this, cx| this.toggle_hidden_files(cx));
                    }),
            )
            .item(
                PopupMenuItem::new("Wrap long lines")
                    .checked(reader_ui_state::code_soft_wrap(cx))
                    .action(Box::new(ToggleSoftWrap))
                    .on_click(move |_, window, cx| {
                        let _ = wrap_reader
                            .update(cx, |this, cx| this.toggle_code_soft_wrap(window, cx));
                    }),
            )
            .separator()
            .menu("Collapse all folders", Box::new(CollapseFolders))
            .separator()
            .label("Appearance")
            .item(appearance("System", None))
            .item(appearance("Light", Some(ThemeMode::Light)))
            .item(appearance("Dark", Some(ThemeMode::Dark)))
            .map(desktop_app_menu::append)
        })
}

// Only Reader presentation controls opt in; document and search clicks retain
// ordinary toolkit selection behavior.
pub(super) fn preserve_reader_selection(
    id: &'static str,
    control: impl IntoElement,
) -> impl IntoElement {
    div()
        .relative()
        .debug_selector(move || id.into())
        .child(control)
        .child(
            canvas(
                |_, _, _| (),
                |bounds, _, window, cx| {
                    gpui_base::TextSelection::preserve_on_pointer_down(bounds, window, cx);
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
}

#[cfg(not(target_os = "linux"))]
#[derive(Clone)]
struct ReaderPanelDrag;
#[cfg(not(target_os = "linux"))]
impl Render for ReaderPanelDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl Reader {
    /// Left and right margins of the note text across the whole document
    /// pane: Reader's centred column (reading width) with its gutter and right
    /// padding. The editor fills the pane, so wheel scrolling works over the
    /// margins, and uses these so text keeps its x and line width between
    /// Reader, Source and Live Preview.
    pub(super) fn note_text_margins(&self, window: &Window, cx: &App) -> (Pixels, Pixels) {
        let available = f32::from(self.body_bounds.size.width);
        let pane = if reader_layout::overlay(available) {
            available
        } else {
            let widths = self
                .workspace_panels()
                .widths(&self.panel_widths, available);
            available - widths.notes - widths.backlinks
        };
        let offset = ((pane - reader_ui_state::note_column_width(cx)) / 2.).max(0.);
        // The editor keeps 10 px between its wrap width and its right padding.
        (
            px(offset) + note_gutter(window),
            px(offset + READER_SIDE_PADDING - 10.),
        )
    }

    /// docs/design/reader.md §Layout: icon controls, breadcrumbs, loading status.
    /// Title navigation is built first, from this small frame; the header
    /// row's own builder temporaries live in a separate frame (#2825).
    pub(super) fn render_header(
        &self,
        available: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let title_nav = self.render_title_nav(available, cx).into_any_element();
        self.render_header_row(available, title_nav, window, cx)
    }

    #[inline(never)]
    fn render_header_row(
        &self,
        available: f32,
        title_nav: AnyElement,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let notes_open = self
            .workspace_panels()
            .visible(reader_layout::Panel::Notes, available);
        let backlinks_open = self
            .panels
            .visible(reader_layout::Panel::Backlinks, available);
        h_flex()
            .id("reader-header")
            .debug_selector(|| "reader-header".into())
            .w_full()
            .when(cfg!(windows) && !self.embedded_in_workspace, |row| {
                // The toolkit's bar content does not shrink. Reserve its native
                // control areas so a long breadcrumb/status cannot push them offscreen.
                let controls = window.window_controls();
                let count = 1 + usize::from(controls.minimize) + usize::from(controls.maximize);
                let reserved = px(12.) + gpui_component::TITLE_BAR_HEIGHT * count as f32;
                row.w((window.viewport_size().width - reserved).max(px(0.)))
            })
            .min_w_0()
            .h(px(READER_HEADER_HEIGHT))
            .gap(px(2.))
            .pr(px(10.))
            .when(self.embedded_in_workspace, |row| row.pl(px(10.)))
            .text_size(px(brand::READER_CHROME_FONT_SIZE))
            .child(
                header_controls()
                    .when(self.project_navigation.active.is_some(), |controls| {
                        controls.child(
                            Button::new("reader-files")
                                .debug_selector(|| "reader-files".into())
                                .ghost()
                                .small()
                                .icon(IconName::Folder)
                                .label("Files")
                                .tooltip(reader_shortcuts::hint(
                                    "Return to Files",
                                    &ToggleNotes,
                                    cx,
                                ))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.show_vault_files(window, cx)
                                })),
                        )
                    })
                    .when(self.project_navigation.active.is_none(), |controls| {
                        controls.child(preserve_reader_selection(
                            "reader-notes-preserve",
                            reader_icon_button(
                                "reader-notes",
                                IconName::PanelLeft,
                                reader_shortcuts::hint("Notes", &ToggleNotes, cx),
                                cx,
                            )
                            .debug_selector(|| "reader-notes".into())
                            .selected(notes_open)
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.toggle_panel(reader_layout::Panel::Notes, window, cx)
                                },
                            )),
                        ))
                    })
                    // Variant A (#1508): switcher, Recent ▾, Pinned ▾, then search.
                    .child(title_nav)
                    .child(
                        div()
                            .id("sidebar-search")
                            .debug_selector(|| "sidebar-search".into())
                            .track_focus(&self.sidebar_search_focus)
                            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                    this.open_quick_open(false, window, cx);
                                    cx.stop_propagation();
                                }
                            }))
                            .child(
                                reader_icon_button(
                                    "reader-search",
                                    IconName::Search,
                                    reader_shortcuts::hint("Search in vault", &FullTextSearch, cx),
                                    cx,
                                )
                                .debug_selector(|| "reader-search".into())
                                .on_click(cx.listener(
                                    |this, _, window, cx| this.open_quick_open(true, window, cx),
                                )),
                            ),
                    ),
            )
            .child(div().flex_1().min_w_0())
            .child(
                header_controls()
                    .flex_shrink(1.)
                    .min_w_0()
                    .when(
                        !self.single_file && self.archive_state.is_archive(),
                        |controls| {
                            controls.child(
                                div()
                                    .id("archive-badge")
                                    .debug_selector(|| "archive-badge".into())
                                    .flex_none()
                                    .px_2()
                                    .py_0p5()
                                    .mr_1()
                                    .rounded(px(6.))
                                    .border_1()
                                    .border_color(brand::palette(cx).border_subtle)
                                    .bg(cx.theme().secondary)
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(cx.theme().muted_foreground)
                                    .child(reader_archive_vault::READ_ONLY_ARCHIVE_LABEL),
                            )
                        },
                    )
                    .when(self.single_file, |controls| {
                        if let Some(vault) = &self.quick_viewer_vault {
                            controls.child(
                                Button::new("open-in-vault")
                                    .debug_selector(|| "open-in-vault".into())
                                    .label(reader_archive_vault::open_in_vault_label(&vault.name))
                                    .tooltip(vault_name::path_tooltip(&vault.root))
                                    .small()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_quick_viewer_in_vault(window, cx);
                                    })),
                            )
                        } else {
                            controls.child(
                                Button::new("open-folder-as-vault")
                                    .label("Open folder as vault")
                                    .small()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.switch_window_mode(window, cx);
                                    })),
                            )
                        }
                    })
                    .child(self.render_loading(cx))
                    .child(reader_feedback::button(cx))
                    .child(reader_more_menu(
                        self.vault_root.clone(),
                        self.selected_file().to_owned(),
                        self.sidebar.show_hidden,
                        self.window_mode_label(),
                        cx.entity().downgrade(),
                        cx,
                    ))
                    .child(preserve_reader_selection(
                        "reader-backlinks-preserve",
                        reader_icon_button(
                            "reader-backlinks",
                            IconName::PanelRight,
                            reader_shortcuts::hint("On this page", &ToggleBacklinks, cx),
                            cx,
                        )
                        .selected(backlinks_open)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_panel(reader_layout::Panel::Backlinks, window, cx)
                        })),
                    )),
            )
            .into_any_element()
    }

    pub(super) fn render_breadcrumbs(
        &self,
        collapse_parents: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_breadcrumbs_with_state(collapse_parents, None, cx)
    }

    pub(super) fn render_breadcrumbs_with_state(
        &self,
        collapse_parents: bool,
        state: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        #[cfg(any(unix, windows))]
        if let Some(rename) = self.renaming.as_ref().filter(|r| r.in_header) {
            return v_flex()
                .id("note-title-rename")
                .key_context("InlineRename")
                .flex_1()
                .min_w_0()
                .on_action(|_: &gpui_component::input::Enter, _, cx| cx.stop_propagation())
                .child(Input::new(&rename.input).small().appearance(false))
                .when_some(rename.error.clone(), |view, error| {
                    view.child(
                        div()
                            .text_size(px(12.))
                            .text_color(cx.theme().danger)
                            .child(error),
                    )
                })
                .into_any_element();
        }
        let p = brand::palette(cx);
        let faint = brand::reader_palette(cx).text_faint;
        // Each folder crumb carries its root-relative path (#366).
        let folders: Vec<(String, String)> = self
            .selected_file()
            .rsplit_once('/')
            .map(|(dir, _)| {
                let mut path = String::new();
                dir.split('/')
                    .map(|name| {
                        if !path.is_empty() {
                            path.push('/');
                        }
                        path.push_str(name);
                        (name.to_owned(), path.clone())
                    })
                    .collect()
            })
            .unwrap_or_default();
        let current = self.selected_file().to_owned();
        let current_menu = current.clone();
        let file_root = self.vault_root.clone();
        let link_scope = self.link_scope();
        let title_reader = cx.entity().downgrade();
        let root = self.selected_title();
        let parent_menu = folders.clone();
        let deleted = self
            .deleted_current()
            .is_some_and(|deleted| deleted.disk == reader_deleted_note::Disk::Missing);
        h_flex()
            .id("reader-breadcrumbs")
            .flex_1()
            .min_w_0()
            .gap_1()
            .px_2()
            .overflow_hidden()
            .whitespace_nowrap()
            .when(collapse_parents, |row| {
                use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
                let reader = cx.entity().downgrade();
                row.child(
                    reader_icon_button(
                        "note-parent-folders",
                        IconName::FolderOpen,
                        "Parent folders",
                        cx,
                    )
                    .dropdown_menu(move |mut menu, _, _| {
                        for (name, path) in &parent_menu {
                            let reader = reader.clone();
                            let path = path.clone();
                            menu = menu.item(PopupMenuItem::new(name.clone()).on_click(
                                move |_, window, cx| {
                                    let _ = reader.update(cx, |this, cx| {
                                        this.reveal_in_tree(&path, window, cx)
                                    });
                                },
                            ));
                        }
                        menu
                    }),
                )
            })
            .children(folders.into_iter().filter(|_| !collapse_parents).flat_map(
                |(folder, path)| {
                    let menu_path = path.clone();
                    let selector = path.clone();
                    let menu_root = self.vault_root.clone();
                    [
                        div()
                            .id(SharedString::from(format!("reader-crumb-{path}")))
                            .debug_selector(move || format!("reader-crumb-{selector}"))
                            .flex_shrink(1.)
                            .min_w(px(24.))
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_color(p.text_muted)
                            .cursor_pointer()
                            .hover(move |d| d.text_color(p.text))
                            // Keep the crumb press local to this control.
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.reveal_in_tree(&path, window, cx)
                            }))
                            .context_menu(move |menu, window, cx| {
                                reader_files::menu(
                                    menu,
                                    menu_root.clone(),
                                    menu_path.clone(),
                                    link_scope,
                                    window,
                                    cx,
                                )
                            })
                            .child(folder)
                            .into_any_element(),
                        Icon::new(IconName::ChevronRight)
                            .xsmall()
                            .text_color(faint)
                            .into_any_element(),
                    ]
                },
            ))
            .child(
                div()
                    .id("reader-document-root")
                    .debug_selector(|| "reader-document-root".into())
                    .min_w_0()
                    .flex_shrink(1.)
                    .max_w_full()
                    .overflow_hidden()
                    .text_ellipsis()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.text)
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        #[cfg(any(unix, windows))]
                        if this.pane.file_preview.is_none() {
                            this.rename_note_title(window, cx);
                            return;
                        }
                        this.reveal_in_tree(&current, window, cx)
                    }))
                    .child(self.selected_title())
                    // A note deleted outside Okilum is struck through (#1986).
                    .when(deleted, |title| {
                        title.line_through().text_color(p.text_muted)
                    })
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(root.clone()).build(window, cx)
                    })
                    .context_menu(move |menu, window, cx| {
                        let pin = reader_pin_row::pin_menu_item(&title_reader, &current_menu, cx);
                        reader_files::menu(
                            menu,
                            file_root.clone(),
                            current_menu.clone(),
                            link_scope,
                            window,
                            cx,
                        )
                        .when_some(pin, |menu, pin| menu.separator().item(pin))
                    }),
            )
            .children(state)
            .when(deleted, |row| {
                row.child(
                    div()
                        .id("reader-document-deleted")
                        .debug_selector(|| "reader-document-deleted".into())
                        .flex_none()
                        .text_color(p.text_muted)
                        .cursor_pointer()
                        .hover(move |d| d.text_color(p.text))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|this, _, _, cx| this.reshow_deleted_notice(cx)))
                        .child("Deleted")
                        .tooltip(|window, cx| {
                            gpui_component::tooltip::Tooltip::new(
                                "Deleted from disk: restore it or save it elsewhere",
                            )
                            .build(window, cx)
                        }),
                )
            })
            .into_any_element()
    }

    /// The right panel's header: its title only.
    fn render_panel_header(
        &self,
        panel: reader_layout::Panel,
        width: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if panel == reader_layout::Panel::Notes {
            return self.render_sidebar_header(width, window, cx);
        }
        let title = if self.project_navigation.active.is_some() {
            "Manager"
        } else {
            "Note history"
        };
        // The Manager panel says whose manager it is with the avatar (#1627).
        let avatar = self
            .project_navigation
            .active
            .as_ref()
            .map(|project| okilum_shell_kit::project_avatar::project_avatar(project, 16., cx));
        h_flex()
            .debug_selector(|| "reader-backlinks-header".into())
            .h(px(READER_HEADER_HEIGHT))
            .flex_none()
            .gap_1()
            .px_3()
            .border_b_1()
            .border_color(brand::palette(cx).border_subtle)
            .text_size(px(brand::READER_CHROME_FONT_SIZE))
            .when_some(avatar, |header, avatar| {
                header.child(
                    div()
                        .debug_selector(|| "manager-project-avatar".into())
                        .flex_none()
                        .mr_1()
                        .child(avatar),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .into_any_element()
    }

    pub(super) fn render_main(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.pane.typed_navigation.active.set(false);
        self.pane.query_navigation.active.borrow_mut().take();
        self.pane.query_navigation.pending.set(false);
        if let Some(preview) = self.render_tree_quick_look() {
            return preview;
        }
        if let Some(preview) = self.render_timeline_preview(window, cx) {
            return preview;
        }
        if self.empty_vault_shown() {
            return self.render_empty_vault(cx);
        }
        if let Some(preview) = &self.pane.file_preview {
            return self.render_file_preview(preview, window, cx);
        }
        if self.pane.editing.is_some() {
            return self.render_source(window, cx);
        }
        if let Some(view) = reader_typed_view::render(self, window, cx) {
            return view;
        }
        self.render_note_column(window, cx)
    }

    /// The rendered note's builder temporaries stay out of the dispatch frame
    /// that every nested file/source draw passes through (#2825).
    #[inline(never)]
    fn render_note_column(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity().downgrade();
        let mut style = reader_text_zoom::document_style(cx);
        style.heading_base_font_size = px(reader_ui_state::font_size(cx));
        style.top_padding = px(44.);
        style.bottom_padding = reader_toast::bottom_space(window, cx);
        let column_bounds = std::rc::Rc::new(std::cell::Cell::new(Bounds::default()));
        let measured_column = column_bounds.clone();
        h_flex()
            .id("reader-scroll-surface")
            .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, _, cx| {
                // The TextView handles events in its own column. Only forward
                // the surrounding document margins, never double-scroll text.
                let body = this.body_bounds;
                let widths = this
                    .workspace_panels()
                    .widths(&this.panel_widths, f32::from(body.size.width));
                let outside_panels = event.position.x >= body.left() + px(widths.notes)
                    && event.position.x < body.right() - px(widths.backlinks);
                if outside_panels && !column_bounds.get().contains(&event.position) {
                    this.pane
                        .content
                        .read(cx)
                        .list_state()
                        .scroll_by(-event.delta.pixel_delta(px(20.)).y);
                    cx.notify();
                    cx.stop_propagation();
                }
            }))
            .flex_1()
            .h_full()
            .min_w(px(0.))
            .overflow_hidden()
            .justify_center()
            .child(
                v_flex()
                    .id("reader-column")
                    .debug_selector(|| "reader-column".into())
                    .relative()
                    .child(
                        canvas(
                            move |bounds, _, _| measured_column.set(bounds),
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    )
                    .h_full()
                    .w_full()
                    .max_w(px(reader_ui_state::note_column_width(cx)))
                    .when_some(self.render_properties_strip(cx), |column, strip| {
                        column.child(strip)
                    })
                    .child({
                        let menu_entity = entity.clone();
                        // The vendored TextView has no selection menu of its own, so
                        // this container offers context actions for the selected text.
                        v_flex()
                            .id("reader-text")
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            // #1615: files and data dropped onto the rendered
                            // note go into Edit, before the block under the
                            // pointer.
                            .drag_over::<ExternalPaths>(|style, _, _, cx| {
                                style.bg(brand::palette(cx).selected.opacity(0.12))
                            })
                            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                                this.drop_on_reader(paths.into(), window, cx)
                            }))
                            .drag_over::<ExternalData>(|style, _, _, cx| {
                                style.bg(brand::palette(cx).selected.opacity(0.12))
                            })
                            .on_drop(cx.listener(|this, data: &ExternalData, window, cx| {
                                this.drop_on_reader(data.into(), window, cx)
                            }))
                            // The TextView drops link clicks while text is
                            // selected, so a right-click on a link would open
                            // nothing; open the link's menu here (#943).
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                    let Some(url) = this.hovered_link_with_selection(cx) else {
                                        return;
                                    };
                                    this.file_link_menu(&url, event.position, window, cx);
                                }),
                            )
                            .context_menu(move |menu, window, cx| {
                                reader_context_menu::menu(menu, &menu_entity, window, cx)
                            })
                            .children(self.render_landing_flash(cx))
                            .child(
                                reader_plugins(
                                    self.vault_root.clone(),
                                    TextView::new(&self.pane.content)
                                        .font_family(reader_fonts::note(cx))
                                        .scrollable(true)
                                        .selectable(true)
                                        // The element pushes its flags into the state every frame
                                        // (same trap as `scrollable`): the state-side
                                        // selection_format is clobbered unless set here too.
                                        .selection_format(self.sel_format)
                                        .style(style)
                                        .text_size(px(reader_ui_state::font_size(cx)))
                                        .pl(note_gutter(window))
                                        .pr(px(READER_SIDE_PADDING))
                                        .w_full()
                                        .flex_1()
                                        .min_h_0(),
                                    entity.clone(),
                                    self.sel_format,
                                    self.pane.link_presentations.clone(),
                                    &self.pane.link_identities,
                                    self.agent_document_enabled(cx),
                                )
                                .table_actions(
                                    move |data, _, _| {
                                        // #368: only a table wider than the column offers it.
                                        let entity = entity.clone();
                                        let markdown = data.markdown.clone();
                                        h_flex().justify_end().when(data.overflows, move |d| {
                                            d.child(
                                                Button::new("expand-table")
                                                    .small()
                                                    .icon(IconName::Maximize)
                                                    .label(format!(
                                                        "{} columns",
                                                        data.headers.len()
                                                    ))
                                                    .ghost()
                                                    .debug_selector(|| "expand-table-badge".into())
                                                    .tooltip("Show the whole table")
                                                    .on_click(move |_, window, cx| {
                                                        let markdown = markdown.clone();
                                                        let _ = entity.update(cx, |this, cx| {
                                                            this.open_table_overlay(
                                                                &markdown, window, cx,
                                                            )
                                                        });
                                                    }),
                                            )
                                        })
                                    },
                                ),
                            )
                    }),
            )
            .into_any_element()
    }

    pub(super) fn open_table_overlay(
        &mut self,
        markdown: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entity = cx.entity().downgrade();
        let configured = reader_plugins(
            self.vault_root.clone(),
            TextView::new(&self.pane.content),
            entity,
            self.sel_format,
            self.pane.link_presentations.clone(),
            &self.pane.link_identities,
            false,
        );
        let markdown = markdown.to_owned();
        let sel_format = self.sel_format;
        let state = cx.new(|cx| {
            let mut state = TextViewState::markdown("", cx)
                .scrollable(true)
                .selectable(true)
                .selection_format(sel_format);
            configured.prepare_state(&mut state, cx);
            state.set_text(&markdown, cx);
            state
        });
        let focus = state.read(cx).focus_handle().clone();
        self.pane.table_overlay = Some(state);
        focus.focus(window, cx);
        cx.notify();
    }

    pub(super) fn close_table_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pane.table_overlay = None;
        let focus = self.pane.content.read(cx).focus_handle().clone();
        focus.focus(window, cx);
        cx.notify();
    }

    /// #368: the table at window width over the document, which stays where
    /// it was. Columns take their natural width, long cells wrap, and the
    /// table scrolls sideways only when it still does not fit.
    fn render_table_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.pane.table_overlay.clone()?;
        let p = brand::palette(cx);
        let mut style = reader_text_zoom::document_style(cx);
        style.heading_base_font_size = px(reader_ui_state::font_size(cx));
        let entity = cx.entity().downgrade();
        let scrim = brand::reader_palette(cx).scrim;
        Some(
            div()
                .id("table-overlay")
                .on_prepaint(gpui_base::GlobalState::record_native_overlay)
                .absolute()
                .inset_0()
                // Nothing under the scrim reacts to the pointer.
                .occlude()
                .flex()
                .justify_center()
                .bg(scrim)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_table_overlay(window, cx)),
                )
                .child(
                    v_flex()
                        .id("table-overlay-panel")
                        .m(px(24.))
                        .w_full()
                        .bg(p.surface)
                        .border_1()
                        .border_color(p.border)
                        .rounded(px(12.))
                        .shadow_lg()
                        .overflow_hidden()
                        // Clicks inside the panel never close it.
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            h_flex()
                                .flex_none()
                                .h(px(READER_HEADER_HEIGHT))
                                .pl_4()
                                .pr_2()
                                .border_b_1()
                                .border_color(p.border_subtle)
                                .text_size(px(brand::READER_CHROME_FONT_SIZE))
                                .child(
                                    div()
                                        .flex_1()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("Table"),
                                )
                                .child(
                                    reader_icon_button(
                                        "table-overlay-close",
                                        IconName::Close,
                                        reader_shortcuts::hint("Close", &Dismiss, cx),
                                        cx,
                                    )
                                    .on_click(cx.listener(
                                        |this, _, window, cx| this.close_table_overlay(window, cx),
                                    )),
                                ),
                        )
                        .child(
                            div().flex_1().min_h_0().child(reader_plugins(
                                self.vault_root.clone(),
                                TextView::new(&state)
                                    .font_family(reader_fonts::note(cx))
                                    .scrollable(true)
                                    .selectable(true)
                                    .selection_format(self.sel_format)
                                    .style(style)
                                    .text_size(px(reader_ui_state::font_size(cx)))
                                    .px(px(24.))
                                    .py(px(16.))
                                    .size_full(),
                                entity,
                                self.sel_format,
                                self.pane.link_presentations.clone(),
                                &self.pane.link_identities,
                                false,
                            )),
                        ),
                )
                .into_any_element(),
        )
    }

    /// Advance the opt-in timing probe after a painted frame (#653).
    fn drive_perf_probe(
        &mut self,
        document_ready: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.perf_probe.is_none() {
            return;
        }
        let elapsed_ms = reader_diagnostics::trace(cx).map_or(0., |trace| trace.elapsed_ms());
        let list = self.pane.content.read(cx).list_state().clone();
        let blocks = if document_ready { list.item_count() } else { 0 };
        let top = list.logical_scroll_top().item_ix;
        let targets: Vec<usize> = self
            .visible_outline()
            .iter()
            .map(|heading| heading.target.block)
            .collect();
        let content = self.pane.content.clone();
        let reader = cx.entity().downgrade();
        let Some(probe) = self.perf_probe.as_mut() else {
            return;
        };
        match probe.painted(blocks, top, &targets, elapsed_ms) {
            reader_perf_probe::Step::Idle => {}
            reader_perf_probe::Step::ScrollBy(distance) => window.on_next_frame(move |_, cx| {
                // As a wheel event does: move the list, notify its view.
                list.scroll_by(distance);
                content.update(cx, |_, cx| cx.notify());
                let _ = reader.update(cx, |_, cx| cx.notify());
            }),
            reader_perf_probe::Step::Jump(row) => window.on_next_frame(move |window, cx| {
                // As a click on the «On this page» row does.
                let _ = reader.update(cx, |this, cx| {
                    if let Some(heading) = this.visible_outline().get(row).cloned() {
                        this.open_outline_heading(heading, window, cx);
                    }
                    cx.notify();
                });
            }),
            reader_perf_probe::Step::StartTimed => {
                // Input on a wall-clock schedule, independent of paint (#3154):
                // the probe only counts the frames these steps cause.
                cx.spawn(async move |reader, cx| {
                    let interval = Duration::from_millis(reader_perf_probe::TIMED_INTERVAL_MS);
                    for _ in 0..reader_perf_probe::TIMED_STEPS {
                        cx.background_executor().timer(interval).await;
                        let scrolled = reader.update(cx, |_, cx| {
                            list.scroll_by(reader_perf_probe::STEP);
                            content.update(cx, |_, cx| cx.notify());
                            cx.notify();
                        });
                        if scrolled.is_err() {
                            return;
                        }
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(reader_perf_probe::TIMED_SETTLE_MS))
                        .await;
                    let _ = reader.update(cx, |this, cx| {
                        if let Some(probe) = this.perf_probe.as_mut() {
                            probe.finish_timed();
                        }
                        this.finish_perf_probe(targets.len(), cx);
                    });
                })
                .detach();
            }
        }
    }

    /// Write the probe's report and quit.
    fn finish_perf_probe(&mut self, outline_rows: usize, cx: &mut Context<Self>) {
        // What the run was drawn with, so a theme A/B can check it (#3156).
        let theme = serde_json::json!({
            "key": brand::theme_id(cx).key(),
            "dark": Theme::global(cx).is_dark(),
            "syntax": Theme::global(cx).highlight_theme.name,
        });
        if let Some(probe) = self.perf_probe.as_ref() {
            if let Err(error) = probe.write_report(outline_rows, theme) {
                diagnostic_error!("{}: {error}", reader_perf_probe::ENV);
            }
        }
        cx.quit();
    }
}

impl Render for Reader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pane.editing.is_some() {
            reader_editor::SOURCE_READER_RENDERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        #[cfg(all(target_os = "linux", feature = "project-ui-harness"))]
        let _work = project::performance::Work::start(30);
        #[cfg(target_os = "linux")]
        {
            let _ = self.layout_signal.read(cx);
            if self.resizing_panel.is_some() && self.panel_capture.get() != window.captured_hitbox()
            {
                self.resizing_panel = None;
            }
            if self.resizing_panel.is_none() {
                self.release_panel_pointer(window);
            }
        }
        if let Some(probe) = &mut self.perf_probe {
            probe.render_started();
        }
        let _render_timing =
            platform::text_diagnostics::time(platform::text_diagnostics::Work::UiRender);
        platform::shaped_line_cache::ShapedLineCache::note_scale_factor(window, cx);
        if self.toast_subscription.is_none() {
            let notifications = Root::read(window, cx).notification.clone();
            self.toast_subscription = Some(cx.observe(&notifications, |_, _, cx| cx.notify()));
        }
        self.sync_notice_toast(window, cx);
        // Root owns overlay state, but the window content renders these layers.
        let sheet_layer = Root::render_sheet_layer(window, cx);
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);
        let sync_started = std::time::Instant::now();
        let sync_timing =
            platform::text_diagnostics::time(platform::text_diagnostics::Work::ReaderUiSync);
        if self
            .tree
            .set_note_titles(reader_ui_state::tree_note_titles(cx))
        {
            self.tree_sticky.clear();
            self.tree_earlier_roots.clear();
            self.tree_revealed.clear();
        }
        self.prepare_tree_follow(cx);
        self.sync_tree();
        self.schedule_tree_follow(cx);
        self.sync_sidebar(cx);
        self.sync_agent_document(cx);
        self.restore_ui_tree();
        self.restore_ui_source(window, cx);
        self.restore_workspace_mode(window, cx);
        self.record_ui_state(window.is_window_active(), cx);
        {
            let _timing =
                platform::text_diagnostics::time(platform::text_diagnostics::Work::BacklinkTitles);
            self.sync_backlink_titles();
        }
        self.sync_outline_section(cx);
        drop(sync_timing);
        let diagnostics = self
            .loading
            .as_ref()
            .and_then(|load| load.opts.diagnostics.clone());
        let document_ready = self.document_ready();
        let inventory_ready = self.vault.inventory_scanned;
        let note_count = self.vault.notes.len();
        if let Some(trace) = &diagnostics {
            trace.once(if inventory_ready { "inventory_ui_sync" } else { "bootstrap_ui_sync" }, serde_json::json!({ "duration_ms": sync_started.elapsed().as_secs_f64() * 1000., "notes": note_count }));
        }
        let available = if self.body_bounds.size.width > px(0.) {
            f32::from(
                (self.body_bounds.size.width + window.viewport_size().width
                    - self.body_viewport_width)
                    .max(px(0.)),
            )
        } else {
            f32::from(window.viewport_size().width)
        };
        let overlay = reader_layout::overlay(available);
        self.panels
            .viewport_changed(f32::from(self.body_bounds.size.width), available);
        let workspace_panels = self.workspace_panels();
        let panel_widths = workspace_panels.widths(&self.panel_widths, available);
        self.sync_project_board_visibility(window, cx);
        #[cfg(native_manager)]
        if let Some(project) = &self.project {
            let visible = self.project_open
                && self.project_navigation.active.is_some()
                && workspace_panels.visible(reader_layout::Panel::Backlinks, available)
                && !self.project_issue_peek_visible(cx);
            project.update(cx, |project, cx| {
                project.manager_surface_visible(visible, cx)
            });
        }
        let frame = RenderFrame {
            sheet_layer: sheet_layer.map(IntoElement::into_any_element),
            dialog_layer: dialog_layer.map(IntoElement::into_any_element),
            notification_layer: notification_layer.map(IntoElement::into_any_element),
            diagnostics,
            document_ready,
            inventory_ready,
            note_count,
            available,
            overlay,
            workspace_panels,
            panel_widths,
        };
        // The nested document draw starts from this small frame. The window
        // composition below has its own frame and receives the finished
        // element, so its builder temporaries are not reserved underneath
        // every nested file/source render on the native main stack (#2825).
        let center = self.render_project_center(window, cx);
        let header = self.render_window_header(frame.available, window, cx);
        self.compose_reader_window(frame, center, header, window, cx)
    }
}

/// Values computed before the document center is built, consumed by the
/// window composition in the same draw.
struct RenderFrame {
    sheet_layer: Option<AnyElement>,
    dialog_layer: Option<AnyElement>,
    notification_layer: Option<AnyElement>,
    diagnostics: Option<reader_diagnostics::Trace>,
    document_ready: bool,
    inventory_ready: bool,
    note_count: usize,
    available: f32,
    overlay: bool,
    workspace_panels: reader_layout::Panels,
    panel_widths: reader_layout::Widths,
}

impl Reader {
    #[inline(never)]
    fn render_window_header(
        &self,
        available: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        #[cfg(target_os = "linux")]
        let board_cache = self.board_manager_header_cache(available);
        #[cfg(not(target_os = "linux"))]
        let board_cache = false;
        let edit_cache = self.pane.editing.is_some() && reader_side_panel::cache_panels();
        if board_cache || edit_cache {
            let viewport_width = f32::from(window.viewport_size().width);
            let scale_factor = window.scale_factor();
            let theme_mode = cx.theme().mode;
            self.header_cache.update(cx, |header, cx| {
                header.sync(available, viewport_width, scale_factor, theme_mode, cx)
            });
            return self
                .header_cache
                .clone()
                .cached(
                    gpui::StyleRefinement::default()
                        .w_full()
                        .h(px(READER_HEADER_HEIGHT))
                        .flex_shrink_0(),
                )
                .into_any_element();
        }
        self.render_header(available, window, cx)
    }

    /// The document column and its overlays, in their own frame (#2825).
    #[inline(never)]
    fn render_document_column(
        &self,
        center: AnyElement,
        overlay: bool,
        panel_widths: &reader_layout::Widths,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        v_flex()
            .id("reader-document")
            .map(|view| reader_text_zoom::document(view, cx))
            .debug_selector(|| "reader-document".into())
            .ml(px(if overlay { 0. } else { panel_widths.notes }))
            .mr(px(if overlay { 0. } else { panel_widths.backlinks }))
            .flex_1()
            .h_full()
            .min_w(px(0.))
            .overflow_hidden()
            .relative()
            .child(
                div()
                    .size_full()
                    // Reveal only a positioned document. Source needs one
                    // layout; a prepared reader is positioned before layout.
                    .when(self.restoring_source() || self.restoring_reader(), |view| {
                        view.opacity(0.)
                    })
                    .when(
                        self.agent_document.is_some() && !self.project_navigation.board_visible,
                        |view| view.flex_1().min_h_0(),
                    )
                    .child(center),
            )
            .children(self.render_agent_document(cx))
            .when(
                self.project_open || self.project_navigation.active.is_some(),
                |view| view.children(self.render_agents_rules_offer(cx)),
            )
            .when(self.pane.find_open, |s| s.child(self.render_find_bar(cx)))
            .children(reader_text_zoom::hud(self, cx))
            .into_any_element()
    }

    #[inline(never)]
    fn compose_reader_window(
        &mut self,
        frame: RenderFrame,
        center: AnyElement,
        window_header: AnyElement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let RenderFrame {
            sheet_layer,
            dialog_layer,
            notification_layer,
            diagnostics,
            document_ready,
            inventory_ready,
            note_count,
            available,
            overlay,
            workspace_panels,
            panel_widths,
        } = frame;
        let bounds_view = cx.entity().downgrade();
        let main = self.render_document_column(center, overlay, &panel_widths, cx);
        let mut panel_bodies =
            [reader_layout::Panel::Notes, reader_layout::Panel::Backlinks].map(|panel| {
                workspace_panels.visible(panel, available).then(|| {
                    if panel == reader_layout::Panel::Backlinks
                        && self.project_navigation.active.is_some()
                    {
                        self.render_manager_panel(cx)
                    } else {
                        self.side_panel(panel == reader_layout::Panel::Notes)
                    }
                })
            });
        // A mounted native conversation supplies its own identity/status/actions.
        // Do not stack the Reader's generic Manager title above that header.
        let manager_headerless = self.project_navigation.active.is_some() && self.project.is_some();
        let panels: Vec<_> = [reader_layout::Panel::Notes, reader_layout::Panel::Backlinks]
            .into_iter()
            .filter(|panel| workspace_panels.visible(*panel, available))
            .map(|panel| {
                let left = panel == reader_layout::Panel::Notes;
                let resize_view = cx.entity().downgrade();
                let contextual_header =
                    self.project_navigation.active.is_some() || self.active_timeline().is_some();
                let header = (left || (!manager_headerless && contextual_header))
                    .then(|| self.render_panel_header(panel, panel_widths.get(panel), window, cx));
                v_flex()
                    .id(if left {
                        "reader-notes-panel"
                    } else {
                        "reader-backlinks-panel"
                    })
                    .debug_selector(move || {
                        if left {
                            "reader-notes-panel".into()
                        } else {
                            "reader-backlinks-panel".into()
                        }
                    })
                    // Overlay panel clicks must not reach the dismiss backdrop.
                    .occlude()
                    .w(px(panel_widths.get(panel)))
                    .absolute()
                    .top_0()
                    .h_full()
                    .when(left, |p| p.left_0().border_r_1())
                    .when(!left, |p| p.right_0().border_l_1())
                    .bg(if left {
                        brand::palette(cx).sidebar
                    } else {
                        cx.theme().background
                    })
                    .border_color(brand::palette(cx).border_subtle)
                    .when(overlay, |p| p.shadow_lg())
                    .children(header)
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .children(panel_bodies[usize::from(!left)].take()),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .h_full()
                            .w(px(0.))
                            .debug_selector(move || {
                                if left {
                                    "reader-notes-splitter-anchor".into()
                                } else {
                                    "reader-backlinks-splitter-anchor".into()
                                }
                            })
                            .when(left, |handle| handle.right_0())
                            .when(!left, |handle| handle.left_0())
                            .child(
                                canvas(
                                    |_, _, _| (),
                                    |bounds, _, window, cx| {
                                        gpui_base::TextSelection::preserve_on_pointer_down(
                                            bounds, window, cx,
                                        );
                                    },
                                )
                                .absolute()
                                .left(px(-4.))
                                .w(px(8.))
                                .h_full(),
                            )
                            .child({
                                let id = if left {
                                    "reader-notes-resize"
                                } else {
                                    "reader-backlinks-resize"
                                };
                                #[cfg(target_os = "linux")]
                                let handle = reader_panel_resize::handle(
                                    id,
                                    self.resizing_panel == Some(panel),
                                    self.panel_capture.clone(),
                                    move |_, _, cx| {
                                        let _ = resize_view.update(cx, |this, cx| {
                                            this.resizing_panel = Some(panel);
                                            cx.notify();
                                        });
                                    },
                                );
                                #[cfg(not(target_os = "linux"))]
                                let handle = gpui_base::resize_handle(id, Axis::Horizontal)
                                    .on_drag(ReaderPanelDrag, move |drag, _, _, cx| {
                                        let _ = resize_view.update(cx, |this, cx| {
                                            this.resizing_panel = Some(panel);
                                            cx.notify();
                                        });
                                        cx.new(|_| drag.as_ref().clone())
                                    });
                                handle
                            }),
                    )
                    .into_any_element()
            })
            .collect();
        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .relative()
            .key_context(
                if self.file_editor_active() && !self.quick_open.open && !self.shortcut_sheet.open {
                    "Reader ReaderCode ReaderFileEdit"
                } else if self.pane.editing.is_none()
                    && !self.quick_open.open
                    && !self.shortcut_sheet.open
                    && self.project_navigation.section == reader_project_navigation::Section::Reader
                    && self
                        .pane
                        .file_preview
                        .as_ref()
                        .is_some_and(|preview| preview.code.is_some())
                {
                    "Reader ReaderCode"
                } else if self
                    .active_timeline()
                    .is_some_and(|t| t.selected.is_some() || self.pane.editing.is_none())
                    && !self.quick_open.open
                    && !self.shortcut_sheet.open
                {
                    if self.read_copy_available() {
                        "Reader ReaderHistory ReaderRead"
                    } else {
                        "Reader ReaderHistory"
                    }
                } else if self.read_copy_available() {
                    "Reader ReaderRead"
                } else {
                    READER_CONTEXT
                },
            )
            .track_focus(&self.focus_handle)
            .map(|view| self.code_action_listeners(view, cx))
            .on_action(
                cx.listener(|this, _: &reader_read_menu::CopyAsMarkdown, window, cx| {
                    this.copy_read_markdown(window, cx)
                }),
            )
            .on_action(cx.listener(
                |this, _: &reader_read_menu::CopyWithFormatting, window, cx| {
                    this.copy_read_formatted(window, cx)
                },
            ))
            .on_action(
                cx.listener(|this, _: &reader_read_menu::CopyAsJira, window, cx| {
                    this.copy_as_jira(window, cx)
                }),
            )
            .map(|view| reader_text_zoom::reader(view, cx))
            .on_action(cx.listener(
                |reader, action: &reader_image_actions::ImageCommand, window, cx| {
                    reader.image_command(action, window, cx)
                },
            ))
            .on_action(
                cx.listener(|this, _: &reader_reminder::CopyLinkToLine, window, cx| {
                    this.copy_link_to_line(window, cx)
                }),
            )
            .on_action(cx.listener(
                |this, action: &reader_reminder::RemindOnSelection, window, cx| {
                    #[cfg(any(unix, windows))]
                    this.remind_on_editor_selection(action, window, cx);
                    #[cfg(not(any(unix, windows)))]
                    let _ = (this, action, window, cx);
                },
            ))
            .on_action(cx.listener(|this, _: &QuickOpen, window, cx| {
                this.open_quick_open(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SwitchWindowMode, window, cx| {
                this.switch_window_mode(window, cx);
            }))
            .on_action(cx.listener(|this, _: &FullTextSearch, window, cx| {
                this.open_quick_open(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleShortcutSheet, window, cx| {
                this.toggle_shortcut_sheet(window, cx)
            }))
            .on_action(cx.listener(|this, _: &RecentOlder, _, cx| this.cycle_recent(1, cx)))
            .on_action(cx.listener(|this, _: &RecentNewer, _, cx| this.cycle_recent(-1, cx)))
            .on_action(cx.listener(|this, _: &PaletteNext, _, cx| this.move_quick_open(1, cx)))
            .on_action(cx.listener(|this, _: &PalettePrevious, _, cx| this.move_quick_open(-1, cx)))
            .on_action(cx.listener(|this, _: &NewNote, window, cx| this.new_note(None, window, cx)))
            .on_action(
                cx.listener(|this, _: &NewFolder, window, cx| this.new_folder(None, window, cx)),
            )
            .on_action(cx.listener(|this, _: &RenameNote, window, cx| this.rename_note(window, cx)))
            .map(|view| {
                #[cfg(any(unix, windows))]
                let view = view.on_action(cx.listener(
                    |this, _: &reader_move_picker::MoveToFolder, window, cx| {
                        if this.tree_focus.contains_focused(window, cx)
                            && this.tree.selected_paths().len() > 1
                        {
                            this.open_batch_dialog(reader_batch::Kind::Move, window, cx);
                        } else {
                            this.choose_move_folder(this.selected_file().to_owned(), window, cx);
                        }
                    },
                ));
                view
            })
            .when(cfg!(any(unix, windows)), |view| {
                #[cfg(any(unix, windows))]
                let view = view.on_action(cx.listener(|this, _: &RenameTreeNote, window, cx| {
                    this.rename_tree_note(window, cx)
                }));
                view
            })
            .on_action(cx.listener(|this, _: &HistoryVersionNext, window, cx| {
                this.step_timeline(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &HistoryVersionPrevious, window, cx| {
                this.step_timeline(false, window, cx)
            }))
            .when(cfg!(any(unix, windows)), |view| {
                #[cfg(any(unix, windows))]
                let view = view.on_action(
                    cx.listener(|this, _: &DeleteNote, window, cx| this.delete_note(window, cx)),
                );
                view
            })
            .on_action(cx.listener(|this, _: &NoteSourceHistory, window, cx| {
                this.source_history(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &RecoverUnsavedNotes, window, cx| {
                this.source_history(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &RecoverLinkMoves, window, cx| {
                this.recover_link_moves(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ToggleSource, window, cx| this.toggle_source(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleSoftWrap, window, cx| {
                this.toggle_code_soft_wrap(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ToggleTask, window, cx| {
                    this.toggle_task_at_caret(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &OpenLivePreview, window, cx| {
                this.open_live_preview(window, cx)
            }))
            .on_action(cx.listener(|this, _: &SaveSource, window, cx| {
                // An untitled draft has no place yet: Save asks for one (#1528).
                if this.current_draft().is_some() {
                    this.save_draft_as(window, cx);
                } else {
                    this.request_source_save(cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &NewUntitled, window, cx| this.new_untitled(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ShowRecentFiles, window, cx| {
                this.toggle_recent_gallery(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &reader_editor::FormatCode, window, cx| {
                    this.run_format_code(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &reader_editor::ValidateCode, window, cx| {
                    this.run_validate_code(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &reader_editor::RepairCode, window, cx| {
                    this.repair_code_entry(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &RevealFile, window, cx| {
                this.file_action(reader_files::FileAction::Reveal, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CopyVaultPath, window, cx| {
                this.file_action(reader_files::FileAction::Relative, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CopyFileContent, window, cx| {
                this.copy_file_content(window, cx)
            }))
            .on_action(cx.listener(|this, _: &CopyFilePath, window, cx| {
                this.file_action(reader_files::FileAction::Absolute, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CopyFileWikilink, window, cx| {
                this.file_action(reader_files::FileAction::Wiki, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &project_terminal::OpenInTerminal, window, cx| {
                    this.file_action(reader_files::FileAction::Terminal, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &OpenFileDefault, window, cx| {
                this.file_action(reader_files::FileAction::Open, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &open_in::OpenInDefaultEditor, window, cx| {
                    this.open_in_default_editor(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &QuickLookFile, window, cx| {
                this.file_action(reader_files::FileAction::QuickLook, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CanvasFit, window, cx| {
                this.invoke_viewer_action("canvas-fit", window, cx)
            }))
            .on_action(cx.listener(|this, _: &CanvasZoomIn, window, cx| {
                this.invoke_viewer_action("canvas-plus", window, cx)
            }))
            .on_action(cx.listener(|this, _: &CanvasZoomOut, window, cx| {
                this.invoke_viewer_action("canvas-minus", window, cx)
            }))
            .on_action(cx.listener(|this, _: &ArchiveExtractSelected, window, cx| {
                this.invoke_viewer_action("archive-extract-selected", window, cx)
            }))
            .on_action(cx.listener(|this, _: &ArchiveExtractAll, window, cx| {
                this.invoke_viewer_action("archive-extract-all", window, cx)
            }))
            .on_action(cx.listener(|this, _: &ExpandDrawing, window, cx| {
                this.invoke_viewer_action("drawing-expand", window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenDrawingExternally, window, cx| {
                this.invoke_viewer_action("drawing-edit", window, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowDrawingDetails, window, cx| {
                this.invoke_viewer_action("drawing-diagnostic-details", window, cx)
            }))
            .on_action(cx.listener(|this, _: &CopyDrawingDetails, window, cx| {
                this.invoke_viewer_action("drawing-copy-details", window, cx)
            }))
            .on_action(cx.listener(|this, _: &CopyViewerImage, window, cx| {
                this.copy_preview_image(window, cx)
            }))
            .on_action(cx.listener(|this, _: &FindInNote, window, cx| this.open_find(window, cx)))
            .on_action(cx.listener(|this, _: &ReaderScrollDown, window, cx| {
                this.scroll_reader_key(1., false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ReaderScrollUp, window, cx| {
                this.scroll_reader_key(-1., false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ReaderPageDown, window, cx| {
                this.scroll_reader_key(1., true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ReaderPageUp, window, cx| {
                this.scroll_reader_key(-1., true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FindNext, _, cx| this.find_step(1, cx)))
            .on_action(cx.listener(|this, _: &FindPrev, _, cx| this.find_step(-1, cx)))
            .on_action(cx.listener(|this, _: &UndoTrash, window, cx| {
                if this.tree_focus.contains_focused(window, cx) && this.creation_undo.is_some() {
                    this.undo_creation(None, window, cx);
                } else {
                    #[cfg(any(unix, windows))]
                    this.undo_last_trash(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &RedoTrash, window, cx| {
                #[cfg(any(unix, windows))]
                this.redo_last_trash(window, cx);
            }))
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.dismiss(window, cx)))
            .on_action(
                cx.listener(|this, _: &TreePreview, window, cx| {
                    this.toggle_tree_preview(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &TreeDown, w, cx| this.tree_key(TreeKey::Down, w, cx)))
            .on_action(cx.listener(|this, _: &TreeUp, w, cx| this.tree_key(TreeKey::Up, w, cx)))
            .on_action(
                cx.listener(|this, _: &TreeRight, w, cx| this.tree_key(TreeKey::Right, w, cx)),
            )
            .on_action(cx.listener(|this, _: &TreeLeft, w, cx| this.tree_key(TreeKey::Left, w, cx)))
            .on_action(cx.listener(|this, _: &TreeOpen, w, cx| this.tree_key(TreeKey::Open, w, cx)))
            .on_action(cx.listener(|this, _: &TreeReturn, w, cx| this.tree_return(w, cx)))
            .on_action(cx.listener(|this, _: &TreeExpandSubtree, w, cx| {
                this.tree_key(TreeKey::ExpandSubtree, w, cx)
            }))
            .on_action(cx.listener(|this, _: &TreeCollapseSubtree, w, cx| {
                this.tree_key(TreeKey::CollapseSubtree, w, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ToggleLeftMode, window, cx| {
                    this.toggle_left_mode(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &CollapseFolders, _, cx| this.collapse_folders(cx)))
            .on_action(
                cx.listener(|this, _: &FocusCurrentFolder, w, cx| this.focus_current_folder(w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ToggleHiddenFiles, _, cx| this.toggle_hidden_files(cx)),
            )
            .on_action(cx.listener(|this, _: &NewFromTemplate, window, cx| {
                #[cfg(any(unix, windows))]
                this.new_from_template(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenDailyNote, window, cx| {
                #[cfg(any(unix, windows))]
                this.open_daily_note(window, cx);
                #[cfg(not(any(unix, windows)))]
                let _ = (this, window, cx);
            }))
            .on_action(cx.listener(|_, _: &reader_settings::OpenSettings, _, cx| {
                reader_settings::show(Some(cx.entity().downgrade()), cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(
                |this, _: &reader_service_storage::OpenServiceStorage, window, cx| {
                    this.open_service_storage(window, cx);
                    cx.stop_propagation();
                },
            ))
            .on_action(
                cx.listener(|this, _: &project_terminal::OpenTerminal, window, cx| {
                    this.open_project_terminal(window, cx);
                }),
            )
            .on_action(cx.listener(|this, _: &ToggleProject, window, cx| {
                if this.project_navigation.active.is_some() {
                    this.leave_vault_project(window, cx);
                } else {
                    this.show_app_section(reader_project_navigation::Section::Projects, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleNotes, window, cx| {
                this.toggle_panel(reader_layout::Panel::Notes, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ToggleSplit, window, cx| this.toggle_split(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &FocusOtherPane, window, cx| {
                    this.focus_other_pane(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ToggleBacklinks, window, cx| {
                this.toggle_panel(reader_layout::Panel::Backlinks, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &MaximizeNote, window, cx| this.maximize_note(window, cx)),
            )
            .on_action(cx.listener(|this, _: &CloseNote, window, cx| this.close_note(window, cx)))
            .on_action(
                cx.listener(|this, _: &HistoryBack, window, cx| this.history_move(-1, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &HistoryForward, window, cx| {
                    this.history_move(1, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ListNext, window, cx| this.list_move(1, window, cx)))
            .on_action(cx.listener(|this, _: &ListPrev, window, cx| this.list_move(-1, window, cx)))
            .on_action(cx.listener(|this, _: &ScrollTop, _, cx| {
                this.visible_markdown()
                    .read(cx)
                    .list_state()
                    .scroll_to_reveal_item(0);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ScrollBottom, _, cx| {
                this.visible_markdown()
                    .read(cx)
                    .list_state()
                    .scroll_to_end();
                cx.notify();
            }))
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .map(|view| {
                let header = window_header;
                let view = if self.embedded_in_workspace {
                    // Workspace owns the window title bar; this is a plain row.
                    view.child(
                        div()
                            .w_full()
                            .flex_none()
                            .border_b_1()
                            .border_color(brand::palette(cx).border_subtle)
                            .child(header),
                    )
                } else {
                    // The vault colour tints the whole bar (#1050), fading when
                    // the window is not focused.
                    let tint = reader_ui_state::title_tint(&self.vault_root, cx).map(|color| {
                        color.title_tint(
                            brand::palette(cx).surface,
                            cx.theme().is_dark(),
                            window.is_window_active(),
                        )
                    });
                    // GNOME draws our own close button; it closes like the
                    // platform request, never by bare removal (#2805).
                    let close_target = cx.entity().downgrade();
                    view.child(
                        TitleBar::new()
                            .on_close_window(move |_, window, cx| {
                                if reader_open::request_reader_close(
                                    &close_target,
                                    "title bar close button",
                                    window,
                                    cx,
                                ) {
                                    window.remove_window();
                                }
                            })
                            .h(px(READER_HEADER_HEIGHT))
                            .when_some(tint, |bar, tint| bar.bg(tint))
                            // Traffic lights are hidden in full screen; drop
                            // the space the toolkit reserves for them (#365).
                            .when(window.is_fullscreen(), |bar| bar.pl(px(10.)))
                            .child(header),
                    )
                };
                let accent =
                    reader_ui_state::vault_color(&self.vault_root, cx).map(|color| color.color(cx));
                view.when_some(accent, |view, color| {
                    // Drawn over the top edge, so the layout never moves (#774).
                    view.child(
                        div()
                            .debug_selector(|| "vault-accent-line".into())
                            .absolute()
                            .top_0()
                            .left_0()
                            .w_full()
                            .h(px(VAULT_ACCENT_LINE))
                            .bg(color),
                    )
                })
            })
            .children({
                #[cfg(any(target_os = "linux", target_os = "macos", windows))]
                {
                    Some(reader_settings_sync::presentation::approval_banner(cx))
                }
                #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
                {
                    None::<AnyElement>
                }
            })
            .child(
                h_flex()
                    .id("reader-body")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_hidden()
                    // Keep this child and its TextView entity mounted across panel/width changes.
                    .child(main)
                    .when(
                        overlay && (panel_widths.notes > 0. || panel_widths.backlinks > 0.),
                        |body| {
                            body.child(
                                div()
                                    .id("reader-panel-backdrop")
                                    .debug_selector(|| "reader-panel-backdrop".into())
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .size_full()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, window, cx| {
                                            let panel = this.panels.dismiss_target(f32::from(
                                                this.body_bounds.size.width,
                                            ));
                                            this.close_panel(panel, window, cx);
                                            cx.stop_propagation();
                                        }),
                                    )
                                    .on_scroll_wheel(|_, _, cx| cx.stop_propagation()),
                            )
                        },
                    )
                    .children(panels)
                    .children(self.render_table_overlay(cx))
                    .child(
                        canvas(
                            |_, _, _| (),
                            move |bounds, _, window, cx| {
                                if let Some(trace) = &diagnostics {
                                    trace.once("reader_first_paint", serde_json::json!({}));
                                    if document_ready {
                                        trace.once(
                                            "document_first_paint",
                                            serde_json::json!({ "notes": note_count }),
                                        );
                                    }
                                    if document_ready && inventory_ready {
                                        trace.once(
                                            "inventory_first_paint",
                                            serde_json::json!({ "notes": note_count }),
                                        );
                                    }
                                }
                                let view = bounds_view.clone();
                                let _ = view.update(cx, |this, cx| {
                                    this.body_viewport_width = window.viewport_size().width;
                                    if this.body_bounds != bounds {
                                        this.resizing_panel = None;
                                        this.body_bounds = bounds;
                                        if !this.panels.visible(
                                            reader_layout::Panel::Notes,
                                            f32::from(bounds.size.width),
                                        ) && this.sidebar_search_focus.is_focused(window)
                                        {
                                            this.focus_document(window, cx);
                                        }
                                        cx.defer_in(window, |_, window, cx| {
                                            cx.notify();
                                            window.refresh();
                                        });
                                    }
                                    if this.perf_probe.is_some() {
                                        this.drive_perf_probe(document_ready, window, cx);
                                    }
                                });
                                let drag_view = view.clone();
                                window.on_mouse_event(
                                    move |event: &MouseMoveEvent, phase, window, cx| {
                                        #[cfg(not(target_os = "linux"))]
                                        let _ = window;
                                        if !phase.bubble() {
                                            return;
                                        }
                                        let _ = drag_view.update(cx, |this, cx| {
                                            let Some(panel) = this.resizing_panel else {
                                                return;
                                            };
                                            #[cfg(target_os = "linux")]
                                            let dragging =
                                                event.pressed_button == Some(MouseButton::Left);
                                            #[cfg(not(target_os = "linux"))]
                                            let dragging = event.dragging() && cx.has_active_drag();
                                            if !dragging
                                                || !this.panels.visible(
                                                    panel,
                                                    f32::from(this.body_bounds.size.width),
                                                )
                                            {
                                                this.resizing_panel = None;
                                                #[cfg(target_os = "linux")]
                                                {
                                                    this.release_panel_pointer(window);
                                                    cx.notify();
                                                }
                                                return;
                                            }
                                            let requested = if panel == reader_layout::Panel::Notes
                                            {
                                                event.position.x - this.body_bounds.left()
                                            } else {
                                                this.body_bounds.right() - event.position.x
                                            };
                                            let width = this.workspace_panels().resize_width(
                                                panel,
                                                f32::from(requested),
                                                &this.panel_widths,
                                                f32::from(this.body_bounds.size.width),
                                            );
                                            this.set_panel_width(panel, width);
                                            #[cfg(target_os = "linux")]
                                            if this.board_manager_header_cache(f32::from(
                                                this.body_bounds.size.width,
                                            )) {
                                                // Geometry changed; Reader source observers and
                                                // the fixed header have no new presentation data.
                                                this.layout_signal.update(cx, |_, cx| cx.notify());
                                            } else {
                                                cx.notify();
                                            }
                                            #[cfg(not(target_os = "linux"))]
                                            cx.notify();
                                        });
                                    },
                                );
                                #[cfg(target_os = "linux")]
                                {
                                    let exit_view = view.clone();
                                    window.on_mouse_event(
                                        move |_: &gpui::MouseExitEvent, phase, window, cx| {
                                            if phase.capture() {
                                                let _ = exit_view.update(cx, |this, cx| {
                                                    if this.resizing_panel.take().is_some() {
                                                        this.release_panel_pointer(window);
                                                        cx.notify();
                                                    }
                                                });
                                            }
                                        },
                                    );
                                }
                                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                                    if !phase.bubble() {
                                        return;
                                    }
                                    let _ = view.update(cx, |this, cx| {
                                        let Some(panel) = this.resizing_panel.take() else {
                                            return;
                                        };
                                        this.persist_panel_width(panel);
                                        cx.notify();
                                    });
                                });
                            },
                        )
                        .absolute()
                        .size_full(),
                    ),
            )
            .when(self.recent_switcher.open(), |view| {
                view.child(self.render_recent(cx))
            })
            .when(self.quick_open.open, |view| {
                view.child(self.render_quick_open(cx))
            })
            .when(reader_ui_state::installed(cx), |view| {
                view.child(reader_ui_state::capture_scroll(cx.entity().downgrade()))
            })
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                reader_ui_state::capture_next_frame(cx.entity().downgrade(), window);
                // Control is still held during the MRU gesture, so the plain
                // Escape action binding does not match this physical key.
                if this.recent_switcher.open() && event.keystroke.key == "escape" {
                    this.recent_switcher.cancel();
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .when(self.shortcut_sheet.open, |view| {
                view.child(self.render_shortcut_sheet(cx))
            })
            .on_modifiers_changed(cx.listener(
                |this, event: &gpui::ModifiersChangedEvent, window, cx| {
                    this.hover_modifiers(window, cx);
                    if !event.modifiers.control {
                        this.queue_recent_release(window, cx);
                    } else {
                        this.recent_switcher.invalidate_release();
                    }
                },
            ))
            .capture_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, _, cx| {
                if this.pane.hover_preview.is_active()
                    && !this.pane.hover_preview.contains(event.position)
                {
                    this.clear_hover(cx);
                }
            }))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                if this.pane.hover_preview.is_active()
                    && !this.pane.hover_preview.contains(event.position)
                {
                    this.clear_hover(cx);
                }
            }))
            .children(self.render_hover_preview(window, cx))
            .map(|view| {
                #[cfg(any(unix, windows))]
                let view = view.children(self.render_move_picker());
                view
            })
            .children(sheet_layer)
            .children(dialog_layer)
            .children(notification_layer)
            .children(self.file_menu.as_ref().map(|(menu, position)| {
                deferred(anchored().position(*position).child(menu.clone()))
                    .with_priority(gpui_base::POPUP_PRIORITY)
            }))
            .children(self.render_title_nav_overlay(cx))
            .map(|view| {
                #[cfg(all(target_os = "linux", feature = "project-ui-harness"))]
                let view = project::performance::Trace::new(view.into_any_element(), 2);
                view
            })
            .into_any_element()
    }
}
