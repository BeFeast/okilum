use super::*;
use presentation::Step;

impl Render for SyncSettings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.inputs(window, cx);
        use gpui_component::WindowExt;
        if !window.has_active_dialog(cx) {
            self.presentation.dialog_open = false;
        }
        let needs_dialog =
            self.presentation.wizard.is_some() || self.presentation.managing.is_some();
        if needs_dialog && !self.presentation.dialog_open && !window.has_active_dialog(cx) {
            self.presentation.dialog_open = true;
            let sync = cx.entity();
            let wizard = cx.new(|cx| {
                let changes = cx.observe(&sync, |_, _, cx| cx.notify());
                presentation::Wizard {
                    sync: sync.clone(),
                    _changes: changes,
                }
            });
            let closed = sync.downgrade();
            window.open_dialog(cx, move |dialog, _, _| {
                dialog
                    .width(px(560.))
                    .close_button(false)
                    .overlay_closable(false)
                    .child(wizard.clone())
                    .on_close({
                        let closed = closed.clone();
                        move |_, _, cx| {
                            let _ = closed.update(cx, |view, cx| {
                                view.presentation.dialog_open = false;
                                view.presentation.wizard = None;
                                view.presentation.managing = None;
                                cx.notify();
                            });
                        }
                    })
            });
        } else if !needs_dialog && self.presentation.dialog_open {
            self.presentation.dialog_open = false;
            window.close_dialog(cx);
        }
        let p = brand::palette(cx);
        let header = v_flex().gap_6().w_full().flex_shrink_0().child(
            h_flex()
                .justify_between()
                .gap_4()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(
                            div()
                                .w_full()
                                .whitespace_normal()
                                .text_xl()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Sync"),
                        )
                        .child(
                            div()
                                .w_full()
                                .whitespace_normal()
                                .text_sm()
                                .text_color(p.text_muted)
                                .child("Keep your vaults together across computers."),
                        ),
                )
                .child(
                    Button::new("sync-add-computer")
                        .debug_selector(|| "sync-add-computer".into())
                        .flex_shrink_0()
                        .primary()
                        .label("Add computer")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.presentation.wizard = Some(Step::Start);
                            cx.notify();
                        })),
                ),
        );
        let header = header.child(self.render_status(cx));
        let mut content = v_flex().gap_6().w_full().min_w_0();
        let empty = self.snapshot.as_ref().is_none_or(|s| s.peers.is_empty());
        if empty {
            content = content.child(v_flex().id("sync-empty").debug_selector(|| "sync-empty".into())
                .w_full().p_6().gap_4().items_center().border_1().border_color(p.border).rounded_lg()
                .child(gpui_component::Icon::new(IconName::Network).size_8().text_color(p.text_muted))
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
                            .child(format!("This computer: {}", input_text(&self.name, cx))),
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
                            .child(
                                gpui_component::Icon::new(IconName::Network)
                                    .size_6()
                                    .flex_shrink_0()
                                    .text_color(p.text_muted),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .child(
                                        div().w_full().whitespace_normal().child(peer.name.clone()),
                                    )
                                    .children(presentation::computer_details(observed.vaults).map(
                                        |details| {
                                            div()
                                                .w_full()
                                                .whitespace_normal()
                                                .text_sm()
                                                .text_color(p.text_muted)
                                                .child(details)
                                        },
                                    )),
                            )
                            .child(
                                v_flex()
                                    .w_32()
                                    .flex_shrink_0()
                                    .gap_1()
                                    .child(
                                        div()
                                            .w_full()
                                            .whitespace_normal()
                                            .text_sm()
                                            .child(connection),
                                    )
                                    .child(
                                        div()
                                            .w_full()
                                            .whitespace_normal()
                                            .text_xs()
                                            .text_color(p.text_muted)
                                            .child(seen),
                                    ),
                            )
                            .child(
                                Button::new(("sync-catalog", index))
                                    .debug_selector(move || format!("sync-catalog-{index}"))
                                    .flex_shrink_0()
                                    .ghost()
                                    .label("Vaults →")
                                    .disabled(self.busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.selected_peer = Some(id);
                                        this.presentation.destinations.clear();
                                        this.presentation.wizard = Some(Step::Computer);
                                        cx.notify();
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
                let ignore_path = vault.path.clone();
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
                        .child(
                            gpui_component::Icon::new(IconName::Folder)
                                .size_6()
                                .flex_shrink_0()
                                .text_color(p.text_muted),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .child(div().w_full().truncate().child(vault.label.clone()))
                                .child(
                                    div()
                                        .w_full()
                                        .truncate()
                                        .text_sm()
                                        .text_color(p.text_muted)
                                        .child(vault.path.display().to_string()),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .whitespace_normal()
                                        .text_sm()
                                        .text_color(p.text_muted)
                                        .child(shared),
                                ),
                        )
                        .child(
                            v_flex()
                                .w_32()
                                .flex_shrink_0()
                                .gap_1()
                                .child(div().w_full().whitespace_normal().text_sm().child(
                                    if observed.state.is_empty() {
                                        "Checking sync status".into()
                                    } else {
                                        observed.state
                                    },
                                ))
                                .child(
                                    div()
                                        .w_full()
                                        .whitespace_normal()
                                        .text_xs()
                                        .text_color(p.text_muted)
                                        .child(if observed.available {
                                            format!(
                                                "{} files · {} MB",
                                                observed.files,
                                                observed.bytes / 1_000_000
                                            )
                                        } else {
                                            "Status not checked".into()
                                        }),
                                ),
                        )
                        .child(
                            Button::new(("sync-shared-policy", index))
                                .debug_selector(move || format!("sync-shared-policy-{index}"))
                                .ghost()
                                .label("Shared exclusions")
                                .disabled(self.busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.edit_ignore_policy(ignore_path.clone(), window, cx)
                                })),
                        )
                        .child(
                            Button::new(("sync-manage", index))
                                .flex_shrink_0()
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
        if let Some(snapshot) = &self.snapshot {
            for (index, setting) in snapshot.history_retention.iter().enumerate() {
                content = content.child(v_flex().gap_1()
                    .child(format!("Previous Sync versions · {}", setting.path.display()))
                    .child(format!("Keep for {} days on this computer. Restore from Note history or Recover notes. Sync is not a backup of every local edit.", setting.days.days()))
                    .child(h_flex().gap_2().children([7u16, 30, 90, 365].into_iter().map(|days| {
                        let path = setting.path.clone();
                        Button::new(("sync-history-days", index * 4 + [7,30,90,365].iter().position(|d| *d == days).unwrap()))
                            .debug_selector(move || format!("sync-history-days-{index}-{days}"))
                            .label(format!("{days} days"))
                            .selected(setting.days.days() == days)
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| this.choose_retention(path.clone(), direct::RetentionDays::try_from(days).expect("valid retention preset"), window, cx)))
                    }))));
            }
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
            .min_w_0()
            .min_h_0()
            .gap_6()
            .when(self.presentation.wizard.is_none(), |panel| {
                panel.child(self.render_approval(cx))
            })
            .child(header)
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
        panel = panel.children(gpui_component::Root::render_dialog_layer(window, cx));
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
                    && matches!(s.status.as_str(), "Ready" | "Bidirectional Sync active")
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
            .id(if self.error.is_some() {
                "sync-error"
            } else {
                "sync-status-line"
            })
            .debug_selector({
                let failed = self.error.is_some();
                move || {
                    if failed {
                        "sync-error".into()
                    } else {
                        "sync-status-line".into()
                    }
                }
            })
            .w_full()
            .p_4()
            .gap_4()
            .justify_between()
            .rounded_lg()
            .bg(if self.error.is_some() {
                p.warning.opacity(0.12)
            } else {
                p.selected
            })
            .child(
                div()
                    .id("sync-status")
                    .debug_selector(|| "sync-status".into())
                    .flex_1()
                    .min_w_0()
                    .whitespace_normal()
                    .text_sm()
                    .child(status),
            );
        if self.error.is_some() {
            let existing = self
                .error
                .as_ref()
                .is_some_and(|e| e.contains("already belongs to Syncthing"));
            let connection = self.error.as_ref().is_some_and(|e| {
                e.contains("Choose which existing Sync connection")
                    || e.contains("Select the existing instance explicitly")
            });
            line = line.child(
                Button::new("sync-retry")
                    .flex_shrink_0()
                    .label(if self.presentation.wizard == Some(Step::Expired) {
                        "Create new link"
                    } else if existing {
                        "Review existing setup"
                    } else if connection {
                        "Choose connection"
                    } else {
                        "Try again"
                    })
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if existing {
                            this.presentation.advanced = true;
                            this.presentation.wizard = None;
                            #[cfg(target_os = "linux")]
                            if let Some(Operation::Share(path)) = this.presentation.retry.clone() {
                                this.run(Operation::Inspect(path), cx);
                                return;
                            }
                            this.choose_folder(None, cx);
                        } else if connection {
                            this.presentation.advanced = true;
                            this.presentation.wizard = None;
                            this.scroll.set_offset(point(px(0.), px(-500.)));
                            cx.notify();
                        } else if this.presentation.wizard == Some(Step::Expired) {
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
                    .flex_shrink_0()
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
                    .debug_selector(|| "sync-error-details".into())
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
        if let Some(snapshot) = &self.snapshot {
            for (index, warning) in snapshot
                .history_warnings
                .iter()
                .map(|w| (&w.path, &w.message))
                .chain(
                    snapshot
                        .policy_warnings
                        .iter()
                        .map(|w| (&w.path, &w.message)),
                )
                .enumerate()
            {
                result = result.child(
                    div()
                        .id(("sync-history-warning", index))
                        .text_sm()
                        .child(format!("{}: {}", warning.0.display(), warning.1)),
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
            .min_w_0()
            .flex_shrink_0()
            .p_4()
            .gap_4()
            .bg(p.selected)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .w_full()
                            .whitespace_normal()
                            .child(format!("{} wants to connect", pending.name)),
                    )
                    .child(
                        div()
                            .w_full()
                            .whitespace_normal()
                            .text_sm()
                            .text_color(p.text_muted)
                            .child(
                                "Approve this computer before it can choose your shared vaults.",
                            ),
                    ),
            )
            .child(
                Button::new("sync-reject-computer")
                    .debug_selector(|| "sync-reject-computer".into())
                    .flex_shrink_0()
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
                    .flex_shrink_0()
                    .primary()
                    .label("Approve")
                    .disabled(self.busy)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.run(Operation::Approve(id), cx)),
                    ),
            )
            .into_any_element()
    }
    pub(super) fn render_wizard(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = brand::palette(cx);
        let mut step = self.presentation.wizard.unwrap_or(Step::Start);
        if let Some(snapshot) = &self.snapshot {
            if snapshot.pending.as_ref().is_some_and(|p| !p.incoming) {
                step = Step::Waiting;
            } else if snapshot.offer_url.is_some() && matches!(step, Step::Start | Step::Link) {
                step = Step::Link;
            }
        }
        let local = input_text(&self.name, cx);
        let remote = self
            .selected_peer
            .and_then(|id| {
                self.snapshot
                    .as_ref()?
                    .peers
                    .iter()
                    .find(|p| p.id == id)
                    .map(|p| p.name.clone())
            })
            .or_else(|| {
                direct::pairing_computer(&input_text(&self.invitation, cx))
                    .ok()
                    .map(|ip| format!("computer at {ip}"))
            })
            .unwrap_or_else(|| "your other computer".into());
        let complete = self
            .presentation
            .first_sync_complete(self.snapshot.as_ref(), self.busy);
        let (title,help)=match step {
            Step::Start=>("Start on the computer with your notes".into(),"Create a link if your notes are here. Keep Okilum open on both computers.".into()),
            Step::Paste=>("Paste the link from your other computer".into(),format!("Paste the link created on the computer with your notes. You will approve {local} on that computer.")),
            Step::Link=>("Copy this link to your other computer".into(),format!("Copy the link. On the other computer open Settings → Sync → Add computer → I have a link. Return to {local} to approve its request.")),
            Step::Waiting=>(format!("Waiting for {remote} to approve"),format!("{local} has requested a connection. On {remote}, approve “{local} wants to connect”. This window keeps waiting; you do not need to retry.")),
            Step::Vaults=>("Choose vaults".into(),"Choose a vault from the approved computer and a new or empty local folder. If the folder already has files, Okilum leaves them untouched and asks you to choose an empty folder.".into()),
            Step::Computer=>(remote.clone(),format!("Vaults shared between {local} and {remote}.")),
            Step::Progress if complete => ("Your first sync is complete".into(), "Your vaults are up to date. Saved changes now sync in both directions.".into()),
            Step::Progress=>("Your first sync is in progress".into(),format!("Receiving on {local}. Keep both computers online until the first transfer finishes. You can close this window and follow progress on the Sync page.")),
            Step::Expired=>("Your pairing link expired".into(),"On the computer with your notes, create a new link, then paste it on the other computer. Your vaults have not changed.".into()),
            Step::Existing => ("Use your existing Sync connection".into(), "Review the folder before allowing Okilum to use its existing Syncthing connection.".into()),
        };
        let mut body = v_flex()
            .gap_3()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(div().text_sm().text_color(p.text_muted).child(help));
        if let Some(pending) = self
            .snapshot
            .as_ref()
            .and_then(|s| s.pending.as_ref())
            .filter(|p| p.incoming)
        {
            return v_flex()
                .gap_3()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!("Allow {} to connect?", pending.name)),
                )
                .child(div().text_sm().text_color(p.text_muted).child(format!(
                    "Confirm this is the computer you just used. {} is waiting for your approval.",
                    pending.name
                )))
                .child(self.render_approval(cx))
                .into_any_element();
        }
        if self.error.is_some() && step != Step::Expired {
            body = body.child(self.render_status(cx));
        }
        match step {
            Step::Existing => {
                body = body.child(self.render_existing_connection(cx));
            }
            Step::Start | Step::Paste => {
                body = body.child("This computer’s name").child(
                    Input::new(
                        self.name
                            .as_ref()
                            .expect("Settings initializes Sync inputs"),
                    )
                    .disabled(self.busy),
                );
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
                        .child(
                            Input::new(
                                self.invitation
                                    .as_ref()
                                    .expect("Settings initializes Sync inputs"),
                            )
                            .disabled(self.busy),
                        )
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
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.presentation.wizard = None;
                                this.run(Operation::Cancel, cx);
                            })),
                    );
            }
            Step::Vaults => {
                body = body.child(self.render_catalog(cx));
            }
            Step::Computer => {
                body = body.child(self.render_computer(cx));
            }
            Step::Progress => {
                if let Some(snapshot) = &self.snapshot {
                    if complete {
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
                                        .filter(|s| s.available)
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
                body = body.child(
                    Button::new("sync-choose-more-vaults")
                        .debug_selector(|| "sync-choose-more-vaults".into())
                        .ghost()
                        .label("Choose more vaults")
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.presentation.wizard = Some(Step::Vaults);
                            cx.notify();
                        })),
                );
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
                            // A receiving computer cannot create an invitation
                            // on the source. Return to the explicit source step
                            // rather than silently reverse the pairing roles.
                            if this.receiving {
                                this.presentation.wizard = Some(Step::Start);
                                cx.notify();
                            } else {
                                this.start_pairing(false, cx);
                            }
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
                let new_id = vault.id.clone();
                let label = vault.label.clone();
                body = body.child(
                    v_flex().gap_2().child(vault.label.clone()).child(
                        h_flex().gap_2().child(
                            Button::new(("sync-new-vault-folder", index))
                                .debug_selector(move || format!("sync-new-vault-folder-{index}"))
                                .primary()
                                .label("Choose folder")
                                .disabled(self.busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.presentation.picking_vault = true;
                                    this.choose_vault_folder(peer, new_id.clone(), &label, cx)
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
        if !snapshot.catalog.is_empty() {
            for (id, operation) in &self.presentation.destinations {
                if let Operation::Attach(_, _, path) | Operation::AttachNew(_, _, path) = operation
                {
                    let label = snapshot
                        .catalog
                        .iter()
                        .find(|v| &v.id == id)
                        .map(|v| v.label.as_str())
                        .unwrap_or(id);
                    body = body.child(
                        div()
                            .text_sm()
                            .child(format!("Selected: {label} → {}", path.display())),
                    );
                }
            }
            body = body.child(
                Button::new("sync-start-first-transfer")
                    .debug_selector(|| "sync-start-first-transfer".into())
                    .primary()
                    .label("Start first sync")
                    .disabled(self.busy || self.presentation.destinations.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.presentation.transfers =
                            std::mem::take(&mut this.presentation.destinations)
                                .into_values()
                                .collect();
                        if let Some(operation) = this.presentation.transfers.pop_front() {
                            this.run(operation, cx);
                        }
                    })),
            );
        }
        body.into_any_element()
    }
    fn render_computer(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut body = v_flex().gap_3();
        let Some(peer) = self
            .selected_peer
            .and_then(|id| self.snapshot.as_ref()?.peers.iter().find(|p| p.id == id))
        else {
            return body.into_any_element();
        };
        let observed = self
            .presentation
            .overview
            .computers
            .get(&peer.id)
            .cloned()
            .unwrap_or_default();
        body = body.child(match observed.connected {
            Some(true) => "Online",
            Some(false) => "Offline",
            None => "Connection not checked",
        });
        let mut count = 0;
        if let Some(snapshot) = &self.snapshot {
            for vault in &snapshot.attached {
                if observed.shared_vaults.contains(&vault.path) {
                    body = body.child(
                        v_flex().gap_1().child(vault.label.clone()).child(
                            div()
                                .text_sm()
                                .text_color(brand::palette(cx).text_muted)
                                .child(vault.path.display().to_string()),
                        ),
                    );
                    count += 1;
                }
            }
        }
        if count == 0 {
            body = body.child("No shared vaults are listed on this computer yet.");
        }
        if observed.can_receive {
            let id = peer.id;
            body = body.child(
                Button::new("sync-receive-more")
                    .primary()
                    .label("Choose vaults to receive")
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.presentation.wizard = Some(Step::Vaults);
                        if let Some(s) = this.snapshot.as_mut() {
                            s.catalog.clear();
                        }
                        this.run(Operation::Catalog(id), cx);
                    })),
            );
        } else {
            body = body.child(format!(
                "To receive another vault on {}, open Sync there and choose it from this computer.",
                peer.name
            ));
        }
        body.into_any_element()
    }
    pub(super) fn render_manage(&self, _cx: &mut Context<Self>) -> AnyElement {
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
