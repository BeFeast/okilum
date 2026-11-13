//! Semantic rows for YAML, TOML and XML over the tolerant whole-file parse.
//! Same row rules as JSON: a key (or tag name) plus a count or a short value.
//! The walk is iterative with an explicit stack, so deeply nested input cannot
//! exhaust a worker stack; error nodes are skipped, never fatal.
use super::json_tree::Label;
use super::*;
use tree_sitter::Node;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Format {
    Yaml,
    Toml,
    Xml,
}

impl Format {
    pub(super) fn of(language: &str) -> Option<Self> {
        match language {
            "yaml" => Some(Self::Yaml),
            "toml" => Some(Self::Toml),
            "xml" => Some(Self::Xml),
            _ => None,
        }
    }
}

/// What a row stands for before it is expanded.
enum Value<'t> {
    Node(Node<'t>),
    /// A key without a value (`key:` in YAML).
    Empty(usize),
    /// TOML `[[name]]` blocks shown as one array row; `[name.sub]` tables
    /// between them belong to the block before them.
    Group(Vec<Block<'t>>),
    /// One `[[name]]` block: its keys, then its sub-tables.
    Block(Block<'t>),
    /// The hidden root row: the entries of the whole document.
    Root(ContainerKind, Vec<Entry<'t>>),
}

struct Block<'t> {
    node: Node<'t>,
    /// Sub-tables of this block with the key range after `name.`.
    tables: Vec<(Node<'t>, Range<usize>)>,
}

impl Block<'_> {
    fn range(&self) -> Range<usize> {
        let end = self
            .tables
            .last()
            .map_or(self.node.end_byte(), |(table, _)| table.end_byte());
        self.node.start_byte()..end
    }
}

struct Entry<'t> {
    owner: Range<usize>,
    /// One-based source line of the owner, from the parse (no text scan).
    line: usize,
    key: Option<Range<usize>>,
    ordinal: Option<usize>,
    value: Value<'t>,
}

enum Work<'t> {
    Row(Entry<'t>, Option<usize>),
    Close(usize),
}

struct Expanded<'t> {
    value: Range<usize>,
    scalar: Option<ScalarKind>,
    container: Option<(ContainerKind, Vec<Entry<'t>>)>,
    aside: Option<Range<usize>>,
}

pub(super) fn index(
    tree: &tree_sitter::Tree,
    format: Format,
    text: &str,
    bytes: usize,
) -> Vec<StructureRow> {
    let root = tree.root_node();
    let (kind, entries) = match format {
        Format::Yaml => yaml_root(root),
        Format::Toml => (ContainerKind::Object, toml_entries(root, text)),
        Format::Xml => (ContainerKind::Element, xml_root(root)),
    };
    let mut rows: Vec<StructureRow> = Vec::new();
    let mut stack = vec![Work::Row(
        Entry {
            owner: 0..bytes,
            line: 1,
            key: None,
            ordinal: None,
            value: Value::Root(kind, entries),
        },
        None,
    )];
    while let Some(work) = stack.pop() {
        let (entry, parent) = match work {
            Work::Close(index) => {
                rows[index].subtree_end = rows.len();
                continue;
            }
            Work::Row(entry, parent) => (entry, parent),
        };
        let expanded = expand(format, entry.value, text);
        let index = rows.len();
        let (container, children) = match expanded.container {
            Some((kind, children)) => (Some((kind, children.len())), children),
            None => (None, Vec::new()),
        };
        rows.push(StructureRow {
            range: entry.owner.clone(),
            subtree_end: index + 1,
            depth: parent.map_or(0, |p| rows[p].depth + 1),
            line: entry.line,
            parent,
            label: Label::Data {
                key: entry.key,
                value: expanded.value,
                container,
                ordinal: entry.ordinal,
                scalar: expanded.scalar,
                aside: expanded.aside,
            },
        });
        stack.push(Work::Close(index));
        stack.extend(
            children
                .into_iter()
                .rev()
                .map(|child| Work::Row(child, Some(index))),
        );
    }
    rows
}

fn line(node: Node<'_>) -> usize {
    node.start_position().row + 1
}

fn expand<'t>(format: Format, value: Value<'t>, text: &str) -> Expanded<'t> {
    match value {
        Value::Root(kind, entries) => Expanded {
            value: 0..0,
            scalar: None,
            container: Some((kind, entries)),
            aside: None,
        },
        Value::Empty(at) => Expanded {
            value: at..at,
            scalar: Some(ScalarKind::Null),
            container: None,
            aside: None,
        },
        Value::Group(blocks) => {
            let range = blocks[0].range().start..blocks[blocks.len() - 1].range().end;
            let children = blocks
                .into_iter()
                .enumerate()
                .map(|(i, block)| Entry {
                    owner: block.range(),
                    line: line(block.node),
                    key: None,
                    ordinal: Some(i),
                    value: Value::Block(block),
                })
                .collect();
            Expanded {
                value: range,
                scalar: None,
                container: Some((ContainerKind::Array, children)),
                aside: None,
            }
        }
        Value::Block(block) => {
            let mut out = toml_expand(block.node);
            if let Some((_, entries)) = &mut out.container {
                entries.extend(block.tables.into_iter().map(|(table, key)| Entry {
                    owner: table.byte_range(),
                    line: line(table),
                    key: Some(key),
                    ordinal: None,
                    value: Value::Node(table),
                }));
            }
            out
        }
        Value::Node(node) => match format {
            Format::Yaml => yaml_expand(node),
            Format::Toml => toml_expand(node),
            Format::Xml => xml_expand(node, text),
        },
    }
}

fn named<'t>(node: Node<'t>) -> impl Iterator<Item = Node<'t>> {
    let mut cursor = node.walk();
    let children: Vec<_> = node.named_children(&mut cursor).collect();
    children.into_iter()
}

/// Quoted keys and values are shown without their quotes.
fn unquoted(node: Node<'_>) -> Range<usize> {
    let range = node.byte_range();
    match node.kind() {
        "double_quote_scalar" | "single_quote_scalar" | "quoted_key" if range.len() >= 2 => {
            range.start + 1..range.end - 1
        }
        _ => range,
    }
}

// ---------------------------------------------------------------- YAML

fn yaml_content(mut node: Node<'_>) -> Node<'_> {
    // block_node / flow_node wrap the content after an optional anchor and tag.
    while matches!(node.kind(), "block_node" | "flow_node") {
        match named(node).find(|child| !matches!(child.kind(), "anchor" | "tag" | "comment")) {
            Some(child) => node = child,
            None => break,
        }
    }
    node
}

/// The content node of a YAML document, after directives.
fn yaml_document_content(document: Node<'_>) -> Option<Node<'_>> {
    named(document).find(|node| matches!(node.kind(), "block_node" | "flow_node"))
}

fn yaml_root(root: Node<'_>) -> (ContainerKind, Vec<Entry<'_>>) {
    let documents: Vec<_> = named(root)
        .filter(|node| node.kind() == "document")
        .collect();

    if let [document] = documents.as_slice() {
        // A single document: its top-level entries are the root's children.
        return match yaml_document_content(*document) {
            Some(node) => match yaml_expand(node).container {
                Some((kind, entries)) => (kind, entries),
                None => (
                    ContainerKind::Array,
                    vec![Entry {
                        owner: node.byte_range(),
                        line: line(node),
                        key: None,
                        ordinal: Some(0),
                        value: Value::Node(node),
                    }],
                ),
            },
            None => (ContainerKind::Object, Vec::new()),
        };
    }
    let entries = documents
        .into_iter()
        .enumerate()
        .map(|(i, document)| Entry {
            owner: document.byte_range(),
            line: line(document),
            key: None,
            ordinal: Some(i),
            value: match yaml_document_content(document) {
                Some(node) => Value::Node(node),
                None => Value::Empty(document.end_byte()),
            },
        })
        .collect();
    (ContainerKind::Array, entries)
}

fn yaml_expand(node: Node<'_>) -> Expanded<'_> {
    let node = yaml_content(node);
    let mut out = Expanded {
        value: node.byte_range(),
        scalar: None,
        container: None,
        aside: None,
    };
    match node.kind() {
        "block_mapping" | "flow_mapping" => {
            let entries = named(node)
                .filter_map(|pair| match pair.kind() {
                    "block_mapping_pair" | "flow_pair" => Some(Entry {
                        owner: pair.byte_range(),
                        line: line(pair),
                        key: pair
                            .child_by_field_name("key")
                            .map(|key| unquoted(yaml_content(key))),
                        ordinal: None,
                        value: match pair.child_by_field_name("value") {
                            Some(value) => Value::Node(value),
                            None => Value::Empty(pair.end_byte()),
                        },
                    }),
                    // `{ a, b }`: a key without a value.
                    "flow_node" => Some(Entry {
                        owner: pair.byte_range(),
                        line: line(pair),
                        key: Some(unquoted(yaml_content(pair))),
                        ordinal: None,
                        value: Value::Empty(pair.end_byte()),
                    }),
                    _ => None,
                })
                .collect();
            out.container = Some((ContainerKind::Object, entries));
        }
        "block_sequence" | "flow_sequence" => {
            let entries = named(node)
                .filter(|item| {
                    matches!(
                        item.kind(),
                        "block_sequence_item" | "flow_node" | "flow_pair"
                    )
                })
                .enumerate()
                .map(|(i, item)| Entry {
                    owner: item.byte_range(),
                    line: line(item),
                    key: None,
                    ordinal: Some(i),
                    value: if item.kind() == "block_sequence_item" {
                        match named(item).find(|child| child.kind() != "comment") {
                            Some(value) => Value::Node(value),
                            None => Value::Empty(item.end_byte()),
                        }
                    } else {
                        Value::Node(item)
                    },
                })
                .collect();
            out.container = Some((ContainerKind::Array, entries));
        }
        "flow_pair" => {
            // A single-pair mapping inside a flow sequence: `[a: 1]`.
            let entries = vec![Entry {
                owner: node.byte_range(),
                line: line(node),
                key: node
                    .child_by_field_name("key")
                    .map(|key| unquoted(yaml_content(key))),
                ordinal: None,
                value: match node.child_by_field_name("value") {
                    Some(value) => Value::Node(value),
                    None => Value::Empty(node.end_byte()),
                },
            }];
            out.container = Some((ContainerKind::Object, entries));
        }
        "plain_scalar" => {
            out.scalar = Some(match named(node).next().map(|n| n.kind()) {
                Some("boolean_scalar") => ScalarKind::Boolean,
                Some("null_scalar") => ScalarKind::Null,
                Some("integer_scalar" | "float_scalar") => ScalarKind::Number,
                _ => ScalarKind::String,
            });
        }
        _ => out.scalar = Some(ScalarKind::String),
    }
    out
}

// ---------------------------------------------------------------- TOML

fn toml_key(node: Node<'_>) -> Option<Node<'_>> {
    named(node).find(|child| matches!(child.kind(), "bare_key" | "quoted_key" | "dotted_key"))
}

fn toml_pair(pair: Node<'_>) -> Entry<'_> {
    let key = toml_key(pair);
    let value = named(pair)
        .filter(|child| Some(*child) != key && !child.is_error() && child.kind() != "comment")
        .last();
    Entry {
        owner: pair.byte_range(),
        line: line(pair),
        key: key.map(unquoted),
        ordinal: None,
        value: match value {
            Some(value) => Value::Node(value),
            None => Value::Empty(pair.end_byte()),
        },
    }
}

/// Pairs and tables in source order. `[[name]]` blocks with the same header
/// form one array row, and a `[name.sub]` table between them belongs to the
/// block before it (TOML semantics), so the blocks stay one row.
fn toml_entries<'t>(node: Node<'t>, text: &str) -> Vec<Entry<'t>> {
    let header = |block: Node<'_>| toml_key(block).map(|key| &text[key.byte_range()]);
    let mut entries: Vec<Entry<'t>> = Vec::new();
    let mut group: Vec<Block<'t>> = Vec::new();
    let flush = |group: &mut Vec<Block<'t>>, entries: &mut Vec<Entry<'t>>| {
        if let (Some(first), Some(last)) = (group.first(), group.last()) {
            entries.push(Entry {
                owner: first.range().start..last.range().end,
                line: line(first.node),
                key: toml_key(first.node).map(unquoted),
                ordinal: None,
                value: Value::Group(std::mem::take(group)),
            });
        }
    };
    for child in named(node) {
        match child.kind() {
            "table_array_element" => {
                if group
                    .first()
                    .is_some_and(|first| header(first.node) != header(child))
                {
                    flush(&mut group, &mut entries);
                }
                group.push(Block {
                    node: child,
                    tables: Vec::new(),
                });
            }
            "table" => {
                let sub = group.first().and_then(|first| {
                    let name = header(first.node)?;
                    let key = toml_key(child)?;
                    // `[bin.meta]` under `[[bin]]`: the key after `bin.`.
                    let rest = text[key.byte_range()].strip_prefix(name)?;
                    let rest = rest.trim_start().strip_prefix('.')?.trim_start();
                    Some(key.end_byte() - rest.len()..key.end_byte())
                });
                match sub {
                    Some(key) => group.last_mut().unwrap().tables.push((child, key)),
                    None => {
                        flush(&mut group, &mut entries);
                        entries.push(Entry {
                            owner: child.byte_range(),
                            line: line(child),
                            key: toml_key(child).map(unquoted),
                            ordinal: None,
                            value: Value::Node(child),
                        });
                    }
                }
            }
            "pair" => {
                flush(&mut group, &mut entries);
                entries.push(toml_pair(child));
            }
            _ => {}
        }
    }
    flush(&mut group, &mut entries);
    entries
}

fn toml_expand(node: Node<'_>) -> Expanded<'_> {
    let mut out = Expanded {
        value: node.byte_range(),
        scalar: None,
        container: None,
        aside: None,
    };
    match node.kind() {
        "table" | "table_array_element" | "inline_table" => {
            let entries = named(node)
                .filter(|child| child.kind() == "pair")
                .map(toml_pair)
                .collect();
            out.container = Some((ContainerKind::Object, entries));
        }
        "array" => {
            let entries = named(node)
                .filter(|child| !child.is_error() && child.kind() != "comment")
                .enumerate()
                .map(|(i, item)| Entry {
                    owner: item.byte_range(),
                    line: line(item),
                    key: None,
                    ordinal: Some(i),
                    value: Value::Node(item),
                })
                .collect();
            out.container = Some((ContainerKind::Array, entries));
        }
        "integer" | "float" => out.scalar = Some(ScalarKind::Number),
        "boolean" => out.scalar = Some(ScalarKind::Boolean),
        _ => out.scalar = Some(ScalarKind::String),
    }
    out
}

// ---------------------------------------------------------------- XML

fn xml_root(document: Node<'_>) -> Vec<Entry<'_>> {
    document
        .child_by_field_name("root")
        .or_else(|| named(document).find(|node| node.kind() == "element"))
        .map(|root| Entry {
            owner: root.byte_range(),
            line: line(root),
            key: xml_name(root),
            ordinal: None,
            value: Value::Node(root),
        })
        .into_iter()
        .collect()
}

/// Child elements of `content`, looking through error nodes of a tolerant parse.
fn xml_children(element: Node<'_>) -> Vec<Node<'_>> {
    let wrapper = |node: &Node<'_>| node.kind() == "content" || node.is_error();
    let mut out = Vec::new();
    let mut stack: Vec<Node<'_>> = named(element).filter(wrapper).collect();
    stack.reverse();
    while let Some(node) = stack.pop() {
        if node.kind() == "element" {
            out.push(node);
            continue;
        }
        let inner: Vec<_> = named(node)
            .filter(|child| child.kind() == "element" || wrapper(child))
            .collect();
        stack.extend(inner.into_iter().rev());
    }
    out
}

fn xml_expand<'t>(element: Node<'t>, text: &str) -> Expanded<'t> {
    let tag = named(element).find(|node| matches!(node.kind(), "STag" | "EmptyElemTag"));
    let mut aside = None;
    if let Some(tag) = tag {
        let attribute = |wanted: &str| {
            named(tag)
                .filter(|node| node.kind() == "Attribute")
                .find_map(|attribute| {
                    let name = named(attribute).find(|node| node.kind() == "Name")?;
                    let name = &text[name.byte_range()];
                    let local = name.rsplit_once(':').map_or(name, |(_, local)| local);
                    (local == wanted).then_some(())?;
                    let value = named(attribute).find(|node| node.kind() == "AttValue")?;
                    let range = value.byte_range();
                    Some(range.start + 1..range.end.saturating_sub(1).max(range.start + 1))
                })
        };
        aside = attribute("id").or_else(|| attribute("name"));
    }
    let children: Vec<Entry<'t>> = xml_children(element)
        .into_iter()
        .map(|child| Entry {
            owner: child.byte_range(),
            line: line(child),
            key: xml_name(child),
            ordinal: None,
            value: Value::Node(child),
        })
        .collect();
    let mut out = Expanded {
        value: element.byte_range(),
        scalar: None,
        container: None,
        aside,
    };
    if !children.is_empty() {
        out.container = Some((ContainerKind::Element, children));
    } else if out.aside.is_none() {
        // Short text of a leaf element; attributes, comments and text are not rows.
        let content = named(element).find(|node| node.kind() == "content");
        let data = content.and_then(|content| {
            named(content).find(|node| matches!(node.kind(), "CharData" | "CDSect"))
        });
        out.scalar = Some(ScalarKind::String);
        out.value = match data {
            Some(data) => trimmed(text, data.byte_range()),
            None => element.end_byte()..element.end_byte(),
        };
    }
    out
}

fn xml_name(element: Node<'_>) -> Option<Range<usize>> {
    let tag = named(element).find(|node| matches!(node.kind(), "STag" | "EmptyElemTag"))?;
    named(tag)
        .find(|node| node.kind() == "Name")
        .map(|name| name.byte_range())
}

fn trimmed(text: &str, range: Range<usize>) -> Range<usize> {
    let slice = &text[range.clone()];
    let start = range.start + (slice.len() - slice.trim_start().len());
    let end = range.end - (slice.len() - slice.trim_end().len());
    start..end.max(start)
}

/// The first error or missing node of a tolerant parse, as a position.
pub(super) fn first_syntax_problem(
    tree: &tree_sitter::Tree,
    text: &str,
    offset: usize,
) -> Option<JsonError> {
    let root = tree.root_node();
    if !root.has_error() {
        return None;
    }
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        if node.is_error() || node.is_missing() {
            let at = (node.start_byte() + offset).min(text.len());
            let (line, column) = position(text, at);
            return Some(JsonError {
                line,
                column,
                message: "Syntax problem".into(),
                offset: at,
            });
        }
        if node.has_error() && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return None;
            }
        }
    }
}

/// One-based line and character column of a byte offset.
pub(super) fn position(text: &str, offset: usize) -> (usize, usize) {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &text[..offset];
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    (
        before.matches('\n').count() + 1,
        text[line_start..offset].chars().count() + 1,
    )
}

/// `serde_yaml` is the only YAML verdict; multi-document files and anchors pass.
pub(super) fn yaml_error(text: &str) -> Option<JsonError> {
    use serde::Deserialize as _;
    let failure = serde_yaml::Deserializer::from_str(text)
        .find_map(|document| serde::de::IgnoredAny::deserialize(document).err())?;
    let message = failure.to_string();
    // Nesting beyond the parser's policy is a refusal, not an invalid file.
    if message.starts_with("recursion limit exceeded") {
        return None;
    }
    let offset = failure
        .location()
        .map_or(0, |location| location.index())
        .min(text.len());
    let (line, column) = position(text, offset);
    let message = message
        .rsplit_once(" at line ")
        .map_or(message.clone(), |(message, _)| message.to_owned());
    let mut offset = offset;
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    Some(JsonError {
        line,
        column,
        message,
        offset,
    })
}
