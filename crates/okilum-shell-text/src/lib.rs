//! Text presentation, Markdown style/plugins, prepared link presentation, code
//! blocks, Mermaid rendering, agent imports/language/context, Obsidian syntax,
//! block embeds, remote Markdown, and non-Reader text zoom below the okilum app
//! crate (#3235, epic #3241). Nothing here knows `Reader`; the app crate
//! (`okilum-shell`, binary `okilum`) builds on it.
//!
//! Module names match their former paths in the app crate, so test names stay
//! the same (`source_presentation::tests::…`, `reader_obsidian::tests::…`, etc.).
#[macro_use]
#[path = "../../okilum-core/src/diagnostic_io.rs"]
mod diagnostic_io;

pub mod hooks;
pub mod markdown_style;
pub mod math_layout;
pub mod prepared_links;
pub mod reader_agent_imports;
pub mod reader_agent_language;
pub mod reader_agents_context;
pub mod reader_code;
pub mod reader_code_language;
pub mod reader_embeds;
pub mod reader_mermaid;
pub mod reader_obsidian;
pub mod reader_remote_markdown;
pub mod reader_text_zoom;
pub mod source_presentation;

#[cfg(test)]
use okilum_shell_kit::test_support::init as test_init;

/// Whether this build has the text crate's `test-support` feature: true
/// only in test builds. `okilum --build-info` reports it (#1043, #3235).
pub const TEST_SUPPORT: bool = cfg!(feature = "test-support");

/// Body size for the reader; the heading scale and the sidebar derive from it.
pub const BODY_FONT_SIZE: f32 = okilum_shell_kit::brand::READING_FONT_SIZE;
/// Horizontal side padding shared by Reader and remote Markdown windows.
pub const READER_SIDE_PADDING: f32 = 40.;

#[derive(Debug, PartialEq, Eq)]
pub enum ReaderLanding {
    Cancelled,
    Waiting,
    Failed,
    Ready,
}

pub fn reader_landing_state(
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

/// What text modules take from the root, as `use super::*` did in the app.
#[allow(unused_imports)]
pub(crate) mod prelude {
    pub(crate) use crate::markdown_style::{
        markdown_plugins, reader_text_style, MarkdownImage, MarkdownLinkHandler, MarkdownScope,
    };
    pub(crate) use crate::{
        prepared_links, reader_code, reader_code_language, reader_embeds, reader_landing_state,
        reader_mermaid, reader_obsidian, reader_text_zoom, source_presentation, ReaderLanding,
        BODY_FONT_SIZE, READER_SIDE_PADDING,
    };
    pub(crate) use gpui::prelude::FluentBuilder as _;
    pub(crate) use gpui::*;
    pub(crate) use gpui_component::menu::ContextMenuExt as _;
    pub(crate) use gpui_component::{
        button::{Button, ButtonVariants as _},
        h_flex,
        input::{Input, InputEvent, InputState},
        text::{
            markdown_ast, MarkdownNode, SelectionFormat, TextView, TextViewState, TextViewStyle,
        },
        v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Root, Selectable as _,
        Sizable as _, Theme, TitleBar,
    };
    pub(crate) use okilum_core::callout::{self, CalloutHeader, CalloutKind};
    pub(crate) use okilum_core::render::{
        split_open_url, AMBIGUOUS_SCHEME, EMBED_LANG, EMBED_MISSING, UNRESOLVED_SCHEME, WIKI_SCHEME,
    };
    pub(crate) use okilum_core::Vault;
    pub(crate) use okilum_shell_kit::appearance::AppearancePreference;
    pub(crate) use okilum_shell_kit::{brand, reader_layout, reader_ui_state, window_state};
    pub(crate) use okilum_shell_platform::reader_ssh_askpass;
    pub(crate) use std::path::{Path, PathBuf};
    pub(crate) use std::rc::Rc;
    pub(crate) use std::sync::Arc;
    pub(crate) use std::time::Duration;
}
