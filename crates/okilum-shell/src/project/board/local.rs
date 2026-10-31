//! Local card UI. Manual task state never becomes execution/build acceptance.
use super::*;
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use okilum_t3::board::local as model;

#[derive(Clone)]
pub(super) enum Mode {
    List,
    Add,
    Edit(String),
}
pub(super) struct Editor {
    session: model::Session,
    mode: Mode,
    id: String,
    title: Entity<InputState>,
    details: Entity<TextareaState>,
    state: model::State,
    busy: bool,
    error: Option<String>,
    undo: Option<String>,
}
impl Editor {
    fn new(session: model::Session, mode: Mode, window: &mut Window, cx: &mut App) -> Self {
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("What needs doing?"));
        let details = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(3)
                .placeholder("Details (optional)")
        });
        let mut editor = Self {
            session,
            mode: Mode::List,
            id: String::new(),
            title,
            details,
            state: model::State::Todo,
            busy: false,
            error: None,
            undo: None,
        };
        editor.show(mode, window, cx);
        editor
    }
    fn show(&mut self, mode: Mode, window: &mut Window, cx: &mut App) {
        let card = match &mode {
            Mode::Edit(id) => self.session.projection.cards.iter().find(|c| &c.id == id),
            _ => None,
        };
        self.id = card.map_or_else(|| uuid::Uuid::new_v4().to_string(), |c| c.id.clone());
        let title = card.map_or(String::new(), |c| c.title.clone());
        let details = card.map_or(String::new(), |c| c.details.clone());
        self.state = card.map_or(model::State::Todo, |c| c.state);
        self.title
            .update(cx, |input, cx| input.set_value(title, window, cx));
        self.details
            .update(cx, |input, cx| input.set_value(details, window, cx));
        let focus_title = !matches!(mode, Mode::List);
        self.mode = mode;
        self.error = None;
        if focus_title {
            self.title.update(cx, |input, cx| input.focus(window, cx));
        }
    }
    fn change(&self, cx: &App) -> Option<model::Change> {
        let draft = model::Draft {
            title: self.title.read(cx).value().to_string(),
            details: self.details.read(cx).value().to_string(),
            state: self.state,
        };
        match &self.mode {
            Mode::List => None,
            Mode::Add => Some(model::Change::Add(self.id.clone(), draft)),
            Mode::Edit(id) => Some(model::Change::Edit(id.clone(), draft)),
        }
    }
}
impl Board {
    pub(super) fn refresh_local_cards(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.local_loading {
            return;
        }
        self.local_loading = true;
        let generation = self.local_generation;
        let area = self.area.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    model::read(&area).map_err(|e| {
                        format!("{}: {e:#}", area.join("ops/board-local.toml").display())
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if !this.active || this.local_generation != generation {
                    return;
                }
                let previous = this.local_cards.error.clone();
                this.local_cards.accept(result, now());
                this.local_loading = false;
                if this.local_cards.error != previous {
                    if let Some(error) = &this.local_cards.error {
                        eprintln!("Board local cards: {error}");
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn open_local_management(
        &mut self,
        mode: Mode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editor_loading {
            return;
        }
        self.editor_loading = true;
        let area = self.area.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { model::Session::load(&area) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.editor_loading = false;
                match result {
                    Ok(session) => this.local_editor = Some(Editor::new(session, mode, window, cx)),
                    Err(error) => {
                        this.notice = Some(format!("Cannot manage local cards: {error:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn show_local_form(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.local_editor {
            if !editor.busy {
                editor.show(mode, window, cx);
            }
        }
        cx.notify();
    }
    fn save_local(
        &mut self,
        change: Option<model::Change>,
        undo: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = &mut self.local_editor else {
            return;
        };
        if editor.busy {
            return;
        }
        let change = change.or_else(|| editor.change(cx));
        if !undo && change.is_none() {
            return;
        }
        let previous = editor.session.document.as_ref().map(|d| d.source.clone());
        let undo_source = if undo {
            let Some(source) = editor.undo.clone() else {
                return;
            };
            Some(source)
        } else {
            None
        };
        editor.busy = true;
        editor.error = None;
        let session = editor.session.clone();
        cx.spawn_in(window, async move |this, cx| {
            let removed = matches!(change, Some(model::Change::Remove(_)));
            let result = cx
                .background_executor()
                .spawn(async move {
                    if let Some(source) = undo_source {
                        session.undo(&source)
                    } else {
                        session.save(change.as_ref().expect("validated local change"))
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                if let Some(editor) = &mut this.local_editor {
                    editor.busy = false;
                    match result {
                        Ok(session) => {
                            this.local_generation = this.local_generation.wrapping_add(1);
                            this.local_loading = false;
                            this.local_cards
                                .accept(Ok(session.projection.clone()), now());
                            editor.session = session;
                            editor.mode = Mode::List;
                            editor.undo = if removed { previous } else { None };
                        }
                        Err(error) => {
                            let detail = format!("{error:#}");
                            eprintln!("Board local card write refused: {detail}");
                            editor.error = Some(detail);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn local_card(&self, card: &model::Card, cx: &mut Context<Self>) -> AnyElement {
        let colors = brand::palette(cx);
        let id = card.id.clone();
        let expanded = self.selected_local.as_ref() == Some(&id);
        let mut view = v_flex()
            .min_w_0()
            .gap_2()
            .p_3()
            .rounded_md()
            .bg(colors.surface_raised)
            .debug_selector({
                let id = id.clone();
                move || format!("board-local-card-{id}")
            })
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(colors.text_muted)
                    .child(format!("Local · {}", card.state.label())),
            )
            .child(
                Button::new(SharedString::from(format!("local-expand-{id}")))
                    .ghost()
                    .h_auto()
                    .w_full()
                    .min_w_0()
                    .debug_selector({
                        let id = id.clone();
                        move || format!("board-local-expand-{id}")
                    })
                    .child(
                        div()
                            .whitespace_normal()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(card.title.clone()),
                    )
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| {
                            this.selected = None;
                            this.selected_local = if this.selected_local.as_ref() == Some(&id) {
                                None
                            } else {
                                Some(id.clone())
                            };
                            cx.notify();
                        }
                    })),
            );
        if expanded {
            let draft=format!("Project: {}\nLocal card: {}\nTitle: {}\nDetails: {}\nManual task state: {}\nSource: {}\n\nQuestion for Manager: ", self.area.display(),id,card.title,card.details,card.state.label(),self.area.join("ops/board-local.toml").display());
            view = view
                .child(
                    div()
                        .whitespace_normal()
                        .text_size(px(12.))
                        .child(card.details.clone()),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(colors.text_muted)
                        .child(
                            "Local task state; no executor, PR/CI or build acceptance is inferred.",
                        ),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new(SharedString::from(format!("local-edit-{id}")))
                                .ghost()
                                .small()
                                .label("Edit")
                                .debug_selector({
                                    let id = id.clone();
                                    move || format!("board-local-edit-{id}")
                                })
                                .on_click(cx.listener({
                                    let id = id.clone();
                                    move |this, _, window, cx| {
                                        this.open_local_management(
                                            Mode::Edit(id.clone()),
                                            window,
                                            cx,
                                        )
                                    }
                                })),
                        )
                        .child(
                            Button::new(SharedString::from(format!("local-discuss-{id}")))
                                .ghost()
                                .small()
                                .label("Discuss")
                                .debug_selector({
                                    let id = id.clone();
                                    move || format!("board-local-discuss-{id}")
                                })
                                .tooltip("Prepare a Manager draft; does not send")
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(Action::Discuss(draft.clone()))
                                })),
                        ),
                );
        }
        view.into_any_element()
    }
    pub(super) fn render_local_management(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let editor = self.local_editor.as_ref().expect("local editor");
        let colors = brand::palette(cx);
        let busy = editor.busy;
        let mut view = v_flex()
            .id("board-local-management")
            .debug_selector(|| "board-local-management".into())
            .size_full()
            .overflow_y_scroll()
            .p_4()
            .gap_3()
            .bg(colors.canvas)
            .text_color(colors.text)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Local cards"),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("local-close")
                            .debug_selector(|| "board-local-close".into())
                            .ghost()
                            .label(if self.editor.is_some() {
                                "Back to Manage board"
                            } else {
                                "Back to Board"
                            })
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.local_editor = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(colors.text_muted)
                    .child(self.area.join("ops/board-local.toml").display().to_string()),
            );
        if let Some(error) = &editor.error {
            view = view.child(
                div()
                    .debug_selector(|| "board-local-error".into())
                    .text_color(colors.danger)
                    .whitespace_normal()
                    .child(error.clone()),
            );
        }
        for error in &editor.session.projection.problems {
            view = view.child(
                div()
                    .text_color(colors.warning)
                    .whitespace_normal()
                    .child(error.clone()),
            );
        }
        if matches!(editor.mode, Mode::List) {
            view = view.child(
                Button::new("local-add")
                    .debug_selector(|| "board-local-new".into())
                    .primary()
                    .label("Add card")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_local_form(Mode::Add, window, cx)
                    })),
            );
            if editor.undo.is_some() {
                view = view.child(
                    Button::new("local-undo")
                        .debug_selector(|| "board-local-undo".into())
                        .ghost()
                        .label("Undo remove")
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.save_local(None, true, window, cx)
                        })),
                );
            }
            for card in &editor.session.projection.cards {
                let id = card.id.clone();
                view = view.child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .whitespace_normal()
                                .child(card.title.clone()),
                        )
                        .child(
                            Button::new(SharedString::from(format!("local-manage-edit-{id}")))
                                .ghost()
                                .small()
                                .label("Edit")
                                .disabled(busy)
                                .debug_selector({
                                    let id = id.clone();
                                    move || format!("board-local-manage-edit-{id}")
                                })
                                .on_click(cx.listener({
                                    let id = id.clone();
                                    move |this, _, window, cx| {
                                        this.show_local_form(Mode::Edit(id.clone()), window, cx)
                                    }
                                })),
                        )
                        .child(
                            Button::new(SharedString::from(format!("local-remove-{id}")))
                                .ghost()
                                .small()
                                .label("Remove")
                                .disabled(busy)
                                .debug_selector({
                                    let id = id.clone();
                                    move || format!("board-local-remove-{id}")
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.save_local(
                                        Some(model::Change::Remove(id.clone())),
                                        false,
                                        window,
                                        cx,
                                    )
                                })),
                        ),
                );
            }
        } else {
            view = view
                .child(div().child("Title"))
                .child(
                    div()
                        .debug_selector(|| "board-local-title".into())
                        .child(Input::new(&editor.title).disabled(busy)),
                )
                .child(div().child("Details (optional)"))
                .child(
                    div()
                        .debug_selector(|| "board-local-details".into())
                        .child(Textarea::new(&editor.details).disabled(busy)),
                );
            let mut states = h_flex().gap_2();
            for (index, state) in model::State::ALL.into_iter().enumerate() {
                states = states.child(
                    Button::new(("local-state", index))
                        .ghost()
                        .label(state.label())
                        .selected(editor.state == state)
                        .disabled(busy)
                        .debug_selector(move || format!("board-local-state-{index}"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(editor) = &mut this.local_editor {
                                editor.state = state;
                            }
                            cx.notify();
                        })),
                );
            }
            view = view
                .child(states)
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(colors.text_muted)
                        .child(
                        "Optional manual task state. No issue, repository or executor is required.",
                    ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("local-save")
                                .debug_selector(|| "board-local-save".into())
                                .primary()
                                .label("Save card")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.save_local(None, false, window, cx)
                                })),
                        )
                        .child(
                            Button::new("local-cancel")
                                .debug_selector(|| "board-local-cancel".into())
                                .ghost()
                                .label("Cancel")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.show_local_form(Mode::List, window, cx)
                                })),
                        ),
                );
        }
        view.into_any_element()
    }
}

#[cfg(test)]
mod tests;
