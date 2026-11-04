//! Source uses the native editor hover lifecycle and the rendered image's
//! bounded, revision-checked metadata reader. No filesystem work runs on the UI.
use super::*;
use gpui_component::input::{HoverProvider, Rope, RopeExt};

pub(crate) fn provider(reader: WeakEntity<Reader>) -> Rc<dyn HoverProvider> {
    Rc::new(SourceHover(reader))
}
struct SourceHover(WeakEntity<Reader>);

impl HoverProvider for SourceHover {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Option<lsp_types::Hover>>> {
        diagnostics(format_args!(
            "source hover offset={offset} bytes={}",
            text.len()
        ));
        if text.len() > okilum_core::source_classifier::MAX_BYTES {
            return Task::ready(Ok(None));
        }
        let Some(reader) = self.0.upgrade() else {
            return Task::ready(Ok(None));
        };
        let state = reader.read(cx);
        let vault = state.vault.clone();
        let note = state.current_rel.clone();
        let generation = state.navigation.preparation_generation;
        let root = state.vault_root.clone();
        let source = text.clone();
        let weak = self.0.clone();
        cx.spawn(async move |cx| {
            // Pointer movement cancels this task before parsing or header I/O.
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            let snapshot = source.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let text = snapshot.to_string();
                    let Some(image) =
                        okilum_core::render::image_reference::references(&text, &vault, &note)
                            .into_iter()
                            .find(|image| image.range.contains(&offset))
                    else {
                        return None;
                    };
                    let range = lsp_types::Range::new(
                        snapshot.offset_to_position(image.range.start),
                        snapshot.offset_to_position(image.range.end),
                    );
                    let Some(url) = image.url else {
                        return Some((
                            range,
                            format!(
                                "{}\n\nFile not found, outside this vault, or ambiguous.",
                                image.target
                            ),
                        ));
                    };
                    let parsed = url::Url::parse(&url).ok()?;
                    let relative = okilum_core::render::image_reference::local_path(&root, &parsed)
                        .and_then(|path| {
                            path.strip_prefix(&root)
                                .ok()
                                .map(okilum_core::vault::note_path)
                        });
                    let Some(relative) = relative else {
                        return Some((range, format!("{url}\n\nRight-click for file actions")));
                    };
                    if vault.cloud_placeholders.contains(&root.join(&relative)) {
                        return Some((
                            range,
                            format!("{relative}\n\nDownload from iCloud before opening."),
                        ));
                    }
                    let details = match metadata::lookup(root, relative.clone(), None).await {
                        metadata::State::Ready(metadata::Facts {
                            bytes,
                            pixels: Some((w, h)),
                        }) => format!("{bytes} bytes · {w} × {h} px"),
                        metadata::State::Ready(metadata::Facts {
                            bytes,
                            pixels: None,
                        }) => format!("{bytes} bytes"),
                        _ => "File not found or unreadable".to_owned(),
                    };
                    Some((
                        range,
                        format!("{relative}\n\n{details}\n\nRight-click for file actions"),
                    ))
                })
                .await;
            let source_range = result.as_ref().map(|(range, _)| {
                source.position_to_offset(&range.start)..source.position_to_offset(&range.end)
            });
            // A tooltip from an edited/replaced note cannot land on new text.
            let current = weak
                .read_with(cx, |reader, cx| {
                    if let Some(input) = reader.source_input() {
                        let input = input.read(cx);
                        diagnostics(format_args!(
                            "source anchor={:?}",
                            source_range
                                .as_ref()
                                .and_then(|range| input.range_to_bounds(range))
                        ));
                    }
                    reader.navigation.preparation_generation == generation
                        && reader.editing.as_ref().is_some_and(|editing| {
                            editing.input().read(cx).source_snapshot().text.rope() == &source
                        })
                })
                .unwrap_or(false);
            diagnostics(format_args!(
                "source hover found={} current={current}",
                result.is_some()
            ));
            if std::env::var_os("OKILUM_EMBED_DIAGNOSTICS").is_some() {
                let weak = weak.clone();
                cx.spawn(async move |cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(300))
                        .await;
                    let _ = weak.read_with(cx, |reader, cx| {
                        diagnostics(format_args!(
                            "source popup retained={}",
                            reader
                                .source_input()
                                .is_some_and(|input| input.read(cx).hover_popover().is_some())
                        ));
                    });
                })
                .detach();
            }
            Ok(result
                .filter(|_| current)
                .map(|(range, value)| lsp_types::Hover {
                    contents: lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
                        kind: lsp_types::MarkupKind::PlainText,
                        value,
                    }),
                    range: Some(range),
                }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn issue1622_source_hover_and_live_caption_use_the_captured_file(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        std::fs::create_dir_all(root.join("attachments")).unwrap();
        let root = root.canonicalize().unwrap();
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(4, 7)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        for path in ["root.png", "attachments/photo.png"] {
            std::fs::write(root.join(path), png.get_ref()).unwrap();
        }
        std::fs::write(root.join("attachments/manual.pdf"), b"synthetic pdf").unwrap();
        let source = "# Files\n\n![[root.png]]\n\n![[attachments/photo.png]]\n\n![[attachments/manual.pdf]]\n\n![[missing.png]]\n\n`![[ignored.png]]`\n";
        std::fs::write(root.join("note.md"), source).unwrap();
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("note.md".into()),
                        session_directory: Some(fixture.path().join("state")),
                        index_dir: Some(fixture.path().join("index")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let reader = reader.unwrap();
        visual.simulate_resize(size(px(1400.), px(1100.)));
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx)
        });
        visual.run_until_parked();
        let input = reader.read_with(visual, |reader, _| reader.source_input().unwrap());
        for (path, expected) in [
            ("root.png", "4 × 7 px"),
            ("attachments/photo.png", "4 × 7 px"),
            ("attachments/manual.pdf", "13 bytes"),
            ("missing.png", "File not found"),
        ] {
            let result = Rc::new(std::cell::RefCell::new(None));
            let output = result.clone();
            visual.update(|window, cx| {
                let (provider, text) = {
                    let state = input.read(cx);
                    (
                        state
                            .lsp()
                            .hover_provider
                            .clone()
                            .expect("Source installs native hover"),
                        state.source_snapshot().text.rope().clone(),
                    )
                };
                let task = provider.hover(&text, source.find(path).unwrap(), window, cx);
                cx.spawn(async move |_| {
                    *output.borrow_mut() = Some(task.await.unwrap());
                })
                .detach();
            });
            visual.run_until_parked();
            visual.executor().advance_clock(Duration::from_millis(500));
            visual.run_until_parked();
            let result = result.borrow();
            let hover = result
                .as_ref()
                .expect("hover task ran")
                .as_ref()
                .expect("embed has a hover");
            let lsp_types::HoverContents::Markup(content) = &hover.contents else {
                panic!("plain hover");
            };
            assert!(content.value.contains(path), "{}", content.value);
            assert!(content.value.contains(expected), "{}", content.value);
        }
        let hover_point = input.read_with(visual, |input, _| {
            let at = source.find("attachments/photo.png").unwrap();
            input
                .range_to_bounds(&(at..at + 5))
                .expect("source embed has native bounds")
                .center()
        });
        visual.simulate_mouse_move(hover_point, None, Modifiers::default());
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(500));
        visual.run_until_parked();
        for _ in 0..3 {
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.run_until_parked();
        }
        input.read_with(visual, |input, _| {
            assert!(
                input.hover_popover().is_some(),
                "native mouse hover reaches the provider"
            )
        });
        assert!(
            visual.debug_bounds("source-embed-hover").is_some(),
            "Source hint is painted in Reader's overlay layer"
        );
        // Copy path acts on the clicked attachment, and never changes the note.
        reader.update_in(visual, |reader, window, cx| {
            let reference = okilum_core::render::image_reference::references(
                source,
                &reader.vault,
                &reader.current_rel,
            )
            .into_iter()
            .find(|image| image.target == "attachments/manual.pdf")
            .unwrap();
            reader.image_command(
                &ImageCommand {
                    target: Target::reference(reader, &reference),
                    command: Command::CopyRelative,
                },
                window,
                cx,
            );
        });
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "attachments/manual.pdf"
            )
        });
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_live_preview(window, cx)
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("embed-path-caption").is_some(),
            "Live shows a file path caption"
        );
        assert!(
            visual.debug_bounds("live-image-unavailable").is_some(),
            "broken embed stays visible beside real images"
        );
        // Use a real rendered surface for right-click and modified-click.
        let surface = visual.debug_bounds("image-action-surface").unwrap();
        visual.simulate_mouse_down(surface.center(), MouseButton::Right, Modifiers::default());
        visual.simulate_mouse_up(surface.center(), MouseButton::Right, Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| assert!(reader.file_menu.is_some()));
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        let modifiers = if cfg!(target_os = "macos") {
            Modifiers {
                platform: true,
                ..Default::default()
            }
        } else {
            Modifiers {
                control: true,
                ..Default::default()
            }
        };
        let image = visual
            .debug_bounds("reader-block-image")
            .expect("positive control: a real image renders");
        visual.simulate_mouse_down(image.center(), MouseButton::Left, modifiers);
        visual.simulate_mouse_up(image.center(), MouseButton::Left, modifiers);
        visual.run_until_parked();
        assert!(
            visual.opened_url().is_some_and(|url| url.ends_with(".png")),
            "modified rendered click opens the image"
        );
        // Source dispatch also opens non-image attachments in the default app.
        reader.update_in(visual, |reader, window, cx| {
            assert!(reader.open_source_image_at(
                source,
                source.find("manual.pdf").unwrap(),
                window,
                cx
            ));
        });
        visual.run_until_parked();
        assert_eq!(
            visual.opened_url(),
            Some(
                url::Url::from_file_path(root.join("attachments/manual.pdf"))
                    .unwrap()
                    .into()
            )
        );
        reader.update(visual, |reader, cx| reader.open_source_find(cx));
        visual.run_until_parked();
        input.read_with(visual, |input, _| {
            assert!(
                input.lsp().hover_provider.is_some(),
                "Find exposes Source hover"
            )
        });
        reader.update_in(visual, |reader, window, cx| {
            reader.open_live_preview(window, cx)
        });
        visual.run_until_parked();
        input.read_with(visual, |input, _| {
            assert!(
                input.lsp().hover_provider.is_none(),
                "Live restores projection after Find"
            )
        });
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_live_preview(window, cx)
        });
        visual.run_until_parked();
        input.read_with(visual, |input, _| {
            assert!(
                input.lsp().hover_provider.is_some(),
                "Source hover returns after leaving Live"
            )
        });
        assert_eq!(
            std::fs::read_to_string(root.join("note.md")).unwrap(),
            source
        );
    }
}

/// Source hints belong to Reader's overlay layer. The editor's internal LSP
/// overlay is clipped by the note pane; the source range remains its anchor.
pub(crate) fn overlay(reader: &Reader, cx: &App) -> Option<AnyElement> {
    if reader.file_menu.is_some() {
        return None;
    }
    let input = reader.source_input()?;
    let input = input.read(cx);
    input.lsp().hover_provider.as_ref()?;
    let hover = input.hover_popover()?;
    let bounds = input.range_to_bounds(&hover.symbol_range)?;
    let lsp_types::HoverContents::Markup(content) = &hover.hover.contents else {
        return None;
    };
    let theme = cx.theme();
    Some(
        deferred(
            anchored()
                .position(point(bounds.left(), bounds.bottom() + px(4.)))
                .child(
                    v_flex()
                        .id("source-embed-hover")
                        .debug_selector(|| "source-embed-hover".into())
                        .gap_1()
                        .p_2()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .text_color(theme.popover_foreground)
                        .text_size(px(12.))
                        .max_w(px(500.))
                        .shadow_md()
                        .children(
                            content
                                .value
                                .split("\n\n")
                                .map(|line| div().child(line.to_owned())),
                        ),
                ),
        )
        .with_priority(gpui_base::POPUP_PRIORITY - 1)
        .into_any_element(),
    )
}
