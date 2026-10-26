//! Opt-in presentation over the existing exact Reader editor and FileEditor.
use super::*;
use crate::source_presentation::{navigation_inventory as nav, CachedProvider, ProjectionColors};
use std::cell::Cell;

#[derive(Clone, PartialEq)]
struct Origin {
    reader: EntityId,
    root: PathBuf,
    reader_path: String,
    selected: String,
    file: PathBuf,
    input: EntityId,
    markdown: bool,
}
impl Origin {
    fn matches(&self, reader: &Reader, editing: &Editing, entity: EntityId) -> bool {
        self.reader == entity
            && self.root == reader.vault_root
            && self.reader_path == reader.current_rel
            && self.selected == reader.selected_file()
            && self.file == editing.store.path()
            && self.input == editing.input.entity_id()
            && self.markdown == editing.markdown
    }
}
struct Job {
    input: nav::Input,
    request: nav::Request,
}
struct Flight {
    key: nav::RequestKey,
    busy: Rc<Cell<bool>>,
}
/// Constructed before first poll; cancellation settles only this physical job.
struct FlightGuard(Rc<Flight>);
impl FlightGuard {
    fn finish(&self) {
        self.0.busy.set(false);
    }
}
impl Drop for FlightGuard {
    fn drop(&mut self) {
        self.finish();
    }
}

pub(super) struct LivePreview {
    pub enabled: bool,
    pub(super) restore_after_find: bool,
    origin: Origin,
    owner: nav::Owner,
    session: nav::Session,
    in_flight: Rc<Cell<bool>>,
    running: Option<Rc<Flight>>,
    task: Option<Task<()>>,
    queued: Option<Job>,
    // An interrupted request requires honest cold work after resume/refusal.
    needs_compute: bool,
    accepted: Option<Arc<CachedProvider>>,
    colors: Cell<Option<ProjectionColors>>,
    #[cfg(target_os = "linux")]
    latency_subscriptions: Option<(Subscription, Subscription, Subscription)>,
    #[cfg(test)]
    worker_reports: Vec<okilum_core::open_diagnostics::Report>,
}
impl LivePreview {
    pub(super) fn new(
        reader: &Reader,
        entity: EntityId,
        store: &FileEditor,
        input: &Entity<EditorState>,
        markdown: bool,
    ) -> Self {
        let owner = nav::Owner::new(input.entity_id());
        Self {
            enabled: false,
            restore_after_find: false,
            origin: Origin {
                reader: entity,
                root: reader.vault_root.clone(),
                reader_path: reader.current_rel.clone(),
                selected: reader.selected_file().to_owned(),
                file: store.path().to_owned(),
                input: input.entity_id(),
                markdown,
            },
            session: nav::Session::new(owner.clone()),
            owner,
            in_flight: Rc::new(Cell::new(false)),
            running: None,
            task: None,
            queued: None,
            needs_compute: false,
            accepted: None,
            colors: Cell::new(None),
            #[cfg(target_os = "linux")]
            latency_subscriptions: None,
            #[cfg(test)]
            worker_reports: Vec::new(),
        }
    }
    fn supersede(&mut self) {
        self.needs_compute |= self.in_flight.get() || self.queued.is_some();
        let _ = self.session.begin(None);
        self.queued = None;
    }
    pub(super) fn suspend(&mut self) {
        self.supersede();
        self.task = None; // Drop runs the pre-poll guard; the worker owns no UI state.
        self.running = None;
        self.in_flight = Rc::new(Cell::new(false));
        // Keep only the old immutable legacy display, never pending delivery rights.
    }
    fn owns(&self, flight: &Rc<Flight>) -> bool {
        self.running.as_ref().is_some_and(|f| Rc::ptr_eq(f, flight))
    }
}

#[cfg(test)]
impl LivePreview {
    pub(super) fn pending_marker(&self) -> Option<Rc<Cell<bool>>> {
        self.running.as_ref().map(|flight| flight.busy.clone())
    }
    pub(super) fn completed_navigation_bytes(&self) -> Vec<u64> {
        self.worker_reports
            .iter()
            .map(|r| r.counts["link_parse_bytes"])
            .collect()
    }
    /// The note as Live Preview draws it with nothing revealed, once it is
    /// classified: a test's proof that some Markdown really is concealed.
    pub(super) fn concealed_text(&self) -> Option<String> {
        use gpui_component::input::projection::{ActiveSource, ProjectionProvider as _};
        let provider = self.accepted.as_ref()?;
        let display = provider.compose(provider.source(), &ActiveSource::default())?;
        Some(display.text().to_string())
    }
}

fn projection_colors(cx: &App) -> ProjectionColors {
    let palette = brand::palette(cx);
    ProjectionColors {
        heading: palette.text,
        link: palette.link,
        muted: palette.text_muted,
    }
}

impl Reader {
    /// Opt-in, cross-platform diagnostics. Sampling reads state only: no notify/draw.
    /// `OKILUM_EDITOR_LAYOUT_DIAGNOSTICS=1` logs a sample every second. It and
    /// every Sprint build also log one `editor_frame_layout` line per laid-out
    /// keystroke or caret move: what the layout cost and how far the view
    /// moved (#1621), so a Feedback log carries the numbers.
    pub(super) fn start_editor_layout_diagnostics(
        &self,
        input: Entity<EditorState>,
        cx: &mut Context<Self>,
    ) {
        let samples = std::env::var("OKILUM_EDITOR_LAYOUT_DIAGNOSTICS").as_deref() == Ok("1");
        if !samples && !crate::reader_feedback::sprint::enabled() {
            return;
        }
        let Some(trace) = self
            .loading
            .as_ref()
            .and_then(|load| load.opts.diagnostics.clone())
        else {
            return;
        };
        input.update(cx, |input, _| input.enable_layout_diagnostics());
        cx.spawn(async move |reader, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let keep = reader.update(cx, |reader, cx| {
                    let Some(editing) = reader.editing.as_ref().filter(|e| e.input == input) else {
                        return false;
                    };
                    let editor = input.read(cx);
                    let Some(counts) = editor.layout_diagnostics() else { return false; };
                    for frame in editor.take_layout_frames() {
                        trace.event("editor_frame_layout", serde_json::json!({
                            "generation": frame.generation,
                            "presentation_epoch": frame.presentation_epoch,
                            "edit": frame.edit,
                            "raw": frame.raw,
                            "prepaint_ms": frame.prepaint_ms,
                            "reshape_ms": frame.reshape_ms,
                            "reshaped_lines": frame.reshaped_lines,
                            "scroll_delta": frame.scroll_delta,
                            "anchor": frame.anchor,
                            "live": editing.live_preview.enabled,
                        }));
                    }
                    if !samples {
                        return true;
                    }
                    let scroll = editor.scroll_offset();
                    let bounds = editor.input_bounds();
                    trace.event("editor_layout_sample", serde_json::json!({
                        "document": editor.source_stamp().document,
                        "generation": editor.source_stamp().generation,
                        "presentation_epoch": editor.presentation_epoch(),
                        "live": editing.live_preview.enabled,
                        "classification_in_flight": editing.live_preview.in_flight.get(),
                        "layout_calls": counts.layout_calls,
                        "metric_changes": counts.metric_changes,
                        "provider_applies": counts.provider_applies,
                        "projection_composes": counts.projection_composes,
                        "active_changes": counts.active_changes,
                        "scroll": [f32::from(scroll.x), f32::from(scroll.y)],
                        "bounds": [f32::from(bounds.origin.x), f32::from(bounds.origin.y),
                            f32::from(bounds.size.width), f32::from(bounds.size.height)],
                        "caret": editor.cursor_layout().map(|(b, _)| [f32::from(b.origin.x),
                            f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height)]),
                    }));
                    true
                }).unwrap_or(false);
                if !keep { break; }
            }
        }).detach();
    }

    pub(super) fn refresh_live_preview_colors(&self, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.as_ref().filter(|e| e.live_preview.enabled) else {
            return;
        };
        let colors = projection_colors(cx);
        if editing.live_preview.colors.get() == Some(colors) {
            return;
        }
        let Some(provider) = self.checked_live_provider(cx.entity_id(), cx) else {
            return;
        };
        // Publish the palette before notifying the input: the resulting render
        // observes it and cannot reinstall the same provider recursively.
        editing.live_preview.colors.set(Some(colors));
        let provider = provider.with_colors(colors);
        editing.input.update(cx, |input, cx| {
            input.set_projection_provider(Some(provider), cx)
        });
    }

    pub(crate) fn render_live_preview_control(
        &self,
        labels: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(editing) = &self.editing else {
            return div().into_any_element();
        };
        let oversize =
            editing.input.read(cx).text().len() > okilum_core::source_classifier::MAX_BYTES;
        let limited = oversize
            || editing
                .live_preview
                .accepted
                .as_ref()
                .is_some_and(|p| p.limited());
        let live = editing.live_preview.enabled;
        // One control switches to the other editor mode (#992); the current
        // mode is what the page shows, so it needs no selected segment.
        let button = if live {
            Button::new("reader-source")
                .icon(Icon::default().path("icons/code-xml.svg"))
                .when(labels, |button| button.label("Source"))
                .accessibility_label("Source")
                .tooltip(if limited {
                    "Live Preview uses Source for this note. Switch to Source"
                } else {
                    "Switch to Source"
                })
                .debug_selector(|| "reader-source".into())
                .on_click(
                    cx.listener(|this, _, window, cx| this.set_live_preview(false, window, cx)),
                )
        } else {
            Button::new("reader-live-preview")
                .icon(IconName::Eye)
                .when(labels, |button| button.label("Live Preview"))
                .accessibility_label("Live Preview")
                .tooltip(reader_shortcuts::hint(
                    if limited {
                        "Live Preview uses Source for this note"
                    } else {
                        "Live Preview"
                    },
                    &OpenLivePreview,
                    cx,
                ))
                .debug_selector(|| "reader-live-preview".into())
                .on_click(
                    cx.listener(|this, _, window, cx| this.set_live_preview(true, window, cx)),
                )
        };
        button
            .ghost()
            .small()
            .h(px(28.))
            .when(!labels, |b| b.w(px(28.)))
            .into_any_element()
    }

    pub(crate) fn set_live_preview(
        &mut self,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        #[cfg(target_os = "linux")]
        self.install_editor_latency_diagnostics(
            std::env::var("OKILUM_EDITOR_LATENCY_DIAGNOSTICS").as_deref() == Ok("1"),
            window,
            cx,
        );
        if let Some(editing) = &mut self.editing {
            editing.live_preview.restore_after_find = false;
            if editing.live_preview.enabled != enabled {
                self.toggle_live_preview(window, cx);
            }
        }
    }

    pub(crate) fn toggle_live_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_preview.is_some() {
            return;
        }
        let colors = projection_colors(cx);
        let held = self.checked_live_provider(cx.entity_id(), cx);
        let Some(editing) = &mut self.editing else {
            return;
        };
        editing.live_preview.restore_after_find = false;
        editing.live_preview.colors.set(Some(colors));
        editing.live_preview.enabled = !editing.live_preview.enabled;
        let input = editing.input.clone();
        let enabled = editing.live_preview.enabled;
        input.update(cx, |input, cx| {
            // Native search operates on source offsets and suppresses projection.
            // Close it before adopting Live Preview; Find returns to Source.
            if enabled {
                input.close_search(cx);
            }
            input.set_searchable(!enabled, cx);
        });
        let provider = if enabled {
            held.map(|p| p.with_colors(colors))
        } else {
            None
        };
        // None retains the native exact-source/grapheme opt-in from ExactSource.
        // Neither mode replaces the buffer or touches its undo transaction chain.
        input.update(cx, |input, cx| input.set_projection_provider(provider, cx));
        self.schedule_live_preview(cx);
        input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn live_input(&self, entity: EntityId, cx: &App) -> Option<nav::Input> {
        let editing = self.editing.as_ref()?;
        let state = &editing.live_preview;
        if !state.origin.matches(self, editing, entity) {
            return None;
        }
        let editor = editing.input.read(cx);
        Some(nav::Input {
            binding: nav::Binding {
                owner: state.owner.clone(),
                workspace: serde_json::json!({
                    "reader_root": self.vault_root.as_os_str().as_encoded_bytes(),
                    "file": editing.store.path().as_os_str().as_encoded_bytes(),
                    "markdown": editing.markdown,
                }),
                path: self.selected_file().to_owned(),
            },
            source: SourceSnapshot {
                stamp: editor.source_stamp(),
                text: Arc::from(editor.value().as_ref()),
            },
        })
    }
    fn checked_live_provider(&self, entity: EntityId, cx: &App) -> Option<Arc<CachedProvider>> {
        if self.move_applying {
            return None;
        }
        let live = self.live_input(entity, cx)?;
        let state = &self.editing.as_ref()?.live_preview;
        let provider = state.accepted.as_ref()?;
        (provider.source().stamp == live.source.stamp && provider.source().text == live.source.text)
            .then(|| provider.clone())
    }
    /// Called only after the real move callback has synchronized paths and text.
    pub(super) fn resume_live_preview(&mut self, cx: &mut Context<Self>) {
        if self.move_applying {
            return;
        }
        if self
            .editing
            .as_ref()
            .is_some_and(|e| e.store.text() == e.input.read(cx).value().as_ref())
        {
            self.schedule_live_preview(cx);
        }
    }
    pub(super) fn schedule_live_preview(&mut self, cx: &mut Context<Self>) {
        let captured = self.live_input(cx.entity_id(), cx);
        let parked = self.move_applying;
        let Some(editing) = &mut self.editing else {
            return;
        };
        let state = &mut editing.live_preview;
        let live = captured.filter(|i| {
            !parked
                && state.enabled
                && i.source.text.len() <= okilum_core::source_classifier::MAX_BYTES
        });
        // An identical already queued/running request is not a new attempt.
        if let Some(input) = &live {
            if state
                .queued
                .as_ref()
                .is_some_and(|j| state.session.is_pending(&j.request.key(), Some(input)))
                || state
                    .running
                    .as_ref()
                    .is_some_and(|f| state.session.is_pending(&f.key, Some(input)) && f.busy.get())
            {
                return;
            }
        }
        let held = live.as_ref().is_some_and(|i| {
            !state.needs_compute
                && state.accepted.as_ref().is_some_and(|p| {
                    p.source().stamp == i.source.stamp && p.source().text == i.source.text
                })
        });
        if held {
            // Same-snapshot legacy display reuse is NOT new NavSession adoption.
            let _ = state.session.begin(None);
            state.queued = None;
            return;
        }
        let interrupted = state.in_flight.get() || state.queued.is_some();
        let request = state.session.begin(live.clone()).ok(); // BEFORE every refusal/busy return.
        state.queued = None;
        let Some((input, request)) = live.zip(request) else {
            state.needs_compute |= interrupted;
            if editing.input.read(cx).text().len() > okilum_core::source_classifier::MAX_BYTES {
                state.accepted = None;
                editing
                    .input
                    .update(cx, |input, cx| input.set_projection_provider(None, cx));
            }
            return;
        };
        // Valid edits keep the installed legacy remap/raw-tail presentation.
        state.queued = Some(Job { input, request });
        self.start_live_preview(cx);
    }
    fn start_live_preview(&mut self, cx: &mut Context<Self>) {
        if self.move_applying {
            return;
        }
        let live = self.live_input(cx.entity_id(), cx);
        let Some(editing) = &mut self.editing else {
            return;
        };
        let state = &mut editing.live_preview;
        if state.in_flight.get() {
            return;
        }
        if let Some(task) = state.task.take() {
            task.detach();
        }
        state.running = None;
        let Some(job) = state.queued.take() else {
            return;
        };
        let key = job.request.key();
        if !state.enabled || !state.session.is_pending(&key, live.as_ref()) {
            self.schedule_live_preview(cx);
            return;
        }
        let input = editing.input.clone();
        let flight = Rc::new(Flight {
            key,
            busy: Rc::new(Cell::new(true)),
        });
        let guard = FlightGuard(flight.clone());
        state.in_flight = flight.busy.clone();
        state.running = Some(flight.clone());
        state.task = Some(cx.spawn(async move |this, cx| {
            let Job {
                input: source,
                request,
            } = job;
            let produced = cx
                .background_executor()
                .spawn(async move {
                    #[cfg(test)]
                    {
                        okilum_core::open_diagnostics::capture(|| {
                            CachedProvider::classify_navigation(request, source)
                        })
                    }
                    #[cfg(not(test))]
                    {
                        CachedProvider::classify_navigation(request, source)
                    }
                })
                .await;
            #[cfg(test)]
            let (produced, report) = produced;
            guard.finish();
            let _ = this.update(cx, |this, cx| {
                let live = this.live_input(cx.entity_id(), cx);
                let colors = projection_colors(cx);
                let parked = this.move_applying;
                let Some(editing) = &mut this.editing else {
                    return;
                };
                let state = &mut editing.live_preview;
                if !state.owns(&flight) {
                    return;
                }
                if let Some(task) = state.task.take() {
                    task.detach();
                }
                state.running = None;
                #[cfg(test)]
                state.worker_reports.push(report);
                // Obsolete work never enters destructive adopt or clears a new queue.
                if !parked
                    && state.enabled
                    && editing.input == input
                    && state.session.is_pending(&flight.key, live.as_ref())
                {
                    match (produced, live) {
                        (Ok((provider, completion)), Some(live)) => {
                            if state.session.adopt(completion, Some(&live)).is_ok() {
                                let provider = Arc::new(provider);
                                state.accepted = Some(provider.clone());
                                state.needs_compute = false;
                                state.colors.set(Some(colors));
                                input.update(cx, |input, cx| {
                                    input.set_projection_provider(
                                        Some(provider.with_colors(colors)),
                                        cx,
                                    )
                                });
                            }
                        }
                        _ => state.supersede(),
                    }
                }
                this.start_live_preview(cx);
                cx.notify();
            });
        }));
    }
}

// Linux CI only. CLOCK_MONOTONIC is shared with Python's monotonic_ns; diagnostic
// writer latency and wall-clock adjustments are excluded from the samples.
#[cfg(target_os = "linux")]
fn latency_now_ns() -> u64 {
    let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: clock_gettime initializes the correctly sized writable timespec.
    let result = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, time.as_mut_ptr()) };
    assert_eq!(result, 0, "editor diagnostic monotonic clock failed");
    // SAFETY: successful clock_gettime initialized time.
    let time = unsafe { time.assume_init() };
    time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64
}

#[cfg(target_os = "linux")]
impl Reader {
    fn install_editor_latency_diagnostics(
        &mut self,
        requested: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !requested {
            return;
        }
        let Some(trace) = self
            .loading
            .as_ref()
            .and_then(|l| l.opts.diagnostics.clone())
        else {
            return;
        };
        let Some(editing) = &mut self.editing else {
            return;
        };
        if editing.live_preview.latency_subscriptions.is_some() {
            return;
        }
        let input = editing.input.clone();
        // GPUI's interceptor runs at application key dispatch, before key
        // bindings, focused-editor actions and default text insertion. Observe
        // only: never stop propagation, notify or request an extra frame.
        let key_received = Rc::new(Cell::new(None));
        let key_seen = key_received.clone();
        let key_input = input.clone();
        let handle = window.window_handle();
        let key = cx.intercept_keystrokes(move |_, event_window, app| {
            let received_ns = latency_now_ns();
            if event_window.window_handle() != handle {
                return;
            }
            let editor = key_input.read(app);
            if editor.focus_handle(app).is_focused(event_window) {
                key_seen.set(Some((editor.source_stamp(), received_ns)));
            } else {
                key_seen.set(None);
            }
        });
        let edit_trace = trace.clone();
        let changed = cx.subscribe_in(
            &input,
            window,
            move |this, input, event: &SourceMutation, window, cx| {
                let mutation_ns = latency_now_ns();
                // One event can yield one measured mutation. Programmatic/IME
                // changes without a matching preceding key stay unmeasured.
                let key_ns = key_received
                    .take()
                    .filter(|(before, received)| {
                        before.document == event.stamp.document
                            && before.generation.checked_add(1) == Some(event.stamp.generation)
                            && *received <= mutation_ns
                            && mutation_ns - *received <= 1_000_000_000
                    })
                    .map_or(0, |(_, received)| received);
                this.trace_editor_latency_frame(
                    input.clone(),
                    event.stamp,
                    "edit",
                    mutation_ns,
                    key_ns,
                    edit_trace.clone(),
                    window,
                    cx,
                );
            },
        );
        let last = Rc::new(Cell::new(None));
        let live_trace = trace.clone();
        let classified = cx.observe_in(&input, window, move |this, input, window, cx| {
            let Some(editing) = this.editing.as_ref().filter(|e| e.input == input) else {
                return;
            };
            if !editing.live_preview.enabled {
                return;
            }
            let editor = input.read(cx);
            let stamp = editor.source_stamp();
            let oversize = editor.text().len() > okilum_core::source_classifier::MAX_BYTES;
            let accepted = editing
                .live_preview
                .accepted
                .as_ref()
                .filter(|provider| provider.source().stamp == stamp);
            if !oversize && accepted.is_none() {
                return;
            }
            let limited = oversize || accepted.is_some_and(|provider| provider.limited());
            let identity = (stamp, editor.presentation_epoch(), limited);
            if last.replace(Some(identity)) == Some(identity) {
                return;
            }
            live_trace.event(
                if limited {
                    "live_preview_limited"
                } else {
                    "live_preview_accepted"
                },
                serde_json::json!({"document": stamp.document, "generation": stamp.generation,
                    "presentation_epoch": editor.presentation_epoch(), "limited": limited,
                    "monotonic_ns": latency_now_ns()}),
            );
            if !limited {
                this.trace_editor_latency_frame(
                    input.clone(),
                    stamp,
                    "live",
                    0,
                    0,
                    live_trace.clone(),
                    window,
                    cx,
                );
            }
        });
        editing.live_preview.latency_subscriptions = Some((changed, classified, key));
        let stamp = input.read(cx).source_stamp();
        self.trace_editor_latency_frame(input, stamp, "open", 0, 0, trace, window, cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn trace_editor_latency_frame(
        &self,
        input: Entity<EditorState>,
        stamp: gpui_component::input::projection::SourceStamp,
        purpose: &'static str,
        mutation_ns: u64,
        key_ns: u64,
        trace: reader_diagnostics::Trace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // GPUI invokes next-frame callbacks BEFORE draw/present. The nested
        // callback is after that draw/present returned, before a later frame.
        // This is a conservative software post-frame boundary, never scanout.
        cx.on_next_frame(window, move |_, window, cx| {
            cx.on_next_frame(window, move |this, _, cx| {
                let matching = this.editing.as_ref().filter(|e| e.input == input);
                let editor = input.read(cx);
                let current = editor.source_stamp();
                let live = matching.is_some_and(|e| e.live_preview.enabled);
                let limited = matching
                    .and_then(|e| e.live_preview.accepted.as_ref())
                    .is_none_or(|p| p.limited());
                let outcome = if matching.is_none() || current != stamp {
                    "superseded"
                } else if editor
                    .range_to_bounds(&(editor.cursor()..editor.cursor()))
                    .is_some()
                {
                    "painted"
                } else {
                    "unpainted"
                };
                trace.event("editor_latency_frame", serde_json::json!({
                    "purpose": purpose, "outcome": outcome,
                    "document": stamp.document, "generation": stamp.generation,
                    "current_document": current.document, "current_generation": current.generation,
                    "presentation_epoch": editor.presentation_epoch(),
                    "mutation_ns": mutation_ns, "key_received_ns": key_ns,
                    "key_boundary": "gpui-before-key-bindings-and-text-input",
                    "monotonic_ns": latency_now_ns(),
                    "live": live, "limited": limited,
                }));
            });
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn latency_fence_fixture_forces_raw_tail_after_local_remap_refuses() {
        use okilum_core::{
            source_classifier::{classify, RetainedPresentation},
            source_projection::Snapshot,
        };
        let source = include_str!("../../../../scripts/perf/editor/raw-tail-60k.md");
        assert_eq!(source.len(), 60 * 1024);
        let original = Snapshot::new("raw-tail-latency", 1, source);
        let retained = RetainedPresentation::new(&classify(&original));
        let edited = Snapshot::new("raw-tail-latency", 2, format!("{source}\n"));
        assert!(
            retained.remap(&edited).is_none(),
            "the latency workload must exercise the fallback, not a local remap"
        );
        assert!(
            retained.remap_raw_tail(&edited).is_some(),
            "Enter at EOF inside this unclosed fence must run remap_raw_tail"
        );
    }

    #[cfg(target_os = "linux")]
    #[gpui::test]
    fn latency_diagnostics_require_opt_in_and_trace(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let vault = fixture.path().join("notes");
        let state = fixture.path().join("state");
        std::fs::create_dir(&vault).unwrap();
        std::fs::write(vault.join("Note.md"), "# Note\n\nBody\n").unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
            reader_ui_state::install(&state, cx);
            cx.set_global(reader_history::TestSessionDirectory(
                fixture.path().join("history"),
            ));
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(vault.clone()),
                        note: Some("Note.md".into()),
                        index_dir: Some(fixture.path().join("index")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx);
            reader.loading.as_mut().unwrap().opts.diagnostics = None;
            reader.install_editor_latency_diagnostics(true, window, cx);
            assert!(reader
                .editing
                .as_ref()
                .unwrap()
                .live_preview
                .latency_subscriptions
                .is_none());
            reader.loading.as_mut().unwrap().opts.diagnostics = Some(
                reader_diagnostics::Trace::new(Some(state.clone()), Some(vault.clone())),
            );
            reader.install_editor_latency_diagnostics(false, window, cx);
            assert!(reader
                .editing
                .as_ref()
                .unwrap()
                .live_preview
                .latency_subscriptions
                .is_none());
        });
        visual.simulate_keystrokes("x");
        visual.run_until_parked();
        assert!(
            !std::fs::read_to_string(state.join("reader-diagnostic.log"))
                .unwrap_or_default()
                .contains("editor_latency_frame")
        );
        reader.update_in(visual, |reader, window, cx| {
            reader.install_editor_latency_diagnostics(true, window, cx);
            assert!(reader
                .editing
                .as_ref()
                .unwrap()
                .live_preview
                .latency_subscriptions
                .is_some());
        });
        visual.simulate_keystrokes("y");
        visual.run_until_parked();
        for _ in 0..2 {
            visual.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
        }
        // Diagnostics are asynchronous; wait only for the writer, outside the
        // measured path. A positive control proves the disabled assertion above.
        let log = state.join("reader-diagnostic.log");
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let events: Vec<serde_json::Value> = std::fs::read_to_string(&log)
                .unwrap_or_default()
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();
            if events.iter().any(|event| {
                event["phase"] == "editor_latency_frame"
                    && event["details"]["purpose"] == "edit"
                    && event["details"]["outcome"] == "painted"
            }) {
                let measured = events
                    .iter()
                    .find(|event| {
                        event["phase"] == "editor_latency_frame"
                            && event["details"]["purpose"] == "edit"
                            && event["details"]["outcome"] == "painted"
                    })
                    .unwrap();
                let key_ns = measured["details"]["key_received_ns"].as_u64().unwrap();
                let mutation_ns = measured["details"]["mutation_ns"].as_u64().unwrap();
                assert!(
                    key_ns > 0 && key_ns <= mutation_ns,
                    "the app key-dispatch positive control must precede mutation"
                );
                assert!(
                    events
                        .iter()
                        .any(|event| event["phase"] == "editor_latency_frame"
                            && event["details"]["outcome"] == "superseded"),
                    "the older open generation must not become a latency sample"
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "enabled frame event missing"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[cfg(unix)]
    #[gpui::test]
    fn mode_segments_follow_labels_preference_and_switch_editor_presentation(
        cx: &mut TestAppContext,
    ) {
        let fixture = tempfile::tempdir().unwrap();
        let vault = fixture.path().join("Personal knowledge — 研究");
        std::fs::create_dir(&vault).unwrap();
        std::fs::write(vault.join("Note.md"), "# Note\n\nBody text\n").unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
            reader_ui_state::install(&fixture.path().join("state"), cx);
            cx.set_global(reader_history::TestSessionDirectory(
                fixture.path().join("history"),
            ));
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(vault.clone()),
                        note: Some("Note.md".into()),
                        index_dir: Some(fixture.path().join("index")),
                        panel_settings_override: Some(fixture.path().join("panels.json")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        visual.simulate_resize(size(px(1400.), px(900.)));
        visual.run_until_parked();
        let icon_width = visual.debug_bounds("reader-edit").unwrap().size.width;
        visual.update(|_, cx| reader_ui_state::set_toolbar_labels(true, cx));
        visual.run_until_parked();
        let edit = visual.debug_bounds("reader-edit").unwrap();
        assert!(
            edit.size.width > icon_width,
            "labels change the actual rendered control"
        );
        visual.simulate_click(edit.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| assert!(reader.editing.is_some()));
        assert!(
            visual.debug_bounds("source-save").is_none(),
            "clean source needs no Save glyph"
        );
        let live = visual.debug_bounds("reader-live-preview").unwrap();
        visual.simulate_click(live.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.editing.as_ref().unwrap().live_preview.enabled)
        });
        let source = visual.debug_bounds("reader-source").unwrap();
        visual.simulate_click(source.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(!reader.editing.as_ref().unwrap().live_preview.enabled)
        });
        // The editor offers only Read, never a second, selected Edit (#992).
        assert!(visual.debug_bounds("reader-edit").is_none());
        let read = visual.debug_bounds("reader-read").unwrap();
        visual.simulate_click(read.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.editing.is_none());
            assert!(reader.renaming.is_none());
        });
        // Reader offers only Edit: the pencil, never Rename (#871).
        assert!(visual.debug_bounds("reader-read").is_none());
        let pencil = visual.debug_bounds("reader-edit").unwrap();
        visual.simulate_click(pencil.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.editing.is_some(), "pencil enters the editor");
            assert!(!reader.source_live_preview());
        });
        reader.update_in(visual, |reader, window, cx| {
            reader.set_live_preview(true, window, cx)
        });
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.editing.is_none());
            assert!(
                reader.ui_state.live_preview,
                "Reader retains the last editor mode"
            );
        });
        reader.update_in(visual, |reader, _, cx| {
            assert!(reader.leave_source(cx), "repeated leave is a no-op");
            assert!(reader.ui_state.live_preview);
        });
        let pencil = visual.debug_bounds("reader-edit").unwrap();
        visual.simulate_click(pencil.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.editing.is_some(), "pencil re-enters the editor");
            assert!(reader.source_live_preview());
        });
        let read = visual.debug_bounds("reader-read").unwrap();
        visual.simulate_click(read.center(), Modifiers::default());
        visual.run_until_parked();
        // One mode button and one mode toggle leave room for labels at 480px (#992).
        visual.simulate_resize(size(px(360.), px(900.)));
        reader.update_in(visual, |reader, window, cx| {
            reader.toggle_source(window, cx)
        });
        visual.run_until_parked();
        assert_eq!(
            visual.debug_bounds("reader-read").unwrap().size.width,
            icon_width,
            "optional labels yield to glyphs in a narrow editor"
        );
        visual.simulate_keystrokes("f2");
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(reader.renaming.as_ref().unwrap().in_header)
        });
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        let title = visual.debug_bounds("reader-document-root").unwrap();
        visual.simulate_click(title.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(
                reader.renaming.as_ref().unwrap().in_header,
                "title click renames"
            );
        });
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        let header = visual.debug_bounds("document-header").unwrap();
        assert!(visual.debug_bounds("reader-edit").is_none());
        assert!(visual.debug_bounds("reader-live-preview").is_none());
        for control in [
            "reader-history-back",
            "reader-history-forward",
            "reader-read",
            "reader-source",
            "document-more",
        ] {
            let bounds = visual.debug_bounds(control).unwrap();
            assert!(
                bounds.left() >= header.left() && bounds.right() <= header.right(),
                "{control} stays inside narrow header"
            );
        }
        visual.simulate_resize(size(px(1100.), px(900.)));
        reader.update_in(visual, |reader, _, cx| {
            reader.panel_widths.notes = 200.;
            reader.panels.open(reader_layout::Panel::Notes);
            cx.notify();
        });
        visual.run_until_parked();
        let panel = visual.debug_bounds("reader-notes-panel").unwrap();
        // #1512: at 200 px the name gives way and only Reveal current and
        // Show hidden move into ⋯; nothing clips past the panel.
        let menu = visual.debug_bounds("sidebar-actions").unwrap();
        assert!(
            menu.right() < panel.right(),
            "menu={menu:?}, panel={panel:?}"
        );
        if cfg!(any(unix, windows)) {
            let new_note = visual.debug_bounds("sidebar-new-note").unwrap();
            let title = visual.debug_bounds("sidebar-vault-title").unwrap();
            assert!(
                title.right() <= new_note.left(),
                "the name never covers an action"
            );
        }
        assert!(visual.debug_bounds("sidebar-collapse-all").is_some());
        for spilled in ["sidebar-reveal-current", "sidebar-show-hidden"] {
            assert!(visual.debug_bounds(spilled).is_none(), "{spilled} is in ⋯");
        }
    }

    use super::*;
    use ::core::prelude::v1::test;
    use gpui_component::input::projection::ActiveSource;

    const ORIGINAL: &str =
        "\u{feff}# Привет 🧠\r\n\r\n**Жирный** e\u{301} и `code` [[target|ссылка]]\r\n\r\nTail\r\n";

    fn fixture<'a>(
        cx: &'a mut TestAppContext,
        text: &str,
    ) -> (Entity<Reader>, &'a mut VisualTestContext, tempfile::TempDir) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("note.md"), text).unwrap();
        std::fs::write(root.join("target.md"), "# Other\n\n**target**\n").unwrap();
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
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        (reader, visual, directory)
    }

    /// #1192: in Live Preview a callout's chevron folds and unfolds the
    /// rendered callout; the press does not reveal its source.
    #[gpui::test]
    fn live_preview_callout_chevron_toggles_fold_without_revealing(cx: &mut TestAppContext) {
        let note = "# Callouts\n\n> [!note]- First callout\n> FIRST BODY\n\n\
            > [!tip]- Second callout\n> SECOND BODY\n\nControl **bold**\n";
        let (reader, visual, _directory) = fixture(cx, note);
        reader.update_in(visual, |r, window, cx| {
            if !r.editing.as_ref().unwrap().live_preview.enabled {
                r.toggle_live_preview(window, cx);
            }
        });
        visual.run_until_parked();
        let input = reader.read_with(visual, |r, _| r.editing.as_ref().unwrap().input.clone());
        input.update(visual, |input, cx| {
            input.set_selected_range(note.len()..note.len(), cx)
        });
        let draw = |visual: &mut VisualTestContext| {
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
        };
        draw(visual);
        // Positive control: both callouts are rendered and folded.
        let chevron = visual
            .debug_bounds("reader-callout-fold-0")
            .expect("a rendered callout chevron");
        assert!(visual.debug_bounds("reader-callout-body-0").is_none());
        visual.simulate_click(chevron.center(), Modifiers::default());
        draw(visual);
        input.read_with(visual, |input, _| {
            assert_eq!(
                input.selected_range(),
                note.len()..note.len(),
                "caret stays"
            );
            assert_eq!(input.value().as_ref(), note);
        });
        assert!(
            visual.debug_bounds("reader-callout-body-0").is_some(),
            "the first callout unfolds, rendered"
        );
    }

    /// #1621: Enter on the blank lines after a long CSS fence at the end of a
    /// note, below a heading, a long link, a table and a callout. Each edit
    /// frame is drawn before the background classification returns, as on a
    /// slow machine. That frame drew the whole note as source with no anchor:
    /// the rows above changed height and the view jumped on every Enter.
    #[gpui::test]
    fn enter_after_a_fence_below_rendered_blocks_keeps_the_view(cx: &mut TestAppContext) {
        use crate::input_anchor_tests::{assert_held, draw};
        let css: String = (0..60)
            .map(|i| format!("  .rule-{i} {{ padding: {i}px; }}\n"))
            .collect();
        let note = format!(
            "# Days Gone\n\n## Section\n\n[a long link label](https://example.com/{})\n\n\
             | a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n\n> [!note] Callout\n> body\n\n\
             ```css\n{css}```\n\n",
            "x".repeat(300)
        );
        let (reader, visual, _directory) = fixture(cx, &note);
        visual.simulate_resize(size(px(1000.), px(600.)));
        reader.update_in(visual, |r, window, cx| {
            if !r.editing.as_ref().unwrap().live_preview.enabled {
                r.toggle_live_preview(window, cx);
            }
        });
        visual.run_until_parked();
        let input = reader.read_with(visual, |r, _| r.editing.as_ref().unwrap().input.clone());
        input.update(visual, |input, cx| {
            input.set_selected_range(note.len()..note.len(), cx)
        });
        let bounds = input.read_with(visual, |input, _| input.input_bounds());
        visual.simulate_event(ScrollWheelEvent {
            position: bounds.center(),
            delta: ScrollDelta::Pixels(point(px(0.), px(-100_000.))),
            ..Default::default()
        });
        visual.run_until_parked();
        // The closing fence line, a few rows above the caret.
        let reference = note.rfind("```").unwrap();
        draw(&input, reference, visual);
        visual.run_until_parked();
        let mut before = draw(&input, reference, visual);
        input.read_with(visual, |input, _| {
            assert!(
                input.scroll_offset().y < px(-200.),
                "positive control: the editor is scrolled"
            );
            assert!(input.value().starts_with("# Days Gone"));
        });
        assert!(before.caret.is_some() && before.reference.is_some());
        for key in 1..=8 {
            visual.update(|window, cx| {
                window.dispatch_keystroke(gpui::Keystroke::parse("enter").unwrap(), cx);
            });
            let edit = draw(&input, reference, visual);
            input.read_with(visual, |input, _| {
                assert_eq!(
                    input.value().len(),
                    note.len() + key,
                    "Enter {key} reached the editor"
                )
            });
            assert_held(
                &before,
                &edit,
                &format!("Enter {key}, before classification"),
            );
            visual.run_until_parked();
            let adopted = draw(&input, reference, visual);
            assert_held(
                &edit,
                &adopted,
                &format!("Enter {key}, classification adopted"),
            );
            // Blocks measured in that frame take their height in the next one.
            visual.run_until_parked();
            let settled = draw(&input, reference, visual);
            assert_held(&adopted, &settled, &format!("Enter {key}, settled"));
            before = settled;
        }
    }

    #[gpui::test]
    fn save_echo_batches_preserve_editor_projection_and_viewport(cx: &mut TestAppContext) {
        let original = format!(
            "# Echo\n\n{}",
            "**Bold** wrapped paragraph with [[target|label]].\n\n".repeat(80)
        );
        let (reader, visual, dir) = fixture(cx, &original);
        reader.update_in(visual, |r, window, cx| {
            r.toggle_live_preview(window, cx);
            r.editing.as_ref().unwrap().input.update(cx, |input, cx| {
                input.enable_layout_diagnostics();
                input.set_value(format!("{original}Saved\n"), window, cx);
            });
            assert!(r.save_source(cx));
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, _, cx| {
            r.editing.as_ref().unwrap().input.update(cx, |input, cx| {
                input.set_scroll_offset(point(px(0.), px(-120.)), cx);
            });
        });
        visual.run_until_parked();
        let snapshot = |r: &Reader, cx: &App| {
            let e = r.editing.as_ref().unwrap();
            let input = e.input.read(cx);
            (
                e.input.entity_id(),
                input.source_stamp(),
                input.presentation_epoch(),
                input.selected_range(),
                input.scroll_offset(),
                input.value().to_string(),
                input.layout_diagnostics().unwrap().provider_applies,
            )
        };
        let before = reader.read_with(visual, snapshot);
        let work = reader.read_with(visual, |r, _| {
            r.editing
                .as_ref()
                .unwrap()
                .live_preview
                .completed_navigation_bytes()
        });
        assert!(
            !work.is_empty(),
            "actual classification worker positive control"
        );
        let accepted = reader.read_with(visual, |r, _| {
            r.editing
                .as_ref()
                .unwrap()
                .live_preview
                .accepted
                .clone()
                .unwrap()
        });
        assert!(
            before.4.y < px(0.),
            "positive control: genuinely scrolled editor"
        );
        for _ in 0..3 {
            // Same bytes, new metadata: includes sync/save echo that must still be indexed.
            let path = dir.path().join("vault/note.md");
            let bytes = std::fs::read(&path).unwrap();
            std::fs::write(&path, bytes).unwrap();
            reader.update_in(visual, |r, window, cx| {
                r.apply_vault_changes(
                    okilum_core::Changes {
                        changed: ["note.md".to_owned()].into(),
                        directories: [String::new()].into(),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            visual.run_until_parked();
            assert_eq!(reader.read_with(visual, snapshot), before);
            reader.read_with(visual, |r, cx| {
                assert_eq!(
                    r.editing
                        .as_ref()
                        .unwrap()
                        .live_preview
                        .completed_navigation_bytes(),
                    work,
                    "same-byte metadata echo cannot reparse classification"
                );
                let (_, report) = okilum_core::open_diagnostics::capture(|| {
                    assert!(r
                        .checked_live_provider(
                            r.editing.as_ref().unwrap().live_preview.origin.reader,
                            cx
                        )
                        .is_some());
                });
                assert!(!report.counts.contains_key("link_parse_bytes"));
                assert!(!report.phases.contains_key("parse"));
            });
            reader.read_with(visual, |r, _| {
                assert!(Arc::ptr_eq(
                    &accepted,
                    r.editing
                        .as_ref()
                        .unwrap()
                        .live_preview
                        .accepted
                        .as_ref()
                        .unwrap()
                ))
            });
        }
        // Different bytes must not be hidden by the echo guard.
        std::fs::write(dir.path().join("vault/note.md"), "# Real external change\n").unwrap();
        reader.update_in(visual, |r, window, cx| {
            r.apply_vault_changes(
                okilum_core::Changes {
                    changed: ["note.md".to_owned()].into(),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        visual.run_until_parked();
        let after = reader.read_with(visual, snapshot);
        assert_ne!(after.1, before.1);
        assert_eq!(after.5, "# Real external change\n");
    }

    #[gpui::test]
    fn layout_counter_sampling_does_not_request_presentation_changes(cx: &mut TestAppContext) {
        let (reader, visual, _) = fixture(cx, ORIGINAL);
        reader.update_in(visual, |r, window, cx| {
            r.editing
                .as_ref()
                .unwrap()
                .input
                .update(cx, |input, _| input.enable_layout_diagnostics());
            r.toggle_live_preview(window, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            let input = r.editing.as_ref().unwrap().input.read(cx);
            let counts = input.layout_diagnostics().unwrap();
            assert!(counts.provider_applies > 0 && counts.projection_composes > 0);
            let epoch = input.presentation_epoch();
            for _ in 0..100 {
                assert_eq!(input.layout_diagnostics(), Some(counts));
                assert_eq!(input.presentation_epoch(), epoch);
            }
        });
    }

    #[gpui::test]
    fn find_returns_to_source_and_live_preview_closes_source_search(cx: &mut TestAppContext) {
        let (reader, visual, _) = fixture(cx, ORIGINAL);
        reader.update_in(visual, |r, window, cx| {
            r.open_source_find(cx);
            assert!(
                r.editing
                    .as_ref()
                    .unwrap()
                    .input
                    .read(cx)
                    .search_session()
                    .open
            );
            r.toggle_live_preview(window, cx);
            let input = r.editing.as_ref().unwrap().input.clone();
            assert!(!input.read(cx).search_session().open);
            // Positive control: native search must be disabled while projected.
            input.update(cx, |input, cx| input.open_search(false, cx));
            assert!(!input.read(cx).search_session().open);
            let stamp = input.read(cx).source_stamp();
            r.open_source_find(cx);
            assert!(!r.editing.as_ref().unwrap().live_preview.enabled);
            assert!(input.read(cx).search_session().open);
            assert_eq!(input.read(cx).source_stamp(), stamp);
            assert_eq!(input.read(cx).value().as_ref(), ORIGINAL);
        });
    }

    #[gpui::test]
    fn closing_find_restores_live_preview_but_preserves_source(cx: &mut TestAppContext) {
        let (reader, visual, _) = fixture(cx, ORIGINAL);
        reader.update_in(visual, |r, window, cx| r.toggle_live_preview(window, cx));
        visual.run_until_parked();
        let input = reader.read_with(visual, |r, _| r.editing.as_ref().unwrap().input.clone());
        let stamp = input.read_with(visual, |input, _| input.source_stamp());
        // The menu action, including repeated activation, must remember Live Preview.
        reader.update_in(visual, |r, window, cx| {
            r.open_find(window, cx);
            r.open_find(window, cx);
            assert!(!r.editing.as_ref().unwrap().live_preview.enabled);
            assert!(input.read(cx).search_session().open);
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            assert!(!input.read(cx).search_session().open);
            assert!(r.editing.as_ref().unwrap().live_preview.enabled);
            assert_eq!(input.read(cx).source_stamp(), stamp);
            assert_eq!(input.read(cx).value().as_ref(), ORIGINAL);
        });
        // The toolbar close uses this same EditorState close notification.
        reader.update_in(visual, |r, window, cx| r.open_find(window, cx));
        input.update(visual, |input, cx| input.close_search(cx));
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(r.editing.as_ref().unwrap().live_preview.enabled)
        });
        reader.update_in(visual, |r, window, cx| {
            r.toggle_live_preview(window, cx);
            r.open_find(window, cx);
        });
        input.update(visual, |input, cx| input.close_search(cx));
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(!r.editing.as_ref().unwrap().live_preview.enabled)
        });
    }

    #[gpui::test]
    fn theme_changes_reuse_classification_without_editing_source(cx: &mut TestAppContext) {
        let (reader, visual, directory) = fixture(cx, ORIGINAL);
        reader.update_in(visual, |r, window, cx| r.toggle_live_preview(window, cx));
        visual.run_until_parked();
        let (accepted, stamp, height) = reader.read_with(visual, |r, cx| {
            let editing = r.editing.as_ref().unwrap();
            (
                editing.live_preview.accepted.clone().unwrap(),
                editing.input.read(cx).source_stamp(),
                editing.input.read(cx).line_height(),
            )
        });
        for mode in [
            gpui_component::ThemeMode::Dark,
            gpui_component::ThemeMode::Light,
        ] {
            visual.update(|window, cx| set_appearance(Some(mode), None, window, cx));
            visual.run_until_parked();
            visual.update(|window, cx| window.draw(cx).clear(cx));
            reader.read_with(visual, |r, cx| {
                let editing = r.editing.as_ref().unwrap();
                assert!(Arc::ptr_eq(
                    &accepted,
                    editing.live_preview.accepted.as_ref().unwrap()
                ));
                assert_eq!(
                    editing.live_preview.colors.get(),
                    Some(projection_colors(cx))
                );
                assert_eq!(editing.input.read(cx).source_stamp(), stamp);
                assert_eq!(editing.input.read(cx).value().as_ref(), ORIGINAL);
                assert_eq!(editing.input.read(cx).line_height(), height);
                assert!(!editing.store.dirty());
            });
            let epoch = reader.read_with(visual, |r, cx| {
                r.editing
                    .as_ref()
                    .unwrap()
                    .input
                    .read(cx)
                    .presentation_epoch()
            });
            visual.update(|window, cx| window.draw(cx).clear(cx));
            reader.read_with(visual, |r, cx| {
                assert_eq!(
                    r.editing
                        .as_ref()
                        .unwrap()
                        .input
                        .read(cx)
                        .presentation_epoch(),
                    epoch,
                    "unchanged palette must not reinstall its provider on redraw"
                );
            });
        }
        assert_eq!(
            std::fs::read(directory.path().join("vault/note.md")).unwrap(),
            ORIGINAL.as_bytes()
        );
    }

    #[gpui::test]
    fn latest_only_classification_rejects_stale_notes_and_preserves_large_source(
        cx: &mut TestAppContext,
    ) {
        let (reader, visual, directory) = fixture(cx, ORIGINAL);
        let input = reader.update_in(visual, |r, window, cx| {
            r.toggle_live_preview(window, cx);
            let editing = r.editing.as_ref().unwrap();
            assert!(
                editing.live_preview.in_flight.get(),
                "positive control: actual background classification started"
            );
            let input = editing.input.clone();
            input.update(cx, |s, cx| {
                s.replace_text_in_range(Some(0..0), "first ", window, cx)
            });
            input.update(cx, |s, cx| {
                s.replace_text_in_range(Some(0..0), "latest ", window, cx)
            });
            input
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            let e = r.editing.as_ref().unwrap();
            assert!(!e.live_preview.in_flight.get() && e.live_preview.queued.is_none());
            let p = e.live_preview.accepted.as_ref().unwrap();
            assert_eq!(p.source().stamp, input.read(cx).source_stamp());
            assert_eq!(
                p.source().text.as_ref(),
                ORIGINAL.replacen('\u{feff}', "\u{feff}latest first ", 1)
            );
            let display = p.compose(p.source(), &ActiveSource::default()).unwrap();
            assert_ne!(
                display.text(),
                p.source().text.as_ref(),
                "positive control: non-active Markdown is concealed"
            );
        });
        reader.update_in(visual, |r, window, cx| {
            input.update(cx, |s, cx| {
                s.replace_text_in_range(Some(0..0), "pending ", window, cx)
            });
            r.schedule_live_preview(cx);
            assert!(r.editing.as_ref().unwrap().live_preview.in_flight.get());
            assert!(r.leave_source(cx));
            r.open_note("target.md", None, window, cx);
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| {
            r.toggle_source(window, cx);
            r.set_live_preview(true, window, cx);
            assert!(
                r.source_live_preview(),
                "classify the next note in Live Preview"
            );
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            let e = r.editing.as_ref().unwrap();
            assert_ne!(e.input, input);
            assert_eq!(
                e.live_preview
                    .accepted
                    .as_ref()
                    .unwrap()
                    .source()
                    .text
                    .as_ref(),
                "# Other\n\n**target**\n"
            );
            assert_eq!(e.input.read(cx).value().as_ref(), "# Other\n\n**target**\n");
        });
        let large = format!("# {}\r\n", "🧠".repeat(17_000));
        reader.update_in(visual, |r, window, cx| {
            r.editing
                .as_ref()
                .unwrap()
                .input
                .update(cx, |s, cx| s.set_value(large.clone(), window, cx));
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            let e = r.editing.as_ref().unwrap();
            assert!(e.live_preview.accepted.is_none());
            assert_eq!(e.input.read(cx).value().as_ref(), large);
        });
        assert_eq!(
            std::fs::read_to_string(directory.path().join("vault/target.md")).unwrap(),
            "# Other\n\n**target**\n"
        );
    }

    #[gpui::test]
    fn glyph_toggle_preserves_buffer_undo_copy_and_durable_conflict(cx: &mut TestAppContext) {
        let (reader, visual, directory) = fixture(cx, ORIGINAL);
        let input = reader.read_with(visual, |r, _| r.editing.as_ref().unwrap().input.clone());
        let stamp = input.read_with(visual, |i, _| i.source_stamp());
        let toggle = visual.debug_bounds("reader-live-preview").unwrap().center();
        visual.simulate_click(toggle, Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            assert!(r.editing.as_ref().unwrap().live_preview.enabled);
            assert_eq!(input.read(cx).source_stamp(), stamp);
            assert_eq!(input.read(cx).value().as_ref(), ORIGINAL);
            assert!(!r.editing.as_ref().unwrap().store.dirty());
        });
        reader.update_in(visual, |_, window, cx| {
            input.update(cx, |i, cx| {
                i.set_selected_range(0..ORIGINAL.len(), cx);
                assert_eq!(
                    i.text_for_range(0..ORIGINAL.encode_utf16().count(), &mut None, window, cx)
                        .as_deref(),
                    Some(ORIGINAL)
                );
            })
        });
        #[cfg(target_os = "macos")]
        visual.simulate_keystrokes("cmd-c");
        #[cfg(not(target_os = "macos"))]
        visual.simulate_keystrokes("ctrl-c");
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some(ORIGINAL)
            )
        });
        reader.update_in(visual, |_, window, cx| {
            input.update(cx, |i, cx| {
                i.set_selected_range(0..0, cx);
                i.replace_text_in_range(Some(0..0), "local ", window, cx);
            })
        });
        visual.run_until_parked();
        // Actual toolbar clicks must not save on focus loss or create history
        // transactions. The single mode toggle offers the other mode (#992).
        for id in ["reader-source", "reader-live-preview"] {
            assert!(visual
                .debug_bounds(if id == "reader-source" {
                    "reader-live-preview"
                } else {
                    "reader-source"
                })
                .is_none());
            let toggle = visual.debug_bounds(id).unwrap().center();
            visual.simulate_click(toggle, Modifiers::default());
            visual.run_until_parked();
        }
        reader.read_with(visual, |r, _| {
            assert!(r.editing.as_ref().unwrap().live_preview.enabled)
        });
        assert_eq!(
            std::fs::read_to_string(directory.path().join("vault/note.md")).unwrap(),
            ORIGINAL
        );
        #[cfg(target_os = "macos")]
        visual.simulate_keystrokes("cmd-z");
        #[cfg(not(target_os = "macos"))]
        visual.simulate_keystrokes("ctrl-z");
        visual.run_until_parked();
        input.read_with(visual, |i, _| assert_eq!(i.value().as_ref(), ORIGINAL));
        #[cfg(target_os = "macos")]
        visual.simulate_keystrokes("cmd-shift-z");
        #[cfg(not(target_os = "macos"))]
        visual.simulate_keystrokes("ctrl-y");
        visual.run_until_parked();
        input.read_with(visual, |i, _| {
            assert_eq!(
                i.value().as_ref(),
                ORIGINAL.replacen('\u{feff}', "\u{feff}local ", 1)
            )
        });
        std::fs::write(directory.path().join("vault/note.md"), "external").unwrap();
        reader.update_in(visual, |r, _, cx| {
            assert!(!r.save_source(cx));
            assert!(r.editing.as_ref().unwrap().conflict_detected);
        });
        let journal = std::fs::read_dir(directory.path().join("state/editor-drafts"))
            .unwrap()
            .flatten()
            .find(|e| e.path().extension().is_some_and(|x| x == "json"))
            .unwrap()
            .path();
        let draft: serde_json::Value =
            serde_json::from_slice(&std::fs::read(journal).unwrap()).unwrap();
        assert_eq!(
            draft["text"].as_str(),
            Some(ORIGINAL.replacen('\u{feff}', "\u{feff}local ", 1).as_str())
        );
        assert_eq!(
            std::fs::read_to_string(directory.path().join("vault/note.md")).unwrap(),
            "external"
        );
    }
    #[gpui::test]
    fn native_reveal_drag_graphemes_and_ime_bridge_keep_source_coordinates(
        cx: &mut TestAppContext,
    ) {
        let original = "# Title\n\n**Ж** e\u{301} 🧠\n\nend\n";
        let (reader, visual, _directory) = fixture(cx, original);
        reader.update_in(visual, |r, window, cx| r.toggle_live_preview(window, cx));
        visual.run_until_parked();
        let input = reader.read_with(visual, |r, _| r.editing.as_ref().unwrap().input.clone());
        let start = original.find('Ж').unwrap();
        let end = original.find('🧠').unwrap();
        let from = input.read_with(visual, |i, _| {
            i.range_to_bounds(&(start..start + 2)).unwrap().center()
        });
        visual.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        let to = input.read_with(visual, |i, _| {
            let bounds = i.range_to_bounds(&(end..end + 4)).unwrap();
            point(bounds.right(), bounds.center().y)
        });
        visual.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::default());
        visual.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        input.read_with(visual, |i, _| {
            let selection = i.selected_range();
            assert!(
                original.is_char_boundary(selection.start)
                    && original.is_char_boundary(selection.end)
            );
            assert!(
                original[selection].contains("e\u{301} 🧠"),
                "drag selects authored Unicode through reveal"
            );
            assert_eq!(i.value().as_ref(), original);
        });
        let combining = original.find("e\u{301}").unwrap();
        reader.update_in(visual, |_, _, cx| {
            input.update(cx, |i, cx| i.set_selected_range(combining..combining, cx))
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("right");
        input.read_with(visual, |i, _| {
            assert_eq!(i.cursor(), combining + "e\u{301}".len())
        });
        visual.simulate_keystrokes("left");
        input.read_with(visual, |i, _| assert_eq!(i.cursor(), combining));
        visual.simulate_keystrokes("home");
        input.read_with(visual, |i, _| {
            assert_eq!(i.cursor(), original.find("**Ж").unwrap())
        });
        visual.simulate_keystrokes("end");
        input.read_with(visual, |i, _| assert_eq!(i.cursor(), end + 4));
        reader.update_in(visual, |_, window, cx| {
            input.update(cx, |i, cx| {
                let eof = original.encode_utf16().count();
                i.replace_and_mark_text_in_range(Some(eof..eof), "中", Some(0..1), window, cx);
                assert_eq!(i.marked_text_range(window, cx), Some(eof..eof + 1));
                assert_eq!(i.value().as_ref(), format!("{original}中"));
                i.unmark_text(window, cx);
            })
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| r.toggle_live_preview(window, cx));
        #[cfg(target_os = "macos")]
        visual.simulate_keystrokes("cmd-z");
        #[cfg(not(target_os = "macos"))]
        visual.simulate_keystrokes("ctrl-z");
        visual.run_until_parked();
        input.read_with(visual, |i, _| assert_eq!(i.value().as_ref(), original));
    }
    fn legacy_equivalent(actual: &dyn SourceProjection, expected: &dyn SourceProjection) {
        use gpui_component::input::projection::{ConcealBias, ProjectedByte, SourceByte};
        assert_eq!(actual.source().stamp, expected.source().stamp);
        assert_eq!(actual.text(), expected.text());
        assert_eq!(actual.styles(), expected.styles());
        assert_eq!(actual.line_scales(), expected.line_scales());
        assert_eq!(actual.blocks(), expected.blocks());
        assert_eq!(
            format!("{:?}", actual.markers()),
            format!("{:?}", expected.markers())
        );
        for offset in 0..=actual.source().text.len() {
            for bias in [ConcealBias::Left, ConcealBias::Right] {
                assert_eq!(
                    actual.to_projected(SourceByte(offset), bias),
                    expected.to_projected(SourceByte(offset), bias)
                );
            }
        }
        for offset in 0..=actual.text().len() {
            for bias in [ConcealBias::Left, ConcealBias::Right] {
                assert_eq!(
                    actual.to_source(ProjectedByte(offset), bias),
                    expected.to_source(ProjectedByte(offset), bias)
                );
            }
        }
        for marker in actual.markers() {
            assert_eq!(
                actual.marker_scope_is_raw(&marker.scope),
                expected.marker_scope_is_raw(&marker.scope)
            );
        }
    }

    #[gpui::test]
    fn actual_latest_inputs_keep_legacy_metadata_and_one_navigation_producer(
        cx: &mut TestAppContext,
    ) {
        let original = "# Heading\r\n\r\n| a | b |\r\n| - | - |\r\n| c | d |\r\n\r\n**tail** e\u{301} 👩‍💻 [[target|alias]]\r\n";
        let (reader, visual, _dir) = fixture(cx, original);
        reader.update_in(visual, |r, window, cx| r.set_live_preview(true, window, cx));
        visual.run_until_parked();
        let held = reader.read_with(visual, |r, _| {
            r.editing
                .as_ref()
                .unwrap()
                .live_preview
                .accepted
                .clone()
                .unwrap()
        });
        let expected = CachedProvider::classify(held.source().clone());
        for insert in ["x", "y"] {
            reader.update_in(visual, |r, window, cx| {
                let input = r.editing.as_ref().unwrap().input.clone();
                input.update(cx, |i, cx| {
                    let at = i.value().find("** e").unwrap();
                    i.set_selected_range(at..at, cx);
                    i.replace(insert, window, cx);
                });
                r.schedule_live_preview(cx);
                let e = r.editing.as_ref().unwrap();
                assert!(Arc::ptr_eq(
                    e.live_preview.accepted.as_ref().unwrap(),
                    &held
                ));
                let snapshot = SourceSnapshot {
                    stamp: input.read(cx).source_stamp(),
                    text: Arc::from(input.read(cx).value().as_ref()),
                };
                let active = ActiveSource {
                    anchor: gpui_component::input::projection::SourceByte(snapshot.text.len()),
                    head: gpui_component::input::projection::SourceByte(snapshot.text.len()),
                    ..Default::default()
                };
                let actual = held.compose(&snapshot, &active).unwrap();
                let reference = expected.compose(&snapshot, &active).unwrap();
                legacy_equivalent(actual.as_ref(), reference.as_ref());
                assert!(
                    !actual.blocks().is_empty(),
                    "rendered table above dirty tail remains rendered"
                );
            });
        }
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            let e = r.editing.as_ref().unwrap();
            let source = e.input.read(cx).value();
            assert!(source.contains("tailxy"));
            assert!(r
                .checked_live_provider(e.live_preview.origin.reader, cx)
                .is_some());
            let reports = &e.live_preview.worker_reports;
            assert_eq!(
                reports.len(),
                3,
                "initial cold, old physical job, latest queued job"
            );
            assert_eq!(reports[0].counts["link_parse_bytes"], original.len() as u64);
            assert_eq!(
                reports[1].counts["link_parse_bytes"],
                (original.len() + 1) as u64
            );
            assert_eq!(reports[2].counts["link_parse_bytes"], source.len() as u64);
            assert!(reports.iter().all(|r| r.phases["parse"].calls > 0));
            assert!(!e.live_preview.in_flight.get() && e.live_preview.queued.is_none());
        });
    }

    #[gpui::test]
    fn actual_refusal_restore_and_binding_checks_do_not_adopt_old_work(cx: &mut TestAppContext) {
        let (reader, visual, _dir) = fixture(cx, ORIGINAL);
        reader.update_in(visual, |r, window, cx| {
            r.set_live_preview(true, window, cx);
            let old_key = r
                .editing
                .as_ref()
                .unwrap()
                .live_preview
                .running
                .as_ref()
                .unwrap()
                .key
                .clone();
            r.set_live_preview(false, window, cx); // Refusal supersedes before return.
            r.set_live_preview(true, window, cx); // Same bytes, distinct new attempt.
            let live = r.live_input(cx.entity_id(), cx).unwrap();
            let state = &r.editing.as_ref().unwrap().live_preview;
            assert!(!state.session.is_pending(&old_key, Some(&live)));
            assert!(state.queued.is_some());
        });
        visual.run_until_parked();
        reader.update_in(visual, |r, _, cx| {
            let root = r.vault_root.clone();
            let path = r.current_rel.clone();
            assert!(r.checked_live_provider(cx.entity_id(), cx).is_some());
            r.vault_root = root.join("different-owner-root");
            r.schedule_live_preview(cx);
            assert!(r.checked_live_provider(cx.entity_id(), cx).is_none());
            r.vault_root = root;
            r.current_rel = "foreign.md".into();
            r.schedule_live_preview(cx);
            assert!(r.checked_live_provider(cx.entity_id(), cx).is_none());
            r.current_rel = path;
            // An exact held legacy snapshot is reusable, not fresh Nav admission.
            assert!(r.checked_live_provider(cx.entity_id(), cx).is_some());
            let live = r.live_input(cx.entity_id(), cx).unwrap();
            assert!(r
                .editing
                .as_mut()
                .unwrap()
                .live_preview
                .session
                .current(Some(&live))
                .is_none());
        });
    }

    #[gpui::test]
    async fn real_park_resume_failed_move_and_deferred_save_echo_keep_delivery_owned(
        cx: &mut TestAppContext,
    ) {
        let original =
            "# Heading\n\n| a | b |\n| - | - |\n| c | d |\n\n[target](target.md)\n\n**tail**\n";
        let (reader, visual, dir) = fixture(cx, original);
        std::fs::create_dir(dir.path().join("vault/New")).unwrap();
        reader.update_in(visual, |r, window, cx| r.set_live_preview(true, window, cx));
        visual.run_until_parked();
        let baseline = reader.read_with(visual, |r, _| {
            r.editing
                .as_ref()
                .unwrap()
                .live_preview
                .accepted
                .clone()
                .unwrap()
        });
        let preview = okilum_core::link_rewrite::Preview::prepare(
            &dir.path().join("vault"),
            "target.md",
            "New/target.md",
        )
        .unwrap();
        assert!(
            preview.editor_paths().contains(&"note.md".to_owned()),
            "explicit Markdown destination actually changes, so the real move must park this note"
        );
        let old = reader.update_in(visual, |r, window, cx| {
            let input = r.editing.as_ref().unwrap().input.clone();
            // A real reset changes generation even though the final bytes return.
            input.update(cx, |i, cx| i.set_value(format!("{original} "), window, cx));
            input.update(cx, |i, cx| i.set_value(original, window, cx));
            r.schedule_live_preview(cx);
            let old = r
                .editing
                .as_ref()
                .unwrap()
                .live_preview
                .pending_marker()
                .unwrap();
            assert!(
                old.get(),
                "actual classification task pending before real park"
            );
            old
        });
        let task = reader.update_in(visual, |r, window, cx| {
            assert!(r.save_source(cx));
            assert!(!r.source_is_dirty(cx));
            let input = r.editing.as_ref().unwrap().input.clone();
            let task = r.apply_link_move(&preview, window, cx).unwrap();
            assert!(
                r.move_applying && r.editing.is_none(),
                "actual Editing::park path"
            );
            assert!(!input.read(cx).is_editable());
            task
        });
        assert!(task.await.unwrap().moved);
        assert!(
            !old.get(),
            "canceled pre-poll job settles its OWN marker; no parse is claimed for it"
        );
        reader.update_in(visual, |r, window, cx| {
            r.finish_move_editor(cx);
            r.sync_move_input(window, cx); // The real complete_move synchronization hook.
            let e = r.editing.as_ref().unwrap();
            assert!(
                e.live_preview.in_flight.get(),
                "explicit resume starts latest work"
            );
            assert!(Arc::ptr_eq(
                e.live_preview.accepted.as_ref().unwrap(),
                &baseline
            ));
            let text = e.input.read(cx).value();
            assert!(text.contains("New/target"));
            let expected = CachedProvider::classify(baseline.source().clone());
            let snapshot = SourceSnapshot {
                stamp: e.input.read(cx).source_stamp(),
                text: Arc::from(text.as_ref()),
            };
            let active = ActiveSource::default();
            legacy_equivalent(
                baseline.compose(&snapshot, &active).unwrap().as_ref(),
                expected.compose(&snapshot, &active).unwrap().as_ref(),
            );
            let input = e.input.clone();
            input.update(cx, |i, _| i.enable_layout_diagnostics());
            let facts = |cx: &App| {
                let i = input.read(cx);
                (i.source_stamp(), i.presentation_epoch(), i.selected_range(), i.scroll_offset(),
                    i.value(), i.layout_diagnostics().unwrap().provider_applies)
            };
            let before_echo = facts(cx);
            let flight = r.editing.as_ref().unwrap().live_preview.running.clone().unwrap();
            let file = dir.path().join("vault/note.md");
            std::fs::write(&file, std::fs::read(&file).unwrap()).unwrap();
            r.apply_vault_changes(okilum_core::Changes { changed: ["note.md".into()].into(), ..Default::default() }, window, cx);
            assert_eq!(facts(cx), before_echo, "same-byte echo while resumed work is pending does not reinstall/reset native presentation");
            assert!(Rc::ptr_eq(&flight, r.editing.as_ref().unwrap().live_preview.running.as_ref().unwrap()),
                "echo cannot launch a second classification worker");
            // A real autosave starts while resumed classification is pending.
            input.update(cx, |i, cx| i.replace("x", window, cx));
        });
        reader.update_in(visual, |r, window, cx| {
            let e = r.editing.as_ref().unwrap();
            let id = e.input.entity_id();
            let epoch = e.recovery_epoch;
            r.autosave(id, epoch, window, cx);
            assert!(r.editing.as_ref().unwrap().autosaving.is_some());
            r.apply_vault_changes(
                okilum_core::Changes {
                    changed: ["note.md".into()].into(),
                    ..Default::default()
                },
                window,
                cx,
            );
            assert!(
                r.editing.as_ref().unwrap().refresh_deferred,
                "actual callback defers behind actual save worker"
            );
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            let e = r.editing.as_ref().unwrap();
            assert!(!e.refresh_deferred && !e.conflict_detected);
            assert!(r
                .checked_live_provider(e.live_preview.origin.reader, cx)
                .is_some());
            assert!(!e.live_preview.in_flight.get());
            assert!(
                e.live_preview.worker_reports.len() >= 2,
                "actual baseline/resumed cold worker controls"
            );
        });
        // A destination race fails a REAL move, then the real resume hook recovers.
        let failed = okilum_core::link_rewrite::Preview::prepare(
            &dir.path().join("vault"),
            "New/target.md",
            "target.md",
        )
        .unwrap();
        std::fs::write(dir.path().join("vault/target.md"), "destination raced").unwrap();
        let task = reader.update_in(visual, |r, window, cx| {
            r.apply_link_move(&failed, window, cx).unwrap()
        });
        let outcome = task.await;
        assert!(!matches!(outcome, Ok(ref a) if a.moved));
        reader.update_in(visual, |r, window, cx| {
            r.finish_move_editor(cx);
            r.sync_move_input(window, cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            assert!(r
                .checked_live_provider(r.editing.as_ref().unwrap().live_preview.origin.reader, cx)
                .is_some());
            assert!(!r.editing.as_ref().unwrap().live_preview.in_flight.get());
        });
    }

    #[gpui::test]
    fn real_close_and_different_root_reopen_retire_only_the_old_flight(cx: &mut TestAppContext) {
        let (reader, visual, _dir) = fixture(cx, ORIGINAL);
        let old = reader.update_in(visual, |r, window, cx| {
            r.set_live_preview(true, window, cx);
            r.editing
                .as_ref()
                .unwrap()
                .live_preview
                .pending_marker()
                .unwrap()
        });
        assert!(old.get());
        visual.update(|window, _| window.remove_window());
        drop(reader);
        let (reader, visual, _other_dir) = fixture(cx, ORIGINAL);
        let new = reader.update_in(visual, |r, window, cx| {
            r.set_live_preview(true, window, cx);
            r.editing
                .as_ref()
                .unwrap()
                .live_preview
                .pending_marker()
                .unwrap()
        });
        assert!(!Rc::ptr_eq(&old, &new));
        visual.run_until_parked();
        assert!(!old.get() && !new.get());
        reader.read_with(visual, |r, cx| {
            assert!(r
                .checked_live_provider(r.editing.as_ref().unwrap().live_preview.origin.reader, cx)
                .is_some());
        });
    }

    #[gpui::test]
    fn real_own_path_rename_recreates_the_reader_editing_owner(cx: &mut TestAppContext) {
        let (reader, visual, dir) = fixture(cx, ORIGINAL);
        reader.update_in(visual, |r, window, cx| r.set_live_preview(true, window, cx));
        visual.run_until_parked();
        // Persist the LP preference through the REAL exit/reopen path first.
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| r.open_live_preview(window, cx));
        visual.run_until_parked();
        let owner = reader.read_with(visual, |r, _| {
            r.editing.as_ref().unwrap().live_preview.owner.clone()
        });
        reader.update_in(visual, |r, window, cx| {
            r.rename_note_title(window, cx);
            r.renaming
                .as_ref()
                .unwrap()
                .input
                .update(cx, |i, cx| i.set_value("renamed.md", window, cx));
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        reader.read_with(visual, |r, cx| {
            assert_eq!(r.current_rel, "renamed.md");
            assert!(!dir.path().join("vault/note.md").exists());
            let e = r.editing.as_ref().unwrap();
            assert!(
                e.live_preview.owner != owner,
                "actual rename/complete_move retires old Editing"
            );
            assert!(r
                .checked_live_provider(e.live_preview.origin.reader, cx)
                .is_some());
        });
    }
    #[gpui::test]
    fn real_find_refusal_before_first_poll_requires_new_delivery_on_restore(
        cx: &mut TestAppContext,
    ) {
        let (reader, visual, _dir) = fixture(cx, ORIGINAL);
        let (old, latest) = reader.update_in(visual, |r, window, cx| {
            r.set_live_preview(true, window, cx);
            let flight = r
                .editing
                .as_ref()
                .unwrap()
                .live_preview
                .running
                .clone()
                .unwrap();
            r.open_source_find(cx); // Real direct enabled=false/source-clear path.
            let live = r.live_input(cx.entity_id(), cx).unwrap();
            assert!(!r
                .editing
                .as_ref()
                .unwrap()
                .live_preview
                .session
                .is_pending(&flight.key, Some(&live)));
            assert!(
                r.editing
                    .as_ref()
                    .unwrap()
                    .input
                    .read(cx)
                    .search_session()
                    .open
            );
            r.toggle_live_preview(window, cx); // Actual restore path closes native Find.
            let latest = r
                .editing
                .as_ref()
                .unwrap()
                .live_preview
                .running
                .clone()
                .unwrap();
            assert!(!Rc::ptr_eq(&flight, &latest));
            assert!(r
                .editing
                .as_ref()
                .unwrap()
                .live_preview
                .session
                .is_pending(&latest.key, Some(&live)));
            (flight.busy.clone(), latest.busy.clone())
        });
        visual.run_until_parked();
        assert!(!old.get() && !latest.get());
        reader.read_with(visual, |r, cx| {
            let e = r.editing.as_ref().unwrap();
            assert_eq!(
                e.live_preview.completed_navigation_bytes(),
                vec![ORIGINAL.len() as u64],
                "the canceled pre-poll request is not a completed immutable parse"
            );
            assert!(r
                .checked_live_provider(e.live_preview.origin.reader, cx)
                .is_some());
        });
    }
}
