//! Native shaping spike only: not wired into the editor; no visual acceptance.
//! Candidate for a reviewed gpui-kit paint seam; no gpui-core modifications.
use gpui::{Pixels, ShapedLine};
use std::ops::Range;
use tessera_core::{source_classifier::RevealSnapshot, source_projection::Snapshot};
use unicode_segmentation::UnicodeSegmentation;

/// Thin read-only adapter over the shared decision; no live caret input.
pub struct MarkerPolicy<'a> {
    pub reveal: &'a RevealSnapshot,
    pub current: &'a Snapshot,
}
impl MarkerPolicy<'_> {
    pub fn is_raw(&self, scope: &Range<usize>) -> bool {
        self.reveal.is_raw(self.current, scope)
    }
}

/// Caller supplies a single existing visual row, its original paint origin and
/// marker offsets mapped through the same immutable LastLayout. No second wrap.
/// `x` is relative to that ORIGINAL row origin (including existing alignment).
/// Empty pieces mean the whole row is suppressed, NOT a request for raw fallback.
pub struct ForegroundPiece {
    pub x: Pixels,
    pub source_start: usize,
    pub shaped: ShapedLine,
}

/// Paint-only candidate: split existing glyph runs; never call shape_line.
/// Call only AFTER exact snapshot, same-prepaint LayoutStamp, policy and mapping
/// validation. None means retain original foreground. Background/selection paint
/// is untouched. Replacement paint is a separate step, clipped to marker bounds.
///
/// Conservative initial seam: reject non-monotone glyph indices (e.g. bidi) and
/// splits inside a glyph cluster. Do not infer cluster safety from UTF-8 alone.
/// Renderer must paint pieces with TextAlign::Left at original_origin + piece.x;
/// it must not align each piece independently or use piece widths for layout.
pub fn foreground_pieces(
    line: &ShapedLine,
    hidden: &[Range<usize>],
) -> Option<Vec<ForegroundPiece>> {
    if unicode_bidi::BidiInfo::new(&line.text, None).has_rtl() {
        return None;
    }
    let glyphs: Vec<_> = line.runs.iter().flat_map(|run| &run.glyphs).collect();
    if glyphs.windows(2).any(|pair| pair[0].index > pair[1].index) {
        return None;
    }
    let boundaries: Vec<_> = line
        .text
        .grapheme_indices(true)
        .map(|(i, _)| i)
        .chain(std::iter::once(line.text.len()))
        .collect();
    let boundary = |index: usize| {
        boundaries.binary_search(&index).is_ok()
            && (index == line.len() || glyphs.iter().any(|glyph| glyph.index == index))
    };
    let mut end = 0;
    for range in hidden {
        if range.start < end
            || range.start >= range.end
            || range.end > line.len()
            || !boundary(range.start)
            || !boundary(range.end)
        {
            return None;
        }
        end = range.end;
    }
    let mut pieces = Vec::new();
    let mut cursor = 0;
    for range in hidden {
        if cursor < range.start {
            let (_, suffix) = line.split_at(cursor);
            let (part, _) = suffix.split_at(range.start - cursor);
            pieces.push(ForegroundPiece {
                x: line.x_for_index(cursor),
                source_start: cursor,
                shaped: part,
            });
        }
        cursor = range.end;
    }
    if cursor < line.len() {
        let (_, part) = line.split_at(cursor);
        pieces.push(ForegroundPiece {
            x: line.x_for_index(cursor),
            source_start: cursor,
            shaped: part,
        });
    }
    Some(pieces)
}
