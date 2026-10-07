//! Settings vocabulary derived from explicit controller evidence. Rendering this
//! model never contacts a service or starts a daemon.
use crate::folder::LocalStatus;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderState {
    Preparing,
    Checking,
    Syncing,
    CaughtUp,
    Paused,
    Offline,
    NeedsAttention,
    Removing,
    Removed,
}
impl FolderState {
    pub fn from_local(local: &LocalStatus) -> Self {
        if local.removed {
            return Self::Removed;
        }
        if local.removal_pending {
            return Self::Removing;
        }
        if local.folder["paused"] == true {
            return Self::Paused;
        }
        let status = &local.status;
        if ["errors", "pullErrors"]
            .iter()
            .any(|field| status[field].as_u64().is_some_and(|n| n > 0))
            || ["error", "invalid", "watchError"]
                .iter()
                .any(|field| status[field].as_str().is_some_and(|s| !s.is_empty()))
            || local.errors["errors"]
                .as_array()
                .is_some_and(|errors| !errors.is_empty())
        {
            return Self::NeedsAttention;
        }
        // Local idle/zero need is not a first-receive completion receipt.
        if local.preparing {
            return Self::Preparing;
        }
        if !local.hub_connected {
            return Self::Offline;
        }
        if status["receiveOnlyTotalItems"]
            .as_u64()
            .is_some_and(|n| n > 0)
        {
            return Self::NeedsAttention;
        }
        if status["needTotalItems"].as_u64().is_some_and(|n| n > 0) || status["state"] == "syncing"
        {
            return Self::Syncing;
        }
        if status["state"] == "idle"
            && status["needTotalItems"] == 0
            && status["receiveOnlyTotalItems"] == 0
            && status["errors"] == 0
            && status["pullErrors"] == 0
            && ["error", "invalid", "watchError"]
                .iter()
                .all(|field| status[field] == "")
            // Syncthing 2.1.6 returns an explicit null for an empty error list.
            // An absent response/key remains unknown (e.g. a paused runner).
            && local.errors.get("errors").is_some_and(|errors| {
                errors.is_null() || errors.as_array().is_some_and(|items| items.is_empty())
            })
        {
            Self::CaughtUp
        } else {
            Self::Checking
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Preparing => "Preparing",
            Self::Checking => "Checking files",
            Self::Syncing => "Syncing",
            Self::CaughtUp => "Up to date with the hub",
            Self::Paused => "Paused",
            Self::Offline => "Hub offline",
            Self::NeedsAttention => "Needs attention",
            Self::Removing => "Removal pending",
            Self::Removed => "Removed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn local() -> LocalStatus {
        LocalStatus {
            reused: false,
            removed: false,
            removal_pending: false,
            preparing: true,
            external_sync_retained: false,
            folder: json!({"paused":false}),
            status: json!({"state":"idle", "needTotalItems":0, "receiveOnlyTotalItems":0,
                "errors":0, "pullErrors":0, "error":"", "invalid":"", "watchError":""}),
            errors: json!({"errors":[]}),
            hub_connected: true,
            last_connected_at: None,
        }
    }
    #[test]
    fn first_receive_requires_durable_promotion_not_local_idle() {
        let mut local = local();
        assert_eq!(FolderState::from_local(&local), FolderState::Preparing);
        local.preparing = false;
        assert_eq!(FolderState::from_local(&local), FolderState::CaughtUp);
        local.hub_connected = false;
        assert_eq!(FolderState::from_local(&local), FolderState::Offline);
        local.removal_pending = true;
        assert_eq!(FolderState::from_local(&local), FolderState::Removing);
        local.removed = true;
        assert_eq!(FolderState::from_local(&local), FolderState::Removed);
    }
    #[test]
    fn missing_observations_and_local_edits_never_report_caught_up() {
        let mut local = local();
        local.preparing = false;
        local.errors = serde_json::Value::Null;
        assert_eq!(FolderState::from_local(&local), FolderState::Checking);
        local.errors = json!({"errors":null});
        assert_eq!(FolderState::from_local(&local), FolderState::CaughtUp);
        local.status["receiveOnlyTotalItems"] = json!(1);
        assert_eq!(FolderState::from_local(&local), FolderState::NeedsAttention);
        local.folder["paused"] = json!(true);
        assert_eq!(FolderState::from_local(&local), FolderState::Paused);
    }
}
