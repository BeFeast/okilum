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
        let history =
            cx.background_executor()
                .spawn(async move {
                    let _phase = diagnostics
                        .as_ref()
                        .map(|trace| trace.phase("startup_history_and_root_validation"));
                    let directory = directory
                        .map(Ok)
                        .unwrap_or_else(reader_history::state_directory)?;
                    let (last, roots) = reader_history::ReadingHistory::startup_roots(&directory)?;
                    let restore = last.map(|root| {
                        match reader_history::ReadingHistory::quick_document(&directory, &root)? {
                            Some(note) if note.is_empty() => Err(anyhow::anyhow!(
                                "The last quick viewer has no selected document"
                            )),
                            Some(note) => {
                                reader_open::OpenIntent::validate(&root.join(note), None, None)
                            }
                            None => reader_open::OpenIntent::validate(&root, Some(&root), None),
                        }
                    });
                    Ok::<_, anyhow::Error>((directory, roots, restore))
                })
                .await;
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
                recent = recent.child(
                    Button::new(("recent-folder", index))
                        .ghost()
                        .label(root.display().to_string())
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
