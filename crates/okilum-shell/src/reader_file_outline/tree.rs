//! Contents rows from the viewer's whole-file tree (#2814): no second parse.
//! Expansion is keyed by key path, so it survives rebuilds and view switches.
//! The visible list is computed on a worker with an explicit stack.
use crate::reader_code_file::document::{ContainerKind, Document, ScalarKind};
use std::collections::HashSet;
use std::sync::Arc;

/// Nodes open on arrival down to this depth (top-level rows have depth 1).
const OPEN_DEPTH: usize = 2;
/// A node with more children starts closed.
const OPEN_CHILDREN: usize = 20;
/// Children per group row, and groups per second-level group.
pub(super) const GROUP: usize = 100;
const SECOND_GROUP: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Row {
    Node {
        index: usize,
        depth: usize,
    },
    /// Children `first..=last` (ordinals) of `parent`.
    Group {
        parent: usize,
        first: usize,
        last: usize,
        depth: usize,
    },
}

impl Row {
    pub(super) fn depth(&self) -> usize {
        match self {
            Self::Node { depth, .. } | Self::Group { depth, .. } => *depth,
        }
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
}

/// `key` or `#ordinal` segments from the top, joined by U+001F.
pub(crate) fn path_key(doc: &Document, index: usize) -> String {
    let mut segments = Vec::new();
    let mut at = Some(index);
    while let Some(i) = at {
        if doc.parent(i).is_none() {
            break;
        }
        segments.push(match (doc.key_text(i), doc.array_ordinal(i)) {
            (Some(key), _) => format!("k{key}"),
            (None, Some(n)) => format!("#{n}"),
            _ => format!("@{i}"),
        });
        at = doc.parent(i);
    }
    segments.reverse();
    segments.join("\u{1f}")
}

pub(super) fn group_key(doc: &Document, parent: usize, first: usize, last: usize) -> String {
    format!("{}\u{1e}{first}-{last}", path_key(doc, parent))
}

pub(super) fn child_count(doc: &Document, index: usize) -> usize {
    doc.container(index).map_or(0, |(_, n)| n)
}

/// Open by default: shallow nodes with a handful of children.
pub(super) fn default_open(doc: &Document, index: usize) -> bool {
    let children = child_count(doc, index);
    children > 0 && children <= OPEN_CHILDREN && depth(doc, index) <= OPEN_DEPTH
}

/// Depth below the hidden document root (top-level rows are 1).
pub(super) fn depth(doc: &Document, index: usize) -> usize {
    doc.structure[index].depth
}

enum Work {
    Node(usize, usize),
    Children(usize, usize),
    Range {
        parent: usize,
        children: Arc<Vec<usize>>,
        from: usize,
        to: usize,
        depth: usize,
    },
    GroupRow(usize, usize, usize, usize),
}

/// Rows a user sees, top-down. Only open nodes contribute their children;
/// more than 100 children come in groups of 100 (and groups of 10,000 above
/// 10,000), so one click adds at most 100 rows.
pub(super) fn visible_rows(doc: &Document, expansion: &Expansion) -> Vec<Row> {
    let mut rows = Vec::new();
    if !doc.semantic || doc.structure.is_empty() {
        return rows;
    }
    let mut stack = vec![Work::Children(0, 1)];
    while let Some(work) = stack.pop() {
        match work {
            Work::Node(index, depth) => {
                rows.push(Row::Node { index, depth });
                if expansion.is_open(&path_key(doc, index), default_open(doc, index)) {
                    stack.push(Work::Children(index, depth + 1));
                }
            }
            Work::Children(parent, depth) => {
                let children: Arc<Vec<usize>> = Arc::new(doc.children(parent).collect());
                let len = children.len();
                stack.push(Work::Range {
                    parent,
                    children,
                    from: 0,
                    to: len,
                    depth,
                });
            }
            Work::Range {
                parent,
                children,
                from,
                to,
                depth,
            } => {
                let count = to - from;
                let size = if count > SECOND_GROUP {
                    SECOND_GROUP
                } else if count > GROUP {
                    GROUP
                } else {
                    0
                };
                if size == 0 {
                    for &child in children[from..to].iter().rev() {
                        stack.push(Work::Node(child, depth));
                    }
                    continue;
                }
                // Groups in order: push in reverse so the first pops first.
                let starts: Vec<usize> = (from..to).step_by(size).collect();
                for &start in starts.iter().rev() {
                    let end = (start + size).min(to);
                    let open = expansion.is_open(&group_key(doc, parent, start, end - 1), false);
                    if open {
                        stack.push(Work::Range {
                            parent,
                            children: children.clone(),
                            from: start,
                            to: end,
                            depth: depth + 1,
                        });
                    }
                    stack.push(Work::GroupRow(parent, start, end - 1, depth));
                }
            }
            Work::GroupRow(parent, first, last, depth) => rows.push(Row::Group {
                parent,
                first,
                last,
                depth,
            }),
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
pub(super) fn group_label(
    doc: &Document,
    parent: usize,
    first: usize,
    last: usize,
) -> (String, String) {
    let mut children = doc.children(parent).skip(first);
    let head = children.next();
    let tail = doc.children(parent).nth(last);
    let keyed = head.is_some_and(|i| doc.key_text(i).is_some());
    if keyed {
        let name = |i: Option<usize>| i.map(|i| row_key(doc, i)).unwrap_or_default();
        (
            format!("{} … {}", name(head), name(tail)),
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
