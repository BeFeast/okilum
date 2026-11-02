//! Persistent byte text. Flattening is an explicit compatibility operation for
//! parsers; input/paint code reads bounded slices and indexed line coordinates.
use ropey::{LineType, Rope};
use std::{
    borrow::Cow,
    ops::{Deref, Range},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, OnceLock,
    },
};

#[derive(Clone, Debug)]
pub struct Text(Arc<Inner>);
#[derive(Debug)]
struct Inner {
    id: u64,
    rope: Rope,
    flat: OnceLock<Arc<str>>,
    ancestors: Vec<Edit>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub previous: u64,
    pub old: Range<usize>,
    pub inserted: usize,
}
impl Edit {
    pub fn unchanged(previous: u64, len: usize) -> Self {
        Self {
            previous,
            old: len..len,
            inserted: 0,
        }
    }
    pub fn new_end(&self) -> usize {
        self.old.start + self.inserted
    }
    pub fn forward(&self, at: usize) -> usize {
        if at < self.old.start {
            at
        } else if at >= self.old.end {
            at - self.old.end + self.new_end()
        } else {
            self.old.start
        }
    }
    pub fn backward(&self, at: usize) -> usize {
        if at < self.old.start {
            at
        } else if at >= self.new_end() {
            at - self.new_end() + self.old.end
        } else {
            self.old.start
        }
    }
}
impl Text {
    pub fn from_rope(rope: Rope) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(Arc::new(Inner {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            rope,
            flat: OnceLock::new(),
            ancestors: Vec::new(),
        }))
    }
    pub fn from_identified_rope(rope: Rope, id: u64) -> Self {
        Self(Arc::new(Inner {
            id,
            rope,
            flat: OnceLock::new(),
            ancestors: Vec::new(),
        }))
    }
    pub fn edit_since(&self, previous: u64) -> Option<Edit> {
        if self.id() == previous {
            return Some(Edit::unchanged(previous, self.len()));
        }
        self.0
            .ancestors
            .iter()
            .find(|e| e.previous == previous)
            .cloned()
    }
    pub fn replace(&self, old: Range<usize>, replacement: &str) -> Self {
        charge(|w| w.display_bytes += replacement.len());
        let mut rope = self.rope().clone();
        rope.remove(old.clone());
        rope.insert(old.start, replacement);
        let edit = Edit {
            previous: self.id(),
            old,
            inserted: replacement.len(),
        };
        let mut result = Self::from_rope(rope);
        let inner = Arc::get_mut(&mut result.0).unwrap();
        inner.ancestors.push(edit.clone());
        for ancestor in self.0.ancestors.iter().take(31) {
            let start = ancestor.old.start.min(ancestor.backward(edit.old.start));
            let end = ancestor.old.end.max(ancestor.backward(edit.old.end));
            let inserted = end - start + ancestor.inserted + edit.inserted
                - ancestor.old.len()
                - edit.old.len();
            inner.ancestors.push(Edit {
                previous: ancestor.previous,
                old: start..end,
                inserted,
            });
        }
        result
    }
    pub fn id(&self) -> u64 {
        self.0.id
    }
    pub fn same(&self, other: &Self) -> bool {
        self.id() == other.id()
            && ropey::extra::esoterica::ropes_are_instances(self.rope(), other.rope())
    }
    pub fn rope(&self) -> &Rope {
        &self.0.rope
    }
    pub fn len(&self) -> usize {
        self.0.rope.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn byte(&self, at: usize) -> Option<u8> {
        (at < self.len()).then(|| {
            charge(|w| w.scanned_bytes += 1);
            self.0.rope.byte(at)
        })
    }
    pub fn is_char_boundary(&self, at: usize) -> bool {
        at <= self.len() && self.0.rope.is_char_boundary(at)
    }
    pub fn slice(&self, range: Range<usize>) -> Cow<'_, str> {
        charge(|w| w.scanned_bytes += range.len());
        let slice = self.0.rope.slice(range);
        if let Some(text) = slice.as_str() {
            Cow::Borrowed(text)
        } else {
            Cow::Owned(slice.to_string())
        }
    }
    pub fn get(&self, range: Range<usize>) -> Option<Cow<'_, str>> {
        (range.start <= range.end
            && self.is_char_boundary(range.start)
            && self.is_char_boundary(range.end))
        .then(|| self.slice(range))
    }
    pub fn line_at(&self, at: usize) -> usize {
        self.0
            .rope
            .byte_to_line_idx(at.min(self.len()), LineType::LF)
    }
    pub fn line_start(&self, line: usize) -> usize {
        self.0.rope.line_to_byte_idx(line, LineType::LF)
    }
    pub fn line_bounds(&self, at: usize) -> Range<usize> {
        let row = self.line_at(at);
        let start = self.line_start(row);
        let end = if row + 1 < self.0.rope.len_lines(LineType::LF) {
            self.line_start(row + 1)
        } else {
            self.len()
        };
        start..end
    }
}
impl From<Arc<str>> for Text {
    fn from(text: Arc<str>) -> Self {
        let result = Self::from_rope(Rope::from_str(&text));
        result.0.flat.set(text).ok();
        result
    }
}
impl Deref for Text {
    type Target = str;
    fn deref(&self) -> &str {
        self.0.flat.get_or_init(|| {
            charge(|w| w.flattened_bytes += self.len());
            self.0.rope.to_string().into()
        })
    }
}
impl PartialEq for Text {
    fn eq(&self, other: &Self) -> bool {
        if self.same(other) {
            return true;
        }
        charge(|w| w.scanned_bytes += self.len().min(other.len()));
        self.0.rope == other.0.rope
    }
}
impl Eq for Text {}

impl PartialEq<str> for Text {
    fn eq(&self, other: &str) -> bool {
        self.rope() == other
    }
}
impl PartialEq<&str> for Text {
    fn eq(&self, other: &&str) -> bool {
        self.rope() == *other
    }
}
impl PartialEq<String> for Text {
    fn eq(&self, other: &String) -> bool {
        self.rope() == other.as_str()
    }
}

/// Opt-in deterministic work accounting, shared by native-adapter regression
/// tests. Disabled counters cost no byte traversal and never affect authority.
#[derive(Clone, Copy, Default, Debug)]
pub struct Work {
    pub flattened_bytes: usize,
    pub scanned_bytes: usize,
    pub display_builds: usize,
    pub display_bytes: usize,
}
thread_local! { static WORK: std::cell::Cell<Option<Work>> = const {std::cell::Cell::new(None)}; }
pub fn reset_work() {
    WORK.with(|w| w.set(Some(Work::default())));
}
pub fn take_work() -> Work {
    WORK.with(|w| w.replace(None).unwrap_or_default())
}
pub(crate) fn charge(update: impl FnOnce(&mut Work)) {
    WORK.with(|w| {
        if let Some(mut work) = w.get() {
            update(&mut work);
            w.set(Some(work));
        }
    });
}

#[cfg(test)]
mod acceptance_tests {
    use super::*;
    use crate::{
        source_classifier::{classify, RetainedPresentation},
        source_projection::Snapshot,
    };
    #[test]
    fn marker_proofs_reuse_unchanged_bytes_but_reject_changed_protected_source() {
        use crate::source_projection::{
            project_reusing, Active, FallbackReason, Plan, Region, MAX_WORK,
        };
        let text = Text::from_rope(Rope::from_str("a*b*c"));
        let original = Snapshot::from_rope("marker-proof", 1, text.clone(), None);
        let plan = |snapshot: &Snapshot, marker: Range<usize>| {
            Plan::new(
                snapshot,
                vec![Region::Conceal {
                    block: 0..snapshot.text().len(),
                    markers: vec![marker],
                }],
            )
        };
        let first = project_reusing(
            &original,
            &plan(&original, 1..2),
            &Active::default(),
            MAX_WORK,
            None,
        )
        .unwrap();
        for replacement in ["\n", "\r", "\u{feff}"] {
            let changed = text.replace(1..2, replacement);
            let snapshot = Snapshot::from_rope(
                "marker-proof",
                2,
                changed,
                Some(Edit {
                    previous: text.id(),
                    old: 1..2,
                    inserted: replacement.len(),
                }),
            );
            let result = project_reusing(
                &snapshot,
                &plan(&snapshot, 1..1 + replacement.len()),
                &Active::default(),
                MAX_WORK,
                Some(&first),
            );
            assert_eq!(result.unwrap_err().reason, FallbackReason::ProtectedSource);
        }
        let changed = text.replace(4..5, "q");
        let snapshot = Snapshot::from_rope(
            "marker-proof",
            2,
            changed,
            Some(Edit {
                previous: text.id(),
                old: 4..5,
                inserted: 1,
            }),
        );
        reset_work();
        let next = project_reusing(
            &snapshot,
            &plan(&snapshot, 1..2),
            &Active::default(),
            MAX_WORK,
            Some(&first),
        )
        .unwrap();
        let work = take_work();
        assert_eq!(next.display(), "ab*q");
        assert_eq!(work.flattened_bytes, 0);
        assert_eq!(work.display_builds, 0);
    }

    #[test]
    fn issue2403_single_logical_line_reports_line_proportional_work() {
        for bytes in [25 * 1024, 130 * 1024, 1024 * 1024] {
            let text = Text::from_rope(Rope::from_str(&"a".repeat(bytes)));
            let original = Snapshot::from_rope("single-line", 1, text.clone(), None);
            reset_work();
            let retained = RetainedPresentation::new(&classify(&original));
            let cold = take_work();
            assert!(
                cold.flattened_bytes >= bytes && cold.display_builds > 0,
                "cold positive controls: {cold:?}"
            );
            let at = bytes / 2;
            let changed = text.replace(at..at + 1, "q");
            let current = Snapshot::from_rope(
                "single-line",
                2,
                changed,
                Some(Edit {
                    previous: text.id(),
                    old: at..at + 1,
                    inserted: 1,
                }),
            );
            reset_work();
            let next = retained
                .remap(&current)
                .expect("ordinary inline edit remains retained");
            let work = take_work();
            assert_eq!(next.snapshot(), &current);
            println!("SINGLE_LINE_KEY_WORK bytes={bytes} work={work:?}");
            assert_eq!(work.flattened_bytes, 0);
            assert_eq!(work.display_builds, 0);
            // Owner decision 44461: segmentation inside a logical line is
            // deliberately outside #2403. Keep this diagnostic visible without
            // applying the ordinary-line scan budget to a one-line note.
            assert!(work.scanned_bytes >= bytes);
        }
    }
}
