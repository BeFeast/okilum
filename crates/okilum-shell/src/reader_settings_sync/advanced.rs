use super::*;
impl SyncSettings {
    pub(super) fn render_advanced(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = brand::palette(cx);
        let mut content = v_flex().gap_4();
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
                    .child("This shares the existing vault with your approved computers. Your existing Syncthing keeps its folders, peers and settings. Next, choose Add computer here, then paste its URL on the computer receiving these notes."))
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
        #[cfg(target_os = "linux")]
        {
            let mut roots: Vec<_> = self
                .snapshot
                .as_ref()
                .map(|s| s.attached.iter().map(|v| v.path.clone()).collect())
                .unwrap_or_default();
            let readers = reader_open::readers(cx);
            roots.extend(
                readers
                    .iter()
                    .map(|r| r.read(cx).vault_root.clone())
                    .filter(|r| !r.as_os_str().is_empty()),
            );
            roots.sort();
            roots.dedup();
            content = content.child(div().text_base().child("Conflicts"));
            for root in roots {
                if !self.conflicts.contains_key(&root) {
                    let owner = readers.iter().find(|r| r.read(cx).vault_root == root);
                    let state = owner
                        .and_then(|r| r.read(cx).session_directory.clone())
                        .map(Ok)
                        .unwrap_or_else(|| reader_ui_state::state_directory(cx));
                    if let Ok(state) = state {
                        let owner = owner.map(Entity::downgrade);
                        let view = cx.new(|cx| {
                            crate::reader_sync_conflicts::InventoryView::new(
                                root.clone(),
                                state,
                                owner,
                                cx,
                            )
                        });
                        self.conflicts.insert(root.clone(), view);
                    }
                }
                if let Some(view) = self.conflicts.get(&root) {
                    content = content.child(view.clone());
                }
            }
        }
        content.into_any_element()
    }
    #[cfg(target_os = "linux")]
    pub(super) fn render_local_vaults(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = brand::palette(cx);
        let mut content = v_flex().gap_2();
        #[cfg(target_os = "linux")]
        {
            let mut vaults = self.vaults.clone();
            for reader in reader_open::readers(cx) {
                let reader = reader.read(cx);
                if !reader.single_file && !vaults.contains(&reader.vault_root) {
                    vaults.push(reader.vault_root.clone());
                }
            }
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
                if attached.is_some() {
                    continue;
                }
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
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(destination_name(
                                    path.file_name().and_then(|n| n.to_str()).unwrap_or("Vault"),
                                )),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(p.text_muted)
                                .child(path.display().to_string()),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(p.text_muted)
                                .child("Only on this computer"),
                        )
                        .child(
                            Button::new(("sync-share-vault", index))
                                .debug_selector(move || format!("sync-share-vault-{index}"))
                                .label(label)
                                .disabled(self.busy || attached.is_some())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.run(Operation::Share(path.clone()), cx)
                                })),
                        ),
                );
            }
        }
        content.into_any_element()
    }
}
