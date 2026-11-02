//! Local capture composer. Parent mounts the entity; all draft/save IO is serial
//! on the background executor. No reader or Brain protocol is extended.
use super::*;
use gpui_component::input::{Textarea, TextareaState};
use okilum_core::capture::{self, ComposerDraft, CopyPreview, CopySource, Receipt, RecoveryInbox};

/// Emitted after a durable checkpoint when Cancel closes the mounted composer.
pub(crate) struct CaptureClosed;

pub(crate) struct CaptureComposer {
    draft: ComposerDraft,
    text: Entity<TextareaState>,
    folder: Entity<InputState>,
    reader: WeakEntity<Reader>,
    root: PathBuf,
    recovered: Vec<ComposerDraft>,
    dismissed_sources: Vec<capture::DismissedRecovery>,
    dismissed_inspection: Option<usize>,
    unbound: Vec<CopySource>,
    legacy_selected: bool,
    copy_source: Option<CopySource>,
    copy_preview: Option<CopyPreview>,
    copy_folder: Entity<InputState>,
    copy_folder_chosen: bool,
    saved: Option<Receipt>,
    saved_recovery_copy: Option<ComposerDraft>,
    error: Option<String>,
    checkpoint_error: bool,
    busy: bool,
    loading: bool,
    generation: u64,
    persisted: Option<u64>,
    queue: async_channel::Sender<Work>,
    _inputs: Vec<Subscription>,
}
impl EventEmitter<CaptureClosed> for CaptureComposer {}

enum PreviewSource {
    Composer(ComposerDraft),
    Legacy(CopySource),
}

enum Work {
    Recover,
    InspectDismissed,
    Reopen(capture::PresentationRevision),
    Dismiss(ComposerDraft),
    Preview(PreviewSource, PathBuf),
    Checkpoint(ComposerDraft, u64),
    Save(ComposerDraft),
    Close(ComposerDraft),
}

enum Completed {
    Recovery(anyhow::Result<RecoveryInbox>),
    Inspected(anyhow::Result<Vec<capture::DismissedRecovery>>),
    Reopened(anyhow::Result<RecoveryInbox>),
    Dismissed(anyhow::Result<RecoveryInbox>),
    Preview(anyhow::Result<CopyPreview>),
    Checkpoint(u64, anyhow::Result<()>),
    Save(anyhow::Result<Receipt>),
    Close(anyhow::Result<()>),
}

impl CaptureComposer {
    /// `records` is a durable per-vault operational directory outside the index.
    /// Mount only for an explicitly opened vault, using its existing draft store.
    pub(crate) fn new(
        reader: WeakEntity<Reader>,
        root: PathBuf,
        drafts: PathBuf,
        records: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let text = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(4, 12)
                .placeholder("Write a thought to capture…")
        });
        let folder = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Vault-relative folder; blank uses the vault root")
        });
        let copy_folder = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Choose a vault-relative folder, or click Use vault root")
        });
        let copy_folder_input = cx.subscribe(&copy_folder, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) && !this.busy {
                this.copy_folder_chosen = true;
                this.copy_preview = None;
                cx.notify();
            }
        });
        let text_input = cx.subscribe(&text, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change)
                && !this.busy
                && !this.loading
                && !this.legacy_selected
            {
                this.draft.text = this.text.read(cx).value().to_string();
                this.checkpoint(cx);
            }
        });
        let folder_input = cx.subscribe(&folder, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change)
                && !this.busy
                && !this.loading
                && !this.legacy_selected
            {
                this.draft.folder = Some(PathBuf::from(this.folder.read(cx).value().as_str()));
                this.checkpoint(cx);
            }
        });
        let (queue, receiver) = async_channel::unbounded();
        let worker_root = root.clone();
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(work) = receiver.recv().await {
                let root = worker_root.clone();
                let drafts = drafts.clone();
                let records = records.clone();
                let completed = cx.background_executor().spawn(async move {
                    match work {
                        Work::InspectDismissed => Completed::Inspected(capture::inspect_dismissed(&root, &records)),
                        Work::Reopen(revision) => Completed::Reopened(capture::reopen_dismissed(&root, &records, &revision).and_then(|_| capture::load_recovery(&root, &records))),
                        Work::Recover => Completed::Recovery(capture::load_recovery(&root, &records)),
                        Work::Dismiss(copy) => Completed::Dismissed(capture::dismiss_saved_copy(&root, &records, &copy).and_then(|_| capture::load_recovery(&root, &records))),
                        Work::Preview(source, folder) => Completed::Preview((|| {
                            let source = match source {
                                PreviewSource::Composer(draft) => CopySource::from_composer(&root, &records, &draft)?,
                                PreviewSource::Legacy(source) => source,
                            };
                            source.preview(&root, &folder, &records)
                        })()),
                        Work::Checkpoint(draft, generation) => Completed::Checkpoint(generation, draft.persist(&root, &records)),
                        Work::Close(draft) => Completed::Close(draft.persist(&root, &records)),
                        Work::Save(draft) => Completed::Save(draft.publish(&root, &drafts, &records)),
                    }
                }).await;
                let _ = this.update_in(cx, |this, window, cx| {
                    match completed {
                        Completed::Recovery(result) => {
                            this.loading = false;
                            match result {
                                Ok(inbox) => {
                                    this.recovered = inbox.drafts;
                                    this.unbound = inbox.unbound;
                                },
                                Err(error) => this.error = Some(format!("Could not recover capture drafts: {error:#}")),
                            }
                        }
                        Completed::Dismissed(result) => {
                            this.busy = false;
                            match result {
                                Ok(inbox) => {
                                    this.recovered = inbox.drafts;
                                    this.unbound = inbox.unbound;
                                    this.saved_recovery_copy = None;
                                    this.error = None;
                                }
                                Err(error) => this.error = Some(format!("Recovery source was not dismissed: {error:#}")),
                            }
                        }
                        Completed::Inspected(result) => {
                            this.busy = false;
                            match result {
                                Ok(sources) => { this.dismissed_sources = sources; this.dismissed_inspection = None; this.error = None; }
                                Err(error) => this.error = Some(format!("Dismissed recovery inspection failed: {error:#}")),
                            }
                        }
                        Completed::Reopened(result) => {
                            this.busy = false;
                            match result {
                                Ok(inbox) => { this.recovered = inbox.drafts; this.unbound = inbox.unbound; this.dismissed_sources.clear(); this.dismissed_inspection = None; this.error = None; }
                                Err(error) => this.error = Some(format!("Source was not reopened. Inspect again before retrying: {error:#}")),
                            }
                        }
                        Completed::Preview(result) => {
                            this.busy = false;
                            match result {
                                Ok(preview) => {
                                    this.copy_source = Some(preview.source().clone());
                                    this.copy_preview = Some(preview);
                                    this.error = None;
                                }
                                Err(error) => this.error = Some(format!("Copy preview could not be prepared: {error:#}")),
                            }
                        }
                        Completed::Checkpoint(generation, result) => match result {
                            Ok(()) => {
                                this.persisted = Some(generation);
                                if generation == this.generation && this.checkpoint_error {
                                    this.error = None;
                                    this.checkpoint_error = false;
                                }
                            }
                            Err(error) => {
                                this.checkpoint_error = true;
                                this.error = Some(format!("Draft is not safely stored yet. Text is retained here: {error:#}"));
                            },
                        },
                        Completed::Save(result) => {
                            this.busy = false;
                            this.checkpoint_error = false;
                            match result {
                                Ok(receipt) => {
                                    let relative = okilum_core::vault::note_path(&receipt.path);
                                    let root = this.root.clone();
                                    let _ = this.reader.update(cx, |reader, cx| {
                                        if reader.vault_root == root {
                                            Arc::make_mut(&mut reader.vault).register_created_note(&relative);
                                            reader.tree.entry_created(&relative, okilum_core::vault::EntryKind::Markdown);
                                            reader.queue_vault_mutation(okilum_core::Changes {
                                                changed: std::collections::BTreeSet::from([relative]),
                                                ..Default::default()
                                            }, cx);
                                            cx.notify();
                                        }
                                    });
                                    this.saved_recovery_copy = this.draft.copy_origin.as_ref().map(|_| this.draft.clone());
                                    this.saved = Some(receipt);
                                    this.copy_source = None;
                                    this.copy_preview = None;
                                    this.copy_folder_chosen = false;
                                    this.legacy_selected = false;
                                    this.draft = ComposerDraft { folder: this.draft.folder.clone(), ..Default::default() };
                                    this.text.update(cx, |input, cx| input.set_value("", window, cx));
                                    this.error = None;
                                    this.persisted = None;
                                }
                                Err(error) => this.error = Some(format!("Capture was not acknowledged. Retry keeps the same destination and text: {error:#}")),
                            }
                        }
                        Completed::Close(result) => {
                            this.busy = false;
                            this.checkpoint_error = false;
                            match result {
                                Ok(()) => cx.emit(CaptureClosed),
                                Err(error) => this.error = Some(format!("Could not retain the draft. Keep this composer open: {error:#}")),
                            }
                        }
                    }
                    cx.notify();
                });
                // Drain queued checkpoints even if the mounted view was dropped.
            }
        }).detach();
        // Recovery runs before any newly queued checkpoint or save.
        let _ = queue.try_send(Work::Recover);
        Self {
            draft: ComposerDraft {
                folder: Some(PathBuf::new()),
                ..Default::default()
            },
            text,
            folder,
            reader,
            root,
            recovered: Vec::new(),
            dismissed_sources: Vec::new(),
            dismissed_inspection: None,
            unbound: Vec::new(),
            legacy_selected: false,
            copy_source: None,
            copy_preview: None,
            copy_folder,
            copy_folder_chosen: false,
            saved: None,
            saved_recovery_copy: None,
            error: None,
            checkpoint_error: false,
            busy: false,
            loading: true,
            generation: 0,
            persisted: None,
            queue,
            _inputs: vec![text_input, folder_input, copy_folder_input],
        }
    }

    fn checkpoint(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.enqueue(Work::Checkpoint(self.draft.clone(), self.generation), cx);
    }

    fn enqueue(&mut self, work: Work, cx: &mut Context<Self>) {
        if self.queue.try_send(work).is_err() {
            self.busy = false;
            self.error = Some("Capture worker stopped. Your text is retained here; reopen the composer to recover stored drafts.".into());
        }
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            return;
        }
        if !self
            .reader
            .upgrade()
            .is_some_and(|reader| reader.read(cx).vault_root == self.root)
        {
            self.checkpoint_error = false;
            self.error = Some("The owning Reader changed vaults or closed. Reopen this capture vault before saving; your draft is retained.".into());
            cx.notify();
            return;
        }
        if self.legacy_selected {
            self.error = Some("This legacy intent has no vault binding. Choose a copy folder and preview the destination before Save copy.".into());
            cx.notify();
            return;
        }
        if let Err(error) = self.draft.prepare_save() {
            self.checkpoint_error = false;
            self.error = Some(format!("{error:#}"));
            cx.notify();
            return;
        }
        self.busy = true;
        self.enqueue(Work::Save(self.draft.clone()), cx);
    }

    fn restore(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || !self.draft.text.is_empty() || self.draft.attempt.is_some() {
            return;
        }
        self.copy_source = None;
        self.copy_preview = None;
        self.copy_folder_chosen = false;
        self.legacy_selected = false;
        self.draft = self.recovered.remove(index);
        // Suppress input checkpoints while installing one recovered snapshot.
        self.busy = true;
        self.text.update(cx, |input, cx| {
            input.set_value(self.draft.text.clone(), window, cx)
        });
        self.folder.update(cx, |input, cx| {
            input.set_value(
                self.draft
                    .folder
                    .as_deref()
                    .unwrap_or(Path::new(""))
                    .to_string_lossy()
                    .to_string(),
                window,
                cx,
            )
        });
        self.busy = false;
        self.error = None;
        self.checkpoint(cx);
    }

    fn select_legacy(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || !self.draft.text.is_empty() || self.draft.attempt.is_some() {
            return;
        }
        let source = self.unbound[index].clone();
        self.busy = true;
        self.text.update(cx, |input, cx| {
            input.set_value(source.text().to_owned(), window, cx)
        });
        self.draft.text = source.text().to_owned();
        self.draft.folder = None;
        self.copy_source = Some(source);
        self.copy_preview = None;
        self.copy_folder_chosen = false;
        self.legacy_selected = true;
        self.busy = false;
        self.error = None;
        cx.notify();
    }

    fn preview_copy(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.loading || !self.copy_folder_chosen {
            return;
        }
        let source = if self.legacy_selected {
            let Some(source) = self.copy_source.clone() else {
                return;
            };
            PreviewSource::Legacy(source)
        } else {
            if self.draft.attempt.is_none() {
                return;
            }
            PreviewSource::Composer(self.draft.clone())
        };
        self.busy = true;
        self.copy_preview = None;
        self.enqueue(
            Work::Preview(
                source,
                PathBuf::from(self.copy_folder.read(cx).value().as_str()),
            ),
            cx,
        );
    }

    fn save_copy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            return;
        }
        if !self
            .reader
            .upgrade()
            .is_some_and(|reader| reader.read(cx).vault_root == self.root)
        {
            self.error = Some(
                "The selected vault changed. Reopen the capture vault and preview again.".into(),
            );
            cx.notify();
            return;
        }
        let Some(preview) = &self.copy_preview else {
            return;
        };
        match preview.start_copy() {
            Ok(draft) => {
                // This explicit click alone creates a new operation. The original
                // draft and intent keep their bytes and their operation identity.
                self.busy = true;
                self.persisted = None;
                self.generation += 1;
                self.draft = draft;
                self.legacy_selected = false;
                self.folder.update(cx, |input, cx| {
                    input.set_value(
                        self.draft
                            .folder
                            .as_ref()
                            .unwrap()
                            .to_string_lossy()
                            .to_string(),
                        window,
                        cx,
                    )
                });
                self.copy_preview = None;
                self.copy_source = None;
                self.copy_folder_chosen = false;
                self.enqueue(Work::Save(self.draft.clone()), cx);
            }
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                cx.notify();
            }
        }
    }

    fn open_saved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(receipt) = &self.saved else { return };
        self.checkpoint_error = false;
        let relative = okilum_core::vault::note_path(&receipt.path);
        let root = self.root.clone();
        match self.reader.update(cx, |reader, cx| {
            if reader.vault_root != root {
                return Err(anyhow::anyhow!("The owning Reader has switched vaults. Reopen the capture vault to open this note."));
            }
            reader.open_note(&relative, None, window, cx);
            Ok(())
        }) {
            Ok(Ok(())) => self.error = None,
            Ok(Err(error)) => self.error = Some(error.to_string()),
            Err(_) => self.error = Some("The owning Reader was closed. Your saved capture remains in its vault.".into()),
        }
        cx.notify();
    }
}

impl Render for CaptureComposer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let locked =
            self.busy || self.loading || self.draft.attempt.is_some() || self.legacy_selected;
        let mut view = v_flex()
            .w_full()
            .gap_2()
            .p_4()
            .child(div().child("Capture a note"))
            .child(div().child(format!("Vault: {}", self.root.display())))
            .child(div().child("Capture folder (relative to this vault)"))
            .child(Input::new(&self.folder).disabled(locked))
            .child(Textarea::new(&self.text).disabled(locked));
        if self.loading {
            view = view.child("Recovering saved drafts…");
        } else if self.legacy_selected {
            view = view.child("Legacy intent retained. Its original vault is unknown. Choose a destination and use the previewed Save copy action to create a separate note.");
        } else if self.persisted == Some(self.generation) {
            view = view.child("Draft stored on this device");
        } else if !self.draft.text.is_empty() {
            view = view.child("Storing draft…");
        }
        if let Some(error) = &self.error {
            view = view.child(div().text_color(cx.theme().danger).child(error.clone()));
        }
        if let Some(receipt) = &self.saved {
            view =
                view.child(
                    h_flex()
                        .gap_2()
                        .child(format!("Saved: {}", receipt.path.display()))
                        .child(Button::new("capture-open").label("Open note").on_click(
                            cx.listener(|this, _, window, cx| this.open_saved(window, cx)),
                        )),
                );
        }
        view = view.child(
            Button::new("capture-inspect-dismissed")
                .label("Inspect dismissed recovery sources")
                .disabled(self.busy || self.loading)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.busy = true;
                    this.enqueue(Work::InspectDismissed, cx);
                })),
        );
        for (index, source) in self.dismissed_sources.iter().enumerate() {
            view = view.child(
                Button::new(("capture-dismissed-source", index))
                    .label(format!("Inspect: {}", source.original_path.display()))
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.dismissed_inspection = Some(index);
                        cx.notify();
                    })),
            );
        }
        if let Some(source) = self
            .dismissed_inspection
            .and_then(|index| self.dismissed_sources.get(index))
        {
            let revision = source.revision.clone();
            view = view.child(v_flex().gap_2()
                .child(format!("Original: {} · Saved copy: {}", source.original_path.display(), source.saved_path.display()))
                .child(source.text.clone())
                .children(source.assets.iter().map(|asset| div().child(format!("{} · {} · {} bytes · SHA256 {}", asset.asset.id, asset.asset.display_name, asset.asset.byte_length, asset.asset.sha256))))
                .child("Reopen restores this source to recovery choices. It does not publish a note or remove files or staged attachments.")
                .child(Button::new("capture-reopen-dismissed").label("Reopen recovery source").disabled(self.busy)
                    .on_click(cx.listener(move |this, _, _, cx| { this.busy = true; this.enqueue(Work::Reopen(revision.clone()), cx); })))
                .child(Button::new("capture-close-inspection").label("Close inspection").disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| { this.dismissed_inspection = None; cx.notify(); }))));
        }
        if self.saved_recovery_copy.is_some() {
            view = view.child(
                Button::new("capture-dismiss-saved-source")
                    .label("Dismiss original recovery source — keep all files")
                    .disabled(self.busy || self.loading)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(copy) = this.saved_recovery_copy.clone() {
                            this.busy = true;
                            this.enqueue(Work::Dismiss(copy), cx);
                        }
                    })),
            );
        }
        for (index, draft) in self.recovered.iter().enumerate() {
            let preview: String = draft.text.chars().take(60).collect();
            view = view.child(
                Button::new(("capture-recover", index))
                    .label(format!("Recover draft: {preview}"))
                    .disabled(
                        self.busy || !self.draft.text.is_empty() || self.draft.attempt.is_some(),
                    )
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.restore(index, window, cx)),
                    ),
            );
        }
        for (index, source) in self.unbound.iter().enumerate() {
            view = view.child(
                Button::new(("capture-legacy", index))
                    .label(format!(
                        "Inspect unbound legacy intent: {}",
                        source.original_path().display()
                    ))
                    .disabled(
                        self.busy || !self.draft.text.is_empty() || self.draft.attempt.is_some(),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_legacy(index, window, cx)
                    })),
            );
        }
        if self.draft.attempt.is_some() || self.legacy_selected {
            view = view.child(div().child("Save a separate copy: choose its destination explicitly. The original note and intent are retained."))
                .child(Input::new(&self.copy_folder).disabled(self.busy))
                .child(h_flex().gap_2()
                    .child(Button::new("capture-copy-root").label("Use vault root").disabled(self.busy)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.copy_folder.update(cx, |input, cx| input.set_value("", window, cx));
                            this.copy_folder_chosen = true;
                            this.copy_preview = None;
                            cx.notify();
                        })))
                    .child(Button::new("capture-preview-copy").label("Preview copy in this vault")
                        .disabled(self.busy || !self.copy_folder_chosen)
                        .on_click(cx.listener(|this, _, _, cx| this.preview_copy(cx)))));
        }
        if let Some(preview) = &self.copy_preview {
            let origin = preview
                .source()
                .bound_root()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "Unknown — legacy intent has no vault binding".into());
            view = view.child(v_flex().gap_2()
                .child(format!("Original vault: {origin}"))
                .child(format!("Recorded original path: {}", preview.source().original_path().display()))
                .child(format!("Selected copy vault: {}", preview.root().display()))
                .child(format!("Selected copy folder: {}", if preview.folder().as_os_str().is_empty() { "(vault root)".into() } else { preview.folder().display().to_string() }))
                .child("Save copy will allocate a new Capture-<UUID>.md filename. The exact captured text shown above will be copied; existing notes will not be replaced.")
                .child(Button::new("capture-confirm-copy").label("Save copy").disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| this.save_copy(window, cx))))
                .child(Button::new("capture-dismiss-preview").label("Cancel preview").disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| { this.copy_preview = None; cx.notify(); }))));
        }
        view.child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("capture-cancel")
                        .label("Cancel — keep draft")
                        .disabled(self.busy || self.loading)
                        .on_click(cx.listener(|this, _, _, cx| {
                            if this.legacy_selected {
                                cx.emit(CaptureClosed);
                                return;
                            }
                            this.busy = true;
                            this.enqueue(Work::Close(this.draft.clone()), cx);
                        })),
                )
                .child(
                    Button::new("capture-save")
                        .label(if self.busy {
                            "Saving…"
                        } else if self.draft.attempt.is_some() {
                            "Retry save"
                        } else {
                            "Save capture"
                        })
                        .disabled(
                            self.busy
                                || self.loading
                                || self.legacy_selected
                                || self.draft.text.trim().is_empty(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                ),
        )
    }
}
