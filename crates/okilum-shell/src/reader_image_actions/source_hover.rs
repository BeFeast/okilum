//! Source uses the native editor hover lifecycle and the rendered image's
//! bounded, revision-checked metadata reader. No filesystem work runs on the UI.
use super::*;
use gpui_component::input::{HoverProvider, Rope, RopeExt};

pub(super) fn provider(reader: WeakEntity<Reader>) -> Rc<dyn HoverProvider> {
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
        if text.len_bytes() > okilum_core::source_classifier::MAX_BYTES {
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
            // A tooltip from an edited/replaced note cannot land on new text.
            let current = weak
                .read_with(cx, |reader, cx| {
                    reader.navigation.preparation_generation == generation
                        && reader.editing.as_ref().is_some_and(|editing| {
                            editing.input.read(cx).source_snapshot().text.rope() == &source
                        })
                })
                .unwrap_or(false);
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
