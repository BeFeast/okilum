//! Explicit, lazily opened user settings. No vault scans or startup reads.
use super::*;
use gpui_component::{
    button::ButtonGroup,
    color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState},
    switch::Switch,
    ThemeMode, WindowExt,
};
use okilum_core::log::source_link;

gpui::actions!(okilum_settings, [OpenSettings, CloseSettings]);

#[derive(Default)]
struct SettingsWindow(Option<WindowHandle<Root>>);
impl Global for SettingsWindow {}

#[derive(Default)]
struct SettingsOwners(std::collections::HashMap<Option<WindowId>, WindowHandle<Root>>);
impl Global for SettingsOwners {}

pub(crate) fn install(cx: &mut App) {
    #[cfg(all(target_os = "linux", not(test)))]
    install_opaque_settings_rule(cx);
    cx.bind_keys([
        KeyBinding::new("secondary-,", OpenSettings, None),
        KeyBinding::new("secondary-w", CloseSettings, Some("OkilumSettings")),
        KeyBinding::new("escape", CloseSettings, Some("OkilumSettings")),
    ]);
    cx.on_action(|_: &OpenSettings, cx| show(None, cx));
}

/// Omarchy gives every native window 98.5% active / 96% inactive opacity.
/// An opaque GPUI surface cannot counter compositor alpha. Opt Settings out
/// using its own app id; no user configuration file or Reader window changes.
#[cfg(all(target_os = "linux", not(test)))]
fn install_opaque_settings_rule(cx: &App) {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return;
    }
    cx.background_executor().spawn(async {
        let result = std::process::Command::new("hyprctl")
            .args([
                "eval",
                "if not _G.okilum_settings_opaque then _G.okilum_settings_opaque = hl.window_rule({ match = { class = '^okilum-settings$' }, opacity = '1 override 1 override' }) end",
            ])
            .output();
        match result {
            Ok(output) if output.status.success() && output.stdout == b"ok\n" => {}
            Ok(output) => eprintln!("Could not request opaque Settings: {}", String::from_utf8_lossy(&output.stdout).trim()),
            Err(error) => eprintln!("Could not request opaque Settings: {error}"),
        }
    }).detach();
}

pub(crate) fn show(reader: Option<WeakEntity<Reader>>, cx: &mut App) {
    show_section(reader, None, cx);
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub(crate) fn show_sync(window: &mut Window, cx: &mut App) {
    show_section_from(None, Some(Section::Sync), Some(window.window_handle()), cx);
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub(crate) fn show_sync_pairing(window: &mut Window, cx: &mut App) {
    reader_features::set_enabled(reader_features::Feature::Sync, true, cx);
    if reader_features::enabled(reader_features::Feature::Sync, cx) {
        reader_settings_sync::shared(window, cx).update(cx, |view, cx| view.receive(cx));
    }
    show_section_from(None, Some(Section::Sync), Some(window.window_handle()), cx);
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub(super) fn share_sync_menu_item(
    reader: WeakEntity<Reader>,
) -> gpui_component::menu::PopupMenuItem {
    gpui_component::menu::PopupMenuItem::new("Share with another computer…")
        .on_click(move |_, _, cx| show_section(Some(reader.clone()), Some(Section::Sync), cx))
}

pub(crate) fn show_features(cx: &mut App) {
    show_section(None, Some(Section::Features), cx);
}

pub(crate) fn show_t3(cx: &mut App) {
    show_section(None, Some(Section::T3), cx);
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn show_updates(cx: &mut App) {
    show_section(None, Some(Section::Updates), cx);
}

/// Open the current vault card at the top of Vaults (#2572).
pub(crate) fn show_vault(reader: Option<WeakEntity<Reader>>, cx: &mut App) {
    show_section(reader, Some(Section::Vaults), cx);
}

#[cfg(any(
    target_os = "macos",
    test,
    all(target_os = "linux", feature = "appimage-acceptance-harness")
))]
pub(crate) fn show_about(cx: &mut App) {
    show_section(None, Some(Section::About), cx);
}

fn show_section(reader: Option<WeakEntity<Reader>>, section: Option<Section>, cx: &mut App) {
    // An application-menu command can arrive with no active native window.
    // Reuse the last Settings family then; explicit desktop entries pass their
    // actual caller and must never switch to another Reader's family.
    let active = cx.active_window().or_else(|| {
        cx.try_global::<SettingsWindow>()
            .and_then(|latest| latest.0)
            .filter(|handle| cx.windows().contains(&(*handle).into()))
            .map(Into::into)
    });
    show_section_from(reader, section, active, cx);
}

pub(crate) fn show_about_in(window: &Window, cx: &mut App) {
    show_section_from(None, Some(Section::About), Some(window.window_handle()), cx);
}

fn window_options(owner: Option<AnyWindowHandle>, cx: &mut App) -> WindowOptions {
    let mut options = reader_owned_windows::options(
        owner,
        size(px(780.), px(520.)),
        size(px(700.), px(440.)),
        cx,
    );
    options.app_id = Some("okilum-settings".into());
    options.window_background = gpui::WindowBackgroundAppearance::Opaque;
    #[cfg(target_os = "macos")]
    {
        // AppKit owns the entire caption, including the native close button.
        // Do not overlay it with TitleBar's mouse-move-driven drag handler or
        // the Close row's press interception (#2755). Ownership is independent
        // of caption handling: keep the Reader's native child relationship.
        options.titlebar = Some(TitlebarOptions {
            title: Some("Okilum Settings".into()),
            appears_transparent: false,
            traffic_light_position: None,
        });
        options.app_owns_titlebar_drag = false;
    }
    options
}

fn show_section_from(
    reader: Option<WeakEntity<Reader>>,
    section: Option<Section>,
    active: Option<AnyWindowHandle>,
    cx: &mut App,
) {
    // Native application-menu actions may bypass the Reader's action handler.
    // Menu dispatch can still hold the active Reader's update borrow.
    cx.defer(move |cx| {
        let owner = reader
            .as_ref()
            .and_then(|reader| reader.upgrade())
            .map(|reader| reader.read(cx).reader_window)
            .or(active)
            .map(|owner| reader_owned_windows::owner(owner, cx));
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
        let owner_id = owner.map(|owner| owner.window_id());
        let alive: std::collections::HashSet<_> = cx
            .windows()
            .iter()
            .map(|window| window.window_id())
            .collect();
        if cx.has_global::<SettingsOwners>() {
            cx.global_mut::<SettingsOwners>()
                .0
                .retain(|_, handle| alive.contains(&handle.window_id()));
        }
        let existing = cx
            .try_global::<SettingsWindow>()
            .and_then(|latest| latest.0)
            .filter(|handle| {
                alive.contains(&handle.window_id())
                    && owner.is_some_and(|owner| {
                        reader_owned_windows::owner((*handle).into(), cx) == owner
                    })
            })
            .or_else(|| {
                cx.try_global::<SettingsOwners>()
                    .and_then(|windows| windows.0.get(&owner_id).copied())
            });
        if let Some(handle) = existing {
            if handle
                .update(cx, |root, window, cx| {
                    if let Ok(settings) = root.view().clone().downcast::<Settings>() {
                        settings.update(cx, |settings, cx| {
                            if let Some(reader) = reader.clone() {
                                settings.set_reader(Some(reader), cx);
                            }
                            if let Some(section) = section {
                                settings.show_entry_section(section, cx);
                            }
                        });
                    }
                    window.activate_window();
                })
                .is_ok()
            {
                cx.set_global(SettingsWindow(Some(handle)));
                return;
            }
        }
        let options = window_options(owner, cx);
        match cx.open_window(options, move |window, cx| {
            window.set_window_title("Okilum Settings");
            reader_owned_windows::attach(owner, window, cx);
            let settings = cx.new(|cx| {
                let mut settings = Settings::new(reader, cx);
                if let Some(section) = section {
                    settings.show_entry_section(section, cx);
                }
                settings
            });
            settings.read(cx).focus.clone().focus(window, cx);
            cx.new(|cx| Root::new(settings, window, cx))
        }) {
            Ok(handle) => {
                reader_owned_windows::register(owner, handle.into(), cx);
                if !cx.has_global::<SettingsOwners>() {
                    cx.set_global(SettingsOwners::default());
                }
                cx.global_mut::<SettingsOwners>().0.insert(owner_id, handle);
                cx.set_global(SettingsWindow(Some(handle)));
            }
            Err(error) => eprintln!("Could not open Settings: {error}"),
        }
    });
}

/// Settings → Editor (#1611): the paste-size threshold, stepped through
/// presets; "Never" turns the question off.
const PASTE_ASK_STEPS: [u32; 9] = [10, 25, 50, 100, 250, 500, 1000, 2000, 0];

fn paste_size_label(mb: u32) -> String {
    match mb {
        0 => "Never ask".into(),
        mb if mb >= 1000 && mb % 1000 == 0 => format!("{} GB", mb / 1000),
        mb => format!("{mb} MB"),
    }
}

fn paste_size_stepper(cx: &App) -> impl IntoElement {
    let current = reader_ui_state::paste_ask_above_mb(cx);
    let index = PASTE_ASK_STEPS
        .iter()
        .position(|step| *step == current)
        .unwrap_or(3);
    let previous = index.checked_sub(1).map(|i| PASTE_ASK_STEPS[i]);
    let next = PASTE_ASK_STEPS.get(index + 1).copied();
    h_flex()
        .gap_1()
        .items_center()
        .debug_selector(|| "settings-paste-ask".into())
        .child(
            Button::new("settings-paste-ask-smaller")
                .ghost()
                .label("−")
                .disabled(previous.is_none())
                .on_click(move |_, _, cx| {
                    if let Some(value) = previous {
                        reader_ui_state::set_paste_ask_above_mb(value, cx);
                    }
                }),
        )
        .child(
            div()
                .min_w(px(84.))
                .text_center()
                .child(paste_size_label(current)),
        )
        .child(
            Button::new("settings-paste-ask-larger")
                .ghost()
                .label("+")
                .disabled(next.is_none())
                .on_click(move |_, _, cx| {
                    if let Some(value) = next {
                        reader_ui_state::set_paste_ask_above_mb(value, cx);
                    }
                }),
        )
}

fn image_quality_stepper(cx: &App) -> impl IntoElement {
    let options = reader_ui_state::image_import_options(cx);
    h_flex()
        .gap_1()
        .items_center()
        .debug_selector(|| "settings-image-quality".into())
        .child(
            Button::new("settings-image-quality-less")
                .ghost()
                .icon(IconName::Minus)
                .tooltip("Lower image quality")
                .disabled(!options.enabled || options.quality <= 1)
                .on_click(move |_, _, cx| {
                    let mut current = reader_ui_state::image_import_options(cx);
                    current.quality = current.quality.saturating_sub(5).max(1);
                    reader_ui_state::set_image_import_options(current, cx);
                }),
        )
        .child(
            div()
                .min_w(px(60.))
                .text_center()
                .child(format!("{}%", options.quality)),
        )
        .child(
            Button::new("settings-image-quality-more")
                .ghost()
                .icon(IconName::Plus)
                .tooltip("Higher image quality")
                .disabled(!options.enabled || options.quality >= 100)
                .on_click(move |_, _, cx| {
                    let mut current = reader_ui_state::image_import_options(cx);
                    current.quality = (current.quality + 5).min(100);
                    reader_ui_state::set_image_import_options(current, cx);
                }),
        )
}

fn image_import_settings(cx: &App) -> impl IntoElement {
    let options = reader_ui_state::image_import_options(cx);
    v_flex().gap_4()
        .child(setting_row("Convert images to WebP",
            "Compress pasted and imported images for all vaults. Animations, vectors and unsupported formats keep their original format.",
            Switch::new("settings-image-webp").accessibility_label("Convert images to WebP")
                .checked(options.enabled).on_click(|value, _, cx| {
                    let mut current = reader_ui_state::image_import_options(cx);
                    current.enabled = *value;
                    reader_ui_state::set_image_import_options(current, cx);
                }), cx))
        .child(setting_row("Image quality", "Higher quality keeps more detail and uses more space.", image_quality_stepper(cx), cx))
        .child(setting_row("Keep original images",
            "Save a separate original beside each converted image. Files imported from outside the vault are always left untouched.",
            Switch::new("settings-image-original").accessibility_label("Keep original images")
                .checked(options.keep_original).disabled(!options.enabled).on_click(|value, _, cx| {
                    let mut current = reader_ui_state::image_import_options(cx);
                    current.keep_original = *value;
                    reader_ui_state::set_image_import_options(current, cx);
                }), cx))
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
/// A custom colour (#1399) adds its own swatch, drawn as it is shown.
fn vault_color_swatches(root: &Path, cx: &App) -> impl IntoElement {
    let p = brand::palette(cx);
    let current = reader_ui_state::vault_color(root, cx);
    let swatch = |color: Option<brand::VaultColor>, selected: bool| {
        let root = root.to_owned();
        let label: SharedString = match color {
            Some(color @ brand::VaultColor::Custom(_)) => {
                format!("{} {}", color.label(), color.key().to_uppercase()).into()
            }
            color => color.map_or("None", brand::VaultColor::label).into(),
        };
        let id = vault_color_swatch_id(color);
        div()
            .id(SharedString::from(id.clone()))
            .debug_selector(move || id.clone())
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
                gpui_component::tooltip::Tooltip::new(label.clone()).build(window, cx)
            })
            .child(div().size_full().rounded_full().map(|dot| match color {
                Some(color) => dot.bg(color.color(cx)),
                None => dot.border_1().border_color(p.text_muted),
            }))
            .on_click(move |_, _, cx| reader_ui_state::set_vault_color(&root, color, cx))
    };
    h_flex().gap_1().children(
        vault_color_swatch_choices(current)
            .into_iter()
            .map(|(color, selected)| swatch(color, selected)),
    )
}

/// Every swatch in order with whether it is the current colour: «None»,
/// the presets, then the custom colour when one is set.
fn vault_color_swatch_choices(
    current: Option<brand::VaultColor>,
) -> Vec<(Option<brand::VaultColor>, bool)> {
    let custom = current.filter(|color| matches!(color, brand::VaultColor::Custom(_)));
    std::iter::once(None)
        .chain(brand::VaultColor::ALL.map(Some))
        .chain(custom.map(Some))
        .map(|color| (color, color == current))
        .collect()
}

/// `settings-vault-color-<key>`; every custom colour shares one id.
fn vault_color_swatch_id(color: Option<brand::VaultColor>) -> String {
    match color {
        None => "settings-vault-color-none".into(),
        Some(brand::VaultColor::Custom(_)) => "settings-vault-color-custom".into(),
        Some(color) => format!("settings-vault-color-{}", color.key()),
    }
}

/// #1399: help under «Custom colour», and the same line when the typed
/// text is not a colour, so the row never changes height.
const CUSTOM_COLOR_HELP: &str =
    "Any colour as #RRGGBB. It may be shown lighter or darker to stay visible.";
const CUSTOM_COLOR_INVALID: &str = "Not a colour. Type # and six hex digits, like #3A7BD5.";

fn compact_vault_path(root: &Path) -> String {
    // The plain form, so a verbatim `\\?\C:\…` root still sits under the home folder (#1391).
    let root = &okilum_core::vault::plain_path(root);
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

/// Settings → Inbox (#1356): what it is, and that there is nothing to set up.
fn log_sources_heading(root: Option<&Path>, cx: &App) -> String {
    match root {
        Some(root) => format!("Log sources · {}", vault_name::shown(root, cx)),
        None => "Log sources".into(),
    }
}

const INBOX_HELP: &str = "Inbox brings in notes you send from your phone or other apps. Connecting to it will come later.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Features,
    /// The T3 server of the open project's Manager (#1507).
    T3,
    Appearance,
    /// Editing notes: pasting and dropping files (#1611).
    Editor,
    Keyboard,
    /// Current vault settings and the other known vaults (#2572).
    Vaults,
    /// Log source links: for logs of software you build (#1355).
    Logs,
    Updates,
    About,
    Inbox,
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    Sync,
}
impl Section {
    fn available(self, cx: &App) -> bool {
        match self {
            Self::T3 => reader_features::enabled(reader_features::Feature::Agents, cx),
            _ => true,
        }
    }
    fn key(self) -> &'static str {
        match self {
            Self::Features => "features",
            Self::T3 => "t3",
            Self::Editor => "editor",
            Self::Keyboard => "keyboard",
            Self::Appearance => "appearance",
            Self::Vaults => "vaults",
            Self::Logs => "logs",
            Self::Updates => "updates",
            Self::About => "about",
            Self::Inbox => "inbox",
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            Self::Sync => "sync",
        }
    }
    fn remembered(cx: &App) -> Self {
        match reader_ui_state::settings_section(cx) {
            Some("features") => Self::Features,
            Some("t3") => Self::T3,
            Some("editor") => Self::Editor,
            Some("keyboard") => Self::Keyboard,
            Some("appearance") => Self::Appearance,
            Some("vaults") => Self::Vaults,
            Some("logs") => Self::Logs,
            Some("updates") => Self::Updates,
            Some("inbox") => Self::Inbox,
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            Some("sync") => Self::Sync,
            _ => Self::About,
        }
    }

    fn icon(self) -> Icon {
        match self {
            Self::Features => Icon::new(IconName::Settings),
            Self::T3 => Icon::new(IconName::SquareTerminal),
            Self::Appearance => Icon::new(IconName::Palette),
            Self::Editor => Icon::new(IconName::FileText),
            Self::Keyboard => Icon::new(IconName::Settings),
            Self::Vaults => Icon::new(IconName::Folder),
            Self::Logs => Icon::new(IconName::SquareTerminal),
            Self::Updates => Icon::default().path("icons/arrow-down-circle.svg"),
            Self::About => Icon::new(IconName::Info),
            Self::Inbox => Icon::new(IconName::Inbox),
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            Self::Sync => Icon::new(IconName::RotateCw),
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Features => "Features",
            Self::T3 => "T3",
            Self::Appearance => "Appearance",
            Self::Editor => "Editor",
            Self::Keyboard => "Keyboard Shortcuts",
            Self::Vaults => "Vaults",
            Self::Logs => "Logs",
            Self::Updates => "Updates",
            Self::About => "About",
            Self::Inbox => "Inbox",
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            Self::Sync => "Sync",
        }
    }
}
struct Settings {
    vaults: Option<Entity<reader_settings_vaults::Vaults>>,
    _vaults_changes: Option<Subscription>,
    content_scroll: ScrollHandle,
    t3: Option<Entity<reader_t3_settings::T3Settings>>,
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    sync: Option<Entity<reader_settings_sync::SyncSettings>>,
    section: Section,
    theme_picker: Entity<theme_picker::ThemePicker>,
    font_picker: Entity<reader_fonts::Picker>,
    keyboard: Option<Entity<reader_keymap::KeyboardSettings>>,
    #[cfg(all(target_os = "linux", feature = "settings-ui-harness"))]
    preview_beta: Option<bool>,
    reader: Option<WeakEntity<Reader>>,
    focus: FocusHandle,
    _reader_changes: Option<Subscription>,
    _features: Subscription,
    #[cfg(any(unix, windows))]
    template_root: Option<PathBuf>,
    #[cfg(any(unix, windows))]
    template_folder: String,
    #[cfg(any(unix, windows))]
    template_error: Option<String>,
    #[cfg(any(unix, windows))]
    template_pending: bool,
    /// The templates folder is not in the vault yet (#1355).
    #[cfg(any(unix, windows))]
    template_missing: bool,
    #[cfg(any(unix, windows))]
    template_epoch: u64,
    /// The custom editor command line for log source links (#1165), made
    /// on first render because an input needs the window.
    editor_command: Option<(Entity<InputState>, Subscription)>,
    /// The build path of a new source-link mapping (#1165).
    map_from: Option<Entity<InputState>>,
    /// The typed custom vault colour (#1399), made on first render.
    vault_color_input: Option<(Entity<InputState>, Subscription)>,
    /// The text the last Apply rejected as not a `#RRGGBB` colour.
    vault_color_rejected: Option<SharedString>,
    vault_color_picker: Option<(Entity<ColorPickerState>, Subscription)>,
    /// The saved value the draft was opened against; presets and other windows
    /// reset the draft rather than leaving a picker for a different accent.
    vault_color_saved: Option<Option<brand::VaultColor>>,
}
impl Settings {
    fn close_button(&self, cx: &Context<Self>) -> Button {
        Button::new("settings-close")
            .debug_selector(|| "settings-close".into())
            .small()
            .ghost()
            .label("Close")
            .accessibility_label("Close Settings")
            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx)))
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        reader_keymap::stop_recording(window, cx);
        window.blur(cx);
        // GPUI delivers focus/blur events during drawing. Let inputs and their
        // Blur subscribers finish before removing the window and its views.
        window.defer(cx, |window, cx| {
            window.draw(cx).clear(cx);
            window.defer(cx, |window, _| window.remove_window());
        });
    }

    fn enable_section(&mut self, _section: Section, _cx: &mut Context<Self>) -> bool {
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        if _section == Section::Sync {
            reader_features::set_enabled(reader_features::Feature::Sync, true, _cx);
            if !reader_features::enabled(reader_features::Feature::Sync, _cx) {
                return false;
            }
        }
        true
    }

    fn select_section(&mut self, section: Section, cx: &mut Context<Self>) {
        if section != Section::Keyboard {
            if let Some(keyboard) = &self.keyboard {
                keyboard.update(cx, |keyboard, cx| keyboard.deactivate(cx));
            }
        }
        if !self.enable_section(section, cx) {
            self.section = Section::Features;
            cx.notify();
            return;
        }
        self.content_scroll.set_offset(point(px(0.), px(0.)));
        self.section = if section.available(cx) {
            section
        } else {
            Section::Features
        };
        if section == Section::Vaults {
            if let Some(vaults) = &self.vaults {
                vaults.update(cx, |vaults, cx| vaults.reload(cx));
            }
        }

        reader_ui_state::set_settings_section(section.key(), cx);
        cx.notify();
    }

    /// Explicit entries do not replace the pane chosen in the sidebar (#1496).
    fn show_entry_section(&mut self, section: Section, cx: &mut Context<Self>) {
        if section != Section::Keyboard {
            if let Some(keyboard) = &self.keyboard {
                keyboard.update(cx, |keyboard, cx| keyboard.deactivate(cx));
            }
        }
        if !self.enable_section(section, cx) {
            self.section = Section::Features;
            cx.notify();
            return;
        }
        self.content_scroll.set_offset(point(px(0.), px(0.)));
        self.section = if section.available(cx) {
            section
        } else {
            Section::Features
        };
        cx.notify();
    }

    fn new(reader: Option<WeakEntity<Reader>>, cx: &mut Context<Self>) -> Self {
        let observer = reader
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .map(|reader| cx.observe(&reader, |_, _, cx| cx.notify()));
        Self {
            _features: cx.observe_global::<reader_features::Features>(|this, cx| {
                this.t3 = None;
                #[cfg(any(target_os = "linux", target_os = "macos", windows))]
                {
                    this.sync = None;
                    if this.section == Section::Sync
                        && !reader_features::enabled(reader_features::Feature::Sync, cx)
                    {
                        this.section = Section::Features;
                    }
                }
                if !this.section.available(cx) {
                    this.section = Section::Features;
                }
                cx.notify();
            }),
            vaults: None,
            _vaults_changes: None,
            content_scroll: ScrollHandle::new(),

            t3: None,
            section: Section::remembered(cx),
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            sync: None,
            theme_picker: cx.new(|cx| theme_picker::ThemePicker::new(None, cx)),
            font_picker: cx.new(|_| reader_fonts::Picker::default()),
            keyboard: None,
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
            vault_color_input: None,
            vault_color_rejected: None,
            vault_color_picker: None,
            vault_color_saved: None,
            #[cfg(any(unix, windows))]
            template_root: None,
            #[cfg(any(unix, windows))]
            template_folder: reader_templates::DEFAULT_FOLDER.into(),
            #[cfg(any(unix, windows))]
            template_error: None,
            #[cfg(any(unix, windows))]
            template_pending: false,
            #[cfg(any(unix, windows))]
            template_missing: false,
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
        self.t3 = None;
        self.vaults = None;
        self._vaults_changes = None;
        // Another vault: the field starts from that vault's own colour.
        self.vault_color_input = None;
        self.vault_color_picker = None;
        self.vault_color_saved = None;
        self.vault_color_rejected = None;
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
        self.template_missing = false;
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
                .spawn(async move {
                    reader_templates::load(&root, &state)
                        .map(|folder| (!root.join(&folder).is_dir(), folder))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.template_epoch != epoch {
                    return;
                }
                this.template_pending = false;
                match result {
                    Ok((missing, folder)) => {
                        this.template_missing = missing;
                        this.template_folder = folder;
                    }
                    Err(error) => {
                        // The error says what is wrong; no second message.
                        this.template_missing = false;
                        this.template_error = Some(format!("{error:#}"));
                    }
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
                        // Chosen in the picker, so it is there.
                        this.template_missing = false;
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

    /// #2202: the library picker and precise hex input edit one draft. Only
    /// Apply/Enter changes the saved accent; Cancel restores the saved value.
    fn custom_vault_color_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = brand::palette(cx);
        let invalid = self.vault_color_rejected.is_some();
        let input = self
            .vault_color_input
            .as_ref()
            .map(|(input, _)| input.clone());
        let saved = self
            .vault(cx)
            .and_then(|reader| reader_ui_state::vault_color(&reader.read(cx).vault_root, cx));
        v_flex()
            .w_full()
            .gap_2()
            .py_2()
            .child("Custom colour")
            .child(
                div()
                    .debug_selector(|| "settings-vault-color-custom-help".into())
                    .text_sm()
                    .text_color(if invalid { p.danger } else { p.text_muted })
                    .child(if invalid {
                        CUSTOM_COLOR_INVALID
                    } else {
                        CUSTOM_COLOR_HELP
                    }),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("settings-vault-color-current")
                            .debug_selector(|| "settings-vault-color-current".into())
                            .size_5()
                            .rounded_md()
                            .border_1()
                            .border_color(p.border)
                            .bg(saved.map(|color| color.color(cx)).unwrap_or(p.accent))
                            .tooltip(|window, cx| {
                                gpui_component::tooltip::Tooltip::new("Current accent")
                                    .build(window, cx)
                            }),
                    )
                    .children(self.vault_color_picker.as_ref().map(|(picker, _)| {
                        div()
                            .debug_selector(|| "settings-vault-color-picker".into())
                            .child(
                                ColorPicker::new(picker)
                                    .label("Choose colour")
                                    .accessibility_label("Choose custom accent colour")
                                    .small(),
                            )
                    }))
                    .children(input.map(|input| {
                        div()
                            .w(px(110.))
                            .debug_selector(|| "settings-vault-color-input".into())
                            .child(Input::new(&input).small())
                    }))
                    .child(
                        Button::new("settings-vault-color-apply")
                            .debug_selector(|| "settings-vault-color-apply".into())
                            .outline()
                            .small()
                            .label("Apply")
                            .on_click(
                                cx.listener(|this, _, _, cx| this.apply_custom_vault_color(cx)),
                            ),
                    )
                    .child(
                        Button::new("settings-vault-color-cancel")
                            .debug_selector(|| "settings-vault-color-cancel".into())
                            .ghost()
                            .small()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.reset_custom_vault_color(window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn reset_custom_vault_color(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let saved = self
            .vault(cx)
            .and_then(|reader| reader_ui_state::vault_color(&reader.read(cx).vault_root, cx));
        let text = saved
            .filter(|color| matches!(color, brand::VaultColor::Custom(_)))
            .map(|color| color.key().to_uppercase())
            .unwrap_or_default();
        let color = saved
            .map(|color| match color {
                brand::VaultColor::Custom([r, g, b]) => {
                    rgb(u32::from_be_bytes([0, r, g, b])).into()
                }
                preset => preset.color(cx),
            })
            .unwrap_or_else(|| brand::palette(cx).accent);
        if let Some((input, _)) = &self.vault_color_input {
            input.update(cx, |input, cx| input.set_value(text, window, cx));
        }
        if let Some((picker, _)) = &self.vault_color_picker {
            picker.update(cx, |picker, cx| {
                picker.set_open(false, cx);
                picker.set_value(color, window, cx);
            });
        }
        self.vault_color_saved = Some(saved);
        self.vault_color_rejected = None;
        cx.notify();
    }

    fn apply_custom_vault_color(&mut self, cx: &mut Context<Self>) {
        let (Some(reader), Some((input, _))) = (self.vault(cx), self.vault_color_input.as_ref())
        else {
            return;
        };
        let root = reader.read(cx).vault_root.clone();
        let text = input.read(cx).value();
        match brand::VaultColor::from_hex(&text) {
            Some(color) => {
                self.vault_color_rejected = None;
                reader_ui_state::set_vault_color(&root, Some(color), cx);
            }
            None => self.vault_color_rejected = Some(text),
        }
        cx.notify();
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
                    if self.template_missing && !self.template_pending {
                        "Templates for new notes. This folder is not in the vault yet."
                    } else {
                        "Templates for new notes."
                    },
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
            Section::Features => content
                .child("Keep the editor fast. Enable optional modules when you need them.")
                .children(reader_features::Feature::ALL.map(|feature| {
                    setting_row(feature.label(), feature.description(),
                        div().debug_selector(move || format!("settings-feature-{}", feature.key())).child(
                            Switch::new(SharedString::from(format!("feature-{}", feature.key())))
                                .accessibility_label(feature.label())
                                .checked(reader_features::enabled(feature, cx))
                                .disabled(!feature.available() || (reader_features::error(cx).is_some() && !reader_features::enabled(feature, cx)))
                                .on_click(move |value, _, cx| reader_features::set_enabled(feature, *value, cx)),
                        ), cx)
                }))
                .children(reader_features::error(cx).map(|error| div().text_color(p.danger).child(error)))
                .into_any_element(),

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
                    .child(setting_row(
                        "Open another vault",
                        "Choose where another vault opens. New Window always creates a window.",
                        h_flex().gap_1().children([
                            ("current", "In this window"),
                            ("new", "In a new window"),
                            ("ask", "Ask every time"),
                        ].into_iter().map(|(value, label)| {
                            Button::new(SharedString::from(format!("vault-open-{value}")))
                                .label(label)
                                .selected(reader_ui_state::vault_open(cx) == value)
                                .on_click(move |_, _, cx| reader_ui_state::set_vault_open(value, cx))
                        })),
                        cx,
                    ))
                    .child(reader_reading_controls::render(window, cx))
                    .child(setting_row(
                        "File tree labels",
                        "Show file names as on disk, or note titles. Sorting follows the chosen label.",
                        h_flex().gap_1().children([
                            ("settings-tree-file-name", "File name", false),
                            ("settings-tree-note-title", "Title", true),
                        ].into_iter().map(|(id, label, titles)| {
                            Button::new(id)
                                .debug_selector(move || id.into())
                                .label(label)
                                .selected(reader_ui_state::tree_note_titles(cx) == titles)
                                .on_click(move |_, _, cx| reader_ui_state::set_tree_note_titles(titles, cx))
                        })),
                        cx,
                    ))
                    .child(setting_row(
                        "Show the open note in the file tree",
                        "Opening a note expands its folders and selects it in the sidebar. When off, Reveal in sidebar does this on request.",
                        div()
                            .debug_selector(|| "settings-tree-auto-reveal-control".into())
                            .child(
                                Switch::new("settings-tree-auto-reveal")
                                    .accessibility_label("Show the open note in the file tree")
                                    .checked(reader_ui_state::tree_auto_reveal(cx))
                                    .on_click(|value, _, cx| {
                                        reader_ui_state::set_tree_auto_reveal(*value, cx)
                                    }),
                            ),
                        cx,
                    ))
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
                    .child(self.font_picker.clone())
                    .into_any_element()
            }
            Section::Keyboard => content.children(self.keyboard.clone()).into_any_element(),
            Section::Editor => content
                .child(image_import_settings(cx))
                .child(setting_row(
                    "Ask before copying files larger than",
                    "Pasting or dropping a file larger than this asks whether to copy it into the vault or link to the original.",
                    paste_size_stepper(cx),
                    cx,
                ))
                .child(setting_row(
                    "Close brackets and quotes",
                    "Typing ( [ { \" ' or « adds the closing one; typing it again steps over it.",
                    div()
                        .debug_selector(|| "settings-auto-pair-brackets-control".into())
                        .child(
                            Switch::new("settings-auto-pair-brackets")
                                .accessibility_label("Close brackets and quotes")
                                .checked(reader_ui_state::auto_pair(cx).0)
                                .on_click(|value, _, cx| {
                                    let (_, markdown) = reader_ui_state::auto_pair(cx);
                                    reader_ui_state::set_auto_pair(*value, markdown, cx)
                                }),
                        ),
                    cx,
                ))
                .child(setting_row(
                    "Close Markdown marks",
                    "Typing * _ or ` adds the closing one. Typing any of them, or ~ and =, around a selection wraps it either way.",
                    div()
                        .debug_selector(|| "settings-auto-pair-markdown-control".into())
                        .child(
                            Switch::new("settings-auto-pair-markdown")
                                .accessibility_label("Close Markdown marks")
                                .checked(reader_ui_state::auto_pair(cx).1)
                                .on_click(|value, _, cx| {
                                    let (brackets, _) = reader_ui_state::auto_pair(cx);
                                    reader_ui_state::set_auto_pair(brackets, *value, cx)
                                }),
                        ),
                    cx,
                ))
                .child(setting_row(
                    "Close code fences",
                    "Typing three backticks or tildes at a line start adds the matching closing fence.",
                    div().debug_selector(|| "settings-close-code-fences-control".into()).child(
                        Switch::new("settings-close-code-fences")
                            .accessibility_label("Close code fences")
                            .checked(reader_ui_state::close_code_fences(cx))
                            .on_click(|value, _, cx| reader_ui_state::set_close_code_fences(*value, cx)),
                    ), cx,
                ))
                .into_any_element(),
            Section::Vaults => {
                let card = if let Some(reader) = self.vault(cx) {
                    let state = reader.read(cx);
                    let hidden = state.sidebar.show_hidden;
                    let root = state.vault_root.clone();
                    let name = crate::vault_name::shown(&root, cx);
                    let rename_root = root.clone();
                    let full_path = okilum_core::vault::display_path(&root);
                    let short_path = compact_vault_path(&root);
                    let color_root = root.clone();
                    let weak = reader.downgrade();
                    let now_reader = weak.clone();
                    let now_enabled = state.sidebar.now_enabled();
                    v_flex()
                        .debug_selector(|| "settings-current-vault".into())
                        .gap_4().p_3().border_1().border_color(p.accent).rounded(px(6.))
                        .child(
                            h_flex()
                                .gap_3()
                                .children(reader_ui_state::vault_color(&color_root, cx).map(|color| {
                                    div().size_2().flex_none().rounded_full().bg(color.color(cx))
                                }))
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap_1()
                                        .child(h_flex().flex_wrap().gap_2()
                                            .child(div().font_weight(FontWeight::MEDIUM).child(name))
                                            .child(div().debug_selector(|| "settings-vault-current-badge".into())
                                                .text_xs().px_1().rounded(px(3.)).bg(p.selected).text_color(p.accent).child("Current"))
                                            .when(self.vaults.as_ref().is_some_and(|vaults| vaults.read(cx).is_default(&color_root)), |row| {
                                                row.child(div().debug_selector(|| "settings-vault-default-badge".into())
                                                    .text_xs().px_1().rounded(px(3.)).bg(p.sidebar).text_color(p.text_muted).child("Default"))
                                            }))
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
                        .children(self.vaults.as_ref().map(|vaults| {
                            reader_settings_vaults::Vaults::current_actions(vaults, &color_root, cx)
                        }))
                        .child(setting_row(
                            "Now sidebar",
                            "Show project activity alongside the file tree in this vault.",
                            div().debug_selector(|| "settings-now-sidebar".into()).child(
                                Switch::new("settings-now-sidebar-switch")
                                    .accessibility_label("Now sidebar")
                                    .checked(now_enabled)
                                    .on_click(move |_, window, cx| {
                                        let _ = now_reader.update(cx, |reader, cx| {
                                            reader.set_now_enabled(!now_enabled, window, cx)
                                        });
                                    }),
                            ),
                            cx,
                        ))
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
                        .child(self.custom_vault_color_row(cx))
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
                    div().child("Open a vault to change its name, colour and files.").into_any_element()
                };
                content.child(card).children(self.vaults.clone()).into_any_element()
            }
            Section::Logs => {
                let root = self
                    .vault(cx)
                    .map(|reader| reader.read(cx).vault_root.clone());
                content
                    .child(
                        div().text_sm().text_color(p.text_muted).child(
                            "For log files of software you build: where file:line links open.",
                        ),
                    )
                    .child(self.log_editor_controls(cx))
                    .child(div().text_sm().child("Saved regex definitions. Create and apply them from the Regex button in a log file. One saved regex applies automatically to Plain text files; with several, choose one."))
                    .children(reader_ui_state::log_regex_definitions(cx).into_iter().map(|definition| {
                        let name = definition.name.clone();
                        h_flex().gap_2()
                            .child(div().flex_1().child(definition.name))
                            .child(Button::new(SharedString::from(format!("delete-regex-{name}"))).small().label("Delete").on_click(cx.listener(move |_, _, _, cx| {
                                reader_ui_state::delete_log_regex(&name, cx);
                                cx.notify();
                            })))
                    }))
                    .child(v_flex().gap_3().border_t_1().border_color(p.border_subtle).pt_4()
                        .debug_selector(|| "settings-log-sources".into())
                        .child(h_flex().gap_2()
                            .children(root.as_ref().and_then(|root| reader_ui_state::vault_color(root, cx)).map(|color| {
                                div().size_2().flex_none().rounded_full().bg(color.color(cx))
                            }))
                            .child(div().debug_selector(|| "settings-log-sources-heading".into())
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(log_sources_heading(root.as_deref(), cx))))
                        .child(div().text_sm().text_color(p.text_muted)
                            .child("Stored with this vault. Switch vault to edit another vault's sources."))
                        .when(root.is_none(), |block| block.child("Open a vault to edit its log sources."))
                    .child(h_flex().flex_wrap().gap_2()
                    .child(Button::new("settings-https-log-sources").debug_selector(|| "settings-https-log-sources".into()).label("HTTPS log files…").disabled(self.vault(cx).is_none()).on_click(cx.listener(|this, _, _, cx| {
                        if let Some(reader) = this.vault(cx) {
                            let root = reader.read(cx).vault_root.clone();
                            reader_log::http::show(root, cx);
                        }
                    })))
                    .child(Button::new("settings-kibana-sources").debug_selector(|| "settings-kibana-sources".into()).label("Kibana sources…").disabled(self.vault(cx).is_none()).on_click(cx.listener(|this,_,_,cx| {
                        if let Some(reader)=this.vault(cx) {
                            let root=reader.read(cx).vault_root.clone();
                            reader_log::remote::show(root,cx);
                        }
                    }))))
                    .child(div().text_sm().child("Import Discover URLs for the current vault. API keys stay in OS credential storage; loaded events stay in memory."))
                    .children(root.map(|root| self.source_map_controls(&root, cx))))
                    .into_any_element()
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
                        .or_else(|| match updater::channel() {
                            "Beta" => Some(true),
                            "Stable" => Some(false),
                            _ => None,
                        });
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
                                                    let selected = beta == Some(value);
                                                    Button::new(id)
                                                        .ghost()
                                                        .debug_selector(move || id.into())
                                                        .label(label)
                                                        .icon(Icon::default().path(icon))
                                                        .selected(selected)
                                                        .disabled(updater::status::in_flight())
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
                        .child(v_flex().gap_2().children(updater::Mode::ALL.map(|mode| {
                            Button::new(SharedString::from(format!("settings-update-mode-{mode:?}")))
                                .debug_selector(move || format!("settings-update-mode-{mode:?}"))
                                .ghost()
                                .label(mode.label())
                                .selected(updater::mode() == mode)
                                .on_click(move |_, _, cx| updater::set_mode(mode, cx))
                        })))
                        .child(div().text_sm().text_color(p.text_muted)
                            .child("Downloaded updates are installed only when you choose Restart now."))
                        .children(update_status_row("settings", cx))
                        .child(div().text_sm().text_color(p.text_muted).child(match beta {
                            Some(true) => "Beta selected — preview new features and fixes.",
                            Some(false) => "Stable selected — receive approved releases.",
                            None => "Connect and check for updates to determine the channel, or choose one above.",
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
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            Section::Sync => div().size_full().children(self.sync.clone()).into_any_element(),
            Section::T3 => content.children(self.t3.clone()).into_any_element(),
            Section::About => content.child(about::content(cx)).into_any_element(),
            // Nothing to do here yet, and it says so (#1356): connecting an
            // Inbox server is not offered in the Reader.
            Section::Inbox => content
                .child("Not connected")
                .child(div().text_sm().text_color(p.text_muted).child(INBOX_HELP))
                .into_any_element(),
        }
    }
}
impl Render for Settings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.section.available(cx) {
            self.section = Section::Features;
        }
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        if self.section == Section::Sync
            && !reader_features::enabled(reader_features::Feature::Sync, cx)
        {
            self.section = Section::Features;
        }
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        if self.section == Section::Sync
            && reader_features::enabled(reader_features::Feature::Sync, cx)
            && self.sync.is_none()
        {
            self.sync = Some(reader_settings_sync::shared(window, cx));
        }
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
        if self.vault_color_picker.is_none() {
            let picker = cx.new(|cx| ColorPickerState::new(window, cx));
            let subscription = cx.subscribe_in(&picker, window, |this, _, event, window, cx| {
                let ColorPickerEvent::Change(Some(color)) = event else {
                    return;
                };
                // Accents are opaque RGB. The reusable picker also supports alpha,
                // which does not change the accent's existing #RRGGBB contract.
                let rgb = color.to_rgb();
                let text = format!(
                    "#{:02X}{:02X}{:02X}",
                    (rgb.r * 255.).round() as u8,
                    (rgb.g * 255.).round() as u8,
                    (rgb.b * 255.).round() as u8
                );
                if let Some((input, _)) = &this.vault_color_input {
                    input.update(cx, |input, cx| input.set_value(text, window, cx));
                }
                this.vault_color_rejected = None;
                cx.notify();
            });
            self.vault_color_picker = Some((picker, subscription));
        }
        if self.vault_color_input.is_none() {
            let current = self
                .vault(cx)
                .and_then(|reader| reader_ui_state::vault_color(&reader.read(cx).vault_root, cx))
                .filter(|color| matches!(color, brand::VaultColor::Custom(_)))
                .map(|color| color.key().to_uppercase())
                .unwrap_or_default();
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("#RRGGBB")
                    .default_value(current)
            });
            let subscription = cx.subscribe_in(
                &input,
                window,
                |this, input, event: &InputEvent, window, cx| {
                    match event {
                        InputEvent::PressEnter { .. } => this.apply_custom_vault_color(cx),
                        // Editing the rejected text clears the complaint; nothing
                        // is applied while typing. Enter also reports a change
                        // without editing, so compare the text itself.
                        InputEvent::Change => {
                            let text = input.read(cx).value();
                            if this
                                .vault_color_rejected
                                .as_ref()
                                .is_some_and(|rejected| *rejected != text)
                            {
                                this.vault_color_rejected = None;
                            }
                            if let Some(brand::VaultColor::Custom([r, g, b])) =
                                brand::VaultColor::from_hex(&text)
                            {
                                if let Some((picker, _)) = &this.vault_color_picker {
                                    picker.update(cx, |picker, cx| {
                                        picker.set_value(
                                            rgb(u32::from_be_bytes([0, r, g, b])),
                                            window,
                                            cx,
                                        )
                                    });
                                }
                            }
                            cx.notify();
                        }
                        _ => {}
                    }
                },
            );
            self.vault_color_input = Some((input, subscription));
        }
        let saved = self
            .vault(cx)
            .and_then(|reader| reader_ui_state::vault_color(&reader.read(cx).vault_root, cx));
        if self.vault_color_saved != Some(saved) {
            self.reset_custom_vault_color(window, cx);
        }
        if self.map_from.is_none() {
            self.map_from =
                Some(cx.new(|cx| {
                    InputState::new(window, cx).placeholder("Build path, e.g. /src/ordgw")
                }));
        }
        if self.section == Section::T3 && self.t3.is_none() {
            self.t3 =
                Some(cx.new(|cx| {
                    reader_t3_settings::T3Settings::new(self.reader.clone(), window, cx)
                }));
        }
        if self.section == Section::Vaults && self.vaults.is_none() {
            let directory = self
                .reader
                .as_ref()
                .and_then(WeakEntity::upgrade)
                .and_then(|reader| reader.read(cx).session_directory.clone())
                .map(Ok)
                .unwrap_or_else(|| reader_ui_state::state_directory(cx));
            let vaults = cx
                .new(|cx| reader_settings_vaults::Vaults::new(directory, self.reader.clone(), cx));
            self._vaults_changes = Some(cx.observe(&vaults, |_, _, cx| cx.notify()));
            self.vaults = Some(vaults);
        }
        if self.section == Section::Keyboard && self.keyboard.is_none() {
            self.keyboard =
                Some(cx.new(|cx| {
                    reader_keymap::KeyboardSettings::new(self.reader.clone(), window, cx)
                }));
        }
        let p = brand::palette(cx);
        let body = self.body(window, cx);
        let content = h_flex()
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if reader_keymap::is_recording(window, cx) {
                    return;
                }
                if event.keystroke.key == "escape"
                    && event.keystroke.modifiers == Modifiers::default()
                    && !window.has_active_dialog(cx)
                {
                    this.close(window, cx);
                    cx.stop_propagation();
                }
            }))
            .flex_1()
            .min_h_0()
            .w_full()
            .items_start()
            .child(
                v_flex()
                    .w(px(176.))
                    .h_full()
                    .flex_none()
                    .p_3()
                    .gap_1()
                    .children(
                        [
                            Section::Features,
                            Section::Appearance,
                            Section::Editor,
                            Section::Keyboard,
                            Section::Vaults,
                            Section::T3,
                            Section::Updates,
                            Section::Inbox,
                            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
                            Section::Sync,
                            // Developer options sit low (#1355).
                            Section::Logs,
                            // Platform convention: About comes last (#995).
                            Section::About,
                        ]
                        .into_iter()
                        .filter(|section| section.available(cx))
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
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.select_section(section, cx);
                                        })),
                                )
                        }),
                    ),
            )
            .child(
                div()
                    .id("settings-content")
                    .track_scroll(&self.content_scroll)
                    .debug_selector(|| "settings-content".into())
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .min_h_0()
                    .when(self.section.key() == "sync", |this| this.overflow_hidden())
                    .when(self.section.key() != "sync", |this| {
                        this.overflow_y_scroll()
                    })
                    .p_6()
                    .child(body),
            );
        v_flex()
            .size_full()
            .bg(p.surface)
            .text_color(p.text)
            .key_context("OkilumSettings")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &CloseSettings, window, cx| this.close(window, cx)))
            .on_action(cx.listener(|_, _: &OpenSettings, window, cx| {
                window.activate_window();
                cx.stop_propagation();
            }))
            // GPUI negotiates SSD through xdg-decoration and reports CSD when
            // unavailable (GNOME). TitleBar hides its controls under SSD.
            .when(!cfg!(target_os = "macos"), |view| {
                view.child(
                    TitleBar::new().child("Settings").child(
                        h_flex()
                            .h_full()
                            .items_center()
                            .pr_3()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(self.close_button(cx)),
                    ),
                )
            })
            .when(cfg!(target_os = "macos"), |view| {
                // Keep the explicit Close action in client content. A GPUI
                // control over the native caption would steal its drag region.
                // Dialogs leave the first TITLE_BAR_HEIGHT client pixels
                // interactive; a footer would be covered by the Sync backdrop.
                view.child(
                    h_flex()
                        .debug_selector(|| "settings-window-actions".into())
                        .flex_none()
                        .h(gpui_component::TITLE_BAR_HEIGHT)
                        .items_center()
                        .justify_end()
                        .pr_3()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(self.close_button(cx)),
                )
            })
            .child(content)
            .children({
                #[cfg(any(target_os = "linux", target_os = "macos", windows))]
                {
                    (self.section != Section::Sync)
                        .then(|| reader_settings_sync::presentation::approval_banner(cx))
                }
                #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
                {
                    None::<AnyElement>
                }
            })
            // Confirmations (Uninstall Okilum…) open as dialogs on this window;
            // without the dialog layer they exist but are never drawn (#1085).
            .children(Root::render_sheet_layer(window, cx))
            .when(self.section.key() != "sync", |view| {
                view.children(Root::render_dialog_layer(window, cx))
            })
            .children(Root::render_notification_layer(window, cx))
    }
}

/// The last check's outcome, inline next to the check button (#995). A found
/// update offers its install action here instead of a modal.
pub(crate) fn update_status_row(id: &'static str, cx: &App) -> Option<AnyElement> {
    render_update_status(id, updater::status::get(), updater::restart_now, cx)
}

pub(crate) fn render_update_status(
    id: &'static str,
    status: updater::status::CheckStatus,
    restart: impl Fn(&mut App) + 'static,
    cx: &App,
) -> Option<AnyElement> {
    let restart = std::rc::Rc::new(restart);
    let line = updater::status::text(&status, std::time::SystemTime::now())?;
    let progress = updater::status::progress(&status);
    let p = brand::palette(cx);
    let available = matches!(status, updater::status::CheckStatus::Available(_));
    Some(
        h_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .items_center()
            .flex_wrap()
            .child(
                div()
                    .debug_selector(move || format!("{id}-update-status"))
                    .min_w_0()
                    .max_w_full()
                    .whitespace_normal()
                    .text_size(px(13.))
                    .text_color(p.text)
                    .child(line),
            )
            .when_some(progress, |row, progress| {
                row.child(
                    gpui_base::Progress::new(SharedString::from(format!("{id}-update-progress")))
                        .debug_selector(move || format!("{id}-update-progress"))
                        .accessibility_label("Update progress")
                        .value(f32::from(progress.percent.unwrap_or(0)))
                        .indeterminate(progress.percent.is_none())
                        .w(px(200.))
                        .h(px(6.))
                        .rounded_full()
                        .overflow_hidden()
                        .bg(p.surface_raised)
                        .child(if let Some(percent) = progress.percent {
                            div()
                                .h_full()
                                .w(gpui::relative(f32::from(percent) / 100.))
                                .rounded_full()
                                .bg(p.accent)
                                .into_any_element()
                        } else {
                            let fill = div().size_full().rounded_full().bg(p.accent);
                            if cx.reduce_motion() {
                                fill.opacity(0.45).into_any_element()
                            } else {
                                fill.with_animation(
                                    SharedString::from(format!("{id}-update-indeterminate")),
                                    Animation::new(std::time::Duration::from_millis(1200)).repeat(),
                                    |fill, t| fill.opacity(0.25 + 0.5 * (1. - (2. * t - 1.).abs())),
                                )
                                .into_any_element()
                            }
                        }),
                )
                .when(progress.restart, |row| {
                    row.child(
                        Button::new(SharedString::from(format!("{id}-restart-update")))
                            .debug_selector(move || format!("{id}-restart-update"))
                            .small()
                            .label("Restart now")
                            .on_click(move |_, _, cx| restart(cx)),
                    )
                })
            })
            .when(
                matches!(status, updater::status::CheckStatus::Downloading(_))
                    || updater::can_cancel_update(),
                |row| {
                    row.child(
                        Button::new(SharedString::from(format!("{id}-cancel-update")))
                            .debug_selector(move || format!("{id}-cancel-update"))
                            .ghost()
                            .small()
                            .label("Cancel update")
                            .on_click(|_, _, _| updater::cancel_update()),
                    )
                },
            )
            .when(available, |row| {
                row.child(
                    Button::new(SharedString::from(format!("{id}-install-update")))
                        .ghost()
                        .small()
                        .debug_selector(move || format!("{id}-install-update"))
                        .label("Download update")
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

    #[gpui::test]
    fn feature_switches_apply_on_click_and_hide_disabled_module_settings(cx: &mut TestAppContext) {
        use reader_features::{enabled, Feature};
        let temp = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            reader_features::install(temp.path(), cx);
        });
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = Settings::new(None, cx);
                view.section = Section::Features;
                view
            });
            Root::new(view, window, cx)
        });
        visual.simulate_resize(size(px(820.), px(650.)));
        visual.run_until_parked();
        assert!(visual.debug_bounds("settings-section-T3").is_none());
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        assert!(visual.debug_bounds("settings-section-Sync").is_some());
        for feature in Feature::ALL
            .into_iter()
            .filter(|feature| feature.available())
        {
            let selector = match feature {
                Feature::Agents => "settings-feature-agents",
                Feature::SecondBrain => "settings-feature-second_brain",
                Feature::Sync => "settings-feature-sync",
            };
            let toggle = visual.debug_bounds(selector).unwrap();
            visual.simulate_click(toggle.center(), Modifiers::default());
            visual.run_until_parked();
            visual.update(|_, cx| {
                assert!(
                    enabled(feature, cx),
                    "a real switch click enabled {}",
                    feature.label()
                )
            });
        }
        assert!(visual.debug_bounds("settings-section-T3").is_some());
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        assert!(visual.debug_bounds("settings-section-Sync").is_some());
        let toggle = visual.debug_bounds("settings-feature-agents").unwrap();
        visual.simulate_click(toggle.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("settings-section-T3").is_none());
        visual.update(|_, cx| {
            reader_features::install(temp.path(), cx);
            assert!(!enabled(Feature::Agents, cx));
            assert_eq!(
                enabled(Feature::SecondBrain, cx),
                Feature::SecondBrain.available()
            );
            assert_eq!(enabled(Feature::Sync, cx), Feature::Sync.available());
        });
    }

    #[cfg(any(unix, windows))]
    #[gpui::test]
    fn file_tree_label_setting_updates_folder_and_vault_live(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        cx.update(|cx| {
            crate::test_init(cx);
            reader_ui_state::install(&state, cx);
        });
        let mut readers = Vec::new();
        let source = "---\ntitle: Ignored metadata\n---\n# Zebra heading\n";
        for folder in [false, true] {
            let root = fixture.path().join(if folder { "folder" } else { "vault" });
            std::fs::create_dir(&root).unwrap();
            for (path, body) in [
                ("a.md", source),
                ("z.md", "---\ntitle: Alpha metadata\n---\nBody\n"),
                ("_index.md", "# Index title\n"),
                ("data.csv", "x,y\n1,2\n"),
            ] {
                std::fs::write(root.join(path), body).unwrap();
            }
            let mut entity = None;
            let handle = cx.add_window(|window, cx| {
                let reader = cx.new(|cx| {
                    Reader::new(
                        Opts {
                            vault: (!folder).then(|| root.clone()),
                            open_path: folder.then(|| root.join("a.md")),
                            note: (!folder).then(|| "a.md".into()),
                            single_file: folder,
                            session_directory: Some(state.clone()),
                            index_dir: Some(fixture.path().join(if folder {
                                "folder-cache"
                            } else {
                                "vault-cache"
                            })),
                            panel_settings_override: Some(fixture.path().join("panels.json")),
                            ..Default::default()
                        },
                        window,
                        cx,
                    )
                });
                entity = Some(reader.clone());
                Root::new(reader, window, cx)
            });
            readers.push((handle, entity.unwrap(), folder));
        }
        cx.run_until_parked();
        let mut reader_visuals = readers
            .iter()
            .map(|(handle, _, _)| VisualTestContext::from_window((*handle).into(), cx))
            .collect::<Vec<_>>();
        // Reopen through the real production path after the initial inventory is
        // ready. No synthetic title projection: quick folders retain the actual
        // file-name fallback, while the indexed vault has captured source titles.
        for ((_, reader, _), reader_visual) in readers.iter().zip(&mut reader_visuals) {
            reader.update_in(reader_visual, |reader, window, cx| {
                reader.open_note("a.md", None, window, cx);
            });
            reader_visual.run_until_parked();
            reader_visual
                .executor()
                .advance_clock(Duration::from_millis(300));
            reader_visual.run_until_parked();
        }
        let (_, visual) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| {
                let mut settings = Settings::new(Some(readers[0].1.downgrade()), cx);
                settings.section = Section::Appearance;
                settings
            });
            Root::new(settings, window, cx)
        });
        visual.simulate_resize(size(px(1400.), px(1000.)));
        visual.run_until_parked();
        assert!(
            !visual.update(|_, cx| reader_ui_state::tree_note_titles(cx)),
            "file names are the default"
        );
        for (id, titles) in [
            ("settings-tree-file-name", false),
            ("settings-tree-note-title", true),
            ("settings-tree-file-name", false),
        ] {
            let button = visual
                .debug_bounds(id)
                .expect("real Settings control is rendered");
            visual.simulate_click(button.center(), Modifiers::default());
            visual.run_until_parked();
            visual.update(|_, cx| assert_eq!(reader_ui_state::tree_note_titles(cx), titles));
            for ((_, reader, folder), reader_visual) in readers.iter().zip(&mut reader_visuals) {
                // Draw outside WindowHandle<Root>::update: rendering Root while
                // borrowing that entity would recursively update the same Root.
                reader_visual.update(|window, cx| window.draw(cx).clear(cx));
                reader.update_in(reader_visual, |reader, window, cx| {
                    assert_eq!(
                        reader.single_file, *folder,
                        "positive control: both opener modes"
                    );
                    // #2278 gives the root index its own header, independent of
                    // filename/title sorting and hidden-file settings.
                    assert_eq!(reader.vault_index_path().as_deref(), Some("_index.md"));
                    assert!(!reader.tree.rows.iter().any(|row| row.path == "_index.md"));
                    let labels = reader
                        .tree
                        .rows
                        .iter()
                        .map(|r| r.label.as_str())
                        .collect::<Vec<_>>();
                    assert_eq!(
                        labels,
                        if titles && !folder {
                            vec!["Alpha metadata", "Zebra heading", "data.csv"]
                        } else {
                            vec!["a", "data.csv", "z"]
                        },
                        "LABEL1834: Settings must update existing tree rows and their sort live"
                    );
                    assert_eq!(reader.current_rel, "a.md");
                    assert_eq!(
                        reader.current_title,
                        if *folder { "a" } else { "Zebra heading" }
                    );
                    assert_eq!(
                        reader.outline[0].text, "Zebra heading",
                        "the opened note retains its real H1"
                    );
                    assert_eq!(
                        reader.note_label("z.md"),
                        if *folder { "z" } else { "Alpha metadata" },
                        "uncaptured quick-folder sibling titles keep the real filename fallback"
                    );
                    if *folder {
                        assert!(
                            !reader.backlink_titles.contains_key("z.md"),
                            "quick folders do not read sibling bodies for titles"
                        );
                    }
                    reader.focus_current_folder(window, cx);
                    assert_eq!(
                        reader.tree.cursor.as_deref(),
                        Some("a.md"),
                        "Reveal follows path identity"
                    );
                    reader.begin_rename("z.md".into(), window, cx);
                    let rename = reader
                        .renaming
                        .as_ref()
                        .expect("rename targets the real file");
                    assert_eq!(rename.path, "z.md");
                    assert_eq!(
                        rename.input.read(cx).value().as_str(),
                        "z",
                        "rename edits the filename in either mode"
                    );
                    reader.cancel_rename(window, cx);
                    assert_eq!(
                        std::fs::read_to_string(reader.vault_root.join("a.md")).unwrap(),
                        source
                    );
                });
                reader_visual.run_until_parked();
            }
            visual.run_until_parked();
        }
    }

    #[gpui::test]
    fn first_open_defaults_to_about_and_last_pane_survives_restart(cx: &mut TestAppContext) {
        let directory =
            std::env::temp_dir().join(format!("okilum-settings-pane-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            reader_ui_state::install(&directory, cx);
        });
        let owner = cx.add_window(|window, cx| {
            let settings = cx.new(|cx| Settings::new(None, cx));
            assert!(settings.read(cx).section == Section::About);
            Root::new(settings, window, cx)
        });
        cx.update(|cx| show(None, cx));
        cx.run_until_parked();
        let handle = cx.read(|cx| cx.global::<SettingsWindow>().0.unwrap());
        handle
            .update(cx, |root, _, cx| {
                let settings = root.view().clone().downcast::<Settings>().unwrap();
                assert!(settings.read(cx).section == Section::About);
                settings.update(cx, |settings, cx| {
                    settings.select_section(Section::Logs, cx)
                });
            })
            .unwrap();
        cx.update(reader_ui_state::flush);
        // Read a fresh store from disk, rather than keeping an in-memory preference.
        cx.update(|cx| reader_ui_state::install(&directory, cx));
        owner
            .update(cx, |_, _, cx| {
                let settings = cx.new(|cx| Settings::new(None, cx));
                assert!(settings.read(cx).section == Section::Logs);
            })
            .unwrap();
        // Explicit entries reuse the window and show their pane, but the
        // remembered pane stays the one the user picked.
        for (entry, expected) in [
            (show_about as fn(&mut App), Section::About),
            (show_updates, Section::Updates),
            (
                (|cx: &mut App| show_vault(None, cx)) as fn(&mut App),
                Section::Vaults,
            ),
        ] {
            cx.update(entry);
            cx.run_until_parked();
            handle
                .update(cx, |root, _, cx| {
                    let settings = root.view().clone().downcast::<Settings>().unwrap();
                    assert!(settings.read(cx).section == expected);
                })
                .unwrap();
            cx.update(reader_ui_state::flush);
            cx.update(|cx| reader_ui_state::install(&directory, cx));
            assert!(cx.read(Section::remembered) == Section::Logs);
        }
        // A fresh window opened by an explicit entry also leaves it alone.
        handle
            .update(cx, |_, window, _| window.remove_window())
            .unwrap();
        cx.run_until_parked();
        cx.update(|cx| cx.set_global(SettingsWindow(None)));
        cx.update(show_about);
        cx.run_until_parked();
        let handle = cx.read(|cx| cx.global::<SettingsWindow>().0.unwrap());
        handle
            .update(cx, |root, _, cx| {
                let settings = root.view().clone().downcast::<Settings>().unwrap();
                assert!(settings.read(cx).section == Section::About);
                // Navigating afterwards is a real choice and is remembered.
                settings.update(cx, |settings, cx| {
                    settings.select_section(Section::Vaults, cx)
                });
            })
            .unwrap();
        cx.update(reader_ui_state::flush);
        cx.update(|cx| reader_ui_state::install(&directory, cx));
        assert!(cx.read(Section::remembered) == Section::Vaults);
        // A pane unavailable on this platform/newer build never breaks Settings.
        cx.update(|cx| {
            reader_ui_state::set_settings_section("future-pane", cx);
            assert!(Section::remembered(cx) == Section::About);
            reader_ui_state::flush(cx);
        });
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(all(target_os = "linux", feature = "settings-ui-harness"))]
    #[gpui::test]
    fn preview_uses_real_channel_controls_without_changing_the_platform_channel(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::test_init);
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
    fn manager_settings_entry_keeps_active_project_prefill(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("ops")).unwrap();
        std::fs::write(
            directory.path().join("ops/project.yaml"),
            "t3: {environment: test, origin: 'https://project.example'}",
        )
        .unwrap();
        let project = okilum_t3::projects::Project {
            folder: directory.path().into(),
            name: "Project".into(),
            kind: okilum_t3::projects::Kind::Knowledge,
            environment: Some("test".into()),
            manager_project: Some("brain".into()),
            manager: okilum_t3::projects::Manager::NotStarted,
            appearance: Default::default(),
        };
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                let mut reader = Reader::new(Opts::default(), window, cx);
                reader.project_navigation.active = Some(project);
                reader
            });
            window.activate_window();
            Root::new(reader, window, cx)
        });
        // This is exactly the Manager button entry, including automatic active
        // Reader resolution in show_section before Settings becomes active.
        visual.update(|_, cx| show_t3(cx));
        visual.run_until_parked();
        let handle = visual.update(|_, cx| cx.global::<SettingsWindow>().0.unwrap());
        handle
            .update(visual, |root, _, cx| {
                let settings = root.view().clone().downcast::<Settings>().unwrap();
                let settings = settings.read(cx);
                assert!(settings
                    .reader
                    .as_ref()
                    .and_then(WeakEntity::upgrade)
                    .is_some());
                let t3 = settings
                    .t3
                    .as_ref()
                    .expect("Projects / T3 pane initialized");
                assert_eq!(t3.read(cx).origin_value(cx), "https://project.example");
            })
            .unwrap();
    }

    #[gpui::test]
    fn about_routes_reuse_settings_without_a_reader_modal(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
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
            crate::test_init(cx);
            reader_ui_state::install(directory.path(), cx);
        });
        let (_, visual) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| {
                let mut settings = Settings::new(None, cx);
                settings.section = Section::Appearance;
                settings
            });
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
        // The normal-width slider renders (its range used to panic: min > max)
        // and shows the stored width.
        assert!(
            visual.debug_bounds("reading-width").is_some(),
            "normal note width slider visible"
        );
        visual.update(|_, cx| reader_ui_state::set_reading_width(900., cx));
        visual.run_until_parked();
        visual.update(|_, cx| assert_eq!(reader_ui_state::reading_width(cx), 900.));
        visual.update(|_, cx| reader_ui_state::set_reading_width(READER_MAX_WIDTH, cx));
        visual.run_until_parked();
        let original = visual.update(|_, cx| reader_ui_state::font_size(cx));
        for (selector, font, width) in [
            ("reading-larger", original + 1., READER_MAX_WIDTH),
            ("reading-smaller", original, READER_MAX_WIDTH),
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
            crate::test_init(cx);
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
                settings.section = Section::Vaults;
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
            crate::test_init(cx);
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
                settings.section = Section::Vaults;
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
            crate::test_init(cx);
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
                settings.section = Section::Vaults;
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

    /// #1355: the vault's settings and the log options live apart, and a
    /// templates folder that is not in the vault says so.
    #[gpui::test]
    fn issue2572_current_vault_card_and_named_log_sources_are_separate(cx: &mut TestAppContext) {
        let temp = std::env::temp_dir().join(format!("okilum-settings-{}", uuid::Uuid::new_v4()));
        let (root, state) = (temp.join("vault"), temp.join("state"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(root.join("start.md"), "# Start\n").unwrap();
        let other = temp.join("other-vault");
        std::fs::create_dir_all(&other).unwrap();
        reader_history::ReadingHistory::record_document_opened(
            &state,
            &other,
            "note.md",
            false,
            Some(1),
        )
        .unwrap();
        reader_history::ReadingHistory::record_document_opened(
            &state,
            &root,
            "start.md",
            false,
            Some(2),
        )
        .unwrap();
        reader_history::ReadingHistory::set_default_vault(&state, Some(&root)).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
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
        let mut settings = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut settings = Settings::new(Some(weak), cx);
                settings.section = Section::Vaults;
                settings
            });
            settings = Some(view.clone());
            Root::new(view, window, cx)
        });
        visual.simulate_resize(size(px(1200.), px(1500.)));
        let settings = settings.unwrap();
        let shown = |visual: &mut gpui::VisualTestContext, label: &str| {
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
            // `debug_bounds` takes a static selector; leaking a few is fine here.
            let selector: &'static str =
                Box::leak(format!("setting-help-{label}").into_boxed_str());
            visual.debug_bounds(selector).is_some()
        };
        assert!(shown(visual, "Vault colour"), "positive control");
        assert!(visual.debug_bounds("settings-section-Vaults").is_some());
        assert!(visual.debug_bounds("settings-section-Vault").is_none());
        assert!(visual.debug_bounds("settings-current-vault").is_some());
        assert!(visual
            .debug_bounds("settings-vault-current-badge")
            .is_some());
        assert!(visual.debug_bounds("settings-log-sources").is_none());
        assert!(visual
            .debug_bounds("settings-vault-default-badge")
            .is_some());
        assert!(
            visual.debug_bounds("settings-vault-row-0").is_none(),
            "current vault is not duplicated in Other vaults"
        );
        assert!(
            visual.debug_bounds("settings-vault-row-1").is_some(),
            "other vault remains listed"
        );
        assert!(shown(visual, "Templates folder"));
        assert!(!shown(visual, "Open log source links in"));
        assert!(!shown(visual, "Log source paths"));
        settings.read_with(visual, |settings, _| {
            assert!(
                settings.template_missing,
                "_Assets/Templates is not in the vault"
            );
        });
        settings.update(visual, |settings, cx| {
            settings.section = Section::Logs;
            cx.notify();
        });
        assert!(shown(visual, "Open log source links in"));
        assert!(shown(visual, "Log source paths"));
        assert!(!shown(visual, "Vault colour"));
        assert!(visual
            .debug_bounds("settings-log-sources-heading")
            .is_some());
        settings.read_with(visual, |_, cx| {
            assert_eq!(log_sources_heading(Some(&root), cx), "Log sources · vault");
            assert_eq!(log_sources_heading(None, cx), "Log sources");
        });
        let app_setting = visual
            .debug_bounds("setting-help-Open log source links in")
            .unwrap();
        let sources = visual.debug_bounds("settings-log-sources").unwrap();
        assert!(
            app_setting.bottom() <= sources.top(),
            "app-wide settings precede vault sources"
        );
        assert!(visual.debug_bounds("settings-https-log-sources").is_some());
        assert!(visual.debug_bounds("settings-kibana-sources").is_some());
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[gpui::test]
    fn issue2572_legacy_vault_sections_open_unified_vaults(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        cx.update(crate::test_init);
        for key in ["vault", "vaults"] {
            std::fs::write(
                directory.path().join("reader-ui.json"),
                serde_json::to_vec(&serde_json::json!({"version": 1, "settings_section": key}))
                    .unwrap(),
            )
            .unwrap();
            cx.update(|cx| {
                reader_ui_state::install(directory.path(), cx);
                assert!(Section::remembered(cx) == Section::Vaults);
            });
            let (_, visual) = cx.add_window_view(|window, cx| {
                let settings = cx.new(|cx| Settings::new(None, cx));
                assert!(settings.read(cx).section == Section::Vaults);
                Root::new(settings, window, cx)
            });
            visual.run_until_parked();
            assert!(visual.debug_bounds("settings-section-Vaults").is_some());
            assert!(visual.debug_bounds("settings-vaults-list").is_some());
            assert!(visual.debug_bounds("settings-section-Vault").is_none());
        }
    }

    #[gpui::test]
    fn issue2572_no_vault_log_sources_are_disabled(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| {
                let mut settings = Settings::new(None, cx);
                settings.section = Section::Logs;
                settings
            });
            Root::new(settings, window, cx)
        });
        visual.run_until_parked();
        assert!(
            visual
                .debug_bounds("settings-log-sources-heading")
                .is_some(),
            "positive control"
        );
        let windows = visual.read(|cx| cx.windows().len());
        for selector in ["settings-https-log-sources", "settings-kibana-sources"] {
            let bounds = visual.debug_bounds(selector).unwrap();
            visual.simulate_click(bounds.center(), Modifiers::default());
            visual.run_until_parked();
            assert_eq!(visual.read(|cx| cx.windows().len()), windows);
        }
        assert!(visual
            .debug_bounds("setting-help-Log source paths")
            .is_none());
    }

    #[gpui::test]
    fn confirmations_opened_from_settings_are_drawn(cx: &mut TestAppContext) {
        use gpui_component::WindowExt;
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            crate::test_init(cx);
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
            crate::test_init(cx);
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
            crate::test_init(cx);
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
        assert!(visual.debug_bounds("settings-section-Vault").is_none());
        for (section, selector) in [
            (Section::Vaults, "settings-section-Vaults"),
            (Section::Logs, "settings-section-Logs"),
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
        cx.update(crate::test_init);
        let (root, visual) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| {
                let mut settings = Settings::new(None, cx);
                settings.section = Section::Appearance;
                settings
            });
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

    /// Both visible hex views serialize the same authored RGB; alpha keeps
    /// the library's existing truncation convention and original state value.
    #[gpui::test]
    fn colour_picker_hex_views_preserve_rgb_and_existing_alpha(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let (picker, visual) = cx.add_window_view(ColorPickerState::new);
        for (value, expected) in [
            (0x16a34a, "#16A34A"),
            (0x3a7bd5, "#3A7BD5"),
            (0x000000, "#000000"),
            (0xffffff, "#FFFFFF"),
            (0xff0000, "#FF0000"),
            (0x00ff00, "#00FF00"),
            (0x0000ff, "#0000FF"),
        ] {
            for (alpha, suffix) in [
                (1., ""),
                (0., "00"),
                (0.1, "19"),
                (0.5, "7F"),
                (128. / 255., "80"),
            ] {
                visual.update(|window, cx| {
                    let color: Hsla = rgb(value).into();
                    picker.update(cx, |picker, cx| {
                        picker.set_value(Hsla { a: alpha, ..color }, window, cx)
                    });
                });
                visual.run_until_parked();
                picker.read_with(visual, |picker, cx| {
                    let expected = format!("{expected}{suffix}");
                    assert_eq!(picker.hex_input().read(cx).value().as_str(), expected);
                    let actual = picker.value().unwrap();
                    assert_eq!(gpui_component::Colorize::to_hex(&actual), expected);
                    assert_eq!(
                        actual.a, alpha,
                        "formatting must not quantize the stored alpha"
                    );
                });
            }
        }
    }

    /// #1399: a custom colour is typed and applied through the real Settings
    /// field; anything that is not a whole `#RRGGBB` is named in place and
    /// changes nothing. The colour persists canonically, comes back after a
    /// reload, is the one marked swatch, and every window of that vault (and
    /// only that vault) draws it.
    #[gpui::test]
    fn custom_vault_colour_is_typed_applied_and_shown_in_every_window_of_that_vault(
        cx: &mut TestAppContext,
    ) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        let other = fixture.path().join("other");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&other).unwrap();
        let state = fixture.path().join("state");
        let stored_path = state.join("reader-ui.json");
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
            reader_ui_state::install(&state, cx);
        });
        let open = |vault: &Path, cx: &mut TestAppContext| {
            let mut reader = None;
            let window = cx.add_window(|window, cx| {
                let view = cx.new(|cx| Reader::new(Opts::default(), window, cx));
                reader = Some(view.clone());
                Root::new(view, window, cx)
            });
            let mut visual = VisualTestContext::from_window(window.into(), cx);
            let view = reader.unwrap();
            visual.simulate_resize(size(px(1200.), px(700.)));
            view.update(&mut visual, |v, cx| {
                v.loading = None;
                v.vault_root = vault.to_owned();
                v.panels.open(reader_layout::Panel::Notes);
                cx.notify();
            });
            visual.run_until_parked();
            (view, visual)
        };
        let (first_reader, mut first) = open(&root, cx);
        let (_, mut second) = open(&root, cx);
        let (_, mut unrelated) = open(&other, cx);
        fn open_settings<'a>(
            reader: &Entity<Reader>,
            cx: &'a mut TestAppContext,
        ) -> (Entity<Settings>, &'a mut VisualTestContext) {
            let reader = reader.downgrade();
            let (view, visual) = cx.add_window_view(|window, cx| {
                let settings = cx.new(|cx| {
                    let mut settings = Settings::new(Some(reader), cx);
                    settings.section = Section::Vaults;
                    settings
                });
                Root::new(settings, window, cx)
            });
            visual.simulate_resize(size(px(1400.), px(1000.)));
            visual.run_until_parked();
            let settings = view.read_with(visual, |root, _| {
                root.view().clone().downcast::<Settings>().unwrap()
            });
            (settings, visual)
        }
        let (settings, visual) = open_settings(&first_reader, cx);
        let redraw = |visual: &mut VisualTestContext| {
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
        };
        let click = |visual: &mut VisualTestContext, selector: &'static str| {
            redraw(visual);
            let bounds = visual
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} is rendered"));
            visual.simulate_click(bounds.center(), Modifiers::default());
            visual.run_until_parked();
        };
        let select_all = if cfg!(target_os = "macos") {
            "cmd-a"
        } else {
            "ctrl-a"
        };
        // Replace the field's text by typing, as a user would.
        let type_in_field = |visual: &mut VisualTestContext, text: &str| {
            click(visual, "settings-vault-color-input");
            visual.simulate_keystrokes(select_all);
            visual.simulate_keystrokes("backspace");
            if !text.is_empty() {
                visual.simulate_input(text);
            }
            visual.run_until_parked();
        };
        let current = |visual: &mut VisualTestContext| {
            visual.update(|_, cx| reader_ui_state::vault_color(&root, cx))
        };
        let stored = |visual: &mut VisualTestContext| {
            visual.update(|_, cx| reader_ui_state::flush(cx));
            let json: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&stored_path).unwrap()).unwrap();
            json["vault_colors"][root.to_str().unwrap()]
                .as_str()
                .map(str::to_owned)
        };
        let invalid = |visual: &mut VisualTestContext| {
            settings.read_with(visual, |settings, _| {
                settings.vault_color_rejected.is_some()
            })
        };
        let marked = |visual: &mut VisualTestContext| {
            let choices = vault_color_swatch_choices(current(visual));
            assert_eq!(
                choices.iter().filter(|(_, selected)| *selected).count(),
                1,
                "exactly one swatch is marked: {choices:?}"
            );
            assert_eq!(
                &choices[..9]
                    .iter()
                    .map(|(color, _)| *color)
                    .collect::<Vec<_>>()[..],
                &std::iter::once(None)
                    .chain(brand::VaultColor::ALL.map(Some))
                    .collect::<Vec<_>>()[..],
                "None and every preset stay offered"
            );
            choices
                .into_iter()
                .find(|(_, selected)| *selected)
                .unwrap()
                .0
        };

        // Nothing set: «None» is marked, there is no custom swatch.
        redraw(visual);
        assert_eq!(marked(visual), None);
        assert!(visual.debug_bounds("settings-vault-color-none").is_some());
        assert!(visual.debug_bounds("settings-vault-color-custom").is_none());
        // A preset first, so rejected text has a previous value to keep.
        click(visual, "settings-vault-color-teal");
        assert_eq!(current(visual), Some(brand::VaultColor::Teal));
        assert_eq!(stored(visual).as_deref(), Some("teal"));
        redraw(visual);
        let help_before = visual
            .debug_bounds("settings-vault-color-custom-help")
            .unwrap();
        let apply_before = visual.debug_bounds("settings-vault-color-apply").unwrap();

        // Empty, partial and malformed text: named in place, nothing applied.
        for (text, by_enter) in [
            ("", false),
            ("#3a7", true),
            ("#3a7bd", false),
            ("3a7bd5", false),
            ("#3a7bdg", true),
            ("#3a7bd5f", false),
        ] {
            type_in_field(visual, text);
            assert!(!invalid(visual), "{text:?}: typing alone does not complain");
            if by_enter {
                visual.simulate_keystrokes("enter");
                visual.run_until_parked();
            } else {
                click(visual, "settings-vault-color-apply");
            }
            assert!(invalid(visual), "{text:?} is named as not a colour");
            assert_eq!(current(visual), Some(brand::VaultColor::Teal), "{text:?}");
            assert_eq!(stored(visual).as_deref(), Some("teal"), "{text:?}");
            redraw(visual);
            assert_eq!(
                visual
                    .debug_bounds("settings-vault-color-custom-help")
                    .unwrap()
                    .size
                    .height,
                help_before.size.height,
                "the complaint replaces the help line without moving the row"
            );
            assert_eq!(
                visual.debug_bounds("settings-vault-color-apply").unwrap(),
                apply_before
            );
        }
        settings.read_with(visual, |settings, cx| {
            let (input, _) = settings.vault_color_input.as_ref().unwrap();
            assert_eq!(
                input.read(cx).value().as_str(),
                "#3a7bd5f",
                "rejected text stays in the field to be corrected"
            );
        });
        // Editing clears the complaint; Apply then takes a valid colour in
        // any case and stores it canonically. Preset -> custom.
        type_in_field(visual, "#3A7bD5");
        assert!(!invalid(visual), "editing clears the complaint");
        assert_eq!(current(visual), Some(brand::VaultColor::Teal), "not yet");
        click(visual, "settings-vault-color-apply");
        let typed = brand::VaultColor::Custom([0x3a, 0x7b, 0xd5]);
        assert!(!invalid(visual));
        assert_eq!(current(visual), Some(typed));
        assert_eq!(stored(visual).as_deref(), Some("#3a7bd5"));
        assert_eq!(marked(visual), Some(typed), "the custom swatch is marked");
        redraw(visual);
        assert!(visual.debug_bounds("settings-vault-color-custom").is_some());
        assert!(
            visual.debug_bounds("settings-title-tint").is_some(),
            "a custom colour offers the title tint like a preset"
        );
        // Every window of this vault draws it; the other vault stays plain.
        for window in [&mut first, &mut second] {
            redraw(window);
            assert!(window.debug_bounds("vault-accent-line").is_some());
            assert!(window.debug_bounds("sidebar-vault-dot").is_some());
        }
        redraw(&mut unrelated);
        assert!(
            unrelated.debug_bounds("vault-accent-line").is_none(),
            "positive control: only this vault's windows change"
        );
        // The swatch shows the colour actually drawn in the live theme.
        visual.update(|_, cx| {
            let shown = typed.color(cx);
            let dark = cx.theme().is_dark();
            assert_eq!(shown, typed.display(brand::theme_id(cx), dark));
        });

        // Reload: a new Store from the same directory and a fresh Settings
        // window read the same colour back and prefill the field with it.
        visual.update(|_, cx| reader_ui_state::flush(cx));
        cx.update(|cx| reader_ui_state::install(&state, cx));
        let (settings, visual) = open_settings(&first_reader, cx);
        redraw(visual);
        assert_eq!(
            visual.update(|_, cx| reader_ui_state::vault_color(&root, cx)),
            Some(typed)
        );
        assert!(visual.debug_bounds("settings-vault-color-custom").is_some());
        settings.read_with(visual, |settings, cx| {
            let (input, _) = settings.vault_color_input.as_ref().unwrap();
            assert_eq!(input.read(cx).value().as_str(), "#3A7BD5");
            let picker = settings.vault_color_picker.as_ref().unwrap().0.read(cx);
            assert_eq!(picker.hex_input().read(cx).value().as_str(), "#3A7BD5");
            assert_eq!(
                gpui_component::Colorize::to_hex(&picker.value().unwrap()),
                "#3A7BD5"
            );
            assert!(settings.vault_color_rejected.is_none());
        });

        // #2202: real library picker changes are drafts until Apply. Cancel
        // restores both controls without writing the selected draft to disk.
        click(visual, "settings-vault-color-picker");
        let picker = settings.read_with(visual, |settings, _| {
            settings.vault_color_picker.as_ref().unwrap().0.clone()
        });
        assert!(picker.read_with(visual, |picker, _| picker.is_open()));
        visual.update(|window, cx| {
            picker.update(cx, |picker, cx| {
                picker.select_color(rgb(0x123456).into(), window, cx)
            });
        });
        visual.run_until_parked();
        settings.read_with(visual, |settings, cx| {
            assert_eq!(
                settings
                    .vault_color_input
                    .as_ref()
                    .unwrap()
                    .0
                    .read(cx)
                    .value(),
                "#123456"
            );
        });
        assert_eq!(
            stored(visual).as_deref(),
            Some("#3a7bd5"),
            "picker alone cannot persist"
        );
        click(visual, "settings-vault-color-cancel");
        settings.read_with(visual, |settings, cx| {
            assert_eq!(
                settings
                    .vault_color_input
                    .as_ref()
                    .unwrap()
                    .0
                    .read(cx)
                    .value(),
                "#3A7BD5"
            );
        });
        assert_eq!(current(visual), Some(typed));
        visual.update(|window, cx| {
            picker.update(cx, |picker, cx| {
                picker.select_color(rgb(0x123456).into(), window, cx)
            });
        });
        visual.run_until_parked();
        click(visual, "settings-vault-color-apply");
        assert_eq!(stored(visual).as_deref(), Some("#123456"));
        type_in_field(visual, "#16A34A");
        settings.read_with(visual, |settings, cx| {
            let picker = settings.vault_color_picker.as_ref().unwrap().0.read(cx);
            assert_eq!(picker.hex_input().read(cx).value().as_str(), "#16A34A");
            assert_eq!(
                gpui_component::Colorize::to_hex(&picker.value().unwrap()),
                "#16A34A"
            );
        });
        click(visual, "settings-vault-color-cancel");
        // A fresh Settings entity reopens with the authored RGB, not its
        // contrast-adapted display colour (the renderer keeps that adaptation).
        let (settings, visual) = open_settings(&first_reader, cx);
        settings.read_with(visual, |settings, cx| {
            let value = settings
                .vault_color_picker
                .as_ref()
                .unwrap()
                .0
                .read(cx)
                .value()
                .unwrap();
            let rgb = value.to_rgb();
            assert_eq!(
                [
                    (rgb.r * 255.).round() as u8,
                    (rgb.g * 255.).round() as u8,
                    (rgb.b * 255.).round() as u8
                ],
                [0x12, 0x34, 0x56]
            );
        });
        type_in_field(visual, "#ABCDEF");
        click(visual, "settings-vault-color-pink");
        click(visual, "settings-vault-color-cancel");
        assert_eq!(
            current(visual),
            Some(brand::VaultColor::Pink),
            "cancel keeps latest saved preset"
        );
        assert_eq!(stored(visual).as_deref(), Some("pink"));

        // Custom -> preset -> custom (by Enter) -> None.
        click(visual, "settings-vault-color-pink");
        let now = visual.update(|_, cx| reader_ui_state::vault_color(&root, cx));
        assert_eq!(now, Some(brand::VaultColor::Pink));
        redraw(visual);
        assert!(
            visual.debug_bounds("settings-vault-color-custom").is_none(),
            "a preset leaves no custom swatch"
        );
        click(visual, "settings-vault-color-input");
        visual.simulate_keystrokes(select_all);
        visual.simulate_input("#FFFFFF");
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        let now = visual.update(|_, cx| reader_ui_state::vault_color(&root, cx));
        assert_eq!(now, Some(brand::VaultColor::Custom([255, 255, 255])));
        click(visual, "settings-vault-color-none");
        let now = visual.update(|_, cx| reader_ui_state::vault_color(&root, cx));
        assert_eq!(now, None);
        visual.update(|_, cx| reader_ui_state::flush(cx));
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&stored_path).unwrap()).unwrap();
        assert!(json["vault_colors"].get(root.to_str().unwrap()).is_none());
        for window in [&mut first, &mut second] {
            redraw(window);
            assert!(window.debug_bounds("vault-accent-line").is_none());
        }
    }

    #[gpui::test]
    fn hidden_files_setting_uses_the_reader_toggle(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
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
            settings.section = Section::Vaults;
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
            crate::test_init(cx);
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
            settings.section = Section::Vaults;
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
    fn issue2755_settings_caption_keeps_native_macos_drag_and_platform_ownership(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            crate::test_init(cx);
            let options = window_options(None, cx);
            assert_eq!(options.kind, gpui::WindowKind::Floating);
            assert!(!options.is_minimizable);
            let caption = options.titlebar.unwrap();
            if cfg!(target_os = "macos") {
                assert!(!options.app_owns_titlebar_drag, "AppKit must own dragging");
                assert!(!caption.appears_transparent, "no client caption overlay");
                assert!(caption.traffic_light_position.is_none(), "native controls");
                assert_eq!(caption.title.as_deref(), Some("Okilum Settings"));
            } else {
                let original = TitleBar::window_options();
                assert_eq!(
                    options.app_owns_titlebar_drag,
                    original.app_owns_titlebar_drag
                );
                assert_eq!(
                    caption.appears_transparent,
                    original.titlebar.unwrap().appears_transparent
                );
            }
        });
    }

    #[gpui::test]
    fn window_chrome_settings_about_closes_with_escape_shortcut_and_action(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            crate::test_init(cx);
            install(cx);
        });
        let owner = cx.add_window(|_, cx| Settings::new(None, cx));
        for path in 0..4 {
            cx.update(show_about);
            cx.run_until_parked();
            let handle = cx.read(|cx| cx.global::<SettingsWindow>().0.unwrap());
            assert!(cx.read(|cx| cx.windows().contains(&handle.into())));
            let mut visual = VisualTestContext::from_window(handle.into(), cx);
            visual.run_until_parked();
            assert_eq!(
                visual.debug_bounds("window-title-bar").is_some(),
                !cfg!(target_os = "macos"),
                "macOS uses AppKit's caption; Linux/Windows keep the client bar"
            );
            if cfg!(target_os = "macos") {
                let actions = visual.debug_bounds("settings-window-actions").unwrap();
                let close = visual.debug_bounds("settings-close").unwrap();
                assert!(close.top() >= actions.top());
                assert!(close.bottom() <= gpui_component::TITLE_BAR_HEIGHT);
            }
            match path {
                0 => visual.simulate_keystrokes("escape"),
                1 => visual.simulate_keystrokes("secondary-w"),
                2 => visual.dispatch_action(CloseSettings),
                _ => {
                    let close = visual.debug_bounds("settings-close").unwrap();
                    visual.simulate_click(close.center(), Modifiers::default());
                }
            }
            visual.run_until_parked();
            assert!(
                !cx.read(|cx| cx.windows().contains(&handle.into())),
                "close path {path} must remove the Settings window"
            );
            assert!(
                cx.read(|cx| cx.windows().contains(&owner.into())),
                "closing Settings must leave its owner open"
            );
        }
    }

    #[gpui::test]
    fn window_chrome_reader_prefers_compositor_decorations(cx: &mut TestAppContext) {
        // None is GPUI's SSD preference. Its Wayland backend falls back to CSD
        // when xdg-decoration is absent and honors the compositor's configure.
        cx.update(|cx| {
            assert!(reader_open::window_options(cx).window_decorations.is_none());
            assert!(TitleBar::window_options().window_decorations.is_none());
        });
    }

    #[gpui::test]
    fn application_menu_finds_active_reader_and_reuses_settings_window(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
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

    #[gpui::test]
    fn about_keeps_each_reader_owner_and_reuses_only_its_own_window(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let (_, first) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| Reader::new(Opts::default(), window, cx));
            Root::new(reader, window, cx)
        });
        let mut first = first.clone();
        let (_, second) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| Reader::new(Opts::default(), window, cx));
            Root::new(reader, window, cx)
        });
        let mut second = second.clone();
        first.update(about::show_about);
        first.run_until_parked();
        let first_about = cx.read(|cx| cx.global::<SettingsWindow>().0.unwrap());
        second.update(about::show_about);
        second.run_until_parked();
        let second_about = cx.read(|cx| cx.global::<SettingsWindow>().0.unwrap());
        assert_ne!(first_about, second_about, "each Reader needs its own About");
        for (mut reader, about) in [(first.clone(), first_about), (second.clone(), second_about)] {
            reader.update(|window, cx| {
                assert_eq!(
                    reader_owned_windows::owner(about.into(), cx),
                    window.window_handle()
                );
            });
            let mut tool = VisualTestContext::from_window(about.into(), cx);
            tool.update(about::show_about);
            tool.run_until_parked();
            cx.read(|cx| {
                assert_eq!(cx.global::<SettingsWindow>().0.unwrap(), about);
                assert_eq!(cx.windows().len(), 4, "reopen must reuse the owner's tool");
            });
            about
                .update(cx, |root, _, cx| {
                    let settings = root.view().clone().downcast::<Settings>().unwrap();
                    assert!(settings.read(cx).section == Section::About);
                })
                .unwrap();
        }
        first.update(|window, _| window.remove_window());
        cx.run_until_parked();
        cx.read(|cx| {
            assert!(!cx.windows().contains(&first_about.into()));
            assert!(cx.windows().contains(&second_about.into()));
        });
    }

    #[gpui::test]
    fn owned_settings_center_reuse_escape_and_close_with_owner(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            install(cx);
        });
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| Reader::new(Opts::default(), window, cx));
            Root::new(reader, window, cx)
        });
        // Own the visual context so the parent context can drive other windows.
        let mut visual = visual.clone();
        visual.simulate_resize(size(px(1200.), px(900.)));
        let main = visual.update(|window, cx| {
            window.activate_window();
            show(None, cx);
            window.window_handle()
        });
        visual.run_until_parked();
        let handle = visual.update(|owner, cx| {
            let handle = cx.global::<SettingsWindow>().0.unwrap();
            assert_eq!(
                reader_owned_windows::owner(handle.into(), cx),
                owner.window_handle()
            );
            let center = owner.bounds().center();
            handle
                .update(cx, |_, window, _| {
                    assert_eq!(
                        window.bounds().center(),
                        center,
                        "center over the actual owner"
                    );
                    assert!(!window.is_minimizable());
                })
                .unwrap();
            handle
        });
        let mut settings = VisualTestContext::from_window(handle.into(), cx);
        settings.update(|window, cx| {
            window.activate_window();
            show(None, cx);
        });
        settings.run_until_parked();
        settings.update(|_, cx| {
            assert_eq!(cx.global::<SettingsWindow>().0.unwrap(), handle);
            assert_eq!(cx.windows().len(), 2, "reopen must focus the same window");
        });
        settings.simulate_keystrokes("escape");
        settings.run_until_parked();
        assert!(
            cx.read(|cx| cx.windows().contains(&main)),
            "Escape leaves owner usable"
        );
        visual.update(|window, cx| {
            window.activate_window();
            show(None, cx);
        });
        visual.run_until_parked();
        let reopened = visual.update(|_, cx| cx.global::<SettingsWindow>().0.unwrap());
        let unrelated = cx.add_window(|_, cx| Settings::new(None, cx));
        visual.update(|window, _| window.remove_window());
        cx.run_until_parked();
        cx.read(|cx| {
            assert!(
                !cx.windows().contains(&reopened.into()),
                "owner closure closes Settings"
            );
            assert!(
                cx.windows().contains(&unrelated.into()),
                "unrelated windows stay open"
            );
        });
    }
}

// Native QA uses the exact Settings methods/actions, without preview status or
// channel fixtures. These adapters are absent from normal product builds.
#[cfg(all(target_os = "linux", feature = "appimage-acceptance-harness"))]
pub(crate) fn acceptance_show(cx: &mut App) {
    show_section(None, Some(Section::Updates), cx);
}
#[cfg(all(target_os = "linux", feature = "appimage-acceptance-harness"))]
pub(crate) fn acceptance_channel(beta: bool, cx: &mut App) -> anyhow::Result<()> {
    let handle = cx
        .try_global::<SettingsWindow>()
        .and_then(|s| s.0)
        .ok_or_else(|| anyhow::anyhow!("Open native Settings first"))?;
    handle.update(cx, |root, _, cx| {
        let settings = root
            .view()
            .clone()
            .downcast::<Settings>()
            .map_err(|_| anyhow::anyhow!("Not native Settings"))?;
        settings.update(cx, |settings, cx| settings.select_update_channel(beta, cx));
        Ok(())
    })?
}
#[cfg(all(target_os = "linux", feature = "appimage-acceptance-harness"))]
pub(crate) fn acceptance_activate(cx: &mut App) -> anyhow::Result<()> {
    anyhow::ensure!(
        cx.try_global::<SettingsWindow>()
            .and_then(|s| s.0)
            .is_some(),
        "Open native Settings first"
    );
    updater::activate(cx);
    Ok(())
}
#[cfg(all(target_os = "linux", feature = "appimage-acceptance-harness"))]
pub(crate) fn acceptance_section(cx: &mut App) -> Option<String> {
    let handle = cx.try_global::<SettingsWindow>()?.0?;
    handle
        .update(cx, |root, _, cx| {
            let settings = root.view().clone().downcast::<Settings>().ok()?;
            Some(settings.read(cx).section.label().to_string())
        })
        .ok()
        .flatten()
}
