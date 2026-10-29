//! App-wide authenticated T3 connection and resumable browser authorization.
use super::*;
mod auth;
#[cfg(target_os = "macos")]
mod browser;

#[derive(Default)]
pub(super) struct Connection {
    pub origin: Option<String>,
}
impl Global for Connection {}

pub(super) struct T3Settings {
    origin: Entity<InputState>,
    callback: Entity<InputState>,
    state: Option<PathBuf>,
    error: Option<String>,
    edited: bool,
    _input: Subscription,
    _auth: Subscription,
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
                .default_value(current.unwrap_or_default())
        });
        let callback = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Paste the browser callback URL here")
        });
        let input = cx.subscribe_in(&origin, window, |this, _, event, _, cx| {
            if let InputEvent::Change = event {
                this.edited = true;
                this.error = None;
                cx.notify();
            }
        });
        let observer = cx.observe_global::<auth::Sessions>(|_, cx| cx.notify());
        let saved = state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let prefill = cx
                .background_executor()
                .spawn(async move {
                    match project {
                        Some(p) => {
                            project::prefill_browser_origin(&p.folder, &alias, saved.as_deref())
                        }
                        None => project::prefill_app_browser_origin(saved.as_deref()),
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.edited {
                    return;
                }
                match prefill {
                    Ok(Some(value)) => {
                        this.origin
                            .update(cx, |input, cx| input.set_value(value.clone(), window, cx));
                        if let Ok(env) = okilum_t3::Environment::from_input("app", &value) {
                            auth::connect(env, this.state.clone(), false, cx);
                        }
                    }
                    Ok(None) => {}
                    Err(e) => this.error = Some(format!("Could not load T3 settings: {e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        Self {
            origin,
            callback,
            state,
            error: None,
            edited: false,
            _input: input,
            _auth: observer,
        }
    }
    #[cfg(test)]
    pub(super) fn origin_value(&self, cx: &App) -> String {
        self.origin.read(cx).value().to_string()
    }

    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match okilum_t3::Environment::from_input("app", &self.origin.read(cx).value()) {
            Ok(env) => {
                self.origin
                    .update(cx, |input, cx| input.set_value(env.origin(), window, cx));
                self.error = None;
                auth::connect(env, self.state.clone(), true, cx);
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
        cx.notify();
    }
}
impl Render for T3Settings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let env = okilum_t3::Environment::from_input("app", &self.origin.read(cx).value()).ok();
        let entry = env
            .as_ref()
            .map(|e| auth::entry(&e.origin(), cx))
            .unwrap_or_default();
        let pending = entry.pending.clone();
        #[cfg(target_os = "macos")]
        if let Some(pending) = &pending {
            if let Err(e) = browser::show(&pending.url, env.as_ref().unwrap(), _window, cx) {
                self.error = Some(format!(
                    "Sign-in view could not open: {e:#}. Use Open in browser below."
                ));
            }
        }
        let status = env.as_ref().map(|e| auth::status(&e.origin(), cx));
        let error = self.error.clone().or(entry.error);
        v_flex()
            .id("t3-connector-form")
            .gap_3()
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "enter"
                    && event.keystroke.modifiers == Modifiers::default()
                    && this.origin.read(cx).focus_handle(cx).is_focused(window)
                {
                    this.connect(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child("Projects / T3")
            .child("Sign in once for every project in this app. Sprint keeps its own connection.")
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
                            .label("Sign in")
                            .disabled(entry.busy)
                            .on_click(cx.listener(|this, _, window, cx| this.connect(window, cx))),
                    ),
            )
            .children(status.map(|s| {
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(s)
            }))
            .when_some(pending, |view, pending| {
                let paste = pending.input.clone();
                let extend = pending.input.clone();
                let cancel = pending.input;
                let url = pending.url;
                view.child(Input::new(&self.callback)).child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("t3-callback-paste")
                                .ghost()
                                .label("Use callback URL")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let url = this.callback.read(cx).value().to_string();
                                    let _ = paste.send(okilum_t3::oauth::Input::Callback(url));
                                    this.callback
                                        .update(cx, |i, cx| i.set_value("", window, cx));
                                })),
                        )
                        .child(
                            Button::new("t3-wait-longer")
                                .ghost()
                                .label("Wait longer")
                                .on_click(move |_, _, _| {
                                    let _ = extend.send(okilum_t3::oauth::Input::WaitLonger);
                                }),
                        )
                        .child(
                            Button::new("t3-open-sign-in")
                                .ghost()
                                .icon(IconName::ExternalLink)
                                .tooltip("Open sign-in in your browser")
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        )
                        .child(
                            Button::new("t3-cancel-sign-in")
                                .ghost()
                                .label("Cancel")
                                .on_click(move |_, _, _| {
                                    let _ = cancel.send(okilum_t3::oauth::Input::Cancel);
                                }),
                        ),
                )
            })
            .children(error.map(|e| div().text_color(cx.theme().danger).child(e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
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
        visual.update(|window, cx| window.draw(cx).clear(cx));
        settings.update_in(visual, |settings, window, cx| {
            assert_eq!(
                window.focused(cx),
                Some(settings.origin.read(cx).focus_handle(cx)),
                "positive control: Settings origin owns the key-dispatch path"
            );
            assert_eq!(settings.origin.read(cx).value().as_str(), "not a host");
        });
        assert!(settings.read_with(visual, |settings, _| settings.error.is_none()));
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        settings.read_with(visual, |settings, cx| {
            assert!(
                settings
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("T3")),
                "Enter must submit validation, actual error: {:?}",
                settings.error
            );
            assert!(
                cx.try_global::<auth::Sessions>().is_none(),
                "invalid input must not start a network request"
            );
        });
        visual.simulate_keystrokes("secondary-a");
        visual.simulate_input("t3.oklabs.uk");
        visual.run_until_parked();
        settings.read_with(visual, |settings, cx| {
            assert_eq!(settings.origin.read(cx).value().as_str(), "t3.oklabs.uk");
            assert!(
                settings.error.is_none(),
                "typing clears the validation error"
            );
        });
    }
}
