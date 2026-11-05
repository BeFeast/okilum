use super::*;
use presentation::Step;

impl Render for SyncSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = brand::palette(cx);
        let mut content = v_flex().gap_6().w_full().child(
            h_flex()
                .justify_between()
                .gap_4()
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xl()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Sync"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(p.text_muted)
                                .child("Keep your vaults together across computers."),
                        ),
                )
                .child(
                    Button::new("sync-add-computer")
                        .debug_selector(|| "sync-add-computer".into())
                        .primary()
                        .label("Add computer")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.presentation.wizard = Some(Step::Start);
                            cx.notify();
                        })),
                ),
        );
        content = content.child(self.render_status(cx));
        let empty = self.snapshot.as_ref().is_none_or(|s| s.peers.is_empty());
        if empty {
            content = content.child(v_flex().id("sync-empty").debug_selector(|| "sync-empty".into())
                .w_full().p_6().gap_4().items_center().border_1().border_color(p.border).rounded_lg()
                .child(IconName::Network.size_8().text_color(p.text_muted))
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("Your notes, on your other computers"))
                .child(div().text_sm().text_color(p.text_muted).child("Create a link on the computer with your notes, approve the other computer, then choose vaults."))
                .child(Button::new("sync-have-link").ghost().label("I already have a pairing link")
                    .on_click(cx.listener(|this, _, _, cx| { this.presentation.wizard=Some(Step::Paste); cx.notify(); }))));
        } else {
            content = content.child(
                h_flex()
                    .justify_between()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Computers"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(p.text_muted)
                            .child(format!("This computer: {}", self.name.read(cx).value())),
                    ),
            );
            if let Some(snapshot) = &self.snapshot {
                let mut computers = v_flex().w_full().gap_2();
                for (index, peer) in snapshot.peers.iter().enumerate() {
                    let id = peer.id;
                    let observed = self
                        .presentation
                        .overview
                        .computers
                        .get(&id)
                        .cloned()
                        .unwrap_or_default();
                    let connection = match observed.connected {
                        Some(true) => "Online",
                        Some(false) => "Offline",
                        None => "Connection not checked",
                    };
                    let seen = if observed.connected == Some(true) {
                        "Last seen just now".into()
                    } else {
                        observed
                            .last_seen
                            .map(|t| presentation::last_seen(&t))
                            .unwrap_or_else(|| "Last seen unavailable".into())
                    };
                    computers = computers.child(
                        h_flex()
                            .w_full()
                            .p_4()
                            .gap_4()
                            .border_1()
                            .border_color(p.border)
                            .rounded_lg()
                            .child(IconName::Network.size_6().text_color(p.text_muted))
                            .child(
                                v_flex().flex_1().gap_1().child(peer.name.clone()).child(
                                    div().text_sm().text_color(p.text_muted).child(format!(
                                        "OS not reported · {} vaults",
                                        observed.vaults
                                    )),
                                ),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(div().text_sm().child(connection))
                                    .child(div().text_xs().text_color(p.text_muted).child(seen)),
                            )
                            .child(
                                Button::new(("sync-catalog", index))
                                    .debug_selector(move || format!("sync-catalog-{index}"))
                                    .ghost()
                                    .label("Vaults →")
                                    .disabled(self.busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.selected_peer = Some(id);
                                        this.presentation.wizard = Some(Step::Vaults);
                                        if let Some(s) = this.snapshot.as_mut() {
                                            s.catalog.clear();
                                        }
                                        this.run(Operation::Catalog(id), cx);
                                    })),
                            ),
                    );
                }
                content = content.child(computers);
            }
        }
        content = content.child(div().font_weight(FontWeight::SEMIBOLD).child(if empty {
            "Local vaults"
        } else {
            "Vaults"
        }));
        if let Some(snapshot) = &self.snapshot {
            for (index, vault) in snapshot.attached.iter().enumerate() {
                let path = vault.path.clone();
                let observed = self
                    .presentation
                    .overview
                    .vaults
                    .get(&vault.path)
                    .cloned()
                    .unwrap_or_default();
                let shared = if observed.shared_with.is_empty() {
                    "Not shared with another computer yet".into()
                } else {
                    format!("Shared with {}", observed.shared_with.join(", "))
                };
                content = content.child(
                    h_flex()
                        .w_full()
                        .p_4()
                        .gap_4()
                        .border_1()
                        .border_color(p.border)
                        .rounded_lg()
                        .child(IconName::Folder.size_6().text_color(p.text_muted))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .child(vault.label.clone())
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(p.text_muted)
                                        .child(vault.path.display().to_string()),
                                )
                                .child(div().text_sm().text_color(p.text_muted).child(shared)),
                        )
                        .child(
                            v_flex()
                                .gap_1()
                                .child(div().text_sm().child(if observed.state.is_empty() {
                                    "Checking sync status".into()
                                } else {
                                    observed.state
                                }))
                                .child(div().text_xs().text_color(p.text_muted).child(format!(
                                    "{} files · {} MB",
                                    observed.files,
                                    observed.bytes / 1_000_000
                                ))),
                        )
                        .child(
                            Button::new(("sync-manage", index))
                                .ghost()
                                .label("Manage →")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.presentation.managing = Some(path.clone());
                                    cx.notify();
                                })),
                        ),
                );
            }
        }
        #[cfg(target_os = "linux")]
        {
            content = content.child(self.render_local_vaults(cx));
        }
        content = content.child(
            Button::new("sync-advanced")
                .debug_selector(|| "sync-advanced".into())
                .ghost()
                .label(if self.presentation.advanced {
                    "▾ Advanced"
                } else {
                    "› Advanced · Network, existing Syncthing & conflicts"
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.presentation.advanced = !this.presentation.advanced;
                    cx.notify();
                })),
        );
        if self.presentation.advanced {
            content = content.child(self.render_advanced(cx));
        }
        let mut panel = v_flex()
            .id("sync-panel")
            .debug_selector(|| "sync-panel".into())
            .relative()
            .size_full()
            .min_h_0()
            .child(self.render_approval(cx))
            .child(
                v_flex()
                    .id("sync-steps")
                    .debug_selector(|| "sync-steps".into())
                    .w_full()
                    .min_h_0()
                    .flex_1()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(content),
            );
        if self.presentation.wizard.is_some() || self.presentation.managing.is_some() {
            let body = if self.presentation.managing.is_some() {
                self.render_manage(cx)
            } else {
                self.render_wizard(cx)
            };
            panel = panel.child(
                div()
                    .id("sync-modal")
                    .debug_selector(|| "sync-modal".into())
                    .absolute()
                    .inset_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(p.canvas.opacity(0.92))
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        if event.keystroke.key == "escape" {
                            this.presentation.wizard = None;
                            this.presentation.managing = None;
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }))
                    .child(
                        v_flex()
                            .id("sync-wizard-scroll")
                            .max_h_full()
                            .w_full()
                            .max_w(px(560.))
                            .overflow_y_scroll()
                            .p_6()
                            .gap_4()
                            .bg(p.surface)
                            .border_1()
                            .border_color(p.border)
                            .rounded_lg()
                            .child(
                                h_flex()
                                    .justify_between()
                                    .child(div().text_sm().text_color(p.text_muted).child(format!(
                                        "On {} · {}",
                                        self.name.read(cx).value(),
                                        std::env::consts::OS
                                    )))
                                    .child(
                                        Button::new("sync-close-wizard")
                                            .ghost()
                                            .label("Close")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.presentation.wizard = None;
                                                this.presentation.managing = None;
                                                cx.notify();
                                            })),
                                    ),
                            )
                            .child(body),
                    ),
            );
        }
        panel
    }
}

impl SyncSettings {
    fn render_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = brand::palette(cx);
        let mut status = presentation::summary(self.snapshot.as_ref(), self.error.as_deref());
        if self.error.is_none()
            && self.snapshot.as_ref().is_some_and(|s| {
                !s.attached.is_empty()
                    && !s.preparing
                    && s.pending.is_none()
                    && s.offer_url.is_none()
            })
            && !self.presentation.overview.vaults.is_empty()
            && self
                .presentation
                .overview
                .vaults
                .values()
                .all(|v| v.state == "Up to date")
        {
            status = "Your local vaults are up to date".into();
        }
        let mut result = v_flex()
            .id("sync-current-step")
            .debug_selector(|| "sync-current-step".into())
            .flex_shrink_0()
            .gap_2();
        let mut line = h_flex()
            .w_full()
            .p_4()
            .gap_4()
            .justify_between()
            .rounded_lg()
            .bg(p.selected)
            .child(
                div()
                    .id("sync-status")
                    .debug_selector(|| "sync-status".into())
                    .text_sm()
                    .child(status),
            );
        if self.error.is_some() {
            line = line
                .id("sync-error")
                .debug_selector(|| "sync-error".into())
                .child(
                    Button::new("sync-retry")
                        .label(if self.presentation.wizard == Some(Step::Expired) {
                            "Create new link"
                        } else {
                            "Try again"
                        })
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            if this.presentation.wizard == Some(Step::Expired) {
                                this.presentation.wizard = Some(Step::Start);
                                this.error = None;
                                cx.notify();
                            } else if let Some(operation) = this.presentation.retry.clone() {
                                this.run(operation, cx);
                            } else {
                                this.run(Operation::Read, cx);
                            }
                        })),
                );
        } else {
            line = line.child(
                Button::new("sync-refresh")
                    .debug_selector(|| "sync-refresh".into())
                    .ghost()
                    .label("Check again")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.run(Operation::Read, cx))),
            );
        }
        result = result.child(line);
        if self.error.is_some() {
            result = result.child(
                Button::new("sync-error-details")
                    .ghost()
                    .label(if self.presentation.details {
                        "▾ Details"
                    } else {
                        "› Details"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.presentation.details = !this.presentation.details;
                        cx.notify();
                    })),
            );
            if self.presentation.details {
                result = result.child(
                    div()
                        .id("sync-technical-details")
                        .debug_selector(|| "sync-technical-details".into())
                        .text_xs()
                        .text_color(p.text_muted)
                        .child(
                            self.presentation
                                .error_details
                                .clone()
                                .unwrap_or_else(|| "No technical details available.".into()),
                        ),
                );
            }
        }
        if self.busy {
            result = result.child(
                div()
                    .id("sync-pending")
                    .debug_selector(|| "sync-pending".into())
                    .text_sm()
                    .text_color(p.text_muted)
                    .child(
                        div()
                            .debug_selector({
                                let setup = self.snapshot.is_none();
                                move || {
                                    if setup {
                                        "sync-setup".into()
                                    } else {
                                        "sync-connecting".into()
                                    }
                                }
                            })
                            .child(if self.snapshot.is_none() {
                                "Setting up Sync on this computer…"
                            } else if self.presentation.wizard == Some(Step::Waiting) {
                                "Sending your request. Keep Okilum open on both computers…"
                            } else {
                                "Checking…"
                            }),
                    ),
            );
        }
        result.into_any_element()
    }
    pub(super) fn render_approval(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(pending) = self
            .snapshot
            .as_ref()
            .and_then(|s| s.pending.as_ref())
            .filter(|p| p.incoming)
        else {
            return div().into_any_element();
        };
        let p = brand::palette(cx);
        let id = pending.request_id;
        h_flex()
            .id("sync-approval-banner")
            .debug_selector(|| "sync-approval-banner".into())
            .w_full()
            .flex_shrink_0()
            .p_4()
            .gap_4()
            .bg(p.selected)
            .child(
                v_flex()
                    .flex_1()
                    .gap_1()
                    .child(format!("{} wants to connect", pending.name))
                    .child(
                        div().text_sm().text_color(p.text_muted).child(
                            "Approve this computer before it can choose your shared vaults.",
                        ),
                    ),
            )
            .child(
                Button::new("sync-reject-computer")
                    .debug_selector(|| "sync-reject-computer".into())
                    .ghost()
                    .label("Reject")
                    .disabled(self.busy)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.run(Operation::Reject(id), cx)),
                    ),
            )
            .child(
                Button::new("sync-approve-computer")
                    .debug_selector(|| "sync-approve-computer".into())
                    .primary()
                    .label("Approve")
                    .disabled(self.busy)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.run(Operation::Approve(id), cx)),
                    ),
            )
            .into_any_element()
    }
    fn render_wizard(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = brand::palette(cx);
        let mut step = self.presentation.wizard.unwrap_or(Step::Start);
        if let Some(snapshot) = &self.snapshot {
            if snapshot.pending.as_ref().is_some_and(|p| !p.incoming) {
                step = Step::Waiting;
            } else if snapshot.offer_url.is_some() && step != Step::Expired {
                step = Step::Link;
            }
        }
        let local = self.name.read(cx).value().to_string();
        let (title,help)=match step {
            Step::Start=>("Start with the computer that has your notes".into(),format!("You are on {local}. Create a link here if this computer has your notes. Keep Okilum open on both computers.")),
            Step::Paste=>("Paste the link from your other computer".into(),format!("On {local}, paste the link created on the computer with your notes. You will approve {local} on that computer.")),
            Step::Link=>("Copy this link to your other computer".into(),format!("On {local}, copy the link. On the other computer open Settings → Sync → Add computer → I have a link. Return to {local} to approve its request.")),
            Step::Waiting=>("Waiting for your other computer to approve".into(),format!("{local} has requested a connection. On the computer that created the link, approve “{local} wants to connect”. This window keeps waiting; you do not need to retry.")),
            Step::Vaults=>(format!("Choose vaults for {local}"),format!("On {local}, choose a vault from the approved computer and a local folder. New or empty folders start Sync; existing files require a lossless merge preview.")),
            Step::Progress=>("Your first sync is in progress".into(),format!("Receiving on {local}. Keep both computers online until the first transfer finishes. You can close this window and follow progress on the Sync page.")),
            Step::Expired=>("Your pairing link expired".into(),"On the computer with your notes, create a new link, then paste it on the other computer. Your vaults have not changed.".into()),
        };
        let mut body = v_flex()
            .gap_4()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(div().text_sm().text_color(p.text_muted).child(help));
        if self
            .snapshot
            .as_ref()
            .and_then(|s| s.pending.as_ref())
            .is_some_and(|p| p.incoming)
        {
            return body.child(self.render_approval(cx)).into_any_element();
        }
        if self.error.is_some() && step != Step::Expired {
            body = body.child(self.render_status(cx));
        }
        match step {
            Step::Start | Step::Paste => {
                body = body
                    .child("This computer’s name")
                    .child(Input::new(&self.name).disabled(self.busy));
                if step == Step::Start {
                    body = body
                        .child(
                            Button::new("sync-create-link")
                                .debug_selector(|| "sync-create-link".into())
                                .primary()
                                .label("Create pairing link")
                                .disabled(self.busy)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.start_pairing(false, cx)),
                                ),
                        )
                        .child(
                            Button::new("sync-paste-link")
                                .ghost()
                                .label("I have a link from another computer")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.presentation.wizard = Some(Step::Paste);
                                    cx.notify();
                                })),
                        );
                } else {
                    body = body
                        .child("Pairing link")
                        .child(Input::new(&self.invitation).disabled(self.busy))
                        .child(
                            Button::new("sync-connect")
                                .debug_selector(|| "sync-connect".into())
                                .primary()
                                .label("Request connection")
                                .disabled(self.busy)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.start_pairing(true, cx)),
                                ),
                        );
                }
            }
            Step::Link => {
                if let Some(url) = self.snapshot.as_ref().and_then(|s| s.offer_url.clone()) {
                    let copy = url.clone();
                    body = body
                        .child(
                            div()
                                .text_sm()
                                .text_color(p.text_muted)
                                .child("One computer · valid for one hour"),
                        )
                        .child(
                            div()
                                .id("sync-pairing-url")
                                .debug_selector(|| "sync-pairing-url".into())
                                .w_full()
                                .overflow_hidden()
                                .truncate()
                                .text_sm()
                                .child(url),
                        )
                        .child(
                            Button::new("sync-copy-url")
                                .debug_selector(|| "sync-copy-url".into())
                                .primary()
                                .label("Copy link")
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                                }),
                        )
                        .child(
                            Button::new("sync-cancel-pairing")
                                .ghost()
                                .label("Cancel invitation")
                                .disabled(self.busy)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.presentation.wizard = None;
                                    this.run(Operation::Cancel, cx);
                                })),
                        );
                }
            }
            Step::Waiting => {
                body = body
                    .child(
                        div()
                            .id("sync-waiting-approval")
                            .debug_selector(|| "sync-waiting-approval".into())
                            .p_4()
                            .bg(p.selected)
                            .rounded_lg()
                            .child("Still waiting · no files are shared before approval"),
                    )
                    .child(
                        Button::new("sync-cancel-request")
                            .debug_selector(|| "sync-cancel-request".into())
                            .ghost()
                            .label("Cancel request")
                            .on_click(cx.listener(|this, _, _, cx| {
                                // Invalidate any in-flight result before applying cancellation.
                                this.epoch.invalidate();
                                this.busy = false;
                                this.presentation.wizard = None;
                                this.run(Operation::Cancel, cx);
                            })),
                    );
            }
            Step::Vaults => {
                body = body.child(self.render_catalog(cx));
            }
            Step::Progress => {
                if let Some(snapshot) = &self.snapshot {
                    if !snapshot.preparing && !snapshot.attached.is_empty() {
                        body = body.child(
                            "First sync complete. Saved changes now sync in both directions.",
                        );
                    }
                    for vault in &snapshot.attached {
                        let state = self.presentation.overview.vaults.get(&vault.path);
                        body = body.child(
                            v_flex().gap_2().child(vault.label.clone()).child(
                                div().text_sm().child(
                                    state
                                        .map(|s| {
                                            format!(
                                                "{} · {} MB remaining of {} MB",
                                                s.state,
                                                s.needed_bytes / 1_000_000,
                                                s.bytes / 1_000_000
                                            )
                                        })
                                        .unwrap_or_else(|| "Checking first transfer…".into()),
                                ),
                            ),
                        );
                    }
                }
            }
            Step::Expired => {
                body = body.child(
                    Button::new("sync-new-link")
                        .debug_selector(|| "sync-new-link".into())
                        .primary()
                        .label("Create new link")
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.error = None;
                            this.start_pairing(false, cx);
                        })),
                );
            }
        }
        body.into_any_element()
    }
    fn render_catalog(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = brand::palette(cx);
        let mut body = v_flex().gap_4();
        let Some(snapshot) = &self.snapshot else {
            return body.into_any_element();
        };
        if snapshot.catalog.is_empty() {
            body=body.child(div().text_sm().text_color(p.text_muted).child(if self.busy{"Loading available vaults…"}else{"No vaults are shared yet. On the other computer, share a vault in Sync settings, then choose Vaults here again."}));
        }
        if let Some(peer) = self.selected_peer {
            if !snapshot.catalog.is_empty() {
                body = body.child(
                    div()
                        .id("sync-authorized-catalog")
                        .debug_selector(|| "sync-authorized-catalog".into())
                        .child("Available vaults"),
                );
            }
            for (index, vault) in snapshot.catalog.iter().enumerate() {
                let id = vault.id.clone();
                let new_id = id.clone();
                let label = vault.label.clone();
                body = body.child(
                    v_flex().gap_2().child(vault.label.clone()).child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new(("sync-new-vault-folder", index))
                                    .debug_selector(move || {
                                        format!("sync-new-vault-folder-{index}")
                                    })
                                    .primary()
                                    .label("Choose local folder")
                                    .disabled(self.busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.choose_new_folder(peer, new_id.clone(), &label, cx)
                                    })),
                            )
                            .child(
                                Button::new(("sync-attach-vault", index))
                                    .ghost()
                                    .label("Use existing folder")
                                    .disabled(self.busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.choose_folder(Some((peer, id.clone())), cx)
                                    })),
                            ),
                    ),
                );
                if let Some((_, message)) = self
                    .destination_error
                    .as_ref()
                    .filter(|(id, _)| id == &vault.id)
                {
                    body = body.child(
                        div()
                            .id(("sync-destination-error", index))
                            .debug_selector(move || format!("sync-destination-error-{index}"))
                            .text_sm()
                            .child(message.clone()),
                    );
                }
            }
        }
        body.into_any_element()
    }
    fn render_manage(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut body = v_flex().gap_4();
        if let Some(vault) = self.snapshot.as_ref().and_then(|s| {
            s.attached
                .iter()
                .find(|v| Some(&v.path) == self.presentation.managing.as_ref())
        }) {
            body = body
                .child(div().text_lg().child(vault.label.clone()))
                .child(vault.path.display().to_string())
                .child(self.vault_connection_label(vault));
            if let Some(state) = self.presentation.overview.vaults.get(&vault.path) {
                body = body
                    .child(format!("Shared with {}", state.shared_with.join(", ")))
                    .child(state.state.clone());
            }
            body = body.child(
                "Use Advanced to review the existing connection, network settings and conflicts.",
            );
        }
        body.into_any_element()
    }
}
impl SyncSettings {
    #[cfg(target_os = "linux")]
    pub(super) fn vault_connection_label(&self, vault: &ExistingVault) -> String {
        let managed = self
            .runtimes
            .iter()
            .any(|r| r.managed && r.identity == vault.daemon);
        let label = if managed {
            "Synced by Okilum"
        } else {
            "Uses your existing Syncthing"
        };
        if vault.paused {
            format!("{label} · Paused")
        } else {
            label.into()
        }
    }
    #[cfg(target_os = "macos")]
    pub(super) fn vault_connection_label(&self, vault: &ExistingVault) -> String {
        if vault.paused {
            "Paused in Syncthing"
        } else {
            "Sync connected"
        }
        .into()
    }
}
