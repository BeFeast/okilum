//! Approved B1.2.0 identity with GUI V2 neutral interface aliases.
//!
//! Assets are embedded: no fontconfig/OS font install or network lookup is needed.
//! Call `load_fonts` once before creating windows and `apply_theme` after every
//! system-appearance sync. Both Reader and AI Brain then share the same typography.
use std::{borrow::Cow, sync::OnceLock};

use gpui::{px, rgb, svg, App, AssetSource, ElementId, Hsla, SharedString, Styled, Svg};
use gpui_component::{
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    Colorize as _, Theme,
};

pub const SANS_FONT: &str = "Noto Sans";
pub const MONO_FONT: &str = "Cascadia Code";
pub const CHROME_FONT_SIZE: f32 = 14.;
pub const READING_FONT_SIZE: f32 = 15.5;
/// Reader chrome (header, panels, tree) per docs/design/reader.md.
pub const READER_CHROME_FONT_SIZE: f32 = 13.;
pub const MONO_FONT_SIZE: f32 = 13.;
pub const CONTROL_HEIGHT: f32 = 36.;
pub const PRIMARY_HEIGHT: f32 = 38.;
pub const CONTROL_RADIUS: f32 = 8.;

const SYMBOL: &str = "brand/symbol-primary.svg";
const REVERSED_SYMBOL: &str = "brand/symbol-reversed.svg";
const ICON_LIGHT: &str = "brand/app-icon-light.svg";
const ICON_DARK: &str = "brand/app-icon-dark.svg";
// Lucide outline glyphs the toolkit set lacks (docs/design/reader.md §Icons).
const READER_LIST_ICON: &str = "icons/list.svg";
const READER_LINK_ICON: &str = "icons/link.svg";
pub const READER_OPEN_ICON: &str = "icons/arrow-up-right.svg";
pub const READER_PIN_ICON: &str = "icons/pin.svg";
pub const READER_CLOCK_ICON: &str = "icons/clock.svg";
pub const READER_COLLAPSE_ICON: &str = "icons/chevrons-down-up.svg";
pub const READER_FOCUS_ICON: &str = "icons/locate-fixed.svg";
const IMAGES: [(&str, &[u8]); 25] = [
    (
        "icons/square-pen.svg",
        include_bytes!("../assets/icons/square-pen.svg"),
    ),
    (
        "icons/text-search.svg",
        include_bytes!("../assets/icons/text-search.svg"),
    ),
    (
        "icons/pencil.svg",
        include_bytes!("../assets/icons/pencil.svg"),
    ),
    (
        "icons/monitor.svg",
        include_bytes!("../assets/icons/monitor.svg"),
    ),
    (
        "icons/arrow-down-circle.svg",
        include_bytes!("../assets/icons/arrow-down-circle.svg"),
    ),
    (
        "icons/trash.svg",
        include_bytes!("../assets/icons/trash.svg"),
    ),
    ("icons/save.svg", include_bytes!("../assets/icons/save.svg")),
    (
        "icons/channel-stable.svg",
        include_bytes!("../assets/icons/channel-stable.svg"),
    ),
    (
        "icons/channel-beta.svg",
        include_bytes!("../assets/icons/channel-beta.svg"),
    ),
    (
        READER_OPEN_ICON,
        include_bytes!("../assets/icons/arrow-up-right.svg"),
    ),
    (
        READER_COLLAPSE_ICON,
        include_bytes!("../assets/icons/chevrons-down-up.svg"),
    ),
    (
        READER_FOCUS_ICON,
        include_bytes!("../assets/icons/locate-fixed.svg"),
    ),
    (
        "icons/sliders.svg",
        include_bytes!("../assets/icons/sliders.svg"),
    ),
    (
        "icons/file-plus.svg",
        include_bytes!("../assets/icons/file-plus.svg"),
    ),
    (
        "icons/folder-plus.svg",
        include_bytes!("../assets/icons/folder-plus.svg"),
    ),
    (READER_PIN_ICON, include_bytes!("../assets/icons/pin.svg")),
    (
        READER_CLOCK_ICON,
        include_bytes!("../assets/icons/clock.svg"),
    ),
    (
        "icons/project-active.svg",
        include_bytes!("../assets/icons/project-active.svg"),
    ),
    (
        "icons/project-planned.svg",
        include_bytes!("../assets/icons/project-planned.svg"),
    ),
    (READER_LIST_ICON, include_bytes!("../assets/icons/list.svg")),
    (READER_LINK_ICON, include_bytes!("../assets/icons/link.svg")),
    (SYMBOL, include_bytes!("../assets/brand/symbol-primary.svg")),
    (
        REVERSED_SYMBOL,
        include_bytes!("../assets/brand/symbol-reversed.svg"),
    ),
    (
        ICON_LIGHT,
        include_bytes!("../assets/brand/app-icon-light.svg"),
    ),
    (
        ICON_DARK,
        include_bytes!("../assets/brand/app-icon-dark.svg"),
    ),
];

/// Keep the toolkit's functional icons while adding the approved Tessera mark.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = IMAGES.iter().find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        gpui_kit_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        let mut paths = gpui_kit_assets::Assets.list(path)?;
        paths.extend(
            IMAGES
                .iter()
                .filter(|(name, _)| name.starts_with(path))
                .map(|(name, _)| (*name).into()),
        );
        Ok(paths)
    }
}

/// Register complete Latin+Cyrillic faces at native weights, without subset
/// family collisions. Errors propagate to startup rather than silently changing
/// the approved typography. Other scripts still use platform fallback.
pub fn load_fonts(cx: &App) -> anyhow::Result<()> {
    cx.text_system().add_fonts(vec![
        Cow::Borrowed(include_bytes!("../assets/brand/fonts/noto-sans-400.ttf")),
        Cow::Borrowed(include_bytes!("../assets/brand/fonts/noto-sans-500.ttf")),
        Cow::Borrowed(include_bytes!("../assets/brand/fonts/noto-sans-600.ttf")),
        Cow::Borrowed(include_bytes!("../assets/brand/fonts/noto-sans-700.ttf")),
        Cow::Borrowed(include_bytes!(
            "../assets/brand/fonts/cascadia-code-400.ttf"
        )),
        Cow::Borrowed(include_bytes!(
            "../assets/brand/fonts/cascadia-code-500.ttf"
        )),
        Cow::Borrowed(include_bytes!(
            "../assets/brand/fonts/cascadia-code-600.ttf"
        )),
        Cow::Borrowed(include_bytes!(
            "../assets/brand/fonts/cascadia-code-700.ttf"
        )),
    ])
}

/// Interface-owned surface/control aliases plus brand-owned status colors.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub canvas: Hsla,
    pub surface: Hsla,
    pub surface_raised: Hsla,
    pub sidebar: Hsla,
    pub text: Hsla,
    pub text_muted: Hsla,
    pub border: Hsla,
    pub border_subtle: Hsla,
    pub selected: Hsla,
    pub accent: Hsla,
    pub on_accent: Hsla,
    pub focus: Hsla,
    pub link: Hsla,
    pub tile: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub danger: Hsla,
    pub info: Hsla,
}

fn palettes() -> &'static [Palette; 2] {
    static PALETTES: OnceLock<[Palette; 2]> = OnceLock::new();
    PALETTES.get_or_init(|| {
        let interface: serde_json::Value =
            serde_json::from_str(include_str!("../assets/brand/interface-tokens.json"))
                .expect("validated interface tokens");
        let brand: serde_json::Value =
            serde_json::from_str(include_str!("../assets/brand/brand-tokens.json"))
                .expect("validated brand tokens");
        ["light", "dark"].map(|mode| {
            let color = |key: &str| -> Hsla {
                let value = interface["themes"][mode]
                    .get(key)
                    .unwrap_or(&brand["themes"][mode][key]);
                let hex = value.as_str().expect("validated color string");
                rgb(u32::from_str_radix(hex.trim_start_matches('#'), 16)
                    .expect("validated RGB token"))
                .into()
            };
            Palette {
                canvas: color("canvas"),
                surface: color("surface"),
                surface_raised: color("surfaceRaised"),
                sidebar: color("sidebar"),
                text: color("text"),
                text_muted: color("textMuted"),
                border: color("border"),
                border_subtle: color("borderSubtle"),
                selected: color("selected"),
                accent: color("accent"),
                on_accent: color("onAccent"),
                focus: color("focus"),
                link: color("link"),
                tile: color("tile"),
                success: color("success"),
                warning: color("warning"),
                danger: color("danger"),
                info: color("info"),
            }
        })
    })
}

/// Reader-only roles from `reader-tokens.json` (docs/design/reader.md). Reader
/// components read colors from here or from `Palette`, never from literals.
#[derive(Clone, Copy, Debug)]
pub struct ReaderPalette {
    pub text_faint: Hsla,
    pub hover: Hsla,
    pub missing_link: Hsla,
    pub code_bg: Hsla,
    pub code_border: Hsla,
    pub callout_question: Hsla,
}

fn reader_palettes() -> &'static [ReaderPalette; 2] {
    static PALETTES: OnceLock<[ReaderPalette; 2]> = OnceLock::new();
    PALETTES.get_or_init(|| {
        let tokens: serde_json::Value =
            serde_json::from_str(include_str!("../assets/reader/reader-tokens.json"))
                .expect("validated reader tokens");
        ["light", "dark"].map(|mode| {
            let color = |key: &str| -> Hsla {
                let hex = tokens["themes"][mode][key]
                    .as_str()
                    .expect("validated color string");
                rgb(u32::from_str_radix(hex.trim_start_matches('#'), 16)
                    .expect("validated RGB token"))
                .into()
            };
            ReaderPalette {
                text_faint: color("textFaint"),
                hover: color("hover"),
                missing_link: color("missingLink"),
                code_bg: color("codeBg"),
                code_border: color("codeBorder"),
                callout_question: color("calloutQuestion"),
            }
        })
    })
}

pub fn reader_palette_for_theme(theme: &Theme) -> ReaderPalette {
    reader_palettes()[usize::from(theme.is_dark())]
}

pub fn reader_palette(cx: &App) -> ReaderPalette {
    reader_palettes()[usize::from(Theme::global(cx).is_dark())]
}

/// For render callbacks that have no `App` (link presentation closures). Kept in
/// step with the live theme by `apply_theme`.
static APPLIED_DARK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn reader_palette_current() -> ReaderPalette {
    reader_palettes()[usize::from(APPLIED_DARK.load(std::sync::atomic::Ordering::Relaxed))]
}

pub fn palette(cx: &App) -> Palette {
    palettes()[usize::from(Theme::global(cx).is_dark())]
}

/// Reapply after `Theme::sync_system_appearance` or `Theme::change`, which resets
/// component colors. Preserve the toolkit's matching syntax highlight theme.
pub fn apply_theme(cx: &mut App) {
    let p = palette(cx);
    APPLIED_DARK.store(
        Theme::global(cx).is_dark(),
        std::sync::atomic::Ordering::Relaxed,
    );
    let t = Theme::global_mut(cx);
    t.font_family = SANS_FONT.into();
    t.mono_font_family = MONO_FONT.into();
    t.font_size = px(CHROME_FONT_SIZE);
    t.mono_font_size = px(MONO_FONT_SIZE);
    t.radius = px(CONTROL_RADIUS);
    t.radius_lg = px(CONTROL_RADIUS);
    t.background = p.canvas;
    t.foreground = p.text;
    t.accent = p.selected;
    t.accent_foreground = p.text;
    t.border = p.border;
    // The toolkit names its input border token `input`. Keep idle fields
    // visible on the neutral surface; input_background derives its fill.
    t.input = p.border;
    t.caret = p.text;
    // Keep both the OFF track and its thumb visible on the reading surface.
    t.switch = p.text_muted;
    t.switch_thumb = p.canvas;
    t.muted = p.surface_raised;
    t.muted_foreground = p.text_muted;
    t.popover = p.surface;
    t.popover_foreground = p.text;
    t.primary = p.accent;
    t.primary_foreground = p.on_accent;
    t.primary_hover = p.accent.darken(0.04);
    t.primary_active = p.accent.darken(0.08);
    t.secondary = p.surface_raised;
    t.secondary_foreground = p.text;
    t.secondary_hover = p.selected;
    t.secondary_active = p.selected;
    t.button = p.surface_raised;
    t.button_foreground = p.text;
    t.button_hover = p.selected;
    t.button_active = p.selected;
    t.button_primary = t.primary;
    t.button_primary_foreground = t.primary_foreground;
    t.button_primary_hover = t.primary_hover;
    t.button_primary_active = t.primary_active;
    t.button_secondary = t.secondary;
    t.button_secondary_foreground = t.secondary_foreground;
    t.button_secondary_hover = t.secondary_hover;
    t.button_secondary_active = t.secondary_active;
    t.ring = p.focus;
    t.link = p.link;
    t.link_hover = p.link;
    t.link_active = p.link;
    t.selection = p.accent.opacity(0.22);
    t.sidebar = p.sidebar;
    t.sidebar_foreground = p.text;
    t.sidebar_border = p.border_subtle;
    t.sidebar_accent = p.selected;
    t.sidebar_accent_foreground = p.text;
    t.sidebar_primary = p.accent;
    t.sidebar_primary_foreground = p.on_accent;
    t.colors.list = p.surface;
    t.list_hover = p.surface_raised;
    t.list_active = p.selected;
    t.list_active_border = p.focus;
    t.list_even = p.surface;
    t.list_head = p.surface_raised;
    t.title_bar = p.surface;
    t.title_bar_border = p.border_subtle;
    t.status_bar = p.surface;
    t.status_bar_border = p.border_subtle;
    t.window_border = p.border;
    t.tab = p.surface;
    t.tab_bar = p.surface_raised;
    t.tab_bar_segmented = p.surface_raised;
    t.tab_foreground = p.text_muted;
    t.tab_active = p.surface;
    t.tab_active_foreground = p.text;
    t.table = p.surface;
    t.table_even = p.surface_raised;
    t.table_hover = p.selected;
    t.table_active = p.selected;
    t.table_active_border = p.focus;
    t.table_head = p.surface_raised;
    t.table_head_foreground = p.text;
    t.table_foot = p.surface_raised;
    t.table_foot_foreground = p.text;
    t.table_row_border = p.border_subtle;
    t.scrollbar_thumb = p.border;
    t.scrollbar_thumb_hover = p.text_muted;
    t.group_box = p.surface;
    t.group_box_foreground = p.text;
    t.accordion = p.surface;
    t.tiles = p.tile;
    t.drag_border = p.focus;
    t.drop_target = p.selected;
    t.progress_bar = p.accent;
    t.success = p.success;
    t.warning = p.warning;
    t.danger = p.danger;
    t.info = p.info;
    let on_status = if t.is_dark() {
        p.canvas
    } else {
        rgb(0xffffff).into()
    };
    t.danger_foreground = on_status;
    t.danger_hover = p.danger.darken(0.04);
    t.danger_active = p.danger.darken(0.08);
    t.button_danger = p.danger;
    t.button_danger_foreground = on_status;
    t.button_danger_hover = t.danger_hover;
    t.button_danger_active = t.danger_active;
    t.success_foreground = on_status;
    t.success_hover = p.success.darken(0.04);
    t.success_active = p.success.darken(0.08);
    t.button_success = p.success;
    t.button_success_foreground = on_status;
    t.button_success_hover = t.success_hover;
    t.button_success_active = t.success_active;
    t.warning_foreground = on_status;
    t.warning_hover = p.warning.darken(0.04);
    t.warning_active = p.warning.darken(0.08);
    t.button_warning = p.warning;
    t.button_warning_foreground = on_status;
    t.button_warning_hover = t.warning_hover;
    t.button_warning_active = t.warning_active;
    t.info_foreground = on_status;
    t.info_hover = p.info.darken(0.04);
    t.info_active = p.info.darken(0.08);
    t.button_info = p.info;
    t.button_info_foreground = on_status;
    t.button_info_hover = t.info_hover;
    t.button_info_active = t.info_active;

    // Styled controls read ThemeTokens, not only ThemeColor. Rebuild both so
    // system appearance flips cannot retain a previous palette's backgrounds.
    t.tokens = (&t.colors).into();
    Theme::sync_base(cx);
}

/// Standard secondary control for existing call sites. `.primary()`/`.ghost()`
/// can still select toolkit variants; geometry and semantic interaction survive.
pub fn control(id: impl Into<ElementId>, _cx: &App) -> Button {
    Button::new(id)
        .secondary()
        .rounded(px(CONTROL_RADIUS))
        .h(px(CONTROL_HEIGHT))
        .px(px(12.))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    Primary,
    Secondary,
    Quiet,
    Danger,
}

/// Native Button retains toolkit focus/disabled/loading/click behavior. The
/// instance sets geometry only, so semantic state colors remain effective.
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    cx: &App,
) -> Button {
    let p = palette(cx);
    let button = control(id, cx).label(label);
    let button = match kind {
        ButtonKind::Primary => button.primary(),
        ButtonKind::Secondary => button.secondary(),
        ButtonKind::Quiet => button.ghost(),
        // Separate destructive text action; never a competing filled primary.
        ButtonKind::Danger => button.custom(
            ButtonCustomVariant::new(cx)
                .foreground(p.danger)
                .hover(p.danger.opacity(0.10))
                .active(p.danger.opacity(0.18))
                .shadow(false),
        ),
    };
    button.h(px(if kind == ButtonKind::Primary {
        PRIMARY_HEIGHT
    } else {
        CONTROL_HEIGHT
    }))
}

/// Canonical B geometry, rendered as the approved single-color symbol. SVG masks
/// intentionally do not pretend to support a multicolor app-icon background.
pub fn logo(size_px: f32, cx: &App) -> Svg {
    let dark = Theme::global(cx).is_dark();
    svg()
        .path(if dark { REVERSED_SYMBOL } else { SYMBOL })
        .size(px(size_px))
        .text_color(if dark { rgb(0xe9f0fc) } else { rgb(0x2563eb) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui_component::IconNamed;
    use gpui_component::ThemeMode;

    #[test]
    fn every_local_functional_icon_is_embedded() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icons");
        let mut checked = 0;
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            if entry.path().extension().is_none_or(|ext| ext != "svg") {
                continue;
            }
            let path = format!("icons/{}", entry.file_name().to_string_lossy());
            let bytes = Assets
                .load(&path)
                .unwrap()
                .unwrap_or_else(|| panic!("Missing {path}"));
            assert!(matches!(bytes, Cow::Borrowed(_)), "{path} must be embedded");
            assert_eq!(
                bytes.as_ref(),
                std::fs::read(entry.path()).unwrap(),
                "{path}"
            );
            checked += 1;
        }
        assert!(
            checked >= 19,
            "positive control: functional icon directory was read"
        );
    }

    #[test]
    fn toolbar_tree_and_caption_icons_are_embedded_even_with_debug_assertions() {
        for icon in [
            gpui_component::IconName::PanelLeft,
            gpui_component::IconName::PanelRight,
            gpui_component::IconName::ArrowLeft,
            gpui_component::IconName::ArrowRight,
            gpui_component::IconName::Search,
            gpui_component::IconName::Ellipsis,
            gpui_component::IconName::Folder,
            gpui_component::IconName::FolderOpen,
            gpui_component::IconName::FileText,
            gpui_component::IconName::File,
            gpui_component::IconName::ChevronRight,
            gpui_component::IconName::ChevronDown,
            gpui_component::IconName::WindowMinimize,
            gpui_component::IconName::WindowMaximize,
            gpui_component::IconName::WindowRestore,
            gpui_component::IconName::WindowClose,
        ] {
            let path = icon.path();
            let data = Assets.load(&path).unwrap().expect("Embedded icon");
            assert!(
                matches!(data, Cow::Borrowed(_)),
                "{path} must not read a build-host file"
            );
            assert!(std::str::from_utf8(&data).unwrap().contains("<svg"));
        }
    }

    #[gpui::test]
    fn appearance_changes_keep_native_control_tokens_and_typography(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            for mode in [ThemeMode::Light, ThemeMode::Dark, ThemeMode::Light] {
                Theme::change(mode, None, cx);
                apply_theme(cx);
                let p = palette(cx);
                let t = Theme::global(cx);
                assert_eq!(t.font_family.as_ref(), SANS_FONT);
                assert_eq!(t.mono_font_family.as_ref(), MONO_FONT);
                // Buttons consume resolved backgrounds, so updating only the
                // legacy color fields would leave them in the stock theme.
                assert_eq!(t.tokens.button_primary.color, p.accent);
                assert_eq!(t.tokens.button_secondary.color, p.surface_raised);
                assert_eq!(t.tokens.sidebar.color, p.sidebar);
                assert_eq!(t.tokens.ring.color, p.focus);
                assert_eq!(t.switch, p.text_muted);
                assert_eq!(t.switch_thumb, p.canvas);
                assert_eq!(t.input, p.border);
                assert_ne!(t.input, p.surface);
                assert_eq!(t.button_primary_foreground, p.on_accent);
                assert_eq!(t.foreground, p.text);
            }
        });
    }
}
