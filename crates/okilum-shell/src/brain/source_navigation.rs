//! One ephemeral return to the previous successfully opened managed note link.
use super::*;
use gpui_component::input::projection::SourceStamp;
use source_input::LinkSelection;
mod context_passage;

#[derive(Clone, PartialEq)]
struct Owner {
    endpoint: SocketAddr,
    workspace: Value,
    goal: String,
    entity: EntityId,
}
#[derive(Clone)]
struct Visit {
    owner: Owner,
    path: String,
    revision: String,
    live: bool,
    selection: LinkSelection,
    scroll: Point<Pixels>,
}
#[derive(Clone)]
struct Previous {
    visit: Visit,
    destination: String,
}
#[derive(Clone)]
enum ReturnEntry {
    Source(Previous),
    Context(context_passage::ContextReturn),
}
impl ReturnEntry {
    fn destination(&self) -> &str {
        match self {
            Self::Source(p) => &p.destination,
            Self::Context(p) => &p.destination,
        }
    }
}
#[derive(Clone)]
struct Passage {
    revision: String,
    start_line: usize,
    end_line: usize,
}
#[derive(Clone)]
struct Flight {
    generation: u64,
    owner: Owner,
    collection: Collection,
    source: Value,
    stamp: SourceStamp,
    request_generation: u64,
    origin: Visit,
    target: String,
    heading: Option<String>,
    passage: Option<Passage>,
    landing: Option<usize>,
    back: Option<Previous>,
}
#[derive(Clone)]
struct Restore {
    owner: Owner,
    path: String,
    revision: String,
    stamp: SourceStamp,
    initial_selection: LinkSelection,
    initial_scroll: Point<Pixels>,
    visit: Visit,
    heading: bool,
}
#[derive(Default)]
pub(super) struct SourceNavigation {
    previous: Option<ReturnEntry>,
    context_flight: Option<context_passage::ContextFlight>,
    flight: Option<Flight>,
    restore: Option<Restore>,
    generation: u64,
    heading_position: Option<(Restore, u64)>,
}

fn validated_source(data: &Value, path: &str, workspace: &Value) -> Result<String, String> {
    if data["path"] != path || data["brain_id"] != workspace["brain_id"] {
        return Err("The note response does not match its requested path or workspace.".into());
    }
    let raw = decode_source(data).map_err(str::to_owned)?;
    if data["revision"] != okilum_core::decision_reuse::revision(raw.as_bytes()) {
        return Err("The note revision does not match its saved bytes.".into());
    }
    Ok(raw)
}

fn passage_offset(raw: &str, data: &Value, passage: &Passage) -> Result<usize, String> {
    if data["revision"] != passage.revision {
        return Err(
            "This reference changed. Refresh incoming references before opening it.".into(),
        );
    }
    if passage.start_line == 0 || passage.end_line < passage.start_line {
        return Err("The reference has invalid source lines.".into());
    }
    let mut offset = 0;
    let mut start = None;
    for (i, line) in raw.split_inclusive('\n').enumerate() {
        if i + 1 == passage.start_line {
            start = Some(offset);
        }
        if i + 1 == passage.end_line {
            return start.ok_or_else(|| "Invalid source passage.".into());
        }
        offset += line.len();
    }
    Err("The reference source lines are no longer available. Refresh incoming references.".into())
}

impl BrainView {
    fn navigation_owner(&self) -> Option<Owner> {
        Some(Owner {
            endpoint: self.endpoint,
            workspace: self.expected_workspace.clone()?,
            goal: self.goal_id(),
            entity: self.source.managed()?.entity_id(),
        })
    }
    fn navigation_visible(&self) -> bool {
        self.surface == Surface::Source
            && !self.show_capture
            && !self.discussion_note.active()
            && !self.goal_criteria.active
            && !self.discussion_decision.active
            && !self.decision_reuse.active
            && !matches!(self.collection, Collection::Inbox | Collection::Attention)
    }
    pub(super) fn source_back_path(&self) -> Option<&str> {
        let ReturnEntry::Source(previous) = self.source_navigation.previous.as_ref()? else {
            return None;
        };
        (self.navigation_visible()
            && self.navigation_owner().as_ref() == Some(&previous.visit.owner)
            && self.source_snapshot.as_ref()?["path"] == previous.destination)
            .then_some(previous.visit.path.as_str())
    }
    pub(super) fn source_navigation_pending(&self) -> bool {
        self.source_navigation.flight.is_some() || self.source_navigation.context_flight.is_some()
    }
    /// Cancel in-flight reads without consuming the previously committed visit.
    pub(super) fn cancel_source_navigation(&mut self) {
        self.source_navigation.generation = self.source_navigation.generation.wrapping_add(1);
        self.source_navigation.flight = None;
        self.source_navigation.context_flight = None;
        self.source_navigation.restore = None;
        self.source_navigation.heading_position = None;
    }
    pub(super) fn sync_source_navigation(&mut self, cx: &App) {
        let owner = self.navigation_owner();
        if self
            .source_navigation
            .previous
            .as_ref()
            .is_some_and(|p| match p {
                ReturnEntry::Source(p) => Some(&p.visit.owner) != owner.as_ref(),
                ReturnEntry::Context(p) => !self.context_origin_retained(&p.origin, cx),
            })
        {
            self.source_navigation.previous = None;
        }
        self.sync_context_passage(cx);
        if self
            .source_navigation
            .flight
            .as_ref()
            .is_some_and(|f| !self.navigation_flight_matches(f, cx))
        {
            self.cancel_source_navigation();
        }
    }
    fn navigation_flight_matches(&self, flight: &Flight, cx: &App) -> bool {
        self.source_navigation.generation == flight.generation
            && self.request_generation == flight.request_generation
            && self.navigation_owner().as_ref() == Some(&flight.owner)
            && self.navigation_visible()
            && self.collection == flight.collection
            && self.source_snapshot.as_ref() == Some(&flight.source)
            && self.source.stamp(cx) == Some(flight.stamp)
            && !self.busy
            && !self.source_loading
            && !self.dirty(cx)
            && self.editor_can_begin_criteria()
            && self.source_conflict.is_none()
            && self.pending_navigation.is_none()
    }
    #[cfg(test)]
    pub(super) fn open_source_link(
        &mut self,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_source_link_at(path, None, window, cx);
    }
    pub(super) fn open_source_link_at(
        &mut self,
        path: String,
        heading: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Preserve the existing legacy preview behavior outside the managed editor.
        if self.source.managed().is_none() {
            if heading.is_some() {
                self.error = Some("Heading navigation requires the managed Source editor.".into());
                cx.notify();
                return;
            }
            self.open_source(path, window, cx);
            return;
        }
        self.begin_source_navigation(path, heading, None, false, window, cx);
    }
    pub(super) fn back_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.source_back_path().map(str::to_owned) else {
            return;
        };
        self.begin_source_navigation(path, None, None, true, window, cx);
    }
    pub(super) fn open_source_passage(
        &mut self,
        path: String,
        revision: String,
        start_line: usize,
        end_line: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_source_navigation(
            path,
            None,
            Some(Passage {
                revision,
                start_line,
                end_line,
            }),
            false,
            window,
            cx,
        );
    }
    fn begin_source_navigation(
        &mut self,
        path: String,
        heading: Option<String>,
        passage: Option<Passage>,
        back: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let intent = if back {
            PendingNavigation::SourceBack
        } else {
            PendingNavigation::SourceLink(path.clone(), heading.clone())
        };
        // A newer visible link choice owns navigation, even if its departure
        // is subsequently blocked or it points back to the current note.
        self.cancel_source_navigation();
        if self.note_guard_navigation(intent.clone(), cx) || self.busy || self.source_loading {
            return;
        }
        if self.dirty(cx) {
            self.pending_navigation = Some(intent);
            self.surface = Surface::Source;
            self.show_capture = false;
            self.error =
                Some("Save or discard the current changes before opening another note.".into());
            cx.notify();
            return;
        }
        if !self.navigation_visible()
            || !self.editor_can_begin_criteria()
            || self.source_conflict.is_some()
            || self.pending_navigation.is_some()
        {
            return;
        }
        let (Some(owner), Some(source), Some(stamp)) = (
            self.navigation_owner(),
            self.source_snapshot.clone(),
            self.source.stamp(cx),
        ) else {
            return;
        };
        if source["path"] == path && heading.is_none() && !back {
            return;
        }
        let Some(selection) = self.source.link_selection(window, cx) else {
            self.notice = "Finish text composition before opening another note.".into();
            cx.notify();
            return;
        };
        let origin = Visit {
            owner: owner.clone(),
            path: text(&source["path"]),
            revision: text(&source["revision"]),
            live: self.source_projection.live,
            selection,
            scroll: self.source.managed().unwrap().read(cx).scroll_offset(),
        };
        self.cancel_source_navigation();
        let flight = Flight {
            generation: self.source_navigation.generation,
            owner: owner.clone(),
            collection: self.collection,
            source,
            stamp,
            request_generation: self.request_generation,
            origin,
            target: path.clone(),
            heading: heading.clone(),
            passage: passage.clone(),
            landing: None,
            back: if back {
                match &self.source_navigation.previous {
                    Some(ReturnEntry::Source(p)) => Some(p.clone()),
                    _ => None,
                }
            } else {
                None
            },
        };
        self.source_navigation.flight = Some(flight.clone());
        self.error = None;
        self.notice = if back {
            "Opening previous note…"
        } else {
            "Opening linked note…"
        }
        .into();
        // This read owns no global busy/loading state: Find and the current input
        // stay intact on failure. Any intervening user action invalidates the owner.
        cx.spawn_in(window, async move |this, cx| {
            let reply = cx
                .background_executor()
                .spawn(async move {
                    let data = rpc_guarded(
                        owner.endpoint,
                        json!({"op":"source_read","path":path}),
                        Some(&owner.workspace),
                    )?;
                    let raw = validated_source(&data, &path, &owner.workspace)?;
                    let landing = if let Some(passage) = passage.as_ref() {
                        Some(passage_offset(&raw, &data, passage)?)
                    } else {
                        heading
                            .as_ref()
                            .map(|heading| {
                                let snapshot = okilum_core::source_projection::Snapshot::new(
                                    path.as_str(),
                                    0,
                                    raw.as_str(),
                                );
                                okilum_core::source_classifier::classify(&snapshot)
                                    .heading_offset(heading)
                                    .map_err(str::to_owned)
                            })
                            .transpose()?
                    };
                    Ok::<_, String>((data, landing))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let mut flight = flight;
                let reply = reply.map(|(data, landing)| {
                    flight.landing = landing;
                    data
                });
                this.finish_source_navigation(flight, reply, window, cx)
            });
        })
        .detach();
        cx.notify();
    }
    fn finish_source_navigation(
        &mut self,
        flight: Flight,
        reply: Result<Value, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.navigation_flight_matches(&flight, cx) {
            if self
                .source_navigation
                .flight
                .as_ref()
                .is_some_and(|active| active.generation == flight.generation)
            {
                self.cancel_source_navigation();
                cx.notify();
            }
            return;
        }
        if (flight.heading.is_some() || flight.passage.is_some())
            && (self.source.link_selection(window, cx).as_ref() != Some(&flight.origin.selection)
                || self.source.managed().unwrap().read(cx).scroll_offset() != flight.origin.scroll
                || self.source_projection.live != flight.origin.live)
        {
            self.cancel_source_navigation();
            return;
        }
        self.source_navigation.flight = None;
        let data = match reply.and_then(|data| {
            let raw = validated_source(&data, &flight.target, &flight.owner.workspace)?;
            if let Some(passage) = &flight.passage {
                if Some(passage_offset(&raw, &data, passage)?) != flight.landing {
                    return Err("The source passage no longer matches its saved position.".into());
                }
            }
            Ok(data)
        }) {
            Ok(data) => data,
            Err(error) => {
                if flight.passage.is_some() {
                    self.incoming_passage_stale();
                }
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        if (flight.heading.is_some() || flight.passage.is_some()) && flight.landing.is_none() {
            self.error = Some("The linked heading has no validated source position.".into());
            cx.notify();
            return;
        }
        // Validation precedes every destructive detach/reset. The stable accepted
        // multiline engine retains exact source bytes; load_source keeps its alarm.
        let previous = flight.back;
        self.cancel_source_selection_context(cx);
        self.load_source(data.clone(), window, cx);
        if !self.source_editable {
            return;
        }
        if let Some(previous) = previous {
            self.source_navigation.previous = None;
            if data["revision"] == previous.visit.revision {
                self.source_projection.live = previous.visit.live;
                self.apply_source_projection(cx);
                let selection = self.source.link_selection(window, cx).unwrap();
                self.source_navigation.restore = Some(Restore {
                    owner: flight.owner,
                    path: flight.target,
                    revision: text(&data["revision"]),
                    stamp: self.source.stamp(cx).unwrap(),
                    initial_selection: selection,
                    initial_scroll: self.source.managed().unwrap().read(cx).scroll_offset(),
                    visit: previous.visit,
                    heading: false,
                });
                self.restore_source_navigation(window, cx);
                self.notice = "Returned to previous note".into();
            } else {
                self.source_projection.live = false;
                self.apply_source_projection(cx);
                self.notice =
                    "The previous note changed. Opened its current saved version at the start."
                        .into();
            }
        } else {
            if let Some(offset) = flight.landing {
                self.source_projection.live = flight.origin.live;
                self.apply_source_projection(cx);
                let raw = self.source.value(cx);
                let utf16 = raw[..offset].encode_utf16().count();
                self.source_navigation.restore = Some(Restore {
                    owner: flight.owner.clone(),
                    path: flight.target.clone(),
                    revision: text(&data["revision"]),
                    stamp: self.source.stamp(cx).unwrap(),
                    initial_selection: self.source.link_selection(window, cx).unwrap(),
                    initial_scroll: self.source.managed().unwrap().read(cx).scroll_offset(),
                    visit: Visit {
                        owner: flight.owner,
                        path: flight.target.clone(),
                        revision: text(&data["revision"]),
                        live: flight.origin.live,
                        selection: LinkSelection {
                            bytes: offset..offset,
                            utf16: utf16..utf16,
                            reversed: false,
                            text: String::new(),
                        },
                        scroll: point(px(0.), px(0.)),
                    },
                    heading: true,
                });
                self.restore_source_navigation(window, cx);
            }
            self.source_navigation.previous = Some(ReturnEntry::Source(Previous {
                visit: flight.origin,
                destination: flight.target,
            }));
            self.notice = if flight.passage.is_some() {
                "Opened source passage"
            } else if flight.heading.is_some() {
                "Opened linked heading"
            } else {
                "Opened linked note"
            }
            .into();
        }
        cx.notify();
    }
    /// Called after load and the existing matching projection completion. Public
    /// set_scroll_offset already defers/clamps to the next layout; no timer is needed.
    pub(super) fn restore_source_navigation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(restore) = self.source_navigation.restore.as_ref() else {
            return;
        };
        if self.navigation_owner().as_ref() != Some(&restore.owner)
            || !self.navigation_visible()
            || self.source_projection.live != restore.visit.live
            || self.source_find.active()
            || self.source.stamp(cx) != Some(restore.stamp)
            || self
                .source_snapshot
                .as_ref()
                .is_none_or(|s| s["path"] != restore.path || s["revision"] != restore.revision)
            || self.source.link_selection(window, cx).as_ref() != Some(&restore.initial_selection)
            || self.source.managed().unwrap().read(cx).scroll_offset() != restore.initial_scroll
        {
            self.source_navigation.restore = None;
            return;
        }
        if self.source_projection.live && !self.source_projection_ready() {
            return;
        }
        let restore = self.source_navigation.restore.take().unwrap();
        let value = self.source.value(cx);
        let range = restore.visit.selection.bytes.clone();
        if value.get(range.clone()).is_none() {
            return;
        }
        let range = if restore.visit.selection.reversed {
            range.end..range.start
        } else {
            range
        };
        self.source.managed().unwrap().update(cx, |state, cx| {
            state.set_selected_range(range, cx);
            if !restore.heading {
                state.set_scroll_offset(restore.visit.scroll, cx);
            }
        });
        if restore.heading {
            self.position_opened_heading(restore, window, cx);
        }
    }
    fn position_opened_heading(
        &mut self,
        restore: Restore,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.source_navigation.heading_position =
            Some((restore, self.source_navigation.generation));
        cx.notify();
    }
    pub(super) fn cancel_heading_position(&mut self) {
        self.source_navigation.heading_position = None;
    }
    pub(super) fn heading_keyboard_cancel_subscription(cx: &mut Context<Self>) -> Subscription {
        let view = cx.entity().downgrade();
        // Bound actions dispatch before ordinary key capture listeners. Intercept
        // without stopping propagation so every actual editor key can cancel.
        cx.intercept_keystrokes(move |_, window, cx| {
            let _ = view.update(cx, |this, cx| {
                if this
                    .source
                    .managed()
                    .is_some_and(|state| state.read(cx).focus_handle(cx).is_focused(window))
                {
                    this.cancel_heading_position();
                }
            });
        })
    }
    pub(super) fn source_heading_paint_hook(&self, cx: &Context<Self>) -> AnyElement {
        let view = cx.entity().downgrade();
        gpui::canvas(
            |_, _, _| (),
            move |bounds, _, window, cx| {
                // Capture real wheel input before the editor stops its bubble phase.
                // Keep this installed before a target load, not just while pending.
                let wheel_view = view.clone();
                window.on_mouse_event(move |event: &gpui::ScrollWheelEvent, phase, _, cx| {
                    if phase == gpui::DispatchPhase::Capture && bounds.contains(&event.position) {
                        let _ = wheel_view.update(cx, |this, _| this.cancel_heading_position());
                    }
                });
                // A defer originating in paint runs after the complete editor paint
                // commits layout and ordinary caret reveal. Frame callbacks run before draw.
                window.defer(cx, move |_, cx| {
                    let _ = view.update(cx, |this, cx| this.position_heading_after_paint(cx));
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .into_any_element()
    }
    fn position_heading_after_paint(&mut self, cx: &mut Context<Self>) {
        let Some((restore, generation)) = self.source_navigation.heading_position.take() else {
            return;
        };
        if !self.heading_position_matches(&restore, generation, cx) {
            return;
        }
        self.source.managed().unwrap().update(cx, |state, cx| {
            let scroll = state.scroll_offset();
            let Some((mut caret, line_height)) = state.cursor_layout() else {
                return;
            };
            // cursor_layout Y is content geometry; painting adds vertical scroll.
            caret.origin.y += scroll.y;
            let bounds = state.input_bounds();
            if bounds.size.height <= line_height {
                return;
            }
            let delta = caret.origin.y - bounds.origin.y - line_height * 2.;
            // A Context departure can paint Source for the first time, before
            // ordinary reveal has a viewport. Completed paint still gives us
            // exact content geometry for an offscreen passage.
            if delta.abs() > line_height {
                // Public setter clamps near EOF; do not manufacture padding.
                state.set_scroll_offset(point(scroll.x, scroll.y - delta), cx);
            }
        });
    }
    fn heading_position_matches(&self, restore: &Restore, generation: u64, cx: &App) -> bool {
        self.source_navigation.generation == generation
            && self.navigation_owner().as_ref() == Some(&restore.owner)
            && self.navigation_visible()
            && self.source_projection.live == restore.visit.live
            && !self.source_find.active()
            && self.source.stamp(cx) == Some(restore.stamp)
            && self
                .source_snapshot
                .as_ref()
                .is_some_and(|s| s["path"] == restore.path && s["revision"] == restore.revision)
            && self.source.managed().is_some_and(|state| {
                state.read(cx).selected_range() == restore.visit.selection.bytes
            })
    }
    pub(super) fn source_navigation_loaded(&mut self, path: &str) {
        // Same-path refresh/discard never consumes the return entry.
        if self
            .source_navigation
            .previous
            .as_ref()
            .is_some_and(|p| p.destination() != path)
        {
            self.source_navigation.previous = None;
        }
        self.cancel_source_navigation();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn source(workspace: &Value, path: &str, raw: &str) -> Value {
        json!({"schema":SCHEMA,"brain_id":workspace["brain_id"],"path":path,
            "revision":okilum_core::decision_reuse::revision(raw.as_bytes()),
            "content_base64":STANDARD.encode(raw),"media_type":"text/markdown"})
    }
    fn setup(
        cx: &mut TestAppContext,
    ) -> (
        Entity<BrainView>,
        &mut VisualTestContext,
        std::path::PathBuf,
        Value,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let directory = std::env::temp_dir().join(format!("back283-{}", uuid()));
        let workspace = json!({"brain_id":uuid(),"root":"/isolated/back283","records_dir":"records","managed":true});
        let store = editor_recovery::EditorRecovery::at(directory.clone(), &workspace).unwrap();
        let (view, visual) = cx.add_window_view(|window, cx| {
            BrainView::new_managed_test(&workspace, &store, window, cx)
        });
        visual.run_until_parked();
        let raw = format!(
            "\u{feff}# Original\r\n{}**😀План**\r\n{}",
            "Earlier line\r\n".repeat(45),
            "Long ordinary source line\r\n".repeat(1655)
        );
        let a = source(&workspace, "notes/a.md", &raw);
        view.update_in(visual, |v, window, cx| {
            v.busy = false;
            v.surface = Surface::Source;
            v.collection = Collection::Sources;
            v.snapshot = json!({"goal":{"id":uuid()}});
            v.capabilities["source_write"] = json!(true);
            v.load_source(a.clone(), window, cx);
            v.source.focus(window, cx);
        });
        visual.run_until_parked();
        (view, visual, directory, a)
    }
    fn paint_source(view: &Entity<BrainView>, visual: &mut VisualTestContext) {
        visual.draw(point(px(0.), px(0.)), size(px(1200.), px(800.)), |_, _| {
            view.clone().into_any_element()
        });
    }
    fn link(v: &mut BrainView, next: &Value, window: &mut Window, cx: &mut Context<BrainView>) {
        v.open_source_link(text(&next["path"]), window, cx);
        let flight = v
            .source_navigation
            .flight
            .clone()
            .expect("positive control: link read started");
        v.finish_source_navigation(flight, Ok(next.clone()), window, cx);
        assert_eq!(v.source_snapshot.as_ref(), Some(next));
    }
    fn back(v: &mut BrainView, next: &Value, window: &mut Window, cx: &mut Context<BrainView>) {
        v.back_source(window, cx);
        let flight = v
            .source_navigation
            .flight
            .clone()
            .expect("positive control: Back read started");
        v.finish_source_navigation(flight, Ok(next.clone()), window, cx);
    }

    #[test]
    fn source_passage_uses_exact_crlf_bytes_and_refuses_stale_or_invalid_lines() {
        let raw = "\u{feff}# 😀\r\nActual [[target]] passage\r\nFollowing\n";
        let data = json!({"revision":"saved"});
        let mut passage = Passage {
            revision: "saved".into(),
            start_line: 2,
            end_line: 2,
        };
        assert_eq!(
            passage_offset(raw, &data, &passage).unwrap(),
            raw.find("Actual").unwrap()
        );
        passage.revision = "stale".into();
        assert!(passage_offset(raw, &data, &passage).is_err());
        passage.revision = "saved".into();
        passage.start_line = 0;
        assert!(passage_offset(raw, &data, &passage).is_err());
        passage.start_line = 2;
        passage.end_line = 4;
        assert!(passage_offset(raw, &data, &passage).is_err());
    }
    #[gpui::test]
    fn source_back_restores_actual_editor_and_one_successful_link_visit(cx: &mut TestAppContext) {
        let (view, visual, directory, a) = setup(cx);
        for live in [false, true] {
            view.update_in(visual, |v, window, cx| {
                v.load_source(a.clone(), window, cx);
                v.source_projection.live = live;
                v.schedule_source_projection(window, cx);
                v.source.managed().unwrap().update(cx, |s, cx| {
                    let start = s.value().find("😀План").unwrap();
                    s.set_selected_range((start + "😀План".len())..start, cx);
                    s.set_scroll_offset(point(px(0.), px(-1000.)), cx);
                });
            });
            visual.run_until_parked();
            paint_source(&view, visual);
            let (selection, scroll, entity) = view.update_in(visual, |v, window, cx| {
                assert!(
                    v.source_projection_ready(),
                    "matching classification settled"
                );
                let selection = v.source.link_selection(window, cx).unwrap();
                assert!(selection.reversed);
                let scroll = v.source.managed().unwrap().read(cx).scroll_offset();
                assert!(
                    scroll.y < px(-100.),
                    "positive control: actual editor scrolled"
                );
                let b = source(
                    v.expected_workspace.as_ref().unwrap(),
                    "notes/b.md",
                    "# Linked\nReference\n",
                );
                // The same explicit link entry point is called by resolved and chosen-ambiguous preview links.
                link(v, &b, window, cx);
                assert_eq!(v.source_back_path(), Some("notes/a.md"));
                back(v, &a, window, cx);
                (selection, scroll, v.source.entity_id())
            });
            visual.run_until_parked();
            paint_source(&view, visual);
            view.update_in(visual, |v, window, cx| {
                assert_eq!(v.source_snapshot.as_ref(), Some(&a));
                assert_eq!(v.source.link_selection(window, cx), Some(selection));
                assert_eq!(v.source.managed().unwrap().read(cx).scroll_offset(), scroll);
                assert_eq!(
                    v.source.entity_id(),
                    entity,
                    "stable subscriptions and decorations"
                );
                assert_eq!(v.source_projection.live, live);
                assert!(
                    v.source_navigation.previous.is_none(),
                    "Back does not create Forward"
                );
            });
        }
        view.update_in(visual, |v, window, cx| {
            let workspace = v.expected_workspace.clone().unwrap();
            let b = source(&workspace, "notes/b.md", "B\r\n");
            let c = source(&workspace, "notes/c.md", "C\r\n");
            link(v, &b, window, cx);
            v.open_source_link("notes/b.md".into(), window, cx);
            assert!(!v.source_navigation_pending(), "same path is not a visit");
            assert_eq!(v.source_back_path(), Some("notes/a.md"));
            link(v, &c, window, cx);
            assert_eq!(v.source_back_path(), Some("notes/b.md"));
            back(v, &b, window, cx);
            assert!(v.source_back_path().is_none());
            link(v, &a, window, cx);
            v.load_source(a.clone(), window, cx);
            assert_eq!(
                v.source_back_path(),
                Some("notes/b.md"),
                "same-path refresh retains entry"
            );
            v.load_source(c, window, cx);
            assert!(
                v.source_back_path().is_none(),
                "unrelated successful source load clears trail"
            );
        });
        if directory.exists() {
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[gpui::test]
    fn source_back_failed_reads_cancel_and_changed_revision_preserve_current_state(
        cx: &mut TestAppContext,
    ) {
        let (view, visual, directory, a) = setup(cx);
        view.update_in(visual, |v, window, cx| {
            let b = source(
                v.expected_workspace.as_ref().unwrap(),
                "notes/b.md",
                "B **😀**\r\n",
            );
            link(v, &b, window, cx);
            v.open_source_find(window, cx);
            let before = v.source.link_selection(window, cx);
            let stamp = v.source.stamp(cx);
            let entity = v.source.entity_id();
            for failure in [
                Err("Previous note is missing".into()),
                Ok(json!({})),
                Ok({
                    let mut bad = a.clone();
                    bad["content_base64"] = json!("/w==");
                    bad
                }),
                Ok({
                    let mut bad = a.clone();
                    bad["path"] = json!("wrong.md");
                    bad
                }),
                Ok({
                    let mut bad = a.clone();
                    bad["revision"] = json!("sha256:wrong");
                    bad
                }),
            ] {
                v.back_source(window, cx);
                let flight = v.source_navigation.flight.clone().unwrap();
                v.finish_source_navigation(flight, failure, window, cx);
                assert_eq!(v.source_snapshot.as_ref(), Some(&b));
                assert_eq!(v.source.entity_id(), entity);
                assert_eq!(v.source.stamp(cx), stamp);
                assert_eq!(v.source.link_selection(window, cx), before);
                assert!(v.source_find.active(), "failed read did not clear Find");
                assert_eq!(v.source_back_path(), Some("notes/a.md"));
            }
            v.source
                .managed()
                .unwrap()
                .update(cx, |s, cx| s.replace_all("dirty B", window, cx));
            v.back_source(window, cx);
            assert!(matches!(
                v.pending_navigation,
                Some(PendingNavigation::SourceBack)
            ));
            assert!(!v.source_navigation_pending());
            v.sync_source_find(window, cx);
            assert!(
                v.source_find.active(),
                "link/Back draft prompt does not consume Find on Cancel"
            );
            // Existing Keep editing clears the intent, never the committed entry.
            v.pending_navigation = None;
            v.navigation_after_source = false;
            assert_eq!(v.source_back_path(), Some("notes/a.md"));
            v.source.reset("B **😀**\r\n", window, cx);
            v.pending_navigation = Some(PendingNavigation::SourceBack);
            v.continue_navigation(window, cx);
            let flight = v
                .source_navigation
                .flight
                .clone()
                .expect("existing continuation retains Back intent");
            let changed = source(
                v.expected_workspace.as_ref().unwrap(),
                "notes/a.md",
                "Changed A\n",
            );
            v.finish_source_navigation(flight, Ok(changed.clone()), window, cx);
            assert_eq!(v.source_snapshot.as_ref(), Some(&changed));
            assert!(!v.source_projection.live);
            assert_eq!(v.source.link_selection(window, cx).unwrap().bytes, 0..0);
            assert!(v.notice.contains("changed"));
            assert!(v.source_back_path().is_none());
            assert!(!v.source_find.active());
        });
        if directory.exists() {
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[gpui::test]
    fn source_back_late_reply_cannot_cross_departure_or_cancel(cx: &mut TestAppContext) {
        let (view, visual, directory, a) = setup(cx);
        view.update_in(visual, |v, window, cx| {
            let workspace = v.expected_workspace.clone().unwrap();
            let goal = v.snapshot.clone();
            let b = source(&workspace, "notes/b.md", "B\n");
            for reason in [
                "cancel",
                "capture",
                "collection",
                "goal",
                "workspace",
                "endpoint",
                "source",
                "new_request",
                "busy",
            ] {
                v.endpoint = "127.0.0.1:1".parse().unwrap();
                v.expected_workspace = Some(workspace.clone());
                v.snapshot = goal.clone();
                v.surface = Surface::Source;
                v.show_capture = false;
                v.collection = Collection::Sources;
                v.busy = false;
                v.load_source(a.clone(), window, cx);
                v.open_source_link("notes/b.md".into(), window, cx);
                let flight = v.source_navigation.flight.clone().unwrap();
                match reason {
                    "cancel" => v.cancel_source_navigation(),
                    "capture" => v.begin_capture(window, cx),
                    "collection" => v.select_collection(Collection::Goals, window, cx),
                    "goal" => v.snapshot["goal"]["id"] = json!(uuid()),
                    "workspace" => v.expected_workspace = Some(json!({"brain_id":uuid()})),
                    "endpoint" => v.endpoint = "127.0.0.1:2".parse().unwrap(),
                    "source" => v.source.reset("late edit", window, cx),
                    "new_request" => v.request_generation += 1,
                    "busy" => v.busy = true,
                    _ => unreachable!(),
                }
                let bytes = v.source.value(cx);
                v.finish_source_navigation(flight, Ok(b.clone()), window, cx);
                assert_eq!(v.source_snapshot.as_ref(), Some(&a), "{reason}");
                assert_eq!(v.source.value(cx), bytes, "{reason}");
                assert!(v.source_navigation.previous.is_none(), "{reason}");
                v.cancel_source_navigation();
            }
            v.endpoint = "127.0.0.1:1".parse().unwrap();
            v.expected_workspace = Some(workspace.clone());
            v.snapshot = goal;
            v.surface = Surface::Source;
            v.show_capture = false;
            v.collection = Collection::Sources;
            v.busy = false;
            v.load_source(a.clone(), window, cx);
            v.open_source_link("notes/b.md".into(), window, cx);
            let first = v.source_navigation.flight.clone().unwrap();
            v.open_source_link("notes/c.md".into(), window, cx);
            let second = v.source_navigation.flight.clone().unwrap();
            assert_ne!(first.generation, second.generation);
            v.finish_source_navigation(first, Ok(b), window, cx);
            assert_eq!(
                v.source_snapshot.as_ref(),
                Some(&a),
                "obsolete link cannot install B"
            );
            assert_eq!(
                v.source_navigation.flight.as_ref().unwrap().target,
                "notes/c.md"
            );
            let c = source(&workspace, "notes/c.md", "Newer choice C\n");
            v.finish_source_navigation(second, Ok(c.clone()), window, cx);
            assert_eq!(v.source_snapshot.as_ref(), Some(&c));
            assert_eq!(v.source_back_path(), Some("notes/a.md"));
        });
        if directory.exists() {
            std::fs::remove_dir_all(directory).unwrap();
        }
    }
    #[gpui::test]
    fn heading_after_paint_is_one_shot_and_real_input_cancels_pending(cx: &mut TestAppContext) {
        use gpui::InputEvent;
        let (view, visual, directory, _) = setup(cx);
        view.update_in(visual, |v, _, cx| {
            let offset = v.source.value(cx).find("**😀").unwrap();
            v.source.managed().unwrap().update(cx, |s, cx| {
                s.set_selected_range(offset..offset, cx);
            });
        });
        visual.run_until_parked();
        let (restore, scroll, bounds) = view.update_in(visual, |v, window, cx| {
            let snapshot = v.source_snapshot.as_ref().unwrap();
            let state = v.source.managed().unwrap().read(cx);
            let scroll = state.scroll_offset();
            let bounds = state.input_bounds();
            let owner = v.navigation_owner().unwrap();
            let selection = v.source.link_selection(window, cx).unwrap();
            let restore = Restore {
                owner: owner.clone(),
                path: text(&snapshot["path"]),
                revision: text(&snapshot["revision"]),
                stamp: v.source.stamp(cx).unwrap(),
                initial_selection: selection.clone(),
                initial_scroll: scroll,
                visit: Visit {
                    owner,
                    path: text(&snapshot["path"]),
                    revision: text(&snapshot["revision"]),
                    live: v.source_projection.live,
                    selection,
                    scroll,
                },
                heading: true,
            };
            (restore, scroll, bounds)
        });
        // The actual existing Source paint hook, not a direct helper invocation,
        // must consume and move the caret above its ordinary bottom-edge reveal.
        view.update_in(visual, |v, window, cx| {
            v.position_opened_heading(restore.clone(), window, cx);
        });
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            assert!(v.source_navigation.heading_position.is_none());
            assert!(v.source.managed().unwrap().read(cx).scroll_offset().y < scroll.y);
            v.source.managed().unwrap().update(cx, |s, cx| {
                s.set_scroll_offset(scroll, cx);
            });
        });
        visual.run_until_parked();
        view.update_in(visual, |v, _, cx| {
            assert_eq!(
                v.source.managed().unwrap().read(cx).scroll_offset(),
                scroll,
                "consumed placement must never follow later movement"
            );
        });
        for kind in ["key", "wheel", "mouse"] {
            // Arm and dispatch inside one outer update: normal native input can
            // arrive after the load but before the next target paint. The input
            // uses the already painted editor listeners, before any newer paint.
            visual.update(|window, cx| {
                view.update(cx, |v, cx| {
                    v.source.managed().unwrap().update(cx, |s, cx| {
                        s.set_selected_range(restore.visit.selection.bytes.clone(), cx);
                    });
                    v.source.focus(window, cx);
                    assert!(v
                        .source
                        .managed()
                        .unwrap()
                        .read(cx)
                        .focus_handle(cx)
                        .is_focused(window));
                    v.position_opened_heading(restore.clone(), window, cx);
                    assert!(v.source_navigation.heading_position.is_some());
                });
                match kind {
                    "wheel" => {
                        window.dispatch_event(
                            gpui::ScrollWheelEvent {
                                position: bounds.center(),
                                delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-25.))),
                                ..Default::default()
                            }
                            .to_platform_input(),
                            cx,
                        );
                    }
                    "mouse" => {
                        window.dispatch_event(
                            gpui::MouseDownEvent {
                                position: bounds.center(),
                                button: gpui::MouseButton::Left,
                                modifiers: gpui::Modifiers::default(),
                                click_count: 1,
                                first_mouse: false,
                            }
                            .to_platform_input(),
                            cx,
                        );
                    }
                    "key" => {
                        window.dispatch_keystroke(gpui::Keystroke::parse("right").unwrap(), cx);
                    }
                    _ => unreachable!(),
                }
                view.update(cx, |v, _| {
                    assert!(
                        v.source_navigation.heading_position.is_none(),
                        "actual {kind} capture must cancel before paint"
                    );
                });
            });
            visual.run_until_parked();
        }
        if directory.exists() {
            std::fs::remove_dir_all(directory).unwrap();
        }
    }
    #[gpui::test]
    fn heading_link_late_target_and_landing_preserve_newer_position_and_back(
        cx: &mut TestAppContext,
    ) {
        let (view, visual, directory, a) = setup(cx);
        view.update_in(visual, |v, window, cx| {
            let workspace = v.expected_workspace.clone().unwrap();
            let b = source(&workspace, "notes/b.md", "# B\n\nCurrent note\n");
            let c = source(&workspace, "notes/c.md", "# C\n\n## Destination\n");
            link(v, &b, window, cx);
            let before = v.source.value(cx);
            v.open_source_link_at("notes/c.md".into(), Some("Destination".into()), window, cx);
            let flight = v.source_navigation.flight.clone().unwrap();
            v.finish_source_navigation(flight, Err("Missing heading".into()), window, cx);
            assert_eq!(v.source.value(cx), before);
            assert_eq!(v.source_back_path(), Some("notes/a.md"));
            for reason in ["selection", "mode", "owner"] {
                v.load_source(b.clone(), window, cx);
                v.open_source_link_at("notes/c.md".into(), Some("Destination".into()), window, cx);
                let mut flight = v.source_navigation.flight.clone().unwrap();
                flight.landing = Some(5);
                match reason {
                    "selection" => v
                        .source
                        .managed()
                        .unwrap()
                        .update(cx, |s, cx| s.set_selected_range(2..2, cx)),
                    "mode" => v.source_projection.live = !v.source_projection.live,
                    "owner" => v.snapshot["goal"]["id"] = json!(uuid()),
                    _ => unreachable!(),
                }
                v.finish_source_navigation(flight, Ok(c.clone()), window, cx);
                assert_eq!(v.source_snapshot.as_ref(), Some(&b));
            }
            // A later selection also cancels deferred landing after a valid read.
            v.load_source(a, window, cx);
            v.source_projection.live = true;
            v.open_source_link_at("notes/c.md".into(), Some("Destination".into()), window, cx);
            let mut flight = v.source_navigation.flight.clone().unwrap();
            flight.landing = Some(5);
            v.finish_source_navigation(flight, Ok(c.clone()), window, cx);
            assert_eq!(v.source_snapshot.as_ref(), Some(&c));
            assert!(
                v.source_navigation.restore.is_some(),
                "LP landing waits for current projection"
            );
            v.source
                .managed()
                .unwrap()
                .update(cx, |s, cx| s.set_selected_range(1..1, cx));
            v.restore_source_navigation(window, cx);
            assert!(v.source_navigation.restore.is_none());
            assert_eq!(v.source.managed().unwrap().read(cx).selected_range(), 1..1);
        });
        if directory.exists() {
            std::fs::remove_dir_all(directory).unwrap();
        }
    }
    #[gpui::test]
    fn self_heading_lands_at_exact_offset_and_back_restores_origin(cx: &mut TestAppContext) {
        let (view, visual, directory, a) = setup(cx);
        let raw = format!(
            "# Origin\r\n\r\n[Self](#Landing)\r\n\r\n{}## Landing\r\nbody\r\n",
            "Preceding paragraph.\r\n\r\n".repeat(60)
        );
        let self_note = source(&json!({"brain_id":a["brain_id"]}), "notes/a.md", &raw);
        let origin = raw.find("[Self]").unwrap();
        let landing = raw.find("## Landing").unwrap();
        view.update_in(visual, |v, window, cx| {
            v.load_source(self_note.clone(), window, cx);
            v.source
                .managed()
                .unwrap()
                .update(cx, |s, cx| s.set_selected_range(origin..origin, cx));
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            v.open_source_link_at("notes/a.md".into(), Some("Landing".into()), window, cx);
            let mut flight = v
                .source_navigation
                .flight
                .clone()
                .expect("self-heading starts the actual guarded read");
            let snapshot =
                okilum_core::source_projection::Snapshot::new("notes/a.md", 0, raw.as_str());
            flight.landing = Some(
                okilum_core::source_classifier::classify(&snapshot)
                    .heading_offset("Landing")
                    .unwrap(),
            );
            v.finish_source_navigation(flight, Ok(self_note.clone()), window, cx);
            assert_eq!(v.source_snapshot.as_ref(), Some(&self_note));
            assert_eq!(
                v.source.managed().unwrap().read(cx).selected_range(),
                landing..landing
            );
            assert_eq!(v.source_back_path(), Some("notes/a.md"));
        });
        visual.run_until_parked();
        view.update_in(visual, |v, window, cx| {
            back(v, &self_note, window, cx);
            assert_eq!(
                v.source.managed().unwrap().read(cx).selected_range(),
                origin..origin
            );
            assert_eq!(v.source.value(cx), raw);
        });
        if directory.exists() {
            std::fs::remove_dir_all(directory).unwrap();
        }
    }
}
