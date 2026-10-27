use serde_json::Value;

use super::{StreamError, StreamResult};

/// Wire-level thread updates, intentionally without a UI projection. Unknown
/// domain events retain their type/payload and advance the cursor after commit;
/// the reducer can skip them just as T3's forward-compatible client does.
#[derive(Clone, Debug, PartialEq)]
pub enum ThreadUpdate {
    Snapshot {
        sequence: u64,
        projection: Value,
        history_cursor: Option<String>,
        has_more_history: bool,
        payload_budget_exceeded: bool,
    },
    Event {
        sequence: u64,
        event: Value,
    },
    Synchronized,
}

fn sequence(value: &Value) -> StreamResult<u64> {
    value
        .as_u64()
        .filter(|value| *value <= 9_007_199_254_740_991)
        .ok_or(StreamError::Protocol("invalid stream sequence"))
}

pub(super) fn decode_chunk(
    value: &Value,
    thread_id: &str,
    mut cursor: Option<u64>,
) -> StreamResult<(Vec<ThreadUpdate>, Option<u64>)> {
    let values = value["values"]
        .as_array()
        .filter(|items| !items.is_empty())
        .ok_or(StreamError::Protocol("invalid chunk"))?;
    let mut updates = Vec::with_capacity(values.len());
    for item in values {
        let update = match item["kind"].as_str() {
            Some("snapshot") => {
                let seq = sequence(&item["snapshotSequence"])?;
                let projection = &item["projection"];
                if projection["thread"]["id"] != thread_id {
                    return Err(StreamError::Protocol("snapshot belongs to another thread"));
                }
                // We did not opt into compact snapshots, which require a
                // separate reconstruction step before applying a projection.
                if item["turnItemsOmitLocalVisible"] == true {
                    return Err(StreamError::Protocol("unrequested compact snapshot"));
                }
                let history_cursor = match item.get("historyCursor") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(cursor)) if !cursor.is_empty() => Some(cursor.clone()),
                    _ => return Err(StreamError::Protocol("invalid history cursor")),
                };
                let flag = |name| match item.get(name) {
                    None => Ok(false),
                    Some(Value::Bool(value)) => Ok(*value),
                    _ => Err(StreamError::Protocol("invalid snapshot flag")),
                };
                // A fresh server snapshot replaces the retained state even
                // if a server reset moved its sequence backwards.
                cursor = Some(seq);
                ThreadUpdate::Snapshot {
                    sequence: seq,
                    projection: projection.clone(),
                    history_cursor,
                    has_more_history: flag("hasMoreHistory")?,
                    payload_budget_exceeded: flag("payloadBudgetExceeded")?,
                }
            }
            Some("event") => {
                let seq = sequence(&item["sequence"])?;
                if cursor.is_none() {
                    return Err(StreamError::Protocol("event before initial snapshot"));
                }
                let event = &item["event"];
                if !event["type"].is_string() {
                    return Err(StreamError::Protocol("invalid domain event"));
                }
                if cursor.is_some_and(|previous| seq <= previous) {
                    continue;
                }
                cursor = Some(seq);
                ThreadUpdate::Event {
                    sequence: seq,
                    event: event.clone(),
                }
            }
            Some("synchronized") if cursor.is_some() => ThreadUpdate::Synchronized,
            _ => return Err(StreamError::Protocol("unknown stream item")),
        };
        updates.push(update);
    }
    Ok((updates, cursor))
}
