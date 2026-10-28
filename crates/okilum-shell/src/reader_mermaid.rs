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
    type Output = Result<Arc<Diagram>, Arc<String>>;
    fn load(
        request: Request,
        cx: &mut App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        cx.background_executor()
            .spawn(async move { render::render(request.source, request.key).map_err(Arc::new) })
    }
}
#[derive(Default)]
struct Cache {
    entries: Vec<(Request, Option<Arc<Diagram>>)>,
}
impl Global for Cache {}
const CACHE_BYTES: usize = 64 * 1024 * 1024;
fn retain(request: &Request, ready: Option<Arc<Diagram>>, cx: &mut App) {
    let cache = cx.default_global::<Cache>();
    let previous = cache
        .entries
        .iter()
        .position(|(r, _)| r == request)
        .and_then(|at| cache.entries.remove(at).1);
    cache.entries.push((request.clone(), ready.or(previous)));
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
        removed.push(cache.entries.remove(0).0);
    }
    for old in removed {
        cx.remove_asset::<DiagramAsset>(&old);
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
            retain(&request, None, cx);
            window.use_asset::<DiagramAsset>(&request, cx)
        };
        let image = match ready {
            Some(Ok(image)) => {
                retain(&request, Some(image.clone()), cx);
                state.update(cx, |s, _| s.last_good = Some(request.key.clone()));
                Some(image)
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
