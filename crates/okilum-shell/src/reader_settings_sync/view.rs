use super::*;

impl Render for SyncSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = brand::palette(cx);
        let mut content = v_flex().w_full().gap_4()
            .child(crate::reader_sync_help::entry())
            .child(div().text_base().child("Connect your computers"))
            .child(div().text_sm().text_color(p.text_muted).child("Pair computers once, then choose which vaults to sync. Confirm each new computer on the computer that shared the URL."))
            .child(v_flex().gap_2().child("This computer").child(Input::new(&self.name).disabled(self.busy)))
            .child(h_flex().gap_2()
                .child(Button::new("sync-add-computer").label("Add computer").disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.start_pairing(false, cx))))
                .child(Button::new("sync-refresh").label("Refresh").ghost().disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.run(Operation::Read, cx)))))
            .child(v_flex().gap_2().child("Connect to an existing computer")
                .child(Input::new(&self.invitation).disabled(self.busy))
                .child(Button::new("sync-connect").label("Connect").disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.start_pairing(true, cx)))));
        #[cfg(target_os = "linux")]
        if self.runtimes.is_empty() {
            content = content.child(
                v_flex().gap_2()
                    .child("No existing Sync connection is selected.")
                    .child(Button::new("sync-prepare-managed")
                        .label("Set up Sync on this computer")
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, _, cx| this.run(Operation::PrepareManaged, cx))))
                    .child(div().text_sm().text_color(p.text_muted)
                        .child("Use the bundled runtime on this LAN. Existing Syncthing instances must be selected instead."))
            );
        }
        #[cfg(target_os = "linux")]
        if !self.runtimes.is_empty() {
            content = content.child(div().text_base().child("Sync connection"));
            for (index, runtime) in self.runtimes.iter().enumerate() {
                let path = runtime.identity.config_file.clone();
                let in_use = self
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| !s.peers.is_empty() || !s.attached.is_empty() || s.preparing);
                content = content.child(
                    h_flex()
                        .gap_2()
                        .child(div().text_sm().child(format!(
                            "{} · {} vaults",
                            format!(
                                    "{}…{}",
                                    runtime
                                        .identity
                                        .device_id
                                        .chars()
                                        .take(7)
                                        .collect::<String>(),
                                    runtime
                                        .identity
                                        .device_id
                                        .chars()
                                        .rev()
                                        .take(7)
                                        .collect::<String>()
                                        .chars()
                                        .rev()
                                        .collect::<String>()
                                ),
                            runtime.folders
                        )))
                        .child(
                            Button::new(("sync-select-runtime", index))
                                .label(if runtime.selected {
                                    "Selected"
                                } else {
                                    "Use connection"
                                })
                                .disabled(self.busy || runtime.selected || in_use)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.run(Operation::SelectConnection(path.clone()), cx)
                                })),
                        ),
                );
            }
        }
        if let Some(snapshot) = &self.snapshot {
            content = content.child(
                div()
                    .id("sync-status")
                    .text_sm()
                    .child(snapshot.status.clone()),
            );
            if let Some(url) = &snapshot.offer_url {
                let copy = url.clone();
                content = content.child(v_flex().gap_2()
                    .child("Open this pairing URL on your other computer")
                    .child(div().id("sync-pairing-url").text_sm().child(url.clone()))
                    .child(div().text_sm().text_color(p.text_muted).child("Keep this window open. The invitation expires after 10 minutes and can be used once."))
                    .child(h_flex().gap_2()
                        .child(Button::new("sync-copy-url").label("Copy pairing URL")
                            .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))))
                        .child(Button::new("sync-cancel-pairing").label("Cancel invitation").ghost().disabled(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| this.run(Operation::Cancel, cx))))));
            }
            if let Some(pending) = &snapshot.pending {
                if pending.incoming {
                    let id = pending.request_id;
                    content = content.child(v_flex().gap_2()
                        .child(format!("Allow {} to connect?", pending.name))
                        .child(div().text_sm().text_color(p.text_muted)
                            .child(format!("Device: {}\nIdentity: {}", pending.device_id, pending.fingerprint)))
                        .child(div().text_sm().text_color(p.text_muted)
                            .child("Only confirm the computer you are connecting. Approval lets it see your shared vault list."))
                        .child(h_flex().gap_2()
                            .child(Button::new("sync-approve-computer").label("Confirm computer").disabled(self.busy)
                                .on_click(cx.listener(move |this, _, _, cx| this.run(Operation::Approve(id), cx))))
                            .child(Button::new("sync-reject-computer").label("Decline").ghost().disabled(self.busy)
                                .on_click(cx.listener(move |this, _, _, cx| this.run(Operation::Reject(id), cx))))));
                } else {
                    content = content.child(
                        v_flex()
                            .gap_2()
                            .child("Waiting for confirmation on your other computer…")
                            .child(
                                Button::new("sync-cancel-request")
                                    .label("Cancel connection")
                                    .ghost()
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.run(Operation::Cancel, cx)
                                    })),
                            ),
                    );
                }
            }
            if !snapshot.peers.is_empty() {
                content = content.child(div().text_base().child("Paired computers"));
                for (index, peer) in snapshot.peers.iter().enumerate() {
                    let id = peer.id;
                    content = content.child(
                        h_flex()
                            .justify_between()
                            .gap_2()
                            .child(peer.name.clone())
                            .child(
                                Button::new(("sync-catalog", index))
                                    .label("Choose vaults")
                                    .ghost()
                                    .selected(self.selected_peer == Some(id))
                                    .disabled(self.busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.selected_peer = Some(id);
                                        // Never display a prior computer’s catalog during this request.
                                        if let Some(snapshot) = this.snapshot.as_mut() {
                                            snapshot.catalog.clear();
                                        }
                                        this.run(Operation::Catalog(id), cx);
                                    })),
                            ),
                    );
                }
            }
            if let Some(peer) = self.selected_peer {
                if snapshot.catalog.is_empty() && !self.busy {
                    content = content.child(div().text_sm().text_color(p.text_muted)
                        .child("No authorized vaults loaded. Choose vaults on a paired computer to refresh its shared list."));
                }
                if !snapshot.catalog.is_empty() {
                    content = content.child(
                        div()
                            .id("sync-authorized-catalog")
                            .text_base()
                            .child("Available vaults"),
                    );
                }
                for (index, vault) in snapshot.catalog.iter().enumerate() {
                    let id = vault.id.clone();
                    content = content.child(
                        h_flex()
                            .justify_between()
                            .gap_2()
                            .child(vault.label.clone())
                            .child(
                                Button::new(("sync-attach-vault", index))
                                    .label("Choose local folder")
                                    .disabled(self.busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.choose_folder(Some((peer, id.clone())), cx)
                                    })),
                            ),
                    );
                }
                content = content.child(div().text_sm().text_color(p.text_muted)
                    .child("An empty folder starts syncing automatically. A folder with existing files requires a lossless merge preview before Sync can start."));
            }
            if !snapshot.attached.is_empty() {
                content = content.child(div().text_base().child("Connected vaults"));
                for vault in &snapshot.attached {
                    content = content.child(
                        v_flex()
                            .gap_1()
                            .child(vault.label.clone())
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(p.text_muted)
                                    .child(vault.path.display().to_string()),
                            )
                            .child(div().text_sm().text_color(p.text_muted).child(
                                if vault.paused {
                                    "Paused in Syncthing"
                                } else {
                                    "Connected to existing Syncthing"
                                },
                            )),
                    );
                }
            }
        }
        content = content.child(v_flex().gap_2()
            .child("Already syncing this vault with Syncthing?")
            .child(div().text_sm().text_color(p.text_muted)
                .child("Choose the existing local vault to inspect its connection, then explicitly allow Okilum to use it."))
            .child(Button::new("sync-inspect-existing").label("Choose existing synced vault").ghost().disabled(self.busy)
                .on_click(cx.listener(|this, _, _, cx| this.choose_folder(None, cx)))));
        if let Some(vault) = &self.inspected {
            let approved = vault.clone();
            content = content.child(v_flex().gap_2()
                .child(format!("Use {} with Okilum?", vault.label))
                .child(div().text_sm().text_color(p.text_muted).child(format!("{}\nSyncthing {} · {}", vault.path.display(), vault.version, vault.daemon.device_id)))
                .child(div().text_sm().text_color(p.text_muted)
                    .child("This saves the connection and makes the vault available to computers you approve. The existing daemon keeps control of its lifecycle and configuration."))
                .child(h_flex().gap_2()
                    .child(Button::new("sync-consent-existing").label("Use this connection").disabled(self.busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.inspected = None;
                            this.run(Operation::Consent(approved.clone()), cx);
                        })))
                    .child(Button::new("sync-dismiss-existing").label("Cancel").ghost().disabled(self.busy)
                        .on_click(cx.listener(|this, _, _, cx| { this.inspected = None; cx.notify(); })))));
        }
        if self.busy {
            content = content.child(
                div()
                    .id("sync-pending")
                    .text_sm()
                    .text_color(p.text_muted)
                    .child("Connecting…"),
            );
        }
        if let Some(error) = &self.error {
            content = content.child(div().id("sync-error").text_sm().child(error.clone()));
        }
        content
    }
}
