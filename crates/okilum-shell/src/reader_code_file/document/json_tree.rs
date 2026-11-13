//! A semantic JSON index over the immutable whole-file parse, never a byte page.
use super::*;

/// A semantic row of JSON, YAML, TOML or XML. Ranges index the snapshot text;
/// no node text is copied into the index.
#[derive(Debug)]
pub(super) enum Label {
    Source,
    Data {
        /// Key or tag-name text, without quotes.
        key: Option<Range<usize>>,
        value: Range<usize>,
        container: Option<(ContainerKind, usize)>,
        ordinal: Option<usize>,
        scalar: Option<ScalarKind>,
        /// Shown in place of the count (XML `id`, then `name`).
        aside: Option<Range<usize>>,
    },
}

fn preview(text: &str, range: &Range<usize>, limit: usize) -> String {
    let mut chars = text[range.clone()].chars();
    let mut label: String = chars
        .by_ref()
        .take(limit)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if chars.next().is_some() {
        label.push('…');
    }
    label
}

impl Label {
    pub fn prefix(&self, text: &str) -> String {
        match self {
            Self::Data { key: Some(key), .. } => format!("{} ", preview(text, key, 48)),
            Self::Data {
                ordinal: Some(index),
                ..
            } => format!("[{index}] "),
            _ => String::new(),
        }
    }

    pub fn display(&self, text: &str, range: &Range<usize>) -> String {
        let Self::Data {
            value,
            container,
            aside,
            ..
        } = self
        else {
            return preview(text, range, 96);
        };
        let prefix = self.prefix(text);
        let value = match (aside, container) {
            (Some(aside), _) => preview(text, aside, 96),
            (None, Some((kind, 0))) => match kind {
                ContainerKind::Object => "{ }".into(),
                ContainerKind::Array => "[ ]".into(),
                ContainerKind::Element => "0 elements".into(),
            },
            (None, Some((kind, count))) => {
                let (one, many) = match kind {
                    ContainerKind::Object => ("key", "keys"),
                    ContainerKind::Array => ("item", "items"),
                    ContainerKind::Element => ("element", "elements"),
                };
                format!(
                    "{} {}",
                    number(*count),
                    if *count == 1 { one } else { many }
                )
            }
            (None, None) => preview(text, value, 96),
        };
        format!("{prefix}{value}")
    }

    pub fn display_hit(&self, text: &str, range: &Range<usize>, hit: &Range<usize>) -> String {
        let label = self.display(text, range);
        if hit.start < range.start || hit.end > range.end || text.get(hit.clone()).is_none() {
            return label;
        }
        if label.contains(&text[hit.clone()]) {
            return label;
        }
        let prefix = match self {
            Self::Data { key: Some(key), .. } => format!("{} ", preview(text, key, 48)),
            Self::Data {
                ordinal: Some(index),
                ..
            } => format!("[{index}] "),
            Self::Data {
                container: Some(_), ..
            } => format!("{label} · "),
            _ => String::new(),
        };
        let mut start = hit.start.saturating_sub(16).max(range.start).min(range.end);
        while start < range.end && !text.is_char_boundary(start) {
            start += 1;
        }
        let ellipsis = if start > range.start { "…" } else { "" };
        format!(
            "{prefix}{ellipsis}{}",
            preview(text, &(start..range.end), 96)
        )
    }
}

pub(super) fn index(tree: &tree_sitter::Tree, offset: usize, bytes: usize) -> Vec<StructureRow> {
    let range = |node: tree_sitter::Node<'_>| {
        let range = node.byte_range();
        range.start + offset..range.end + offset
    };
    let mut rows: Vec<StructureRow> = Vec::new();
    let mut cursor = tree.walk();
    let mut depth = 0;
    let mut ancestors = Vec::new();
    // Actual syntax depth, visible parent, next array ordinal. Iterative even
    // for deeply nested raw JSON; the UI never recursively constructs nodes.
    let mut parents: Vec<(usize, usize, usize)> = Vec::new();
    loop {
        let value = cursor.node();
        let parent: Option<tree_sitter::Node<'_>> = ancestors.last().copied();
        let property =
            parent.filter(|pair| pair.kind() == "pair" && cursor.field_name() == Some("value"));
        let array_item = value.is_named() && parent.is_some_and(|node| node.kind() == "array");
        let root = value.is_named() && parent.is_some_and(|node| node.kind() == "document");
        if root || property.is_some() || array_item {
            let index = rows.len();
            let parent = parents.last().map(|&(_, index, _)| index);
            let ordinal = if array_item {
                parents.last_mut().map(|(_, _, next)| {
                    let ordinal = *next;
                    *next += 1;
                    ordinal
                })
            } else {
                None
            };
            let owner = property.unwrap_or(value);
            let container = match value.kind() {
                "object" => Some((ContainerKind::Object, value.named_child_count())),
                "array" => Some((ContainerKind::Array, value.named_child_count())),
                _ => None,
            };
            let scalar = container.is_none().then(|| match value.kind() {
                "string" => ScalarKind::String,
                "true" | "false" => ScalarKind::Boolean,
                "null" => ScalarKind::Null,
                _ => ScalarKind::Number,
            });
            rows.push(StructureRow {
                range: if root { 0..bytes } else { range(owner) },
                subtree_end: index + 1,
                depth: parent.map_or(0, |p| rows[p].depth + 1),
                line: owner.start_position().row + 1,
                parent,
                label: Label::Data {
                    // The key is a JSON string node; its text excludes the quotes.
                    key: property
                        .and_then(|pair| pair.child_by_field_name("key"))
                        .map(range)
                        .map(|key| key.start + 1..key.end.saturating_sub(1).max(key.start + 1)),
                    value: range(value),
                    container,
                    ordinal,
                    scalar,
                    aside: None,
                },
            });
            parents.push((depth, index, 0));
        }
        if cursor.goto_first_child() {
            ancestors.push(value);
            depth += 1;
            continue;
        }
        loop {
            if parents.last().is_some_and(|&(at, _, _)| at == depth) {
                let (_, index, _) = parents.pop().unwrap();
                rows[index].subtree_end = rows.len();
            }
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return rows;
            }
            ancestors.pop();
            depth -= 1;
        }
    }
}

pub(super) fn number(count: usize) -> String {
    let digits = count.to_string();
    digits
        .chars()
        .enumerate()
        .fold(String::new(), |mut out, (i, c)| {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                out.push(',');
            }
            out.push(c);
            out
        })
}
