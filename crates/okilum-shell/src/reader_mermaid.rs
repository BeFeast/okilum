//! Shared Reader/LP Mermaid rendering. Every unavailable render stays code.
use super::*;
use gpui_component::highlighter::{LanguageConfig, LanguageRegistry};
mod render;
use render::{Diagram, Key};

/// The language name a ```` ```mermaid ```` fence resolves to.
pub(super) const MERMAID: &str = "mermaid";

/// Register the Mermaid grammar with the shared highlighter registry. Code
/// blocks look languages up by name, and Markdown injects fenced code the same
/// way, so this one registration covers every view. Idempotent.
pub(super) fn register() {
    LanguageRegistry::singleton().register(
        MERMAID,
        &LanguageConfig::new(
            MERMAID,
            tree_sitter::Language::new(tree_sitter_mermaid::LANGUAGE),
            vec![],
            tree_sitter_mermaid::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_mermaid::LOCALS_QUERY,
        ),
    );
}

#[derive(Clone)]
struct Block {
    source: Arc<str>,
    markdown: SharedString,
    offset: usize,
}
pub(super) fn parse(
    node: &markdown_ast::Node,
    cx: &gpui_component::text::MarkdownParseContext<'_>,
) -> Option<MarkdownNode> {
    let markdown_ast::Node::Code(code) = node else {
        return None;
    };
    if !code
        .lang
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case(MERMAID))
    {
        return None;
    }
    let markdown: SharedString = cx.node_source(node)?.to_owned().into();
    Some(
        MarkdownNode::new(
            "mermaid",
            Block {
                source: code.value.clone().into(),
                markdown: markdown.clone(),
                offset: cx.offset() + node.position()?.start.offset,
            },
        )
        .text(code.value.clone())
        .markdown(markdown),
    )
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct Request {
    key: Key,
    source: Arc<str>,
}
struct DiagramAsset;
impl Asset for DiagramAsset {
    type Source = Request;
    type Output = Result<std::sync::Weak<Diagram>, Arc<String>>;
    fn load(
        request: Request,
        cx: &mut App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        let work = cx.background_executor().spawn({
            let request = request.clone();
            async move { render::render(request.source, request.key).map_err(Arc::new) }
        });
        cx.spawn(async move |cx| {
            let result = work.await;
            // GPUI may keep a completed asset after its block leaves the view.
            // Only our budgeted cache owns the raster; the asset owns a weak ref.
            let output = result.as_ref().map(Arc::downgrade).map_err(Arc::clone);
            cx.update(|cx| complete(&request, result.ok(), cx));
            output
        })
    }
}
#[derive(Default)]
struct Cache {
    entries: Vec<(Request, Option<Arc<Diagram>>)>,
}
impl Global for Cache {}
const CACHE_BYTES: usize = 64 * 1024 * 1024;
fn retain(request: &Request, window: &mut Window, cx: &mut App) {
    let cache = cx.default_global::<Cache>();
    let previous = cache
        .entries
        .iter()
        .position(|(r, _)| r == request)
        .and_then(|at| cache.entries.remove(at).1);
    cache.entries.push((request.clone(), previous));
    evict(Some(window), cx);
}

/// Runs on the foreground executor when work completes, even if the block was
/// navigated away from and never renders again. Late evicted work stays evicted.
fn complete(request: &Request, image: Option<Arc<Diagram>>, cx: &mut App) {
    let cache = cx.default_global::<Cache>();
    if let Some((_, ready)) = cache.entries.iter_mut().find(|(r, _)| r == request) {
        *ready = image;
    }
    evict(None, cx);
}

fn evict(mut window: Option<&mut Window>, cx: &mut App) {
    let cache = cx.default_global::<Cache>();
    let mut removed = Vec::new();
    while cache.entries.len() > 32
        || cache
            .entries
            .iter()
            .filter_map(|(_, image)| image.as_ref())
            .map(|image| image.bytes)
            .sum::<usize>()
            > CACHE_BYTES
    {
        removed.push(cache.entries.remove(0));
    }
    for (old, image) in removed {
        cx.remove_asset::<DiagramAsset>(&old);
        if let Some(image) = image {
            cx.drop_image(image.image.clone(), window.as_deref_mut());
        }
    }
}
#[derive(Default)]
struct ViewState {
    last_good: Option<Key>,
    width: f32,
}

pub(super) fn render(node: &MarkdownNode, live: bool, _: &mut Window, _: &mut App) -> AnyElement {
    let Some(block) = node.data::<Block>() else {
        return div().into_any_element();
    };
    MermaidView {
        block: block.clone(),
        live,
    }
    .into_any_element()
}
#[derive(IntoElement)]
struct MermaidView {
    block: Block,
    live: bool,
}
impl RenderOnce for MermaidView {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state =
            window.use_keyed_state(("mermaid-view", self.block.offset), cx, |_, _| ViewState {
                width: 640.,
                ..Default::default()
            });
        let width = state.read(cx).width;
        let request = Request {
            key: Key::new(&self.block.source, width, cx.theme().is_dark()),
            source: self.block.source.clone(),
        };
        let ready = if self.block.source.len() > render::MAX_SOURCE {
            Some(Err(Arc::new("source limit".into())))
        } else {
            retain(&request, window, cx);
            window.use_asset::<DiagramAsset>(&request, cx)
        };
        let image = match ready {
            Some(Ok(image)) => {
                let image = image.upgrade();
                if image.is_some() {
                    state.update(cx, |s, _| s.last_good = Some(request.key.clone()));
                }
                image
            }
            Some(Err(_)) => {
                state.update(cx, |s, _| s.last_good = None);
                None
            }
            None => {
                let last = state.read(cx).last_good.as_ref();
                cx.global::<Cache>()
                    .entries
                    .iter()
                    .find(|(r, _)| Some(&r.key) == last)
                    .and_then(|(_, image)| image.clone())
            }
        };
        let content = if let Some(image) = image {
            div()
                .debug_selector(|| "mermaid-diagram".into())
                .max_w(px(image.width))
                .child(reader_image::ReaderImage::new(image.image.clone().into()))
                .into_any_element()
        } else {
            // Deliberately no Mermaid parser on this nested view: the exact
            // original fence uses today's highlighter without recursive capture.
            TextView::markdown(
                ("mermaid-code-fallback", self.block.offset),
                self.block.markdown,
            )
            .code_block_language(reader_code_language::resolver())
            .code_block_actions(reader_code::actions)
            .selectable(!self.live)
            .style(reader_text_zoom::document_style(cx))
            .text_size(px(reader_ui_state::font_size(cx)))
            .w_full()
            .into_any_element()
        };
        let observed = state.clone();
        div()
            .relative()
            .w_full()
            .min_w_0()
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let width = f32::from(bounds.size.width);
                        if render::bucket(observed.read(cx).width) != render::bucket(width) {
                            observed.update(cx, |s, cx| {
                                s.width = width;
                                cx.notify();
                            });
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn request(index: usize) -> Request {
        let source: Arc<str> = format!("flowchart TD\nA{index}-->B").into();
        Request {
            key: Key::new(&source, 640., false),
            source,
        }
    }

    fn diagram(bytes: usize) -> Arc<Diagram> {
        Arc::new(Diagram {
            image: Arc::new(RenderImage::new(vec![image::Frame::new(
                image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 255, 255])),
            )])),
            width: 2.,
            bytes,
        })
    }

    #[gpui::test]
    async fn asset_completion_is_accounted_without_rendering_again(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let request = request(0);
        let work = cx.update(|cx| {
            cx.default_global::<Cache>()
                .entries
                .push((request.clone(), None));
            DiagramAsset::load(request.clone(), cx)
        });
        let result = work.await.unwrap();
        cx.update(|cx| {
            let ready = cx.global::<Cache>().entries[0].1.as_ref().unwrap();
            assert!(
                ready.bytes > 0,
                "positive control: actual worker raster completed"
            );
            assert!(Arc::ptr_eq(ready, &result.upgrade().unwrap()));
            cx.global_mut::<Cache>().entries.clear();
            assert!(
                result.upgrade().is_none(),
                "GPUI's output cannot retain an evicted raster"
            );
        });
    }

    #[gpui::test]
    fn background_completions_enforce_budget_and_do_not_revive_evicted_work(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            for index in 0..5 {
                cx.default_global::<Cache>()
                    .entries
                    .push((request(index), None));
            }
            let first = diagram(CACHE_BYTES / 4);
            let output = Arc::downgrade(&first);
            complete(&request(0), Some(first), cx);
            assert!(
                output.upgrade().is_some(),
                "positive control: cache owns completed raster"
            );
            for index in 1..5 {
                complete(&request(index), Some(diagram(CACHE_BYTES / 4)), cx);
                let bytes: usize = cx
                    .global::<Cache>()
                    .entries
                    .iter()
                    .filter_map(|(_, image)| image.as_ref())
                    .map(|image| image.bytes)
                    .sum();
                assert!(bytes <= CACHE_BYTES);
            }
            assert!(output.upgrade().is_none());
            assert_eq!(cx.global::<Cache>().entries.len(), 4);
            complete(&request(0), Some(diagram(CACHE_BYTES / 4)), cx);
            assert!(!cx
                .global::<Cache>()
                .entries
                .iter()
                .any(|(r, _)| r == &request(0)));
        });
    }

    struct ImageHarness(Arc<RenderImage>);
    impl Render for ImageHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().child(img(self.0.clone()).size(px(20.)))
        }
    }

    #[gpui::test]
    fn eviction_releases_current_and_other_window_atlases(cx: &mut gpui::TestAppContext) {
        let diagram = diagram(16);
        let image = diagram.image.clone();
        let a = cx.add_window(|_, _| ImageHarness(image.clone()));
        let b = cx.add_window(|_, _| ImageHarness(image.clone()));
        cx.run_until_parked();
        for handle in [a, b] {
            handle
                .update(cx, |_, window, cx| {
                    window.simulate_next_frame(cx);
                    window.draw(cx).clear(cx);
                    assert!(
                        window.has_image_atlas_entry(&image),
                        "positive control: image was uploaded"
                    );
                })
                .unwrap();
        }
        a.update(cx, |_, window, cx| {
            let cache = cx.default_global::<Cache>();
            cache.entries.push((request(0), Some(diagram)));
            for index in 1..=32 {
                cache.entries.push((request(index), None));
            }
            evict(Some(window), cx);
            assert!(!window.has_image_atlas_entry(&image));
        })
        .unwrap();
        b.update(cx, |_, window, _| {
            assert!(!window.has_image_atlas_entry(&image));
        })
        .unwrap();
    }

    #[test]
    fn mermaid_fences_have_a_grammar_and_highlights() {
        // Control: before registration the language is unknown.
        assert!(LanguageRegistry::singleton()
            .language("mermaid-unregistered")
            .is_none());
        register();
        let config = LanguageRegistry::singleton().language(MERMAID).unwrap();
        let language = config.language.clone().unwrap();
        // The query compiles against this grammar, and a real diagram parses
        // without errors and yields highlight captures.
        let query = tree_sitter::Query::new(&language, &config.highlights).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let source = "flowchart TD\n  A[Start] -->|yes| B{Decision}\n  %% comment\n";
        let tree = parser.parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut captures = std::collections::BTreeSet::new();
        use tree_sitter::StreamingIterator as _;
        let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
        while let Some(m) = matches.next() {
            for capture in m.captures {
                captures.insert(query.capture_names()[capture.index as usize]);
            }
        }
        assert!(captures.contains("keyword"), "{captures:?}");
        assert!(captures.contains("comment"), "{captures:?}");
    }
}
