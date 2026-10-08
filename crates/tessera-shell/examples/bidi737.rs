//! Read-only native shaping probe for #737; includes an ASCII positive control.
use gpui::*;
#[path = "../../../vendor/gpui-component/crates/base/src/input/bidi_geometry.rs"]
mod geometry;

fn main() {
    gpui_platform::application().run(|cx| {
        cx.open_window(WindowOptions::default(), |window, cx| {
            for text in ["abcd", "שלום", "abc שלום xyz", "שלום abc עולם", "a שָׁלוֹם z", "abc אבג 123 דהו", "office", "Привет שלום"] {
                let line = window.text_system().shape_line(
                    text.into(),
                    px(16.),
                    &[TextRun {
                        len: text.len(),
                        font: font("Noto Sans"),
                        color: rgb(0).into(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }],
                    None,
                );
                println!("TEXT {text:?} width={:?}", line.width);
                for run in &line.runs {
                    println!(
                        "GLYPHS {:?}",
                        run.glyphs
                            .iter()
                            .map(|g| (g.index, g.position.x))
                            .collect::<Vec<_>>()
                    );
                }
                let map = geometry::Geometry::new(&line);
                println!("VISUAL CELLS {:?}", map.cells);
                for cell in &map.cells {
                    assert!(cell.right > cell.left, "visible cell must have an extent");
                    assert!(edges(&map, cell.source.start).contains(&cell.leading()));
                    assert!(edges(&map, cell.source.end).contains(&cell.trailing()));
                    let near_start = cell.leading() * 0.9 + cell.trailing() * 0.1;
                    let near_end = cell.leading() * 0.1 + cell.trailing() * 0.9;
                    let start_hit = map.hit(near_start).unwrap();
                    let end_hit = map.hit(near_end).unwrap();
                    assert_eq!(start_hit.index, cell.source.start);
                    assert_eq!(end_hit.index, cell.source.end);
                    assert_eq!(map.position(start_hit), Some(cell.leading()));
                    assert_eq!(map.position(end_hit), Some(cell.trailing()));
                    let crossed = map.step(start_hit, !cell.rtl).unwrap();
                    assert_eq!(crossed.index, cell.source.end, "arrow must keep crossed-cell ownership");
                    for right in [false, true] {
                        if let Some(next) = map.step(start_hit, right) {
                            let next_x = map.position(next).unwrap();
                            assert!(if right { next_x > cell.leading() }
                                else { next_x < cell.leading() });
                            let previous = map.step(next, !right).unwrap();
                            assert_eq!(map.position(previous), Some(cell.leading()));
                        }
                    }
                }
                if text == "abc שלום xyz" {
                    let internal: Vec<_> = [6, 8, 10].map(|i| edges(&map, i)).into();
                    assert!(internal.iter().all(|edges| edges.len() == 1));
                    assert!(internal[0][0] > internal[1][0]);
                    assert!(internal[1][0] > internal[2][0]);
                    let before = geometry::Caret { index: 4, affinity: geometry::Affinity::Before };
                    let after = geometry::Caret { index: 4, affinity: geometry::Affinity::After };
                    assert_ne!(map.position(before), map.position(after),
                        "same logical offset has two positions at a bidi boundary");
                    let last_letter = map.selection(10..12);
                    assert_eq!(last_letter.len(), 1);
                    assert!(last_letter[0].end - last_letter[0].start < px(15.));
                    assert_eq!(map.selection(0..6).len(), 2,
                        "mixed logical range needs disjoint visual spans");
                    println!("REPAIR Hebrew internal edges / last-letter selection / disjoint spans PASS");
                }
                if text == "abcd" {
                    for index in 0..=text.len() {
                        assert_eq!(
                            line.closest_index_for_x(line.x_for_index(index)),
                            index,
                            "ASCII positive control must round-trip"
                        );
                    }
                    println!("CONTROL ASCII round-trip PASS");
                }
                for index in text.char_indices().map(|(i, _)| i).chain([text.len()]) {
                    let x = line.x_for_index(index);
                    println!(
                        "BOUNDARY {index} x={x:?} hit={}",
                        line.closest_index_for_x(x)
                    );
                }
            }
            cx.new(|_| Empty)
        })
        .unwrap();
        cx.defer(|cx| cx.quit());
    });
}

struct Empty;
impl Render for Empty {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn edges(map: &geometry::Geometry, index: usize) -> Vec<Pixels> {
    let mut edges: Vec<_> = [geometry::Affinity::Before, geometry::Affinity::After]
        .into_iter()
        .filter_map(|affinity| map.position(geometry::Caret { index, affinity }))
        .collect();
    edges.sort();
    edges.dedup();
    edges
}
