//! Reader blocks for the Obsidian syntax core rewrites (#651): footnote
//! definitions, display math, block-ID markers, and callout fold state.
//!
//! Core turns each construct into Markdown a stock renderer can show (see
//! `tessera_core::obsidian`); these plugins give the tagged fences their
//! Reader look. Every plugin replaces exactly one top-level block, so block
//! indices from core land on the right list item.
use super::*;
use tessera_core::callout::Fold;
use tessera_core::obsidian::{self, FOOTNOTE_BACK_SCHEME, FOOTNOTE_LANG, MATH_LANG};

#[derive(Clone)]
pub(super) struct Footnote {
    pub number: usize,
    pub id: String,
    pub offset: usize,
}

/// Parse a [`FOOTNOTE_LANG`] fence into a `"footnote"` node.
pub(super) fn parse_footnote(
    node: &markdown_ast::Node,
    cx: &gpui_component::text::MarkdownParseContext<'_>,
) -> Option<MarkdownNode> {
    let markdown_ast::Node::Code(code) = node else {
        return None;
    };
    if code.lang.as_deref() != Some(FOOTNOTE_LANG) {
        return None;
    }
    let (number, id) = obsidian::parse_footnote_info(code.meta.as_deref().unwrap_or(""))?;
    let offset = cx.offset() + node.position()?.start.offset;
    Some(
        MarkdownNode::new("footnote", Footnote { number, id, offset })
            .markdown_part("body", code.value.clone())
            .text(format!("{number}. {}", code.value))
            .markdown(cx.node_source(node).unwrap_or("").to_string()),
    )
}

/// Number, body, and a glyph back to the reference: Obsidian's footnote
/// list, without a frame.
pub(super) fn render_footnote(
    node: &MarkdownNode,
    link: &MarkdownLinkHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(data) = node.data::<Footnote>() else {
        return div().into_any_element();
    };
    let muted = cx.theme().muted_foreground;
    let font_size = reader_ui_state::font_size(cx);
    let back = format!(
        "{FOOTNOTE_BACK_SCHEME}{}",
        tessera_core::document_links::encode(&data.id)
    );
    let link = link.clone();
    h_flex()
        .items_start()
        .gap_2()
        .debug_selector({
            let offset = data.offset;
            move || format!("reader-footnote-{offset}")
        })
        .child(
            div()
                .min_w(px(24.))
                .flex_none()
                .text_right()
                .text_color(muted)
                .child(format!("{}.", data.number)),
        )
        .child(div().flex_1().min_w_0().child(node.render_part(
            "body",
            |style| style.with_heading_base_font_size(px(font_size)),
            window,
            cx,
        )))
        .child(
            Button::new(("reader-footnote-back", data.offset as u64))
                .xsmall()
                .ghost()
                .icon(IconName::ArrowUp)
                .tooltip("Back to reference")
                .accessibility_label("Back to reference")
                .debug_selector({
                    let offset = data.offset;
                    move || format!("reader-footnote-back-{offset}")
                })
                .on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    link(&back, event, window, cx)
                }),
        )
        .into_any_element()
}

#[derive(Clone)]
pub(super) struct Math {
    pub offset: usize,
}

/// Parse a [`MATH_LANG`] fence into a `"math"` node.
pub(super) fn parse_math(
    node: &markdown_ast::Node,
    cx: &gpui_component::text::MarkdownParseContext<'_>,
) -> Option<MarkdownNode> {
    let markdown_ast::Node::Code(code) = node else {
        return None;
    };
    if code.lang.as_deref() != Some(MATH_LANG) {
        return None;
    }
    let offset = cx.offset() + node.position()?.start.offset;
    Some(
        MarkdownNode::new("math", Math { offset })
            .plain_part("tex", code.value.clone())
            .text(code.value.clone())
            .markdown(cx.node_source(node).unwrap_or("").to_string()),
    )
}

/// The formula's TeX source, centred in the monospace face on the code
/// tint: there is no math renderer, and the source is the honest fallback.
pub(super) fn render_math(node: &MarkdownNode, window: &mut Window, cx: &mut App) -> AnyElement {
    let Some(data) = node.data::<Math>() else {
        return div().into_any_element();
    };
    let offset = data.offset;
    let theme = cx.theme();
    let font = theme.mono_font_family.clone();
    let bg = brand::reader_palette_for_theme(theme).code_bg;
    h_flex()
        .my_1()
        .px_3()
        .py_2()
        .justify_center()
        .rounded(px(8.))
        .bg(bg)
        .font_family(font)
        .debug_selector(move || format!("reader-math-{offset}"))
        .child(node.render_part("tex", |style| style, window, cx))
        .into_any_element()
}

/// A block ID alone on its line, left by core as a marker: it occupies no
/// space, but stays its own block so landing indices match core's.
pub(super) fn parse_block_marker(
    node: &markdown_ast::Node,
    _cx: &gpui_component::text::MarkdownParseContext<'_>,
) -> Option<MarkdownNode> {
    let markdown_ast::Node::Html(html) = node else {
        return None;
    };
    let id = obsidian::marker_id(&html.value)?;
    Some(MarkdownNode::new("block-marker", id.to_string()))
}

/// Fold overrides for foldable callouts, by callout key. A Global rather than
/// element state: the reader's list drops off-screen items, and a callout the
/// reader opened must not snap shut when it scrolls back into view.
#[derive(Default)]
struct CalloutFolds(std::collections::HashMap<u64, bool>);

impl Global for CalloutFolds {}

/// A key that tells callouts in one document apart and survives re-renders.
pub(super) fn callout_key(offset: usize, source: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    offset.hash(&mut hasher);
    source.hash(&mut hasher);
    hasher.finish()
}

pub(super) fn callout_open(key: u64, fold: Fold, cx: &App) -> bool {
    cx.try_global::<CalloutFolds>()
        .and_then(|folds| folds.0.get(&key).copied())
        .unwrap_or(fold != Fold::Closed)
}

pub(super) fn toggle_callout(key: u64, fold: Fold, window: &mut Window, cx: &mut App) {
    let open = callout_open(key, fold, cx);
    cx.default_global::<CalloutFolds>().0.insert(key, !open);
    window.refresh();
}

/// Handle the in-document footnote links. `None` when `url` is not one.
pub(super) fn footnote_landing(url: &str, source: &str) -> Option<Result<usize, &'static str>> {
    if let Some(id) = url.strip_prefix(obsidian::FOOTNOTE_SCHEME) {
        let id = tessera_core::document_links::decode(id);
        return Some(
            obsidian::footnote_block(source, &id).ok_or("This footnote has no definition."),
        );
    }
    let id = url.strip_prefix(FOOTNOTE_BACK_SCHEME)?;
    let id = tessera_core::document_links::decode(id);
    Some(
        obsidian::footnote_reference_block(source, &id)
            .ok_or("This footnote is no longer referenced."),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    struct Fixture {
        source: String,
        clicked: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl Render for Fixture {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let clicked = self.clicked.clone();
            div().w(px(640.)).child(
                markdown_plugins(
                    TextView::markdown("obsidian-fixture", self.source.clone()),
                    Arc::new(move |url, _, _, _| clicked.lock().unwrap().push(url.to_string())),
                    Arc::new(|_| None),
                    SelectionFormat::default(),
                )
                .style(reader_text_style(cx.theme()))
                .text_size(px(BODY_FONT_SIZE)),
            )
        }
    }

    /// Debug selectors are `&'static str`; tests build them per offset.
    fn sel(selector: String) -> &'static str {
        Box::leak(selector.into_boxed_str())
    }

    fn rendered_fixture() -> String {
        let vault = Vault::from_note_paths(["obsidian-syntax.md".into()]);
        tessera_core::render::reader_document_from_source(
            &vault,
            "obsidian-syntax.md",
            include_str!("../../../fixtures/reader/obsidian-syntax.md"),
        )
        .rendered
    }

    #[gpui::test]
    fn corpus_note_renders_obsidian_blocks(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let source = rendered_fixture();
        let offset = |needle: &str| source.find(needle).expect(needle);
        let math = offset("~~~~math");
        let footnote = offset("~~~~footnote 1 ");
        let (_, visual) = cx.add_window_view(|_, _| Fixture {
            source: source.clone(),
            clicked: Default::default(),
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let math_bounds = visual
            .debug_bounds(sel(format!("reader-math-{math}")))
            .expect("display math renders as a math block");
        assert!(math_bounds.size.height > px(BODY_FONT_SIZE));
        assert!(
            visual
                .debug_bounds(sel(format!("reader-footnote-{footnote}")))
                .is_some(),
            "footnote definitions render as footnote blocks"
        );
    }

    #[gpui::test]
    fn footnote_back_link_routes_through_the_link_handler(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        // Short enough that the footnote is inside the test window.
        let source = tessera_core::obsidian::after_links("Claim.[^1]\n\n[^1]: Source.\n");
        let footnote = source.find("~~~~footnote").unwrap();
        let clicked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (_, visual) = cx.add_window_view(|_, _| Fixture {
            source,
            clicked: clicked.clone(),
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let back = visual
            .debug_bounds(sel(format!("reader-footnote-back-{footnote}")))
            .expect("back-link glyph");
        visual.simulate_click(back.center(), Modifiers::default());
        assert_eq!(
            clicked.lock().unwrap().as_slice(),
            ["tessera://footnote-back/1"],
        );
    }

    #[gpui::test]
    fn foldable_callouts_start_as_written_and_toggle(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let source = "> [!faq]- Closed question\n> Hidden answer body.\n\n> [!tip]+ Open tip\n> Visible tip body.\n\n> [!note] Plain\n> Always shown.\n".to_string();
        let (_, visual) = cx.add_window_view(|_, _| Fixture {
            source: source.clone(),
            clicked: Default::default(),
        });
        let draw = |visual: &mut gpui::VisualTestContext| {
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
        };
        draw(visual);
        let closed = source.find("> [!faq]").unwrap();
        let open = source.find("> [!tip]").unwrap();
        let plain = source.find("> [!note]").unwrap();
        assert!(visual
            .debug_bounds(sel(format!("reader-callout-body-{closed}")))
            .is_none());
        assert!(visual
            .debug_bounds(sel(format!("reader-callout-body-{open}")))
            .is_some());
        assert!(
            visual
                .debug_bounds(sel(format!("reader-callout-body-{plain}")))
                .is_some(),
            "positive control: a plain callout body renders"
        );
        assert!(
            visual
                .debug_bounds(sel(format!("reader-callout-fold-{plain}")))
                .is_none(),
            "a callout without +/- is not foldable"
        );
        let toggle = visual
            .debug_bounds(sel(format!("reader-callout-fold-{closed}")))
            .expect("fold toggle");
        visual.simulate_click(toggle.center(), Modifiers::default());
        draw(visual);
        assert!(
            visual
                .debug_bounds(sel(format!("reader-callout-body-{closed}")))
                .is_some(),
            "clicking the title opens a closed callout"
        );
        let toggle = visual
            .debug_bounds(sel(format!("reader-callout-fold-{open}")))
            .unwrap();
        visual.simulate_click(toggle.center(), Modifiers::default());
        draw(visual);
        assert!(visual
            .debug_bounds(sel(format!("reader-callout-body-{open}")))
            .is_none());
    }

    #[gpui::test]
    fn reader_lands_block_references_and_footnotes(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root = std::env::temp_dir().join(format!("tessera-651-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let filler = "Filler paragraph for scrolling.\n\n".repeat(60);
        std::fs::write(
            root.join("long.md"),
            format!("# Long\n\nClaim.[^1]\n\n{filler}Target paragraph. ^target\n\n{filler}[^1]: Source.\n"),
        )
        .unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("long.md".into()),
                        ..Opts::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = reader.unwrap();
        let settle = |visual: &mut gpui::VisualTestContext| {
            visual.run_until_parked();
            visual.executor().advance_clock(Duration::from_millis(300));
            visual.run_until_parked();
        };
        settle(visual);
        let top = |visual: &mut gpui::VisualTestContext| {
            view.read_with(visual, |v, cx| {
                v.content.read(cx).list_state().logical_scroll_top().item_ix
            })
        };
        assert_eq!(top(visual), 0, "positive control: opens at the top");
        // Find-in-note sees rendered text: the block ID is not in it.
        let matches = |visual: &mut gpui::VisualTestContext, query: &'static str| {
            view.update(visual, |v, cx| {
                v.content.update(cx, |s, cx| s.set_search_query(query, cx));
            });
            visual.run_until_parked();
            view.read_with(visual, |v, cx| v.content.read(cx).search_status().1)
        };
        assert_eq!(matches(visual, "Target paragraph."), 1, "positive control");
        assert_eq!(matches(visual, "^target"), 0, "the block ID is hidden");
        assert_eq!(matches(visual, "!--"), 0, "the marker is not shown either");
        assert_eq!(matches(visual, ""), 0);
        let (target, definition, reference) = view.read_with(visual, |v, _| {
            let inventory = tessera_core::document_links::HeadingInventory::new(&v.note_source);
            (
                inventory.locate("^target").unwrap().block,
                tessera_core::obsidian::footnote_block(&v.note_source, "1").unwrap(),
                tessera_core::obsidian::footnote_reference_block(&v.note_source, "1").unwrap(),
            )
        });
        assert_eq!(reference, 1);
        assert!(target > 30 && definition > target);
        view.update_in(visual, |v, window, cx| {
            v.open_note_at("long.md", None, Some("^target"), window, cx)
        });
        settle(visual);
        assert_eq!(top(visual), target, "[[long#^target]] lands on the block");
        let weak = view.downgrade();
        visual.update(|window, cx| handle_link(&weak, "tessera://footnote/1", window, cx));
        settle(visual);
        // The footnote is the last block: the list clamps at its end rather
        // than lifting it to the top, so it lands in view, not at the top.
        let landed = top(visual);
        assert!(
            landed > target && landed <= definition,
            "a reference scrolls to its footnote ({landed})"
        );
        view.read_with(visual, |v, _| assert_eq!(v.link_notice, None));
        visual.update(|window, cx| handle_link(&weak, "tessera://footnote-back/1", window, cx));
        settle(visual);
        assert_eq!(top(visual), reference, "the back-link returns to the text");
        view.update_in(visual, |v, window, cx| {
            v.open_note_at("long.md", None, Some("^missing"), window, cx)
        });
        settle(visual);
        view.read_with(visual, |v, _| {
            assert_eq!(
                v.link_notice.as_deref(),
                Some("No block with that ID exists in the current document.")
            );
        });
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn footnote_links_land_on_blocks() {
        let source = rendered_fixture();
        let to_def = footnote_landing("tessera://footnote/1", &source)
            .unwrap()
            .unwrap();
        let back = footnote_landing("tessera://footnote-back/1", &source)
            .unwrap()
            .unwrap();
        assert!(to_def > back, "definitions sit after their reference");
        assert!(footnote_landing("tessera://footnote/nope", &source)
            .unwrap()
            .is_err());
        assert!(footnote_landing("tessera://open/x.md", &source).is_none());
    }
}
