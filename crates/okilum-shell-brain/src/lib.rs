//! Optional Unix AI Brain client and connector settings UI below the okilum app
//! crate (#3236, epic #3241). Nothing here knows `Reader`; the app crate
//! (`okilum-shell`, binary `okilum`) builds on it behind the `brain` feature.
//!
//! Module names match their former paths in the app crate, so test names stay
//! the same (`brain::tests::…`, `connectors::tests::…`, etc.).

#[cfg(unix)]
#[allow(dead_code, unused_macros)]
#[macro_use]
#[path = "../../okilum-core/src/diagnostic_io.rs"]
mod diagnostic_io;

#[cfg(unix)]
pub mod brain;
#[cfg(unix)]
pub mod connectors;
#[cfg(unix)]
pub mod export;
#[cfg(unix)]
pub mod hooks;

/// Whether this build has the brain crate's `test-support` feature: true
/// only in test builds. `okilum --build-info` reports it (#1043, #3236).
pub const TEST_SUPPORT: bool = cfg!(feature = "test-support");

#[cfg(all(unix, test))]
pub(crate) fn test_init(cx: &mut gpui::App) {
    okilum_shell_kit::test_support::init(cx);
}

#[cfg(unix)]
pub(crate) use okilum_shell_kit::brand;
#[cfg(unix)]
pub(crate) use okilum_shell_platform::platform;
#[cfg(unix)]
pub(crate) use okilum_shell_text::markdown_style::{
    markdown_plugins, reader_text_style, MarkdownImage,
};
#[cfg(unix)]
pub(crate) use okilum_shell_text::prepared_links;
#[cfg(unix)]
pub(crate) use okilum_shell_text::source_presentation;

#[cfg(unix)]
pub(crate) mod reader_features {
    use gpui::App;
    use okilum_t3::NetworkAccess;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Feature {
        SecondBrain,
    }

    pub(crate) fn enabled(_feature: Feature, cx: &App) -> bool {
        crate::hooks::second_brain_enabled(cx)
    }

    pub(crate) fn access(_feature: Feature, cx: &App) -> NetworkAccess {
        crate::hooks::second_brain_access(cx)
    }
}
