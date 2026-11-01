//! A transient read-only surface: never enters the ordinary file-navigation path.
use super::*;

pub(super) struct Preview {
    rel: String,
    text: Option<Entity<reader_plain_text::PlainTextPreview>>,
}

impl Preview {
    pub(super) fn new(root: PathBuf, rel: String, path: PathBuf, cx: &mut Context<Self>) -> Self {
        // Reuse the plain-file codec, including its bounded UTF-8 prefix and
        // binary rejection. Do not instantiate editable table/log/archive views
        // or an unbounded image decoder for a transient selection.
        let textual = reader_plain_text::eligible(&rel)
            || reader_code_file::eligible(&rel)
            || reader_delimited::eligible(&rel);
        let text = textual.then(|| {
            cx.new(|cx| reader_plain_text::PlainTextPreview::new(root, rel.clone(), path, cx))
        });
        Self { rel, text }
    }
}

impl Render for Preview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("tree-readonly-quick-look")
            .size_full()
            .min_h_0()
            .gap_3()
            .child(
                div()
                    .px_6()
                    .py_3()
                    .child(self.rel.clone())
                    .child(" · Quick Look · Space or Escape to close"),
            )
            .child(match &self.text {
                Some(text) => div().flex_1().min_h_0().child(text.clone()).into_any_element(),
                None => div()
                    .px_6()
                    .text_color(cx.theme().muted_foreground)
                    .child("This file type has no bounded Quick Look preview. Press Enter in the tree to open it in the reader.")
                    .into_any_element(),
            })
    }
}

#[cfg(test)]
#[path = "reader_quick_look/tests.rs"]
mod tests;
