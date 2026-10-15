//! Explicit, lazily opened user settings. No vault scans or startup reads.
use super::*;
use gpui_component::{button::ButtonGroup, switch::Switch, ThemeMode};
use okilum_core::log::source_link;

gpui::actions!(okilum_settings, [OpenSettings, CloseSettings]);

#[derive(Default)]
struct SettingsWindow(Option<WindowHandle<Root>>);
impl Global for SettingsWindow {}

pub(crate) fn install(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-,", OpenSettings, None),
        KeyBinding::new("secondary-w", CloseSettings, Some("OkilumSettings")),
    ]);
    cx.on_action(|_: &OpenSettings, cx| show(None, cx));
}

pub(crate) fn show(reader: Option<WeakEntity<Reader>>, cx: &mut App) {
    show_section(reader, None, cx);
}

pub(crate) fn show_about(cx: &mut App) {
    show_section(None, Some(Section::About), cx);
}

fn show_section(reader: Option<WeakEntity<Reader>>, section: Option<Section>, cx: &mut App) {
    // Native application-menu actions may bypass the Reader's action handler.
    let active = cx.active_window();
    // Menu dispatch can still hold the active Reader's update borrow.
    cx.defer(move |cx| {
        let reader = reader.or_else(|| {
            let root = active?.downcast::<Root>()?;
            let reader = root
                .read(cx)
                .ok()?
                .view()
                .clone()
                .downcast::<Reader>()
                .ok()?;
            Some(reader.downgrade())
        });
        if let Some(handle) = cx.try_global::<SettingsWindow>().and_then(|s| s.0) {
            if handle
                .update(cx, |root, window, cx| {
                    if let Ok(settings) = root.view().clone().downcast::<Settings>() {
                        settings.update(cx, |settings, cx| {
                            if let Some(reader) = reader.clone() {
                                settings.set_reader(Some(reader), cx);
                            }
                            if let Some(section) = section {
                                settings.section = section;
                                cx.notify();
                            }
                        });
                    }
                    window.activate_window();
                })
                .is_ok()
            {
                return;
            }
        }
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(super::window_state::default_bounds(
                size(px(780.), px(520.)),
                cx,
            ))),
            window_min_size: Some(size(px(700.), px(440.))),
            ..Default::default()
        };
        match cx.open_window(options, move |window, cx| {
            window.set_window_title("Okilum Settings");
            let settings = cx.new(|cx| {
                let mut settings = Settings::new(reader, cx);
                if let Some(section) = section {
                    settings.section = section;
                }
                settings
            });
            settings.read(cx).focus.clone().focus(window, cx);
            cx.new(|cx| Root::new(settings, window, cx))
        }) {
            Ok(handle) => cx.set_global(SettingsWindow(Some(handle))),
            Err(error) => eprintln!("Could not open Settings: {error}"),
        }
    });
}

pub(super) fn setting_row(
    label: &'static str,
    help: &'static str,
    control: impl IntoElement,
    cx: &App,
) -> impl IntoElement {
    h_flex()
        .w_full()
        .justify_between()
        .gap_4()
        .py_2()
        .child(
            v_flex().flex_1().min_w_0().gap_1().child(label).child(
                // Help wraps: a cut-off sentence can hide the part that
                // matters, such as the quiet hours (#957).
                div()
                    .debug_selector(move || format!("setting-help-{label}"))
                    .text_sm()
                    .text_color(brand::palette(cx).text_muted)
                    .child(help),
            ),
        )
        .child(div().flex_none().child(control))
}

/// #774: «None» and the presets as swatches; the current one is ringed.
fn vault_color_swatches(root: &Path, cx: &App) -> impl IntoElement {
    let p = brand::palette(cx);
    let current = reader_ui_state::vault_color(root, cx);
    let swatch = |color: Option<brand::VaultColor>| {
        let root = root.to_owned();
        let label = color.map_or("None", brand::VaultColor::label);
        let selected = current == color;
        div()
            .id(SharedString::from(format!(
                "settings-vault-color-{}",
                color.map_or("none", brand::VaultColor::key)
            )))
            .debug_selector(move || {
                format!(
                    "settings-vault-color-{}",
                    color.map_or("none", brand::VaultColor::key)
                )
            })
            .flex_none()
            .size(px(22.))
            .p(px(2.))
            .rounded_full()
            .border_2()
            .border_color(if selected {
                p.focus
            } else {
                gpui::transparent_black()
            })
            .cursor_pointer()
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(label).build(window, cx)
            })
            .child(div().size_full().rounded_full().map(|dot| match color {
                Some(color) => dot.bg(color.color(cx)),
                None => dot.border_1().border_color(p.text_muted),
            }))
            .on_click(move |_, _, cx| reader_ui_state::set_vault_color(&root, color, cx))
    };
    h_flex()
        .gap_1()
        .child(swatch(None))
        .children(brand::VaultColor::ALL.map(|color| swatch(Some(color))))
}

fn compact_vault_path(root: &Path) -> String {
    let home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    let display = home
        .as_ref()
        .and_then(|home| root.strip_prefix(home).ok())
        .map(|relative| format!("~/{}", relative.to_string_lossy()))
        .unwrap_or_else(|| root.to_string_lossy().into_owned());
    if display.starts_with("~/") && display.chars().count() <= 36 {
        return display;
    }
    let name = root.file_name().unwrap_or_default().to_string_lossy();
    format!(
        "{}…/{name}",
        if display.starts_with("~/") { "~/" } else { "" }
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Appearance,
    Files,
    Updates,
    About,
    Inbox,
    #[cfg(target_os = "linux")]
    Sync,
}
impl Section {
    fn icon(self) -> Icon {
        match self {
            Self::Appearance => Icon::new(IconName::Palette),
            Self::Files => Icon::new(IconName::Folder),
            Self::Updates => Icon::default().path("icons/arrow-down-circle.svg"),
            Self::About => Icon::new(IconName::Info),
            Self::Inbox => Icon::new(IconName::Inbox),
            #[cfg(target_os = "linux")]
            Self::Sync => Icon::new(IconName::RotateCw),
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Files => "Files",
            Self::Updates => "Updates",
            Self::About => "About",
            Self::Inbox => "Inbox",
            #[cfg(target_os = "linux")]
            Self::Sync => "Sync",
        }
    }
}
struct Settings {
    #[cfg(target_os = "linux")]
    sync: Option<Entity<reader_settings_sync::SyncSettings>>,
    section: Section,
    theme_picker: Entity<theme_picker::ThemePicker>,
    #[cfg(all(target_os = "linux", feature = "settings-ui-harness"))]
    preview_beta: Option<bool>,
    reader: Option<WeakEntity<Reader>>,
    focus: FocusHandle,
    _reader_changes: Option<Subscription>,
    #[cfg(any(unix, windows))]
    template_root: Option<PathBuf>,
    #[cfg(any(unix, windows))]
    template_folder: String,
    #[cfg(any(unix, windows))]
    template_error: Option<String>,
    #[cfg(any(unix, windows))]
    template_pending: bool,
    #[cfg(any(unix, windows))]
    template_epoch: u64,
    /// The custom editor command line for log source links (#1165), made
    /// on first render because an input needs the window.
    editor_command: Option<(Entity<InputState>, Subscription)>,
    /// The build path of a new source-link mapping (#1165).
    map_from: Option<Entity<InputState>>,
}
impl Settings {
    fn new(reader: Option<WeakEntity<Reader>>, cx: &mut Context<Self>) -> Self {
        let observer = reader
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .map(|reader| cx.observe(&reader, |_, _, cx| cx.notify()));
        Self {
            section: Section::Appearance,
            #[cfg(target_os = "linux")]
            sync: None,
            theme_picker: cx.new(|cx| theme_picker::ThemePicker::new(None, cx)),
            #[cfg(all(target_os = "linux", feature = "settings-ui-harness"))]
            preview_beta: match std::env::var("OKILUM_DEBUG_UPDATER_UI").as_deref() {
                Ok("sparkle" | "velopack") => Some(false),
                _ => None,
            },
            reader,
            focus: cx.focus_handle(),
            _reader_changes: observer,
            editor_command: None,
            map_from: None,
            #[cfg(any(unix, windows))]
            template_root: None,
            #[cfg(any(unix, windows))]
            template_folder: reader_templates::DEFAULT_FOLDER.into(),
            #[cfg(any(unix, windows))]
            template_error: None,
            #[cfg(any(unix, windows))]
            template_pending: false,
            #[cfg(any(unix, windows))]
            template_epoch: 0,
        }
    }
    // The harness renders the production control tree, with in-memory actions.
    // It never initializes Sparkle/Velopack, changes preferences, or uses the network.
    fn preview_channel(&self) -> Option<bool> {
        #[cfg(all(target_os = "linux", feature = "settings-ui-harness"))]
        return self.preview_beta;
        #[cfg(not(all(target_os = "linux", feature = "settings-ui-harness")))]
        None
    }
    /// Harness only: report a fixed outcome instead of contacting a feed.
    fn preview_check(&mut self, cx: &mut Context<Self>) {
        #[cfg(all(target_os = "linux", feature = "settings-ui-harness"))]
        {
            use updater::status::{self, CheckStatus};
            status::set(
                match std::env::var("OKILUM_DEBUG_UPDATE_RESULT").as_deref() {
                    Ok("checking") => CheckStatus::Checking,
                    Ok("available") => CheckStatus::Available("0.1.9999".into()),
                    Ok("error") => {
                        CheckStatus::Failed("the update feed could not be reached".into())
                    }
                    _ => CheckStatus::UpToDate(std::time::SystemTime::now()),
                },
            );
            cx.refresh_windows();
        }
        let _ = cx;
    }
    fn select_update_channel(&mut self, beta: bool, cx: &mut Context<Self>) {
        #[cfg(all(target_os = "linux", feature = "settings-ui-harness"))]
        if let Some(value) = self.preview_beta.as_mut() {
            *value = beta;
            cx.notify();
            return;
        }
        updater::set_beta(beta, cx);
    }
    fn set_reader(&mut self, reader: Option<WeakEntity<Reader>>, cx: &mut Context<Self>) {
        self._reader_changes = reader
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .map(|reader| cx.observe(&reader, |_, _, cx| cx.notify()));
        self.reader = reader;
        cx.notify();
    }
    fn vault(&self, cx: &App) -> Option<Entity<Reader>> {
        self.reader
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .filter(|reader| !reader.read(cx).vault_root.as_os_str().is_empty())
    }
    #[cfg(any(unix, windows))]
    fn refresh_template_folder(&mut self, cx: &mut Context<Self>) {
        let current = self.vault(cx).map(|reader| {
            let reader = reader.read(cx);
            (reader.vault_root.clone(), reader.session_directory.clone())
        });
        let root = current.as_ref().map(|(root, _)| root.clone());
        if self.template_root == root {
            return;
        }
        self.template_root = root;
        self.template_epoch += 1;
        self.template_folder = reader_templates::DEFAULT_FOLDER.into();
        self.template_error = None;
        self.template_pending = false;
        let Some((root, state)) = current else {
            return;
        };
        let Some(state) = state else {
            self.template_error = Some("No preference storage is available.".into());
            return;
        };
        let epoch = self.template_epoch;
        self.template_pending = true;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { reader_templates::load(&root, &state) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.template_epoch != epoch {
                    return;
                }
                this.template_pending = false;
                match result {
                    Ok(folder) => this.template_folder = folder,
                    Err(error) => this.template_error = Some(format!("{error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }
    #[cfg(any(unix, windows))]
    fn choose_template_folder(&mut self, cx: &mut Context<Self>) {
        let Some(reader) = self.vault(cx) else {
            return;
        };
        let root = reader.read(cx).vault_root.clone();
        let Some(state) = reader.read(cx).session_directory.clone() else {
            return;
        };
        let epoch = self.template_epoch;
        self.template_pending = true;
        cx.notify();
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose templates folder inside this vault".into()),
        });
        cx.spawn(async move |this, cx| {
            let chosen = match picker.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                _ => None,
            };
            let Some(chosen) = chosen else {
                let _ = this.update(cx, |this, cx| {
                    if this.template_epoch == epoch {
                        this.template_pending = false;
                        cx.notify();
                    }
                });
                return;
            };
            let valid = this
                .update(cx, |this, cx| {
                    this.template_epoch == epoch
                        && this
                            .vault(cx)
                            .is_some_and(|reader| reader.read(cx).vault_root == root)
                })
                .unwrap_or(false);
            if !valid {
                return;
            }
            let result = cx
                .background_executor()
                .spawn(async move { reader_templates::save(&root, &state, &chosen) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.template_epoch != epoch {
                    return;
                }
                this.template_pending = false;
                match result {
                    Ok(folder) => {
                        this.template_folder = folder;
                        this.template_error = None;
                    }
                    Err(error) => this.template_error = Some(format!("{error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }
    /// Where log source locations open (#1165).
    fn log_editor_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        let current = reader_ui_state::log_editor(cx);
        let label = |editor: &source_link::Editor| match editor {
            source_link::Editor::VsCode => "VS Code",
            source_link::Editor::Cursor => "Cursor",
            source_link::Editor::Zed => "Zed",
            source_link::Editor::JetBrains => "CLion / JetBrains",
            source_link::Editor::Custom(_) => "Custom command",
        };
        let custom = match &current {
            source_link::Editor::Custom(command) => Some(command.clone()),
            _ => None,
        };
        let input = self.editor_command.as_ref().map(|(input, _)| input.clone());
        let choices = [
            source_link::Editor::VsCode,
            source_link::Editor::Cursor,
            source_link::Editor::Zed,
            source_link::Editor::JetBrains,
            source_link::Editor::Custom(
                input
                    .as_ref()
                    .map(|input| input.read(cx).value().to_string())
                    .unwrap_or_default(),
            ),
        ];
        let selected = label(&current);
        v_flex()
            .gap_2()
            .child(setting_row(
                "Open log source links in",
                "A click on file:line in a log, such as a stack frame, opens it here.",
                Button::new("settings-log-editor")
                    .debug_selector(|| "settings-log-editor".into())
                    .outline()
                    .small()
                    .label(selected)
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, _, _| {
                        let mut menu = menu;
                        for choice in choices.clone() {
                            let checked = label(&choice) == selected;
                            menu = menu.item(
                                PopupMenuItem::new(label(&choice))
                                    .checked(checked)
                                    .on_click(move |_, _, cx| {
                                        reader_ui_state::set_log_editor(&choice, cx);
                                        cx.refresh_windows();
                                    }),
                            );
                        }
                        menu
                    }),
                cx,
            ))
            .when_some(custom.and(input), |controls, input| {
                controls.child(setting_row(
                    "Editor command",
                    "Run without a shell; {file}, {line} and {col} are filled in.",
                    div()
                        .w(px(260.))
                        .debug_selector(|| "settings-log-editor-command".into())
                        .child(Input::new(&input).small()),
                    cx,
                ))
            })
            .into_any_element()
    }

    /// Build paths of this vault's logs that open in a local checkout (#1165).
    fn source_map_controls(&self, root: &Path, cx: &mut Context<Self>) -> AnyElement {
        let maps = reader_ui_state::source_maps(root, cx);
        let muted = brand::palette(cx).text_muted;
        let rows = maps.iter().enumerate().map(|(position, map)| {
            let root = root.to_owned();
            h_flex()
                .debug_selector(move || format!("settings-source-map-{position}"))
                .gap_2()
                .text_sm()
                .child(
                    div()
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(map.from.clone()),
                )
                .child(div().text_color(muted).child("→"))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(map.to.to_string_lossy().into_owned()),
                )
                .child(
                    Button::new(("settings-source-map-remove", position))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .accessibility_label("Remove mapping")
                        .tooltip("Remove mapping")
                        .on_click(move |_, _, cx| {
                            let mut maps = reader_ui_state::source_maps(&root, cx);
                            if position < maps.len() {
                                maps.remove(position);
                            }
                            reader_ui_state::set_source_maps(&root, maps, cx);
                        }),
                )
        });
        let input = self.map_from.clone();
        let add_root = root.to_owned();
        v_flex()
            .gap_2()
            .child(setting_row(
                "Log source paths",
                "Stack frames from a build made elsewhere open in your local checkout.",
                h_flex()
                    .gap_2()
                    .children(input.as_ref().map(|input| {
                        div()
                            .w(px(200.))
                            .debug_selector(|| "settings-source-map-from".into())
                            .child(Input::new(input).small())
                    }))
                    .child(
                        Button::new("settings-source-map-add")
                            .debug_selector(|| "settings-source-map-add".into())
                            .ghost()
                            .icon(IconName::FolderOpen)
                            .accessibility_label("Choose local folder")
                            .tooltip("Choose the local folder for this build path")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.add_source_map(add_root.clone(), cx)
                            })),
                    ),
                cx,
            ))
            .children(rows)
            .into_any_element()
    }

    /// The typed build path maps to a folder the user picks.
    fn add_source_map(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let Some(input) = self.map_from.clone() else {
            return;
        };
        let from = input.read(cx).value().trim().to_owned();
        if from.is_empty() {
            return;
        }
        let folder = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Local checkout".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = folder.await else {
                return;
            };
            let Some(to) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |this, cx| this.push_source_map(&root, from, to, cx));
        })
        .detach();
    }

    fn push_source_map(&mut self, root: &Path, from: String, to: PathBuf, cx: &mut Context<Self>) {
        let mut maps = reader_ui_state::source_maps(root, cx);
        maps.retain(|map| map.from != from);
        maps.push(source_link::PathMap { from, to });
        reader_ui_state::set_source_maps(root, maps, cx);
        cx.notify();
    }

    fn template_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        #[cfg(any(unix, windows))]
        {
            let has_storage = self
                .vault(cx)
                .is_some_and(|reader| reader.read(cx).session_directory.is_some());
            let folder = if self.template_pending {
                "Loading…".to_owned()
            } else {
                self.template_folder.clone()
            };
            v_flex()
                .gap_2()
                .child(setting_row(
                    "Templates folder",
                    "Templates for new notes.",
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .max_w(px(160.))
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_sm()
                                .text_color(brand::palette(cx).text_muted)
                                .child(folder.clone()),
                        )
                        .child(
                            Button::new("settings-template-folder")
                                .ghost()
                                .icon(IconName::FolderOpen)
                                .accessibility_label("Change templates folder")
                                .tooltip(format!("Change templates folder ({folder})"))
                                .disabled(self.template_pending || !has_storage)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.choose_template_folder(cx)),
                                ),
                        ),
                    cx,
                ))
                .children(self.template_error.as_ref().map(|error| {
                    div()
                        .text_sm()
                        .text_color(brand::palette(cx).danger)
                        .child(error.clone())
                }))
                .into_any_element()
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = cx;
            div().into_any_element()
        }
    }
    fn body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = brand::palette(cx);
        let content = v_flex().gap_4().child(
            div()
                .text_xl()
                .font_weight(FontWeight::SEMIBOLD)
                .child(self.section.label()),
        );
        match self.section {
            Section::Appearance => {
                let current = cx.try_global::<AppearancePreference>().and_then(|p| p.0);
                let vault = self
                    .vault(cx)
                    .map(|reader| reader.read(cx).vault_root.clone());
                self.theme_picker
                    .update(cx, |picker, cx| picker.set_vault(vault.clone(), cx));
                content
                    .child(setting_row(
                        "Mode",
                        "Light, dark, or automatic.",
                        ButtonGroup::new("settings-theme").flex_none().children(
                            [
                                (
                                    "settings-system",
                                    "System",
                                    Icon::default().path("icons/monitor.svg"),
                                    None,
                                ),
                                (
                                    "settings-light",
                                    "Light",
                                    Icon::new(IconName::Sun),
                                    Some(ThemeMode::Light),
                                ),
                                (
                                    "settings-dark",
                                    "Dark",
                                    Icon::new(IconName::Moon),
                                    Some(ThemeMode::Dark),
                                ),
                            ]
                            .map(|(id, label, icon, mode)| {
                                let vault = vault.clone();
                                let selected = current == mode;
                                Button::new(id)
                                    .ghost()
                                    .debug_selector(move || id.into())
                                    .label(label)
                                    .icon(icon)
                                    .selected(selected)
                                    .when(selected, |button| button.primary())
                                    .on_click(move |_, window, cx| {
                                        set_appearance(mode, vault.as_deref(), window, cx)
                                    })
                            }),
                        ),
                        cx,
                    ))
                    .child(reader_reading_controls::render(cx))
                    .child(setting_row(
                        "Show labels on toolbar buttons",
                        "Show names beside icons when there is room.",
                        div()
                            .debug_selector(|| "settings-toolbar-labels-control".into())
                            .child(
                                Switch::new("settings-toolbar-labels")
                                    .accessibility_label("Show labels on toolbar buttons")
                                    .checked(reader_ui_state::toolbar_labels(cx))
                                    .on_click(|value, _, cx| {
                                        reader_ui_state::set_toolbar_labels(*value, cx)
                                    }),
                            ),
                        cx,
                    ))
                    .child(
                        v_flex()
                            .gap_2()
                            .child("Color theme")
                            .child(self.theme_picker.clone()),
                    )
                    .into_any_element()
            }
            Section::Files => {
                if let Some(reader) = self.vault(cx) {
                    let state = reader.read(cx);
                    let hidden = state.sidebar.show_hidden;
                    let root = state.vault_root.clone();
                    let map_root = root.clone();
                    let name = crate::vault_name::shown(&root, cx);
                    let rename_root = root.clone();
                    let full_path = root.to_string_lossy().into_owned();
                    let short_path = compact_vault_path(&root);
                    let color_root = root.clone();
                    let weak = reader.downgrade();
                    content
                        .child(
                            h_flex()
                                .gap_3()
                                .child(Icon::new(IconName::Folder).size_5())
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap_1()
                                        .child(div().font_weight(FontWeight::MEDIUM).child(name))
                                        .child(
                                            div()
                                                .id("settings-vault-path")
                                                .text_sm()
                                                .text_color(p.text_muted)
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .whitespace_nowrap()
                                                .child(short_path)
                                                .tooltip(move |window, cx| {
                                                    let path = full_path.clone();
                                                    let width =
                                                        (f32::from(window.viewport_size().width)
                                                            - 64.)
                                                            .clamp(160., 360.);
                                                    gpui_component::tooltip::Tooltip::element(
                                                        move |_, _| {
                                                            div()
                                                                .w(px(width))
                                                                .whitespace_normal()
                                                                .child(path.clone())
                                                        },
                                                    )
                                                    .build(window, cx)
                                                }),
                                        ),
                                )
                                .child(
                                    Button::new("settings-rename-vault")
                                        .ghost()
                                        .label("Rename…")
                                        .on_click(move |_, window, cx| {
                                            crate::vault_name::open_rename(
                                                rename_root.clone(),
                                                window,
                                                cx,
                                            )
                                        }),
                                )
                                .child(
                                    Button::new("settings-reveal-vault")
                                        .ghost()
                                        .icon(IconName::ExternalLink)
                                        .accessibility_label("Reveal vault")
                                        .tooltip(if cfg!(target_os = "macos") {
                                            "Reveal in Finder"
                                        } else if cfg!(target_os = "windows") {
                                            "Reveal in Explorer"
                                        } else {
                                            "Reveal in File Manager"
                                        })
                                        .on_click(move |_, window, cx| {
                                            reader_files::reveal(&root, window, cx)
                                        }),
                                ),
                        )
                        .child(setting_row(
                            "Show hidden files",
                            "Include hidden files in this vault’s sidebar.",
                            div()
                                .debug_selector(|| "settings-hidden-files".into())
                                .child(
                                    Switch::new("settings-hidden-files-switch")
                                        .accessibility_label("Show hidden files")
                                        .checked(hidden)
                                        .on_click(move |_, _, cx| {
                                            let _ = weak.update(cx, |reader, cx| {
                                                reader.toggle_hidden_files(cx)
                                            });
                                        }),
                                ),
                            cx,
                        ))
                        .children(reader_stignore::settings_row(&reader, cx))
                        .child(setting_row(
                            "Vault colour",
                            "A dot by the name and a line along the top.",
                            vault_color_swatches(&color_root, cx),
                            cx,
                        ))
                        .when(
                            reader_ui_state::vault_color(&color_root, cx).is_some(),
                            |content| {
                                let root = color_root.clone();
                                let tinted = reader_ui_state::title_tint(&root, cx).is_some();
                                content.child(setting_row(
                                    "Tint title bar",
                                    "Colour the window's title bar for this vault.",
                                    div().debug_selector(|| "settings-title-tint".into()).child(
                                        Switch::new("settings-title-tint-switch")
                                            .accessibility_label("Tint title bar")
                                            .checked(tinted)
                                            .on_click(move |_, _, cx| {
                                                reader_ui_state::set_title_tint(&root, !tinted, cx)
                                            }),
                                    ),
                                    cx,
                                ))
                            },
                        )
                        .child(self.template_controls(cx))
                        .child(self.log_editor_controls(cx))
                        .child(self.source_map_controls(&map_root, cx))
                        .children(self.vault(cx).map(|reader| {
                            #[cfg(any(unix, windows))]
                            {
                                reader_reminder_settings::controls(&reader, cx)
                            }
                            #[cfg(not(any(unix, windows)))]
                            {
                                let _ = reader;
                                div().into_any_element()
                            }
                        }))
                        .into_any_element()
                } else {
                    content
                        .child("Open a vault to change its file visibility.")
                        .child(self.log_editor_controls(cx))
                        .into_any_element()
                }
            }
            Section::Updates => {
                let content =
                    content.child(div().text_sm().text_color(p.text_muted).child(format!(
                        "Version {}{} · Build {} · Channel: {}",
                        env!("OKILUM_RELEASE_VERSION"),
                        crate::version_info::package_part(),
                        env!("OKILUM_BUILD_VERSION"),
                        match self.preview_channel() {
                            Some(true) => "Beta",
                            Some(false) => "Stable",
                            None => updater::channel(),
                        }
                    )));
                if self.preview_channel().is_some() || updater::available() {
                    let beta = self
                        .preview_channel()
                        .unwrap_or_else(|| updater::channel() == "Beta");
                    content
                        .child(
                            h_flex()
                                .flex_wrap()
                                .gap_2()
                                .child(
                                    ButtonGroup::new("settings-update-channel")
                                        .flex_none()
                                        .children(
                                            [
                                                (
                                                    "settings-stable",
                                                    "Stable",
                                                    false,
                                                    "icons/channel-stable.svg",
                                                ),
                                                (
                                                    "settings-beta",
                                                    "Beta",
                                                    true,
                                                    "icons/channel-beta.svg",
                                                ),
                                            ]
                                            .map(
                                                |(id, label, value, icon)| {
                                                    let selected = beta == value;
                                                    Button::new(id)
                                                        .ghost()
                                                        .debug_selector(move || id.into())
                                                        .label(label)
                                                        .icon(Icon::default().path(icon))
                                                        .selected(selected)
                                                        .when(selected, |button| button.primary())
                                                        .tooltip(if value {
                                                            "Beta: preview new features and fixes"
                                                        } else {
                                                            "Stable: receive approved releases"
                                                        })
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.select_update_channel(
                                                                    value, cx,
                                                                );
                                                            },
                                                        ))
                                                },
                                            ),
                                        ),
                                )
                                .child(
                                    Button::new("settings-check-updates")
                                        .debug_selector(|| "settings-check-updates".into())
                                        .flex_none()
                                        .icon(IconName::RotateCw)
                                        .ghost()
                                        .accessibility_label(updater::action_label())
                                        .tooltip(updater::action_label())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            if this.preview_channel().is_none() {
                                                updater::activate(cx);
                                            } else {
                                                this.preview_check(cx);
                                            }
                                        })),
                                ),
                        )
                        .children(update_status_row("settings", cx))
                        .child(div().text_sm().text_color(p.text_muted).child(if beta {
                            "Beta selected — preview new features and fixes."
                        } else {
                            "Stable selected — receive approved releases."
                        }))
                        .into_any_element()
                } else {
                    let _ = window;
                    let message = if cfg!(target_os = "linux") {
                        "Updates are managed by your system package manager."
                    } else if cfg!(target_os = "windows") {
                        "Download a new Windows ZIP from Releases to update."
                    } else {
                        "Automatic updates are available in the installed release build."
                    };
                    content
                        .child(message)
                        .child(
                            h_flex().flex_wrap().gap_2()
                                .when(cfg!(target_os = "linux"), |row| {
                                    row.child(
                                        Button::new("settings-linux-channels")
                                            .flex_none()
                                            .icon(IconName::Settings2)
                                            .ghost()
                                            .accessibility_label("Repository setup")
                                            .tooltip("Configure the Stable or Beta package repository")
                                            .on_click(|_, _, cx| {
                                                cx.open_url("https://github.com/BeFeast/okilum/blob/main/docs/linux-releases.md")
                                            }),
                                    )
                                })
                                .child(
                                    Button::new("settings-releases")
                                        .flex_none()
                                        .icon(IconName::ExternalLink)
                                        .ghost()
                                        .accessibility_label("Release notes")
                                        .tooltip("Release notes")
                                        .on_click(|_, _, cx| {
                                            cx.open_url("https://git.oklabs.uk/BeFeast/okilum/releases")
                                        }),
                                ),
                        )
                        .into_any_element()
                }
            }
            #[cfg(target_os = "linux")]
            Section::Sync => content.children(self.sync.clone()).into_any_element(),
            Section::About => content.child(about::content(cx)).into_any_element(),
            Section::Inbox => content
                .child("Not connected")
                .child(
                    div()
                        .text_sm()
                        .text_color(p.text_muted)
                        .child("Inbox is optional. Your local vault works without a connection."),
                )
                .into_any_element(),
        }
    }
}
impl Render for Settings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(any(unix, windows))]
        self.refresh_template_folder(cx);
        if self.editor_command.is_none() {
            let command = match reader_ui_state::log_editor(cx) {
                source_link::Editor::Custom(command) => command,
                _ => String::new(),
            };
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("code --goto {file}:{line}:{col}")
                    .default_value(command)
            });
            let subscription = cx.subscribe(&input, |_, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let command = input.read(cx).value().to_string();
                    reader_ui_state::set_log_editor(&source_link::Editor::Custom(command), cx);
                    cx.notify();
                }
            });
            self.editor_command = Some((input, subscription));
        }
        if self.map_from.is_none() {
            self.map_from =
                Some(cx.new(|cx| {
                    InputState::new(window, cx).placeholder("Build path, e.g. /src/ordgw")
                }));
        }
        let p = brand::palette(cx);
        let body = self.body(window, cx);
        h_flex()
            .size_full()
            .items_start()
            .bg(p.surface)
            .text_color(p.text)
            .key_context("OkilumSettings")
            .track_focus(&self.focus)
            .on_action(cx.listener(|_, _: &CloseSettings, window, _| window.remove_window()))
            .on_action(cx.listener(|_, _: &OpenSettings, window, cx| {
                window.activate_window();
                cx.stop_propagation();
            }))
            .child(
                v_flex()
                    .w(px(176.))
                    .h_full()
                    .flex_none()
                    .p_3()
                    .gap_1()
                    .children(
                        [
                            Section::Appearance,
                            Section::Files,
                            Section::Updates,
                            Section::Inbox,
                            #[cfg(target_os = "linux")]
                            Section::Sync,
                            // Platform convention: About comes last (#995).
                            Section::About,
                        ]
                        .map(|section| {
                            div()
                                .debug_selector(move || {
                                    format!("settings-section-{}", section.label())
                                })
                                .child(
                                    Button::new(section.label())
                                        .w_full()
                                        .ghost()
                                        .accessibility_label(section.label())
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .gap_2()
                                                .child(section.icon().size_4())
                                                .child(section.label()),
                                        )
                                        .selected(self.section == section)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            #[cfg(target_os = "linux")]
                                            if section == Section::Sync && this.sync.is_none() {
                                                this.sync =
                                                    Some(reader_settings_sync::shared(window, cx));
                                            }
                                            #[cfg(not(target_os = "linux"))]
                                            let _ = window;
                                            this.section = section;
                                            cx.notify();
                                        })),
                                )
                        }),
                    ),
            )
            .child(
                div()
                    .id("settings-content")
                    .debug_selector(|| "settings-content".into())
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .p_6()
                    .child(body),
            )
            // Confirmations (Uninstall Okilum…) open as dialogs on this window;
            // without the dialog layer they exist but are never drawn (#1085).
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}

/// The last check's outcome, inline next to the check button (#995). A found
/// update offers its install action here instead of a modal.
pub(crate) fn update_status_row(id: &'static str, cx: &App) -> Option<AnyElement> {
    let line = updater::status_line()?;
    let p = brand::palette(cx);
    let available = matches!(
        updater::status::get(),
        updater::status::CheckStatus::Available(_)
    );
    Some(
        h_flex()
            .gap_2()
            .items_center()
            .flex_wrap()
            .child(
                div()
                    .debug_selector(move || format!("{id}-update-status"))
                    .text_sm()
                    .text_color(p.text_muted)
                    .child(line),
            )
            .when(available, |row| {
                row.child(
                    Button::new(SharedString::from(format!("{id}-install-update")))
                        .ghost()
                        .small()
                        .label("Install Update…")
                        .on_click(|_, _, _| updater::install_update()),
                )
            })
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[cfg(all(target_os = "linux", feature = "settings-ui-harness"))]
    #[gpui::test]
    fn preview_uses_real_channel_controls_without_changing_the_platform_channel(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (root, visual) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| {
                let mut settings = Settings::new(None, cx);
                settings.section = Section::Updates;
                settings.preview_beta = Some(false);
                settings
            });
            Root::new(settings, window, cx)
        });
        let settings = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<Settings>().unwrap()
        });
        let platform_channel = updater::channel();
        visual.run_until_parked();
        for (selector, expected) in [("settings-beta", true), ("settings-stable", false)] {
            let bounds = visual.debug_bounds(selector).expect("real channel segment");
            visual.simulate_click(bounds.center(), Modifiers::default());
            visual.run_until_parked();
            settings.read_with(visual, |settings, _| {
                assert_eq!(settings.preview_channel(), Some(expected));
            });
            assert_eq!(updater::channel(), platform_channel);
        }
        let bounds = visual
            .debug_bounds("settings-check-updates")
            .expect("refresh glyph");
        visual.simulate_click(bounds.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(updater::channel(), platform_channel);
    }

    #[gpui::test]
    fn about_routes_reuse_settings_without_a_reader_modal(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| Reader::new(Opts::default(), window, cx));
            Root::new(reader, window, cx)
        });
        visual.update(about::show_about);
        visual.run_until_parked();
        let handle = visual.update(|_, cx| cx.global::<SettingsWindow>().0.unwrap());
        handle
            .update(visual, |root, _, cx| {
                let settings = root.view().clone().downcast::<Settings>().unwrap();
                assert!(settings.read(cx).section == Section::About);
                settings.update(cx, |this, _| this.section = Section::Appearance);
            })
            .unwrap();
        assert!(
            visual.debug_bounds("desktop-about").is_none(),
            "Reader is not covered by About"
        );
        visual.update(|_, cx| about::show_from_menu(cx));
        visual.run_until_parked();
        visual.update(|_, cx| {
            assert_eq!(
                cx.windows().len(),
                2,
                "Both About entries reuse one Settings window"
            );
            assert_eq!(
                cx.global::<SettingsWindow>().0.unwrap().window_id(),
                handle.window_id()
            );
        });
        handle
            .update(visual, |root, _, cx| {
                let settings = root.view().clone().downcast::<Settings>().unwrap();
                assert!(settings.read(cx).section == Section::About);
            })
            .unwrap();
    }

    #[gpui::test]
    fn reading_controls_update_the_shared_store(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            reader_ui_state::install(directory.path(), cx);
        });
        let (_, visual) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| Settings::new(None, cx));
            Root::new(settings, window, cx)
        });
        visual.run_until_parked();
        let labels = visual
            .debug_bounds("settings-toolbar-labels-control")
            .expect("labels setting visible");
        assert!(!visual.update(|_, cx| reader_ui_state::toolbar_labels(cx)));
        visual.simulate_click(labels.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(visual.update(|_, cx| reader_ui_state::toolbar_labels(cx)));
        let original = visual.update(|_, cx| reader_ui_state::font_size(cx));
        for (selector, font, width) in [
            ("reading-larger", original + 1., READER_MAX_WIDTH),
            ("reading-wide", original + 1., 960.),
            ("reading-smaller", original, 960.),
        ] {
            let bounds = visual
                .debug_bounds(selector)
                .expect("reading control visible");
            visual.simulate_click(bounds.center(), Modifiers::default());
            visual.run_until_parked();
            visual.update(|_, cx| {
                assert_eq!(reader_ui_state::font_size(cx), font);
                assert_eq!(reader_ui_state::reading_width(cx), width);
            });
        }
    }

    #[cfg(any(unix, windows))]
    /// #954: the reminders note follows a rename or folder move inside the
    /// vault, persisted; a note that is not there is said in Settings.
    #[gpui::test]
    fn reminders_note_follows_moves_and_a_missing_note_is_said(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().unwrap();
        let (root, state) = (temp.path().join("vault"), temp.path().join("state"));
        std::fs::create_dir_all(root.join("Inbox")).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(root.join("start.md"), "# Start\n").unwrap();
        std::fs::write(root.join("Inbox/Remind.md"), "- [ ] Call Dana\n").unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            reader_ui_state::install(&state, cx);
        });
        let mut reader = None;
        cx.add_window(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("start.md".into()),
                        session_directory: Some(state.clone()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        cx.run_until_parked();
        let weak = reader.downgrade();
        let (_, visual) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| {
                let mut settings = Settings::new(Some(weak), cx);
                settings.section = Section::Files;
                settings
            });
            Root::new(settings, window, cx)
        });
        let note = |visual: &mut gpui::VisualTestContext| {
            reader.read_with(visual, |r, _| r.reminder_prefs.note.clone())
        };
        let missing = |visual: &mut gpui::VisualTestContext| {
            visual.update(|window, _| window.refresh());
            visual.run_until_parked();
            visual.debug_bounds("reminder-note-missing").is_some()
        };
        reader.update(visual, |r, cx| {
            let mut prefs = r.reminder_prefs.clone();
            prefs.note = "Inbox/Remind.md".into();
            r.set_reminder_prefs(prefs, cx);
        });
        assert!(!missing(visual), "positive control: the note exists");

        // Rename: the setting follows, persisted.
        std::fs::rename(root.join("Inbox/Remind.md"), root.join("Inbox/Todo.md")).unwrap();
        reader.update(visual, |r, cx| {
            r.follow_moved_reminder_note("Inbox/Remind.md", "Inbox/Todo.md", cx)
        });
        assert_eq!(note(visual), "Inbox/Todo.md");
        assert_eq!(
            reader_reminder_settings::load(&state, &root).note,
            "Inbox/Todo.md"
        );
        assert!(!missing(visual));
        // A folder move carries the note inside it.
        std::fs::create_dir_all(root.join("Old")).unwrap();
        std::fs::rename(root.join("Inbox"), root.join("Old/Inbox")).unwrap();
        reader.update(visual, |r, cx| {
            r.follow_moved_reminder_note("Inbox", "Old/Inbox", cx)
        });
        assert_eq!(note(visual), "Old/Inbox/Todo.md");
        // An unrelated move leaves it alone.
        reader.update(visual, |r, cx| {
            r.follow_moved_reminder_note("start.md", "Begin.md", cx)
        });
        assert_eq!(note(visual), "Old/Inbox/Todo.md");
        // Deleted outside Okilum: Settings says so.
        std::fs::remove_file(root.join("Old/Inbox/Todo.md")).unwrap();
        assert!(missing(visual), "a note that is gone is said");
    }

    #[cfg(any(unix, windows))]
    #[gpui::test]
    fn reminder_rows_step_the_time_inside_waking_hours_and_persist(cx: &mut TestAppContext) {
        use time::Time;
        let temp = std::env::temp_dir().join(format!("okilum-settings-{}", uuid::Uuid::new_v4()));
        let (root, state) = (temp.join("vault"), temp.join("state"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(root.join("start.md"), "# Start\n").unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            reader_ui_state::install(&state, cx);
        });
        let mut reader = None;
        cx.add_window(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("start.md".into()),
                        session_directory: Some(state.clone()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        cx.run_until_parked();
        let weak = reader.downgrade();
        let (_, visual) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| {
                let mut settings = Settings::new(Some(weak), cx);
                settings.section = Section::Files;
                settings
            });
            Root::new(settings, window, cx)
        });
        visual.run_until_parked();
        let at = |h, m| Time::from_hms(h, m, 0).unwrap();
        let current = |visual: &mut VisualTestContext| {
            reader.read_with(visual, |reader, _| reader.reminder_prefs.notify_at)
        };
        let click = |visual: &mut VisualTestContext, selector: &'static str| {
            let bounds = visual
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} visible"));
            visual.simulate_click(bounds.center(), Modifiers::default());
            visual.run_until_parked();
        };
        assert!(visual.debug_bounds("reminder-time").is_some());
        assert!(visual.debug_bounds("reminder-note").is_some());
        assert_eq!(current(visual), at(9, 0));

        // Positive control: "later" moves and is saved per vault.
        click(visual, "reminder-later");
        assert_eq!(current(visual), at(9, 30));
        let saved = reader_reminder_settings::load(&state, &root);
        assert_eq!(saved.notify_at, at(9, 30));
        // "Earlier" returns, and at the end of quiet hours it stops: 08:30 would
        // only be held back until 09:00, so it is not offered.
        click(visual, "reminder-earlier");
        assert_eq!(current(visual), at(9, 0));
        click(visual, "reminder-earlier");
        assert_eq!(current(visual), at(9, 0));
        assert_eq!(
            reader_reminder_settings::load(&state, &root).notify_at,
            at(9, 0)
        );
        for _ in 0..30 {
            click(visual, "reminder-later");
        }
        assert_eq!(
            current(visual),
            at(20, 30),
            "stops before quiet hours begin"
        );

        // An earlier complaint is cleared by the next accepted choice, even one
        // that changes nothing.
        reader.update(visual, |reader, cx| {
            reader.reminder_prefs_error = Some("Choose a Markdown note inside this vault.".into());
            let same = reader.reminder_prefs.clone();
            reader.set_reminder_prefs(same, cx);
            assert!(reader.reminder_prefs_error.is_none());
        });
        std::fs::remove_dir_all(temp).unwrap();
    }

    /// #957: at the default Settings width (half of a 1366 px screen) the
    /// Reminder time help wraps instead of hiding the quiet hours.
    #[cfg(any(unix, windows))]
    #[gpui::test]
    fn setting_help_wraps_instead_of_being_cut_off(cx: &mut TestAppContext) {
        let temp = std::env::temp_dir().join(format!("okilum-settings-{}", uuid::Uuid::new_v4()));
        let (root, state) = (temp.join("vault"), temp.join("state"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(root.join("start.md"), "# Start\n").unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            reader_ui_state::install(&state, cx);
        });
        let mut reader = None;
        cx.add_window(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("start.md".into()),
                        session_directory: Some(state.clone()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let weak = reader.unwrap().downgrade();
        cx.run_until_parked();
        let (_, visual) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| {
                let mut settings = Settings::new(Some(weak), cx);
                settings.section = Section::Files;
                settings
            });
            Root::new(settings, window, cx)
        });
        let mut measure = |width: f32| {
            visual.simulate_resize(size(px(width), px(768.)));
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
            let help = visual
                .debug_bounds("setting-help-Reminder time")
                .expect("Reminder time help shown");
            let control = visual
                .debug_bounds("reminder-earlier")
                .expect("Reminder time control shown");
            assert!(
                help.right() <= control.left(),
                "help {help:?} stays left of its control {control:?} at {width}"
            );
            help.size.height
        };
        let wide = measure(1600.);
        let narrow = measure(683.);
        assert!(wide > px(0.), "positive control: the help is laid out");
        assert!(
            narrow > wide,
            "at 683 px the help wraps onto more lines ({narrow:?}) instead of being cut ({wide:?})"
        );
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[gpui::test]
    fn confirmations_opened_from_settings_are_drawn(cx: &mut TestAppContext) {
        use gpui_component::WindowExt;
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
            cx.set_global(AppearancePreference(None));
        });
        let (root, visual) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| Settings::new(None, cx));
            Root::new(settings, window, cx)
        });
        visual.run_until_parked();
        let settings = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<Settings>().unwrap()
        });
        let answer = settings.update_in(visual, |_, window, cx| {
            crate::reader_confirm::confirm(
                window,
                cx,
                "Uninstall Okilum?",
                crate::reader_confirm::Body {
                    intro: vec!["Okilum and its data will be removed.".into()],
                    items: vec![],
                    outro: vec![],
                },
                "Uninstall",
            )
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(visual.update(|window, cx| window.has_active_dialog(cx)));
        assert!(
            visual.debug_bounds("confirm-action").is_some(),
            "the Settings window must draw its confirmation"
        );
        // Positive control: the drawn dialog answers and closes.
        let cancel = visual
            .debug_bounds("cancel-confirm")
            .expect("cancel button");
        visual.simulate_click(cancel.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(!visual.update(|window, cx| window.has_active_dialog(cx)));
        assert_eq!(answer.try_recv(), Ok(false));
    }

    #[gpui::test]
    fn about_copies_one_line_version_info_and_says_so_inline(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(AppearancePreference(None));
        });
        let (_, visual) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| Settings::new(None, cx));
            Root::new(settings, window, cx)
        });
        visual.run_until_parked();
        let about = visual
            .debug_bounds("settings-section-About")
            .expect("About section");
        visual.simulate_click(about.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("about-version-copied").is_none());
        let copy = visual
            .debug_bounds("about-copy-version")
            .expect("copy button next to the version");
        visual.simulate_click(copy.center(), Modifiers::default());
        visual.run_until_parked();
        let copied = visual
            .update(|_, cx| cx.read_from_clipboard())
            .and_then(|item| item.text())
            .expect("clipboard text");
        assert_eq!(copied, crate::version_info::report());
        assert!(!copied.contains('\n'));
        assert!(
            visual.debug_bounds("about-version-copied").is_some(),
            "the label answers inline"
        );
        // Positive control: the label returns to Copy after the timer.
        visual
            .executor()
            .advance_clock(std::time::Duration::from_secs(3));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(visual.debug_bounds("about-copy-version").is_some());
        assert!(visual.debug_bounds("about-version-copied").is_none());
    }

    #[gpui::test]
    fn sections_switch_and_appearance_changes_without_a_vault(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(AppearancePreference(None));
        });
        let (root, visual) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| Settings::new(None, cx));
            Root::new(settings, window, cx)
        });
        visual.run_until_parked();
        let settings = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<Settings>().unwrap()
        });
        for (section, selector) in [
            (Section::Files, "settings-section-Files"),
            (Section::Updates, "settings-section-Updates"),
            (Section::About, "settings-section-About"),
            (Section::Inbox, "settings-section-Inbox"),
            (Section::Appearance, "settings-section-Appearance"),
        ] {
            let bounds = visual
                .debug_bounds(selector)
                .expect("section button rendered");
            visual.simulate_click(bounds.center(), Modifiers::default());
            visual.run_until_parked();
            settings.read_with(visual, |settings, cx| {
                assert!(settings.section == section);
                assert!(settings.vault(cx).is_none());
            });
        }
        let theme = visual
            .debug_bounds("theme-picker-nord")
            .expect("color themes are hosted in production Settings");
        visual.simulate_click(theme.center(), Modifiers::default());
        visual.run_until_parked();
        visual.update(|_, cx| assert_eq!(brand::theme_id(cx), brand::ThemeId::Nord));
        for (id, label) in [("settings-dark", "Dark"), ("settings-system", "System")] {
            let bounds = visual.debug_bounds(id).expect("appearance option rendered");
            visual.simulate_click(bounds.center(), Modifiers::default());
            visual.run_until_parked();
            visual.update(|_, cx| {
                assert_eq!(appearance_label(cx), label);
                assert_eq!(brand::theme_id(cx), brand::ThemeId::Nord);
            });
        }
    }
    #[gpui::test]
    fn color_theme_actions_receive_the_current_canonical_root(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (root, visual) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| Settings::new(None, cx));
            Root::new(settings, window, cx)
        });
        let settings = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<Settings>().unwrap()
        });
        let reader =
            visual.update(|window, cx| cx.new(|cx| Reader::new(Opts::default(), window, cx)));
        visual.run_until_parked();
        // No shared store: this is the legacy appearance persistence path.
        visual.update(|_, cx| assert!(!reader_ui_state::installed(cx)));
        settings.update(visual, |settings, cx| {
            settings.set_reader(Some(reader.downgrade()), cx);
        });
        for (name, theme) in [("first-vault", "nord"), ("second-vault", "paper")] {
            let vault = PathBuf::from(name);
            reader.update(visual, |reader, cx| {
                reader.vault_root = vault.clone();
                cx.notify();
            });
            visual.run_until_parked();
            let bounds = visual
                .debug_bounds(format!("theme-picker-{theme}").leak())
                .expect("production theme card rendered");
            visual.simulate_click(bounds.center(), Modifiers::default());
            visual.run_until_parked();
            visual.update(|_, cx| {
                assert_eq!(
                    cx.global::<theme_picker::ThemeActionVault>().0,
                    Some(vault),
                    "theme action must protect the current canonical root after a vault switch"
                );
            });
        }
        settings.update(visual, |settings, cx| settings.set_reader(None, cx));
        visual.run_until_parked();
        let bounds = visual.debug_bounds("theme-picker-okilum").unwrap();
        visual.simulate_click(bounds.center(), Modifiers::default());
        visual.run_until_parked();
        visual.update(|_, cx| {
            assert_eq!(cx.global::<theme_picker::ThemeActionVault>().0, None);
        });
    }

    #[gpui::test]
    fn hidden_files_setting_uses_the_reader_toggle(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (root, visual) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| Settings::new(None, cx));
            Root::new(settings, window, cx)
        });
        let reader = visual.update(|window, cx| {
            cx.new(|cx| {
                let mut reader = Reader::new(Opts::default(), window, cx);
                reader.vault_root = PathBuf::from("/synthetic-vault");
                reader.sidebar_path = None;
                reader
            })
        });
        let settings = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<Settings>().unwrap()
        });
        settings.update(visual, |settings, cx| {
            settings.set_reader(Some(reader.downgrade()), cx);
            settings.section = Section::Files;
        });
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| assert!(!reader.sidebar.show_hidden));
        let bounds = visual
            .debug_bounds("settings-hidden-files")
            .expect("vault setting rendered");
        visual.simulate_click(bounds.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.sidebar.show_hidden);
            assert!(reader.tree.show_hidden());
        });
    }
    #[gpui::test]
    fn settings_files_shows_the_syncthing_ignore_state_and_its_button(cx: &mut TestAppContext) {
        // #1248: the same state and the same button as the offer when the vault opens.
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(root.join(".stfolder")).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("Note.md"), "# Note\n").unwrap();
        let before: &[u8] = b".archive-index\n// kept as it is\n";
        std::fs::write(root.join(".stignore"), before).unwrap();
        let (window_root, visual) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| Settings::new(None, cx));
            Root::new(settings, window, cx)
        });
        let settings = window_root.read_with(visual, |root, _| {
            root.view().clone().downcast::<Settings>().unwrap()
        });
        let reader = visual.update(|window, cx| {
            cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Note.md".into()),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(temp.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            })
        });
        visual.run_until_parked();
        settings.update(visual, |settings, cx| {
            settings.set_reader(Some(reader.downgrade()), cx);
            settings.section = Section::Files;
        });
        visual.run_until_parked();
        // The offer's own toast goes away first, so the button clicked is the Settings one.
        visual.executor().advance_clock(Duration::from_secs(25));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            visual.debug_bounds("settings-stignore-add").is_some(),
            "the row is shown for a Syncthing vault without the lines"
        );
        assert_eq!(
            std::fs::read(root.join(".stignore")).unwrap(),
            before,
            "showing the row writes nothing"
        );
        let button = visual.debug_bounds("settings-stignore-add").unwrap();
        visual.simulate_click(button.center(), Modifiers::default());
        visual.run_until_parked();
        let after = std::fs::read(root.join(".stignore")).unwrap();
        assert!(after.starts_with(before), "every other byte stays");
        assert!(String::from_utf8(after).unwrap().contains(".okilum-save-*"));
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            visual.debug_bounds("settings-stignore-add").is_none(),
            "nothing left to add"
        );
        assert!(
            visual.debug_bounds("settings-stignore").is_some(),
            "the row now says the lines are ignored"
        );
        // A vault that is not a Syncthing folder has no such row.
        std::fs::remove_dir(root.join(".stfolder")).unwrap();
        reader.update_in(visual, |reader, window, cx| {
            reader.check_stignore(window, cx)
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(visual.debug_bounds("settings-stignore").is_none());
    }

    #[gpui::test]
    fn application_menu_finds_active_reader_and_reuses_settings_window(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let mut owner = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| Reader::new(Opts::default(), window, cx));
            owner = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let owner = owner.unwrap();
        visual.run_until_parked();
        visual.update(|window, cx| {
            window.activate_window();
            show(None, cx);
        });
        visual.run_until_parked();
        let first = visual.update(|_, cx| {
            let handle = cx.global::<SettingsWindow>().0.unwrap();
            let settings = handle
                .read(cx)
                .unwrap()
                .view()
                .clone()
                .downcast::<Settings>()
                .unwrap();
            assert_eq!(
                settings.read(cx).reader.as_ref().unwrap().entity_id(),
                owner.entity_id()
            );
            handle.window_id()
        });
        visual.update(|window, cx| {
            window.activate_window();
            show(None, cx);
        });
        visual.run_until_parked();
        visual.update(|_, cx| {
            assert_eq!(cx.global::<SettingsWindow>().0.unwrap().window_id(), first)
        });
    }
}
