use super::*;

impl Render for SyncSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = brand::palette(cx);
        let mut result = v_flex()
            .id("sync-current-step")
            .debug_selector(|| "sync-current-step".into())
            .w_full()
            .flex_shrink_0()
            .gap_2();
        let mut content = v_flex()
            .id("sync-steps")
            .debug_selector(|| "sync-steps".into())
            .w_full()
            .min_h_0()
            .flex_1()
            .gap_4()
            .overflow_y_scroll()
            .track_scroll(&self.scroll);
        if let Some(error) = &self.error {
            result = result.child(
                div()
                    .id("sync-error")
                    .debug_selector(|| "sync-error".into())
                    .text_sm()
                    .child(error.clone()),
            );
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
                                let setting_up = self.snapshot.is_none();
                                move || {
                                    if setting_up {
                                        "sync-setup".into()
                                    } else {
                                        "sync-connecting".into()
                                    }
                                }
                            })
                            .child(if self.snapshot.is_none() {
                                "Setting up Sync on this computer…"
                            } else {
                                "Connecting…"
                            }),
                    ),
            );
        }
        if let Some(snapshot) = &self.snapshot {
            result = result.child(
                div()
                    .id("sync-status")
                    .debug_selector(|| "sync-status".into())
                    .text_sm()
                    .child(snapshot.status.clone()),
            );
            if let Some(url) = snapshot
                .offer_url
                .as_ref()
                .filter(|_| snapshot.pending.is_none())
            {
                let copy = url.clone();
                result = result.child(
                    v_flex()
                        .gap_2()
                        .child("Open this pairing URL on your other computer")
                        .child(
                            div()
                                .id("sync-pairing-url")
                                .debug_selector(|| "sync-pairing-url".into())
                                .w_full()
                                .overflow_hidden()
                                .truncate()
                                .text_sm()
                                .child(url.clone()),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("sync-copy-url")
                                        .debug_selector(|| "sync-copy-url".into())
                                        .label("Copy pairing URL")
                                        .on_click(move |_, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                copy.clone(),
                                            ))
                                        }),
                                )
                                .child(
                                    Button::new("sync-cancel-pairing")
                                        .debug_selector(|| "sync-cancel-pairing".into())
                                        .label("Cancel invitation")
                                        .ghost()
                                        .disabled(self.busy)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.run(Operation::Cancel, cx)
                                        })),
                                ),
                        ),
                );
            }
            if let Some(pending) = &snapshot.pending {
                if pending.incoming {
                    let id = pending.request_id;
                    result = result.child(
                        v_flex()
                            .gap_2()
                            .child(format!("Allow {} to connect?", pending.name))
                            .child(div().text_sm().text_color(p.text_muted).child(format!(
                                "Device: {}\nIdentity: {}",
                                pending.device_id, pending.fingerprint
                            )))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new("sync-approve-computer")
                                            .debug_selector(|| "sync-approve-computer".into())
                                            .label("Confirm computer")
                                            .disabled(self.busy)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.run(Operation::Approve(id), cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("sync-reject-computer")
                                            .debug_selector(|| "sync-reject-computer".into())
                                            .label("Decline")
                                            .ghost()
                                            .disabled(self.busy)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.run(Operation::Reject(id), cx)
                                            })),
                                    ),
                            ),
                    );
                } else {
                    result = result.child(
                        v_flex()
                            .gap_2()
                            .child("Waiting for confirmation on your other computer…")
                            .child(
                                Button::new("sync-cancel-request")
                                    .debug_selector(|| "sync-cancel-request".into())
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
        }
        if let Some(snapshot) = &self.snapshot {
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
                                    .debug_selector(move || format!("sync-catalog-{index}"))
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
                            .debug_selector(|| "sync-authorized-catalog".into())
                            .text_base()
                            .child("Available vaults"),
                    );
                }
                for (index, vault) in snapshot.catalog.iter().enumerate() {
                    let id = vault.id.clone();
                    let new_id = id.clone();
                    let label = vault.label.clone();
                    content = content.child(
                        h_flex()
                            .justify_between()
                            .gap_2()
                            .child(vault.label.clone())
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new(("sync-new-vault-folder", index))
                                            .debug_selector(move || {
                                                format!("sync-new-vault-folder-{index}")
                                            })
                                            .label("Choose local folder")
                                            .disabled(self.busy)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.choose_new_folder(
                                                    peer,
                                                    new_id.clone(),
                                                    &label,
                                                    cx,
                                                )
                                            })),
                                    )
                                    .child(
                                        Button::new(("sync-attach-vault", index))
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
                        content = content.child(
                            div()
                                .id(("sync-destination-error", index))
                                .debug_selector(move || format!("sync-destination-error-{index}"))
                                .text_sm()
                                .child(message.clone()),
                        );
                    }
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
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(p.text_muted)
                                    .child(self.vault_connection_label(vault)),
                            ),
                    );
                }
            }
        }
        if !self
            .snapshot
            .as_ref()
            .is_some_and(|s| s.pending.is_some() || s.offer_url.is_some())
        {
            content = content
            .child(crate::reader_sync_help::entry())
            .child(div().text_base().child("Connect your computers"))
            .child(div().text_sm().text_color(p.text_muted).child("Pair computers once, then choose which vaults to sync. Confirm each new computer on the computer that shared the URL."))
            .child(v_flex().gap_2().child("This computer").child(Input::new(&self.name).disabled(self.busy)))
            .when(!self.receiving, |content| content.child(h_flex().gap_2()
                .child(Button::new("sync-add-computer")
                                        .debug_selector(|| "sync-add-computer".into()).label("Add computer").disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.start_pairing(false, cx))))
                .child(Button::new("sync-refresh")
                                        .debug_selector(|| "sync-refresh".into()).label("Refresh").ghost().disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.run(Operation::Read, cx))))))
            .child(v_flex().gap_2().child("Connect to an existing computer")
                .child(Input::new(&self.invitation).disabled(self.busy))
                .child(Button::new("sync-connect")
                                        .debug_selector(|| "sync-connect".into()).label("Connect").disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.start_pairing(true, cx)))))
            .when(self.receiving, |content| content.child(
                Button::new("sync-add-computer")
                                        .debug_selector(|| "sync-add-computer".into()).label("Share from this computer instead").ghost().disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| { this.receiving = false; cx.notify(); }))));
        }
        #[cfg(target_os = "linux")]
        if !self.runtimes.is_empty() {
            content = content.child(div().text_base().child("Sync connection"));
            if !self.runtimes.iter().any(|r| r.selected) {
                content = content.child(div().text_sm().child("Existing Syncthing found. Choose a connection explicitly before pairing; Okilum will keep its lifecycle unchanged."));
            }
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
                            "{}…{} · {} vaults",
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
                                .collect::<String>(),
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
        #[cfg(target_os = "linux")]
        if let Some(runtime) = self.runtimes.iter().find(|r| r.selected) {
            if let Some(network) = &runtime.network {
                content = content
                    .child(div().text_base().child("Connections on this computer"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(p.text_muted)
                            .child(network.description()),
                    );
                if runtime.managed {
                    let identity = runtime.identity.clone();
                    content = content.child(
                        v_flex().gap_2()
                            .child(h_flex().gap_2()
                                .child("Only local network and VPN")
                                .child(div().debug_selector(|| "sync-network-local-only".into()).child(
                                    gpui_component::switch::Switch::new("sync-network-local-only")
                                        .accessibility_label("Only local network and VPN")
                                        .checked(network.local_only())
                                        .disabled(self.busy)
                                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                            use okilum_sync_controller::network_policy::Policy;
                                            this.run(Operation::Network(identity.clone(), if *checked {
                                                Policy::LocalNetworkAndVpn
                                            } else { Policy::PublicDiscoveryAndRelays }), cx);
                                        })))))
                            .child(div().text_sm().text_color(p.text_muted).child(
                                "Applies to every vault using this connection on this computer. When off, public services help your computers find and reach each other. Files stay encrypted between your computers. Turn on to stop public discovery, relays and router address lookup."))
                            .child(div().text_sm().text_color(p.text_muted).child(
                                "Pair new computers on the same local network first.")));
                } else {
                    content = content.child(div().text_sm().text_color(p.text_muted).child(
                        "Your existing Syncthing controls these settings. Change them in Syncthing; Okilum leaves its network options unchanged."));
                }
            }
        }
        #[cfg(target_os = "linux")]
        {
            let mut vaults = self.vaults.clone();
            for reader in reader_open::readers(cx) {
                let reader = reader.read(cx);
                if !reader.single_file && !vaults.contains(&reader.vault_root) {
                    vaults.push(reader.vault_root.clone());
                }
            }
            content = content.child(div().text_base().child("Vaults on this computer"))
                .child(div().text_sm().text_color(p.text_muted)
                    .child("Share a vault so your approved computers can choose it. Files stay here. Later edits and deletions sync both ways."));
            if vaults.is_empty() {
                content = content.child(
                    div()
                        .text_sm()
                        .child("Open a vault to share it with your other computers."),
                );
            }
            for (index, path) in vaults.into_iter().enumerate() {
                let attached = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.attached.iter().find(|v| v.path == path));
                let shared = self
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| s.shared.contains(&path));
                let label = if shared {
                    "Shared with approved computers"
                } else if attached.is_some() {
                    "Sync connected"
                } else {
                    "Share with my computers"
                };
                content = content.child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child(path.display().to_string()))
                        .child(
                            Button::new(("sync-share-vault", index))
                                .debug_selector(move || format!("sync-share-vault-{index}"))
                                .label(label)
                                .disabled(
                                    self.busy
                                        || attached.is_some()
                                        || !self.runtimes.iter().any(|r| r.selected),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.run(Operation::Share(path.clone()), cx)
                                })),
                        ),
                );
            }
        }
        content = content.child(v_flex().gap_2()
            .child("Already syncing this vault with Syncthing?")
            .child(div().text_sm().text_color(p.text_muted)
                .child("Choose the existing local vault to inspect its connection, then explicitly allow Okilum to use it."))
            .child(Button::new("sync-inspect-existing")
                                        .debug_selector(|| "sync-inspect-existing".into()).label("Choose existing synced vault").ghost().disabled(self.busy)
                .on_click(cx.listener(|this, _, _, cx| this.choose_folder(None, cx)))));
        if let Some(vault) = &self.inspected {
            let approved = vault.clone();
            content = content.child(v_flex().gap_2()
                .child(format!("Use {} with Okilum?", vault.label))
                .child(div().text_sm().text_color(p.text_muted).child(format!("{}\nSyncthing {} · {}", vault.path.display(), vault.version, vault.daemon.device_id)))
                .child(div().text_sm().text_color(p.text_muted)
                    .child("This saves the connection and makes the vault available to computers you approve. The existing daemon keeps control of its lifecycle and configuration."))
                .child(h_flex().gap_2()
                    .child(Button::new("sync-consent-existing")
                                        .debug_selector(|| "sync-consent-existing".into()).label("Use this connection").disabled(self.busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.inspected = None;
                            this.run(Operation::Consent(approved.clone()), cx);
                        })))
                    .child(Button::new("sync-dismiss-existing")
                                        .debug_selector(|| "sync-dismiss-existing".into()).label("Cancel").ghost().disabled(self.busy)
                        .on_click(cx.listener(|this, _, _, cx| { this.inspected = None; cx.notify(); })))));
        }
        v_flex()
            .id("sync-panel")
            .debug_selector(|| "sync-panel".into())
            .size_full()
            .min_h_0()
            .gap_4()
            .child(result)
            .child(content)
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
