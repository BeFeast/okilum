//! Code-block actions. Copy is distinct from Markdown selection/copy.
use super::*;
use gpui_base::text::CodeBlock;

struct CopyState {
    value: SharedString,
    copied: bool,
    generation: u64,
}

pub(crate) fn actions(block: &CodeBlock, _: &mut Window, _: &mut App) -> AnyElement {
    let Some(value) = block.copy_code() else {
        return div().into_any_element();
    };
    CodeActions {
        value,
        language: block.lang(),
        offset: block.span.map(|span| span.start).unwrap_or_default(),
    }
    .into_any_element()
}

/// Render inside the code block's element-id scope. Constructing keyed state
/// directly in the actions callback would share it between sibling blocks.
#[derive(IntoElement)]
struct CodeActions {
    value: SharedString,
    language: Option<SharedString>,
    offset: usize,
}

impl RenderOnce for CodeActions {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            value,
            offset,
            language,
        } = self;
        let state = window.use_keyed_state("reader-code-copy-state", cx, |_, _| CopyState {
            value: value.clone(),
            copied: false,
            generation: 0,
        });
        if state.read(cx).value != value {
            state.update(cx, |s, _| {
                s.value = value.clone();
                s.copied = false;
                s.generation += 1;
            });
        }
        let copied = state.read(cx).copied;
        div()
            .flex()
            .items_center()
            .gap_2()
            .when_some(language, |el, language| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .max_w(px(160.))
                        .truncate()
                        .child(language)
                        .debug_selector(move || format!("reader-code-language-{offset}")),
                )
            })
            .tab_group()
            .child(
                Button::new("reader-code-copy")
                    .xsmall()
                    .ghost()
                    .bg(brand::reader_palette(cx).code_bg)
                    .label(if copied { "Copied" } else { "Copy" })
                    .tooltip("Copy code")
                    .accessibility_label("Copy code")
                    .opacity(if copied { 1. } else { 0. })
                    .group_hover("code-block", |s| s.opacity(1.))
                    .focus(|s| s.opacity(1.))
                    .debug_selector(move || {
                        format!(
                            "reader-code-{}-{offset}",
                            if copied { "copied" } else { "copy" }
                        )
                    })
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        cx.write_to_clipboard(ClipboardItem::new_string(value.to_string()));
                        let generation = state.update(cx, |s, cx| {
                            s.copied = true;
                            s.generation += 1;
                            cx.notify();
                            s.generation
                        });
                        let state = state.downgrade();
                        cx.spawn(async move |cx| {
                            cx.background_executor()
                                .timer(std::time::Duration::from_secs(2))
                                .await;
                            let _ = state.update(cx, |s, cx| {
                                if s.generation == generation {
                                    s.copied = false;
                                    cx.notify();
                                }
                            });
                        })
                        .detach();
                    }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use std::sync::Mutex;

    struct Fixture {
        text: Entity<TextViewState>,
        copies: Arc<Mutex<Vec<Option<SharedString>>>>,
    }
    impl Render for Fixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let copies = self.copies.clone();
            div().w(px(700.)).child(
                markdown_plugins(
                    TextView::new(&self.text),
                    Arc::new(|_, _, _, _| {}),
                    Arc::new(|_| None),
                    SelectionFormat::Plain,
                )
                .code_block_actions(move |block, window, cx| {
                    copies.lock().unwrap().push(block.copy_code());
                    actions(block, window, cx)
                }),
            )
        }
    }

    #[gpui::test]
    fn copy_content_preserves_endings_indentation_and_nested_blocks(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let cases = [
            ("```rs\r\n  café\r\n\r\n```\r\n", "  café\r\n\r\n"),
            ("```\nx\n", "x\n"),
            ("```\nx", "x"),
            ("```\n", ""),
            ("```\n```\n", ""),
            ("```\n\n```", "\n"),
            ("~~~~rs\n```\n~~~~", "```\n"),
            ("```\n    ```", "    ```"),
            ("    a\r\n      b\r\n", "a\r\n  b\r\n"),
            ("    ```\n", "```\n"),
            ("    final", "final"),
            ("> ```\n>  a\n> ```\n", " a\n"),
            ("- ```\r\n  \tline\r\n  ```\r\n", "\tline\r\n"),
            ("  ```\r\n    inner\r\n  ```", "  inner\r\n"),
            ("```\rline\r```", "line\r"),
            ("> [!note]\r\n> ```\r\n>   x\r\n> ```", "  x\r\n"),
            ("> [!note]\n> ```\n> x  ", "x  "),
        ];
        for (source, expected) in cases {
            let copies = Arc::new(Mutex::new(Vec::new()));
            let (_, visual) = cx.add_window_view(|window, cx| {
                let text = cx.new(|cx| TextViewState::markdown(source, cx));
                let view = cx.new(|_| Fixture {
                    text,
                    copies: copies.clone(),
                });
                Root::new(view, window, cx)
            });
            visual.run_until_parked();
            let recorded = copies.lock().unwrap();
            assert!(
                !recorded.is_empty(),
                "positive control: rendered code action for {source:?}"
            );
            for value in recorded.iter() {
                assert_eq!(value.as_deref(), Some(expected), "{source:?}");
            }
        }
    }

    #[gpui::test]
    fn sibling_blocks_keep_independent_copy_feedback(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let text = cx.new(|cx| TextViewState::markdown("```\none\n```\n\n```\ntwo\n```", cx));
            let view = cx.new(|_| Fixture {
                text,
                copies: Default::default(),
            });
            Root::new(view, window, cx)
        });
        visual.run_until_parked();
        let first = visual.debug_bounds("reader-code-copy-0").unwrap();
        let second = visual.debug_bounds("reader-code-copy-13").unwrap();
        visual.simulate_click(first.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("reader-code-copied-0").is_some());
        assert!(visual.debug_bounds("reader-code-copy-13").is_some());
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("one\n")
            )
        });
        visual
            .executor()
            .advance_clock(std::time::Duration::from_secs(1));
        visual.simulate_click(second.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("reader-code-copied-0").is_some());
        assert!(visual.debug_bounds("reader-code-copied-13").is_some());
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("two\n")
            )
        });
        visual
            .executor()
            .advance_clock(std::time::Duration::from_millis(1100));
        visual.run_until_parked();
        assert!(visual.debug_bounds("reader-code-copy-0").is_some());
        assert!(visual.debug_bounds("reader-code-copied-13").is_some());
    }

    #[gpui::test]
    fn copy_click_keyboard_feedback_and_document_replacement(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let mut text = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let state = cx.new(|cx| TextViewState::markdown("```\r\n  one\r\n```", cx));
            state.read(cx).focus_handle().clone().focus(window, cx);
            text = Some(state.clone());
            let view = cx.new(|_| Fixture {
                text: state,
                copies: Default::default(),
            });
            Root::new(view, window, cx)
        });
        let text = text.unwrap();
        visual.run_until_parked();
        let button = visual.debug_bounds("reader-code-copy-0").unwrap();
        visual.simulate_mouse_move(button.center(), MouseButton::Left, Modifiers::default());
        visual.simulate_click(button.center(), Modifiers::default());
        visual.run_until_parked();
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("  one\r\n")
            )
        });
        assert!(visual.debug_bounds("reader-code-copied-0").is_some());
        visual
            .executor()
            .advance_clock(std::time::Duration::from_secs(3));
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("reader-code-copy-0").is_some(),
            "feedback expires"
        );
        visual.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into())));
        // No mouse click: Tab reaches the action and Enter activates it.
        for _ in 0..4 {
            visual.simulate_keystrokes("tab");
            visual.run_until_parked();
            let keystroke = Keystroke::parse("enter").unwrap();
            visual.simulate_event(KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            visual.simulate_event(KeyUpEvent { keystroke });
            visual.run_until_parked();
            if visual.debug_bounds("reader-code-copied-0").is_some() {
                break;
            }
        }
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("  one\r\n")
            )
        });
        text.update(visual, |v, cx| v.set_text("```\ntwo\n```", cx));
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("reader-code-copy-0").is_some(),
            "new block cannot inherit Copied"
        );
        let button = visual.debug_bounds("reader-code-copy-0").unwrap();
        visual.simulate_click(button.center(), Modifiers::default());
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("two\n")
            )
        });
        // Identical rendered code and byte spans, but a different source EOL:
        // retained parsed blocks must update the clipboard payload too.
        text.update(visual, |v, cx| v.set_text("```\rtwo\r```", cx));
        visual.run_until_parked();
        let button = visual.debug_bounds("reader-code-copy-0").unwrap();
        visual.simulate_click(button.center(), Modifiers::default());
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("two\r")
            )
        });
        text.update(visual, |v, cx| v.set_text("Only `inline code` here.", cx));
        visual.run_until_parked();
        assert!(visual.debug_bounds("reader-code-copy-0").is_none());
        assert!(visual.debug_bounds("reader-code-copied-0").is_none());
    }
}
