//! Native chat attachment drafts. Local sources stay runtime-only; only
//! confirmed server references enter the durable message journal.
use super::*;
use gpui_component::WindowExt as _;
use okilum_t3::{
    chat_drafts::{Attachment, AttachmentKind as DraftKind, AttachmentState},
    stream::{
        AttachmentKind, AttachmentMetadata, ChatAttachment, NativeClient, MAX_ATTACHMENTS,
        MAX_FILE_BYTES, MAX_IMAGE_BYTES, MAX_TOTAL_IMAGE_BYTES,
    },
};
use std::{collections::HashMap, io::Read};

mod restored;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Scope {
    origin: String,
    project: String,
    thread: String,
    generation: u64,
}

#[derive(Clone)]
pub(in crate::project) enum Source {
    File(PathBuf),
    Image(Arc<Vec<u8>>),
}

#[derive(Clone)]
struct Local {
    scope: Scope,
    revision: uuid::Uuid,
    source: Source,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PreparationLease {
    scope: Scope,
    id: String,
    revision: uuid::Uuid,
    job: uuid::Uuid,
}

struct CachedPreparation {
    scope: Scope,
    revision: uuid::Uuid,
    prepared: Arc<Prepared>,
}

#[derive(Default)]
pub(super) struct Runtime {
    sources: HashMap<String, Local>,
    inflight: Option<(Scope, String)>,
    upload_job: Option<uuid::Uuid>,
    preparing: Option<PreparationLease>,
    prepared: HashMap<String, CachedPreparation>,
    identity: Option<(std::sync::Weak<NativeClient>, ServerIdentity)>,
    identity_pending: bool,
    identity_failed: bool,
    notice: Option<String>,
    previews: HashMap<String, Arc<gpui::Image>>,
    expanded_previews: HashMap<String, Arc<gpui::Image>>,
    restored: restored::Previews,
}

impl Runtime {
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }
    pub(super) fn connection_changed(&mut self) {
        self.inflight = None;
        self.upload_job = None;
        // Local preparation is source-scoped, not credential/client-scoped.
        // It keeps running while upload access is renewed or disconnected.
        self.identity = None;
        self.identity_pending = false;
        self.identity_failed = false;
        self.restored = restored::Previews::default();
    }
}

fn metadata(name: String) -> AttachmentMetadata {
    let mime = match Path::new(&name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "avif" => "image/avif",
        "heic" | "heif" => "image/heic",
        _ => "application/octet-stream",
    };
    AttachmentMetadata {
        kind: if matches!(
            mime,
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        ) {
            AttachmentKind::Image
        } else {
            AttachmentKind::File
        },
        name,
        mime_type: mime.into(),
        size_bytes: 0,
    }
}

fn draft_kind(kind: AttachmentKind) -> DraftKind {
    match kind {
        AttachmentKind::Image => DraftKind::Image,
        AttachmentKind::File => DraftKind::File,
    }
}

fn supported_image_mime(bytes: &[u8]) -> anyhow::Result<&'static str> {
    match image::guess_format(bytes)
        .context("Unsupported image type. Attach a PNG, JPEG, GIF or WebP image.")?
    {
        image::ImageFormat::Png => Ok("image/png"),
        image::ImageFormat::Jpeg => Ok("image/jpeg"),
        image::ImageFormat::Gif => Ok("image/gif"),
        image::ImageFormat::WebP => Ok("image/webp"),
        _ => anyhow::bail!("Unsupported image type. Attach a PNG, JPEG, GIF or WebP image."),
    }
}

fn decode_image(bytes: &[u8]) -> Option<image::DynamicImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    use image::ImageDecoder as _;
    let mut decoder = reader.into_decoder().ok()?;
    let orientation = decoder.orientation().ok()?;
    let mut decoded = image::DynamicImage::from_decoder(decoder).ok()?;
    decoded.apply_orientation(orientation);
    Some(decoded)
}

pub(super) fn thumbnail(bytes: &[u8]) -> Option<Arc<gpui::Image>> {
    let image = decode_image(bytes)?.thumbnail(640, 480);
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).ok()?;
    Some(Arc::new(gpui::Image::from_bytes(
        gpui::ImageFormat::Png,
        png.into_inner(),
    )))
}

pub(in crate::project) struct Prepared {
    pub(in crate::project) metadata: AttachmentMetadata,
    pub(in crate::project) bytes: Vec<u8>,
    pub(in crate::project) previews: Option<(Arc<gpui::Image>, Arc<gpui::Image>)>,
}

/// Validate with the bounded decoder before retaining the original encoded
/// image for the viewer. GIF stays GIF so its animation is not flattened.
fn image_previews(bytes: &[u8]) -> Option<(Arc<gpui::Image>, Arc<gpui::Image>)> {
    let thumb = thumbnail(bytes)?;
    let format = match image::guess_format(bytes).ok()? {
        image::ImageFormat::Png => gpui::ImageFormat::Png,
        image::ImageFormat::Jpeg => gpui::ImageFormat::Jpeg,
        image::ImageFormat::Gif => gpui::ImageFormat::Gif,
        image::ImageFormat::WebP => gpui::ImageFormat::Webp,
        _ => return None,
    };
    Some((
        thumb,
        Arc::new(gpui::Image::from_bytes(format, bytes.to_vec())),
    ))
}

pub(in crate::project) fn prepare_with_preview(
    source: &Source,
    name: &str,
    other_image_bytes: u64,
) -> anyhow::Result<Arc<Prepared>> {
    let (metadata, bytes) = prepare(source, name, other_image_bytes)?;
    let previews = if metadata.kind == AttachmentKind::Image {
        Some(
            image_previews(&bytes)
                .context("This image could not be read for preview; select it again")?,
        )
    } else {
        None
    };
    Ok(Arc::new(Prepared {
        metadata,
        bytes,
        previews,
    }))
}

/// AppKit screenshots and Preview commonly offer TIFF. T3 accepts PNG,
/// JPEG, GIF and WebP; convert TIFF on the preparation worker, with the same
/// decoder limits and orientation handling as previews. Preserve other bytes.
fn prepare_image(mut bytes: Vec<u8>, name: &str) -> anyhow::Result<(AttachmentMetadata, Vec<u8>)> {
    let mut name = name.to_owned();
    if image::guess_format(&bytes).ok() == Some(image::ImageFormat::Tiff) {
        anyhow::ensure!(
            bytes.len() <= crate::note_paste::offer::MAX_IMAGE_BYTES,
            "Clipboard image is too large"
        );
        let decoded = decode_image(&bytes).context("This TIFF image could not be read safely")?;
        let mut png = std::io::Cursor::new(Vec::new());
        decoded.write_to(&mut png, image::ImageFormat::Png)?;
        bytes = png.into_inner();
        name = Path::new(&name)
            .with_extension("png")
            .to_string_lossy()
            .into_owned();
    }
    Ok((
        AttachmentMetadata {
            kind: AttachmentKind::Image,
            name,
            mime_type: supported_image_mime(&bytes)?.into(),
            size_bytes: 0,
        },
        bytes,
    ))
}

fn prepare(
    source: &Source,
    name: &str,
    other_image_bytes: u64,
) -> anyhow::Result<(AttachmentMetadata, Vec<u8>)> {
    let (mut meta, bytes) = match source {
        Source::File(path) => {
            let file = std::fs::File::open(path)
                .context("Cannot read this attachment; select it again")?;
            let size = file.metadata()?.len();
            anyhow::ensure!(
                size > 0 && size <= MAX_FILE_BYTES,
                "File is empty or exceeds T3's 50 MiB file limit"
            );
            let mut bytes = Vec::new();
            file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
            anyhow::ensure!(
                bytes.len() as u64 == size,
                "File changed while it was being attached; select it again"
            );
            let meta = metadata(name.to_owned());
            if image::guess_format(&bytes).is_ok() || meta.mime_type.starts_with("image/") {
                prepare_image(bytes, name)?
            } else {
                (meta, bytes)
            }
        }
        Source::Image(bytes) => prepare_image(bytes.as_ref().clone(), name)?,
    };
    meta.size_bytes = bytes.len() as u64;
    if meta.kind == AttachmentKind::Image {
        anyhow::ensure!(
            meta.size_bytes <= MAX_IMAGE_BYTES,
            "Image exceeds T3's 10 MiB image limit"
        );
        anyhow::ensure!(
            other_image_bytes.saturating_add(meta.size_bytes) <= MAX_TOTAL_IMAGE_BYTES,
            "Images exceed T3's 80 MiB total image limit; remove a file before retrying"
        );
    }
    meta.validate()?;
    Ok((meta, bytes))
}

/// Validate an explicit draft collection against its confirmed server identity.
/// Shared by message and question drafts; never reads or swaps the composer.
pub(in crate::project) fn uploaded_references(
    rows: &[Attachment],
    environment: &str,
) -> anyhow::Result<Vec<ChatAttachment>> {
    anyhow::ensure!(
        !environment.trim().is_empty(),
        "Confirm the T3 connection before sending attachments"
    );
    let references = rows.iter().map(|row| {
        let AttachmentState::Uploaded { attachment_id, environment_id } = &row.state else {
            anyhow::bail!("Wait for attachments to finish, or retry, reselect or remove the unavailable file");
        };
        anyhow::ensure!(environment_id == environment, "This file belongs to another T3 environment; select it again");
        Ok(ChatAttachment {
            id: attachment_id.clone(),
            metadata: AttachmentMetadata {
                kind: match row.kind { DraftKind::Image => AttachmentKind::Image, DraftKind::File => AttachmentKind::File },
                name: row.name.clone(), mime_type: row.mime_type.clone(), size_bytes: row.size_bytes,
            },
            source: row.source.clone(),
        })
    }).collect::<anyhow::Result<Vec<_>>>()?;
    okilum_t3::stream::validate_attachments(&references)?;
    Ok(references)
}

impl ProjectView {
    fn set_attachment_draft(
        &mut self,
        rows: Vec<Attachment>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.draft_set_attachments(rows, window, cx);
        // Attachment changes repaint even when the writer has no error update
        // (or no durable writer is available yet).
        cx.notify();
    }

    pub(super) fn attachment_can_add(&self) -> bool {
        self.native.active()
            && self.draft_ready()
            && !self.busy
            && self.attachment_scope().is_some()
    }
    fn attachment_scope(&self) -> Option<Scope> {
        Some(Scope {
            origin: self.environment.as_ref()?.origin(),
            project: self.binding.as_ref()?.manager.project_id.clone()?,
            thread: self.chat.as_ref()?.thread_id.clone(),
            generation: self.generation,
        })
    }

    pub(in crate::project) fn attachment_identity(&self) -> Option<&ServerIdentity> {
        let client = self.native.model_client()?;
        self.attachments
            .identity
            .as_ref()
            .filter(|(known, _)| {
                known
                    .upgrade()
                    .is_some_and(|known| Arc::ptr_eq(&known, &client))
            })
            .map(|(_, identity)| identity)
    }

    pub(super) fn prepared_attachments(&self) -> anyhow::Result<Vec<ChatAttachment>> {
        if self.draft_attachments().is_empty() {
            return Ok(Vec::new());
        }
        let environment = &self
            .attachment_identity()
            .context("Confirm the T3 connection before sending attachments")?
            .environment_id;
        uploaded_references(self.draft_attachments(), environment)
    }

    pub(super) fn attachment_request(
        &mut self,
        request: &composer::AttachmentRequested,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match request {
            composer::AttachmentRequested::Choose => self.pick_attachment(None, window, cx),
            composer::AttachmentRequested::Paths(paths) => {
                self.attach_paths(paths, None, window, cx)
            }
            composer::AttachmentRequested::Paste => self.paste_attachment(window, cx),
        }
    }

    fn paste_attachment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::note_paste::offer::PasteOffer;
        let offer = cx
            .try_global::<crate::note_paste::ManagedNoteClipboard>()
            .map(|p| p.0.read_chat_offer_in_command(cx));
        match offer {
            Some(Ok(PasteOffer::Files(paths))) => {
                self.attach_paths(&paths, None, window, cx);
                return;
            }
            Some(Ok(PasteOffer::Image(image))) => {
                self.attach_image(image.bytes, window, cx);
                return;
            }
            Some(Ok(PasteOffer::Text(text))) => {
                self.compose
                    .update(cx, |input, cx| input.replace(text, window, cx));
                return;
            }
            Some(Err(crate::note_paste::offer::OfferError::TooLarge)) => {
                self.attachments.notice = Some(
                    "Clipboard content is too large. Images support up to 10 MiB; choose a smaller image.".into(),
                );
                cx.notify();
                return;
            }
            Some(Err(_)) => {
                self.attachments.notice = Some(
                    "Clipboard could not be read. Copy the item again or choose a file.".into(),
                );
                cx.notify();
                return;
            }
            _ => {}
        }
        if let Some(item) = cx.read_from_clipboard() {
            let paths = item.entries().iter().find_map(|entry| match entry {
                gpui::ClipboardEntry::ExternalPaths(paths) => Some(paths.0.to_vec()),
                _ => None,
            });
            if let Some(paths) = paths {
                self.attach_paths(&paths, None, window, cx);
                return;
            }
            let image = item.entries().iter().find_map(|entry| match entry {
                gpui::ClipboardEntry::Image(image) => Some(image.bytes.clone()),
                _ => None,
            });
            if let Some(bytes) = image {
                self.attach_image(bytes, window, cx);
                return;
            }
            if let Some(text) = item.text() {
                self.compose
                    .update(cx, |input, cx| input.replace(text, window, cx));
            }
        }
    }

    fn pick_attachment(
        &mut self,
        replace: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(scope) = self.attachment_scope() else {
            return;
        };
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: replace.is_none(),
            prompt: Some("Attach files to this message".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let selected = picker.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.attachment_scope().as_ref() != Some(&scope) {
                    return;
                }
                match selected {
                    Ok(Ok(Some(paths))) => this.attach_paths(&paths, replace, window, cx),
                    Ok(Ok(None)) => {}
                    _ => {
                        this.attachments.notice = Some(
                            "Could not open the file chooser. Drop the file here instead.".into(),
                        );
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn attach_paths(
        &mut self,
        paths: &[PathBuf],
        replace: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(scope) = self.attachment_scope() else {
            return;
        };
        if !self.attachment_can_add()
            || replace
                .as_ref()
                .is_some_and(|id| !self.draft_attachments().iter().any(|row| &row.id == id))
        {
            return;
        }
        let mut rows = self.draft_attachments().to_vec();
        let mut rejected = false;
        for (index, path) in paths.iter().enumerate() {
            let id = if index == 0 { replace.clone() } else { None }
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            if rows.len() >= MAX_ATTACHMENTS && !rows.iter().any(|row| row.id == id) {
                rejected = true;
                continue;
            }
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "attachment".into());
            let meta = metadata(name);
            let row = Attachment {
                id: id.clone(),
                kind: draft_kind(meta.kind),
                name: meta.name,
                mime_type: meta.mime_type,
                size_bytes: 0,
                source: None,
                state: AttachmentState::Pending,
            };
            if let Some(old) = rows.iter_mut().find(|row| row.id == id) {
                self.attachments.previews.remove(&id);
                self.attachments.expanded_previews.remove(&id);
                self.attachments.prepared.remove(&id);
                *old = row;
            } else {
                rows.push(row);
            }
            self.attachments.sources.insert(
                id,
                Local {
                    scope: scope.clone(),
                    revision: uuid::Uuid::new_v4(),
                    source: Source::File(path.clone()),
                },
            );
        }
        self.attachments.notice =
            rejected.then(|| "You can attach up to 8 files per message.".into());
        self.set_attachment_draft(rows, window, cx);
        self.attachment_start_next(window, cx);
    }

    fn attach_image(&mut self, bytes: Vec<u8>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(scope) = self.attachment_scope() else {
            return;
        };
        if !self.attachment_can_add() {
            return;
        }
        if self.draft_attachments().len() >= MAX_ATTACHMENTS {
            self.attachments.notice = Some("You can attach up to 8 files per message.".into());
            cx.notify();
            return;
        }
        let id = uuid::Uuid::new_v4().to_string();
        let mut rows = self.draft_attachments().to_vec();
        rows.push(Attachment {
            id: id.clone(),
            kind: DraftKind::Image,
            name: "Screenshot.png".into(),
            mime_type: "image/png".into(),
            size_bytes: 0,
            source: None,
            state: AttachmentState::Pending,
        });
        self.attachments.sources.insert(
            id,
            Local {
                scope,
                revision: uuid::Uuid::new_v4(),
                source: Source::Image(Arc::new(bytes)),
            },
        );
        self.attachments.notice = None;
        self.set_attachment_draft(rows, window, cx);
        self.attachment_start_next(window, cx);
    }

    fn has_attachment_source(&self, id: &str) -> bool {
        self.attachments
            .sources
            .get(id)
            .is_some_and(|local| Some(&local.scope) == self.attachment_scope().as_ref())
    }

    fn retry_attachment(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if !self.has_attachment_source(id) {
            self.pick_attachment(Some(id.to_owned()), window, cx);
            return;
        }
        let mut rows = self.draft_attachments().to_vec();
        if let Some(row) = rows.iter_mut().find(|row| row.id == id) {
            row.state = AttachmentState::Pending;
        }
        self.attachments.notice = None;
        self.set_attachment_draft(rows, window, cx);
        self.attachment_start_next(window, cx);
    }

    fn remove_attachment(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.attachments.sources.remove(id);
        self.attachments.previews.remove(id);
        self.attachments.expanded_previews.remove(id);
        self.attachments.prepared.remove(id);
        self.attachments.restored.remove(id);
        let rows = self
            .draft_attachments()
            .iter()
            .filter(|row| row.id != id)
            .cloned()
            .collect();
        self.set_attachment_draft(rows, window, cx);
    }

    pub(super) fn attachment_restore_connection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.active
            || !self.native.active()
            || !reader_features::enabled(reader_features::Feature::Agents, cx)
        {
            return;
        }
        let mut rows = self.draft_attachments().to_vec();
        let scope = self.attachment_scope();
        let mut changed = false;
        for row in &mut rows {
            if matches!(row.state, AttachmentState::Pending)
                && !self
                    .attachments
                    .sources
                    .get(&row.id)
                    .is_some_and(|local| Some(&local.scope) == scope.as_ref())
            {
                row.state = AttachmentState::NeedsReselect;
                changed = true;
            }
        }
        if changed {
            self.set_attachment_draft(rows, window, cx);
        }
        self.attachment_start_next(window, cx);
        self.restore_attachment_previews(window, cx);
        if self.attachments.identity_pending
            || self.attachments.identity_failed
            || self.attachment_identity().is_some()
            || !self
                .draft_attachments()
                .iter()
                .any(|row| matches!(row.state, AttachmentState::Uploaded { .. }))
        {
            return;
        }
        let Some(scope) = self.attachment_scope() else {
            return;
        };
        let Some(client) = self
            .native
            .model_client()
            .filter(|client| client.access_allowed())
        else {
            return;
        };
        self.attachments.identity_pending = true;
        cx.spawn_in(window,async move|this,cx|{
            let reader=client.clone();
            let result=cx.background_executor().spawn(async move{reader.server_identity()}).await;
            let _=this.update_in(cx,|this,window,cx|{
                if this.attachment_scope().as_ref()!=Some(&scope) || !this.native.model_client().is_some_and(|current|Arc::ptr_eq(&current,&client)){return;}
                this.attachments.identity_pending=false;
                match result {
                    Ok(identity)=>{
                        let mut rows=this.draft_attachments().to_vec();
                        for row in &mut rows {
                            if matches!(&row.state,AttachmentState::Uploaded{environment_id,..} if environment_id!=&identity.environment_id){row.state=AttachmentState::NeedsReselect;}
                        }
                        this.identity=Some(identity.clone());this.attachments.identity=Some((Arc::downgrade(&client),identity));
                        this.set_attachment_draft(rows,window,cx);
                        this.restore_attachment_previews(window,cx);
                    }
                    Err(_)=>{this.attachments.identity_failed=true;this.attachments.notice=Some("Could not confirm attachment access. Reconnect or retry.".into());}
                }
                cx.notify();
            });
        }).detach();
    }

    pub(super) fn render_native_attachment_drafts(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut tray = v_flex()
            .id("project-attachment-tray")
            .w_full()
            .max_h(px(200.))
            .overflow_y_scroll()
            .gap_1()
            .debug_selector(|| "project-attachment-tray".into());
        for row in self.draft_attachments() {
            if let Some((preview, _)) = self.attachment_preview(row) {
                let preview_id = row.id.clone();
                let selector = format!("attachment-preview-{}", row.id);
                let label = format!("Preview {}", row.name);
                tray = tray.child(
                    Button::new(SharedString::from(selector.clone()))
                        .debug_selector(move || selector.clone())
                        .accessibility_label(label.clone())
                        .tooltip(label)
                        .ghost()
                        .w(px(128.))
                        .h(px(96.))
                        .p_0()
                        .child(
                            gpui::img(preview)
                                .size_full()
                                .object_fit(ObjectFit::Contain),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.preview_attachment(&preview_id, window, cx);
                        })),
                );
            }
            let id = row.id.clone();
            let retry = id.clone();
            let remove = id.clone();
            let status = match &row.state {
                AttachmentState::Pending => {
                    if self
                        .attachments
                        .inflight
                        .as_ref()
                        .is_some_and(|(_, active)| active == &id)
                    {
                        "Uploading…"
                    } else {
                        "Waiting…"
                    }
                }
                AttachmentState::Error { .. } => "Upload failed",
                AttachmentState::NeedsReselect => "Needs reselect",
                AttachmentState::Uploaded { .. } => "Attached",
            };
            let needs_action = matches!(
                row.state,
                AttachmentState::Error { .. } | AttachmentState::NeedsReselect
            );
            tray = tray.child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().truncate().child(row.name.clone()))
                    .child(
                        div()
                            .debug_selector({
                                let id = id.clone();
                                move || format!("attachment-status-{id}")
                            })
                            .text_color(brand::palette(cx).text_muted)
                            .child(status),
                    )
                    .when(needs_action, |line| {
                        line.child(
                            Button::new(SharedString::from(format!("attachment-retry-{id}")))
                                .debug_selector({
                                    let id = id.clone();
                                    move || format!("attachment-retry-{id}")
                                })
                                .small()
                                .ghost()
                                .label(if self.has_attachment_source(&id) {
                                    "Retry"
                                } else {
                                    "Select again"
                                })
                                .disabled(self.busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.retry_attachment(&retry, window, cx)
                                })),
                        )
                    })
                    .child(
                        Button::new(SharedString::from(format!("attachment-remove-{id}")))
                            .debug_selector({
                                let id = id.clone();
                                move || format!("attachment-remove-{id}")
                            })
                            .small()
                            .ghost()
                            .label("Remove")
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.remove_attachment(&remove, window, cx)
                            })),
                    ),
            );
            if matches!(row.state, AttachmentState::NeedsReselect) {
                tray = tray.child(div().text_sm().whitespace_normal().child("The local file is no longer available. Select it again or remove it before sending."));
            }
            tray = tray.child(self.render_restored_attachment_status(row, cx));
            if let AttachmentState::Error { message } = &row.state {
                tray = tray.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(message.clone()),
                );
            }
        }
        if let Some(notice) = &self.attachments.notice {
            tray = tray.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(notice.clone()),
            );
        }
        if self.attachments.identity_failed {
            tray = tray.child(
                Button::new("attachment-reconnect")
                    .small()
                    .ghost()
                    .label("Retry attachment access")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.attachments.identity_failed = false;
                        this.attachments.notice = None;
                        this.attachment_restore_connection(window, cx);
                    })),
            )
        }
        tray.into_any_element()
    }

    fn preview_attachment(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(scope) = self.attachment_scope() else {
            return;
        };
        let Some(row) = self.draft_attachments().iter().find(|row| row.id == id) else {
            return;
        };
        let Some((_, image)) = self.attachment_preview(row) else {
            return;
        };
        let name = row.name.clone();
        let selector = format!("attachment-expanded-{id}");
        let parent = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, _| {
            let width = (window.viewport_size().width - px(48.))
                .min(px(960.))
                .max(px(120.));
            let height = (window.viewport_size().height - px(200.)).max(px(120.));
            let parent = parent.clone();
            let scope = scope.clone();
            let selector = selector.clone();
            dialog
                .title(format!("Preview {name}"))
                .width(width)
                .on_ok(|_, _, _| false)
                .on_close(move |_, window, cx| {
                    let _ = parent.update(cx, |view, cx| {
                        if view.attachment_scope().as_ref() == Some(&scope) {
                            view.compose.update(cx, |input, cx| input.focus(window, cx));
                        }
                    });
                })
                .child(
                    div()
                        .w_full()
                        .h(height)
                        .debug_selector(|| "attachment-expanded-image".into())
                        .child(
                            div()
                                .size_full()
                                .debug_selector(move || selector.clone())
                                .child(
                                    gpui::img(image.clone())
                                        .size_full()
                                        .object_fit(ObjectFit::Contain),
                                ),
                        ),
                )
        });
    }

    fn attachment_source_is_current(&self, lease: &PreparationLease) -> bool {
        self.attachment_scope().as_ref() == Some(&lease.scope)
            && self
                .attachments
                .sources
                .get(&lease.id)
                .is_some_and(|source| {
                    source.scope == lease.scope && source.revision == lease.revision
                })
            && self
                .draft_attachments()
                .iter()
                .any(|row| row.id == lease.id)
    }

    fn cached_preparation(&self, id: &str, source: &Local) -> Option<Arc<Prepared>> {
        self.attachments
            .prepared
            .get(id)
            .filter(|cache| cache.scope == source.scope && cache.revision == source.revision)
            .map(|cache| cache.prepared.clone())
    }

    fn attachment_adopt_prepared(
        &mut self,
        lease: &PreparationLease,
        prepared: Arc<Prepared>,
        rows: &mut [Attachment],
    ) {
        let row = rows
            .iter_mut()
            .find(|row| row.id == lease.id)
            .expect("current source has a row");
        row.kind = draft_kind(prepared.metadata.kind);
        row.name = prepared.metadata.name.clone();
        row.mime_type = prepared.metadata.mime_type.clone();
        row.size_bytes = prepared.metadata.size_bytes;
        if prepared.metadata.kind == AttachmentKind::Image {
            if let Some((thumb, original)) = &prepared.previews {
                self.attachments
                    .previews
                    .insert(lease.id.clone(), thumb.clone());
                self.attachments
                    .expanded_previews
                    .insert(lease.id.clone(), original.clone());
            }
            // Only image bytes are retained. Generic 50 MiB files are prepared
            // lazily by the one upload worker and never accumulate in this map.
            self.attachments.prepared.insert(
                lease.id.clone(),
                CachedPreparation {
                    scope: lease.scope.clone(),
                    revision: lease.revision,
                    prepared,
                },
            );
        }
    }

    /// One local image decoder at a time, independent of serial upload access.
    /// Staging already reserved the count slots; this worker needs no server
    /// identity, operate scope or HTTP acknowledgement to show local previews.
    fn attachment_prepare_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.attachments.preparing.is_some() {
            return;
        }
        let Some(scope) = self.attachment_scope() else {
            return;
        };
        let next = self.draft_attachments().iter().find_map(|row| {
            if row.kind != DraftKind::Image || !matches!(row.state, AttachmentState::Pending) {
                return None;
            }
            let source = self
                .attachments
                .sources
                .get(&row.id)
                .filter(|source| source.scope == scope)?;
            if self.cached_preparation(&row.id, source).is_some() {
                return None;
            }
            Some((row.id.clone(), row.name.clone(), source.clone()))
        });
        let Some((id, name, source)) = next else {
            return;
        };
        let other_images = self
            .draft_attachments()
            .iter()
            .filter(|row| row.id != id && row.kind == DraftKind::Image)
            .fold(0_u64, |bytes, row| bytes.saturating_add(row.size_bytes));
        let lease = PreparationLease {
            scope,
            id,
            revision: source.revision,
            job: uuid::Uuid::new_v4(),
        };
        self.attachments.preparing = Some(lease.clone());
        cx.spawn_in(window, async move |this, cx| {
            let prepared = cx
                .background_executor()
                .spawn(async move { prepare_with_preview(&source.source, &name, other_images) })
                .await
                .map_err(|error| format!("{error:#}"));
            let _ = this.update_in(cx, |this, window, cx| {
                this.attachment_finish_local_preparation(&lease, prepared, window, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn attachment_finish_local_preparation(
        &mut self,
        lease: &PreparationLease,
        prepared: Result<Arc<Prepared>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // A late completion never releases another source/client/job's lease.
        if self.attachments.preparing.as_ref() != Some(lease) {
            return false;
        }
        self.attachments.preparing = None;
        if !self.attachment_source_is_current(lease) {
            self.attachment_start_next(window, cx);
            return false;
        }
        let mut rows = self.draft_attachments().to_vec();
        match prepared {
            Ok(prepared) => self.attachment_adopt_prepared(lease, prepared, &mut rows),
            Err(message) => {
                rows.iter_mut()
                    .find(|row| row.id == lease.id)
                    .unwrap()
                    .state = AttachmentState::Error { message }
            }
        }
        self.set_attachment_draft(rows, window, cx);
        self.attachment_start_next(window, cx);
        true
    }

    fn attachment_owns_upload(&self, lease: &PreparationLease) -> bool {
        self.attachments.upload_job == Some(lease.job)
            && self.attachments.inflight.as_ref() == Some(&(lease.scope.clone(), lease.id.clone()))
    }

    fn attachment_release_upload(&mut self, lease: &PreparationLease) -> bool {
        if !self.attachment_owns_upload(lease) {
            return false;
        }
        self.attachments.inflight = None;
        self.attachments.upload_job = None;
        true
    }

    fn attachment_upload_is_current(
        &self,
        lease: &PreparationLease,
        client: &Arc<NativeClient>,
    ) -> bool {
        self.attachment_owns_upload(lease)
            && self.attachment_source_is_current(lease)
            && self
                .native
                .model_client()
                .is_some_and(|current| Arc::ptr_eq(&current, client))
    }

    pub(super) fn attachment_start_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let retained: std::collections::HashSet<_> = self
            .draft_attachments()
            .iter()
            .map(|row| row.id.clone())
            .collect();
        let scope = self.attachment_scope();
        self.attachments
            .sources
            .retain(|id, source| retained.contains(id) && Some(&source.scope) == scope.as_ref());
        let current: std::collections::HashSet<_> =
            self.attachments.sources.keys().cloned().collect();
        self.attachments
            .previews
            .retain(|id, _| current.contains(id));
        self.attachments
            .expanded_previews
            .retain(|id, _| current.contains(id));
        let sources = &self.attachments.sources;
        self.attachments.prepared.retain(|id, cached| {
            sources.get(id).is_some_and(|source| {
                source.revision == cached.revision && source.scope == cached.scope
            })
        });
        if !reader_features::enabled(reader_features::Feature::Agents, cx) {
            return;
        }
        // This must precede the upload lease/capability checks. Another image
        // still receives its local preview while the first upload is held.
        self.attachment_prepare_next(window, cx);
        if self.attachments.inflight.is_some() {
            return;
        }
        let Some(scope) = self.attachment_scope() else {
            return;
        };
        let Some(client) = self
            .native
            .model_client()
            .filter(|client| client.can_operate())
        else {
            return;
        };
        let mut next = None;
        for row in self.draft_attachments() {
            if !matches!(row.state, AttachmentState::Pending) {
                continue;
            }
            let Some(source) = self
                .attachments
                .sources
                .get(&row.id)
                .filter(|source| source.scope == scope)
            else {
                continue;
            };
            let cached = self.cached_preparation(&row.id, source);
            if row.kind == DraftKind::Image && cached.is_none() {
                break;
            }
            next = Some((row.id.clone(), row.name.clone(), source.clone(), cached));
            break;
        }
        let Some((id, name, source, cached)) = next else {
            return;
        };
        let other_images = self
            .draft_attachments()
            .iter()
            .filter(|row| row.id != id && row.kind == DraftKind::Image)
            .fold(0_u64, |bytes, row| bytes.saturating_add(row.size_bytes));
        let known = self.attachment_identity().cloned();
        let lease = PreparationLease {
            scope: scope.clone(),
            id: id.clone(),
            revision: source.revision,
            job: uuid::Uuid::new_v4(),
        };
        self.attachments.inflight = Some((scope, id));
        self.attachments.upload_job = Some(lease.job);
        cx.spawn_in(window, async move |this, cx| {
            let prepared = match cached {
                Some(prepared) => Ok(prepared),
                None => cx
                    .background_executor()
                    .spawn(async move { prepare_with_preview(&source.source, &name, other_images) })
                    .await
                    .map_err(|error| format!("{error:#}")),
            };
            let published = this
                .update_in(cx, |this, window, cx| {
                    if !this.attachment_upload_is_current(&lease, &client) {
                        if this.attachment_release_upload(&lease) {
                            this.attachment_start_next(window, cx);
                        }
                        return false;
                    }
                    let mut rows = this.draft_attachments().to_vec();
                    match &prepared {
                        Ok(prepared) => {
                            this.attachment_adopt_prepared(&lease, prepared.clone(), &mut rows)
                        }
                        Err(message) => {
                            rows.iter_mut()
                                .find(|row| row.id == lease.id)
                                .unwrap()
                                .state = AttachmentState::Error {
                                message: message.clone(),
                            };
                            this.attachment_release_upload(&lease);
                            this.set_attachment_draft(rows, window, cx);
                            this.attachment_start_next(window, cx);
                            return false;
                        }
                    }
                    this.set_attachment_draft(rows, window, cx);
                    true
                })
                .unwrap_or(false);
            if !published {
                return;
            }
            let Ok(prepared) = prepared else {
                return;
            };
            let worker = client.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let identity = match known {
                        Some(identity) => identity,
                        None => worker.server_identity()?,
                    };
                    // Image uploads use exactly the immutable bytes that produced
                    // their preview, including retries after the external file edits.
                    let (metadata, bytes) = match Arc::try_unwrap(prepared) {
                        // A generic file has no cache owner: move its one buffer.
                        Ok(prepared) => (prepared.metadata, prepared.bytes),
                        Err(prepared) => (prepared.metadata.clone(), prepared.bytes.clone()),
                    };
                    let attachment = worker.upload_attachment(&metadata, bytes)?;
                    Ok::<_, anyhow::Error>((identity, attachment))
                })
                .await
                .map_err(|error| format!("{error:#}"));
            let cleanup = result.as_ref().ok().map(|(_, asset)| asset.id.clone());
            let applied = this
                .update_in(cx, |this, window, cx| {
                    if !this.attachment_upload_is_current(&lease, &client) {
                        if this.attachment_release_upload(&lease) {
                            this.attachment_start_next(window, cx);
                        }
                        return false;
                    }
                    this.attachment_release_upload(&lease);
                    let mut rows = this.draft_attachments().to_vec();
                    let row = rows.iter_mut().find(|row| row.id == lease.id).unwrap();
                    match result {
                        Ok((identity, asset)) => {
                            row.kind = draft_kind(asset.metadata.kind);
                            row.name = asset.metadata.name;
                            row.mime_type = asset.metadata.mime_type;
                            row.size_bytes = asset.metadata.size_bytes;
                            row.source = asset.source;
                            row.state = AttachmentState::Uploaded {
                                attachment_id: asset.id,
                                environment_id: identity.environment_id.clone(),
                            };
                            this.identity = Some(identity.clone());
                            this.attachments.identity = Some((Arc::downgrade(&client), identity));
                        }
                        Err(message) => row.state = AttachmentState::Error { message },
                    }
                    this.set_attachment_draft(rows, window, cx);
                    this.attachment_start_next(window, cx);
                    true
                })
                .unwrap_or(false);
            if !applied {
                if let Some(id) = cleanup {
                    cx.background_executor()
                        .spawn(async move {
                            let _ = client.delete_attachment(&id);
                        })
                        .detach();
                }
            }
        })
        .detach();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn preparation_uploads_actual_file_bytes_and_rejects_missing_empty_and_over_budget_images() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("report.txt");
        let bytes = b"Actual file bytes, not a path.\n\0\xff";
        std::fs::write(&path, bytes).unwrap();
        let (meta, actual) = prepare(&Source::File(path.clone()), "report.txt", 0).unwrap();
        assert_eq!(meta.kind, AttachmentKind::File);
        assert_eq!(meta.size_bytes, bytes.len() as u64);
        assert_eq!(actual, bytes);
        std::fs::write(&path, []).unwrap();
        assert!(prepare(&Source::File(path.clone()), "report.txt", 0).is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(prepare(&Source::File(path), "report.txt", 0).is_err());
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 2)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let png = png.into_inner();
        assert!(thumbnail(&png).is_some());
        assert!(prepare(
            &Source::Image(Arc::new(png.clone())),
            "Screenshot.png",
            MAX_TOTAL_IMAGE_BYTES
        )
        .is_err());
        let (meta, actual) =
            prepare(&Source::Image(Arc::new(png.clone())), "Screenshot.png", 0).unwrap();
        assert_eq!(meta.kind, AttachmentKind::Image);
        assert_eq!(actual, png);
    }

    #[test]
    fn image_preparation_preserves_every_supported_format_and_rejects_unsupported_or_oversized() {
        let temp = tempfile::tempdir().unwrap();
        for (format, name) in [
            (image::ImageFormat::Png, "exact.png"),
            (image::ImageFormat::Jpeg, "exact.jpg"),
            (image::ImageFormat::Gif, "exact.gif"),
            (image::ImageFormat::WebP, "exact.webp"),
        ] {
            let mut bytes = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(8, 6)
                .write_to(&mut bytes, format)
                .unwrap();
            let bytes = bytes.into_inner();
            let path = temp.path().join(name);
            std::fs::write(&path, &bytes).unwrap();
            for source in [Source::File(path), Source::Image(Arc::new(bytes.clone()))] {
                let prepared = prepare_with_preview(&source, name, 0).unwrap();
                assert_eq!(
                    prepared.bytes, bytes,
                    "upload must retain original encoded bytes"
                );
                assert!(prepared.previews.is_some());
            }
        }
        let mut bmp = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(8, 6)
            .write_to(&mut bmp, image::ImageFormat::Bmp)
            .unwrap();
        let path = temp.path().join("unsupported.bmp");
        std::fs::write(&path, bmp.get_ref()).unwrap();
        for source in [
            Source::File(path),
            Source::Image(Arc::new(bmp.into_inner())),
        ] {
            assert!(prepare(&source, "unsupported.bmp", 0)
                .unwrap_err()
                .to_string()
                .contains("Unsupported image type"));
        }
        let path = temp.path().join("oversized.png");
        let mut bytes = vec![0; MAX_IMAGE_BYTES as usize + 1];
        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        std::fs::write(&path, &bytes).unwrap();
        for source in [Source::File(path), Source::Image(Arc::new(bytes))] {
            assert!(prepare(&source, "oversized.png", 0)
                .unwrap_err()
                .to_string()
                .contains("10 MiB"));
        }
    }

    #[test]
    fn issue2521_tiff_attachment_converts_losslessly_with_preview_and_output_budgets() {
        let pixels = image::RgbaImage::from_fn(8, 6, |x, y| {
            image::Rgba([x as u8 * 20, y as u8 * 30, 7, if x == 0 { 0 } else { 255 }])
        });
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(pixels.clone())
            .write_to(&mut encoded, image::ImageFormat::Tiff)
            .unwrap();
        let bytes = encoded.into_inner();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Preview.tiff");
        std::fs::write(&path, &bytes).unwrap();
        for source in [Source::Image(Arc::new(bytes.clone())), Source::File(path)] {
            let prepared = prepare_with_preview(&source, "Preview.tiff", 0).unwrap();
            assert_eq!(prepared.metadata.kind, AttachmentKind::Image);
            assert_eq!(prepared.metadata.name, "Preview.png");
            assert_eq!(prepared.metadata.mime_type, "image/png");
            assert_eq!(prepared.metadata.size_bytes, prepared.bytes.len() as u64);
            assert_eq!(
                image::load_from_memory(&prepared.bytes).unwrap().to_rgba8(),
                pixels
            );
            assert!(prepared.previews.is_some());
            assert!(prepare(&source, "Preview.tiff", MAX_TOTAL_IMAGE_BYTES)
                .unwrap_err()
                .to_string()
                .contains("80 MiB"));
        }
        assert!(prepare(
            &Source::Image(Arc::new(b"II*\0truncated TIFF".to_vec())),
            "bad.tiff",
            0
        )
        .is_err());
    }

    #[gpui::test]
    fn chooser_remove_restore_guard_and_confirmed_native_references_preserve_text(
        cx: &mut TestAppContext,
    ) {
        use okilum_t3::stream::NativeToken;
        cx.update(crate::test_init);
        let temp = tempfile::tempdir().unwrap();
        let env = Environment::new("fixture", "http://127.0.0.1:1").unwrap();
        // No network requests can be sent by this fixture.
        let token = NativeToken::from_secret(&env, &serde_json::to_vec(&serde_json::json!({"origin":env.origin(),"access_token":"fixture","expires_at":chrono::Utc::now().timestamp() as u64+3600,"scope":"orchestration:read"})).unwrap()).unwrap();
        let client =
            Arc::new(NativeClient::new(env.clone(), token, Duration::from_millis(10)).unwrap());
        let (root, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = super::super::fixture::view(
                    temp.path().to_owned(),
                    Arc::new(super::super::fixture::Fake::default()),
                    window,
                    cx,
                );
                view.active = false;
                view.environment = Some(env);
                view.native.set_detail_test_client(client.clone());
                view.compose.update(cx, |input, cx| {
                    input.set_value("Keep this\nmultiline text", window, cx)
                });
                view
            });
            Root::new(view, window, cx)
        });
        let view = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<ProjectView>().unwrap()
        });
        view.update_in(visual, |view, window, cx| {
            view.pick_attachment(None, window, cx)
        });
        visual.simulate_path_prompt_response(|options| {
            assert!(options.files && !options.directories && options.multiple);
            Some(vec![
                PathBuf::from("/missing/notes.pdf"),
                PathBuf::from("/missing/image.png"),
            ])
        });
        visual.run_until_parked();
        view.update_in(visual, |view, window, cx| {
            assert_eq!(view.draft_attachments().len(), 2);
            assert_eq!(view.draft_attachments()[0].mime_type, "application/pdf");
            assert_eq!(view.draft_attachments()[1].mime_type, "image/png");
            let ids: Vec<_> = view
                .draft_attachments()
                .iter()
                .map(|row| row.id.clone())
                .collect();
            assert!(view.prepared_attachments().is_err());
            for id in ids {
                view.remove_attachment(&id, window, cx);
            }
            assert!(view.draft_attachments().is_empty());
            assert_eq!(
                view.compose.read(cx).value().as_str(),
                "Keep this\nmultiline text"
            );
            // Pending rows reserve slots synchronously, before preparation/upload.
            let paths: Vec<_> = (0..8)
                .map(|n| {
                    PathBuf::from(format!(
                        "/missing/{n}.{}",
                        if n % 2 == 0 { "png" } else { "txt" }
                    ))
                })
                .collect();
            view.attach_paths(&paths, None, window, cx);
            let first_eight = view.draft_attachments().to_vec();
            assert_eq!(first_eight.len(), MAX_ATTACHMENTS);
            assert!(first_eight
                .iter()
                .all(|row| row.state == AttachmentState::Pending));
            assert_eq!(
                first_eight
                    .iter()
                    .filter(|row| row.kind == DraftKind::Image)
                    .count(),
                4
            );
            view.attach_image(vec![1, 2, 3], window, cx);
            view.attach_paths(&[PathBuf::from("/missing/ninth.txt")], None, window, cx);
            assert_eq!(view.draft_attachments(), first_eight);
            assert_eq!(
                view.attachments.sources.len(),
                MAX_ATTACHMENTS,
                "no ninth source or upload"
            );
            assert_eq!(
                view.attachments.notice.as_deref(),
                Some("You can attach up to 8 files per message.")
            );
            assert_eq!(
                view.compose.read(cx).value().as_str(),
                "Keep this\nmultiline text"
            );
            // Reselect replaces its slot at capacity; removal releases one slot.
            let replace_id = first_eight[0].id.clone();
            view.attach_paths(
                &[PathBuf::from("/missing/reselected.txt")],
                Some(replace_id.clone()),
                window,
                cx,
            );
            assert_eq!(view.draft_attachments().len(), MAX_ATTACHMENTS);
            assert_eq!(view.draft_attachments()[0].id, replace_id);
            assert_eq!(view.draft_attachments()[0].name, "reselected.txt");
            view.remove_attachment(&replace_id, window, cx);
            view.attach_image(vec![1, 2, 3], window, cx);
            assert_eq!(view.draft_attachments().len(), MAX_ATTACHMENTS);
            assert!(view.attachments.notice.is_none());
            let identity = ServerIdentity {
                environment_id: "native-environment".into(),
                label: "Fixture".into(),
            };
            view.attachments.identity = Some((Arc::downgrade(&client), identity));
            let mut row = Attachment {
                id: "local".into(),
                kind: DraftKind::File,
                name: "notes.txt".into(),
                mime_type: "text/plain".into(),
                size_bytes: 4,
                source: None,
                state: AttachmentState::NeedsReselect,
            };
            view.draft_set_attachments(vec![row.clone()], window, cx);
            assert!(view.prepared_attachments().is_err());
            row.state = AttachmentState::Uploaded {
                attachment_id: "server-asset".into(),
                environment_id: "wrong-environment".into(),
            };
            view.draft_set_attachments(vec![row.clone()], window, cx);
            assert!(view.prepared_attachments().is_err());
            row.state = AttachmentState::Uploaded {
                attachment_id: "server-asset".into(),
                environment_id: "native-environment".into(),
            };
            view.draft_set_attachments(vec![row], window, cx);
            let prepared = view.prepared_attachments().unwrap();
            assert_eq!(prepared[0].id, "server-asset");
            assert!(
                !view.draft_is_sendable(),
                "MCP must never discard native attachments"
            );
            view.attachments.connection_changed();
            assert!(
                view.prepared_attachments().is_err(),
                "a replacement client must reconfirm its environment"
            );
            assert_eq!(
                view.compose.read(cx).value().as_str(),
                "Keep this\nmultiline text"
            );
            view.draft_prepare_identity(window, cx);
            assert!(!view.draft_ready());
            assert!(
                !view.attachment_can_add(),
                "restore must finish before attaching files"
            );
        });
    }
    #[test]
    fn preview_keeps_original_supported_encoded_bytes_and_uses_a_bounded_thumbnail() {
        for format in [
            image::ImageFormat::Png,
            image::ImageFormat::Jpeg,
            image::ImageFormat::Gif,
            image::ImageFormat::WebP,
        ] {
            let mut encoded = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(1600, 1200)
                .write_to(&mut encoded, format)
                .unwrap();
            let encoded = encoded.into_inner();
            let (thumb, original) =
                image_previews(&encoded).expect("supported image has a preview");
            assert_eq!(
                original.bytes, encoded,
                "expanded image must retain its original codec payload"
            );
            assert_eq!(
                original.format.mime_type(),
                metadata(format!("image.{}", format.extensions_str()[0])).mime_type
            );
            let thumbnail = image::load_from_memory(&thumb.bytes).unwrap();
            assert_eq!((thumbnail.width(), thumbnail.height()), (640, 480));
            assert!(!Arc::ptr_eq(&thumb, &original));
        }
        assert!(image_previews(b"not an image").is_none());
        use image::AnimationDecoder as _;
        let mut animated = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut animated);
            encoder
                .encode_frames([
                    image::Frame::new(image::RgbaImage::from_pixel(
                        4,
                        4,
                        image::Rgba([255, 0, 0, 255]),
                    )),
                    image::Frame::new(image::RgbaImage::from_pixel(
                        4,
                        4,
                        image::Rgba([0, 255, 0, 255]),
                    )),
                ])
                .unwrap();
        }
        let (_, original) = image_previews(&animated).unwrap();
        assert_eq!(original.format, gpui::ImageFormat::Gif);
        assert_eq!(original.bytes, animated);
        let frames = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(&original.bytes))
            .unwrap()
            .into_frames()
            .collect_frames()
            .unwrap();
        assert_eq!(frames.len(), 2, "expanded GIF keeps both animation frames");
    }

    struct PreviewHost(Entity<ProjectView>);
    impl Render for PreviewHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(self.0.clone())
                .children(Root::render_dialog_layer(window, cx))
        }
    }

    fn draw_attachment_preview(visual: &mut VisualTestContext) {
        // Like the history-image fixture, refresh cached ancestor views before
        // reading debug selectors. A cached paint can be visible without
        // replaying selector registration in the pinned GPUI test context.
        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
    }

    #[gpui::test]
    fn prepared_pending_and_failed_images_open_a_real_viewer_without_sending(
        cx: &mut TestAppContext,
    ) {
        use okilum_t3::stream::NativeToken;
        cx.update(crate::test_init);
        let temp = tempfile::tempdir().unwrap();
        let gateway = Arc::new(super::super::fixture::Fake::default());
        let env = Environment::new("preview", "http://127.0.0.1:1").unwrap();
        let token = NativeToken::from_secret(&env, &serde_json::to_vec(&serde_json::json!({"origin":env.origin(),"access_token":"fixture","expires_at":chrono::Utc::now().timestamp() as u64+3600,"scope":"orchestration:read"})).unwrap()).unwrap();
        let client =
            Arc::new(NativeClient::new(env.clone(), token, Duration::from_millis(10)).unwrap());
        assert!(
            !client.can_operate(),
            "positive control: this fixture cannot upload HTTP"
        );
        let image = |color| {
            let mut encoded = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                1600,
                1200,
                image::Rgba(color),
            ))
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
            encoded.into_inner()
        };
        let red = image([255, 0, 0, 255]);
        let blue = image([0, 0, 255, 255]);
        let paths = [temp.path().join("red.png"), temp.path().join("blue.png")];
        std::fs::write(&paths[0], &red).unwrap();
        std::fs::write(&paths[1], &blue).unwrap();
        let mut view = None;
        let mut ids = Vec::new();
        let mut held = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let project = cx.new(|cx| {
                let mut view = super::super::fixture::view(
                    temp.path().to_owned(),
                    gateway.clone(),
                    window,
                    cx,
                );
                view.active = false;
                view.environment = Some(env);
                view.native.set_detail_test_client(client.clone());
                view.compose.update(cx, |input, cx| {
                    input.set_value("Keep this draft", window, cx)
                });
                // Exercise the actual asynchronous local queue, not its publish
                // helper. The serial upload is represented by a held lease;
                // no real HTTP request is claimed by this read-only fixture.
                view.attach_paths(&paths, None, window, cx);
                ids = view
                    .draft_attachments()
                    .iter()
                    .map(|row| row.id.clone())
                    .collect();
                assert_eq!(ids.len(), 2);
                held = Some((view.attachment_scope().unwrap(), ids[0].clone()));
                view.attachments.inflight = held.clone();
                assert!(
                    view.attachments.preparing.is_some(),
                    "positive control: first local preparation really started"
                );
                view
            });
            view = Some(project.clone());
            let host = cx.new(|_| PreviewHost(project));
            Root::new(host, window, cx)
        });
        let view = view.unwrap();
        visual.simulate_resize(size(px(1000.), px(900.)));
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert_eq!(
                view.attachments.inflight, held,
                "local preparation must not consume/release the held upload lease"
            );
            assert!(view.attachments.preparing.is_none());
            assert_eq!(
                view.attachments.prepared.len(),
                2,
                "both image sources prepared behind the first held upload"
            );
            for (index, expected) in [&red, &blue].into_iter().enumerate() {
                assert_eq!(
                    &view.attachments.expanded_previews[&ids[index]].bytes,
                    expected
                );
                assert_eq!(
                    &view.attachments.prepared[&ids[index]].prepared.bytes,
                    expected
                );
                assert!(matches!(
                    view.draft_attachments()[index].state,
                    AttachmentState::Pending
                ));
                assert!(view.attachments.previews.contains_key(&ids[index]));
            }
            assert!(
                view.attachments.identity.is_none(),
                "previews do not wait for server identity"
            );
        });
        for failure in [false, true] {
            if failure {
                view.update_in(visual, |view, window, cx| {
                    let mut rows = view.draft_attachments().to_vec();
                    rows[0].state = AttachmentState::Error {
                        message: "Upload unavailable".into(),
                    };
                    view.draft_set_attachments(rows, window, cx);
                });
                visual.run_until_parked();
            }
            for id in &ids {
                let before = view.read_with(visual, |view, cx| {
                    (
                        view.compose.read(cx).value().to_string(),
                        view.draft_attachments().to_vec(),
                    )
                });
                let selector: &'static str =
                    Box::leak(format!("attachment-preview-{id}").into_boxed_str());
                let expanded_selector: &'static str =
                    Box::leak(format!("attachment-expanded-{id}").into_boxed_str());
                draw_attachment_preview(visual);
                let mut thumb = visual
                    .debug_bounds(selector)
                    .expect("both pending/failed images have actual clickable thumbnails");
                let tray = visual.debug_bounds("project-attachment-tray").unwrap();
                if thumb.center().y < tray.top() || thumb.center().y > tray.bottom() {
                    visual.simulate_event(gpui::ScrollWheelEvent {
                        position: tray.center(),
                        delta: gpui::ScrollDelta::Pixels(point(
                            px(0.),
                            if thumb.center().y > tray.bottom() {
                                px(-1600.)
                            } else {
                                px(1600.)
                            },
                        )),
                        ..Default::default()
                    });
                    visual.run_until_parked();
                    draw_attachment_preview(visual);
                    thumb = visual.debug_bounds(selector).expect("scrolled thumbnail");
                }
                let viewport = visual.update(|window, _| Bounds {
                    origin: Point::default(),
                    size: window.viewport_size(),
                });
                let tray = visual.debug_bounds("project-attachment-tray").unwrap();
                let visible_thumb = thumb.intersect(&tray).intersect(&viewport);
                assert!(visible_thumb.size.width > px(0.) && visible_thumb.size.height > px(0.),
                    "positive control: actual thumbnail pixels must be visible before clicking: thumb={thumb:?}, tray={tray:?}, viewport={viewport:?}");
                // A scrolled thumbnail may be partially clipped. Click its
                // genuinely visible pixels rather than an off-window center.
                visual.simulate_click(visible_thumb.center(), Modifiers::default());
                visual.run_until_parked();
                draw_attachment_preview(visual);
                assert!(
                    visual.debug_bounds("dialog-layer").is_some(),
                    "positive control: the real thumbnail click opened the host's visible dialog layer"
                );
                let expanded = visual
                    .debug_bounds(expanded_selector)
                    .expect("viewer identifies the clicked image, not its neighbour");
                assert!(
                    expanded.size.width > thumb.size.width
                        && expanded.size.height > thumb.size.height,
                    "positive control: actual viewer expands beyond thumbnail"
                );
                visual.simulate_keystrokes("enter");
                visual.run_until_parked();
                assert!(
                    visual.debug_bounds(expanded_selector).is_some(),
                    "Enter is inert"
                );
                visual.simulate_keystrokes("escape");
                visual.run_until_parked();
                draw_attachment_preview(visual);
                assert!(visual.debug_bounds("attachment-expanded-image").is_none());
                view.read_with(visual, |view, cx| {
                    assert_eq!(view.compose.read(cx).value().as_str(), before.0);
                    assert_eq!(view.draft_attachments(), before.1);
                    assert_eq!(view.attachments.inflight, held);
                });
                visual.update(|window, cx| {
                    assert!(view
                        .read(cx)
                        .compose
                        .read(cx)
                        .focus_handle(cx)
                        .is_focused(window))
                });
                assert!(gateway.sends.lock().unwrap().is_empty());
            }
        }
        // File edits do not change an already displayed/upload-ready source.
        std::fs::write(&paths[0], &blue).unwrap();
        let old = view.read_with(visual, |view, _| {
            view.attachments.prepared[&ids[0]].prepared.clone()
        });
        view.update_in(visual, |view, window, cx| {
            view.retry_attachment(&ids[0], window, cx)
        });
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            let source = &view.attachments.sources[&ids[0]];
            let cached = view.cached_preparation(&ids[0], source).unwrap();
            assert!(Arc::ptr_eq(&cached, &old));
            assert_eq!(
                cached.bytes, red,
                "Retry uploads the image that was previewed, even after its external file changes"
            );
        });
        let old_revision =
            view.read_with(visual, |view, _| view.attachments.sources[&ids[0]].revision);
        let stale = PreparationLease {
            scope: held.as_ref().unwrap().0.clone(),
            id: ids[0].clone(),
            revision: old_revision,
            job: uuid::Uuid::new_v4(),
        };
        view.update_in(visual, |view, window, cx| {
            view.attach_paths(&[paths[0].clone()], Some(ids[0].clone()), window, cx);
            assert_ne!(view.attachments.sources[&ids[0]].revision, old_revision);
            assert!(!view.attachments.previews.contains_key(&ids[0]));
            assert!(!view.attachments.expanded_previews.contains_key(&ids[0]));
            assert!(!view.attachments.prepared.contains_key(&ids[0]));
            let current = view
                .attachments
                .preparing
                .clone()
                .expect("replacement starts a new local preparation");
            assert!(!view.attachment_finish_local_preparation(&stale, Ok(old.clone()), window, cx));
            assert_eq!(
                view.attachments.preparing.as_ref(),
                Some(&current),
                "late old preparation must not release replacement's lease"
            );
            view.attachments.upload_job = Some(stale.job);
            assert!(
                !view.attachment_upload_is_current(&stale, &client),
                "source revision rejects an old upload even when it still owns its job"
            );
            let newer_job = uuid::Uuid::new_v4();
            view.attachments.upload_job = Some(newer_job);
            assert!(!view.attachment_release_upload(&stale));
            assert_eq!(
                view.attachments.upload_job,
                Some(newer_job),
                "old upload completion cannot release a newer job"
            );
            view.attachments.upload_job = None;
        });
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert_eq!(
                view.attachments.expanded_previews[&ids[0]].bytes, blue,
                "new revision owns the replacement image"
            );
            assert_eq!(view.attachments.prepared[&ids[0]].prepared.bytes, blue);
            assert_eq!(view.attachments.inflight, held);
        });
        // Generic files do not collect in the local image byte cache.
        let generic = temp.path().join("report.pdf");
        std::fs::write(&generic, b"generic file bytes").unwrap();
        view.update_in(visual, |view, window, cx| {
            view.attach_paths(&[generic], None, window, cx)
        });
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert_eq!(view.attachments.prepared.len(), 2)
        });
        let (removed, next) = view.update_in(visual, |view, window, cx| {
            for row in view.draft_attachments().to_vec() {
                view.remove_attachment(&row.id, window, cx);
            }
            assert!(view.attachments.previews.is_empty());
            assert!(view.attachments.expanded_previews.is_empty());
            assert!(view.attachments.prepared.is_empty());
            view.attachments.inflight = None;
            // Remove the currently preparing source before its UI completion;
            // the queued next source must still be prepared afterwards.
            view.attach_paths(&[paths[0].clone()], None, window, cx);
            let removed = view.draft_attachments()[0].id.clone();
            let lease = view.attachments.preparing.clone().unwrap();
            view.remove_attachment(&removed, window, cx);
            view.attach_paths(&[paths[1].clone()], None, window, cx);
            let next = view.draft_attachments()[0].id.clone();
            assert_eq!(view.attachments.preparing.as_ref(), Some(&lease));
            (removed, next)
        });
        visual.run_until_parked();
        view.read_with(visual, |view, _| {
            assert!(!view.attachments.prepared.contains_key(&removed));
            assert!(!view.attachments.previews.contains_key(&removed));
            assert!(view.attachments.previews.contains_key(&next), "positive control: removal released its own preparation and advanced the real local queue");
            assert!(view.attachments.preparing.is_none());
        });
        view.update_in(visual, |view, window, cx| {
            view.generation = view.generation.wrapping_add(1);
            view.attachment_start_next(window, cx);
            assert!(view.attachments.sources.is_empty());
            assert!(view.attachments.prepared.is_empty());
            assert!(view.attachments.previews.is_empty());
            assert!(
                view.attachments.expanded_previews.is_empty(),
                "scope change drops local image material"
            );
        });
        assert!(gateway.sends.lock().unwrap().is_empty());
    }
}
