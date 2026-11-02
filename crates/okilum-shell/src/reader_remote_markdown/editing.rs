//! Explicit remote editing in the existing window. Draft protection is local only.
use super::view::Panel;
use super::*;
use crate::*;
use base64::Engine as _;
use gpui_component::input::{projection::SourceMutation, Editor, EditorState};
use okilum_core::remote_markdown::{
    draft::{Record, Recovery},
    save::{self, Attempt, Outcome, Snapshot},
};
use std::path::PathBuf;

pub(super) type Saver =
    Arc<dyn Fn(&Source, &Attempt, &AtomicBool) -> io::Result<Outcome> + Send + Sync>;
pub(super) struct Editing {
    pub(super) record: Record,
    pub(super) recovery: Recovery,
    pub(super) input: Entity<EditorState>,
    pub(super) _changed: Subscription,
    pub(super) epoch: u64,
    pub(super) protecting: bool,
    pub(super) protection: Option<Task<()>>,
    pub(super) saving: Option<Attempt>,
    pub(super) conflict: Option<Outcome>,
    pub(super) compared: bool,
    pub(super) unsupported: bool,
}
impl Panel {
    fn edit_owner(&self, source: &Source, generation: u64) -> bool {
        generation == self.controller.generation
            && self
                .requested_source
                .as_ref()
                .is_some_and(|s| s.url() == source.url())
            && matches!(
                self.controller.state,
                State::Ready { .. } | State::Disconnected(_)
            )
    }

    pub(super) fn begin_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit.is_some() || self.edit_task.is_some() {
            return;
        }
        let (source, markdown) = match &self.controller.state {
            State::Ready { source, markdown } => (source.clone(), Some(markdown.clone())),
            State::Disconnected(_) => {
                let Some(source) = self.requested_source.clone() else {
                    return;
                };
                (source, None)
            }
            _ => return,
        };
        let generation = self.controller.generation;
        let Some(root) = self.recovery_root.clone() else {
            self.notice = Some(
                "Local draft storage is unavailable. This remote file stays read-only.".into(),
            );
            cx.notify();
            return;
        };
        self.notice = Some("Opening protected local draft…".into());
        self.edit_task=Some(cx.spawn_in(window, async move |this,cx| {
            let target=source.clone();
            let result=cx.background_executor().spawn(async move { match markdown { Some(markdown)=>Recovery::open(&root,&target,markdown),None=>Recovery::recover(&root,&target) } }).await;
            let _=this.update_in(cx,|this,window,cx| {
                if !this.edit_owner(&source,generation) { return; }
                this.edit_task=None;
                match result {
                    Err(error)=>this.notice=Some(error.to_string()),
                    Ok((recovery,record))=> {
                        let recovered=record.dirty();
                        let input=cx.new(|cx| {
                            let mut state=EditorState::new(window,cx).document_newlines(true).keep_leading_bom(true).folding(false).searchable(true).replaceable(false).soft_wrap(true);
                            state.set_value(record.text.clone(),window,cx);state
                        });
                        let changed=cx.subscribe(&input,|this,input,_:&SourceMutation,cx| {
                            let text=input.read(cx).value().to_string();
                            let Some(edit)=&mut this.edit else { return; };
                            if edit.input != input { return; }
                            edit.record.text=text;edit.epoch+=1;edit.protecting=true;
                            let (epoch,record,recovery,id)=(edit.epoch,edit.record.clone(),edit.recovery.clone(),input.entity_id());
                            edit.protection=Some(cx.spawn(async move |this,cx| {
                                cx.background_executor().timer(Duration::from_millis(150)).await;
                                let result=cx.background_executor().spawn(async move { recovery.persist(epoch,&record) }).await;
                                let _=this.update(cx,|this,cx| {
                                    let Some(edit)=&mut this.edit else { return; };
                                    if edit.input.entity_id()!=id || edit.epoch!=epoch { return; }
                                    edit.protecting=false;
                                    if let Err(error)=result { this.notice=Some(format!("Draft protection failed: {error}. Keep this window open and retry Save.")); }
                                    cx.notify();
                                });
                            }));
                            cx.notify();
                        });
                        this.edit=Some(Editing { record,recovery,input:input.clone(),_changed:changed,epoch:0,protecting:false,protection:None,saving:None,conflict:None,compared:false,unsupported:false });
                        this.notice=Some(if recovered { "Recovered local changes. Save/Retry checks the reviewed remote revision before writing." } else { "Changes stay local until you explicitly Save." }.into());
                        input.update(cx,|state,cx| state.focus(window,cx));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    /// Synchronous barrier only for explicit Save/Retry/close/navigation. Typing
    /// coalesces local protection on a background worker; it never saves remotely.
    pub(super) fn protect(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(edit) = &mut self.edit else {
            return true;
        };
        edit.record.text = edit.input.read(cx).value().to_string();
        edit.epoch += 1;
        edit.protection = None;
        match edit.recovery.persist(edit.epoch, &edit.record) {
            Ok(_) => {
                edit.protecting = false;
                true
            }
            Err(error) => {
                self.notice = Some(format!(
                    "Cannot protect this draft: {error}. The window stays open; retry Save."
                ));
                cx.notify();
                false
            }
        }
    }
    pub(super) fn explicit_save(&mut self, cx: &mut Context<Self>) {
        if self
            .edit
            .as_ref()
            .is_none_or(|edit| edit.saving.is_some() || edit.conflict.is_some())
        {
            return;
        }
        if !self.protect(cx) {
            return;
        }
        let Some(source) = self.requested_source.clone() else {
            return;
        };
        let generation = self.controller.generation;
        if !self.edit_owner(&source, generation) {
            return;
        }
        let edit = self.edit.as_mut().unwrap();
        let attempt = edit.record.attempt();
        edit.epoch += 1;
        if let Err(error) = edit.recovery.persist(edit.epoch, &edit.record) {
            self.notice = Some(error.to_string());
            cx.notify();
            return;
        }
        edit.saving = Some(attempt.clone());
        let cancel = Arc::new(AtomicBool::new(false));
        self.write_cancel = Some(cancel.clone());
        let saver = self.saver.clone();
        let (send, receive) = async_channel::bounded(1);
        let target = source.clone();
        let submitted = attempt.clone();
        // OpenSSH and all pipe IO use the existing window's background-worker
        // boundary. The UI executor never waits for remote IO.
        std::thread::spawn(move || {
            let result = saver(&target, &submitted, &cancel);
            let _ = send.send_blocking(result);
        });
        self.notice = Some("Saving explicitly… Your local draft is protected.".into());
        self.save_task=Some(cx.spawn(async move |this,cx| {
            let Ok(result)=receive.recv().await else { return; };
            let _=this.update(cx,|this,cx| {
                if !this.edit_owner(&source,generation) { return; }
                let Some(edit)=&mut this.edit else { return; };
                if edit.saving.as_ref().is_none_or(|a| a.id!=attempt.id) { return; }
                edit.saving=None;this.write_cancel=None;
                match result.and_then(|outcome| { outcome.validate(&attempt)?;Ok(outcome) }) {
                    Err(error)=>this.notice=Some(format!("{error}. Changes are kept locally; Retry checks the remote version first.")),
                    Ok(Outcome::Saved{..})=> {
                        edit.record.text=edit.input.read(cx).value().to_string();
                        edit.record.acknowledge(&attempt);edit.unsupported=false;
                        if let State::Ready{markdown,..}=&mut this.controller.state { *markdown=attempt.text; }
                        this.notice=Some("Saved atomically. The previous remote version is recoverable in .okilum-remote-history.".into());
                    }
                    Ok(outcome @ Outcome::Conflict{..})=> {
                        this.notice=Some("The remote file changed. Compare it, then explicitly choose Use remote or Keep mine. Your draft is protected.".into());
                        edit.conflict=Some(outcome);edit.compared=false;
                    }
                    Ok(Outcome::Unsupported{..})=>{edit.unsupported=true;this.notice=Some(save::UNSUPPORTED.into());}
                    Ok(Outcome::Error{message})=>this.notice=Some(message),
                }
                this.protect(cx);cx.notify();
            });
        }));
        cx.notify();
    }
    pub(super) fn compare(&mut self, cx: &mut Context<Self>) {
        if let Some(edit) = &mut self.edit {
            edit.compared = true;
        }
        cx.notify();
    }
    pub(super) fn resolve_conflict(
        &mut self,
        keep_mine: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.protect(cx) {
            return;
        }
        let Some(edit) = &mut self.edit else {
            return;
        };
        if !edit.compared {
            return;
        }
        let Some(Outcome::Conflict {
            remote: Some(remote),
            ..
        }) = &edit.conflict
        else {
            return;
        };
        let text = remote.text.clone();
        // Review chooses a new base only. Keep mine does NOT send: another
        // explicit Save still verifies it, including any later remote edit.
        edit.record.base = text.clone();
        edit.record.pending = None;
        if !keep_mine {
            edit.record.text = text.clone();
            edit.input
                .update(cx, |state, cx| state.set_value(text, window, cx));
        }
        edit.conflict = None;
        edit.compared = false;
        edit.unsupported = false;
        if !self.protect(cx) {
            return;
        }
        self.notice=Some(if keep_mine { "Reviewed remote version selected as the new base. Click Save to keep your changes." } else { "Remote version selected. The earlier local draft remains in local recovery history." }.into());
        cx.notify();
    }
    pub(super) fn edit_controls(&self, cx: &Context<Self>) -> AnyElement {
        let Some(edit) = &self.edit else {
            return Button::new("remote-edit")
                .debug_selector(|| "remote-edit".into())
                .small()
                .label("Edit")
                .disabled(
                    !matches!(
                        self.controller.state,
                        State::Ready { .. } | State::Disconnected(_)
                    ) || self.edit_task.is_some(),
                )
                .on_click(cx.listener(|this, _, window, cx| this.begin_edit(window, cx)))
                .into_any_element();
        };
        h_flex()
            .gap_2()
            .child(
                Button::new("remote-save")
                    .debug_selector(|| "remote-save".into())
                    .small()
                    .label("Save")
                    .disabled(edit.saving.is_some() || edit.conflict.is_some() || edit.unsupported)
                    .on_click(cx.listener(|this, _, _, cx| this.explicit_save(cx))),
            )
            .child(
                Button::new("remote-retry")
                    .debug_selector(|| "remote-retry".into())
                    .small()
                    .label("Retry")
                    .disabled(edit.saving.is_some() || edit.conflict.is_some())
                    .on_click(cx.listener(|this, _, _, cx| this.explicit_save(cx))),
            )
            .child(if edit.protecting {
                "Protecting draft…"
            } else {
                "Draft protected locally"
            })
            .into_any_element()
    }
    pub(super) fn editor(&self, cx: &Context<Self>) -> Option<AnyElement> {
        self.edit.as_ref().map(|edit| {
            let mut body = v_flex().flex_1().min_h_0().gap_2();
            if let Some(Outcome::Conflict {
                remote,
                raced,
                recovery,
                message,
                ..
            }) = &edit.conflict
            {
                body = body.child(div().child(message.clone())).child(
                    Button::new("remote-compare")
                        .debug_selector(|| "remote-compare".into())
                        .small()
                        .label("Compare")
                        .on_click(cx.listener(|this, _, _, cx| this.compare(cx))),
                );
                if edit.compared {
                    let mut versions = v_flex()
                        .id("remote-comparison")
                        .max_h(px(200.))
                        .overflow_y_scroll();
                    for (label, snapshot) in [
                        ("Remote version", remote),
                        ("Retained concurrent version", raced),
                    ] {
                        if let Some(Snapshot { text, .. }) = snapshot {
                            versions = versions
                                .child(div().child(label))
                                .child(div().child(text.clone()));
                        }
                    }
                    body = body
                        .child(versions)
                        .child(div().text_sm().child(format!(
                                "Recoverable remote files: {}",
                                recovery
                                    .iter()
                                    .map(|path| base64::engine::general_purpose::STANDARD
                                        .decode(path)
                                        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                                        .unwrap_or_else(|_| "Recovery path unavailable".into()))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("remote-use-remote")
                                        .debug_selector(|| "remote-use-remote".into())
                                        .small()
                                        .label("Use remote")
                                        .disabled(remote.is_none())
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.resolve_conflict(false, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("remote-keep-mine")
                                        .debug_selector(|| "remote-keep-mine".into())
                                        .small()
                                        .label("Keep mine")
                                        .disabled(remote.is_none())
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.resolve_conflict(true, window, cx)
                                        })),
                                ),
                        );
                }
            }
            body.child(
                Editor::new(&edit.input)
                    .readonly(edit.unsupported)
                    .size_full(),
            )
            .into_any_element()
        })
    }
}
pub(super) fn default_saver() -> Saver {
    Arc::new(|source, attempt, cancel| {
        save::save(source, attempt, || cancel.load(Ordering::Acquire))
    })
}
pub(super) fn recovery_root() -> Option<PathBuf> {
    crate::reader_layout::config_base().map(|base| base.join("okilum/remote-drafts"))
}
