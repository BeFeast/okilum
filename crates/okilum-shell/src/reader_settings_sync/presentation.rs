//! UI state is separate from the controller's durable pairing journal.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    Start,
    Paste,
    Link,
    Waiting,
    Vaults,
    Computer,
    Progress,
    Expired,
    Existing,
    Merge,
}

#[derive(Default)]
pub(super) struct Presentation {
    pub wizard: Option<Step>,
    pub advanced: bool,
    pub details: bool,
    pub error_details: Option<String>,
    pub retry: Option<Operation>,
    pub notified: Option<uuid::Uuid>,
    pub overview: direct::Overview,
    pub managing: Option<PathBuf>,
    pub picking_vault: bool,
    pub destinations: std::collections::BTreeMap<String, Operation>,
    pub transfers: std::collections::VecDeque<Operation>,
    pub dialog_open: bool,
}

impl Presentation {
    pub fn first_sync_complete(&self, snapshot: Option<&Snapshot>, busy: bool) -> bool {
        !busy
            && self.transfers.is_empty()
            && snapshot.is_some_and(|snapshot| {
                !snapshot.preparing
                    && !snapshot.attached.is_empty()
                    && snapshot.attached.iter().all(|vault| {
                        self.overview.vaults.get(&vault.path).is_some_and(|state| {
                            state.available
                                && state.state == "Up to date"
                                && state.needed_bytes == 0
                        })
                    })
            })
    }
}

pub(super) struct Wizard {
    pub sync: Entity<SyncSettings>,
    pub _changes: Subscription,
}
impl Render for Wizard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sync = self.sync.clone();
        if sync.read(cx).presentation.wizard.is_none()
            && sync.read(cx).presentation.managing.is_none()
        {
            use gpui_component::WindowExt;
            window.close_dialog(cx);
            return div().into_any_element();
        }
        let max_height = (window.viewport_size().height - px(120.)).max(px(160.));
        sync.update(cx, |view, cx| {
            let body = if view.presentation.managing.is_some() {
                view.render_manage(cx)
            } else {
                view.render_wizard(cx)
            };
            v_flex()
                .id("sync-modal")
                .debug_selector(|| "sync-modal".into())
                .w_full()
                .gap_3()
                .child(
                    h_flex()
                        .justify_between()
                        .child(
                            div()
                                .text_sm()
                                .text_color(brand::palette(cx).text_muted)
                                .child(format!("This computer: {}", input_text(&view.name, cx))),
                        )
                        .child(
                            Button::new("sync-close-wizard")
                                .debug_selector(|| "sync-close-wizard".into())
                                .ghost()
                                .label("Close")
                                .on_click(cx.listener(|view, _, window, cx| {
                                    use gpui_component::WindowExt;
                                    view.presentation.wizard = None;
                                    view.presentation.managing = None;
                                    view.presentation.dialog_open = false;
                                    view.close_first_sync_preview(cx);
                                    window.close_dialog(cx);
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    v_flex()
                        .id("sync-wizard-scroll")
                        .w_full()
                        .max_h(max_height)
                        .overflow_y_scroll()
                        .child(body),
                )
        })
        .into_any_element()
    }
}

pub(super) fn summary(snapshot: Option<&Snapshot>, error: Option<&str>) -> String {
    if let Some(error) = error {
        return error.into();
    }
    let Some(snapshot) = snapshot else {
        return "Setting up Sync on this computer…".into();
    };
    if snapshot.pending.as_ref().is_some_and(|p| p.incoming) {
        return "A computer is waiting for your approval".into();
    }
    if snapshot.pending.is_some() {
        return "Waiting for your other computer to approve".into();
    }
    if snapshot.offer_url.is_some() {
        return "Your pairing link is ready to copy".into();
    }
    if snapshot.status.starts_with("Not connected") {
        return "A computer is offline. Open Okilum there to reconnect.".into();
    }
    if snapshot.status.contains("held") {
        return "A vault needs review before Sync can continue".into();
    }
    if snapshot.preparing {
        return "Your first sync is in progress".into();
    }
    if snapshot.peers.is_empty() {
        return "Connect your first computer to start Sync".into();
    }
    "Your computers are paired. Vault status is shown below.".into()
}

/// The pairing protocol carries no OS. Only show metadata actually observed.
pub(super) fn computer_details(vaults: Option<usize>) -> Option<String> {
    vaults.map(vault_count)
}

pub(super) fn vault_count(count: usize) -> String {
    format!("{count} {}", if count == 1 { "vault" } else { "vaults" })
}

pub(super) fn last_seen(value: &str) -> String {
    let Ok(seen) = chrono::DateTime::parse_from_rfc3339(value) else {
        return "Last seen unavailable".into();
    };
    let elapsed = chrono::Utc::now().signed_duration_since(seen);
    if elapsed.num_minutes() < 1 {
        "Last seen just now".into()
    } else if elapsed.num_hours() < 1 {
        format!("Last seen {} min ago", elapsed.num_minutes())
    } else if elapsed.num_days() < 1 {
        format!("Last seen {} h ago", elapsed.num_hours())
    } else {
        format!("Last seen {} days ago", elapsed.num_days())
    }
}

pub(super) fn notify_approval(name: &str) {
    #[cfg(all(not(test), unix))]
    {
        use std::process::{Command, Stdio};
        let title = format!("{name} wants to connect");
        let body =
            "Open Okilum to approve or reject this computer. No files are shared before approval.";
        #[cfg(target_os = "linux")]
        let result = Command::new("notify-send")
            .args(["--app-name=Okilum", "--", &title, body])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        #[cfg(target_os = "macos")]
        let result = Command::new("osascript").args(["-e", "on run argv\n display notification (item 2 of argv) with title (item 1 of argv)\nend run", &title, body])
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
        match result {
            Err(error) => eprintln!("okilum-sync: approval notification failed: {error}"),
            Ok(status) if !status.success() => {
                eprintln!("okilum-sync: approval notification failed: {status}")
            }
            _ => {}
        }
    }
    #[cfg(any(test, windows))]
    let _ = name;
}

/// Shared approval banner, visible even when Settings is on another section.
pub(crate) fn approval_banner(cx: &mut App) -> AnyElement {
    let view = cx.try_global::<BackgroundSync>().map(|s| s.0.clone());
    view.map(|view| view.update(cx, |view, cx| view.render_approval(cx)))
        .unwrap_or_else(|| div().into_any_element())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn vault_counts_use_singular_only_for_one() {
        assert_eq!(vault_count(0), "0 vaults");
        assert_eq!(vault_count(1), "1 vault");
        assert_eq!(vault_count(2), "2 vaults");
        assert_eq!(vault_count(11), "11 vaults");
    }

    #[test]
    fn computer_details_hide_unknown_os_and_unobserved_counts() {
        assert_eq!(computer_details(None), None);
        assert_eq!(computer_details(Some(1)).as_deref(), Some("1 vault"));
        assert_eq!(computer_details(Some(3)).as_deref(), Some("3 vaults"));
    }

    #[test]
    fn raw_controller_ready_is_never_the_page_status() {
        let snapshot = Snapshot {
            computers: vec![],
            pending_removals: 0,
            policy_warnings: vec![],
            history_warnings: vec![],
            history_retention: vec![],
            offer_url: None,
            pending: None,
            peers: vec![],
            catalog: vec![],
            attached: vec![],
            shared: vec![],
            status: "Ready".into(),
            preparing: false,
        };
        assert_eq!(
            summary(Some(&snapshot), None),
            "Connect your first computer to start Sync"
        );
        assert_eq!(
            summary(Some(&snapshot), Some("Check the other computer")),
            "Check the other computer"
        );
        assert!(summary(None, None).contains("Setting up"));
    }

    #[test]
    fn advanced_and_wizard_start_closed() {
        let presentation = Presentation::default();
        assert!(!presentation.advanced);
        assert!(!presentation.details);
        assert!(presentation.wizard.is_none());
    }

    #[test]
    fn first_sync_requires_observed_completion_for_every_vault() {
        let mut snapshot = Snapshot {
            computers: vec![],
            pending_removals: 0,
            policy_warnings: vec![],
            history_warnings: vec![],
            history_retention: vec![],
            offer_url: None,
            pending: None,
            peers: vec![],
            catalog: vec![],
            attached: vec![direct::ExistingVault {
                daemon: okilum_sync_controller::daemon::DaemonIdentity {
                    config_file: "fixture.xml".into(),
                    device_id: "local-device".into(),
                    certificate_hash: "fixture".into(),
                },
                version: "v2.1.7".into(),
                folder_id: "notes".into(),
                path: "Notes".into(),
                label: "Notes".into(),
                paused: false,
                folder_type: "sendreceive".into(),
                peers: vec![],
                root_identity: None,
                #[cfg(any(target_os = "linux", target_os = "macos", windows))]
                stable_root_identity: None,
            }],
            shared: vec![],
            status: "Ready".into(),
            preparing: false,
        };
        let mut presentation = Presentation::default();
        assert!(!presentation.first_sync_complete(None, false));
        assert!(!presentation.first_sync_complete(Some(&snapshot), false));
        let path = snapshot.attached[0].path.clone();
        presentation.overview.vaults.insert(
            path.clone(),
            direct::VaultOverview {
                available: true,
                state: "Syncing".into(),
                needed_bytes: 100,
                ..Default::default()
            },
        );
        assert!(!presentation.first_sync_complete(Some(&snapshot), false));
        presentation.overview.vaults.get_mut(&path).unwrap().state = "Up to date".into();
        assert!(!presentation.first_sync_complete(Some(&snapshot), false));
        presentation
            .overview
            .vaults
            .get_mut(&path)
            .unwrap()
            .needed_bytes = 0;
        assert!(presentation.first_sync_complete(Some(&snapshot), false));
        assert!(!presentation.first_sync_complete(Some(&snapshot), true));
        snapshot.preparing = true;
        assert!(!presentation.first_sync_complete(Some(&snapshot), false));
        snapshot.preparing = false;
        presentation
            .overview
            .vaults
            .get_mut(&path)
            .unwrap()
            .available = false;
        assert!(!presentation.first_sync_complete(Some(&snapshot), false));
        presentation
            .overview
            .vaults
            .get_mut(&path)
            .unwrap()
            .available = true;
        let mut second = snapshot.attached[0].clone();
        second.path = "Other notes".into();
        snapshot.attached.push(second);
        assert!(!presentation.first_sync_complete(Some(&snapshot), false));
    }

    #[test]
    fn attention_takes_priority_over_an_unfinished_first_transfer() {
        let mut snapshot = Snapshot {
            computers: vec![],
            pending_removals: 0,
            policy_warnings: vec![],
            history_warnings: vec![],
            history_retention: vec![],
            offer_url: None,
            pending: None,
            peers: vec![],
            catalog: vec![],
            attached: vec![],
            shared: vec![],
            status: "Not connected to QA laptop".into(),
            preparing: true,
        };
        assert!(summary(Some(&snapshot), None).contains("offline"));
        snapshot.status = "Initial receive held: ownership witness unavailable".into();
        assert!(summary(Some(&snapshot), None).contains("needs review"));
        snapshot.status = "Receiving vault; checking safe bidirectional readiness".into();
        assert!(summary(Some(&snapshot), None).contains("in progress"));
    }
}
