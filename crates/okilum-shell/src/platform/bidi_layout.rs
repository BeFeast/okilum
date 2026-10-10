//! Right-to-left text on Windows (#1121). GPUI's DirectWrite backend places
//! every glyph of a line at a running pen position and ignores each run's bidi
//! level, so Hebrew and Arabic come out in logical order: letters reversed.
//! CoreText (macOS) and cosmic-text (Linux) already return visual positions.
//!
//! `repair` moves glyphs to their visual positions after shaping. Glyph ids,
//! text indices and widths are unchanged, so caret, selection and search keep
//! working from the same logical indices. gpui core stays unpatched: on Windows
//! the platform is wrapped and only `layout_line` goes through here.
use gpui::{px, LineLayout};
use unicode_bidi::{bidi_class, BidiClass, BidiInfo};

/// Whether `text` has right-to-left characters; other lines are left alone.
fn has_rtl(text: &str) -> bool {
    text.chars()
        .any(|c| matches!(bidi_class(c), BidiClass::R | BidiClass::AL))
}

/// One base glyph with the marks drawn on it, in shaping order.
struct Cluster {
    index: usize,
    start: f32,
    width: f32,
    /// (run, glyph, x relative to the cluster start)
    glyphs: Vec<(usize, usize, f32)>,
}

/// Place glyphs in Unicode bidi visual order. The paragraph direction comes
/// from the first strong character, as cosmic-text (Linux) and CoreText do, so
/// a line that starts in Hebrew reads right to left on every platform.
pub(crate) fn repair(text: &str, mut layout: LineLayout) -> LineLayout {
    if !has_rtl(text) {
        return layout;
    }
    let mut clusters: Vec<Cluster> = Vec::new();
    for (r, run) in layout.runs.iter().enumerate() {
        for (g, glyph) in run.glyphs.iter().enumerate() {
            let x = f32::from(glyph.position.x);
            let mark = text
                .get(glyph.index..)
                .and_then(|rest| rest.chars().next())
                .is_some_and(|c| bidi_class(c) == BidiClass::NSM);
            match clusters.last_mut() {
                Some(cluster) if mark || cluster.index == glyph.index => {
                    cluster.glyphs.push((r, g, x - cluster.start));
                }
                _ => clusters.push(Cluster {
                    index: glyph.index,
                    start: x,
                    width: 0.,
                    glyphs: vec![(r, g, 0.)],
                }),
            }
        }
    }
    // The pen only moves forward, so each cluster's advance is the distance
    // to the next one.
    let line_width = f32::from(layout.width);
    for i in 0..clusters.len() {
        let end = clusters.get(i + 1).map_or(line_width, |next| next.start);
        clusters[i].width = (end - clusters[i].start).max(0.);
    }

    let bidi = BidiInfo::new(text, None);
    let mut order: Vec<usize> = Vec::with_capacity(clusters.len());
    let mut placed = vec![false; clusters.len()];
    for paragraph in &bidi.paragraphs {
        let (levels, runs) = bidi.visual_runs(paragraph, paragraph.range.clone());
        for run in runs {
            let mut members: Vec<usize> = (0..clusters.len())
                .filter(|&c| !placed[c] && run.contains(&clusters[c].index))
                .collect();
            members.sort_by_key(|&c| clusters[c].index);
            if levels.get(run.start).is_some_and(|level| level.is_rtl()) {
                members.reverse();
            }
            for c in members {
                placed[c] = true;
                order.push(c);
            }
        }
    }
    // Anything bidi did not cover keeps its shaping order at the end.
    order.extend((0..clusters.len()).filter(|&c| !placed[c]));

    let mut pen = 0.;
    for c in order {
        let cluster = &clusters[c];
        for &(r, g, offset) in &cluster.glyphs {
            layout.runs[r].glyphs[g].position.x = px(pen + offset);
        }
        pen += cluster.width;
    }
    layout
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::{point, FontId, GlyphId, ShapedGlyph, ShapedRun};

    /// What GPUI's DirectWrite path returns: every glyph in text order at a
    /// pen that advances by `advance`; marks share the pen of their letter.
    fn direct_write(text: &str, advance: f32) -> LineLayout {
        let mut pen = 0.;
        let glyphs = text
            .char_indices()
            .map(|(index, c)| {
                let mark = bidi_class(c) == BidiClass::NSM;
                let glyph = ShapedGlyph {
                    id: GlyphId(c as u32),
                    position: point(px(if mark { pen - advance + 2. } else { pen }), px(0.)),
                    index,
                    is_emoji: false,
                };
                if !mark {
                    pen += advance;
                }
                glyph
            })
            .collect();
        LineLayout {
            font_size: px(16.),
            width: px(pen),
            ascent: px(12.),
            descent: px(4.),
            runs: vec![ShapedRun {
                font_id: FontId(0),
                glyphs,
            }],
            len: text.len(),
        }
    }

    /// Characters left to right as painted.
    fn painted(text: &str, layout: &LineLayout) -> String {
        let mut glyphs: Vec<_> = layout.runs.iter().flat_map(|r| r.glyphs.iter()).collect();
        glyphs.sort_by(|a, b| a.position.x.partial_cmp(&b.position.x).unwrap());
        glyphs
            .iter()
            .map(|g| text[g.index..].chars().next().unwrap())
            .collect()
    }

    #[test]
    fn hebrew_words_read_right_to_left_inside_a_left_to_right_line() {
        let text = "ab שלום cd";
        let fixed = repair(text, direct_write(text, 10.));
        assert_eq!(painted(text, &fixed), "ab םולש cd");
        assert_eq!(fixed.width, px(100.), "the line keeps its width");
        // Text indices stay logical for caret, selection and search.
        let indices: Vec<_> = fixed.runs[0].glyphs.iter().map(|g| g.index).collect();
        assert_eq!(
            indices,
            text.char_indices().map(|(i, _)| i).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_line_that_starts_in_hebrew_reads_right_to_left() {
        // The #1121 sample: a right-to-left paragraph, like the browser
        // reference and the Linux and macOS shapers.
        let text = "שלום חברים. Hello friends. Привет друзья.";
        let fixed = repair(text, direct_write(text, 10.));
        let shown = painted(text, &fixed);
        assert!(
            shown.starts_with('.'),
            "the final period goes left: {shown}"
        );
        assert!(
            shown.ends_with('ש'),
            "the first word starts at the right: {shown}"
        );
        assert!(shown.contains("םירבח םולש"), "{shown}");
        assert!(shown.contains("Hello friends. Привет друзья"), "{shown}");
    }

    #[test]
    fn a_markdown_heading_keeps_its_hash_at_the_start_of_the_line() {
        let text = "# שלום עולם";
        let fixed = repair(text, direct_write(text, 10.));
        assert_eq!(painted(text, &fixed), "םלוע םולש #");
    }

    #[test]
    fn marks_stay_on_their_letter() {
        // Shin with a qamats mark, then lamed.
        let text = "\u{5E9}\u{5B8}\u{5DC}";
        let fixed = repair(text, direct_write(text, 10.));
        let x = |i: usize| f32::from(fixed.runs[0].glyphs[i].position.x);
        // Lamed is painted first (left), shin to its right, the mark on shin.
        assert_eq!(x(2), 0.);
        assert_eq!(x(0), 10.);
        assert_eq!(x(1), 12.);
    }

    #[test]
    fn lines_without_right_to_left_text_are_untouched() {
        // Positive control for the fast path.
        let text = "Hello friends. Привет друзья.";
        let before = direct_write(text, 10.);
        let after = repair(text, direct_write(text, 10.));
        let xs = |l: &LineLayout| {
            l.runs[0]
                .glyphs
                .iter()
                .map(|g| g.position.x)
                .collect::<Vec<_>>()
        };
        assert_eq!(xs(&after), xs(&before));
    }
}
