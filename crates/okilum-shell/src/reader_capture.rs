//! Local capture composer. Parent mounts the entity; all draft/save IO is serial
//! on the background executor. No reader or Brain protocol is extended.
use super::*;
use gpui_component::input::{Textarea, TextareaState};
use okilum_core::capture::{self, ComposerDraft, Receipt};

/// Emitted after a durable checkpoint when Cancel closes the mounted composer.
pub(crate) struct CaptureClosed;

pub(crate) struct CaptureComposer {
    draft: ComposerDraft,
    text: Entity<TextareaState>,
    folder: Entity<InputState>,
    reader: WeakEntity<Reader>,
    root: PathBuf,
    recovered: Vec<ComposerDraft>,
    saved: Option<Receipt>,
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

enum Work {
    Recover,
    Checkpoint(ComposerDraft, u64),
    Save(ComposerDraft),
    Close(ComposerDraft),
}

enum Completed {
    Recovery(anyhow::Result<Vec<ComposerDraft>>),
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
        let text_input = cx.subscribe(&text, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) && !this.busy && !this.loading {
                this.draft.text = this.text.read(cx).value().to_string();
                this.checkpoint(cx);
            }
        });
        let folder_input = cx.subscribe(&folder, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) && !this.busy && !this.loading {
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
                        Work::Recover => Completed::Recovery(capture::recover_composers(&root, &records)),
                        Work::Checkpoint(draft, generation) => Completed::Checkpoint(generation, draft.persist(&root, &records)),
                        Work::Close(draft) => Completed::Close(draft.persist(&root, &records)),
                        Work::Save(draft) => Completed::Save((|| {
                            // Persist operation identity before attempting any canonical IO.
                            draft.persist(&root, &records)?;
                            draft.attempt.as_ref().ok_or_else(|| anyhow::anyhow!("Capture has no prepared operation"))?.publish(&root, &drafts, &records)
                        })()),
                    }
                }).await;
                let _ = this.update_in(cx, |this, window, cx| {
                    match completed {
                        Completed::Recovery(result) => {
                            this.loading = false;
                            match result {
                                Ok(drafts) => this.recovered = drafts,
                                Err(error) => this.error = Some(format!("Could not recover capture drafts: {error:#}")),
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
                                    this.saved = Some(receipt);
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
            saved: None,
            error: None,
            checkpoint_error: false,
            busy: false,
            loading: true,
            generation: 0,
            persisted: None,
            queue,
            _inputs: vec![text_input, folder_input],
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
        let locked = self.busy || self.loading || self.draft.attempt.is_some();
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
        view.child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("capture-cancel")
                        .label("Cancel — keep draft")
                        .disabled(self.busy || self.loading)
                        .on_click(cx.listener(|this, _, _, cx| {
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
                        .disabled(self.busy || self.loading || self.draft.text.trim().is_empty())
                        .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                ),
        )
    }
}
