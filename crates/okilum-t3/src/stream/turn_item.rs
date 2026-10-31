//! On-demand full detail for the source identity of a projected activity row.
//!
//! Add `pub mod turn_item` to `stream` and expose the sibling Manager RPC helper
//! as `pub(super)` when integrating. Requests use `RowKey`'s source thread/item,
//! including inherited rows; the currently selected thread must not replace it.
//! Unknown/unsupported RPC and transport failures propagate without fallback.

use super::{NativeClient, StreamError, StreamResult};
use crate::projection::RowKey;
use serde::Deserialize;
use serde_json::{json, Value};

const METHOD: &str = "orchestration.getTurnItem";

impl NativeClient {
    /// One explicit read through the existing bounded RPC connection. Run off
    /// the UI thread. `None` means the server explicitly returned `item: null`;
    /// a missing, malformed, or foreign item is an error instead.
    pub fn turn_item(&self, key: &RowKey, revision: Option<&str>) -> StreamResult<Option<Value>> {
        let response = self.manager_rpc(METHOD, payload(key, revision)?, false)?;
        decode(response, key)
    }
}

/// Match upstream `turnItemDetailRevision`: live rows fetch under one stable
/// revision, then fetch again under `updatedAt` when they become terminal.
/// A terminal row without its timestamp is explicitly invalid, not `live`.
pub fn revision(item: &Value) -> StreamResult<&str> {
    let status = item["status"]
        .as_str()
        .filter(|status| !status.is_empty())
        .ok_or(StreamError::Protocol("missing turn item detail status"))?;
    if matches!(status, "idle" | "pending" | "running" | "waiting") {
        return Ok("live");
    }
    item["updatedAt"]
        .as_str()
        .filter(|updated_at| !updated_at.trim().is_empty())
        .ok_or(StreamError::Protocol("missing turn item detail revision"))
}

fn payload(key: &RowKey, revision: Option<&str>) -> StreamResult<Value> {
    if key.thread.trim().is_empty() || key.item.trim().is_empty() {
        return Err(StreamError::Protocol("invalid turn item source identity"));
    }
    let mut payload = json!({"threadId": key.thread, "itemId": key.item});
    if let Some(revision) = revision {
        payload["revision"] = Value::String(revision.into());
    }
    Ok(payload)
}

#[derive(Deserialize)]
struct Response {
    // Value is deliberately required, unlike Option<Value>, whose Deserialize
    // implementation would quietly turn an absent field into None.
    item: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceIdentity {
    thread_id: String,
    id: String,
}

fn decode(response: Value, key: &RowKey) -> StreamResult<Option<Value>> {
    if !response.is_object() {
        return Err(StreamError::Protocol("invalid turn item detail response"));
    }
    let response: Response = serde_json::from_value(response)
        .map_err(|_| StreamError::Protocol("missing turn item detail response"))?;
    if response.item.is_null() {
        return Ok(None);
    }
    if !response.item.is_object() {
        return Err(StreamError::Protocol("invalid turn item detail"));
    }
    let source = SourceIdentity::deserialize(&response.item)
        .map_err(|_| StreamError::Protocol("invalid turn item detail source identity"))?;
    if source.thread_id != key.thread || source.id != key.item {
        return Err(StreamError::Protocol(
            "turn item detail belongs to another source item",
        ));
    }
    Ok(Some(response.item))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inherited_source() -> RowKey {
        RowKey {
            thread: "original-thread".into(),
            item: "original-command".into(),
        }
    }

    #[test]
    fn inherited_row_requests_its_source_identity_and_optional_revision() {
        let key = inherited_source();
        assert_eq!(METHOD, "orchestration.getTurnItem");
        assert_eq!(
            payload(&key, Some("live")).unwrap(),
            json!({"threadId": "original-thread", "itemId": "original-command", "revision": "live"})
        );
        assert_eq!(
            payload(&key, None).unwrap(),
            json!({"threadId": "original-thread", "itemId": "original-command"})
        );
        assert!(payload(
            &RowKey {
                thread: String::new(),
                item: "item".into()
            },
            None
        )
        .is_err());
    }

    #[test]
    fn decoded_detail_preserves_the_full_item_and_rejects_foreign_sources() {
        let key = inherited_source();
        let item = json!({
            "threadId": key.thread, "id": key.item, "type": "command_execution",
            "output": format!("{}\nDETAIL_FINAL", "line\n".repeat(500))
        });
        assert_eq!(
            decode(json!({"item": item}), &key).unwrap(),
            Some(item.clone())
        );
        for (thread, id) in [
            ("selected-child-thread", "original-command"),
            ("original-thread", "other-command"),
        ] {
            let mut foreign = item.clone();
            foreign["threadId"] = json!(thread);
            foreign["id"] = json!(id);
            assert!(matches!(
                decode(json!({"item": foreign}), &key),
                Err(StreamError::Protocol(
                    "turn item detail belongs to another source item"
                ))
            ));
        }
    }

    #[test]
    fn only_explicit_null_means_unavailable_detail() {
        let key = inherited_source();
        assert_eq!(decode(json!({"item": null}), &key).unwrap(), None);
        for malformed in [
            json!({}),
            json!(null),
            json!([null]),
            json!({"item": "not an item"}),
            json!({"item": []}),
            json!({"item": {"threadId": key.thread}}),
            json!({"item": {"threadId": key.thread, "id": 7}}),
        ] {
            assert!(decode(malformed, &key).is_err());
        }
    }

    #[test]
    fn live_revision_is_stable_until_a_terminal_timestamp_is_available() {
        for status in ["idle", "pending", "running", "waiting"] {
            assert_eq!(revision(&json!({"status": status})).unwrap(), "live");
        }
        for status in ["completed", "failed", "cancelled", "interrupted"] {
            let item = json!({"status": status, "updatedAt": "2026-10-31T12:34:56.000Z"});
            assert_eq!(revision(&item).unwrap(), "2026-10-31T12:34:56.000Z");
        }
        assert!(revision(&json!({"status": "completed"})).is_err());
        assert!(revision(&json!({"updatedAt": "2026-10-31T12:34:56.000Z"})).is_err());
    }
}
