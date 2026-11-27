//! Schema snapshots are independent of ordinary JSON parsing and presentation.
use super::*;
use gpui_component::WindowExt as _;
use okilum_core::json_schema::{Report, Verdict};

impl CodePreview {
    pub fn source_language(&self) -> &'static str {
        if self.json_routed {
            "json"
        } else {
            language(&self.rel).unwrap_or("text")
        }
    }

    pub fn load_schema(&mut self, refresh: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(raw) = self.raw_document.clone() else {
            return;
        };
        self.schema_generation = self.schema_generation.wrapping_add(1);
        let generation = self.schema_generation;
        let source = raw.clone();
        self.schema_loading = true;
        self.schema_report = None;
        self.schema_rows = Arc::default();
        self.schema_pointers = Arc::default();
        if let Some(table) = &self.table {
            table.update(cx, |table, _| table.set_schema(None));
        }
        let association = if refresh {
            None
        } else {
            self.association.clone()
        };
        let (root, path) = (self.root.clone(), self.path.clone());
        #[cfg(any(test, feature = "test-support"))]
        let gate = self.schema_gate.take();
        let task = cx.background_executor().spawn(async move {
            #[cfg(any(test, feature = "test-support"))]
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
            (association, report.map(Arc::new), Arc::new(rows), Arc::new(paths))
        });
        cx.spawn_in(window, async move |this, cx| {
            let (association, report, rows, paths) = task.await;
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
                this.schema_pointers = paths;
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

    pub fn schema_action(
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

    pub fn schema_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let report = self.schema_report.as_ref();
        if report.is_none() && !self.schema_loading {
            return None;
        }
        let palette = brand::palette(cx);
        let faint = brand::reader_palette(cx).text_faint;
        let (schema_label, verdict_label, verdict_color, detail_hint) = match report {
            None => (
                "Schema: checking…".to_owned(),
                String::new(),
                faint,
                String::new(),
            ),
            Some(report) => {
                let name = report
                    .schema
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "local schema".to_owned());
                let (badge, color) = match report.verdict {
                    Verdict::Valid => ("Valid".to_owned(), cx.theme().foreground),
                    Verdict::Invalid => (
                        if report.violations.len() == 1 {
                            "1 schema violation".to_owned()
                        } else {
                            format!("{} schema violations", report.violations.len())
                        },
                        palette.warning,
                    ),
                    Verdict::Incomplete => ("Incomplete".to_owned(), palette.warning),
                    Verdict::Unavailable => ("Unavailable".to_owned(), palette.warning),
                    Verdict::SyntaxInvalid => (
                        "Syntax error (schema check skipped)".to_owned(),
                        palette.warning,
                    ),
                };
                let hint = if let Some(first) = report.violations.first() {
                    let loc = if first.path.is_empty() {
                        "/ (root)"
                    } else {
                        &first.path
                    };
                    format!("{loc}: {}", first.message)
                } else {
                    report.summary.clone()
                };
                (format!("Schema: {name}"), badge, color, hint)
            }
        };
        let entity = cx.entity();
        let enabled = !self.schema_loading && self.schema_report.is_some();
        Some(
            h_flex()
                .id("code-schema-bar")
                .debug_selector(|| "code-schema-bar".into())
                .w_full()
                .flex_none()
                .px(px(READER_SIDE_PADDING))
                .py_1()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(palette.border_subtle)
                .text_xs()
                .child(
                    div()
                        .flex_none()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(cx.theme().foreground)
                        .child(schema_label),
                )
                .when(!verdict_label.is_empty(), |bar| {
                    bar.child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(verdict_color)
                            .child(format!("· {verdict_label}")),
                    )
                })
                .when(!detail_hint.is_empty(), |bar| {
                    bar.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(faint)
                            .child(detail_hint),
                    )
                })
                .when(enabled, |bar| {
                    bar.child(
                        Button::new("code-schema-bar-details")
                            .small()
                            .ghost()
                            .label("Details")
                            .on_click(move |_, window, cx| {
                                entity.update(cx, |this, cx| this.show_schema(window, cx));
                            }),
                    )
                })
                .into_any_element(),
        )
    }

    pub fn tree_row_schema_hint(&self, index: usize) -> Option<(String, bool)> {
        let report = self.schema_report.as_ref()?;
        let pointer = self.schema_pointers.get(index)?;
        if let Some(issue) = report.violations.iter().find(|v| v.path == *pointer) {
            return Some((format!("Schema violation: {}", issue.message), true));
        }
        let field = report.fields.get(pointer)?;
        let mut parts = Vec::new();
        if let Some(title) = &field.title {
            parts.push(title.clone());
        } else if let Some(description) = &field.description {
            parts.push(description.clone());
        }
        if !field.types.is_empty() {
            parts.push(field.types.join(" | "));
        }
        if field.property {
            parts.push(if field.required {
                "required".into()
            } else {
                "optional".into()
            });
        }
        if let Some(enumeration) = &field.enumeration {
            parts.push(format!("enum {enumeration}"));
        }
        if parts.is_empty() {
            None
        } else {
            Some((parts.join(" · "), false))
        }
    }

    pub fn schema_field_inspector(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let report = self.schema_report.as_ref()?;
        let palette = brand::palette(cx);
        let faint = brand::reader_palette(cx).text_faint;
        let selected_pointer = self
            .structure
            .selected
            .and_then(|index| self.schema_pointers.get(index))
            .map(String::as_str);

        let mut lines: Vec<(String, Hsla, FontWeight)> = Vec::new();
        if let Some(pointer) = selected_pointer {
            let display_path = if pointer.is_empty() {
                "/ (root)"
            } else {
                pointer
            };
            if let Some(field) = report.fields.get(pointer) {
                let mut header = display_path.to_owned();
                if let Some(title) = &field.title {
                    header.push_str(&format!(" — {title}"));
                }
                let mut meta = Vec::new();
                if !field.types.is_empty() {
                    meta.push(format!("type: {}", field.types.join(" | ")));
                }
                if field.property {
                    meta.push(if field.required {
                        "required".into()
                    } else {
                        "optional".into()
                    });
                }
                if let Some(enumeration) = &field.enumeration {
                    meta.push(format!("enum: {enumeration}"));
                }
                if !meta.is_empty() {
                    header.push_str(&format!(" ({})", meta.join(", ")));
                }
                lines.push((header, cx.theme().foreground, FontWeight::MEDIUM));
                if let Some(description) = &field.description {
                    lines.push((description.clone(), faint, FontWeight::NORMAL));
                }
            } else {
                lines.push((
                    format!("{display_path} — No schema field declaration available"),
                    faint,
                    FontWeight::NORMAL,
                ));
            }
            for issue in report.violations.iter().filter(|v| v.path == pointer) {
                lines.push((
                    format!("Schema violation: {}", issue.message),
                    palette.warning,
                    FontWeight::MEDIUM,
                ));
            }
        } else {
            let root_title = report
                .fields
                .get("")
                .and_then(|f| f.title.as_deref())
                .unwrap_or("Local JSON Schema");
            let root_desc = report.fields.get("").and_then(|f| f.description.as_deref());
            let mut heading = root_title.to_owned();
            if let Some(desc) = root_desc {
                heading.push_str(&format!(" — {desc}"));
            }
            lines.push((heading, cx.theme().foreground, FontWeight::MEDIUM));
            if let Some(first) = report.violations.first() {
                let loc = if first.path.is_empty() {
                    "/ (root)"
                } else {
                    &first.path
                };
                lines.push((
                    format!("Schema violation at {loc}: {}", first.message),
                    palette.warning,
                    FontWeight::MEDIUM,
                ));
            }
        }

        Some(
            v_flex()
                .id("code-schema-field-inspector")
                .debug_selector(|| "code-schema-field-inspector".into())
                .w_full()
                .flex_none()
                .px_3()
                .py_2()
                .gap_1()
                .border_t_1()
                .border_color(palette.border_subtle)
                .text_xs()
                .children(lines.into_iter().map(|(text, color, weight)| {
                    div()
                        .w_full()
                        .truncate()
                        .text_color(color)
                        .font_weight(weight)
                        .child(text)
                }))
                .into_any_element(),
        )
    }

    pub fn show_schema(&self, window: &mut Window, cx: &mut Context<Self>) {
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
