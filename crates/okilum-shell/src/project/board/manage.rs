//! Inline Board management. File work runs off the UI thread; an editing
//! session never adopts a newer revision behind the user's draft.
use super::*;
use gpui_component::input::{Input, InputState};
use okilum_t3::board::manage::{Card, Change, Details, Problems, Session};

#[derive(Clone, Copy)]
enum Screen {
    List,
    Details,
    Add,
    Edit(usize),
}
pub(super) struct Editor {
    session: Session,
    screen: Screen,
    title: Entity<InputState>,
    goal: Entity<InputState>,
    issue: Entity<InputState>,
    expected: Entity<InputState>,
    executor: Entity<InputState>,
    stage_note: String,
    stage: String,
    errors: std::collections::BTreeMap<String, String>,
    busy: bool,
    undo: Option<(okilum_t3::area_files::Document, String)>,
}
impl Editor {
    fn new(session: Session, window: &mut Window, cx: &mut App) -> Self {
        let mut field = |placeholder, cx: &mut App| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let mut editor = Self {
            screen: if session.document.is_none() {
                Screen::Details
            } else {
                Screen::List
            },
            title: field("Board title", cx),
            goal: field("Expected project outcome", cx),
            issue: field("Issue number or Forgejo URL", cx),
            expected: field("Expected result", cx),
            executor: field("Executor", cx),
            stage_note: session.details().stage_note,
            stage: String::new(),
            errors: Default::default(),
            busy: false,
            undo: None,
            session,
        };
        editor.fill_details(window, cx);
        editor
    }
    fn fill_details(&mut self, window: &mut Window, cx: &mut App) {
        let details = self.session.details();
        self.title
            .update(cx, |input, cx| input.set_value(details.title, window, cx));
        self.goal
            .update(cx, |input, cx| input.set_value(details.goal, window, cx));
        self.stage_note = details.stage_note;
    }
    fn show(&mut self, screen: Screen, window: &mut Window, cx: &mut App) {
        self.errors.clear();
        self.screen = screen;
        match screen {
            Screen::Details => self.fill_details(window, cx),
            Screen::Add | Screen::Edit(_) => {
                let card = if let Screen::Edit(i) = screen {
                    self.session.card(i)
                } else {
                    Card::default()
                };
                self.issue
                    .update(cx, |input, cx| input.set_value(card.issue, window, cx));
                self.expected
                    .update(cx, |input, cx| input.set_value(card.expected, window, cx));
                self.executor
                    .update(cx, |input, cx| input.set_value(card.executor, window, cx));
                self.stage = card.stage;
            }
            Screen::List => {}
        }
    }
    fn change(&self, cx: &App) -> Option<Change> {
        Some(match self.screen {
            Screen::Details => Change::Details(Details {
                title: self.title.read(cx).value().to_string(),
                goal: self.goal.read(cx).value().to_string(),
                stage_note: self.stage_note.clone(),
            }),
            Screen::Add | Screen::Edit(_) => {
                let card = Card {
                    issue: self.issue.read(cx).value().to_string(),
                    expected: self.expected.read(cx).value().to_string(),
                    executor: self.executor.read(cx).value().to_string(),
                    stage: self.stage.clone(),
                };
                if let Screen::Edit(i) = self.screen {
                    Change::Edit(i, card)
                } else {
                    Change::Add(card)
                }
            }
            Screen::List => return None,
        })
    }
}
fn note_label(path: &str) -> String {
    path.strip_prefix("planning/")
        .unwrap_or(path)
        .trim_end_matches(".md")
        .replace(['-', '_'], " ")
}
impl Board {
    pub(super) fn open_management(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor_loading {
            return;
        }
        self.editor_loading = true;
        let area = self.area.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { Session::load(&area) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.editor_loading = false;
                match result {
                    Ok(session) => this.editor = Some(Editor::new(session, window, cx)),
                    Err(error) => {
                        eprintln!("Board management: {error:#}");
                        reader_toast::error(format!("Cannot manage Board: {error:#}"), window, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn management_screen(&mut self, screen: Screen, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.editor {
            if !editor.busy {
                editor.show(screen, window, cx);
            }
        }
        cx.notify();
    }
    fn save_management(
        &mut self,
        change: Option<Change>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        if editor.busy {
            return;
        }
        let Some(change) = change.or_else(|| editor.change(cx)) else {
            return;
        };
        // Validation, path checks and file publication all stay off the UI thread.
        editor.busy = true;
        editor.errors.clear();
        let session = editor.session.clone();
        let removed = matches!(change, Change::Remove(_));
        let previous = session.document.as_ref().map(|d| d.source.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { session.save(&change) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(editor) = &mut this.editor {
                    editor.busy = false;
                    match result {
                        Ok(session) => {
                            editor.undo = if removed {
                                session.document.clone().zip(previous)
                            } else {
                                None
                            };
                            editor.session = session;
                            editor.screen = Screen::List;
                            editor.errors.clear();
                            reader_toast::transient(
                                if removed {
                                    "Card removed. Undo is available in Board management."
                                } else {
                                    "Board saved"
                                },
                                window,
                                cx,
                            );
                        }
                        Err(error) => {
                            eprintln!("Board write refused: {error:#}");
                            editor.errors = error
                                .downcast_ref::<Problems>()
                                .map(|p| p.0.clone())
                                .unwrap_or_else(|| {
                                    std::collections::BTreeMap::from([(
                                        "file".into(),
                                        format!("{error:#}"),
                                    )])
                                });
                        }
                    }
                }
                this.reload_binding(window, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn undo_management(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        if editor.busy {
            return;
        }
        let Some((document, source)) = editor.undo.clone() else {
            return;
        };
        editor.busy = true;
        let area = self.area.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    document.replace(&source)?;
                    Session::load(&area)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(editor) = &mut this.editor {
                    editor.busy = false;
                    match result {
                        Ok(session) => {
                            editor.session = session;
                            editor.undo = None;
                            editor.errors.clear();
                        }
                        Err(error) => {
                            editor.errors.insert("file".into(), format!("{error:#}"));
                        }
                    }
                }
                this.reload_binding(window, cx);
                cx.notify();
            });
        })
        .detach();
    }
    fn management_field(
        &self,
        key: &str,
        label: &str,
        input: &Entity<InputState>,
        cx: &App,
    ) -> AnyElement {
        let colors = brand::palette(cx);
        let mut row = v_flex()
            .gap_1()
            .w_full()
            .child(div().text_size(px(13.)).child(label.to_owned()))
            .child(Input::new(input).disabled(self.editor.as_ref().is_some_and(|e| e.busy)));
        if let Some(error) = self.editor.as_ref().and_then(|e| e.errors.get(key)) {
            row = row.child(
                div()
                    .debug_selector({
                        let key = key.to_owned();
                        move || format!("board-field-error-{key}")
                    })
                    .text_size(px(12.))
                    .text_color(colors.danger)
                    .child(error.clone()),
            );
        }
        row.into_any_element()
    }
    pub(super) fn render_management(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        let colors = brand::palette(cx);
        let editor = self.editor.as_ref().unwrap();
        let busy = editor.busy;
        let creating = editor.session.document.is_none();
        let weak = cx.entity().downgrade();
        let mut content =
            v_flex()
                .w_full()
                .max_w(px(680.))
                .gap_4()
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(
                            if creating {
                                "Create board"
                            } else {
                                "Manage board"
                            },
                        ))
                        .child(div().flex_1())
                        .child(
                            Button::new("board-management-close")
                                .ghost()
                                .small()
                                .disabled(busy)
                                .child(appearance::icon("x", colors.text_muted))
                                .tooltip("Back to Board")
                                .accessibility_label("Back to Board")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.editor = None;
                                    this.reload_binding(window, cx);
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    div()
                        .debug_selector(|| "board-file-format".into())
                        .text_size(px(12.))
                        .text_color(colors.text_muted)
                        .child(editor.session.format_label()),
                );
        if let Some(error) = editor.errors.get("file") {
            content = content.child(
                div()
                    .debug_selector(|| "board-write-error".into())
                    .text_size(px(12.))
                    .text_color(colors.danger)
                    .child(error.clone()),
            );
        }
        match editor.screen {
            Screen::List => {
                content = content.child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("board-add")
                                .debug_selector(|| "board-add".into())
                                .primary()
                                .label("Add card")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.management_screen(Screen::Add, window, cx)
                                })),
                        )
                        .child(
                            Button::new("board-details")
                                .ghost()
                                .small()
                                .child(appearance::icon("settings-2", colors.text_muted))
                                .tooltip("Board details")
                                .accessibility_label("Board details")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.management_screen(Screen::Details, window, cx)
                                })),
                        ),
                );
                if editor.undo.is_some() {
                    content = content.child(
                        Button::new("board-remove-undo")
                            .debug_selector(|| "board-remove-undo".into())
                            .ghost()
                            .label("Undo remove")
                            .disabled(busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.undo_management(window, cx)),
                            ),
                    );
                }
                if editor.session.cards().is_empty() {
                    content = content.child(
                        div()
                            .text_color(colors.text_muted)
                            .child("Add an issue to start planning this project."),
                    );
                }
                for (index, card) in editor.session.cards().iter().enumerate() {
                    let number = card["number"]
                        .as_u64()
                        .map(|n| format!("#{n}"))
                        .unwrap_or_else(|| "Card needs attention".into());
                    content = content.child(
                        h_flex()
                            .debug_selector(move || format!("board-manage-card-{index}"))
                            .gap_2()
                            .py_2()
                            .child(
                                v_flex().flex_1().min_w_0().child(number).child(
                                    div()
                                        .text_size(px(12.))
                                        .text_color(colors.text_muted)
                                        .child(
                                            card["expected"]
                                                .as_str()
                                                .unwrap_or("Expected result is missing")
                                                .to_owned(),
                                        ),
                                ),
                            )
                            .child(
                                Button::new(("board-edit", index))
                                    .debug_selector(move || format!("board-edit-{index}"))
                                    .ghost()
                                    .small()
                                    .disabled(busy)
                                    .child(appearance::icon("pencil", colors.text_muted))
                                    .tooltip("Edit card")
                                    .accessibility_label("Edit card")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.management_screen(Screen::Edit(index), window, cx)
                                    })),
                            )
                            .child(
                                Button::new(("board-remove", index))
                                    .debug_selector(move || format!("board-remove-{index}"))
                                    .ghost()
                                    .small()
                                    .disabled(busy)
                                    .child(appearance::icon("trash-2", colors.text_muted))
                                    .tooltip("Remove card")
                                    .accessibility_label("Remove card")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.save_management(
                                            Some(Change::Remove(index)),
                                            window,
                                            cx,
                                        )
                                    })),
                            ),
                    );
                }
            }
            Screen::Details => {
                content = content
                    .child(self.management_field("title", "Title", &editor.title, cx))
                    .child(self.management_field("goal", "Goal", &editor.goal, cx));
                let selected = editor.stage_note.clone();
                let mut notes = editor.session.stage_notes.clone();
                if !selected.is_empty() && !notes.contains(&selected) {
                    notes.push(selected.clone());
                }
                let selector = Button::new("board-stage-note")
                    .debug_selector(|| "board-stage-note".into())
                    .label(if selected.is_empty() {
                        "Choose a stage note".into()
                    } else {
                        note_label(&selected)
                    })
                    .disabled(busy)
                    .dropdown_menu(move |mut menu, _, _| {
                        for note in &notes {
                            let target = weak.clone();
                            let note = note.clone();
                            menu = menu.item(
                                PopupMenuItem::new(note_label(&note))
                                    .checked(note == selected)
                                    .on_click(move |_, _, cx| {
                                        let _ = target.update(cx, |this, cx| {
                                            if let Some(e) = &mut this.editor {
                                                e.stage_note = note.clone();
                                                e.errors.remove("stage_note");
                                            }
                                            cx.notify();
                                        });
                                    }),
                            );
                        }
                        menu
                    });
                let mut note_field = v_flex()
                    .gap_1()
                    .child(div().text_size(px(13.)).child("Stage note"))
                    .child(selector);
                if let Some(error) = editor.errors.get("stage_note") {
                    note_field = note_field.child(
                        div()
                            .debug_selector(|| "board-field-error-stage_note".into())
                            .text_size(px(12.))
                            .text_color(colors.danger)
                            .child(error.clone()),
                    );
                }
                if notes_empty(editor) {
                    note_field = note_field.child(
                        div()
                            .text_size(px(12.))
                            .text_color(colors.text_muted)
                            .child(
                                "Add a Markdown note to Planning, then reopen Board management.",
                            ),
                    );
                }
                content = content.child(note_field);
                if let Some(error) = editor.errors.get("repository") {
                    content = content.child(
                        div()
                            .text_size(px(12.))
                            .text_color(colors.danger)
                            .child(error.clone()),
                    );
                }
                content = content.child(self.management_save(creating, busy, cx));
            }
            Screen::Add | Screen::Edit(_) => {
                content = content
                    .child(self.management_field("issue", "Issue", &editor.issue, cx))
                    .child(self.management_field(
                        "expected",
                        "Expected result",
                        &editor.expected,
                        cx,
                    ))
                    .child(self.management_field("executor", "Executor", &editor.executor, cx));
                let mut stages =
                    gpui_component::button::ButtonGroup::new("board-stage").disabled(busy);
                for (index, (value, label)) in [
                    ("", "Automatic"),
                    ("ready", "Ready"),
                    ("verified", "Verified"),
                ]
                .into_iter()
                .enumerate()
                {
                    stages = stages.child(
                        Button::new(("board-stage", index))
                            .debug_selector(move || format!("board-stage-{index}"))
                            .small()
                            .selected(editor.stage == value)
                            .label(label)
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(e) = &mut this.editor {
                                    e.stage = value.into();
                                }
                                cx.notify();
                            })),
                    );
                }
                content = content.child(
                    v_flex()
                        .gap_1()
                        .child(div().text_size(px(13.)).child("Stage"))
                        .child(stages),
                );
                if let Some(error) = editor.errors.get("stage") {
                    content = content.child(
                        div()
                            .text_size(px(12.))
                            .text_color(colors.danger)
                            .child(error.clone()),
                    );
                }
                content = content.child(self.management_save(false, busy, cx));
            }
        }
        v_flex()
            .id("board-management")
            .debug_selector(|| "board-management".into())
            .size_full()
            .overflow_y_scroll()
            .p_4()
            .bg(colors.canvas)
            .text_color(colors.text)
            .items_center()
            .child(content)
            .into_any_element()
    }
    fn management_save(&self, creating: bool, busy: bool, cx: &Context<Self>) -> AnyElement {
        h_flex()
            .gap_2()
            .child(
                Button::new("board-save")
                    .debug_selector(|| "board-save".into())
                    .primary()
                    .label(if creating { "Create board" } else { "Save" })
                    .disabled(busy)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.save_management(None, window, cx)),
                    ),
            )
            .child(
                Button::new("board-edit-cancel")
                    .ghost()
                    .label("Cancel")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this
                            .editor
                            .as_ref()
                            .is_some_and(|e| e.session.document.is_none())
                        {
                            this.editor = None;
                            cx.notify();
                        } else {
                            this.management_screen(Screen::List, window, cx);
                        }
                    })),
            )
            .into_any_element()
    }
}
fn notes_empty(editor: &Editor) -> bool {
    editor.session.stage_notes.is_empty() && editor.stage_note.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn empty_board_create_add_validate_edit_remove_and_undo_use_the_real_controls(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_reduce_motion(true);
        });
        let area = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(area.path().join("ops")).unwrap();
        std::fs::create_dir(area.path().join("planning")).unwrap();
        std::fs::write(area.path().join("planning/stage.md"), "# Stage").unwrap();
        let project="name: Example\nproduct: {outcome: A useful outcome}\nrepo: {host: git.oklabs.uk, full_name: BeFeast/example}\n";
        std::fs::write(area.path().join("ops/project.yaml"), project).unwrap();
        let (root, visual) = cx.add_window_view(|window, cx| {
            let board = cx.new(|cx| {
                let mut board = Board::with_reader(
                    area.path().into(),
                    |binding| {
                        binding
                            .issues
                            .iter()
                            .map(|scope| {
                                (
                                    scope.number,
                                    Ok(okilum_t3::board::Issue {
                                        number: scope.number,
                                        title: format!("Fixture issue {}", scope.number),
                                        state: "open".into(),
                                        body: String::new(),
                                        labels: Vec::new(),
                                        open_pull_requests: Vec::new(),
                                    }),
                                )
                            })
                            .collect()
                    },
                    window,
                    cx,
                );
                board.active = true;
                board
            });
            Root::new(board, window, cx)
        });
        let board = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<Board>().unwrap()
        });
        visual.run_until_parked();
        for theme in [
            gpui_component::ThemeMode::Light,
            gpui_component::ThemeMode::Dark,
        ] {
            visual.update(|_, cx| {
                Theme::change(theme, None, cx);
                brand::apply_theme(cx);
            });
            visual.run_until_parked();
            assert!(visual.debug_bounds("board-empty").is_some());
            assert!(visual.debug_bounds("board-create").is_some());
        }
        let click = |visual: &mut VisualTestContext, selector: &'static str| {
            let bounds = visual
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("Missing {selector}"));
            visual.simulate_click(bounds.center(), Modifiers::default());
            visual.run_until_parked();
        };
        click(visual, "board-create");
        assert!(visual.debug_bounds("board-management").is_some());
        board.read_with(visual, |board, _| {
            assert_eq!(
                board.editor.as_ref().unwrap().session.format_label(),
                "New board.toml · TOML"
            )
        });
        click(visual, "board-save");
        assert!(area.path().join("ops/board.toml").exists());
        click(visual, "board-add");
        click(visual, "board-save");
        assert!(visual.debug_bounds("board-field-error-issue").is_some());
        assert!(visual.debug_bounds("board-field-error-expected").is_some());
        assert!(visual.debug_bounds("board-field-error-executor").is_some());
        board.update_in(visual, |board, window, cx| {
            let editor = board.editor.as_mut().unwrap();
            editor
                .issue
                .update(cx, |input, cx| input.set_value("#42", window, cx));
            editor.expected.update(cx, |input, cx| {
                input.set_value("The expected result", window, cx)
            });
            editor
                .executor
                .update(cx, |input, cx| input.set_value("Editing", window, cx));
        });
        click(visual, "board-stage-1");
        click(visual, "board-save");
        assert!(visual.debug_bounds("board-manage-card-0").is_some());
        assert_eq!(
            Binding::read(area.path()).unwrap().unwrap().issues[0].stage,
            Some(okilum_t3::board::HumanStage::Ready)
        );
        click(visual, "board-edit-0");
        board.update_in(visual, |board, window, cx| {
            board
                .editor
                .as_mut()
                .unwrap()
                .expected
                .update(cx, |input, cx| {
                    input.set_value("Edited expected result", window, cx)
                })
        });
        click(visual, "board-save");
        let binding = Binding::read(area.path()).unwrap().unwrap();
        assert_eq!(binding.issues[0].expected, "Edited expected result");
        click(visual, "board-remove-0");
        assert!(Binding::read(area.path())
            .unwrap()
            .unwrap()
            .issues
            .is_empty());
        click(visual, "board-remove-undo");
        assert_eq!(
            Binding::read(area.path()).unwrap().unwrap().issues[0].number,
            42
        );
        assert_eq!(
            std::fs::read_to_string(area.path().join("ops/project.yaml")).unwrap(),
            project
        );
        assert!(!area.path().join("ops/board.yaml").exists());
        // A human edits the selected file while the form is open: retain draft,
        // refuse to overwrite, and make the reason visible beside the controls.
        click(visual, "board-edit-0");
        let path = area.path().join("ops/board.toml");
        let external = std::fs::read_to_string(&path).unwrap() + "\n# Human update\n";
        std::fs::write(&path, &external).unwrap();
        board.update_in(visual, |board, window, cx| {
            board
                .editor
                .as_mut()
                .unwrap()
                .expected
                .update(cx, |input, cx| input.set_value("Stale edit", window, cx))
        });
        click(visual, "board-save");
        assert_eq!(std::fs::read_to_string(path).unwrap(), external);
        assert!(visual.debug_bounds("board-write-error").is_some());
        board.read_with(visual, |board, cx| {
            assert_eq!(
                board
                    .editor
                    .as_ref()
                    .unwrap()
                    .expected
                    .read(cx)
                    .value()
                    .as_str(),
                "Stale edit"
            )
        });
        click(visual, "board-management-close");
        assert!(visual.debug_bounds("board-empty").is_none());
        assert!(visual.debug_bounds("board-expand-42").is_some());
        assert!(visual.debug_bounds("board-manage").is_some());
    }
}
