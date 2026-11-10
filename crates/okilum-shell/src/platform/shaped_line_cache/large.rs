//! A byte-bounded set of exact native layouts, with ASCII-only incremental work.
//! Native word boundaries retain complete ligature/shaping context.
use super::*;

pub(super) const MIN_TEXT: usize = 64 * 1024;
pub(super) const CACHE_MIN_TEXT: usize = 32 * 1024;
const MAX_TEXT: usize = 4 * 1024 * 1024;
pub(super) const MAX_BYTES: usize = 160 * 1024 * 1024;
const MAX_RESHAPE: usize = 64 * 1024;

#[derive(Clone)]
pub(super) struct LargeLine {
    text: Arc<str>,
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

impl LargeLine {
    pub(super) fn new(
        text: &str,
        font_size: Pixels,
        runs: &[FontRun],
        layout: &LineLayout,
        native: Option<Arc<native_ascii::NativeAscii>>,
        shaper: &dyn PlatformTextSystem,
    ) -> Option<Self> {
        let bytes = entry_bytes(text, runs, layout)
            + size_of::<Self>()
            + size_of::<LineLayout>()
            + 6 * size_of::<usize>();
        if bytes > MAX_BYTES {
            return None;
        }
        let native = native.or_else(|| {
            (eligible(text, runs) && layout.runs.iter().all(|run| run.font_id == runs[0].font_id))
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
                .flatten()
        });
        let native = native.filter(|data| bytes + data.bytes() <= MAX_BYTES);
        let bytes = bytes + native.as_ref().map_or(0, |data| data.bytes());
        let incremental = native.is_some();
        Some(Self {
            text: text.into(),
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
        self.font_size == font_size && self.runs.as_ref() == runs && self.text.as_ref() == text
    }

    pub(super) fn copy_layout(&self) -> LineLayout {
        super::copy_layout(&self.layout)
    }

    pub(super) fn reshape(
        &self,
        text: &str,
        font_size: Pixels,
        runs: &[FontRun],
        shaper: &dyn PlatformTextSystem,
    ) -> Option<(LineLayout, Arc<native_ascii::NativeAscii>)> {
        if !self.incremental || self.font_size != font_size || self.font != runs[0].font_id {
            return None;
        }
        let native = self.native.as_ref()?;
        if self.text.as_ref() == text {
            return Some((copy_layout(&self.layout), native.clone()));
        }
        let old = self.text.as_bytes();
        let new = text.as_bytes();
        let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let left_at = native
            .boundaries
            .partition_point(|end| *end <= prefix)
            .saturating_sub(2);
        let left = native.boundaries[left_at];
        let old_end = old.len() - suffix;
        let right_at = (native.boundaries.partition_point(|end| *end < old_end) + 1)
            .min(native.boundaries.len() - 1);
        let right = native.boundaries[right_at];
        let new_right = new.len().checked_sub(old.len() - right)?;
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
        let mut glyphs = Vec::with_capacity(
            self.layout
                .runs
                .iter()
                .map(|r| r.glyphs.len())
                .sum::<usize>()
                + middle_layout
                    .runs
                    .iter()
                    .map(|r| r.glyphs.len())
                    .sum::<usize>(),
        );
        for run in &self.layout.runs {
            let end = run.glyphs.partition_point(|glyph| glyph.index < left);
            glyphs.extend_from_slice(&run.glyphs[..end]);
        }
        for glyph in middle_layout.runs.iter().flat_map(|run| &run.glyphs) {
            let mut glyph = glyph.clone();
            glyph.index += left;
            glyphs.push(glyph);
        }
        for run in &self.layout.runs {
            let start = run.glyphs.partition_point(|glyph| glyph.index < right);
            let appended = glyphs.len();
            glyphs.extend_from_slice(&run.glyphs[start..]);
            for glyph in &mut glyphs[appended..] {
                glyph.index = new_right + (glyph.index - right);
            }
        }
        let mut layout = LineLayout {
            font_size,
            width: self.layout.width,
            ascent: self.layout.ascent,
            descent: self.layout.descent,
            runs: vec![ShapedRun {
                font_id: self.font,
                glyphs,
            }],
            len: text.len(),
        };
        let next = native_ascii::NativeAscii::for_glyphs(
            text,
            font_size,
            self.font,
            layout.runs.iter().flat_map(|run| &run.glyphs),
            Some(&middle_native),
            shaper,
        )?;
        next.reflow(&mut layout);
        Some((layout, Arc::new(next)))
    }
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
    pub(super) fn candidate(&self, size: Pixels, runs: &[FontRun]) -> Option<LargeLine> {
        self.entries
            .iter()
            .rev()
            .find(|e| {
                e.incremental && e.font_size == size && runs.len() == 1 && e.font == runs[0].font_id
            })
            .cloned()
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
            let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
            let suffix = old[prefix..]
                .iter()
                .rev()
                .zip(new[prefix..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
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
