//! Reader integration boundary for actual GPUI layout metrics.
//!
//! Parent hooks: export core `bidi_highlight`, declare this module, implement
//! `ReaderHighlightMetrics` at the retained GPUI shaped-layout owner, and paint
//! returned viewport-space rectangles for find and selection. Clipboard uses
//! `exact_copy`, never visual cells or rendered text. No native acceptance implied.
use okilum_core::bidi_highlight::{self, HighlightError, Rect, VisualCell};
use std::ops::Range;

/// Must enumerate every shaped cluster fragment intersecting `viewport` using
/// the SAME layout, scroll offset, wrapping and alignment used for painting.
/// Source ranges are document-global UTF-8 bytes (including rendered/source
/// mapping where needed), not visual glyph order. No caret-endpoint estimates.
/// Missing/stale layouts or unavailable lossless source mapping must return
/// MetricsUnavailable. Implementations stop at `limit` and fail on overflow;
/// they must not truncate silently or measure offscreen document content.
pub trait ReaderHighlightMetrics {
    fn visible_cells(
        &self,
        viewport: Rect,
        limit: usize,
    ) -> Result<Vec<VisualCell>, HighlightError>;
}

pub struct ReaderHighlight<'a> {
    pub source: &'a str,
    pub logical_range: Range<usize>,
    pub viewport: Rect,
    pub max_cells: usize,
    pub max_rectangles: usize,
}

impl<'a> ReaderHighlight<'a> {
    pub fn exact_copy(&self) -> Result<&'a str, HighlightError> {
        bidi_highlight::copy_source(self.source, self.logical_range.clone())
    }

    pub fn rectangles(
        &self,
        metrics: &impl ReaderHighlightMetrics,
    ) -> Result<Vec<Rect>, HighlightError> {
        self.exact_copy()?;
        if !self.viewport.is_valid() {
            return Err(HighlightError::InvalidGeometry);
        }
        if self.logical_range.is_empty() {
            return Ok(Vec::new());
        }
        let cells = metrics.visible_cells(self.viewport, self.max_cells)?;
        bidi_highlight::visual_rectangles(
            self.source,
            self.logical_range.clone(),
            self.viewport,
            cells,
            self.max_cells,
            self.max_rectangles,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    struct Metrics {
        calls: Cell<usize>,
    }
    impl ReaderHighlightMetrics for Metrics {
        fn visible_cells(
            &self,
            viewport: Rect,
            limit: usize,
        ) -> Result<Vec<VisualCell>, HighlightError> {
            self.calls.set(self.calls.get() + 1);
            assert_eq!(viewport.left, 5.);
            assert_eq!(limit, 4);
            Ok(vec![VisualCell {
                source: 0..2,
                bounds: Rect {
                    left: 0.,
                    top: 0.,
                    right: 12.,
                    bottom: 16.,
                },
            }])
        }
    }
    #[test]
    fn adapter_uses_measured_bounds_and_copy_retains_logical_source() {
        let metrics = Metrics {
            calls: Cell::new(0),
        };
        let mut selection = ReaderHighlight {
            source: "א\n",
            logical_range: 0..3,
            viewport: Rect {
                left: 5.,
                top: 2.,
                right: 10.,
                bottom: 12.,
            },
            max_cells: 4,
            max_rectangles: 4,
        };
        assert_eq!(selection.exact_copy(), Ok("א\n"));
        assert_eq!(selection.rectangles(&metrics), Ok(vec![selection.viewport]));
        assert_eq!(metrics.calls.get(), 1);
        selection.logical_range = 1..2;
        assert_eq!(
            selection.rectangles(&metrics),
            Err(HighlightError::InvalidRange)
        );
        assert_eq!(metrics.calls.get(), 1);
    }
}
