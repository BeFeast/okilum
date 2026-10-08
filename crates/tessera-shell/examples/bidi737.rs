//! Read-only native shaping probe for #737; includes an ASCII positive control.
use gpui::*;

fn main() {
    gpui_platform::application().run(|cx| {
        cx.open_window(WindowOptions::default(), |window, cx| {
            for text in ["abcd", "שלום", "abc שלום xyz", "שלום abc עולם"] {
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
