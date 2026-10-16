//! Local Reader startup. Saved managed identities are never read on this path.
use super::{reader_history, reader_open, Opts};
use gpui::{prelude::FluentBuilder, *};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex, v_flex, ActiveTheme, Root, TitleBar,
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
                (windows, (history, names))
            })
            .await;
        let (history, names) = history;
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
            let (roots, restore, error) = match history {
                Ok((directory, roots, restore)) => {
                    opts.session_directory = Some(directory);
                    match restore {
                        Some(Ok(intent)) => (roots, Some(intent), None),
                        Some(Err(error)) => (
                            roots,
                            None,
                            Some(format!(
                                "Cannot restore your last Reader session: {error:#}"
                            )),
                        ),
                        None => (roots, None, None),
                    }
                }
                Err(error) => (
                    Vec::new(),
                    None,
                    Some(format!(
                        "Cannot restore your last Reader session: {error:#}"
                    )),
                ),
            };
            if let Some(intent) = restore {
                // Folder intent revalidates the last document and falls back if it disappeared.
                if intent.single_file {
                    opts.open_path = intent.note.map(|note| intent.root.join(note));
                } else {
                    opts.vault = Some(intent.root);
                }
                if let Err(error) = reader_open::open_window(opts, cx) {
                    show_entry(
                        roots,
                        Some(format!("Cannot open your last folder: {error:#}")),
                        cx,
                    );
                }
            } else {
                show_entry(roots, error, cx);
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
    Option<anyhow::Result<reader_open::OpenIntent>>,
)> {
    let directory = directory?;
    let (last, roots) = reader_history::ReadingHistory::startup_roots(&directory)?;
    let restore =
        last.map(
            |root| match reader_history::ReadingHistory::quick_document(&directory, &root)? {
                Some(note) if note.is_empty() => Err(anyhow::anyhow!(
                    "The last quick viewer has no selected document"
                )),
                Some(note) => reader_open::OpenIntent::validate(&root.join(note), None, None),
                None => reader_open::OpenIntent::validate(&root, Some(&root), None),
            },
        );
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

fn show_entry(roots: Vec<PathBuf>, error: Option<String>, cx: &mut App) {
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
        let entry = cx.new(|_| Entry { roots, error });
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
    error: Option<String>,
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
        if let Some(error) = &self.error {
            card = card.child(div().text_color(cx.theme().danger).child(error.clone()));
        }
        if !self.roots.is_empty() {
            let mut recent = v_flex()
                .w_full()
                .border_t_1()
                .border_color(cx.theme().border)
                .pt_4()
                .gap_2();
            for (index, root) in self.roots.iter().enumerate() {
                let root = root.clone();
                let label = recent_label(&root, cx);
                recent = recent.child(
                    Button::new(("recent-folder", index))
                        .ghost()
                        .label(label)
                        .on_click(move |_, _, cx| {
                            if let Err(error) = reader_open::open_window(
                                Opts {
                                    vault: Some(root.clone()),
                                    ..Default::default()
                                },
                                cx,
                            ) {
                                eprintln!("Cannot open recent folder: {error:#}");
                            }
                        }),
                );
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
