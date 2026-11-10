//! A byte-bounded set of exact native layouts, with ASCII-only incremental work.
//! Context ends at spaces, so shaping never cuts a word/ligature or bidi run.
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
        verified_order: bool,
    ) -> Option<Self> {
        let bytes = entry_bytes(text, runs, layout)
            + size_of::<Self>()
            + size_of::<LineLayout>()
            + 6 * size_of::<usize>();
        if bytes > MAX_BYTES {
            return None;
        }
        let mut incremental =
            eligible(text, runs) && layout.runs.iter().all(|run| run.font_id == runs[0].font_id);
        if incremental && !verified_order {
            let mut previous = None;
            for glyph in layout.runs.iter().flat_map(|run| &run.glyphs) {
                if previous.is_some_and(|index| index > glyph.index) {
                    incremental = false;
                    break;
                }
                previous = Some(glyph.index);
            }
        }
        Some(Self {
            text: text.into(),
            font_size,
            font: runs.first().map_or(FontId(0), |r| r.font_id),
            runs: runs.into(),
            layout: Arc::new(copy_layout(layout)),
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
    ) -> Option<LineLayout> {
        if !self.incremental || self.font_size != font_size || self.font != runs[0].font_id {
            return None;
        }
        if self.text.as_ref() == text {
            return Some(copy_layout(&self.layout));
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
        let left = old[..prefix]
            .iter()
            .rposition(|byte| *byte == b' ')
            .map_or(0, |i| i + 1);
        let old_end = old.len() - suffix;
        let right = old[old_end..]
            .iter()
            .position(|byte| *byte == b' ')
            .map_or(old.len(), |i| old_end + i + 1);
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
        // Subtract local native advances, not two multi-million-pixel f32
        // coordinates whose subtraction can lose several pixels per edit.
        let old_middle = &self.text[left..right];
        let old_layout = shaper.layout_line(
            old_middle,
            font_size,
            &[FontRun {
                len: old_middle.len(),
                font_id: self.font,
            }],
        );
        let x_left = self.layout.x_for_index(left);
        let dx = middle_layout.width - old_layout.width;
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
            glyph.position.x += x_left;
            glyphs.push(glyph);
        }
        for run in &self.layout.runs {
            let start = run.glyphs.partition_point(|glyph| glyph.index < right);
            let appended = glyphs.len();
            glyphs.extend_from_slice(&run.glyphs[start..]);
            for glyph in &mut glyphs[appended..] {
                glyph.index = new_right + (glyph.index - right);
                glyph.position.x += dx;
            }
        }
        Some(LineLayout {
            font_size,
            width: self.layout.width + dx,
            ascent: self.layout.ascent,
            descent: self.layout.descent,
            runs: vec![ShapedRun {
                font_id: self.font,
                glyphs,
            }],
            len: text.len(),
        })
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
