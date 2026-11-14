//! A byte-bounded set of exact native layouts, with ASCII-only incremental work.
//! Native word boundaries retain complete ligature/shaping context.
use super::*;

pub(super) const MIN_TEXT: usize = 64 * 1024;
pub(super) const CACHE_MIN_TEXT: usize = 32 * 1024;
const MAX_TEXT: usize = 4 * 1024 * 1024;
pub(super) const MAX_BYTES: usize = 160 * 1024 * 1024;
const MAX_RESHAPE: usize = 64 * 1024;

/// Spare glyph capacity reserved when an edit outgrows an entry, so typing
/// edits the retained layout in place instead of reallocating it.
const GLYPH_SLACK: usize = 16 * 1024;

#[derive(Clone)]
pub(super) struct LargeLine {
    text: Arc<String>,
    font_size: Pixels,
    font: FontId,
    runs: Arc<[FontRun]>,
    layout: Arc<LineLayout>,
    native: Option<Arc<native_ascii::NativeAscii>>,
    pub(super) incremental: bool,
    pub(super) bytes: usize,
}

pub(super) fn eligible(text: &str, runs: &[FontRun]) -> bool {
    (MIN_TEXT..=MAX_TEXT).contains(&text.len())
        && runs.len() == 1
        && runs[0].len == text.len()
        && text.bytes().all(|byte| (b' '..=b'~').contains(&byte))
}

/// The bytes an entry accounts for.
fn line_bytes(text: &str, runs: &[FontRun], layout: &LineLayout) -> usize {
    entry_bytes(text, runs, layout)
        + size_of::<LargeLine>()
        + size_of::<LineLayout>()
        + 6 * size_of::<usize>()
}

impl LargeLine {
    pub(super) fn new(
        text: &str,
        font_size: Pixels,
        runs: &[FontRun],
        layout: &LineLayout,
        shaper: &dyn PlatformTextSystem,
    ) -> Option<Self> {
        let bytes = line_bytes(text, runs, layout);
        if bytes > MAX_BYTES {
            return None;
        }
        let native = (eligible(text, runs)
            && layout.runs.iter().all(|run| run.font_id == runs[0].font_id))
        .then(|| {
            native_ascii::NativeAscii::prepare(
                text,
                font_size,
                runs[0].font_id,
                layout,
                None,
                shaper,
            )
            .map(Arc::new)
        })
        .flatten();
        let native = native.filter(|data| bytes + data.bytes() <= MAX_BYTES);
        let bytes = bytes + native.as_ref().map_or(0, |data| data.bytes());
        let incremental = native.is_some();
        Some(Self {
            text: Arc::new(text.into()),
            font_size,
            font: runs.first().map_or(FontId(0), |r| r.font_id),
            runs: runs.into(),
            layout: Arc::new(copy_layout(layout)),
            native,
            incremental,
            bytes,
        })
    }

    pub(super) fn matches(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> bool {
        self.font_size == font_size && self.runs.as_ref() == runs && self.text.as_str() == text
    }

    pub(super) fn copy_layout(&self) -> LineLayout {
        super::copy_layout(&self.layout)
    }

    /// Edits this entry in place into the layout of `text`, which keeps
    /// `prefix` and `suffix` bytes of this entry's text. The returned layout
    /// is the only full copy made per keystroke; the retained one is spliced.
    /// None leaves nothing worth keeping: the caller shapes `text` itself.
    pub(super) fn reshape(
        mut self,
        text: &str,
        font_size: Pixels,
        runs: &[FontRun],
        (prefix, suffix): (usize, usize),
        shaper: &dyn PlatformTextSystem,
    ) -> Option<(LineLayout, Option<Self>)> {
        if !self.incremental || self.font_size != font_size || self.font != runs[0].font_id {
            return None;
        }
        // Borrowed, not cloned: the in-place edit below must own it uniquely.
        let native = self.native.as_deref()?;
        let old_len = self.text.len();
        let left_at = native
            .boundaries
            .partition_point(|end| *end <= prefix)
            .saturating_sub(2);
        let left = native.boundaries[left_at];
        let old_end = old_len - suffix;
        let right_at = (native.boundaries.partition_point(|end| *end < old_end) + 1)
            .min(native.boundaries.len() - 1);
        let right = native.boundaries[right_at];
        let new_right = text.len().checked_sub(old_len - right)?;
        let middle = text.get(left..new_right)?;
        if middle.len() > MAX_RESHAPE {
            return None;
        }
        let middle_layout = shaper.layout_line(
            middle,
            font_size,
            &[FontRun {
                len: middle.len(),
                font_id: self.font,
            }],
        );
        if middle_layout
            .runs
            .iter()
            .any(|run| run.font_id != self.font)
            || middle_layout.ascent != self.layout.ascent
            || middle_layout.descent != self.layout.descent
        {
            return None;
        }
        let middle_native = native_ascii::NativeAscii::prepare(
            middle,
            font_size,
            self.font,
            &middle_layout,
            Some(native),
            shaper,
        )?;
        let edit = native.plan_edit(
            &self.layout,
            text,
            (prefix, suffix, left_at),
            &middle_native,
            shaper,
            font_size,
            self.font,
        )?;
        Arc::make_mut(&mut self.text)
            .replace_range(prefix..old_end, &text[prefix..text.len() - suffix]);
        debug_assert_eq!(self.text.as_str(), text);
        let layout = Arc::make_mut(&mut self.layout);
        if layout.runs.len() != 1 {
            // Native batches of one font become one run on the first edit.
            let glyphs = layout
                .runs
                .iter()
                .flat_map(|r| r.glyphs.iter().cloned())
                .collect();
            layout.runs = vec![ShapedRun {
                font_id: self.font,
                glyphs,
            }];
        }
        let glyphs = &mut layout.runs[0].glyphs;
        let from = glyphs.partition_point(|g| g.index < left);
        let to = glyphs.partition_point(|g| g.index < right);
        let inserted: Vec<_> = middle_layout
            .runs
            .iter()
            .flat_map(|run| &run.glyphs)
            .map(|glyph| ShapedGlyph {
                index: glyph.index + left,
                ..glyph.clone()
            })
            .collect();
        let count = inserted.len();
        let grow = count.saturating_sub(to - from);
        if glyphs.capacity() - glyphs.len() < grow {
            glyphs.reserve_exact(grow + GLYPH_SLACK);
        }
        glyphs.splice(from..to, inserted).for_each(drop);
        for glyph in &mut glyphs[from + count..] {
            glyph.index = new_right + (glyph.index - right);
        }
        layout.len = text.len();
        Arc::make_mut(self.native.as_mut()?).apply_edit(edit, layout, &middle_native);
        // Every test edit is checked against the complete derivation.
        #[cfg(test)]
        self.native.as_ref()?.assert_matches_full_replay(
            text,
            &self.layout,
            shaper,
            font_size,
            self.font,
        );
        self.runs = runs.into();
        let native_bytes = self.native.as_ref().map_or(0, |data| data.bytes());
        self.bytes = line_bytes(text, runs, &self.layout) + native_bytes;
        let layout = self.copy_layout();
        Some((layout, (self.bytes <= MAX_BYTES).then_some(self)))
    }
}

/// Length of the common prefix, compared a block at a time.
fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    const BLOCK: usize = 4096;
    let n = a.len().min(b.len());
    let mut at = 0;
    while at + BLOCK <= n && a[at..at + BLOCK] == b[at..at + BLOCK] {
        at += BLOCK;
    }
    at + a[at..n]
        .iter()
        .zip(&b[at..n])
        .take_while(|(x, y)| x == y)
        .count()
}

/// Length of the common suffix, compared a block at a time.
fn common_suffix(a: &[u8], b: &[u8]) -> usize {
    const BLOCK: usize = 4096;
    let n = a.len().min(b.len());
    let mut at = 0;
    while at + BLOCK <= n
        && a[a.len() - at - BLOCK..a.len() - at] == b[b.len() - at - BLOCK..b.len() - at]
    {
        at += BLOCK;
    }
    at + a[..a.len() - at]
        .iter()
        .rev()
        .zip(b[..b.len() - at].iter().rev())
        .take_while(|(x, y)| x == y)
        .count()
}

#[derive(Default)]
pub(super) struct LargeLines {
    pub(super) entries: VecDeque<LargeLine>,
    pub(super) bytes: usize,
}
impl LargeLines {
    pub(super) fn exact(
        &mut self,
        text: &str,
        size: Pixels,
        runs: &[FontRun],
    ) -> Option<LargeLine> {
        let at = self
            .entries
            .iter()
            .position(|e| e.matches(text, size, runs))?;
        let entry = self.entries.remove(at)?;
        self.entries.push_back(entry.clone());
        Some(entry)
    }
    /// Removes the retained incremental entry this text is an edit of, with
    /// the unchanged prefix and suffix byte counts.
    pub(super) fn take_candidate(
        &mut self,
        text: &str,
        size: Pixels,
        runs: &[FontRun],
    ) -> Option<(LargeLine, (usize, usize))> {
        let (at, edit) = self.entries.iter().enumerate().rev().find_map(|(at, e)| {
            if !e.incremental
                || e.font_size != size
                || runs.len() != 1
                || e.font != runs[0].font_id
                || e.text.len().abs_diff(text.len()) > MAX_RESHAPE
            {
                return None;
            }
            let old = e.text.as_bytes();
            let new = text.as_bytes();
            let prefix = common_prefix(old, new);
            let suffix = common_suffix(&old[prefix..], &new[prefix..]);
            (old.len().max(new.len()) - prefix - suffix <= MAX_RESHAPE)
                .then_some((at, (prefix, suffix)))
        })?;
        let entry = self.entries.remove(at)?;
        self.bytes -= entry.bytes;
        Some((entry, edit))
    }
    pub(super) fn retire_incremental_for(&mut self, text: &str) {
        // Retire ASCII edit state on a Unicode transition. Immutable exact
        // entries for other source rows remain valid across cache-hit frames.
        for entry in &mut self.entries {
            if !entry.incremental {
                continue;
            }
            let old = entry.text.as_bytes();
            let new = text.as_bytes();
            let prefix = common_prefix(old, new);
            let suffix = common_suffix(&old[prefix..], &new[prefix..]);
            if old.len().saturating_sub(prefix + suffix) <= MAX_RESHAPE {
                entry.incremental = false;
            }
        }
    }
    pub(super) fn insert(&mut self, entry: Option<LargeLine>) {
        let Some(entry) = entry else {
            return;
        };
        while self.bytes + entry.bytes > MAX_BYTES || self.entries.len() >= 128 {
            let Some(old) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= old.bytes;
        }
        self.bytes += entry.bytes;
        self.entries.push_back(entry);
    }
}
