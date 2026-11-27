//! Electron's Linux reveal can navigate without transferring Wayland focus.
//! Sway allows the initiating application to focus a known window over IPC.
//! Keep URI dispatch with the registered handler; only supplement its reveal.
use serde_json::Value;
#[cfg(not(test))]
use std::process::Command;

const T3_APP_ID: &str = "com.t3tools.T3Code";

/// Never choose between multiple T3 windows/profiles or match arbitrary titles.
fn single_t3_window(tree: &Value) -> Option<u64> {
    fn collect(node: &Value, windows: &mut Vec<Option<u64>>) {
        if node["app_id"] == T3_APP_ID {
            windows.push(node["id"].as_u64().filter(|id| *id > 0));
        }
        for branch in ["nodes", "floating_nodes"] {
            if let Some(children) = node[branch].as_array() {
                for child in children {
                    collect(child, windows);
                }
            }
        }
    }
    let mut windows = Vec::new();
    collect(tree, &mut windows);
    (windows.len() == 1).then(|| windows[0]).flatten()
}

/// Best effort: other desktops keep their normal URI-handler activation path.
/// Both IPC commands have a one-second bound and run off the GPUI thread.
#[cfg(not(test))]
pub(super) fn focus_existing_window() {
    if std::env::var_os("SWAYSOCK").is_none() {
        return;
    }
    let Ok(output) = Command::new("timeout")
        .args(["1", "swaymsg", "-r", "-t", "get_tree"])
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let Some(id) = serde_json::from_slice::<Value>(&output.stdout)
        .ok()
        .and_then(|tree| single_t3_window(&tree))
    else {
        return;
    };
    let _ = Command::new("timeout")
        .args(["1", "swaymsg", &format!("[con_id={id}] focus")])
        .output();
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use serde_json::json;

    #[test]
    fn focuses_only_one_exact_t3_application_including_floating_windows() {
        let t3 = json!({"id":42,"app_id":T3_APP_ID,"pid":123});
        let other = json!({"id":43,"app_id":"foot","name":"T3 Code (Nightly)"});
        let tree = json!({"nodes":[{"nodes":[other],"floating_nodes":[t3]}]});
        assert_eq!(single_t3_window(&tree), Some(42));
        assert_eq!(single_t3_window(&json!({"nodes":[other]})), None);
        assert_eq!(
            single_t3_window(&json!({"nodes":[t3,{"id":44,"app_id":T3_APP_ID}]})),
            None,
            "multiple desktop profiles/windows are ambiguous"
        );
        assert_eq!(
            single_t3_window(&json!({"nodes":[t3,{"app_id":T3_APP_ID}]})),
            None,
            "a malformed second T3 window is still ambiguous"
        );
        assert_eq!(single_t3_window(&json!({"app_id":T3_APP_ID})), None);
        assert_eq!(single_t3_window(&json!({"id":0,"app_id":T3_APP_ID})), None);
        assert_eq!(single_t3_window(&Value::Null), None);
    }
}
