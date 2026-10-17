//! The vault windows that were open, so a launch reopens every one of them
//! (#1325). The set is rewritten whenever a vault window opens, closes or
//! shows another vault, so a crash, a forced quit or a logout keeps the latest
//! one; quit writes the final set and later closes no longer change it.
//! Each window reopens on the note it showed (#1375); a window on its vault's
//! last note also gets its position back, which is stored per vault.
//! Quick viewers of a single file are not part of the set.
use gpui::{App, Global};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const FILE: &str = "open-windows.json";
const SCHEMA: u32 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    version: u32,
    /// One vault root per window, in window order; a vault open in two
    /// windows is listed twice. The window that was in front comes last, so
    /// reopening them in this order leaves it in front again. Builds before
    /// #1335 wrote one entry per vault, which still reads as one window each.
    roots: Vec<PathBuf>,
    /// The note each window showed, matching `roots` by position; empty when
    /// unknown (#1375). A separate list, so builds before it still read the
    /// roots.
    notes: Vec<String>,
}

/// A window to reopen: its vault and the note it showed, if known.
pub(crate) type Window = (PathBuf, Option<String>);

struct State {
    directory: PathBuf,
    written: Option<Vec<Window>>,
    quitting: bool,
    /// The vault window focused last; it stays the front one while another
    /// application has the focus (#1329).
    front: Option<gpui::AnyWindowHandle>,
    pending: Option<gpui::Task<()>>,
}
impl Global for State {}

pub(crate) fn install(directory: PathBuf, cx: &mut App) {
    cx.set_global(State {
        directory,
        written: None,
        quitting: false,
        front: None,
        pending: None,
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

/// A vault window got the focus: it is the front one. Written after the
/// switching settles, so a run of quick switches is one write (#1329).
pub(crate) fn focused(window: gpui::AnyWindowHandle, cx: &mut App) {
    if !cx.has_global::<State>() {
        return;
    }
    let task = cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(std::time::Duration::from_millis(300))
            .await;
        cx.update(record);
    });
    let state = cx.global_mut::<State>();
    state.front = Some(window);
    // Dropping the previous task cancels its write.
    state.pending = Some(task);
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
    let windows = open_windows(cx);
    let state = cx.global::<State>();
    if state.written.as_ref() == Some(&windows) {
        return;
    }
    match write(&state.directory, &windows) {
        Ok(()) => cx.global_mut::<State>().written = Some(windows),
        Err(error) => eprintln!("Cannot save the open windows: {error:#}"),
    }
}

/// Vault windows that are open now, the one focused last at the end.
fn open_windows(cx: &App) -> Vec<Window> {
    let windows = cx.windows();
    let active = cx
        .try_global::<State>()
        .and_then(|state| state.front)
        .or_else(|| cx.active_window());
    let mut roots: Vec<(bool, Window)> = Vec::new();
    for reader in super::reader_open::readers(cx) {
        let reader = reader.read(cx);
        // One entry per window: two windows of a vault are listed twice and
        // both come back (#1335).
        if reader.single_file || !windows.contains(&reader.reader_window) {
            continue;
        }
        let note = Some(reader.current_rel.clone()).filter(|note| !note.is_empty());
        roots.push((
            active == Some(reader.reader_window),
            (reader.vault_root.clone(), note),
        ));
    }
    // A stable sort keeps the opening order and moves the front window last.
    roots.sort_by_key(|(front, _)| *front);
    roots.into_iter().map(|(_, window)| window).collect()
}

fn write(directory: &Path, windows: &[Window]) -> anyhow::Result<()> {
    std::fs::create_dir_all(directory)?;
    let saved = Saved {
        version: SCHEMA,
        roots: windows.iter().map(|(root, _)| root.clone()).collect(),
        notes: windows
            .iter()
            .map(|(_, note)| note.clone().unwrap_or_default())
            .collect(),
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
#[cfg(test)]
pub(crate) fn load(directory: &Path) -> Vec<PathBuf> {
    load_windows(directory)
        .into_iter()
        .map(|(root, _)| root)
        .collect()
}

/// The windows to reopen, each with the note it showed. A note that is not
/// a plain relative path is dropped, and the window opens as its vault would.
pub(crate) fn load_windows(directory: &Path) -> Vec<Window> {
    let Some(saved) = std::fs::read(directory.join(FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Saved>(&bytes).ok())
        .filter(|saved| saved.version == SCHEMA)
    else {
        return Vec::new();
    };
    let mut notes = saved.notes.into_iter();
    saved
        .roots
        .into_iter()
        .map(|root| {
            let note = notes.next().filter(|note| {
                !note.is_empty()
                    && Path::new(note)
                        .components()
                        .all(|part| matches!(part, std::path::Component::Normal(_)))
            });
            (root, note)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn the_set_round_trips_and_a_damaged_file_reads_as_none() {
        let fixture = tempfile::tempdir().unwrap();
        let roots = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        let windows: Vec<Window> = roots.iter().map(|root| (root.clone(), None)).collect();
        write(fixture.path(), &windows).unwrap();
        assert_eq!(load(fixture.path()), roots);
        std::fs::write(fixture.path().join(FILE), b"{ not json").unwrap();
        assert!(load(fixture.path()).is_empty());
        // Positive control: an empty set is written and read as empty too.
        write(fixture.path(), &[]).unwrap();
        assert!(load(fixture.path()).is_empty());
        assert!(fixture.path().join(FILE).exists());
    }

    #[test]
    fn each_window_keeps_its_note_and_older_files_still_read() {
        // #1375: two windows of one vault, each on its own note.
        let fixture = tempfile::tempdir().unwrap();
        let windows = vec![
            (PathBuf::from("/vault"), Some("Alpha.md".to_string())),
            (PathBuf::from("/vault"), Some("Folder/Beta.md".to_string())),
            (PathBuf::from("/other"), None),
        ];
        write(fixture.path(), &windows).unwrap();
        assert_eq!(load_windows(fixture.path()), windows);
        // A file from before #1375 has no notes: each window opens as its
        // vault would.
        std::fs::write(
            fixture.path().join(FILE),
            br#"{"version":1,"roots":["/vault","/vault"]}"#,
        )
        .unwrap();
        assert_eq!(
            load_windows(fixture.path()),
            vec![
                (PathBuf::from("/vault"), None),
                (PathBuf::from("/vault"), None)
            ]
        );
        // A note that leaves its vault is not trusted.
        std::fs::write(
            fixture.path().join(FILE),
            br#"{"version":1,"roots":["/vault","/vault"],"notes":["../x.md","/etc/x.md"]}"#,
        )
        .unwrap();
        assert_eq!(
            load_windows(fixture.path()),
            vec![
                (PathBuf::from("/vault"), None),
                (PathBuf::from("/vault"), None)
            ]
        );
    }
}
