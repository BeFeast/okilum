//! Reader source mode. No Brain enrollment or changes to the reader protocol.
use super::*;
use gpui_component::input::projection::{
    ActiveSource, ProjectionProvider, SourceClick, SourceMutation, SourceProjection, SourceSnapshot,
};
use gpui_component::input::{Editor, EditorState, WrappingIndent};

#[cfg(test)]
fn source_link_at(source: &str, offset: usize) -> Option<tessera_core::document_links::ParsedLink> {
    if !source.is_char_boundary(offset) {
        return None;
    }
    tessera_core::document_links::parse(source)
        .into_iter()
        .find(|link| link.range.contains(&offset))
}

struct ExactSource;
impl ProjectionProvider for ExactSource {
    fn compose(&self, _: &SourceSnapshot, _: &ActiveSource) -> Option<Arc<dyn SourceProjection>> {
        None
    }
}

use tessera_core::file_editor::{FileEditor, Save};

pub(super) struct Editing {
    store: FileEditor,
    input: Entity<EditorState>,
    conflict: Option<String>,
    conflict_detected: bool,
    compare: bool,
    save_failed: bool,
    protecting: bool,
    recovery_epoch: u64,
    save_pending: bool,
    saved_at: Option<time::OffsetDateTime>,
    current_input: Entity<EditorState>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Default)]
struct Editors(Vec<(WeakEntity<Reader>, AnyWindowHandle)>);
impl Global for Editors {}

pub(crate) fn save_all(cx: &mut App) -> bool {
    let editors = cx.default_global::<Editors>().0.clone();
    editors.into_iter().all(|(editor, _)| {
        editor
            .update(cx, |reader, cx| reader.save_source(cx))
            .unwrap_or(true)
    })
}

pub(crate) fn save_window(window: AnyWindowHandle, cx: &mut App) -> bool {
    let editors = cx.default_global::<Editors>().0.clone();
    editors
        .into_iter()
        .filter(|(_, owner)| *owner == window)
        .all(|(editor, _)| {
            editor
                .update(cx, |reader, cx| reader.save_source(cx))
                .unwrap_or(true)
        })
}

impl Editing {
    #[cfg(test)]
    pub(super) fn set_value(&self, value: &str, window: &mut Window, cx: &mut Context<Reader>) {
        self.input
            .update(cx, |input, cx| input.set_value(value, window, cx));
    }

    fn status(&self) -> &'static str {
        if self.conflict_detected {
            "Conflict"
        } else if self.save_pending {
            "Saving…"
        } else if self.save_failed {
            "Save failed"
        } else if self.store.dirty() {
            "Edited"
        } else {
            "Saved"
        }
    }
}

impl Reader {
    pub(super) fn discover_source_recovery(&mut self, cx: &mut Context<Self>) {
        self.recovery_offer = false;
        self.recovery_checked = false;
        self.recovery_error = false;
        let Some(directory) = self.session_directory.clone() else {
            self.recovery_error = true;
            cx.notify();
            return;
        };
        let path = self.vault_root.join(&self.current_rel);
        let generation = self.document_preparation_generation;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    FileEditor::has_unsaved_draft(&path, &directory.join("editor-drafts"))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.document_preparation_generation != generation || this.editing.is_some() {
                    return;
                }
                match result {
                    Ok(available) => {
                        this.recovery_offer = available;
                        if available {
                            this.recovery_dismissed = false;
                        }
                        this.recovery_checked = true;
                    }
                    Err(error) => {
                        this.recovery_error = true;
                        this.link_notice = Some(format!(
                        "Could not check saved drafts: {error:#}. Draft files have been preserved."
                    ))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn install_source_lifecycle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let participant = (cx.weak_entity(), window.window_handle());
        cx.default_global::<Editors>().0.push(participant);
        self._subs
            .push(cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    this.save_source(cx);
                }
            }));
        self._subs.push(cx.on_app_quit(|this, cx| {
            this.save_source(cx);
            async {}
        }));
    }

    pub(super) fn toggle_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_timeline().is_some_and(|t| t.selected.is_some()) {
            self.toggle_timeline_source(window, cx);
            return;
        }
        if self.file_preview.is_some() {
            return;
        }
        if self.editing.is_some() {
            if !self.leave_source(cx) {
                return;
            }
            let rel = self.current_rel.clone();
            self.prepare_document(&rel, None, None, window, cx);
            self.focus_handle.focus(window, cx);
            return;
        }
        if self.current_rel.is_empty() || self.loading.as_ref().is_some_and(|l| l.active) {
            return;
        }
        let result = (|| -> anyhow::Result<FileEditor> {
            let state = self
                .session_directory
                .clone()
                .ok_or_else(|| anyhow::anyhow!("No recovery directory is available"))?;
            FileEditor::open(
                &self.vault_root.join(&self.current_rel),
                &state.join("editor-drafts"),
            )
        })();
        let store = match result {
            Ok(store) => store,
            Err(error) => {
                self.link_notice = Some(format!("Cannot edit: {error:#}"));
                cx.notify();
                return;
            }
        };
        self.close_find(window, cx);
        // Cancel a pending navigation before exposing an editable snapshot.
        self.document_preparation_generation = self.document_preparation_generation.wrapping_add(1);
        let clipboard = cx
            .try_global::<platform::ManagedClipboard>()
            .map(|p| p.0.clone());
        let input = cx.new(|cx| {
            let mut input = EditorState::new(window, cx)
                .language("markdown")
                .line_number(false)
                .folding(false)
                .searchable(false)
                .replaceable(false)
                .soft_wrap(true)
                .wrapping_indent(WrappingIndent::None);
            input.set_projection_provider(Some(Arc::new(ExactSource)), cx);
            input.set_exact_clipboard_provider(clipboard, cx);
            input.set_value(store.text().to_owned(), window, cx);
            input
        });
        let current_input = cx.new(|cx| {
            let mut input = EditorState::new(window, cx)
                .language("markdown")
                .line_number(false)
                .soft_wrap(true);
            input.set_readonly(true, cx);
            input
        });
        let changed = cx.subscribe(&input, |this, input, _: &SourceMutation, cx| {
            let Some(editing) = &mut this.editing else { return; };
            if editing.input.entity_id() != input.entity_id() { return; }
            // Includes Silent mutations emitted by native undo and redo.
            let write = editing.store.queue_text(input.read(cx).value().to_string());
            editing.recovery_epoch = editing.recovery_epoch.wrapping_add(1);
            let epoch = editing.recovery_epoch;
            let input_id = input.entity_id();
            editing.protecting = true;
            cx.spawn(async move |this, cx| {
                let result = cx.background_executor().spawn(async move { write.persist() }).await;
                let _ = this.update(cx, |this, cx| {
                    let Some(editing) = &mut this.editing else { return; };
                    if editing.input.entity_id() != input_id || editing.recovery_epoch != epoch { return; }
                    editing.protecting = false;
                    if let Err(error) = result {
                        editing.save_failed = true;
                        this.link_notice = Some(format!("Draft recovery could not be saved: {error:#}. Keep this window open and retry Save or copy your draft."));
                    }
                    cx.notify();
                });
            }).detach();
            cx.notify();
        });
        let clicked = cx.subscribe_in(
            &input,
            window,
            |this, input, click: &SourceClick, window, cx| {
                if this
                    .editing
                    .as_ref()
                    .is_none_or(|editing| editing.input.entity_id() != input.entity_id())
                    || input.read(cx).source_stamp() != click.stamp
                    || click.event.button != MouseButton::Left
                    || !click.event.modifiers.platform
                {
                    return;
                }
                let source = input.read(cx).value().to_string();
                let Some(link) = tessera_core::document_links::parse_in_vault(
                    &source,
                    &this.vault,
                    &this.current_rel,
                )
                .into_iter()
                .find(|link| link.range.contains(&click.offset.0)) else {
                    return;
                };
                let resolved = tessera_core::document_links::resolve(
                    &link.target,
                    link.wiki,
                    &this.vault,
                    &this.current_rel,
                );
                // Protect the current draft before navigation, including browser links.
                if !this.save_source(cx) {
                    return;
                }
                match resolved.status {
                    "resolved" => this.open_note_at(
                        &resolved.candidates[0],
                        None,
                        resolved.heading.as_deref(),
                        window,
                        cx,
                    ),
                    "external" => cx.open_url(&resolved.url),
                    "attachment" => this.preview_file(&resolved.candidates[0], window, cx),
                    "outside_file" => this.outside_file_menu(&resolved.url, window, cx),
                    "ambiguous" => {
                        this.link_notice =
                            Some("This document link is ambiguous. Choose its destination.".into());
                        this.link_choices = resolved
                            .candidates
                            .into_iter()
                            .map(|path| (path, resolved.heading.clone()))
                            .collect();
                    }
                    _ => {
                        this.link_choices.clear();
                        this.link_notice = Some(
                            resolved
                                .reason
                                .unwrap_or("No document matches this link.")
                                .into(),
                        );
                    }
                }
                cx.notify();
            },
        );
        let blur = cx.on_blur(&input.focus_handle(cx), window, |this, _, cx| {
            // History is a read-only detour; preserve a dirty live buffer.
            if this.active_timeline().is_none() {
                this.save_source(cx);
            }
        });
        if store.dirty() {
            self.link_notice = Some(
                "Recovered an unsaved draft. Save to check it against the file on disk.".into(),
            );
        }
        self.editing = Some(Editing {
            store,
            input: input.clone(),
            conflict: None,
            conflict_detected: false,
            compare: false,
            save_failed: false,
            protecting: false,
            recovery_epoch: 0,
            save_pending: false,
            saved_at: None,
            current_input,
            _subscriptions: vec![changed, blur, clicked],
        });
        input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    pub(super) fn request_source_save(&mut self, cx: &mut Context<Self>) {
        if self.active_timeline().is_some_and(|t| t.selected.is_some()) {
            return;
        }
        let Some(editing) = &mut self.editing else {
            return;
        };
        editing.save_pending = true;
        let input_id = editing.input.entity_id();
        cx.notify();
        // Yield one frame so queued explicit Save is visible. Lifecycle saves
        // remain synchronous: quitting/navigation never relies on this task.
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(32))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this
                    .editing
                    .as_ref()
                    .is_some_and(|e| e.input.entity_id() == input_id && e.save_pending)
                {
                    this.save_source(cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn render_save_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(editing) = &self.editing else {
            return div().into_any_element();
        };
        let tooltip = match editing.saved_at {
            Some(at) => format!("Last saved at {:02}:{:02}:{:02}. Changes also save when you leave the editor or switch apps.", at.hour(), at.minute(), at.second()),
            None => "Changes save with ⌘S, when you leave the editor, or when you switch apps.".into(),
        };
        let tooltip = format!("{} · {}", editing.status(), tooltip);
        div()
            .id("source-save-status")
            .flex_none()
            .px_2()
            .text_color(if editing.conflict_detected || editing.save_failed {
                cx.theme().warning
            } else {
                brand::palette(cx).text_muted
            })
            .child(if self.source_is_dirty(cx) { "●" } else { "" })
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
            })
            .into_any_element()
    }

    pub(super) fn refresh_source_from_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::WindowExt;
        let Some(editing) = &mut self.editing else {
            return;
        };
        // Sample native state before looking at disk: a queued mutation event
        // cannot make an actually dirty buffer look clean.
        let text = editing.input.read(cx).value().to_string();
        if editing.store.text() != text {
            if let Err(error) = editing.store.set_text(text.clone()) {
                editing.save_failed = true;
                self.link_notice = Some(format!(
                    "Could not protect your edits: {error:#}. Your draft remains open."
                ));
                cx.notify();
                return;
            }
        }
        match editing.store.refresh_from_disk() {
            Ok(Save::Conflict) => {
                editing.conflict_detected = true;
                if editing.conflict.is_none() {
                    editing.conflict = editing.store.current().ok();
                }
                self.link_notice = Some("This file changed outside Tessera. Your edits are still here. Reload, keep your version, or compare before saving.".into());
            }
            Ok(Save::Saved) => {
                if editing.store.text() != text {
                    let updated = editing.store.text().to_owned();
                    editing
                        .input
                        .update(cx, |input, cx| input.set_value(updated, window, cx));
                    editing.conflict = None;
                    editing.conflict_detected = false;
                    editing.save_failed = false;
                    editing.saved_at = None;
                    window.push_notification("Updated from disk", cx);
                }
            }
            Err(error) => {
                self.link_notice = Some(format!(
                    "Could not read the changed file: {error:#}. Your edits are still here."
                ));
            }
        }
        cx.notify();
    }

    pub(super) fn save_source(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(editing) = &mut self.editing else {
            return true;
        };
        editing.save_pending = false;
        editing.recovery_epoch = editing.recovery_epoch.wrapping_add(1);
        editing.protecting = false;
        let text = editing.input.read(cx).value().to_string();
        let source_changed = editing.store.dirty() || editing.store.text() != text;
        let result = editing
            .store
            .set_text(text)
            .and_then(|_| editing.store.save());
        match result {
            Ok(Save::Saved) => {
                editing.saved_at = Some(
                    time::OffsetDateTime::now_local()
                        .unwrap_or_else(|_| time::OffsetDateTime::now_utc()),
                );
                editing.conflict = None;
                editing.conflict_detected = false;
                editing.save_failed = false;
                editing.protecting = false;
                if source_changed {
                    self.queue_saved_source(cx);
                }
                cx.notify();
                true
            }
            Ok(Save::Conflict) => {
                editing.conflict_detected = true;
                if editing.conflict.is_none() {
                    editing.conflict = editing.store.current().ok();
                }
                self.link_notice = Some("This file changed outside Tessera. Reload, keep your version, or compare before saving.".into());
                cx.notify();
                false
            }
            Err(error) => {
                editing.save_failed = true;
                self.link_notice =
                    Some(format!("Save failed: {error:#}. Your draft remains open."));
                cx.notify();
                false
            }
        }
    }
    pub(super) fn leave_source(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.save_source(cx) {
            return false;
        }
        self.editing = None;
        true
    }
    fn resolve_source(&mut self, reload: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut editing) = self.editing.take() else {
            return;
        };
        editing.recovery_epoch = editing.recovery_epoch.wrapping_add(1);
        editing.protecting = false;
        let result = if reload {
            editing.store.reload().map(|_| Save::Saved)
        } else if let Some(current) = &editing.conflict {
            editing.store.keep_mine(current)
        } else {
            Ok(Save::Conflict)
        };
        match result {
            Ok(Save::Saved) => {
                editing.saved_at = Some(
                    time::OffsetDateTime::now_local()
                        .unwrap_or_else(|_| time::OffsetDateTime::now_utc()),
                );
                if reload {
                    editing.input.update(cx, |input, cx| {
                        input.set_value(editing.store.text().to_owned(), window, cx)
                    });
                }
                editing.conflict = None;
                editing.conflict_detected = false;
                editing.compare = false;
                editing.save_failed = false;
                editing.protecting = false;
                self.link_notice = None;
            }
            Ok(Save::Conflict) => {
                editing.conflict_detected = true;
                editing.conflict = editing.store.current().ok();
                if let Some(current) = &editing.conflict {
                    editing
                        .current_input
                        .update(cx, |input, cx| input.set_value(current.clone(), window, cx));
                    editing.compare = true;
                }
                self.link_notice = Some(
                    "The file changed again. Compare the current version before retrying.".into(),
                );
            }
            Err(error) => {
                editing.save_failed = true;
                self.link_notice = Some(format!("Could not resolve: {error:#}"));
            }
        }
        self.editing = Some(editing);
        cx.notify();
    }
    fn park_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = &mut self.editing else {
            return;
        };
        // Explicit exit preserves the latest buffer even if the note was moved,
        // deleted, or is temporarily unwritable. Failed journal writes never exit.
        match editing
            .store
            .set_text(editing.input.read(cx).value().to_string())
        {
            Ok(()) => {
                self.editing = None;
                self.link_notice = Some("Draft kept in recovery. Reopen this note and enter source mode to restore it. If it was moved, restore its original path first.".into());
                self.focus_handle.focus(window, cx);
            }
            Err(error) => {
                self.link_notice = Some(format!(
                    "Cannot protect this draft: {error:#}. Copy the draft before closing."
                ));
            }
        }
        cx.notify();
    }
    pub(super) fn source_scroll_offset(&self, cx: &App) -> Option<Point<Pixels>> {
        self.editing
            .as_ref()
            .map(|editing| editing.input.read(cx).scroll_offset())
    }
    pub(super) fn scroll_source_by(&mut self, delta: Pixels, cx: &mut Context<Self>) -> bool {
        let Some(editing) = &self.editing else {
            return false;
        };
        editing.input.update(cx, |input, cx| {
            let mut offset = input.scroll_offset();
            offset.y += delta;
            input.set_scroll_offset(offset, cx);
        });
        true
    }

    pub(super) fn render_source(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some(editing) = &self.editing {
            // The editor API accepts whole rows. Round up so its end space
            // never falls below the shared pixel token (13px font, 1.5 leading).
            let rows = (f32::from(reader_bottom_space(window.viewport_size().height)) / (13. * 1.5))
                .ceil() as usize;
            editing.input.update(cx, |input, cx| {
                input.set_scroll_beyond_last_line(Some(rows), window, cx);
            });
        }
        let editing = self.editing.as_ref().unwrap();
        let palette = brand::palette(cx);
        v_flex()
            .size_full()
            .min_w_0()
            .when(editing.save_failed || editing.conflict_detected, |column| {
                column.child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .px_4()
                        .py_2()
                        .when(editing.save_failed || editing.conflict_detected, |row| {
                            row.child(
                                reader_icon_button(
                                    "source-park",
                                    IconName::ArrowRight,
                                    "Leave, keeping draft",
                                    cx,
                                )
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.park_source(window, cx)),
                                ),
                            )
                            .child(
                                reader_icon_button(
                                    "source-copy-draft",
                                    IconName::Copy,
                                    "Copy draft",
                                    cx,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        if let Some(editing) = &this.editing {
                                            let text = editing.input.read(cx).value().to_string();
                                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                                        }
                                    },
                                )),
                            )
                        })
                        .when(editing.conflict_detected, |row| {
                            row.child(
                                reader_icon_button(
                                    "source-reload",
                                    IconName::RotateCw,
                                    "Reload from disk",
                                    cx,
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| this.resolve_source(true, window, cx),
                                )),
                            )
                            .child(
                                reader_icon_button(
                                    "source-keep",
                                    IconName::Check,
                                    "Keep my version",
                                    cx,
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| this.resolve_source(false, window, cx),
                                )),
                            )
                            .child(
                                reader_icon_button(
                                    "source-compare",
                                    IconName::PanelRight,
                                    "Compare versions",
                                    cx,
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        if let Some(editing) = &mut this.editing {
                                            editing.compare = !editing.compare;
                                            if let Some(current) = &editing.conflict {
                                                editing.current_input.update(cx, |input, cx| {
                                                    input.set_value(current.clone(), window, cx)
                                                });
                                            }
                                        }
                                        cx.notify();
                                    },
                                )),
                            )
                        }),
                )
            })
            .when(editing.compare, |column| {
                column
                    .child(
                        div()
                            .px_4()
                            .text_color(palette.text_muted)
                            .child("File on disk (above) · Your version (below)"),
                    )
                    .child(
                        Editor::new(&editing.current_input)
                            .readonly(true)
                            .font_family("Cascadia Code")
                            .h(px(220.)),
                    )
            })
            .child(
                Editor::new(&editing.input)
                    .font_family("Cascadia Code")
                    .text_size(px(13.))
                    .size_full(),
            )
            .into_any_element()
    }
}

fn same_move_root(a: &Path, b: &Path) -> bool {
    a == b
        || a.canonicalize()
            .ok()
            .zip(b.canonicalize().ok())
            .is_some_and(|(a, b)| a == b)
}
impl Reader {
    pub(super) fn revert_link_move(
        &mut self,
        path: &Path,
        state: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let operation = tessera_core::link_rewrite::Operation::load(path)?;
        anyhow::ensure!(
            same_move_root(&operation.root, &self.vault_root),
            "Recovery belongs to a different folder"
        );
        let paths: Vec<_> = operation
            .files
            .keys()
            .cloned()
            .chain(std::iter::once(operation.to.clone()))
            .collect();
        self.check_move_editors(&paths, cx)?;
        let self_affected = paths.contains(&self.current_rel);
        let self_editing = if self_affected {
            self.editing.take().is_some()
        } else {
            false
        };
        let readers = cx.default_global::<Editors>().0.clone();
        let mut parked = vec![];
        for (reader, owner) in readers {
            if reader.entity_id() == cx.entity_id() {
                continue;
            }
            if let Ok(Some(was_editing)) = reader.update(cx, |r, _| {
                if same_move_root(&r.vault_root, &operation.root) && paths.contains(&r.current_rel)
                {
                    Some(r.editing.take().is_some())
                } else {
                    None
                }
            }) {
                parked.push((reader, owner, was_editing));
            }
        }
        let result = tessera_core::link_rewrite::Operation::revert(path, state);
        for (reader, owner, was_editing) in parked {
            let _ = owner.update(cx, |_, window, cx| {
                reader.update(cx, |r, cx| {
                    if result.is_ok() && r.current_rel == operation.to {
                        r.current_rel = operation.from.clone();
                    }
                    let rel = r.current_rel.clone();
                    r.prepare_document(&rel, None, None, window, cx);
                    if was_editing {
                        r.toggle_source(window, cx);
                    }
                    cx.notify();
                })
            });
        }
        if self_affected {
            if result.is_ok() && self.current_rel == operation.to {
                self.current_rel = operation.from.clone();
            }
            let rel = self.current_rel.clone();
            self.prepare_document(&rel, None, None, window, cx);
            if self_editing {
                self.toggle_source(window, cx);
            }
        }
        result
    }
    pub(super) fn sync_move_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(e) = self.editing.as_mut() {
            e.input.update(cx, |input, cx| {
                input.set_value(e.store.text().to_owned(), window, cx)
            });
        }
    }
    pub(super) fn source_has_other_editor(&self, cx: &mut Context<Self>) -> bool {
        let readers = cx.default_global::<Editors>().0.clone();
        readers
            .into_iter()
            .filter(|(r, _)| r.entity_id() != cx.entity_id())
            .any(|(r, _)| {
                r.upgrade().is_some_and(|r| {
                    let r = r.read(cx);
                    same_move_root(&r.vault_root, &self.vault_root)
                        && r.current_rel == self.current_rel
                        && r.editing.is_some()
                })
            })
    }
    pub(super) fn source_is_dirty(&self, cx: &App) -> bool {
        self.editing.as_ref().is_some_and(|e| {
            e.store.dirty()
                || e.input.read(cx).value().as_ref() != e.store.text()
                || e.conflict_detected
                || e.protecting
        })
    }
    pub(super) fn check_move_editors(
        &self,
        paths: &[String],
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !paths.contains(&self.current_rel) || !self.source_is_dirty(cx),
            "Save or discard this note's unsaved edits before moving"
        );
        let readers = cx.default_global::<Editors>().0.clone();
        for (reader, _) in readers {
            if reader.entity_id() == cx.entity_id() {
                continue;
            }
            if let Some(reader) = reader.upgrade() {
                let reader = reader.read(cx);
                anyhow::ensure!(
                    !same_move_root(&reader.vault_root, &self.vault_root)
                        || !paths.contains(&reader.current_rel)
                        || !reader.source_is_dirty(cx),
                    "{} has unsaved edits in another window; save or discard them first",
                    reader.current_rel
                );
            }
        }
        Ok(())
    }
    pub(super) fn apply_link_move(
        &mut self,
        preview: &tessera_core::link_rewrite::Preview,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<tessera_core::link_rewrite::Applied> {
        let paths = preview.affected_paths();
        self.check_move_editors(&paths, cx)?;
        let state = self
            .session_directory
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No recovery storage"))?;
        let readers = cx.default_global::<Editors>().0.clone();
        let mut parked = vec![];
        for (reader, window) in readers {
            if reader.entity_id() == cx.entity_id() {
                continue;
            }
            if let Ok(Some((path, editing))) = reader.update(cx, |r, _| {
                if same_move_root(&r.vault_root, &self.vault_root) && paths.contains(&r.current_rel)
                {
                    Some((r.current_rel.clone(), r.editing.take()))
                } else {
                    None
                }
            }) {
                parked.push((reader, window, path, editing));
            }
        }
        let result = (|| -> anyhow::Result<_> {
            let mut stores = std::collections::BTreeMap::new();
            if let Some(editing) = self.editing.as_mut() {
                stores.insert(self.current_rel.clone(), &mut editing.store);
            }
            for (_, _, path, editing) in &mut parked {
                if let Some(editing) = editing {
                    anyhow::ensure!(stores.insert(path.clone(), &mut editing.store).is_none(),"{path} is open in multiple source editors; close duplicate editors before moving");
                }
            }
            let refreshed = stores
                .values_mut()
                .try_for_each(|store| store.refresh_from_disk().map(|_| ()));
            refreshed.and_then(|()| preview.apply(&self.vault_root, &state, &mut stores))
        })();
        let moved = result.as_ref().is_ok_and(|a| a.moved);
        for (reader, window, path, editing) in parked {
            let _ = window.update(cx, |_, window, cx| {
                reader.update(cx, |r, cx| {
                    let was_editing = editing.is_some();
                    r.editing = editing;
                    if moved && path == preview.from {
                        r.editing = None;
                        r.current_rel = preview.to.clone();
                        for p in &mut r.history {
                            if *p == preview.from {
                                *p = preview.to.clone();
                            }
                        }
                    }
                    if let Some(e) = r.editing.as_mut() {
                        e.input.update(cx, |input, cx| {
                            input.set_value(e.store.text().to_owned(), window, cx)
                        });
                    } else {
                        let rel = r.current_rel.clone();
                        r.prepare_document(&rel, None, None, window, cx);
                        if moved
                            && path == preview.from
                            && was_editing
                            && result.as_ref().is_ok_and(|a| a.warning.is_none())
                        {
                            r.toggle_source(window, cx);
                        }
                    }
                    cx.notify();
                })
            });
        }
        result
    }
}

impl Reader {
    pub(super) fn restore_source_version(
        &mut self,
        reviewed: &str,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.source_is_dirty(cx),
            "Save or discard unsaved edits before restoring a version"
        );
        self.check_move_editors(std::slice::from_ref(&self.current_rel), cx)?;
        let state = self
            .session_directory
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No recovery storage"))?;
        let result = if let Some(editing) = self.editing.as_mut() {
            tessera_core::source_history::restore(&mut editing.store, reviewed, text)
        } else {
            let mut editor = FileEditor::open(
                &self.vault_root.join(&self.current_rel),
                &state.join("editor-drafts"),
            )?;
            tessera_core::source_history::restore(&mut editor, reviewed, text)
        };
        // On a racing write the recovered version remains dirty and visible.
        self.sync_move_input(window, cx);
        if result.is_ok() && self.editing.is_none() {
            let rel = self.current_rel.clone();
            self.prepare_document(&rel, None, None, window, cx);
        }
        result
    }
}

#[cfg(test)]
mod tests {

    #[gpui::test]
    fn native_history_restore_preserves_source_and_refuses_dirty_or_stale_input(
        cx: &mut TestAppContext,
    ) {
        use gpui_component::WindowExt;
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let directory =
            std::env::temp_dir().join(format!("tessera-history-{}", uuid::Uuid::new_v4()));
        let root = directory.join("notes");
        std::fs::create_dir_all(&root).unwrap();
        let original = "\u{feff}---\r\ntitle: Привет e\u{301}\r\n---\r\n[[Заметка 🧠]]\r\n";
        std::fs::write(root.join("note.md"), original).unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("note.md")),
                        index_dir: Some(directory.join("index")),
                        session_directory: Some(directory.join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            r.toggle_source(window, cx);
            let input = r.editing.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| {
                input.replace_text_in_range(Some(0..0), "edit", window, cx)
            });
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            assert!(r
                .restore_source_version(original, "bad", window, cx)
                .is_err());
            assert!(r.save_source(cx));
        });
        let edited = format!("edit{original}");
        reader.update_in(visual, |r, window, cx| {
            r.restore_source_version(&edited, original, window, cx)
                .unwrap();
            assert_eq!(
                r.editing.as_ref().unwrap().input.read(cx).value().as_ref(),
                original
            );
            r.source_history(false, window, cx);
            assert!(!window.has_active_dialog(cx));
            assert!(r.active_timeline().is_some());
        });
        visual.run_until_parked();
        visual.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.close_dialog(cx);
        });
        assert_eq!(
            std::fs::read(root.join("note.md")).unwrap(),
            original.as_bytes()
        );
        let versions =
            tessera_core::source_history::list(&directory.join("state/editor-drafts"), &root)
                .unwrap();
        assert!(versions.versions.iter().any(|v| v.text == edited));
        std::fs::write(root.join("note.md"), "external").unwrap();
        reader.update_in(visual, |r, window, cx| {
            assert!(r
                .restore_source_version(original, &edited, window, cx)
                .is_err());
        });
        assert_eq!(
            std::fs::read_to_string(root.join("note.md")).unwrap(),
            "external"
        );
        std::fs::remove_file(root.join("note.md")).unwrap();
        reader.update_in(visual, |r, window, cx| {
            r.source_history(false, window, cx);
            assert!(
                r.active_timeline().is_some(),
                "missing current file must not hide its retained history"
            );
            assert!(!window.has_active_dialog(cx));
        });
        visual.run_until_parked();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn document_end_space_resizes_in_reader_and_editor(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root = std::env::temp_dir().join(format!("tessera-reader419-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let source = (0..90)
            .map(|i| format!("Paragraph {i}\n\n"))
            .collect::<String>()
            + "LAST LINE";
        std::fs::write(root.join("long.md"), &source).unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("long.md".into()),
                        session_directory: Some(root.join("state")),
                        ..Opts::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        for height in [700., 1000.] {
            visual.simulate_resize(size(px(1500.), px(height)));
            visual.run_until_parked();
            reader.update_in(visual, |v, _, cx| {
                v.content.read(cx).list_state().scroll_to_end();
                cx.notify();
            });
            visual.run_until_parked();
            visual.update(|w, cx| w.draw(cx).clear(cx));
            reader.read_with(visual, |v, cx| {
                let list = v.content.read(cx).list_state();
                assert!(
                    list.logical_scroll_top().item_ix > 0,
                    "document actually scrolled"
                );
                let last = list.bounds_for_item(list.item_count() - 1).unwrap();
                let gap = list.viewport_bounds().bottom() - last.bottom();
                assert!(
                    (gap - reader_bottom_space(px(height))).abs() < px(2.),
                    "Reader gap {gap:?}"
                );
            });
        }
        reader.update_in(visual, |v, window, cx| v.toggle_source(window, cx));
        visual.run_until_parked();
        for height in [700., 1000.] {
            visual.simulate_resize(size(px(1500.), px(height)));
            visual.run_until_parked();
            let input = reader.read_with(visual, |v, _| v.editing.as_ref().unwrap().input.clone());
            let bounds = input.read_with(visual, |v, _| v.input_bounds());
            visual.simulate_event(ScrollWheelEvent {
                position: bounds.center(),
                delta: ScrollDelta::Pixels(point(px(0.), px(-100000.))),
                ..Default::default()
            });
            visual.run_until_parked();
            visual.update(|w, cx| w.draw(cx).clear(cx));
            input.read_with(visual, |v, _| {
                assert!(v.scroll_offset().y < px(-100.), "editor actually scrolled");
                let last = v
                    .range_to_bounds(&(source.len() - 9..source.len()))
                    .unwrap();
                let gap = v.input_bounds().bottom() - last.bottom();
                let expected = reader_bottom_space(px(height));
                assert!(
                    gap >= expected - px(2.) && gap < expected + px(25.),
                    "Editor gap {gap:?}, expected {expected:?}"
                );
                assert_eq!(
                    v.value().to_string(),
                    source,
                    "spacing does not insert source text"
                );
            });
        }
        assert_eq!(
            std::fs::read_to_string(root.join("long.md")).unwrap(),
            source
        );
        let _ = std::fs::remove_dir_all(root);
    }

    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn links_use_bytes_of_current_unicode_source() {
        for source in [
            "Кириллица 🧠e\u{301} [[Заметка|ссылка]] хвост🦀",
            "🧠e\u{301} [ссылка](target.md) хвост",
        ] {
            let links = tessera_core::document_links::parse(source);
            assert_eq!(links.len(), 1);
            let link = &links[0];
            for (offset, _) in source.char_indices() {
                assert_eq!(
                    source_link_at(source, offset).is_some(),
                    link.range.contains(&offset)
                );
            }
            assert!(source.get(link.range.clone()).is_some());
            let changed = format!("новый 🧠 {source}");
            let offset = changed.find("ссылка").unwrap();
            assert_eq!(
                source_link_at(&changed, offset).unwrap().target,
                link.target
            );
        }
    }

    #[gpui::test]
    fn save_status_clean_refresh_and_dirty_conflict(cx: &mut TestAppContext) {
        use gpui_component::WindowExt;
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let directory =
            std::env::temp_dir().join(format!("tessera-save-state-{}", uuid::Uuid::new_v4()));
        let root = directory.join("notes");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("start.md");
        std::fs::write(&path, "original").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(path.clone()),
                        index_dir: Some(directory.join("index")),
                        session_directory: Some(directory.join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx)
        });
        visual.run_until_parked();
        let input = reader.read_with(visual, |r, _| {
            assert_eq!(r.editing.as_ref().unwrap().status(), "Saved");
            r.editing.as_ref().unwrap().input.clone()
        });
        input.update_in(visual, |input, window, cx| {
            input.replace_text_in_range(Some(0..0), "edited ", window, cx)
        });
        visual.run_until_parked();
        reader.update_in(visual, |reader, _, cx| {
            assert_eq!(reader.editing.as_ref().unwrap().status(), "Edited");
            reader.request_source_save(cx);
            assert_eq!(reader.editing.as_ref().unwrap().status(), "Saving…");
        });
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(40));
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.editing.as_ref().unwrap().status(), "Saved")
        });
        // A clean explicit Save also acknowledges the request and updates time.
        reader.update_in(visual, |reader, _, cx| {
            assert!(reader.save_source(cx));
            assert_eq!(reader.editing.as_ref().unwrap().status(), "Saved");
            assert!(reader.editing.as_ref().unwrap().saved_at.is_some());
        });
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "edited original");
        std::fs::write(&path, "external clean").unwrap();
        reader.update_in(visual, |reader, window, cx| {
            assert!(window.notifications(cx).is_empty());
            reader.refresh_source_from_disk(window, cx);
            assert_eq!(input.read(cx).value().as_ref(), "external clean");
            assert_eq!(
                window.notifications(cx).len(),
                1,
                "clean replacement must announce itself"
            );
            reader.refresh_source_from_disk(window, cx);
            assert_eq!(
                window.notifications(cx).len(),
                1,
                "unchanged/self-write refresh must be quiet"
            );
        });
        visual.run_until_parked();
        input.update_in(visual, |input, window, cx| {
            input.replace_text_in_range(Some(0..0), "unsaved ", window, cx)
        });
        visual.run_until_parked();
        std::fs::write(&path, "external conflict").unwrap();
        reader.update_in(visual, |reader, window, cx| {
            reader.refresh_source_from_disk(window, cx);
            assert_eq!(reader.editing.as_ref().unwrap().status(), "Conflict");
            assert_eq!(input.read(cx).value().as_ref(), "unsaved external clean");
            assert!(!reader.save_source(cx));
            let selected = reader.selected_file().to_owned();
            reader.close_note(window, cx);
            assert_eq!(reader.selected_file(), selected);
            assert_eq!(input.read(cx).value().as_ref(), "unsaved external clean");
        });
        reader.update_in(visual, |reader, _, _| {
            let editing = reader.editing.as_mut().unwrap();
            // A file can disappear between conflict detection and snapshot read.
            editing.conflict = None;
            assert_eq!(editing.status(), "Conflict");
        });
        assert_eq!(std::fs::read_to_string(path).unwrap(), "external conflict");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn unclean_startup_offers_draft_without_restoring_editor(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
            cx.set_global(reader_recovery::RecoveryStartup(true));
        });
        let directory =
            std::env::temp_dir().join(format!("tessera-recovery-{}", uuid::Uuid::new_v4()));
        let root = directory.join("notes");
        let state = directory.join("state");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("start.md");
        std::fs::write(&path, "# Original disk note").unwrap();
        {
            let mut editor = FileEditor::open(&path, &state.join("editor-drafts")).unwrap();
            editor
                .set_text("# Несохранённый 🧠e\u{301} [[target]]".into())
                .unwrap();
        }
        let preferences = directory.join("reader-layout.json");
        let preferences_bytes = br#"{"notes":777,"backlinks":888}"#;
        std::fs::write(&preferences, preferences_bytes).unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(path.clone()),
                        index_dir: Some(directory.join("index")),
                        session_directory: Some(state.clone()),
                        panel_settings_override: Some(preferences.clone()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.recovery_startup);
            assert!(reader.recovery_offer);
            assert!(reader.editing.is_none());
            assert!(!reader.panels.notes && !reader.panels.backlinks);
            assert_eq!(
                reader.panel_widths.notes,
                reader_layout::Widths::default().notes
            );
            assert_eq!(std::fs::read(&preferences).unwrap(), preferences_bytes);
            assert_eq!(reader.current_rel, "start.md");
        });
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# Original disk note"
        );
        assert!(FileEditor::has_unsaved_draft(&path, &state.join("editor-drafts")).unwrap());
        // Dismissing an earlier banner cannot hide a subsequently discovered draft.
        reader.update_in(visual, |reader, _, cx| {
            reader.recovery_dismissed = true;
            reader.discover_source_recovery(cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.recovery_offer);
            assert!(!reader.recovery_dismissed);
        });
        // The Restore unsaved edits button invokes this explicit action.
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx)
        });
        visual.run_until_parked();
        reader.read_with(visual, |reader, cx| {
            assert_eq!(
                reader
                    .editing
                    .as_ref()
                    .unwrap()
                    .input
                    .read(cx)
                    .value()
                    .as_ref(),
                "# Несохранённый 🧠e\u{301} [[target]]"
            );
        });
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "# Original disk note"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn unicode_native_click_places_cursor_and_command_click_navigates(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let directory =
            std::env::temp_dir().join(format!("tessera-unicode-click-{}", uuid::Uuid::new_v4()));
        let root = directory.join("notes");
        std::fs::create_dir_all(&root).unwrap();
        let original = "[[target|ссылка 🧠e\u{301}]] Кириллица хвост 🦀";
        std::fs::write(root.join("start.md"), original).unwrap();
        std::fs::write(root.join("target.md"), "# Цель 🧠e\u{301}").unwrap();
        std::fs::write(
            root.join("backlink.md"),
            format!("{}[[start|сс]] хвост 🧠e\u{301}", "я".repeat(79)),
        )
        .unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("start.md")),
                        index_dir: Some(directory.join("index")),
                        session_directory: Some(directory.join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |reader, _, cx| {
            let backlink = reader
                .backlinks
                .iter()
                .find(|b| b.path == "backlink.md")
                .expect("backlink inventory must be ready");
            let (context, range, _) = backlink_occurrence(backlink);
            assert!(context.ends_with('…'));
            assert_eq!(range, Some(158..160));
            reader.panels.open(reader_layout::Panel::Backlinks);
            cx.notify();
        });
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.panels.visible(
                reader_layout::Panel::Backlinks,
                reader.body_viewport_width.into()
            ));
        });
        // Read mode uses rendered link ranges over Unicode label text.
        let bounds = reader.read_with(visual, |reader, cx| reader.content.read(cx).bounds());
        visual.simulate_click(
            bounds.origin + point(px(25.), px(10.)),
            gpui::Modifiers::default(),
        );
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.current_rel, "target.md")
        });
        reader.update_in(visual, |reader, window, cx| {
            reader.open_note_at("start.md", None, None, window, cx)
        });
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx)
        });
        visual.run_until_parked();
        let input = reader.read_with(visual, |reader, _| {
            reader.editing.as_ref().unwrap().input.clone()
        });
        let offset = original.find("ссылка").unwrap();
        let position = input.read_with(visual, |input, _| {
            input
                .range_to_bounds(&(offset..offset + 2))
                .unwrap()
                .center()
        });
        visual.simulate_click(position, gpui::Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.current_rel, "start.md")
        });
        input.read_with(visual, |input, _| {
            let cursor = input.cursor();
            assert!(cursor >= offset && cursor <= offset + 2);
        });
        // Change source and re-layout: the next click must use the new string.
        reader.update_in(visual, |_, window, cx| {
            input.update(cx, |input, cx| {
                input.replace_text_in_range(Some(0..0), "новый 🧠 ", window, cx);
            })
        });
        visual.run_until_parked();
        let position = input.read_with(visual, |input, _| {
            let text = input.value();
            let offset = text.find("ссылка").unwrap();
            input
                .range_to_bounds(&(offset..offset + 2))
                .unwrap()
                .center()
        });
        visual.simulate_click(
            position,
            gpui::Modifiers {
                platform: true,
                ..Default::default()
            },
        );
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert_eq!(reader.current_rel, "target.md")
        });
        assert_eq!(
            std::fs::read_to_string(root.join("start.md")).unwrap(),
            format!("новый 🧠 {original}")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn native_edit_undo_save_and_conflict_navigation(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let directory =
            std::env::temp_dir().join(format!("tessera-source-{}", uuid::Uuid::new_v4()));
        let root = directory.join("notes");
        std::fs::create_dir_all(&root).unwrap();
        let original = "\u{feff}---\r\ntitle: Привет\r\n---\r\nbody 🧠\r\n";
        std::fs::write(root.join("first.md"), original).unwrap();
        std::fs::write(root.join("second.md"), "Second").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("first.md")),
                        index_dir: Some(directory.join("index")),
                        session_directory: Some(directory.join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx);
            let input = reader.editing.as_ref().unwrap().input.clone();
            assert_eq!(input.read(cx).value().as_ref(), original);
            input.update(cx, |input, cx| {
                input.replace_text_in_range(Some(0..0), "edit", window, cx)
            });
        });
        visual.run_until_parked();
        reader.update_in(visual, |reader, _, cx| {
            assert!(reader.editing.as_ref().unwrap().store.dirty());
            assert!(reader.save_source(cx));
        });
        assert_eq!(
            std::fs::read_to_string(root.join("first.md")).unwrap(),
            format!("edit{original}")
        );
        #[cfg(target_os = "macos")]
        visual.simulate_keystrokes("cmd-z");
        #[cfg(not(target_os = "macos"))]
        visual.simulate_keystrokes("ctrl-z");
        visual.run_until_parked();
        let journal = std::fs::read_dir(directory.join("state/editor-drafts"))
            .unwrap()
            .flatten()
            .find(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .unwrap()
            .path();
        let recovered: serde_json::Value =
            serde_json::from_slice(&std::fs::read(journal).unwrap()).unwrap();
        assert_eq!(
            recovered["text"].as_str(),
            Some(original),
            "undo is durable before Save"
        );
        reader.update_in(visual, |reader, _, cx| {
            assert_eq!(
                reader
                    .editing
                    .as_ref()
                    .unwrap()
                    .input
                    .read(cx)
                    .value()
                    .as_ref(),
                original
            );
            assert!(reader.save_source(cx));
        });
        reader.update_in(visual, |reader, window, cx| {
            let input = reader.editing.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| {
                input.replace_text_in_range(Some(0..0), "mine", window, cx)
            });
        });
        visual.run_until_parked();
        std::fs::write(root.join("first.md"), "external").unwrap();
        reader.update_in(visual, |reader, window, cx| {
            reader.open_note("second.md", None, window, cx);
            assert_eq!(reader.current_rel, "first.md");
            assert!(reader.editing.as_ref().unwrap().conflict.is_some());
            assert!(!reader.save_source(cx));
            std::fs::write(root.join("first.md"), "external v2").unwrap();
            assert!(!reader.save_source(cx));
            assert_eq!(
                reader.editing.as_ref().unwrap().conflict.as_deref(),
                Some("external")
            );
            reader.resolve_source(false, window, cx);
            assert_eq!(
                std::fs::read_to_string(root.join("first.md")).unwrap(),
                "external v2"
            );
            assert!(reader.editing.as_ref().unwrap().compare);
            assert_eq!(
                reader
                    .editing
                    .as_ref()
                    .unwrap()
                    .current_input
                    .read(cx)
                    .value()
                    .as_ref(),
                "external v2"
            );
            reader.resolve_source(true, window, cx);
            assert_eq!(
                reader
                    .editing
                    .as_ref()
                    .unwrap()
                    .input
                    .read(cx)
                    .value()
                    .as_ref(),
                "external v2"
            );
            reader.open_note("second.md", None, window, cx);
        });
        visual.run_until_parked();
        reader.update_in(visual, |reader, _, _| {
            assert_eq!(reader.current_rel, "second.md");
            assert!(reader.editing.is_none());
        });
        assert_eq!(
            std::fs::read_to_string(root.join("first.md")).unwrap(),
            "external v2"
        );
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx);
            reader
                .editing
                .as_ref()
                .unwrap()
                .input
                .clone()
                .update(cx, |input, cx| {
                    input.replace_text_in_range(Some(0..0), "recover me", window, cx);
                });
        });
        visual.run_until_parked();
        std::fs::remove_file(root.join("second.md")).unwrap();
        reader.update_in(visual, |reader, window, cx| {
            assert!(!reader.save_source(cx));
            assert!(reader.editing.as_ref().unwrap().save_failed);
            reader.park_source(window, cx);
            assert!(
                reader.editing.is_none(),
                "failed canonical save can exit with durable recovery"
            );
        });
        assert!(std::fs::read_dir(directory.join("state/editor-drafts"))
            .unwrap()
            .flatten()
            .any(|entry| {
                entry.path().extension().is_some_and(|ext| ext == "json")
                    && std::fs::read_to_string(entry.path())
                        .unwrap()
                        .contains("recover meSecond")
            }));
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[gpui::test]
    fn move_blocks_dirty_other_window_and_reloads_clean_input(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let directory =
            std::env::temp_dir().join(format!("tessera-move-windows-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(directory.join("notes/New")).unwrap();
        let root = directory.join("notes").canonicalize().unwrap();
        std::fs::write(root.join("start.md"), "# Source").unwrap();
        std::fs::write(root.join("ref.md"), "[[start]]").unwrap();
        let mut reference_cx = cx.clone();
        let mut source = None;
        let (_, source_visual) = cx.add_window_view(|window, cx| {
            let r = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("start.md")),
                        index_dir: Some(directory.join("index")),
                        session_directory: Some(directory.join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            source = Some(r.clone());
            Root::new(r, window, cx)
        });
        let mut reference = None;
        let (_, reference_visual) = reference_cx.add_window_view(|window, cx| {
            let r = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("ref.md")),
                        index_dir: Some(directory.join("index")),
                        session_directory: Some(directory.join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reference = Some(r.clone());
            Root::new(r, window, cx)
        });
        let source = source.unwrap();
        let reference = reference.unwrap();
        source_visual.run_until_parked();
        reference_visual.run_until_parked();
        reference.update_in(reference_visual, |r, window, cx| {
            r.toggle_source(window, cx)
        });
        let input = reference.read_with(reference_visual, |r, _| {
            r.editing.as_ref().unwrap().input.clone()
        });
        input.update_in(reference_visual, |input, window, cx| {
            input.replace_text_in_range(Some(0..0), "edited ", window, cx)
        });
        reference_visual.run_until_parked();
        let preview =
            tessera_core::link_rewrite::Preview::prepare(&root, "start.md", "New/renamed.md")
                .unwrap();
        source.update_in(source_visual, |r, _, cx| {
            assert!(r
                .apply_link_move(&preview, cx)
                .err()
                .unwrap()
                .to_string()
                .contains("unsaved edits"))
        });
        assert!(root.join("start.md").exists());
        assert!(!root.join("New/renamed.md").exists());
        reference.update_in(reference_visual, |r, _, cx| assert!(r.save_source(cx)));
        let preview =
            tessera_core::link_rewrite::Preview::prepare(&root, "start.md", "New/renamed.md")
                .unwrap();
        source.update_in(source_visual, |r, _, cx| {
            assert!(r.apply_link_move(&preview, cx).unwrap().moved)
        });
        reference_visual.run_until_parked();
        reference.read_with(reference_visual, |r, cx| {
            let editing = r.editing.as_ref().unwrap();
            assert_eq!(
                editing.input.read(cx).value().as_ref(),
                "edited [[renamed]]"
            );
            assert!(!editing.store.dirty());
        });
        assert_eq!(
            std::fs::read_to_string(root.join("ref.md")).unwrap(),
            "edited [[renamed]]"
        );
        let state = directory.join("state");
        let operations = tessera_core::link_rewrite::Operation::list(&state, &root).unwrap();
        source.update_in(source_visual, |r, window, cx| {
            r.revert_link_move(&operations.operations[0], &state, window, cx)
                .unwrap()
        });
        reference_visual.run_until_parked();
        reference.read_with(reference_visual, |r, cx| {
            let editing = r.editing.as_ref().unwrap();
            assert_eq!(editing.input.read(cx).value().as_ref(), "edited [[start]]");
            assert!(!editing.store.dirty());
        });
        assert!(root.join("start.md").exists());
        assert!(!root.join("New/renamed.md").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
