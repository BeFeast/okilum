//! First text is independent of full-file parsing and formatted snapshots.
use super::*;

impl CodePreview {
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
                this.apply_view(this.first_view(cx), window, cx);
                this.search(window, cx);
                cx.notify();
            });
        })
        .detach();
    }
}
