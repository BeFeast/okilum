//! Compact Tier 1 presentation; commands/files use upstream work-log's
//! input/path/status summaries. Full details and unsupported controls open T3.
use serde_json::Value;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    Message { user: bool, text: String },
    Reasoning(String),
    Compact { label: String, detail: String },
}
fn text(v: &Value, field: &str) -> String {
    v[field].as_str().unwrap_or("").to_owned()
}
pub fn body(item: &Value) -> Body {
    let kind = item["type"].as_str().unwrap_or("");
    if matches!(kind, "user_message" | "assistant_message") {
        return Body::Message {
            user: kind == "user_message",
            text: text(item, "text"),
        };
    }
    if kind == "reasoning" {
        return Body::Reasoning(text(item, "text"));
    }
    let (label, detail) = match kind {
        "command_execution" => ("Command", text(item, "input")),
        "file_change" => (
            "File changed",
            format!(
                "{} · +{} −{}",
                text(item, "fileName"),
                item["additions"].as_u64().unwrap_or(0),
                item["deletions"].as_u64().unwrap_or(0)
            ),
        ),
        "file_search" => ("File search", text(item, "pattern")),
        "web_search" => (
            "Web search",
            item["patterns"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default(),
        ),
        "approval_request" => (
            "Approval in T3",
            item["prompt"]
                .as_str()
                .or(item["reason"].as_str())
                .unwrap_or("")
                .into(),
        ),
        "user_input_request" => (
            "Question in T3",
            item["questions"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|v| v["question"].as_str())
                .unwrap_or("")
                .into(),
        ),
        "proposed_plan" => ("Plan in T3", text(item, "markdown")),
        "todo_list" => (
            "Plan",
            format!("{} steps", item["steps"].as_array().map_or(0, Vec::len)),
        ),
        "error" => ("Error", text(&item["failure"], "message")),
        "secret_request" => ("Secret requested in T3", text(item, "label")),
        "checkpoint" => ("Checkpoint in T3", text(item, "title")),
        "compaction" => ("Context compacted", text(item, "summary")),
        "handoff" => ("Context handoff", text(item, "summary")),
        "subagent" => ("Subagent", text(item, "progress")),
        "dynamic_tool" => ("Tool", text(item, "toolName")),
        "fork" | "thread_created" => ("Thread in T3", text(item, "targetThreadId")),
        _ => (
            "Activity",
            item["message"]
                .as_str()
                .or(item["title"].as_str())
                .unwrap_or("")
                .into(),
        ),
    };
    // Keep tool cards compact, including commands with large stdin/scripts.
    let detail = detail
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(240)
        .collect::<String>();
    let status = item["status"].as_str().unwrap_or("");
    Body::Compact {
        label: if status.is_empty() {
            label.into()
        } else {
            format!("{label} · {status}")
        },
        detail,
    }
}
