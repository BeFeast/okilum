//! App-wide authenticated T3 connection and resumable browser authorization.
use super::*;
mod auth;
#[cfg(native_manager)]
pub(crate) mod manager;

#[derive(Default)]
pub(super) struct Connection {
    pub origin: Option<String>,
}
impl Global for Connection {}

pub(super) fn suspend(cx: &mut App) {
    auth::suspend(cx);
}

pub(super) fn signed_out(origin: &str, cx: &App) -> bool {
    auth::entry(origin, cx).signed_out
}

pub(super) fn connection_error(origin: &str, cx: &App) -> Option<String> {
    auth::entry(origin, cx).error
}

pub(super) fn can_sign_out(env: &okilum_t3::Environment, cx: &App) -> bool {
    let entry = auth::entry(&env.origin(), cx);
    !entry.busy && entry.pending.is_none()
}

pub(super) fn sign_out(env: okilum_t3::Environment, cx: &mut App) {
    auth::sign_out(env, cx);
}

pub(super) fn gateway(
    env: &okilum_t3::Environment,
    cx: &App,
) -> Option<Arc<dyn okilum_t3::ManagerGateway>> {
    if !reader_features::enabled(reader_features::Feature::Agents, cx) {
        return None;
    }
    auth::entry(&env.origin(), cx).gateway.filter(|gateway| {
        matches!(
            gateway.status(),
            okilum_t3::GatewayStatus::Connected { .. }
                | okilum_t3::GatewayStatus::ExpiresSoon { .. }
        )
    })
}

/// Explicit project setup reuses the existing origin-bound sign-in UI.
pub(super) fn sign_in_to(
    environment: okilum_t3::Environment,
    state: Option<PathBuf>,
    cx: &mut App,
) {
    auth::connect(environment, state, true, cx);
    reader_settings::show_t3(cx);
}

#[cfg(test)]
pub(super) fn fixture_connection(
    env: &okilum_t3::Environment,
    gateway: Arc<dyn okilum_t3::ManagerGateway>,
    cx: &mut App,
) {
    auth::fixture_connection(env, gateway, cx);
}

pub(super) struct T3Settings {
    origin: Entity<InputState>,
    callback: Entity<InputState>,
    state: Option<PathBuf>,
    error: Option<String>,
    edited: bool,
    error_details_open: bool,
    _input: Subscription,
    _auth: Subscription,
    #[cfg(native_manager)]
    manager_input: Entity<InputState>,
    #[cfg(native_manager)]
    manager_details_open: bool,
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
        let current = auth::pending_origin(cx)
            .or_else(|| cx.try_global::<Connection>().and_then(|c| c.origin.clone()));
        let current_for_prefill = auth::pending_origin(cx);
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
                this.error_details_open = false;
                cx.notify();
            }
        });
        let observer = cx.observe_global::<auth::Sessions>(|_, cx| cx.notify());
        let saved = state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let prefill = cx
                .background_executor()
                .spawn(async move {
                    if current_for_prefill.is_some() {
                        return Ok(current_for_prefill);
                    }
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
                            #[cfg(native_manager)]
                            manager::restore(env.clone(), cx);
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
        #[cfg(native_manager)]
        let manager_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Paste a T3 pair link or pairing code")
                .masked(true)
        });
        #[cfg(native_manager)]
        {
            cx.observe_global::<manager::Connections>(|_, cx| cx.notify())
                .detach();
            cx.observe_window_activation(window, |_, window, cx| {
                if window.is_window_active() {
                    manager::activated(window, cx);
                }
            })
            .detach();
        }
        Self {
            #[cfg(native_manager)]
            manager_input,
            #[cfg(native_manager)]
            manager_details_open: false,
            origin,
            callback,
            state,
            error: None,
            edited: false,
            error_details_open: false,
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
                self.error_details_open = false;
                #[cfg(native_manager)]
                manager::restore(env.clone(), cx);
                auth::connect(env, self.state.clone(), true, cx);
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
        cx.notify();
    }
}
#[cfg(native_manager)]
impl T3Settings {
    fn render_manager(&self, cx: &mut Context<Self>) -> AnyElement {
        use manager::Stage;
        let env = okilum_t3::Environment::from_input("app", &self.origin.read(cx).value()).ok();
        let entry = env
            .as_ref()
            .map(|env| manager::entry(env, cx))
            .unwrap_or_default();
        let signed_in = env.as_ref().is_some_and(|env| gateway(env, cx).is_some());
        let stage = if entry.stage == Stage::NotConnected && !signed_in {
            Stage::SignedOut
        } else if entry.stage == Stage::Connected
            && entry
                .expiry
                .is_some_and(|expiry| expiry <= chrono::Local::now().timestamp().max(0) as u64)
        {
            Stage::Expired
        } else {
            entry.stage
        };
        let details = format!(
            "Manager: {}\nServer: {}\n{}\nLast error: {}",
            stage.label(),
            env.as_ref()
                .map(|env| env.origin())
                .unwrap_or_else(|| "Not configured".into()),
            entry
                .expiry
                .map(project::expiry_note)
                .unwrap_or_else(|| "No saved Manager credential".into()),
            entry.error.unwrap_or_else(|| "None".into())
        );
        let copy = details.clone();
        v_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .debug_selector(|| "t3-manager-connection".into())
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("Manager · {}", stage.label())),
            )
            .child(div().whitespace_normal().child(stage.sentence()))
            .child(
                Button::new("t3-manager-connect")
                    .debug_selector(|| "t3-manager-connect".into())
                    .primary()
                    .label(stage.action())
                    .disabled(stage == Stage::Connecting || env.is_none())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let Some(env) = okilum_t3::Environment::from_input(
                            "app",
                            &this.origin.read(cx).value(),
                        )
                        .ok() else {
                            return;
                        };
                        match stage {
                            Stage::SignedOut => this.connect(window, cx),
                            Stage::Connected | Stage::Waiting => {
                                cx.open_url(&manager::connections_url(&env))
                            }
                            Stage::Connecting => {}
                            Stage::Offline => manager::retry(&env, cx),
                            _ => manager::begin(&env, cx),
                        }
                    })),
            )
            .child(
                Button::new("t3-manager-details-toggle")
                    .ghost()
                    .small()
                    .label(if self.manager_details_open {
                        "▾ Details"
                    } else {
                        "▸ Details"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.manager_details_open = !this.manager_details_open;
                        cx.notify();
                    })),
            )
            .when(self.manager_details_open, |view| {
                view.child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .debug_selector(|| "t3-manager-details".into())
                        .child(div().whitespace_normal().text_sm().child(details))
                        .child(
                            Button::new("t3-manager-copy-details")
                                .ghost()
                                .small()
                                .label("Copy details")
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                                }),
                        )
                        .child(Input::new(&self.manager_input).w_full())
                        .child(
                            Button::new("t3-manager-paste-link")
                                .ghost()
                                .small()
                                .label("Use pasted link")
                                .disabled(stage == Stage::Connecting)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    if let Ok(env) = okilum_t3::Environment::from_input(
                                        "app",
                                        &this.origin.read(cx).value(),
                                    ) {
                                        let text = this.manager_input.read(cx).value().to_string();
                                        this.manager_input.update(cx, |input, cx| {
                                            input.set_value("", window, cx)
                                        });
                                        manager::paste(env, &text, cx);
                                    }
                                })),
                        )
                        .when(stage == Stage::Waiting, |view| {
                            view.child(
                                Button::new("t3-manager-cancel")
                                    .ghost()
                                    .small()
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Ok(env) = okilum_t3::Environment::from_input(
                                            "app",
                                            &this.origin.read(cx).value(),
                                        ) {
                                            manager::cancel(&env, cx);
                                        }
                                    })),
                            )
                        }),
                )
            })
            .into_any_element()
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
            .child("T3")
            .child("Sign in once for every project in this app.")
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
            .when(cfg!(native_manager), |view| {
                #[cfg(native_manager)]
                let view = view.child(self.render_manager(cx));
                view
            })
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
            .children(error.map(|e| {
                div()
                    .debug_selector(|| "t3-connection-error".into())
                    .text_color(cx.theme().danger)
                    .child(e)
            }))
            .when_some(entry.error_details, |view, details| {
                view.child(
                    Button::new("t3-connection-details")
                        .debug_selector(|| "t3-connection-details".into())
                        .ghost()
                        .small()
                        .label("Details")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.error_details_open = !this.error_details_open;
                            cx.notify();
                        })),
                )
                .when(self.error_details_open, |view| {
                    view.child(
                        div()
                            .debug_selector(|| "t3-connection-error-details".into())
                            .w_full()
                            .min_w_0()
                            .whitespace_normal()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(details),
                    )
                })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[gpui::test]
    fn enter_validates_settings_input_and_editing_recovers(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
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
