#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

//! Tessera desktop shell — a reader over `tessera-core`, on GPUI via
//! `gpui-component`.
//!
//! Ported from the framework spike. The probe instrumentation that existed to
//! decide the framework (self-driven scroll sweeps, height dumps, frame
//! timing) is deliberately gone: it measured a decision that has been made.

mod about;
#[cfg(all(unix, feature = "brain"))]
mod brain;
mod brand;
#[cfg(all(unix, feature = "brain"))]
mod connectors;
mod desktop_app_menu;
#[cfg(all(unix, feature = "brain"))]
mod export;
#[cfg(windows)]
mod markdown_handler;
mod pdf_engine;
mod platform;
mod prepared_links;
mod quick_open;
#[cfg(not(all(unix, feature = "brain")))]
mod reader_app_menu;
mod reader_cache;
mod reader_code;
mod reader_code_language;
#[cfg(unix)]
mod reader_create;
mod reader_diagnostics;
mod reader_document_menu;
mod reader_drawing;
#[cfg(unix)]
mod reader_editor;
#[cfg(windows)]
#[path = "reader_editor_windows.rs"]
mod reader_editor;
mod reader_files;
mod reader_history;
mod reader_hover;
mod reader_image;
mod reader_incremental;
mod reader_instance;
mod reader_layout;
mod reader_loading;
mod reader_log;
#[cfg(unix)]
mod reader_move;
#[cfg(unix)]
mod reader_move_picker;
mod reader_obsidian;
mod reader_open;
mod reader_pdf;
mod reader_properties;
mod reader_reading_controls;
#[cfg(unix)]
mod reader_recovery;
#[cfg(windows)]
#[path = "reader_recovery_windows.rs"]
mod reader_recovery;
mod reader_replay;
mod reader_right_panel;
mod reader_session;
mod reader_settings;
#[cfg(target_os = "linux")]
mod reader_settings_sync;
mod reader_sidebar;
use reader_sidebar::SectionAction;
#[cfg(unix)]
mod reader_source_history;
mod reader_startup;
#[cfg(test)]
mod reader_table_tests;
mod reader_tasks;
#[cfg(unix)]
mod reader_templates;
#[cfg(any(target_os = "macos", all(test, unix)))]
mod reader_thumbnail;
#[cfg(unix)]
mod reader_timeline;
#[cfg(windows)]
#[path = "reader_timeline_windows.rs"]
mod reader_timeline;
mod reader_toast;
#[cfg(unix)]
mod reader_trash;
#[cfg(unix)]
mod reader_trash_fs;
mod reader_tree;
mod reader_ui_state;
#[cfg(unix)]
mod source_presentation;
mod text_ranges;
mod theme_picker;
mod updater;
// Run the actual vendor geometry regressions in shell CI: gpui-base is not a
// workspace member, so Cargo cannot run its dev-dependency tests from this root.
#[cfg(test)]
#[allow(dead_code)]
#[path = "../../../vendor/gpui-component/crates/base/src/input/bidi_geometry.rs"]
mod vendor_bidi_geometry;
#[cfg(test)]
#[allow(dead_code)]
#[path = "../../../vendor/gpui-component/crates/base/src/text_boundary.rs"]
mod vendor_text_boundary;
mod window_state;
#[cfg(all(unix, feature = "brain"))]
mod workspace;

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use brand::Assets;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::menu::ContextMenuExt as _;
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    text::{markdown_ast, MarkdownNode, SelectionFormat, TextView, TextViewState, TextViewStyle},
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Root, Selectable as _,
    Sizable as _, Theme, TitleBar,
};
use platform::labels::with_shortcut;
// The schemes come from core, not from copies here. The shell only needs to
// recognise what core emitted; two independent definitions are free to drift,
// and the click handler would then quietly stop matching links the renderer
// had produced.
use tessera_core::callout::{self, CalloutHeader, CalloutKind};
use tessera_core::render::{
    split_open_url, AMBIGUOUS_SCHEME, EMBED_LANG, EMBED_MISSING, UNRESOLVED_SCHEME, WIKI_SCHEME,
};
use tessera_core::{Backlink, Searcher, Vault, VaultWatcher};

/// Body size for the reader; the heading scale and the sidebar derive from it.
const BODY_FONT_SIZE: f32 = brand::READING_FONT_SIZE;
/// Reader and source editor reserve the same scrollable end space (#419).
fn reader_bottom_space(viewport_height: Pixels) -> Pixels {
    px(120.).max(viewport_height * 0.30)
}

/// Reader measure (docs/design/reader.md): a 740px column with 40px side
/// padding keeps lines near 75 characters. It is a maximum: narrower panes
/// still use their full width.
const READER_MAX_WIDTH: f32 = 740.;
const READER_SIDE_PADDING: f32 = 40.;

/// Key context of the reader window. Every binding below is scoped to it so
/// the inputs keep their own (`Input`) bindings: gpui dispatches the deepest
/// matching binding first, and an input that does not consume a key (Escape,
/// Enter on a single-line input) propagates it up to these.
const READER_CONTEXT: &str = "Reader";

actions!(
    tessera,
    [
        NewNote,
        CloseNote,
        NewFolder,
        RenameNote,
        RenameTreeNote,
        DeleteNote,
        UndoTrash,
        RecoverLinkMoves,
        NoteSourceHistory,
        HistoryVersionNext,
        HistoryVersionPrevious,
        RecoverUnsavedNotes,
        ToggleSource,
        SaveSource,
        RevealFile,
        CopyVaultPath,
        QuickLookFile,
        FindInNote,
        ReaderScrollDown,
        ReaderScrollUp,
        ReaderPageDown,
        ReaderPageUp,
        FindNext,
        FindPrev,
        Dismiss,
        QuickOpen,
        FullTextSearch,
        PaletteNext,
        PalettePrevious,
        HistoryBack,
        HistoryForward,
        ListNext,
        ListPrev,
        ScrollTop,
        ScrollBottom,
        ToggleNotes,
        ToggleBacklinks,
        ToggleHiddenFiles,
        NewFromTemplate,
        TreeDown,
        TreeUp,
        TreeRight,
        TreeLeft,
        TreeOpen,
        TreeExpandSubtree,
        TreeCollapseSubtree,
        CollapseSidebarSections,
        ExpandSidebarSections,
        CollapseFolders,
        FocusCurrentFolder,
        PdfZoomIn,
        PdfZoomOut,
        PdfZoomFit,
    ]
);

/// Tree shortcuts (#410) work while the folder tree has focus. Arrow keys
/// keep their modifiers on macOS, so these strings are what a Mac reports;
/// the tests below check that against the macOS key shape.
const TREE_KEYS: &str = "ReaderTree && !Input";
const TREE_EXPAND_SUBTREE_KEY: &str = "alt-right";
const TREE_COLLAPSE_SUBTREE_KEY: &str = "alt-left";
#[cfg(target_os = "macos")]
const COLLAPSE_SECTIONS_KEY: &str = "cmd-shift-left";
#[cfg(not(target_os = "macos"))]
const COLLAPSE_SECTIONS_KEY: &str = "ctrl-shift-left";

/// X11/Wayland may report the press either way: `.` with shift, or `>`.
const HIDDEN_FILES_KEYS: [&str; 2] = ["ctrl-shift-.", "ctrl->"];
/// macOS reports shifted punctuation as the shifted character with shift
/// cleared, so `cmd-shift-.` never matches; ⇧⌘. arrives as `cmd->` (#395).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const HIDDEN_FILES_KEY_MAC: &str = "cmd->";

fn bind_keys(cx: &mut App) {
    #[cfg(unix)]
    reader_move_picker::bind_keys(cx);
    reader_log::bind_keys(cx);
    cx.bind_keys([
        KeyBinding::new("alt-cmd-r", RevealFile, Some(READER_CONTEXT)),
        KeyBinding::new("alt-cmd-c", CopyVaultPath, Some(READER_CONTEXT)),
        KeyBinding::new("space", QuickLookFile, Some("ReaderFile && !Input")),
        KeyBinding::new("space", QuickLookFile, Some("ReaderTree && !Input")),
    ]);
    // ⌘/Ctrl + and − arrive as `=`/`+` and `-` depending on layout and shift.
    let pdf = Some("ReaderPdf");
    cx.bind_keys([
        KeyBinding::new("secondary-=", PdfZoomIn, pdf),
        KeyBinding::new("secondary-+", PdfZoomIn, pdf),
        KeyBinding::new("secondary-shift-=", PdfZoomIn, pdf),
        KeyBinding::new("secondary--", PdfZoomOut, pdf),
        KeyBinding::new("secondary-0", PdfZoomFit, pdf),
    ]);

    cx.bind_keys([
        KeyBinding::new(
            "down",
            ReaderScrollDown,
            Some("Reader > TextView && !Input && !ReaderTree"),
        ),
        KeyBinding::new(
            "up",
            ReaderScrollUp,
            Some("Reader > TextView && !Input && !ReaderTree"),
        ),
        KeyBinding::new(
            "pagedown",
            ReaderPageDown,
            Some("Reader > TextView && !Input && !ReaderTree"),
        ),
        KeyBinding::new(
            "pageup",
            ReaderPageUp,
            Some("Reader > TextView && !Input && !ReaderTree"),
        ),
    ]);

    cx.bind_keys([
        KeyBinding::new("down", HistoryVersionNext, Some("ReaderHistory")),
        KeyBinding::new("up", HistoryVersionPrevious, Some("ReaderHistory")),
        KeyBinding::new("down", HistoryVersionNext, Some("ReaderHistory > Input")),
        KeyBinding::new("up", HistoryVersionPrevious, Some("ReaderHistory > Input")),
    ]);
    let ctx = Some(READER_CONTEXT);
    // `j`/`k` are plain letters: with an input focused they must keep being
    // text, so those two are disabled anywhere under an `Input` context.
    let not_input = format!("{READER_CONTEXT} && !Input");
    cx.bind_keys([
        KeyBinding::new("ctrl-shift-f", FullTextSearch, ctx),
        KeyBinding::new("ctrl-shift-f", FullTextSearch, Some("Reader > Input")),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-k", QuickOpen, ctx),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-k", QuickOpen, Some("Reader > Input")),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-f", FullTextSearch, ctx),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-f", FullTextSearch, Some("Reader > Input")),
        KeyBinding::new("down", PaletteNext, Some("Reader > QuickOpen > Input")),
        KeyBinding::new("up", PalettePrevious, Some("Reader > QuickOpen > Input")),
        KeyBinding::new("escape", Dismiss, Some("Reader > QuickOpen > Input")),
        KeyBinding::new("escape", Dismiss, Some("InlineCreate > Input")),
        #[cfg(unix)]
        KeyBinding::new("escape", Dismiss, Some("InlineRename > Input")),
        #[cfg(unix)]
        KeyBinding::new("f2", RenameTreeNote, Some("ReaderTree && !Input")),
        KeyBinding::new("secondary-n", NewNote, ctx),
        #[cfg(unix)]
        KeyBinding::new("secondary-backspace", DeleteNote, Some("Reader && !Input")),
        #[cfg(unix)]
        KeyBinding::new("secondary-z", UndoTrash, Some("Reader && !Input")),
        KeyBinding::new("secondary-w", CloseNote, ctx),
        KeyBinding::new("secondary-w", CloseNote, Some("Reader > Input")),
        KeyBinding::new("secondary-[", HistoryBack, ctx),
        KeyBinding::new("secondary-]", HistoryForward, ctx),
        KeyBinding::new("secondary-e", ToggleSource, ctx),
        KeyBinding::new("secondary-s", SaveSource, ctx),
        KeyBinding::new("secondary-e", ToggleSource, Some("Reader > Input")),
        KeyBinding::new("secondary-s", SaveSource, Some("Reader > Input")),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-f", FindInNote, ctx),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-f", FindInNote, ctx),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-f", FindInNote, Some("Reader > Input")),
        // Reader commands outrank toolkit editing aliases only inside Reader inputs.
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-f", FindInNote, Some("Reader > Input")),
        KeyBinding::new("ctrl-k", QuickOpen, Some("Reader > Input")),
        KeyBinding::new("escape", Dismiss, ctx),
        KeyBinding::new("ctrl-k", QuickOpen, ctx),
        KeyBinding::new("alt-left", HistoryBack, ctx),
        KeyBinding::new("alt-right", HistoryForward, ctx),
        KeyBinding::new("ctrl-down", ListNext, ctx),
        KeyBinding::new("ctrl-up", ListPrev, ctx),
        KeyBinding::new("j", ListNext, Some(&not_input)),
        KeyBinding::new("k", ListPrev, Some(&not_input)),
        KeyBinding::new("ctrl-\\", ToggleNotes, ctx),
        KeyBinding::new("ctrl-alt-\\", ToggleBacklinks, ctx),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-\\", ToggleNotes, ctx),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-alt-\\", ToggleBacklinks, ctx),
        KeyBinding::new("down", TreeDown, Some("ReaderTree && !Input")),
        KeyBinding::new("up", TreeUp, Some("ReaderTree && !Input")),
        KeyBinding::new("right", TreeRight, Some("ReaderTree && !Input")),
        KeyBinding::new("left", TreeLeft, Some("ReaderTree && !Input")),
        #[cfg(not(unix))]
        KeyBinding::new("enter", TreeOpen, Some("ReaderTree && !Input")),
        #[cfg(unix)]
        KeyBinding::new("enter", RenameTreeNote, Some("ReaderTree && !Input")),
        KeyBinding::new("space", TreeOpen, Some("ReaderTree && !Input")),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-down", TreeOpen, Some(TREE_KEYS)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-down", TreeOpen, Some(TREE_KEYS)),
        KeyBinding::new(TREE_EXPAND_SUBTREE_KEY, TreeExpandSubtree, Some(TREE_KEYS)),
        KeyBinding::new(
            TREE_COLLAPSE_SUBTREE_KEY,
            TreeCollapseSubtree,
            Some(TREE_KEYS),
        ),
        KeyBinding::new(
            COLLAPSE_SECTIONS_KEY,
            CollapseSidebarSections,
            Some("Reader && !Input"),
        ),
        KeyBinding::new(
            "secondary-shift-right",
            ExpandSidebarSections,
            Some("Reader && !Input"),
        ),
        KeyBinding::new(HIDDEN_FILES_KEYS[0], ToggleHiddenFiles, ctx),
        KeyBinding::new(HIDDEN_FILES_KEYS[1], ToggleHiddenFiles, ctx),
        #[cfg(target_os = "macos")]
        KeyBinding::new(HIDDEN_FILES_KEY_MAC, ToggleHiddenFiles, ctx),
        KeyBinding::new("ctrl-home", ScrollTop, ctx),
        KeyBinding::new("ctrl-end", ScrollBottom, ctx),
    ]);
}

/// Application-only preference. Never changes the desktop's appearance setting.
#[derive(Default, Clone, Copy)]
struct AppearancePreference(Option<gpui_component::ThemeMode>);
impl Global for AppearancePreference {}
fn appearance_label(cx: &App) -> &'static str {
    match cx.try_global::<AppearancePreference>().and_then(|p| p.0) {
        None => "System",
        Some(gpui_component::ThemeMode::Light) => "Light",
        Some(gpui_component::ThemeMode::Dark) => "Dark",
    }
}
fn sync_appearance(window: &mut Window, cx: &mut App) {
    match cx.try_global::<AppearancePreference>().and_then(|p| p.0) {
        Some(mode) => Theme::change(mode, Some(window), cx),
        None => Theme::sync_system_appearance(Some(window), cx),
    }
    brand::apply_theme(cx);
}
fn cycle_appearance(window: &mut Window, cx: &mut App) {
    let next = match cx.try_global::<AppearancePreference>().and_then(|p| p.0) {
        None => Some(gpui_component::ThemeMode::Light),
        Some(gpui_component::ThemeMode::Light) => Some(gpui_component::ThemeMode::Dark),
        Some(gpui_component::ThemeMode::Dark) => None,
    };
    set_appearance(next, None, window, cx);
}
/// `None` follows the system. The choice is app config, never note content.
fn set_appearance(
    mode: Option<gpui_component::ThemeMode>,
    vault: Option<&Path>,
    window: &mut Window,
    cx: &mut App,
) {
    cx.set_global(AppearancePreference(mode));
    sync_appearance(window, cx);
    window.refresh();
    cx.refresh_windows();
    if reader_ui_state::set_appearance(mode, cx) {
        return;
    }
    save_appearance(vault, cx);
}
/// One app-wide color theme (#349); Light/Dark/System stays a separate choice.
fn set_theme(theme: brand::ThemeId, vault: Option<&Path>, window: &mut Window, cx: &mut App) {
    #[cfg(test)]
    cx.set_global(theme_picker::ThemeActionVault(vault.map(Path::to_path_buf)));
    cx.set_global(brand::ThemeChoice(theme));
    sync_appearance(window, cx);
    window.refresh();
    cx.refresh_windows();
    if reader_ui_state::set_theme(theme, cx) {
        return;
    }
    save_appearance(vault, cx);
}
fn save_appearance(vault: Option<&Path>, cx: &App) {
    #[cfg(test)]
    let _ = (vault, cx);
    #[cfg(not(test))]
    if let Some(path) = appearance_settings_path().filter(|path| {
        // Never write presentation state into the opened canonical tree.
        vault.is_none_or(|vault| reader_layout::settings_path_at(vault, path.clone()).is_some())
    }) {
        let value = match cx.try_global::<AppearancePreference>().and_then(|p| p.0) {
            None => "system",
            Some(gpui_component::ThemeMode::Light) => "light",
            Some(gpui_component::ThemeMode::Dark) => "dark",
        };
        let saved = (|| {
            let parent = path
                .parent()
                .ok_or_else(|| std::io::Error::other("missing settings directory"))?;
            std::fs::create_dir_all(parent)?;
            let temporary = parent.join(format!(".appearance-{}.json", uuid::Uuid::new_v4()));
            std::fs::write(
                &temporary,
                appearance_settings_json(value, brand::theme_id(cx)).to_string(),
            )?;
            std::fs::rename(&temporary, &path).inspect_err(|_| {
                let _ = std::fs::remove_file(&temporary);
            })
        })();
        if let Err(error) = saved {
            eprintln!("Could not save appearance: {error}");
        }
    }
}
fn appearance_settings_json(appearance: &str, theme: brand::ThemeId) -> serde_json::Value {
    serde_json::json!({ "appearance": appearance, "theme": theme.key() })
}
fn appearance_settings_path() -> Option<PathBuf> {
    reader_layout::config_base().map(|base| base.join("tessera/appearance.json"))
}
/// Unknown or missing values fall back to System and the default theme; a file
/// written before #349 has no `theme` key.
fn parse_appearance_settings(
    value: &serde_json::Value,
) -> (AppearancePreference, brand::ThemeChoice) {
    let mode = match value["appearance"].as_str() {
        Some("light") => Some(gpui_component::ThemeMode::Light),
        Some("dark") => Some(gpui_component::ThemeMode::Dark),
        _ => None,
    };
    let theme = value["theme"]
        .as_str()
        .and_then(brand::ThemeId::from_key)
        .unwrap_or_default();
    (AppearancePreference(mode), brand::ThemeChoice(theme))
}
fn load_appearance(cx: &App) -> (AppearancePreference, brand::ThemeChoice) {
    if reader_recovery::is_recovering(cx) {
        return Default::default();
    }
    if let Some(saved) = reader_ui_state::appearance(cx) {
        return (
            saved,
            brand::ThemeChoice(reader_ui_state::theme(cx).unwrap_or_default()),
        );
    }
    appearance_settings_path()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .map(|value| parse_appearance_settings(&value))
        .unwrap_or_default()
}

/// Reader typography, derived from the live theme.
///
/// Built on every `render_main` rather than cached: the theme flips between
/// light and dark while the app runs (#37), and the highlight theme plus
/// `is_dark` (code background, heading colours) have to follow it. Colours
/// not set here are refined onto the themed defaults at layout time.
fn reader_text_style(theme: &Theme) -> TextViewStyle {
    let tokens = brand::reader_palette_for_theme(theme);
    TextViewStyle {
        highlight_theme: theme.highlight_theme.clone(),
        is_dark: theme.is_dark(),
        ..TextViewStyle::default()
    }
    .table({
        let mut style = StyleRefinement::default();
        style.overflow.x = Some(Overflow::Scroll);
        style
    })
    .code_block(
        StyleRefinement::default()
            .bg(tokens.code_bg)
            .border_1()
            .border_color(tokens.code_border)
            .rounded(px(8.)),
    )
    .inline_code(HighlightStyle {
        background_color: Some(tokens.code_bg),
        ..Default::default()
    })
    .paragraph_gap(rems(0.75))
    // docs/design/reader.md: 15.5 body → H1 30, H2 21, H3 17, H4–H6 body size.
    .heading_font_size(|level, base| {
        let size = match level {
            1 => 30.,
            2 => 21.,
            3 => 17.,
            _ => return base,
        };
        base * (size / brand::READING_FONT_SIZE)
    })
}

#[cfg(test)]
type IndexBuildHook = Arc<dyn Fn(&Path) + Send + Sync>;

#[derive(Default, Clone)]
struct Opts {
    /// Opt-in backend client; the ordinary reader remains unchanged.
    brain_endpoint: Option<std::net::SocketAddr>,
    /// Open the managed workspace chooser only by explicit request.
    managed_workspace: bool,
    /// OS document delivery may arrive after launch; never auto-connect while awaiting it.
    defer_saved_connection: bool,
    /// Vault root. Required: there is no default vault, and guessing one would
    /// be a good way to index the wrong directory.
    vault: Option<PathBuf>,
    open_path: Option<PathBuf>,
    single_file: bool,
    defer_loading: bool,
    reusable_roots: Vec<PathBuf>,
    session_directory: Option<PathBuf>,
    exact_restore: bool,
    diagnostics: Option<reader_diagnostics::Trace>,
    /// Explicit manual refresh verifies canonical bytes even with unchanged revisions.
    force_source_read: bool,
    reconcile_recent: Vec<String>,
    #[cfg(test)]
    preparation_hold: Option<async_channel::Receiver<()>>,
    #[cfg(test)]
    validation_hold: Option<async_channel::Receiver<()>>,
    #[cfg(test)]
    panel_preferences_hold: Option<async_channel::Receiver<()>>,
    #[cfg(test)]
    panel_settings_override: Option<PathBuf>,
    #[cfg(test)]
    rest_snapshot_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    #[cfg(test)]
    index_build_hook: Option<IndexBuildHook>,
    #[cfg(test)]
    search_publish_in_place: bool,
    cache_lease: Option<Arc<reader_cache::Lease>>,
    /// Explicit search index override. Direct opens default to application cache.
    index_dir: Option<PathBuf>,
    #[cfg(test)]
    cache_base_override: Option<PathBuf>,
    use_html: bool,
    note: Option<String>,
    query: Option<String>,
    jump: bool,
    copy_source: bool,
}

/// Payload for the custom "local-image" markdown block node. TextView's stock
/// image path always builds `Resource::Uri` and fetches via the HTTP client,
/// so local `file://` images can never load; this plugin renders standalone
/// image paragraphs through `Resource::Path` (fs::read) instead.
#[derive(Clone)]
struct LocalImage {
    url: String,
    caption: String,
    drawing_size: Option<(f32, Option<f32>)>,
}

fn mdast_text(node: &markdown_ast::Node, out: &mut String) {
    match node {
        markdown_ast::Node::Text(t) => out.push_str(&t.value),
        markdown_ast::Node::InlineCode(c) => out.push_str(&c.value),
        _ => {}
    }
    if let Some(children) = node.children() {
        for c in children {
            mdast_text(c, out);
        }
    }
}

/// Payload for the custom "callout" block node (#46). An Obsidian callout is a
/// blockquote whose first line is `[!type]`; markdown-rs hands it over as
/// `Node::Blockquote`, so the block parser recognises it by its source and
/// the renderer draws a tinted box with a title row and the body as nested
/// Markdown. The body is re-rendered from source rather than from the mdast
/// children so lists, fences and nested quotes keep working unchanged.
#[derive(Clone)]
struct Callout {
    header: CalloutHeader,
    /// Body Markdown, `>` markers already stripped. Empty for a bare header.
    body: String,
    /// Byte offset of the quote in the document, for test selectors.
    offset: usize,
    /// Fold-state key of a foldable (`+`/`-`) callout (#651).
    key: u64,
}

/// Parse an Obsidian callout out of a blockquote node. `None` for a plain quote.
fn parse_callout(
    node: &markdown_ast::Node,
    cx: &gpui_component::text::MarkdownParseContext<'_>,
) -> Option<MarkdownNode> {
    let markdown_ast::Node::Blockquote(_) = node else {
        return None;
    };
    let source = cx.node_source(node)?;
    let header = callout::parse_header(source.lines().next()?)?;
    let body = callout::body_from_source(source);
    let offset = cx.offset() + node.position()?.start.offset;
    Some(
        MarkdownNode::new(
            "callout",
            Callout {
                header: header.clone(),
                body: body.clone(),
                offset,
                key: reader_obsidian::callout_key(offset, source),
            },
        )
        .plain_part("title", header.title.clone())
        .markdown_part("body", body)
        .text(source.to_string())
        .markdown(source.to_string()),
    )
}

/// Payload for the custom "embed" block node (#49). Core expands `![[note]]`
/// into the target's body inside a tilde fence tagged [`EMBED_LANG`], with
/// the `tessera://open/` payload as the rest of the info string; markdown-rs
/// hands the fence over as `Node::Code`, so the block parser recognises it by
/// its language and the renderer draws a bordered box with a title row that
/// links to the note and the body as nested Markdown.
#[derive(Clone)]
struct Embed {
    /// Vault-relative path of the embedded note, or the target as written
    /// when `missing`.
    path: String,
    /// Heading the embed names, if any.
    heading: Option<String>,
    missing: bool,
    /// Byte offset of the fence in the document; keys the nested TextView's
    /// state so two embeds in one note never share one.
    offset: usize,
}

/// Parse an expanded embed out of a code node. `None` for real code.
fn parse_embed(
    node: &markdown_ast::Node,
    cx: &gpui_component::text::MarkdownParseContext<'_>,
) -> Option<MarkdownNode> {
    let markdown_ast::Node::Code(code) = node else {
        return None;
    };
    if code.lang.as_deref() != Some(EMBED_LANG) {
        return None;
    }
    let meta = code.meta.as_deref().unwrap_or("").trim();
    let offset = cx.offset() + node.position()?.start.offset;
    let pending = meta
        .strip_prefix(tessera_core::render::EMBED_PENDING)
        .is_some_and(|rest| rest.starts_with(' '));
    let (path, heading, missing) = match meta.strip_prefix(if pending {
        tessera_core::render::EMBED_PENDING
    } else {
        EMBED_MISSING
    }) {
        Some(rest) if rest.starts_with(' ') || rest.is_empty() => {
            (rest.trim().to_string(), None, true)
        }
        _ => {
            let (rel, heading) = split_open_url(meta);
            (rel, heading, false)
        }
    };
    let title = if pending {
        format!("embed loading: {path}")
    } else if missing {
        format!("embed not found: {path}")
    } else {
        let title = tessera_core::Vault::title_of(&path);
        match &heading {
            Some(h) => format!("{title} › {h}"),
            None => title,
        }
    };
    Some(
        MarkdownNode::new(
            "embed",
            Embed {
                path,
                heading,
                missing,
                offset,
            },
        )
        .plain_part("title", title.clone())
        .markdown_part(
            "body",
            if missing {
                String::new()
            } else {
                code.value.clone()
            },
        )
        .text(title)
        .markdown(cx.node_source(node).unwrap_or("").to_string()),
    )
}

/// Parse a paragraph or heading that contains an inline `<mark>` into a
/// `"highlight"` node. `None` for every other block, so plain paragraphs keep
/// TextView's stock Markdown rendering.
fn parse_highlight(
    node: &markdown_ast::Node,
    cx: &gpui_component::text::MarkdownParseContext<'_>,
) -> Option<MarkdownNode> {
    let children = match node {
        markdown_ast::Node::Paragraph(p) => &p.children,
        markdown_ast::Node::Heading(h) => &h.children,
        _ => return None,
    };
    let has_mark = children.iter().any(|c| {
        matches!(c, markdown_ast::Node::Html(h) if h.value.to_ascii_lowercase().starts_with("<mark"))
    });
    if !has_mark {
        return None;
    }
    let source = cx.node_source(node)?;
    // The `highlight` token is translucent, so it reads on every theme. It is
    // resolved when the block is parsed; a theme switch applies on re-parse.
    let color = brand::css_color(brand::reader_palette_current().highlight);
    let html = tessera_core::render::block_html(source)
        .replace("<mark>", &format!("<mark color=\"{color}\">"));
    let mut text = String::new();
    mdast_text(node, &mut text);
    Some(
        MarkdownNode::new("highlight", ())
            .html_part("body", html)
            .text(text)
            .markdown(source.to_string()),
    )
}

fn callout_look(header: &CalloutHeader, theme: &Theme) -> (Hsla, IconName) {
    if header.type_name == "important" {
        return (
            brand::reader_palette_for_theme(theme).callout_important,
            IconName::Asterisk,
        );
    }
    match header.kind {
        CalloutKind::Warning => (theme.warning, IconName::TriangleAlert),
        CalloutKind::Danger => (theme.danger, IconName::TriangleAlert),
        CalloutKind::Failure => (theme.danger, IconName::CircleX),
        CalloutKind::Bug => (theme.danger, IconName::Bot),
        CalloutKind::Tip => (theme.success, IconName::Star),
        CalloutKind::Success => (theme.success, IconName::CircleCheck),
        CalloutKind::Todo => (theme.info, IconName::Check),
        CalloutKind::Info | CalloutKind::Unknown => (theme.info, IconName::Info),
        CalloutKind::Note => (theme.info, IconName::FileText),
        // No orange in the theme palette; the Reader tokens carry one.
        CalloutKind::Question => (
            brand::reader_palette_for_theme(theme).callout_question,
            IconName::Search,
        ),
        CalloutKind::Summary => (theme.muted_foreground, IconName::Menu),
        CalloutKind::Example => (theme.muted_foreground, IconName::SquareTerminal),
        CalloutKind::Quote => (theme.muted_foreground, IconName::BookOpen),
    }
}

fn backlink_occurrence(b: &Backlink) -> (String, Option<std::ops::Range<usize>>, Option<String>) {
    let (mut context, mut link) = b.context_plain_with_link();
    // A frontmatter relation reads as «key: Target» (#386)
    // and lands at the top: frontmatter is not rendered.
    let property = b.property.is_some();
    if let Some(key) = &b.property {
        let target = link
            .clone()
            .and_then(|r| context.get(r).map(str::to_owned))
            .unwrap_or_else(|| b.link.clone());
        let prefix = format!("{key}: ");
        link = Some(prefix.len()..prefix.len() + target.len());
        context = prefix + &target;
    }
    // Where to land: the linking line itself, which tells
    // occurrences apart better than the link text.
    let jump = (!property).then(|| context.chars().take(48).collect::<String>());
    let (context, link) = text_ranges::snippet(context, link, 160);
    (context, link, jump)
}

/// Link handling shared by the reader's TextView and every nested one (a
/// callout body): wikilinks open notes, ambiguous ones go to search,
/// unresolved ones are inert, http(s) leaves the app.
fn handle_link(entity: &WeakEntity<Reader>, url: &str, window: &mut Window, cx: &mut App) {
    if let Some(entity) = entity.upgrade() {
        let landed = entity.update(cx, |this, cx| {
            let landing = reader_obsidian::footnote_landing(url, &this.note_source)?;
            match landing {
                Ok(ix) => this.scroll_to_block(ix, cx),
                Err(reason) => this.link_notice = Some(reason.into()),
            }
            cx.notify();
            Some(())
        });
        if landed.is_some() {
            return;
        }
    }
    let prepared = entity.upgrade().and_then(|entity| {
        let reader = entity.read(cx);
        reader.prepared_links.get(url).cloned().or_else(|| {
            reader
                .link_identities
                .iter()
                .any(|link| link.url == url)
                .then(tessera_core::document_links::prepared::LinkState::unknown)
        })
    });
    if prepared
        .as_ref()
        .is_some_and(|state| state.status.is_missing())
    {
        return;
    }
    if let Some(state) = prepared
        .as_ref()
        .filter(|state| state.status == tessera_core::document_links::prepared::LinkStatus::Unknown)
    {
        if let Some(entity) = entity.upgrade() {
            entity.update(cx, |this, cx| {
                this.link_notice = Some(state.reason.clone().into());
                cx.notify();
            });
        }
        return;
    }
    let url = prepared
        .as_ref()
        .and_then(|state| state.action_url.as_deref())
        .unwrap_or(url);
    if url.starts_with("tessera://outside-file/") {
        let _ = entity.update(cx, |this, cx| this.outside_file_menu(url, window, cx));
    } else if let Some(rest) = url.strip_prefix("tessera://attachment/") {
        let _ = entity.update(cx, |this, cx| {
            this.preview_file(&tessera_core::document_links::decode(rest), window, cx)
        });
    } else if let Some(rest) = url.strip_prefix(WIKI_SCHEME) {
        // `[[note#Heading]]` carries the heading past the rewrite (#49). An
        // empty path is `[[#Heading]]` in a file outside the vault: the note
        // on screen.
        let (rel, heading) = split_open_url(rest);
        if let Some(entity) = entity.upgrade() {
            entity.update(cx, |this, cx| {
                let rel = if rel.is_empty() {
                    this.current_rel.clone()
                } else {
                    rel
                };
                this.open_note_at(&rel, None, heading.as_deref(), window, cx);
            });
        }
    } else if let Some((rest, wiki)) = url
        .strip_prefix(AMBIGUOUS_SCHEME)
        .map(|r| (r, true))
        .or_else(|| {
            url.strip_prefix("tessera://ambiguous-markdown/")
                .map(|r| (r, false))
        })
    {
        let target = tessera_core::document_links::decode(rest);
        if let Some(entity) = entity.upgrade() {
            entity.update(cx, |this, cx| {
                let resolved = tessera_core::document_links::resolve(
                    &target,
                    wiki,
                    &this.vault,
                    &this.current_rel,
                );
                this.link_notice =
                    Some("This document link is ambiguous. Choose its destination.".into());
                this.link_choices = resolved
                    .candidates
                    .into_iter()
                    .map(|path| (path, resolved.heading.clone()))
                    .collect();
                cx.notify();
            });
        }
    } else if let Some(reason) = url.strip_prefix("tessera://unsupported/") {
        if let Some(entity) = entity.upgrade() {
            entity.update(cx, |this, cx| {
                this.link_notice = Some(tessera_core::document_links::decode(reason).into());
                this.link_choices.clear();
                cx.notify();
            });
        }
    } else if url.starts_with(UNRESOLVED_SCHEME) {
        if let Some(entity) = entity.upgrade() {
            entity.update(cx, |this, cx| {
                this.link_notice = Some(
                    format!(
                        "No document matches this link: {}",
                        tessera_core::document_links::decode(
                            url.trim_start_matches(UNRESOLVED_SCHEME)
                        )
                    )
                    .into(),
                );
                cx.notify();
            });
        }
    } else if prepared_links::external_tooltip(url).is_some() {
        cx.open_url(url);
    } else if let Some(entity) = entity.upgrade() {
        entity.update(cx, |this, cx| {
            this.link_notice = Some("This link action is not supported.".into());
            cx.notify();
        });
    }
}

/// Install the reader's Markdown plugins and link handler on a TextView.
/// Recursive on purpose: a callout body is rendered by a nested TextView
/// built through the same function, so images and callouts inside a callout
/// render the same way as at top level.
fn reader_plugins(
    root: PathBuf,
    view: TextView,
    entity: WeakEntity<Reader>,
    sel_format: SelectionFormat,
    states: prepared_links::States,
    identities: &[tessera_core::document_links::prepared::LinkIdentity],
) -> TextView {
    // Use exactly the same eligibility as the Create note hover action.
    #[cfg(unix)]
    let missing_cards = identities
        .iter()
        .filter_map(|identity| {
            reader_hover::missing_note_target(&identity.url, &states, identities)
                .map(|_| identity.url.clone())
        })
        .collect();
    #[cfg(not(unix))]
    let missing_cards = {
        let _ = identities;
        std::collections::BTreeSet::new()
    };
    let hover_entity = entity.clone();
    let tasks_entity = entity.clone();
    let view = markdown_plugins(
        view,
        Arc::new(move |url, event, window, cx| {
            if matches!(event, ClickEvent::Mouse(e) if e.up.button == MouseButton::Right) {
                let _ = entity.update(cx, |this, cx| {
                    this.file_link_menu(url, event.position(), window, cx)
                });
            } else {
                handle_link(&entity, url, window, cx);
            }
        }),
        Arc::new(move |url| {
            if let Some(payload) = url.strip_prefix("tessera-drawing-unavailable:") {
                return Some(MarkdownImage::DrawingUnavailable(payload.to_owned()));
            }
            if let Some(path) = url::Url::parse(url)
                .ok()
                .and_then(|url| url.to_file_path().ok())
            {
                if tessera_core::excalidraw::is_drawing(&path.to_string_lossy()) {
                    Some(MarkdownImage::Drawing(root.clone(), path))
                } else {
                    Some(MarkdownImage::Source(reader_files::image_source(path)))
                }
            } else if url.starts_with("https://")
                || url.starts_with("http://")
                || url.starts_with("data:")
            {
                Some(MarkdownImage::Source(ImageSource::from(url)))
            } else {
                None
            }
        }),
        sel_format,
    )
    .on_link_hover(move |url, active, position, window, cx| {
        let _ = hover_entity.update(cx, |this, cx| {
            this.hover_link(url, active, position, window, cx)
        });
    })
    .link_presentation(move |url| reader_hover::link_presentation(url, &states, &missing_cards));
    reader_tasks::plugins(view, tasks_entity)
}

type MarkdownLinkHandler = Arc<dyn Fn(&str, &ClickEvent, &mut Window, &mut App) + Send + Sync>;
enum MarkdownImage {
    Drawing(PathBuf, PathBuf),
    DrawingUnavailable(String),
    Source(ImageSource),
    Unavailable,
}
type MarkdownImageResolver = Arc<dyn Fn(&str) -> Option<MarkdownImage> + Send + Sync>;

/// The same recursive Markdown plugins serve local Reader paths and remote
/// Brain image bytes. The caller supplies navigation and image authority.
fn markdown_plugins(
    view: TextView,
    link_handler: MarkdownLinkHandler,
    image_resolver: MarkdownImageResolver,
    sel_format: SelectionFormat,
) -> TextView {
    let link_click = link_handler.clone();
    let parse_image = image_resolver.clone();
    let render_image = image_resolver.clone();
    let embed_link_handler = link_handler.clone();
    let footnote_link_handler = link_handler.clone();
    view.selection_format(sel_format)
        .code_block_language(reader_code_language::resolver())
        .code_block_actions(reader_code::actions)
        .markdown_block_parser(move |node, cx| {
            let markdown_ast::Node::Paragraph(p) = node else {
                return None;
            };
            let mut url = None;
            let mut unavailable = false;
            let mut caption = String::new();
            let mut drawing_size = None;
            for child in &p.children {
                match child {
                    markdown_ast::Node::Image(image) if url.is_none() => {
                        drawing_size = image.title.as_deref().and_then(reader_drawing::parse_size);
                        if let Some(image_source) = parse_image(&image.url) {
                            unavailable = matches!(image_source, MarkdownImage::Unavailable);
                            url = Some(image.url.clone());
                        }
                    }
                    other => caption.push_str(cx.node_source(other)?),
                }
            }
            let url = url?;
            let node = MarkdownNode::new(
                "local-image",
                LocalImage {
                    url,
                    caption: caption.trim().to_string(),
                    drawing_size,
                },
            );
            Some(if unavailable {
                node.plain_part("status", "Image unavailable")
            } else {
                node.markdown_part("caption", caption.trim().to_string())
            })
        })
        .markdown_block_renderer("local-image", move |node, _window, cx| {
            match node.data::<LocalImage>() {
                Some(data) => {
                    match render_image(&data.url) {
                        Some(MarkdownImage::Drawing(root, path)) => {
                            return reader_drawing::render(
                                &root,
                                &path,
                                data.drawing_size,
                                _window,
                                cx,
                            )
                        }
                        Some(MarkdownImage::DrawingUnavailable(payload)) => {
                            return reader_drawing::unavailable(&payload, cx);
                        }
                        _ => {}
                    }
                    let Some(MarkdownImage::Source(image)) = render_image(&data.url) else {
                        return node.render_part("status", |style| style, _window, cx);
                    };
                    let mut el = v_flex()
                        .py_2()
                        .gap_1()
                        .child(reader_image::ReaderImage::new(image));
                    if !data.caption.is_empty() {
                        el = el.child(node.render_part("caption", |style| style, _window, cx));
                    }
                    el.into_any_element()
                }
                None => div().into_any_element(),
            }
        })
        .markdown_block_parser(parse_callout)
        .markdown_block_renderer("callout", move |node, _window, cx| {
            let Some(data) = node.data::<Callout>() else {
                return div().into_any_element();
            };
            let theme = cx.theme();
            let (accent, icon) = callout_look(&data.header, theme);
            // `[!type]-` starts closed and `[!type]+` open; the title row
            // toggles either (#651). Without a sign there is nothing to fold.
            let (key, fold, offset) = (data.key, data.header.fold, data.offset);
            let foldable = fold != callout::Fold::None;
            let open = !foldable || reader_obsidian::callout_open(key, fold, cx);
            let mut el = v_flex()
                .my_1()
                .px_3()
                .py_2()
                .gap_1()
                .rounded_md()
                .border_1()
                .border_color(accent.opacity(0.45))
                .border_l(px(3.))
                .bg(accent.opacity(0.08))
                .child(
                    h_flex()
                        .id(("reader-callout-title", key))
                        .gap_2()
                        .items_center()
                        .text_color(accent)
                        .font_weight(FontWeight::BOLD)
                        .child(Icon::new(icon).small().text_color(accent))
                        .child(node.render_part("title", |style| style, _window, cx))
                        .when(foldable, |el| {
                            el.cursor_pointer()
                                .debug_selector(move || format!("reader-callout-fold-{offset}"))
                                .child(
                                    Icon::new(if open {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .small()
                                    .text_color(accent),
                                )
                                .on_click(move |_, window, cx| {
                                    cx.stop_propagation();
                                    reader_obsidian::toggle_callout(key, fold, window, cx);
                                })
                        }),
                );
            if open && !data.body.is_empty() {
                let font_size = reader_ui_state::font_size(cx);
                el = el.child(
                    div()
                        .debug_selector(move || format!("reader-callout-body-{offset}"))
                        .child(node.render_part(
                            "body",
                            |style| style.with_heading_base_font_size(px(font_size)),
                            _window,
                            cx,
                        )),
                );
            }
            el.into_any_element()
        })
        .markdown_block_parser(parse_embed)
        .markdown_block_renderer("embed", move |node, _window, cx| {
            let Some(data) = node.data::<Embed>() else {
                return div().into_any_element();
            };
            let theme = cx.theme();
            let border = theme.border;
            let muted = theme.muted_foreground;
            let fg = theme.foreground;
            if data.missing {
                return div()
                    .my_1()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .text_sm()
                    .italic()
                    .text_color(muted)
                    .child(node.render_part("title", |style| style, _window, cx))
                    .into_any_element();
            }
            let font_size = reader_ui_state::font_size(cx);
            let embed_link = embed_link_handler.clone();
            let open_url = match &data.heading {
                Some(h) => format!("{WIKI_SCHEME}{}#{h}", data.path),
                None => format!("{WIKI_SCHEME}{}", data.path),
            };
            v_flex()
                .my_1()
                .rounded_md()
                .border_1()
                .border_color(border)
                .child(
                    h_flex()
                        .id(("embed-title", data.offset as u64))
                        .px_3()
                        .py_1()
                        .gap_2()
                        .items_center()
                        .bg(theme.accent.opacity(0.5))
                        .text_sm()
                        .text_color(muted)
                        .cursor_pointer()
                        .hover(|s| s.text_color(fg).bg(theme.accent))
                        .child(Icon::new(IconName::FileText).small().text_color(muted))
                        .child(node.render_part("title", |style| style, _window, cx))
                        .on_click(move |event, window, cx| {
                            embed_link(&open_url, event, window, cx)
                        }),
                )
                .child(div().px_3().py_2().child(node.render_part(
                    "body",
                    |style| style.with_heading_base_font_size(px(font_size)),
                    _window,
                    cx,
                )))
                .into_any_element()
        })
        .markdown_block_parser(parse_highlight)
        .markdown_block_renderer("highlight", move |node, _window, cx| {
            node.render_part("body", |style| style, _window, cx)
        })
        .markdown_block_parser(reader_obsidian::parse_footnote)
        .markdown_block_renderer("footnote", move |node, window, cx| {
            reader_obsidian::render_footnote(node, &footnote_link_handler, window, cx)
        })
        .markdown_block_parser(reader_obsidian::parse_math)
        .markdown_block_renderer("math", reader_obsidian::render_math)
        .markdown_block_parser(reader_obsidian::parse_block_marker)
        .markdown_block_renderer("block-marker", |_, _, _| div())
        .on_link_click(move |url, ev, window, cx| link_click(url, ev, window, cx))
}

struct Reader {
    ui_state: reader_ui_state::Session,
    single_file: bool,
    hover_preview: reader_hover::HoverPreview,
    file_preview: Option<reader_files::FilePreview>,
    file_menu: Option<(Entity<gpui_component::menu::PopupMenu>, Point<Pixels>)>,
    editing: Option<reader_editor::Editing>,
    #[cfg(unix)]
    creation: Option<reader_create::Creation>,
    #[cfg(unix)]
    creation_undo: Option<Arc<reader_create::CreatedUndo>>,
    #[cfg(unix)]
    renaming: Option<reader_move::Renaming>,
    #[cfg(unix)]
    move_picker: reader_move_picker::PickerState,
    #[cfg(unix)]
    note_move_pending: bool,
    #[cfg(unix)]
    move_notice_generation: u64,
    #[cfg(unix)]
    move_applying: bool,
    #[cfg(unix)]
    trash_pending: bool,
    #[cfg(unix)]
    trash_undo: reader_trash::UndoHistory,
    #[cfg(unix)]
    move_index: Option<Arc<tessera_core::link_rewrite::CandidateIndex>>,
    recovery_offer: bool,
    recovery_checked: bool,
    recovery_error: bool,
    recovery_dismissed: bool,
    recovery_startup: bool,
    loading: Option<reader_loading::Loading>,
    pending_open_document: Option<reader_loading::PendingDocument>,
    queued_open_note: Option<String>,
    shared_session: Option<reader_session::Shared>,
    shared_version: u64,
    usable_document: bool,
    session_directory: Option<PathBuf>,
    session_records: Option<async_channel::Sender<reader_loading::SessionRecord>>,
    last_recorded_document: Option<(PathBuf, String, u64)>,
    index_dir: Option<PathBuf>,
    cache_lease: Option<Arc<reader_cache::Lease>>,
    vault: Arc<Vault>,
    searcher: Option<Arc<Searcher>>,
    vault_root: PathBuf,
    /// The watcher lives here so it is dropped with the reader; the poll
    /// loop only borrows it through the entity.
    watcher: Option<VaultWatcher>,
    watcher_generation: u64,
    watcher_poll_active: bool,
    #[cfg(test)]
    watcher_poll_hold: Option<async_channel::Receiver<()>>,
    deferred_vault_changes: tessera_core::Changes,
    incremental_state: Option<tessera_core::vault::warm::incremental::State>,
    tasks_index: Option<Arc<tessera_core::tasks::Index>>,
    incremental_initializing: bool,
    incremental_active: bool,
    incremental_epoch: u64,
    incremental_cancel: Option<reader_loading::Cancellation>,
    #[cfg(test)]
    incremental_hold: Option<async_channel::Receiver<()>>,
    reader_window: AnyWindowHandle,
    sidebar_search_focus: FocusHandle,
    quick_open: quick_open::Palette,
    content: Entity<TextViewState>,
    _content_sub: Subscription,
    current_rel: String,
    current_title: String,
    link_notice: Option<reader_toast::Notice>,
    displayed_notice: Option<reader_toast::Notice>,
    displayed_choices: Vec<(String, Option<String>)>,
    displayed_recovery: Option<(String, u64)>,
    displayed_history_notice: Option<(uuid::Uuid, uuid::Uuid, String)>,
    history_notice_generation: u64,
    notice_generation: u64,
    toast_subscription: Option<Subscription>,
    link_choices: Vec<(String, Option<String>)>,
    navigation_generation: u64,
    pending_landing: Option<ListOffset>,
    landing_generation: u64,
    prepared_links: prepared_links::States,
    /// Last verified appearance for this exact displayed source. Pending action
    /// evidence is cleared separately during same-document verification.
    link_presentations: prepared_links::States,
    link_preparation_generation: u64,
    document_preparation_generation: u64,
    link_original_source: Option<String>,
    link_identities: Vec<tessera_core::document_links::prepared::LinkIdentity>,
    history_positions: Vec<ListOffset>,
    backlinks: Vec<Backlink>,
    use_html: bool,
    sel_format: SelectionFormat,
    /// Focus of the reader root: what receives the key bindings when no
    /// input has focus. Clicking the text hands focus to the TextView, which
    /// sits under the same key context, so the bindings keep working.
    focus_handle: FocusHandle,
    /// Source of the open note as rendered, before any find marks. Kept so
    /// the find bar can re-mark it on every keystroke and restore it on Esc.
    note_source: String,
    find_input: Entity<InputState>,
    find_open: bool,
    /// Top-level headings of the open document (docs/design/reader.md §Right
    /// panel, #337). Prepared with the document; never reparsed on scroll.
    outline: Vec<tessera_core::document_links::HeadingEntry>,
    /// Folder tree over the published inventory (#335). Rebuilt only when the
    /// vault Arc or the current document changes.
    tree: reader_tree::Tree,
    tree_source: Option<Arc<Vault>>,
    tree_revealed: String,
    tree_focus: FocusHandle,
    tree_scroll: UniformListScrollHandle,
    section_scroll: [UniformListScrollHandle; 4],
    scroll_sections: reader_sidebar::ScrollSections,
    /// Recent / Pinned / collapsed sections for this root (#369), app state.
    sidebar: reader_sidebar::State,
    sidebar_path: Option<PathBuf>,
    inbox: Vec<reader_sidebar::InboxItem>,
    projects: Arc<tessera_core::projects::Index>,
    projects_done_expanded: bool,
    inbox_source: Option<Arc<Vault>>,
    recent_expanded: bool,
    sidebar_save_sequence: Arc<std::sync::atomic::AtomicU64>,
    /// Root whose first Inbox result arrived; the tree re-reveals once then.
    inbox_ready_root: Option<PathBuf>,
    /// Read-only properties of the open note (#386); `Err` carries a YAML
    /// error to show instead of silently hiding the block.
    properties: Result<Vec<tessera_core::properties::Property>, String>,
    /// A wide table shown at window width over the document (#368).
    table_overlay: Option<Entity<TextViewState>>,
    timeline: Option<reader_timeline::Timeline>,
    /// Inline properties expanded above the document (right panel closed).
    properties_open: bool,
    show_hidden_properties: bool,
    /// Linking notes whose places are all shown (#394, «Show N more»).
    backlinks_expanded: std::collections::HashSet<String>,
    /// Display titles of notes (#381), read from their first bytes. Shared
    /// with the quick-open worker, which matches and labels by them (#645).
    backlink_titles: Arc<std::collections::HashMap<String, String>>,
    panels: reader_layout::Panels,
    embedded_in_workspace: bool,
    panel_widths: reader_layout::Widths,
    panel_settings: Option<PathBuf>,
    panel_preferences_ready: bool,
    panel_widths_revision: u64,
    body_bounds: Bounds<Pixels>,
    // The viewport associated with body_bounds, so resize rendering never uses
    // the previous frame's dock/overlay width for the new viewport.
    body_viewport_width: Pixels,
    resizing_panel: Option<reader_layout::Panel>,
    /// Notes opened, oldest first, and the position in it. Every `open_note`
    /// that is not itself a history move pushes onto it (#48).
    history: Vec<String>,
    history_ix: usize,
    history_nav: Option<usize>,
    _subs: Vec<Subscription>,
}

#[derive(Debug, PartialEq, Eq)]
enum ReaderLanding {
    Cancelled,
    Waiting,
    Failed,
    Ready,
}
fn reader_landing_state(
    current: bool,
    same_content: bool,
    count: usize,
    target: usize,
    attempt: usize,
) -> ReaderLanding {
    if !current || !same_content {
        ReaderLanding::Cancelled
    } else if target < count {
        ReaderLanding::Ready
    } else if attempt >= 99 {
        ReaderLanding::Failed
    } else {
        ReaderLanding::Waiting
    }
}

impl Reader {
    /// Share the enclosing Workspace chrome without changing document state.
    pub(crate) fn embedded_in_workspace(mut self) -> Self {
        self.embedded_in_workspace = true;
        self
    }

    fn new(mut opts: Opts, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let _constructor_phase = reader_diagnostics::phase(cx, "reader_constructor");
        if opts.diagnostics.is_none() {
            opts.diagnostics = reader_diagnostics::trace(cx);
        }
        if let (Some(trace), Some(root)) = (&opts.diagnostics, &opts.vault) {
            opts.diagnostics = Some(trace.for_root(root.clone()));
        }
        if opts.session_directory.is_none() {
            #[cfg(not(test))]
            {
                opts.session_directory = reader_history::state_directory().ok();
            }
            #[cfg(test)]
            {
                opts.session_directory = cx
                    .try_global::<reader_history::TestSessionDirectory>()
                    .map(|d| d.0.clone());
            }
        }
        let use_html = opts.use_html;
        let vault_root = opts.vault.clone().unwrap_or_default();
        let index_dir = opts.index_dir.clone();
        let mut placeholder = Vault::from_note_paths([]);
        placeholder.root = vault_root.clone();
        let vault = Arc::new(placeholder);
        let searcher = None;
        let quick_open = quick_open::Palette::new(window, cx);
        let find_input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in note…"));
        let sel_format = if opts.copy_source {
            SelectionFormat::Source
        } else {
            SelectionFormat::Plain
        };
        let content = cx.new(|cx| {
            if use_html {
                TextViewState::html("", cx)
                    .retain_selection_on_layout(true)
                    .scrollable(true)
                    .selectable(true)
            } else {
                TextViewState::markdown("", cx)
                    .retain_selection_on_layout(true)
                    .scrollable(true)
                    .selectable(true)
                    .selection_format(sel_format)
            }
        });
        // Enter on a single-line input propagates the key and emits
        // PressEnter, so cycling the matches hangs off the event rather than
        // off a binding that would have to outrank the input's own.
        let find_sub = cx.subscribe_in(
            &find_input,
            window,
            |this: &mut Reader, _, e: &InputEvent, window, cx| match e {
                InputEvent::Change => this.run_find(window, cx),
                InputEvent::PressEnter { shift: true, .. } => this.find_step(-1, cx),
                InputEvent::PressEnter { .. } => this.find_step(1, cx),
                _ => {}
            },
        );

        let content_sub = cx.observe(&content, |_, _, cx| cx.notify());

        let mut this = Self {
            ui_state: Default::default(),
            loading: None,
            pending_open_document: None,
            queued_open_note: None,
            shared_session: None,
            shared_version: 0,
            usable_document: false,
            session_directory: opts.session_directory.clone(),
            session_records: None,
            last_recorded_document: None,
            index_dir,
            cache_lease: None,
            vault,
            searcher,
            vault_root: vault_root.clone(),
            watcher: None,
            watcher_generation: 0,
            watcher_poll_active: false,
            #[cfg(test)]
            watcher_poll_hold: None,
            deferred_vault_changes: Default::default(),
            incremental_state: None,
            tasks_index: None,
            incremental_initializing: false,
            incremental_active: false,
            incremental_epoch: 0,
            incremental_cancel: None,
            #[cfg(test)]
            incremental_hold: None,
            single_file: false,
            reader_window: window.window_handle(),
            sidebar_search_focus: cx.focus_handle(),
            quick_open,
            content,
            editing: None,
            #[cfg(unix)]
            creation: None,
            #[cfg(unix)]
            creation_undo: None,
            #[cfg(unix)]
            renaming: None,
            #[cfg(unix)]
            move_picker: Default::default(),
            #[cfg(unix)]
            note_move_pending: false,
            #[cfg(unix)]
            move_notice_generation: 0,
            #[cfg(unix)]
            move_applying: false,
            #[cfg(unix)]
            trash_pending: false,
            #[cfg(unix)]
            trash_undo: reader_trash::UndoHistory::default(),
            #[cfg(unix)]
            move_index: None,
            recovery_offer: false,
            recovery_checked: false,
            recovery_error: false,
            recovery_dismissed: false,
            recovery_startup: cx
                .try_global::<reader_recovery::RecoveryStartup>()
                .is_some_and(|state| state.0),
            current_rel: String::new(),
            current_title: String::new(),
            link_notice: None,
            displayed_notice: None,
            displayed_choices: Vec::new(),
            displayed_recovery: None,
            displayed_history_notice: None,
            history_notice_generation: 0,
            notice_generation: 0,
            toast_subscription: None,
            link_choices: Vec::new(),
            navigation_generation: 0,
            pending_landing: None,
            landing_generation: 0,
            prepared_links: Arc::default(),
            link_presentations: Arc::default(),
            link_preparation_generation: 0,
            document_preparation_generation: 0,
            link_original_source: None,
            link_identities: Vec::new(),
            history_positions: Vec::new(),
            backlinks: Vec::new(),
            use_html,
            sel_format,
            focus_handle: cx.focus_handle(),
            note_source: String::new(),
            find_input,
            find_open: false,
            outline: Vec::new(),
            tree: reader_tree::Tree::default(),
            tree_source: None,
            tree_revealed: String::new(),
            hover_preview: Default::default(),
            file_preview: None,
            file_menu: None,
            tree_focus: cx.focus_handle(),
            tree_scroll: UniformListScrollHandle::new(),
            section_scroll: std::array::from_fn(|_| UniformListScrollHandle::new()),
            scroll_sections: Default::default(),
            sidebar: reader_sidebar::State::default(),
            sidebar_path: None,
            inbox: Vec::new(),
            projects: Arc::default(),
            projects_done_expanded: false,
            inbox_source: None,
            recent_expanded: false,
            sidebar_save_sequence: Arc::default(),
            inbox_ready_root: None,
            backlink_titles: Arc::default(),
            backlinks_expanded: std::collections::HashSet::new(),
            properties: Ok(Vec::new()),
            table_overlay: None,
            timeline: None,
            properties_open: false,
            show_hidden_properties: false,
            // docs/design/reader.md: from R2 the sidebar starts open where it
            // docks; compact windows still start with every panel closed.
            // Tests keep the accepted closed default (#321).
            panels: reader_layout::Panels {
                notes: cfg!(not(test)) && !reader_recovery::is_recovering(cx),
                backlinks: cfg!(not(test)) && !reader_recovery::is_recovering(cx),
                ..Default::default()
            },
            embedded_in_workspace: false,
            panel_widths: reader_layout::Widths::default(),
            panel_settings: None,
            panel_preferences_ready: false,
            panel_widths_revision: 0,
            body_bounds: Bounds::default(),
            body_viewport_width: window.viewport_size().width,
            resizing_panel: None,
            history: Vec::new(),
            history_ix: 0,
            history_nav: None,
            _subs: vec![find_sub],
            _content_sub: content_sub,
        };
        // Nothing has focus on a fresh window, and gpui then dispatches keys
        // to the root node only, where the reader's context is not on the
        // stack. Take focus so the bindings are live from the first keystroke.
        window.focus(&this.focus_handle, cx);
        // Poll the watcher on a timer. notify delivers on its own thread; the
        // UI must not be touched from there, so events are drained here on the
        // main thread instead. 250 ms is below the watcher's own quiet window,
        // so it never adds latency of its own.
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let alive = this
                .update_in(cx, |this, window, cx| this.poll_vault(window, cx))
                .is_ok();
            if !alive {
                break;
            }
        })
        .detach();

        if let Some(query) = &opts.query {
            this.open_quick_open(true, window, cx);
            this.quick_open
                .input
                .update(cx, |input, cx| input.set_value(query.clone(), window, cx));
            this.refresh_quick_open(cx);
        }
        cx.observe_self(|this, cx| this.record_ui_state(this.ui_state.was_active(), cx))
            .detach();
        cx.observe_window_activation(window, |this, window, cx| {
            this.record_ui_state(window.is_window_active(), cx);
        })
        .detach();
        this.install_source_lifecycle(window, cx);
        this.start_session_records(cx);
        if !opts.defer_loading {
            this.start_loading(opts, window, cx);
        }
        this
    }

    /// Drain/coalesce notifications off the UI thread. Known note batches use
    /// the reconciled baseline, including directory hints. Overflow needs reconcile.
    fn poll_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shared_session.is_some() {
            if !self.loading.as_ref().is_some_and(|load| load.active) {
                self.poll_shared_session(window, cx);
            }
            return;
        }
        if self.loading.as_ref().is_some_and(|l| l.active)
            || self.incremental_active
            || self.incremental_initializing
        {
            return;
        }
        if !self.deferred_vault_changes.is_empty() {
            let changes = std::mem::take(&mut self.deferred_vault_changes);
            self.apply_vault_changes(changes, window, cx);
            return;
        }
        let Some(mut watcher) = self.watcher.take() else {
            return;
        };
        #[cfg(test)]
        let hold = self.watcher_poll_hold.take();
        self.watcher_poll_active = true;
        let generation = self.watcher_generation;
        let root = self.vault_root.clone();
        cx.spawn_in(window, async move |this, cx| {
            let (watcher, changes) = cx
                .background_executor()
                .spawn(async move {
                    #[cfg(test)]
                    if let Some(hold) = hold {
                        let _ = hold.recv().await;
                    }
                    let changes = watcher.poll();
                    (watcher, changes)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.vault_root != root
                    || this.watcher_generation != generation
                    || this.watcher.is_some()
                {
                    return;
                }
                this.watcher_poll_active = false;
                this.watcher = Some(watcher);
                if let Some(changes) = changes {
                    if this.loading.as_ref().is_some_and(|load| load.active) {
                        this.deferred_vault_changes.rescan |= changes.rescan;
                        this.deferred_vault_changes
                            .directories
                            .extend(changes.directories);
                        this.deferred_vault_changes.changed.extend(changes.changed);
                        this.deferred_vault_changes.removed.extend(changes.removed);
                    } else {
                        this.apply_vault_changes(changes, window, cx);
                    }
                }
            });
        })
        .detach();
    }

    fn apply_vault_changes(
        &mut self,
        changes: tessera_core::Changes,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.shared_session.is_some() {
            self.deferred_vault_changes.rescan |= changes.rescan;
            self.deferred_vault_changes.changed.extend(changes.changed);
            self.deferred_vault_changes.removed.extend(changes.removed);
            self.deferred_vault_changes
                .directories
                .extend(changes.directories);
            self.poll_vault(window, cx);
            return;
        }
        reader_drawing::invalidate(&self.vault_root, cx);
        if let Some(viewer) = self.pdf_viewer().cloned() {
            viewer.update(cx, |viewer, cx| viewer.check_revision(cx));
        }
        if self.incremental_active || self.incremental_initializing {
            self.deferred_vault_changes.rescan |= changes.rescan;
            self.deferred_vault_changes
                .directories
                .extend(changes.directories);
            self.deferred_vault_changes.changed.extend(changes.changed);
            self.deferred_vault_changes.removed.extend(changes.removed);
            return;
        }
        if !changes.is_empty()
            && !changes.rescan
            && self.incremental_state.is_some()
            && self.searcher.is_some()
        {
            self.start_incremental(changes, window, cx);
        } else {
            self.refresh_inventory(changes, window, cx);
        }
    }

    fn open_note(
        &mut self,
        rel: &str,
        jump_term: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_note_at(rel, jump_term, None, window, cx);
    }

    /// `open_note`, scrolled to the heading `[[note#Heading]]` names (#49).
    /// A heading that is not in the note opens at the top, like Obsidian.
    fn open_note_at(
        &mut self,
        rel: &str,
        jump_term: Option<&str>,
        heading: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if rel.is_empty() {
            self.show_empty_vault(window, cx);
            return;
        }
        if tessera_core::excalidraw::is_drawing(rel) || !rel.to_lowercase().ends_with(".md") {
            self.preview_file(rel, window, cx);
            return;
        }
        self.clear_hover(cx);
        self.file_preview = None;
        self.prepare_document(rel, jump_term, heading, window, cx);
    }

    fn accept_prepared_document(
        &mut self,
        request: prepared_links::DocumentRequest,
        document: anyhow::Result<prepared_links::PreparedDocument>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.accept_prepared_document_using(request, document, None, window, cx);
    }

    fn accept_prepared_document_using(
        &mut self,
        request: prepared_links::DocumentRequest,
        document: anyhow::Result<prepared_links::PreparedDocument>,
        prepared_content: Option<Entity<TextViewState>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing.is_some() {
            return;
        }
        self.clear_hover(cx);
        let prepared_links::PreparedDocument {
            source,
            original: original_body,
            identities,
            frontmatter,
        } = match document {
            Ok(document) => document,
            Err(error) => {
                self.link_notice = Some(format!("Document unavailable: {error:#}").into());
                cx.notify();
                return;
            }
        };
        let rel = request.rel.as_str();
        let jump_term = request.jump.as_deref();
        let heading = request.heading.as_deref();
        let heading_ix = if let Some(target) = heading {
            let result = if self.use_html {
                Err("Heading navigation is unavailable in HTML mode.")
            } else {
                tessera_core::document_links::heading(&source, target).map(|h| h.block)
            };
            match result {
                Ok(ix) => Some(ix),
                Err(reason) => {
                    self.link_notice = Some(reason.into());
                    cx.notify();
                    return;
                }
            }
        } else {
            None
        };
        self.link_notice = None;
        self.link_choices.clear();
        self.cancel_pending_landing();
        self.navigation_generation = self.navigation_generation.wrapping_add(1);
        if request.history_index.is_none() {
            if let Some(position) = self.history_positions.get_mut(self.history_ix) {
                *position = self.content.read(cx).list_state().logical_scroll_top();
            }
        }
        // Focus inside the table overlay returns to the document when a link
        // in the overlay navigates (#368).
        let content_had_focus = self.content.read(cx).focus_handle().is_focused(window)
            || self.table_overlay.as_ref().is_some_and(|overlay| {
                overlay.read(cx).focus_handle().contains_focused(window, cx)
            });
        // A new state owns this document's parse/list. An old document's list
        // can never satisfy heading/Back readiness while replacement parses.
        self.content = prepared_content.unwrap_or_else(|| {
            cx.new(|cx| {
                let state = if self.use_html {
                    TextViewState::html("", cx)
                } else {
                    TextViewState::markdown("", cx)
                };
                state
                    .scrollable(true)
                    .selectable(true)
                    .selection_format(self.sel_format)
                    .retain_selection_on_layout(true)
            })
        });
        self._content_sub = cx.observe(&self.content, |this, _, cx| {
            this.record_usable_document(cx);
            cx.notify();
        });
        // Transfer focus only at replacement; asynchronous landing never steals it.
        if content_had_focus {
            window.focus(&self.content.read(cx).focus_handle().clone(), cx);
        }
        self.invalidate_links();
        self.link_presentations = Arc::default();
        self.link_original_source = original_body.clone();
        self.link_identities = identities;
        let configured = reader_plugins(
            self.vault_root.clone(),
            TextView::new(&self.content),
            cx.entity().downgrade(),
            self.sel_format,
            self.link_presentations.clone(),
            &self.link_identities,
        );
        self.content.update(cx, |s, cx| {
            configured.prepare_state(s, cx);
            s.set_search_query("", cx);
            s.set_text_with_source(&source, original_body.map(SharedString::from), cx);
            if let Some(term) = jump_term {
                s.set_search_query(term, cx);
            }
        });
        self.outline = if self.use_html {
            Vec::new()
        } else {
            tessera_core::document_links::HeadingInventory::new(&source)
                .entries
                .into_iter()
                .filter(|heading| heading.supported_container)
                .collect()
        };
        self.properties = frontmatter
            .as_deref()
            .map(tessera_core::properties::parse)
            .unwrap_or_else(|| Ok(Vec::new()));
        self.note_source = source;
        if request.history_index.is_none() {
            self.quick_open.remember(rel);
        }
        if self.current_rel != rel {
            self.timeline = None;
        }
        self.current_rel = rel.to_string();
        self.file_preview = if tessera_core::excalidraw::is_drawing(rel) {
            reader_files::FilePreview::load(&self.vault_root, rel).ok()
        } else {
            None
        };
        self.table_overlay = None;
        self.discover_source_recovery(cx);
        self.note_opened(cx);
        self.usable_document = true;
        self.record_usable_document(cx);
        self.refresh_link_preparation(cx);
        self.current_title = self.note_label(rel);
        self.backlinks = self.vault.backlinks(rel);
        window.set_window_title(&format!("Tessera — {}", self.current_title));
        if request.history_index.is_none()
            && (heading.is_some()
                || self.history.get(self.history_ix).map(String::as_str) != Some(rel))
        {
            // A new branch: forward entries are dropped, like a browser.
            if !self.history.is_empty() {
                self.history.truncate(self.history_ix + 1);
                self.history_positions.truncate(self.history_ix + 1);
            }
            self.history.push(rel.to_string());
            self.history_positions.push(ListOffset {
                item_ix: 0,
                offset_in_item: px(0.),
            });
            self.history_ix = self.history.len() - 1;
        }
        if self.find_open {
            self.run_find(window, cx);
        }

        // Scroll to top now; if jumping, scroll again once the new text has
        // been parsed and the block list rebuilt (deferred, best-effort).
        self.content.read(cx).list_state().scroll_to_reveal_item(0);
        // The reader's list has one item per top-level block, in source
        // order, so the heading's block index in the rendered source is its
        // item index. Plugins replace a block, never split or merge one.
        if let Some(ix) = heading_ix {
            self.scroll_to_block(ix, cx);
        }
        if let Some(index) = request.history_index {
            self.history_ix = index;
        }
        if let Some(position) = request.restore_position {
            self.scroll_to_position(position, cx);
        }
        if self.shared_session.is_some() {
            self.restore_ui_state(window, cx);
        }
        cx.notify();
    }

    // --- find in note (#48) ---

    fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_preview.is_some() {
            return;
        }
        self.quick_open.open = false;
        self.quick_open.invalidate();
        if self.editing.is_some() {
            self.open_source_find(cx);
            return;
        }
        self.resizing_panel = None;
        // Docked panels stay while finding; a compact overlay would cover
        // the matches, so it closes.
        let available = f32::from(self.body_bounds.size.width);
        if reader_layout::overlay(available) {
            let active = self.panels.dismiss_target(available);
            self.panels.close(active);
        }
        self.find_open = true;
        self.find_input.update(cx, |s, cx| s.focus(window, cx));
        self.run_find(window, cx);
    }

    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find_open = false;
        self.content.update(cx, |s, cx| s.set_search_query("", cx));
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Recompute the matches for the find term and re-mark the note.
    fn run_find(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let term = self.find_input.read(cx).value().trim().to_string();
        let sensitive = reader_ui_state::find_case_sensitive(cx);
        self.content.update(cx, |s, cx| {
            s.set_search_case_sensitive(sensitive, cx);
            s.set_search_query(term, cx);
        });
        cx.notify();
    }

    fn find_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.content
            .update(cx, |s, cx| s.step_search_match(delta, cx));
        cx.notify();
    }

    /// Scroll the reader so top-level block `ix` is at the top of the view,
    /// once the new text has been parsed (deferred, best-effort). Used for
    /// heading targets, where "at the top" is what a reader expects (#49).
    fn scroll_to_block(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.scroll_to_position(
            ListOffset {
                item_ix: ix,
                offset_in_item: px(0.),
            },
            cx,
        );
    }
    fn cancel_pending_landing(&mut self) {
        self.pending_landing = None;
        self.landing_generation = self.landing_generation.wrapping_add(1);
    }

    fn scroll_to_position(&mut self, position: ListOffset, cx: &mut Context<Self>) {
        self.cancel_pending_landing();
        self.pending_landing = Some(position);
        let landing = self.landing_generation;
        let generation = self.navigation_generation;
        let content = self.content.entity_id();
        cx.spawn(async move |entity, cx| {
            for attempt in 0..100 {
                cx.background_executor().timer(Duration::from_millis(50)).await;
                let done = entity.update(cx, |this, cx| {
                    if landing != this.landing_generation { return true; }
                    match reader_landing_state(generation == this.navigation_generation,
                        content == this.content.entity_id(), this.content.read(cx).list_state().item_count(), position.item_ix, attempt)
                    {
                        ReaderLanding::Cancelled => { this.pending_landing = None; true },
                        ReaderLanding::Waiting => { cx.notify(); false },
                        ReaderLanding::Failed => {
                            this.pending_landing = None;
                            this.link_notice = Some("The document did not finish rendering at the requested position. Heading/Back landing failed; try again.".into());
                            cx.notify(); true
                        },
                        ReaderLanding::Ready => {
                            this.pending_landing = None;
                            this.content.update(cx, |s, cx| { s.list_state().scroll_to(position); cx.notify(); });
                            true
                        }
                    }
                }).unwrap_or(true);
                if done { break; }
            }
        }).detach();
    }

    // --- keyboard actions (#48) ---

    /// Escape clears or dismisses local transient UI; it never toggles panels (#483).
    /// A non-empty focused search field is cleared and keeps focus; an empty one closes.
    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(unix)]
        if self.renaming.is_some() {
            self.cancel_rename(window, cx);
            return;
        }
        #[cfg(unix)]
        if self.creation.is_some() {
            self.cancel_creation(window, cx);
            return;
        }
        if self.hover_preview.is_active() {
            self.clear_hover(cx);
            return;
        }
        if self.quick_open.open {
            if self.quick_open.input.read(cx).value().is_empty() {
                self.close_quick_open(window, cx);
            } else {
                self.quick_open.input.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                    input.focus(window, cx);
                });
                self.refresh_quick_open(cx);
            }
            return;
        }
        if self.table_overlay.is_some() {
            self.close_table_overlay(window, cx);
            return;
        }
        if self.active_timeline().is_some_and(|t| t.selected.is_some()) {
            self.back_from_timeline(window, cx);
            return;
        }
        if self.loading.as_ref().is_some_and(|l| l.active) {
            self.cancel_loading(cx);
            return;
        }
        let find_focused = self.find_input.read(cx).focus_handle(cx).is_focused(window);
        if self.find_open && find_focused {
            if self.find_input.read(cx).value().is_empty() {
                self.close_find(window, cx);
            } else {
                self.find_input
                    .update(cx, |input, cx| input.set_value("", window, cx));
                self.run_find(window, cx);
            }
            return;
        }
        #[cfg(unix)]
        if self.dismiss_trash_toast(window, cx) {
            return;
        }
        if reader_toast::dismiss(window, cx) {
            return;
        }
        if self.find_open {
            self.close_find(window, cx);
        }
    }

    fn focus_sidebar_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.resizing_panel = None;
        self.panels.open(reader_layout::Panel::Notes);
        cx.notify();
        self.sidebar_search_focus.focus(window, cx);
    }

    fn set_panel_width(&mut self, panel: reader_layout::Panel, width: f32) {
        self.ui_state.interacted = true;
        self.panel_widths_revision = self.panel_widths_revision.wrapping_add(1);
        self.panel_widths.set(panel, width);
    }

    fn persist_panel_width(&mut self, panel: reader_layout::Panel) {
        if self.ui_state.ready {
            return;
        }
        if let Some(path) = &self.panel_settings {
            if let Err(error) = self.panel_widths.save_panel(panel, path) {
                self.link_notice = Some(format!("Could not save panel width: {error}").into());
            }
        }
    }

    fn close_panel(
        &mut self,
        panel: reader_layout::Panel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resizing_panel = None;
        self.ui_state.interacted = true;
        self.panels.close(panel);
        let focus = self.content.read(cx).focus_handle().clone();
        focus.focus(window, cx);
        cx.notify();
    }

    fn toggle_panel(
        &mut self,
        panel: reader_layout::Panel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ui_state.interacted = true;
        self.resizing_panel = None;
        if self
            .panels
            .visible(panel, f32::from(self.body_bounds.size.width))
        {
            self.close_panel(panel, window, cx);
        } else {
            self.panels.open(panel);
            if panel == reader_layout::Panel::Notes {
                self.focus_sidebar_search(window, cx);
            } else {
                self.focus_handle.focus(window, cx);
                cx.notify();
            }
        }
    }

    fn select_panel_note(
        &mut self,
        panel: reader_layout::Panel,
        rel: &str,
        jump: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_note(rel, jump, window, cx);
        if panel == reader_layout::Panel::Notes {
            self.dismiss_sidebar_after_selection(window, cx);
        }
        let focus = self.content.read(cx).focus_handle().clone();
        focus.focus(window, cx);
        cx.notify();
    }

    /// Navigation dismisses a compact overlay without changing the wide layout.
    fn dismiss_sidebar_after_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let width = f32::from(self.body_bounds.size.width);
        if reader_layout::overlay(width) && self.panels.visible(reader_layout::Panel::Notes, width)
        {
            self.panels.active = reader_layout::Panel::Closed;
            self.resizing_panel = None;
            self.content
                .read(cx)
                .focus_handle()
                .clone()
                .focus(window, cx);
            cx.notify();
        }
    }

    fn scroll_reader_key(
        &mut self,
        direction: f32,
        page: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing.is_some()
            || self.file_preview.is_some()
            || !self.content.read(cx).focus_handle().is_focused(window)
        {
            cx.propagate();
            return;
        }
        let content = self.content.read(cx);
        let distance = if page {
            (f32::from(content.list_state().viewport_bounds().size.height) * 0.9).max(40.)
        } else {
            reader_ui_state::font_size(cx) * 2.
        };
        content.list_state().scroll_by(px(direction * distance));
        cx.notify();
    }

    fn scroll_tree_to(&self, ix: usize) {
        if self
            .sidebar
            .collapsed
            .contains(&reader_sidebar::Section::Folders)
        {
            return;
        }
        self.tree_scroll.scroll_to_item(ix, ScrollStrategy::Center);
    }

    fn save_sidebar(&self, cx: &mut Context<Self>) {
        let Some(path) = self.sidebar_path.clone() else {
            return;
        };
        let mut state = self.sidebar.clone();
        state.scroll_revealed = self.scroll_sections.revealed.clone();
        let latest = self.sidebar_save_sequence.clone();
        let sequence = latest.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        cx.background_executor()
            .spawn(async move {
                if let Err(error) = state.save_ordered(&path, sequence, &latest) {
                    eprintln!("Could not save sidebar state: {error:#}");
                }
            })
            .detach();
    }

    fn note_opened(&mut self, cx: &mut Context<Self>) {
        if self.current_rel.is_empty() {
            return;
        }
        if self.single_file
            && self.vault.inventory_scanned
            && !self
                .vault
                .notes
                .iter()
                .any(|note| note.path == self.current_rel)
        {
            let parent = Path::new(&self.current_rel)
                .parent()
                .unwrap_or(Path::new(""));
            self.load_quick_folder(tessera_core::vault::note_path(parent), cx);
        }
        self.load_sidebar_state();
        let rel = self.current_rel.clone();
        if self.sidebar.record_open(&rel, reader_sidebar::now()) {
            self.save_sidebar(cx);
        }
    }

    /// Switch sidebar state to the current root before anything records into
    /// it. Only the very first load keeps opens recorded before it.
    fn load_sidebar_state(&mut self) {
        let path = self
            .session_directory
            .as_ref()
            .map(|directory| reader_sidebar::State::path(directory, &self.vault_root));
        if path == self.sidebar_path {
            return;
        }
        let mut state = path
            .as_deref()
            .map(reader_sidebar::State::load)
            .unwrap_or_default();
        if self.sidebar_path.is_none() {
            for (rel, at) in self.sidebar.recent.iter().rev() {
                if !state.recent.iter().any(|(p, _)| p == rel) {
                    state.record_open(rel, *at);
                }
            }
        }
        self.scroll_sections = reader_sidebar::ScrollSections::from_state(&state);
        self.section_scroll = std::array::from_fn(|_| UniformListScrollHandle::new());
        self.sidebar = state;
        self.sidebar_path = path;
        self.inbox.clear();
        self.projects_done_expanded = false;
        self.inbox_source = None;
        self.recent_expanded = false;
        self.tree_revealed.clear();
    }

    /// Show hidden files (#395): per root, remembered with the sidebar.
    fn toggle_hidden_files(&mut self, cx: &mut Context<Self>) {
        self.sidebar.show_hidden = !self.sidebar.show_hidden;
        self.tree.set_show_hidden(self.sidebar.show_hidden);
        self.tree_revealed.clear();
        self.save_sidebar(cx);
        cx.notify();
    }

    fn toggle_pin(&mut self, path: &str, cx: &mut Context<Self>) {
        self.sidebar.toggle_pin(path);
        self.save_sidebar(cx);
        cx.notify();
    }

    fn toggle_section(&mut self, section: reader_sidebar::Section, cx: &mut Context<Self>) {
        if section != reader_sidebar::Section::Properties {
            self.sidebar.folders_only_restore = None;
        }
        if self
            .scroll_sections
            .closed(section, &self.sidebar.collapsed)
            && self.scroll_sections.compact
            && matches!(
                section,
                reader_sidebar::Section::Recent
                    | reader_sidebar::Section::Pinned
                    | reader_sidebar::Section::Inbox
            )
        {
            self.scroll_sections.reveal(section);
            // A manual expansion is a user preference; automatic folding is not.
            self.sidebar.collapsed.remove(&section);
        } else {
            self.scroll_sections.revealed.remove(&section);
            self.sidebar.toggle_section(section);
        }
        self.save_sidebar(cx);
        cx.notify();
    }

    /// Load this root's sidebar state and recompute Inbox when the published
    /// inventory changes. Birth times are read off the UI thread.
    fn sync_sidebar(&mut self, cx: &mut Context<Self>) {
        if self.single_file {
            return;
        }
        self.load_sidebar_state();
        if self.tree.show_hidden() != self.sidebar.show_hidden {
            self.tree.set_show_hidden(self.sidebar.show_hidden);
            self.tree_revealed.clear();
        }
        if !self.vault.inventory_complete
            || self
                .inbox_source
                .as_ref()
                .is_some_and(|source| Arc::ptr_eq(source, &self.vault))
        {
            return;
        }
        self.inbox_source = Some(self.vault.clone());
        let vault = self.vault.clone();
        let root = self.vault_root.clone();
        let mut first_seen = self.sidebar.first_seen.clone();
        let baseline = self.sidebar.baseline_taken;
        cx.spawn(async move |this, cx| {
            let compute_vault = vault.clone();
            let (items, first_seen) = cx
                .background_executor()
                .spawn(async move {
                    let items = reader_sidebar::compute_inbox(
                        &compute_vault,
                        &mut first_seen,
                        baseline,
                        reader_sidebar::now(),
                        |rel| reader_sidebar::birth_time(&root, rel),
                    );
                    (items, first_seen)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                // A newer inventory superseded this computation.
                if !this
                    .inbox_source
                    .as_ref()
                    .is_some_and(|source| Arc::ptr_eq(source, &vault))
                {
                    return;
                }
                this.inbox = items;
                if this.inbox_ready_root.as_ref() != Some(&this.vault_root) {
                    // Rows above the tree changed height; place the current
                    // note again once per root.
                    this.inbox_ready_root = Some(this.vault_root.clone());
                    this.tree_revealed.clear();
                }
                this.sidebar.first_seen = first_seen;
                this.sidebar.baseline_taken = true;
                this.sidebar.prune(&vault);
                this.save_sidebar(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// #381: the display title of a linking note (first H1, then frontmatter
    /// title, then file name) and, as secondary text, its folder path. Index
    /// notes (`_index`, `index`, `README`) take their folder's name.
    fn backlink_title(&self, path: &str) -> (String, Option<String>) {
        let (folder, file) = path.rsplit_once('/').unwrap_or(("", path));
        let stem = file.strip_suffix(".md").unwrap_or(file);
        let location = (!folder.is_empty()).then(|| folder.replace('/', " / "));
        let is_index = matches!(stem.to_lowercase().as_str(), "_index" | "index" | "readme");
        if is_index && !folder.is_empty() {
            let name = folder.rsplit('/').next().unwrap_or(folder).to_owned();
            return (name, location);
        }
        let title = self
            .backlink_titles
            .get(path)
            .cloned()
            .unwrap_or_else(|| stem.to_owned());
        (title, location)
    }

    /// Provisional labels use filenames. Ready supplies titles from sources
    /// already read by reconcile, never another round of cloud hydration.
    fn sync_backlink_titles(&mut self) {
        let missing: Vec<String> = self
            .backlinks
            .iter()
            .map(|b| b.path.clone())
            .filter(|p| !self.backlink_titles.contains_key(p))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if missing.is_empty() {
            return;
        }
        // Placeholders keep this from re-spawning while the read runs.
        let titles = Arc::make_mut(&mut self.backlink_titles);
        for path in &missing {
            let stem = path
                .rsplit('/')
                .next()
                .unwrap_or(path)
                .trim_end_matches(".md")
                .to_owned();
            titles.insert(path.clone(), stem);
        }
    }

    fn note_label(&self, path: &str) -> String {
        if let Some(title) = quick_open::drawing_title(path) {
            return title;
        }
        if path.ends_with(".md") {
            self.vault.note_title(path)
        } else {
            path.rsplit('/').next().unwrap_or(path).to_owned()
        }
    }

    /// The title shared by attachment breadcrumbs and the native window.
    fn selected_title(&self) -> String {
        if self.file_preview.is_some() {
            self.note_label(self.selected_file())
        } else {
            self.current_title.clone()
        }
    }

    /// Section rows; headers are mounted outside their content viewports (#434).
    fn sidebar_items(&self) -> Vec<SideItem> {
        use reader_sidebar::Section;
        if self.single_file {
            let mut items = vec![SideItem::Header(
                Section::Folders,
                reader_sidebar::section_count(self.vault.entries.len()),
            )];
            if !self
                .scroll_sections
                .closed(Section::Folders, &self.sidebar.collapsed)
            {
                items.extend(self.tree.rows.iter().cloned().map(SideItem::Tree));
            }
            return items;
        }
        let now = reader_sidebar::now();
        let open = |section| {
            if section == Section::Projects {
                return !self.sidebar.projects_collapsed;
            }
            !self
                .scroll_sections
                .closed(section, &self.sidebar.collapsed)
        };
        // Hidden notes follow Show hidden files here as in the tree (#635).
        let shown = |path: &str| !self.tree.hidden_by_preference(path);
        let recent: Vec<_> = self
            .sidebar
            .recent
            .iter()
            .filter(|(path, _)| shown(path))
            .collect();
        let pinned: Vec<_> = self.sidebar.pinned.iter().filter(|p| shown(p)).collect();
        let inbox: Vec<_> = self.inbox.iter().filter(|i| shown(&i.path)).collect();
        let mut items = Vec::new();
        items.push(SideItem::Header(
            Section::Recent,
            reader_sidebar::section_count(recent.len()),
        ));
        if open(Section::Recent) {
            let visible = if self.recent_expanded {
                recent.len()
            } else {
                recent.len().min(reader_sidebar::RECENT_SHOWN)
            };
            if recent.is_empty() {
                items.push(SideItem::Empty("Notes you open appear here."));
            }
            for (path, at) in &recent[..visible] {
                items.push(SideItem::Entry {
                    section: Section::Recent,
                    path: path.clone(),
                    label: self.note_label(path),
                    meta: Some(reader_sidebar::age_label(*at, now)),
                    location: None,
                    folder: false,
                });
            }
            if visible < recent.len() {
                items.push(SideItem::More(recent.len() - visible));
            }
        }
        items.push(SideItem::Header(
            Section::Pinned,
            reader_sidebar::section_count(pinned.len()),
        ));
        if open(Section::Pinned) {
            if pinned.is_empty() {
                items.push(SideItem::Empty("Hover a note or folder to pin it."));
            }
            for path in pinned {
                items.push(SideItem::Entry {
                    section: Section::Pinned,
                    path: path.clone(),
                    label: self.note_label(path),
                    meta: None,
                    location: None,
                    folder: !path.ends_with(".md"),
                });
            }
        }
        items.push(SideItem::Header(
            Section::Inbox,
            reader_sidebar::section_count(inbox.len()),
        ));
        if open(Section::Inbox) {
            if inbox.is_empty() {
                items.push(SideItem::Empty("No new unfiled notes."));
            }
            for item in inbox {
                let vault_name = self
                    .vault_root
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("Vault");
                items.push(SideItem::Entry {
                    section: Section::Inbox,
                    path: item.path.clone(),
                    label: quick_open::folder_note_title(vault_name, &item.path)
                        .unwrap_or_else(|| self.note_label(&item.path)),
                    meta: Some(reader_sidebar::age_label(item.created, now)),
                    location: Some(quick_open::result_location(vault_name, &item.path)),
                    folder: false,
                });
            }
        }
        items.push(SideItem::Header(Section::Projects, None));
        if open(Section::Projects) {
            use tessera_core::projects::Status;
            let rows = self.projects.rows();
            items.extend(
                rows.iter()
                    .filter(|p| p.status != Status::Done)
                    .cloned()
                    .map(SideItem::Project),
            );
            let done = rows.iter().filter(|p| p.status == Status::Done).count();
            if done > 0 {
                items.push(SideItem::ProjectsDone(done, self.projects_done_expanded));
                if self.projects_done_expanded {
                    items.extend(
                        rows.iter()
                            .filter(|p| p.status == Status::Done)
                            .cloned()
                            .map(SideItem::Project),
                    );
                }
            }
        }
        items.push(SideItem::Header(Section::Folders, None));
        if open(Section::Folders) {
            #[cfg(unix)]
            let creation = self.creation.as_ref();
            #[cfg(unix)]
            if let Some(create) = creation.filter(|c| c.folder.is_empty()) {
                append_creation_rows(&mut items, create, 0);
            }
            for row in self.tree.rows.iter() {
                #[cfg(unix)]
                if let Some(rename) = self
                    .renaming
                    .as_ref()
                    .filter(|r| !r.in_header && r.path == row.path)
                {
                    items.push(SideItem::Rename(
                        rename.input.clone(),
                        row.depth,
                        row.kind == tessera_core::vault::EntryKind::Directory,
                    ));
                    if let Some(error) = &rename.error {
                        let mut line = String::new();
                        for word in error.split_whitespace() {
                            if !line.is_empty()
                                && line.chars().count() + word.chars().count() + 1 > 28
                            {
                                items.push(SideItem::CreateError(
                                    std::mem::take(&mut line),
                                    row.depth,
                                ));
                            }
                            if !line.is_empty() {
                                line.push(' ');
                            }
                            line.push_str(word);
                        }
                        if !line.is_empty() {
                            items.push(SideItem::CreateError(line, row.depth));
                        }
                    }
                    continue;
                }
                items.push(SideItem::Tree(row.clone()));
                #[cfg(unix)]
                if let Some(create) = creation.filter(|c| c.folder == row.path) {
                    append_creation_rows(&mut items, create, row.depth + 1);
                }
            }
        }
        items
    }

    fn activate_side_entry(
        &mut self,
        path: &str,
        folder: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if folder {
            self.reveal_in_tree(path, window, cx);
        } else {
            self.select_panel_note(reader_layout::Panel::Notes, path, None, window, cx);
        }
    }

    fn sync_tree(&mut self) {
        if !self
            .tree_source
            .as_ref()
            .is_some_and(|source| Arc::ptr_eq(source, &self.vault))
        {
            let same_root = self
                .tree_source
                .as_ref()
                .is_some_and(|source| source.root == self.vault.root);
            self.tree.refresh(&self.vault_root, &self.vault.entries);
            #[cfg(unix)]
            if !self.single_file {
                let folder = self
                    .creation_templates()
                    .map(|catalog| catalog.folder.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| tessera_core::note_templates::DEFAULT_FOLDER.into());
                self.tree.set_templates_folder(folder);
            }
            self.tree_source = Some(self.vault.clone());
            // Reconciliation must not re-center a tree the user already scrolled.
            if !same_root || self.tree.cursor.is_none() {
                self.tree_revealed.clear();
            }
            // Titles belong to this inventory and root.
            Arc::make_mut(&mut self.backlink_titles).clear();
            self.backlinks_expanded.clear();
        }
        let selected = self.selected_file().to_owned();
        #[cfg(unix)]
        let selected = self
            .creation
            .as_ref()
            .map_or(selected, |create| create.folder.clone());
        #[cfg(unix)]
        let selected = self
            .renaming
            .as_ref()
            .map_or(selected, |rename| rename.path.clone());
        if self.tree_revealed != selected {
            self.tree_revealed = selected.clone();
            if let Some(ix) = self.tree.reveal(&selected) {
                self.scroll_tree_to(ix);
            }
        }
    }

    /// Breadcrumb target (#366): show the tree and select
    /// `path`; a folder is also expanded, the current note is revealed.
    fn reveal_in_tree(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.resizing_panel = None;
        self.panels.open(reader_layout::Panel::Notes);
        if self
            .sidebar
            .collapsed
            .remove(&reader_sidebar::Section::Folders)
        {
            self.save_sidebar(cx);
        }
        self.sync_tree();
        if self.tree.reveal(path).is_some() {
            if self
                .tree
                .cursor_index()
                .map(|ix| &self.tree.rows[ix])
                .is_some_and(|row| {
                    row.kind == tessera_core::vault::EntryKind::Directory && !row.expanded
                })
            {
                self.tree.toggle(path);
                if self.single_file {
                    self.load_quick_folder(path.to_owned(), cx);
                }
            }
            if let Some(ix) = self.tree.cursor_index() {
                self.scroll_tree_to(ix);
            }
        }
        self.tree_focus.focus(window, cx);
        cx.notify();
    }

    fn tree_key(&mut self, key: TreeKey, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .sidebar
            .collapsed
            .contains(&reader_sidebar::Section::Folders)
        {
            return;
        }
        let lazy_folder =
            if self.single_file && matches!(key, TreeKey::Right | TreeKey::ExpandSubtree) {
                self.tree.cursor_folder()
            } else {
                None
            };
        let tree = &mut self.tree;
        match key {
            TreeKey::Down => tree.step(1),
            TreeKey::Up => tree.step(-1),
            TreeKey::Right => tree.right(),
            TreeKey::Left => tree.left(),
            TreeKey::ExpandSubtree | TreeKey::CollapseSubtree => {
                if let Some(folder) = tree.cursor_folder() {
                    tree.cursor = Some(folder.clone());
                    tree.set_subtree(&folder, key == TreeKey::ExpandSubtree);
                }
            }
            TreeKey::Open => {
                if let Some(ix) = tree.cursor_index() {
                    let row = tree.rows[ix].clone();
                    self.activate_tree_row(&row, window, cx);
                }
            }
        }
        if let Some(folder) = lazy_folder {
            self.load_quick_folder(folder, cx);
        }
        if let Some(ix) = self.tree.cursor_index() {
            self.scroll_tree_to(ix);
        }
        cx.notify();
    }

    /// ⌥-click and the folder context menu (#410).
    fn set_folder_subtree(
        &mut self,
        path: &str,
        expand: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tree = &mut self.tree;
        tree.cursor = Some(path.to_owned());
        tree.set_subtree(path, expand);
        if self.single_file && expand {
            self.load_quick_folder(path.to_owned(), cx);
        }
        self.tree_focus.focus(window, cx);
        cx.notify();
    }

    fn set_sidebar_sections(&mut self, mode: SectionAction, cx: &mut Context<Self>) {
        // Capture the visible state, including automatic folding while scrolling.
        if self.sidebar.folders_only_restore.is_none()
            && matches!(
                mode,
                SectionAction::FoldersOnly | SectionAction::ToggleFoldersOnly
            )
        {
            self.sidebar.folders_only_restore = Some(reader_sidebar::LEFT_SECTIONS.map(|s| {
                if s == reader_sidebar::Section::Projects {
                    self.sidebar.projects_collapsed
                } else {
                    self.scroll_sections.closed(s, &self.sidebar.collapsed)
                }
            }));
            // Toggle must enter Folders-only rather than consume the new snapshot.
            self.sidebar.folders_only(false);
        } else {
            match mode {
                SectionAction::FoldersOnly => self.sidebar.folders_only(false),
                SectionAction::ExpandAll => self.sidebar.all_sections(false),
                SectionAction::ToggleFoldersOnly => self.sidebar.folders_only(true),
                SectionAction::CollapseAll => self.sidebar.all_sections(true),
            }
        }
        self.scroll_sections.honor_expanded(&self.sidebar.collapsed);
        self.save_sidebar(cx);
        cx.notify();
    }

    /// Folders header «Collapse all» (#410).
    fn collapse_folders(&mut self, cx: &mut Context<Self>) {
        self.tree.collapse_all();
        cx.notify();
    }

    /// Folders header «Focus current» (#410): only the path to the
    /// open note stays expanded, and the note is selected.
    fn focus_current_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_file().is_empty() {
            return;
        }
        self.resizing_panel = None;
        self.panels.open(reader_layout::Panel::Notes);
        if self
            .sidebar
            .collapsed
            .remove(&reader_sidebar::Section::Folders)
        {
            self.save_sidebar(cx);
        }
        self.sync_tree();
        let current = self.selected_file().to_owned();
        if let Some(ix) = self.tree.focus(&current) {
            self.scroll_tree_to(ix);
        }
        self.tree_focus.focus(window, cx);
        cx.notify();
    }

    fn activate_tree_row(
        &mut self,
        row: &reader_tree::Row,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use tessera_core::vault::EntryKind;
        let tree = &mut self.tree;
        tree.cursor = Some(row.path.clone());
        match row.kind {
            EntryKind::Directory => {
                tree.toggle(&row.path);
                if self.single_file && !row.expanded {
                    self.load_quick_folder(row.path.clone(), cx);
                }
                self.tree_focus.focus(window, cx);
            }
            EntryKind::Markdown => {
                self.open_note(&row.path, None, window, cx);
            }
            EntryKind::Attachment => {
                self.preview_file(&row.path, window, cx);
            }
        }
        // Tree activation keeps keyboard navigation/rename in the navigator.
        // Async document replacement only transfers focus from the old content.
        self.tree_focus.focus(window, cx);
        if row.kind != EntryKind::Directory {
            self.dismiss_sidebar_after_selection(window, cx);
        }
        cx.notify();
    }

    fn history_move(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.history_ix as isize + delta;
        if target < 0 || target as usize >= self.history.len() {
            return;
        }
        self.history_positions[self.history_ix] =
            self.content.read(cx).list_state().logical_scroll_top();
        let rel = self.history[target as usize].clone();
        self.history_nav = Some(target as usize);
        self.open_note(&rel, None, window, cx);
        self.history_nav = None;
    }

    /// Move through the visible folder tree and open the note under the cursor.
    fn list_move(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_tree();
        let tree = &self.tree;
        let rels: Vec<String> = tree
            .rows
            .iter()
            .filter(|row| row.kind == tessera_core::vault::EntryKind::Markdown)
            .map(|row| row.path.clone())
            .collect();
        if rels.is_empty() {
            return;
        }
        let cur = rels.iter().position(|r| *r == self.current_rel);
        let next = match cur {
            Some(ix) => (ix as isize + delta).clamp(0, rels.len() as isize - 1) as usize,
            None if delta < 0 => rels.len() - 1,
            None => 0,
        };
        if Some(next) != cur {
            if self
                .panels
                .dismiss_target(f32::from(self.body_bounds.size.width))
                == reader_layout::Panel::Closed
            {
                self.open_note(&rels[next], None, window, cx);
            } else {
                self.select_panel_note(reader_layout::Panel::Notes, &rels[next], None, window, cx);
            }
        }
    }

    /// docs/design/reader.md §Layout: icon controls, breadcrumbs, loading status.
    fn render_header(&self, available: f32, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let notes_open = self.panels.visible(reader_layout::Panel::Notes, available);
        let backlinks_open = self
            .panels
            .visible(reader_layout::Panel::Backlinks, available);
        h_flex()
            .id("reader-header")
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
                    .child(preserve_reader_selection(
                        "reader-notes-preserve",
                        reader_icon_button("reader-notes", IconName::PanelLeft, NOTES_TOOLTIP, cx)
                            .selected(notes_open)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_panel(reader_layout::Panel::Notes, window, cx)
                            })),
                    ))
                    .child(
                        reader_icon_button(
                            "reader-history-back",
                            IconName::ArrowLeft,
                            with_shortcut("Back", "alt-left"),
                            cx,
                        )
                        .disabled(self.history_ix == 0)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.history_move(-1, window, cx)),
                        ),
                    )
                    .child(
                        reader_icon_button(
                            "reader-history-forward",
                            IconName::ArrowRight,
                            with_shortcut("Forward", "alt-right"),
                            cx,
                        )
                        .disabled(self.history_ix + 1 >= self.history.len())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.history_move(1, window, cx)),
                        ),
                    ),
            )
            .child(div().flex_1().min_w_0())
            .child(
                header_controls()
                    .flex_shrink(1.)
                    .min_w_0()
                    .when(self.single_file, |controls| {
                        controls.child(
                            Button::new("open-folder-as-vault")
                                .label("Open folder as vault")
                                .small()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.start_loading(
                                        Opts {
                                            vault: Some(this.vault_root.clone()),
                                            note: Some(this.current_rel.clone()),
                                            ..Default::default()
                                        },
                                        window,
                                        cx,
                                    );
                                })),
                        )
                    })
                    .child(self.render_loading(cx))
                    .child(
                        reader_icon_button(
                            "reader-search",
                            IconName::Search,
                            VAULT_SEARCH_TOOLTIP,
                            cx,
                        )
                        .debug_selector(|| "reader-search".into())
                        .on_click(
                            cx.listener(|this, _, window, cx| {
                                this.open_quick_open(true, window, cx)
                            }),
                        ),
                    )
                    .child(preserve_reader_selection(
                        "reader-backlinks-preserve",
                        reader_icon_button(
                            "reader-backlinks",
                            IconName::PanelRight,
                            BACKLINKS_TOOLTIP,
                            cx,
                        )
                        .selected(backlinks_open)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_panel(reader_layout::Panel::Backlinks, window, cx)
                        })),
                    ))
                    .child(
                        div()
                            .id("reader-appearance-tooltip")
                            .tooltip(|window, cx| {
                                gpui_component::tooltip::Tooltip::element(|_, cx| {
                                    div().child(format!("Appearance: {}", appearance_label(cx)))
                                })
                                .build(window, cx)
                            })
                            .child(
                                Button::new("reader-appearance")
                                    .ghost()
                                    .icon(if appearance_label(cx) == "Dark" {
                                        IconName::Moon
                                    } else {
                                        IconName::Sun
                                    })
                                    .w(px(28.))
                                    .h(px(28.))
                                    .rounded(px(6.))
                                    .accessibility_label("Appearance")
                                    .on_click(|_, window, cx| cycle_appearance(window, cx)),
                            ),
                    )
                    .child(
                        reader_icon_button(
                            "reader-settings",
                            IconName::Settings,
                            with_shortcut("Settings", "secondary-,"),
                            cx,
                        )
                        .on_click(cx.listener(|_, _, _, cx| {
                            reader_settings::show(Some(cx.entity().downgrade()), cx)
                        })),
                    )
                    .child(reader_more_menu(
                        self.vault_root.clone(),
                        self.selected_file().to_owned(),
                        self.sidebar.show_hidden,
                        cx.entity().downgrade(),
                        cx,
                    )),
            )
            .into_any_element()
    }

    fn render_breadcrumbs(&self, cx: &mut Context<Self>) -> AnyElement {
        #[cfg(unix)]
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
        let root = format!("Root: {}", self.vault_root.display());
        h_flex()
            .id("reader-breadcrumbs")
            .flex_1()
            .min_w_0()
            .gap_1()
            .px_2()
            .overflow_hidden()
            .whitespace_nowrap()
            .children(folders.into_iter().flat_map(|(folder, path)| {
                let menu_path = path.clone();
                let menu_root = self.vault_root.clone();
                [
                    div()
                        .id(SharedString::from(format!("reader-crumb-{path}")))
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
                        .context_menu(move |menu, _, _| {
                            reader_files::menu(menu, menu_root.clone(), menu_path.clone())
                        })
                        .child(folder)
                        .into_any_element(),
                    Icon::new(IconName::ChevronRight)
                        .xsmall()
                        .text_color(faint)
                        .into_any_element(),
                ]
            }))
            .child(
                div()
                    .id("reader-document-root")
                    .flex_shrink_0()
                    .max_w_full()
                    .overflow_hidden()
                    .text_ellipsis()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.text)
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.reveal_in_tree(&current, window, cx)
                    }))
                    .child(self.selected_title())
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(root.clone()).build(window, cx)
                    })
                    .context_menu(move |menu, _, _| {
                        reader_files::menu(menu, file_root.clone(), current_menu.clone())
                    }),
            )
            .into_any_element()
    }

    fn notes_count_label(&self) -> String {
        if !self.vault.inventory_scanned {
            "inventory pending".into()
        } else if !self.vault.unreadable.is_empty() {
            format!("{} · partial", self.vault.notes.len())
        } else {
            self.vault.notes.len().to_string()
        }
    }

    /// «Linked from» with counts only when something links here (#646).
    fn linked_from_title(&self) -> String {
        let (notes, places) = if self.file_preview.is_some() {
            (0, 0)
        } else {
            reader_right_panel::link_counts(self.backlinks.iter().map(|b| b.path.as_str()))
        };
        reader_right_panel::linked_from_title(
            self.vault.inventory_scanned,
            notes,
            places,
            !self.vault.unreadable.is_empty(),
        )
    }

    /// Panel title row: name and count, with compact quick-open in Notes.
    fn render_panel_header(
        &self,
        panel: reader_layout::Panel,
        width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let left = panel == reader_layout::Panel::Notes;
        let faint = brand::reader_palette(cx).text_faint;
        let (title, count) = if left {
            (
                self.vault_root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| self.vault_root.display().to_string()),
                self.notes_count_label(),
            )
        } else {
            (
                if self.active_timeline().is_some() {
                    "Note history"
                } else {
                    "On this page"
                }
                .to_owned(),
                String::new(),
            )
        };
        h_flex()
            .h(px(READER_HEADER_HEIGHT))
            .flex_none()
            .gap_2()
            .pl(px(16.))
            .pr(px(8.))
            .border_b_1()
            .border_color(brand::palette(cx).border_subtle)
            .text_size(px(brand::READER_CHROME_FONT_SIZE))
            .child(
                div()
                    .id("sidebar-vault-title")
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title)
                    .when(left, |this| {
                        let root = self.vault_root.display().to_string();
                        this.tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(root.clone()).build(window, cx)
                        })
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(11.))
                    .text_color(faint)
                    .child(count),
            )
            .when(left, |header| {
                #[cfg(unix)]
                let header = header
                    .child(
                        reader_icon_button(
                            "sidebar-new-note",
                            Icon::default().path("icons/square-pen.svg"),
                            with_shortcut("New note", "secondary-n"),
                            cx,
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| this.new_note(None, window, cx)),
                        ),
                    )
                    .child(
                        reader_icon_button(
                            "sidebar-new-folder",
                            Icon::default().path("icons/folder-plus.svg"),
                            "New folder",
                            cx,
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| this.new_folder(None, window, cx)),
                        ),
                    );
                header
                    .child(
                        reader_icon_button(
                            "sidebar-folders-only",
                            Icon::default().path(brand::READER_COLLAPSE_ICON),
                            with_shortcut(
                                "Folders only / Restore sections",
                                "secondary-shift-left",
                            ),
                            cx,
                        )
                        .debug_selector(|| "sidebar-folders-only".into())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.set_sidebar_sections(SectionAction::ToggleFoldersOnly, cx)
                        })),
                    )
                    .child(
                        h_flex()
                            .id("sidebar-search")
                            .debug_selector(|| "sidebar-search".into())
                            .track_focus(&self.sidebar_search_focus)
                            .flex_none()
                            .h(px(28.))
                            .px(px(6.))
                            .gap_1()
                            .rounded(px(6.))
                            .text_color(faint)
                            .cursor_pointer()
                            .hover(|s| s.bg(brand::reader_palette(cx).hover))
                            .focus(|s| s.bg(brand::reader_palette(cx).hover))
                            .tooltip(|window, cx| {
                                gpui_component::tooltip::Tooltip::new(format!(
                                    "Search notes ({SEARCH_SHORTCUT})"
                                ))
                                .build(window, cx)
                            })
                            .child(Icon::new(IconName::Search).small())
                            .when(width >= SEARCH_HINT_MIN_PANEL_WIDTH, |button| {
                                button.child(
                                    div()
                                        .text_size(px(11.))
                                        .child(SEARCH_SHORTCUT)
                                        .debug_selector(|| "sidebar-search-shortcut".into()),
                                )
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_quick_open(false, window, cx)
                            }))
                            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                    this.open_quick_open(false, window, cx);
                                    cx.stop_propagation();
                                }
                            })),
                    )
            })
            .into_any_element()
    }

    fn render_find_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let (current, total) = self.content.read(cx).search_status();
        let count = if total == 0 {
            if self.find_input.read(cx).value().trim().is_empty() {
                String::new()
            } else {
                "No matches".to_string()
            }
        } else {
            format!("{} of {}", current + 1, total)
        };
        // docs/design/reader.md §Find: floats over the document's top-right
        // corner so opening it never reflows the text.
        h_flex()
            .id("find-bar")
            .absolute()
            .top(px(10.))
            .right(px(18.))
            .gap_1()
            .pl_2()
            .pr_1()
            .py_1()
            .bg(cx.theme().popover)
            .border_1()
            .border_color(border)
            .rounded(px(8.))
            .shadow_md()
            .child(
                div().w(px(220.)).child(
                    Input::new(&self.find_input)
                        .appearance(false)
                        .cleanable(true)
                        .prefix(Icon::new(IconName::Search).small().text_color(muted)),
                ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .min_w(px(56.))
                    .child(count),
            )
            .child(
                Button::new("find-case-sensitive")
                    .ghost()
                    .small()
                    .icon(IconName::CaseSensitive)
                    .selected(reader_ui_state::find_case_sensitive(cx))
                    .tooltip("Match case")
                    .on_click(cx.listener(|this, _, window, cx| {
                        let sensitive = !reader_ui_state::find_case_sensitive(cx);
                        reader_ui_state::set_find_case_sensitive(sensitive, cx);
                        this.run_find(window, cx);
                    })),
            )
            .child(
                Button::new("find-prev")
                    .ghost()
                    .small()
                    .icon(IconName::ChevronUp)
                    .tooltip(with_shortcut("Previous match", "shift-enter"))
                    .on_click(cx.listener(|this, _, _, cx| this.find_step(-1, cx))),
            )
            .child(
                Button::new("find-next")
                    .ghost()
                    .small()
                    .icon(IconName::ChevronDown)
                    .tooltip("Next match ⏎")
                    .on_click(cx.listener(|this, _, _, cx| this.find_step(1, cx))),
            )
            .child(
                Button::new("find-close")
                    .ghost()
                    .small()
                    .icon(IconName::Close)
                    .tooltip("Close Esc")
                    .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
            )
            .into_any_element()
    }

    /// `![alt](relative path)` → `![alt](file:///abs%20path)` via resolve_asset.
    fn render_sidebar(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_tree(window, cx)
    }

    /// docs/design/reader.md §Sidebar: Recent, Pinned, Inbox and the real
    /// folder hierarchy (#335, #369), with a quick-open entry point (#433).
    fn render_tree(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        #[cfg(unix)]
        use gpui_component::menu::DropdownMenu as _;
        use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
        use reader_sidebar::Section;
        use tessera_core::vault::EntryKind;
        let p = brand::palette(cx);
        let tokens = brand::reader_palette(cx);
        let entity = cx.entity().downgrade();
        let tree = &self.tree;
        let items = self.sidebar_items();
        let pinned: Rc<std::collections::BTreeSet<String>> =
            Rc::new(self.sidebar.pinned.iter().cloned().collect());
        let collapsed: std::collections::BTreeSet<_> = [
            Section::Recent,
            Section::Pinned,
            Section::Inbox,
            Section::Projects,
            Section::Folders,
        ]
        .into_iter()
        .filter(|section| {
            if *section == Section::Projects {
                return self.sidebar.projects_collapsed;
            }
            self.scroll_sections
                .closed(*section, &self.sidebar.collapsed)
        })
        .collect();
        let show_hidden = self.sidebar.show_hidden;
        let current = self.selected_file().to_owned();
        let cursor = self
            .tree_focus
            .is_focused(window)
            .then(|| tree.cursor.clone())
            .flatten();
        let published_root = self.vault_root.clone();
        let published_generation = self.watcher_generation;
        // Every row action is bound to the inventory it was rendered from.
        let act =
            move |entity: &WeakEntity<Reader>,
                  window: &mut Window,
                  cx: &mut App,
                  f: &dyn Fn(&mut Reader, &mut Window, &mut Context<Reader>)| {
                if let Some(e) = entity.upgrade() {
                    e.update(cx, |this, cx| {
                        if this.vault_root == published_root
                            && this.watcher_generation == published_generation
                        {
                            f(this, window, cx)
                        }
                    });
                }
            };
        let act = Rc::new(act);
        #[cfg(unix)]
        let drag_root = self.vault_root.clone();
        let render_row = Rc::new(move |item: SideItem, ix: usize| {
            let entity = entity.clone();
            let act = act.clone();
            let row_base = |id: SharedString| {
                h_flex()
                    .id(id)
                    .w_full()
                    .h(px(28.))
                    .gap(px(6.))
                    .pr_2()
                    .rounded(px(6.))
                    .whitespace_nowrap()
            };
            let pin = |path: String, group: SharedString, on: bool| {
                let entity = entity.clone();
                let act = act.clone();
                div()
                    .id(SharedString::from(format!("pin-{group}")))
                    .flex_none()
                    .opacity(if on { 1. } else { 0. })
                    .group_hover(group, |s| s.opacity(1.))
                    .child(
                        Icon::default()
                            .path(brand::READER_PIN_ICON)
                            .xsmall()
                            .text_color(if on { p.accent } else { tokens.text_faint }),
                    )
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        let path = path.clone();
                        act(&entity, window, cx, &|this, _, cx| {
                            this.toggle_pin(&path, cx)
                        });
                    })
            };
            match item {
                #[cfg(unix)]
                SideItem::Rename(input, depth, directory) => row_base("inline-rename-row".into())
                    .debug_selector(|| "inline-rename-row".into())
                    .key_context("InlineRename")
                    // Consume Input's propagated submit before text fallback can
                    // replace a selected filename with an empty newline.
                    .on_action(|_: &gpui_component::input::Enter, _, cx| {
                        cx.stop_propagation();
                    })
                    .pl(px(18. + depth as f32 * 14.))
                    .child(
                        Icon::new(if directory {
                            IconName::Folder
                        } else {
                            IconName::FileText
                        })
                        .small(),
                    )
                    .child(div().flex_1().min_w_0().child(Input::new(&input).small()))
                    .into_any_element(),
                #[cfg(unix)]
                SideItem::Create(input, depth, directory, templates, selected) => {
                    row_base("inline-create-row".into())
                        .debug_selector(|| "inline-create-row".into())
                        .key_context("InlineCreate")
                        // Consume Input's propagated submit before text fallback can
                        // replace a selected filename with an empty newline.
                        .on_action(|_: &gpui_component::input::Enter, _, cx| {
                            cx.stop_propagation();
                        })
                        .pl(px(18. + depth as f32 * 14.))
                        .child(
                            Icon::new(if directory {
                                IconName::Folder
                            } else {
                                IconName::FileText
                            })
                            .small(),
                        )
                        .child(div().flex_1().min_w_0().child(Input::new(&input).small()))
                        .when(!directory, |row| {
                            row.child(
                                Button::new("create-template-picker")
                                    .ghost()
                                    .small()
                                    .label(
                                        selected
                                            .as_deref()
                                            .unwrap_or("Built-in")
                                            .trim_end_matches(".md")
                                            .chars()
                                            .take(10)
                                            .collect::<String>(),
                                    )
                                    .icon(IconName::ChevronDown)
                                    .tooltip(format!(
                                        "Template: {}",
                                        selected.as_deref().unwrap_or("Built-in")
                                    ))
                                    .dropdown_menu_with_anchor(
                                        Anchor::TopRight,
                                        move |menu, _, _| {
                                            let option = |label: String, value: Option<String>| {
                                                let entity = entity.clone();
                                                let act = act.clone();
                                                PopupMenuItem::new(label)
                                                    .checked(value == selected)
                                                    .on_click(move |_, window, cx| {
                                                        act(
                                                            &entity,
                                                            window,
                                                            cx,
                                                            &|this, window, cx| {
                                                                this.choose_creation_template(
                                                                    value.clone(),
                                                                    window,
                                                                    cx,
                                                                );
                                                            },
                                                        )
                                                    })
                                            };
                                            let mut menu =
                                                menu.item(option("Built-in note".into(), None));
                                            for name in &templates {
                                                menu = menu
                                                    .item(option(name.clone(), Some(name.clone())));
                                            }
                                            menu
                                        },
                                    ),
                            )
                        })
                        .into_any_element()
                }
                #[cfg(unix)]
                SideItem::CreateError(error, depth) => {
                    row_base(format!("inline-create-error-{ix}").into())
                        .pl(px(38. + depth as f32 * 14.))
                        .text_color(p.danger)
                        .text_size(px(12.))
                        .child(error.clone())
                        .tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(error.clone()).build(window, cx)
                        })
                        .into_any_element()
                }
                SideItem::Header(section, count) => {
                    let closed = collapsed.contains(&section);
                    let icon = match section {
                        Section::Recent => Icon::default().path(brand::READER_CLOCK_ICON),
                        Section::Pinned => Icon::default().path(brand::READER_PIN_ICON),
                        Section::Inbox => Icon::new(IconName::Inbox),
                        Section::Projects => Icon::new(IconName::Folder),
                        Section::Folders => Icon::new(IconName::Folder),
                        Section::Properties => Icon::default().path(READER_SLIDERS_ICON),
                    };
                    row_base(SharedString::from(format!(
                        "side-section-{}",
                        section.label()
                    )))
                    .pl(px(6.))
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(tokens.text_faint)
                    .cursor_pointer()
                    .tooltip(|window, cx| {
                        gpui_component::tooltip::Tooltip::new(if cfg!(target_os = "macos") {
                            "Option-click to collapse or expand all sections"
                        } else {
                            "Alt-click to collapse or expand all sections"
                        }).build(window, cx)
                    })
                    .hover(move |d| d.text_color(p.text_muted))
                    .child(
                        Icon::new(if closed {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .xsmall(),
                    )
                    .child(icon.xsmall())
                    .child(div().flex_1().child(section.label()))
                    .when(section == Section::Folders, |row| {
                        let entity = entity.clone();
                        let act = act.clone();
                        // Collapse all / Focus current appear on
                        // hover of the header (#410).
                        let action = |id: &'static str,
                                                  icon: &'static str,
                                                  tooltip: &'static str,
                                                  f: fn(
                                        &mut Reader,
                                        &mut Window,
                                        &mut Context<Reader>,
                                    )| {
                                        let entity = entity.clone();
                                        let act = act.clone();
                                        div()
                                            .id(id)
                                            .flex_none()
                                            .px_1()
                                            .opacity(if id == "folders-collapse-all" { 1. } else { 0. })
                                            .debug_selector(move || id.into())
                                            .group_hover(FOLDERS_HEADER_GROUP, |s| s.opacity(1.))
                                            .text_color(p.text_muted)
                                            .hover(move |s| s.text_color(p.text))
                                            .tooltip(move |window, cx| {
                                                gpui_component::tooltip::Tooltip::new(tooltip)
                                                    .build(window, cx)
                                            })
                                            .child(Icon::default().path(icon).small())
                                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                cx.stop_propagation()
                                            })
                                            .on_click(move |_, window, cx| {
                                                cx.stop_propagation();
                                                act(&entity, window, cx, &|this, window, cx| {
                                                    f(this, window, cx)
                                                })
                                            })
                                    };
                        row.group(FOLDERS_HEADER_GROUP)
                            .when(cfg!(unix), |row| {
                                row.child(action(
                                    "folders-new-note",
                                    "icons/file-plus.svg",
                                    with_shortcut("New File", "secondary-n"),
                                    |this, window, cx| {
                                        let folder = this
                                            .tree
                                            .rows
                                            .iter()
                                            .find(|row| {
                                                Some(&row.path) == this.tree.cursor.as_ref()
                                            })
                                            .map(|row| {
                                                if row.kind
                                                    == tessera_core::vault::EntryKind::Directory
                                                {
                                                    row.path.clone()
                                                } else {
                                                    Path::new(&row.path)
                                                        .parent()
                                                        .unwrap_or(Path::new(""))
                                                        .to_string_lossy()
                                                        .into_owned()
                                                }
                                            });
                                        this.new_note(folder.as_deref(), window, cx);
                                    },
                                ))
                                .child(action(
                                    "folders-new-folder",
                                    "icons/folder-plus.svg",
                                    "New Folder",
                                    |this, window, cx| {
                                        let folder = this
                                            .tree
                                            .rows
                                            .iter()
                                            .find(|row| {
                                                Some(&row.path) == this.tree.cursor.as_ref()
                                            })
                                            .map(|row| {
                                                if row.kind
                                                    == tessera_core::vault::EntryKind::Directory
                                                {
                                                    row.path.clone()
                                                } else {
                                                    Path::new(&row.path)
                                                        .parent()
                                                        .unwrap_or(Path::new(""))
                                                        .to_string_lossy()
                                                        .into_owned()
                                                }
                                            });
                                        this.new_folder(folder.as_deref(), window, cx);
                                    },
                                ))
                            })
                            .child(action(
                                "folders-collapse-all",
                                brand::READER_COLLAPSE_ICON,
                                COLLAPSE_FOLDERS_TOOLTIP,
                                |this, _, cx| this.collapse_folders(cx),
                            ))
                            .child(action(
                                "folders-focus-current",
                                brand::READER_FOCUS_ICON,
                                FOCUS_CURRENT_TOOLTIP,
                                |this, window, cx| this.focus_current_folder(window, cx),
                            ))
                            .child(
                                div()
                                    .id("toggle-hidden-files")
                                    .flex_none()
                                    .px_1()
                                    .text_color(if show_hidden { p.accent } else { p.text_muted })
                                    .tooltip(|window, cx| {
                                        gpui_component::tooltip::Tooltip::new(HIDDEN_FILES_TOOLTIP)
                                            .build(window, cx)
                                    })
                                    .child(
                                        Icon::new(if show_hidden {
                                            IconName::Eye
                                        } else {
                                            IconName::EyeOff
                                        })
                                        .small(),
                                    )
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_click(move |_, window, cx| {
                                        cx.stop_propagation();
                                        act(&entity, window, cx, &|this, _, cx| {
                                            this.toggle_hidden_files(cx)
                                        })
                                    }),
                            )
                    })
                    .map(|d| {
                        #[cfg(unix)]
                        let d = if section == Section::Folders {
                            reader_move_picker::drop_target(
                                d,
                                entity.clone(),
                                drag_root.clone(),
                                String::new(),
                            )
                        } else {
                            d
                        };
                        d
                    })
                    .when_some(count, |d, count| {
                        d.child(
                            div()
                                .px_1p5()
                                .rounded_full()
                                .bg(p.tile)
                                .text_color(p.link)
                                .child(count.to_string()),
                        )
                    })
                    .on_click({
                        let entity = entity.clone();
                        let act = act.clone();
                        move |event, window, cx| {
                            act(&entity, window, cx, &|this, _, cx| {
                                if event.modifiers().alt {
                                    this.set_sidebar_sections(if closed { SectionAction::ExpandAll } else { SectionAction::CollapseAll }, cx);
                                } else {
                                    this.toggle_section(section, cx);
                                }
                            })
                        }
                    })
                    .context_menu({
                        let entity = entity.clone();
                        let act = act.clone();
                        move |menu, _, _| {
                            if section != Section::Folders || !cfg!(unix) {
                                return menu;
                            }
                            let item = |label: &'static str, directory: bool| {
                                let entity = entity.clone();
                                let act = act.clone();
                                PopupMenuItem::new(label).on_click(move |_, window, cx| {
                                    act(&entity, window, cx, &|this, window, cx| {
                                        if directory {
                                            this.new_folder(Some(""), window, cx);
                                        } else {
                                            this.new_note(Some(""), window, cx);
                                        }
                                    });
                                })
                            };
                            menu.item(item("New File in vault root", false))
                                .item(item("New Folder in vault root", true))
                        }
                    })
                    .into_any_element()
                }
                SideItem::ProjectsDone(count, expanded) => row_base("projects-done".into())
                    .pl(px(30.))
                    .text_size(px(12.))
                    .text_color(tokens.text_faint)
                    .cursor_pointer()
                    .hover(move |d| d.bg(tokens.hover))
                    .debug_selector(|| "projects-done".into())
                    .child(if expanded {
                        "Hide done".into()
                    } else {
                        format!("Show {count} done")
                    })
                    .on_click(move |_, window, cx| {
                        act(&entity, window, cx, &|this, _, cx| {
                            this.projects_done_expanded = !this.projects_done_expanded;
                            cx.notify();
                        })
                    })
                    .into_any_element(),
                SideItem::Project(project) => {
                    use tessera_core::projects::Status;
                    let path = project.path.clone();
                    let selector = format!("side-project-{path}");
                    let is_current = path == current;
                    let icon = match project.status {
                        Status::Active => Icon::default().path("icons/project-active.svg"),
                        Status::Planned => Icon::default().path("icons/project-planned.svg"),
                        Status::Done => Icon::new(IconName::CircleCheck),
                        Status::Other => Icon::default().path(brand::READER_CLOCK_ICON),
                    };
                    let location = path
                        .strip_suffix("/_index.md")
                        .unwrap_or(&path)
                        .replace('/', " › ");
                    let status = format!("{} · {}", project.status_label, location);
                    row_base(selector.clone().into())
                        .pl(px(10.))
                        .debug_selector(move || selector.clone())
                        .cursor_pointer()
                        .hover(move |d| d.bg(tokens.hover))
                        .when(is_current, |d| {
                            d.bg(p.selected).font_weight(FontWeight::MEDIUM)
                        })
                        .child(icon.small().text_color(p.text_muted))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(project.title),
                        )
                        .when_some(project.domain, |d, domain| {
                            d.child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(tokens.text_faint)
                                    .max_w(px(72.))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(domain),
                            )
                        })
                        .when(project.open_tasks > 0, |d| {
                            d.child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(tokens.text_faint)
                                    .child(project.open_tasks.to_string()),
                            )
                        })
                        .tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(status.clone()).build(window, cx)
                        })
                        .on_click(move |_, window, cx| {
                            act(&entity, window, cx, &|this, window, cx| {
                                this.activate_side_entry(&path, false, window, cx);
                            })
                        })
                        .into_any_element()
                }
                SideItem::More(more) => row_base(SharedString::from("side-recent-more"))
                    .pl(px(30.))
                    .text_xs()
                    .text_color(tokens.text_faint)
                    .cursor_pointer()
                    .hover(move |d| d.text_color(p.text_muted))
                    .child(format!("{more} more"))
                    .on_click(move |_, window, cx| {
                        act(&entity, window, cx, &|this, _, cx| {
                            this.recent_expanded = true;
                            cx.notify();
                        })
                    })
                    .into_any_element(),
                SideItem::Empty(text) => row_base(SharedString::from(format!("side-empty-{ix}")))
                    .pl(px(30.))
                    .text_xs()
                    .text_color(tokens.text_faint)
                    .child(text)
                    .into_any_element(),
                SideItem::Entry {
                    section,
                    path,
                    label,
                    meta,
                    location,
                    folder,
                } => {
                    let group = SharedString::from(format!("side-{}-{path}", section.label()));
                    let is_current = path == current;
                    let on = pinned.contains(&path);
                    let open_path = path.clone();
                    let open_entity = entity.clone();
                    let open_act = act.clone();
                    row_base(group.clone())
                        .when(location.is_some(), |row| row.h(px(44.)))
                        .debug_selector({
                            let group = group.clone();
                            move || group.to_string()
                        })
                        .group(group.clone())
                        .pl(px(10.))
                        .cursor_pointer()
                        .when(is_current, |d| {
                            d.bg(p.selected).font_weight(FontWeight::MEDIUM)
                        })
                        .when(!is_current, |d| d.hover(move |d| d.bg(tokens.hover)))
                        .child(
                            Icon::new(if folder {
                                IconName::Folder
                            } else {
                                IconName::FileText
                            })
                            .small()
                            .text_color(p.text_muted),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .child(div().text_ellipsis().child(label))
                                .when_some(location, |column, location| {
                                    column.child(
                                        div()
                                            .debug_selector({
                                                let group = group.clone();
                                                move || format!("{group}-location")
                                            })
                                            .text_size(px(12.))
                                            .line_height(px(16.))
                                            .text_color(p.text_muted)
                                            .text_ellipsis()
                                            .child(location),
                                    )
                                }),
                        )
                        .when_some(meta, |d, meta| {
                            d.child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(tokens.text_faint)
                                    .child(meta),
                            )
                        })
                        .when(section != Section::Inbox, |d| {
                            d.child(pin(path.clone(), group.clone(), on))
                        })
                        .on_click(move |_, window, cx| {
                            let path = open_path.clone();
                            open_act(&open_entity, window, cx, &|this, window, cx| {
                                this.activate_side_entry(&path, folder, window, cx)
                            })
                        })
                        .into_any_element()
                }
                SideItem::Tree(row) => {
                    let directory = row.kind == EntryKind::Directory;
                    let is_current = row.path == current;
                    let is_cursor = cursor.as_ref() == Some(&row.path);
                    let label = match row.kind {
                        EntryKind::Markdown => row
                            .label
                            .strip_suffix(".md")
                            .unwrap_or(&row.label)
                            .to_string(),
                        _ => row.label.clone(),
                    };
                    let icon = match row.kind {
                        EntryKind::Directory if row.depth == 0 => None,
                        EntryKind::Directory if row.expanded => Some(IconName::FolderOpen),
                        EntryKind::Directory => Some(IconName::Folder),
                        EntryKind::Markdown => Some(IconName::FileText),
                        EntryKind::Attachment => Some(IconName::File),
                    };
                    let group = SharedString::from(format!("tree-row-{}", row.path));
                    let pinnable =
                        row.kind != EntryKind::Attachment && !(directory && row.depth == 0);
                    let on = pinned.contains(&row.path);
                    let path = row.path.clone();
                    row_base(group.clone())
                        .map(|d| {
                            #[cfg(unix)]
                            let d = {
                                let root = drag_root.clone();
                                let d = if directory || row.kind == EntryKind::Markdown {
                                    reader_move_picker::draggable(
                                        d,
                                        root.clone(),
                                        row.path.clone(),
                                        label.clone(),
                                    )
                                } else {
                                    d
                                };
                                if directory {
                                    reader_move_picker::drop_target(
                                        d,
                                        entity.clone(),
                                        root,
                                        row.path.clone(),
                                    )
                                } else {
                                    d
                                }
                            };
                            d
                        })
                        .debug_selector({
                            let group = group.clone();
                            move || group.to_string()
                        })
                        .group(group.clone())
                        .pl(px(4. + row.depth as f32 * 14.))
                        .cursor_pointer()
                        .when(is_current, |d| {
                            d.bg(p.selected).font_weight(FontWeight::MEDIUM)
                        })
                        .when(!is_current, |d| d.hover(move |d| d.bg(tokens.hover)))
                        .when(is_cursor, |d| d.border_1().border_color(p.focus))
                        .when(directory && row.depth == 0, |d| {
                            d.font_weight(FontWeight::SEMIBOLD)
                        })
                        // Archive stays visible but recedes (#369);
                        // hidden items shown on demand too (#395).
                        .when((row.archived || row.hidden) && !is_current, |d| {
                            d.text_color(p.text_muted).opacity(0.75)
                        })
                        .child(if directory {
                            let toggle_entity = entity.clone();
                            let toggle_row = row.clone();
                            div()
                                .id(SharedString::from(format!(
                                    "folder-disclosure-{}",
                                    row.path
                                )))
                                .on_click(move |event, window, cx| {
                                    cx.stop_propagation();
                                    if event.click_count() != 1 {
                                        return;
                                    }
                                    let _ = toggle_entity.update(cx, |this, cx| {
                                        this.activate_tree_row(&toggle_row, window, cx)
                                    });
                                })
                                .child(
                                    Icon::new(if row.expanded {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .xsmall()
                                    .text_color(tokens.text_faint),
                                )
                                .into_any_element()
                        } else {
                            div().w(px(14.)).flex_none().into_any_element()
                        })
                        .when_some(icon, |d, icon| {
                            d.child(Icon::new(icon).small().text_color(p.text_muted))
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(label),
                        )
                        .when(pinnable, |d| d.child(pin(path, group.clone(), on)))
                        .on_click({
                            let entity = entity.clone();
                            let act = act.clone();
                            let row = row.clone();
                            move |event, window, cx| {
                                let row = row.clone();
                                // ⌥-click: the folder and all its
                                // subfolders, as in Finder (#410).
                                let deep = directory && event.modifiers().alt;
                                act(&entity, window, cx, &|this, window, cx| {
                                    if deep {
                                        this.set_folder_subtree(
                                            &row.path,
                                            !row.expanded,
                                            window,
                                            cx,
                                        )
                                    } else if directory && event.click_count() != 2 {
                                        this.tree.cursor = Some(row.path.clone());
                                        this.tree_focus.focus(window, cx);
                                        cx.notify();
                                    } else {
                                        this.activate_tree_row(&row, window, cx)
                                    }
                                })
                            }
                        })
                        .map(|d| {
                            if !directory {
                                let file_entity = entity.clone();
                                let file_rel = row.path.clone();
                                return d
                                    .context_menu(move |menu, _, cx| {
                                        if let Some(view) = file_entity.upgrade() {
                                            reader_item_menu(menu, &view, file_rel.clone(), cx)
                                        } else {
                                            menu
                                        }
                                    })
                                    .into_any_element();
                            }
                            let folder = row.path.clone();
                            d.context_menu(move |menu, _, cx| {
                                let menu = if let Some(view) = entity.upgrade() {
                                    reader_item_menu(menu, &view, folder.clone(), cx).separator()
                                } else {
                                    menu
                                };
                                let subtree = |label: &'static str, expand: bool| {
                                    let entity = entity.clone();
                                    let act = act.clone();
                                    let folder = folder.clone();
                                    PopupMenuItem::new(label).on_click(move |_, window, cx| {
                                        let folder = folder.clone();
                                        act(&entity, window, cx, &|this, window, cx| {
                                            this.set_folder_subtree(&folder, expand, window, cx)
                                        })
                                    })
                                };
                                let create = |label: &'static str, directory: bool| {
                                    let entity = entity.clone();
                                    let act = act.clone();
                                    let folder = folder.clone();
                                    PopupMenuItem::new(label).on_click(move |_, window, cx| {
                                        act(&entity, window, cx, &|this, window, cx| {
                                            if directory {
                                                this.new_folder(Some(&folder), window, cx);
                                            } else {
                                                this.new_note(Some(&folder), window, cx);
                                            }
                                        });
                                    })
                                };
                                menu.when(cfg!(unix), |menu| {
                                    menu.item(create("New File", false))
                                        .item(create("New Folder", true))
                                })
                                .separator()
                                .item(subtree("Expand all subfolders", true))
                                .item(subtree("Collapse all subfolders", false))
                            })
                            .into_any_element()
                        })
                }
            }
        });
        // Headers never enter a scrollable list. Upper bodies have a bounded
        // viewport so large Inbox/Pinned collections cannot displace Folders.
        let mut sections: Vec<(reader_sidebar::Section, SideItem, Vec<SideItem>)> = Vec::new();
        for item in items {
            if let SideItem::Header(section, _) = &item {
                sections.push((*section, item, Vec::new()));
            } else if let Some((_, _, rows)) = sections.last_mut() {
                rows.push(item);
            }
        }
        let expanded = sections
            .iter()
            .filter(|(s, _, rows)| *s != Section::Folders && !rows.is_empty())
            .count()
            .max(1);
        let upper_budget = (f32::from(self.body_bounds.size.height)
            - READER_HEADER_HEIGHT
            - sections.len() as f32 * 28.
            - 8.
            - 140.)
            .max(0.);
        let per_section = upper_budget / expanded as f32;
        let mut section_views = Vec::new();
        for (index, (section, header, rows)) in sections.into_iter().enumerate() {
            section_views.push(
                div()
                    .h(px(28.))
                    .flex_none()
                    .debug_selector(move || format!("sidebar-header-{}", section.label()))
                    .child(render_row(header, 0))
                    .into_any_element(),
            );
            if rows.is_empty() {
                continue;
            }
            let count = rows.len();
            let row_height = if section == Section::Inbox
                && matches!(rows.first(), Some(SideItem::Entry { .. }))
            {
                44.
            } else {
                28.
            };
            let render = render_row.clone();
            let list = uniform_list(
                SharedString::from(format!("sidebar-body-{}", section.label())),
                count,
                move |range: std::ops::Range<usize>, _, _| {
                    range
                        .map(|ix| render(rows[ix].clone(), ix))
                        .collect::<Vec<_>>()
                },
            );
            if section == Section::Folders {
                let observer = cx.entity().downgrade();
                let observed_root = self.vault_root.clone();
                let observed_generation = self.watcher_generation;
                section_views.push(
                    div()
                        .id("reader-tree")
                        .debug_selector(|| "sidebar-tree-body".into())
                        .key_context("ReaderTree")
                        .track_focus(&self.tree_focus)
                        .flex_1()
                        .min_h_0()
                        .overflow_hidden()
                        .on_scroll_wheel(cx.listener(|_, event: &ScrollWheelEvent, window, cx| {
                            if event.delta.pixel_delta(px(28.)).y > px(0.) {
                                cx.defer_in(window, |this, _, cx| {
                                    if this.tree_scroll.0.borrow().base_handle.offset().y
                                        >= px(-0.5)
                                        && this.scroll_sections.restore()
                                    {
                                        this.save_sidebar(cx);
                                        cx.notify();
                                    }
                                });
                            }
                        }))
                        .child(list.track_scroll(&self.tree_scroll).size_full())
                        .child(
                            canvas(
                                |_, _, _| (),
                                move |_, _, _, cx| {
                                    let observer = observer.clone();
                                    let observed_root = observed_root.clone();
                                    cx.defer(move |cx| {
                                        let _ = observer.update(cx, |this, cx| {
                                            if this.vault_root != observed_root
                                                || this.watcher_generation != observed_generation
                                            {
                                                return;
                                            }
                                            let offset = f32::from(
                                                this.tree_scroll.0.borrow().base_handle.offset().y,
                                            );
                                            if this.scroll_sections.observe(offset) {
                                                this.save_sidebar(cx);
                                                cx.notify();
                                            }
                                        });
                                    });
                                },
                            )
                            .absolute()
                            .size_0(),
                        )
                        .into_any_element(),
                );
            } else {
                section_views.push(
                    div()
                        .h(px((count as f32 * row_height).min(per_section)))
                        .flex_none()
                        .overflow_hidden()
                        .debug_selector(move || format!("sidebar-body-{}", section.label()))
                        .child(list.track_scroll(&self.section_scroll[index]).size_full())
                        .into_any_element(),
                );
            }
        }

        v_flex()
            .w_full()
            .h_full()
            .flex_none()
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .px_2()
                    .pb_2()
                    .overflow_hidden()
                    .text_size(px(brand::READER_CHROME_FONT_SIZE))
                    .children(section_views),
            )
            .into_any_element()
    }

    fn render_main(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some(preview) = self.render_timeline_preview(window, cx) {
            return preview;
        }
        if self.selected_file().is_empty()
            && !self
                .loading
                .as_ref()
                .is_some_and(|load| load.active && !load.published)
        {
            return self.render_empty_vault(cx);
        }
        if let Some(preview) = &self.file_preview {
            return self.render_file_preview(preview, window, cx);
        }
        if self.editing.is_some() {
            return self.render_source(window, cx);
        }
        let entity = cx.entity().downgrade();
        let mut style = reader_text_style(cx.theme());
        style.heading_base_font_size = px(reader_ui_state::font_size(cx));
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
                    .panels
                    .widths(&this.panel_widths, f32::from(body.size.width));
                let outside_panels = event.position.x >= body.left() + px(widths.notes)
                    && event.position.x < body.right() - px(widths.backlinks);
                if outside_panels && !column_bounds.get().contains(&event.position) {
                    this.content
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
                    .max_w(px(reader_ui_state::reading_width(cx)))
                    .when_some(self.render_properties_strip(cx), |column, strip| {
                        column.child(strip)
                    })
                    .child(
                        reader_plugins(
                            self.vault_root.clone(),
                            TextView::new(&self.content)
                                .scrollable(true)
                                .selectable(true)
                                // The element pushes its flags into the state every frame
                                // (same trap as `scrollable`): the state-side
                                // selection_format is clobbered unless set here too.
                                .selection_format(self.sel_format)
                                .style(style)
                                .text_size(px(reader_ui_state::font_size(cx)))
                                .px(px(READER_SIDE_PADDING))
                                .pt(self.reader_top_inset(cx))
                                .w_full()
                                .flex_1()
                                .min_h_0(),
                            entity.clone(),
                            self.sel_format,
                            self.link_presentations.clone(),
                            &self.link_identities,
                        )
                        .table_actions(move |data, _, _| {
                            // #368: only a table wider than the column offers it.
                            let entity = entity.clone();
                            let markdown = data.markdown.clone();
                            h_flex().justify_end().when(data.overflows, move |d| {
                                d.child(
                                    Button::new("expand-table")
                                        .small()
                                        .icon(IconName::Maximize)
                                        .label(format!("{} columns", data.headers.len()))
                                        .ghost()
                                        .debug_selector(|| "expand-table-badge".into())
                                        .tooltip("Show the whole table")
                                        .on_click(move |_, window, cx| {
                                            let markdown = markdown.clone();
                                            let _ = entity.update(cx, |this, cx| {
                                                this.open_table_overlay(&markdown, window, cx)
                                            });
                                        }),
                                )
                            })
                        }),
                    ),
            )
            .into_any_element()
    }

    fn open_table_overlay(&mut self, markdown: &str, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.entity().downgrade();
        let configured = reader_plugins(
            self.vault_root.clone(),
            TextView::new(&self.content),
            entity,
            self.sel_format,
            self.link_presentations.clone(),
            &self.link_identities,
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
        self.table_overlay = Some(state);
        focus.focus(window, cx);
        cx.notify();
    }

    fn close_table_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.table_overlay = None;
        let focus = self.content.read(cx).focus_handle().clone();
        focus.focus(window, cx);
        cx.notify();
    }

    /// #368: the table at window width over the document, which stays where
    /// it was. Columns take their natural width, long cells wrap, and the
    /// table scrolls sideways only when it still does not fit.
    fn render_table_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.table_overlay.clone()?;
        let p = brand::palette(cx);
        let mut style = reader_text_style(cx.theme());
        style.heading_base_font_size = px(reader_ui_state::font_size(cx));
        let entity = cx.entity().downgrade();
        let scrim = brand::reader_palette(cx).scrim;
        Some(
            div()
                .id("table-overlay")
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
                                        "Close Esc",
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
                                self.link_presentations.clone(),
                                &self.link_identities,
                            )),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_backlinks(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        if self.file_preview.is_some() {
            return panel_empty_line(muted, "—");
        }
        let current_bg = cx.theme().accent;
        let hover_bg = brand::reader_palette(cx).hover;

        // One group per source note (#22). `Vault::backlinks` returns them
        // grouped by path in source-line order, so a run of equal paths is a
        // group; no re-sorting here.
        let mut groups: Vec<(&Backlink, Vec<&Backlink>)> = Vec::new();
        for b in &self.backlinks {
            match groups.last_mut() {
                Some((head, links)) if head.path == b.path => links.push(b),
                _ => groups.push((b, vec![b])),
            }
        }

        let p = brand::palette(cx);
        let faint = brand::reader_palette(cx).text_faint;
        let mark = p.accent.opacity(0.18);
        let items: Vec<AnyElement> = groups
            .into_iter()
            .enumerate()
            .map(|(gx, (head, links))| {
                let (title, location) = self.backlink_title(&head.path);
                // Ambiguity is per link, but the flag is worth one word on the
                // card when any link in the group carries it.
                let any_ambiguous = links.iter().any(|b| b.ambiguous);
                let is_current = head.path == self.current_rel;
                let via: Vec<String> = links
                    .iter()
                    .filter_map(|b| b.property.clone())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                let occurrences: Vec<_> = links
                    .iter()
                    .map(|b| {
                        let (context, link, jump) = backlink_occurrence(b);
                        let external = if b.property.is_none() {
                            prepared_links::snippet_links(&b.context, &context)
                        } else {
                            Vec::new()
                        };
                        let (context, link, external) =
                            prepared_links::decorate_snippet(context, link, external);
                        (context, link, jump, external)
                    })
                    .collect();
                let count = occurrences.len();
                let expanded = self.backlinks_expanded.contains(&head.path);
                let shown = if expanded {
                    count
                } else {
                    count.min(BACKLINK_PLACES_SHOWN)
                };

                let card_rel = head.path.clone();
                let more_rel = head.path.clone();
                let group = SharedString::from(format!("bl-card-{gx}"));
                // #394 (variant A): the header is the source note — bold title
                // with its folder right beside it, place count, ↗ on hover —
                // and opens it. The rows below are places inside it: indented
                // under a rule, muted, this note's name bold on a light mark,
                // never link-blue. Each place opens the source there.
                let hover_rel = head.path.clone();
                let header = h_flex()
                    .id(SharedString::from(format!("bl-source-{gx}")))
                    .on_hover(cx.listener(move |this, active, window, cx| {
                        let key = format!("backlink:{hover_rel}");
                        if *active {
                            this.hover_note(
                                key,
                                reader_hover::Target {
                                    path: hover_rel.clone(),
                                    heading: None,
                                },
                                window.mouse_position(),
                                window,
                                cx,
                            );
                        } else {
                            this.leave_hover(&key, cx);
                        }
                    }))
                    .group(group.clone())
                    .gap_1p5()
                    .px_2()
                    .py_1p5()
                    .rounded(px(8.))
                    .cursor_pointer()
                    .when(is_current, |s| s.bg(current_bg))
                    .hover(move |s| s.bg(hover_bg))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_panel_note(
                            reader_layout::Panel::Backlinks,
                            &card_rel,
                            None,
                            window,
                            cx,
                        )
                    }))
                    .child(
                        Icon::new(if any_ambiguous {
                            IconName::TriangleAlert
                        } else {
                            IconName::FileText
                        })
                        .small()
                        .text_color(if any_ambiguous {
                            cx.theme().warning
                        } else {
                            p.text_muted
                        }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .max_w(relative(0.6))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.text)
                            .child(title),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_xs()
                            .text_color(faint)
                            .children(location),
                    )
                    .children(via.into_iter().map(|key| {
                        div()
                            .flex_none()
                            .px_1()
                            .rounded(px(4.))
                            .border_1()
                            .border_color(p.border_subtle)
                            .text_size(px(10.5))
                            .text_color(faint)
                            .child(key)
                    }))
                    .when(any_ambiguous, |row| {
                        // Ambiguity is surfaced, never resolved silently.
                        row.child(
                            div()
                                .flex_none()
                                .px_1p5()
                                .rounded(px(4.))
                                .text_size(px(10.5))
                                .bg(cx.theme().warning.opacity(0.15))
                                .text_color(cx.theme().warning)
                                .child("ambiguous"),
                        )
                    })
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(faint)
                            .child(count.to_string()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .opacity(0.)
                            .group_hover(group.clone(), |s| s.opacity(1.))
                            .child(
                                Icon::default()
                                    .path(brand::READER_OPEN_ICON)
                                    .small()
                                    .text_color(p.text_muted),
                            ),
                    );
                let rows = occurrences
                    .into_iter()
                    .take(shown)
                    .enumerate()
                    .map(|(lx, (context, link, jump, external))| {
                        let rel = head.path.clone();
                        let highlight = link
                            .and_then(|r| text_ranges::safe_highlight(&context, r))
                            .map(|r| {
                                (
                                    r,
                                    HighlightStyle {
                                        background_color: Some(mark),
                                        color: Some(p.text),
                                        font_weight: Some(FontWeight::SEMIBOLD),
                                        ..Default::default()
                                    },
                                )
                            });
                        let mut highlights: Vec<_> = highlight.into_iter().collect();
                        highlights.extend(external.iter().map(|(range, _)| {
                            (
                                range.clone(),
                                HighlightStyle {
                                    color: Some(p.link),
                                    ..Default::default()
                                },
                            )
                        }));
                        highlights.sort_by_key(|(range, _)| range.start);
                        let ranges = external.iter().map(|(range, _)| range.clone()).collect();
                        let tooltip_links = external.clone();
                        let text = InteractiveText::new(
                            ("bl-context", lx),
                            StyledText::new(context).with_highlights(highlights),
                        )
                        .on_click(ranges, move |ix, _, cx| {
                            cx.stop_propagation();
                            cx.open_url(&external[ix].1);
                        })
                        .tooltip(move |ix, window, cx| {
                            let (_, url) = tooltip_links.iter().find(|(r, _)| r.contains(&ix))?;
                            Some(
                                gpui_component::tooltip::Tooltip::new(
                                    prepared_links::external_tooltip(url)?,
                                )
                                .build(window, cx),
                            )
                        });
                        let hover_path = head.path.clone();
                        div()
                            .id(SharedString::from(format!("bl-{gx}-{lx}")))
                            .on_hover(cx.listener(move |this, active, window, cx| {
                                let key = format!("backlink:{gx}:{lx}:{hover_path}");
                                if *active {
                                    this.hover_note(
                                        key,
                                        reader_hover::Target {
                                            path: hover_path.clone(),
                                            heading: None,
                                        },
                                        window.mouse_position(),
                                        window,
                                        cx,
                                    );
                                } else {
                                    this.leave_hover(&key, cx);
                                }
                            }))
                            .px_2()
                            .py_1()
                            .rounded(px(6.))
                            .text_size(px(12.5))
                            .line_height(px(18.))
                            .text_color(muted)
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover_bg))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.select_panel_note(
                                    reader_layout::Panel::Backlinks,
                                    &rel,
                                    jump.as_deref(),
                                    window,
                                    cx,
                                )
                            }))
                            .child(text)
                    })
                    .collect::<Vec<_>>();
                v_flex()
                    .mx_1()
                    .mb_1p5()
                    .child(header)
                    .child(
                        v_flex()
                            .ml(px(15.))
                            .pl_2()
                            .border_l_2()
                            .border_color(p.border_subtle)
                            .children(rows)
                            .when(shown < count, |places| {
                                places.child(
                                    div()
                                        .id(SharedString::from(format!("bl-more-{gx}")))
                                        .px_2()
                                        .py_0p5()
                                        .text_xs()
                                        .text_color(faint)
                                        .cursor_pointer()
                                        .hover(move |s| s.text_color(muted))
                                        .child(format!("Show {} more", count - shown))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.backlinks_expanded.insert(more_rel.clone());
                                            cx.notify();
                                        })),
                                )
                            }),
                    )
                    .into_any_element()
            })
            .collect();

        let empty = (items.is_empty() || !self.vault.inventory_scanned).then(|| {
            reader_right_panel::linked_from_empty(
                self.vault.inventory_scanned,
                self.vault.inventory_complete,
            )
        });
        v_flex()
            .id("backlinks")
            .flex_1()
            .min_h(px(96.))
            .overflow_y_scroll()
            .py_1()
            .when_some(empty, |list, line| {
                list.child(panel_empty_line(muted, line))
            })
            .when(empty.is_none(), |list| list.px_1().children(items))
            .into_any_element()
    }

    /// docs/design/reader.md §Right panel: Contents above «Linked from», each
    /// scrolling on its own so neither can hide the other.
    fn render_right_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.active_timeline().is_some() {
            return self.render_timeline(cx);
        }
        let p = brand::palette(cx);
        let faint = brand::reader_palette(cx).text_faint;
        let section = |icon: &'static str, label: String| {
            h_flex()
                .flex_none()
                .gap_1p5()
                .px_4()
                .pt_3()
                .pb_1p5()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(faint)
                .child(Icon::default().path(icon).xsmall().text_color(faint))
                .child(label)
        };
        let linked = self.linked_from_title();
        v_flex()
            .w_full()
            .h_full()
            .flex_none()
            .text_size(px(brand::READER_CHROME_FONT_SIZE))
            .when_some(self.render_properties_section(cx), |panel, props| {
                panel.child(props).child(
                    div()
                        .flex_none()
                        .h(px(1.))
                        .mx_4()
                        .my_1()
                        .bg(p.border_subtle),
                )
            })
            .child(section("icons/list.svg", "Contents".into()))
            .child(self.render_outline(cx))
            .child(
                div()
                    .flex_none()
                    .h(px(1.))
                    .mx_4()
                    .my_1()
                    .bg(p.border_subtle),
            )
            .child(section("icons/link.svg", linked))
            .child(self.render_backlinks(cx))
            .into_any_element()
    }

    fn has_properties(&self) -> bool {
        match &self.properties {
            Ok(props) => !props.is_empty(),
            Err(_) => true,
        }
    }

    /// Properties in the right panel (#386): above Contents, collapsible.
    fn render_properties_section(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use reader_sidebar::Section;
        if !self.has_properties() {
            return None;
        }
        let faint = brand::reader_palette(cx).text_faint;
        let collapsed = self.sidebar.is_collapsed(Section::Properties);
        let summary = match &self.properties {
            Ok(props) => reader_properties::summary(props),
            Err(_) => "unreadable".into(),
        };
        Some(
            v_flex()
                .flex_none()
                .max_h(relative(0.45))
                .child(
                    h_flex()
                        .id("properties-section")
                        .gap_1p5()
                        .px_4()
                        .pt_3()
                        .pb_1p5()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(faint)
                        .cursor_pointer()
                        .child(
                            Icon::new(if collapsed {
                                IconName::ChevronRight
                            } else {
                                IconName::ChevronDown
                            })
                            .xsmall(),
                        )
                        .child(Icon::default().path(READER_SLIDERS_ICON).xsmall())
                        .child("Properties")
                        .when(collapsed, |row| {
                            row.child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .font_weight(FontWeight::NORMAL)
                                    .child(summary),
                            )
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toggle_section(Section::Properties, cx)
                        })),
                )
                .when(!collapsed, |section| {
                    section.child(
                        div()
                            .id("properties-body")
                            .overflow_y_scroll()
                            .px_4()
                            .pb_2()
                            .child(self.render_properties_grid(cx)),
                    )
                })
                .into_any_element(),
        )
    }

    /// Right panel closed (or compact): one summary line above the document
    /// that expands in place (#386).
    fn render_properties_strip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let available = f32::from(self.body_bounds.size.width);
        if !self.has_properties()
            || self
                .panels
                .visible(reader_layout::Panel::Backlinks, available)
        {
            return None;
        }
        let p = brand::palette(cx);
        let tokens = brand::reader_palette(cx);
        let summary = match &self.properties {
            Ok(props) => reader_properties::summary(props),
            Err(_) => "Properties could not be read".into(),
        };
        let open = self.properties_open;
        let header = h_flex()
            .id("properties-strip")
            .gap_2()
            .px_2p5()
            .py_1p5()
            .rounded(px(8.))
            .text_size(px(12.5))
            .text_color(p.text_muted)
            .cursor_pointer()
            .hover(move |d| d.bg(tokens.hover))
            .child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .xsmall()
                .text_color(tokens.text_faint),
            )
            .child(Icon::default().path(READER_SLIDERS_ICON).small())
            .child(if open {
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.text)
                    .child("Properties")
            } else {
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(summary)
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.properties_open = !this.properties_open;
                cx.notify();
            }));
        Some(
            v_flex()
                .id("properties-inline")
                .flex_none()
                .mx(px(READER_SIDE_PADDING - 10.))
                .mt(px(16.))
                .max_h(relative(0.45))
                .when(open, |d| {
                    d.border_1()
                        .border_color(p.border_subtle)
                        .rounded(px(10.))
                        .p_1()
                })
                .child(header)
                .when(open, |d| {
                    d.child(
                        div()
                            .id("properties-inline-body")
                            .overflow_y_scroll()
                            .px_2p5()
                            .pb_2()
                            .text_size(px(brand::READER_CHROME_FONT_SIZE))
                            .child(self.render_properties_grid(cx)),
                    )
                })
                .into_any_element(),
        )
    }

    fn render_properties_grid(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = brand::reader_palette(cx);
        let props = match &self.properties {
            Ok(props) => props,
            Err(error) => {
                return div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(format!("Frontmatter is not valid YAML: {error}"))
                    .into_any_element()
            }
        };
        let hidden = props.iter().filter(|p| p.hidden()).count();
        let today = reader_properties::today();
        let rows = props
            .iter()
            .filter(|p| self.show_hidden_properties || !p.hidden())
            .enumerate()
            .map(|(ix, property)| {
                h_flex()
                    .items_start()
                    .gap_2p5()
                    .py(px(3.))
                    .child(
                        div()
                            .w(px(92.))
                            .flex_none()
                            .pt(px(1.))
                            .text_color(tokens.text_faint)
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(property.key.clone()),
                    )
                    .child(div().flex_1().min_w_0().child(self.render_property_value(
                        &property.key,
                        &property.value,
                        (ix, 0),
                        today,
                        cx,
                    )))
            })
            .collect::<Vec<_>>();
        v_flex()
            .children(rows)
            .when(hidden > 0, |grid| {
                grid.child(
                    div()
                        .id("properties-hidden-toggle")
                        .pt_1p5()
                        .text_xs()
                        .text_color(tokens.text_faint)
                        .cursor_pointer()
                        .child(if self.show_hidden_properties {
                            format!("Hide system properties ({hidden})")
                        } else {
                            format!("Show system properties ({hidden})")
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_hidden_properties = !this.show_hidden_properties;
                            cx.notify();
                        })),
                )
            })
            .into_any_element()
    }

    fn render_property_value(
        &self,
        key: &str,
        value: &tessera_core::properties::PropertyValue,
        id: (usize, usize),
        today: i64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use tessera_core::properties::PropertyValue as V;
        let p = brand::palette(cx);
        let tokens = brand::reader_palette(cx);
        let chip = |text: String, status: bool| {
            h_flex()
                .child(
                    div()
                        .px_2()
                        .rounded_full()
                        .text_xs()
                        .when(status, |d| {
                            d.bg(p.accent.opacity(0.15))
                                .text_color(p.link)
                                .font_weight(FontWeight::MEDIUM)
                        })
                        .when(!status, |d| d.bg(p.surface_raised).text_color(p.text))
                        .child(text),
                )
                .into_any_element()
        };
        match value {
            V::Text(text) | V::Number(text) if reader_properties::chip_key(key) => {
                let text = if key.eq_ignore_ascii_case("tags") || key.eq_ignore_ascii_case("tag") {
                    format!("#{}", text.trim_start_matches('#'))
                } else {
                    text.clone()
                };
                chip(text, reader_properties::status_key(key))
            }
            V::Text(text) | V::Number(text) => div().child(text.clone()).into_any_element(),
            V::Bool(b) => div()
                .child(if *b { "yes" } else { "no" })
                .into_any_element(),
            V::Empty => div()
                .text_color(tokens.text_faint)
                .child("—")
                .into_any_element(),
            V::Date {
                year,
                month,
                day,
                time,
            } => div()
                .child(reader_properties::date_label(
                    *year, *month, *day, *time, today,
                ))
                .into_any_element(),
            V::Url(url) => {
                let shown = url
                    .trim_start_matches("https://")
                    .trim_start_matches("http://")
                    .trim_end_matches('/')
                    .to_owned();
                let open = url.clone();
                let tooltip = prepared_links::external_tooltip(url).unwrap_or_else(|| url.clone());
                div()
                    .id(SharedString::from(format!("prop-url-{}-{}", id.0, id.1)))
                    .text_color(p.link)
                    .cursor_pointer()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(format!("{shown} ↗"))
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
                    })
                    .on_click(move |_, _, cx| cx.open_url(&open))
                    .into_any_element()
            }
            V::Link { target, label } => {
                let resolution = self.vault.resolve_from(target, &self.current_rel);
                let missing = matches!(resolution, tessera_core::vault::Resolution::Unresolved);
                let target = target.clone();
                div()
                    .id(SharedString::from(format!("prop-link-{}-{}", id.0, id.1)))
                    .text_color(if missing { tokens.missing_link } else { p.link })
                    .cursor_pointer()
                    .child(label.clone())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_property_link(&target, window, cx)
                    }))
                    .into_any_element()
            }
            V::List(items) => h_flex()
                .flex_wrap()
                .gap_1()
                .children(items.iter().enumerate().map(|(ix, item)| {
                    let shown = match item {
                        V::Text(text) | V::Number(text) => chip(
                            if key.eq_ignore_ascii_case("tags") || key.eq_ignore_ascii_case("tag") {
                                format!("#{}", text.trim_start_matches('#'))
                            } else {
                                text.clone()
                            },
                            false,
                        ),
                        other => self.render_property_value(key, other, (id.0, ix + 1), today, cx),
                    };
                    shown
                }))
                .into_any_element(),
        }
    }

    /// A property wikilink resolves like a body link from the open note;
    /// ambiguity is offered as choices, never picked silently.
    fn open_property_link(&mut self, target: &str, window: &mut Window, cx: &mut Context<Self>) {
        use tessera_core::vault::Resolution;
        let (path, heading) = match target.split_once('#') {
            Some((path, heading)) => (path, Some(heading.to_owned())),
            None => (target, None),
        };
        match self.vault.resolve_from(path, &self.current_rel) {
            Resolution::Resolved { path } => {
                self.open_note_at(&path, None, heading.as_deref(), window, cx)
            }
            Resolution::Ambiguous { candidates } => {
                self.link_notice = Some(format!("«{path}» names several notes:").into());
                self.link_choices = candidates
                    .into_iter()
                    .map(|c| (c, heading.clone()))
                    .collect();
                cx.notify();
            }
            Resolution::Unresolved => {
                self.link_notice = Some(format!("No note named «{path}».").into());
                cx.notify();
            }
        }
    }

    /// Index into `outline` of the section containing the top visible block.
    fn current_section(&self, cx: &App) -> Option<usize> {
        let top = self
            .content
            .read(cx)
            .list_state()
            .logical_scroll_top()
            .item_ix;
        let after = self
            .outline
            .partition_point(|heading| heading.target.block <= top);
        after.checked_sub(1)
    }

    fn render_outline(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = brand::palette(cx);
        if self.file_preview.is_some() || self.outline.is_empty() {
            return div()
                .flex_none()
                .py_1()
                .child(panel_empty_line(
                    p.text_muted,
                    reader_right_panel::contents_empty(self.file_preview.is_some()),
                ))
                .into_any_element();
        }
        let hover = brand::reader_palette(cx).hover;
        let current = self.current_section(cx);
        let base = self.outline.iter().map(|h| h.level).min().unwrap_or(1);
        v_flex()
            .id("reader-outline")
            .flex_none()
            .max_h(relative(0.6))
            .overflow_y_scroll()
            .py_1()
            .children(self.outline.iter().enumerate().map(|(ix, heading)| {
                let block = heading.target.block;
                let is_current = current == Some(ix);
                let indent = f32::from(heading.level.saturating_sub(base)) * 12.;
                div()
                    .id(("reader-outline-item", ix))
                    .debug_selector(move || format!("reader-outline-row-{ix}"))
                    // Scroll long outlines; never shrink a row below its text.
                    .flex_none()
                    .pl(px(14. + indent))
                    .pr_3()
                    .py(px(3.))
                    .border_l_2()
                    .border_color(if is_current {
                        p.accent
                    } else {
                        gpui::transparent_black()
                    })
                    .text_color(if is_current { p.text } else { p.text_muted })
                    .when(is_current, |d| d.font_weight(FontWeight::MEDIUM))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .hover(move |d| d.bg(hover))
                    .child(
                        div()
                            .debug_selector(move || format!("reader-outline-text-{ix}"))
                            .text_ellipsis()
                            .child(heading.text.clone()),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.scroll_to_block(block, cx);
                        let focus = this.content.read(cx).focus_handle().clone();
                        focus.focus(window, cx);
                    }))
            }))
            .into_any_element()
    }
}

/// The one muted line an empty right-panel section shows under its header
/// (#646), aligned with the section label.
fn panel_empty_line(color: Hsla, text: &'static str) -> AnyElement {
    div()
        .px_4()
        .py_1()
        .text_sm()
        .text_color(color)
        .child(text)
        .into_any_element()
}

const READER_HEADER_HEIGHT: f32 = 46.;
/// Places shown per linking note before «Show N more» (#394).
const BACKLINK_PLACES_SHOWN: usize = 3;

/// A note's display title from its first 16 KiB: first `# ` heading, then a
/// frontmatter `title:`, else `None` (callers fall back to the file name).
fn display_title(path: &Path) -> Option<String> {
    use std::io::Read as _;
    let mut head = Vec::with_capacity(16 * 1024);
    std::fs::File::open(path)
        .ok()?
        .take(16 * 1024)
        .read_to_end(&mut head)
        .ok()?;
    display_title_bytes(&head)
}

fn display_title_source(source: &str) -> Option<String> {
    display_title_bytes(&source.as_bytes()[..source.len().min(16 * 1024)])
}

fn display_title_bytes(head: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(head);
    let mut lines = text.lines();
    let mut frontmatter_title = None;
    if text.starts_with("---") {
        lines.next();
        for line in lines.by_ref() {
            if line.trim() == "---" {
                break;
            }
            if let Some(value) = line.strip_prefix("title:") {
                let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
                if !value.is_empty() {
                    frontmatter_title = Some(value.to_owned());
                }
            }
        }
    }
    let mut fenced = false;
    // Stop at the first H1 instead of collecting the whole head.
    for line in lines {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            if let Some(h1) = line.strip_prefix("# ") {
                let h1 = h1.trim();
                if !h1.is_empty() {
                    return Some(h1.to_owned());
                }
            }
        }
    }
    frontmatter_title
}

/// One row of the browsing sidebar (#369).
#[derive(Clone)]
enum SideItem {
    Header(reader_sidebar::Section, Option<usize>),
    Entry {
        section: reader_sidebar::Section,
        path: String,
        label: String,
        meta: Option<String>,
        location: Option<String>,
        folder: bool,
    },
    Project(tessera_core::projects::Project),
    ProjectsDone(usize, bool),
    More(usize),
    Empty(&'static str),
    Tree(reader_tree::Row),
    #[cfg(unix)]
    Create(Entity<InputState>, usize, bool, Vec<String>, Option<String>),
    #[cfg(unix)]
    CreateError(String, usize),
    #[cfg(unix)]
    Rename(Entity<InputState>, usize, bool),
}

#[cfg(unix)]
fn append_creation_rows(items: &mut Vec<SideItem>, create: &reader_create::Creation, depth: usize) {
    items.push(SideItem::Create(
        create.input.clone(),
        depth,
        create.directory,
        create
            .templates
            .as_ref()
            .map_or_else(Vec::new, |c| c.files.clone()),
        create.selected_template.clone(),
    ));
}

#[derive(Clone, Copy, PartialEq)]
enum TreeKey {
    Down,
    Up,
    Right,
    Left,
    Open,
    ExpandSubtree,
    CollapseSubtree,
}
// Below this width the icon remains; only its shortcut hint is hidden.
const SEARCH_HINT_MIN_PANEL_WIDTH: f32 = 260.;

#[cfg(target_os = "macos")]
const SEARCH_SHORTCUT: &str = "⌘K";
#[cfg(not(target_os = "macos"))]
const SEARCH_SHORTCUT: &str = "Ctrl+K";
#[cfg(target_os = "macos")]
const NOTES_TOOLTIP: &str = "Notes ⌘\\";
#[cfg(not(target_os = "macos"))]
const NOTES_TOOLTIP: &str = "Notes Ctrl+\\";
#[cfg(target_os = "macos")]
const BACKLINKS_TOOLTIP: &str = "On this page ⌥⌘\\";
#[cfg(not(target_os = "macos"))]
const BACKLINKS_TOOLTIP: &str = "On this page Ctrl+Alt+\\";
#[cfg(target_os = "macos")]
const HIDDEN_FILES_TOOLTIP: &str = "Show hidden files ⇧⌘.";
#[cfg(not(target_os = "macos"))]
const HIDDEN_FILES_TOOLTIP: &str = "Show hidden files Ctrl+Shift+.";
const HIDDEN_FILES_MENU: &str = "Show hidden files";
const FOLDERS_HEADER_GROUP: &str = "folders-header";
const COLLAPSE_FOLDERS_TOOLTIP: &str = "Collapse all";
#[cfg(target_os = "macos")]
const FOCUS_CURRENT_TOOLTIP: &str = "Reveal in sidebar";
#[cfg(not(target_os = "macos"))]
const FOCUS_CURRENT_TOOLTIP: &str = "Reveal in sidebar";

#[cfg(target_os = "macos")]
const VAULT_SEARCH_TOOLTIP: &str = "Search in vault (⇧⌘F)";
#[cfg(not(target_os = "macos"))]
const VAULT_SEARCH_TOOLTIP: &str = "Search in vault (Ctrl+Shift+F)";

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

const READER_SLIDERS_ICON: &str = "icons/sliders.svg";

/// A run of header controls. The standalone header lives inside the window's
/// TitleBar, which starts a window move on press and zooms on double-click;
/// the controls handle their press first, then keep it from reaching the bar.
fn header_controls() -> Div {
    h_flex()
        .flex_none()
        .gap(px(2.))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// Icon-only Reader control (docs/design/reader.md §Icons): 28px, ghost, tooltip.
fn reader_icon_button(
    id: impl Into<ElementId>,
    icon: impl Into<Icon>,
    tooltip: &'static str,
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
fn reader_more_menu(
    vault: PathBuf,
    _selected: String,
    show_hidden: bool,
    reader: WeakEntity<Reader>,
    cx: &App,
) -> impl IntoElement {
    use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
    use gpui_component::ThemeMode;
    let current = cx.try_global::<AppearancePreference>().and_then(|p| p.0);
    reader_icon_button("reader-more", IconName::Ellipsis, "More", cx)
        .debug_selector(|| "reader-more".into())
        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
            let appearance = |label: &'static str, mode: Option<ThemeMode>| {
                let vault = vault.clone();
                PopupMenuItem::new(label)
                    .checked(current == mode)
                    .on_click(move |_, window, cx| set_appearance(mode, Some(&vault), window, cx))
            };
            let settings_reader = reader.clone();
            let reader = reader.clone();
            menu.item(PopupMenuItem::new("Settings…").on_click(move |_, _, cx| {
                reader_settings::show(Some(settings_reader.clone()), cx);
            }))
            .separator()
            .when(cfg!(unix), |menu| {
                menu.menu("New note…", Box::new(NewNote))
                    .menu("New Folder", Box::new(NewFolder))
                    .menu("New note from template…", Box::new(NewFromTemplate))
                    .menu("Recover notes…", Box::new(RecoverUnsavedNotes))
                    .menu("Recover link moves…", Box::new(RecoverLinkMoves))
            })
            .separator()
            .menu("Open file…", Box::new(reader_open::OpenFile))
            .menu("Open folder…", Box::new(reader_open::OpenFolder))
            .menu("New Window", Box::new(reader_open::NewWindow))
            .separator()
            .item(
                PopupMenuItem::new(HIDDEN_FILES_MENU)
                    .checked(show_hidden)
                    .on_click(move |_, _, cx| {
                        let _ = reader.update(cx, |this, cx| this.toggle_hidden_files(cx));
                    }),
            )
            .separator()
            .menu("Folders only", Box::new(CollapseSidebarSections))
            .menu("Expand sidebar sections", Box::new(ExpandSidebarSections))
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
fn preserve_reader_selection(id: &'static str, control: impl IntoElement) -> impl IntoElement {
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

#[derive(Clone)]
struct ReaderPanelDrag;
impl Render for ReaderPanelDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl Render for Reader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.toast_subscription.is_none() {
            let notifications = Root::read(window, cx).notification.clone();
            self.toast_subscription = Some(cx.observe(&notifications, |_, _, cx| cx.notify()));
        }
        self.sync_notice_toast(window, cx);
        // Root owns overlay state, but the window content renders these layers.
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);
        let sync_started = std::time::Instant::now();
        self.sync_tree();
        self.sync_sidebar(cx);
        self.restore_ui_tree();
        self.restore_ui_source(window, cx);
        self.record_ui_state(window.is_window_active(), cx);
        self.sync_backlink_titles();
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
        let panel_widths = self.panels.widths(&self.panel_widths, available);
        let bounds_view = cx.entity().downgrade();
        let main = v_flex()
            .id("reader-document")
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
                    .child(self.render_document_surface(window, cx)),
            )
            .when(self.find_open, |s| s.child(self.render_find_bar(cx)));
        let panels: Vec<_> = [reader_layout::Panel::Notes, reader_layout::Panel::Backlinks]
            .into_iter()
            .filter(|panel| self.panels.visible(*panel, available))
            .map(|panel| {
                let left = panel == reader_layout::Panel::Notes;
                let resize_view = cx.entity().downgrade();
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
                    .child(self.render_panel_header(panel, panel_widths.get(panel), cx))
                    .child(div().flex_1().min_h_0().child(if left {
                        self.render_sidebar(window, cx)
                    } else {
                        self.render_right_panel(cx)
                    }))
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
                            .child(
                                gpui_base::resize_handle(
                                    if left {
                                        "reader-notes-resize"
                                    } else {
                                        "reader-backlinks-resize"
                                    },
                                    Axis::Horizontal,
                                )
                                .on_drag(
                                    ReaderPanelDrag,
                                    move |drag, _, _, cx| {
                                        let _ = resize_view.update(cx, |this, cx| {
                                            this.resizing_panel = Some(panel);
                                            cx.notify();
                                        });
                                        cx.new(|_| drag.as_ref().clone())
                                    },
                                ),
                            ),
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
                if self
                    .active_timeline()
                    .is_some_and(|t| t.selected.is_some() || self.editing.is_none())
                    && !self.quick_open.open
                {
                    "Reader ReaderHistory"
                } else {
                    READER_CONTEXT
                },
            )
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &QuickOpen, window, cx| {
                this.open_quick_open(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FullTextSearch, window, cx| {
                this.open_quick_open(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &PaletteNext, _, cx| this.move_quick_open(1, cx)))
            .on_action(cx.listener(|this, _: &PalettePrevious, _, cx| this.move_quick_open(-1, cx)))
            .on_action(cx.listener(|this, _: &NewNote, window, cx| this.new_note(None, window, cx)))
            .on_action(
                cx.listener(|this, _: &NewFolder, window, cx| this.new_folder(None, window, cx)),
            )
            .on_action(cx.listener(|this, _: &RenameNote, window, cx| this.rename_note(window, cx)))
            .map(|view| {
                #[cfg(unix)]
                let view = view.on_action(cx.listener(
                    |this, _: &reader_move_picker::MoveToFolder, window, cx| {
                        this.choose_move_folder(this.selected_file().to_owned(), window, cx);
                    },
                ));
                view
            })
            .when(cfg!(unix), |view| {
                #[cfg(unix)]
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
            .when(cfg!(unix), |view| {
                #[cfg(unix)]
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
            .on_action(cx.listener(|this, _: &SaveSource, _, cx| {
                this.request_source_save(cx);
            }))
            .on_action(cx.listener(|this, _: &RevealFile, window, cx| {
                this.file_action(reader_files::FileAction::Reveal, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CopyVaultPath, window, cx| {
                this.file_action(reader_files::FileAction::Relative, window, cx)
            }))
            .on_action(cx.listener(|this, _: &QuickLookFile, window, cx| {
                this.file_action(reader_files::FileAction::QuickLook, window, cx)
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
                #[cfg(unix)]
                {
                    if this.tree_focus.contains_focused(window, cx) && this.creation_undo.is_some()
                    {
                        this.undo_creation(None, window, cx);
                    } else {
                        this.undo_last_trash(window, cx);
                    }
                }
                #[cfg(not(unix))]
                let _ = (this, window, cx);
            }))
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.dismiss(window, cx)))
            .on_action(cx.listener(|this, _: &TreeDown, w, cx| this.tree_key(TreeKey::Down, w, cx)))
            .on_action(cx.listener(|this, _: &TreeUp, w, cx| this.tree_key(TreeKey::Up, w, cx)))
            .on_action(
                cx.listener(|this, _: &TreeRight, w, cx| this.tree_key(TreeKey::Right, w, cx)),
            )
            .on_action(cx.listener(|this, _: &TreeLeft, w, cx| this.tree_key(TreeKey::Left, w, cx)))
            .on_action(cx.listener(|this, _: &TreeOpen, w, cx| this.tree_key(TreeKey::Open, w, cx)))
            .on_action(cx.listener(|this, _: &TreeExpandSubtree, w, cx| {
                this.tree_key(TreeKey::ExpandSubtree, w, cx)
            }))
            .on_action(cx.listener(|this, _: &TreeCollapseSubtree, w, cx| {
                this.tree_key(TreeKey::CollapseSubtree, w, cx)
            }))
            .on_action(cx.listener(|this, _: &CollapseSidebarSections, _, cx| {
                this.set_sidebar_sections(SectionAction::FoldersOnly, cx)
            }))
            .on_action(cx.listener(|this, _: &ExpandSidebarSections, _, cx| {
                this.set_sidebar_sections(SectionAction::ExpandAll, cx)
            }))
            .on_action(cx.listener(|this, _: &CollapseFolders, _, cx| this.collapse_folders(cx)))
            .on_action(
                cx.listener(|this, _: &FocusCurrentFolder, w, cx| this.focus_current_folder(w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ToggleHiddenFiles, _, cx| this.toggle_hidden_files(cx)),
            )
            .on_action(cx.listener(|this, _: &NewFromTemplate, window, cx| {
                #[cfg(unix)]
                this.new_from_template(window, cx);
                #[cfg(windows)]
                let _ = (this, window, cx);
            }))
            .on_action(cx.listener(|_, _: &reader_settings::OpenSettings, _, cx| {
                reader_settings::show(Some(cx.entity().downgrade()), cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &ToggleNotes, window, cx| {
                this.toggle_panel(reader_layout::Panel::Notes, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleBacklinks, window, cx| {
                this.toggle_panel(reader_layout::Panel::Backlinks, window, cx)
            }))
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
                this.content.read(cx).list_state().scroll_to_reveal_item(0);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ScrollBottom, _, cx| {
                this.content.read(cx).list_state().scroll_to_end();
                cx.notify();
            }))
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .map(|view| {
                let header = self.render_header(available, window, cx);
                if self.embedded_in_workspace {
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
                    view.child(
                        TitleBar::new()
                            .h(px(READER_HEADER_HEIGHT))
                            // Traffic lights are hidden in full screen; drop
                            // the space the toolkit reserves for them (#365).
                            .when(window.is_fullscreen(), |bar| bar.pl(px(10.)))
                            .child(header),
                    )
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
                                            let focus =
                                                this.content.read(cx).focus_handle().clone();
                                            focus.focus(window, cx);
                                        }
                                        cx.defer_in(window, |_, window, cx| {
                                            cx.notify();
                                            window.refresh();
                                        });
                                    }
                                });
                                let drag_view = view.clone();
                                window.on_mouse_event(
                                    move |event: &MouseMoveEvent, phase, _, cx| {
                                        if !phase.bubble() {
                                            return;
                                        }
                                        let _ = drag_view.update(cx, |this, cx| {
                                            let Some(panel) = this.resizing_panel else {
                                                return;
                                            };
                                            if !event.dragging()
                                                || !cx.has_active_drag()
                                                || !this.panels.visible(
                                                    panel,
                                                    f32::from(this.body_bounds.size.width),
                                                )
                                            {
                                                this.resizing_panel = None;
                                                return;
                                            }
                                            let requested = if panel == reader_layout::Panel::Notes
                                            {
                                                event.position.x - this.body_bounds.left()
                                            } else {
                                                this.body_bounds.right() - event.position.x
                                            };
                                            let width = this.panels.resize_width(
                                                panel,
                                                f32::from(requested),
                                                &this.panel_widths,
                                                f32::from(this.body_bounds.size.width),
                                            );
                                            this.set_panel_width(panel, width);
                                            cx.notify();
                                        });
                                    },
                                );
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
            .when(self.quick_open.open, |view| {
                view.child(self.render_quick_open(cx))
            })
            .when(reader_ui_state::installed(cx), |view| {
                view.child(reader_ui_state::capture_scroll(cx.entity().downgrade()))
            })
            .capture_key_down(cx.listener(|_, _, window, cx| {
                reader_ui_state::capture_next_frame(cx.entity().downgrade(), window);
            }))
            .on_modifiers_changed(
                cx.listener(|this, _, window, cx| this.hover_modifiers(window, cx)),
            )
            .capture_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, _, cx| {
                if this.hover_preview.is_active() && !this.hover_preview.contains(event.position) {
                    this.clear_hover(cx);
                }
            }))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                if this.hover_preview.is_active() && !this.hover_preview.contains(event.position) {
                    this.clear_hover(cx);
                }
            }))
            .children(self.render_hover_preview(window, cx))
            .map(|view| {
                #[cfg(unix)]
                let view = view.children(self.render_move_picker());
                view
            })
            .children(dialog_layer)
            .children(notification_layer)
            .children(
                self.file_menu
                    .as_ref()
                    .map(|(menu, position)| anchored().position(*position).child(menu.clone())),
            )
    }
}

const USAGE: &str = "\
tessera — a reader for a plain-Markdown vault

USAGE:
    tessera [FILE.md | FOLDER] [--vault <PATH>] [OPTIONS]

OPTIONS:
    --managed-workspace       Open the saved Brain/workspace chooser explicitly.
    --brain-endpoint <IP:PORT>  Open the AI Brain POC using a loopback backend.
    --vault <PATH>       Read-only vault root (or set TESSERA_VAULT).
    --index-dir <PATH>   Search index location [default: application cache]
    --note <REL>         Note to open, relative to the vault root
    --query <TEXT>       Open full-text search with this query
    --jump               Open the first search hit, scrolled to the match
    --copy-source        Copy selections as Markdown source instead of plain text
    --html               Render the content pane as HTML instead of Markdown
    -h, --help           Print this help
";

fn main() {
    #[cfg(windows)]
    velopack::VelopackApp::build()
        .on_after_install_fast_callback(|_| markdown_handler::install())
        .on_after_update_fast_callback(|_| markdown_handler::install())
        .on_before_uninstall_fast_callback(|_| markdown_handler::uninstall())
        .run();
    let process_start = std::time::Instant::now();
    // Before any thread exists (see `init_local_offset`).
    reader_properties::init_local_offset();
    let offset_elapsed = process_start.elapsed();
    // Restricted pasteboard-only child must not acquire its parent's GUI lock.
    #[cfg(target_os = "macos")]
    if let Some(code) = platform::exact_macos_clipboard::helper_entry() {
        std::process::exit(code);
    }
    let diagnostics = reader_diagnostics::Trace::new_at(None, None, process_start);
    diagnostics.event(
        "local_offset_init",
        serde_json::json!({ "duration_ms": offset_elapsed.as_secs_f64() * 1000. }),
    );
    let defaults = Opts {
        diagnostics: Some(diagnostics.clone()),
        vault: std::env::var_os("TESSERA_VAULT").map(PathBuf::from),
        index_dir: std::env::var_os("TESSERA_INDEX_DIR").map(PathBuf::from),
        ..Opts::default()
    };
    let opts = match reader_open::parse_args(std::env::args_os().skip(1), defaults) {
        Ok(reader_open::Command::Help) => {
            print!("{USAGE}");
            return;
        }
        Ok(reader_open::Command::Launch(opts)) => *opts,
        Err(error) => {
            eprintln!("{error:#}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    #[cfg(windows)]
    let opts = {
        let mut opts = opts;
        // Preserve relative CLI arguments before selecting the packaged HLSL base.
        let cwd = std::env::current_dir().expect("Cannot read launch directory");
        for path in [&mut opts.vault, &mut opts.open_path, &mut opts.index_dir] {
            if let Some(value) = path {
                if value.is_relative() {
                    *value = cwd.join(&*value);
                }
            }
        }
        let executable = std::env::current_exe().expect("Cannot locate Tessera executable");
        std::env::set_current_dir(executable.parent().unwrap())
            .expect("Cannot open Tessera resource directory");
        opts
    };

    let (instance_lock, instance_requests) = if opts.brain_endpoint.is_none()
        && !opts.managed_workspace
    {
        match reader_instance::connect(&opts) {
            Ok(reader_instance::Instance::Forwarded) => return,
            Ok(reader_instance::Instance::Primary(lock, requests)) => (Some(lock), Some(requests)),
            Err(error) => {
                eprintln!("Cannot start Reader: {error:#}");
                std::process::exit(1);
            }
        }
    } else {
        (None, None)
    };
    let _instance_lock = instance_lock;

    let platform_phase = diagnostics.phase("platform_application");
    let clipboard = platform::managed_clipboard();
    let app = gpui_platform::application().with_assets(Assets);
    drop(platform_phase);
    // Register before launch: macOS can deliver files before the launch callback.
    let (open_tx, open_rx) = async_channel::unbounded();
    app.on_open_urls(move |urls| {
        let _ = open_tx.try_send(urls);
    });
    app.run(move |cx| {
        diagnostics.event("app_run_callback", serde_json::json!({}));
        cx.set_global(reader_diagnostics::LaunchTrace(diagnostics.clone()));
        let recovery_phase = diagnostics.phase("recovery_and_window_state");
        if let Ok(directory) = opts
            .session_directory
            .clone()
            .map(Ok)
            .unwrap_or_else(reader_history::state_directory)
        {
            let config_directory = reader_layout::config_base().map(|base| base.join("tessera"));
            reader_recovery::install(&directory, config_directory.as_deref(), cx);
            reader_ui_state::install(&directory, cx);
            window_state::install(directory, cx);
        }
        drop(recovery_phase);
        cx.set_global(platform::ManagedClipboard(clipboard));
        let components_phase = diagnostics.phase("components_init");
        gpui_component::init(cx);
        drop(components_phase);
        let services_phase = diagnostics.phase("menus_and_updater");
        bind_keys(cx);
        reader_open::install(cx);
        reader_open::receive_events(open_rx, cx);
        if let Some(requests) = instance_requests {
            reader_instance::receive(requests, cx);
        }
        #[cfg(all(unix, feature = "brain"))]
        {
            brain::bind_keys(cx);
            brain::source_trace::install(cx);
        }
        updater::install(cx);
        #[cfg(all(unix, feature = "brain"))]
        brain::app_quit::install(cx);
        #[cfg(not(all(unix, feature = "brain")))]
        reader_app_menu::install(cx);
        drop(services_phase);
        let fonts_phase = diagnostics.phase("fonts_load");
        if let Err(error) = brand::load_fonts(cx) {
            eprintln!("Embedded typography unavailable: {error}");
        }
        drop(fonts_phase);
        let appearance_phase = diagnostics.phase("appearance_load");
        let (appearance, theme) = load_appearance(cx);
        cx.set_global(appearance);
        cx.set_global(theme);
        // Native window construction must already see the persisted palette.
        // The per-window callback still handles system appearance updates.
        match cx.global::<AppearancePreference>().0 {
            Some(mode) => Theme::change(mode, None, cx),
            None => Theme::sync_system_appearance(None, cx),
        }

        brand::apply_theme(cx);
        cx.activate(true);
        drop(appearance_phase);

        if (opts.vault.is_some() || opts.open_path.is_some()) && opts.brain_endpoint.is_none() {
            if let Err(error) = reader_open::open_window(opts, cx) {
                eprintln!("Cannot open Reader: {error:#}");
            }
            return;
        }
        if opts.brain_endpoint.is_none() && !opts.managed_workspace {
            reader_startup::launch(opts, cx);
            return;
        }
        #[cfg(all(unix, feature = "brain"))]
        {
            let bounds = Bounds::centered(None, size(px(1500.), px(1000.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(600.), px(400.))),
                kind: WindowKind::Normal,
                // Wayland app id. Also the handle test tooling keys on, so it
                // is part of the contract, not decoration.
                app_id: Some("tessera".into()),
                #[cfg(target_os = "linux")]
                window_background: WindowBackgroundAppearance::Opaque,
                #[cfg(target_os = "linux")]
                window_decorations: Some(WindowDecorations::Client),
                ..TitleBar::window_options()
            };
            let (options, frame_key) = window_state::prepare(options, "managed", cx);
            cx.open_window(options, |window, cx| {
                window.set_window_title("Tessera");
                // Follow the desktop's light/dark preference; `init` forces Light.
                sync_appearance(window, cx);
                // ...and keep following it while the app runs (#37). On Linux
                // gpui sources this from the xdg-desktop-portal setting
                // `org.freedesktop.appearance color-scheme` and re-fires the
                // window callback on every change. `Theme::change` already
                // pushes the Base theme and the TextView defaults; `sync_base`
                // stays as a belt-and-braces call in case a vendor rebase moves
                // that out of `change`. The font overrides set above survive
                // the flip because the stock theme configs carry no font fields.
                window
                    .observe_window_appearance(|window, cx| {
                        sync_appearance(window, cx);
                        window.refresh();
                    })
                    .detach();
                let root = if let Some(endpoint) = opts.brain_endpoint {
                    window.set_window_title("Tessera — AI Brain");
                    let view = cx.new(|cx| brain::BrainView::new(endpoint, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                } else {
                    #[cfg(target_os = "macos")]
                    let opts = Opts {
                        defer_saved_connection: true,
                        ..opts
                    };
                    let workspace = cx.new(|cx| workspace::Workspace::new(opts, window, cx));
                    cx.new(|cx| Root::new(workspace, window, cx))
                };
                window_state::track(&root, frame_key, window, cx);
                root
            })
            .expect("failed to open window");
        }
    });
}

#[cfg(test)]
mod hidden_files_shortcut_tests {
    use super::{
        COLLAPSE_SECTIONS_KEY, HIDDEN_FILES_KEYS, HIDDEN_FILES_KEY_MAC, TREE_COLLAPSE_SUBTREE_KEY,
        TREE_EXPAND_SUBTREE_KEY,
    };
    use gpui::{KeybindingKeystroke, Keystroke, Modifiers};

    fn binding(keys: &str) -> KeybindingKeystroke {
        KeybindingKeystroke::from_keystroke(Keystroke::parse(keys).unwrap())
    }

    #[test]
    fn mac_binding_matches_what_macos_reports_for_shift_cmd_period() {
        // gpui's macOS backend turns ⇧⌘. into the shifted character with
        // shift cleared (gpui-macos events.rs, `chars_with_shift`).
        let typed = Keystroke {
            modifiers: Modifiers {
                platform: true,
                ..Default::default()
            },
            key: ">".into(),
            key_char: None,
        };
        assert!(typed.should_match(&binding(HIDDEN_FILES_KEY_MAC)));
        // Positive control: the binding shipped in 6023 never fired on a Mac.
        assert!(!typed.should_match(&binding("cmd-shift-.")));
    }

    /// The keystroke gpui's macOS backend builds for an arrow key: arrows
    /// keep every modifier as pressed (gpui-macos events.rs, function keys).
    fn mac_arrow(key: &str, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.into(),
            key_char: None,
        }
    }

    #[test]
    fn tree_shortcuts_match_macos_reports() {
        let alt = Modifiers {
            alt: true,
            ..Default::default()
        };
        assert!(mac_arrow("right", alt).should_match(&binding(TREE_EXPAND_SUBTREE_KEY)));
        assert!(mac_arrow("left", alt).should_match(&binding(TREE_COLLAPSE_SUBTREE_KEY)));
        // Positive control: plain → must stay the one-level expand.
        assert!(!mac_arrow("right", Modifiers::default())
            .should_match(&binding(TREE_EXPAND_SUBTREE_KEY)));
        let focus = binding("cmd-shift-left");
        assert!(mac_arrow(
            "left",
            Modifiers {
                platform: true,
                shift: true,
                ..Default::default()
            }
        )
        .should_match(&focus));
        #[cfg(target_os = "macos")]
        assert_eq!(COLLAPSE_SECTIONS_KEY, "cmd-shift-left");
        #[cfg(not(target_os = "macos"))]
        assert!(mac_arrow(
            "left",
            Modifiers {
                control: true,
                shift: true,
                ..Default::default()
            }
        )
        .should_match(&binding(COLLAPSE_SECTIONS_KEY)));
    }

    #[test]
    fn linux_bindings_cover_both_reports() {
        let shifted = Keystroke {
            modifiers: Modifiers {
                control: true,
                shift: true,
                ..Default::default()
            },
            key: ".".into(),
            key_char: None,
        };
        let char_only = Keystroke {
            modifiers: Modifiers::control(),
            key: ">".into(),
            key_char: None,
        };
        for typed in [shifted, char_only] {
            assert!(HIDDEN_FILES_KEYS
                .iter()
                .any(|keys| typed.should_match(&binding(keys))));
        }
    }
}

#[cfg(test)]
mod document_link_landing_tests {
    use super::*;

    #[test]
    fn linked_from_titles_prefer_h1_then_frontmatter_then_name() {
        let dir = std::env::temp_dir().join(format!("tessera-381-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, body: &str| {
            let path = dir.join(name);
            std::fs::write(&path, body).unwrap();
            path
        };
        let h1 = write(
            "a.md",
            "---\ntitle: \"Front\"\n---\n```\n# not a title\n```\n# Real title\n",
        );
        let front = write("b.md", "---\ntitle: Front only\n---\nText.\n");
        let plain = write("c.md", "Just text.\n");
        assert_eq!(display_title(&h1).as_deref(), Some("Real title"));
        assert_eq!(display_title(&front).as_deref(), Some("Front only"));
        assert_eq!(
            display_title(&plain),
            None,
            "positive control: file name fallback"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A point a few pixels inside the first text line of the centred
    /// Reader column (docs/design/reader.md: ≤740px, 40px sides, 44px top).
    fn reader_text_origin(document: Bounds<Pixels>) -> Point<Pixels> {
        let column = document.size.width.min(px(READER_MAX_WIDTH));
        point(
            document.left() + (document.size.width - column) / 2. + px(READER_SIDE_PADDING + 4.),
            document.top() + px(44. + 20.),
        )
    }
    use ::core::prelude::v1::test;
    #[test]
    fn delayed_parse_and_old_list_cannot_satisfy_landing() {
        for attempt in 0..99 {
            assert_eq!(
                reader_landing_state(true, true, 0, 8, attempt),
                ReaderLanding::Waiting
            );
            assert_eq!(
                reader_landing_state(true, false, 100, 8, attempt),
                ReaderLanding::Cancelled
            );
        }
        assert_eq!(
            reader_landing_state(true, true, 3, 8, 99),
            ReaderLanding::Failed
        );
        assert_eq!(
            reader_landing_state(true, true, 9, 8, 99),
            ReaderLanding::Ready
        );
        assert_eq!(
            reader_landing_state(false, true, 9, 8, 10),
            ReaderLanding::Cancelled
        );
    }
    #[gpui::test]
    fn table_alias_fixture_uses_one_three_column_grid(cx: &mut gpui::TestAppContext) {
        struct TableView {
            source: String,
            captured: Arc<std::sync::Mutex<Option<gpui_component::text::TableData>>>,
        }
        impl Render for TableView {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let captured = self.captured.clone();
                div().w(px(600.)).child(
                    TextView::markdown("table-fixture", self.source.clone())
                        .style(reader_text_style(cx.theme()))
                        .table_actions(move |table, _, _| {
                            *captured.lock().unwrap() = Some(table.clone());
                            div()
                                .id("table-end")
                                .debug_selector(|| "table-end".into())
                                .child("After table")
                        }),
                )
            }
        }
        cx.update(gpui_component::init);
        let source = include_str!("../../../fixtures/reader/table-alias-columns.md");
        let vault = Vault::from_note_paths([
            "notes/alpha.md".into(),
            "notes/beta.md".into(),
            "notes/gamma.md".into(),
        ]);
        let source = tessera_core::render::rewrite_source_links(source, &vault, "table.md");
        let captured = Arc::new(std::sync::Mutex::new(None));
        let (_, visual) = cx.add_window_view(|_, _| TableView {
            source,
            captured: captured.clone(),
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let table = captured
            .lock()
            .unwrap()
            .clone()
            .expect("table actually rendered");
        assert_eq!(table.headers.len(), 3);
        assert_eq!(table.rows.len(), 4);
        assert!(table.rows.iter().all(|row| row.len() == 3));
        assert_eq!(table.rows[0][0], "First alias Second alias");
        assert_eq!(table.rows[2], ["Missing cells", "", ""]);
        assert_eq!(table.rows[3][2], "Kept third column.");
        assert!(!table.markdown.contains("Discarded fourth"));
        assert!(
            visual
                .debug_bounds("table-content-frame")
                .unwrap()
                .size
                .height
                > px(100.),
            "long text wraps within the shared grid"
        );
    }

    #[gpui::test]
    fn escape_in_preview_and_source_preserves_docked_and_overlay_panels(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let fixture =
            std::env::temp_dir().join(format!("tessera-escape483-{}", uuid::Uuid::new_v4()));
        let root = fixture.join("vault");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("note.md"), "# Note\n\nKeep reading.\n").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("note.md".into()),
                        session_directory: Some(fixture.join("state")),
                        index_dir: Some(fixture.join("index")),
                        panel_settings_override: Some(fixture.join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        for width in [1366., 700.] {
            visual.simulate_resize(size(px(width), px(768.)));
            visual.run_until_parked();
            for source in [false, true]
                .into_iter()
                .filter(|source| !source || cfg!(unix))
            {
                for panel in [reader_layout::Panel::Notes, reader_layout::Panel::Backlinks] {
                    reader.update_in(visual, |reader, window, cx| {
                        assert_eq!(
                            reader.current_rel, "note.md",
                            "loaded note positive control"
                        );
                        reader.panels.open(panel);
                        if reader.editing.is_some() != source {
                            reader.toggle_source(window, cx);
                        }
                        if !source {
                            reader
                                .content
                                .read(cx)
                                .focus_handle()
                                .clone()
                                .focus(window, cx);
                        }
                        cx.notify();
                    });
                    visual.run_until_parked();
                    let before = reader.read_with(visual, |reader, _| {
                        assert!(
                            reader.panels.visible(panel, width),
                            "open panel positive control"
                        );
                        (
                            reader.panels.visible(reader_layout::Panel::Notes, width),
                            reader
                                .panels
                                .visible(reader_layout::Panel::Backlinks, width),
                        )
                    });
                    let focus = visual.update(|window, cx| window.focused(cx).unwrap());
                    visual.simulate_keystrokes("escape escape");
                    visual.run_until_parked();
                    reader.update_in(visual, |reader, window, _| {
                        assert_eq!(
                            before,
                            (
                                reader.panels.visible(reader_layout::Panel::Notes, width),
                                reader
                                    .panels
                                    .visible(reader_layout::Panel::Backlinks, width)
                            )
                        );
                        assert!(focus.is_focused(window), "Escape preserves the note focus");
                        assert_eq!(reader.editing.is_some(), source);
                    });
                }
            }
        }
        std::fs::remove_dir_all(fixture).unwrap();
    }

    #[gpui::test]
    fn wheel_scrolls_both_document_margins_without_double_scrolling_text(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root = std::env::temp_dir().join(format!("tessera-scroll377-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("long.md"),
            "Paragraph for scrolling.\n\n".repeat(160),
        )
        .unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("long.md".into()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = reader.unwrap();
        visual.simulate_resize(size(px(1800.), px(860.)));
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            v.panels = Default::default();
            cx.notify();
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let document = visual.debug_bounds("reader-document").unwrap();
        let column = visual.debug_bounds("reader-column").unwrap();
        assert!(
            column.left() - document.left() > px(100.),
            "real left margin"
        );
        assert!(
            document.right() - column.right() > px(100.),
            "real right margin"
        );
        let mut positions = Vec::new();
        for x in [
            column.left() + px(100.),
            document.left() + px(20.),
            document.right() - px(20.),
        ] {
            view.update_in(visual, |v, _, cx| {
                v.content
                    .read(cx)
                    .list_state()
                    .scroll_to(ListOffset::default());
                cx.notify();
            });
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.simulate_event(ScrollWheelEvent {
                position: point(x, document.top() + px(120.)),
                delta: ScrollDelta::Pixels(point(px(0.), px(-80.))),
                ..Default::default()
            });
            visual.run_until_parked();
            let offset = view.read_with(visual, |v, cx| {
                v.content.read(cx).list_state().logical_scroll_top()
            });
            assert!(
                offset.item_ix > 0 || offset.offset_in_item > px(0.),
                "wheel positive control"
            );
            positions.push((offset.item_ix, offset.offset_in_item));
        }
        assert_eq!(
            positions[0], positions[1],
            "left margin matches one text wheel event"
        );
        assert_eq!(
            positions[0], positions[2],
            "right margin matches one text wheel event"
        );

        // At compact widths the panels cover document margins. A wheel over
        // either overlay must not reach the document behind it. Closing the
        // panel at the same pointer position is the positive control.
        visual.simulate_resize(size(px(900.), px(860.)));
        for panel in [reader_layout::Panel::Notes, reader_layout::Panel::Backlinks] {
            for open in [true, false] {
                view.update_in(visual, |v, _, cx| {
                    v.panels = Default::default();
                    if open {
                        v.panels.open(panel);
                    }
                    v.content
                        .read(cx)
                        .list_state()
                        .scroll_to(ListOffset::default());
                    cx.notify();
                });
                visual.run_until_parked();
                visual.update(|window, cx| window.draw(cx).clear(cx));
                let document = visual.debug_bounds("reader-document").unwrap();
                let column = visual.debug_bounds("reader-column").unwrap();
                let x = if panel == reader_layout::Panel::Notes {
                    document.left() + px(20.)
                } else {
                    document.right() - px(20.)
                };
                let position = point(x, document.top() + px(120.));
                assert!(!column.contains(&position), "pointer is over a margin");
                if open {
                    let selector = if panel == reader_layout::Panel::Notes {
                        "reader-notes-panel"
                    } else {
                        "reader-backlinks-panel"
                    };
                    assert!(visual.debug_bounds(selector).unwrap().contains(&position));
                }
                visual.simulate_event(ScrollWheelEvent {
                    position,
                    delta: ScrollDelta::Pixels(point(px(0.), px(-80.))),
                    ..Default::default()
                });
                visual.run_until_parked();
                let offset = view.read_with(visual, |v, cx| {
                    v.content.read(cx).list_state().logical_scroll_top()
                });
                let moved = offset.item_ix > 0 || offset.offset_in_item > px(0.);
                assert_eq!(moved, !open, "overlay {panel:?}, open={open}");
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn outline_rows_contain_text_at_every_heading_level(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root =
            std::env::temp_dir().join(format!("tessera-outline417-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let source = (0..36)
            .map(|i| {
                format!(
                    "{} Section — Раздел {}\n\nText.\n\n",
                    "#".repeat(i % 6 + 1),
                    i + 1
                )
            })
            .collect::<String>();
        std::fs::write(root.join("headings.md"), source).unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("headings.md".into()),
                        ..Opts::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = reader.unwrap();
        visual.run_until_parked();
        view.update(visual, |v, cx| {
            assert_eq!(v.outline.len(), 36, "long outline positive control");
            v.panels.open(reader_layout::Panel::Backlinks);
            cx.notify();
        });
        for height in [700., 1000.] {
            visual.simulate_resize(size(px(1400.), px(height)));
            visual.run_until_parked();
            // The first six rendered rows exercise H1-H6, Latin and Cyrillic,
            // while the remaining headings force the outline height limit.
            for (ix, (row_id, text_id)) in [
                ("reader-outline-row-0", "reader-outline-text-0"),
                ("reader-outline-row-1", "reader-outline-text-1"),
                ("reader-outline-row-2", "reader-outline-text-2"),
                ("reader-outline-row-3", "reader-outline-text-3"),
                ("reader-outline-row-4", "reader-outline-text-4"),
                ("reader-outline-row-5", "reader-outline-text-5"),
            ]
            .into_iter()
            .enumerate()
            {
                let row = visual.debug_bounds(row_id).unwrap();
                let text = visual.debug_bounds(text_id).unwrap();
                assert!(text.size.height > px(10.), "text must actually be laid out");
                assert!(
                    row.top() <= text.top()
                        && row.bottom() >= text.bottom()
                        && row.left() <= text.left()
                        && row.right() >= text.right(),
                    "H{} at window height {height}: row {row:?} must contain text {text:?}",
                    ix + 1,
                );
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn reader_outline_follows_scroll_jumps_to_headings_and_clears_without_headings(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root = std::env::temp_dir().join(format!(
            "tessera-reader348-outline-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let filler = "Filler paragraph for scrolling.\n\n".repeat(40);
        let source = format!(
            "# Title\n\n{filler}## Alpha\n\n{filler}### Alpha detail\n\n{filler}```md\n# not a heading\n```\n\nSetext\n------\n\n{filler}## Alpha\n\n{filler}"
        );
        std::fs::write(root.join("long.md"), &source).unwrap();
        std::fs::write(root.join("plain.md"), "Just text, no headings.\n").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("long.md".into()),
                        ..Opts::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = reader.unwrap();
        visual.simulate_resize(size(px(1400.), px(860.)));
        visual.run_until_parked();
        // Real top-level headings in source order: fenced examples excluded,
        // Setext included, repeated labels kept as separate entries.
        let outline = view.read_with(visual, |v, _| {
            v.outline
                .iter()
                .map(|h| (h.text.clone(), h.level))
                .collect::<Vec<_>>()
        });
        assert_eq!(
            outline,
            [
                ("Title".to_string(), 1),
                ("Alpha".to_string(), 2),
                ("Alpha detail".to_string(), 3),
                ("Setext".to_string(), 2),
                ("Alpha".to_string(), 2),
            ]
        );
        view.read_with(visual, |v, cx| assert_eq!(v.current_section(cx), Some(0)));
        // Jumping to the second «Alpha» lands on that exact block, and the
        // highlight follows the rendered scroll position, not the label.
        let block = view.read_with(visual, |v, _| v.outline[4].target.block);
        view.update_in(visual, |v, _, cx| v.scroll_to_block(block, cx));
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(300));
        visual.run_until_parked();
        view.read_with(visual, |v, cx| {
            assert_eq!(
                v.content.read(cx).list_state().logical_scroll_top().item_ix,
                block
            );
            assert_eq!(v.current_section(cx), Some(4));
        });
        // A note without headings clears the previous outline.
        view.update_in(visual, |v, window, cx| {
            v.open_note("plain.md", None, window, cx)
        });
        visual.run_until_parked();
        view.read_with(visual, |v, cx| {
            assert_eq!(v.current_rel, "plain.md");
            assert!(v.outline.is_empty());
            assert_eq!(v.current_section(cx), None);
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[gpui::test]
    fn wide_table_persistent_cues_follow_scroll_and_expand(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root = std::env::temp_dir().join(format!("tessera-reader418-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        // #379 wraps headings: force overflow beyond the per-column floors
        // instead of relying on long headings staying on one line.
        let source = format!(
            "|{}|\n|{}|\n|{}|\n",
            (0..24)
                .map(|i| format!("Long column heading {i}"))
                .collect::<Vec<_>>()
                .join("|"),
            ["---"; 24].join("|"),
            [" "; 24].join("|")
        );
        std::fs::write(root.join("wide.md"), source).unwrap();
        std::fs::write(root.join("narrow.md"), "| A | B |\n|---|---|\n| 1 | 2 |\n").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("wide.md".into()),
                        ..Opts::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.simulate_resize(size(px(1500.), px(1000.)));
        visual.run_until_parked();
        for _ in 0..3 {
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.run_until_parked();
        }
        let track = visual
            .debug_bounds("table-scroll-track")
            .expect("overflow positive control");
        let thumb = visual.debug_bounds("table-scroll-thumb").unwrap();
        assert!(thumb.size.width < track.size.width);
        assert!((thumb.left() - track.left()).abs() < px(1.));
        assert!(visual.debug_bounds("table-shadow-right").is_some());
        assert!(visual.debug_bounds("table-shadow-left").is_none());
        let badge = visual
            .debug_bounds("expand-table-badge")
            .expect("badge visible without hovering table");
        visual.simulate_click(badge.center(), Modifiers::default());
        reader.read_with(visual, |v, _| assert!(v.table_overlay.is_some()));
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        for delta in [-100., -10000.] {
            visual.simulate_event(ScrollWheelEvent {
                position: point(track.center().x, track.top() - px(15.)),
                delta: ScrollDelta::Pixels(point(px(delta), px(0.))),
                ..Default::default()
            });
            for _ in 0..3 {
                visual.update(|window, cx| window.draw(cx).clear(cx));
                visual.run_until_parked();
            }
            let moved = visual.debug_bounds("table-scroll-thumb").unwrap();
            assert!(
                moved.left() > thumb.left(),
                "horizontal wheel actually moved content"
            );
            assert!(visual.debug_bounds("table-shadow-left").is_some());
            if delta == -100. {
                assert!(visual.debug_bounds("table-shadow-right").is_some());
            } else {
                assert!(visual.debug_bounds("table-shadow-right").is_none());
                assert!((moved.right() - track.right()).abs() < px(2.));
            }
        }
        reader.update_in(visual, |v, window, cx| {
            v.open_note("narrow.md", None, window, cx)
        });
        visual.run_until_parked();
        for _ in 0..3 {
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.run_until_parked();
        }
        for selector in [
            "table-scroll-track",
            "table-shadow-left",
            "table-shadow-right",
            "expand-table-badge",
        ] {
            assert!(
                visual.debug_bounds(selector).is_none(),
                "narrow table has no {selector}"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[gpui::test]
    fn reader_table_overlay_opens_closes_and_leaves_on_navigation(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root = std::env::temp_dir().join(format!("tessera-reader368-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("a.md"),
            "# A\n\n| x | y |\n|---|---|\n| 1 | 2 |\n",
        )
        .unwrap();
        std::fs::write(root.join("b.md"), "# B\n").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("a.md".into()),
                        ..Opts::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = reader.unwrap();
        visual.run_until_parked();
        let document = view.read_with(visual, |v, _| v.content.entity_id());
        view.update_in(visual, |v, window, cx| {
            v.open_table_overlay("| x | y |\n|---|---|\n| 1 | 2 |\n", window, cx);
            assert!(v.table_overlay.is_some(), "positive control: overlay opens");
            // Escape closes the overlay before anything else; the document
            // underneath is the same entity, so its position is untouched.
            v.dismiss(window, cx);
            assert!(v.table_overlay.is_none());
            assert_eq!(v.content.entity_id(), document);
            v.open_table_overlay("| x |\n|---|\n| 1 |\n", window, cx);
            v.open_note("b.md", None, window, cx);
        });
        visual.run_until_parked();
        view.read_with(visual, |v, _| {
            assert_eq!(v.current_rel, "b.md");
            assert!(
                v.table_overlay.is_none(),
                "navigation from the overlay closes it"
            );
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[gpui::test]
    fn reader_properties_parse_on_open_and_links_navigate(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root = std::env::temp_dir().join(format!("tessera-reader386-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("plan.md"),
            "---\ntype: Note\nrelated: \"[[menu]]\"\nmissing: \"[[nowhere]]\"\n_id: x\n---\n# Plan\n",
        )
        .unwrap();
        std::fs::write(root.join("menu.md"), "# Menu\n").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("plan.md".into()),
                        ..Opts::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = reader.unwrap();
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            let props = v.properties.as_ref().expect("valid frontmatter");
            let keys: Vec<&str> = props.iter().map(|p| p.key.as_str()).collect();
            assert_eq!(keys, ["type", "related", "missing", "_id"]);
            assert_eq!(
                reader_properties::summary(props),
                "Note · 2 relations",
                "hidden key not counted, both links are relations"
            );
            // An unresolved relation says so and stays put (positive control
            // for the navigation below).
            v.open_property_link("nowhere", window, cx);
            assert_eq!(v.current_rel, "plan.md");
            assert!(v.link_notice.is_some());
            v.open_property_link("menu", window, cx);
        });
        visual.run_until_parked();
        view.read_with(visual, |v, _| {
            assert_eq!(v.current_rel, "menu.md");
            assert!(v.properties.as_ref().unwrap().is_empty(), "menu has none");
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[gpui::test]
    fn empty_sidebar_sections_show_no_zero_count(cx: &mut gpui::TestAppContext) {
        use reader_sidebar::Section;
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| Reader::new(Opts::default(), window, cx));
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = reader.unwrap();
        visual.run_until_parked();
        let counts = |v: &Reader| -> Vec<(Section, Option<usize>)> {
            v.sidebar_items()
                .into_iter()
                .filter_map(|item| match item {
                    SideItem::Header(section, count) => Some((section, count)),
                    _ => None,
                })
                .collect()
        };
        view.update(visual, |v, _| {
            v.loading = None;
            v.vault = Arc::new(Vault::from_note_paths(["Work/Plan.md".to_string()]));
            v.sidebar.recent.clear();
            v.sidebar.pinned.clear();
            v.inbox.clear();
            assert_eq!(
                counts(v),
                [
                    (Section::Recent, None),
                    (Section::Pinned, None),
                    (Section::Inbox, None),
                    (Section::Projects, None),
                    (Section::Folders, None),
                ]
            );
            // Positive control: a filled section still carries its count.
            v.sidebar.recent.push(("Work/Plan.md".into(), 0));
            v.sidebar.pinned.push("Work/Plan.md".into());
            v.inbox.push(reader_sidebar::InboxItem {
                path: "Work/Plan.md".into(),
                created: 0,
                reason: reader_sidebar::InboxReason::NoIncomingLinks,
                domain: None,
            });
            assert_eq!(
                counts(v),
                [
                    (Section::Recent, Some(1)),
                    (Section::Pinned, Some(1)),
                    (Section::Inbox, Some(1)),
                    (Section::Projects, None),
                    (Section::Folders, None),
                ]
            );
        });
    }

    #[gpui::test]
    fn recursive_folder_actions_preserve_scroll_and_take_keyboard_focus(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| Reader::new(Opts::default(), window, cx));
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = reader.unwrap();
        visual.simulate_resize(size(px(1400.), px(860.)));
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            v.loading = None;
            v.vault = Arc::new(Vault::from_note_paths(
                (0..1000).map(|i| format!("Big/Sub/Note {i}.md")),
            ));
            v.current_rel = "Big/Sub/Note 0.md".into();
            v.panels.open(reader_layout::Panel::Notes);
            v.sync_tree();
            v.tree.set_subtree("Big", true);
            cx.notify();
        });
        visual.run_until_parked();
        // Start below the top, with keyboard focus in the document rather
        // than the tree. A mouse subtree action must establish both anchors.
        view.update_in(visual, |v, window, cx| {
            v.content.read(cx).focus_handle().clone().focus(window, cx);
            assert!(!v.tree_focus.is_focused(window));
            v.tree_scroll
                .scroll_to_item_strict(800, ScrollStrategy::Top);
            cx.notify();
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert!(v.tree_scroll.0.borrow().base_handle.offset().y < px(-1000.));
            v.set_folder_subtree("Big", false, window, cx);
            assert!(v.tree_focus.is_focused(window));
        });
        visual.run_until_parked();
        view.read_with(visual, |v, _| {
            assert_eq!(v.tree.rows.len(), 1);
            assert_eq!(v.tree_scroll.0.borrow().base_handle.offset().y, px(0.));
        });
        visual.simulate_keystrokes("alt-right");
        visual.run_until_parked();
        view.read_with(visual, |v, _| assert_eq!(v.tree.rows.len(), 1002));
        view.update_in(visual, |v, _, cx| {
            v.tree_scroll
                .scroll_to_item_strict(800, ScrollStrategy::Top);
            cx.notify();
        });
        visual.run_until_parked();
        view.update(visual, |v, cx| {
            assert!(v.tree_scroll.0.borrow().base_handle.offset().y < px(-1000.));
            v.collapse_folders(cx);
        });
        visual.run_until_parked();
        view.read_with(visual, |v, _| {
            assert_eq!(v.tree_scroll.0.borrow().base_handle.offset().y, px(0.));
        });
        // Focus current restores a collapsed Folders section.
        view.update_in(visual, |v, window, cx| {
            v.sidebar.collapsed.insert(reader_sidebar::Section::Folders);
            v.focus_current_folder(window, cx);
            assert_eq!(v.tree.cursor.as_deref(), Some("Big/Sub/Note 0.md"));
            assert!(!v
                .sidebar
                .collapsed
                .contains(&reader_sidebar::Section::Folders));
        });
    }

    #[gpui::test]
    fn sections_follow_show_hidden_files_live(cx: &mut gpui::TestAppContext) {
        use reader_sidebar::{InboxItem, InboxReason, Section};
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| Reader::new(Opts::default(), window, cx));
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = reader.unwrap();
        visual.run_until_parked();
        let notes = ["Visible.md", ".Dot note.md", "_Hidden/Inside.md"];
        let section_paths = |v: &Reader, section: Section| -> Vec<String> {
            v.sidebar_items()
                .into_iter()
                .filter_map(|item| match item {
                    SideItem::Entry {
                        section: s, path, ..
                    } if s == section => Some(path),
                    _ => None,
                })
                .collect()
        };
        let header_count = |v: &Reader, section: Section| {
            v.sidebar_items().into_iter().find_map(|item| match item {
                SideItem::Header(s, count) if s == section => count,
                _ => None,
            })
        };
        view.update(visual, |v, cx| {
            v.loading = None;
            v.vault = Arc::new(Vault::from_note_paths(notes.map(String::from)));
            v.sync_tree();
            v.sidebar.show_hidden = true;
            v.tree.set_show_hidden(true);
            for (at, path) in notes.iter().enumerate() {
                v.sidebar.record_open(path, at as u64);
                v.sidebar.toggle_pin(path);
            }
            v.sidebar.toggle_pin("_Hidden");
            v.inbox = notes
                .iter()
                .map(|path| InboxItem {
                    path: path.to_string(),
                    created: reader_sidebar::now(),
                    reason: InboxReason::VaultRoot,
                    domain: None,
                })
                .collect();
            // Positive control: with Show hidden files on, every section lists
            // the hidden notes, so their absence below is the filter's doing.
            for section in [Section::Recent, Section::Pinned, Section::Inbox] {
                let paths = section_paths(v, section);
                assert!(
                    paths.iter().any(|p| p == ".Dot note.md")
                        && paths.iter().any(|p| p == "_Hidden/Inside.md"),
                    "{section:?} with hidden files shown: {paths:?}"
                );
            }
            assert!(section_paths(v, Section::Pinned).contains(&"_Hidden".to_string()));
            // Switching it off hides them everywhere at once, as in the tree.
            v.toggle_hidden_files(cx);
            assert!(!v
                .tree
                .rows
                .iter()
                .any(|row| row.path.starts_with(['.', '_'])));
            for section in [Section::Recent, Section::Pinned, Section::Inbox] {
                assert_eq!(
                    section_paths(v, section),
                    ["Visible.md"],
                    "{section:?} with hidden files off"
                );
                assert_eq!(header_count(v, section), Some(1), "{section:?} count");
            }
            // And back on without a rescan or recomputation.
            v.toggle_hidden_files(cx);
            assert_eq!(section_paths(v, Section::Inbox).len(), notes.len());
            assert_eq!(header_count(v, Section::Pinned), Some(notes.len() + 1));
            // Hidden-only collections are empty on screen: their headers stay
            // present, but #687 must not show either zero or the raw hidden count.
            v.sidebar.recent.retain(|(path, _)| path != "Visible.md");
            v.sidebar.pinned.retain(|path| path != "Visible.md");
            v.inbox.retain(|item| item.path != "Visible.md");
            v.toggle_hidden_files(cx);
            for section in [Section::Recent, Section::Pinned, Section::Inbox] {
                assert!(section_paths(v, section).is_empty());
                assert!(
                    v.sidebar_items()
                        .iter()
                        .any(|item| { matches!(item, SideItem::Header(s, None) if *s == section) }),
                    "{section:?} header remains without a count"
                );
            }
        });
    }

    #[gpui::test]
    fn sidebar_headers_stay_visible_and_restore_sections(cx: &mut gpui::TestAppContext) {
        use reader_sidebar::Section;
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| Reader::new(Opts::default(), window, cx));
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = reader.unwrap();
        visual.simulate_resize(size(px(1400.), px(700.)));
        visual.run_until_parked();
        view.update(visual, |v, cx| {
            v.loading = None;
            v.vault = Arc::new(Vault::from_note_paths(
                (0..300).map(|i| format!("Note {i}.md")),
            ));
            v.panels.open(reader_layout::Panel::Notes);
            v.sync_tree();
            v.sidebar.pinned = (0..200).map(|i| format!("Note {i}.md")).collect();
            v.sidebar.record_open("Note 0.md", 1);
            v.sidebar.collapsed.insert(Section::Inbox);
            cx.notify();
        });
        visual.run_until_parked();
        let headers_visible = |visual: &mut gpui::VisualTestContext| {
            let panel = visual.debug_bounds("reader-notes-panel").unwrap();
            for section in [
                "sidebar-header-Recent",
                "sidebar-header-Pinned",
                "sidebar-header-Inbox",
                "sidebar-header-Folders",
            ] {
                let b = visual.debug_bounds(section).unwrap();
                assert!(
                    b.origin.y >= panel.origin.y && b.bottom() <= panel.bottom(),
                    "{section}: {b:?} outside {panel:?}"
                );
                assert_eq!(b.size.height, px(28.));
            }
        };
        headers_visible(visual);
        assert!(
            visual
                .debug_bounds("sidebar-body-Pinned")
                .unwrap()
                .size
                .height
                > px(28.)
        );
        view.update(visual, |v, cx| {
            v.tree_scroll
                .scroll_to_item_strict(150, ScrollStrategy::Top);
            cx.notify();
        });
        visual.run_until_parked();
        headers_visible(visual);
        view.read_with(visual, |v, _| {
            assert!(
                v.scroll_sections.compact,
                "positive control: real rendered scroll folds sections"
            );
            assert!(v
                .scroll_sections
                .closed(Section::Pinned, &v.sidebar.collapsed));
            assert!(!v.sidebar.collapsed.contains(&Section::Pinned));
        });
        let header = visual.debug_bounds("sidebar-header-Pinned").unwrap();
        visual.simulate_click(header.center(), Modifiers::default());
        visual.run_until_parked();
        headers_visible(visual);
        view.read_with(visual, |v, _| {
            assert!(!v
                .scroll_sections
                .closed(Section::Pinned, &v.sidebar.collapsed));
            assert!(v.tree_scroll.0.borrow().base_handle.offset().y < px(-100.));
        });
        view.update(visual, |v, cx| {
            v.tree_scroll.scroll_to_item_strict(0, ScrollStrategy::Top);
            cx.notify();
        });
        visual.run_until_parked();
        headers_visible(visual);
        view.read_with(visual, |v, _| {
            assert!(!v.scroll_sections.compact);
            assert!(!v
                .scroll_sections
                .closed(Section::Recent, &v.sidebar.collapsed));
            assert!(!v
                .scroll_sections
                .closed(Section::Pinned, &v.sidebar.collapsed));
            assert!(v
                .scroll_sections
                .closed(Section::Inbox, &v.sidebar.collapsed));
        });
        // A short tree fits only after the upper sections fold. Its clamped
        // zero offset must not oscillate, and an upward wheel must restore it.
        view.update(visual, |v, cx| {
            v.vault = Arc::new(Vault::from_note_paths(
                (0..15).map(|i| format!("Note {i}.md")),
            ));
            v.sync_tree();
            cx.notify();
        });
        visual.run_until_parked();
        let tree = visual.debug_bounds("sidebar-tree-body").unwrap();
        visual.simulate_event(ScrollWheelEvent {
            position: tree.center(),
            delta: ScrollDelta::Pixels(point(px(0.), px(-100.))),
            ..Default::default()
        });
        visual.run_until_parked();
        view.read_with(visual, |v, _| {
            assert!(
                v.scroll_sections.compact,
                "short-tree wheel positive control"
            );
            assert_eq!(v.tree_scroll.0.borrow().base_handle.offset().y, px(0.));
        });
        let tree = visual.debug_bounds("sidebar-tree-body").unwrap();
        visual.simulate_event(ScrollWheelEvent {
            position: tree.center(),
            delta: ScrollDelta::Pixels(point(px(0.), px(100.))),
            ..Default::default()
        });
        visual.run_until_parked();
        view.read_with(visual, |v, _| assert!(!v.scroll_sections.compact));
        // Inbox has its own virtual list: both lines must fit without clipping
        // a neighbouring row or shrinking the persistent section headers.
        view.update(visual, |v, cx| {
            v.sidebar.collapsed.remove(&Section::Inbox);
            v.sidebar.pinned.clear();
            v.inbox = ["Work/Projects/Idea.md", "Work/Resources/README.md"]
                .into_iter()
                .map(|path| reader_sidebar::InboxItem {
                    path: path.into(),
                    created: 1,
                    reason: reader_sidebar::InboxReason::NoIncomingLinks,
                    domain: None,
                })
                .collect();
            let entries: Vec<_> = v
                .sidebar_items()
                .into_iter()
                .filter_map(|item| match item {
                    SideItem::Entry {
                        section: Section::Inbox,
                        label,
                        location,
                        ..
                    } => Some((label, location)),
                    _ => None,
                })
                .collect();
            assert_eq!(
                entries,
                vec![
                    ("Idea".into(), Some("Work / Projects".into())),
                    ("Resources".into(), Some("Work".into())),
                ]
            );
            cx.notify();
        });
        visual.run_until_parked();
        headers_visible(visual);
        let body = visual.debug_bounds("sidebar-body-Inbox").unwrap();
        assert_eq!(body.size.height, px(88.));
        let mut previous_bottom = body.origin.y;
        for (row_selector, location_selector) in [
            (
                "side-Inbox-Work/Projects/Idea.md",
                "side-Inbox-Work/Projects/Idea.md-location",
            ),
            (
                "side-Inbox-Work/Resources/README.md",
                "side-Inbox-Work/Resources/README.md-location",
            ),
        ] {
            let row = visual.debug_bounds(row_selector).unwrap();
            let location = visual.debug_bounds(location_selector).unwrap();
            assert_eq!(row.size.height, px(44.));
            assert!(row.origin.y >= previous_bottom && row.bottom() <= body.bottom());
            assert!(location.origin.y >= row.origin.y && location.bottom() <= row.bottom());
            previous_bottom = row.bottom();
        }
    }

    #[gpui::test]
    fn sidebar_explicit_sections_survive_save_close_launch_with_scrolled_tree(
        cx: &mut gpui::TestAppContext,
    ) {
        use reader_sidebar::{Section, LEFT_SECTIONS};
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        for i in 0..100 {
            std::fs::write(root.join(format!("Note {i:03}.md")), "# Note").unwrap();
        }
        for launch in 0..2 {
            let mut entity = None;
            let (_, visual) = cx.add_window_view(|window, cx| {
                let view = cx.new(|cx| {
                    Reader::new(
                        Opts {
                            vault: Some(root.clone()),
                            note: Some("Note 090.md".into()),
                            index_dir: Some(fixture.path().join("cache")),
                            session_directory: Some(fixture.path().join("state")),
                            panel_settings_override: Some(fixture.path().join("panels.json")),
                            ..Default::default()
                        },
                        window,
                        cx,
                    )
                });
                entity = Some(view.clone());
                Root::new(view, window, cx)
            });
            let view = entity.unwrap();
            visual.simulate_resize(size(px(1400.), px(960.)));
            visual.run_until_parked();
            view.update(visual, |v, cx| {
                v.panels.open(reader_layout::Panel::Notes);
                v.tree_revealed.clear();
                cx.notify();
            });
            visual.run_until_parked();
            if launch == 0 {
                view.update(visual, |v, cx| {
                    v.panels.open(reader_layout::Panel::Notes);
                    v.sidebar.pinned = vec!["Note 001.md".into()];
                    v.tree_scroll.scroll_to_item_strict(80, ScrollStrategy::Top);
                    cx.notify();
                });
                visual.run_until_parked();
                view.update(visual, |v, cx| {
                    v.set_sidebar_sections(SectionAction::ExpandAll, cx)
                });
                visual.run_until_parked();
            }
            view.read_with(visual, |v, _| {
                assert!(
                    v.tree_scroll.0.borrow().base_handle.offset().y < px(-100.),
                    "scrolled positive control on launch {launch}"
                );
                for section in LEFT_SECTIONS {
                    let closed = if section == Section::Projects {
                        v.sidebar.projects_collapsed
                    } else {
                        v.scroll_sections.closed(section, &v.sidebar.collapsed)
                    };
                    assert!(!closed, "{section:?} closed on launch {launch}");
                }
            });
            view.update_in(visual, |_, window, _| window.remove_window());
            visual.run_until_parked();
        }
    }

    #[gpui::test]
    fn sidebar_bulk_controls_restore_alt_click_and_shortcuts(cx: &mut gpui::TestAppContext) {
        use reader_sidebar::{Section, LEFT_SECTIONS};
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(root.join("Folder")).unwrap();
        std::fs::write(root.join("Folder/Note.md"), "# Note").unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Folder/Note.md".into()),
                        index_dir: Some(temp.path().join("cache")),
                        session_directory: Some(temp.path().join("state")),
                        panel_settings_override: Some(temp.path().join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = entity.unwrap();
        visual.run_until_parked();
        visual.simulate_resize(size(px(1400.), px(960.)));
        visual.run_until_parked();
        view.update(visual, |v, cx| {
            v.panels.open(reader_layout::Panel::Notes);
            v.sidebar.toggle_section(Section::Pinned);
            v.scroll_sections.honor_expanded(&v.sidebar.collapsed);
            cx.notify();
        });
        visual.run_until_parked();
        let before = view.update(visual, |v, _| {
            v.scroll_sections.observe(-120.);
            LEFT_SECTIONS.map(|s| {
                if s == Section::Projects {
                    v.sidebar.projects_collapsed
                } else {
                    v.scroll_sections.closed(s, &v.sidebar.collapsed)
                }
            })
        });
        let toggle = visual.debug_bounds("sidebar-folders-only").unwrap();
        visual.simulate_click(toggle.center(), Modifiers::default());
        visual.run_until_parked();
        view.update(visual, |v, _| {
            v.scroll_sections.observe(-120.);
            assert_eq!(
                LEFT_SECTIONS.map(|s| if s == Section::Projects {
                    v.sidebar.projects_collapsed
                } else {
                    v.scroll_sections.closed(s, &v.sidebar.collapsed)
                }),
                [true, true, true, true, false]
            )
        });
        visual.simulate_click(toggle.center(), Modifiers::default());
        visual.run_until_parked();
        view.update(visual, |v, _| {
            v.scroll_sections.observe(-120.);
            assert_eq!(
                LEFT_SECTIONS.map(|s| if s == Section::Projects {
                    v.sidebar.projects_collapsed
                } else {
                    v.scroll_sections.closed(s, &v.sidebar.collapsed)
                }),
                before
            )
        });
        let folders = visual.debug_bounds("sidebar-header-Folders").unwrap();
        // Click the chevron area, not the neighbouring folder actions.
        visual.simulate_click(
            point(folders.left() + px(10.), folders.center().y),
            Modifiers {
                alt: true,
                ..Default::default()
            },
        );
        visual.run_until_parked();
        view.update(visual, |v, _| {
            v.scroll_sections.observe(-120.);
            assert!(LEFT_SECTIONS
                .into_iter()
                .all(|s| if s == Section::Projects {
                    v.sidebar.projects_collapsed
                } else {
                    v.scroll_sections.closed(s, &v.sidebar.collapsed)
                }))
        });
        let folders = visual.debug_bounds("sidebar-header-Folders").unwrap();
        visual.simulate_click(
            point(folders.left() + px(10.), folders.center().y),
            Modifiers {
                alt: true,
                ..Default::default()
            },
        );
        visual.run_until_parked();
        view.update(visual, |v, _| {
            v.scroll_sections.observe(-120.);
            assert!(LEFT_SECTIONS
                .into_iter()
                .all(|s| !if s == Section::Projects {
                    v.sidebar.projects_collapsed
                } else {
                    v.scroll_sections.closed(s, &v.sidebar.collapsed)
                }))
        });
        view.update_in(visual, |v, window, cx| {
            v.content.read(cx).focus_handle().clone().focus(window, cx);
        });
        visual.simulate_keystrokes(COLLAPSE_SECTIONS_KEY);
        visual.run_until_parked();
        view.update(visual, |v, _| {
            v.scroll_sections.observe(-120.);
            assert_eq!(
                LEFT_SECTIONS.map(|s| if s == Section::Projects {
                    v.sidebar.projects_collapsed
                } else {
                    v.scroll_sections.closed(s, &v.sidebar.collapsed)
                }),
                [true, true, true, true, false]
            )
        });
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-shift-right"
        } else {
            "ctrl-shift-right"
        });
        visual.run_until_parked();
        view.update(visual, |v, _| {
            v.scroll_sections.observe(-120.);
            assert!(LEFT_SECTIONS
                .into_iter()
                .all(|s| !if s == Section::Projects {
                    v.sidebar.projects_collapsed
                } else {
                    v.scroll_sections.closed(s, &v.sidebar.collapsed)
                }))
        });
        let collapse = visual.debug_bounds("folders-collapse-all").unwrap();
        visual.simulate_click(collapse.center(), Modifiers::default());
        visual.run_until_parked();
        view.update(visual, |v, _| {
            v.scroll_sections.observe(-120.);
            assert!(!v.sidebar.is_collapsed(Section::Folders));
            assert!(!v.tree.rows.iter().any(|row| row.path == "Folder/Note.md"));
        });
        assert_eq!(
            std::fs::read_to_string(root.join("Folder/Note.md")).unwrap(),
            "# Note"
        );
    }

    #[gpui::test]
    fn projects_sidebar_refreshes_and_opens_canonical_note(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let first = "Work/Projects/One/_index.md";
        let done = "Home/Projects/Done/_index.md";
        let source = "---\ntype: project\nstatus: active\n---\n# One\n- [ ] Next\n";
        for path in [first, done] {
            std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
        }
        std::fs::write(root.join(first), source).unwrap();
        std::fs::write(root.join(done), source.replace("active", "closed")).unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some(first.into()),
                        index_dir: Some(temp.path().join("cache")),
                        session_directory: Some(temp.path().join("state")),
                        panel_settings_override: Some(temp.path().join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = entity.unwrap();
        visual.simulate_resize(size(px(1400.), px(960.)));
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            v.panels.open(reader_layout::Panel::Notes);
            v.sidebar.projects_collapsed = false;
            v.projects_done_expanded = false;
            v.sync_tree();
            cx.notify();
            let _ = window;
        });
        visual.run_until_parked();
        view.read_with(visual, |v, _| {
            use reader_sidebar::Section;
            let headers: Vec<_> = v
                .sidebar_items()
                .into_iter()
                .filter_map(|i| {
                    if let SideItem::Header(s, _) = i {
                        Some(s)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(
                headers,
                [
                    Section::Recent,
                    Section::Pinned,
                    Section::Inbox,
                    Section::Projects,
                    Section::Folders
                ]
            );
            assert_eq!(v.projects.rows().len(), 2);
            assert!(v
                .sidebar_items()
                .iter()
                .any(|i| matches!(i, SideItem::ProjectsDone(1, false))));
            assert_eq!(
                v.sidebar_items()
                    .iter()
                    .filter(|i| matches!(i, SideItem::Project(_)))
                    .count(),
                1
            );
        });
        assert!(
            visual
                .debug_bounds("side-project-Work/Projects/One/_index.md")
                .is_some(),
            "positive control: active row rendered"
        );
        let button = visual.debug_bounds("projects-done").unwrap();
        visual.simulate_click(button.center(), Modifiers::default());
        visual.run_until_parked();
        let row = visual
            .debug_bounds("side-project-Home/Projects/Done/_index.md")
            .unwrap();
        visual.simulate_click(row.center(), Modifiers::default());
        visual.run_until_parked();
        view.read_with(visual, |v, _| assert_eq!(v.current_rel, done));
        assert_eq!(std::fs::read_to_string(root.join(first)).unwrap(), source);
        std::fs::write(
            root.join(first),
            source.replace("active", "planned").replace("[ ]", "[x]"),
        )
        .unwrap();
        view.update_in(visual, |v, window, cx| {
            v.start_incremental(
                tessera_core::Changes {
                    changed: [first.into()].into_iter().collect(),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        visual.run_until_parked();
        view.read_with(visual, |v, _| {
            let p = v.projects.rows().iter().find(|p| p.path == first).unwrap();
            assert_eq!(p.status, tessera_core::projects::Status::Planned);
            assert_eq!(p.open_tasks, 0);
        });
    }

    #[gpui::test]
    fn reader_folder_tree_reveals_current_and_panels_survive_selection(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root =
            std::env::temp_dir().join(format!("tessera-reader348-tree-{}", uuid::Uuid::new_v4()));
        for dir in ["Work/Projects/Cafe", "Home/Empty"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("Work/Projects/Cafe/Plan.md"), "# Plan\n\nText.\n").unwrap();
        std::fs::write(root.join("Work/Projects/Cafe/Menu.md"), "# Menu\n").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Work/Projects/Cafe/Plan.md".into()),
                        ..Opts::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = reader.unwrap();
        visual.simulate_resize(size(px(1400.), px(860.)));
        view.update_in(visual, |v, window, cx| {
            v.panel_settings = None;
            v.toggle_panel(reader_layout::Panel::Notes, window, cx);
        });
        visual.run_until_parked();
        let paths = |visual: &mut gpui::VisualTestContext| {
            view.read_with(visual, |v, _| {
                v.tree
                    .rows
                    .iter()
                    .map(|r| r.path.clone())
                    .collect::<Vec<_>>()
            })
        };
        // Real folders, current ancestry expanded, the empty folder retained.
        assert_eq!(
            paths(visual),
            [
                "Home",
                "Work",
                "Work/Projects",
                "Work/Projects/Cafe",
                "Work/Projects/Cafe/Menu.md",
                "Work/Projects/Cafe/Plan.md",
            ]
        );
        // Docked: choosing a note keeps the navigator open.
        view.update_in(visual, |v, window, cx| {
            let row = v.tree.rows[4].clone();
            v.activate_tree_row(&row, window, cx);
        });
        visual.run_until_parked();
        view.read_with(visual, |v, _| {
            assert_eq!(v.current_rel, "Work/Projects/Cafe/Menu.md");
            assert!(v.panels.visible(
                reader_layout::Panel::Notes,
                f32::from(v.body_bounds.size.width)
            ));
        });
        // Folder rows toggle; the expanded Home folder shows its empty child.
        view.update_in(visual, |v, window, cx| {
            let row = v.tree.rows[0].clone();
            v.activate_tree_row(&row, window, cx);
        });
        visual.run_until_parked();
        assert!(paths(visual).contains(&"Home/Empty".to_string()));
        // Automatic compact entry hides the dock; explicitly reopening it
        // creates an overlay, and choosing a note dismisses it (#677).
        visual.simulate_resize(size(px(800.), px(860.)));
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert!(!v.panels.visible(
                reader_layout::Panel::Notes,
                f32::from(v.body_bounds.size.width)
            ));
            v.focus_sidebar_search(window, cx);
            v.select_panel_note(
                reader_layout::Panel::Notes,
                "Work/Projects/Cafe/Plan.md",
                None,
                window,
                cx,
            );
        });
        visual.run_until_parked();
        view.read_with(visual, |v, _| {
            assert_eq!(v.current_rel, "Work/Projects/Cafe/Plan.md");
            assert!(!v.panels.visible(
                reader_layout::Panel::Notes,
                f32::from(v.body_bounds.size.width)
            ));
        });
        view.update_in(visual, |v, window, cx| v.focus_sidebar_search(window, cx));
        visual.run_until_parked();
        assert!(visual.debug_bounds("reader-notes-panel").is_some());
        view.update_in(visual, |v, window, cx| {
            let row = v
                .tree
                .rows
                .iter()
                .find(|r| r.path == "Work/Projects/Cafe/Menu.md")
                .unwrap()
                .clone();
            v.activate_tree_row(&row, window, cx);
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("reader-notes-panel").is_none());
        view.update_in(visual, |v, window, cx| {
            assert_eq!(v.current_rel, "Work/Projects/Cafe/Menu.md");
            assert!(v.content.read(cx).focus_handle().is_focused(window));
            assert!(v.panels.notes, "wide sidebar preference survives selection");
            v.open_note("Work/Projects/Cafe/Plan.md", None, window, cx);
        });
        // Breadcrumbs (#366): with the sidebar closed, a folder crumb
        // shows the tree, selects and expands the folder.
        visual.simulate_resize(size(px(1400.), px(860.)));
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            v.close_panel(reader_layout::Panel::Notes, window, cx);
            v.tree.toggle("Work/Projects");
        });
        visual.run_until_parked();
        view.read_with(visual, |v, _| {
            assert!(!v.panels.visible(
                reader_layout::Panel::Notes,
                f32::from(v.body_bounds.size.width)
            ));
            assert!(!v.tree.rows.iter().any(|r| r.path == "Work/Projects/Cafe"));
        });
        view.update_in(visual, |v, window, cx| {
            v.reveal_in_tree("Work/Projects", window, cx)
        });
        visual.run_until_parked();
        view.read_with(visual, |v, _| {
            assert!(v.panels.visible(
                reader_layout::Panel::Notes,
                f32::from(v.body_bounds.size.width)
            ));
            assert_eq!(v.tree.cursor.as_deref(), Some("Work/Projects"));
            assert!(v.tree.rows.iter().any(|r| r.path == "Work/Projects/Cafe"));
        });
        // The current-note crumb reveals the note itself.
        view.update_in(visual, |v, window, cx| {
            let current = v.current_rel.clone();
            v.reveal_in_tree(&current, window, cx)
        });
        visual.run_until_parked();
        view.read_with(visual, |v, _| {
            assert_eq!(v.tree.cursor.as_deref(), Some("Work/Projects/Cafe/Plan.md"));
        });
        // Compact search lives in the header and drops only its hint when narrow.
        let header_button = visual.debug_bounds("sidebar-search").unwrap();
        let panel = visual.debug_bounds("reader-notes-panel").unwrap();
        assert!(header_button.bottom() <= panel.top() + px(READER_HEADER_HEIGHT));
        assert!(visual.debug_bounds("sidebar-search-shortcut").is_some());
        assert!(visual.debug_bounds("reader-close-notes-preserve").is_none());
        view.update(visual, |v, cx| {
            v.panel_widths.notes = 200.;
            cx.notify();
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("sidebar-search").is_some());
        assert!(visual.debug_bounds("sidebar-search-shortcut").is_none());
        view.update(visual, |v, cx| {
            v.panel_widths.notes = 280.;
            cx.notify();
        });
        visual.run_until_parked();
        let saved_panels = view.read_with(visual, |v, _| v.panels.clone());
        visual.simulate_resize(size(px(800.), px(860.)));
        visual.run_until_parked();
        for side in [reader_layout::Panel::Notes, reader_layout::Panel::Backlinks] {
            view.update(visual, |v, cx| {
                v.panels.open(side);
                cx.notify();
            });
            visual.run_until_parked();
            let backdrop = visual
                .debug_bounds("reader-panel-backdrop")
                .expect("overlay positive control");
            assert!(visual
                .debug_bounds("reader-close-backlinks-preserve")
                .is_none());
            visual.simulate_click(
                point(backdrop.center().x, backdrop.top() + px(80.)),
                Modifiers::default(),
            );
            visual.run_until_parked();
            view.read_with(visual, |v, _| assert!(!v.panels.visible(side, 800.)));
            view.update(visual, |v, cx| {
                v.panels.open(side);
                cx.notify();
            });
            visual.run_until_parked();
            visual.simulate_keystrokes("escape");
            visual.run_until_parked();
            view.read_with(visual, |v, _| assert!(v.panels.visible(side, 800.)));
            visual.simulate_keystrokes(if side == reader_layout::Panel::Notes {
                "ctrl-\\"
            } else {
                "ctrl-alt-\\"
            });
            visual.run_until_parked();
            view.read_with(visual, |v, _| assert!(!v.panels.visible(side, 800.)));
        }
        view.update(visual, |v, cx| {
            v.panels = saved_panels;
            cx.notify();
        });
        visual.simulate_resize(size(px(1400.), px(860.)));
        visual.run_until_parked();
        // The sidebar entry opens quick open; searching never filters the tree.
        let before = paths(visual);
        let button = visual
            .debug_bounds("sidebar-search")
            .expect("search button rendered");
        visual.simulate_click(button.center(), Modifiers::default());
        view.update_in(visual, |v, window, cx| {
            assert!(v.quick_open.open);
            assert!(v
                .quick_open
                .input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window));
            v.quick_open
                .input
                .update(cx, |input, cx| input.set_value("Menu", window, cx));
        });
        visual.run_until_parked();
        assert_eq!(paths(visual), before);
        view.update_in(visual, |v, window, cx| v.close_quick_open(window, cx));
        // Keyboard activation shares the same palette; merely focusing the
        // sidebar does not open it.
        view.update_in(visual, |v, window, cx| {
            v.focus_sidebar_search(window, cx);
            assert!(!v.quick_open.open);
        });
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert!(v.quick_open.open);
            v.close_quick_open(window, cx);
        });
        visual.simulate_keystrokes("ctrl-k");
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert!(v.quick_open.open);
            v.close_quick_open(window, cx);
        });
        // Sections (#369): Recent remembers opens, pins toggle, and fresh
        // notes without incoming links are computed into Inbox.
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            assert_eq!(
                v.sidebar.recent.first().map(|(p, _)| p.as_str()),
                Some("Work/Projects/Cafe/Plan.md")
            );
            assert!(v
                .sidebar
                .recent
                .iter()
                .any(|(p, _)| p == "Work/Projects/Cafe/Menu.md"));
            assert!(matches!(
                v.sidebar_items().first(),
                Some(SideItem::Header(reader_sidebar::Section::Recent, _))
            ));
            v.toggle_pin("Work/Projects", cx);
            assert!(v.sidebar.is_pinned("Work/Projects"));
            let inbox: Vec<_> = v
                .inbox
                .iter()
                .map(|i| (i.path.as_str(), i.reason))
                .collect();
            assert!(
                inbox.contains(&(
                    "Work/Projects/Cafe/Plan.md",
                    reader_sidebar::InboxReason::NoIncomingLinks
                )),
                "fresh unlinked note in Inbox: {inbox:?}"
            );
            v.toggle_section(reader_sidebar::Section::Folders, cx);
            assert!(!v
                .sidebar_items()
                .iter()
                .any(|item| matches!(item, SideItem::Tree(..))));
            v.toggle_section(reader_sidebar::Section::Folders, cx);
        });
        // Find keeps docked panels open and Escape clears/closes it.
        visual.simulate_resize(size(px(1400.), px(860.)));
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            v.panels.open(reader_layout::Panel::Notes);
            v.panels.open(reader_layout::Panel::Backlinks);
            let width = f32::from(v.body_bounds.size.width);
            assert!(v.panels.visible(reader_layout::Panel::Notes, width));
            v.open_find(window, cx);
            assert!(v.panels.visible(reader_layout::Panel::Notes, width));
            assert!(v.panels.visible(reader_layout::Panel::Backlinks, width));
            v.find_input
                .update(cx, |input, cx| input.set_value("Text", window, cx));
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert!(v.find_open, "positive control: find is open");
            v.dismiss(window, cx);
            assert!(v.find_open);
            assert!(v.find_input.read(cx).value().is_empty());
            v.dismiss(window, cx);
            assert!(!v.find_open);
        });
        view.update_in(visual, |v, window, cx| {
            // Find marks follow the find field the same way.
            v.open_find(window, cx);
            v.find_input
                .update(cx, |input, cx| input.set_value("Text", window, cx));
            v.run_find(window, cx);
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert!(v.content.read(cx).search_status().1 > 0);
            v.find_input
                .update(cx, |input, cx| input.set_value("", window, cx));
            v.run_find(window, cx);
            assert_eq!(v.content.read(cx).search_status().1, 0);
            v.close_find(window, cx);
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[gpui::test]
    fn reader_compact_clamp_preserves_nonzero_document_position(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root =
            std::env::temp_dir().join(format!("tessera-reader321-scroll-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        // With the test font this is tall at the correct document width but
        // fits a transient full-window width, exposing ListState's top clamp.
        let source = format!(
            "# Wrapped selection\n\n{}\n",
            (0..400)
                .map(|i| format!("word{i:03}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        std::fs::write(root.join("wrapped.md"), &source).unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("wrapped.md".into()),
                        ..Opts::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = reader.unwrap();
        visual.simulate_resize(size(px(1600.), px(860.)));
        view.update_in(visual, |v, window, cx| {
            v.panel_settings = None;
            v.panel_widths = reader_layout::Widths {
                notes: 662.8242,
                backlinks: 355.79297,
            };
            v.toggle_panel(reader_layout::Panel::Backlinks, window, cx);
            v.toggle_panel(reader_layout::Panel::Notes, window, cx);
        });
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            v.content.read(cx).list_state().scroll_by(px(180.));
            cx.notify();
        });
        visual.run_until_parked();
        let before = view.read_with(visual, |v, cx| {
            let p = v.content.read(cx).list_state().logical_scroll_top();
            (
                v.content.entity_id(),
                v.current_rel.clone(),
                v.vault_root.clone(),
                v.history.clone(),
                p.item_ix,
                p.offset_in_item,
            )
        });
        assert!(
            before.4 > 0 || before.5 > px(0.),
            "nonzero position positive control"
        );
        let wide = visual.debug_bounds("reader-document").unwrap();
        visual.simulate_resize(size(px(600.), px(860.)));
        visual.run_until_parked();
        assert!(visual.debug_bounds("reader-notes-panel").is_none());
        view.update_in(visual, |v, window, cx| v.focus_sidebar_search(window, cx));
        visual.run_until_parked();
        assert_eq!(
            visual
                .debug_bounds("reader-notes-panel")
                .unwrap()
                .size
                .width,
            px(552.)
        );
        view.read_with(visual, |v, cx| {
            let position = v.content.read(cx).list_state().logical_scroll_top();
            assert_eq!(
                (position.item_ix, position.offset_in_item),
                (before.4, before.5)
            );
        });
        visual.simulate_resize(size(px(1600.), px(860.)));
        visual.run_until_parked();
        let after = view.read_with(visual, |v, cx| {
            assert_eq!(v.panel_widths.notes, 662.8242);
            assert_eq!(v.panel_widths.backlinks, 355.79297);
            assert_eq!(v.note_source, source);
            let p = v.content.read(cx).list_state().logical_scroll_top();
            (
                v.content.entity_id(),
                v.current_rel.clone(),
                v.vault_root.clone(),
                v.history.clone(),
                p.item_ix,
                p.offset_in_item,
            )
        });
        assert_eq!(visual.debug_bounds("reader-document").unwrap(), wide);
        assert_eq!(
            after, before,
            "settled compact roundtrip must preserve document position"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn reader_wrapped_selection_survives_notes_reflow_and_invalidates_on_new_input(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root =
            std::env::temp_dir().join(format!("tessera-reader321-wrap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let source = (0..600)
            .map(|ix| format!("word{ix:03} "))
            .collect::<String>();
        assert!(!source.contains('\n'), "one paragraph must wrap visually");
        std::fs::write(root.join("wrapped.md"), &source).unwrap();
        std::fs::write(
            root.join("replacement.md"),
            "Replacement document has different content.",
        )
        .unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("wrapped.md".into()),
                        ..Opts::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = reader.unwrap();
        visual.simulate_resize(size(px(1366.), px(768.)));
        view.update_in(visual, |v, window, cx| {
            v.panel_settings = None;
            v.panel_widths = reader_layout::Widths::default();
            v.toggle_panel(reader_layout::Panel::Notes, window, cx);
        });
        visual.run_until_parked();
        let document = visual.debug_bounds("reader-document").unwrap();
        let origin = reader_text_origin(visual.debug_bounds("reader-column").unwrap());
        let first = origin;
        let later = origin + point(px(0.), px(70.));
        let select = |visual: &mut gpui::VisualTestContext, start: Point<Pixels>| {
            let end = start + point(px(300.), px(0.));
            visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
            visual.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
            visual.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
            visual.run_until_parked();
        };
        select(visual, first);
        let first_text = view.read_with(visual, |v, cx| v.content.read(cx).selected_text());
        assert!(!first_text.trim().is_empty());
        select(visual, later);
        let selected = view.read_with(visual, |v, cx| v.content.read(cx).selected_text());
        assert!(
            !selected.trim().is_empty(),
            "later wrapped-line positive control"
        );
        assert_eq!(
            source.matches(selected.trim()).count(),
            1,
            "unique selection: {selected:?}"
        );
        let selected_range = source.find(selected.trim()).unwrap()
            ..source.find(selected.trim()).unwrap() + selected.trim().len();
        assert!(
            selected_range.start
                > source.find(first_text.trim()).unwrap() + first_text.trim().len(),
            "selection is on a later visual line"
        );
        let entity = view.read_with(visual, |v, _| v.content.entity_id());
        let anchor = visual.debug_bounds("reader-notes-splitter-anchor").unwrap();
        let start = point(anchor.left(), anchor.top() + px(140.));
        visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        view.read_with(visual, |v, cx| {
            assert_eq!(v.content.read(cx).selected_text(), selected)
        });
        visual.simulate_mouse_move(
            start + point(px(35.), px(0.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.simulate_mouse_move(
            start + point(px(200.), px(0.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.simulate_mouse_up(
            start + point(px(200.), px(0.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("reader-document").unwrap().size.width
                < document.size.width - px(100.)
        );
        view.read_with(visual, |v, cx| {
            assert_eq!(
                v.content.read(cx).selected_text(),
                selected,
                "settled wrapped reflow after real Notes resize"
            )
        });
        for _ in 0..2 {
            let control = visual.debug_bounds("reader-notes-preserve").unwrap();
            visual.simulate_click(control.center(), Modifiers::default());
            visual.run_until_parked();
            view.read_with(visual, |v, cx| {
                let current = v.content.read(cx).selected_text();
                assert_eq!(current, selected, "settled registration after Notes toggle");
                assert_eq!(
                    source.find(current.trim()).unwrap()
                        ..source.find(current.trim()).unwrap() + current.trim().len(),
                    selected_range
                );
                assert_eq!(v.content.entity_id(), entity);
            });
        }
        for distance in [80., -80.] {
            let before = view.update_in(visual, |v, _, cx| {
                let list = v.content.read(cx).list_state();
                let before = list.logical_scroll_top();
                list.scroll_by(px(distance));
                cx.notify();
                before
            });
            visual.run_until_parked();
            view.read_with(visual, |v, cx| {
                let after = v.content.read(cx).list_state().logical_scroll_top();
                assert_ne!(
                    (after.item_ix, after.offset_in_item),
                    (before.item_ix, before.offset_in_item),
                    "scroll positive control"
                );
                assert_eq!(
                    v.content.read(cx).selected_text(),
                    selected,
                    "settled scroll projection preserves range"
                );
            });
        }
        // A new ordinary gesture must replace the settled byte range.
        let document = visual.debug_bounds("reader-column").unwrap();
        select(visual, reader_text_origin(document));
        let next = view.read_with(visual, |v, cx| v.content.read(cx).selected_text());
        assert!(!next.trim().is_empty());
        assert_ne!(next, selected);
        assert_eq!(source.matches(next.trim()).count(), 1);
        view.update_in(visual, |v, window, cx| {
            v.open_note("replacement.md", None, window, cx)
        });
        visual.run_until_parked();
        view.read_with(visual, |v, cx| {
            assert!(
                v.content.read(cx).selected_text().is_empty(),
                "content replacement invalidates selection"
            )
        });
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn reader_actual_large_heading_navigation_uses_new_parse_and_restores_back(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root = std::env::temp_dir().join(format!("tessera-reader314-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let start = format!(
            "# Origin\n\n{}[Target](target.md#Landing)\n",
            (0..30)
                .map(|ix| format!("{ix:02} unique paragraph.\n\n"))
                .collect::<String>()
        );
        let target = format!(
            "# Target\n\n{}## Landing\n\n{}",
            "Preceding content deliberately exceeds asynchronous parsing threshold.\n\n"
                .repeat(100),
            "Following content.\n\n".repeat(100)
        );
        assert!(target.len() > 4096);
        std::fs::write(root.join("start.md"), &start).unwrap();
        std::fs::write(root.join("target.md"), &target).unwrap();
        let mut reader = None;
        let (_root_view, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("start.md".into()),
                        ..Opts::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = reader.unwrap();
        view.update_in(visual, |v, _, _| v.panel_settings = None);
        visual.run_until_parked();
        let origin = ListOffset {
            item_ix: 5,
            offset_in_item: px(7.),
        };
        view.update_in(visual, |v, _, cx| {
            v.content.read(cx).list_state().scroll_to(origin);
            cx.notify();
        });
        visual.run_until_parked();
        let (old, origin) = view.update_in(visual, |v, _, cx| {
            assert!(
                v.content.read(cx).list_state().item_count() > 5,
                "old-list positive control"
            );
            (
                v.content.entity_id(),
                v.content.read(cx).list_state().logical_scroll_top(),
            )
        });
        // The actual rendered topology, both native drag directions, selection
        // and the document identity must survive docking/overlay transitions.
        visual.simulate_resize(size(px(1366.), px(768.)));
        view.update_in(visual, |v, window, cx| {
            v.toggle_panel(reader_layout::Panel::Notes, window, cx);
            v.toggle_panel(reader_layout::Panel::Backlinks, window, cx);
        });
        visual.run_until_parked();
        let left = visual.debug_bounds("reader-notes-panel").unwrap();
        let document = visual.debug_bounds("reader-document").unwrap();
        let right = visual.debug_bounds("reader-backlinks-panel").unwrap();
        assert!(left.right() <= document.left());
        assert!(document.right() <= right.left());
        assert!(document.size.width >= px(reader_layout::DOCUMENT_MIN_WIDTH));
        // Select a measured visible paragraph, not a fixed y-coordinate that
        // can land in the inter-paragraph gap after layout changes.
        let text_start = view.read_with(visual, |v, cx| {
            let list = v.content.read(cx).list_state();
            let block = list
                .bounds_for_item(list.logical_scroll_top().item_ix + 1)
                .unwrap();
            block.origin + point(px(4.), px(10.))
        });
        let text_end = text_start + point(px(85.), px(0.));
        visual.simulate_mouse_down(text_start, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_move(text_end, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(text_end, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        let selection = view.read_with(visual, |v, cx| v.content.read(cx).selected_text());
        assert!(
            !selection.is_empty() && selection.len() < 100,
            "partial-selection positive control: {selection:?}"
        );
        assert_eq!(
            start.matches(selection.trim()).count(),
            1,
            "selected substring identifies one source range, not repeated text"
        );
        // Probe both edges while the splitters are still mounted. An inside
        // press preserves the pre-existing selection; an adjacent outside
        // press must retain ordinary clearing behavior.
        for anchor in [
            "reader-notes-splitter-anchor",
            "reader-backlinks-splitter-anchor",
        ] {
            let edge = visual
                .debug_bounds(anchor)
                .expect("live splitter anchor")
                .left();
            for dx in [-3.75, 3.75] {
                let inside = point(edge + px(dx), left.bottom() - px(20.));
                visual.simulate_mouse_down(inside, MouseButton::Left, Modifiers::default());
                view.read_with(visual, |v, cx| {
                    assert_eq!(
                        v.content.read(cx).selected_text(),
                        selection,
                        "inside {anchor} {dx}"
                    )
                });
                visual.simulate_mouse_up(inside, MouseButton::Left, Modifiers::default());
            }
            for dx in [-4.25, 4.25] {
                let outside = point(edge + px(dx), left.bottom() - px(20.));
                visual.simulate_mouse_down(outside, MouseButton::Left, Modifiers::default());
                view.read_with(visual, |v, cx| {
                    assert!(
                        v.content.read(cx).selected_text().is_empty(),
                        "outside live splitter {dx}"
                    )
                });
                visual.simulate_mouse_up(outside, MouseButton::Left, Modifiers::default());
                // Restore the fixture viewport for the next independent
                // boundary case; ordinary outside clicks may move focus/scroll.
                view.update_in(visual, |v, _, cx| {
                    v.content.read(cx).list_state().scroll_to(origin);
                    cx.notify();
                });
                visual.run_until_parked();
                visual.simulate_mouse_down(text_start, MouseButton::Left, Modifiers::default());
                visual.simulate_mouse_move(text_end, MouseButton::Left, Modifiers::default());
                visual.simulate_mouse_up(text_end, MouseButton::Left, Modifiers::default());
                view.read_with(visual, |v, cx| {
                    assert_eq!(
                        v.content.read(cx).selected_text(),
                        selection,
                        "reselect after {anchor} {dx}"
                    )
                });
            }
        }
        for (panel, start, delta) in [
            (
                reader_layout::Panel::Notes,
                point(left.right() - px(2.), left.top() + px(100.)),
                70.,
            ),
            (
                reader_layout::Panel::Backlinks,
                point(right.left(), right.top() + px(100.)),
                -70.,
            ),
        ] {
            visual.simulate_mouse_move(start, None, Modifiers::default());
            visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
            view.read_with(visual, |v, cx| {
                assert_eq!(
                    v.content.read(cx).selected_text(),
                    selection,
                    "after MouseDown {panel:?}"
                )
            });
            visual.simulate_mouse_move(
                start + point(px(delta / 3.), px(0.)),
                MouseButton::Left,
                Modifiers::default(),
            );
            visual.run_until_parked();
            visual.simulate_mouse_move(
                start + point(px(delta), px(0.)),
                MouseButton::Left,
                Modifiers::default(),
            );
            visual.run_until_parked();
            visual.simulate_mouse_up(
                start + point(px(delta), px(0.)),
                MouseButton::Left,
                Modifiers::default(),
            );
            visual.run_until_parked();
            view.read_with(visual, |v, cx| {
                assert_eq!(
                    v.content.read(cx).selected_text(),
                    selection,
                    "selection retained through actual drag"
                );
                assert!(
                    v.panel_widths.get(panel) > 320.,
                    "real side-specific drag: {panel:?} width {}",
                    v.panel_widths.get(panel)
                )
            });
        }
        // Real mouse title-bar toggles must not clear or replace the range.
        for id in [
            "reader-notes-preserve",
            "reader-notes-preserve",
            "reader-backlinks-preserve",
            "reader-backlinks-preserve",
        ] {
            let control = visual.debug_bounds(id).expect("visible Reader control");
            visual.simulate_mouse_down(control.center(), MouseButton::Left, Modifiers::default());
            view.read_with(visual, |v, cx| {
                assert_eq!(
                    v.content.read(cx).selected_text(),
                    selection,
                    "control MouseDown {id}, {control:?}"
                )
            });
            visual.simulate_mouse_up(control.center(), MouseButton::Left, Modifiers::default());
            visual.run_until_parked();
            view.read_with(visual, |v, cx| {
                assert_eq!(
                    v.content.read(cx).selected_text(),
                    selection,
                    "mouse control {id}"
                )
            });
        }
        for width in [666., 1366., 640., 1366., 1000., 1366.] {
            visual.simulate_resize(size(px(width), px(720.)));
            visual.run_until_parked();
            let document = visual.debug_bounds("reader-document").unwrap();
            if width < 1000. {
                let state = view.read_with(visual, |v, _| (v.body_bounds, v.panels.clone()));
                assert!(
                    visual.debug_bounds("reader-notes-panel").is_none(),
                    "compact state: {state:?}"
                );
                assert!(visual.debug_bounds("reader-backlinks-panel").is_none());
                assert_eq!(document.left(), state.0.left());
                assert_eq!(document.right(), state.0.right());
                assert!(state.1.notes && state.1.backlinks, "wide choices retained");
            } else {
                let right = visual.debug_bounds("reader-backlinks-panel").unwrap();
                let left = visual.debug_bounds("reader-notes-panel").unwrap();
                assert!(left.right() <= document.left());
                assert!(document.right() <= right.left());
                assert!(document.size.width >= px(479.9));
            }
            view.read_with(visual, |v, cx| {
                assert_eq!(v.content.entity_id(), old);
                assert_eq!(v.content.read(cx).selected_text(), selection);
            });
        }
        // Switch the compact overlay to Notes and verify the opposite edge.
        visual.simulate_resize(size(px(640.), px(720.)));
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| v.focus_sidebar_search(window, cx));
        visual.run_until_parked();
        let left = visual.debug_bounds("reader-notes-panel").unwrap();
        let document = visual.debug_bounds("reader-document").unwrap();
        assert_eq!(left.left(), document.left());
        assert!(visual.debug_bounds("reader-backlinks-panel").is_none());
        view.read_with(visual, |v, cx| {
            assert_eq!(v.content.read(cx).selected_text(), selection)
        });
        visual.simulate_resize(size(px(1366.), px(768.)));
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            v.close_panel(reader_layout::Panel::Notes, window, cx);
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("reader-notes-panel").is_none());
        assert!(visual.debug_bounds("reader-backlinks-panel").is_some());
        view.read_with(visual, |v, cx| {
            assert_eq!(v.content.read(cx).selected_text(), selection)
        });
        // Lose the release of a control gesture; a subsequent ordinary press
        // must still clear selection after the panel has been dismissed.
        let close = visual.debug_bounds("reader-backlinks-preserve").unwrap();
        visual.simulate_mouse_down(close.center(), MouseButton::Left, Modifiers::default());
        view.read_with(visual, |v, cx| {
            assert_eq!(v.content.read(cx).selected_text(), selection)
        });
        view.update_in(visual, |v, window, cx| {
            v.close_panel(reader_layout::Panel::Backlinks, window, cx);
            v.panel_widths = reader_layout::Widths::default();
            v.content.read(cx).list_state().scroll_to(origin);
        });
        visual.run_until_parked();
        // Removed controls leave no stale exemption. An ordinary document
        // MouseDown at the former splitter position still clears selection.
        visual.simulate_click(
            point(left.right() - px(2.), left.top() + px(100.)),
            Modifiers::default(),
        );
        visual.run_until_parked();
        view.read_with(visual, |v, cx| {
            assert!(
                v.content.read(cx).selected_text().is_empty(),
                "ordinary document click clears selection"
            )
        });
        // #321: toggling UI panels must not replace the parsed document or its
        // navigation/scroll state. Closing must not leave focus in a hidden control.
        let history_before = view.read_with(visual, |v, _| v.history.clone());
        view.update_in(visual, |v, window, cx| {
            v.toggle_panel(reader_layout::Panel::Notes, window, cx);
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, _| {
            assert!(v.sidebar_search_focus.is_focused(window));
            assert_eq!(v.content.entity_id(), old);
            assert_eq!(v.history, history_before);
        });
        for (width, height) in [(640., 720.), (1366., 768.), (640., 720.)] {
            visual.simulate_resize(size(px(width), px(height)));
            visual.run_until_parked();
            if width < reader_layout::DOCK_MIN_WIDTH {
                view.update_in(visual, |v, window, cx| {
                    assert!(!v.panels.visible(reader_layout::Panel::Notes, width));
                    assert!(!v.sidebar_search_focus.is_focused(window));
                    assert!(v.content.read(cx).focus_handle().is_focused(window));
                    v.focus_sidebar_search(window, cx);
                });
                visual.run_until_parked();
            }
            view.update_in(visual, |v, window, _| {
                assert_eq!(v.content.entity_id(), old);
                assert_eq!(v.history, history_before);
                assert!(v.panels.visible(
                    reader_layout::Panel::Notes,
                    f32::from(v.body_bounds.size.width)
                ));
                assert!(v.sidebar_search_focus.is_focused(window));
            });
        }
        // Drive the actual toolkit drag handle, not a direct state setter.
        let preference = root.with_extension("layout.json");
        let handle = view.update_in(visual, |v, _, _| {
            v.panel_settings = Some(preference.clone());
            point(
                v.body_bounds.left()
                    + px(reader_layout::displayed_width(
                        v.panel_widths.notes,
                        f32::from(v.body_bounds.size.width),
                    ))
                    - px(2.),
                v.body_bounds.top() + px(100.),
            )
        });
        visual.simulate_mouse_move(handle, None, Modifiers::default());
        visual.simulate_mouse_down(handle, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_move(
            handle + point(px(20.), px(0.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.run_until_parked();
        visual.simulate_mouse_move(
            handle + point(px(80.), px(0.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.run_until_parked();
        visual.simulate_mouse_up(
            handle + point(px(80.), px(0.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.run_until_parked();
        view.update_in(visual, |v, _, _| {
            assert!(
                v.panel_widths.notes > reader_layout::DEFAULT_PANEL_WIDTH + 40.,
                "real drag changed width"
            );
            assert!(v.resizing_panel.is_none());
            assert_eq!(
                reader_layout::Widths::load(&preference).notes,
                v.panel_widths.notes
            );
            assert_eq!(v.content.entity_id(), old);
            assert_eq!(v.history, history_before);
        });
        // Close during a real drag must not let ordinary motion resize a
        // reopened/different panel after its release was lost.
        let handle = view.read_with(visual, |v, _| {
            point(
                v.body_bounds.left() + px(v.panel_widths.notes) - px(2.),
                v.body_bounds.top() + px(100.),
            )
        });
        visual.simulate_mouse_down(handle, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_move(
            handle + point(px(20.), px(0.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert!(v.resizing_panel.is_some(), "active drag positive control");
            v.close_panel(reader_layout::Panel::Notes, window, cx);
            assert!(v.resizing_panel.is_none());
            v.toggle_panel(reader_layout::Panel::Backlinks, window, cx);
        });
        visual.run_until_parked();
        visual.simulate_mouse_move(point(px(100.), px(300.)), None, Modifiers::default());
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert_eq!(v.panel_widths.backlinks, reader_layout::DEFAULT_PANEL_WIDTH);
            assert!(v.resizing_panel.is_none());
            cx.stop_active_drag(window);
            v.toggle_panel(reader_layout::Panel::Notes, window, cx);
        });
        visual.run_until_parked();
        std::fs::remove_file(preference).unwrap();
        visual.simulate_keystrokes("ctrl-\\");
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert_eq!(
                v.panels.dismiss_target(f32::from(v.body_bounds.size.width)),
                reader_layout::Panel::Closed
            );
            assert!(v.content.read(cx).focus_handle().is_focused(window));
            assert_eq!(v.content.entity_id(), old);
            let position = v.content.read(cx).list_state().logical_scroll_top();
            assert_eq!(position.item_ix, origin.item_ix);
            assert_eq!(position.offset_in_item, origin.offset_in_item);
            v.toggle_panel(reader_layout::Panel::Backlinks, window, cx);
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("ctrl-alt-\\");
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert!(v.content.read(cx).focus_handle().is_focused(window));
            assert_eq!(v.content.entity_id(), old);
            assert_eq!(v.history, history_before);
            v.open_note_at("target.md", None, Some("Landing"), window, cx);
            assert_eq!(
                v.content.entity_id(),
                old,
                "pending preparation keeps the current document"
            );
        });
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(300));
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert_eq!(v.current_rel, "target.md");
            assert_eq!(
                v.content.read(cx).list_state().logical_scroll_top().item_ix,
                101
            );
            v.open_note_at("start.md", None, Some("Missing"), window, cx);
            assert_eq!(
                v.current_rel, "target.md",
                "missing heading preserves document"
            );
        });
        visual.run_until_parked();
        view.update_in(visual, |v, _, _| {
            assert_eq!(v.current_rel, "target.md");
            assert!(v.link_notice.is_some());
        });
        visual.simulate_keystrokes("alt-left");
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(300));
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            assert_eq!(v.current_rel, "start.md");
            let actual = v.content.read(cx).list_state().logical_scroll_top();
            assert_eq!(actual.item_ix, origin.item_ix);
            assert_eq!(actual.offset_in_item, origin.offset_in_item);
        });
        // A later user focus change during parsing must survive the landing retry.
        view.update_in(visual, |v, window, cx| {
            v.open_note_at("target.md", None, Some("Landing"), window, cx);
            v.focus_sidebar_search(window, cx);
        });
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(300));
        visual.run_until_parked();
        view.update_in(visual, |v, window, _| {
            assert!(v.sidebar_search_focus.is_focused(window));
        });
        // Selecting from a compact sidebar dismisses it (#677) and leaves
        // immediate history shortcuts live, with no search/focus workaround.
        let before = view.read_with(visual, |v, _| {
            v.panels.dismiss_target(f32::from(v.body_bounds.size.width))
        });
        assert_eq!(before, reader_layout::Panel::Notes, "positive control");
        view.update_in(visual, |v, window, cx| {
            v.select_panel_note(reader_layout::Panel::Notes, "start.md", None, window, cx);
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert_eq!(
                v.panels.dismiss_target(f32::from(v.body_bounds.size.width)),
                reader_layout::Panel::Closed
            );
            assert!(v.content.read(cx).focus_handle().is_focused(window));
        });
        visual.simulate_keystrokes("alt-left");
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(300));
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            assert_eq!(v.current_rel, "target.md");
            assert_eq!(
                v.content.read(cx).list_state().logical_scroll_top().item_ix,
                101
            );
        });
        visual.simulate_keystrokes("alt-right");
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(300));
        visual.run_until_parked();
        view.update_in(visual, |v, _, _| assert_eq!(v.current_rel, "start.md"));
        visual.simulate_keystrokes("ctrl-k");
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert!(v.quick_open.open);
            assert!(v
                .quick_open
                .input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window));
        });
        #[cfg(not(target_os = "macos"))]
        visual.simulate_keystrokes("ctrl-f");
        #[cfg(target_os = "macos")]
        visual.simulate_keystrokes("cmd-f");
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            assert_eq!(
                v.panels.dismiss_target(f32::from(v.body_bounds.size.width)),
                reader_layout::Panel::Closed
            );
            assert!(v.find_open);
            assert!(v.find_input.read(cx).focus_handle(cx).is_focused(window));
        });
        #[cfg(target_os = "macos")]
        {
            view.update_in(visual, |v, window, cx| v.focus_sidebar_search(window, cx));
            visual.simulate_keystrokes("cmd-f");
            visual.run_until_parked();
            view.update_in(visual, |v, window, cx| {
                assert_eq!(
                    v.panels.dismiss_target(f32::from(v.body_bounds.size.width)),
                    reader_layout::Panel::Closed
                );
                assert!(v.find_open);
                assert!(v.find_input.read(cx).focus_handle(cx).is_focused(window));
            });
        }
        assert_eq!(
            std::fs::read(root.join("start.md")).unwrap(),
            start.as_bytes()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

fn reader_item_menu(
    menu: gpui_component::menu::PopupMenu,
    reader: &Entity<Reader>,
    relative: String,
    cx: &App,
) -> gpui_component::menu::PopupMenu {
    let menu = reader_files::menu(menu, reader.read(cx).vault_root.clone(), relative.clone());
    #[cfg(unix)]
    let menu = {
        let movable =
            reader.read(cx).vault.entries.iter().any(|e| {
                e.path == relative && e.kind != tessera_core::vault::EntryKind::Attachment
            });
        let rename_reader = reader.downgrade();
        let rename_path = relative.clone();
        let menu = menu.when(movable, |menu| {
            menu.item(
                gpui_component::menu::PopupMenuItem::new("Rename…").on_click(
                    move |_, window, cx| {
                        let _ = rename_reader.update(cx, |this, cx| {
                            this.begin_rename(rename_path.clone(), window, cx)
                        });
                    },
                ),
            )
        });
        let move_reader = reader.downgrade();
        let move_path = relative.clone();
        let menu = menu.when(movable, |menu| {
            menu.item(
                gpui_component::menu::PopupMenuItem::new("Move to…").on_click(
                    move |_, window, cx| {
                        let _ = move_reader.update(cx, |r, cx| {
                            r.choose_move_folder(move_path.clone(), window, cx)
                        });
                    },
                ),
            )
        });
        let reader = reader.downgrade();
        menu.separator().item(
            gpui_component::menu::PopupMenuItem::new("Move to Trash").on_click(
                move |_, window, cx| {
                    let _ = reader.update(cx, |this, cx| {
                        this.delete_path(relative.clone(), window, cx)
                    });
                },
            ),
        )
    };
    menu
}

#[cfg(test)]
mod reader_scroll_key_tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn document_keys_scroll_after_click_and_compact_sidebar_selection(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Start.md"), "# Start").unwrap();
        std::fs::write(
            root.join("Long.md"),
            (0..200)
                .map(|i| format!("Paragraph {i}. Enough text to scroll.\n\n"))
                .collect::<String>(),
        )
        .unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let r = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Long.md".into()),
                        index_dir: Some(temp.path().join("index")),
                        session_directory: Some(temp.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(r.clone());
            Root::new(r, window, cx)
        });
        let reader = reader.unwrap();
        let position = |visual: &mut gpui::VisualTestContext| {
            reader.read_with(visual, |r, cx| {
                let top = r.content.read(cx).list_state().logical_scroll_top();
                (top.item_ix, f32::from(top.offset_in_item))
            })
        };
        for width in [1400., 664.] {
            visual.simulate_resize(size(px(width), px(800.)));
            visual.run_until_parked();
            if width < 1000. {
                reader.update_in(visual, |r, window, cx| {
                    r.open_note("Start.md", None, window, cx)
                });
                visual.run_until_parked();
                reader.update_in(visual, |r, window, cx| {
                    r.reveal_in_tree("Long.md", window, cx);
                    let row = r
                        .tree
                        .rows
                        .iter()
                        .find(|row| row.path == "Long.md")
                        .unwrap()
                        .clone();
                    r.activate_tree_row(&row, window, cx);
                });
                visual.run_until_parked();
                reader.update_in(visual, |r, window, cx| {
                    assert!(!r.panels.visible(reader_layout::Panel::Notes, width));
                    assert!(r.content.read(cx).focus_handle().is_focused(window));
                });
            } else {
                let bounds = reader.read_with(visual, |r, cx| r.content.read(cx).bounds());
                visual.simulate_click(bounds.center(), Modifiers::default());
                visual.run_until_parked();
            }
            let before = position(visual);
            // The action itself must reject another focus owner, even when
            // dispatched directly instead of through its narrow key context.
            reader.update_in(visual, |r, window, cx| {
                r.tree_focus.focus(window, cx);
                r.scroll_reader_key(1., true, window, cx);
                r.content.read(cx).focus_handle().clone().focus(window, cx);
            });
            visual.run_until_parked();
            assert_eq!(
                before,
                position(visual),
                "tree focus cannot scroll the note"
            );
            visual.simulate_keystrokes("down");
            visual.run_until_parked();
            let after_line = position(visual);
            assert_ne!(before, after_line, "Down scrolls at width {width}");
            visual.simulate_keystrokes("pagedown");
            visual.run_until_parked();
            let after_page = position(visual);
            assert_ne!(after_line, after_page, "Page Down scrolls at width {width}");
            visual.simulate_keystrokes("pageup");
            visual.run_until_parked();
            assert_ne!(after_page, position(visual));
        }
    }
}
