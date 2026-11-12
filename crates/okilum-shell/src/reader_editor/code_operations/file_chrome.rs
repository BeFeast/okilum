//! File-only presentation over the existing revision-safe operations. Notes
//! retain their fenced-code host. Live views sit beside the cached Header.
use super::*;
use crate::reader_code_file::{document::JsonError, PresentationState, View};
use crate::reader_find_field::editor::EditorTarget;
use crate::reader_viewer_chrome::{LiveSlot, ViewerAction, ViewerChrome, ViewerView};
use gpui::Action as _;
use gpui_component::menu::DropdownMenu as _;
use gpui_component::{button::DropdownButton, input::InputState, menu::PopupMenu};

/// The one strict verdict for an immutable editable revision. Contents can
/// consume this same snapshot/verdict; no value tree or flattened copy is kept
/// beside the editor, and no second validity model is inferred from a grammar.
pub(crate) struct JsonSnapshot {
    pub(crate) source: SourceSnapshot,
    pub(crate) verdict: Result<Result<(), model::Invalid>, String>,
    pub(crate) diagnostic: Option<JsonError>,
}

pub(crate) struct FileChrome {
    reader: WeakEntity<Reader>,
    window: AnyWindowHandle,
    source: Entity<EditorState>,
    pub(crate) find: Entity<EditorTarget>,
    read_state: Option<PresentationState>,
    json: bool,
    generation: u64,
    parsing: bool,
    control: model::Control,
    task: Option<Task<()>>,
    pub(crate) current: Option<Arc<JsonSnapshot>>,
    pub(crate) last_valid: Option<Arc<JsonSnapshot>>,
    format: LiveSlot,
    badge: LiveSlot,
    language: LiveSlot,
    bottom: Entity<Part>,
    sheet: Entity<Part>,
    #[cfg(test)]
    pub(crate) scans: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    pub(crate) parses: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    pub(crate) installs: usize,
    #[cfg(test)]
    pub(crate) worker_gate: Option<futures::channel::oneshot::Receiver<()>>,
}

impl Drop for FileChrome {
    fn drop(&mut self) {
        self.control.cancel();
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Format,
    Language,
    Badge,
    Bottom,
    Sheet,
}

struct Part {
    file: WeakEntity<FileChrome>,
    kind: Kind,
    _subscriptions: [Subscription; 2],
}

#[cfg(test)]
type DrawnStatus = (usize, String, Vec<&'static str>);
#[cfg(test)]
type PopupState = Option<(WeakEntity<PopupMenu>, Vec<(String, bool)>)>;
#[cfg(test)]
thread_local! {
    static DRAWN_STATUS: std::cell::RefCell<std::collections::HashMap<EntityId, DrawnStatus>> = Default::default();
    static FORMAT_POPUP: std::cell::RefCell<PopupState> = const { std::cell::RefCell::new(None) };
    static DRAWN_FORMAT: std::cell::RefCell<std::collections::HashMap<EntityId, Option<String>>> = Default::default();
}

impl Part {
    fn new(
        file: Entity<FileChrome>,
        reader: Entity<Reader>,
        kind: Kind,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            file: file.downgrade(),
            kind,
            _subscriptions: [
                cx.observe(&file, |_, _, cx| cx.notify()),
                cx.observe(&reader, |_, _, cx| cx.notify()),
            ],
        }
    }
}

impl Render for Part {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(file) = self.file.upgrade() else {
            return div().into_any_element();
        };
        let state = file.read(cx);
        let Some(reader) = state.reader.upgrade() else {
            return div().into_any_element();
        };
        let reader = reader.read(cx);
        // Autosave temporarily parks/reassembles Editing. A retained Part may
        // be drawn in that interval; it must never address a different editor.
        if reader
            .editing
            .as_ref()
            .is_none_or(|e| e.input != state.source)
        {
            return div().into_any_element();
        }
        match self.kind {
            Kind::Format => format_control(reader, state, cx),
            Kind::Language => language_control(reader, state, cx),
            Kind::Badge => badge(reader, state, cx),
            Kind::Bottom => bottom(reader, state, cx),
            Kind::Sheet => sheet(reader, window, cx),
        }
    }
}

impl FileChrome {
    fn new(
        reader: Entity<Reader>,
        source: Entity<EditorState>,
        saved: Option<PresentationState>,
        json: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| {
            let mut field = InputState::new(window, cx).placeholder("Find");
            if let Some(saved) = &saved {
                field.set_value(saved.query.clone(), window, cx);
            }
            field
        });
        let sensitive = Rc::new(std::cell::Cell::new(saved.as_ref().map_or_else(
            || reader_ui_state::find_case_sensitive(cx),
            |s| s.case_sensitive,
        )));
        source.update(cx, |input, cx| {
            input.set_external_search_presentation(true, cx)
        });
        let find = cx.new(|cx| EditorTarget::with_query(source.clone(), query, sensitive, cx));
        find.update(cx, |find, cx| find.resume(cx));
        let file = cx.entity();
        let part = |kind, cx: &mut Context<Self>| {
            cx.new(|cx| Part::new(file.clone(), reader.clone(), kind, cx))
        };
        let format = LiveSlot::new(part(Kind::Format, cx), 104.);
        let badge = LiveSlot::new(part(Kind::Badge, cx), 28.);
        let language = LiveSlot::new(part(Kind::Language, cx), 68.);
        Self {
            reader: reader.downgrade(),
            window: window.window_handle(),
            source,
            find,
            read_state: saved,
            json,
            generation: 0,
            parsing: false,
            control: Default::default(),
            task: None,
            current: None,
            last_valid: None,
            format,
            badge,
            language,
            bottom: part(Kind::Bottom, cx),
            sheet: part(Kind::Sheet, cx),
            #[cfg(test)]
            scans: Default::default(),
            #[cfg(test)]
            parses: Default::default(),
            #[cfg(test)]
            installs: 0,
            #[cfg(test)]
            worker_gate: None,
        }
    }

    pub(crate) fn source_changed(
        &mut self,
        presentation_changed: bool,
        stale_preview: bool,
        cx: &mut Context<Self>,
    ) {
        self.control.cancel();
        self.generation = self.generation.wrapping_add(1);
        if stale_preview {
            // SourceMutation dispatch cannot re-enter its owning Window.
            let owner = self.window;
            cx.defer(move |cx| {
                let _ = owner.update(cx, |_, window, cx| {
                    reader_toast::transient("The text changed. Open Repair again.", window, cx)
                });
            });
        }
        if presentation_changed {
            cx.notify();
        }
        if self.json && !self.parsing {
            self.schedule(cx);
        }
    }

    fn schedule(&mut self, cx: &mut Context<Self>) {
        let generation = self.generation;
        self.task = Some(cx.spawn(async move |file, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            let Some((source, control)) = file
                .update(cx, |state, cx| {
                    if state.generation != generation {
                        return None;
                    }
                    state.parsing = true;
                    state.control = model::Control::default();
                    Some((
                        state.source.read(cx).source_snapshot(),
                        state.control.clone(),
                    ))
                })
                .ok()
                .flatten()
            else {
                return;
            };
            #[cfg(test)]
            let (scans, parses, gate) = match file.update(cx, |s, _| {
                (s.scans.clone(), s.parses.clone(), s.worker_gate.take())
            }) {
                Ok(x) => x,
                Err(_) => return,
            };
            let result = cx
                .background_executor()
                .spawn(async move {
                    #[cfg(test)]
                    {
                        scans.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if let Some(gate) = gate {
                            let _ = gate.await;
                        }
                    }
                    if control.cancelled() {
                        return None;
                    }
                    #[cfg(test)]
                    parses.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let text = source.text.rope().to_string();
                    let verdict = model::json_validation_controlled(&text, &control)?;
                    let diagnostic =
                        verdict
                            .as_ref()
                            .ok()
                            .and_then(|v| v.as_ref().err())
                            .map(|error| {
                                let bom = usize::from(text.starts_with('\u{feff}')) * 3;
                                let start = text[bom..]
                                    .split_inclusive('\n')
                                    .take(error.line.saturating_sub(1))
                                    .map(str::len)
                                    .sum::<usize>();
                                let mut offset =
                                    (bom + start + error.column.saturating_sub(1)).min(text.len());
                                while !text.is_char_boundary(offset) {
                                    offset -= 1;
                                }
                                JsonError {
                                    line: error.line,
                                    column: error.column,
                                    message: error
                                        .message
                                        .rsplit_once(" at line ")
                                        .map_or_else(|| error.message.clone(), |(s, _)| s.into()),
                                    offset,
                                }
                            });
                    Some(Arc::new(JsonSnapshot {
                        source,
                        verdict,
                        diagnostic,
                    }))
                })
                .await;
            let _ = file.update(cx, |state, cx| {
                state.parsing = false;
                if state.generation != generation {
                    state.schedule(cx);
                    return;
                }
                let Some(result) =
                    result.filter(|r| r.source.stamp == state.source.read(cx).source_stamp())
                else {
                    return;
                };
                let valid = matches!(&result.verdict, Ok(Ok(())));
                let old_valid = if valid {
                    state.last_valid.replace(result.clone())
                } else {
                    None
                };
                let old = state.current.replace(result.clone());
                // Replacing a whole source revision can retire a large Rope.
                cx.background_executor()
                    .spawn(async move {
                        drop((old, old_valid));
                    })
                    .detach();
                #[cfg(test)]
                {
                    state.installs += 1;
                }
                state.source.update(cx, |input, cx| {
                    use gpui_base::input::{Diagnostic, DiagnosticSeverity, RopeExt};
                    use gpui_component::input::Position;
                    let text = input.text().clone();
                    if let Some(diagnostics) = input.diagnostics_mut() {
                        diagnostics.reset(&text);
                        if let Some(error) = &result.diagnostic {
                            let start = text.offset_to_position(error.offset);
                            diagnostics.push(Diagnostic {
                                range: start..Position {
                                    character: start.character + 1,
                                    ..start
                                },
                                severity: DiagnosticSeverity::Error,
                                message: error.message.clone().into(),
                                ..Default::default()
                            });
                        }
                    }
                    cx.notify();
                });
                cx.notify();
            });
        }));
    }
}

impl Reader {
    pub(crate) fn file_conflict_in_status(&self, notice: &str) -> bool {
        self.file_editor_active()
            && self.editing.as_ref().is_some_and(|e| e.conflict_detected)
            && notice.starts_with(CONFLICT_NOTICE)
    }

    pub(crate) fn file_editor_active(&self) -> bool {
        self.editing
            .as_ref()
            .is_some_and(|e| e.file_chrome.is_some())
    }
    pub(crate) fn install_file_editor_chrome(
        &mut self,
        saved: Option<PresentationState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let source = self.editing.as_ref().unwrap().input.clone();
        let reader = cx.entity();
        let json = Path::new(self.selected_file())
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json"));
        let file = cx.new(|cx| FileChrome::new(reader, source, saved, json, window, cx));
        file.update(cx, |file, cx| {
            if file.json {
                file.schedule(cx);
            }
        });
        self.editing.as_mut().unwrap().file_chrome = Some(file);
    }

    pub(crate) fn file_editor_find_target(
        &self,
        cx: &App,
    ) -> Option<crate::reader_find_field::Target> {
        let file = self.editing.as_ref()?.file_chrome.as_ref()?.read(cx);
        let mut target = EditorTarget::target(&file.find, cx);
        target.large_file = true; // Edit never replaces its field with an icon.
        Some(target)
    }

    pub(crate) fn file_editor_read_state(&self, cx: &App) -> Option<PresentationState> {
        let file = self.editing.as_ref()?.file_chrome.as_ref()?.read(cx);
        let mut saved = file.read_state.clone()?;
        saved.query = file.find.read(cx).query.read(cx).value().to_string();
        saved.case_sensitive = reader_ui_state::find_case_sensitive(cx);
        Some(saved)
    }

    pub(crate) fn file_editor_chrome(&self, cx: &App, live: bool) -> Option<ViewerChrome> {
        let file = self.editing.as_ref()?.file_chrome.as_ref()?.read(cx);
        let mut chrome = ViewerChrome {
            compact_views: true,
            ..Default::default()
        };
        let mut find = if live {
            EditorTarget::target(&file.find, cx)
        } else {
            EditorTarget::descriptor(&file.find, cx)
        };
        find.large_file = true;
        chrome.find = Some(find);
        let unavailable = reader_shortcuts::hint(
            "Not available while editing. Read brings it back",
            &ToggleSource,
            cx,
        );
        for view in file
            .read_state
            .as_ref()
            .map(|s| s.views.as_slice())
            .unwrap_or(&[])
        {
            let reader = file.reader.clone();
            let mut action = ViewerAction::new(
                view.id(),
                view.icon(),
                view.label(),
                Rc::new(move |w, cx| {
                    let _ = reader.update(cx, |r, cx| {
                        if let Some(input) = r.source_input() {
                            input.update(cx, |i, cx| i.focus(w, cx));
                        }
                    });
                }),
            );
            action.enabled = *view == View::Raw;
            if !action.enabled {
                action.disabled_reason = Some(unavailable.clone());
            }
            chrome.views.push(ViewerView {
                action,
                active: *view == View::Raw,
            });
        }
        if chrome.views.len() < 2 {
            chrome.views.clear();
        }
        if crate::reader_code_file::language(self.selected_file()).is_some_and(|l| l != "text") {
            for folded in [true, false] {
                let action = if folded {
                    crate::reader_code_file::FoldAll.boxed_clone()
                } else {
                    crate::reader_code_file::UnfoldAll.boxed_clone()
                };
                let mut fold = ViewerAction::new(
                    if folded {
                        "code-fold-all"
                    } else {
                        "code-unfold-all"
                    },
                    Icon::default().path(if folded {
                        brand::READER_COLLAPSE_ICON
                    } else {
                        brand::READER_EXPAND_ICON
                    }),
                    if folded { "Fold all" } else { "Unfold all" },
                    Rc::new(|_, _| {}),
                );
                fold.enabled = false;
                fold.disabled_reason = Some(reader_shortcuts::hint(
                    "Folding is not available while editing. Read brings it back",
                    &ToggleSource,
                    cx,
                ));
                fold.shortcut = Some(Rc::from(action));
                chrome.actions.push(fold);
            }
        }
        if Path::new(self.selected_file()).extension().is_none()
            || crate::reader_code_file::language(self.selected_file()).is_none_or(|l| l == "text")
        {
            let mut language = ViewerAction::new(
                "file-edit-language",
                Icon::default().path("icons/code-xml.svg"),
                "Language",
                Rc::new(|_, _| {}),
            );
            language.minimum_width = 68.;
            language.live = Some(file.language.clone());
            chrome.actions.push(language);
        }
        let reader = file.reader.clone();
        let mut format = ViewerAction::new(
            "file-edit-format",
            Icon::default().path("icons/indent.svg"),
            "Format",
            Rc::new(move |w, cx| {
                let _ = reader.update(cx, |r, cx| r.code_operation(Action::Format, w, cx));
            }),
        )
        .with_action(FormatCode, cx);
        format.never_dropped = true;
        format.minimum_width = 104.;
        format.filled = true;
        format.live = Some(file.format.clone());
        chrome.actions.push(format);
        if file.json {
            let reader = file.reader.clone();
            let mut warning = ViewerAction::new(
                "file-edit-state",
                Icon::new(IconName::TriangleAlert),
                "JSON validity",
                Rc::new(move |w, cx| {
                    let _ = reader.update(cx, |r, cx| r.file_editor_go_to_error(w, cx));
                }),
            );
            warning.live = Some(file.badge.clone());
            chrome.state = Some(warning);
        }
        Some(chrome)
    }

    pub(crate) fn file_editor_go_to_error(&self, window: &mut Window, cx: &mut App) {
        let Some(editing) = &self.editing else {
            return;
        };
        let Some(file) = &editing.file_chrome else {
            return;
        };
        let state = file.read(cx);
        let offset = state
            .current
            .as_ref()
            .filter(|s| s.source.stamp == editing.input.read(cx).source_stamp())
            .and_then(|s| s.diagnostic.as_ref())
            .map(|e| e.offset);
        let position = {
            editing
                .operations
                .status
                .as_ref()
                .and_then(|(_, range, _, s)| {
                    location(s).map(|(line, column)| (line, column, range.start))
                })
        };
        if offset.is_some() || position.is_some() {
            let find = state.find.clone();
            find.update(cx, |f, _| f.preserve_selection());
            editing.input.update(cx, |i, cx| {
                use gpui_base::input::RopeExt;
                let at = offset.unwrap_or_else(|| {
                    let (line, column, base) = position.unwrap();
                    let line_start =
                        i.text()
                            .position_to_offset(&gpui_component::input::Position {
                                line: line.saturating_sub(1) as u32,
                                character: 0,
                            });
                    (line_start + column.saturating_sub(1) + if line == 1 { base } else { 0 })
                        .min(i.text().len())
                });
                i.set_selected_range(at..at, cx);
                i.focus(window, cx);
            });
        }
    }

    pub(crate) fn repair_code_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_preview.is_some() && self.editing.is_none() {
            self.toggle_source(window, cx);
        }
        if self.editing.is_some() {
            self.code_operation(Action::Repair, window, cx);
        }
    }

    pub(crate) fn file_editor_bottom(&self, cx: &App) -> Option<AnyElement> {
        Some(
            self.editing
                .as_ref()?
                .file_chrome
                .as_ref()?
                .read(cx)
                .bottom
                .clone()
                .into_any_element(),
        )
    }
    pub(crate) fn file_editor_sheet(&self, cx: &App) -> Option<AnyElement> {
        Some(
            self.editing
                .as_ref()?
                .file_chrome
                .as_ref()?
                .read(cx)
                .sheet
                .clone()
                .into_any_element(),
        )
    }

    pub(crate) fn publish_file_operation_notice(&mut self, window: &mut Window, cx: &mut App) {
        let Some(editing) = self.editing.as_mut().filter(|e| !e.markdown) else {
            return;
        };
        let Some(text) = editing.operations.notice.as_deref() else {
            return;
        };
        let toast = if text.starts_with("Formatted.") {
            Some("Formatted. Undo restores the original text.")
        } else if text.starts_with("Repair applied") {
            Some("Repair applied as one Undo step.")
        } else if text.starts_with("Source changed") {
            Some("The text changed while this ran. Nothing was applied.")
        } else if text.starts_with("Repair preview is stale") {
            Some("The text changed. Open Repair again.")
        } else if text == "Already formatted." {
            Some("Already formatted.")
        } else if text.starts_with("JSON is already valid") {
            Some("JSON is already valid. No repair is needed.")
        } else if matches!(text, "Operation cancelled." | "Repair cancelled.") {
            Some(text)
        } else {
            None
        };
        if let Some(toast) = toast {
            let toast = toast.to_owned();
            editing.operations.notice = None;
            editing.operations.notice_stamp = None;
            reader_toast::transient(toast, window, cx);
        }
    }
}

fn disabled_reason(reader: &Reader, cx: &App) -> Option<String> {
    let editing = reader.editing.as_ref()?;
    if editing.conflict_detected {
        Some(
            "This file changed on disk. Resolve the conflict to use Format, Validate and Repair."
                .into(),
        )
    } else if editing.save_failed {
        Some("Saving failed. Save again to use Format, Validate and Repair.".into())
    } else if editing.operations.busy {
        Some(
            editing
                .operations
                .running
                .as_ref()
                .map(|r| r.action.busy_text())
                .unwrap_or("Working…")
                .into(),
        )
    } else if !editing.input.read(cx).is_editable() {
        Some("The editor is temporarily unavailable.".into())
    } else {
        None
    }
}

fn language_menu(menu: PopupMenu, owner: WeakEntity<Reader>, cx: &App) -> PopupMenu {
    let Some(scope) = owner.upgrade().and_then(|r| r.read(cx).code_scope(cx)) else {
        return menu;
    };
    model::LANGUAGES
        .iter()
        .copied()
        .chain(["jsx", "scss"])
        .fold(menu, |menu, name| {
            let owner = owner.clone();
            let scope = scope.clone();
            menu.item(
                PopupMenuItem::new(name)
                    .checked(scope.language == name)
                    .on_click(move |_, _, cx| {
                        let _ = owner.update(cx, |r, cx| r.select_code_language(&scope, name, cx));
                    }),
            )
        })
}

#[inline(never)]
fn language_control(reader: &Reader, state: &FileChrome, cx: &App) -> AnyElement {
    let language = reader
        .code_scope_for_display(cx, true)
        .map(|s| s.language)
        .unwrap_or_else(|| "text".into());
    let owner = state.reader.clone();
    Button::new("file-language")
        .w_full()
        .small()
        .outline()
        .h(px(28.))
        .label(language.to_uppercase())
        .dropdown_caret(true)
        .disabled(disabled_reason(reader, cx).is_some())
        .debug_selector(|| "file-language".into())
        .tooltip("Language used by Format, Validate and Repair")
        .dropdown_menu(move |m, _, cx| language_menu(m, owner.clone(), cx))
        .into_any_element()
}

#[inline(never)]
fn format_control(reader: &Reader, state: &FileChrome, cx: &App) -> AnyElement {
    let owner = state.reader.clone();
    let primary = owner.clone();
    let reason = disabled_reason(reader, cx);
    #[cfg(test)]
    DRAWN_FORMAT.with(|f| {
        f.borrow_mut()
            .insert(state.source.entity_id(), reason.clone());
    });
    let tooltip = reason.clone().map(SharedString::from).unwrap_or_else(|| {
        reader_shortcuts::hint(
            "Format code: rewrites the text, Undo restores it",
            &FormatCode,
            cx,
        )
    });
    let language_bounds = state.language.bounds.clone();
    let split = DropdownButton::new("file-format-split")
        .small()
        .primary()
        .w(px(104.))
        .h(px(28.))
        .button(
            Button::new("file-format")
                .label("Format")
                .icon(Icon::default().path("icons/indent.svg").size(px(14.)))
                .text_size(px(12.))
                .px_2()
                .w(px(76.))
                .h(px(28.))
                .tooltip(tooltip)
                .disabled(reason.is_some())
                .debug_selector(|| "file-format".into())
                .on_click(move |_, w, cx| {
                    let _ = primary.update(cx, |r, cx| r.code_operation(Action::Format, w, cx));
                }),
        )
        .dropdown_menu(move |mut menu, window, cx| {
            #[cfg(test)]
            FORMAT_POPUP.with(|m| *m.borrow_mut() = Some((cx.entity().downgrade(), Vec::new())));
            let Some(reader) = owner.upgrade() else {
                return menu;
            };
            let reason = disabled_reason(reader.read(cx), cx);
            for (name, action, command) in [
                ("Format code", Action::Format, FormatCode.boxed_clone()),
                (
                    "Validate code",
                    Action::Validate,
                    ValidateCode.boxed_clone(),
                ),
                ("Repair code…", Action::Repair, RepairCode.boxed_clone()),
            ] {
                let owner = owner.clone();
                let label = reason
                    .as_ref()
                    .map_or_else(|| name.to_owned(), |reason| format!("{name} — {reason}"));
                let item = PopupMenuItem::new(label)
                    .disabled(reason.is_some())
                    .action(command)
                    .on_click(move |_, w, cx| {
                        let _ = owner.update(cx, |r, cx| r.code_operation(action, w, cx));
                    });
                #[cfg(test)]
                if let PopupMenuItem::Item {
                    label, disabled, ..
                } = &item
                {
                    FORMAT_POPUP.with(|m| {
                        m.borrow_mut()
                            .as_mut()
                            .unwrap()
                            .1
                            .push((label.to_string(), *disabled))
                    });
                }
                menu = menu.item(item);
            }
            if language_bounds.get().is_none() {
                let language = reader
                    .read(cx)
                    .code_scope_for_display(cx, true)
                    .map(|s| s.language.to_uppercase())
                    .unwrap_or_else(|| "Text".into());
                let owner = owner.clone();
                menu = menu.separator().submenu(
                    format!("Language: {language}"),
                    window,
                    cx,
                    move |m, _, cx| language_menu(m, owner.clone(), cx),
                );
                #[cfg(test)]
                FORMAT_POPUP.with(|m| {
                    m.borrow_mut()
                        .as_mut()
                        .unwrap()
                        .1
                        .push((format!("Language: {language}"), false))
                });
            }
            menu
        });
    div()
        .debug_selector(|| "file-format-split".into())
        .flex_none()
        .w(px(104.))
        .h(px(28.))
        .child(split)
        .into_any_element()
}

#[inline(never)]
fn badge(_reader: &Reader, state: &FileChrome, cx: &App) -> AnyElement {
    let Some(snapshot) = &state.current else {
        return div().size_full().into_any_element();
    };
    let Some(error) = &snapshot.diagnostic else {
        return div().size_full().into_any_element();
    };
    let owner = state.reader.clone();
    reader_icon_button(
        "file-json-warning",
        IconName::TriangleAlert,
        format!(
            "Not valid JSON. Line {}, column {}. Click to go there",
            error.line, error.column
        ),
        cx,
    )
    .text_color(brand::palette(cx).warning)
    .bg(brand::reader_palette(cx).warning_tint)
    .debug_selector(|| "file-json-warning".into())
    .on_click(move |_, w, cx| {
        let _ = owner.update(cx, |r, cx| r.file_editor_go_to_error(w, cx));
    })
    .into_any_element()
}

fn location(text: &str) -> Option<(usize, usize)> {
    let at = text
        .split(" · ")
        .nth(1)
        .or_else(|| text.split("invalid at ").nth(1))?;
    let (line, rest) = at.split_once(':')?;
    let column = rest.split(|c: char| !c.is_ascii_digit()).next()?;
    Some((line.parse().ok()?, column.parse().ok()?))
}

/// Translate legacy operation outcomes at the file host, leaving note wording
/// and every source-operation result/guard unchanged.
fn status_text(text: &str, language: &str, operation: Option<Action>, bytes: usize) -> String {
    if text == "Valid strict JSON" {
        return "Valid JSON.".into();
    }
    if let Some((line, column)) = location(text) {
        if text.starts_with("Invalid JSON") {
            return format!(
                "Not valid JSON. Line {line}, column {column}: {}",
                text.split(" · ").nth(2).unwrap_or("syntax error")
            );
        }
        if text.starts_with("Syntax error") {
            return format!(
                "Syntax problem in {}. Line {line}, column {column}.",
                language.to_uppercase()
            );
        }
        if text.starts_with("JSON is invalid") {
            return format!(
                "Not formatted. The text has a syntax error on line {line}. Nothing was changed."
            );
        }
    }
    if text.starts_with("Validation unavailable above 256 KiB")
        || text.starts_with("Syntax validation for")
    {
        return format!(
            "Too large to validate. {} KB is over the 256 KB limit.",
            bytes.div_ceil(1024)
        );
    }
    if text.starts_with("Validation unavailable") {
        return format!(
            "Validation is not available for {}. Nothing was checked.",
            language.to_uppercase()
        );
    }
    if text.starts_with("Syntax checked") {
        return format!(
            "No syntax problems found. Full validation is not available for {}.",
            language.to_uppercase()
        );
    }
    if text.starts_with("Syntax check exceeded") {
        return "Syntax check stopped: it took too long. Nothing was changed.".into();
    }
    if text == "Validation not checked for this revision" {
        return "Not validated since your last change.".into();
    }
    match operation {
        Some(Action::Format) => format!("Not formatted. {text} Nothing was changed."),
        Some(Action::Repair) => format!("No safe repair. {text} Nothing was changed."),
        _ => text.into(),
    }
}

#[inline(never)]
fn bottom(reader: &Reader, state: &FileChrome, cx: &App) -> AnyElement {
    use crate::reader_status_line::{StatusLine, Tone};
    let editing = reader.editing.as_ref().unwrap();
    let ops = &editing.operations;
    let language = reader
        .code_scope_for_display(cx, true)
        .map(|s| s.language)
        .unwrap_or_else(|| "text".into());
    let mut status = StatusLine::default();
    let mut trailing = None;
    if editing.conflict_detected || editing.save_failed {
        status.text = disabled_reason(reader, cx).unwrap().into();
        status.tone = Tone::Warning;
        let owner = state.reader.clone();
        let mut park = ViewerAction::new(
            "source-park",
            Icon::new(IconName::ArrowRight),
            "Close, keeping draft",
            Rc::new(move |w, cx| {
                let _ = owner.update(cx, |r, cx| r.park_source(w, cx));
            }),
        );
        park.labelled = true;
        status.actions.push(park);
        trailing = Some(conflict_menu(
            state.reader.clone(),
            editing.conflict_detected,
            cx,
        ));
    } else if ops.busy {
        status.text = ops
            .running
            .as_ref()
            .map(|r| r.action.busy_text())
            .unwrap_or("Working…")
            .into();
        let owner = state.reader.clone();
        let mut cancel = ViewerAction::new(
            "file-operation-cancel",
            Icon::new(IconName::Close),
            "Cancel",
            Rc::new(move |_, cx| {
                let _ = owner.update(cx, |r, cx| r.cancel_code_operation(cx));
            }),
        );
        cancel.labelled = true;
        status.actions.push(cancel);
    } else if let Some(notice) = &ops.notice {
        status.text = status_text(
            notice,
            &language,
            ops.last_action,
            state.source.read(cx).text().len(),
        )
        .into();
    } else if let Some((_, _, _, result)) = ops.status.as_ref().filter(|(stamp, _, _, result)| {
        *stamp == state.source.read(cx).source_stamp()
            && !result.starts_with("Valid strict JSON")
            && !result.starts_with("Invalid JSON")
    }) {
        status.text =
            status_text(result, &language, None, state.source.read(cx).text().len()).into();
    } else if state.json {
        match state.current.as_ref().map(|s| &s.verdict) {
            Some(Ok(Ok(()))) => {
                status.label = "Valid JSON.".into();
                status.icon = Some(Icon::new(IconName::Check));
            }
            Some(Ok(Err(_))) => {
                let error = state.current.as_ref().unwrap().diagnostic.as_ref().unwrap();
                status.label = "Not valid JSON.".into();
                status.text = format!(
                    "Line {}, column {}: {}",
                    error.line, error.column, error.message
                )
                .into();
                status.tone = Tone::Warning;
            }
            Some(Err(reason)) => status.text = reason.clone().into(),
            None => status.text = "Checking JSON…".into(),
        }
    } else {
        let current = ops
            .status
            .as_ref()
            .filter(|(stamp, _, _, _)| *stamp == state.source.read(cx).source_stamp())
            .map(|(_, _, _, s)| s.as_str())
            .unwrap_or("Validation not checked for this revision");
        status.text =
            status_text(current, &language, None, state.source.read(cx).text().len()).into();
        if current == "Validation not checked for this revision" {
            let owner = state.reader.clone();
            let mut validate = ViewerAction::new(
                "file-status-validate",
                Icon::new(IconName::Check),
                "Validate",
                Rc::new(move |w, cx| {
                    let _ = owner.update(cx, |r, cx| r.code_operation(Action::Validate, w, cx));
                }),
            )
            .with_action(ValidateCode, cx);
            validate.labelled = true;
            status.actions.push(validate);
        }
    }
    if !editing.conflict_detected && !editing.save_failed && !ops.busy {
        let has_error = state
            .current
            .as_ref()
            .is_some_and(|s| s.diagnostic.is_some())
            || ops
                .status
                .as_ref()
                .is_some_and(|(_, _, _, s)| location(s).is_some());
        if has_error {
            let owner = state.reader.clone();
            let mut go = ViewerAction::new(
                "file-go-to-error",
                Icon::new(IconName::ArrowRight),
                "Go to error",
                Rc::new(move |w, cx| {
                    let _ = owner.update(cx, |r, cx| r.file_editor_go_to_error(w, cx));
                }),
            );
            go.labelled = true;
            status.actions.insert(0, go);
            if state.json {
                let owner = state.reader.clone();
                let mut repair = ViewerAction::new(
                    "file-status-repair",
                    Icon::default().path("icons/wrench.svg"),
                    "Repair…",
                    Rc::new(move |w, cx| {
                        let _ = owner.update(cx, |r, cx| r.code_operation(Action::Repair, w, cx));
                    }),
                )
                .with_action(RepairCode, cx);
                repair.labelled = true;
                status.actions.push(repair);
            }
        }
    }
    #[cfg(test)]
    DRAWN_STATUS.with(|all| {
        let mut all = all.borrow_mut();
        let previous = all.get(&state.source.entity_id()).map_or(0, |s| s.0);
        all.insert(
            state.source.entity_id(),
            (
                previous + 1,
                format!("{} {}", status.label, status.text)
                    .trim()
                    .to_owned(),
                status.actions.iter().take(2).map(|a| a.id).collect(),
            ),
        );
    });
    crate::reader_status_line::render(&status, "file-edit-status", trailing, cx)
}

#[inline(never)]
fn conflict_menu(owner: WeakEntity<Reader>, conflict: bool, _cx: &App) -> AnyElement {
    Button::new("file-conflict-actions")
        .small()
        .label("Resolve…")
        .debug_selector(|| "file-conflict-actions".into())
        .dropdown_menu(move |mut menu, _, _| {
            for (label, kind) in [
                ("Save again", 0),
                ("Reload from disk", 1),
                ("Keep my version", 2),
                ("Keep both versions", 3),
                ("Compare versions", 4),
                ("Copy draft", 5),
            ] {
                if !conflict && matches!(kind, 1..=4) {
                    continue;
                }
                let owner = owner.clone();
                menu = menu.item(PopupMenuItem::new(label).on_click(move |_, w, cx| {
                    let _ = owner.update(cx, |r, cx| match kind {
                        0 | 3 => {
                            r.save_source_explicit(cx);
                        }
                        1 => r.resolve_source(true, w, cx),
                        2 => r.resolve_source(false, w, cx),
                        4 => {
                            if let Some(e) = &mut r.editing {
                                e.compare = !e.compare;
                                if let Some(text) = &e.conflict {
                                    e.current_input
                                        .update(cx, |i, cx| i.set_value(text.clone(), w, cx));
                                }
                            }
                            cx.notify();
                        }
                        _ => {
                            if let Some(input) = r.source_input() {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    input.read(cx).value().to_string(),
                                ));
                            }
                        }
                    });
                }));
            }
            menu
        })
        .into_any_element()
}

#[inline(never)]
fn sheet(reader: &Reader, window: &Window, cx: &App) -> AnyElement {
    let Some(preview) = reader
        .editing
        .as_ref()
        .and_then(|e| e.operations.preview.as_ref())
    else {
        return div().absolute().into_any_element();
    };
    let owner = reader
        .editing
        .as_ref()
        .unwrap()
        .file_chrome
        .as_ref()
        .unwrap()
        .read(cx)
        .reader
        .clone();
    let cancel = owner.clone();
    let apply = owner;
    let header = h_flex()
        .gap_2()
        .min_w_0()
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child("Repair preview"),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .text_color(brand::palette(cx).text_muted)
                .child(preview.explanation.clone()),
        )
        .child(
            Button::new("file-repair-cancel")
                .small()
                .label("Cancel")
                .debug_selector(|| "file-repair-cancel".into())
                .on_click(move |_, w, cx| {
                    let _ = cancel.update(cx, |r, cx| {
                        if let Some(e) = &mut r.editing {
                            e.operations.preview = None;
                            e.operations.notice = None;
                        }
                        reader_toast::transient("Repair cancelled.", w, cx);
                        cx.notify();
                    });
                }),
        )
        .child(
            Button::new("file-repair-apply")
                .small()
                .primary()
                .label("Apply repair")
                .debug_selector(|| "file-repair-apply".into())
                .on_click(move |_, w, cx| {
                    let _ = apply.update(cx, |r, cx| r.apply_code_repair(w, cx));
                }),
        )
        .into_any_element();
    let pane = |name: &'static str, input: &Entity<EditorState>| {
        v_flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .gap_1()
            .child(
                div()
                    .text_sm()
                    .text_color(brand::palette(cx).text_muted)
                    .child(name),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(Editor::new(input).readonly(true).size_full()),
            )
            .into_any_element()
    };
    let columns = h_flex()
        .flex_1()
        .min_h_0()
        .gap_3()
        .child(pane("Original (unchanged)", &preview.before))
        .child(pane("Proposed repair", &preview.after))
        .into_any_element();
    v_flex()
        .debug_selector(|| "file-repair-sheet".into())
        .absolute()
        .left_0()
        .right_0()
        .bottom(px(28.))
        .h((window.viewport_size().height * 0.4)
            .min(px(320.))
            .max(px(120.)))
        .p_3()
        .gap_2()
        .border_t_1()
        .border_color(brand::palette(cx).border)
        .bg(brand::palette(cx).surface)
        .child(header)
        .child(columns)
        .into_any_element()
}

#[cfg(test)]
mod tests;

#[cfg(feature = "data-view-native-harness")]
mod native;
