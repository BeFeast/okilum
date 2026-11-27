use super::*;

pub(super) use okilum_shell_text::markdown_style::{
    note_outline, reader_text_style, MarkdownImage, MarkdownImageResolver, MarkdownLinkHandler,
    MarkdownScope,
};

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
    let parse_image = image_resolver.clone();
    let render_image = image_resolver;
    // Read mode only: a right-click on a table offers to copy it (#2025).
    let view = reader_context_menu::install(
        reader_table_copy::install(view, image_actions.clone(), live),
        image_actions.clone(),
        live,
    );
    okilum_shell_text::markdown_style::text_markdown_plugins(
        fold_base,
        view,
        link_handler,
        sel_format,
        live,
    )
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
}
