//! Full available detail for native activity rows, independent of UI disclosure.
//!
//! The presentation adapter should use `detail(item).markdown` for the expanded
//! body and retain its existing input/path/tool summary for the collapsed preview.
//! Do not apply the old first-line/240-character limit to this Markdown. Preserve
//! `needs_remote_detail` separately: an omitted output or summarized input needs a
//! source-item detail fetch before the UI can claim to show the complete detail.
//! Attachments remain sibling records; this module never creates attachment links.

use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Content {
    pub markdown: String,
    pub needs_remote_detail: bool,
}

/// Cheap disclosure metadata; do not build a second copy of large tool output
/// merely to decide whether an on-demand source read is needed.
pub fn needs_remote_detail(item: &Value) -> bool {
    match item["type"].as_str() {
        Some("command_execution") => item["outputOmitted"] == true,
        Some("dynamic_tool") => {
            item["outputOmitted"] == true
                || ["input", "output"]
                    .iter()
                    .any(|field| item.get(*field).is_some_and(summarized))
        }
        _ => false,
    }
}

/// Extracts supported activity details. `None` leaves other presentation kinds
/// (including ordinary chat and reasoning) to the existing adapter.
pub fn detail(item: &Value) -> Option<Content> {
    let mut markdown = String::new();
    let mut needs_remote_detail = false;
    match item["type"].as_str()? {
        "proposed_plan" => markdown.push_str(string(item, "markdown").unwrap_or("")),
        "todo_list" => {
            if let Some(explanation) = string(item, "explanation") {
                markdown.push_str(explanation);
            }
            if let Some(steps) = item["steps"].as_array() {
                for (index, step) in steps.iter().enumerate() {
                    let status = string(step, "status").unwrap_or("unknown");
                    heading(&mut markdown, &format!("Step {} · {status}", index + 1));
                    if let Some(text) = string(step, "text") {
                        markdown.push_str(text);
                    } else {
                        // Retain malformed/forward-compatible step data visibly.
                        markdown.push_str(&fenced("json", &json(step)));
                    }
                }
            }
        }
        "command_execution" => {
            field(&mut markdown, item, "input", "Input", "sh");
            if let Some(output) = string(item, "output") {
                code(&mut markdown, "Output", "text", output);
            } else if item["outputOmitted"] != true {
                paragraph(&mut markdown, "Output is not included in this item.");
            }
            if item["outputOmitted"] == true {
                needs_remote_detail = true;
                paragraph(
                    &mut markdown,
                    "Output was omitted from the thread projection; full detail must be loaded.",
                );
            }
            if let Some(exit_code) = item["exitCode"].as_i64() {
                paragraph(&mut markdown, &format!("Exit code: {exit_code}"));
            }
            if item["outputIndicatesFailure"] == true {
                paragraph(&mut markdown, "Output indicates failure.");
            }
        }
        "file_change" => {
            field(&mut markdown, item, "fileName", "File", "text");
            for (key, label) in [("additions", "Additions"), ("deletions", "Deletions")] {
                if let Some(count) = item[key].as_u64() {
                    paragraph(&mut markdown, &format!("{label}: {count}"));
                }
            }
            field(&mut markdown, item, "diffStr", "Diff", "diff");
            field(&mut markdown, item, "oldStr", "Before", "text");
            field(&mut markdown, item, "newStr", "After", "text");
            if let Some(changes) = item.get("changes") {
                code(&mut markdown, "Changes", "json", &json(changes));
            }
        }
        "file_search" => {
            field(&mut markdown, item, "pattern", "Pattern", "text");
            if let Some(results) = item["results"].as_array() {
                for (index, result) in results.iter().enumerate() {
                    heading(&mut markdown, &format!("Result {}", index + 1));
                    field(&mut markdown, result, "fileName", "File", "text");
                    for (key, label) in [("line", "Line"), ("column", "Column")] {
                        if let Some(position) = result[key].as_u64() {
                            paragraph(&mut markdown, &format!("{label}: {position}"));
                        }
                    }
                    field(&mut markdown, result, "preview", "Preview", "text");
                }
            }
        }
        "web_search" => {
            if let Some(patterns) = item["patterns"].as_array() {
                for (index, pattern) in patterns.iter().enumerate() {
                    if let Some(pattern) = pattern.as_str() {
                        code(
                            &mut markdown,
                            &format!("Pattern {}", index + 1),
                            "text",
                            pattern,
                        );
                    }
                }
            }
            if let Some(results) = item["results"].as_array() {
                for (index, result) in results.iter().enumerate() {
                    heading(&mut markdown, &format!("Result {}", index + 1));
                    field(&mut markdown, result, "title", "Title", "text");
                    field(&mut markdown, result, "url", "URL", "text");
                    field(&mut markdown, result, "snippet", "Snippet", "text");
                }
            }
        }
        "dynamic_tool" => {
            field(&mut markdown, item, "toolName", "Tool", "text");
            for (key, label) in [("input", "Input"), ("output", "Output")] {
                if let Some(value) = item.get(key) {
                    tool_value(&mut markdown, label, value);
                    if summarized(value) {
                        needs_remote_detail = true;
                        paragraph(
                            &mut markdown,
                            &format!(
                                "{label} was summarized in the thread projection; full detail must be loaded."
                            ),
                        );
                    }
                } else if key == "output" && item["outputOmitted"] != true {
                    paragraph(&mut markdown, "Output is not included in this item.");
                }
            }
            if item["outputOmitted"] == true {
                needs_remote_detail = true;
                paragraph(
                    &mut markdown,
                    "Output was omitted from the thread projection; full detail must be loaded.",
                );
            }
        }
        _ => return None,
    }
    Some(Content {
        markdown,
        needs_remote_detail,
    })
}

fn string<'a>(item: &'a Value, key: &str) -> Option<&'a str> {
    item.get(key).and_then(Value::as_str)
}

fn summarized(value: &Value) -> bool {
    value["truncated"] == true && value["summary"].is_string()
}

fn paragraph(markdown: &mut String, text: &str) {
    if !markdown.is_empty() {
        markdown.push_str("\n\n");
    }
    markdown.push_str(text);
}

fn heading(markdown: &mut String, title: &str) {
    paragraph(markdown, &format!("### {title}\n\n"));
}

fn field(markdown: &mut String, item: &Value, key: &str, title: &str, language: &str) {
    if let Some(text) = string(item, key) {
        code(markdown, title, language, text);
    }
}

fn code(markdown: &mut String, title: &str, language: &str, text: &str) {
    heading(markdown, title);
    markdown.push_str(&fenced(language, text));
}

/// Framing may add a final LF, but never rewrites the supplied content or its
/// line endings. Embedded backtick runs cannot terminate the enclosing fence.
fn fenced(language: &str, text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for byte in text.bytes() {
        if byte == b'`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat(3.max(longest + 1));
    let mut result = format!("{fence}{language}\n{text}");
    if !text.ends_with(['\n', '\r']) {
        result.push('\n');
    }
    result.push_str(&fence);
    result
}

fn json(value: &Value) -> String {
    // Serializing a serde_json::Value cannot fail.
    serde_json::to_string_pretty(value).expect("JSON value is serializable")
}

fn tool_value(markdown: &mut String, title: &str, value: &Value) {
    let value = without_image_data(value, false);
    if let Some(text) = value.as_str() {
        code(markdown, title, "text", text);
    } else {
        code(markdown, title, "json", &json(&value));
    }
}

/// Keep structured values inspectable while image bytes stay in the separate
/// image/attachment surface. Ordinary JSON fields named `data` remain intact.
fn without_image_data(value: &Value, image_context: bool) -> Value {
    match value {
        Value::String(text) if text.starts_with("data:image/") => {
            Value::String("[Image data omitted]".into())
        }
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| without_image_data(value, image_context))
                .collect(),
        ),
        Value::Object(values) => {
            let image_context = image_context
                || matches!(
                    string(value, "type"),
                    Some("image" | "image_url" | "input_image")
                )
                || ["mimeType", "mime_type", "media_type"]
                    .iter()
                    .any(|key| string(value, key).is_some_and(|mime| mime.starts_with("image/")));
            Value::Object(
                values
                    .iter()
                    .map(|(key, value)| {
                        let value = if image_context
                            && matches!(key.as_str(), "data" | "base64" | "bytes")
                        {
                            Value::String("[Image data omitted]".into())
                        } else {
                            without_image_data(value, image_context)
                        };
                        (key.clone(), value)
                    })
                    .collect::<Map<_, _>>(),
            )
        }
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn markdown(item: Value) -> String {
        detail(&item).expect("supported activity").markdown
    }

    #[test]
    fn commands_keep_full_input_output_exit_code_and_embedded_fences() {
        let input = format!(
            "  echo café\r\n{}\r\n```literal````\r\nINPUT_FINAL",
            "x".repeat(600)
        );
        let output = format!("first\r\n{}\r\nOUTPUT_FINAL", "λ".repeat(800));
        let content = detail(&json!({
            "type": "command_execution", "input": input, "output": output, "exitCode": 19
        }))
        .unwrap();
        assert!(content.markdown.contains(&input));
        assert!(content.markdown.contains(&output));
        assert!(content.markdown.contains("Exit code: 19"));
        assert!(content.markdown.contains("`````sh\n"));
        assert!(!content.needs_remote_detail);
        assert_eq!(fenced("text", "café\r\n"), "```text\ncafé\r\n```");
    }

    #[test]
    fn plans_are_verbatim_and_every_todo_step_keeps_its_status_and_text() {
        let plan = format!(
            "# Plan\r\n\r\n- first\r\n{}\r\nPLAN_FINAL",
            "paragraph\n".repeat(100)
        );
        assert_eq!(
            markdown(json!({"type": "proposed_plan", "markdown": plan})),
            plan
        );
        let final_step = format!("multiline\r\n{}\r\nTODO_FINAL", "last step ".repeat(100));
        let content = markdown(json!({
            "type": "todo_list", "explanation": "Full explanation\nsecond line",
            "steps": [
                {"id": "a", "status": "completed", "text": "first step"},
                {"id": "b", "status": "running", "text": "second step\nwith detail"},
                {"id": "c", "status": "pending", "text": final_step}
            ]
        }));
        assert!(content.contains("Full explanation\nsecond line"));
        for status in ["completed", "running", "pending"] {
            assert!(content.contains(status), "missing {status}");
        }
        assert!(content.contains("second step\nwith detail"));
        assert!(content.contains(&final_step));
    }

    #[test]
    fn files_and_all_search_results_keep_their_final_details() {
        let long = format!("{}\nFINAL_SENTINEL", "unchanged\n".repeat(100));
        let content = markdown(json!({
            "type": "file_change", "fileName": "a/λ.rs", "additions": 2, "deletions": 1,
            "diffStr": long, "oldStr": "OLD_FINAL", "newStr": "NEW_FINAL",
            "changes": [{"operation": "rename", "oldPath": "old.rs", "path": "CHANGE_FINAL"}]
        }));
        for expected in [&long, "OLD_FINAL", "NEW_FINAL", "CHANGE_FINAL"] {
            assert!(content.contains(expected));
        }
        let files = markdown(json!({
            "type": "file_search", "pattern": "PATTERN_FINAL",
            "results": [{"fileName": "first.rs"}, {"fileName": "last.rs", "line": 42, "column": 7, "preview": long}]
        }));
        for expected in ["first.rs", "last.rs", "Line: 42", "Column: 7", &long] {
            assert!(files.contains(expected));
        }
        let web = markdown(json!({
            "type": "web_search", "patterns": ["first query", "LAST_QUERY"],
            "results": [{"title": "first"}, {"title": "LAST_TITLE", "url": "https://example.org/final", "snippet": long}]
        }));
        for expected in [
            "LAST_QUERY",
            "LAST_TITLE",
            "https://example.org/final",
            &long,
        ] {
            assert!(web.contains(expected));
        }
    }

    #[test]
    fn omissions_and_summaries_are_explicit_without_guessing_missing_output() {
        for kind in ["command_execution", "dynamic_tool"] {
            let omitted = detail(&json!({
                "type": kind, "input": "call", "outputOmitted": true
            }))
            .unwrap();
            assert!(omitted.needs_remote_detail);
            assert!(omitted.markdown.contains("Output was omitted"));
            let missing = detail(&json!({"type": kind, "input": "call"})).unwrap();
            assert!(!missing.needs_remote_detail);
            assert!(missing.markdown.contains("Output is not included"));
            let empty = detail(&json!({"type": kind, "input": "call", "output": ""})).unwrap();
            assert!(!empty.markdown.contains("Output is not included"));
        }
        let summary = detail(&json!({
            "type": "dynamic_tool", "input": {"truncated": true, "summary": "INPUT_SUMMARY"},
            "output": "AVAILABLE_OUTPUT_FINAL"
        }))
        .unwrap();
        assert!(summary.needs_remote_detail);
        assert!(summary.markdown.contains("INPUT_SUMMARY"));
        assert!(summary.markdown.contains("Input was summarized"));
        assert!(summary.markdown.contains("AVAILABLE_OUTPUT_FINAL"));
    }

    #[test]
    fn dynamic_values_keep_json_and_strings_without_an_image_base64_wall() {
        let raw = "  café\r\n```literal\r\nSTRING_FINAL";
        let content = markdown(json!({
            "type": "dynamic_tool", "toolName": "fixture", "input": raw,
            "output": {"content": [
                {"type": "text", "text": "TEXT_FINAL"},
                {"type": "image", "mimeType": "image/png", "data": "BASE64_SECRET_WALL"},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "NESTED_BASE64_WALL"}},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,DATA_URL_WALL"}}
            ], "ordinary": {"data": "PRESERVED_DATA_FINAL"}}
        }));
        assert!(content.contains(raw));
        assert!(content.contains("TEXT_FINAL"));
        assert!(content.contains("PRESERVED_DATA_FINAL"));
        assert!(content.contains("Image data omitted"));
        for bytes in ["BASE64_SECRET_WALL", "NESTED_BASE64_WALL", "DATA_URL_WALL"] {
            assert!(!content.contains(bytes), "image payload leaked: {bytes}");
        }
        assert!(markdown(json!({
            "type": "dynamic_tool", "input": {"empty": "", "nullable": null}, "output": false
        }))
        .contains("\"nullable\": null"));
    }

    #[test]
    fn ordinary_messages_and_attachments_stay_with_their_existing_adapters() {
        assert!(detail(&json!({
            "type": "user_message", "text": "hello", "attachments": [{"id": "attachment"}]
        }))
        .is_none());
        assert!(detail(&json!({"type": "reasoning", "text": "thinking"})).is_none());
        let with_attachment = markdown(json!({
            "type": "proposed_plan", "markdown": "plan", "attachments": [{"url": "ATTACHMENT_URL"}]
        }));
        assert_eq!(with_attachment, "plan");
    }
}
