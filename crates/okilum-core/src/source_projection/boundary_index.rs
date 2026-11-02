//! Persistent grapheme membership index. LF is an unconditional grapheme break;
//! only complete changed lines need segmentation after an edit. AVL branches
//! retain the untouched prefix/suffix with relative offsets, including CRLF.
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Debug)]
pub(super) struct Index(Option<Arc<Node>>);
#[derive(Debug)]
struct Node {
    bytes: usize,
    height: u32,
    newlines: usize,
    kind: Kind,
}
#[derive(Debug)]
enum Kind {
    Leaf {
        boundaries: Vec<usize>,
        newlines: Vec<usize>,
    },
    Branch(Arc<Node>, Arc<Node>),
}

#[cfg(test)]
thread_local! {
    pub(crate) static INDEXED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl Index {
    pub(super) fn new(text: &str) -> Self {
        let mut root = None;
        let mut start = 0;
        let mut end = 0;
        for line in text.split_inclusive('\n') {
            end += line.len();
            if end - start >= 4096 {
                root = join(root, leaf(&text[start..end]));
                start = end;
            }
        }
        Self(join(root, leaf(&text[start..])))
    }

    pub(super) fn update(&self, old: &str, new: &str) -> Self {
        if old == new {
            return self.clone();
        }
        let prefix = old
            .bytes()
            .zip(new.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        // Align to LF, so neither UTF-8 nor a grapheme can cross the seam.
        let start = old.as_bytes()[..prefix]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |at| at + 1);
        let suffix = old.as_bytes()[prefix..]
            .iter()
            .rev()
            .zip(new.as_bytes()[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let shared = old.len() - suffix;
        let tail = old.as_bytes()[shared..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(0, |at| suffix - at - 1);
        let (left, rest) = split(self.0.clone(), start);
        let (_, right) = split(rest, old.len() - tail - start);
        Self(join(
            join(left, Self::new(&new[start..new.len() - tail]).0),
            right,
        ))
    }

    pub(super) fn binary_search(&self, byte: &usize) -> Result<(), ()> {
        let Some(mut node) = self.0.as_deref() else {
            return (*byte == 0).then_some(()).ok_or(());
        };
        if *byte > node.bytes {
            return Err(());
        }
        let mut at = *byte;
        loop {
            match &node.kind {
                Kind::Leaf { boundaries, .. } => {
                    return boundaries.binary_search(&at).map(|_| ()).map_err(|_| ())
                }
                Kind::Branch(left, right) => {
                    if at < left.bytes {
                        node = left;
                    } else {
                        at -= left.bytes;
                        node = right;
                    }
                }
            }
        }
    }
    pub(super) fn line_at(&self, byte: usize) -> usize {
        let Some(mut node) = self.0.as_deref() else {
            return 0;
        };
        let mut at = byte.min(node.bytes);
        let mut line = 0;
        loop {
            match &node.kind {
                Kind::Leaf { newlines, .. } => return line + newlines.partition_point(|&b| b < at),
                Kind::Branch(left, right) => {
                    if at < left.bytes {
                        node = left;
                    } else {
                        line += left.newlines;
                        at -= left.bytes;
                        node = right;
                    }
                }
            }
        }
    }
    pub(super) fn line_start(&self, line: usize) -> Option<usize> {
        if line == 0 {
            return Some(0);
        }
        let mut node = self.0.as_deref()?;
        if line > node.newlines {
            return None;
        }
        let mut remaining = line - 1;
        let mut offset = 0;
        loop {
            match &node.kind {
                Kind::Leaf { newlines, .. } => return Some(offset + newlines[remaining] + 1),
                Kind::Branch(left, right) => {
                    if remaining < left.newlines {
                        node = left;
                    } else {
                        remaining -= left.newlines;
                        offset += left.bytes;
                        node = right;
                    }
                }
            }
        }
    }
}

fn leaf(text: &str) -> Option<Arc<Node>> {
    if text.is_empty() {
        return None;
    }
    #[cfg(test)]
    INDEXED_BYTES.with(|n| n.set(n.get() + text.len()));
    let newlines: Vec<_> = text.match_indices('\n').map(|(at, _)| at).collect();
    Some(Arc::new(Node {
        bytes: text.len(),
        height: 1,
        newlines: newlines.len(),
        kind: Kind::Leaf {
            boundaries: text
                .grapheme_indices(true)
                .map(|(at, _)| at)
                .chain(std::iter::once(text.len()))
                .collect(),
            newlines,
        },
    }))
}
fn branch(left: Arc<Node>, right: Arc<Node>) -> Arc<Node> {
    Arc::new(Node {
        bytes: left.bytes + right.bytes,
        newlines: left.newlines + right.newlines,
        height: 1 + left.height.max(right.height),
        kind: Kind::Branch(left, right),
    })
}
fn balance(left: Arc<Node>, right: Arc<Node>) -> Arc<Node> {
    if left.height > right.height + 1 {
        let Kind::Branch(a, b) = &left.kind else {
            unreachable!()
        };
        if a.height >= b.height {
            return branch(a.clone(), branch(b.clone(), right));
        }
        let Kind::Branch(c, d) = &b.kind else {
            unreachable!()
        };
        return branch(branch(a.clone(), c.clone()), branch(d.clone(), right));
    }
    if right.height > left.height + 1 {
        let Kind::Branch(a, b) = &right.kind else {
            unreachable!()
        };
        if b.height >= a.height {
            return branch(branch(left, a.clone()), b.clone());
        }
        let Kind::Branch(c, d) = &a.kind else {
            unreachable!()
        };
        return branch(branch(left, c.clone()), branch(d.clone(), b.clone()));
    }
    branch(left, right)
}
fn join(left: Option<Arc<Node>>, right: Option<Arc<Node>>) -> Option<Arc<Node>> {
    let (left, right) = match (left, right) {
        (None, r) => return r,
        (l, None) => return l,
        (Some(l), Some(r)) => (l, r),
    };
    if left.height > right.height + 1 {
        let Kind::Branch(a, b) = &left.kind else {
            unreachable!()
        };
        return Some(balance(
            a.clone(),
            join(Some(b.clone()), Some(right)).unwrap(),
        ));
    }
    if right.height > left.height + 1 {
        let Kind::Branch(a, b) = &right.kind else {
            unreachable!()
        };
        return Some(balance(
            join(Some(left), Some(a.clone())).unwrap(),
            b.clone(),
        ));
    }
    Some(branch(left, right))
}
fn split(root: Option<Arc<Node>>, byte: usize) -> (Option<Arc<Node>>, Option<Arc<Node>>) {
    let Some(node) = root else {
        assert_eq!(byte, 0);
        return (None, None);
    };
    if byte == 0 {
        return (None, Some(node));
    }
    if byte == node.bytes {
        return (Some(node), None);
    }
    assert!(byte < node.bytes);
    match &node.kind {
        Kind::Leaf {
            boundaries: bounds,
            newlines,
        } => {
            // Every caller splits after LF. Both new leaves have the same
            // grapheme boundaries as the original; no segmentation is needed.
            let at = bounds.binary_search(&byte).expect("LF seam");
            let nl = newlines.partition_point(|&at| at < byte);
            let left = Arc::new(Node {
                bytes: byte,
                newlines: nl,
                height: 1,
                kind: Kind::Leaf {
                    boundaries: bounds[..=at].to_vec(),
                    newlines: newlines[..nl].to_vec(),
                },
            });
            let right = Arc::new(Node {
                bytes: node.bytes - byte,
                newlines: newlines.len() - nl,
                height: 1,
                kind: Kind::Leaf {
                    boundaries: bounds[at..].iter().map(|b| b - byte).collect(),
                    newlines: newlines[nl..].iter().map(|b| b - byte).collect(),
                },
            });
            (Some(left), Some(right))
        }
        Kind::Branch(left, right) => {
            if byte < left.bytes {
                let (a, b) = split(Some(left.clone()), byte);
                (a, join(b, Some(right.clone())))
            } else {
                let (a, b) = split(Some(right.clone()), byte - left.bytes);
                (join(Some(left.clone()), a), b)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sparse_edits_reuse_indexes_and_match_full_unicode_segmentation() {
        for rows in [700, 12000] {
            let mut text = "e\u{301} אבג 👩\u{200d}💻 🇺🇸\r\n".repeat(rows);
            let mut index = Index::new(&text);
            INDEXED_BYTES.with(|n| n.set(0));
            let _cold = Index::new(&text);
            assert_eq!(
                INDEXED_BYTES.with(|n| n.get()),
                text.len(),
                "cold positive control"
            );
            for (at, removed, added) in [
                (0, 0, "x"),
                (text.len() / 2, 0, "中\n"),
                (0, 1, ""),
                (0, 0, "\r\n"),
            ] {
                let at = (at..text.len())
                    .find(|&at| text.is_char_boundary(at))
                    .unwrap();
                let mut next = text.clone();
                next.replace_range(at..at + removed, added);
                INDEXED_BYTES.with(|n| n.set(0));
                let updated = index.update(&text, &next);
                assert!(
                    INDEXED_BYTES.with(|n| n.get()) < 256,
                    "only edited lines segmented"
                );
                let expected: Vec<_> = next
                    .grapheme_indices(true)
                    .map(|(at, _)| at)
                    .chain(std::iter::once(next.len()))
                    .collect();
                for at in 0..=next.len() {
                    assert_eq!(
                        updated.binary_search(&at).is_ok(),
                        expected.binary_search(&at).is_ok(),
                        "byte {at}"
                    );
                }
                let newlines: Vec<_> = next.match_indices('\n').map(|(at, _)| at).collect();
                for (line, &at) in newlines.iter().enumerate() {
                    assert_eq!(updated.line_at(at), line);
                    assert_eq!(updated.line_at(at + 1), line + 1);
                    assert_eq!(updated.line_start(line + 1), Some(at + 1));
                }
                assert_eq!(updated.line_start(newlines.len() + 1), None);
                // An earlier immutable index remains usable after adoption.
                assert!(index.binary_search(&text.len()).is_ok());
                text = next;
                index = updated;
            }
        }
    }
}
