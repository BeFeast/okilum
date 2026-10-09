//! Quiet block embeds; source identity stays in the click target, not the label.
use super::*;
use okilum_core::render::block_embed::Status;

pub(super) fn explanation(status: Status) -> &'static str {
    match status {
        Status::Ready => "",
        Status::MissingBlock => "Block not found in ",
        Status::DuplicateBlock => "This block ID is used more than once in ",
        Status::MissingNote => "Source note not found: ",
        Status::UnreadableNote => "Couldn't read ",
        Status::SelfReference => "This note cannot embed itself: ",
        Status::Pending => "Loading embedded block…",
    }
}

pub(super) fn render_block(
    node: &MarkdownNode,
    link: &MarkdownLinkHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(data) = node.data::<Embed>() else {
        return div().into_any_element();
    };
    let Some(info) = &data.block else {
        return div().into_any_element();
    };
    let offset = data.offset;
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let fg = theme.foreground;
    let border = theme.border;
    let font_size = reader_ui_state::font_size(cx);
    let open_url = info.path.as_ref().map(|path| {
        let fragment = if info.status == Status::Ready {
            format!(
                "#{}",
                okilum_core::document_links::encode(&format!("^{}", info.id))
            )
        } else {
            String::new()
        };
        format!(
            "{WIKI_SCHEME}{}{fragment}",
            okilum_core::document_links::encode(path)
        )
    });
    let link = link.clone();
    let source = div()
        .id(("reader-embed-source", offset as u64))
        .debug_selector(move || format!("reader-embed-source-{offset}"))
        .text_color(muted)
        .text_size(px(12.))
        .child(node.render_part("title", |style| style, window, cx))
        .when_some(open_url, |el, url| {
            el.cursor_pointer()
                .hover(|s| s.text_color(fg))
                .on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    link(&url, event, window, cx);
                })
        });
    if info.status == Status::Ready {
        v_flex()
            .my_1()
            .pl_3()
            .gap_1()
            .border_l_2()
            .border_color(border)
            .debug_selector(move || format!("reader-block-embed-{offset}"))
            .child(div().child(node.render_part(
                "body",
                |style| style.with_heading_base_font_size(px(font_size)),
                window,
                cx,
            )))
            .child(source)
            .into_any_element()
    } else {
        h_flex()
            .my_1()
            .text_size(px(12.))
            .text_color(muted)
            .debug_selector(move || format!("reader-block-embed-error-{offset}"))
            .child(node.render_part("reason", |style| style, window, cx))
            .when(info.status != Status::Pending, |el| el.child(source))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    struct Fixture {
        text: Entity<TextViewState>,
        clicked: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl Render for Fixture {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let clicked = self.clicked.clone();
            div().w(px(640.)).child(
                markdown_plugins(
                    TextView::new(&self.text),
                    Arc::new(move |url, _, _, _| clicked.lock().unwrap().push(url.into())),
                    Arc::new(|_| None),
                    SelectionFormat::Plain,
                )
                .style(reader_text_style(cx.theme()))
                .text_size(px(BODY_FONT_SIZE)),
            )
        }
    }

    #[gpui::test]
    fn block_embed_find_copy_and_source_links_use_visible_human_context(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("Projects")).unwrap();
        std::fs::write(temp.path().join("Projects/technical-name.md"), "# Human Roadmap\n\nVisible paragraph. ^unique\n\nFirst hidden candidate. ^dup\n\nSecond hidden candidate. ^DUP\n").unwrap();
        let vault = Vault::scan(temp.path()).unwrap();
        let source = okilum_core::render::reader_document_from_source(&vault, "host.md", "![[Projects/technical-name#^unique]]\n\n![[Projects/technical-name#^missing]]\n\n![[Projects/technical-name#^dup]]\n").rendered;
        let offsets: Vec<_> = source
            .match_indices("~~~~embed block ")
            .map(|(at, _)| at)
            .collect();
        assert_eq!(offsets.len(), 3);
        let text = cx.new(|cx| TextViewState::markdown(&source, cx));
        let clicked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (_, visual) = cx.add_window_view(|_, _| Fixture {
            text: text.clone(),
            clicked: clicked.clone(),
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.run_until_parked();
        let find = |visual: &mut VisualTestContext, query: &str| {
            text.update(visual, |state, cx| state.set_search_query(query, cx));
            visual.run_until_parked();
            text.read_with(visual, |state, _| state.search_status().1)
        };
        assert_eq!(
            find(visual, "Visible paragraph"),
            1,
            "positive control: the expanded body is searchable"
        );
        assert_eq!(find(visual, "Human Roadmap"), 3);
        assert_eq!(find(visual, "Block not found"), 1);
        assert_eq!(find(visual, "used more than once"), 1);
        for hidden in [
            "technical-name",
            "^unique",
            "#^",
            "MissingBlock",
            "hidden candidate",
        ] {
            assert_eq!(
                find(visual, hidden),
                0,
                "internal source context never becomes a visible part: {hidden}"
            );
        }
        find(visual, "");
        text.update(visual, |state, cx| state.select_all(cx));
        let copied = text.read_with(visual, |state, _| state.selected_text());
        assert!(copied.contains("Visible paragraph"));
        assert!(copied.contains("Block not found"));
        assert!(copied.contains("used more than once"));
        assert!(
            copied.find("Visible paragraph").unwrap() < copied.find("Human Roadmap").unwrap(),
            "copy follows body then source label"
        );
        assert!(
            !copied.contains("technical-name")
                && !copied.contains("^unique")
                && !copied.contains("MissingBlock")
        );
        text.update(visual, |state, cx| state.clear_selection(cx));
        visual.run_until_parked();
        for (i, offset) in offsets.iter().enumerate() {
            let selector: &'static str =
                Box::leak(format!("reader-embed-source-{offset}").into_boxed_str());
            let bounds = visual
                .debug_bounds(selector)
                .expect("source-note link renders");
            visual.simulate_click(bounds.center(), Modifiers::default());
            visual.run_until_parked();
            assert_eq!(
                clicked.lock().unwrap().last().unwrap(),
                if i == 0 {
                    "okilum://open/Projects/technical-name.md#%5Eunique"
                } else {
                    "okilum://open/Projects/technical-name.md"
                }
            );
        }
    }
}
