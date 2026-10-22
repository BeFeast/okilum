//! Post-update information, separate from installing or promoting an update.
use crate::*;
use gpui_component::{notification::Notification, WindowExt};
mod model;
use model::{Build, Range};

struct Session {
    current: Option<Build>,
    pending_since: Option<u64>,
}
impl Global for Session {}

pub(super) fn install(cx: &mut App) {
    let current = Build::current();
    cx.set_global(Session {
        current: current.clone(),
        pending_since: None,
    });
    let Some(current) = current else { return };
    cx.spawn(async move |cx| {
        let since = cx
            .background_executor()
            .spawn(async move {
                model::record_launch(&crate::reader_history::state_directory()?, &current)
            })
            .await;
        match since {
            Ok(Some(previous)) => cx.update(|cx| {
                cx.global_mut::<Session>().pending_since = Some(previous);
                cx.refresh_windows();
            }),
            Ok(None) => {}
            Err(error) => eprintln!("What's new state unavailable: {error}"),
        }
    })
    .detach();
}

/// The existing Reader notification hook calls this. No focus, sheet, network
/// or installer action occurs until the user explicitly chooses What's new.
pub(super) fn notice(window: &mut Window, cx: &mut App) {
    if !window.is_window_active() {
        return;
    }
    let Some(session) = cx.try_global::<Session>() else {
        return;
    };
    let Some(current) = session.current.clone() else {
        return;
    };
    let Some(since) = cx.global_mut::<Session>().pending_since.take() else {
        return;
    };
    announce(current, since, window, cx);
}
fn announce(current: Build, since: u64, window: &mut Window, cx: &mut App) {
    window.defer(cx, move |window, cx| {
        crate::reader_toast::push(
            Notification::new()
                .message(format!("Updated to 0.1.{} ·", current.build))
                .action(move |_, _, _| {
                    let current = current.clone();
                    Button::new("updated-whats-new")
                        .debug_selector(|| "updated-whats-new".into())
                        .small()
                        .ghost()
                        .label("What's new")
                        .on_click(move |_, window, cx| {
                            show(current.clone(), Some(since), window, cx)
                        })
                }),
            Some(Duration::from_secs(4)),
            window,
            cx,
        );
    });
}

pub(crate) fn show_current(window: &mut Window, cx: &mut App) {
    // Development builds still have an honest build/source/link fallback.
    let current = Build::current().unwrap_or_else(|| Build {
        build: 0,
        source: env!("OKILUM_SOURCE_COMMIT").to_owned(),
    });
    show(current, None, window, cx);
}
fn show(current: Build, since: Option<u64>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| {
        let mut view = NotesView {
            current,
            since,
            notes: None,
            _task: None,
        };
        view.reload(cx);
        view
    });
    open(view, window, cx);
}
fn open(view: Entity<NotesView>, window: &mut Window, cx: &mut App) {
    window.open_sheet(cx, move |sheet, _, _| {
        sheet
            .title("What's new")
            .size(px(440.))
            .resizable(false)
            .child(view.clone())
    });
}
struct NotesView {
    current: Build,
    since: Option<u64>,
    notes: Option<Range>,
    _task: Option<Task<()>>,
}
impl NotesView {
    fn reload(&mut self, cx: &mut Context<Self>) {
        let build = self.current.clone();
        let since = self.since;
        self.notes = None;
        self._task = Some(cx.spawn(async move |this: WeakEntity<NotesView>, cx| {
            let notes = cx
                .background_executor()
                .spawn(async move { model::load(&build, since) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.notes = Some(notes);
                cx.notify();
            });
        }));
        cx.notify();
    }
}

impl Render for NotesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = brand::palette(cx);
        let mut body = v_flex()
            .id("whats-new-content")
            .debug_selector(|| "whats-new-content".into())
            .size_full()
            .overflow_y_scroll()
            .gap_3()
            .text_color(palette.text);
        if let Some(notes) = &self.notes {
            for build in &notes.builds {
                body = body.child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!("0.1.{}", build.build)),
                );
                if build.items.is_empty() {
                    body = body.child(div().text_sm().child("Maintenance and stability fixes"));
                }
                for item in &build.items {
                    let url = item.link();
                    let kind = match item.kind {
                        model::Kind::Feature => "New",
                        model::Kind::Fix => "Fixed",
                    };
                    body = body.child(
                        div()
                            .id(("whats-new-pr", item.pr))
                            .cursor_pointer()
                            .text_sm()
                            .hover(|row| row.underline())
                            .child(format!("{kind} · {} ↗", item.text))
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    );
                }
            }
            if let Some(reason) = notes.unavailable {
                let partial = !notes.builds.is_empty();
                let message = match (reason, partial) {
                    (model::Unavailable::NotPublished, false) => {
                        "Release notes for this build haven't been published yet."
                    }
                    (model::Unavailable::NotPublished, true) => {
                        "Some release notes for earlier builds haven't been published yet."
                    }
                    (model::Unavailable::FetchFailed, false) => "Release notes couldn't be loaded.",
                    (model::Unavailable::FetchFailed, true) => {
                        "Some release notes couldn't be loaded."
                    }
                };
                body = body.child(
                    div()
                        .debug_selector(move || match reason {
                            model::Unavailable::NotPublished => "whats-new-unpublished".into(),
                            model::Unavailable::FetchFailed => "whats-new-offline".into(),
                        })
                        .text_sm()
                        .text_color(palette.text_muted)
                        .child(message),
                );
                let url = self.current.link();
                body = body
                    .child(div().text_sm().child(format!(
                        "Build {} · Source {}",
                        if self.current.build == 0 {
                            env!("OKILUM_BUILD_VERSION").to_owned()
                        } else {
                            self.current.build.to_string()
                        },
                        self.current.source.chars().take(12).collect::<String>()
                    )))
                    .child(
                        h_flex()
                            .w_full()
                            .gap_4()
                            .flex_wrap()
                            .child(
                                div()
                                    .id("whats-new-source")
                                    .debug_selector(|| "whats-new-source".into())
                                    .cursor_pointer()
                                    .text_sm()
                                    .hover(|row| row.underline())
                                    .child("Changes merged in this build ↗")
                                    .on_click(move |_, _, cx| cx.open_url(&url)),
                            )
                            .when(reason == model::Unavailable::FetchFailed, |row| {
                                row.child(
                                    Button::new("whats-new-retry")
                                        .debug_selector(|| "whats-new-retry".into())
                                        .small()
                                        .ghost()
                                        .label("Retry")
                                        .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                                )
                            }),
                    );
            }
        } else {
            body = body.child(
                div()
                    .text_sm()
                    .text_color(palette.text_muted)
                    .child("Loading changes…"),
            );
        }
        body.child(
            h_flex().w_full().child(
                div()
                    .id("whats-new-releases")
                    .debug_selector(|| "whats-new-releases".into())
                    .cursor_pointer()
                    .text_sm()
                    .hover(|row| row.underline())
                    .child("Release notes ↗")
                    .on_click(|_, _, cx| cx.open_url(model::RELEASES)),
            ),
        )
    }
}

#[cfg(test)]
#[path = "whats_new/ui_tests.rs"]
mod ui_tests;
