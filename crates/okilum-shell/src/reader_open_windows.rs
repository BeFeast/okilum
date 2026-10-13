//! The vault windows that were open, so a launch reopens every one of them
//! (#1325). The set is rewritten whenever a vault window opens, closes or
//! shows another vault, so a crash, a forced quit or a logout keeps the latest
//! one; quit writes the final set and later closes no longer change it.
//! Each vault reopens on its own last note and position, which are stored per
//! vault already. Quick viewers of a single file are not part of the set.
use gpui::{App, Global};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const FILE: &str = "open-windows.json";
const SCHEMA: u32 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    version: u32,
    /// Vault roots in window order; the window that was in front comes last,
    /// so reopening them in this order leaves it in front again.
    roots: Vec<PathBuf>,
}

struct State {
    directory: PathBuf,
    written: Option<Vec<PathBuf>>,
    quitting: bool,
}
impl Global for State {}

pub(crate) fn install(directory: PathBuf, cx: &mut App) {
    cx.set_global(State {
        directory,
        written: None,
        quitting: false,
    });
    cx.on_window_closed(|cx, _| record(cx)).detach();
    cx.on_app_quit(|cx| {
        quit(cx);
        async {}
    })
    .detach();
}

/// Write the final set. Windows closing as the app ends leave it as it is.
pub(crate) fn quit(cx: &mut App) {
    record(cx);
    if cx.has_global::<State>() {
        cx.global_mut::<State>().quitting = true;
    }
}

/// Rewrite the set after this effect cycle, once a new window is in the
/// window list.
pub(crate) fn record_soon(cx: &mut App) {
    cx.defer(record);
}

fn record(cx: &mut App) {
    let Some(state) = cx.try_global::<State>() else {
        return;
    };
    if state.quitting {
        return;
    }
    let roots = open_roots(cx);
    let state = cx.global::<State>();
    if state.written.as_ref() == Some(&roots) {
        return;
    }
    match write(&state.directory, &roots) {
        Ok(()) => cx.global_mut::<State>().written = Some(roots),
        Err(error) => eprintln!("Cannot save the open windows: {error:#}"),
    }
}

/// Vault windows that are open now, the active one last.
fn open_roots(cx: &App) -> Vec<PathBuf> {
    let windows = cx.windows();
    let active = cx.active_window();
    let mut roots: Vec<(bool, PathBuf)> = Vec::new();
    for reader in super::reader_open::readers(cx) {
        let reader = reader.read(cx);
        if reader.single_file
            || !windows.contains(&reader.reader_window)
            || roots.iter().any(|(_, root)| *root == reader.vault_root)
        {
            continue;
        }
        roots.push((
            active == Some(reader.reader_window),
            reader.vault_root.clone(),
        ));
    }
    // A stable sort keeps the opening order and moves the front window last.
    roots.sort_by_key(|(front, _)| *front);
    roots.into_iter().map(|(_, root)| root).collect()
}

fn write(directory: &Path, roots: &[PathBuf]) -> anyhow::Result<()> {
    std::fs::create_dir_all(directory)?;
    let saved = Saved {
        version: SCHEMA,
        roots: roots.to_vec(),
    };
    let temporary = directory.join(format!("{FILE}.{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&temporary, serde_json::to_vec_pretty(&saved)?)?;
    std::fs::rename(&temporary, directory.join(FILE)).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })?;
    Ok(())
}

/// The vault roots to reopen, in order; empty when none were recorded or the
/// file cannot be read, so startup falls back to the last vault.
pub(crate) fn load(directory: &Path) -> Vec<PathBuf> {
    std::fs::read(directory.join(FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Saved>(&bytes).ok())
        .filter(|saved| saved.version == SCHEMA)
        .map(|saved| saved.roots)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn the_set_round_trips_and_a_damaged_file_reads_as_none() {
        let fixture = tempfile::tempdir().unwrap();
        let roots = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        write(fixture.path(), &roots).unwrap();
        assert_eq!(load(fixture.path()), roots);
        std::fs::write(fixture.path().join(FILE), b"{ not json").unwrap();
        assert!(load(fixture.path()).is_empty());
        // Positive control: an empty set is written and read as empty too.
        write(fixture.path(), &[]).unwrap();
        assert!(load(fixture.path()).is_empty());
        assert!(fixture.path().join(FILE).exists());
    }
}
