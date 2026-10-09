//! Plain attachment previews. File reads and text preparation stay off the UI thread.
use super::*;
mod source;

pub(crate) fn eligible(rel: &str) -> bool {
    Path::new(rel)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            ["txt", "log", "csv"]
                .iter()
                .any(|supported| ext.eq_ignore_ascii_case(supported))
        })
}

enum State {
    Loading,
    Content {
        content: Entity<TextViewState>,
        partial: bool,
    },
    Message(&'static str),
}

pub(crate) struct PlainTextPreview {
    root: PathBuf,
    rel: String,
    state: State,
}

impl PlainTextPreview {
    pub(crate) fn new(root: PathBuf, rel: String, path: PathBuf, cx: &mut Context<Self>) -> Self {
        let task = cx.background_executor().spawn(async move {
            source::load(&path).map(|preview| match preview {
                source::Preview::Content { text, partial } => {
                    let rendered = source::fenced(&text);
                    Some((text, rendered, partial))
                }
                source::Preview::Unsupported => None,
            })
        });
        // Each selection owns its own entity. A late read cannot overwrite a newer preview.
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.state = match result {
                    Ok(Some((text, _, _))) if text.is_empty() => State::Message("Empty file"),
                    Ok(Some((text, rendered, partial))) => {
                        let content = cx.new(|cx| TextViewState::markdown("", cx));
                        content.update(cx, |state, cx| {
                            state.set_text_with_source(&rendered, Some(text.into()), cx);
                        });
                        State::Content { content, partial }
                    }
                    Ok(None) => State::Message("This file can’t be previewed as text."),
                    Err(_) => State::Message("This file couldn’t be read."),
                };
                cx.notify();
            });
        })
        .detach();
        Self {
            root,
            rel,
            state: State::Loading,
        }
    }

    fn open_button(&self, cx: &App) -> impl IntoElement {
        let root = self.root.clone();
        let rel = self.rel.clone();
        reader_icon_button(
            "plain-file-open",
            IconName::ExternalLink,
            "Open externally",
            cx,
        )
        .accessibility_label("Open externally")
        .on_click(move |_, window, cx| {
            reader_files::run(reader_files::FileAction::Open, &root, &rel, window, cx);
        })
    }
}

impl Render for PlainTextPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let font_size = px(reader_ui_state::font_size(cx));
        // Monospace text uses the code size, not the reading size (#998).
        let code_size = reader_code_file::text_size(cx);
        let view = v_flex().w_full().flex_1().min_h_0().px_6().pb_6().gap_3();
        match &self.state {
            State::Loading => view.child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("Opening file…"),
            ),
            State::Message(message) => view.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_size(font_size)
                            .text_color(cx.theme().muted_foreground)
                            .child(*message),
                    )
                    .child(self.open_button(cx)),
            ),
            State::Content { content, partial } => view
                .child(
                    TextView::new(content)
                        .selectable(true)
                        .selection_format(SelectionFormat::Plain)
                        .scrollable(true)
                        .w_full()
                        .flex_1()
                        .min_h_0()
                        .style(
                            TextViewStyle::default().code_block(
                                StyleRefinement::default()
                                    .bg(gpui::rgba(0))
                                    .border_0()
                                    .rounded(px(0.))
                                    .p_0()
                                    .text_size(code_size),
                            ),
                        ),
                )
                .when(*partial, |view| {
                    view.child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Showing the beginning of this file"),
                            )
                            .child(self.open_button(cx)),
                    )
                }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn supported_extensions_do_not_capture_markdown_or_images() {
        for path in ["a.txt", "a.TXT", "a.log", "a.csv"] {
            assert!(eligible(path));
        }
        for path in ["a.md", "a.excalidraw.md", "a.pdf", "a.png"] {
            assert!(!eligible(path));
        }
    }

    #[gpui::test]
    fn rendered_selection_preserves_literal_syntax_and_never_writes(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("literal.TXT");
        let source = "# Heading\n[[missing]] **bold** <b>literal</b>\n```\n[link](https://example.com)\n````\n";
        std::fs::write(&path, source).unwrap();
        cx.update(gpui_component::init);
        let mut preview = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| {
                PlainTextPreview::new(
                    fixture.path().into(),
                    "literal.TXT".into(),
                    path.clone(),
                    cx,
                )
            });
            preview = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let preview = preview.unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let content = loop {
            visual.run_until_parked();
            let ready = visual.read(|cx| match &preview.read(cx).state {
                State::Content { content, .. }
                    if matches!(content.read(cx).preparation_status(), Some(Ok(()))) =>
                {
                    Some(content.clone())
                }
                _ => None,
            });
            if let Some(content) = ready {
                break content;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "plain preview did not publish its source"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        };
        content.update(visual, |state, cx| state.select_all(cx));
        visual.run_until_parked();
        let copied = content.read_with(visual, |state, _| state.selected_text());
        assert_eq!(copied.trim_end(), source.trim_end());
        assert_eq!(std::fs::read_to_string(path).unwrap(), source);
        assert_eq!(visual.opened_url(), None);
    }
}
