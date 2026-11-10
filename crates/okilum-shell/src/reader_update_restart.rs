//! Exact update relaunch targets, independent of the ordinary saved session.
use crate::{reader_open, reader_startup, Opts};
use gpui::{App, Task};
use serde::{Deserialize, Serialize};
#[cfg(any(windows, target_os = "linux", test))]
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Window {
    Vault { root: PathBuf, note: Option<String> },
    File { path: PathBuf },
}

pub(crate) fn capture(cx: &App) -> Vec<Window> {
    let windows = cx.windows();
    let mut readers = reader_open::readers(cx);
    // About/Settings may own focus when Restart is clicked. Keep the last
    // focused document in front, as ordinary session restoration does.
    let front = reader_open::opening_source(cx)
        .and_then(|reader| reader.upgrade())
        .map(|reader| reader.read(cx).reader_window);
    readers.sort_by_key(|reader| Some(reader.read(cx).reader_window) == front);
    readers
        .iter()
        .filter_map(|reader| {
            let reader = reader.read(cx);
            if !windows.contains(&reader.reader_window) {
                return None;
            }
            Some(if reader.single_file {
                Window::File {
                    path: reader.vault_root.join(reader.selected_file()),
                }
            } else {
                Window::Vault {
                    root: reader.vault_root.clone(),
                    note: Some(reader.selected_file().to_owned()).filter(|note| !note.is_empty()),
                }
            })
        })
        .collect()
}

#[cfg(any(windows, target_os = "linux", test))]
pub(crate) fn arguments(cx: &App) -> Vec<OsString> {
    capture(cx)
        .into_iter()
        .flat_map(|window| match window {
            Window::Vault { root, note } => vec![
                "--restore-vault-window".into(),
                root.into_os_string(),
                note.unwrap_or_default().into(),
            ],
            Window::File { path } => vec!["--restore-file-window".into(), path.into_os_string()],
        })
        .collect()
}

/// Shared final save boundary for Windows, AppImage and Sparkle.
pub(crate) fn protect_editors(cx: &mut App) -> anyhow::Result<()> {
    anyhow::ensure!(
        crate::reader_editor::protect_all_for_quit(cx),
        "Your latest edits could not be protected. Okilum will stay open."
    );
    Ok(())
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn prepare(cx: &mut App) -> anyhow::Result<Vec<OsString>> {
    let args = reader_open::update_restart_args(cx)?;
    protect_editors(cx)?;
    crate::reader_ui_state::prepare_for_restart(cx)?;
    Ok(args)
}

pub(crate) fn restore(mut base: Opts, cx: &mut App) -> Task<bool> {
    let windows = std::mem::take(&mut base.restart_windows);
    // Inherited launch defaults must not redirect any of these targets.
    base.vault = None;
    base.open_path = None;
    base.note = None;
    base.single_file = false;
    base.reusable_roots.clear();
    base.restore_workspace = true;
    cx.spawn(async move |cx| {
        let mut opened = false;
        for window in windows {
            match window {
                Window::Vault { root, note } => {
                    opened |= cx
                        .update(|cx| {
                            reader_startup::restore_windows(vec![(root, note)], base.clone(), cx)
                        })
                        .await;
                }
                Window::File { path } => {
                    let resolving = cx.update(|cx| {
                        reader_open::resolve(
                            Opts {
                                vault: path.parent().map(std::path::Path::to_path_buf),
                                note: path
                                    .file_name()
                                    .map(|name| name.to_string_lossy().into_owned()),
                                single_file: true,
                                ..base.clone()
                            },
                            cx,
                        )
                    });
                    match resolving.await {
                        Ok(opts) => {
                            // Do not route this file into an already restored vault,
                            // even when its directory is inside that vault.
                            opened |= cx.update(|cx| {
                                reader_open::create_window(opts, None, None, cx)
                                    .map_err(|error| reader_open::show_error(error, cx))
                                    .is_ok()
                            });
                        }
                        Err(error) => cx.update(|cx| reader_open::show_error(error, cx)),
                    }
                }
            }
        }
        // If every saved vault disappeared, use the normal start screen /
        // missing-folder notice instead of leaving an invisible application.
        cx.update(|cx| {
            if cx.windows().is_empty() {
                reader_startup::launch(base, cx);
            }
        });
        opened
    })
}

// Sparkle relaunches without application arguments. Hand off the same exact
// window set through a one-use file outside disposable indexes instead.
#[cfg(any(target_os = "macos", test))]
const FILE: &str = "update-restart-windows.json";

#[cfg(any(target_os = "macos", test))]
pub(crate) fn persist(directory: &std::path::Path, cx: &App) -> anyhow::Result<()> {
    std::fs::create_dir_all(directory)?;
    let mut pending = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer(&mut pending, &capture(cx))?;
    pending.as_file().sync_all()?;
    pending.persist(directory.join(FILE))?;
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn clear(directory: &std::path::Path) {
    let _ = std::fs::remove_file(directory.join(FILE));
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn take(directory: &std::path::Path) -> anyhow::Result<Vec<Window>> {
    let bytes = match std::fs::read(directory.join(FILE)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error.into()),
    };
    let windows = serde_json::from_slice(&bytes)?;
    std::fs::remove_file(directory.join(FILE))?;
    Ok(windows)
}

/// Cold launch only: a Dock reopen in the old process must not consume an
/// updater handoff while Sparkle is still waiting for termination.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn with_pending(mut opts: Opts) -> Opts {
    let windows = opts
        .session_directory
        .clone()
        .map(Ok)
        .unwrap_or_else(crate::reader_history::state_directory)
        .and_then(|directory| take(&directory));
    match windows {
        Ok(windows) if !windows.is_empty() => opts.restart_windows = windows,
        Ok(_) => {}
        Err(error) => diagnostic_error!("Cannot restore update windows: {error:#}"),
    }
    opts
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn update_restart_without_documents_has_no_targets(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| assert!(reader_open::update_restart_args(cx).unwrap().is_empty()));
    }

    #[gpui::test]
    fn update_restart_reopens_vault_and_single_file_windows(cx: &mut gpui::TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("A.md"), "# A").unwrap();
        std::fs::write(root.join("Б.md"), "# B").unwrap();
        let path = root.join("standalone file Ю.txt");
        std::fs::write(&path, "standalone").unwrap();
        let base = Opts {
            session_directory: Some(fixture.path().join("state")),
            cache_base_override: Some(fixture.path().join("cache")),
            ..Default::default()
        };
        cx.update(|cx| {
            crate::test_init(cx);
            reader_open::install(cx);
            cx.set_quit_mode(gpui::QuitMode::Explicit);
            cx.set_global(crate::reader_history::TestSessionDirectory(
                base.session_directory.clone().unwrap(),
            ));
            reader_open::open_window(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("Б.md".into()),
                    ..base.clone()
                },
                cx,
            )
            .unwrap();
        });
        cx.run_until_parked();
        cx.update(|cx| {
            // A separate file window inside the same vault must stay separate.
            // Bypass external-delivery routing, exactly as restoration must do.
            reader_open::create_window(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("standalone file Ю.txt".into()),
                    single_file: true,
                    ..base.clone()
                },
                None,
                None,
                cx,
            )
            .unwrap();
        });
        cx.run_until_parked();
        cx.update(|cx| {
            let source = reader_open::reader_of(&root, cx).unwrap();
            reader_open::duplicate_window(source, Some("A.md".into()), cx).detach();
        });
        cx.run_until_parked();
        let mut expected = vec![
            Window::Vault {
                root: root.clone(),
                note: Some("A.md".into()),
            },
            Window::Vault {
                root: root.clone(),
                note: Some("Б.md".into()),
            },
            Window::File { path },
        ];
        // Snapshot both the native argv path and Sparkle's one-use handoff.
        let opts = cx.update(|cx| {
            assert_eq!(capture(cx).len(), 3);
            persist(fixture.path(), cx).unwrap();
            let saved = with_pending(Opts {
                session_directory: Some(fixture.path().to_path_buf()),
                vault: Some(fixture.path().join("inherited-target")),
                ..Default::default()
            })
            .restart_windows;
            assert_eq!(saved, capture(cx));
            assert!(
                take(fixture.path()).unwrap().is_empty(),
                "handoff is consumed once"
            );
            persist(fixture.path(), cx).unwrap();
            clear(fixture.path());
            assert!(
                take(fixture.path()).unwrap().is_empty(),
                "a refused install clears its handoff"
            );
            let args = reader_open::update_restart_args(cx).unwrap();
            let reader_open::Command::Launch(opts) = reader_open::parse_args(
                args,
                Opts {
                    vault: Some(fixture.path().join("old-launch-target")),
                    ..base.clone()
                },
            )
            .unwrap() else {
                panic!("restart must launch the reader")
            };
            let opts = with_pending(*opts);
            assert_eq!(opts.restart_windows, saved);
            for window in cx.windows() {
                window
                    .update(cx, |_, window, _| window.remove_window())
                    .unwrap();
            }
            opts
        });
        cx.run_until_parked();
        // A stale ordinary session cannot override explicit update-restart modes.
        // In particular each vault restore starts at index zero of its own list.
        std::fs::create_dir_all(base.session_directory.as_ref().unwrap()).unwrap();
        std::fs::write(
            base.session_directory
                .as_ref()
                .unwrap()
                .join("open-windows.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": 1, "roots": [&root], "notes": ["A.md"], "single_file": [true]
            }))
            .unwrap(),
        )
        .unwrap();
        let finished = std::rc::Rc::new(std::cell::Cell::new(None));
        cx.update(|cx| {
            let finished = finished.clone();
            let restoring = restore(opts, cx);
            cx.spawn(async move |_| finished.set(Some(restoring.await)))
                .detach();
        });
        // Normal vault restoration waits on loading timers between windows.
        // Drain the whole transaction, not just its first runnable batch.
        for _ in 0..60 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(100));
            cx.run_until_parked();
            if finished.get().is_some() {
                break;
            }
        }
        assert_eq!(finished.get(), Some(true), "the restore task completed");
        cx.update(|cx| {
            let mut actual = capture(cx);
            // Focus ordering is platform-specific in GPUI's test context.
            let order = |window: &Window| match window {
                Window::Vault { root, note } => (false, root.clone(), note.clone()),
                Window::File { path } => (true, path.clone(), None),
            };
            actual.sort_by_key(&order);
            expected.sort_by_key(order);
            assert_eq!(
                actual, expected,
                "both current targets reopen in their original window kinds"
            );
        });
    }

    #[test]
    fn update_window_arguments_require_complete_targets_and_reject_other_launches() {
        let parse = |args: &[&str]| {
            reader_open::parse_args(args.iter().map(|arg| OsString::from(*arg)), Opts::default())
        };
        assert!(parse(&["--restore-file-window"]).is_err());
        assert!(parse(&["--restore-vault-window", "/vault"]).is_err());
        assert!(parse(&["--restore-file-window", "/note.md", "--vault", "/vault"]).is_err());
        assert!(parse(&["--restore-file-window", "/note.md", "/other.md"]).is_err());
        assert!(parse(&["--restore-file-window", "/note.md"]).is_ok());
    }

    #[gpui::test]
    fn update_restart_does_not_recreate_a_missing_single_file(cx: &mut gpui::TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("missing.md");
        cx.update(|cx| {
            crate::test_init(cx);
            reader_open::install(cx);
            restore(
                Opts {
                    restart_windows: vec![Window::File { path: path.clone() }],
                    session_directory: Some(fixture.path().join("state")),
                    cache_base_override: Some(fixture.path().join("cache")),
                    ..Default::default()
                },
                cx,
            )
            .detach();
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(
                !path.exists(),
                "session replay cannot create canonical files"
            );
            assert!(capture(cx).is_empty());
            assert_eq!(
                cx.windows().len(),
                1,
                "the failed target is surfaced in an error window"
            );
        });
    }

    #[gpui::test]
    fn update_restart_with_only_missing_vaults_keeps_a_visible_start_window(
        cx: &mut gpui::TestAppContext,
    ) {
        let fixture = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            reader_open::install(cx);
            restore(
                Opts {
                    restart_windows: vec![Window::Vault {
                        root: fixture.path().join("missing-vault"),
                        note: None,
                    }],
                    session_directory: Some(fixture.path().join("state")),
                    cache_base_override: Some(fixture.path().join("cache")),
                    ..Default::default()
                },
                cx,
            )
            .detach();
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(capture(cx).is_empty());
            assert_eq!(cx.windows().len(), 1, "normal startup remains available");
        });
    }
}
