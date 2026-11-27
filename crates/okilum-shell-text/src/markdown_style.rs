//! Reader Markdown typography, block parsers/renderers (callouts, embeds,
//! footnotes, math, Mermaid, code actions/languages) and base plugin wiring
//! (#3235, epic #3241).
use crate::prelude::*;

/// Reader typography, derived from the live theme.
///
/// Built on every `render_main` rather than cached: the theme flips between
/// light and dark while the app runs (#37), and the highlight theme plus
/// `is_dark` (code background, heading colours) have to follow it. Colours
/// not set here are refined onto the themed defaults at layout time.
pub fn reader_text_style(theme: &Theme) -> TextViewStyle {
    let tokens = brand::reader_palette_for_theme(theme);
    let mut style = TextViewStyle {
        highlight_theme: theme.highlight_theme.clone(),
        is_dark: theme.is_dark(),
        ..TextViewStyle::default()
    }
    .table({
        let mut style = StyleRefinement::default();
        style.overflow.x = Some(Overflow::Scroll);
        style
    })
    .code_block(
        StyleRefinement::default()
            .bg(tokens.code_bg)
            .border_1()
            .border_color(tokens.code_border)
            .rounded(px(8.)),
    )
    .inline_code(HighlightStyle {
        background_color: Some(tokens.code_bg),
        ..Default::default()
    })
    .paragraph_gap(rems(0.75))
    // One scale and one colour per level for Reader and Live Preview
    // (#1076, #3147, #3153).
    .heading_font_size(|level, base| base * source_presentation::heading_scale(level))
    // Theme decoration slots, shared with Live Preview (#3154). `==x==`
    // reaches the renderer as inline `<mark>`, which takes the theme's
    // translucent highlight wherever it sits: in bold, links, table cells.
    .decorations(gpui_base::text::TextDecorations {
        highlight: Some(tokens.highlight),
        ..tokens.decorations
    });
    for (level, color) in (1u8..).zip(tokens.headings) {
        style = style.heading_level_color(level, color);
    }
    style
}

/// Payload for the custom "callout" block node (#46). An Obsidian callout is a
/// blockquote whose first line is `[!type]`; markdown-rs hands it over as
/// `Node::Blockquote`, so the block parser recognises it by its source and
/// the renderer draws a tinted box with a title row and the body as nested
/// Markdown. The body is re-rendered from source rather than from the mdast
/// children so lists, fences and nested quotes keep working unchanged.
#[derive(Clone)]
pub struct Callout {
    pub header: CalloutHeader,
    /// Body Markdown, `>` markers already stripped. Empty for a bare header.
    pub body: String,
    /// Byte offset of the quote in the document, for test selectors.
    pub offset: usize,
    /// Fold-state key of a foldable (`+`/`-`) callout (#651).
    pub key: u64,
}

/// Parse an Obsidian callout out of a blockquote node. `None` for a plain quote.
/// `fold_base` offsets the callout's fold-state key: a Live Preview block is
/// its own document, so identical callouts in two blocks must not share
/// their fold state (#1067). The Reader passes 0.
pub fn parse_callout(
    node: &markdown_ast::Node,
    cx: &gpui_component::text::MarkdownParseContext<'_>,
    fold_base: usize,
) -> Option<MarkdownNode> {
    let markdown_ast::Node::Blockquote(_) = node else {
        return None;
    };
    let source = cx.node_source(node)?;
    let header = callout::parse_header(source.lines().next()?)?;
    let body = callout::body_from_source(source);
    let offset = cx.offset() + node.position()?.start.offset;
    Some(
        MarkdownNode::new(
            "callout",
            Callout {
                header: header.clone(),
                body: body.clone(),
                offset,
                key: reader_obsidian::callout_key(fold_base + offset, source),
            },
        )
        .plain_part("title", header.title.clone())
        .markdown_part("body", body)
        .text(source.to_string())
        .markdown(source.to_string()),
    )
}

/// Payload for the custom "embed" block node (#49). Core expands `![[note]]`
/// into the target's body inside a tilde fence tagged [`EMBED_LANG`], with
/// the `okilum://open/` payload as the rest of the info string; markdown-rs
/// hands the fence over as `Node::Code`, so the block parser recognises it by
/// its language and the renderer draws a bordered box with a title row that
/// links to the note and the body as nested Markdown.
#[derive(Clone)]
pub struct Embed {
    /// Vault-relative path of the embedded note, or the target as written
    /// when `missing`.
    pub path: String,
    /// Heading the embed names, if any.
    pub heading: Option<String>,
    pub missing: bool,
    pub block: Option<okilum_core::render::block_embed::Info>,
    /// Byte offset of the fence in the document; keys the nested TextView's
    /// state so two embeds in one note never share one.
    pub offset: usize,
}

/// Parse an expanded embed out of a code node. `None` for real code.
pub fn parse_embed(
    node: &markdown_ast::Node,
    cx: &gpui_component::text::MarkdownParseContext<'_>,
) -> Option<MarkdownNode> {
    let markdown_ast::Node::Code(code) = node else {
        return None;
    };
    if code.lang.as_deref() != Some(EMBED_LANG) {
        return None;
    }
    let meta = code.meta.as_deref().unwrap_or("").trim();
    let offset = cx.offset() + node.position()?.start.offset;
    if let Some(info) = okilum_core::render::block_embed::Info::parse(meta) {
        let missing = info.status != okilum_core::render::block_embed::Status::Ready;
        let mut parsed = MarkdownNode::new(
            "embed",
            Embed {
                path: info.path.clone().unwrap_or_default(),
                heading: Some(format!("^{}", info.id)),
                missing,
                block: Some(info.clone()),
                offset,
            },
        )
        .text(info.title.clone())
        .markdown(cx.node_source(node).unwrap_or("").to_string());
        // Parts follow visual order so find, selection and copy see exactly
        // the body/error and source label that the renderer displays.
        if missing {
            parsed = parsed.plain_part("reason", reader_embeds::explanation(info.status));
        } else {
            parsed = parsed.markdown_part("body", code.value.clone());
        }
        if info.status != okilum_core::render::block_embed::Status::Pending {
            parsed = parsed.plain_part("title", info.title);
        }
        return Some(parsed);
    }

    let pending = meta
        .strip_prefix(okilum_core::render::EMBED_PENDING)
        .is_some_and(|rest| rest.starts_with(' '));
    let (path, heading, missing) = match meta.strip_prefix(if pending {
        okilum_core::render::EMBED_PENDING
    } else {
        EMBED_MISSING
    }) {
        Some(rest) if rest.starts_with(' ') || rest.is_empty() => {
            (rest.trim().to_string(), None, true)
        }
        _ => {
            let (rel, heading) = split_open_url(meta);
            (rel, heading, false)
        }
    };
    let title = if pending {
        format!("embed loading: {path}")
    } else if missing {
        format!("embed not found: {path}")
    } else {
        let title = okilum_core::Vault::title_of(&path);
        match &heading {
            Some(h) => format!("{title} › {h}"),
            None => title,
        }
    };
    Some(
        MarkdownNode::new(
            "embed",
            Embed {
                path,
                heading,
                missing,
                block: None,
                offset,
            },
        )
        .plain_part("title", title.clone())
        .markdown_part(
            "body",
            if missing {
                String::new()
            } else {
                code.value.clone()
            },
        )
        .text(title)
        .markdown(cx.node_source(node).unwrap_or("").to_string()),
    )
}

pub fn callout_look(header: &CalloutHeader, theme: &Theme) -> (Hsla, IconName) {
    if header.type_name == "important" {
        return (
            brand::reader_palette_for_theme(theme).callout_important,
            IconName::Asterisk,
        );
    }
    match header.kind {
        CalloutKind::Warning => (theme.warning, IconName::TriangleAlert),
        CalloutKind::Danger => (theme.danger, IconName::TriangleAlert),
        CalloutKind::Failure => (theme.danger, IconName::CircleX),
        CalloutKind::Bug => (theme.danger, IconName::Bot),
        CalloutKind::Tip => (theme.success, IconName::Star),
        CalloutKind::Success => (theme.success, IconName::CircleCheck),
        CalloutKind::Todo => (theme.info, IconName::Check),
        CalloutKind::Info | CalloutKind::Unknown => (theme.info, IconName::Info),
        CalloutKind::Note => (theme.info, IconName::FileText),
        // No orange in the theme palette; the Reader tokens carry one.
        CalloutKind::Question => (
            brand::reader_palette_for_theme(theme).callout_question,
            IconName::Search,
        ),
        CalloutKind::Summary => (theme.muted_foreground, IconName::Menu),
        CalloutKind::Example => (theme.muted_foreground, IconName::SquareTerminal),
        CalloutKind::Quote => (theme.muted_foreground, IconName::BookOpen),
    }
}

/// The «On this page» entries of a rendered Markdown source.
pub fn note_outline(source: &str) -> Vec<okilum_core::document_links::HeadingEntry> {
    okilum_core::document_links::HeadingInventory::new(source)
        .entries
        .into_iter()
        .filter(|heading| heading.supported_container)
        .collect()
}

#[derive(Default)]
pub struct MarkdownScope {
    pub fold_base: usize,
    pub live: bool,
}

pub type MarkdownLinkHandler = Arc<dyn Fn(&str, &ClickEvent, &mut Window, &mut App) + Send + Sync>;

pub enum MarkdownImage {
    Drawing(PathBuf, PathBuf),
    Video(PathBuf, PathBuf),
    Audio(PathBuf, PathBuf),
    DrawingUnavailable(String),
    Source(ImageSource),
    // Constructed only by the remote Brain/remote-markdown image authority; renderer is shared.
    Unavailable,
}

pub type MarkdownImageResolver = Arc<dyn Fn(&str) -> Option<MarkdownImage> + Send + Sync>;

/// Text-layer Markdown plugins: code language/actions, Mermaid, callouts,
/// embeds, footnotes, math, block markers, and link clicks. The app crate
/// layers viewer image/table/context-menu plugins on top of this.
pub fn text_markdown_plugins(
    fold_base: usize,
    view: TextView,
    link_handler: MarkdownLinkHandler,
    sel_format: SelectionFormat,
    live: bool,
) -> TextView {
    let link_click = link_handler.clone();
    let embed_link_handler = link_handler.clone();
    let footnote_link_handler = link_handler.clone();
    view.selection_format(sel_format)
        .code_block_language(reader_code_language::resolver())
        .code_block_actions(reader_code::actions)
        .markdown_block_parser(reader_mermaid::parse)
        .markdown_block_renderer("mermaid", move |node, window, cx| {
            reader_mermaid::render(node, live, window, cx)
        })
        .markdown_block_parser(move |node, cx| parse_callout(node, cx, fold_base))
        .markdown_block_renderer("callout", move |node, _window, cx| {
            let Some(data) = node.data::<Callout>() else {
                return div().into_any_element();
            };
            let theme = cx.theme();
            let (accent, icon) = callout_look(&data.header, theme);
            // `[!type]-` starts closed and `[!type]+` open; the title row
            // toggles either (#651). Without a sign there is nothing to fold.
            let (key, fold, offset) = (data.key, data.header.fold, data.offset);
            let foldable = fold != callout::Fold::None;
            let open = !foldable || reader_obsidian::callout_open(key, fold, cx);
            let mut el = v_flex()
                .my_1()
                .px_3()
                .py_2()
                .gap_1()
                .rounded_md()
                .border_1()
                .border_color(accent.opacity(0.45))
                .border_l(px(3.))
                .bg(accent.opacity(0.08))
                .child(
                    h_flex()
                        .id(("reader-callout-title", key))
                        .gap_2()
                        .items_center()
                        .text_color(accent)
                        .font_weight(FontWeight::BOLD)
                        .child(Icon::new(icon).small().text_color(accent))
                        .child(node.render_part("title", |style| style, _window, cx))
                        .when(foldable, |el| {
                            el.cursor_pointer()
                                .debug_selector(move || format!("reader-callout-fold-{offset}"))
                                .child(
                                    Icon::new(if open {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .small()
                                    .text_color(accent),
                                )
                                // Toggle on press and keep the press here: in Live
                                // Preview a press that reaches the editor reveals
                                // the callout's source instead (#1192).
                                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                                    cx.stop_propagation();
                                    reader_obsidian::toggle_callout(key, fold, window, cx);
                                })
                        }),
                );
            if open && !data.body.is_empty() {
                let font_size = reader_ui_state::font_size(cx);
                el = el.child(
                    div()
                        .debug_selector(move || format!("reader-callout-body-{offset}"))
                        .child(node.render_part(
                            "body",
                            |style| style.with_heading_base_font_size(px(font_size)),
                            _window,
                            cx,
                        )),
                );
            }
            el.into_any_element()
        })
        .markdown_block_parser(parse_embed)
        .markdown_block_renderer("embed", move |node, _window, cx| {
            let Some(data) = node.data::<Embed>() else {
                return div().into_any_element();
            };
            if data.block.is_some() {
                return reader_embeds::render_block(node, &embed_link_handler, _window, cx);
            }
            let theme = cx.theme();
            let border = theme.border;
            let muted = theme.muted_foreground;
            let fg = theme.foreground;
            if data.missing {
                return div()
                    .my_1()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .text_sm()
                    .italic()
                    .text_color(muted)
                    .child(node.render_part("title", |style| style, _window, cx))
                    .into_any_element();
            }
            let font_size = reader_ui_state::font_size(cx);
            let embed_link = embed_link_handler.clone();
            let open_url = match &data.heading {
                Some(h) => format!("{WIKI_SCHEME}{}#{h}", data.path),
                None => format!("{WIKI_SCHEME}{}", data.path),
            };
            v_flex()
                .my_1()
                .rounded_md()
                .border_1()
                .border_color(border)
                .child(
                    h_flex()
                        .id(("embed-title", data.offset as u64))
                        .px_3()
                        .py_1()
                        .gap_2()
                        .items_center()
                        .bg(theme.accent.opacity(0.5))
                        .text_sm()
                        .text_color(muted)
                        .cursor_pointer()
                        .hover(|s| s.text_color(fg).bg(theme.accent))
                        .child(Icon::new(IconName::FileText).small().text_color(muted))
                        .child(node.render_part("title", |style| style, _window, cx))
                        .on_click(move |event, window, cx| {
                            embed_link(&open_url, event, window, cx)
                        }),
                )
                .child(div().px_3().py_2().child(node.render_part(
                    "body",
                    |style| style.with_heading_base_font_size(px(font_size)),
                    _window,
                    cx,
                )))
                .into_any_element()
        })
        .markdown_block_parser(reader_obsidian::parse_footnote)
        .markdown_block_renderer("footnote", move |node, window, cx| {
            reader_obsidian::render_footnote(node, &footnote_link_handler, window, cx)
        })
        .markdown_inline_parser(reader_obsidian::parse_inline_math)
        .markdown_block_parser(reader_obsidian::parse_math)
        .markdown_block_renderer("math", reader_obsidian::render_math)
        .markdown_block_parser(reader_obsidian::parse_block_marker)
        .markdown_block_renderer("block-marker", |_, _, _| div())
        .on_link_click(move |url, ev, window, cx| link_click(url, ev, window, cx))
}

#[derive(Clone)]
struct StandaloneImages {
    urls: Vec<String>,
    caption: String,
}

/// Standalone Markdown plugins for callers in `okilum-shell-text` and
/// `okilum-shell-brain` (remote Markdown, Brain preview, and tests) that
/// resolve `MarkdownImage::Source` or `MarkdownImage::Unavailable`.
pub fn markdown_plugins(
    view: TextView,
    link_handler: MarkdownLinkHandler,
    image_resolver: MarkdownImageResolver,
    sel_format: SelectionFormat,
) -> TextView {
    let parse_image = image_resolver.clone();
    let render_image = image_resolver;
    text_markdown_plugins(0, view, link_handler, sel_format, false)
        .markdown_block_parser(move |node, cx| {
            let markdown_ast::Node::Paragraph(p) = node else {
                return None;
            };
            let mut urls = Vec::new();
            let mut caption = String::new();
            for child in &p.children {
                match child {
                    markdown_ast::Node::Image(image)
                        if !image.url.starts_with(okilum_core::obsidian::MATH_SCHEME)
                            && parse_image(&image.url).is_some() =>
                    {
                        urls.push(image.url.clone());
                    }
                    other => caption.push_str(cx.node_source(other)?),
                }
            }
            if urls.is_empty() {
                return None;
            }
            let mut node = MarkdownNode::new(
                "local-image",
                StandaloneImages {
                    urls: urls.clone(),
                    caption: caption.trim().to_owned(),
                },
            )
            .markdown_part("caption", caption.trim().to_owned());
            for (index, url) in urls.iter().enumerate() {
                if matches!(parse_image(url), Some(MarkdownImage::Unavailable)) {
                    let missing = okilum_core::render::unavailable_target(url).unwrap_or_default();
                    node = node.plain_part(
                        format!("status-{index}"),
                        format!("File not found or ambiguous: {missing}"),
                    );
                }
            }
            Some(node)
        })
        .markdown_block_renderer("local-image", move |node, window, cx| {
            let Some(data) = node.data::<StandaloneImages>() else {
                return div().into_any_element();
            };
            let mut column = v_flex().py_2().gap_1();
            for (index, url) in data.urls.iter().enumerate() {
                if let Some(MarkdownImage::Source(source)) = render_image(url) {
                    column = column.child(img(source).max_w_full());
                } else {
                    column = column.child(
                        div()
                            .debug_selector(|| "live-image-unavailable".into())
                            .child(node.render_part(
                                &format!("status-{index}"),
                                |style| style,
                                window,
                                cx,
                            )),
                    );
                }
            }
            if !data.caption.is_empty() {
                column = column.child(node.render_part("caption", |style| style, window, cx));
            }
            column.into_any_element()
        })
}
