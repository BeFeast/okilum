//! Shared policy editing with the exact local include preview beside the source.
use super::*;
use gpui_component::{
    input::{Editor, EditorState},
    WindowExt,
};
use okilum_sync_controller::direct::IgnorePreview;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

impl SyncSettings {
    pub(super) fn edit_ignore_policy(
        &self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(controller) = self.controller.clone() else {
            return;
        };
        let alive = Arc::new(AtomicBool::new(true));
        let panel = cx.new(|cx| {
            PolicyEditor::new(
                controller,
                path,
                self.lifetime.ticket(),
                alive.clone(),
                window,
                cx,
            )
        });
        panel.update(cx, |panel, cx| panel.load(window, cx));
        window.open_sheet(cx, move |sheet, _, _| {
            let alive = alive.clone();
            sheet
                .title("Shared Sync exclusions")
                .size(px(640.))
                .overlay_closable(false)
                .on_close(move |_, _, _| alive.store(false, Ordering::Release))
                .child(panel.clone())
        });
    }
}
struct PolicyEditor {
    controller: Controller,
    path: PathBuf,
    parent: Ticket,
    alive: Arc<AtomicBool>,
    _release: Subscription,
    preview: Option<IgnorePreview>,
    editor: Entity<EditorState>,
    local: Entity<EditorState>,
    busy: bool,
    message: Option<String>,
}
impl PolicyEditor {
    fn new(
        controller: Controller,
        path: PathBuf,
        parent: Ticket,
        alive: Arc<AtomicBool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| EditorState::new(window, cx).soft_wrap(true));
        let local = cx.new(|cx| {
            let mut editor = EditorState::new(window, cx).soft_wrap(true);
            editor.set_readonly(true, cx);
            editor
        });
        let released = alive.clone();
        let release = cx.on_release(move |_, _| released.store(false, Ordering::Release));
        Self {
            controller,
            path,
            parent,
            alive,
            _release: release,
            preview: None,
            editor,
            local,
            busy: false,
            message: None,
        }
    }
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.preview = None;
        self.message = None;
        let controller = self.controller.clone();
        let path = self.path.clone();
        let parent = self.parent.clone();
        let alive = self.alive.clone();
        let access = reader_features::access(reader_features::Feature::Sync, cx);
        cx.spawn_in(window, async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(okilum_t3::scope_network(access, async move {
                    parent.check()?;
                    anyhow::ensure!(alive.load(Ordering::Acquire), "Policy editor closed");
                    controller.preview_ignore_policy(&path)
                }))
                .await;
            let _ = view.update_in_own_window(cx, |view, window, cx| {
                if !view.parent.is_current() || !view.alive.load(Ordering::Acquire) {
                    return;
                }
                view.busy = false;
                match result {
                    Ok(preview) => {
                        view.editor.update(cx, |input, cx| {
                            input.set_value(preview.text.clone(), window, cx)
                        });
                        view.local.update(cx, |input, cx| {
                            input.set_value(preview.proposed_local.clone(), window, cx)
                        });
                        view.preview = Some(preview);
                    }
                    Err(error) => {
                        view.message = Some(format!(
                            "Could not load exclusions. {}",
                            short_cause(&error)
                        ))
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(preview) = self.preview.clone() else {
            return;
        };
        let text = self.editor.read(cx).value().to_string();
        self.busy = true;
        self.message = None;
        let controller = self.controller.clone();
        let parent = self.parent.clone();
        let alive = self.alive.clone();
        let access = reader_features::access(reader_features::Feature::Sync, cx);
        cx.spawn_in(window,async move |view,cx| {
            let result = cx.background_executor().spawn(okilum_t3::scope_network(access,async move {
                controller.apply_ignore_policy_checked(&preview,&text,|| {
                    parent.check()?;
                    anyhow::ensure!(alive.load(Ordering::Acquire),"Policy editor closed");
                    Ok(())
                })
            })).await;
            let _ = view.update_in_own_window(cx,|view,window,cx| {
                if !view.parent.is_current() || !view.alive.load(Ordering::Acquire) { return; }
                view.busy = false;
                match result {
                    Ok(_) => { view.load(window,cx); view.message = Some("Saved. The shared file syncs to the other computers; their local rules are kept.".into()); },
                    Err(error) => view.message = Some(format!("Could not save exclusions. {} Refresh the preview before trying again.",short_cause(&error))),
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }
}
impl Render for PolicyEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = brand::palette(cx);
        v_flex().id("sync-policy-editor").debug_selector(||"sync-policy-editor".into()).w_full().gap_4()
            .child(div().text_sm().whitespace_normal().child(self.path.display().to_string()))
            .child("Edit once for this vault. Required service and worktree exclusions stay protected.")
            .child(div().text_sm().text_color(p.text_muted).child("Shared policy · .okilum/sync-ignore"))
            .child(Editor::new(&self.editor).readonly(self.busy || self.preview.is_none()).h(px(240.)))
            .child(div().text_sm().text_color(p.text_muted).child("This computer’s .stignore after Save. Its local bytes stay below the shared include; changes on other computers never replace them."))
            .child(Editor::new(&self.local).readonly(true).h(px(120.)))
            .children(self.message.clone().map(|message|div().text_sm().whitespace_normal().child(message)))
            .child(h_flex().gap_2()
                .child(Button::new("sync-policy-refresh").ghost().label("Refresh preview").disabled(self.busy)
                    .on_click(cx.listener(|this,_,window,cx|this.load(window,cx))))
                .child(Button::new("sync-policy-save").debug_selector(||"sync-policy-save".into()).primary().label("Save shared policy")
                    .disabled(self.busy || self.preview.is_none())
                    .on_click(cx.listener(|this,_,window,cx|this.save(window,cx)))))
    }
}
