//! Bounded, display-only outlines from the highlighter's existing grammars.
use gpui_component::highlighter::LanguageRegistry;
use std::{
    ops::ControlFlow,
    time::{Duration, Instant},
};
use tree_sitter::{Node, ParseOptions, Parser};

pub(super) const MAX_BYTES: usize = 256 * 1024;
const MAX_ENTRIES: usize = 1000;
const MAX_NODES: usize = 50_000;
const MAX_DEPTH: usize = 64;
const BUDGET: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub(super) struct Entry {
    pub label: String,
    pub line: usize,
    pub parent: Option<usize>,
    pub depth: usize,
    pub children: bool,
}

#[derive(Debug, Default)]
pub(super) struct Outline {
    pub entries: Vec<Entry>,
}

pub(super) fn supported(language: &str) -> bool {
    matches!(language, "yaml" | "json" | "toml" | "rust" | "python")
}

/// No unbounded reads, custom parser, error UI or dependency on a theme/App.
/// An incomplete syntax tree can still contribute correctly located entries.
pub(super) fn parse(language: &str, source: &str) -> Outline {
    if !supported(language) || source.len() > MAX_BYTES {
        return Outline::default();
    }
    let Some(grammar) = LanguageRegistry::singleton()
        .language(language)
        .and_then(|config| config.language)
    else {
        return Outline::default();
    };
    let mut parser = Parser::new();
    if parser.set_language(&grammar).is_err() {
        return Outline::default();
    }
    let deadline = Instant::now() + BUDGET;
    let mut cancelled = |_: &tree_sitter::ParseState| {
        if Instant::now() >= deadline {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let Some(tree) = parser.parse_with_options(
        &mut |offset, _| source.as_bytes().get(offset..).unwrap_or_default(),
        None,
        Some(ParseOptions::new().progress_callback(&mut cancelled)),
    ) else {
        return Outline::default();
    };
    let mut outline = Outline::default();
    let mut stack = vec![(tree.root_node(), None, 0, None)];
    let mut visited = 0;
    while let Some((node, parent, depth, array_index)) = stack.pop() {
        visited += 1;
        if visited > MAX_NODES || outline.entries.len() >= MAX_ENTRIES || Instant::now() >= deadline
        {
            break;
        }
        if depth > MAX_DEPTH || node.is_missing() {
            continue;
        }
        let label = label(language, node, array_index, source);
        let parent = if let Some(label) = label {
            let ix = outline.entries.len();
            let entry_depth = parent.map_or(0, |ix: usize| outline.entries[ix].depth + 1);
            if let Some(ix) = parent {
                outline.entries[ix].children = true;
            }
            outline.entries.push(Entry {
                label,
                line: node.start_position().row,
                parent,
                depth: entry_depth,
                children: false,
            });
            Some(ix)
        } else {
            parent
        };
        let array = matches!(node.kind(), "array" | "flow_sequence" | "block_sequence");
        let count = node
            .named_child_count()
            .min(MAX_NODES.saturating_sub(visited + stack.len()));
        for ix in (0..count).rev() {
            // The visited-node cap is below u32::MAX (the tree-sitter index type).
            if let Some(child) = node.named_child(ix as u32) {
                stack.push((child, parent, depth + 1, array.then_some(ix)));
            }
        }
    }
    outline
}

fn text(node: Node<'_>, source: &str) -> Option<String> {
    // Long/multiline keys or impl headers must not copy arbitrarily large text.
    let raw = source.get(node.byte_range())?;
    if raw.len() > 512 || raw.contains(['\n', '\r']) {
        return None;
    }
    let raw = raw.trim();
    let label = if node.kind() == "string" {
        serde_json::from_str::<String>(raw).ok()?
    } else {
        raw.trim_matches(['\'', '"']).to_owned()
    };
    // Keep each row a single line, including escaped JSON control characters.
    Some(
        label
            .chars()
            .take(120)
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect(),
    )
}

fn label(
    language: &str,
    node: Node<'_>,
    array_index: Option<usize>,
    source: &str,
) -> Option<String> {
    if matches!(language, "yaml" | "json" | "toml") {
        if let Some(ix) = array_index {
            if node.kind() != "comment" {
                return Some(format!("[{ix}]"));
            }
        }
        let key = match (language, node.kind()) {
            ("yaml", "block_mapping_pair" | "flow_pair") | ("json", "pair") => {
                node.child_by_field_name("key")
            }
            ("toml", "pair" | "table" | "table_array_element") => node.named_child(0),
            _ => None,
        }?;
        return text(key, source);
    }
    if language == "rust" && node.kind() == "impl_item" {
        let target = text(node.child_by_field_name("type")?, source)?;
        return Some(
            match node
                .child_by_field_name("trait")
                .and_then(|n| text(n, source))
            {
                Some(trait_name) => format!("impl {trait_name} for {target}"),
                None => format!("impl {target}"),
            },
        );
    }
    let prefix = match (language, node.kind()) {
        ("rust", "function_item" | "function_signature_item") => "fn",
        ("rust", "struct_item") => "struct",
        ("rust", "enum_item") => "enum",
        ("rust", "trait_item") => "trait",
        ("rust", "type_item") => "type",
        ("rust", "mod_item") => "mod",
        ("rust", "const_item") => "const",
        ("rust", "static_item") => "static",
        ("rust", "union_item") => "union",
        ("python", "function_definition") => "def",
        ("python", "class_definition") => "class",
        _ => return None,
    };
    Some(format!(
        "{prefix} {}",
        text(node.child_by_field_name("name")?, source)?
    ))
}

impl Outline {
    pub fn visible(&self, expanded: &std::collections::BTreeSet<usize>) -> Vec<usize> {
        let mut visible = vec![false; self.entries.len()];
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(ix, entry)| {
                visible[ix] = entry
                    .parent
                    .is_none_or(|parent| visible[parent] && expanded.contains(&parent));
                visible[ix].then_some(ix)
            })
            .collect()
    }

    pub fn current(&self, top_line: usize, visible: &[usize]) -> Option<usize> {
        let mut ix = self
            .entries
            .partition_point(|entry| entry.line <= top_line)
            .checked_sub(1)?;
        loop {
            if visible.binary_search(&ix).is_ok() {
                return Some(ix);
            }
            ix = self.entries[ix].parent?;
        }
    }
}
