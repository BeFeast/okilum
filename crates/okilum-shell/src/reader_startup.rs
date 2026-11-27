//! Local Reader startup. Saved managed identities are never read on this path.
use super::{
    reader_history,
    reader_loading::now_start_note,
    reader_open, reader_path_moves,
    reader_recent_vaults::{self, Status, Unavailable},
    Opts,
};
use gpui::*;
use gpui_component::Root;
use std::path::{Path, PathBuf};

#[derive(Default)]
pub(crate) struct Startup {
    superseded: bool,
    entry: Option<AnyWindowHandle>,
}
impl Global for Startup {}
impl Startup {
    /// The start window, while it is up.
    pub(crate) fn entry_window(&self) -> Option<AnyWindowHandle> {
        self.entry
    }
}

/// Explicit file delivery or a picker choice wins over an in-flight history read.
///
/// The start window closes only once another window exists. A vault window
/// is created after a background step, and closing the last window ends the
/// app on Linux and Windows (#1110); callers run this again once their window
/// is up, which retires the start window then.
pub(crate) fn supersede(cx: &mut App) {
    if cx.try_global::<Startup>().is_none() {
        return;
    }
    let state = cx.global_mut::<Startup>();
    state.superseded = true;
    let Some(entry) = state.entry else {
        return;
    };
    // Owned tools (including first-run Sync Settings) do not replace the start
    // window. Keep its owner alive until the received vault window really exists.
    if cx
        .windows()
        .iter()
        .any(|window| *window != entry && super::reader_owned_windows::owner(*window, cx) != entry)
    {
        cx.global_mut::<Startup>().entry = None;
        let _ = entry.update(cx, |_, window, cx| {
            // Named in the close log, not left as an unexplained removal (#2805).
            super::reader_diagnostics::window_close_requested(
                window,
                cx,
                "start window replaced by a vault window",
            );
            window.remove_window()
        });
    }
}

/// The options the process started its Reader with (state, index and
/// diagnostics), kept so a macOS reopen can start the same way again.
pub(crate) struct ReopenBase(pub Opts);
impl Global for ReopenBase {}

/// macOS reopen (Dock click, `open -a`) while no window is visible (#1137).
/// Closing every window keeps the app running there, so this brings a
/// minimized window back, or runs startup again: the last vault, or the
/// start screen. Linux and Windows end the app with its last window (#1110).
pub(crate) fn reopen(cx: &mut App) {
    let windows = cx.windows();
    if let Some(trace) = super::reader_diagnostics::trace(cx) {
        trace.event(
            "app_reopen",
            serde_json::json!({ "windows": windows.len() }),
        );
    }
    if let Some(window) = windows.first() {
        let _ = window.update(cx, |_, window, _| window.activate_window());
        cx.activate(true);
        return;
    }
    let Some(base) = cx.try_global::<ReopenBase>().map(|base| base.0.clone()) else {
        return;
    };
    cx.set_global(Startup::default());
    launch(base, cx);
}

pub(crate) fn launch(mut opts: Opts, cx: &mut App) {
    opts.restore_workspace = true;
    let directory = opts.session_directory.clone();
    let diagnostics = opts.diagnostics.clone();
    cx.spawn(async move |cx| {
        let (restorable, history) = cx
            .background_executor()
            .spawn(async move {
                let _phase = diagnostics
                    .as_ref()
                    .map(|trace| trace.phase("startup_history_and_root_validation"));
                let directory = directory
                    .map(Ok)
                    .unwrap_or_else(reader_history::state_directory);
                // #3208: the user's own history drops folders nobody chose
                // as a vault (fixtures, temp and app-support folders).
                if let Ok(directory) = &directory {
                    if reader_recent_vaults::users_history(directory) {
                        if let Err(error) = reader_recent_vaults::prune(
                            directory,
                            reader_recent_vaults::Places::current(),
                        ) {
                            diagnostic_error!("Cannot tidy the recent vaults: {error:#}");
                        }
                    }
                }
                // The window set does not depend on the reading history: a
                // damaged history still reopens every recorded window.
                let windows = directory
                    .as_deref()
                    .map(|directory| {
                        restorable_windows(
                            directory,
                            super::reader_open_windows::load_session_windows(directory),
                        )
                    })
                    .unwrap_or_default();
                let history = history(directory);
                let listing = history
                    .as_ref()
                    .map(|(directory, _, _)| Listing::read(directory))
                    .unwrap_or_default();
                // Why a listed vault cannot be opened (#1368, #3208), each
                // folder on its own thread with a deadline: a dead network
                // share must not hold up the start screen.
                let status: Vec<(PathBuf, Status)> = history
                    .as_ref()
                    .map(|(_, roots, restore)| {
                        let mut checked: Vec<PathBuf> = roots
                            .iter()
                            .take(2 * reader_recent_vaults::LIMIT)
                            .cloned()
                            .collect();
                        // The window that did not reopen is named by its reason
                        // too, even a quick viewer's folder off the list.
                        if let Some((last, Err(_))) = restore {
                            if !checked.contains(last) {
                                checked.push(last.clone());
                            }
                        }
                        reader_recent_vaults::check_all(
                            &checked,
                            &listing.network,
                            reader_recent_vaults::CHECK_PATIENCE,
                        )
                    })
                    .unwrap_or_default();
                let unavailable = |root: &PathBuf| {
                    status
                        .iter()
                        .any(|(checked, status)| checked == root && status.unavailable().is_some())
                };
                // The start screen lists vaults by their names too (#1364).
                let names: Vec<(PathBuf, String)> = history
                    .as_ref()
                    .map(|(_, roots, _)| {
                        roots
                            .iter()
                            .filter(|root| !unavailable(root))
                            .filter_map(|root| {
                                super::vault_name::read(root).map(|name| (root.clone(), name))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let sync_names = history
                    .as_ref()
                    .map(|(profile, _, _)| super::vault_name::read_sync_names(profile))
                    .unwrap_or_default();
                (windows, (history, names, status, sync_names, listing))
            })
            .await;
        let (history, names, status, sync_names, listing) = history;
        cx.update(|cx| {
            super::vault_name::remember(names, cx);
            super::vault_name::remember_sync(sync_names, cx);
        });
        if !restorable.is_empty() {
            let restored = cx.update(|cx| {
                if cx.global::<Startup>().superseded {
                    return None;
                }
                if let Ok((directory, ..)) = &history {
                    opts.session_directory = Some(directory.clone());
                }
                Some(restore_session_windows(restorable, opts.clone(), cx))
            });
            let Some(restoring) = restored else {
                return;
            };
            if restoring.await {
                return;
            }
            // No saved window could be opened: try the default, then the
            // existing last-used fallback when no default is configured.
        }
        cx.update(|cx| {
            if cx.global::<Startup>().superseded {
                return;
            }
            // The details go to the log; the start screen says it in plain words (#1368).
            let (roots, restore, notice) = match history {
                Ok((directory, roots, restore)) => {
                    opts.session_directory = Some(directory);
                    match restore {
                        Some((_, Ok(intent))) => (roots, Some(intent), None),
                        Some((last, Err(error))) => {
                            diagnostic_error!("Cannot restore {}: {error:#}", last.display());
                            // A listed vault shows why on its own row (#3208);
                            // a window off the list is named above it.
                            let notice = (!roots.contains(&last)).then(|| {
                                let reason = status
                                    .iter()
                                    .find(|(root, _)| root == &last)
                                    .and_then(|(_, status)| status.reason());
                                Notice::unopened(&last, reason, cx)
                            });
                            (roots, None, notice)
                        }
                        None => (roots, None, None),
                    }
                }
                Err(error) => {
                    diagnostic_error!("Cannot read the Reader session: {error:#}");
                    let notice = Notice {
                        text:
                            "Your last session couldn’t be restored. Choose a folder to continue."
                                .into(),
                        root: None,
                    };
                    (Vec::new(), None, Some(notice))
                }
            };
            let entry = EntryState {
                directory: opts.session_directory.clone(),
                status: status.into_iter().collect(),
                notice,
                listing,
            };
            if let Some(intent) = restore {
                let last = intent.root.clone();
                // Folder intent revalidates the last document and falls back if it disappeared.
                if intent.single_file {
                    // A folder view comes back as a folder view (#3098), and a
                    // quick viewer on the folder it was opened with, not on the
                    // folder of its last document (#3124).
                    opts.vault = Some(intent.root);
                    opts.note = intent.note;
                    opts.single_file = true;
                } else {
                    opts.vault = Some(intent.root);
                    opts.note = intent.note;
                }
                if let Err(error) = reader_open::open_window(opts, cx) {
                    diagnostic_error!("Cannot open {}: {error:#}", last.display());
                    let notice = Notice::unopened(&last, None, cx);
                    show_entry(
                        roots,
                        EntryState {
                            notice: Some(notice),
                            ..entry
                        },
                        cx,
                    );
                }
            } else {
                show_entry(roots, entry, cx);
            }
        });
    })
    .detach();
}

/// The last vault, and whether it can be reopened.
type Restore = (PathBuf, anyhow::Result<reader_open::OpenIntent>);

/// The last vault or quick viewer to reopen, and the vaults the start screen lists.
fn history(
    directory: anyhow::Result<PathBuf>,
) -> anyhow::Result<(PathBuf, Vec<PathBuf>, Option<Restore>)> {
    let directory = directory?;
    let (last, _) = reader_history::ReadingHistory::startup_roots(&directory)?;
    // Folders opened as vaults, newest first (#3208). A folder that only held
    // a file opened on its own, or a folder view, was never chosen as a vault.
    let roots = reader_history::ReadingHistory::known_vault_roots(&directory)?;
    // The caller tries the saved window set first. An explicit file/folder
    // delivery supersedes this entire path; a default never redirects it.
    let last = reader_history::ReadingHistory::default_vault(&directory)?.or(last);
    let restore = last.map(|root| {
        let intent = match reader_history::ReadingHistory::quick_document(&directory, &root) {
            // A folder view (#3098): the folder again, still not a vault.
            Ok(Some(note)) if note.is_empty() && root.is_dir() => Ok(reader_open::OpenIntent {
                root: root.clone(),
                single_file: true,
                note: None,
                created_note: false,
            }),
            Ok(Some(note)) if note.is_empty() => Err(anyhow::anyhow!(
                "The last folder view is missing or inaccessible"
            )),
            // The recorded root is the folder the window was opened with; a
            // document in a subfolder keeps it (#3124). Not a vault either way.
            Ok(Some(note))
                if matches!(root.join(&note).try_exists(), Ok(false)) && root.is_dir() =>
            {
                Ok(reader_open::OpenIntent {
                    root: root.clone(),
                    single_file: true,
                    note: None,
                    created_note: false,
                })
            }
            Ok(Some(note)) => {
                reader_open::OpenIntent::validate(&root.join(note), Some(&root), None).map(
                    |intent| reader_open::OpenIntent {
                        single_file: true,
                        ..intent
                    },
                )
            }
            Ok(None) => {
                let mapped =
                    reader_path_moves::Cutover::from_archive(&root, &roots).and_then(|cutover| {
                        let old = reader_history::ReadingHistory::last_document(&directory, &root)
                            .ok()??;
                        let target = cutover.target(&old)?;
                        Some((cutover.grove, target))
                    });
                match mapped {
                    Some((grove, target)) => {
                        let note = now_start_note(&directory, &grove).unwrap_or(target);
                        reader_open::OpenIntent::validate(&grove.join(note), Some(&grove), None)
                    }
                    None => reader_open::OpenIntent::validate(&root, Some(&root), None),
                }
            }
            Err(error) => Err(error),
        };
        (root, intent)
    });
    Ok((directory, roots, restore))
}

/// Reconcile all saved windows of one vault against the same move evidence.
fn restorable_windows(
    directory: &Path,
    windows: Vec<(super::reader_open_windows::Window, bool)>,
) -> Vec<(super::reader_open_windows::Window, bool)> {
    let mut paths = std::collections::BTreeMap::<PathBuf, Vec<String>>::new();
    for ((root, note), single_file) in &windows {
        if let Some(note) = note.as_ref().filter(|_| !single_file) {
            paths.entry(root.clone()).or_default().push(note.clone());
        }
    }
    let roots = reader_path_moves::known_roots(Some(directory));
    let cutovers: std::collections::BTreeMap<_, _> = paths
        .keys()
        .filter_map(|root| {
            Some((
                root.clone(),
                reader_path_moves::Cutover::from_archive(root, &roots)?,
            ))
        })
        .collect();
    let plans: std::collections::BTreeMap<_, _> = paths
        .into_iter()
        .map(|(root, saved)| {
            let plan = reader_path_moves::resolve(&root, Some(directory), saved, None);
            (root, plan)
        })
        .collect();
    windows
        .into_iter()
        .map(|((root, note), single_file)| {
            if !single_file {
                if let Some((cutover, target)) = cutovers
                    .get(&root)
                    .and_then(|cutover| Some((cutover, cutover.target(note.as_deref()?)?)))
                {
                    let note = now_start_note(directory, &cutover.grove).unwrap_or(target);
                    return ((cutover.grove.clone(), Some(note)), false);
                }
            }
            let start = (!single_file)
                .then(|| now_start_note(directory, &root))
                .flatten();
            let note = start.or_else(|| {
                note.and_then(|note| {
                    if let Some(target) = plans.get(&root).and_then(|plan| plan.target(&note)) {
                        return Some(target.to_owned());
                    }
                    if root.join(&note).is_file() {
                        Some(note)
                    } else {
                        // Keep the window in its original mode with no selected file.
                        (!single_file).then(String::new)
                    }
                })
            });
            ((root, note), single_file)
        })
        .collect()
}

/// Reopen every recorded vault window, one after another so the window that
/// was in front (recorded last) is in front again (#1325), each on the note
/// it showed (#1375). A vault that is gone or unreadable is skipped and named
/// in a notice; the others open. Resolves to whether any window opened.
pub(crate) fn restore_windows(
    windows: Vec<super::reader_open_windows::Window>,
    base: Opts,
    cx: &mut App,
) -> Task<bool> {
    // Explicit update-restart vault targets must ignore ordinary saved modes.
    restore_session_windows(
        windows.into_iter().map(|window| (window, false)).collect(),
        base,
        cx,
    )
}

fn restore_session_windows(
    windows: Vec<(super::reader_open_windows::Window, bool)>,
    base: Opts,
    cx: &mut App,
) -> Task<bool> {
    cx.spawn(async move |cx| {
        let mut opened = 0;
        let mut skipped = Vec::new();
        for ((root, note), single_file) in windows {
            // A single-file window without a file is a folder view (#3098);
            // `resolve` refuses a folder that is gone, and it is skipped below.
            // A second window of a vault already back: New Window's path,
            // sharing that window's session, not a second open (#1335).
            if let Some(source) = cx.update(|cx| {
                (!single_file)
                    .then(|| reader_open::reader_of(&root, cx))
                    .flatten()
            }) {
                let duplicating =
                    cx.update(|cx| reader_open::duplicate_window(source, note.clone(), cx));
                // It waits for the vault to load; one that is still loading
                // does not hold back the vaults after it.
                let patience = cx.background_executor().timer(RESTORE_PATIENCE);
                match futures::future::select(duplicating, patience).await {
                    futures::future::Either::Left((true, _)) => opened += 1,
                    futures::future::Either::Left((false, _)) => {}
                    futures::future::Either::Right((_, duplicating)) => duplicating.detach(),
                }
                continue;
            }
            let opts = Opts {
                vault: Some(root.clone()),
                note,
                single_file,
                restore_workspace: true,
                ..base.clone()
            };
            let resolving = cx.update(|cx| reader_open::resolve(opts, cx));
            match resolving.await {
                Ok(opts) => {
                    if single_file {
                        if let Err(error) =
                            cx.update(|cx| reader_open::create_window(opts, None, None, cx))
                        {
                            diagnostic_error!("Cannot reopen {}: {error:#}", root.display());
                            skipped.push(root);
                            continue;
                        }
                    } else {
                        cx.update(|cx| reader_open::finish_open(Ok(opts), cx));
                    }
                    opened += 1;
                    // One window at a time: the next opens once this one has
                    // its note, so restored windows never prepare at once.
                    for _ in 0..RESTORE_PATIENCE.as_millis() / 50 {
                        if cx.update(|cx| settled(&root, cx)) {
                            break;
                        }
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(50))
                            .await;
                    }
                }
                Err(error) => {
                    diagnostic_error!("Cannot reopen {}: {error:#}", root.display());
                    skipped.push(root);
                }
            }
        }
        if opened > 0 && !skipped.is_empty() {
            cx.update(|cx| skipped_notice(&skipped, cx));
        }
        opened > 0
    })
}

/// How long a restored window may take to show its note before the next opens.
const RESTORE_PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);

/// The window of this vault shows its note, or its preparation has ended.
fn settled(root: &std::path::Path, cx: &App) -> bool {
    reader_open::readers(cx).iter().any(|reader| {
        let reader = reader.read(cx);
        reader.vault_root == root
            && (reader.document_ready() || reader.loading.as_ref().is_some_and(|l| !l.active))
    })
}

/// A quiet notice in the front window naming the vaults that did not reopen.
fn skipped_notice(skipped: &[PathBuf], cx: &mut App) {
    let names = skipped
        .iter()
        .map(|root| super::vault_name::shown(root, cx))
        .collect::<Vec<_>>()
        .join(", ");
    let message = format!("Couldn’t reopen {names}: the folder is missing or can’t be read.");
    let Some(window) = cx.active_window().or_else(|| cx.windows().last().copied()) else {
        return;
    };
    let _ = window.update(cx, |_, window, cx| {
        super::reader_toast::transient(message, window, cx)
    });
}

/// A vault on the start screen: its name (display name, else folder) and
/// where it is, as every other place names a vault (#1364).
#[cfg_attr(not(test), allow(dead_code))] // The row draws name and path apart (#3208).
pub(crate) fn recent_label(root: &std::path::Path, cx: &App) -> String {
    format!(
        "{} — {}",
        super::vault_name::shown(root, cx),
        okilum_core::vault::display_path(root)
    )
}

/// A notice the start screen shows above its list: the history could not be
/// read, a window that is not on the list did not reopen, or the list could
/// not be updated. A listed vault's problem is shown on its own row (#3208).
#[derive(Clone, Debug)]
pub(crate) struct Notice {
    pub(crate) text: String,
    pub(crate) root: Option<PathBuf>,
}
impl Notice {
    fn unopened(root: &Path, reason: Option<String>, cx: &App) -> Self {
        let name = super::vault_name::shown(root, cx);
        let reason = reason.unwrap_or_else(|| "it can’t be opened right now".into());
        Self {
            text: format!("Couldn’t reopen “{name}”: {reason}."),
            root: Some(root.to_path_buf()),
        }
    }
}

/// What the start screen lists besides the vault roots, read off the UI
/// thread with the history (#3208).
#[derive(Clone, Debug, Default)]
pub(crate) struct Listing {
    /// Unsaved untitled drafts, newest first (#1528).
    pub(crate) drafts: Vec<super::reader_untitled::Listed>,
    /// When each vault was last opened (seconds since the epoch).
    pub(crate) last_opened: std::collections::HashMap<PathBuf, u64>,
    pub(crate) default_root: Option<PathBuf>,
    /// Vaults last seen on a network volume.
    pub(crate) network: std::collections::BTreeSet<PathBuf>,
}
impl Listing {
    fn read(directory: &Path) -> Self {
        let vaults = reader_history::ReadingHistory::known_vaults(directory).unwrap_or_default();
        Self {
            drafts: super::reader_untitled::list(directory),
            last_opened: vaults
                .iter()
                .filter_map(|vault| vault.last_opened.map(|at| (vault.root.clone(), at)))
                .collect(),
            default_root: vaults
                .iter()
                .find(|vault| vault.is_default)
                .map(|vault| vault.root.clone()),
            network: reader_history::ReadingHistory::network_roots(directory).unwrap_or_default(),
        }
    }
}

/// Start screen state besides the vault list.
#[derive(Default)]
pub(crate) struct EntryState {
    /// The Reader state the list comes from; removing a vault writes there.
    pub(crate) directory: Option<PathBuf>,
    /// Whether each checked vault opens, and why not (#3208).
    pub(crate) status: std::collections::HashMap<PathBuf, Status>,
    pub(crate) notice: Option<Notice>,
    pub(crate) listing: Listing,
}
impl EntryState {
    pub(crate) fn unavailable(&self, root: &Path) -> Option<&Unavailable> {
        self.status.get(root).and_then(Status::unavailable)
    }
}

/// The start window's smallest size: the narrow layout goes down to here.
fn entry_min_size() -> Size<Pixels> {
    size(px(400.), px(420.))
}

fn show_entry(roots: Vec<PathBuf>, state: EntryState, cx: &mut App) {
    let (mut options, frame_key) =
        super::window_state::prepare(reader_open::window_options(cx), "entry", cx);
    options.window_min_size = Some(entry_min_size());
    match cx.open_window(options, |window, cx| {
        super::sync_appearance(window, cx);
        window.set_window_title("Okilum");
        window
            .observe_window_appearance(|window, cx| {
                super::sync_appearance(window, cx);
                window.refresh();
            })
            .detach();
        let entry = cx.new(|cx| Entry::new(roots, state, window, cx));
        let root = cx.new(|cx| Root::new(entry, window, cx));
        super::window_state::track(&root, frame_key, window, cx);
        root
    }) {
        Ok(window) => {
            // Updating a typed WindowHandle<Root> also borrows Root. Notifications
            // update Root themselves, so use the window-only handle here.
            let window = AnyWindowHandle::from(window);
            cx.global_mut::<Startup>().entry = Some(window);
            #[cfg(target_os = "linux")]
            let _ = window.update(cx, |_, window, cx| {
                super::reader_sync_conflicts::show_startup_notice(window, cx);
            });
        }
        Err(error) => diagnostic_error!("Cannot open Reader entry: {error:#}"),
    }
}

/// New file (#3208): an untitled draft (#1528) from the start screen, where
/// no Reader exists to create one. The start screen closes once the draft's
/// window is up, as for any other open.
pub(crate) fn new_file(cx: &mut App) {
    let directory = cx
        .try_global::<Startup>()
        .and_then(|startup| startup.entry)
        .and_then(|window| {
            window
                .downcast::<Root>()?
                .read(cx)
                .ok()?
                .view()
                .clone()
                .downcast::<Entry>()
                .ok()
        })
        .and_then(|entry| entry.read(cx).state.directory.clone());
    #[cfg(test)]
    let directory = directory.or_else(|| {
        cx.try_global::<reader_history::TestSessionDirectory>()
            .map(|directory| directory.0.clone())
    });
    #[cfg(not(test))]
    let directory = directory.or_else(|| reader_history::state_directory().ok());
    let Some(session) = directory else {
        diagnostic_error!("Draft storage is unavailable, so no untitled file was created");
        return;
    };
    let creating = cx.background_executor().spawn({
        let session = session.clone();
        async move { super::reader_untitled::create(&session, None) }
    });
    cx.spawn(async move |cx| {
        let created = creating.await;
        cx.update(|cx| match created {
            Ok(file) => {
                if let Err(error) = reader_open::open_window(
                    Opts {
                        open_path: Some(file),
                        single_file: true,
                        created_note: true,
                        session_directory: Some(session),
                        ..Default::default()
                    },
                    cx,
                ) {
                    reader_open::show_error(error, cx);
                }
            }
            Err(error) => {
                diagnostic_error!("Could not create an untitled draft: {error:#}");
                reader_open::show_error(
                    anyhow::anyhow!("The untitled file couldn’t be created."),
                    cx,
                );
            }
        });
    })
    .detach();
}

/// One row of the recents list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Row {
    /// An unsaved untitled draft: its file.
    Draft(PathBuf),
    Vault(PathBuf),
}

pub(crate) struct Entry {
    /// Every vault in the history, newest first.
    pub(crate) roots: Vec<PathBuf>,
    pub(crate) state: EntryState,
    pub(crate) view: super::reader_welcome::View,
}
impl Entry {
    fn new(
        roots: Vec<PathBuf>,
        state: EntryState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = super::reader_welcome::View::new(window, cx);
        // Availability is checked again whenever the window comes back to
        // the front: a volume may have been mounted meanwhile (#3208).
        view.keep(cx.observe_window_activation(window, |entry, window, cx| {
            if window.is_window_active() {
                entry.recheck(cx);
            }
        }));
        let mut entry = Self { roots, state, view };
        entry.select_first_available(cx);
        let first_launch = entry.first_launch();
        entry.view.focus_initial(first_launch, window, cx);
        #[cfg(feature = "welcome-ui-harness")]
        super::reader_welcome::apply_harness(&mut entry, window, cx);
        entry
    }

    /// Nothing to list: the first-launch screen (#3208).
    pub(crate) fn first_launch(&self) -> bool {
        self.roots.is_empty() && self.state.listing.drafts.is_empty()
    }

    /// The vaults listed without Show all: the newest
    /// [`reader_recent_vaults::LIMIT`], less the drafts listed above them.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn shown(&self) -> &[PathBuf] {
        let (_, vaults, _) = super::reader_welcome::compose(
            self.state.listing.drafts.len(),
            self.roots.len(),
            false,
        );
        &self.roots[..vaults]
    }

    fn select_first_available(&mut self, cx: &App) {
        let rows = self.rows(cx);
        self.view.selected = rows
            .iter()
            .position(|row| match row {
                Row::Vault(root) => self.state.unavailable(root).is_none(),
                Row::Draft(_) => false,
            })
            .unwrap_or(0);
    }

    /// The rows on screen now: drafts first, then vaults, filtered by the
    /// query or capped unless Show all.
    pub(crate) fn rows(&self, cx: &App) -> Vec<Row> {
        super::reader_welcome::visible_rows(self, cx)
    }

    pub(crate) fn open_row(&mut self, row: Row, cx: &mut Context<Self>) {
        match row {
            Row::Vault(root) => self.open(root, cx),
            Row::Draft(file) => {
                if let Err(error) = reader_open::open_window(
                    Opts {
                        open_path: Some(file),
                        single_file: true,
                        session_directory: self.state.directory.clone(),
                        ..Default::default()
                    },
                    cx,
                ) {
                    reader_open::show_error(error, cx);
                }
            }
        }
    }

    pub(crate) fn open(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.view.start_opening(Row::Vault(root.clone()), cx);
        // The folder may have gone while the start screen was up. Checked off
        // the UI thread: a dead network share must not stall the window.
        let network = self.state.listing.network.clone();
        let checking = cx.background_executor().spawn({
            let root = root.clone();
            async move {
                reader_recent_vaults::check_all(
                    std::slice::from_ref(&root),
                    &network,
                    reader_recent_vaults::CHECK_PATIENCE,
                )
                .pop()
                .map_or(Status::Ready, |(_, status)| status)
            }
        });
        cx.spawn(async move |entry, cx| {
            let status = checking.await;
            let _ = entry.update(cx, |entry, cx| {
                let unavailable = status.unavailable().is_some();
                entry.state.status.insert(root.clone(), status);
                if unavailable {
                    entry.view.opening = None;
                    cx.notify();
                    return;
                }
                if let Err(error) = reader_open::open_window(
                    Opts {
                        vault: Some(root.clone()),
                        ..Default::default()
                    },
                    cx,
                ) {
                    diagnostic_error!("Cannot open recent folder: {error:#}");
                    entry.view.opening = None;
                    entry.state.status.insert(
                        root.clone(),
                        Status::Unavailable(Unavailable::CantOpen(format!("{error:#}"))),
                    );
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Check every listed vault again (window back in front, Try again).
    pub(crate) fn recheck(&mut self, cx: &mut Context<Self>) {
        let roots: Vec<PathBuf> = self
            .roots
            .iter()
            .take(2 * reader_recent_vaults::LIMIT)
            .cloned()
            .collect();
        self.check(roots, cx);
    }

    /// Check these vaults off the UI thread and show what was found.
    pub(crate) fn check(&mut self, roots: Vec<PathBuf>, cx: &mut Context<Self>) {
        if roots.is_empty() {
            return;
        }
        let network = self.state.listing.network.clone();
        let checking = cx.background_executor().spawn(async move {
            reader_recent_vaults::check_all(&roots, &network, reader_recent_vaults::CHECK_PATIENCE)
        });
        cx.spawn(async move |entry, cx| {
            let checked = checking.await;
            let _ = entry.update(cx, |entry, cx| {
                for (root, status) in checked {
                    entry.state.status.insert(root, status);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Try again on one vault; if it is still unavailable, its row briefly
    /// says so.
    pub(crate) fn try_again(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let network = self.state.listing.network.clone();
        let checking = cx.background_executor().spawn({
            let root = root.clone();
            async move {
                reader_recent_vaults::check_all(
                    std::slice::from_ref(&root),
                    &network,
                    reader_recent_vaults::CHECK_PATIENCE,
                )
            }
        });
        cx.spawn(async move |entry, cx| {
            let checked = checking.await;
            let _ = entry.update(cx, |entry, cx| {
                let still = checked
                    .iter()
                    .any(|(_, status)| status.unavailable().is_some());
                for (checked, status) in checked {
                    entry.state.status.insert(checked, status);
                }
                if still {
                    entry.view.still_unavailable(root, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Locate… / Allow access…: the folder chosen replaces the entry and
    /// keeps its colour (its display name lives in the folder itself).
    pub(crate) fn locate(&mut self, old: PathBuf, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose folder".into()),
        });
        cx.spawn(async move |entry, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            let Some(chosen) = paths.into_iter().next() else {
                return;
            };
            let chosen = cx
                .background_executor()
                .spawn(async move { okilum_core::vault::canonical_root(&chosen).ok() })
                .await;
            let Some(chosen) = chosen else {
                return;
            };
            let _ = entry.update(cx, |entry, cx| {
                if chosen != old {
                    let color = super::reader_ui_state::vault_color(&old, cx);
                    if color.is_some() {
                        super::reader_ui_state::set_vault_color(&chosen, color, cx);
                    }
                    if let Some(index) = entry.roots.iter().position(|root| root == &old) {
                        entry.roots[index] = chosen.clone();
                    }
                    entry.state.status.remove(&old);
                    entry.forget_in_history(old.clone(), cx);
                }
                entry.open(chosen, cx);
            });
        })
        .detach();
    }

    fn forget_in_history(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let Some(directory) = self.state.directory.clone() else {
            return;
        };
        cx.background_executor()
            .spawn(async move {
                if let Err(error) = reader_history::ReadingHistory::forget_root(&directory, &root) {
                    diagnostic_error!("Cannot replace {} in the list: {error:#}", root.display());
                }
            })
            .detach();
    }

    /// Take a vault off the list; its folder is left alone. Undo lists it
    /// again in the same place.
    pub(crate) fn remove(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let directory = self.state.directory.clone();
        let hiding = cx.background_executor().spawn({
            let root = root.clone();
            async move {
                match directory {
                    Some(directory) => reader_history::ReadingHistory::hide_roots(
                        &directory,
                        std::slice::from_ref(&root),
                    ),
                    None => Ok(()),
                }
            }
        });
        cx.spawn(async move |entry, cx| {
            let result = hiding.await;
            let _ = entry.update(cx, |entry, cx| {
                match result {
                    Ok(()) => {
                        if let Some(index) = entry.roots.iter().position(|old| old == &root) {
                            entry.roots.remove(index);
                            entry.view.removed(root.clone(), index, cx);
                        }
                        entry.state.status.remove(&root);
                        if entry
                            .state
                            .notice
                            .as_ref()
                            .is_some_and(|notice| notice.root.as_ref() == Some(&root))
                        {
                            entry.state.notice = None;
                        }
                        let rows = entry.rows(cx).len();
                        entry.view.clamp_selection(rows);
                    }
                    Err(error) => {
                        diagnostic_error!(
                            "Cannot remove {} from the list: {error:#}",
                            root.display()
                        );
                        entry.state.notice = Some(Notice {
                            text: "The list couldn’t be updated. Try again.".into(),
                            root: Some(root),
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Undo of Remove: the vault returns to its place in the list.
    pub(crate) fn undo_remove(&mut self, root: PathBuf, index: usize, cx: &mut Context<Self>) {
        let directory = self.state.directory.clone();
        let showing = cx.background_executor().spawn({
            let root = root.clone();
            async move {
                match directory {
                    Some(directory) => reader_history::ReadingHistory::unhide_roots(
                        &directory,
                        std::slice::from_ref(&root),
                    ),
                    None => Ok(()),
                }
            }
        });
        cx.spawn(async move |entry, cx| {
            let result = showing.await;
            let _ = entry.update(cx, |entry, cx| {
                match result {
                    Ok(()) if !entry.roots.contains(&root) => {
                        let index = index.min(entry.roots.len());
                        entry.roots.insert(index, root.clone());
                        entry.check(vec![root], cx);
                    }
                    Ok(()) => {}
                    Err(error) => {
                        diagnostic_error!("Cannot undo the removal: {error:#}");
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Discard an unsaved draft: its folder goes, after the person agreed.
    pub(crate) fn discard_draft(&mut self, file: PathBuf, cx: &mut Context<Self>) {
        let removing = cx.background_executor().spawn({
            let file = file.clone();
            async move {
                match file.parent() {
                    Some(folder) => std::fs::remove_dir_all(folder),
                    None => Ok(()),
                }
            }
        });
        cx.spawn(async move |entry, cx| {
            let result = removing.await;
            let _ = entry.update(cx, |entry, cx| {
                match result {
                    Ok(()) => {
                        entry
                            .state
                            .listing
                            .drafts
                            .retain(|draft| draft.file != file);
                        let rows = entry.rows(cx).len();
                        entry.view.clamp_selection(rows);
                    }
                    Err(error) => {
                        diagnostic_error!("Cannot discard {}: {error:#}", file.display());
                        entry.state.notice = Some(Notice {
                            text: "The draft couldn’t be discarded. Try again.".into(),
                            root: None,
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Clear list (#3208): every listed vault leaves the list; the folders
    /// and their reading places are left alone, unsaved drafts stay, and
    /// opening one as a vault again lists it again.
    pub(crate) fn clear(&mut self, cx: &mut Context<Self>) {
        let directory = self.state.directory.clone();
        let roots = self.roots.clone();
        let hiding = cx.background_executor().spawn(async move {
            match directory {
                Some(directory) => reader_history::ReadingHistory::hide_roots(&directory, &roots),
                None => Ok(()),
            }
        });
        cx.spawn(async move |entry, cx| {
            let result = hiding.await;
            let _ = entry.update(cx, |entry, cx| {
                match result {
                    Ok(()) => {
                        entry.roots.clear();
                        entry.state.status.clear();
                        entry.state.notice = None;
                        entry.view.cleared();
                    }
                    Err(error) => {
                        diagnostic_error!("Cannot clear the list of vaults: {error:#}");
                        entry.state.notice = Some(Notice {
                            text: "The list couldn’t be updated. Try again.".into(),
                            root: None,
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `gpui::*` brings its own `test`, which `#[gpui::test]` expands into.
    use ::core::prelude::v1::test;

    #[test]
    fn session_restore_uses_the_start_note_only_for_vault_windows_in_saved_now_mode() {
        use crate::reader_sidebar::{LeftMode, State};

        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let root = fixture.path().join("vault");
        std::fs::create_dir_all(root.join(".okilum")).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(
            okilum_core::vault_descriptor::path(&root),
            r#"{"start":{"note":"Feed.md"}}"#,
        )
        .unwrap();
        for note in ["Feed.md", "Other.md"] {
            std::fs::write(root.join(note), "# Note").unwrap();
        }
        let window = (root.clone(), Some("Other.md".to_owned()));
        let windows = vec![(window.clone(), false), (window, true)];
        let sidebar_path = State::path(&state, &root);
        let mut sidebar = State::default();
        sidebar.choose_now(true);
        sidebar.select_left_mode(LeftMode::Tree);
        sidebar.save(&sidebar_path).unwrap();
        assert_eq!(restorable_windows(&state, windows.clone()), windows);

        sidebar.select_left_mode(LeftMode::Now);
        sidebar.save(&sidebar_path).unwrap();
        let restored = restorable_windows(&state, windows.clone());
        assert_eq!(restored[0], ((root.clone(), Some("Feed.md".into())), false));
        assert_eq!(restored[1], windows[1]);

        std::fs::remove_file(root.join("Feed.md")).unwrap();
        assert_eq!(restorable_windows(&state, windows.clone()), windows);
    }

    #[gpui::test]
    fn restore_keeps_vault_and_quick_windows_of_the_same_file(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("notes");
        let state = fixture.path().join("state");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(root.join("note.md"), "# Note\n").unwrap();
        std::fs::write(
            state.join("open-windows.json"),
            serde_json::to_vec(&serde_json::json!({"version": 1, "roots": [&root, &root],
                "notes": ["note.md", "note.md"], "single_file": [false, true]}))
            .unwrap(),
        )
        .unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            reader_open::install(cx);
            super::super::reader_open_windows::install(state.clone(), cx);
            launch(
                Opts {
                    session_directory: Some(state.clone()),
                    cache_base_override: Some(state.join("cache")),
                    ..Default::default()
                },
                cx,
            );
        });
        // Restoration waits between publications on the foreground timer.
        for _ in 0..60 {
            cx.run_until_parked();
            if cx.update(|cx| {
                let readers = reader_open::readers(cx);
                readers.len() == 2
                    && readers
                        .iter()
                        .all(|reader| reader.read(cx).document_ready())
            }) {
                break;
            }
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(100));
        }
        cx.update(|cx| {
            let readers = reader_open::readers(cx);
            assert_eq!(readers.len(), 2);
            let modes: Vec<_> = readers
                .iter()
                .map(|reader| {
                    let reader = reader.read(cx);
                    assert_eq!(reader.pane.current_rel, "note.md");
                    reader.single_file
                })
                .collect();
            assert_eq!(modes, vec![false, true]);
        });
    }

    fn default_priority(cx: &mut TestAppContext, explicit: bool) {
        use reader_history::ReadingHistory;
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let roots: Vec<_> = ["default", "restored", "explicit"]
            .into_iter()
            .map(|name| {
                let root = fixture.path().join(name);
                std::fs::create_dir(&root).unwrap();
                std::fs::write(root.join("note.md"), format!("# {name}\r\n")).unwrap();
                root.canonicalize().unwrap()
            })
            .collect();
        for root in &roots[..2] {
            ReadingHistory::record_usable_document(&state, root, "note.md").unwrap();
        }
        ReadingHistory::set_default_vault(&state, Some(&roots[0])).unwrap();
        std::fs::write(
            state.join("open-windows.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": 1, "roots": [&roots[1]], "notes": ["note.md"]
            }))
            .unwrap(),
        )
        .unwrap();
        let base = Opts {
            session_directory: Some(state.clone()),
            cache_base_override: Some(state.join("cache")),
            ..Default::default()
        };
        cx.update(|cx| {
            gpui_component::init(cx);
            reader_open::install(cx);
            crate::reader_ui_state::install(&state, cx);
            launch(base.clone(), cx);
            if explicit {
                // Deliver the explicit intent before startup's worker returns.
                reader_open::finish_open(
                    Ok(Opts {
                        vault: Some(roots[2].clone()),
                        note: Some("note.md".into()),
                        ..base
                    }),
                    cx,
                );
            }
        });
        cx.run_until_parked();
        cx.update(|cx| {
            let readers = reader_open::readers(cx);
            assert_eq!(readers.len(), 1, "no extra default or restored window");
            let reader = readers[0].read(cx);
            assert_eq!(reader.vault_root, roots[if explicit { 2 } else { 1 }]);
            assert!(
                reader.document_ready(),
                "positive control: usable note opened"
            );
            assert_eq!(reader.pane.current_rel, "note.md");
        });
        assert_eq!(
            ReadingHistory::default_vault(&state).unwrap(),
            Some(roots[0].clone())
        );
        for (root, name) in roots.iter().zip(["default", "restored", "explicit"]) {
            assert_eq!(
                std::fs::read(root.join("note.md")).unwrap(),
                format!("# {name}\r\n").as_bytes()
            );
        }
    }

    #[gpui::test]
    fn saved_windows_restore_before_the_default_vault(cx: &mut TestAppContext) {
        default_priority(cx, false);
    }

    #[gpui::test]
    fn explicit_open_supersedes_saved_windows_and_default_vault(cx: &mut TestAppContext) {
        default_priority(cx, true);
    }

    /// #3098: the last window was a folder view (a quick viewer with no
    /// file). Startup reopens that folder view instead of refusing it, and
    /// never as a vault.
    #[test]
    fn the_last_folder_view_reopens_as_a_folder_view() {
        use reader_history::ReadingHistory;
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let folder = fixture.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        ReadingHistory::record_document_mode(&state, &folder, "", true).unwrap();
        let (_, _, selected) = history(Ok(state.clone())).unwrap();
        let (root, intent) = selected.unwrap();
        assert_eq!(root, folder);
        let intent = intent.unwrap();
        assert!(intent.single_file && intent.note.is_none());
        assert_eq!(intent.root, folder);
        // Gone: a visible failure, not a silent vault or another folder.
        std::fs::remove_dir(&folder).unwrap();
        let (_, _, selected) = history(Ok(state)).unwrap();
        assert!(selected.unwrap().1.is_err());
    }

    /// #3124: the last window was a quick viewer or folder view on `A`
    /// showing `B/C.md`. The startup fallback keeps `A` as the root instead
    /// of narrowing it to `B`, and the file is listed in Recent.
    #[test]
    fn the_last_quick_viewer_keeps_the_folder_it_was_opened_with() {
        use reader_history::ReadingHistory;
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let folder = fixture.path().join("A");
        std::fs::create_dir_all(folder.join("B")).unwrap();
        let folder = folder.canonicalize().unwrap();
        std::fs::write(folder.join("B").join("C.md"), "# C").unwrap();
        ReadingHistory::record_document_mode(&state, &folder, "B/C.md", true).unwrap();
        let (_, roots, selected) = history(Ok(state.clone())).unwrap();
        let (root, intent) = selected.unwrap();
        assert_eq!(root, folder);
        assert_eq!(
            intent.unwrap(),
            reader_open::OpenIntent {
                root: folder.clone(),
                single_file: true,
                note: Some("B/C.md".into()),
                created_note: false,
            }
        );
        assert!(
            !roots.contains(&folder),
            "#3208: a quick viewer's folder is not a recent vault"
        );
        assert!(
            ReadingHistory::known_vault_roots(&state)
                .unwrap()
                .is_empty(),
            "still not a vault"
        );
        assert_eq!(
            ReadingHistory::quick_files(&state).unwrap()[0].0,
            folder.join("B/C.md"),
            "the document is in Recent"
        );
        // A document that is gone reopens its window in the original mode
        // with no file selected (#3267): still the folder `A`, never `B`,
        // and never a vault.
        std::fs::remove_file(folder.join("B").join("C.md")).unwrap();
        let (_, _, selected) = history(Ok(state)).unwrap();
        let (root, intent) = selected.unwrap();
        assert_eq!(root, folder);
        assert_eq!(
            intent.unwrap(),
            reader_open::OpenIntent {
                root: folder.clone(),
                single_file: true,
                note: None,
                created_note: false,
            }
        );
    }

    /// #3124, end to end: whether the window set was saved (Quit, macOS
    /// close) or is empty (the last window closed on Linux/Windows), the
    /// window comes back rooted at `A` on `B/C.md`, as a quick viewer.
    fn nested_quick_viewer_restores_on_its_folder(cx: &mut TestAppContext, saved_set: bool) {
        use reader_history::ReadingHistory;
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let folder = fixture.path().join("A");
        std::fs::create_dir_all(folder.join("B")).unwrap();
        let folder = folder.canonicalize().unwrap();
        std::fs::write(folder.join("top.md"), "# Top").unwrap();
        std::fs::write(folder.join("B").join("C.md"), "# C").unwrap();
        ReadingHistory::record_document_mode(&state, &folder, "B/C.md", true).unwrap();
        let (roots, notes, modes) = if saved_set {
            (vec![&folder], vec!["B/C.md"], vec![true])
        } else {
            (vec![], vec![], vec![])
        };
        std::fs::write(
            state.join("open-windows.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": 1, "roots": roots, "notes": notes, "single_file": modes
            }))
            .unwrap(),
        )
        .unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            reader_open::install(cx);
            launch(
                Opts {
                    session_directory: Some(state.clone()),
                    cache_base_override: Some(state.join("cache")),
                    ..Default::default()
                },
                cx,
            );
        });
        for _ in 0..60 {
            cx.run_until_parked();
            if cx.update(|cx| {
                reader_open::readers(cx)
                    .iter()
                    .any(|reader| reader.read(cx).document_ready())
            }) {
                break;
            }
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(100));
        }
        cx.update(|cx| {
            let readers = reader_open::readers(cx);
            assert_eq!(readers.len(), 1);
            let reader = readers[0].read(cx);
            assert!(reader.document_ready(), "positive control: document opened");
            assert_eq!(reader.vault_root, folder, "rooted at A, not at B");
            assert_eq!(reader.pane.current_rel, "B/C.md");
            assert!(reader.single_file, "still not a vault");
        });
    }

    #[gpui::test]
    fn issue3124_last_window_closed_reopens_on_the_opened_folder(cx: &mut TestAppContext) {
        nested_quick_viewer_restores_on_its_folder(cx, false);
    }

    #[gpui::test]
    fn issue3124_saved_window_set_reopens_on_the_opened_folder(cx: &mut TestAppContext) {
        nested_quick_viewer_restores_on_its_folder(cx, true);
    }

    #[test]
    fn default_vault_precedes_last_used_only_in_the_startup_fallback() {
        use reader_history::ReadingHistory;
        let fixture = tempfile::tempdir().unwrap();
        let state = fixture.path().join("state");
        let default = fixture.path().join("default");
        let recent = fixture.path().join("recent");
        for root in [&default, &recent] {
            std::fs::create_dir(root).unwrap();
            std::fs::write(root.join("note.md"), "# Synthetic note").unwrap();
            ReadingHistory::record_usable_document(&state, root, "note.md").unwrap();
        }
        ReadingHistory::set_default_vault(&state, Some(&default)).unwrap();
        let (_, _, selected) = history(Ok(state.clone())).unwrap();
        let (root, intent) = selected.unwrap();
        assert_eq!(root, default);
        assert_eq!(intent.unwrap().root, default.canonicalize().unwrap());
        std::fs::remove_dir_all(&default).unwrap();
        let (_, roots, selected) = history(Ok(state.clone())).unwrap();
        let (root, intent) = selected.unwrap();
        assert_eq!(
            root, default,
            "a missing default must not silently select another vault"
        );
        assert!(intent.is_err());
        assert!(roots.contains(&default), "unmounted defaults remain known");
        ReadingHistory::set_default_vault(&state, None).unwrap();
        let (_, _, selected) = history(Ok(state)).unwrap();
        assert_eq!(
            selected.unwrap().0,
            recent,
            "clearing the preference restores the existing last-used fallback"
        );
    }

    /// The start screen's state, read through its window.
    fn entry(cx: &mut TestAppContext) -> Entity<Entry> {
        cx.update(|cx| {
            let window = cx
                .global::<Startup>()
                .entry
                .expect("the start screen is up");
            window
                .downcast::<Root>()
                .unwrap()
                .read(cx)
                .unwrap()
                .view()
                .clone()
                .downcast::<Entry>()
                .unwrap()
        })
    }

    #[gpui::test]
    fn issue2805_replaced_start_window_names_its_close(cx: &mut TestAppContext) {
        struct VaultSurface;
        impl Render for VaultSurface {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                gpui::div()
            }
        }
        let state = tempfile::tempdir().unwrap();
        let trace =
            super::super::reader_diagnostics::Trace::new(Some(state.path().to_owned()), None);
        cx.update(|cx| {
            crate::test_init(cx);
            cx.set_global(super::super::reader_diagnostics::LaunchTrace(trace.clone()));
            trace.record_window_closes(cx);
            cx.set_global(Startup::default());
            show_entry(
                Vec::new(),
                EntryState {
                    directory: None,
                    ..Default::default()
                },
                cx,
            );
        });
        cx.run_until_parked();
        cx.add_window(|_, _| VaultSurface);
        cx.update(supersede);
        cx.run_until_parked();
        let log = std::fs::read_to_string(
            state
                .path()
                .join(super::super::reader_diagnostics::LOG_FILE),
        )
        .unwrap();
        let closed: Vec<serde_json::Value> = log
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|event| event["phase"] == "window_closed")
            .collect();
        assert_eq!(closed.len(), 1, "{log}");
        assert_eq!(
            closed[0]["details"]["origin"],
            "start window replaced by a vault window"
        );
        assert_eq!(closed[0]["details"]["remaining_windows"], 1);
    }

    #[cfg(target_os = "linux")]
    #[gpui::test]
    fn issue2044_startup_recovery_notice_is_visible_without_a_vault(cx: &mut TestAppContext) {
        use gpui_component::WindowExt;
        cx.update(|cx| {
            crate::test_init(cx);
            cx.set_global(Startup::default());
            cx.set_global(super::super::reader_sync_conflicts::StartupNotice(vec![
                "Recovery needs attention; use Recover notes.".into(),
            ]));
            show_entry(
                Vec::new(),
                EntryState {
                    directory: None,
                    ..Default::default()
                },
                cx,
            );
        });
        cx.run_until_parked();
        cx.update(|cx| {
            let window = cx.global::<Startup>().entry.unwrap();
            window
                .update(cx, |_, window, cx| {
                    assert_eq!(
                        window.notifications(cx).len(),
                        1,
                        "global notice on the start screen"
                    );
                })
                .unwrap();
            assert!(cx
                .global::<super::super::reader_sync_conflicts::StartupNotice>()
                .0
                .is_empty());
        });
    }

    #[gpui::test]
    fn a_missing_last_vault_is_named_plainly_and_can_be_removed(cx: &mut TestAppContext) {
        // #1368, #3208: the folder of the last vault is gone. Its own row
        // says so in plain words (no detached notice), and Remove takes it
        // off the list.
        use reader_history::ReadingHistory;
        let fixture =
            std::env::temp_dir().join(format!("okilum-missing1368-{}", uuid::Uuid::new_v4()));
        let state = fixture.join("state");
        let kept = fixture.join("Kept");
        let gone_root = fixture.join("Work Notes");
        for root in [&kept, &gone_root] {
            std::fs::create_dir_all(root).unwrap();
            std::fs::write(root.join("a.md"), "# A").unwrap();
        }
        let kept = kept.canonicalize().unwrap();
        let gone_root = gone_root.canonicalize().unwrap();
        ReadingHistory::record_usable_document(&state, &kept, "a.md").unwrap();
        ReadingHistory::record_usable_document(&state, &gone_root, "a.md").unwrap();
        std::fs::remove_dir_all(&gone_root).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            reader_open::install(cx);
            launch(
                Opts {
                    session_directory: Some(state.clone()),
                    ..Default::default()
                },
                cx,
            );
        });
        cx.run_until_parked();
        assert_eq!(cx.windows().len(), 1, "the start screen, no vault window");
        let entry = entry(cx);
        entry.read_with(cx, |entry, _| {
            assert!(
                entry.state.notice.is_none(),
                "the row says why, not a notice"
            );
            assert_eq!(
                entry
                    .state
                    .status
                    .get(&gone_root)
                    .and_then(Status::reason)
                    .as_deref(),
                Some("Folder was moved or deleted")
            );
            assert_eq!(entry.roots, vec![gone_root.clone(), kept.clone()]);
            assert_eq!(
                entry.view.selected, 1,
                "the first available vault is selected, so Enter reopens it"
            );
            assert_eq!(
                entry.state.unavailable(&gone_root),
                Some(&Unavailable::Missing)
            );
            assert_eq!(
                entry.state.unavailable(&kept),
                None,
                "a vault that is there is not marked"
            );
        });

        // Choosing the missing vault in the list does not try to open it.
        entry.update(cx, |entry, cx| entry.open(gone_root.clone(), cx));
        cx.run_until_parked();
        assert_eq!(cx.windows().len(), 1);
        entry.read_with(cx, |entry, _| {
            assert_eq!(
                entry.state.unavailable(&gone_root),
                Some(&Unavailable::Missing)
            );
            assert!(entry.view.opening.is_none(), "nothing is left opening");
        });

        // Removing it takes it off the list and out of the saved history; the
        // other vault stays.
        entry.update(cx, |entry, cx| entry.remove(gone_root.clone(), cx));
        cx.run_until_parked();
        entry.read_with(cx, |entry, _| {
            assert_eq!(entry.roots, vec![kept.clone()]);
            assert!(entry.state.notice.is_none(), "its notice goes with it");
            assert_eq!(entry.state.unavailable(&gone_root), None);
        });
        let (last, roots) = ReadingHistory::startup_roots(&state).unwrap();
        assert_eq!(last, Some(kept.clone()));
        assert_eq!(roots, vec![kept.clone()]);
        std::fs::remove_dir_all(fixture).unwrap();
    }

    /// #3208: the start screen lists folders opened as vaults, newest first,
    /// at most `LIMIT`; a folder that only held a file opened on its own is
    /// not one. Clear list empties it for good.
    #[gpui::test]
    fn issue3208_recent_vaults_are_chosen_vaults_newest_first_capped(cx: &mut TestAppContext) {
        use reader_history::ReadingHistory;
        use reader_recent_vaults::LIMIT;
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path().canonicalize().unwrap();
        let state = base.join("state");
        let mut vaults = Vec::new();
        let quick = base.join("Downloads");
        for index in 0..LIMIT + 2 {
            let root = base.join(format!("Vault {index:02}"));
            std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("a.md"), "# A").unwrap();
            if index == LIMIT {
                // A file opened on its own from a folder: never a vault.
                std::fs::create_dir(&quick).unwrap();
                std::fs::write(quick.join("readme.md"), "# Readme").unwrap();
                ReadingHistory::record_document_mode(&state, &quick, "readme.md", true).unwrap();
            }
            ReadingHistory::record_usable_document(&state, &root, "a.md").unwrap();
            vaults.push(root);
        }
        // The newest vault is gone, so startup shows the start screen.
        let newest = vaults.last().unwrap().clone();
        std::fs::remove_dir_all(&newest).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            reader_open::install(cx);
            launch(
                Opts {
                    session_directory: Some(state.clone()),
                    ..Default::default()
                },
                cx,
            );
        });
        cx.run_until_parked();
        assert_eq!(cx.windows().len(), 1, "the start screen, no vault window");
        let entry = entry(cx);
        let newest_first: Vec<PathBuf> = vaults.iter().rev().cloned().collect();
        entry.read_with(cx, |entry, _| {
            assert_eq!(
                entry.roots, newest_first,
                "chosen vaults only, newest first"
            );
            assert_eq!(entry.shown(), &newest_first[..LIMIT]);
            assert_eq!(
                entry.state.unavailable(&newest),
                Some(&Unavailable::Missing)
            );
            assert_eq!(
                entry
                    .state
                    .status
                    .get(&newest)
                    .and_then(Status::reason)
                    .as_deref(),
                Some("Folder was moved or deleted")
            );
            assert_eq!(entry.state.unavailable(&vaults[0]), None);
        });

        entry.update(cx, |entry, cx| entry.clear(cx));
        cx.run_until_parked();
        entry.read_with(cx, |entry, _| {
            assert!(entry.roots.is_empty());
            assert!(entry.shown().is_empty());
            assert!(entry.state.notice.is_none());
        });
        assert!(ReadingHistory::known_vault_roots(&state)
            .unwrap()
            .is_empty());
        // Only the quick viewer's folder is still remembered, as before.
        assert_eq!(
            ReadingHistory::startup_roots(&state).unwrap().1,
            vec![quick]
        );
    }

    /// #3208: type-to-filter narrows the list by name or path, ⌘n opens the
    /// nth visible row, and New file creates an untitled draft that the
    /// start screen then lists at the top once it holds text.
    #[gpui::test]
    fn issue3208_filter_number_keys_and_new_file(cx: &mut TestAppContext) {
        use reader_history::ReadingHistory;
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path().canonicalize().unwrap();
        let state = base.join("state");
        let mut vaults = Vec::new();
        for name in [
            "Gone",
            "Personal",
            "Zettel notes",
            "Quokka",
            "zettel-archive",
        ] {
            let root = base.join(name);
            std::fs::create_dir(&root).unwrap();
            vaults.push(root);
        }
        // Recorded oldest first: zettel-archive ends up newest, Gone (deleted
        // below) last of all, so startup shows the start screen.
        for root in vaults[1..].iter().chain(std::iter::once(&vaults[0])) {
            ReadingHistory::record_usable_document(&state, root, "a.md").unwrap();
        }
        std::fs::remove_dir_all(&vaults[0]).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            reader_open::install(cx);
            cx.set_global(reader_history::TestSessionDirectory(state.clone()));
            launch(
                Opts {
                    session_directory: Some(state.clone()),
                    ..Default::default()
                },
                cx,
            );
        });
        cx.run_until_parked();
        let entry = entry(cx);
        let window = cx.update(|cx| cx.global::<Startup>().entry.unwrap());
        entry.update(cx, |entry, cx| entry.set_query("zettel".into(), cx));
        entry.read_with(cx, |entry, cx| {
            assert_eq!(
                entry.rows(cx),
                vec![Row::Vault(vaults[4].clone()), Row::Vault(vaults[2].clone()),],
                "names holding «zettel», newest first"
            );
        });
        // ⌘2 opens the second visible row of the filtered list.
        window
            .update(cx, |_, window, cx| {
                entry.update(cx, |entry, cx| entry.open_nth(2, window, cx))
            })
            .unwrap();
        entry.read_with(cx, |entry, _| {
            assert_eq!(entry.view.opening, Some(Row::Vault(vaults[2].clone())));
            assert_eq!(entry.view.selected, 1);
        });
        // A path match counts too.
        let parent = base.file_name().unwrap().to_string_lossy().into_owned();
        entry.update(cx, |entry, cx| entry.set_query("quokka".into(), cx));
        entry.read_with(cx, |entry, cx| {
            assert_eq!(entry.rows(cx), vec![Row::Vault(vaults[3].clone())]);
        });
        entry.update(cx, |entry, cx| entry.set_query(parent, cx));
        entry.read_with(cx, |entry, cx| {
            assert_eq!(
                entry.rows(cx).len(),
                5,
                "every vault sits in the fixture folder"
            );
        });

        // New file: a draft in the state directory, opened on its own.
        cx.update(new_file);
        cx.run_until_parked();
        let created: Vec<_> = std::fs::read_dir(state.join("untitled"))
            .unwrap()
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .collect();
        assert_eq!(created.len(), 1, "one untitled draft folder");
        let file = created[0].path().join("Untitled.md");
        assert!(file.is_file());
        std::fs::write(&file, "# Plan\n").unwrap();
        let listing = Listing::read(&state);
        assert_eq!(listing.drafts.len(), 1);
        assert_eq!(listing.drafts[0].label, "Plan");
    }
}
