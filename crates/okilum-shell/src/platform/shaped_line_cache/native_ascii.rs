//! Exact replay of native LTR advances, gated by the complete native layout.
//! A global delta is insufficient: f32 pen accumulation at the far end of a
//! huge line must run in the same order as the native shaper, not shift old Xs.
use super::*;

#[derive(Clone)]
pub(super) struct NativeAscii {
    pub(super) boundaries: Arc<[usize]>,
    advances: Arc<[f32]>,
    pairs: Arc<HashMap<[u8; 2], bool>>,
}

impl NativeAscii {
    pub(super) fn prepare(
        text: &str,
        size: Pixels,
        font: FontId,
        layout: &LineLayout,
        previous: Option<&Self>,
        shaper: &dyn PlatformTextSystem,
    ) -> Option<Self> {
        let result = Self::for_glyphs(
            text,
            size,
            font,
            layout.runs.iter().flat_map(|r| &r.glyphs),
            previous,
            shaper,
        )?;
        let mut copy = copy_layout(layout);
        result.reflow(&mut copy);
        // This admission check covers font-specific kerning, offsets, glyph
        // ordering, native word boundaries and the platform's accumulation.
        exact_geometry(&copy, layout).then_some(result)
    }

    pub(super) fn for_glyphs<'a>(
        text: &str,
        size: Pixels,
        font: FontId,
        glyphs: impl Iterator<Item = &'a ShapedGlyph> + Clone,
        previous: Option<&Self>,
        shaper: &dyn PlatformTextSystem,
    ) -> Option<Self> {
        let em = shaper.font_metrics(font).units_per_em as f32;
        if em == 0. {
            return None;
        }
        let maximum = glyphs.clone().map(|g| g.id.0 as usize).max().unwrap_or(0);
        if maximum > u16::MAX as usize {
            return None;
        }
        let mut advances = previous.map_or_else(Vec::new, |p| p.advances.to_vec());
        advances.resize(advances.len().max(maximum + 1), f32::NAN);
        for glyph in glyphs {
            let slot = &mut advances[glyph.id.0 as usize];
            if slot.is_nan() {
                *slot =
                    f32::from(size).mul_add(shaper.advance(font, glyph.id).ok()?.width / em, 0.);
            }
            if !slot.is_finite() || *slot < 0. {
                return None;
            }
        }
        let mut pairs = previous.map_or_else(HashMap::new, |p| (*p.pairs).clone());
        let mut boundaries = vec![0];
        let mut start = 0;
        let bytes = text.as_bytes();
        for (end, _) in unicode_linebreak::linebreaks(text) {
            if end > 0
                && end < bytes.len()
                && bytes[end - 1].is_ascii_punctuation()
                && bytes[end].is_ascii_punctuation()
            {
                let pair = [bytes[end - 1], bytes[end]];
                if let std::collections::hash_map::Entry::Vacant(entry) = pairs.entry(pair) {
                    let probe = shaper.layout_line(
                        &text[end - 1..end + 1],
                        size,
                        &[FontRun {
                            len: 2,
                            font_id: font,
                        }],
                    );
                    if probe.runs.iter().any(|r| r.font_id != font) {
                        return None;
                    }
                    let ids: Vec<_> = probe
                        .runs
                        .iter()
                        .flat_map(|r| &r.glyphs)
                        .map(|g| g.id)
                        .collect();
                    let standard = [
                        shaper.glyph_for_char(font, pair[0] as char)?,
                        shaper.glyph_for_char(font, pair[1] as char)?,
                    ];
                    entry.insert(ids.len() < 2 || (ids.len() == 2 && ids.as_slice() != standard));
                }
                if pairs[&pair] {
                    continue;
                }
            }
            let mut nonblank_end = end;
            while nonblank_end > start && bytes[nonblank_end - 1] == b' ' {
                nonblank_end -= 1;
            }
            if start < nonblank_end {
                boundaries.push(nonblank_end);
            }
            // The native engine treats each trailing ASCII space as a word.
            boundaries.extend(nonblank_end + 1..=end);
            start = end;
        }
        if boundaries.last().copied() != Some(text.len()) {
            return None;
        }
        Some(Self {
            boundaries: boundaries.into(),
            advances: advances.into(),
            pairs: Arc::new(pairs),
        })
    }

    pub(super) fn reflow(&self, layout: &mut LineLayout) {
        let mut pen = 0f32;
        let mut width = 0f32;
        let mut glyphs = layout
            .runs
            .iter_mut()
            .flat_map(|r| &mut r.glyphs)
            .peekable();
        for &end in self.boundaries.iter().skip(1) {
            let mut word_width = 0f32;
            while glyphs.peek().is_some_and(|g| g.index < end) {
                let glyph = glyphs.next().unwrap();
                glyph.position.x = gpui::px(pen);
                let advance = self.advances[glyph.id.0 as usize];
                pen += advance;
                word_width += advance;
            }
            width += word_width;
        }
        layout.width = gpui::px(width);
    }

    pub(super) fn bytes(&self) -> usize {
        size_of::<Self>()
            + size_of_val(self.boundaries.as_ref())
            + size_of_val(self.advances.as_ref())
            + self.pairs.capacity() * (size_of::<([u8; 2], bool)>() + size_of::<usize>() * 2)
    }
}

pub(super) fn exact_geometry(a: &LineLayout, b: &LineLayout) -> bool {
    a.font_size == b.font_size
        && a.width == b.width
        && a.ascent == b.ascent
        && a.descent == b.descent
        && a.len == b.len
        && a.runs.len() == b.runs.len()
        && a.runs.iter().zip(&b.runs).all(|(a, b)| {
            a.font_id == b.font_id
                && a.glyphs.len() == b.glyphs.len()
                && a.glyphs.iter().zip(&b.glyphs).all(|(a, b)| {
                    a.id == b.id
                        && a.index == b.index
                        && a.position == b.position
                        && a.is_emoji == b.is_emoji
                })
        })
}
