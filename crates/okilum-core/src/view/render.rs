use super::{
    bind::{self, Params},
    parse::{Part, Show},
    Row, Section, Status, ViewResult,
};
use serde_json::Value as Json;
use std::fmt::Write;

fn parameters(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.match_indices('$').filter_map(|(pos, _)| {
        let tail = &text[pos + 1..];
        let len = tail
            .bytes()
            .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            .count();
        let name = &tail[..len];
        super::parse::name(name).then_some((pos, name))
    })
}

/// Stable JSON envelope. Struct member order keeps view_api at the front.
pub fn render_json(result: &ViewResult) -> serde_json::Result<String> {
    let mut json = serde_json::to_string_pretty(result)?;
    json.push('\n');
    Ok(json)
}

fn escape(text: &str) -> String {
    let mut output = String::new();
    for ch in text.chars() {
        if matches!(ch, '\r' | '\n' | '\t') {
            output.push(' ');
        } else {
            if ch.is_ascii_punctuation() {
                output.push('\\');
            }
            output.push(ch);
        }
    }
    output
}

pub(super) fn substitute(text: &str, params: &Params, markdown: bool) -> String {
    let mut result = String::new();
    let mut start = 0;
    for (pos, name) in parameters(text) {
        let Some(value) = params.get(name) else {
            continue;
        };
        let value = bind::value_text(value);
        result.push_str(&text[start..pos]);
        if markdown {
            result.push_str(&escape(&value));
        } else {
            result.push_str(&value);
        }
        start = pos + 1 + name.len();
    }
    result.push_str(&text[start..]);
    result
}

pub(super) fn bind_headings(
    doc: &super::ViewDoc,
    params: &Params,
    findings: &mut Vec<super::Finding>,
) -> std::collections::BTreeMap<String, String> {
    let mut labels = std::collections::BTreeMap::new();
    for (line, title, markdown) in &doc.headings {
        let mut missing = std::collections::BTreeSet::new();
        let rendered = crate::prose::replace_in_prose(markdown, |chunk| {
            missing.extend(
                parameters(chunk)
                    .map(|(_, name)| name)
                    .filter(|name| !params.contains_key(*name))
                    .map(str::to_owned),
            );
            substitute(chunk, params, true)
        });
        findings.extend(missing.into_iter().map(|name| super::Finding {
            path: doc.path.clone(),
            line: *line,
            col: 1,
            severity: super::Severity::Warning,
            message: format!("Unbound heading parameter '${name}'"),
        }));
        if rendered != *markdown {
            if let Some(heading) = crate::document_links::HeadingInventory::new(&rendered)
                .entries
                .into_iter()
                .next()
            {
                labels.insert(title.clone(), heading.text);
            }
        }
    }
    labels
}

pub(super) fn substitute_parts(parts: &[Part], params: &Params) -> Vec<Part> {
    parts
        .iter()
        .map(|part| match part {
            Part::Markdown(markdown) => Part::Markdown(substitute_prose(markdown, params)),
            Part::Section { index, heading } => Part::Section {
                index: *index,
                heading: substitute_prose(heading, params),
            },
        })
        .collect()
}

fn substitute_prose(text: &str, params: &Params) -> String {
    crate::prose::replace_in_prose(text, |chunk| substitute(chunk, params, true))
}

fn destination(path: &str) -> String {
    let mut encoded = String::new();
    // Snapshots live at @views/out/<name>.md. Relative source links leave
    // that directory and remain portable with the rest of the vault.
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/' | b'@') {
            encoded.push(char::from(byte));
        } else {
            write!(encoded, "%{byte:02X}").unwrap();
        }
    }
    encoded
}

fn source_link(row: &Row) -> String {
    let mut target = format!("../../{}", destination(&row.path));
    if let Some(anchor) = &row.anchor {
        write!(target, "#{}", destination(anchor)).unwrap();
    }
    let label = row
        .line
        .map_or_else(|| row.path.clone(), |line| format!("{}:{line}", row.path));
    format!("[{}]({target})", escape(&label))
}

fn cell(value: &Json) -> String {
    match value {
        Json::Null => String::new(),
        Json::String(text) => escape(text),
        Json::Array(values) => values.iter().map(cell).collect::<Vec<_>>().join(", "),
        Json::Object(object) if object.contains_key("link") => {
            let label = object["link"].as_str().unwrap_or_default();
            if let Some(path) = object.get("path").and_then(Json::as_str) {
                format!("[{}](../../{})", escape(label), destination(path))
            } else if let Some(paths) = object.get("ambiguous").and_then(Json::as_array) {
                format!(
                    "{} \u{2014} ambiguous: {}",
                    escape(label),
                    paths.iter().map(cell).collect::<Vec<_>>().join(", ")
                )
            } else {
                escape(label)
            }
        }
        other => escape(&other.to_string()),
    }
}

/// Render a portable Markdown snapshot using the same selected rows as JSON.
pub fn render_markdown(result: &ViewResult) -> String {
    let mut markdown = String::new();
    for finding in &result.findings {
        writeln!(
            markdown,
            "> {}:{}:{} {}\n",
            escape(&finding.path),
            finding.line,
            finding.col,
            escape(&finding.message)
        )
        .unwrap();
    }
    for part in &result.parts {
        let section = match part {
            Part::Markdown(prose) => {
                markdown.push_str(prose.trim_matches(['\n', '\r']));
                markdown.push_str("\n\n");
                continue;
            }
            Part::Section { index, heading } => {
                if !heading.is_empty() {
                    markdown.push_str(heading.trim_end());
                    markdown.push_str("\n\n");
                }
                &result.sections[*index]
            }
        };
        if section.status != Status::Ok {
            for finding in &section.diagnostics {
                writeln!(
                    markdown,
                    "> {}:{} {}\n",
                    finding.line,
                    finding.col,
                    escape(&finding.message)
                )
                .unwrap();
            }
            continue;
        }
        if let Some(nodes) = &section.nodes {
            render_tree(&mut markdown, nodes, section, 0);
        } else if section.groups.is_empty() {
            render_rows(&mut markdown, section, &section.rows);
        } else {
            let mut offset = 0;
            for group in &section.groups {
                if !group.key.is_empty() {
                    let title = group
                        .key
                        .iter()
                        .map(|value| {
                            if value.is_null() {
                                "(none)".to_owned()
                            } else {
                                cell(value)
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" / ");
                    writeln!(markdown, "### {title}\n").unwrap();
                }
                let agg = group
                    .agg
                    .iter()
                    .map(|(name, value)| format!("{} {}", escape(name), cell(value)))
                    .collect::<Vec<_>>();
                if !agg.is_empty() {
                    writeln!(markdown, "{}\n", agg.join(" · ")).unwrap();
                }
                let end = (offset + group.count).min(section.rows.len());
                if end > offset {
                    render_rows(&mut markdown, section, &section.rows[offset..end]);
                    markdown.push('\n');
                }
                offset = end;
            }
        }
        if section.truncated {
            markdown.push_str("\n> Output budget reached.\n");
        }
        markdown.push('\n');
    }
    markdown
}

fn render_tree(markdown: &mut String, nodes: &[super::TreeNode], section: &Section, depth: usize) {
    let indent = "  ".repeat(depth);
    for node in nodes {
        writeln!(
            markdown,
            "{indent}- {} ({})",
            escape(&node.label),
            node.count
        )
        .unwrap();
        render_tree(markdown, &node.children, section, depth + 1);
        for &index in &node.rows {
            let row = &section.rows[index];
            writeln!(
                markdown,
                "{indent}  - {}",
                list_item(row, &column_values(section, row))
            )
            .unwrap();
        }
    }
}

fn column_values(section: &Section, row: &Row) -> String {
    section
        .cols
        .iter()
        .filter_map(|key| row.fields.get(key))
        .map(cell)
        .collect::<Vec<_>>()
        .join(" · ")
}

fn list_item(row: &Row, values: &str) -> String {
    if values.is_empty() {
        source_link(row)
    } else {
        format!("{values} — {}", source_link(row))
    }
}

fn render_rows(markdown: &mut String, section: &Section, rows: &[Row]) {
    if matches!(section.show, Show::List | Show::Sections) {
        for row in rows {
            let values = column_values(section, row);
            if section.show == Show::Sections {
                writeln!(
                    markdown,
                    "### {}\n",
                    if values.is_empty() {
                        source_link(row)
                    } else {
                        values.clone()
                    }
                )
                .unwrap();
                if !values.is_empty() {
                    writeln!(markdown, "{}\n", source_link(row)).unwrap();
                }
                if let Some(content) = &row.markdown {
                    writeln!(markdown, "{content}\n").unwrap();
                }
            } else {
                writeln!(markdown, "- {}", list_item(row, &values)).unwrap();
            }
        }
    } else {
        markdown.push('|');
        for col in &section.cols {
            write!(markdown, " {} |", escape(col)).unwrap();
        }
        markdown.push_str(" Source |\n|");
        for _ in 0..=section.cols.len() {
            markdown.push_str(" --- |");
        }
        markdown.push('\n');
        for row in rows {
            markdown.push('|');
            for key in &section.cols {
                let value = row.fields.get(key).map_or_else(String::new, cell);
                write!(markdown, " {value} |").unwrap();
            }
            writeln!(markdown, " {} |", source_link(row)).unwrap();
        }
    }
}
