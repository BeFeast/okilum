//! Approved B1.2.0 identity with GUI V2 neutral interface aliases.
//!
//! Assets are embedded: no fontconfig/OS font install or network lookup is needed.
//! Call `load_fonts` once before creating windows and `apply_theme` after every
//! system-appearance sync. Both Reader and AI Brain then share the same typography.
use std::{borrow::Cow, sync::OnceLock};

#[cfg(all(unix, feature = "brain"))]
use gpui::ElementId;
use gpui::{px, rgb, svg, App, AssetSource, Hsla, SharedString, Styled, Svg};
#[cfg(all(unix, feature = "brain"))]
use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_component::{Colorize as _, Theme};

pub const SANS_FONT: &str = "Noto Sans";
pub const MONO_FONT: &str = "Cascadia Code";
pub const CHROME_FONT_SIZE: f32 = 14.;
pub const READING_FONT_SIZE: f32 = 15.5;
/// Reader chrome (header, panels, tree) per docs/design/reader.md.
pub const READER_CHROME_FONT_SIZE: f32 = 13.;
pub const MONO_FONT_SIZE: f32 = 13.;
#[cfg(all(unix, feature = "brain"))]
pub const CONTROL_HEIGHT: f32 = 36.;
#[cfg(all(unix, feature = "brain"))]
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
pub const SYSTEM_APPEARANCE_ICON: &str = "icons/monitor.svg";
const IMAGES: [(&str, &[u8]); 26] = [
    (
        "icons/code-xml.svg",
        include_bytes!("../assets/icons/code-xml.svg"),
    ),
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
        SYSTEM_APPEARANCE_ICON,
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

/// A user-selectable color theme (#349). Every theme is a pure token set: the
/// default `Tessera` is `interface-tokens.json` + `reader-tokens.json`, the others
/// are `assets/themes/<key>.json`. Light and dark stay a separate choice
/// (`AppearancePreference`); each theme defines both variants.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeId {
    #[default]
    Tessera,
    Graphite,
    Paper,
    HighContrast,
    Nord,
}

impl ThemeId {
    pub const ALL: [Self; 5] = [
        Self::Tessera,
        Self::Graphite,
        Self::Paper,
        Self::HighContrast,
        Self::Nord,
    ];

    /// Stable key for app config and token files.
    pub fn key(self) -> &'static str {
        match self {
            Self::Tessera => "tessera",
            Self::Graphite => "graphite",
            Self::Paper => "paper",
            Self::HighContrast => "high-contrast",
            Self::Nord => "nord",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Tessera => "Tessera",
            Self::Graphite => "Graphite",
            Self::Paper => "Paper",
            Self::HighContrast => "High contrast",
            Self::Nord => "Nord",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|theme| theme.key() == key)
    }

    fn tokens(self) -> Option<&'static str> {
        match self {
            Self::Tessera => None,
            Self::Graphite => Some(include_str!("../assets/themes/graphite.json")),
            Self::Paper => Some(include_str!("../assets/themes/paper.json")),
            Self::HighContrast => Some(include_str!("../assets/themes/high-contrast.json")),
            Self::Nord => Some(include_str!("../assets/themes/nord.json")),
        }
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|theme| *theme == self)
            .unwrap_or(0)
    }
}

/// The selected theme. One app-wide choice, never per vault or in notes.
#[derive(Clone, Copy, Debug, Default)]
pub struct ThemeChoice(pub ThemeId);
impl gpui::Global for ThemeChoice {}

pub fn theme_id(cx: &App) -> ThemeId {
    cx.try_global::<ThemeChoice>()
        .map(|choice| choice.0)
        .unwrap_or_default()
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
    /// Text and icons on a filled status color.
    pub on_status: Hsla,
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
    pub callout_important: Hsla,
    /// `<mark>` background; translucent so it reads over any surface.
    pub highlight: Hsla,
    /// Dims the document under an overlay.
    pub scrim: Hsla,
}

/// Status roles may fall back to the brand tokens; every other role must be
/// named by the theme itself so no theme silently inherits a foreign surface.
const STATUS_KEYS: [&str; 4] = ["success", "warning", "danger", "info"];

/// `#rrggbb` or `#rrggbbaa`.
fn parse_color(hex: &str) -> Hsla {
    let hex = hex.trim_start_matches('#');
    let value = u32::from_str_radix(hex, 16).expect("validated color token");
    match hex.len() {
        6 => rgb(value).into(),
        8 => gpui::rgba(value).into(),
        _ => panic!("color token must be #rrggbb or #rrggbbaa: {hex}"),
    }
}

/// `#rrggbbaa` for APIs that take color strings (HTML attributes).
pub fn css_color(color: Hsla) -> String {
    let c = gpui::Rgba::from(color);
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}{:02x}",
        byte(c.r),
        byte(c.g),
        byte(c.b),
        byte(c.a)
    )
}

type ThemeTokens = [(Palette, ReaderPalette); 2];

fn resolve(theme: ThemeId) -> ThemeTokens {
    let parse = |json: &str| -> serde_json::Value {
        serde_json::from_str(json).expect("validated token file")
    };
    let brand = parse(include_str!("../assets/brand/brand-tokens.json"));
    let (layers, own): (Vec<serde_json::Value>, &str) = match theme.tokens() {
        None => (
            vec![
                parse(include_str!("../assets/brand/interface-tokens.json")),
                parse(include_str!("../assets/reader/reader-tokens.json")),
            ],
            "themes",
        ),
        Some(json) => (vec![parse(json)], "modes"),
    };
    ["light", "dark"].map(|mode| {
        let color = |key: &str| -> Hsla {
            let value = layers
                .iter()
                .find_map(|layer| layer[own][mode].get(key))
                .or_else(|| {
                    STATUS_KEYS
                        .contains(&key)
                        .then(|| &brand["themes"][mode][key])
                })
                .unwrap_or_else(|| panic!("theme {} {mode} lacks {key}", theme.key()));
            parse_color(value.as_str().expect("validated color string"))
        };
        (
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
                on_status: color("onStatus"),
            },
            ReaderPalette {
                text_faint: color("textFaint"),
                hover: color("hover"),
                missing_link: color("missingLink"),
                code_bg: color("codeBg"),
                code_border: color("codeBorder"),
                callout_question: color("calloutQuestion"),
                callout_important: color("calloutImportant"),
                highlight: color("highlight"),
                scrim: color("scrim"),
            },
        )
    })
}

fn tokens(theme: ThemeId, dark: bool) -> (Palette, ReaderPalette) {
    static TOKENS: OnceLock<Vec<ThemeTokens>> = OnceLock::new();
    TOKENS.get_or_init(|| ThemeId::ALL.map(resolve).to_vec())[theme.index()][usize::from(dark)]
}

/// Both variants of a theme, for previews that must not depend on the live mode.
pub fn theme_palette(theme: ThemeId, dark: bool) -> Palette {
    tokens(theme, dark).0
}

pub fn reader_palette_for_theme(theme: &Theme) -> ReaderPalette {
    tokens(applied_theme(), theme.is_dark()).1
}

pub fn reader_palette(cx: &App) -> ReaderPalette {
    tokens(theme_id(cx), Theme::global(cx).is_dark()).1
}

/// For render callbacks that have no `App` (link presentation closures). Kept in
/// step with the live theme by `apply_theme`.
static APPLIED_DARK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static APPLIED_THEME: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn applied_theme() -> ThemeId {
    ThemeId::ALL
        .get(APPLIED_THEME.load(std::sync::atomic::Ordering::Relaxed))
        .copied()
        .unwrap_or_default()
}

pub fn reader_palette_current() -> ReaderPalette {
    tokens(
        applied_theme(),
        APPLIED_DARK.load(std::sync::atomic::Ordering::Relaxed),
    )
    .1
}

pub fn palette(cx: &App) -> Palette {
    tokens(theme_id(cx), Theme::global(cx).is_dark()).0
}

/// Reapply after `Theme::sync_system_appearance` or `Theme::change`, which resets
/// component colors. Preserve the toolkit's matching syntax highlight theme.
pub fn apply_theme(cx: &mut App) {
    let p = palette(cx);
    APPLIED_DARK.store(
        Theme::global(cx).is_dark(),
        std::sync::atomic::Ordering::Relaxed,
    );
    APPLIED_THEME.store(theme_id(cx).index(), std::sync::atomic::Ordering::Relaxed);
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
    let on_status = p.on_status;
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
#[cfg(all(unix, feature = "brain"))]
pub fn control(id: impl Into<ElementId>, _cx: &App) -> Button {
    Button::new(id)
        .secondary()
        .rounded(px(CONTROL_RADIUS))
        .h(px(CONTROL_HEIGHT))
        .px(px(12.))
}

#[cfg(all(unix, feature = "brain"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    Primary,
    Secondary,
    Quiet,
    Danger,
}

/// Native Button retains toolkit focus/disabled/loading/click behavior. The
/// instance sets geometry only, so semantic state colors remain effective.
#[cfg(all(unix, feature = "brain"))]
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

/// The mark's single color comes from the brand tokens, not from the selected
/// theme: the identity stays the same under every theme.
fn brand_mark(dark: bool) -> Hsla {
    static MARK: OnceLock<[Hsla; 2]> = OnceLock::new();
    MARK.get_or_init(|| {
        let brand: serde_json::Value =
            serde_json::from_str(include_str!("../assets/brand/brand-tokens.json"))
                .expect("validated brand tokens");
        // Primary symbol in brand blue; reversed symbol in the dark text color.
        [("light", "accent"), ("dark", "text")].map(|(mode, key)| {
            parse_color(brand["themes"][mode][key].as_str().expect("brand color"))
        })
    })[usize::from(dark)]
}

/// Canonical B geometry, rendered as the approved single-color symbol. SVG masks
/// intentionally do not pretend to support a multicolor app-icon background.
pub fn logo(size_px: f32, cx: &App) -> Svg {
    let dark = Theme::global(cx).is_dark();
    svg()
        .path(if dark { REVERSED_SYMBOL } else { SYMBOL })
        .size(px(size_px))
        .text_color(brand_mark(dark))
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

    /// WCAG 2.x relative luminance of an opaque color.
    fn luminance(color: Hsla) -> f32 {
        let c = gpui::Rgba::from(color);
        let channel = |v: f32| {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
    }

    fn contrast(a: Hsla, b: Hsla) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn contrast_helper_matches_the_wcag_reference_points() {
        let black: Hsla = rgb(0x000000).into();
        let white: Hsla = rgb(0xffffff).into();
        assert!((contrast(black, white) - 21.).abs() < 0.01);
        assert!((contrast(white, white) - 1.).abs() < 0.01);
        // #767676 on white is the canonical 4.54:1 AA boundary grey.
        let grey: Hsla = rgb(0x767676).into();
        assert!((contrast(grey, white) - 4.54).abs() < 0.01);
    }

    /// #349: every theme, in both variants, keeps reading text at WCAG AA
    /// (AAA for the high-contrast theme) on every surface it is drawn on.
    #[test]
    fn every_theme_meets_wcag_contrast_for_text_muted_and_links() {
        let mut failures = Vec::new();
        for theme in ThemeId::ALL {
            let reading = if theme == ThemeId::HighContrast {
                7.0
            } else {
                4.5
            };
            for dark in [false, true] {
                let (p, r) = tokens(theme, dark);
                let mut check = |what: &str, fg: Hsla, bg: Hsla, min: f32| {
                    let ratio = contrast(fg, bg);
                    if ratio < min {
                        failures.push(format!(
                            "{} {}: {what} {ratio:.2} < {min}",
                            theme.key(),
                            if dark { "dark" } else { "light" }
                        ));
                    }
                };
                for (name, bg) in [
                    ("surface", p.surface),
                    ("canvas", p.canvas),
                    ("sidebar", p.sidebar),
                ] {
                    check(&format!("text on {name}"), p.text, bg, reading);
                    check(&format!("text-muted on {name}"), p.text_muted, bg, reading);
                }
                for (name, bg) in [("surface", p.surface), ("canvas", p.canvas)] {
                    check(&format!("link on {name}"), p.link, bg, reading);
                    check(
                        &format!("missing-link on {name}"),
                        r.missing_link,
                        bg,
                        reading,
                    );
                }
                for (name, bg) in [
                    ("code-bg", r.code_bg),
                    ("surface-raised", p.surface_raised),
                    ("selected", p.selected),
                    ("hover", r.hover),
                ] {
                    check(&format!("text on {name}"), p.text, bg, reading);
                }
                check("on-accent on accent", p.on_accent, p.accent, 4.5);
                // Counts and hints are supplementary; still never below 3:1.
                let faint = if theme == ThemeId::HighContrast {
                    4.5
                } else {
                    3.0
                };
                check("text-faint on surface", r.text_faint, p.surface, faint);
                for (name, status) in [
                    ("success", p.success),
                    ("warning", p.warning),
                    ("danger", p.danger),
                    ("info", p.info),
                ] {
                    check(&format!("{name} on surface"), status, p.surface, 4.5);
                    check(&format!("on-status on {name}"), p.on_status, status, 4.5);
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn themes_are_distinct_token_sets_with_stable_keys() {
        for theme in ThemeId::ALL {
            assert_eq!(ThemeId::from_key(theme.key()), Some(theme));
            if let Some(json) = theme.tokens() {
                let file: serde_json::Value = serde_json::from_str(json).unwrap();
                assert_eq!(file["schema"], "tessera-theme/v1");
                assert_eq!(file["id"], theme.key());
            }
        }
        assert_eq!(ThemeId::from_key("solarized"), None);
        for (i, a) in ThemeId::ALL.into_iter().enumerate() {
            for b in ThemeId::ALL.into_iter().skip(i + 1) {
                for dark in [false, true] {
                    let (a, b) = (theme_palette(a, dark), theme_palette(b, dark));
                    assert!(
                        a.surface != b.surface || a.accent != b.accent,
                        "two themes render the same"
                    );
                }
            }
        }
    }

    #[test]
    fn default_theme_keeps_the_approved_reader_tokens() {
        // Selecting nothing must look exactly like the R1 spec.
        let (light, reader) = tokens(ThemeId::Tessera, false);
        assert_eq!(light.surface, rgb(0xffffff).into());
        assert_eq!(light.text, rgb(0x24262b).into());
        assert_eq!(light.link, rgb(0x0969e8).into());
        assert_eq!(reader.missing_link, rgb(0x9a5b00).into());
        assert_eq!(
            theme_palette(ThemeId::Tessera, true).surface,
            rgb(0x202226).into()
        );
        assert_eq!(css_color(reader.highlight), "#ffd00066");
    }

    #[gpui::test]
    fn selected_theme_drives_palette_and_native_tokens(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            for theme in [ThemeId::Nord, ThemeId::HighContrast, ThemeId::Tessera] {
                for mode in [ThemeMode::Light, ThemeMode::Dark] {
                    cx.set_global(ThemeChoice(theme));
                    Theme::change(mode, None, cx);
                    apply_theme(cx);
                    let expected = theme_palette(theme, mode == ThemeMode::Dark);
                    let p = palette(cx);
                    assert_eq!(p.surface, expected.surface);
                    let t = Theme::global(cx);
                    assert_eq!(t.background, expected.canvas);
                    assert_eq!(t.tokens.button_primary.color, expected.accent);
                    assert_eq!(t.danger_foreground, expected.on_status);
                }
            }
        });
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
