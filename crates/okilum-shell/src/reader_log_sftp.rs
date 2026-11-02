//! Explicit read-only SFTP sessions using existing OpenSSH configuration.
use crate::*;
use okilum_core::log::{sftp::Target, stream::Snapshot};
mod worker;

#[derive(Default)]
struct Windows(std::collections::BTreeMap<String, WindowHandle<Root>>);
impl Global for Windows {}

pub(crate) fn show(vault: PathBuf, cx: &mut App) {
    let scope = okilum_core::remote_log::vault_scope(&vault);
    if let Some(handle) = cx
        .try_global::<Windows>()
        .and_then(|windows| windows.0.get(&scope))
        .copied()
    {
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            return;
        }
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(window_state::default_bounds(
            size(px(1100.), px(760.)),
            cx,
        ))),
        window_min_size: Some(size(px(720.), px(480.))),
        ..TitleBar::window_options()
    };
    let key = scope.clone();
    if let Ok(handle) = cx.open_window(options, move |window, cx| {
        window.set_window_title("SFTP logs");
        let panel = cx.new(|cx| Panel::new(scope, window, cx));
        cx.new(|cx| Root::new(panel, window, cx))
    }) {
        if cx.try_global::<Windows>().is_none() {
            cx.set_global(Windows::default());
        }
        cx.global_mut::<Windows>().0.insert(key, handle);
    }
}

struct Panel {
    scope: String,
    url: Entity<InputState>,
    requested_url: Option<String>,
    displayed_url: Option<String>,
    follow: bool,
    generation: u64,
    active: bool,
    stopping: bool,
    message: String,
    error: bool,
    pending: Option<Arc<Snapshot>>,
    view: Option<Entity<reader_log::LogView>>,
    worker: Option<worker::Worker>,
    events: Option<Task<()>>,
}
impl Panel {
    fn new(scope: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            scope,
            url: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("sftp://ssh-config-alias/absolute/path/app.log")
            }),
            requested_url: None,
            displayed_url: None,
            follow: true,
            generation: 0,
            active: false,
            stopping: false,
            message:
                "Choose a file, then Start. Uses an existing OpenSSH host alias and known host key."
                    .into(),
            error: false,
            pending: None,
            view: None,
            worker: None,
            events: None,
        }
    }
    fn start(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.prepare_start(cx) else {
            return;
        };
        let generation = self.generation;
        let (worker, events) = worker::Worker::start(
            target,
            self.follow,
            format!("sftp:{}:{generation}", self.scope),
        );
        self.worker = Some(worker);
        self.events = Some(cx.spawn(async move |this, cx| {
            while let Ok(frame) = events.recv().await {
                if this
                    .update(cx, |this, cx| this.receive(generation, frame, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }
    /// Validate and establish UI ownership before starting the transport. The
    /// previous immutable result remains readable until new data is published.
    fn prepare_start(&mut self, cx: &mut Context<Self>) -> Option<Target> {
        if self.active {
            return None;
        }
        let target = match Target::parse(self.url.read(cx).value().trim()) {
            Ok(target) => target,
            Err(error) => {
                self.message = error.to_string();
                self.error = true;
                cx.notify();
                return None;
            }
        };
        self.generation = self.generation.wrapping_add(1);
        self.requested_url = Some(self.url.read(cx).value().trim().to_owned());
        self.active = true;
        self.stopping = false;
        self.error = false;
        self.message = if self.view.is_some() || self.pending.is_some() {
            "Opening SFTP log… previous session rows retained until new data arrives."
        } else {
            "Opening SFTP log…"
        }
        .into();
        cx.notify();
        Some(target)
    }
    fn receive(&mut self, generation: u64, frame: worker::Frame, cx: &mut Context<Self>) {
        if self.generation != generation {
            return;
        }
        if let Some(data) = frame.data {
            self.pending = Some(data);
            self.displayed_url = self.requested_url.clone();
        }
        self.active = frame.active;
        self.error = frame.error;
        self.message = frame.message;
        if !self.active {
            self.worker = None;
            self.stopping = false;
        }
        cx.notify();
    }
    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(worker) = &self.worker {
            worker.cancel();
            self.stopping = true;
            self.message = "Stopping after the current SFTP request…".into();
            cx.notify();
        }
    }
}
impl Render for Panel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(data) = self.pending.take() {
            if let Some(view) = &self.view {
                view.update(cx, |view, cx| view.replace_stream(data, cx));
            } else {
                self.view = Some(cx.new(|cx| reader_log::LogView::stream(data, window, cx)));
            }
        }
        let controls = v_flex().p_3().gap_2().flex_none()
            .child(div().text_xs().child("Remote file · sftp://host-alias/absolute/path"))
            .child(h_flex().gap_2()
                .child(div().flex_1().min_w_0().child(Input::new(&self.url).small().disabled(self.active)))
                .child(Button::new("sftp-follow").small().ghost().label("Follow").selected(self.follow).disabled(self.active)
                    .tooltip("Continue reading appended lines after initial history")
                    .on_click(cx.listener(|this, _, _, cx| { this.follow = !this.follow; cx.notify(); })))
                .child(Button::new("sftp-start").debug_selector(|| "sftp-start".into()).small().primary().label(if self.view.is_some() { "New session" } else { "Start" }).disabled(self.active)
                    .on_click(cx.listener(|this, _, _, cx| this.start(cx))))
                .child(Button::new("sftp-stop").small().label("Stop").disabled(!self.active || self.stopping)
                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))))
            .child(div().text_sm().text_color(if self.error { brand::palette(cx).danger } else { cx.theme().muted_foreground }).child(self.message.clone()))
            .children(self.displayed_url.as_ref().map(|url| div().text_xs().child(format!("Displayed source: {url}"))))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Loaded history is session-only, up to 8 MiB / 20,000 lines. Rotation or rewrite stops this session; existing rows remain readable."));
        let mut body = v_flex()
            .size_full()
            .min_h_0()
            .child(TitleBar::new().child("SFTP logs"))
            .child(controls);
        body = if let Some(view) = &self.view {
            body.child(div().flex_1().min_h_0().child(view.clone()))
        } else {
            body.child(div().flex_1().p_4().child("No log rows loaded. Passwords and host-key changes are not requested here; use your existing OpenSSH configuration."))
        };
        body
    }
}

#[cfg(test)]
#[path = "reader_log_sftp/replacement_tests.rs"]
mod replacement_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::TestAppContext;
    #[gpui::test]
    fn invalid_url_and_stale_reply_do_not_replace_current_sftp_session(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let mut panel = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| Panel::new("fixture".into(), window, cx));
            panel = Some(view.clone());
            Root::new(view, window, cx)
        });
        visual.run_until_parked();
        panel.unwrap().update_in(visual, |panel, window, cx| {
            assert!(panel.worker.is_none(), "opening the panel does not connect");
            panel.url.update(cx, |input, cx| {
                input.set_value("sftp://user:password@host/a.log", window, cx)
            });
            panel.start(cx);
            assert!(panel.error && panel.worker.is_none());
            panel.generation = 2;
            panel.active = true;
            panel.error = false;
            panel.message = "current".into();
            panel.receive(
                1,
                worker::Frame {
                    data: None,
                    message: "stale".into(),
                    active: false,
                    error: true,
                },
                cx,
            );
            assert!(panel.active && !panel.error);
            assert_eq!(panel.message, "current");
            panel.receive(
                2,
                worker::Frame {
                    data: None,
                    message: "Stopped".into(),
                    active: false,
                    error: false,
                },
                cx,
            );
            assert!(
                !panel.active,
                "positive control: current completion accepted"
            );
        });
    }
}
