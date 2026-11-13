//! First text is independent of full-file parsing and formatted snapshots.
use super::*;

impl CodePreview {
    pub(super) fn load_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.raw_requested {
            return;
        }
        self.raw_requested = true;
        self.state = State::Loading;
        let started = self.opened_at;
        let path = self.path.clone();
        let json = is_json(&self.rel);
        let table_path = path.clone();
        let language = language(&self.rel).unwrap_or("text");
        let task = cx.background_executor().spawn(async move {
            let bytes = std::fs::metadata(&path)?.len();
            if bytes > MAX_BYTES {
                return anyhow::Ok(Loaded::TooLarge(bytes));
            }
            Ok(
                match String::from_utf8({
                    use std::io::Read as _;
                    let mut bytes = Vec::new();
                    std::fs::File::open(&path)?
                        .take(MAX_BYTES + 1)
                        .read_to_end(&mut bytes)?;
                    if bytes.len() as u64 > MAX_BYTES {
                        return Ok(Loaded::TooLarge(bytes.len() as u64));
                    }
                    bytes
                }) {
                    Ok(text) => Loaded::Text(text),
                    Err(_) => Loaded::Unsupported,
                },
            )
        });
        crate::source_presentation::code_languages::install();
        // Each selection owns its own entity; a late read cannot reach a newer one.
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.state = match result {
                    Ok(Loaded::Text(text)) => {
                        this.source_bytes = Some(text.len());
                        let range = document::first_page(&text);
                        this.paged = range.end < text.len();
                        this.find.update(cx, |find, cx| {
                            find.set_placeholder(
                                if this.paged || text.len() > EDIT_BYTES {
                                    "Find in whole file"
                                } else {
                                    "Find"
                                },
                                window,
                                cx,
                            )
                        });
                        let input = cx.new(|cx| editor(language, &text[range], window, cx));
                        this.document_revision += 1;
                        this.applied_page = Some((this.document_revision, 0));
                        #[cfg(test)]
                        {
                            this.first_content_ms = Some(started.elapsed().as_millis());
                        }
                        timing(&input, started, cx);
                        this.watch_provisional(&input, cx);
                        this.editor_find = Some(cx.new(|cx| {
                            reader_find_field::editor::EditorTarget::with_query(
                                input.clone(),
                                this.find.clone(),
                                this.case_sensitive.clone(),
                                cx,
                            )
                        }));
                        // Text is available before any full-file parse/index or
                        // formatted snapshot. Those run on subsequent workers.
                        this.prepare_loaded(text, table_path.clone(), json, window, cx);
                        State::Content(input)
                    }
                    Ok(Loaded::TooLarge(bytes)) => State::Message(format!(
                        "Too large to preview. {}, the limit is 64 MB.",
                        view::size(bytes as usize)
                    )),
                    Ok(Loaded::Unsupported) => {
                        State::Message("This file cannot be previewed as text.".into())
                    }
                    Err(_) => State::Message("This file could not be read.".into()),
                };
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn attach_table(
        &mut self,
        table: Entity<reader_delimited::TablePreview>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self._table_subscription =
            Some(cx.observe_in(&table, window, |this, table, window, cx| {
                let (bytes, failure) = {
                    let table = table.read(cx);
                    (
                        table.bytes().and_then(|n| usize::try_from(n).ok()),
                        table.failure().map(str::to_owned),
                    )
                };
                if reader_delimited::eligible(&this.rel) && this.source_bytes != bytes {
                    this.source_bytes = bytes;
                    cx.notify();
                }
                if this.table_failure != failure {
                    this.table_failure = failure;
                    cx.notify();
                }
                if this.table_failure.is_some() && this.view == View::Table {
                    this.apply_view(View::Raw, window, cx);
                    cx.notify();
                }
            }));
        self.table = Some(table);
    }

    /// Observe only the provisional editor. Never forward Input notifications:
    /// blinking/highlighting must not invalidate cached Reader side panels.
    pub(super) fn watch_provisional(
        &mut self,
        input: &Entity<EditorState>,
        cx: &mut Context<Self>,
    ) {
        let state = input.read(cx);
        let initial = (
            state.selected_range(),
            state.scroll_offset(),
            state.search_activation_revision(),
        );
        self.provisional_subscription = Some(cx.observe(input, move |this, input, cx| {
            let state = input.read(cx);
            if (
                state.selected_range(),
                state.scroll_offset(),
                state.search_activation_revision(),
            ) != initial
            {
                this.provisional_interacted.set(true);
            }
        }));
    }

    pub(super) fn initial_view_timing(&self) {
        if std::env::var("OKILUM_CODE_TIMING").as_deref() == Ok("1") {
            diagnostic_error!(
                "code_preview initial_view_ready_ms={} view={:?}",
                self.opened_at.elapsed().as_millis(),
                self.view
            );
        }
    }

    pub(super) fn finish_automatic_view(&mut self) {
        self.automatic_view = false;
        self.provisional_subscription = None;
    }

    pub(super) fn prepare_loaded(
        &mut self,
        text: String,
        table_path: PathBuf,
        json: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let language = language(&self.rel).unwrap_or("text");
        #[cfg(test)]
        let gate = self.preparation_gate.take();
        let task = cx.background_executor().spawn(async move {
            #[cfg(test)]
            if let Some(gate) = gate {
                let _ = gate.await;
            }
            let raw = Arc::new(Document::for_file(text, language, json));
            let refusal = if json && raw.invalid.is_none() {
                okilum_core::json_view::formatting_bound(
                    &raw.text,
                    okilum_core::json_view::MAX_VIEW_BYTES,
                    &|| false,
                )
                .err()
                .map(Err)
            } else {
                None
            };
            (raw, refusal)
        });
        cx.spawn_in(window, async move |this, cx| {
            let (raw, refusal) = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if json
                    && raw.json_tree
                    && raw
                        .text
                        .trim_start_matches('\u{feff}')
                        .trim_start()
                        .starts_with('[')
                {
                    let table = cx.new(|cx| {
                        reader_delimited::TablePreview::new(table_path, window, cx).without_header()
                    });
                    this.attach_table(table, window, cx);
                }
                if json {
                    this.json = Some(JsonText {
                        raw: raw.clone(),
                        pretty: refusal,
                        formatting: false,
                        requested: View::Raw,
                    });
                }
                this.structure.reset(&raw);
                this.raw_document = Some(raw.clone());
                this.document = Some(raw);
                if !this.read_active {
                    return;
                }
                if let Some(input) = this.input() {
                    this.apply_diagnostics(&input, cx);
                }
                if this.automatic_view && !this.provisional_interacted.get() {
                    this.apply_view(this.first_view(cx), window, cx);
                    // Only repair focus if the default actually unmounted its
                    // Input. A user mouse/selection/scroll/Find keeps Raw.
                    if matches!(this.view, View::Tree | View::Table)
                        && this
                            .input()
                            .is_some_and(|input| input.focus_handle(cx).is_focused(window))
                    {
                        this.focus.focus(window, cx);
                    }
                }
                if !this.json.as_ref().is_some_and(|json| json.formatting) {
                    this.initial_view_timing();
                    this.finish_automatic_view();
                }
                this.search(window, cx);
                this.restore_loaded_presentation(window, cx);
                cx.notify();
            });
        })
        .detach();
    }
}
