//! A durable launch marker, separate from draft journals and disposable indexes.
use anyhow::Result;
use gpui::{App, Global};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    sync::Arc,
};

struct Run {
    marker: PathBuf,
    stale: Vec<PathBuf>,
    _lock: File,
}
impl Run {
    fn begin(directory: &Path) -> Result<Self> {
        let directory = directory.join("reader-runs");
        fs::create_dir_all(&directory)?;
        let mut stale = Vec::new();
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            if path.extension().is_none_or(|ext| ext != "active") {
                continue;
            }
            let file = File::open(&path)?;
            // Another live Tessera process is not a crashed launch.
            match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => stale.push(path),
                Err(rustix::io::Errno::WOULDBLOCK) => (),
                Err(error) => return Err(error.into()),
            }
        }
        let marker = directory.join(format!("{}.active", uuid::Uuid::new_v4()));
        let pending = marker.with_extension("pending");
        let lock = File::create_new(&pending)?;
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)?;
        lock.sync_all()?;
        fs::rename(pending, &marker)?;
        File::open(&directory)?.sync_all()?;
        Ok(Self {
            marker,
            stale,
            _lock: lock,
        })
    }

    // Explicit only: dropping on panic must leave evidence of the failed launch.
    fn clean(&self) -> Result<()> {
        for path in self.stale.iter().chain(std::iter::once(&self.marker)) {
            match fs::remove_file(path) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        File::open(self.marker.parent().unwrap())?.sync_all()?;
        Ok(())
    }
}

pub(crate) struct RecoveryStartup(pub bool);
impl Global for RecoveryStartup {}

pub(crate) fn is_recovering(cx: &App) -> bool {
    cx.try_global::<RecoveryStartup>()
        .is_some_and(|state| state.0)
}

pub(crate) fn install(directory: &Path, config_directory: Option<&Path>, cx: &mut App) {
    // Track both historical locations: moving either one aside must not hide a
    // crash recorded in the other. Neither journals nor preferences are moved.
    let mut directories = vec![directory.to_path_buf()];
    if let Some(config) = config_directory {
        if config != directory {
            directories.push(config.to_path_buf());
        }
    }
    let mut runs = Vec::new();
    let mut recovering = false;
    for directory in directories {
        match Run::begin(&directory) {
            Ok(run) => {
                recovering |= !run.stale.is_empty();
                runs.push(run);
            }
            Err(error) => {
                recovering = true;
                eprintln!("Cannot record Reader launch: {error}");
            }
        }
    }
    cx.set_global(RecoveryStartup(recovering));
    let runs = Arc::new(runs);
    cx.on_app_quit(move |cx| {
        let saved = super::reader_editor::save_all(cx);
        let runs = runs.clone();
        async move {
            if saved {
                for run in runs.iter() {
                    if let Err(error) = run.clean() {
                        eprintln!("Cannot finish Reader launch marker: {error}");
                    }
                }
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[gpui::test]
    fn either_state_location_can_trigger_safe_startup(cx: &mut gpui::TestAppContext) {
        for stale_config in [false, true] {
            let root =
                std::env::temp_dir().join(format!("tessera-two-roots-{}", uuid::Uuid::new_v4()));
            let state = root.join("uk.oklabs.tessera");
            let config = root.join("tessera");
            drop(Run::begin(if stale_config { &config } else { &state }).unwrap());
            cx.update(|cx| {
                install(&state, Some(&config), cx);
                assert!(is_recovering(cx));
            });
            // Both locations now carry the live launch, while all other files remain untouched.
            assert!(state.join("reader-runs").is_dir());
            assert!(config.join("reader-runs").is_dir());
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn crash_marker_survives_drop_and_live_launches_are_not_crashes() {
        let directory =
            std::env::temp_dir().join(format!("tessera-run-test-{}", uuid::Uuid::new_v4()));
        let first = Run::begin(&directory).unwrap();
        assert!(first.stale.is_empty());
        let concurrent = Run::begin(&directory).unwrap();
        assert!(concurrent.stale.is_empty());
        concurrent.clean().unwrap();
        drop(first); // Abrupt termination: release the lock without a clean marker.
        let recovered = Run::begin(&directory).unwrap();
        assert_eq!(recovered.stale.len(), 1);
        recovered.clean().unwrap();
        let clean = Run::begin(&directory).unwrap();
        assert!(clean.stale.is_empty());
        clean.clean().unwrap();
        fs::remove_dir_all(directory).unwrap();
    }
}
