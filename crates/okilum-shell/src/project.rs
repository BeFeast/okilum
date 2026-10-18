//! The area's remote manager in the existing Reader window (#1418).
//! Network work stays off the UI thread; questionnaires remain in T3.
use super::*;
use gpui_component::input::{Textarea, TextareaState};
use okilum_t3::{
    chat::{Conversation, MessageJournal, MessageRecord},
    manifest::AreaBinding,
    Environment, GatewayStatus, ManagerGateway, McpGateway, ServerIdentity, StoredToken,
};
use std::time::Duration;

mod connections;

pub(crate) struct ProjectView {
    binding: Option<AreaBinding>,
    state: Option<PathBuf>,
    environment: Option<Environment>,
    gateway: Option<Arc<dyn ManagerGateway>>,
    identity: Option<ServerIdentity>,
    chat: Option<Conversation>,
    records: Vec<MessageRecord>,
    origin: Entity<InputState>,
    compose: Entity<TextareaState>,
    reason: Option<String>,
    error: Option<String>,
    busy: bool,
    active: bool,
    polling: bool,
    generation: u64,
    _input: Subscription,
}

impl ProjectView {
    pub(crate) fn new(
        vault: PathBuf,
        note: PathBuf,
        state: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let view = Self::empty(state.clone(), window, cx);
        let state_copy = state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let area = connections::area_for(&vault, &note)?;
                    let binding = AreaBinding::read(&area)?;
                    let env = match (&binding, &state_copy) {
                        (Some(binding), Some(state)) => connections::load(state, binding)?,
                        _ => None,
                    };
                    Ok::<_, anyhow::Error>((binding, env))
                })
                .await;
            let env = this
                .update_in(cx, |this, window, cx| match result {
                    Ok((binding, env)) => {
                        this.reason = match &binding {
                            None => Some("This area is not connected to T3.".into()),
                            Some(binding) if binding.manager.thread_id.is_none() => {
                                Some("This area has no linked manager yet.".into())
                            }
                            _ => None,
                        };
                        this.binding = binding;
                        if let Some(env) = &env {
                            this.origin
                                .update(cx, |input, cx| input.set_value(env.origin(), window, cx));
                        }
                        cx.notify();
                        env
                    }
                    Err(error) => {
                        this.reason = None;
                        this.error = Some(format!("{error:#}"));
                        cx.notify();
                        None
                    }
                })
                .ok()
                .flatten();
            let Some(env) = env else {
                return;
            };
            let url = env.credential_url();
            let task = match this.update_in(cx, |_, _, cx| cx.read_credentials(&url)) {
                Ok(task) => task,
                Err(_) => return,
            };
            let token = match task.await {
                Ok(Some((_, bytes))) => StoredToken::from_secret(&bytes).map(Some),
                Ok(None) => Ok(None),
                Err(error) => Err(error),
            };
            let result = match token {
                Ok(token) => {
                    cx.background_executor()
                        .spawn(async move {
                            McpGateway::new(env.clone(), token)
                                .map(|gateway| (env, Arc::new(gateway) as Arc<dyn ManagerGateway>))
                        })
                        .await
                }
                Err(error) => Err(error),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish_connection(result, window, cx)
            });
        })
        .detach();
        view
    }

    fn empty(state: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let origin =
            cx.new(|cx| InputState::new(window, cx).placeholder("T3 server address (https://…)"));
        let compose = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(3)
                .placeholder("Message your manager…")
        });
        let input = cx.subscribe(&compose, |_, _, _: &InputEvent, cx| cx.notify());
        Self {
            binding: None,
            state,
            environment: None,
            gateway: None,
            identity: None,
            chat: None,
            records: Vec::new(),
            origin,
            compose,
            reason: Some("Loading project…".into()),
            error: None,
            busy: false,
            active: true,
            polling: false,
            generation: 0,
            _input: input,
        }
    }

    pub(crate) fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.active = active;
        if active {
            self.start_polling(window, cx);
        }
    }

    fn finish_connection(
        &mut self,
        result: anyhow::Result<(Environment, Arc<dyn ManagerGateway>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy = false;
        match result {
            Ok((env, gateway)) => {
                self.environment = Some(env);
                self.gateway = Some(gateway);
                self.chat = self
                    .binding
                    .as_ref()
                    .and_then(|b| b.manager.thread_id.as_ref())
                    .map(Conversation::new);
                self.identity = None;
                self.error = None;
                self.generation += 1;
                self.start_polling(window, cx);
            }
            Err(error) => self.error = Some(format!("{error:#}")),
        }
        cx.notify();
    }

    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(binding) = &self.binding else {
            return;
        };
        let Some(state) = self.state.clone() else {
            self.error = Some("Application state is unavailable.".into());
            cx.notify();
            return;
        };
        let env = match Environment::new(&binding.environment, &self.origin.read(cx).value()) {
            Ok(env) => env,
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        self.busy = true;
        self.error = None;
        let (send_url, receive_url) = async_channel::bounded::<String>(1);
        let worker_env = env.clone();
        cx.spawn_in(window, async move |this, cx| {
            let job = cx.background_executor().spawn(async move {
                okilum_t3::oauth::connect(
                    &worker_env,
                    "Okilum",
                    move |url| {
                        send_url
                            .send_blocking(url.to_string())
                            .map_err(|_| anyhow::anyhow!("The connection window closed"))
                    },
                    Duration::from_secs(180),
                )
            });
            if let Ok(url) = receive_url.recv().await {
                let _ = this.update_in(cx, |_, _, cx| cx.open_url(&url));
            }
            let token = match job.await {
                Ok(token) => token,
                Err(error) => {
                    let _ = this.update_in(cx, |this, _, cx| {
                        this.busy = false;
                        this.error = Some(format!("{error:#}"));
                        cx.notify();
                    });
                    return;
                }
            };
            let task = match this.update_in(cx, |_, _, cx| {
                cx.write_credentials(&env.credential_url(), &env.alias, &token.to_secret())
            }) {
                Ok(task) => task,
                Err(_) => return,
            };
            let saved = task.await;
            let result = match saved {
                Ok(()) => {
                    cx.background_executor()
                        .spawn(async move {
                            connections::save(&state, &env)?;
                            McpGateway::new(env.clone(), Some(token))
                                .map(|gateway| (env, Arc::new(gateway) as Arc<dyn ManagerGateway>))
                        })
                        .await
                }
                Err(error) => Err(error),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish_connection(result, window, cx)
            });
        })
        .detach();
        cx.notify();
    }

    fn journal(&self) -> Option<MessageJournal> {
        Some(MessageJournal::new(
            self.state.as_ref()?,
            self.environment.as_ref()?,
            &self.chat.as_ref()?.thread_id,
        ))
    }

    fn start_polling(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.polling || !self.active || self.gateway.is_none() || self.chat.is_none() {
            return;
        }
        self.polling = true;
        cx.spawn_in(window, async move |this, cx| loop {
            let job = this
                .update_in(cx, |this, window, cx| {
                    if !this.active {
                        this.polling = false;
                        return None;
                    }
                    let gateway = this.gateway.clone()?;
                    if matches!(
                        gateway.status(),
                        GatewayStatus::NotConnected
                            | GatewayStatus::Expired
                            | GatewayStatus::Revoked
                    ) {
                        this.polling = false;
                        cx.notify();
                        return None;
                    }
                    Some((
                        gateway,
                        this.chat.clone()?,
                        this.journal(),
                        this.generation,
                        window.is_window_active(),
                    ))
                })
                .ok()
                .flatten();
            let Some((gateway, mut chat, journal, generation, focused)) = job else {
                break;
            };
            let result = cx
                .background_executor()
                .spawn(async move {
                    let identity = gateway.server_identity()?;
                    chat.poll(gateway.as_ref())?;
                    let records = journal
                        .map(|journal| journal.records())
                        .transpose()?
                        .unwrap_or_default();
                    Ok::<_, anyhow::Error>((chat, identity, records))
                })
                .await;
            let next = this
                .update_in(cx, |this, _, cx| {
                    if generation != this.generation {
                        return 0;
                    }
                    let mut delay = if focused { 2_000 } else { 15_000 };
                    match result {
                        Ok((chat, identity, records)) => {
                            if chat.has_more {
                                delay = 100;
                            }
                            this.chat = Some(chat);
                            this.identity = Some(identity);
                            this.records = records;
                            this.error = None;
                        }
                        Err(error) => {
                            this.error = Some(format!("{error:#}"));
                            delay = 15_000;
                        }
                    }
                    cx.notify();
                    delay
                })
                .unwrap_or(0);
            if next == 0 {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.polling = false;
                    this.start_polling(window, cx);
                });
                break;
            }
            gpui::Timer::after(Duration::from_millis(next)).await;
        })
        .detach();
    }

    fn send(&mut self, retry: Option<MessageRecord>, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(gateway) = self.gateway.clone() else {
            return;
        };
        let Some(journal) = self.journal() else {
            return;
        };
        let draft = self.compose.read(cx).value().to_string();
        if retry.is_none() && draft.trim().is_empty() {
            return;
        }
        let is_new = retry.is_none();
        self.busy = true;
        self.error = None;
        let generation = self.generation;
        cx.spawn_in(window, async move |this, cx| {
            let sent_text = draft.clone();
            let (result, records) = cx
                .background_executor()
                .spawn(async move {
                    let result = (|| {
                        let record = match retry {
                            Some(record) => record,
                            None => journal.prepare(&draft)?,
                        };
                        journal.send(gateway.as_ref(), &record)
                    })();
                    (result, journal.records())
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                if generation != this.generation {
                    cx.notify();
                    return;
                }
                let retained = records.as_ref().is_ok_and(|records| {
                    records
                        .iter()
                        .any(|record| record.message.text == sent_text)
                });
                if is_new && retained && this.compose.read(cx).value().to_string() == sent_text {
                    this.compose
                        .update(cx, |input, cx| input.set_value("", window, cx));
                }
                match records {
                    Ok(records) => this.records = records,
                    Err(error) => this.error = Some(format!("{error:#}")),
                }
                if let Err(error) = result {
                    this.error = Some(format!("{error:#}"));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

fn status_note(status: &GatewayStatus) -> Option<&'static str> {
    match status {
        GatewayStatus::Connected { .. } => None,
        GatewayStatus::NotConnected => Some("Connect to T3 to talk to this manager."),
        GatewayStatus::ExpiresSoon { .. } => {
            Some("Your T3 connection expires soon. Reconnect to renew it.")
        }
        GatewayStatus::Expired => Some("Your T3 connection has expired. Reconnect to continue."),
        GatewayStatus::Revoked => Some("Your T3 connection was revoked. Reconnect to continue."),
        GatewayStatus::Unreachable { .. } => {
            Some("T3 is unreachable. Your history and retained messages are kept.")
        }
    }
}

impl Render for ProjectView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self
            .gateway
            .as_ref()
            .map(|g| g.status())
            .unwrap_or(GatewayStatus::NotConnected);
        let enabled = matches!(
            status,
            GatewayStatus::Connected { .. } | GatewayStatus::ExpiresSoon { .. }
        );
        let note = status_note(&status);
        let pending = self
            .chat
            .as_ref()
            .map(|chat| chat.pending_questions)
            .unwrap_or(0);
        let open = self
            .identity
            .as_ref()
            .zip(self.chat.as_ref())
            .map(|(identity, chat)| identity.thread_url(&chat.thread_id));
        let mut history = v_flex()
            .id("project-messages")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap_4();
        if let Some(chat) = &self.chat {
            for item in chat.items.values() {
                if !matches!(item.kind.as_str(), "user_message" | "assistant_message") {
                    continue;
                }
                let Some(text) = &item.text else {
                    continue;
                };
                history = history.child(
                    v_flex()
                        .gap_1()
                        .child(div().text_color(brand::palette(cx).text_muted).child(
                            if item.kind == "user_message" {
                                "You"
                            } else {
                                "Manager"
                            },
                        ))
                        .child(
                            TextView::markdown(
                                ("project-message", item.position as usize),
                                text.clone(),
                            )
                            .selectable(true)
                            .style(reader_text_style(cx.theme())),
                        ),
                );
            }
        }
        for (index, record) in self.records.iter().enumerate() {
            let projected = record.receipt.as_ref().is_some_and(|receipt| {
                self.chat.as_ref().is_some_and(|chat| {
                    chat.items
                        .values()
                        .any(|item| item.message_id.as_ref() == Some(&receipt.message_id))
                })
            });
            if projected {
                continue;
            }
            let retry = record.clone();
            history = history.child(
                v_flex()
                    .gap_1()
                    .child(div().text_color(brand::palette(cx).text_muted).child(
                        if record.receipt.is_some() {
                            "You · queued"
                        } else {
                            "Message delivery unconfirmed"
                        },
                    ))
                    .when(record.receipt.is_some(), |row| {
                        row.child(
                            TextView::markdown(
                                ("project-retained", index),
                                record.message.text.clone(),
                            )
                            .selectable(true)
                            .style(reader_text_style(cx.theme())),
                        )
                    })
                    .when(record.receipt.is_none(), |row| {
                        row.child(
                            Button::new(("project-retry", index))
                                .debug_selector(move || format!("project-retry-{index}"))
                                .ghost()
                                .icon(IconName::RotateCw)
                                .tooltip(format!("Retry retained message: {}", record.message.text))
                                .disabled(self.busy || !enabled)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.send(Some(retry.clone()), window, cx)
                                })),
                        )
                    }),
            );
        }
        v_flex()
            .id("project-tab")
            .debug_selector(|| "project-tab".into())
            .size_full()
            .min_h_0()
            .p_4()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        gpui_component::button::ButtonGroup::new("project-segments")
                            .child(
                                Button::new("project-manager")
                                    .ghost()
                                    .label("Manager")
                                    .selected(true),
                            )
                            .child(
                                Button::new("project-executors")
                                    .ghost()
                                    .label("Executors")
                                    .disabled(true),
                            ),
                    )
                    .child(div().flex_1())
                    .when(pending > 0, |row| {
                        row.child(
                            div()
                                .text_color(brand::palette(cx).text_muted)
                                .child("Question in T3"),
                        )
                    })
                    .when_some(open, |row, url| {
                        row.child(
                            Button::new("project-open-t3")
                                .ghost()
                                .icon(IconName::ExternalLink)
                                .tooltip("Open in T3")
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        )
                    }),
            )
            .when_some(self.reason.clone(), |view, reason| {
                view.child(
                    div()
                        .text_color(brand::palette(cx).text_muted)
                        .child(reason),
                )
            })
            .when_some(note, |view, note| {
                view.child(
                    div()
                        .debug_selector(|| "project-connection-status".into())
                        .text_color(brand::palette(cx).text_muted)
                        .child(note),
                )
            })
            .when(
                self.binding
                    .as_ref()
                    .is_some_and(|b| b.manager.thread_id.is_some()),
                |view| {
                    view.child(
                        h_flex()
                            .gap_2()
                            .child(Input::new(&self.origin).flex_1())
                            .child(
                                Button::new("project-connect")
                                    .ghost()
                                    .icon(IconName::RotateCw)
                                    .tooltip("Connect or reconnect to T3")
                                    .disabled(self.busy)
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.connect(window, cx)),
                                    ),
                            ),
                    )
                },
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .debug_selector(|| "project-error".into())
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .child(history)
            .child(
                h_flex()
                    .items_end()
                    .gap_2()
                    .child(Textarea::new(&self.compose).flex_1())
                    .child(
                        Button::new("project-send")
                            .debug_selector(|| "project-send".into())
                            .ghost()
                            .icon(IconName::ArrowUp)
                            .tooltip("Send to manager")
                            .disabled(
                                self.busy
                                    || !enabled
                                    || self.compose.read(cx).value().trim().is_empty(),
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.send(None, window, cx)),
                            ),
                    ),
            )
    }
}

impl Reader {
    pub(super) fn close_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(project) = self.project.as_ref().filter(|_| self.project_open) {
            project.update(cx, |project, cx| project.set_active(false, window, cx));
        }
        self.project_open = false;
    }

    fn toggle_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.project_open {
            self.close_project(window, cx);
        } else {
            let vault = self.vault_root.clone();
            let note = vault.join(&self.current_rel);
            let state = self.session_directory.clone();
            self.project = Some(cx.new(|cx| ProjectView::new(vault, note, state, window, cx)));
            self.project_open = true;
        }
        cx.notify();
    }
}

#[cfg(any(test, feature = "project-ui-harness"))]
mod fixture;
#[cfg(test)]
mod tests;

#[cfg(feature = "project-ui-harness")]
pub(crate) fn open_harness(cx: &mut App) -> bool {
    fixture::open(cx)
}
