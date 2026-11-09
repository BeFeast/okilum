//! Explicit source operations. The reader protocol and read-only preview never
//! gain write authority; every accepted edit uses the normal source Undo/save.
use super::*;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use okilum_core::code_operations as model;
use std::{
    ops::Range,
    time::{Duration, Instant},
};

gpui::actions!(okilum, [FormatCode, ValidateCode, RepairCode]);

#[derive(Default)]
pub(super) struct State {
    language: Option<(usize, String)>,
    generation: u64,
    busy: bool,
    status: Option<(SourceStamp, Range<usize>, String, String)>,
    notice: Option<String>,
    preview: Option<Preview>,
    inventory: Option<Inventory>,
    inventory_requested: Option<SourceStamp>,
    inventory_task: Option<Task<()>>,
}

struct Inventory {
    stamp: SourceStamp,
    blocks: Vec<code_comforts::Block>,
    line_ending: model::LineEnding,
}

impl State {
    pub(super) fn park(&mut self) {
        // A completion may arrive while the editor is temporarily removed or
        // after it is reassembled. Retire that request in either case.
        *self = Self {
            generation: self.generation.wrapping_add(1),
            language: self.language.take(),
            inventory: self.inventory.take(),
            ..Default::default()
        };
    }

    pub(super) fn source_changed(&mut self, stamp: SourceStamp) -> bool {
        let stale = self
            .status
            .as_ref()
            .is_some_and(|(known, _, _, _)| *known != stamp);
        if stale {
            self.status = None;
        }
        stale
    }
}

#[derive(Clone)]
struct Target {
    range: Range<usize>,
    language: String,
    text: String,
    stamp: SourceStamp,
    line_ending: model::LineEnding,
}

#[derive(Clone)]
struct Scope {
    range: Range<usize>,
    language: String,
    stamp: SourceStamp,
    line_ending: Option<model::LineEnding>,
}

struct Preview {
    target: Target,
    proposed: String,
    explanation: String,
    before: Entity<EditorState>,
    after: Entity<EditorState>,
}

#[derive(Clone, Copy)]
enum Action {
    Validate,
    Format,
    Repair,
}

fn validation(language: &str, source: &str) -> String {
    if source.len() > model::MAX_BYTES {
        return "Validation unavailable above 256 KiB; source unchanged.".into();
    }
    if language == "json" {
        return match model::json_validation(source) {
            Ok(()) => "Valid strict JSON".into(),
            Err(e) => format!("Invalid JSON · {}:{} · {}", e.line, e.column, e.message),
        };
    }
    // A highlighting grammar is only a syntax check, never semantic validation.
    // Dialects without a matching grammar must not borrow a stricter language.
    if matches!(language, "jsonc" | "jsx" | "scss" | "text") {
        return format!("Validation unavailable for {language}; no other dialect substituted.");
    }
    let Some(grammar) = gpui_component::highlighter::LanguageRegistry::singleton()
        .language(language)
        .and_then(|config| config.language)
    else {
        return format!("Validation unavailable for {language}; no grammar installed.");
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&grammar).is_err() {
        return format!("Validation unavailable for {language}; grammar could not load.");
    }
    let deadline = Instant::now() + Duration::from_millis(250);
    let mut progress = |_: &tree_sitter::ParseState| {
        if Instant::now() >= deadline {
            std::ops::ControlFlow::Break(())
        } else {
            std::ops::ControlFlow::Continue(())
        }
    };
    let Some(tree) = parser.parse_with_options(
        &mut |at, _| source.as_bytes().get(at..).unwrap_or_default(),
        None,
        Some(tree_sitter::ParseOptions::new().progress_callback(&mut progress)),
    ) else {
        return "Syntax check exceeded its budget; source unchanged.".into();
    };
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        if node.is_error() || node.is_missing() {
            let at = node.start_position();
            return format!(
                "Syntax error ({language}) · {}:{} · {}{}",
                at.row + 1,
                at.column + 1,
                if node.is_missing() {
                    "missing "
                } else {
                    "unexpected "
                },
                node.kind()
            );
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return format!(
                    "Syntax checked ({language}); semantic validation is not available."
                );
            }
        }
    }
}

fn file_language(rel: &str) -> String {
    let ext = Path::new(rel)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    match ext.to_ascii_lowercase().as_str() {
        "jsonc" => "jsonc",
        "jsx" => "jsx",
        "scss" => "scss",
        _ => crate::reader_code_file::language(rel).unwrap_or("text"),
    }
    .into()
}

impl Reader {
    pub(super) fn schedule_code_inventory(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.as_mut().filter(|e| e.markdown) else {
            return;
        };
        let input_id = editing.input.entity_id();
        let stamp = editing.input.read(cx).source_stamp();
        if editing.operations.inventory_requested == Some(stamp)
            || editing
                .operations
                .inventory
                .as_ref()
                .is_some_and(|i| i.stamp == stamp)
        {
            return;
        }
        editing.operations.inventory_requested = Some(stamp);
        // Debounce revisions and cancel superseded requests before taking a
        // snapshot. No source materialization/scanning belongs on the UI path.
        // Keep only an EntityId across waits: a closed app must not retain its
        // native input while cancellation of a pending timer is being drained.
        editing.operations.inventory_task = Some(cx.spawn(async move |reader, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            let snapshot = reader
                .update(cx, |reader, cx| {
                    let editing = reader
                        .editing
                        .as_ref()
                        .filter(|e| e.input.entity_id() == input_id)?;
                    let state = editing.input.read(cx);
                    (state.source_stamp() == stamp
                        && editing.operations.inventory_requested == Some(stamp))
                    .then(|| state.source_snapshot())
                })
                .ok()
                .flatten();
            let Some(snapshot) = snapshot else {
                return;
            };
            let inventory = cx
                .background_executor()
                .spawn(async move {
                    let text = snapshot.text.rope().to_string();
                    Inventory {
                        stamp,
                        blocks: code_comforts::blocks(&text),
                        line_ending: model::line_ending(&text),
                    }
                })
                .await;
            let _ = reader.update(cx, |reader, cx| {
                let before = reader.code_operation_target_key(cx);
                let Some(editing) = reader
                    .editing
                    .as_mut()
                    .filter(|e| e.input.entity_id() == input_id)
                else {
                    return;
                };
                if editing.input.read(cx).source_stamp() != stamp
                    || editing.operations.inventory_requested != Some(stamp)
                {
                    return;
                }
                editing.operations.inventory = Some(inventory);
                if before != reader.code_operation_target_key(cx) {
                    cx.notify();
                }
            });
        }));
    }

    fn code_scope(&self, cx: &App) -> Option<Scope> {
        let editing = self.editing.as_ref()?;
        let input = editing.input.read(cx);
        let stamp = input.source_stamp();
        let (range, language, line_ending) = if editing.markdown {
            let selection = input.selected_range();
            let inventory = editing
                .operations
                .inventory
                .as_ref()
                .filter(|i| i.stamp == stamp)?;
            let at = inventory
                .blocks
                .partition_point(|block| block.body.end < selection.start);
            let block = inventory.blocks.get(at).filter(|block| {
                block.body.start <= selection.start && selection.end <= block.body.end
            })?;
            (
                block.body.clone(),
                block.language.clone().unwrap_or_else(|| "text".into()),
                Some(inventory.line_ending),
            )
        } else {
            (
                0..input.text().len(),
                file_language(self.selected_file()),
                None,
            )
        };
        let language = editing
            .operations
            .language
            .as_ref()
            .filter(|(start, _)| *start == range.start)
            .map(|(_, name)| name.clone())
            .unwrap_or(language);
        Some(Scope {
            line_ending,
            range,
            language,
            stamp,
        })
    }

    fn code_target(&self, cx: &App) -> Option<Target> {
        let scope = self.code_scope(cx)?;
        let input = self.editing.as_ref()?.input.read(cx);
        let text = input
            .source_snapshot()
            .text
            .rope()
            .slice(scope.range.clone())
            .to_string();
        Some(Target {
            line_ending: scope
                .line_ending
                .unwrap_or_else(|| model::line_ending(&text)),
            range: scope.range,
            language: scope.language,
            stamp: scope.stamp,
            text,
        })
    }

    fn code_operation(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.code_target(cx) else {
            return;
        };
        if !self.ime_allows(ime_guard::Trigger::Text, cx) {
            return;
        }
        let editing = self.editing.as_mut().unwrap();
        if editing.operations.busy
            || editing.conflict_detected
            || editing.save_failed
            || !editing.input.read(cx).is_editable()
        {
            return;
        }
        editing.operations.generation = editing.operations.generation.wrapping_add(1);
        let generation = editing.operations.generation;
        editing.operations.busy = true;
        editing.operations.notice = None;
        let input = editing.input.clone();
        crate::source_presentation::code_languages::install();
        let work = target.clone();
        let task = cx.background_executor().spawn(async move {
            let status = validation(&work.language, &work.text);
            let format = matches!(action, Action::Format).then(|| {
                model::format_with_line_ending(&work.language, &work.text, work.line_ending)
            });
            let repair =
                matches!(action, Action::Repair).then(|| model::repair(&work.language, &work.text));
            (status, format, repair)
        });
        cx.spawn_in(window, async move |this, cx| {
            let (mut status, format, repair) = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(editing) = this.editing.as_mut().filter(|e| e.input == input) else {
                    return;
                };
                if editing.operations.generation != generation {
                    return;
                }
                editing.operations.busy = false;
                if input.read(cx).source_stamp() != target.stamp
                    || editing.conflict_detected
                    || editing.save_failed
                    || !input.read(cx).is_editable()
                {
                    editing.operations.notice =
                        Some("Source changed; run the operation again. No result applied.".into());
                    cx.notify();
                    return;
                }
                let mut status_range = target.range.clone();
                match format {
                    Some(model::Formatted::Changed(text)) => {
                        // The exact native revision was checked immediately above.
                        input.update(cx, |state, cx| {
                            state.replace_source_range(
                                target.range.clone(),
                                text.as_str(),
                                window,
                                cx,
                            );
                            state.focus(window, cx);
                        });
                        status_range.end = status_range.start + text.len();
                        status = validation(&target.language, &text);
                        this.editing.as_mut().unwrap().operations.notice = Some(
                            "Formatted. Validation refreshed; Undo restores the original source."
                                .into(),
                        );
                    }
                    Some(model::Formatted::Unchanged(reason)) => {
                        editing.operations.notice = Some(reason)
                    }
                    None => {}
                }
                let editing = this.editing.as_mut().unwrap();
                editing.operations.status = Some((
                    input.read(cx).source_stamp(),
                    status_range,
                    target.language.clone(),
                    status,
                ));
                if let Some(result) = repair {
                    match result {
                        Err(reason) => editing.operations.notice = Some(reason),
                        Ok(repair) => {
                            let before = cx.new(|cx| {
                                preview_editor(&repair.source, &target.language, window, cx)
                            });
                            let after = cx.new(|cx| {
                                preview_editor(&repair.proposed, &target.language, window, cx)
                            });
                            editing.operations.preview = Some(Preview {
                                target,
                                proposed: repair.proposed,
                                explanation: repair.explanation,
                                before,
                                after,
                            });
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn apply_code_repair(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(current) = self.code_target(cx) else {
            return;
        };
        if !self.ime_allows(ime_guard::Trigger::Text, cx) {
            return;
        }
        let editing = self.editing.as_mut().unwrap();
        let Some(preview) = editing.operations.preview.as_ref() else {
            return;
        };
        if current.stamp != preview.target.stamp
            || current.range != preview.target.range
            || current.language != preview.target.language
            || current.text != preview.target.text
            || editing.conflict_detected
            || editing.save_failed
            || !editing.input.read(cx).is_editable()
        {
            editing.operations.notice = Some("Repair preview is stale or source is in conflict. Nothing applied; review a fresh proposal.".into());
            cx.notify();
            return;
        }
        let preview = editing.operations.preview.take().unwrap();
        let input = editing.input.clone();
        let status = validation(&preview.target.language, &preview.proposed);
        input.update(cx, |state, cx| {
            state.replace_source_range(preview.target.range, preview.proposed.as_str(), window, cx)
        });
        let editing = self.editing.as_mut().unwrap();
        editing.operations.status = Some((
            input.read(cx).source_stamp(),
            current.range.start..current.range.start + preview.proposed.len(),
            current.language,
            status,
        ));
        editing.operations.notice =
            Some("Repair applied as one Undo step. Save behavior is unchanged.".into());
        input.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    pub(super) fn render_code_operations(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let target = self.code_scope(cx)?;
        let editing = self.editing.as_ref()?;
        let state = &editing.operations;
        let owner = cx.entity().downgrade();
        let language = target.language.clone();
        let menu_target = target.clone();
        let disabled = state.busy
            || editing.conflict_detected
            || editing.save_failed
            || !editing.input.read(cx).is_editable();
        let selector = Button::new("source-code-language")
            .small()
            .ghost()
            .label(language.clone())
            .debug_selector(|| "source-code-language".into())
            .dropdown_menu(move |menu, _, _| {
                model::LANGUAGES
                    .iter()
                    .copied()
                    .chain(["jsx", "scss"])
                    .fold(menu, |menu, name| {
                        let owner = owner.clone();
                        let target = menu_target.clone();
                        menu.item(PopupMenuItem::new(name).checked(name == language).on_click(
                            move |_, _, cx| {
                                let _ = owner.update(cx, |this, cx| {
                                    this.select_code_language(&target, name, cx);
                                });
                            },
                        ))
                    })
            });
        let status = state
            .status
            .as_ref()
            .filter(|(stamp, range, language, _)| {
                *stamp == target.stamp && *range == target.range && *language == target.language
            })
            .map(|(_, _, _, text)| text.clone())
            .unwrap_or_else(|| "Validation not checked for this revision".into());
        let controls = h_flex()
            .gap_2()
            .flex_wrap()
            .child(selector)
            .child(
                Button::new("source-code-format")
                    .small()
                    .label("Format")
                    .disabled(disabled)
                    .debug_selector(|| "source-code-format".into())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.code_operation(Action::Format, window, cx)
                    })),
            )
            .child(
                Button::new("source-code-validate")
                    .small()
                    .ghost()
                    .label("Validate")
                    .disabled(disabled)
                    .debug_selector(|| "source-code-validate".into())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.code_operation(Action::Validate, window, cx)
                    })),
            )
            .child(
                Button::new("source-code-repair")
                    .small()
                    .ghost()
                    .label("Repair…")
                    .disabled(disabled)
                    .debug_selector(|| "source-code-repair".into())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.code_operation(Action::Repair, window, cx)
                    })),
            );
        let mut view = v_flex()
            .px_4()
            .py_2()
            .gap_1()
            .child(controls)
            .child(
                div()
                    .text_sm()
                    .debug_selector(|| "source-code-validation".into())
                    .child(status),
            )
            .children(
                state
                    .notice
                    .iter()
                    .map(|text| div().text_sm().child(text.clone())),
            );
        if let Some(preview) = &state.preview {
            let columns = h_flex()
                .gap_2()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child("Original (unchanged)")
                        .child(Editor::new(&preview.before).readonly(true).h(px(160.))),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child("Proposed repair")
                        .child(Editor::new(&preview.after).readonly(true).h(px(160.))),
                );
            view = view
                .child(div().text_sm().child(preview.explanation.clone()))
                .child(columns)
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("source-code-repair-apply")
                                .small()
                                .label("Apply repair")
                                .debug_selector(|| "source-code-repair-apply".into())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.apply_code_repair(window, cx)
                                })),
                        )
                        .child(
                            Button::new("source-code-repair-cancel")
                                .small()
                                .ghost()
                                .label("Cancel")
                                .debug_selector(|| "source-code-repair-cancel".into())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(editing) = this.editing.as_mut() {
                                        editing.operations.preview = None;
                                    }
                                    cx.notify();
                                })),
                        ),
                );
        }
        Some(view.into_any_element())
    }

    fn select_code_language(&mut self, target: &Scope, name: &str, cx: &mut Context<Self>) {
        // A menu can outlive its originating caret location or revision.
        // Never let an old item assign a language to a different code body.
        if self.code_scope(cx).is_none_or(|current| {
            current.stamp != target.stamp
                || current.range != target.range
                || current.language != target.language
        }) {
            return;
        }
        let Some(editing) = self.editing.as_mut() else {
            return;
        };
        let generation = editing.operations.generation.wrapping_add(1);
        editing.operations = State {
            language: Some((target.range.start, name.into())),
            generation,
            inventory: editing.operations.inventory.take(),
            inventory_requested: editing.operations.inventory_requested,
            inventory_task: editing.operations.inventory_task.take(),
            ..Default::default()
        };
        cx.notify();
    }

    pub(super) fn code_operation_target_key(&self, cx: &App) -> Option<(Range<usize>, String)> {
        self.code_scope(cx)
            .map(|target| (target.range, target.language))
    }

    pub(super) fn code_operation_actions(&self, column: Div, cx: &mut Context<Self>) -> Div {
        column
            .on_action(cx.listener(|this, _: &FormatCode, window, cx| {
                this.code_operation(Action::Format, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ValidateCode, window, cx| {
                this.code_operation(Action::Validate, window, cx)
            }))
            .on_action(cx.listener(|this, _: &RepairCode, window, cx| {
                this.code_operation(Action::Repair, window, cx)
            }))
    }
}

fn preview_editor(
    text: &str,
    language: &str,
    window: &mut Window,
    cx: &mut Context<EditorState>,
) -> EditorState {
    let mut editor = EditorState::new(window, cx)
        .language(language.to_owned())
        .line_number(true)
        .soft_wrap(true);
    editor.set_value(text.to_owned(), window, cx);
    editor.set_readonly(true, cx);
    editor
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui_component::input::Undo;

    struct Fixture {
        reader: Entity<Reader>,
        input: Entity<EditorState>,
        directory: tempfile::TempDir,
    }

    fn open<'a>(cx: &'a mut TestAppContext, source: &str) -> (Fixture, &'a mut VisualTestContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            crate::bind_keys(cx);
        });
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("note.md"), source).unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("note.md")),
                        index_dir: Some(directory.path().join("index")),
                        session_directory: Some(directory.path().join("state")),
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
        let input = reader.read_with(visual, |reader, _| {
            reader.editing.as_ref().unwrap().input.clone()
        });
        let start = source.find('{').unwrap();
        input.update(visual, |state, cx| {
            state.set_selected_range(start..start, cx)
        });
        settle(visual);
        (
            Fixture {
                reader,
                input,
                directory,
            },
            visual,
        )
    }

    fn value(f: &Fixture, cx: &App) -> String {
        f.input.read(cx).value().to_string()
    }

    fn settle(visual: &mut VisualTestContext) {
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(101));
        visual.run_until_parked();
    }

    fn click(visual: &mut VisualTestContext, selector: &'static str) {
        settle(visual);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let bounds = visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing {selector}"));
        visual.simulate_click(bounds.center(), Modifiers::default());
        visual.run_until_parked();
    }

    #[gpui::test]
    fn code2771_invalid_format_is_callable_and_repair_preview_cancel_apply_is_explicit(
        cx: &mut TestAppContext,
    ) {
        let original = "# Before\n\n```json\n{ a: 1 }\n```\n\nAfter\n";
        let (f, visual) = open(cx, original);
        click(visual, "source-code-validate");
        f.reader.read_with(visual, |reader, _| {
            assert!(reader
                .editing
                .as_ref()
                .unwrap()
                .operations
                .status
                .as_ref()
                .unwrap()
                .3
                .starts_with("Invalid JSON"))
        });
        click(visual, "source-code-format");
        f.reader.read_with(visual, |reader, cx| {
            assert_eq!(value(&f, cx), original);
            assert!(reader
                .editing
                .as_ref()
                .unwrap()
                .operations
                .notice
                .as_ref()
                .unwrap()
                .contains("Source unchanged"));
        });
        click(visual, "source-code-repair");
        f.reader.read_with(visual, |reader, cx| {
            assert_eq!(value(&f, cx), original);
            let preview = reader
                .editing
                .as_ref()
                .unwrap()
                .operations
                .preview
                .as_ref()
                .unwrap();
            assert_eq!(preview.proposed, "{ \"a\": 1 }");
            assert!(
                !preview.before.read(cx).is_editable() && !preview.after.read(cx).is_editable()
            );
        });
        assert_eq!(
            std::fs::read_to_string(f.directory.path().join("vault/note.md")).unwrap(),
            original
        );
        click(visual, "source-code-repair-cancel");
        f.reader.read_with(visual, |reader, cx| {
            assert!(reader
                .editing
                .as_ref()
                .unwrap()
                .operations
                .preview
                .is_none());
            assert_eq!(value(&f, cx), original);
        });
        click(visual, "source-code-repair");
        click(visual, "source-code-repair-apply");
        f.reader.read_with(visual, |reader, cx| {
            assert_eq!(value(&f, cx), original.replace("{ a: 1 }", "{ \"a\": 1 }"));
            assert_eq!(
                reader
                    .editing
                    .as_ref()
                    .unwrap()
                    .operations
                    .status
                    .as_ref()
                    .unwrap()
                    .3,
                "Valid strict JSON"
            );
        });
        visual.dispatch_action(Undo);
        settle(visual);
        f.reader
            .read_with(visual, |_, cx| assert_eq!(value(&f, cx), original));
    }

    #[gpui::test]
    fn code2771_stale_repair_never_replaces_newer_source(cx: &mut TestAppContext) {
        let original = "```json\n{ a: 1 }\n```\n";
        let (f, visual) = open(cx, original);
        click(visual, "source-code-repair");
        f.input.update_in(visual, |input, window, cx| {
            let at = original.find('1').unwrap();
            input.replace_source_range(at..at + 1, "2", window, cx);
        });
        let newer = original.replace('1', "2");
        visual.run_until_parked();
        click(visual, "source-code-repair-apply");
        f.reader.read_with(visual, |reader, cx| {
            assert_eq!(value(&f, cx), newer);
            assert!(reader
                .editing
                .as_ref()
                .unwrap()
                .operations
                .notice
                .as_ref()
                .unwrap()
                .contains("stale"));
        });
    }

    #[gpui::test]
    fn code2771_late_valid_formatter_has_positive_replacement_control(cx: &mut TestAppContext) {
        let original = "```json\n{\"a\":1}\n```\n";
        let (f, visual) = open(cx, original);
        // Positive control: this exact formatter produces a replacement.
        click(visual, "source-code-format");
        f.reader.read_with(visual, |_, cx| {
            assert_eq!(value(&f, cx), "```json\n{\n  \"a\": 1\n}\n```\n");
        });
        visual.dispatch_action(Undo);
        settle(visual);
        f.reader
            .read_with(visual, |_, cx| assert_eq!(value(&f, cx), original));
        // Queue the same real replacement, then advance the native revision
        // before any completion can be delivered to the UI executor.
        f.reader.update_in(visual, |reader, window, cx| {
            reader.code_operation(Action::Format, window, cx)
        });
        f.input.update_in(visual, |input, window, cx| {
            let at = original.find('1').unwrap();
            input.replace_source_range(at..at + 1, "3", window, cx);
        });
        visual.run_until_parked();
        f.reader.read_with(visual, |reader, cx| {
            assert_eq!(value(&f, cx), original.replace('1', "3"));
            let state = &reader.editing.as_ref().unwrap().operations;
            assert!(!state.busy);
            assert!(state.notice.as_ref().unwrap().contains("Source changed"));
        });
    }

    #[gpui::test]
    fn code2771_single_line_crlf_fence_formats_without_mixed_endings(cx: &mut TestAppContext) {
        let original = "Before\r\n```json\r\n{\"a\":1}\r\n```\r\nAfter\r\n";
        let (f, visual) = open(cx, original);
        click(visual, "source-code-format");
        f.reader.read_with(visual, |_, cx| {
            assert_eq!(
                value(&f, cx),
                original.replace("{\"a\":1}", "{\r\n  \"a\": 1\r\n}")
            );
        });
        visual.dispatch_action(Undo);
        settle(visual);
        f.reader
            .read_with(visual, |_, cx| assert_eq!(value(&f, cx), original));
    }

    #[gpui::test]
    fn code2771_parking_source_retires_late_jobs_and_preserves_operations(cx: &mut TestAppContext) {
        let original = "```json\n{\"a\":1}\n```\n";
        let (f, visual) = open(cx, original);
        f.reader.update_in(visual, |reader, window, cx| {
            reader.code_operation(Action::Format, window, cx);
            assert!(reader.editing.as_ref().unwrap().operations.busy);
            let editing = reader.editing.take().unwrap();
            let (store, restore) = editing.park(cx);
            reader.editing = Some(restore(store));
            f.input
                .update(cx, |input, cx| input.set_disabled(false, cx));
        });
        visual.run_until_parked();
        f.reader.read_with(visual, |reader, cx| {
            assert_eq!(value(&f, cx), original);
            assert!(!reader.editing.as_ref().unwrap().operations.busy);
        });
        // The reassembled editor accepts a fresh operation normally.
        click(visual, "source-code-format");
        f.reader.read_with(visual, |_, cx| {
            assert_eq!(value(&f, cx), "```json\n{\n  \"a\": 1\n}\n```\n");
        });
    }

    #[gpui::test]
    fn code2771_caret_crosses_targets_without_forced_reader_notification(cx: &mut TestAppContext) {
        let original =
            "Before\n```json\n{\"a\":1}\n```\nBetween\n```python\nprint({})\n```\nAfter\n";
        let (f, visual) = open(cx, original);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(visual.debug_bounds("source-code-language").is_some());
        let old = f
            .reader
            .read_with(visual, |reader, cx| reader.code_scope(cx).unwrap());
        let at = original.find("Between").unwrap();
        f.input
            .update(visual, |state, cx| state.set_selected_range(at..at, cx));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(visual.debug_bounds("source-code-language").is_none());
        let at = original.find("print").unwrap();
        f.input
            .update(visual, |state, cx| state.set_selected_range(at..at, cx));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(visual.debug_bounds("source-code-language").is_some());
        f.reader.update(visual, |reader, cx| {
            assert_eq!(reader.code_target(cx).unwrap().language, "python");
            reader.select_code_language(&old, "jsonc", cx);
            assert_eq!(reader.code_target(cx).unwrap().language, "python");
            assert!(reader
                .editing
                .as_ref()
                .unwrap()
                .operations
                .language
                .is_none());
            let current = reader.code_scope(cx).unwrap();
            reader.select_code_language(&current, "text", cx);
            assert_eq!(reader.code_target(cx).unwrap().language, "text");
        });
    }

    #[gpui::test]
    fn code2771_toolbar_paint_and_caret_reuse_background_metadata_without_source_work(
        cx: &mut TestAppContext,
    ) {
        let original = format!(
            "{}\n```json\n{{\"a\":1}}\n```\nAfter\n",
            "Ordinary prose.\n".repeat(1000)
        );
        let (f, visual) = open(cx, &original);
        let at = original.find('{').unwrap();
        let key = f.reader.read_with(visual, |reader, cx| {
            reader.code_operation_target_key(cx).unwrap()
        });
        super::super::block_entry::take_source_work_bytes();
        for offset in [at + 1, at + 3, at] {
            f.input.update(visual, |input, cx| {
                input.set_selected_range(offset..offset, cx)
            });
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
            f.reader.read_with(visual, |reader, cx| {
                assert_eq!(reader.code_operation_target_key(cx).as_ref(), Some(&key))
            });
        }
        assert_eq!(
            super::super::block_entry::take_source_work_bytes(),
            0,
            "toolbar render and in-body caret movement share metadata, never flatten the note"
        );
        f.reader.update_in(visual, |reader, window, cx| {
            let at = original.find("json").unwrap();
            f.input.update(cx, |input, cx| {
                input.replace_source_range(at..at + 4, "python", window, cx);
                let body = original.find('{').unwrap() + 2;
                input.set_selected_range(body..body, cx);
            });
            assert!(
                reader.code_scope(cx).is_none(),
                "old inventory cannot authorize a new revision"
            );
        });
        settle(visual);
        f.reader.read_with(visual, |reader, cx| {
            let scope = reader.code_scope(cx).unwrap();
            assert_eq!(scope.language, "python");
            assert_eq!(scope.stamp, f.input.read(cx).source_stamp());
        });
    }

    #[gpui::test]
    fn code2771_app_teardown_does_not_retain_input_during_metadata_debounce(
        cx: &mut TestAppContext,
    ) {
        let mut app = cx.new_app();
        let (fixture, visual) = open(&mut app, "```json\n{\"a\":1}\n```\n");
        fixture.reader.update(visual, |reader, cx| {
            let state = &mut reader.editing.as_mut().unwrap().operations;
            state.inventory = None;
            state.inventory_requested = None;
            reader.schedule_code_inventory(cx);
        });
        visual.run_until_parked();
        fixture.reader.read_with(visual, |reader, cx| {
            let editing = reader.editing.as_ref().unwrap();
            assert_eq!(
                editing.operations.inventory_requested,
                Some(editing.input.read(cx).source_stamp())
            );
            assert!(editing.operations.inventory_task.is_some());
            assert!(
                editing.operations.inventory.is_none(),
                "positive control: timer has not fired"
            );
        });
        visual.update(|window, _| window.remove_window());
        let Fixture {
            reader,
            input,
            directory,
        } = fixture;
        drop(reader);
        drop(input);
        app.run_until_parked();
        // Exercise the actual leak-checking teardown while debounce is pending,
        // rather than advancing time to let its captured handles disappear.
        app.quit();
        drop(app);
        cx.run_until_parked();
        drop(directory);
    }

    #[test]
    fn code2771_background_inventory_matches_fenced_body_target_rules() {
        for source in [
            "---\ntitle: '```json'\n---\nBefore\n```json\n{\"a\":1}\n```\nAfter\n",
            "```json\r\n{\"a\":1}\r\n```\r\n~~~python\r\nprint({})\r\n~~~\r\n",
            "> ```json\n> {}\n> ```\n\n```unknown\nvalue",
            "```\n```\n```json\n{}\n```\n```python\n[]\n",
        ] {
            let blocks = code_comforts::blocks(source);
            assert!(!blocks.is_empty());
            for block in blocks {
                assert_eq!(
                    code_comforts::block_at(source, block.body.start),
                    Some(block.clone())
                );
                assert_eq!(code_comforts::block_at(source, block.body.end), Some(block));
            }
        }
    }

    #[gpui::test]
    fn code2771_valid_format_keeps_fences_undo_and_explicit_dialect(cx: &mut TestAppContext) {
        let original = "Before\n```json\n{\"a\":1,\"a\":2e3}\n```\nAfter\n";
        let (f, visual) = open(cx, original);
        click(visual, "source-code-format");
        f.reader.read_with(visual, |reader, cx| {
            let text = value(&f, cx);
            assert!(text.starts_with("Before\n```json\n{\n") && text.ends_with("}\n```\nAfter\n"));
            assert_eq!(text.matches("\"a\"").count(), 2);
            assert!(text.contains("2e3"));
            assert_eq!(
                reader
                    .editing
                    .as_ref()
                    .unwrap()
                    .operations
                    .status
                    .as_ref()
                    .unwrap()
                    .3,
                "Valid strict JSON"
            );
        });
        visual.dispatch_action(Undo);
        settle(visual);
        f.reader
            .read_with(visual, |_, cx| assert_eq!(value(&f, cx), original));
        f.reader.update(visual, |reader, cx| {
            reader.editing.as_mut().unwrap().operations.language =
                Some(("Before\n```json\n".len(), "jsonc".into()));
            cx.notify();
        });
        click(visual, "source-code-format");
        f.reader.read_with(visual, |reader, cx| {
            assert_eq!(value(&f, cx), original);
            assert!(reader
                .editing
                .as_ref()
                .unwrap()
                .operations
                .notice
                .as_ref()
                .unwrap()
                .contains("jsonc"));
        });
    }

    #[test]
    fn code2771_validation_preserves_selected_grammar_and_dialect() {
        crate::source_presentation::code_languages::install();
        assert!(validation("json", "{ a: 1 }").starts_with("Invalid JSON"));
        assert!(validation("javascript", "const value = { a: 1 };").starts_with("Syntax checked"));
        assert!(validation("python", "def run(:\n print(1)").starts_with("Syntax error"));
        assert!(validation("jsonc", "// comment\n{}").contains("unavailable"));
        assert_eq!(file_language("example.JSONC"), "jsonc");
        assert_eq!(file_language("example.jsx"), "jsx");
        assert_eq!(file_language("example.scss"), "scss");
    }
}
