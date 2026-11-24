//! Shared shell components below the okilum app crate (epic #3241): brand and
//! themes, offline Help sheets, reading history, recent vaults, the Projects
//! picker and its dialogs. Nothing here knows the Reader; the app crate
//! (`okilum-shell`, binary `okilum`) builds on it.
//!
//! Module names match their former paths in the app crate, so test names stay
//! the same (`reader_history::tests::…`). New code that does not need `Reader`
//! belongs in a kit (or feature) crate, not in `crates/okilum-shell/src`.
#[macro_use]
#[path = "../../okilum-core/src/diagnostic_io.rs"]
mod diagnostic_io;

pub mod appearance;
pub mod brand;
pub mod clock;
pub mod code_view;
pub mod contextual_help;
pub mod count_label;
pub mod font_role;
pub mod lucide;
#[cfg(target_os = "macos")]
pub mod macos_cli;
pub mod new_project_dialog;
pub mod note_paste_settings;
pub mod project_avatar;
pub mod project_icon_dialog;
pub mod projects_picker;
pub mod reader_help_assets;
pub mod reader_history;
pub mod reader_layout;
pub mod reader_projects_help;
pub mod reader_recent_vaults;
pub mod reader_sidebar;
pub mod reader_sync_help;
pub mod reader_ui_state;
pub mod ui_zoom;
pub mod window_state;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
#[cfg(test)]
use test_support::init as test_init;

/// Whether this build has the kit's `test-support` feature: true only in test
/// builds. `okilum --build-info` reports it (#1043).
pub const TEST_SUPPORT: bool = cfg!(feature = "test-support");

/// What kit modules take from the root, as `use super::*` did in the app.
#[allow(unused_imports)]
pub(crate) mod prelude {
    pub(crate) use crate::{
        brand, reader_history, reader_layout, reader_sidebar, ui_zoom, window_state,
    };
    pub(crate) use gpui::prelude::FluentBuilder as _;
    pub(crate) use gpui::*;
    pub(crate) use gpui_component::{
        button::{Button, ButtonVariants as _},
        h_flex, v_flex, ActiveTheme as _, Disableable as _, Sizable as _,
    };
    pub(crate) use std::path::{Path, PathBuf};
    pub(crate) use std::rc::Rc;
    pub(crate) use std::sync::Arc;
    pub(crate) use std::time::Duration;
}
