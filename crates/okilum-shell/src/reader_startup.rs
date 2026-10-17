//! Local Reader startup. Saved managed identities are never read on this path.
use super::{reader_history, reader_open, Opts};
use gpui::{prelude::FluentBuilder, *};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex, v_flex, ActiveTheme, Root, Sizable, TitleBar,
};
use std::path::PathBuf;

#[derive(Default)]
pub(crate) struct Startup {
    superseded: bool,
    entry: Option<AnyWindowHandle>,
}
impl Global for Startup {}

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
    if cx.windows().iter().any(|window| *window != entry) {
        cx.global_mut::<Startup>().entry = None;
        let _ = entry.update(cx, |_, window, _| window.remove_window());
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
                // The window set does not depend on the reading history: a
                // damaged history still reopens every recorded window.
                let windows = directory
                    .as_deref()
                    .map(super::reader_open_windows::load)
                    .unwrap_or_default();
                let history = history(directory);
                // The start screen lists vaults by their names too (#1364).
                let names: Vec<(PathBuf, String)> = history
                    .as_ref()
                    .map(|(_, roots, _)| {
                        roots
                            .iter()
                            .filter_map(|root| {
                                super::vault_name::read(root).map(|name| (root.clone(), name))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                // Vaults whose folder is gone are marked on the start screen (#1368).
                let missing: Vec<PathBuf> = history
                    .as_ref()
                    .map(|(_, roots, _)| roots.iter().filter(|root| gone(root)).cloned().collect())
                    .unwrap_or_default();
                (windows, (history, names, missing))
            })
            .await;
        let (history, names, missing) = history;
        cx.update(|cx| super::vault_name::remember(names, cx));
        if !restorable.is_empty() {
            let restored = cx.update(|cx| {
                if cx.global::<Startup>().superseded {
                    return None;
                }
                if let Ok((directory, ..)) = &history {
                    opts.session_directory = Some(directory.clone());
                }
                Some(restore_windows(restorable, opts.clone(), cx))
            });
            let Some(restoring) = restored else {
                return;
            };
            if restoring.await {
                return;
            }
            // Not one recorded vault could be opened: the last vault, as before.
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
                            eprintln!("Cannot restore {}: {error:#}", last.display());
                            let notice = Notice::unopened(&last, missing.contains(&last), cx);
                            (roots, None, Some(notice))
                        }
                        None => (roots, None, None),
                    }
                }
                Err(error) => {
                    eprintln!("Cannot read the Reader session: {error:#}");
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
                missing: missing.into_iter().collect(),
                notice,
            };
            if let Some(intent) = restore {
                // Folder intent revalidates the last document and falls back if it disappeared.
                if intent.single_file {
                    opts.open_path = intent.note.map(|note| intent.root.join(note));
                } else {
                    opts.vault = Some(intent.root);
                }
                let last = intent.root.clone();
                if let Err(error) = reader_open::open_window(opts, cx) {
                    eprintln!("Cannot open {}: {error:#}", last.display());
                    let notice = Notice::unopened(&last, false, cx);
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

/// The last vault or quick viewer to reopen, and the vaults the start screen lists.
fn history(
    directory: anyhow::Result<PathBuf>,
) -> anyhow::Result<(
    PathBuf,
    Vec<PathBuf>,
    Option<(PathBuf, anyhow::Result<reader_open::OpenIntent>)>,
)> {
    let directory = directory?;
    let (last, roots) = reader_history::ReadingHistory::startup_roots(&directory)?;
    let restore = last.map(|root| {
        let intent = match reader_history::ReadingHistory::quick_document(&directory, &root) {
            Ok(Some(note)) if note.is_empty() => Err(anyhow::anyhow!(
                "The last quick viewer has no selected document"
            )),
            Ok(Some(note)) => reader_open::OpenIntent::validate(&root.join(note), None, None),
            Ok(None) => reader_open::OpenIntent::validate(&root, Some(&root), None),
            Err(error) => Err(error),
        };
        (root, intent)
    });
    Ok((directory, roots, restore))
}

/// Reopen every recorded vault window, one after another so the window that
/// was in front (recorded last) is in front again (#1325). A vault that is
/// gone or unreadable is skipped and named in a notice; the others open.
/// Resolves to whether any window opened.
pub(crate) fn restore_windows(roots: Vec<PathBuf>, base: Opts, cx: &mut App) -> Task<bool> {
    cx.spawn(async move |cx| {
        let mut opened = 0;
        let mut skipped = Vec::new();
        for root in roots {
            // A second window of a vault already back: New Window's path,
            // sharing that window's session, not a second open (#1335).
            if let Some(source) = cx.update(|cx| reader_open::reader_of(&root, cx)) {
                let duplicating = cx.update(|cx| reader_open::duplicate_window(source, cx));
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
                ..base.clone()
            };
            let resolving = cx.update(|cx| reader_open::resolve(opts, cx));
            match resolving.await {
                Ok(opts) => {
                    cx.update(|cx| reader_open::finish_open(Ok(opts), cx));
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
                    eprintln!("Cannot reopen {}: {error:#}", root.display());
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
pub(crate) fn recent_label(root: &std::path::Path, cx: &App) -> String {
    format!(
        "{} — {}",
        super::vault_name::shown(root, cx),
        okilum_core::vault::display_path(root)
    )
}

/// A vault folder that is no longer there (or no longer a folder).
fn gone(root: &std::path::Path) -> bool {
    !std::fs::metadata(root).is_ok_and(|metadata| metadata.is_dir())
}

/// What the start screen says about a vault it could not open, and which
/// vault, so it can offer to remove it from the list (#1368).
#[derive(Clone)]
struct Notice {
    text: String,
    root: Option<PathBuf>,
}
impl Notice {
    fn unopened(root: &std::path::Path, missing: bool, cx: &App) -> Self {
        let name = super::vault_name::shown(root, cx);
        let text = if missing {
            format!("“{name}” was moved or deleted. Choose its folder again, or remove it from the list.")
        } else {
            format!(
                "“{name}” can’t be opened right now. Choose a folder, or remove it from the list."
            )
        };
        Self {
            text,
            root: Some(root.to_path_buf()),
        }
    }
}

/// Start screen state besides the vault list.
struct EntryState {
    /// The Reader state the list comes from; removing a vault writes there.
    directory: Option<PathBuf>,
    missing: std::collections::HashSet<PathBuf>,
    notice: Option<Notice>,
}

fn show_entry(roots: Vec<PathBuf>, state: EntryState, cx: &mut App) {
    let (options, frame_key) =
        super::window_state::prepare(reader_open::window_options(cx), "entry", cx);
    match cx.open_window(options, |window, cx| {
        super::sync_appearance(window, cx);
        window.set_window_title("Okilum");
        window
            .observe_window_appearance(|window, cx| {
                super::sync_appearance(window, cx);
                window.refresh();
            })
            .detach();
        let entry = cx.new(|_| Entry { roots, state });
        let root = cx.new(|cx| Root::new(entry, window, cx));
        super::window_state::track(&root, frame_key, window, cx);
        root
    }) {
        Ok(window) => cx.global_mut::<Startup>().entry = Some(window.into()),
        Err(error) => eprintln!("Cannot open Reader entry: {error:#}"),
    }
}

struct Entry {
    roots: Vec<PathBuf>,
    state: EntryState,
}
impl Entry {
    fn open(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        // The folder may have gone while the start screen was up.
        if gone(&root) {
            self.state.missing.insert(root.clone());
            self.state.notice = Some(Notice::unopened(&root, true, cx));
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
            eprintln!("Cannot open recent folder: {error:#}");
            self.state.notice = Some(Notice::unopened(&root, false, cx));
            cx.notify();
        }
    }

    /// Take a vault off the list; its folder is left alone.
    fn remove(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let directory = self.state.directory.clone();
        let forgetting = cx.background_executor().spawn({
            let root = root.clone();
            async move {
                match directory {
                    Some(directory) => {
                        reader_history::ReadingHistory::forget_root(&directory, &root)
                    }
                    None => Ok(()),
                }
            }
        });
        cx.spawn(async move |entry, cx| {
            let result = forgetting.await;
            let _ = entry.update(cx, |entry, cx| {
                match result {
                    Ok(()) => {
                        entry.roots.retain(|old| old != &root);
                        entry.state.missing.remove(&root);
                        if entry
                            .state
                            .notice
                            .as_ref()
                            .is_some_and(|notice| notice.root.as_ref() == Some(&root))
                        {
                            entry.state.notice = None;
                        }
                    }
                    Err(error) => {
                        eprintln!("Cannot remove {} from the list: {error:#}", root.display());
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
}
impl Render for Entry {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut card = v_flex()
            .w(px(440.))
            .gap_4()
            .items_center()
            .child(super::brand::logo(52., cx))
            .child(div().text_2xl().child("Open your notes"))
            .child(div().text_center().text_color(cx.theme().muted_foreground).child(
                "Okilum works with your Markdown files in place. Nothing changes until you edit.",
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("startup-open-folder")
                            .primary()
                            .label("Open folder…")
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(reader_open::OpenFolder), cx)
                            }),
                    )
                    .child(
                        Button::new("startup-open-file")
                            .label("Open file…")
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(reader_open::OpenFile), cx)
                            }),
                    ),
            );
        if let Some(notice) = self.state.notice.clone() {
            let mut actions = h_flex().gap_2().child(
                Button::new("startup-notice-choose")
                    .small()
                    .label("Choose folder…")
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(reader_open::OpenFolder), cx)
                    }),
            );
            if let Some(root) = notice.root.filter(|root| self.roots.contains(root)) {
                actions = actions.child(
                    Button::new("startup-notice-remove")
                        .small()
                        .ghost()
                        .label("Remove from list")
                        .on_click(
                            cx.listener(move |entry, _, _, cx| entry.remove(root.clone(), cx)),
                        ),
                );
            }
            card = card.child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_center()
                            .text_color(cx.theme().danger)
                            .child(notice.text),
                    )
                    .child(actions),
            );
        }
        if !self.roots.is_empty() {
            let mut recent = v_flex()
                .w_full()
                .border_t_1()
                .border_color(cx.theme().border)
                .pt_4()
                .gap_2();
            for (index, root) in self.roots.iter().enumerate() {
                let missing = self.state.missing.contains(root);
                let label = if missing {
                    format!("{} · not found", recent_label(root, cx))
                } else {
                    recent_label(root, cx)
                };
                let open = root.clone();
                let mut row = h_flex().w_full().gap_1().child(
                    Button::new(("recent-folder", index))
                        .ghost()
                        .flex_1()
                        .label(label)
                        .when(missing, |button| {
                            // `Button` has a private method of this name.
                            Styled::text_color(button, cx.theme().muted_foreground)
                        })
                        .on_click(cx.listener(move |entry, _, _, cx| entry.open(open.clone(), cx))),
                );
                if missing {
                    let remove = root.clone();
                    row =
                        row.child(
                            Button::new(("recent-remove", index))
                                .ghost()
                                .small()
                                .label("Remove")
                                .on_click(cx.listener(move |entry, _, _, cx| {
                                    entry.remove(remove.clone(), cx)
                                })),
                        );
                }
                recent = recent.child(row);
            }
            card = card.child(recent);
        }
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // Same height as the Reader header so the traffic lights sit where
            // they will after a folder opens (#365), but with nothing to show
            // it blends into the page instead of drawing a separate strip.
            .child(
                TitleBar::new()
                    .h(px(super::READER_HEADER_HEIGHT))
                    .bg(cx.theme().background)
                    .border_color(transparent_black())
                    .when(window.is_fullscreen(), |bar| bar.pl(px(10.))),
            )
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(card),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_missing_last_vault_is_named_plainly_and_can_be_removed(cx: &mut TestAppContext) {
        // #1368: the folder of the last vault is gone. The start screen names
        // it in plain words, marks it in the list and can take it off the list.
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
            gpui_component::init(cx);
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
            let notice = entry.state.notice.as_ref().expect("the start screen says why");
            assert_eq!(
                notice.text,
                "“Work Notes” was moved or deleted. Choose its folder again, or remove it from the list."
            );
            assert!(!notice.text.contains("os error"));
            assert_eq!(notice.root.as_ref(), Some(&gone_root));
            assert_eq!(entry.roots, vec![gone_root.clone(), kept.clone()]);
            assert!(entry.state.missing.contains(&gone_root));
            assert!(!entry.state.missing.contains(&kept), "a vault that is there is not marked");
        });

        // Choosing the missing vault in the list does not try to open it.
        entry.update(cx, |entry, cx| {
            entry.state.notice = None;
            entry.open(gone_root.clone(), cx);
        });
        cx.run_until_parked();
        assert_eq!(cx.windows().len(), 1);
        entry.read_with(cx, |entry, _| {
            assert!(entry
                .state
                .notice
                .as_ref()
                .is_some_and(|n| n.text.contains("moved or deleted")))
        });

        // Removing it takes it off the list and out of the saved history; the
        // other vault stays.
        entry.update(cx, |entry, cx| entry.remove(gone_root.clone(), cx));
        cx.run_until_parked();
        entry.read_with(cx, |entry, _| {
            assert_eq!(entry.roots, vec![kept.clone()]);
            assert!(entry.state.notice.is_none(), "its notice goes with it");
            assert!(entry.state.missing.is_empty());
        });
        let (last, roots) = ReadingHistory::startup_roots(&state).unwrap();
        assert_eq!(last, Some(kept.clone()));
        assert_eq!(roots, vec![kept.clone()]);
        std::fs::remove_dir_all(fixture).unwrap();
    }
}
