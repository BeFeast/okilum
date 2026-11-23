use okilum_core::view::{Finding, Row, Section, Show, Status, ViewPart, ViewResult};
use serde_json::Value;
use std::fmt::Write;

/// Values are data, even when they contain Markdown syntax or a table delimiter.
fn literal(text: &str) -> String {
    let mut escaped = String::new();
    for ch in text.chars() {
        if matches!(ch, '\r' | '\n' | '\t') {
            escaped.push(' ');
        } else {
            if ch.is_ascii_punctuation() {
                escaped.push('\\');
            }
            escaped.push(ch);
        }
    }
    escaped
}

fn value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => literal(text),
        Value::Array(items) => items.iter().map(self::value).collect::<Vec<_>>().join(", "),
        Value::Object(object) if object.get("link").and_then(Value::as_str).is_some() => {
            let mut label = literal(object["link"].as_str().unwrap_or_default());
            if let Some(paths) = object.get("ambiguous").and_then(Value::as_array) {
                write!(
                    label,
                    " (ambiguous: {})",
                    paths.iter().map(self::value).collect::<Vec<_>>().join(", ")
                )
                .unwrap();
            }
            label
        }
        other => literal(&other.to_string()),
    }
}

fn columns(row: &Row, keys: &[String]) -> Vec<String> {
    keys.iter()
        .filter_map(|key| row.fields.get(key))
        .map(value)
        .filter(|cell| !cell.is_empty())
        .collect()
}

fn source(row: &Row) -> String {
    match row.line {
        Some(line) => format!("{}:{line}", literal(&row.path)),
        None => literal(&row.path),
    }
}

fn table(out: &mut String, keys: &[String], rows: &[Row]) {
    for key in keys {
        write!(out, "| {} ", literal(key)).unwrap();
    }
    out.push_str("| Source |\n");
    for _ in 0..=keys.len() {
        out.push_str("| --- ");
    }
    out.push_str("|\n");
    for row in rows {
        for key in keys {
            let cell = row.fields.get(key).map_or_else(String::new, value);
            write!(out, "| {cell} ").unwrap();
        }
        writeln!(out, "| {} |", source(row)).unwrap();
    }
}

fn rows(out: &mut String, section: &Section, rows: &[Row]) {
    match section.show {
        Show::List => {
            for row in rows {
                let values = columns(row, &section.cols);
                let label = if values.is_empty() {
                    source(row)
                } else {
                    values.join(" · ")
                };
                writeln!(out, "- {label}").unwrap();
            }
        }
        Show::Sections => {
            for row in rows {
                let values = columns(row, &section.cols);
                let label = if values.is_empty() {
                    source(row)
                } else {
                    values.join(" · ")
                };
                writeln!(out, "### {label}\n").unwrap();
                if let Some(markdown) = &row.markdown {
                    let body = markdown
                        .split_once('\n')
                        .and_then(|(first, body)| {
                            let title = first.strip_prefix("# ")?;
                            (row.line.is_none() && literal(title.trim()) == label).then_some(body)
                        })
                        .unwrap_or(markdown);
                    writeln!(out, "{body}\n").unwrap();
                }
            }
        }
        Show::Log => {
            // Place the event message before supplemental fields so it stays
            // readable in the same column as the surrounding note.
            let mut keys = section.cols.clone();
            keys.sort_by_key(|key| match key.as_str() {
                "ts" | "@time" => 0,
                "level" => 1,
                "msg" | "message" => 2,
                _ => 3,
            });
            for row in rows {
                let values = columns(row, &keys);
                let label = if values.is_empty() {
                    source(row)
                } else {
                    values.join(" · ")
                };
                writeln!(out, "{label}\n").unwrap();
            }
        }
        _ => table(out, &section.cols, rows),
    }
    out.push('\n');
}

fn diagnostics(out: &mut String, findings: &[Finding]) {
    for finding in findings {
        writeln!(
            out,
            "> Line {} · {}\n",
            finding.line,
            literal(&finding.message)
        )
        .unwrap();
    }
}

fn section(out: &mut String, section: &Section, heading: &str, as_of: &str) {
    if !heading.is_empty() {
        writeln!(out, "{}\n", heading.trim_end()).unwrap();
    }
    match section.status {
        Status::Error => out.push_str("> Section error\n\n"),
        Status::Degraded => out.push_str("> Unavailable\n\n"),
        Status::Ok => {}
    }
    diagnostics(out, &section.diagnostics);
    if section.status != Status::Ok {
        return;
    }
    if let Some(time) = section.snapshot_at.as_deref().and_then(|stamp| {
        let stamp = chrono::DateTime::parse_from_rfc3339(stamp).ok()?;
        let offset = *chrono::DateTime::parse_from_rfc3339(as_of).ok()?.offset();
        Some(stamp.with_timezone(&offset).format("%H:%M").to_string())
    }) {
        writeln!(out, "> Snapshot {time}\n").unwrap();
    }
    if let Some(label) = match section.show {
        Show::Board => Some("Board"),
        Show::Timeline => Some("Timeline"),
        Show::Cards => Some("Cards"),
        Show::Graph => Some("Graph"),
        Show::Tree => Some("Tree"),
        _ => None,
    } {
        writeln!(out, "> {label} shown as table\n").unwrap();
    }
    if section.rows.is_empty() && section.groups.is_empty() && !section.truncated {
        out.push_str("No results.\n\n");
    }
    if section.groups.is_empty() {
        if !section.rows.is_empty() {
            rows(out, section, &section.rows);
        }
    } else {
        let mut offset = 0;
        for group in &section.groups {
            if !group.key.is_empty() {
                let heading = group
                    .key
                    .iter()
                    .map(|key| {
                        if key.is_null() {
                            "(none)".into()
                        } else {
                            value(key)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" / ");
                writeln!(out, "### {heading}\n").unwrap();
            }
            if !group.agg.is_empty() {
                let summary = group
                    .agg
                    .iter()
                    .map(|(name, item)| format!("{} {}", literal(name), value(item)))
                    .collect::<Vec<_>>()
                    .join(" · ");
                writeln!(out, "{summary}\n").unwrap();
            }
            let end = offset + group.count.min(section.rows.len() - offset);
            if end > offset {
                rows(out, section, &section.rows[offset..end]);
                offset = end;
            }
        }
    }
    if section.truncated {
        out.push_str("> Output budget reached.\n\n");
    }
}

pub(super) fn render(result: &ViewResult) -> String {
    let mut markdown = String::new();
    diagnostics(&mut markdown, &result.findings);
    for part in result.parts() {
        match part {
            ViewPart::Markdown(source) => writeln!(markdown, "{}\n", source.trim_end()).unwrap(),
            ViewPart::Section {
                heading,
                section: output,
            } => {
                section(&mut markdown, output, heading, &result.as_of);
            }
        }
    }
    markdown
}

pub(super) fn unavailable(message: &str) -> String {
    format!("> View unavailable\n> {}\n", literal(message))
}
