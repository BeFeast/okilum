//! UI state is separate from the controller's durable pairing journal.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    Start,
    Paste,
    Link,
    Waiting,
    Vaults,
    Progress,
    Expired,
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
    if snapshot.preparing {
        return "Your first sync is in progress".into();
    }
    if snapshot.status.starts_with("Not connected") {
        return "A computer is offline. Open Okilum there to reconnect.".into();
    }
    if snapshot.status.contains("held") {
        return "A vault needs review before Sync can continue".into();
    }
    if snapshot.peers.is_empty() {
        return "Connect your first computer to start Sync".into();
    }
    "Your computers are paired. Vault status is shown below.".into()
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
    #[cfg(not(test))]
    {
        use std::process::{Command, Stdio};
        let title = format!("{name} wants to connect");
        let body =
            "Open Okilum to approve or reject this computer. No files are shared before approval.";
        #[cfg(target_os = "linux")]
        let result = Command::new("notify-send")
            .args(["--app-name=Okilum", &title, body])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        #[cfg(target_os = "macos")]
        let result = Command::new("osascript").args(["-e", "on run argv\n display notification (item 2 of argv) with title (item 1 of argv)\nend run", &title, body])
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
        if let Err(error) = result {
            eprintln!("okilum-sync: approval notification failed: {error}");
        }
    }
    #[cfg(test)]
    let _ = name;
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn raw_controller_ready_is_never_the_page_status() {
        let snapshot = Snapshot {
            offer_url: None,
            pending: None,
            peers: vec![],
            catalog: vec![],
            attached: vec![],
            #[cfg(target_os = "linux")]
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
}

/// Shared approval banner, visible even when Settings is on another section.
pub(crate) fn approval_banner(cx: &mut App) -> AnyElement {
    let view = cx.try_global::<BackgroundSync>().map(|s| s.0.clone());
    view.map(|view| view.update(cx, |view, cx| view.render_approval(cx)))
        .unwrap_or_else(|| div().into_any_element())
}
