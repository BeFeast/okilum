//! Experimental vendor-boundary geometry; not connected to the product editor.
use std::{collections::BTreeMap, ops::Range};

use gpui::{Pixels, ShapedLine};
use unicode_bidi::BidiInfo;
use unicode_segmentation::UnicodeSegmentation;

/// Which neighboring logical grapheme owns the caret. This is independent of
/// soft-wrap affinity; one boolean cannot encode both choices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Affinity {
    Before,
    After,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caret {
    pub index: usize,
    pub affinity: Affinity,
}

#[derive(Debug)]
pub struct Cell {
    pub source: Range<usize>,
    pub left: Pixels,
    pub right: Pixels,
    pub rtl: bool,
}

impl Cell {
    pub fn leading(&self) -> Pixels {
        if self.rtl {
            self.right
        } else {
            self.left
        }
    }

    pub fn trailing(&self) -> Pixels {
        if self.rtl {
            self.left
        } else {
            self.right
        }
    }
}

#[derive(Debug)]
pub struct Geometry {
    pub cells: Vec<Cell>,
}

impl Geometry {
    pub fn new(line: &ShapedLine) -> Self {
        let text = line.text.as_ref();
        let bidi = BidiInfo::new(text, None);
        let boundaries: Vec<_> = text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain([text.len()])
            .collect();
        // Neither source nor X ordering of platform glyph collections is guaranteed.
        // Coalesce fallback glyphs/combining marks to their grapheme before sorting.
        let mut clusters = BTreeMap::<usize, Pixels>::new();
        for glyph in line.runs.iter().flat_map(|run| &run.glyphs) {
            let slot = boundaries
                .partition_point(|&i| i <= glyph.index)
                .saturating_sub(1);
            let start = boundaries[slot];
            clusters
                .entry(start)
                .and_modify(|x| *x = (*x).min(glyph.position.x))
                .or_insert(glyph.position.x);
        }
        let starts: Vec<_> = clusters.keys().copied().chain([text.len()]).collect();
        let mut visual: Vec<_> = clusters.into_iter().collect();
        visual.sort_by_key(|(_, x)| *x);
        let mut cells = Vec::new();
        for (n, &(start, left)) in visual.iter().enumerate() {
            let right = visual.get(n + 1).map_or(line.width, |(_, x)| *x);
            let end = starts[starts.binary_search(&start).unwrap() + 1];
            let graphemes: Vec<_> = boundaries
                .iter()
                .copied()
                .filter(|&i| start <= i && i <= end)
                .collect();
            let rtl = bidi.levels[start].is_rtl();
            let count = graphemes.len() - 1;
            // A ligature cluster can span several graphemes. Equal subdivision is
            // provisional: native ligature caret evidence is required before shipping.
            for (i, pair) in graphemes.windows(2).enumerate() {
                let visual_i = if rtl { count - i - 1 } else { i };
                let width = (right - left) / count as f32;
                cells.push(Cell {
                    source: pair[0]..pair[1],
                    left: left + width * visual_i as f32,
                    right: left + width * (visual_i + 1) as f32,
                    rtl,
                });
            }
        }
        cells.sort_by_key(|cell| cell.left);
        Self { cells }
    }

    /// Carries both candidates at a bidi boundary; callers must retain affinity.
    pub fn edges(&self, index: usize) -> Vec<Pixels> {
        let mut edges = Vec::new();
        for cell in &self.cells {
            if cell.source.start == index {
                edges.push(cell.leading());
            }
            if cell.source.end == index {
                edges.push(cell.trailing());
            }
        }
        edges.sort();
        edges.dedup();
        edges
    }

    pub fn position(&self, caret: Caret) -> Option<Pixels> {
        self.cells.iter().find_map(|cell| match caret.affinity {
            Affinity::Before if cell.source.start == caret.index => Some(cell.leading()),
            Affinity::After if cell.source.end == caret.index => Some(cell.trailing()),
            _ => None,
        })
    }

    /// Select the visual cell first, so an equal-X edge in another directional
    /// run cannot steal its logical index. Preserve ownership at the boundary.
    pub fn hit(&self, x: Pixels) -> Option<Caret> {
        let cell = self.cells.iter().min_by_key(|cell| {
            if x < cell.left {
                cell.left - x
            } else if x > cell.right {
                x - cell.right
            } else {
                gpui::px(0.)
            }
        })?;
        if (cell.leading() - x).abs() <= (cell.trailing() - x).abs() {
            Some(Caret {
                index: cell.source.start,
                affinity: Affinity::Before,
            })
        } else {
            Some(Caret {
                index: cell.source.end,
                affinity: Affinity::After,
            })
        }
    }

    /// Move in visual X order, retaining the selected edge's logical ownership.
    /// At row limits the caller decides paragraph/wrap traversal.
    pub fn step(&self, caret: Caret, right: bool) -> Option<Caret> {
        let x = self.position(caret)?;
        // Choose the cell crossed by the arrow, not an arbitrary equal-X
        // neighbor. Shift+Left across the final Hebrew letter must select that
        // letter, not the LTR prefix that shares its trailing coordinate.
        let cell = self
            .cells
            .iter()
            .filter(|cell| if right { cell.right > x } else { cell.left < x })
            .min_by_key(|cell| {
                if right {
                    (cell.right - x).abs()
                } else {
                    (cell.left - x).abs()
                }
            })?;
        let leading = right == cell.rtl;
        Some(if leading {
            Caret {
                index: cell.source.start,
                affinity: Affinity::Before,
            }
        } else {
            Caret {
                index: cell.source.end,
                affinity: Affinity::After,
            }
        })
    }

    pub fn selection(&self, range: Range<usize>) -> Vec<Range<Pixels>> {
        let mut spans: Vec<Range<Pixels>> = Vec::new();
        for cell in &self.cells {
            if cell.source.start >= range.end || cell.source.end <= range.start {
                continue;
            }
            if let Some(previous) = spans.last_mut() {
                if previous.end == cell.left {
                    previous.end = cell.right;
                    continue;
                }
            }
            spans.push(cell.left..cell.right);
        }
        spans
    }
}
