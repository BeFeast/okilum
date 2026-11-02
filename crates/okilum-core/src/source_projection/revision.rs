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
        (at < self.len()).then(|| self.0.rope.byte(at))
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
        self.same(other) || self.0.rope == other.0.rope
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
