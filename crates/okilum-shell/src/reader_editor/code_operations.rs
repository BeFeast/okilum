//! Explicit source operations. The reader protocol and read-only preview never
//! gain write authority; every accepted edit uses the normal source Undo/save.
use super::*;
pub(super) mod file_chrome;
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
    running: Option<Running>,
    last_action: Option<Action>,
    status: Option<(SourceStamp, Range<usize>, String, String)>,
    notice: Option<String>,
    notice_stamp: Option<SourceStamp>,
    preview: Option<Preview>,
    inventory: Option<Inventory>,
    inventory_requested: Option<SourceStamp>,
    inventory_task: Option<Task<()>>,
    #[cfg(test)]
    worker_gate: Option<futures::channel::oneshot::Receiver<()>>,
}

struct Running {
    control: model::Control,
    action: Action,
    scope: Scope,
}
impl Drop for Running {
    fn drop(&mut self) {
        self.control.cancel();
    }
}

#[derive(Default)]
struct JobGate(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl gpui::Global for JobGate {}
struct Permit(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Permit {
    fn acquire(cx: &mut App) -> Option<Self> {
        use std::sync::atomic::Ordering;
        if cx.try_global::<JobGate>().is_none() {
            cx.set_global(JobGate::default());
        }
        let gate = cx.global::<JobGate>().0.clone();
        gate.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(Self(gate))
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

struct Inventory {
    stamp: SourceStamp,
    blocks: Vec<code_comforts::Block>,
    line_ending: model::LineEnding,
}

impl State {
    pub(super) fn clear_notice(&mut self) {
        self.notice = None;
        self.notice_stamp = None;
    }
    pub(super) fn preview_present(&self) -> bool {
        self.preview.is_some()
    }
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
        if let Some(running) = self.running.as_ref().filter(|run| run.scope.stamp != stamp) {
            running.control.cancel();
        }
        let notice_stale =
            self.notice.is_some() && self.notice_stamp.is_some_and(|known| known != stamp);
        if notice_stale {
            self.notice = None;
            self.notice_stamp = None;
        }
        let stale = self
            .status
            .as_ref()
            .is_some_and(|(known, _, _, _)| *known != stamp);
        if stale {
            self.status = None;
        }
        let preview_stale = self
            .preview
            .as_ref()
            .is_some_and(|p| p.target.stamp != stamp);
        if preview_stale {
            self.preview = None;
            self.notice = Some("Repair preview is stale after editing; run Repair again.".into());
            self.notice_stamp = Some(stamp);
        }
        stale || preview_stale || notice_stale
    }
}

#[derive(Clone)]
struct Target {
    binding_root: PathBuf,
    binding_file: String,
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
    validation: String,
    before: Entity<EditorState>,
    after: Entity<EditorState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Validate,
    Format,
    Repair,
}

#[cfg(test)]
fn validation(language: &str, source: &str) -> String {
    validation_with_control(language, source, &model::Control::default())
}

fn validation_with_control(language: &str, source: &str, control: &model::Control) -> String {
    if source.len() > model::MAX_BYTES {
        return model::size_notice("Validate", source.len());
    }
    if language == "json" {
        return match model::json_validation_controlled(source, control) {
            None => "Operation cancelled.".into(),
            Some(Err(reason)) => reason,
            Some(Ok(Ok(()))) => "Valid strict JSON".into(),
            Some(Ok(Err(e))) => format!("Invalid JSON · {}:{} · {}", e.line, e.column, e.message),
        };
    }
    // Grammar trees can amplify their input substantially. Keep their existing
    // 256 KiB / 250 ms budget; JSON uses the allocation-bounded strict parser.
    if source.len() > 256 * 1024 {
        return format!("Syntax validation for {language} works with files up to 256 KiB; this file is {:.1} MiB.", source.len() as f64 / 1048576.);
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
        if control.cancelled() || Instant::now() >= deadline {
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
        if control.cancelled() {
            return "Operation cancelled.".into();
        }
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
    pub(crate) fn run_format_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.code_operation(Action::Format, window, cx);
    }
    pub(crate) fn run_validate_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.code_operation(Action::Validate, window, cx);
    }
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
        self.code_scope_for_display(cx, false)
    }

    // Keep the last toolbar while the debounced fence inventory catches up.
    // Actions still require the exact current revision through code_scope().
    fn code_scope_for_display(&self, cx: &App, retain_inventory: bool) -> Option<Scope> {
        let editing = self.editing.as_ref()?;
        if retain_inventory {
            if let Some(running) = &editing.operations.running {
                return Some(running.scope.clone());
            }
        }
        let input = editing.input.read(cx);
        let stamp = input.source_stamp();
        let (range, language, line_ending) = if editing.markdown {
            let selection = input.selected_range();
            let inventory = editing
                .operations
                .inventory
                .as_ref()
                .filter(|i| retain_inventory || i.stamp == stamp)?;
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

    #[cfg(test)]
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
            binding_root: self.vault_root.clone(),
            binding_file: self.selected_file().to_owned(),
            line_ending: scope
                .line_ending
                .unwrap_or_else(|| model::line_ending(&text)),
            range: scope.range,
            language: scope.language,
            stamp: scope.stamp,
            text,
        })
    }

    fn refresh_code_host(&self, cx: &mut Context<Self>) {
        if let Some(file) = self.editing.as_ref().and_then(|e| e.file_chrome.clone()) {
            file.update(cx, |_, cx| cx.notify());
        } else {
            cx.notify();
        }
    }

    fn cancel_code_operation(&mut self, cx: &mut Context<Self>) {
        if let Some(editing) = self.editing.as_mut() {
            if let Some(running) = &editing.operations.running {
                running.control.cancel();
                editing.operations.notice = Some("Cancelling…".into());
                editing.operations.notice_stamp = Some(editing.input.read(cx).source_stamp());
            }
        }
        self.refresh_code_host(cx);
    }

    fn code_operation(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let Some(scope) = self.code_scope(cx) else {
            return;
        };
        if !self.ime_allows(ime_guard::Trigger::Text, cx) {
            return;
        }
        let binding_root = self.vault_root.clone();
        let binding_file = self.selected_file().to_owned();
        let editing = self.editing.as_mut().unwrap();
        if editing.operations.busy
            || editing.conflict_detected
            || editing.save_failed
            || !editing.input.read(cx).is_editable()
        {
            return;
        }
        editing.operations.last_action = Some(action);
        if scope.range.len() > model::MAX_BYTES {
            editing.operations.status = None;
            editing.operations.preview = None;
            editing.operations.notice = Some(model::size_notice(action.name(), scope.range.len()));
            editing.operations.notice_stamp = Some(scope.stamp);
            self.refresh_code_host(cx);
            return;
        }
        let Some(permit) = Permit::acquire(cx) else {
            editing.operations.notice =
                Some("Another code operation is finishing; try again.".into());
            editing.operations.notice_stamp = Some(scope.stamp);
            self.refresh_code_host(cx);
            return;
        };
        let control = model::Control::default();
        editing.operations.generation = editing.operations.generation.wrapping_add(1);
        let generation = editing.operations.generation;
        editing.operations.busy = true;
        editing.operations.notice = None;
        editing.operations.notice_stamp = None;
        editing.operations.status = None;
        editing.operations.preview = None;
        editing.operations.running = Some(Running {
            control: control.clone(),
            action,
            scope: scope.clone(),
        });
        let input = editing.input.clone();
        // Pin an immutable native revision; flattening and newline scans belong
        // to the admitted worker, not the button/focus event on the UI thread.
        let snapshot = input.read(cx).source_snapshot();
        crate::source_presentation::code_languages::install();
        let snapshot_stamp = scope.stamp;
        let started = Instant::now();
        let bytes = scope.range.len();
        let trace = reader_diagnostics::trace(cx);
        if let Some(trace) = &trace {
            trace.event("code_operation_queued", serde_json::json!({"action":action.name(), "bytes":bytes, "thread":format!("{:?}", std::thread::current().id())}));
        }
        let worker_trace = trace.clone();
        let worker_control = control.clone();
        let target_root = binding_root.clone();
        let target_file = binding_file.clone();
        #[cfg(test)]
        let worker_gate = editing.operations.worker_gate.take();
        let task = cx.background_executor().spawn(async move {
            #[cfg(test)]
            if let Some(gate) = worker_gate { let _ = gate.await; }
            let worker_started = Instant::now();
            let result = (|| {
                if worker_control.cancelled() {
                    return None;
                }
                let text = snapshot.text.rope().slice(scope.range.clone()).to_string();
                let target = Target {
                    binding_root: target_root,
                    binding_file: target_file,
                    line_ending: scope
                        .line_ending
                        .unwrap_or_else(|| model::line_ending(&text)),
                    range: scope.range,
                    language: scope.language,
                    stamp: scope.stamp,
                    text,
                };
                let mut status = None;
                let mut format = None;
                let mut repair = None;
                let mut proposed_validation = None;
                match action {
                    Action::Validate => {
                        status = Some(validation_with_control(
                            &target.language,
                            &target.text,
                            &worker_control,
                        ))
                    }
                    Action::Format => {
                        let result = model::format_controlled(
                            &target.language,
                            &target.text,
                            target.line_ending,
                            &worker_control,
                        )?;
                        match &result {
                            model::Formatted::Changed(text) => {
                                status = Some(validation_with_control(
                                    &target.language,
                                    text,
                                    &worker_control,
                                ))
                            }
                            model::Formatted::Unchanged(reason)
                                if reason == "Already formatted."
                                    || reason.starts_with("JSON is invalid") =>
                            {
                                status = Some(validation_with_control(
                                    &target.language,
                                    &target.text,
                                    &worker_control,
                                ))
                            }
                            _ => {}
                        }
                        format = Some(result);
                    }
                    Action::Repair => {
                        let mut result = model::repair_controlled(
                            &target.language,
                            &target.text,
                            &worker_control,
                        )?;
                        if let Ok(repair) = &mut result {
                            // Target already owns the exact original revision.
                            // Drop the model's compatibility copy on this worker.
                            drop(std::mem::take(&mut repair.source));
                            // Core has already checked both the invalid original
                            // and the complete strict-JSON proposal on this worker.
                            status = Some("Repair proposal ready.".into());
                            proposed_validation = Some("Valid strict JSON".into());
                        }
                        repair = Some(result);
                    }
                }
                if worker_control.cancelled() {
                    return None;
                }
                Some((target, status, format, repair, proposed_validation))
            })();
            if let Some(trace) = &worker_trace {
                trace.event("code_operation_worker", serde_json::json!({"action":action.name(), "bytes":bytes, "duration_ms":worker_started.elapsed().as_secs_f64()*1000., "cancelled":worker_control.cancelled(), "thread":format!("{:?}", std::thread::current().id())}));
            }
            // Admission remains held until UI consumption drops all transient
            // results, even after Cancel, navigation or a retired editor.
            (result, permit)
        });
        cx.spawn_in(window, async move |this, cx| {
            let (result, _permit) = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let composition_ready = this.ime_allows(ime_guard::Trigger::Text, cx);
                let binding_ready = this.vault_root == binding_root && this.selected_file() == binding_file;
                let Some(editing) = this.editing.as_mut().filter(|e| e.input == input) else { return; };
                if editing.operations.generation != generation { return; }
                // Snapshot cancellation before dropping its cancel-on-retirement guard.
                let cancelled = control.cancelled();
                if let Some(trace) = &trace {
                    trace.event("code_operation_delivered", serde_json::json!({"action":action.name(), "bytes":bytes, "duration_ms":started.elapsed().as_secs_f64()*1000., "cancelled":cancelled, "binding_current":binding_ready, "source_current":input.read(cx).source_stamp()==snapshot_stamp}));
                }
                editing.operations.busy = false;
                editing.operations.running = None;
                editing.operations.notice_stamp = Some(input.read(cx).source_stamp());
                if !binding_ready || !composition_ready || input.read(cx).source_stamp() != snapshot_stamp
                    || editing.conflict_detected || editing.save_failed || !input.read(cx).is_editable() {
                    editing.operations.notice = Some("Source changed; run the operation again. No result applied.".into());
                    this.publish_file_operation_notice(window, cx); this.refresh_code_host(cx); return;
                }
                let Some((target, status, format, repair, proposed_validation)) = result else {
                    editing.operations.notice = Some(if input.read(cx).source_stamp() != snapshot_stamp { "Source changed; run the operation again. No result applied." } else { "Operation cancelled." }.into());
                    this.publish_file_operation_notice(window, cx); this.refresh_code_host(cx); return;
                };
                if cancelled {
                    editing.operations.notice = Some("Operation cancelled.".into());
                    this.publish_file_operation_notice(window, cx); this.refresh_code_host(cx); return;
                }
                let mut status_range = target.range.clone();
                match format {
                    Some(model::Formatted::Changed(text)) => {
                        if !editing.markdown {
                            this.retire_large_plain_preview(input.read(cx).text().len() - target.range.len() + text.len());
                        }
                        input.update(cx, |state, cx| {
                            state.replace_source_range(target.range.clone(), text.as_str(), window, cx);
                            state.focus(window, cx);
                        });
                        status_range.end = status_range.start + text.len();
                        this.editing.as_mut().unwrap().operations.notice = Some(
                            "Formatted. Validation refreshed; Undo restores the original source.".into());
                    }
                    Some(model::Formatted::Unchanged(reason)) => editing.operations.notice = Some(reason),
                    None => {}
                }
                let editing = this.editing.as_mut().unwrap();
                editing.operations.status = status.map(|status| (input.read(cx).source_stamp(), status_range, target.language.clone(), status));
                if let Some(result) = repair {
                    match result {
                        Err(reason) => editing.operations.notice = Some(reason),
                        Ok(repair) => {
                            let before = cx.new(|cx| preview_editor(preview_excerpt(&target.text), &target.language, window, cx));
                            let after = cx.new(|cx| preview_editor(preview_excerpt(&repair.proposed), &target.language, window, cx));
                            let mut explanation = repair.explanation;
                            if target.text.len() > PREVIEW_BYTES || repair.proposed.len() > PREVIEW_BYTES {
                                explanation.push_str(" Preview shows the first 64 KiB of each version; Apply uses the complete result.");
                            }
                            editing.operations.preview = Some(Preview { target, proposed: repair.proposed, explanation,
                                validation: proposed_validation.expect("prepared repair validation"), before, after });
                        }
                    }
                }
                this.editing.as_mut().unwrap().operations.notice_stamp = Some(input.read(cx).source_stamp());
                this.publish_file_operation_notice(window, cx);
                this.refresh_code_host(cx);
            });
        }).detach();
        self.refresh_code_host(cx);
    }

    fn apply_code_repair(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(current) = self.code_scope(cx) else {
            return;
        };
        if !self.ime_allows(ime_guard::Trigger::Text, cx) {
            return;
        }
        let binding_root = self.vault_root.clone();
        let binding_file = self.selected_file().to_owned();
        let editing = self.editing.as_mut().unwrap();
        let Some(preview) = editing.operations.preview.as_ref() else {
            return;
        };
        if binding_root != preview.target.binding_root
            || binding_file != preview.target.binding_file
            || current.stamp != preview.target.stamp
            || current.range != preview.target.range
            || current.language != preview.target.language
            || editing.conflict_detected
            || editing.save_failed
            || !editing.input.read(cx).is_editable()
        {
            editing.operations.notice = Some("Repair preview is stale or source is in conflict. Nothing applied; review a fresh proposal.".into());
            editing.operations.notice_stamp = Some(editing.input.read(cx).source_stamp());
            self.refresh_code_host(cx);
            return;
        }
        let preview = editing.operations.preview.take().unwrap();
        let input = editing.input.clone();
        let status = preview.validation;
        if !editing.markdown {
            self.retire_large_plain_preview(
                input.read(cx).text().len() - preview.target.range.len() + preview.proposed.len(),
            );
        }
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
        editing.operations.notice_stamp = Some(input.read(cx).source_stamp());
        input.update(cx, |state, cx| state.focus(window, cx));
        self.publish_file_operation_notice(window, cx);
        self.refresh_code_host(cx);
    }

    pub(super) fn render_code_operations(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self
            .editing
            .as_ref()
            .is_some_and(|editing| !editing.markdown)
        {
            return None;
        }
        let target = self.code_scope_for_display(cx, true)?;
        let editing = self.editing.as_ref()?;
        let state = &editing.operations;
        let owner = cx.entity().downgrade();
        let language = target.language.clone();
        let disabled = state.busy
            || editing.conflict_detected
            || editing.save_failed
            || !editing.input.read(cx).is_editable();
        let selector = Button::new("source-code-language")
            .disabled(disabled)
            .small()
            .ghost()
            .label(language.clone())
            .debug_selector(|| "source-code-language".into())
            .dropdown_menu(move |menu, _, cx| {
                // Snapshot the current scope when the popup opens, not on the
                // last Reader render. Ordinary typing need not rebuild Reader.
                let Some(menu_target) = owner
                    .upgrade()
                    .and_then(|reader| reader.read(cx).code_scope(cx))
                else {
                    return menu;
                };
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
        let status = if state.busy {
            state.notice.clone().unwrap_or_else(|| {
                state
                    .running
                    .as_ref()
                    .map(|run| run.action.busy_text())
                    .unwrap_or("Working…")
                    .into()
            })
        } else {
            state.notice.clone().unwrap_or(status)
        };
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
            )
            .when(state.busy, |controls| {
                controls.child(
                    Button::new("source-code-operation-cancel")
                        .small()
                        .ghost()
                        .label("Cancel")
                        .debug_selector(|| "source-code-operation-cancel".into())
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_code_operation(cx))),
                )
            });
        let mut view = v_flex().px_4().py_2().gap_1().child(controls).child(
            div()
                .text_sm()
                .debug_selector(|| "source-code-validation".into())
                .child(status),
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
                                        editing.operations.status = None;
                                        editing.operations.notice =
                                            Some("Repair cancelled.".into());
                                        editing.operations.notice_stamp =
                                            Some(editing.input.read(cx).source_stamp());
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
        if editing.operations.busy {
            return;
        }
        let generation = editing.operations.generation.wrapping_add(1);
        editing.operations = State {
            language: Some((target.range.start, name.into())),
            generation,
            inventory: editing.operations.inventory.take(),
            inventory_requested: editing.operations.inventory_requested,
            inventory_task: editing.operations.inventory_task.take(),
            ..Default::default()
        };
        self.refresh_code_host(cx);
    }

    pub(super) fn code_operation_target_key(&self, cx: &App) -> Option<(usize, String)> {
        // Body length changes on every key; it is not a toolbar boundary.
        self.code_scope_for_display(cx, true)
            .map(|target| (target.range.start, target.language))
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

const PREVIEW_BYTES: usize = 64 * 1024;
fn preview_excerpt(text: &str) -> &str {
    let mut end = text.len().min(PREVIEW_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
impl Action {
    fn name(self) -> &'static str {
        match self {
            Self::Validate => "Validate",
            Self::Format => "Format",
            Self::Repair => "Repair",
        }
    }
    fn busy_text(self) -> &'static str {
        match self {
            Self::Validate => "Validating…",
            Self::Format => "Formatting…",
            Self::Repair => "Preparing repair…",
        }
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

    pub(super) struct Fixture {
        pub(super) reader: Entity<Reader>,
        pub(super) input: Entity<EditorState>,
        pub(super) directory: tempfile::TempDir,
    }

    pub(super) fn open<'a>(
        cx: &'a mut TestAppContext,
        source: &str,
    ) -> (Fixture, &'a mut VisualTestContext) {
        open_file(cx, source, "note.md")
    }

    pub(super) fn open_file<'a>(
        cx: &'a mut TestAppContext,
        source: &str,
        relative: &str,
    ) -> (Fixture, &'a mut VisualTestContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            crate::bind_keys(cx);
        });
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join(relative), source).unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join(relative)),
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

    pub(super) fn value(f: &Fixture, cx: &App) -> String {
        f.input.read(cx).value().to_string()
    }

    pub(super) fn settle(visual: &mut VisualTestContext) {
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(101));
        visual.run_until_parked();
    }

    pub(super) fn click(visual: &mut VisualTestContext, selector: &'static str) {
        settle(visual);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let bounds = visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing {selector}"));
        visual.simulate_click(bounds.center(), Modifiers::default());
        visual.run_until_parked();
    }

    #[gpui::test]
    fn edit2803_continuous_typing_has_one_timer_and_a_two_second_save_ceiling(
        cx: &mut TestAppContext,
    ) {
        let original = "Before\n```json\n{\"a\":1}\n```\nAfter\n";
        let (f, visual) = open(cx, original);
        for i in 0..9 {
            visual.simulate_input(" ");
            visual.run_until_parked();
            visual.executor().advance_clock(Duration::from_millis(250));
            visual.run_until_parked();
            if i < 6 {
                assert_eq!(
                    std::fs::read_to_string(f.directory.path().join("vault/note.md")).unwrap(),
                    original,
                    "the idle debounce must coalesce quick keys"
                );
            }
        }
        let saved = std::fs::read_to_string(f.directory.path().join("vault/note.md")).unwrap();
        assert_ne!(
            saved, original,
            "continuous input still publishes within two seconds"
        );
        let newest = f.reader.read_with(visual, |_, cx| value(&f, cx));
        visual.executor().advance_clock(Duration::from_millis(1100));
        visual.run_until_parked();
        assert_eq!(
            std::fs::read_to_string(f.directory.path().join("vault/note.md")).unwrap(),
            newest
        );
    }

    #[gpui::test]
    fn edit2803_blur_saves_off_thread_and_quit_keeps_the_newest_revision(cx: &mut TestAppContext) {
        let original = "Before\n```json\n{\"a\":1}\n```\nAfter\n";
        let (f, visual) = open(cx, original);
        let changed = original.replace("1", "2");
        f.input.update_in(visual, |input, window, cx| {
            input.set_value(&changed, window, cx)
        });
        f.reader.update_in(visual, |reader, window, cx| {
            reader.save_source_in_background(window, cx);
            assert!(reader.editing.as_ref().unwrap().autosaving.is_some());
            assert_eq!(
                std::fs::read_to_string(f.directory.path().join("vault/note.md")).unwrap(),
                original,
                "blur queues work; no canonical write on the foreground executor"
            );
        });
        visual.run_until_parked();
        assert_eq!(
            std::fs::read_to_string(f.directory.path().join("vault/note.md")).unwrap(),
            changed
        );
        let newest = original.replace("1", "3");
        f.input.update_in(visual, |input, window, cx| {
            input.set_value(&newest, window, cx)
        });
        f.reader.update_in(visual, |reader, window, cx| {
            reader.save_source_in_background(window, cx);
            assert!(
                reader.save_source_for_close(cx),
                "quit still waits for durable latest bytes"
            );
        });
        visual.run_until_parked();
        assert_eq!(
            std::fs::read_to_string(f.directory.path().join("vault/note.md")).unwrap(),
            newest
        );
    }

    #[gpui::test]
    fn edit2803_unwrapped_popup_frames_reuse_shapes_and_invalidate_on_edit(
        cx: &mut TestAppContext,
    ) {
        let original = format!("{{\"padding\":\"{}\"}}", "a".repeat(512 * 1024));
        let (f, visual) = open(cx, &original);
        f.input.update_in(visual, |input, window, cx| {
            input.set_soft_wrap(false, window, cx);
            input.enable_layout_diagnostics();
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let before = f
            .input
            .read_with(visual, |input, _| input.layout_diagnostics().unwrap());
        let started = Instant::now();
        for _ in 0..10 {
            // The same native layout work caused by popup hover/caret blink.
            f.input.update(visual, |_, cx| cx.notify());
            visual.update(|window, cx| window.draw(cx).clear(cx));
        }
        let elapsed = started.elapsed();
        let after = f
            .input
            .read_with(visual, |input, _| input.layout_diagnostics().unwrap());
        assert!(after.source_shape_cache_hits >= before.source_shape_cache_hits + 10);
        assert_eq!(
            after.layout_calls, before.layout_calls,
            "no whole-buffer shaping on hover"
        );
        assert!(
            elapsed < Duration::from_millis(300),
            "ten cached native frames: {elapsed:?}"
        );
        f.input.update_in(visual, |input, window, cx| {
            input.replace_source_range(12..12, "x", window, cx);
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let edited = f
            .input
            .read_with(visual, |input, _| input.layout_diagnostics().unwrap());
        assert!(
            edited.layout_calls > after.layout_calls,
            "changed bytes must invalidate shapes"
        );
        f.reader.read_with(visual, |_, cx| {
            assert_eq!(value(&f, cx), original.replacen("aaa", "xaaa", 1));
        });
    }

    #[gpui::test]
    fn edit2803_typing_inside_a_code_body_keeps_the_toolbar_identity(cx: &mut TestAppContext) {
        let (f, visual) = open(cx, "Before\n```json\n{ a: 1 }\n```\nAfter\n");
        let key = f
            .reader
            .read_with(visual, |reader, cx| reader.code_operation_target_key(cx));
        for _ in 0..8 {
            visual.simulate_input(" ");
            visual.run_until_parked();
            f.reader.read_with(visual, |reader, cx| {
                assert_eq!(
                    reader.code_operation_target_key(cx),
                    key,
                    "pending fence classification must not hide the toolbar on each key"
                );
            });
            settle(visual);
            f.reader.read_with(visual, |reader, cx| {
                assert_eq!(
                    reader.code_operation_target_key(cx),
                    key,
                    "a key changes the body length, not its toolbar identity"
                );
            });
        }
        click(visual, "source-code-repair");
        click(visual, "source-code-repair-cancel");
        visual.simulate_input(" ");
        settle(visual);
        f.reader.read_with(visual, |reader, cx| {
            assert_eq!(reader.code_operation_target_key(cx), key);
            assert!(reader
                .editing
                .as_ref()
                .unwrap()
                .operations
                .preview
                .is_none());
        });
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
        visual.update(|window, cx| window.draw(cx).clear(cx));
        assert!(visual.debug_bounds("source-code-repair-apply").is_none());
        f.reader.read_with(visual, |reader, cx| {
            assert!(reader
                .editing
                .as_ref()
                .unwrap()
                .operations
                .preview
                .is_none());
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

#[cfg(test)]
mod large_tests;
