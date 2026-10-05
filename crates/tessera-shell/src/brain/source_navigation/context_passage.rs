//! Context-origin passage reads share generation cancellation and the single return slot.
use super::*;
use context_ui::ContextOrigin;

#[derive(Clone)]
pub(super) struct ContextReturn {
    pub origin: ContextOrigin,
    pub destination: String,
}
#[derive(Clone)]
pub(super) struct ContextFlight {
    generation: u64,
    request_generation: u64,
    origin: ContextOrigin,
    owner: Owner,
    source: Option<Value>,
    stamp: SourceStamp,
    selection: LinkSelection,
    scroll: Point<Pixels>,
    live: bool,
    path: String,
    passage: Passage,
}
impl BrainView {
    pub(in crate::brain) fn cancel_context_passage_input(&mut self) {
        if self.source_navigation.context_flight.is_some() {
            self.cancel_source_navigation();
        }
    }
    pub(super) fn sync_context_passage(&mut self, cx: &App) {
        if self
            .source_navigation
            .context_flight
            .as_ref()
            .is_some_and(|f| !self.context_flight_matches(f, cx))
        {
            self.cancel_source_navigation();
        }
    }
    fn context_flight_matches(&self, f: &ContextFlight, cx: &App) -> bool {
        self.source_navigation.generation == f.generation
            && self.request_generation == f.request_generation
            && self.context_origin_matches(&f.origin, cx)
            && self.navigation_owner().as_ref() == Some(&f.owner)
            && self.source_snapshot == f.source
            && self.source.stamp(cx) == Some(f.stamp)
            && self.source_projection.live == f.live
            && self.source.managed().unwrap().read(cx).scroll_offset() == f.scroll
    }
    #[allow(clippy::too_many_arguments)]
    pub(in crate::brain) fn begin_context_passage(
        &mut self,
        origin: ContextOrigin,
        path: String,
        revision: String,
        start_line: usize,
        end_line: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let flight = ContextFlight {
            generation: self.source_navigation.generation,
            request_generation: self.request_generation,
            origin,
            owner: self.navigation_owner().unwrap(),
            source: self.source_snapshot.clone(),
            stamp: self.source.stamp(cx).unwrap(),
            selection: self.source.link_selection(window, cx).unwrap(),
            scroll: self.source.managed().unwrap().read(cx).scroll_offset(),
            live: self.source_projection.live,
            path,
            passage: Passage {
                revision,
                start_line,
                end_line,
            },
        };
        self.source_navigation.context_flight = Some(flight.clone());
        self.error = None;
        self.notice = "Opening Context source passage…".into();
        cx.spawn_in(window,async move |this,cx| {
            let f=flight.clone();
            let reply=cx.background_executor().spawn(async move {
                let data=rpc_guarded(f.owner.endpoint,json!({"op":"source_read","path":f.path}),Some(&f.owner.workspace))?;
                let raw=validated_source(&data,&f.path,&f.owner.workspace)?;
                let offset=passage_offset(&raw,&data,&f.passage).map_err(|_|"This search passage changed or its lines are invalid. Find sources again before opening it.".to_owned())?;
                Ok::<_,String>((data,offset))
            }).await;
            let _=this.update_in(cx,|this,window,cx|this.finish_context_passage(flight,reply,window,cx));
        }).detach();
        cx.notify();
    }
    fn finish_context_passage(
        &mut self,
        f: ContextFlight,
        reply: Result<(Value, usize), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.context_flight_matches(&f, cx)
            || self.context_inputs_composing(window, cx)
            || self.source.link_selection(window, cx).as_ref() != Some(&f.selection)
        {
            if self
                .source_navigation
                .context_flight
                .as_ref()
                .is_some_and(|active| active.generation == f.generation)
            {
                self.cancel_source_navigation();
                cx.notify();
            }
            return;
        }
        self.source_navigation.context_flight = None;
        let validated = reply.and_then(|(data, offset)| {
            let raw = validated_source(&data, &f.path, &f.owner.workspace)?;
            if passage_offset(&raw, &data, &f.passage).ok() != Some(offset) {
                return Err(
                    "This search passage changed. Find sources again before opening it.".into(),
                );
            }
            Ok((data, offset))
        });
        let (data, offset) = match validated {
            Ok(value) => value,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        // All source/owner/form checks precede surface changes and replacing the return entry.
        let same = self
            .source_snapshot
            .as_ref()
            .is_some_and(|s| s["path"] == data["path"] && s["revision"] == data["revision"]);
        let live = if self.source_snapshot.is_some() {
            f.live
        } else {
            false
        };
        self.cancel_source_selection_context(cx);
        if !same {
            self.load_source(data.clone(), window, cx);
        } else {
            self.cancel_source_navigation();
        }
        if !self.source_editable {
            return;
        }
        self.surface = Surface::Source;
        self.source_projection.live = live;
        self.apply_source_projection(cx);
        // Move focus with the surface so subsequent native input belongs to Source.
        self.source.focus(window, cx);
        self.source_navigation.previous = Some(ReturnEntry::Context(ContextReturn {
            origin: f.origin,
            destination: f.path.clone(),
        }));
        let owner = self.navigation_owner().unwrap();
        let utf16 = self.source.value(cx)[..offset].encode_utf16().count();
        self.source_navigation.restore = Some(Restore {
            owner: owner.clone(),
            path: f.path.clone(),
            revision: text(&data["revision"]),
            stamp: self.source.stamp(cx).unwrap(),
            initial_selection: self.source.link_selection(window, cx).unwrap(),
            initial_scroll: self.source.managed().unwrap().read(cx).scroll_offset(),
            visit: Visit {
                owner,
                path: f.path,
                revision: text(&data["revision"]),
                live,
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
        self.notice = "Opened source passage. Return to Context keeps your current form.".into();
        cx.notify();
    }
    pub(in crate::brain) fn context_return_available(&self, cx: &App) -> bool {
        let Some(ReturnEntry::Context(p)) = &self.source_navigation.previous else {
            return false;
        };
        self.navigation_visible()
            && self.context_origin_retained(&p.origin, cx)
            && self
                .source_snapshot
                .as_ref()
                .is_some_and(|s| s["path"] == p.destination)
    }
    pub(in crate::brain) fn return_to_context(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.context_return_available(cx) {
            return;
        }
        if let Err(error) = self.context_passage_guard(cx) {
            self.error = Some(error);
            cx.notify();
            return;
        }
        if self.source_navigation_pending()
            || self.source.link_selection(window, cx).is_none()
            || self.context_inputs_composing(window, cx)
        {
            self.error =
                Some("Finish navigation or text composition before returning to Context.".into());
            cx.notify();
            return;
        }
        self.cancel_source_navigation();
        self.source_navigation.previous = None;
        self.surface = Surface::Context;
        self.notice = "Returned to retained Context".into();
        // Deliberately no open_context(), form reconstruction, query, Source read or save.
        cx.notify();
    }
}
