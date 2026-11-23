use super::*;

// Shared-session publications can replace the inventory without advancing this
// Reader's local watcher counter. Both identities must stay current.
pub(super) fn agent_publication_is_current<T>(
    captured: &Arc<T>,
    current: &Arc<T>,
    captured_generation: u64,
    current_generation: u64,
) -> bool {
    Arc::ptr_eq(captured, current) && captured_generation == current_generation
}

impl Reader {
    /// Event-driven from normal Reader/editor notifications and reconciled vault
    /// publications. No timer, extra watcher, remote gateway or source writes.
    pub(super) fn sync_agent_document(&mut self, cx: &mut Context<Self>) {
        let enabled = reader_features::enabled(reader_features::Feature::Agents, cx)
            && !self.single_file
            && okilum_core::agent_imports::is_agent_document(&self.pane.current_rel);
        let source = enabled
            .then(|| {
                self.source_text(cx)
                    .map(Arc::<str>::from)
                    .or_else(|| self.pane.note_canonical_source.clone())
            })
            .flatten();
        let Some(source) = source else {
            self.agent_document = None;
            if let Some(controller) = &mut self.agent_language {
                controller.retire();
            }
            self.sync_agent_language_marks(cx);
            return;
        };
        if self.agent_document.as_ref().is_some_and(|current| {
            current.root == self.vault_root
                && current.path == self.pane.current_rel
                && current.source == source
                && agent_publication_is_current(
                    &current.inventory_source,
                    &self.vault,
                    current.inventory,
                    self.watcher_generation,
                )
        }) {
            // Edit mode may open on an already checked source: mark it too.
            self.sync_agent_language_marks(cx);
            return;
        }
        // Retire immediately, before loading the next policy or preparing links.
        if let Some(controller) = &mut self.agent_language {
            controller.retire();
        }
        if self.agent_language.is_none() {
            let (send, receive) = async_channel::bounded(1);
            match reader_agent_language::Controller::new(move || {
                let _ = send.try_send(());
            }) {
                Ok(controller) => {
                    self.agent_language = Some(controller);
                    self._agent_language_events = Some(cx.spawn(async move |this, cx| {
                        while receive.recv().await.is_ok() {
                            if this
                                .update(cx, |this, cx| {
                                    // Captures can change before their next paint. Never apply
                                    // completion ranges to a newer editor or policy generation.
                                    this.sync_agent_document(cx);
                                    if this
                                        .agent_language
                                        .as_mut()
                                        .is_some_and(|c| c.apply_ready())
                                    {
                                        this.sync_agent_language_marks(cx);
                                        cx.notify();
                                    }
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    }));
                }
                Err(error) => {
                    diagnostic_error!("Agent language worker unavailable: {error}");
                }
            }
        }
        let root = self.vault_root.clone();
        let path = self.pane.current_rel.clone();
        let expanded = self.agent_document.as_ref().is_some_and(|current| {
            current.root == root && current.path == path && current.imports.expanded()
        });
        let mut imports = reader_agent_imports::LiveImportView::new(
            root.clone(),
            root.join(&path),
            source.to_string(),
            Default::default(),
        );
        imports.set_expanded(expanded);
        let token = Arc::new(());
        let (expanded_lines, expand_all) = self
            .agent_document
            .as_ref()
            .filter(|current| {
                current.root == root && current.path == path && current.source == source
            })
            .map(|current| (current.expanded_lines.clone(), current.expand_all))
            .unwrap_or_default();
        self.agent_document = Some(AgentDocument {
            root: root.clone(),
            path: path.clone(),
            source: source.clone(),
            inventory: self.watcher_generation,
            inventory_source: self.vault.clone(),
            token: token.clone(),
            imports,
            expanded_lines,
            expand_all,
            imported_documents: Default::default(),
            notice: None,
            language_error: self
                .agent_language
                .is_none()
                .then(|| "Language check unavailable: local worker could not start".into()),
            _preparation: None,
        });
        self.prepare_agent_document(cx);
    }

    /// Mirror the published matches into the source editor as wavy marks.
    /// Marks follow text edits, so a capture older than the editor text keeps
    /// the current marks until its replacement arrives; no source is changed.
    fn sync_agent_language_marks(&mut self, cx: &mut Context<Self>) {
        self.sync_hover_revision(cx);
        let input = self.source_input();
        let published = self.agent_language.as_ref().and_then(|c| c.published());
        // Key on the publication, never the source: a policy-only refresh
        // publishes new matches for the same captured source `Arc` (#1593).
        let key = input
            .as_ref()
            .zip(self.agent_language.as_ref().and_then(|c| c.publication()))
            .map(|(input, publication)| (input.entity_id(), publication));
        let Some(input) = input else {
            self.agent_language_marks = None;
            return;
        };
        if let Some(marks) = &self.agent_language_marks {
            if marks.input != input.entity_id() {
                self.agent_language_marks = None;
            } else if key.is_some() && marks.capture == key.map(|k| k.1) {
                return;
            }
        }
        let ranges = match published {
            // A check of this agent file is still running: keep the marks,
            // which follow edits, until its result replaces them.
            None if self.agent_document.is_some() => return,
            None => Vec::new(),
            Some(published) => {
                if !reader_agent_language::same_text(
                    &published.snapshot.source,
                    input.read(cx).text(),
                ) {
                    return;
                }
                reader_agent_language::mark_ranges(&published.report)
            }
        };
        let style = reader_agent_language::mark_style(cx.theme().warning);
        let decorations: Vec<_> = ranges
            .into_iter()
            .map(|range| gpui_component::input::TextDecoration::new(range, style))
            .collect();
        match &mut self.agent_language_marks {
            Some(marks) => {
                marks.collection.set(decorations, cx);
                marks.capture = key.map(|k| k.1);
            }
            None => {
                if decorations.is_empty() {
                    return;
                }
                let collection = input.update(cx, |input, cx| {
                    input.create_decorations_collection(decorations, cx)
                });
                self.agent_language_marks = Some(AgentLanguageMarks {
                    input: input.entity_id(),
                    capture: key.map(|k| k.1),
                    collection,
                });
            }
        }
        // Marked terms hover in Live as in Source (#2953).
        self.sync_live_language_hover(cx);
    }

    /// The native hover keeps its popup and skips the provider while the
    /// pointer stays in the cached range, so it would bypass the published
    /// text guard. Clear it whenever the source revision or the language
    /// publication (the key the marks use) changes, in Source and Live (#2953).
    pub(super) fn sync_hover_revision(&mut self, cx: &mut Context<Self>) {
        let key = self.source_input().map(|input| {
            (
                input.entity_id(),
                input.read(cx).source_stamp(),
                self.agent_language.as_ref().and_then(|c| c.publication()),
            )
        });
        if self.hover_revision == key {
            return;
        }
        self.hover_revision = key;
        if let Some(input) = self.source_input() {
            input.update(cx, |input, cx| input.clear_hover_state(cx));
        }
    }

    /// The vault's language policy changed on disk: re-read it for the open
    /// agent file, so its diagnostics refresh without a source edit.
    pub(super) fn agent_language_policy_changed(&mut self, cx: &mut Context<Self>) {
        if self.agent_document.is_some() {
            self.prepare_agent_document(cx);
        }
    }

    pub(super) fn prepare_agent_document(&mut self, cx: &mut Context<Self>) {
        let Some(current) = &mut self.agent_document else {
            return;
        };
        let root = current.root.clone();
        let path = current.path.clone();
        let source = current.source.clone();
        let token = current.token.clone();
        let preparation = current.imports.preparation();
        let vault = self.vault.clone();
        current.notice = None;
        current._preparation = Some(cx.spawn(async move |this, cx| {
            let read_root = root.clone();
            let read_source = source.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    use okilum_core::agent_imports::Source as _;
                    use sha2::{Digest, Sha256};
                    let source_revision = format!("{:x}", Sha256::digest(read_source.as_bytes()));
                    let store = okilum_core::agent_imports::RootedSource::new(&read_root);
                    let policy: Result<Arc<str>, Arc<str>> = match &store {
                        Ok(store) => store
                            .canonicalize(&store.root().join(reader_agent_language::POLICY_PATH))
                            .and_then(|path| {
                                store.read(&path, okilum_core::agent_language::MAX_SCHEMA_BYTES)
                            })
                            .map(Arc::from)
                            .map_err(|failure| Arc::from(format!("{failure:?}"))),
                        Err(failure) => Err(Arc::from(format!("{failure:?}"))),
                    };
                    let policy_bytes = match &policy {
                        Ok(text) => text.as_bytes(),
                        Err(reason) => reason.as_bytes(),
                    };
                    let schema_revision = format!(
                        "{}:{:x}",
                        if policy.is_ok() { "ok" } else { "error" },
                        Sha256::digest(policy_bytes)
                    );
                    let prepared = store
                        .as_ref()
                        .map(|store| preparation.run(store))
                        .map_err(|failure| format!("{failure:?}"));
                    let documents = prepared
                        .as_ref()
                        .ok()
                        .map(|prepared| {
                            reader_document_decorations::prepare_imported_documents(
                                &vault,
                                &read_root,
                                prepared.tree(),
                            )
                        })
                        .unwrap_or_default();
                    (
                        prepared,
                        policy,
                        source_revision,
                        schema_revision,
                        documents,
                    )
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.sync_agent_document(cx);
                let Some(current) = &mut this.agent_document else {
                    return;
                };
                if !Arc::ptr_eq(&current.token, &token) {
                    return;
                }
                match result.0 {
                    Ok(prepared) => match current.imports.apply_prepared(prepared) {
                        Ok(true) => current.imported_documents = result.4,
                        Ok(false) => return,
                        Err(error) => {
                            current.notice = Some(format!("Imports unavailable: {error:?}"))
                        }
                    },
                    Err(error) => current.notice = Some(format!("Imports unavailable: {error}")),
                }
                if let Some(controller) = &mut this.agent_language {
                    controller.observe(reader_agent_language::Snapshot {
                        document: format!("{:?}", root.join(&path)),
                        path,
                        source_revision: result.2,
                        schema_revision: result.3,
                        source,
                        policy: result.1,
                    });
                }
                cx.notify();
            });
        }));
    }

    pub(super) fn render_agent_document(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.project_navigation.board_visible
            || self.project_navigation.section != reader_project_navigation::Section::Reader
            || (self.project_open && self.project_navigation.active.is_none())
        {
            return None;
        }
        let current = self.agent_document.as_ref()?;
        let token = current.token.clone();
        let expanded = current.expand_all;
        let mut panel = v_flex()
            .id("agent-document-context")
            .debug_selector(|| "agent-document-context".into())
            .w_full()
            .max_h(px(160.))
            .overflow_y_scroll()
            .border_t_1()
            .border_color(cx.theme().border)
            .px_3()
            .py_1()
            .gap_1()
            .child(
                h_flex().gap_2().child("Agent file").child(
                    Button::new("agent-expand-imports")
                        .ghost()
                        .small()
                        .label(if expanded {
                            "Collapse imports"
                        } else {
                            "Expand imports"
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.sync_agent_document(cx);
                            let Some(current) = &mut this.agent_document else {
                                return;
                            };
                            if !Arc::ptr_eq(&current.token, &token) {
                                return;
                            }
                            current.expand_all = !expanded;
                            current.expanded_lines.clear();
                            current.imports.set_expanded(!expanded);
                            this.prepare_agent_document(cx);
                            cx.notify();
                        })),
                ),
            );
        if let Some(notice) = &current.notice {
            panel = panel.child(notice.clone());
        }
        for (index, link) in current.imports.links().iter().enumerate() {
            let label = match &link.destination {
                Ok(path) => format!("@{} → {}", link.import.target, path.display()),
                Err(error) => format!("@{} — {error:?}", link.import.target),
            };
            let token = current.token.clone();
            let captured_link = link.clone();
            panel = panel.child(
                Button::new(("agent-import-open", index))
                    .ghost()
                    .small()
                    .label(label)
                    .disabled(link.destination.is_err())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_agent_import(token.clone(), captured_link.clone(), window, cx);
                    })),
            );
        }
        for (index, (line, hint)) in current.imports.ignored().into_iter().enumerate() {
            panel = panel.child(
                div()
                    .id(("agent-import-ignored", index))
                    .debug_selector(move || format!("agent-import-ignored-{index}"))
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("Line {line} · {hint}")),
            );
        }
        if let Some(error) = &current.language_error {
            panel = panel.child(error.clone());
        }
        if let Some(published) = self
            .agent_language
            .as_ref()
            .and_then(|controller| controller.published())
        {
            panel = panel.child(reader_agent_language::status_text(&published.report));
            for (index, diagnostic) in published.report.diagnostics.iter().enumerate() {
                let Some(text) = published.snapshot.source.get(diagnostic.range.clone()) else {
                    continue;
                };
                let hover = diagnostic.replacement.clone();
                panel = panel.child(
                    div()
                        .id(("agent-language-diagnostic", index))
                        .child(format!(
                            "Line {} · {:?} · {}",
                            diagnostic.line, diagnostic.bucket, text
                        ))
                        .tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(hover.clone()).build(window, cx)
                        }),
                );
            }
        } else if current.language_error.is_none() {
            panel = panel.child("Checking language…");
        }
        Some(panel.into_any_element())
    }
}
