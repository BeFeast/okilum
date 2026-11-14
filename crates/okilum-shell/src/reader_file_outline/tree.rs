//! Contents rows from the viewer's whole-file tree (#2814): no second parse.
//! Expansion is keyed by key path, so it survives rebuilds and view switches.
//! The visible list is computed on a worker with an explicit stack; each row
//! carries what rendering needs, so drawing a row is O(1).
use crate::reader_code_file::document::{ContainerKind, Document, ScalarKind};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Nodes open on arrival down to this depth (top-level rows have depth 1).
const OPEN_DEPTH: usize = 2;
/// A node with more children starts closed.
const OPEN_CHILDREN: usize = 20;
/// Children per group row, and groups per second-level group.
pub(super) const GROUP: usize = 100;
const SECOND_GROUP: usize = 10_000;
/// Separates path segments, a duplicate key's occurrence, and group ranges.
const SEGMENT: char = '\u{1f}';
const OCCURRENCE: char = '\u{1d}';
const RANGE: char = '\u{1e}';

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Row {
    Node {
        index: usize,
        depth: usize,
        /// Path identity; a repeated key gets its occurrence number.
        key: Arc<str>,
        /// Open unless the user closed it. Children listed through a group
        /// start closed, so one click adds at most 100 rows.
        default_open: bool,
    },
    /// Children `first..=last` (ordinals) of `parent`; `first_index` and
    /// `last_index` are their document rows, for the label.
    Group {
        parent: usize,
        first: usize,
        last: usize,
        first_index: usize,
        last_index: usize,
        depth: usize,
        key: Arc<str>,
    },
}

impl Row {
    pub(super) fn depth(&self) -> usize {
        match self {
            Self::Node { depth, .. } | Self::Group { depth, .. } => *depth,
        }
    }
    pub(super) fn key(&self) -> &str {
        match self {
            Self::Node { key, .. } | Self::Group { key, .. } => key,
        }
    }
    pub(super) fn default_open(&self) -> bool {
        matches!(
            self,
            Self::Node {
                default_open: true,
                ..
            }
        )
    }
}

/// User choices against the defaults, by path; defaults need no entries.
#[derive(Clone, Debug, Default)]
pub(super) struct Expansion {
    pub opened: HashSet<String>,
    pub closed: HashSet<String>,
}

impl Expansion {
    pub(super) fn is_open(&self, key: &str, default: bool) -> bool {
        if default {
            !self.closed.contains(key)
        } else {
            self.opened.contains(key)
        }
    }
    pub(super) fn set(&mut self, key: String, open: bool, default: bool) {
        self.opened.remove(&key);
        self.closed.remove(&key);
        if open != default {
            if open {
                self.opened.insert(key);
            } else {
                self.closed.insert(key);
            }
        }
    }
    /// Flip a row's open state against its own default.
    pub(super) fn toggle(&mut self, row: &Row) {
        let default = row.default_open();
        let open = self.is_open(row.key(), default);
        self.set(row.key().to_owned(), !open, default);
    }
    pub(super) fn row_open(&self, row: &Row) -> bool {
        self.is_open(row.key(), row.default_open())
    }
}

pub(super) fn child_count(doc: &Document, index: usize) -> usize {
    doc.container(index).map_or(0, |(_, n)| n)
}

/// Open by default: shallow nodes with a handful of children.
fn default_open(doc: &Document, index: usize) -> bool {
    let children = child_count(doc, index);
    children > 0 && children <= OPEN_CHILDREN && doc.structure[index].depth <= OPEN_DEPTH
}

enum Work {
    Node {
        index: usize,
        depth: usize,
        key: Arc<str>,
        grouped: bool,
    },
    Range {
        parent: usize,
        parent_key: Arc<str>,
        children: Arc<Vec<(usize, Arc<str>)>>,
        from: usize,
        to: usize,
        depth: usize,
        grouped: bool,
    },
    Group(Row),
}

/// Children of `parent` with their path keys. A key that repeats in the same
/// object gets its occurrence number, so each node has its own identity.
fn keyed_children(doc: &Document, parent: usize, parent_key: &str) -> Vec<(usize, Arc<str>)> {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    doc.children(parent)
        .map(|child| {
            let segment = match (doc.key_text(child), doc.array_ordinal(child)) {
                (Some(key), _) => {
                    let n = seen.entry(key).or_insert(0);
                    let segment = if *n == 0 {
                        format!("k{key}")
                    } else {
                        format!("k{key}{OCCURRENCE}{n}")
                    };
                    *n += 1;
                    segment
                }
                (None, Some(n)) => format!("#{n}"),
                _ => format!("@{child}"),
            };
            let key: Arc<str> = if parent_key.is_empty() {
                segment.into()
            } else {
                format!("{parent_key}{SEGMENT}{segment}").into()
            };
            (child, key)
        })
        .collect()
}

/// Children per group for a range of `count` children, or 0 for no groups.
fn group_size(count: usize) -> usize {
    if count > SECOND_GROUP {
        SECOND_GROUP
    } else if count > GROUP {
        GROUP
    } else {
        0
    }
}

/// Open every ancestor of `node` and the groups that hold it, by the keys
/// `visible_rows` gives them, so the node is listed (#2814 9b). The node
/// itself keeps its state. Walks the siblings on the path once.
pub(super) fn open_path(doc: &Document, node: usize, expansion: &mut Expansion) {
    let mut path = vec![node];
    let mut at = doc.parent(node);
    while let Some(index) = at.filter(|&i| i > 0) {
        path.push(index);
        at = doc.parent(index);
    }
    let mut parent = 0;
    let mut parent_key: Arc<str> = "".into();
    for (depth, &target) in path.iter().rev().enumerate() {
        let children = keyed_children(doc, parent, &parent_key);
        let Some(ordinal) = children.iter().position(|(child, _)| *child == target) else {
            return;
        };
        let grouped = group_size(children.len()) > 0;
        let (mut from, mut to) = (0, children.len());
        loop {
            let size = group_size(to - from);
            if size == 0 {
                break;
            }
            let start = from + (ordinal - from) / size * size;
            let end = (start + size).min(to);
            expansion.set(
                format!("{parent_key}{RANGE}{start}-{}", end - 1),
                true,
                false,
            );
            (from, to) = (start, end);
        }
        let key = children[ordinal].1.clone();
        if depth + 1 < path.len() {
            expansion.set(key.to_string(), true, !grouped && default_open(doc, target));
        }
        parent = target;
        parent_key = key;
    }
}

/// Rows a user sees, top-down. Only open nodes contribute their children;
/// more than 100 children come in groups of 100 (and groups of 10,000 above
/// 10,000), so one click adds at most 100 rows.
pub(super) fn visible_rows(doc: &Document, expansion: &Expansion) -> Vec<Row> {
    let mut rows = Vec::new();
    if !doc.semantic || doc.structure.is_empty() {
        return rows;
    }
    let root_key: Arc<str> = "".into();
    let children = Arc::new(keyed_children(doc, 0, &root_key));
    let mut stack = vec![Work::Range {
        parent: 0,
        parent_key: root_key,
        to: children.len(),
        children,
        from: 0,
        depth: 1,
        grouped: false,
    }];
    while let Some(work) = stack.pop() {
        match work {
            Work::Node {
                index,
                depth,
                key,
                grouped,
            } => {
                let row = Row::Node {
                    index,
                    depth,
                    key: key.clone(),
                    default_open: !grouped && default_open(doc, index),
                };
                let open = child_count(doc, index) > 0 && expansion.row_open(&row);
                rows.push(row);
                if open {
                    let children = Arc::new(keyed_children(doc, index, &key));
                    stack.push(Work::Range {
                        parent: index,
                        parent_key: key,
                        to: children.len(),
                        children,
                        from: 0,
                        depth: depth + 1,
                        grouped: false,
                    });
                }
            }
            Work::Range {
                parent,
                parent_key,
                children,
                from,
                to,
                depth,
                grouped,
            } => {
                let size = group_size(to - from);
                if size == 0 {
                    for (child, key) in children[from..to].iter().rev() {
                        stack.push(Work::Node {
                            index: *child,
                            depth,
                            key: key.clone(),
                            grouped,
                        });
                    }
                    continue;
                }
                // Groups in order: push in reverse so the first pops first.
                let starts: Vec<usize> = (from..to).step_by(size).collect();
                for &start in starts.iter().rev() {
                    let end = (start + size).min(to);
                    let row = Row::Group {
                        parent,
                        first: start,
                        last: end - 1,
                        first_index: children[start].0,
                        last_index: children[end - 1].0,
                        depth,
                        key: format!("{parent_key}{RANGE}{start}-{}", end - 1).into(),
                    };
                    if expansion.row_open(&row) {
                        stack.push(Work::Range {
                            parent,
                            parent_key: parent_key.clone(),
                            children: children.clone(),
                            from: start,
                            to: end,
                            depth: depth + 1,
                            grouped: true,
                        });
                    }
                    stack.push(Work::Group(row));
                }
            }
            Work::Group(row) => rows.push(row),
        }
    }
    rows
}

/// The left part of a row: key, ordinal (0, not [0]) or tag name.
pub(super) fn row_key(doc: &Document, index: usize) -> String {
    match (doc.key_text(index), doc.array_ordinal(index)) {
        (Some(key), _) => key
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect(),
        (None, Some(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// The right part: a count or a short value (strings up to 16 characters).
pub(super) fn row_value(doc: &Document, index: usize) -> String {
    // An XML element's `id` or `name` names it better than its count (#2813).
    if let Some(aside) = doc.aside(index) {
        return short(aside, 24);
    }
    if let Some((kind, count)) = doc.container(index) {
        let (one, many, empty) = match kind {
            ContainerKind::Object => ("key", "keys", "{ }"),
            ContainerKind::Array => ("item", "items", "[ ]"),
            ContainerKind::Element => ("element", "elements", "0 elements"),
        };
        return match count {
            0 => empty.into(),
            1 => format!("1 {one}"),
            n => format!("{} {many}", grouped(n)),
        };
    }
    match doc.scalar(index) {
        Some((ScalarKind::String, text)) => short(&text, 16),
        Some((_, text)) => short(&text, 24),
        None => String::new(),
    }
}

fn short(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let mut out: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

pub(super) fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// «/v1/account … /v1/charges» with «1–100» on the right, or «0 … 99».
/// O(1): the boundary rows were found on the worker.
pub(super) fn group_label(doc: &Document, row: &Row) -> (String, String) {
    let Row::Group {
        first,
        last,
        first_index,
        last_index,
        ..
    } = *row
    else {
        return (String::new(), String::new());
    };
    if doc.key_text(first_index).is_some() {
        (
            format!(
                "{} … {}",
                row_key(doc, first_index),
                row_key(doc, last_index)
            ),
            format!("{}–{}", grouped(first + 1), grouped(last + 1)),
        )
    } else {
        (
            format!("{} … {}", grouped(first), grouped(last)),
            String::new(),
        )
    }
}

/// Full key and its path for the tooltip.
pub(super) fn row_tooltip(doc: &Document, index: usize) -> String {
    let mut names = Vec::new();
    let mut at = Some(index);
    while let Some(i) = at {
        if doc.parent(i).is_none() {
            break;
        }
        names.push(row_key(doc, i));
        at = doc.parent(i);
    }
    names.reverse();
    names.join(" › ")
}
