//! Display context is joined by exact strong native identity, never by ordering.
use crate::projection::Projection;
use serde_json::Value;

fn nonempty<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value[field].as_str().filter(|text| !text.trim().is_empty())
}

fn strong_reference(value: &Value) -> Option<(&str, &str)> {
    if value["strength"] != "strong" {
        return None;
    }
    Some((nonempty(value, "driver")?, nonempty(value, "nativeId")?))
}

pub(super) fn command_detail(
    projection: &Projection,
    runtime: &Value,
    approval: &Value,
) -> Option<String> {
    if runtime["kind"] != "command" || approval["type"] != "approval_request" {
        return None;
    }
    let reference = strong_reference(&approval["nativeItemRef"])?;
    if let Some(native_request) = runtime
        .get("nativeRequestRef")
        .filter(|value| !value.is_null())
    {
        if strong_reference(native_request) != Some(reference) {
            return None;
        }
    }
    for field in ["threadId", "runId", "providerThreadId", "providerTurnId"] {
        let expected = nonempty(approval, field)?;
        if runtime
            .get(field)
            .filter(|value| !value.is_null())
            .is_some_and(|value| value.as_str() != Some(expected))
        {
            return None;
        }
    }
    // Native IDs are scoped. Only the complete strong reference and exact
    // thread/run/provider-thread/provider-turn key identifies this command.
    // A second match for that full key is ambiguous regardless of its text.
    let mut commands = projection.items.values().filter(|item| {
        item["type"] == "command_execution"
            && strong_reference(&item["nativeItemRef"]) == Some(reference)
            && ["threadId", "runId", "providerThreadId", "providerTurnId"]
                .iter()
                .all(|field| nonempty(item, field) == nonempty(approval, field))
    });
    let command = commands.next()?;
    if commands.next().is_some() {
        return None;
    }
    nonempty(command, "input").map(str::to_owned)
}

#[cfg(test)]
mod tests;
