//! Preserve diagnostic causes without recording pairing capabilities or keys.
use super::Operation;

impl Operation {
    pub(super) fn action_name(&self) -> &'static str {
        match self {
            Self::Read => "refresh",
            Self::Retention(..) => "history_retention",
            Self::EnterPairing => "open_settings",
            Self::Begin(_) => "add_computer",
            Self::Redeem(..) => "connect",
            Self::Approve(_) => "approve",
            Self::Reject(_) => "reject",
            Self::Cancel => "cancel",
            Self::Catalog(_) => "catalog",
            Self::Inspect(_) => "inspect_vault",
            Self::Consent(_) => "attach_existing",
            Self::Attach(..) => "receive_vault",
            Self::AttachNew(..) => "receive_new_vault",
            #[cfg(target_os = "linux")]
            Self::SelectConnection(_) => "select_connection",
            #[cfg(target_os = "linux")]
            Self::Network(..) => "network_policy",
            #[cfg(target_os = "linux")]
            Self::Share(_) => "share_vault",
        }
    }
}

fn redact(text: &str) -> String {
    use std::sync::OnceLock;
    static FILTERS: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    let filters = FILTERS.get_or_init(|| {
        [
            r"(?s)-----BEGIN [^-]*PRIVATE KEY-----.*?-----END [^-]*PRIVATE KEY-----",
            r#"(?i)[a-z][a-z0-9+.-]*://[^\s<>\"']+"#,
            r#"(?i)(?:bearer\s+|(?:api[_-]?key|token|password|secret|credential|authorization)\s*[\"']?\s*[:=]\s*[\"']?)[^\s,;\"'}]+"#,
            // Standalone serialized capabilities/keys may lack a field name.
            r"[A-Za-z0-9_+/=-]{32,}",
        ]
        .into_iter()
        .map(|pattern| regex::Regex::new(pattern).expect("static Sync redaction pattern"))
        .collect()
    });
    let mut text = text.to_owned();
    for filter in filters {
        text = filter.replace_all(&text, "[redacted]").into_owned();
    }
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub(super) fn failure_details(action: &str, error: &anyhow::Error) -> serde_json::Value {
    serde_json::json!({
        "action": action,
        "error": redact(&format!("{error:#}")),
        "causes": error.chain().map(|cause| redact(&cause.to_string())).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn issue2589_full_chain_keeps_os_cause_and_redacts_urls_and_keys() {
        let error = anyhow::Error::new(std::io::Error::from_raw_os_error(13))
            .context("cannot start LAN pairing listener")
            .context("pairing failed for okilum://pair/private-token api_key=private-value");
        let details = failure_details("add_computer", &error);
        let text = details.to_string();
        assert_eq!(details["causes"].as_array().unwrap().len(), 3);
        assert!(text.contains("cannot start LAN pairing listener"));
        assert!(text.contains("Permission denied"));
        assert!(text.contains("os error 13"));
        assert!(!text.contains("private-token"));
        assert!(!text.contains("private-value"));
        assert!(!super::super::user_error(&error).contains("Permission denied"));
        assert!(!super::super::user_error(&error).contains("private-token"));
        assert_eq!(
            Operation::Begin("private-name".into()).action_name(),
            "add_computer"
        );
    }
}
