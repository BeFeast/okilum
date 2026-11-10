//! A single bounded incremental layout for large plain ASCII source lines.
//! Context ends at spaces, so shaping never cuts a word/ligature or bidi run.
use super::*;

const MIN_TEXT: usize = 64 * 1024;
const MAX_TEXT: usize = 4 * 1024 * 1024;
pub(super) const MAX_BYTES: usize = 160 * 1024 * 1024;
const MAX_RESHAPE: usize = 64 * 1024;

#[derive(Clone)]
pub(super) struct LargeLine {
    text: Arc<str>,
    font_size: Pixels,
    font: FontId,
    layout: Arc<LineLayout>,
    #[cfg(test)]
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
    ) -> Option<Self> {
        let bytes = entry_bytes(text, runs, layout);
        if bytes > MAX_BYTES || layout.runs.iter().any(|run| run.font_id != runs[0].font_id) {
            return None;
        }
        let mut previous = None;
        for glyph in layout.runs.iter().flat_map(|run| &run.glyphs) {
            if previous.is_some_and(|index| index > glyph.index) {
                return None;
            }
            previous = Some(glyph.index);
        }
        Some(Self {
            text: text.into(),
            font_size,
            font: runs[0].font_id,
            layout: Arc::new(copy_layout(layout)),
            #[cfg(test)]
            bytes,
        })
    }

    pub(super) fn reshape(
        &self,
        text: &str,
        font_size: Pixels,
        runs: &[FontRun],
        shaper: &dyn PlatformTextSystem,
    ) -> Option<LineLayout> {
        if self.font_size != font_size || self.font != runs[0].font_id {
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
        let x_left = self.layout.x_for_index(left);
        let x_right = self.layout.x_for_index(right);
        let dx = middle_layout.width - (x_right - x_left);
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
        for glyph in self
            .layout
            .runs
            .iter()
            .flat_map(|run| &run.glyphs)
            .filter(|g| g.index < left)
        {
            glyphs.push(glyph.clone());
        }
        for glyph in middle_layout.runs.iter().flat_map(|run| &run.glyphs) {
            let mut glyph = glyph.clone();
            glyph.index += left;
            glyph.position.x += x_left;
            glyphs.push(glyph);
        }
        for glyph in self
            .layout
            .runs
            .iter()
            .flat_map(|run| &run.glyphs)
            .filter(|g| g.index >= right)
        {
            let mut glyph = glyph.clone();
            glyph.index = new_right + (glyph.index - right);
            glyph.position.x += dx;
            glyphs.push(glyph);
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
