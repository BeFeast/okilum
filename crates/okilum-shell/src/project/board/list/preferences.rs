//! Project presentation lives in app state, never in canonical project notes.
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Mode {
    #[default]
    List,
    Board,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Preferences {
    version: u32,
    project: PathBuf,
    pub mode: Mode,
    pub collapsed: [bool; 5],
    #[serde(default)]
    pub waiting_collapsed: bool,
}
impl Preferences {
    pub fn new(project: PathBuf) -> Self {
        Self {
            version: 1,
            project,
            mode: Mode::List,
            collapsed: [true, false, false, false, true],
            waiting_collapsed: false,
        }
    }
    pub fn load(path: &Path, project: PathBuf) -> anyhow::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => {
                anyhow::ensure!(bytes.len() <= 16 * 1024, "Board preferences exceed 16 KiB");
                let saved: Self = serde_json::from_slice(&bytes)?;
                anyhow::ensure!(
                    saved.version == 1 && saved.project == project,
                    "Board preferences have an unsupported version or project"
                );
                Ok(saved)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::new(project)),
            Err(e) => Err(e.into()),
        }
    }
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let parent = path.parent().expect("app-state child");
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(&serde_json::to_vec(self)?)?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    }
}
pub(super) fn path(project: &Path, cx: &App) -> Option<PathBuf> {
    #[cfg(test)]
    let state = cx
        .try_global::<reader_history::TestSessionDirectory>()
        .map(|s| s.0.clone());
    #[cfg(not(test))]
    let state = {
        let _ = cx;
        reader_history::state_directory().ok()
    };
    let key = format!("{:x}", Sha256::digest(project.to_string_lossy().as_bytes()));
    state.map(|state| state.join("board-presentation").join(format!("{key}.json")))
}
