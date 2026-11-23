use super::*;

/// Reader typography, derived from the live theme.
///
/// Built on every `render_main` rather than cached: the theme flips between
/// light and dark while the app runs (#37), and the highlight theme plus
/// `is_dark` (code background, heading colours) have to follow it. Colours
/// not set here are refined onto the themed defaults at layout time.
pub(super) fn reader_text_style(theme: &Theme) -> TextViewStyle {
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
    // Theme decoration slots, shared with Live Preview (#3154).
    .decorations(tokens.decorations);
    for (level, color) in (1u8..).zip(tokens.headings) {
        style = style.heading_level_color(level, color);
    }
    style
}

/// Payload for the custom "local-image" markdown block node. TextView's stock
/// image path always builds `Resource::Uri` and fetches via the HTTP client,
/// so local `file://` images can never load; this plugin renders standalone
/// image paragraphs through `Resource::Path` (fs::read) instead.
#[derive(Clone)]
struct LocalImage {
    url: String,
    drawing_size: Option<(f32, Option<f32>)>,
}
#[derive(Clone)]
struct LocalImages {
    offset: usize,
    images: Vec<LocalImage>,
    caption: String,
}

fn mdast_text(node: &markdown_ast::Node, out: &mut String) {
    match node {
        markdown_ast::Node::Text(t) => out.push_str(&t.value),
        markdown_ast::Node::InlineCode(c) => out.push_str(&c.value),
        _ => {}
    }
    if let Some(children) = node.children() {
        for c in children {
            mdast_text(c, out);
        }
    }
}

/// Payload for the custom "callout" block node (#46). An Obsidian callout is a
/// blockquote whose first line is `[!type]`; markdown-rs hands it over as
/// `Node::Blockquote`, so the block parser recognises it by its source and
/// the renderer draws a tinted box with a title row and the body as nested
/// Markdown. The body is re-rendered from source rather than from the mdast
/// children so lists, fences and nested quotes keep working unchanged.
#[derive(Clone)]
struct Callout {
    header: CalloutHeader,
    /// Body Markdown, `>` markers already stripped. Empty for a bare header.
    body: String,
    /// Byte offset of the quote in the document, for test selectors.
    offset: usize,
    /// Fold-state key of a foldable (`+`/`-`) callout (#651).
    key: u64,
}

/// Parse an Obsidian callout out of a blockquote node. `None` for a plain quote.
/// `fold_base` offsets the callout's fold-state key: a Live Preview block is
/// its own document, so identical callouts in two blocks must not share
/// their fold state (#1067). The Reader passes 0.
fn parse_callout(
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
pub(super) struct Embed {
    /// Vault-relative path of the embedded note, or the target as written
    /// when `missing`.
    path: String,
    /// Heading the embed names, if any.
    heading: Option<String>,
    missing: bool,
    pub(super) block: Option<okilum_core::render::block_embed::Info>,
    /// Byte offset of the fence in the document; keys the nested TextView's
    /// state so two embeds in one note never share one.
    pub(super) offset: usize,
}

/// Parse an expanded embed out of a code node. `None` for real code.
fn parse_embed(
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

/// Parse a paragraph or heading that contains an inline `<mark>` into a
/// `"highlight"` node. `None` for every other block, so plain paragraphs keep
/// TextView's stock Markdown rendering.
fn parse_highlight(
    node: &markdown_ast::Node,
    cx: &gpui_component::text::MarkdownParseContext<'_>,
) -> Option<MarkdownNode> {
    let children = match node {
        markdown_ast::Node::Paragraph(p) => &p.children,
        markdown_ast::Node::Heading(h) => &h.children,
        _ => return None,
    };
    let has_mark = children.iter().any(|c| {
        matches!(c, markdown_ast::Node::Html(h) if h.value.to_ascii_lowercase().starts_with("<mark"))
    });
    if !has_mark {
        return None;
    }
    let source = cx.node_source(node)?;
    // The `highlight` token is translucent, so it reads on every theme. It is
    // resolved when the block is parsed; a theme switch applies on re-parse.
    let color = brand::css_color(brand::reader_palette_current().highlight);
    let html = okilum_core::render::block_html(source)
        .replace("<mark>", &format!("<mark color=\"{color}\">"));
    let mut text = String::new();
    mdast_text(node, &mut text);
    Some(
        MarkdownNode::new("highlight", ())
            .html_part("body", html)
            .text(text)
            .markdown(source.to_string()),
    )
}

fn callout_look(header: &CalloutHeader, theme: &Theme) -> (Hsla, IconName) {
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

pub(super) fn backlink_occurrence(
    b: &Backlink,
) -> (String, Option<std::ops::Range<usize>>, Option<String>) {
    let (mut context, mut link) = b.context_plain_with_link();
    // A frontmatter relation reads as «key: Target» (#386)
    // and lands at the top: frontmatter is not rendered.
    let property = b.property.is_some();
    if let Some(key) = &b.property {
        let target = link
            .clone()
            .and_then(|r| context.get(r).map(str::to_owned))
            .unwrap_or_else(|| b.link.clone());
        let prefix = format!("{key}: ");
        link = Some(prefix.len()..prefix.len() + target.len());
        context = prefix + &target;
    }
    // Where to land: the linking line itself, which tells
    // occurrences apart better than the link text.
    let jump = (!property).then(|| context.chars().take(48).collect::<String>());
    let (context, link) = text_ranges::snippet(context, link, 160);
    (context, link, jump)
}

/// The «On this page» entries of a rendered Markdown source.
pub(super) fn note_outline(source: &str) -> Vec<okilum_core::document_links::HeadingEntry> {
    okilum_core::document_links::HeadingInventory::new(source)
        .entries
        .into_iter()
        .filter(|heading| heading.supported_container)
        .collect()
}

/// Install the reader's Markdown plugins and link handler on a TextView.
/// Recursive on purpose: a callout body is rendered by a nested TextView
/// built through the same function, so images and callouts inside a callout
/// render the same way as at top level.
pub(super) fn reader_plugins(
    root: PathBuf,
    view: TextView,
    entity: WeakEntity<Reader>,
    sel_format: SelectionFormat,
    states: prepared_links::States,
    identities: &[okilum_core::document_links::prepared::LinkIdentity],
    decorate_document: bool,
) -> TextView {
    let documents_entity = entity.clone();
    let view = reader_plugins_at(
        MarkdownScope::default(),
        root,
        view,
        entity,
        sel_format,
        states,
        identities,
    );
    if decorate_document {
        reader_document_decorations::install(view, documents_entity, None)
    } else {
        view
    }
}

#[derive(Default)]
pub(super) struct MarkdownScope {
    pub(super) fold_base: usize,
    pub(super) live: bool,
}

/// [`reader_plugins`] for a document that is one part of a note: `fold_base`
/// keeps its callouts' fold state apart from other parts' (#1067).
pub(super) fn reader_plugins_at(
    scope: MarkdownScope,
    root: PathBuf,
    view: TextView,
    entity: WeakEntity<Reader>,
    sel_format: SelectionFormat,
    states: prepared_links::States,
    identities: &[okilum_core::document_links::prepared::LinkIdentity],
) -> TextView {
    #[cfg(any(unix, windows))]
    let missing_cards = reader_hover::missing_cards(&states, identities);
    #[cfg(not(any(unix, windows)))]
    let missing_cards = {
        let _ = identities;
        Arc::new(std::collections::BTreeSet::new())
    };
    let hover_entity = entity.clone();
    let tasks_entity = entity.clone();
    let image_actions = entity.clone();
    let view = markdown_plugins_at(
        scope.fold_base,
        view,
        Arc::new(move |url, event, window, cx| {
            if matches!(event, ClickEvent::Mouse(e) if e.up.button == MouseButton::Right) {
                let _ = entity.update(cx, |this, cx| {
                    this.file_link_menu(url, event.position(), window, cx)
                });
            } else {
                // Cmd/Ctrl+click opens a resolved note beside, +Shift in a
                // new window (#773); every other link keeps its own route.
                let destination = reader_pane::Destination::of_click(event.modifiers());
                reader_link_navigation::handle_link_to(&entity, url, destination, window, cx);
            }
        }),
        Arc::new(move |url| {
            if url.starts_with(okilum_core::render::UNAVAILABLE_IMAGE) {
                return Some(MarkdownImage::Unavailable);
            }
            if let Some(payload) = url.strip_prefix("okilum-drawing-unavailable:") {
                return Some(MarkdownImage::DrawingUnavailable(payload.to_owned()));
            }
            if let Some(path) = url::Url::parse(url)
                .ok()
                .and_then(|url| url.to_file_path().ok())
            {
                if okilum_core::excalidraw::is_drawing(&path.to_string_lossy()) {
                    Some(MarkdownImage::Drawing(root.clone(), path))
                } else if reader_media::is_video(&path) {
                    Some(MarkdownImage::Video(root.clone(), path))
                } else if reader_audio::eligible(&path) {
                    Some(MarkdownImage::Audio(root.clone(), path))
                } else {
                    Some(MarkdownImage::Source(reader_files::image_source(path)))
                }
            } else if url.starts_with("https://")
                || url.starts_with("http://")
                || url.starts_with("data:")
            {
                Some(MarkdownImage::Source(note_upload::image_source(url)))
            } else {
                None
            }
        }),
        sel_format,
        scope.live,
        Some(image_actions),
    )
    .on_link_hover(move |url, active, position, window, cx| {
        let _ = hover_entity.update(cx, |this, cx| {
            if active {
                this.pointer_link =
                    Some((url.to_string(), this.pane.navigation.preparation_generation));
            } else if this
                .pointer_link
                .as_ref()
                .is_some_and(|(link, _)| link.as_str() == url.as_ref())
            {
                this.pointer_link = None;
            }
            this.hover_link(url, active, position, window, cx)
        });
    })
    .link_presentation(move |url| reader_hover::link_presentation(url, &states, &missing_cards));
    reader_tasks::plugins(view, tasks_entity)
}

pub(super) type MarkdownLinkHandler =
    Arc<dyn Fn(&str, &ClickEvent, &mut Window, &mut App) + Send + Sync>;
pub(super) enum MarkdownImage {
    Drawing(PathBuf, PathBuf),
    Video(PathBuf, PathBuf),
    Audio(PathBuf, PathBuf),
    DrawingUnavailable(String),
    Source(ImageSource),
    // Constructed only by the remote Brain image authority; renderer is shared.
    #[cfg_attr(not(all(unix, feature = "brain")), allow(dead_code))]
    Unavailable,
}
type MarkdownImageResolver = Arc<dyn Fn(&str) -> Option<MarkdownImage> + Send + Sync>;

/// The same recursive Markdown plugins serve local Reader paths and remote
/// Brain image bytes. The caller supplies navigation and image authority.
pub(super) fn markdown_plugins(
    view: TextView,
    link_handler: MarkdownLinkHandler,
    image_resolver: MarkdownImageResolver,
    sel_format: SelectionFormat,
) -> TextView {
    markdown_plugins_at(
        0,
        view,
        link_handler,
        image_resolver,
        sel_format,
        false,
        None,
    )
}

/// [`markdown_plugins`] for one part of a note; see [`reader_plugins_at`].
fn markdown_plugins_at(
    fold_base: usize,
    view: TextView,
    link_handler: MarkdownLinkHandler,
    image_resolver: MarkdownImageResolver,
    sel_format: SelectionFormat,
    live: bool,
    image_actions: Option<WeakEntity<Reader>>,
) -> TextView {
    let link_click = link_handler.clone();
    let parse_image = image_resolver.clone();
    let render_image = image_resolver.clone();
    let embed_link_handler = link_handler.clone();
    let footnote_link_handler = link_handler.clone();
    // Read mode only: a right-click on a table offers to copy it (#2025).
    reader_context_menu::install(
        reader_table_copy::install(view, image_actions.clone(), live),
        image_actions.clone(),
        live,
    )
    .selection_format(sel_format)
    .code_block_language(reader_code_language::resolver())
    .code_block_actions(reader_code::actions)
    .markdown_block_parser(reader_mermaid::parse)
    .markdown_block_renderer("mermaid", move |node, window, cx| {
        reader_mermaid::render(node, live, window, cx)
    })
    .markdown_block_parser(move |node, cx| {
        let markdown_ast::Node::Paragraph(p) = node else {
            return None;
        };
        let mut images = Vec::new();
        let mut caption = String::new();
        for child in &p.children {
            match child {
                markdown_ast::Node::Image(image)
                    if !image.url.starts_with(okilum_core::obsidian::MATH_SCHEME)
                        && parse_image(&image.url).is_some() =>
                {
                    images.push(LocalImage {
                        url: image.url.clone(),
                        drawing_size: image.title.as_deref().and_then(reader_drawing::parse_size),
                    });
                }
                other => caption.push_str(cx.node_source(other)?),
            }
        }
        if images.is_empty() {
            return None;
        }
        let mut node = MarkdownNode::new(
            "local-image",
            LocalImages {
                offset: cx.offset() + node.position()?.start.offset,
                images: images.clone(),
                caption: caption.trim().to_owned(),
            },
        )
        .markdown_part("caption", caption.trim().to_owned());
        for (index, image) in images.iter().enumerate() {
            if matches!(parse_image(&image.url), Some(MarkdownImage::Unavailable)) {
                let missing =
                    okilum_core::render::unavailable_target(&image.url).unwrap_or_default();
                node = node.plain_part(
                    format!("status-{index}"),
                    format!("File not found or ambiguous: {missing}"),
                );
            }
        }
        Some(node)
    })
    .markdown_block_renderer("local-image", move |node, window, cx| {
        let Some(data) = node.data::<LocalImages>() else {
            return div().into_any_element();
        };
        let mut column = v_flex().py_2().gap_1();
        for (index, image) in data.images.iter().enumerate() {
            let element = match render_image(&image.url) {
                Some(MarkdownImage::Audio(root, path)) => {
                    reader_audio::inline(root, path, data.offset + index, window, cx)
                }
                Some(MarkdownImage::Video(root, path)) => {
                    reader_media::video_card(&root, &path, cx)
                }
                Some(MarkdownImage::Drawing(root, path)) => reader_drawing::render(
                    &root,
                    &path,
                    image.drawing_size,
                    image_actions.clone(),
                    window,
                    cx,
                ),
                Some(MarkdownImage::DrawingUnavailable(payload)) => {
                    reader_drawing::unavailable(&payload, cx)
                }
                _ => {
                    if let Some(reader) = &image_actions {
                        if let Some(element) =
                            reader_image_actions::attachment(reader.clone(), &image.url, cx)
                        {
                            column = column.child(element);
                            continue;
                        }
                    }
                    let element = match render_image(&image.url) {
                        Some(MarkdownImage::Source(source)) => {
                            reader_image::ReaderImage::new(source)
                                .with_error_message("File not found or unreadable")
                                .into_any_element()
                        }
                        _ => div()
                            .debug_selector(|| "live-image-unavailable".into())
                            .child(node.render_part(
                                &format!("status-{index}"),
                                |style| style,
                                window,
                                cx,
                            ))
                            .into_any_element(),
                    };
                    match &image_actions {
                        Some(reader) => reader_image_actions::decorate(
                            reader.clone(),
                            &image.url,
                            element,
                            live,
                            cx,
                        ),
                        None => element,
                    }
                }
            };
            column = column.child(element);
        }
        if !data.caption.is_empty() {
            column = column.child(node.render_part("caption", |style| style, window, cx));
        }
        column.into_any_element()
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
                    .on_click(move |event, window, cx| embed_link(&open_url, event, window, cx)),
            )
            .child(div().px_3().py_2().child(node.render_part(
                "body",
                |style| style.with_heading_base_font_size(px(font_size)),
                _window,
                cx,
            )))
            .into_any_element()
    })
    .markdown_block_parser(parse_highlight)
    .markdown_block_renderer("highlight", move |node, _window, cx| {
        node.render_part("body", |style| style, _window, cx)
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
