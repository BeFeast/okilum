//! FSEvents replay cursors are operational state, separate from disposable indexes.
#[cfg(not(target_os = "macos"))]
use std::path::Path;

#[cfg(target_os = "macos")]
#[path = "reader_replay_macos.rs"]
mod native;
#[cfg(target_os = "macos")]
pub use native::prepare;

#[cfg(not(target_os = "macos"))]
#[derive(Default)]
pub struct Replay {
    pub force_all: bool,
    pub dirty: Vec<String>,
}
#[cfg(not(target_os = "macos"))]
pub fn prepare(_: &Path, _: Option<&Path>, _: Option<&str>) -> Replay {
    Replay::default()
}
#[cfg(not(target_os = "macos"))]
impl Replay {
    pub fn save(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}
