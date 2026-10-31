//! Native chat attachment drafts. Local sources stay runtime-only; only
//! confirmed server references enter the durable message journal.
use super::*;
use okilum_t3::{
    chat_drafts::{Attachment, AttachmentKind as DraftKind, AttachmentState},
    stream::{
        AttachmentKind, AttachmentMetadata, ChatAttachment, NativeClient, MAX_FILE_BYTES,
        MAX_IMAGE_BYTES, MAX_TOTAL_IMAGE_BYTES,
    },
};
use std::{collections::HashMap, io::Read};

#[derive(Clone, PartialEq, Eq)]
struct Scope {
    origin: String,
    project: String,
    thread: String,
    generation: u64,
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
            Some(vec![PathBuf::from("/missing/notes.txt")])
        });
        visual.run_until_parked();
        view.update_in(visual, |view, window, cx| {
            assert_eq!(view.draft_attachments().len(), 1);
            let id = view.draft_attachments()[0].id.clone();
            assert!(view.prepared_attachments().is_err());
            view.remove_attachment(&id, window, cx);
            assert!(view.draft_attachments().is_empty());
            assert_eq!(
                view.compose.read(cx).value().as_str(),
                "Keep this\nmultiline text"
            );
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
}

#[derive(Clone)]
enum Source {
    File(PathBuf),
    Image(Arc<Vec<u8>>),
}

#[derive(Clone)]
struct Local {
    scope: Scope,
    source: Source,
}

#[derive(Default)]
pub(super) struct Runtime {
    sources: HashMap<String, Local>,
    inflight: Option<(Scope, String)>,
    identity: Option<(std::sync::Weak<NativeClient>, ServerIdentity)>,
    identity_pending: bool,
    identity_failed: bool,
    notice: Option<String>,
    previews: HashMap<String, Arc<gpui::Image>>,
}

impl Runtime {
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }
    pub(super) fn connection_changed(&mut self) {
        self.inflight = None;
        self.identity = None;
        self.identity_pending = false;
        self.identity_failed = false;
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

fn encode_image(bytes: &[u8]) -> anyhow::Result<(String, Vec<u8>)> {
    use image::ImageFormat;
    let format = image::guess_format(bytes)?;
    let mime = match format {
        ImageFormat::Png => Some("image/png"),
        ImageFormat::Jpeg => Some("image/jpeg"),
        ImageFormat::Gif => Some("image/gif"),
        ImageFormat::WebP => Some("image/webp"),
        _ => None,
    };
    if let Some(mime) = mime {
        return Ok((mime.into(), bytes.to_vec()));
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?;
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, ImageFormat::Png)?;
    Ok(("image/png".into(), png.into_inner()))
}

pub(super) fn thumbnail(bytes: &[u8]) -> Option<Arc<gpui::Image>> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().ok()?.thumbnail(640, 480);
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).ok()?;
    Some(Arc::new(gpui::Image::from_bytes(
        gpui::ImageFormat::Png,
        png.into_inner(),
    )))
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
            let mut meta = metadata(name.to_owned());
            if let Ok(format) = image::guess_format(&bytes) {
                let mime = match format {
                    image::ImageFormat::Png => Some("image/png"),
                    image::ImageFormat::Jpeg => Some("image/jpeg"),
                    image::ImageFormat::Gif => Some("image/gif"),
                    image::ImageFormat::WebP => Some("image/webp"),
                    _ => None,
                };
                if let Some(mime) = mime {
                    meta.kind = AttachmentKind::Image;
                    meta.mime_type = mime.into();
                }
            }
            (meta, bytes)
        }
        Source::Image(bytes) => {
            let (mime, bytes) = encode_image(bytes)?;
            (
                AttachmentMetadata {
                    kind: AttachmentKind::Image,
                    name: name.to_owned(),
                    mime_type: mime,
                    size_bytes: 0,
                },
                bytes,
            )
        }
    };
    meta.size_bytes = bytes.len() as u64;
    meta.validate()?;
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
    Ok((meta, bytes))
}

impl ProjectView {
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

    fn attachment_identity(&self) -> Option<&ServerIdentity> {
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
        let result=self.draft_attachments().iter().map(|row|{
            let AttachmentState::Uploaded{attachment_id,environment_id}=&row.state else {anyhow::bail!("Wait for attachments to finish, or retry, reselect or remove the unavailable file");};
            anyhow::ensure!(environment_id==environment,"This file belongs to another T3 environment; select it again");
            Ok(ChatAttachment{id:attachment_id.clone(),metadata:AttachmentMetadata{kind:match row.kind{DraftKind::Image=>AttachmentKind::Image,DraftKind::File=>AttachmentKind::File},name:row.name.clone(),mime_type:row.mime_type.clone(),size_bytes:row.size_bytes},source:row.source.clone()})
        }).collect::<anyhow::Result<Vec<_>>>()?;
        okilum_t3::stream::validate_attachments(&result)?;
        Ok(result)
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
            .map(|p| p.0.read_chat_offer());
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
                    .update(cx, |input, cx| input.insert(text, window, cx));
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
                    .update(cx, |input, cx| input.insert(text, window, cx));
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
        for (index, path) in paths.iter().enumerate() {
            let id = if index == 0 { replace.clone() } else { None }
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
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
                *old = row;
            } else {
                rows.push(row);
            }
            self.attachments.sources.insert(
                id,
                Local {
                    scope: scope.clone(),
                    source: Source::File(path.clone()),
                },
            );
        }
        self.attachments.notice = None;
        self.draft_set_attachments(rows, window, cx);
        self.attachment_start_next(window, cx);
    }

    fn attach_image(&mut self, bytes: Vec<u8>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(scope) = self.attachment_scope() else {
            return;
        };
        if !self.attachment_can_add() {
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
                source: Source::Image(Arc::new(bytes)),
            },
        );
        self.attachments.notice = None;
        self.draft_set_attachments(rows, window, cx);
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
        self.draft_set_attachments(rows, window, cx);
        self.attachment_start_next(window, cx);
    }

    fn remove_attachment(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.attachments.sources.remove(id);
        self.attachments.previews.remove(id);
        let rows = self
            .draft_attachments()
            .iter()
            .filter(|row| row.id != id)
            .cloned()
            .collect();
        self.draft_set_attachments(rows, window, cx);
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
            self.draft_set_attachments(rows, window, cx);
        }
        self.attachment_start_next(window, cx);
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
                        this.draft_set_attachments(rows,window,cx);
                    }
                    Err(_)=>{this.attachments.identity_failed=true;this.attachments.notice=Some("Could not confirm attachment access. Reconnect or retry.".into());}
                }
                cx.notify();
            });
        }).detach();
    }

    pub(super) fn render_native_attachment_drafts(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut tray = v_flex()
            .w_full()
            .gap_1()
            .debug_selector(|| "project-attachment-tray".into());
        for row in self.draft_attachments() {
            if let Some(preview) = self.attachments.previews.get(&row.id) {
                tray = tray.child(
                    gpui::img(preview.clone())
                        .w(px(128.))
                        .h(px(96.))
                        .object_fit(ObjectFit::Contain),
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
                AttachmentState::NeedsReselect => "Select again",
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
                            .text_color(brand::palette(cx).text_muted)
                            .child(status),
                    )
                    .when(needs_action, |line| {
                        line.child(
                            Button::new(SharedString::from(format!("attachment-retry-{id}")))
                                .small()
                                .ghost()
                                .label(if self.has_attachment_source(&id) {
                                    "Retry"
                                } else {
                                    "Select"
                                })
                                .disabled(self.busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.retry_attachment(&retry, window, cx)
                                })),
                        )
                    })
                    .child(
                        Button::new(SharedString::from(format!("attachment-remove-{id}")))
                            .small()
                            .ghost()
                            .label("Remove")
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.remove_attachment(&remove, window, cx)
                            })),
                    ),
            );
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

    pub(super) fn attachment_start_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let retained: std::collections::HashSet<_> = self
            .draft_attachments()
            .iter()
            .map(|row| row.id.clone())
            .collect();
        self.attachments
            .sources
            .retain(|id, _| retained.contains(id));
        self.attachments
            .previews
            .retain(|id, _| retained.contains(id));
        if self.attachments.inflight.is_some()
            || !reader_features::enabled(reader_features::Feature::Agents, cx)
        {
            return;
        }
        let Some(scope) = self.attachment_scope() else {
            return;
        };
        let Some(client) = self.native.model_client().filter(|c| c.can_operate()) else {
            return;
        };
        let next = self.draft_attachments().iter().find_map(|row| {
            if !matches!(row.state, AttachmentState::Pending) {
                return None;
            }
            self.attachments
                .sources
                .get(&row.id)
                .filter(|local| local.scope == scope)
                .map(|local| (row.id.clone(), row.name.clone(), local.source.clone()))
        });
        let Some((id, name, source)) = next else {
            return;
        };
        let other_images = self
            .draft_attachments()
            .iter()
            .filter(|row| row.id != id && row.kind == DraftKind::Image)
            .fold(0_u64, |n, row| n.saturating_add(row.size_bytes));
        let known = self.attachment_identity().cloned();
        self.attachments.inflight = Some((scope.clone(), id.clone()));
        cx.spawn_in(window, async move |this, cx| {
            let worker = client.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let (meta, bytes) = prepare(&source, &name, other_images)?;
                    let identity = match known {
                        Some(identity) => identity,
                        None => worker.server_identity()?,
                    };
                    let preview = (meta.kind == AttachmentKind::Image)
                        .then(|| thumbnail(&bytes))
                        .flatten();
                    let attachment = worker.upload_attachment(&meta, bytes)?;
                    Ok::<_, anyhow::Error>((identity, attachment, preview))
                })
                .await
                .map_err(|error| format!("{error:#}"));
            let cleanup = result.as_ref().ok().map(|(_, asset, _)| asset.id.clone());
            let applied = this
                .update_in(cx, |this, window, cx| {
                    if !this
                        .native
                        .model_client()
                        .is_some_and(|current| Arc::ptr_eq(&current, &client))
                        || this.attachment_scope().as_ref() != Some(&scope)
                        || this.attachments.inflight.as_ref() != Some(&(scope.clone(), id.clone()))
                    {
                        return false;
                    }
                    this.attachments.inflight = None;
                    let mut rows = this.draft_attachments().to_vec();
                    let Some(row) = rows.iter_mut().find(|row| row.id == id) else {
                        this.attachment_start_next(window, cx);
                        return false;
                    };
                    match result {
                        Ok((identity, asset, preview)) => {
                            if let Some(preview) = preview {
                                this.attachments.previews.insert(id.clone(), preview);
                            }
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
                    this.draft_set_attachments(rows, window, cx);
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
