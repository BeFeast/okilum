//! Structured references, visible-only thumbnails and original-image expansion.
use super::*;
use gpui_component::WindowExt as _;
use okilum_t3::stream::{AttachmentKind, ChatAttachment, NativeClient};
use serde_json::Value;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Weak,
};

const MAX_TRANSFERS: usize = 2;
static TRANSFERS: AtomicUsize = AtomicUsize::new(0);
struct Transfer;
impl Transfer {
    fn acquire() -> Option<Self> {
        TRANSFERS
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_TRANSFERS).then_some(count + 1)
            })
            .ok()
            .map(|_| Self)
    }
}
impl Drop for Transfer {
    fn drop(&mut self) {
        TRANSFERS.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct AssetKey {
    origin: String,
    reference: String,
}
impl AssetKey {
    fn new(origin: &str, attachment: &ChatAttachment) -> Self {
        Self {
            origin: origin.into(),
            reference: serde_json::to_string(attachment).expect("attachment serializes"),
        }
    }
}
#[cfg(test)]
type AttachmentFetch = dyn Fn(&ChatAttachment) -> anyhow::Result<Vec<u8>> + Send + Sync;

#[derive(Default)]
pub(super) struct History {
    pub(super) rows: HashMap<u64, Vec<Value>>,
    downloads: HashMap<AssetKey, Download>,
    lease: Arc<()>,
    retry_paint_pending: bool,
    #[cfg(test)]
    fetch: Option<Arc<AttachmentFetch>>,
}
enum Download {
    Loading {
        lease: Arc<()>,
        client: Weak<NativeClient>,
    },
    Ready {
        directory: Arc<tempfile::TempDir>,
        path: PathBuf,
        thumbnail: Option<Arc<gpui::Image>>,
        original: Option<Arc<gpui::Image>>,
    },
    Error,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Thumbnail,
    Expand,
    Open,
    Save,
}

struct AttachmentTarget {
    position: u64,
    attachment: ChatAttachment,
}

struct AttachmentOperation {
    action: Action,
    retry: bool,
}

fn filename(name: &str) -> String {
    let leaf = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = leaf
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    if clean.is_empty() || matches!(clean.as_str(), "." | "..") {
        "attachment".into()
    } else {
        clean
    }
}
fn parsed(raw: &Value) -> Option<ChatAttachment> {
    serde_json::from_value::<ChatAttachment>(raw.clone())
        .ok()
        .filter(|a| a.validate().is_ok())
}
impl History {
    pub(super) fn retain(&mut self, position: u64, item: &Value) -> bool {
        let rows = item["attachments"].as_array().cloned().unwrap_or_default();
        let prior = self.rows.get(&position);
        if (rows.is_empty() && prior.is_none()) || prior.is_some_and(|old| old == &rows) {
            return false;
        }
        let replaced = prior.is_some();
        let changed = if rows.is_empty() {
            self.rows.remove(&position).is_some()
        } else {
            self.rows.insert(position, rows);
            true
        };
        // A changed source/metadata with the same ID cannot reuse old bytes.
        // Initial insertions cannot invalidate a cache entry. Empty ordinary
        // rows and initial image pages therefore do not scan retained history.
        if replaced && !self.downloads.is_empty() {
            let retained: HashSet<_> = self
                .rows
                .values()
                .flatten()
                .filter_map(parsed)
                .map(|a| serde_json::to_string(&a).expect("attachment serializes"))
                .collect();
            self.downloads
                .retain(|key, _| retained.contains(&key.reference));
        }
        changed
    }
    fn contains(&self, position: u64, attachment: &ChatAttachment) -> bool {
        self.rows
            .get(&position)
            .into_iter()
            .flatten()
            .filter_map(parsed)
            .any(|current| &current == attachment)
    }

    fn accepts(&self, key: &AssetKey, lease: &Arc<()>) -> bool {
        matches!(self.downloads.get(key), Some(Download::Loading { lease: current, .. })
            if Arc::ptr_eq(current, lease))
    }
}
impl Feed {
    pub(super) fn render_attachments(&self, position: u64, cx: &mut App) -> gpui::Div {
        let origin = self.parent.upgrade().and_then(|parent| {
            parent
                .read(cx)
                .environment
                .as_ref()
                .map(Environment::origin)
        });
        let mut images = div().grid().grid_cols(2).gap_2().w_full().max_w(px(420.));
        let mut files = v_flex().gap_2().w_full();
        let mut image_count = 0;
        for (index, raw) in self
            .attachments
            .rows
            .get(&position)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let Some(attachment) = parsed(raw) else {
                files = files.child(div().child(format!(
                    "{} · unsupported attachment",
                    raw["name"].as_str().unwrap_or("File")
                )));
                continue;
            };
            let key = origin
                .as_deref()
                .map(|origin| AssetKey::new(origin, &attachment));
            let state = key
                .as_ref()
                .and_then(|key| self.attachments.downloads.get(key));
            let image = attachment.metadata.kind == AttachmentKind::Image;
            let mut row = v_flex().min_w_0().gap_1();
            if image {
                image_count += 1;
                let tile = match state {
                    Some(Download::Ready {
                        thumbnail: Some(image),
                        ..
                    }) => gpui::img(image.clone())
                        .w_full()
                        .h_full()
                        .object_fit(ObjectFit::Cover)
                        .into_any_element(),
                    _ => div()
                        .w_full()
                        .h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .px_2()
                        .text_sm()
                        .child(attachment.metadata.name.clone())
                        .into_any_element(),
                };
                let parent = self.parent.clone();
                let reference = attachment.clone();
                let expected_origin = origin.clone();
                let history_lease = self.attachments.lease.clone();
                let parent_visible = self.parent.clone();
                let visible_reference = attachment.clone();
                let visible_origin = origin.clone();
                let visible_lease = history_lease.clone();
                let clickable = matches!(
                    state,
                    Some(Download::Ready {
                        original: Some(_),
                        ..
                    })
                );
                row = row.child(
                    div()
                        .on_children_prepainted(move |bounds, window, cx| {
                            // Measurement alone must never perform a transfer.
                            let viewport = window.content_mask().bounds;
                            if !bounds.iter().any(|bounds| {
                                bounds.size.width > px(0.)
                                    && bounds.size.height > px(0.)
                                    && bounds.intersects(&viewport)
                            }) {
                                return;
                            }
                            let parent = parent_visible.clone();
                            let attachment = visible_reference.clone();
                            let origin = visible_origin.clone();
                            let lease = visible_lease.clone();
                            window.defer(cx, move |window, cx| {
                                request(
                                    &parent,
                                    AttachmentTarget {
                                        position,
                                        attachment,
                                    },
                                    origin,
                                    lease,
                                    AttachmentOperation {
                                        action: Action::Thumbnail,
                                        retry: false,
                                    },
                                    window,
                                    cx,
                                )
                            });
                        })
                        .id(SharedString::from(format!(
                            "history-image-{position}-{index}"
                        )))
                        .debug_selector(move || format!("history-image-{position}-{index}"))
                        .w_full()
                        .aspect_ratio(4. / 3.)
                        .min_w_0()
                        .rounded_md()
                        .border_1()
                        .overflow_hidden()
                        .when(clickable, |tile| tile.cursor_pointer())
                        .child(tile)
                        .on_click(move |_, window, cx| {
                            request(
                                &parent,
                                AttachmentTarget {
                                    position,
                                    attachment: reference.clone(),
                                },
                                expected_origin.clone(),
                                history_lease.clone(),
                                AttachmentOperation {
                                    action: Action::Expand,
                                    retry: false,
                                },
                                window,
                                cx,
                            )
                        }),
                );
            } else {
                row = row.child(div().whitespace_normal().child(format!(
                    "{} · {} bytes",
                    attachment.metadata.name, attachment.metadata.size_bytes
                )));
            }
            let mut controls = h_flex().flex_wrap().gap_1();
            let mut actions = vec![("Open", Action::Open), ("Save…", Action::Save)];
            if image && matches!(state, Some(Download::Error)) {
                actions.insert(0, ("Retry", Action::Thumbnail));
            }
            for (label, action) in actions {
                let parent = self.parent.clone();
                let attachment = attachment.clone();
                let origin = origin.clone();
                let lease = self.attachments.lease.clone();
                controls = controls.child(
                    Button::new(SharedString::from(format!(
                        "history-attachment-{position}-{index}-{label}"
                    )))
                    .debug_selector(move || {
                        format!("history-attachment-{position}-{index}-{label}")
                    })
                    .small()
                    .ghost()
                    .label(label)
                    .disabled(matches!(state, Some(Download::Loading { .. })))
                    .on_click(move |_, window, cx| {
                        request(
                            &parent,
                            AttachmentTarget {
                                position,
                                attachment: attachment.clone(),
                            },
                            origin.clone(),
                            lease.clone(),
                            AttachmentOperation {
                                action,
                                retry: label == "Retry",
                            },
                            window,
                            cx,
                        )
                    }),
                );
            }
            row = row.child(controls);
            if matches!(state, Some(Download::Loading { .. })) {
                row = row.child("Downloading…");
            }
            if matches!(state, Some(Download::Error)) {
                row = row.child(
                    div()
                        .text_sm()
                        .child("Download failed. Retry when connected."),
                );
            }
            if image
                && matches!(
                    state,
                    Some(Download::Ready {
                        thumbnail: None,
                        ..
                    })
                )
            {
                row = row.child("Image preview is unavailable. Open or save the original file.");
            }
            if image {
                images = images.child(row);
            } else {
                files = files.child(row);
            }
        }
        v_flex()
            .gap_2()
            .w_full()
            .when(image_count > 0, |tray| tray.child(images))
            .child(files)
    }

    fn download_attachment(
        &mut self,
        target: AttachmentTarget,
        origin: String,
        operation: AttachmentOperation,
        client: Arc<NativeClient>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let AttachmentTarget {
            position,
            attachment,
        } = target;
        let AttachmentOperation { action, retry } = operation;
        if !self.attachments.contains(position, &attachment) {
            return;
        }
        let key = AssetKey::new(&origin, &attachment);
        self.attachments.downloads.retain(|cached, state| cached.origin == origin
            && !matches!(state, Download::Loading { client: old, .. } if !old.upgrade().is_some_and(|old| Arc::ptr_eq(&old, &client))));
        if let Some(Download::Ready {
            directory,
            path,
            original,
            ..
        }) = self.attachments.downloads.get(&key)
        {
            let directory = directory.clone();
            let path = path.clone();
            let original = original.clone();
            if action == Action::Expand {
                if let Some(image) = original {
                    expand_image(image, attachment.metadata.name, window, cx);
                }
            } else {
                self.attachment_action(
                    directory,
                    path,
                    filename(&attachment.metadata.name),
                    action,
                    window,
                    cx,
                );
            }
            return;
        }
        if matches!(
            self.attachments.downloads.get(&key),
            Some(Download::Loading { .. })
        ) {
            return;
        }
        if action == Action::Thumbnail
            && !retry
            && matches!(self.attachments.downloads.get(&key), Some(Download::Error))
        {
            return;
        }
        let Some(transfer) = Transfer::acquire() else {
            if action == Action::Thumbnail && !retry {
                // Retry capacity via a fresh visible paint, never via a saved
                // offscreen target. One wakeup per Feed also avoids timer storms.
                if !self.attachments.retry_paint_pending {
                    self.attachments.retry_paint_pending = true;
                    let lease = self.attachments.lease.clone();
                    cx.spawn_in(window, async move |this, cx| {
                        cx.background_executor()
                            .timer(Duration::from_millis(100))
                            .await;
                        let _ = this.update(cx, |this, cx| {
                            if Arc::ptr_eq(&this.attachments.lease, &lease) {
                                this.attachments.retry_paint_pending = false;
                                cx.notify();
                            }
                        });
                    })
                    .detach();
                }
                return;
            }
            // Explicit actions retain their target while a bounded transfer is busy.
            let parent = self.parent.clone();
            let lease = self.attachments.lease.clone();
            cx.spawn_in(window, async move |_, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let _ = cx.update(|window, cx| {
                    request(
                        &parent,
                        AttachmentTarget {
                            position,
                            attachment,
                        },
                        Some(origin),
                        lease,
                        AttachmentOperation { action, retry },
                        window,
                        cx,
                    )
                });
            })
            .detach();
            return;
        };
        let lease = Arc::new(());
        self.attachments.downloads.insert(
            key.clone(),
            Download::Loading {
                lease: lease.clone(),
                client: Arc::downgrade(&client),
            },
        );
        self.attachments_changed(cx);
        #[cfg(test)]
        let fetch = self.attachments.fetch.clone();
        cx.spawn_in(window, async move |this, cx| {
            let worker = client.clone();
            let asset = attachment.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let _transfer = transfer;
                    #[cfg(test)]
                    let bytes = if let Some(fetch) = fetch {
                        fetch(&asset)?
                    } else {
                        worker.download_attachment(&asset)?
                    };
                    #[cfg(not(test))]
                    let bytes = worker.download_attachment(&asset)?;
                    let thumbnail = if asset.metadata.kind == AttachmentKind::Image {
                        super::super::attachments::thumbnail(&bytes)
                    } else {
                        None
                    };
                    // Full preview retains ORIGINAL encoded bytes, never the 640px thumbnail.
                    let original = thumbnail.as_ref().and_then(|_| original_image(&bytes));
                    let directory = Arc::new(tempfile::tempdir()?);
                    let path = directory.path().join(filename(&asset.metadata.name));
                    std::fs::write(&path, bytes)?;
                    Ok::<_, anyhow::Error>((directory, path, thumbnail, original))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.attachments.accepts(&key, &lease) {
                    // Releasing a slot may unblock newly visible sources after reset.
                    cx.notify();
                    return;
                }
                if !current_access(&this.parent, &origin, &client, cx)
                    || !this.attachments.contains(position, &attachment)
                {
                    this.attachments.downloads.remove(&key);
                    cx.notify();
                    return;
                }
                match result {
                    Ok((directory, path, thumbnail, original)) => {
                        let expanded = original.clone();
                        this.attachments.downloads.insert(
                            key,
                            Download::Ready {
                                directory: directory.clone(),
                                path: path.clone(),
                                thumbnail,
                                original,
                            },
                        );
                        if action == Action::Expand {
                            if let Some(image) = expanded {
                                expand_image(image, attachment.metadata.name, window, cx);
                            }
                        } else {
                            this.attachment_action(
                                directory,
                                path,
                                filename(&attachment.metadata.name),
                                action,
                                window,
                                cx,
                            );
                        }
                    }
                    Err(_) => {
                        this.attachments.downloads.insert(key, Download::Error);
                    }
                }
                this.attachments_changed(cx);
            });
        })
        .detach();
    }

    fn attachment_action(
        &mut self,
        directory: Arc<tempfile::TempDir>,
        path: PathBuf,
        name: String,
        action: Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            Action::Thumbnail | Action::Expand => {}
            Action::Open => {
                if let Ok(url) = url::Url::from_file_path(&path) {
                    cx.open_url(url.as_str());
                }
            }
            Action::Save => {
                let picker = cx.prompt_for_new_path(&std::env::temp_dir(), Some(&name));
                cx.spawn_in(window, async move |this, cx| {
                    let Ok(Ok(Some(destination))) = picker.await else { return; };
                    let result = cx.background_executor().spawn(async move {
                        let _keep = directory;
                        let parent = destination.parent().context("Missing download directory")?;
                        let mut file = tempfile::NamedTempFile::new_in(parent)?;
                        std::io::copy(&mut std::fs::File::open(path)?, &mut file)?;
                        file.as_file().sync_all()?; file.persist(destination)?;
                        Ok::<_, anyhow::Error>(())
                    }).await;
                    let _ = this.update(cx, |this, cx| {
                        if result.is_err() { this.parent.update(cx, |view, cx| {
                            view.error = Some("Could not save the attachment. Choose another destination.".into()); cx.notify(); }).ok(); }
                    });
                }).detach();
            }
        }
    }
    fn attachments_changed(&mut self, cx: &mut Context<Self>) {
        let indices: Vec<_> = self
            .attachments
            .rows
            .keys()
            .filter_map(|p| self.indices.get(p).copied())
            .collect();
        self.scroller.update(cx, |state, cx| {
            for index in indices {
                state.remeasure_items(index..index + 1, cx);
            }
        });
        cx.notify();
    }
}

fn request(
    parent: &WeakEntity<ProjectView>,
    target: AttachmentTarget,
    origin: Option<String>,
    lease: Arc<()>,
    operation: AttachmentOperation,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(origin) = origin else {
        return;
    };
    let _ = parent.update(cx, |view, cx| {
        if !view.active
            || !reader_features::enabled(reader_features::Feature::Agents, cx)
            || view
                .environment
                .as_ref()
                .is_none_or(|environment| environment.origin() != origin)
        {
            return;
        }
        let Some(client) = view
            .native
            .model_client()
            .filter(|c| c.access_allowed() && c.origin() == origin)
        else {
            return;
        };
        view.feed.update(cx, |feed, cx| {
            if !Arc::ptr_eq(&feed.attachments.lease, &lease) {
                return;
            }
            feed.download_attachment(target, origin, operation, client, window, cx);
        });
    });
}
fn current_access(
    parent: &WeakEntity<ProjectView>,
    origin: &str,
    client: &Arc<NativeClient>,
    cx: &App,
) -> bool {
    client.access_allowed()
        && client.origin() == origin
        && reader_features::enabled(reader_features::Feature::Agents, cx)
        && parent.upgrade().is_some_and(|parent| {
            let parent = parent.read(cx);
            parent.active
                && parent
                    .environment
                    .as_ref()
                    .is_some_and(|env| env.origin() == origin)
                && parent
                    .native
                    .model_client()
                    .is_some_and(|current| Arc::ptr_eq(&current, client))
        })
}
fn original_image(bytes: &[u8]) -> Option<Arc<gpui::Image>> {
    let format = match image::guess_format(bytes).ok()? {
        image::ImageFormat::Png => gpui::ImageFormat::Png,
        image::ImageFormat::Jpeg => gpui::ImageFormat::Jpeg,
        image::ImageFormat::Gif => gpui::ImageFormat::Gif,
        image::ImageFormat::WebP => gpui::ImageFormat::Webp,
        image::ImageFormat::Bmp => gpui::ImageFormat::Bmp,
        image::ImageFormat::Tiff => gpui::ImageFormat::Tiff,
        image::ImageFormat::Ico => gpui::ImageFormat::Ico,
        image::ImageFormat::Pnm => gpui::ImageFormat::Pnm,
        _ => return None,
    };
    Some(Arc::new(gpui::Image::from_bytes(format, bytes.to_vec())))
}
fn expand_image(image: Arc<gpui::Image>, name: String, window: &mut Window, cx: &mut App) {
    // Root restores focus on close; no timeline splice, scroll or anchor change.
    window.open_dialog(cx, move |dialog, window, _| {
        let width = (f32::from(window.viewport_size().width) - 64.).max(1.);
        let height = (f32::from(window.viewport_size().height) - 160.).max(1.);
        // The dialog supplies 16 px content padding on each side. This ratio
        // describes the finite viewport box, never the original image ratio.
        let box_ratio = (width - 32.).max(1.) / height;
        dialog
            .title(div().w_full().truncate().pr_4().child(name.clone()))
            .width(px(width))
            .margin_top(px(32.))
            .overlay_closable(true)
            .child(
                div()
                    .debug_selector(|| "history-image-expanded".into())
                    .w_full()
                    .h(px(height))
                    .min_w_0()
                    .min_h_0()
                    .flex_shrink_0()
                    .overflow_hidden()
                    .child(
                        gpui::img(image.clone())
                            .debug_selector(|| "history-image-expanded-original".into())
                            .w_full()
                            .h(px(height))
                            .min_w_0()
                            .min_h_0()
                            .flex_shrink_0()
                            // GPUI 0.3.3 otherwise injects the intrinsic aspect
                            // ratio, which can grow a percentage-height image
                            // past its container before Contain is applied.
                            .aspect_ratio(box_ratio)
                            .object_fit(ObjectFit::Contain),
                    ),
            )
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    /// Root owns dialog state, while window content draws its overlay layer.
    /// Match the production Reader host rather than opening an invisible dialog
    /// into a bare ProjectView, which has no overlay layer of its own.
    struct DialogHost {
        manager: Entity<ProjectView>,
    }
    impl Render for DialogHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .relative()
                .size_full()
                .child(self.manager.clone())
                .children(Root::render_dialog_layer(window, cx))
        }
    }

    fn draw_history(visual: &mut VisualTestContext) {
        // The pinned GPUI reuses cached paint without replaying debug bounds.
        // Refresh the whole window so cached ancestor views also re-register
        // selectors, without changing the list offset or masking anchor shifts.
        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
    }

    fn png() -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1200, 900)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    fn image_reference(id: &str, bytes: usize) -> Value {
        serde_json::json!({"id":id,"type":"image","name":format!("{id}.png"),
            "mimeType":"image/png","sizeBytes":bytes})
    }

    fn client(environment: &Environment) -> Arc<NativeClient> {
        let token = okilum_t3::stream::NativeToken::from_secret(
            environment,
            &serde_json::to_vec(&serde_json::json!({
                "origin":environment.origin(),"access_token":"fixture",
                "expires_at":chrono::Utc::now().timestamp() as u64+3600,
                "scope":"orchestration:read"
            }))
            .unwrap(),
        )
        .unwrap();
        Arc::new(NativeClient::new(environment.clone(), token, Duration::from_millis(10)).unwrap())
    }

    #[test]
    fn original_bytes_and_asset_scope_survive_thumbnail_creation_but_not_source_replacement() {
        let bytes = png();
        let thumbnail = super::super::super::attachments::thumbnail(&bytes).unwrap();
        let original = original_image(&bytes).unwrap();
        assert_eq!(
            original.id(),
            gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes.clone()).id()
        );
        assert_ne!(
            original.id(),
            thumbnail.id(),
            "expanded image must not be the bounded thumbnail"
        );
        let raw = image_reference("same-id", bytes.len());
        let asset = parsed(&raw).unwrap();
        let key = AssetKey::new("https://one.example", &asset);
        assert_ne!(key, AssetKey::new("https://two.example", &asset));
        let mut history = History::default();
        history.retain(7, &serde_json::json!({"attachments":[raw.clone()]}));
        let lease = Arc::new(());
        history.downloads.insert(
            key.clone(),
            Download::Loading {
                lease: lease.clone(),
                client: Weak::new(),
            },
        );
        assert!(history.accepts(&key, &lease));
        assert!(!history.retain(7, &serde_json::json!({"attachments":[raw.clone()]})));
        assert!(
            history.accepts(&key, &lease),
            "identical reconnect reference keeps its in-flight identity"
        );
        let mut replacement = raw;
        replacement["source"] = serde_json::json!({"kind":"new-source"});
        history.retain(7, &serde_json::json!({"attachments":[replacement]}));
        assert!(
            !history.accepts(&key, &lease),
            "late old source cannot replace the new reference"
        );
        history = History::default();
        assert!(
            !history.accepts(&key, &lease),
            "reset denies the old completion"
        );
    }

    #[gpui::test]
    fn painted_images_fetch_without_preview_offscreen_measure_does_not_and_expansion_keeps_anchor(
        cx: &mut TestAppContext,
    ) {
        use std::sync::Mutex;
        cx.update(|cx| {
            crate::test_init(cx);
            cx.set_reduce_motion(true);
        });
        let state = tempfile::tempdir().unwrap();
        let environment = Environment::new("fixture", "http://127.0.0.1:1").unwrap();
        let bytes = png();
        let observed = Arc::new(Mutex::new(Vec::<String>::new()));
        let fetched = observed.clone();
        let image_bytes = bytes.clone();
        let (root, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = super::super::super::fixture::view(state.path().to_owned(),
                    Arc::new(super::super::super::fixture::Fake::default()), window, cx);
                view.active = false;
                view.environment = Some(environment.clone());
                view.native.set_detail_test_client(client(&environment));
                view.threads.enabled = true;
                let rows: Vec<TimelineItem> = (0..100).map(|position| {
                    serde_json::from_value(serde_json::json!({
                        "position":position,"itemId":format!("row-{position}"),"type":"user_message",
                        "status":"completed","text":format!("History {position}"),"updatedAt":"now"
                    })).unwrap()
                }).collect();
                view.feed.update(cx, |feed, cx| {
                    feed.attachments.fetch = Some(Arc::new(move |attachment| {
                        fetched.lock().unwrap().push(attachment.id.clone());
                        Ok(image_bytes.clone())
                    }));
                    feed.attachments.retain(0, &serde_json::json!({"attachments":[image_reference("old",bytes.len())]}));
                    feed.attachments.retain(99, &serde_json::json!({"attachments":[
                        image_reference("last-a",bytes.len()),image_reference("last-b",bytes.len())]}));
                    feed.apply(rows.iter(), Some(&[]), cx);
                });
                // No subscription or fixture HTTP server is started. The injected
                // byte reader replaces only transfer I/O; real prepaint schedules it.
                view.active = true;
                view
            });
            let host = cx.new(|_| DialogHost { manager: view });
            Root::new(host, window, cx)
        });
        // A wide viewport reproduces the native crop: the 4:3 original would
        // become taller than the dialog box if its intrinsic ratio drove layout.
        visual.simulate_resize(size(px(1280.), px(720.)));
        visual.run_until_parked();
        let view = root.read_with(visual, |root, cx| {
            root.view()
                .clone()
                .downcast::<DialogHost>()
                .unwrap()
                .read(cx)
                .manager
                .clone()
        });
        let mut downloaded = observed.lock().unwrap().clone();
        downloaded.sort();
        assert_eq!(
            downloaded,
            ["last-a", "last-b"],
            "only actually visible assets may transfer"
        );
        let first = visual
            .debug_bounds("history-image-99-0")
            .expect("first image shown without Preview");
        let second = visual
            .debug_bounds("history-image-99-1")
            .expect("second image shown without Preview");
        assert_eq!(
            first.top(),
            second.top(),
            "images share a two-column grid row"
        );
        assert!(second.right() - first.left() <= px(420.));
        assert!(visual
            .debug_bounds("history-attachment-99-0-Preview")
            .is_none());
        view.update(visual, |view, cx| {
            view.native.set_detail_test_client(client(&environment));
            view.feed.update(cx, |_, cx| cx.notify());
        });
        visual.run_until_parked();
        assert_eq!(
            observed.lock().unwrap().len(),
            2,
            "same-origin reconnect reuses confirmed bytes"
        );
        let feed = view.read_with(visual, |view, _| view.feed.clone());
        feed.update(visual, |feed, cx| {
            feed.scroller.update(cx, |scroller, cx| {
                scroller.scroll_to_item(0, cx);
            });
        });
        visual.run_until_parked();
        assert_eq!(
            observed
                .lock()
                .unwrap()
                .iter()
                .filter(|id| *id == "old")
                .count(),
            1,
            "positive control: scrolling the old image into view starts its download"
        );
        feed.read_with(visual, |feed, cx| {
            let asset = parsed(&feed.attachments.rows[&0][0]).unwrap();
            assert!(feed.attachments.contains(0, &asset));
            let key = AssetKey::new(&environment.origin(), &asset);
            let Some(Download::Ready {
                path,
                thumbnail: Some(_),
                original: Some(original),
                ..
            }) = feed.attachments.downloads.get(&key) else {
                panic!("positive control: original image and thumbnail are fully downloaded before clicking");
            };
            assert!(path.is_file());
            assert_eq!(original.id(), gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes.clone()).id());
            let parent = feed.parent.upgrade().unwrap();
            let client = parent.read(cx).native.model_client().unwrap();
            assert!(current_access(&feed.parent, &environment.origin(), &client, cx));
        });
        // Refresh hitboxes after download-driven remeasurement before choosing
        // the click point. A started fetch is not a completed/painted thumbnail.
        draw_history(visual);
        let before = visual.debug_bounds("history-image-0-0").unwrap();
        let viewport = visual.debug_bounds("project-history").unwrap();
        assert!(before.center().y >= viewport.top() && before.center().y < viewport.bottom());
        assert!(before.center().x >= viewport.left() && before.center().x < viewport.right());
        assert!(!feed.read_with(visual, |feed, cx| {
            feed.scroller.read(cx).is_following_tail()
        }));
        visual.simulate_click(before.center(), Modifiers::default());
        visual.run_until_parked();
        draw_history(visual);
        assert!(
            visual.debug_bounds("dialog-layer").is_some(),
            "positive control: the real click opened the host's visible dialog layer"
        );
        let expanded_box = visual.debug_bounds("history-image-expanded").unwrap();
        let expanded_original = visual
            .debug_bounds("history-image-expanded-original")
            .unwrap();
        let window_size = visual.update(|window, _| window.viewport_size());
        assert!(
            expanded_box.size.width / expanded_box.size.height > 4. / 3.,
            "positive control: intrinsic 4:3 sizing would exceed this wide containment box"
        );
        assert_eq!(expanded_original, expanded_box,
            "the original image's layout box must match its finite container before Contain fits the pixels");
        assert_eq!(
            expanded_original.size.height,
            window_size.height - px(160.),
            "intrinsic image ratio must not inflate the explicit viewport box height"
        );
        assert!(expanded_original.left() >= px(0.) && expanded_original.right() <= window_size.width,
            "the full original image box stays inside the horizontal viewport: {expanded_original:?}");
        assert!(
            expanded_original.top() >= px(0.) && expanded_original.bottom() <= window_size.height,
            "the full original image box stays inside the vertical viewport: {expanded_original:?}"
        );
        visual.update(|window, cx| window.close_dialog(cx));
        visual.run_until_parked();
        draw_history(visual);
        assert!(visual.debug_bounds("history-image-expanded").is_none());
        assert_eq!(
            visual.debug_bounds("history-image-0-0").unwrap(),
            before,
            "closing expansion preserves the exact visible anchor"
        );
        assert!(!feed.read_with(visual, |feed, cx| {
            feed.scroller.read(cx).is_following_tail()
        }));
        assert_eq!(
            observed.lock().unwrap().len(),
            3,
            "expand/close never downloads again"
        );
        feed.update(visual, |feed, cx| {
            let mut replacement = image_reference("old", bytes.len());
            replacement["source"] = serde_json::json!({"revision":"new-source"});
            feed.attachments
                .retain(0, &serde_json::json!({"attachments":[replacement]}));
            feed.attachments_changed(cx);
        });
        visual.run_until_parked();
        assert_eq!(
            observed.lock().unwrap().len(),
            4,
            "a changed source is downloaded once"
        );
        feed.update(visual, |_, cx| cx.notify());
        visual.run_until_parked();
        assert_eq!(
            observed.lock().unwrap().len(),
            4,
            "ordinary paints reuse the changed source"
        );
        let failures = observed.clone();
        feed.update(visual, |feed, cx| {
            feed.attachments.fetch = Some(Arc::new(move |asset| {
                failures.lock().unwrap().push(asset.id.clone());
                anyhow::bail!("fixture transfer failure")
            }));
            feed.attachments.retain(
                0,
                &serde_json::json!({
                    "attachments":[image_reference("failed", bytes.len())]
                }),
            );
            feed.attachments_changed(cx);
        });
        visual.run_until_parked();
        assert_eq!(observed.lock().unwrap().len(), 5);
        feed.update(visual, |_, cx| cx.notify());
        visual.run_until_parked();
        assert_eq!(
            observed.lock().unwrap().len(),
            5,
            "failed transfers do not retry during paint"
        );
        let retry = visual.debug_bounds("history-attachment-0-0-Retry").unwrap();
        visual.simulate_click(retry.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            observed.lock().unwrap().len(),
            6,
            "only an explicit Retry attempts transfer again"
        );
        let active = view.read_with(visual, |view, cx| {
            let client = view.native.model_client().unwrap();
            assert!(current_access(
                &view.feed.read(cx).parent,
                &environment.origin(),
                &client,
                cx
            ));
            client
        });
        view.update(visual, |view, _| {
            view.environment = Some(Environment::new("other", "http://127.0.0.1:2").unwrap());
        });
        feed.read_with(visual, |feed, cx| {
            assert!(
                !current_access(&feed.parent, &environment.origin(), &active, cx),
                "a late reply from the old origin cannot enter the new view"
            );
        });
        visual.update(|_, cx| {
            reader_features::set_enabled(reader_features::Feature::Agents, false, cx);
        });
        feed.read_with(visual, |feed, cx| {
            assert!(
                !current_access(&feed.parent, &environment.origin(), &active, cx),
                "feature-off revokes attachment visibility and transfer authority"
            );
        });
    }
    #[test]
    fn attachment_only_history_and_unknown_types_remain_visible_without_path_escape() {
        let mut history = History::default();
        let item = serde_json::json!({"type":"user_message","text":"","attachments":[{"id":"asset-1","type":"file","name":"../report.txt","mimeType":"text/plain","sizeBytes":4},{"type":"future","name":"future.bin"}]});
        assert!(history.retain(1, &item));
        assert!(!history.retain(1, &item));
        assert_eq!(history.rows[&1].len(), 2);
        let asset: ChatAttachment = serde_json::from_value(history.rows[&1][0].clone()).unwrap();
        assert!(asset.validate().is_ok());
        assert_eq!(filename(&asset.metadata.name), "report.txt");
        assert_eq!(filename("C:\\private\\report.txt"), "report.txt");
        assert_eq!(filename(".."), "attachment");
        assert!(history.retain(1, &serde_json::json!({"attachments":[]})));
        assert!(!history.rows.contains_key(&1));
    }

    #[gpui::test]
    fn save_download_uses_exact_bytes_and_cancel_keeps_existing_files(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let temp = tempfile::tempdir().unwrap();
        let downloaded = Arc::new(tempfile::tempdir().unwrap());
        let source = downloaded.path().join("report.bin");
        let bytes = b"Downloaded binary\0\xff\n";
        std::fs::write(&source, bytes).unwrap();
        let destination = temp.path().join("chosen.bin");
        std::fs::write(&destination, b"Original bytes").unwrap();
        let (root, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = super::super::super::fixture::view(
                    temp.path().to_owned(),
                    Arc::new(super::super::super::fixture::Fake::default()),
                    window,
                    cx,
                );
                view.active = false;
                view
            });
            Root::new(view, window, cx)
        });
        let view = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<ProjectView>().unwrap()
        });
        for save in [false, true] {
            view.update_in(visual, |view, window, cx| {
                view.feed.update(cx, |feed, cx| {
                    feed.attachment_action(
                        downloaded.clone(),
                        source.clone(),
                        "report.bin".into(),
                        Action::Save,
                        window,
                        cx,
                    )
                });
            });
            assert!(visual.did_prompt_for_new_path());
            let chosen = destination.clone();
            visual.simulate_new_path_selection(move |_| save.then_some(chosen));
            visual.run_until_parked();
            assert_eq!(
                std::fs::read(&destination).unwrap().as_slice(),
                if save {
                    bytes.as_slice()
                } else {
                    b"Original bytes".as_slice()
                }
            );
            assert_eq!(
                std::fs::read(&source).unwrap(),
                bytes,
                "the retained download is reusable after Save"
            );
        }
    }
}
