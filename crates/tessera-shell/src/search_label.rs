//! Read-only palette labels: logical UTF-8 text, visual glyph-cluster match backgrounds.
use crate::vendor_bidi_geometry::Geometry;
use gpui::*;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

pub(super) struct SearchLabel {
    text: SharedString,
    matches: Vec<Range<usize>>,
    mark: Hsla,
}

impl SearchLabel {
    pub(super) fn new(text: String, matches: Vec<Range<usize>>, mark: Hsla) -> Self {
        Self {
            text: text.into(),
            matches,
            mark,
        }
    }
}

pub(super) struct Layout {
    style: TextStyle,
    line_height: Pixels,
}

fn shape(text: SharedString, style: &TextStyle, window: &Window) -> ShapedLine {
    let mut run = style.to_run(text.len());
    // Logical decoration runs in GPUI assume monotonic glyph indices. Paint
    // match backgrounds ourselves; the foreground stays one coherent bidi line.
    run.background_color = None;
    window.text_system().shape_line(
        text,
        style.font_size.to_pixels(window.rem_size()),
        &[run],
        None,
    )
}

fn fit(text: &str, width: Pixels, mut measure: impl FnMut(&str) -> Pixels) -> String {
    if measure(text) <= width {
        return text.to_owned();
    }
    // Retain a logical prefix without splitting a grapheme. Never reorder text
    // or inject bidi controls to make a one-line excerpt fit its viewport.
    if measure("…") > width {
        return String::new();
    }
    let boundaries: Vec<_> = text.grapheme_indices(true).map(|(at, _)| at).collect();
    let mut low = 0;
    let mut high = boundaries.len();
    while low < high {
        let mid = (low + high).div_ceil(2);
        let end = boundaries.get(mid).copied().unwrap_or(text.len());
        let candidate = format!("{}…", &text[..end]);
        if measure(&candidate) <= width {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    let end = boundaries.get(low).copied().unwrap_or(text.len());
    format!("{}…", &text[..end])
}

fn match_spans(line: &ShapedLine, matches: &[Range<usize>], retained: usize) -> Vec<Range<Pixels>> {
    if matches.is_empty() {
        return vec![];
    }
    let geometry = Geometry::new(line);
    matches
        .iter()
        .flat_map(|r| {
            let range = r.start.min(retained)..r.end.min(retained);
            geometry.selection(range)
        })
        .collect()
}

impl IntoElement for SearchLabel {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for SearchLabel {
    type RequestLayoutState = Layout;
    type PrepaintState = (ShapedLine, Vec<Range<Pixels>>);
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        _: &mut App,
    ) -> (LayoutId, Layout) {
        let style = window.text_style();
        let line_height = style.line_height_in_pixels(window.rem_size());
        let intrinsic = shape(self.text.clone(), &style, window).width;
        let id = window.request_measured_layout(Style::default(), move |known, available, _, _| {
            let width = known
                .width
                .or(match available.width {
                    AvailableSpace::Definite(w) => Some(w),
                    _ => None,
                })
                .unwrap_or(intrinsic);
            size(intrinsic.min(width).max(px(0.)), line_height)
        });
        (id, Layout { style, line_height })
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Layout,
        window: &mut Window,
        _: &mut App,
    ) -> Self::PrepaintState {
        let shown = fit(&self.text, bounds.size.width, |t| {
            shape(t.to_owned().into(), &layout.style, window).width
        });
        let retained = if shown == self.text.as_ref() {
            self.text.len()
        } else {
            shown.strip_suffix('…').map_or(0, str::len)
        };
        let line = shape(shown.into(), &layout.style, window);
        let spans = match_spans(&line, &self.matches, retained);
        (line, spans)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Layout,
        state: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for span in &state.1 {
                window.paint_quad(fill(
                    Bounds::new(
                        point(bounds.left() + span.start, bounds.top()),
                        size(span.end - span.start, layout.line_height),
                    ),
                    self.mark,
                ));
            }
            // An empty viewport has no glyphs to paint.
            if !state.0.text.is_empty() {
                let _ = state.0.paint(
                    bounds.origin,
                    layout.line_height,
                    TextAlign::Left,
                    Some(bounds.size.width),
                    window,
                    cx,
                );
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vendor_bidi_geometry::Cell;
    use ::core::prelude::v1::test;
    use gpui_component::{Colorize as _, ThemeMode};

    #[gpui::test]
    fn search_label_paints_matches_and_clips_without_marking_cut_text(cx: &mut TestAppContext) {
        struct Fixture {
            text: String,
            matches: Vec<Range<usize>>,
            width: f32,
        }
        impl Render for Fixture {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let palette = crate::brand::palette(cx);
                div()
                    .w(px(self.width))
                    .bg(palette.canvas)
                    .text_color(palette.text)
                    .text_size(px(16.))
                    .line_height(px(20.))
                    .child(SearchLabel::new(
                        self.text.clone(),
                        self.matches.clone(),
                        palette.accent.opacity(0.18),
                    ))
            }
        }
        cx.update(gpui_component::init);
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            for (text, needle) in [
                ("שלום abc xyz", "שלום"),
                ("abc שלום xyz", "שלום"),
                ("abc xyz שלום", "שלום"),
                ("שלום abc 123 עולם 456 xyz", "עולם"),
                ("שלום abc 123 עולם 456 xyz", "123"),
                ("שלום abc 123 עולם 456 xyz", "abc"),
                ("ASCII positive control", "positive"),
            ] {
                let from = text.find(needle).unwrap();
                let (view, visual) = cx.add_window_view(|_, _| Fixture {
                    text: text.into(),
                    matches: std::iter::once(from..from + needle.len()).collect(),
                    width: 500.,
                });
                visual.update(|window, cx| crate::set_appearance(Some(mode), None, window, cx));
                visual.run_until_parked();
                visual.update(|window, cx| window.draw(cx).clear(cx));
                visual.update(|window, cx| {
                    let mark: Background = crate::brand::palette(cx).accent.opacity(0.18).into();
                    let quads: Vec<_> = window
                        .painted_quads()
                        .into_iter()
                        .filter(|q| q.background == mark)
                        .collect();
                    assert!(
                        !quads.is_empty(),
                        "match painting positive control: {mode:?} {text} / {needle}"
                    );
                    assert!(quads
                        .iter()
                        .all(|q| q.bounds.size.width > ScaledPixels::from(0.)
                            && q.bounds.size.height > ScaledPixels::from(0.)));
                });
                view.update(visual, |v, cx| {
                    v.width = 1.;
                    cx.notify();
                });
                visual.run_until_parked();
                visual.update(|window, cx| {
                    let mark: Background = crate::brand::palette(cx).accent.opacity(0.18).into();
                    assert!(
                        !window.painted_quads().iter().any(|q| q.background == mark),
                        "no false mark when the entire match is cut: {mode:?} {text} / {needle}"
                    )
                });
                view.read_with(visual, |v, _| {
                    assert_eq!(v.text, text, "layout never mutates source order")
                });
            }
        }
    }

    #[test]
    fn rtl_matches_have_positive_visual_width_and_mixed_spans_stay_disjoint() {
        // Logical "a אב z", with visual glyph order independent of source order.
        let geometry = Geometry {
            cells: vec![
                Cell {
                    source: 0..1,
                    left: px(0.),
                    right: px(10.),
                    rtl: false,
                },
                Cell {
                    source: 1..2,
                    left: px(10.),
                    right: px(15.),
                    rtl: false,
                },
                Cell {
                    source: 4..6,
                    left: px(15.),
                    right: px(25.),
                    rtl: true,
                },
                Cell {
                    source: 2..4,
                    left: px(25.),
                    right: px(35.),
                    rtl: true,
                },
                Cell {
                    source: 6..7,
                    left: px(35.),
                    right: px(40.),
                    rtl: false,
                },
                Cell {
                    source: 7..8,
                    left: px(40.),
                    right: px(50.),
                    rtl: false,
                },
            ],
        };
        assert_eq!(
            geometry.selection(0..1),
            [px(0.)..px(10.)],
            "ASCII positive control"
        );
        assert_eq!(geometry.selection(2..6), [px(15.)..px(35.)]);
        assert_eq!(
            geometry.selection(0..4),
            [px(0.)..px(15.), px(25.)..px(35.)]
        );
        assert!(
            geometry.selection(2..2).is_empty(),
            "cropped-away matches never mark the ellipsis"
        );
    }

    #[test]
    fn width_crop_preserves_source_order_and_graphemes_with_a_cut_marker() {
        let source = "abc שָׁלוֹם 😀 xyz";
        let measure = |text: &str| px(text.graphemes(true).count() as f32);
        let shown = fit(source, px(7.), measure);
        assert_eq!(shown, "abc שָׁל…");
        assert!(source.starts_with(shown.trim_end_matches('…')));
        assert_eq!(fit(source, px(100.), measure), source);
        assert_eq!(fit(source, px(0.), measure), "");
    }
}
