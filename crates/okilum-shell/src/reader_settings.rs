//! Explicit, lazily opened user settings. No vault scans or startup reads.
use super::*;
use gpui_component::{button::ButtonGroup, switch::Switch, ThemeMode};

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
                div()
                    .text_sm()
                    .text_color(brand::palette(cx).text_muted)
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
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
                    let name = root
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
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
                        .child(setting_row(
                            "Vault colour",
                            "A dot by the name and a line along the top.",
                            vault_color_swatches(&color_root, cx),
                            cx,
                        ))
                        .child(self.template_controls(cx))
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
                        .into_any_element()
                }
            }
            Section::Updates => {
                let content =
                    content.child(div().text_sm().text_color(p.text_muted).child(format!(
                        "Version {} · Build {} · Channel: {}",
                        env!("OKILUM_RELEASE_VERSION"),
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
