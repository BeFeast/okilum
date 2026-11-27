//! File viewers, shared viewer chrome, status line, split pane, segmented
//! switch, find field, and copy/menu policies below the okilum app crate
//! (#3238, epic #3241). Nothing here knows `Reader`; the app crate
//! (`okilum-shell`, binary `okilum`) builds on it.
//!
//! Module names match their former paths in the app crate, so test names stay
//! the same (`reader_audio::tests::...`, `reader_code_file::...`, etc.).
#![allow(private_interfaces)]
#[macro_use]
#[path = "../../okilum-core/src/diagnostic_io.rs"]
mod diagnostic_io;

pub mod hooks;
pub mod reader_archive;
pub mod reader_audio;
pub mod reader_canvas;
pub mod reader_code_file;
pub mod reader_delimited;
pub mod reader_drawing;
pub mod reader_drawing_edit;
pub mod reader_file_copy;
pub mod reader_find_field;
pub mod reader_har;
pub mod reader_image;
pub mod reader_media;
pub mod reader_pdf;
pub mod reader_pin_row;
pub mod reader_plain_text;
pub mod reader_read_menu;
pub mod reader_segmented;
pub mod reader_split;
pub mod reader_status_line;
pub mod reader_table_copy;
#[cfg(any(target_os = "macos", all(any(test, feature = "test-support"), unix)))]
pub mod reader_thumbnail;
pub mod reader_viewer_chrome;
pub mod shortcut_hint;

/// Whether this build has the viewers crate's `test-support` feature: true
/// only in test builds. `okilum --build-info` reports it (#1043, #3238).
pub const TEST_SUPPORT: bool = cfg!(feature = "test-support");

/// Row height shared by the outline and HAR overview lists.
pub const OUTLINE_ROW_HEIGHT: f32 = 27.;

/// Canonicalize `rel` inside `root`, refusing any path that escapes the vault.
pub fn checked_path(root: &std::path::Path, rel: &str) -> anyhow::Result<std::path::PathBuf> {
    let root = root.canonicalize()?;
    let path = root.join(rel).canonicalize()?;
    anyhow::ensure!(path.starts_with(&root), "The file is outside this vault");
    Ok(path)
}

/// Measure toolbar label width using the chrome font size.
pub fn toolbar_text_width(text: &str, weight: gpui::FontWeight, window: &gpui::Window) -> f32 {
    let mut font = window.text_style().font();
    font.weight = weight;
    let run = gpui::TextRun {
        len: text.len(),
        font,
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(
            text.to_owned().into(),
            gpui::px(okilum_shell_kit::brand::READER_CHROME_FONT_SIZE),
            &[run],
            None,
        )
        .width()
        .as_f32()
}

/// Standard 28x28 ghost icon button used in viewer and document headers.
pub fn reader_icon_button(
    id: impl Into<gpui::ElementId>,
    icon: impl Into<gpui_component::Icon>,
    tooltip: impl Into<gpui::SharedString>,
    _cx: &gpui::App,
) -> gpui_component::button::Button {
    use gpui::{px, Styled as _};
    use gpui_component::button::ButtonVariants as _;
    gpui_component::button::Button::new(id)
        .ghost()
        .icon(icon)
        .tooltip(tooltip)
        .w(px(28.))
        .h(px(28.))
        .rounded(px(6.))
}

pub(crate) use okilum_shell_kit::actions::{
    ArchiveExtractAll, ArchiveExtractSelected, CanvasFit, CanvasZoomIn, CanvasZoomOut,
    CopyDrawingDetails, CopyFileContent, CopyViewerImage, Dismiss, ExpandDrawing, FindInNote,
    FindNext, FindPrev, OpenDrawingExternally, PdfZoomFit, PdfZoomIn, PdfZoomOut, RepairCode,
    ShowDrawingDetails, ToggleSoftWrap,
};
pub(crate) use okilum_shell_kit::{
    brand, count_label, reader_layout, reader_ui_state, ui_zoom, window_state,
};
pub(crate) use okilum_shell_platform::{open_in, pdf_engine, platform};
pub(crate) use okilum_shell_text::markdown_style::{
    markdown_plugins, reader_text_style, MarkdownImage, MarkdownLinkHandler, MarkdownScope,
};
pub(crate) use okilum_shell_text::{
    prepared_links, reader_code, reader_code_language, reader_embeds, reader_mermaid,
    reader_text_zoom, source_presentation, BODY_FONT_SIZE, READER_SIDE_PADDING,
};

pub(crate) mod reader_shortcuts {
    pub(crate) use crate::shortcut_hint::{hint, key_cap, shortcut};
}

pub(crate) mod reader_editor {
    pub(crate) use okilum_shell_kit::actions::RepairCode;
}

pub(crate) mod reader_fonts {
    pub(crate) fn code(cx: &gpui::App) -> gpui::SharedString {
        crate::hooks::code_font(cx)
    }

    pub(crate) fn note(cx: &gpui::App) -> gpui::SharedString {
        crate::hooks::note_font(cx)
    }
}

pub(crate) mod reader_toast {
    pub(crate) fn error(message: impl Into<String>, window: &mut gpui::Window, cx: &mut gpui::App) {
        crate::hooks::toast_error(message, window, cx);
    }

    pub(crate) fn transient(
        message: impl Into<String>,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) {
        crate::hooks::toast_transient(message, window, cx);
    }

    pub(crate) fn duckdb_error(
        message: impl Into<String>,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) {
        crate::hooks::toast_duckdb_error(message, window, cx);
    }
}

pub(crate) mod reader_files {
    use gpui::{App, ImageSource, Window};
    use std::path::{Path, PathBuf};

    #[derive(Clone, Copy)]
    pub(crate) enum FileAction {
        Open,
    }

    pub(crate) fn checked_path(root: &Path, rel: &str) -> anyhow::Result<PathBuf> {
        crate::checked_path(root, rel)
    }

    pub(crate) fn run(
        action: FileAction,
        root: &Path,
        rel: &str,
        window: &mut Window,
        cx: &mut App,
    ) {
        match action {
            FileAction::Open => crate::hooks::open_file(root, rel, window, cx),
        }
    }

    pub(crate) fn image_source(path: PathBuf, cx: &App) -> ImageSource {
        crate::hooks::image_source(path, cx)
    }
}

#[cfg(test)]
pub(crate) fn test_init(cx: &mut gpui::App) {
    okilum_shell_kit::test_support::init(cx);
    hooks::install(
        hooks::ViewerHooks {
            code_font: |_| gpui::SharedString::from("monospace"),
            note_font: |_| gpui::SharedString::from("sans-serif"),
            toast_error: |_, _, _| {},
            toast_transient: |_, _, _| {},
            toast_duckdb_error: |_, _, _| {},
            open_file: |root, rel, _, cx| {
                if let Ok(path) = checked_path(root, rel) {
                    if let Ok(url) = url::Url::from_file_path(path) {
                        cx.open_url(url.as_str());
                    }
                }
            },
            image_source: gpui::ImageSource::from,
            start_thumbnail: None,
        },
        cx,
    );
}

#[cfg(test)]
pub(crate) fn bind_keys(cx: &mut gpui::App) {
    reader_code_file::bind_keys(cx);
    reader_har::bind_keys(cx);
    cx.bind_keys([
        gpui::KeyBinding::new("escape", Dismiss, None),
        gpui::KeyBinding::new("secondary-f", FindInNote, None),
        gpui::KeyBinding::new("secondary-g", FindNext, None),
        gpui::KeyBinding::new("secondary-shift-g", FindPrev, None),
        gpui::KeyBinding::new("alt-z", ToggleSoftWrap, None),
    ]);
}

/// What viewer modules take from the root, as `use super::*` did in the app.
#[allow(unused_imports)]
pub(crate) mod prelude {
    #[cfg(test)]
    pub(crate) use crate::{bind_keys, test_init};
    pub(crate) use crate::{
        brand, count_label, markdown_plugins, open_in, pdf_engine, platform, prepared_links,
        reader_archive, reader_audio, reader_canvas, reader_code, reader_code_file,
        reader_code_language, reader_delimited, reader_drawing, reader_drawing_edit, reader_editor,
        reader_embeds, reader_file_copy, reader_files, reader_find_field, reader_fonts, reader_har,
        reader_icon_button, reader_image, reader_layout, reader_media, reader_mermaid, reader_pdf,
        reader_pin_row, reader_plain_text, reader_read_menu, reader_segmented, reader_shortcuts,
        reader_split, reader_status_line, reader_table_copy, reader_text_style, reader_text_zoom,
        reader_toast, reader_ui_state, reader_viewer_chrome, source_presentation,
        toolbar_text_width, ui_zoom, window_state, ArchiveExtractAll, ArchiveExtractSelected,
        CanvasFit, CanvasZoomIn, CanvasZoomOut, CopyDrawingDetails, CopyFileContent,
        CopyViewerImage, Dismiss, ExpandDrawing, FindInNote, FindNext, FindPrev, MarkdownImage,
        MarkdownLinkHandler, MarkdownScope, OpenDrawingExternally, PdfZoomFit, PdfZoomIn,
        PdfZoomOut, RepairCode, ShowDrawingDetails, ToggleSoftWrap, BODY_FONT_SIZE,
        OUTLINE_ROW_HEIGHT, READER_SIDE_PADDING,
    };
    pub(crate) use gpui::prelude::FluentBuilder as _;
    pub(crate) use gpui::*;
    pub(crate) use gpui_base::ElementExt as _;
    pub(crate) use gpui_component::menu::ContextMenuExt as _;
    pub(crate) use gpui_component::{
        button::{Button, ButtonVariants as _},
        h_flex,
        input::{Input, InputEvent, InputState},
        text::{
            markdown_ast, MarkdownNode, SelectionFormat, TextView, TextViewState, TextViewStyle,
        },
        v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Root, Selectable as _,
        Sizable as _, Theme, TitleBar, WindowExt as _,
    };
    pub(crate) use okilum_core::render::{
        split_open_url, AMBIGUOUS_SCHEME, EMBED_LANG, EMBED_MISSING, UNRESOLVED_SCHEME, WIKI_SCHEME,
    };
    pub(crate) use okilum_core::Vault;
    pub(crate) use std::path::{Path, PathBuf};
    pub(crate) use std::rc::Rc;
    pub(crate) use std::sync::Arc;
    pub(crate) use std::time::Duration;
}
