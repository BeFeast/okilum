//! Schema snapshots are independent of ordinary JSON parsing and presentation.
use super::*;
use gpui_component::WindowExt as _;
use okilum_core::json_schema::{Report, Verdict};

impl CodePreview {
    pub(crate) fn source_language(&self) -> &'static str {
        if self.json_routed {
            "json"
        } else {
            language(&self.rel).unwrap_or("text")
        }
    }

    pub(super) fn load_schema(
        &mut self,
        refresh: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(raw) = self.raw_document.clone() else {
            return;
        };
        self.schema_generation = self.schema_generation.wrapping_add(1);
        let generation = self.schema_generation;
        let source = raw.clone();
        self.schema_loading = true;
        self.schema_report = None;
        self.schema_rows = Arc::default();
        if let Some(table) = &self.table {
            table.update(cx, |table, _| table.set_schema(None));
        }
        let association = if refresh {
            None
        } else {
            self.association.clone()
        };
        let (root, path) = (self.root.clone(), self.path.clone());
        #[cfg(test)]
        let gate = self.schema_gate.take();
        let task = cx.background_executor().spawn(async move {
            #[cfg(test)]
            if let Some(gate) = gate { let _ = gate.await; }
            let association = association.unwrap_or_else(|| okilum_core::json_schema::associate(&root, &path));
            let mut report = match &association {
                Ok(association) => okilum_core::json_schema::inspect(association, &raw.text),
                Err(error) => Some(Report::unavailable(None, error.clone())),
            };
            let paths = if report.is_some() { raw.json_pointers() } else { Vec::new() };
            let mut rows = Vec::new();
            let mut bytes = 0usize;
            if let Some(report) = report.as_mut() {
                for path in &paths {
                    let text = report.explanation(path);
                    bytes += text.len();
                    if bytes > okilum_core::json_schema::DOCUMENT_BYTES {
                        report.summary.push_str("; Tree field explanations limited to 4 MiB. See schema details for remaining declarations.");
                        break;
                    }
                    rows.push(text);
                }
            }
            (association, report.map(Arc::new), Arc::new(rows))
        });
        cx.spawn_in(window, async move |this, cx| {
            let (association, report, rows) = task.await;
            let _ = this.update_in(cx, |this, _, cx| {
                // A superseded refresh or source snapshot cannot restore a stale verdict.
                if this.schema_generation != generation
                    || this
                        .raw_document
                        .as_ref()
                        .is_none_or(|raw| !Arc::ptr_eq(raw, &source))
                {
                    return;
                }
                this.association = Some(association);
                this.schema_loading = false;
                this.schema_report = report;
                this.schema_rows = rows;
                if let Some(table) = &this.table {
                    table.update(cx, |table, cx| {
                        table.set_schema(this.schema_report.clone());
                        cx.notify();
                    });
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn schema_action(
        &self,
        entity: Entity<Self>,
    ) -> Option<reader_viewer_chrome::ViewerAction> {
        use reader_viewer_chrome::ViewerAction;
        use std::rc::Rc;
        let report = self.schema_report.clone();
        if report.is_none() && !self.schema_loading {
            return None;
        }
        let label = report
            .as_ref()
            .map_or("Schema: checking…".to_owned(), |report| {
                let state = match report.verdict {
                    Verdict::Valid => "valid",
                    Verdict::Invalid => "violations",
                    Verdict::Incomplete => "incomplete",
                    Verdict::Unavailable => "unavailable",
                    Verdict::SyntaxInvalid => "syntax error",
                };
                let name = report
                    .schema
                    .as_ref()
                    .and_then(|path| path.file_name())
                    .map(|name| name.to_string_lossy())
                    .unwrap_or_default();
                format!("Schema: {name} · {state}")
            });
        let mut action = ViewerAction::new(
            "code-schema",
            Icon::default().path("icons/code-xml.svg"),
            label,
            Rc::new(move |window, cx| entity.update(cx, |this, cx| this.show_schema(window, cx))),
        );
        action.labelled = true;
        action.label_by_default = true;
        action.tooltip = report
            .as_ref()
            .map_or("Checking local schema…".to_owned(), |report| {
                format!(
                    "{}\n{}",
                    report
                        .schema
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "Project viewer associations".into()),
                    report.summary
                )
            })
            .into();
        if self.schema_loading {
            action.enabled = false;
            action.disabled_reason = Some("Checking local schema…".into());
        }
        Some(action)
    }

    pub(super) fn show_schema(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(report) = self.schema_report.clone() else {
            return;
        };
        let target = cx.entity().downgrade();
        let selected = self
            .structure
            .selected
            .and_then(|index| self.schema_rows.get(index))
            .cloned();
        window.open_dialog(cx, move |dialog, _, _| {
            let target = target.clone();
            let heading = report.schema.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "Project viewer associations".into());
            let details = v_flex().id("schema-details").gap_2().max_h(px(440.)).overflow_y_scroll()
                .child(div().child(heading).into_any_element())
                .child(div().child(report.summary.clone()).into_any_element())
                .child(div().child("Read-only snapshot. Syntax diagnostics are separate. No defaults or repairs are applied. Reopen the file after changing viewer associations.").into_any_element())
                .when_some(selected.clone(), |view, selected| view.child(div().child(selected).into_any_element()))
                .children(report.violations.iter().enumerate().map(|(index, issue)| {
                    let location = if issue.path.is_empty() { "/ (root)" } else { &issue.path };
                    div().id(("schema-violation", index)).child(format!("{location}: {}", issue.message)).into_any_element()
                }))
                .child(div().child(format!("Declared fields (showing {} of {}; inspect other fields in Tree or Table):", report.fields.len().min(100), report.fields.len())).into_any_element())
                .children(report.fields.iter().take(100).enumerate().map(|(index, (path, field))| {
                    div().id(("schema-field", index)).child(format!("{}\n{}", if path.is_empty() { "/ (root)" } else { path }, field.explain())).into_any_element()
                }));
            dialog.title("Local JSON Schema").width(px(720.)).child(details.into_any_element())
                .child(Button::new("schema-refresh").label("Reload schema").on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    let _ = target.update(cx, |this, cx| this.load_schema(true, window, cx));
                }))
        });
    }
}
