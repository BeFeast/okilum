//! Logical UTF-8 selection projected through measured visual cluster cells.
//! No bidi reordering or font measurements are guessed here.
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Rect {
    pub fn is_valid(self) -> bool {
        [self.left, self.top, self.right, self.bottom]
            .iter()
            .all(|n| n.is_finite())
            && self.left <= self.right
            && self.top <= self.bottom
    }

    pub fn intersection(self, other: Self) -> Option<Self> {
        let rect = Self {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        };
        (rect.left < rect.right && rect.top < rect.bottom).then_some(rect)
    }
}

/// A shaped cluster's source bytes and actual visual bounds in viewport coordinates.
/// Wrapped clusters must be split into their measured row fragments by the adapter.
#[derive(Clone, Debug)]
pub struct VisualCell {
    pub source: Range<usize>,
    pub bounds: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HighlightError {
    InvalidRange,
    InvalidGeometry,
    CellBudgetExceeded,
    RectangleBudgetExceeded,
    MetricsUnavailable,
}

/// Copy preserves logical order and every authored byte, including controls/newlines.
pub fn copy_source<'a>(source: &'a str, range: Range<usize>) -> Result<&'a str, HighlightError> {
    source.get(range).ok_or(HighlightError::InvalidRange)
}

/// Returns pairwise interior-disjoint rectangles, clipped on all four sides.
/// A partial cluster is painted in full; copy still uses the exact requested bytes.
/// Budgets fail explicitly rather than returning a plausible incomplete highlight.
pub fn visual_rectangles(
    source: &str,
    range: Range<usize>,
    viewport: Rect,
    cells: impl IntoIterator<Item = VisualCell>,
    max_cells: usize,
    max_rectangles: usize,
) -> Result<Vec<Rect>, HighlightError> {
    copy_source(source, range.clone())?;
    if !viewport.is_valid() {
        return Err(HighlightError::InvalidGeometry);
    }
    if range.is_empty() {
        return Ok(Vec::new());
    }
    let mut clipped = Vec::new();
    for (count, cell) in cells.into_iter().enumerate() {
        if count >= max_cells {
            return Err(HighlightError::CellBudgetExceeded);
        }
        if !cell.bounds.is_valid() {
            return Err(HighlightError::InvalidGeometry);
        }
        if cell.source.is_empty() || copy_source(source, cell.source.clone()).is_err() {
            return Err(HighlightError::InvalidRange);
        }
        if cell.source.start < range.end && range.start < cell.source.end {
            if let Some(rect) = cell.bounds.intersection(viewport) {
                clipped.push(rect);
            }
        }
    }
    // Sweep horizontal bands: unlike a start/end caret box this preserves holes
    // between selected bidi runs, and also removes overlapping cluster ink boxes.
    let mut edges: Vec<_> = clipped.iter().flat_map(|r| [r.top, r.bottom]).collect();
    edges.sort_by(f32::total_cmp);
    edges.dedup();
    let mut result = Vec::new();
    for ys in edges.windows(2) {
        let mut spans: Vec<_> = clipped
            .iter()
            .filter(|r| r.top <= ys[0] && r.bottom >= ys[1])
            .map(|r| (r.left, r.right))
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut merged: Vec<(f32, f32)> = Vec::new();
        for (left, right) in spans {
            if let Some(last) = merged.last_mut().filter(|last| left <= last.1) {
                last.1 = last.1.max(right);
            } else {
                merged.push((left, right));
            }
        }
        for (left, right) in merged {
            if result.len() >= max_rectangles {
                return Err(HighlightError::RectangleBudgetExceeded);
            }
            result.push(Rect {
                left,
                right,
                top: ys[0],
                bottom: ys[1],
            });
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(left: f32, top: f32, right: f32, bottom: f32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }
    fn cell(source: Range<usize>, left: f32, right: f32) -> VisualCell {
        VisualCell {
            source,
            bounds: rect(left, 0., right, 10.),
        }
    }
    #[test]
    fn mixed_direction_selection_keeps_visual_hole_and_exact_copy() {
        let text = "ab אב cd";
        // Logical selection b/space/alef; bet is visually between the selected runs.
        let cells = vec![
            cell(0..1, 0., 10.),
            cell(1..2, 10., 20.),
            cell(2..3, 20., 30.),
            cell(3..5, 40., 50.),
            cell(5..7, 30., 40.),
        ];
        assert_eq!(copy_source(text, 1..5), Ok("b א"));
        assert_eq!(
            visual_rectangles(text, 1..5, rect(15., 2., 48., 8.), cells, 8, 8),
            Ok(vec![rect(15., 2., 30., 8.), rect(40., 2., 48., 8.)])
        );
    }
    #[test]
    fn cluster_overlap_clips_without_double_painting() {
        let cells = vec![cell(0..2, -5., 15.), cell(2..3, 10., 25.)];
        assert_eq!(
            visual_rectangles("éx", 0..3, rect(0., 0., 20., 10.), cells, 2, 1),
            Ok(vec![rect(0., 0., 20., 10.)])
        );
        assert_eq!(copy_source("éx", 1..2), Err(HighlightError::InvalidRange));
    }
    #[test]
    fn partial_combining_cluster_paints_once_but_copies_only_requested_bytes() {
        let source = "a\u{301}b";
        let cells = [cell(0..3, 0., 10.), cell(3..4, 10., 20.)];
        assert_eq!(copy_source(source, 1..3), Ok("\u{301}"));
        assert_eq!(
            visual_rectangles(source, 1..3, rect(0., 0., 20., 10.), cells, 2, 2),
            Ok(vec![rect(0., 0., 10., 10.)])
        );
    }
    #[test]
    fn empty_offscreen_and_budget_have_distinct_results() {
        let viewport = rect(0., 0., 20., 10.);
        assert_eq!(
            visual_rectangles("x", 0..1, viewport, [cell(0..1, 30., 40.)], 1, 1),
            Ok(vec![])
        );
        assert_eq!(
            visual_rectangles("x", 0..1, viewport, [cell(0..1, 0., 10.)], 0, 1),
            Err(HighlightError::CellBudgetExceeded)
        );
        assert_eq!(
            visual_rectangles("x", 0..1, viewport, [cell(0..1, 0., 10.)], 1, 0),
            Err(HighlightError::RectangleBudgetExceeded)
        );
    }
}
