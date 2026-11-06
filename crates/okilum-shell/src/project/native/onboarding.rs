//! Variant B: one sentence and one action, with technical detail disclosed.
use super::*;
use reader_t3_settings::manager::{self, Stage};

impl ProjectView {
    pub(in crate::project) fn manager_stage(&self, cx: &App) -> Stage {
        let shared = self
            .environment
            .as_ref()
            .map(|env| manager::entry(env, cx))
            .unwrap_or_default();
        if matches!(
            shared.stage,
            Stage::Waiting | Stage::Connecting | Stage::Error
        ) {
            return shared.stage;
        }
        if let Some(error) = self.native.setup_cause.as_ref().or(self
            .native
            .failure
            .as_ref()
            .map(|f| &f.cause))
        {
            return match error {
                StreamError::Expired | StreamError::PairingRequired => Stage::Expired,
                error if recovery_state::is_transient(error) => Stage::Offline,
                _ => Stage::Error,
            };
        }
        if self.native.busy {
            return Stage::Connecting;
        }
        if self.native.active() {
            return if self
                .native
                .expires_at
                .is_some_and(|expiry| expiry <= chrono::Utc::now().timestamp().max(0) as u64)
            {
                Stage::Expired
            } else if self.native.stream_ready {
                Stage::Connected
            } else {
                Stage::Connecting
            };
        }
        if self.native.setup_error.is_some() || self.error.is_some() {
            return Stage::Error;
        }
        if matches!(
            self.gateway.as_ref().map(|g| g.status()),
            Some(GatewayStatus::Connected { .. } | GatewayStatus::ExpiresSoon { .. })
        ) {
            Stage::NotConnected
        } else {
            Stage::SignedOut
        }
    }

    pub(in crate::project) fn manager_connection_changed(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(env) = self.environment.clone() else {
            cx.notify();
            return;
        };
        let entry = manager::entry(&env, cx);
        if entry.retry_epoch != self.native.retry_epoch {
            self.native.retry_epoch = entry.retry_epoch;
            self.native_retry_now(window, cx);
        }
        if entry.stage == Stage::Connected && !reader_t3_settings::signed_out(&env.origin(), cx) {
            if let Some((client, expiry)) = entry.client.zip(entry.expiry) {
                if self
                    .native
                    .client
                    .as_ref()
                    .is_none_or(|old| !Arc::ptr_eq(old, &client))
                {
                    self.install_native(client, expiry, window, cx);
                } else if self.native.worker.is_none() && !self.native.stream_ready {
                    self.native_start(window, cx);
                }
            }
        }
        cx.notify();
    }

    fn manager_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.manager_stage(cx) {
            Stage::SignedOut => {
                if let Ok(env) = self.connection_environment(cx) {
                    reader_t3_settings::sign_in_to(env, self.state.clone(), cx);
                } else {
                    reader_settings::show_t3(cx);
                }
            }
            Stage::Offline => self.native_retry_now(window, cx),
            Stage::Waiting => {
                if let Some(env) = &self.environment {
                    cx.open_url(&manager::connections_url(env));
                }
            }
            Stage::Connecting | Stage::Connected => {}
            Stage::NotConnected | Stage::Expired | Stage::Error => self.pair_manager(window, cx),
        }
    }

    fn manager_details(&self, cx: &App) -> String {
        let shared = self
            .environment
            .as_ref()
            .map(|env| manager::entry(env, cx))
            .unwrap_or_default();
        let mut lines = vec![format!("Manager: {}", self.manager_stage(cx).label())];
        if let Some(env) = &self.environment {
            lines.push(format!("Server: {}", env.origin()));
        }
        if let Some(chat) = &self.chat {
            lines.push(format!("Manager thread: {}", chat.thread_id));
        }
        if let Some(binding) = &self.binding {
            lines.push(format!(
                "Bound thread: {}",
                binding.manager.thread_id.as_deref().unwrap_or("none")
            ));
        }
        if let Some(expiry) = self.native.expires_at.or(shared.expiry) {
            lines.push(super::super::expiry_note(expiry));
        }
        if let Some(error) = self.native.setup_error.as_ref() {
            lines.push(format!("Last error: {error}"));
        }
        if let Some(failure) = &self.native.failure {
            lines.push(format!("Last error: {}", failure.cause));
        }
        for error in [
            self.error.as_ref(),
            shared.error.as_ref(),
            self.model_error.as_ref(),
            self.native.history_error.as_ref(),
        ] {
            if let Some(error) = error {
                lines.push(format!("Last error: {error}"));
            }
        }
        if let Some(time) = shared.error_at {
            lines.push(format!(
                "Error time: {}",
                time.format("%Y-%m-%d %H:%M:%S %Z")
            ));
        }
        if let Some(snapshot) = &self.models {
            lines.push(format!(
                "Model: {} / {}; thread mode: {}; credential mode: {}",
                snapshot.configuration.model_selection.instance_id,
                snapshot.configuration.model_selection.model,
                snapshot.configuration.runtime_mode,
                snapshot.catalog.runtime_mode
            ));
        }
        lines.push(okilum_t3::models::SCHEDULED_TASK_LIMIT.into());
        lines.join("\n")
    }

    pub(super) fn render_manager_onboarding(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let stage = self.manager_stage(cx);
        let mut view = v_flex().w_full().min_w_0().gap_2();
        if stage != Stage::Connected {
            view = view.child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_3()
                    .p_3()
                    .when(
                        !self.native.active()
                            && matches!(
                                stage,
                                Stage::SignedOut
                                    | Stage::NotConnected
                                    | Stage::Waiting
                                    | Stage::Connecting
                            ),
                        |view| {
                            view.min_h(px(200.))
                                .items_center()
                                .justify_center()
                                .text_center()
                        },
                    )
                    .debug_selector(|| "manager-onboarding".into())
                    .child(div().w_full().whitespace_normal().child(stage.sentence()))
                    .child(
                        Button::new("manager-primary")
                            .debug_selector(|| "manager-primary".into())
                            .primary()
                            .label(stage.action())
                            .disabled(stage == Stage::Connecting)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.manager_action(window, cx)),
                            ),
                    ),
            );
        }
        view = view.child(
            Button::new("manager-details-toggle")
                .debug_selector(|| "manager-details-toggle".into())
                .ghost()
                .small()
                .label(if self.connection_details {
                    "▾ Details"
                } else {
                    "▸ Details"
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.connection_details = !this.connection_details;
                    cx.notify();
                })),
        );
        if self.connection_details {
            let details = self.manager_details(cx);
            let copy = details.clone();
            let mut panel = v_flex()
                .id("manager-details")
                .debug_selector(|| "manager-details".into())
                .w_full()
                .min_w_0()
                .max_h(px(280.))
                .overflow_y_scroll()
                .gap_2()
                .child(div().w_full().whitespace_normal().text_sm().child(details))
                .child(
                    Button::new("manager-copy-details")
                        .debug_selector(|| "manager-copy-details".into())
                        .ghost()
                        .small()
                        .label("Copy details")
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                        }),
                )
                .child(
                    div()
                        .debug_selector(|| "project-live-token-row".into())
                        .w_full()
                        .min_w_0()
                        .child(Input::new(&self.native.input).w_full()),
                )
                .child(
                    Button::new("manager-paste-link")
                        .ghost()
                        .small()
                        .label("Use pasted link")
                        .disabled(stage == Stage::Connecting)
                        .on_click(cx.listener(|this, _, window, cx| {
                            if let Some(env) = this
                                .environment
                                .clone()
                                .or_else(|| this.connection_environment(cx).ok())
                            {
                                let text = this.native.input.read(cx).value().to_string();
                                this.native
                                    .input
                                    .update(cx, |input, cx| input.set_value("", window, cx));
                                manager::paste(env, &text, cx);
                            }
                        })),
                );
            if let Some(env) = self.environment.clone() {
                let open = manager::connections_url(&env);
                panel = panel.child(
                    Button::new("manager-details-open-t3")
                        .ghost()
                        .small()
                        .label("Open T3")
                        .on_click(move |_, _, cx| cx.open_url(&open)),
                );
                if stage == Stage::Waiting {
                    panel = panel.child(
                        Button::new("manager-cancel-link")
                            .ghost()
                            .small()
                            .label("Cancel")
                            .on_click(move |_, _, cx| manager::cancel(&env, cx)),
                    );
                }
            }
            view = view.child(panel);
        }
        view.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[gpui::test]
    fn manager_states_distinguish_sign_in_pairing_transport_and_expiry(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        cx.update(reader_features::enable_test_modules);
        let files = tempfile::tempdir().unwrap();
        let env = Environment::new("fixture", "https://t3.example").unwrap();
        let (view, visual) = cx.add_window_view(|window, cx| {
            super::super::super::fixture::view(
                files.path().to_owned(),
                Arc::new(super::super::super::fixture::Fake::default()),
                window,
                cx,
            )
        });
        view.update_in(visual, |view, window, cx| {
            view.environment = Some(env.clone());
            view.gateway = None;
            assert_eq!(view.manager_stage(cx), Stage::SignedOut);
            view.gateway = Some(Arc::new(super::super::super::fixture::Fake::default()));
            assert_eq!(view.manager_stage(cx), Stage::NotConnected);
            manager::begin(&env, cx);
            assert_eq!(view.manager_stage(cx), Stage::Waiting);
            manager::cancel(&env, cx);
            assert_eq!(view.manager_stage(cx), Stage::NotConnected);
            view.native.busy = true;
            assert_eq!(view.manager_stage(cx), Stage::Connecting);
            view.native.busy = false;
            for (error, stage) in [(StreamError::Unreachable, Stage::Offline), (StreamError::Expired, Stage::Expired),
                (StreamError::PairingRequired, Stage::Expired), (StreamError::Rejected, Stage::Error)] {
                view.native_failed(error, ConnectionContext::Connect, window, cx);
                assert_eq!(view.manager_stage(cx), stage);
            }
            view.native.failure = None;
            let expiry = chrono::Utc::now().timestamp().max(0) as u64 + 3600;
            let token = NativeToken::from_secret(&env, &serde_json::to_vec(&serde_json::json!({
                "origin": env.origin(), "access_token":"synthetic-native-secret", "expires_at": expiry,
                "scope":"orchestration:read orchestration:operate"
            })).unwrap()).unwrap();
            view.native.client = Some(Arc::new(NativeClient::new(env, token, Duration::from_secs(1)).unwrap()));
            view.native.expires_at = Some(expiry);
            view.native.stream_ready = true;
            assert_eq!(view.manager_stage(cx), Stage::Connected);
            view.native.expires_at = Some(1);
            assert_eq!(view.manager_stage(cx), Stage::Expired);
        });
    }
}
