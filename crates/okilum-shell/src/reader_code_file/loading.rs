//! First text is independent of full-file parsing and formatted snapshots.
use super::*;

impl CodePreview {
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
                    this.table =
                        Some(cx.new(|cx| reader_delimited::TablePreview::new(table_path, cx)));
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
                cx.notify();
            });
        })
        .detach();
    }
}
