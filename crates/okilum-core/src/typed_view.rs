//! Declarative native-view selection. Parsing never mutates canonical Markdown.
use std::collections::BTreeMap;

use comrak::{nodes::NodeValue, parse_document, Arena};
use serde_yaml::Value;
use time::Date;

use crate::tasks::Query;

pub mod layout;

/// Stable IDs are the boundary for future validated schema sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub id: &'static str,
    pub version: u32,
}

pub const TASKS: Descriptor = Descriptor {
    id: "tasks",
    version: 1,
};

pub fn builtin(id: &str) -> Option<Descriptor> {
    (id == TASKS.id).then_some(TASKS)
}

/// App-owned preferences, never read from special definition files in a vault.
/// Type names are exact, including case; existing authored values are preserved.
pub type TypeMappings = BTreeMap<String, String>;

/// App settings payload. Unknown view IDs survive persistence so selection can
/// explicitly fall back instead of silently deleting future/optional mappings.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preferences {
    pub mappings: TypeMappings,
    pub tasks: layout::Defaults,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Selection {
    Markdown,
    Native(Descriptor),
    Fallback(String),
}

/// An explicit override is authoritative even when invalid/unknown: never guess
/// a different view from the type mapping after an override fails.
pub fn select(source: &str, mappings: &TypeMappings) -> Selection {
    let Some(yaml) = crate::properties::frontmatter_block(source) else {
        return Selection::Markdown;
    };
    let Ok(Value::Mapping(frontmatter)) = serde_yaml::from_str::<Value>(yaml) else {
        return Selection::Fallback("The note's properties could not be read.".into());
    };
    let id = if let Some(value) = frontmatter.get("view") {
        let Some(id) = value.as_str() else {
            return Selection::Fallback("The view name must be text.".into());
        };
        Some(id)
    } else {
        frontmatter
            .get("type")
            .and_then(Value::as_str)
            .and_then(|kind| mappings.get(kind))
            .map(String::as_str)
    };
    match id {
        None => Selection::Markdown,
        Some(id) => builtin(id).map_or_else(
            || Selection::Fallback("This view is not available. Showing Markdown.".into()),
            Selection::Native,
        ),
    }
}

#[derive(Debug)]
pub struct TasksSection {
    /// Plain heading text; no source paths or generated IDs in display copy.
    pub title: Option<String>,
    pub query_source: String,
    pub query: Query,
    /// One-based line in the original source, including frontmatter.
    pub source_line: usize,
}

/// Preserve document order and reuse the existing Obsidian Tasks query parser.
/// Only top-level fenced blocks are dashboard sections; examples inside quotes,
/// lists, or larger code fences remain ordinary Markdown.
pub fn tasks_sections(source: &str, today: Date) -> Result<Vec<TasksSection>, String> {
    let body = crate::render::without_frontmatter(source);
    let prefix_lines = source[..source.len() - body.len()]
        .bytes()
        .filter(|b| *b == b'\n')
        .count();
    let arena = Arena::new();
    let root = parse_document(&arena, body, &crate::render::comrak_options());
    let mut sections = Vec::new();
    let mut title = None;
    for node in root.children() {
        let data = node.data.borrow();
        match &data.value {
            NodeValue::Heading(_) => {
                let mut text = String::new();
                for child in node.descendants() {
                    match &child.data.borrow().value {
                        NodeValue::Text(value) => text.push_str(value),
                        NodeValue::Code(value) => text.push_str(&value.literal),
                        NodeValue::SoftBreak | NodeValue::LineBreak => text.push(' '),
                        _ => {}
                    }
                }
                title = Some(text);
            }
            NodeValue::CodeBlock(block) if block.fenced && block.info.trim() == "tasks" => {
                let query = Query::parse(&block.literal, today);
                if !query.unsupported.is_empty() {
                    return Err(
                        "A Tasks section uses an unsupported query. Showing Markdown.".into(),
                    );
                }
                sections.push(TasksSection {
                    title: title.take(),
                    query_source: block.literal.clone(),
                    query,
                    source_line: prefix_lines + data.sourcepos.start.line,
                });
            }
            _ => title = None,
        }
    }
    if sections.is_empty() {
        return Err("This dashboard has no Tasks sections. Showing Markdown.".into());
    }
    Ok(sections)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_override_wins_and_invalid_overrides_never_use_mapping() {
        let mappings = BTreeMap::from([("Project".into(), "tasks".into())]);
        assert_eq!(
            select("---\ntype: Project\n---\n", &mappings),
            Selection::Native(TASKS)
        );
        assert_eq!(
            select("---\ntype: project\n---\n", &mappings),
            Selection::Markdown
        );
        assert_eq!(
            select("---\nview: tasks\n---\n", &TypeMappings::new()),
            Selection::Native(TASKS)
        );
        for value in ["unknown", "null", "[]", "{}", "true", "42", "''"] {
            assert!(matches!(
                select(
                    &format!("---\ntype: Project\nview: {value}\n---\n"),
                    &mappings
                ),
                Selection::Fallback(_)
            ));
        }
        assert!(matches!(
            select("---\nview: [\n---\n", &mappings),
            Selection::Fallback(_)
        ));
        assert_eq!(
            select("# Project\nview: tasks", &mappings),
            Selection::Markdown
        );
    }

    #[test]
    fn sections_keep_unicode_crlf_positions_and_ignore_fenced_examples() {
        let source = "---\r\nview: tasks\r\n---\r\n# Сегодня 🧪\r\n\r\n```tasks\r\nnot done\r\n```\r\n\r\n````markdown\r\n```tasks\r\ndone\r\n```\r\n````\r\n\r\n# Later\r\n~~~tasks\r\ndone\r\n~~~\r\n";
        let sections = tasks_sections(source, time::macros::date!(2026 - 10 - 07)).unwrap();
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].title.as_deref(), Some("Сегодня 🧪"));
        assert_eq!(sections[0].source_line, 6);
        assert_eq!(sections[1].title.as_deref(), Some("Later"));
        assert_eq!(sections[1].source_line, 17);
        assert!(tasks_sections(
            "```tasks\nunknown command\n```",
            time::macros::date!(2026 - 10 - 07)
        )
        .is_err());
        assert!(tasks_sections(
            "> ```tasks\n> done\n> ```",
            time::macros::date!(2026 - 10 - 07)
        )
        .is_err());
    }
}
