//! Platform adapters, clipboard/smart-paste, note upload, capture namespace,
//! trash transport, SSH askpass, PDF engine, iCloud hydration, Open-in editor
//! detection and Windows handlers below the okilum app crate (#3234, epic #3241).
//! Nothing here knows `Reader`; the app crate (`okilum-shell`, binary `okilum`)
//! builds on it.
//!
//! Module names match their former paths in the app crate, so test names stay
//! the same (`platform::reveal::tests::…`, `note_paste::…`, etc.).
#[macro_use]
#[path = "../../okilum-core/src/diagnostic_io.rs"]
mod diagnostic_io;

pub mod hooks;
#[cfg(windows)]
pub mod markdown_handler;
#[cfg(any(test, feature = "test-support"))]
pub mod network_write_qa;
pub mod note_paste;
pub mod note_upload;
pub mod open_in;
// #1502: Windows own-window capture; its pixel checks also run in tests elsewhere.
#[cfg(any(windows, test, feature = "test-support"))]
#[cfg_attr(not(windows), allow(dead_code))]
pub mod own_window_capture;
pub mod pdf_engine;
pub mod platform;
pub mod reader_capture;
#[cfg(any(target_os = "macos", test, feature = "test-support"))]
pub mod reader_icloud;
pub mod reader_ssh_askpass;
#[cfg(unix)]
pub mod reader_trash_fs;
#[cfg(windows)]
#[path = "reader_trash_fs_windows.rs"]
pub mod reader_trash_fs;
#[cfg(windows)]
pub mod url_protocol;

#[cfg(test)]
#[allow(dead_code)]
#[rustfmt::skip]
#[path = "../../../vendor/gpui-component/crates/base/src/input/bidi_geometry.rs"]
mod vendor_bidi_geometry;

#[cfg(test)]
use okilum_shell_kit::test_support::init as test_init;

/// Whether this build has the platform crate's `test-support` feature: true
/// only in test builds. `okilum --build-info` reports it (#1043, #3234).
pub const TEST_SUPPORT: bool = cfg!(feature = "test-support");

/// Linux Sprint credential key prefix, mirroring `linux_sprint::credential_key`.
pub(crate) fn credential_key(key: &str) -> String {
    if cfg!(all(target_os = "linux", feature = "linux-sprint")) {
        format!("okilum-sprint:{key}")
    } else {
        key.to_owned()
    }
}

/// What platform modules take from the root, as `use super::*` did in the app.
#[allow(unused_imports)]
pub(crate) mod prelude {
    pub(crate) use crate::{note_paste, note_upload, open_in, pdf_engine, platform};
    pub(crate) use gpui::prelude::FluentBuilder as _;
    pub(crate) use gpui::*;
    pub(crate) use gpui_component::{
        button::{Button, ButtonVariants as _},
        h_flex,
        input::{Input, InputEvent, InputState},
        v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Root, Sizable as _, Theme,
        TitleBar,
    };
    pub(crate) use okilum_shell_kit::{brand, reader_ui_state, window_state};
    pub(crate) use std::path::{Path, PathBuf};
    pub(crate) use std::rc::Rc;
    pub(crate) use std::sync::Arc;
    pub(crate) use std::time::Duration;
}
