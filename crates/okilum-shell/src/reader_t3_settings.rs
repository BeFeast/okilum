//! App-wide browser connection. Authentication remains in T3's native webview.
use super::*;

#[derive(Default)]
pub(super) struct Connection {
    pub origin: Option<String>,
}
impl Global for Connection {}

pub(super) struct T3Settings {
    origin: Entity<InputState>,
    state: Option<PathBuf>,
    error: Option<String>,
    status: Option<String>,
    busy: bool,
    edited: bool,
    _input: Subscription,
}
impl T3Settings {
    pub(super) fn new(
        reader: Option<WeakEntity<Reader>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let reader = reader.and_then(|r| r.upgrade());
        let project = reader
            .as_ref()
            .and_then(|r| r.read(cx).project_navigation.active.clone());
        let state = reader
            .as_ref()
            .and_then(|r| r.read(cx).session_directory.clone())
            .or_else(|| reader_ui_state::state_directory(cx).ok());
        let alias = project
            .as_ref()
            .and_then(|p| p.environment.clone())
            .unwrap_or_else(|| "app".into());
        let current = cx.try_global::<Connection>().and_then(|c| c.origin.clone());
        let origin = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("t3.oklabs.uk or 10.10.0.23:23110")
                .default_value(current.clone().unwrap_or_default())
        });
        let input = cx.subscribe_in(&origin, window, |this, _, event, window, cx| match event {
            InputEvent::PressEnter { .. } => this.connect(window, cx),
            InputEvent::Change => {
                this.edited = true;
                this.error = None;
                cx.notify();
            }
            _ => {}
        });
        let saved = state.clone();
        let saved_alias = alias.clone();
        cx.spawn_in(window, async move |this, cx| {
            let prefill = cx
                .background_executor()
                .spawn(async move {
                    match project {
                        Some(p) => project::prefill_browser_origin(
                            &p.folder,
                            &saved_alias,
                            saved.as_deref(),
                        ),
                        None => project::prefill_app_browser_origin(saved.as_deref()),
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.edited || this.busy {
                    return;
                }
                match prefill {
                    Ok(Some(value)) => this
                        .origin
                        .update(cx, |input, cx| input.set_value(value, window, cx)),
                    Ok(None) => {}
                    Err(e) => this.error = Some(format!("Could not load T3 settings: {e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        Self {
            origin,
            state,
            error: None,
            status: current.map(|o| format!("Connected to {o}")),
            busy: false,
            edited: false,
            _input: input,
        }
    }
    #[cfg(test)]
    pub(super) fn origin_value(&self, cx: &App) -> String {
        self.origin.read(cx).value().to_string()
    }

    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let env = match okilum_t3::Environment::from_input("app", &self.origin.read(cx).value()) {
            Ok(env) => env,
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                cx.notify();
                return;
            }
        };
        self.origin
            .update(cx, |input, cx| input.set_value(env.origin(), window, cx));
        self.status = Some(format!("{} · {}", env.origin(), env.transport_rule()));
        self.busy = true;
        self.error = None;
        let state = self.state.clone();
        let job = cx.background_executor().spawn(async move {
            okilum_t3::projects::browser_identity(&env)?;
            let state = state.ok_or_else(|| {
                anyhow::anyhow!("Application state is unavailable; connection was not saved")
            })?;
            project::save_browser_environment(&state, &env)?;
            Ok::<_, anyhow::Error>(env.origin())
        });
        publish_when_ready(job, cx.weak_entity(), cx);
    }
}
// The app owns publication even if the initiating Settings form/window closes.
fn publish_when_ready(
    job: Task<anyhow::Result<String>>,
    form: WeakEntity<T3Settings>,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        let result = job.await;
        cx.update(|cx| {
            if let Ok(origin) = &result {
                cx.set_global(Connection { origin: Some(origin.clone()) });
            }
            let _ = form.update(cx, |form, cx| {
                form.busy = false;
                match result {
                    Ok(origin) => form.status = Some(format!("Connected to {origin}. Pair or sign in inside the Manager view.")),
                    Err(e) => form.error = Some(format!("Could not connect to T3: {e:#}. Enter a T3 address such as https://t3.oklabs.uk or http://10.10.0.23:23110.")),
                }
                cx.notify();
            });
        });
    }).detach();
}

impl Render for T3Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child("Projects / T3")
            .child("One connection for this app. A project manifest may override the server.")
            .child(
                h_flex()
                    .gap_2()
                    .child(Input::new(&self.origin).flex_1())
                    .child(
                        Button::new("t3-settings-connect")
                            .ghost()
                            .small()
                            .h(px(28.))
                            .rounded(px(brand::CONTROL_RADIUS))
                            .label("Connect")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| this.connect(window, cx))),
                    ),
            )
            .children(self.status.clone().map(|s| div().text_sm().child(s)))
            .children(self.busy.then_some("Connecting to T3…"))
            .children(
                self.error
                    .clone()
                    .map(|e| div().text_color(cx.theme().danger).child(e)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn completed_connection_is_published_after_form_is_dropped(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let form = cx.new(|cx| T3Settings::new(None, window, cx));
            Root::new(form, window, cx)
        });
        let (send, receive) = async_channel::bounded(1);
        let notifications = std::rc::Rc::new(std::cell::Cell::new(0));
        let changes = notifications.clone();
        let observer = visual.update(|_, cx| {
            cx.observe_global::<Connection>(move |cx| {
                if cx.global::<Connection>().origin.as_deref() == Some("https://t3.oklabs.uk") {
                    changes.set(changes.get() + 1);
                }
            })
        });
        let weak = visual.update(|window, cx| {
            let form = cx.new(|cx| T3Settings::new(None, window, cx));
            let weak = form.downgrade();
            let job = cx
                .background_executor()
                .spawn(async move { receive.recv().await.map_err(anyhow::Error::from) });
            publish_when_ready(job, weak.clone(), cx);
            weak
        });
        visual.run_until_parked();
        assert!(
            visual.update(|_, _| weak.upgrade().is_none()),
            "initiating form is gone before success"
        );
        send.try_send("https://t3.oklabs.uk".to_string()).unwrap();
        visual.run_until_parked();
        visual.update(|_, cx| {
            assert_eq!(
                cx.global::<Connection>().origin.as_deref(),
                Some("https://t3.oklabs.uk")
            )
        });
        assert_eq!(
            notifications.get(),
            1,
            "existing app observers receive the connection refresh"
        );
        drop(observer);
    }

    #[gpui::test]
    fn enter_validates_settings_input_and_editing_recovers(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (root, visual) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| T3Settings::new(None, window, cx));
            Root::new(settings, window, cx)
        });
        visual.run_until_parked();
        let settings = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<T3Settings>().unwrap()
        });
        settings.update_in(visual, |settings, window, cx| {
            settings.origin.update(cx, |input, cx| {
                input.set_value("not a host", window, cx);
                input.focus_handle(cx).focus(window, cx);
            });
        });
        visual.run_until_parked();
        assert!(settings.read_with(visual, |settings, _| settings.error.is_none()));
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        settings.read_with(visual, |settings, _| {
            assert!(settings
                .error
                .as_deref()
                .is_some_and(|error| error.contains("T3")));
            assert!(
                !settings.busy,
                "invalid input must not start a network request"
            );
        });
        settings.update_in(visual, |settings, window, cx| {
            settings
                .origin
                .update(cx, |input, cx| input.set_value("t3.oklabs.uk", window, cx))
        });
        visual.run_until_parked();
        assert!(settings.read_with(visual, |settings, _| settings.error.is_none()));
    }
}
