//! Native shaping-only seam spike. No editor integration or visual acceptance.
#[path = "decoration784/paint.rs"]
mod paint;
use gpui::*;
use tessera_core::{
    source_classifier::{classify, RetainedPresentation},
    source_projection::{Active, Snapshot},
};

fn main() {
    gpui_platform::application().run(|cx| {
        cx.open_window(WindowOptions::default(), |window, cx| {
            let source = Snapshot::new("spike", 1, "- marker\n\nbody");
            let classified = classify(&source);
            let retained = RetainedPresentation::new(&classified);
            let reveal = retained.prepare_reveal(&Active::default()).unwrap();
            let policy = paint::MarkerPolicy { reveal: &reveal, current: &source };
            assert!(!policy.is_raw(&(0..8)));
            let active = Active { selection: Some(2..2), composition: None };
            let revealed = retained.prepare_reveal(&active).unwrap();
            assert!(paint::MarkerPolicy { reveal: &revealed, current: &source }.is_raw(&(0..8)));
            let stale = Snapshot::new("spike", 2, source.source());
            assert!(paint::MarkerPolicy { reveal: &reveal, current: &stale }.is_raw(&(0..8)));
            println!("POLICY shared snapshot / caret / stale PASS");
            for text in ["- marker", "  * проверка Billing", "> quote", "***", "- office á", "- 🙂 emoji", "- שלום", "- abc שלום xyz"] {
                let line = window.text_system().shape_line(text.into(), px(16.), &[TextRun {
                    len: text.len(), font: font("Noto Sans"), color: rgb(0).into(),
                    background_color: None, underline: None, strikethrough: None,
                }], None);
                if unicode_bidi::BidiInfo::new(text, None).has_rtl() {
                    assert!(paint::foreground_pieces(&line, std::slice::from_ref(&(0..1))).is_none());
                    println!("BIDI raw fallback PASS {text:?}");
                    continue;
                }
                for index in 1..text.len() {
                    if text.is_char_boundary(index) && !line.runs.iter().flat_map(|r| &r.glyphs).any(|g| g.index == index) {
                        assert!(paint::foreground_pieces(&line, std::slice::from_ref(&(0..index))).is_none());
                        println!("CLUSTER boundary rejection PASS {text:?} at {index}");
                    }
                }
                let marker = text.find(['-', '*', '>']).unwrap();
                let range = marker..marker+1;
                let before = format!("{line:?}");
                let pieces = paint::foreground_pieces(&line, std::slice::from_ref(&range)).unwrap();
                let original: Vec<_> = line.runs.iter().flat_map(|r| r.glyphs.iter().map(move |g| (r.font_id, g))).collect();
                let expected: Vec<_> = original.iter().filter(|(_,g)| !range.contains(&g.index)).collect();
                let actual: Vec<_> = pieces.iter().flat_map(|p| p.shaped.runs.iter().flat_map(move |r| r.glyphs.iter().map(move |g| (r.font_id, g, p)))).collect();
                assert_eq!(expected.len(), actual.len());
                for ((font, old), (new_font, new, piece)) in expected.iter().zip(&actual) {
                    assert_eq!(font, new_font);
                    assert_eq!(old.id, new.id);
                    assert_eq!(old.index, new.index + piece.source_start);
                    assert!((old.position.x - (new.position.x + piece.x)).abs() < px(0.001));
                    assert_eq!(old.position.y, new.position.y);
                }
                assert_eq!(before, format!("{line:?}"), "original layout mutated");
                assert!(paint::foreground_pieces(&line, std::slice::from_ref(&(0..text.len()+1))).is_none());
                assert!(paint::foreground_pieces(&line, &[range.clone(), range.clone()]).is_none());
                // Positive control: choosing a different range retains the marker.
                let all = paint::foreground_pieces(&line, &[]).unwrap();
                assert_eq!(all.iter().map(|p| p.shaped.runs.iter().map(|r| r.glyphs.len()).sum::<usize>()).sum::<usize>(), original.len());
                println!("GLYPH ID / original position / immutable layout / invalid-range controls PASS {text:?}");
            }
            cx.new(|_| Empty)
        }).unwrap();
        cx.defer(|cx| cx.quit());
    });
}
struct Empty;
impl Render for Empty {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
