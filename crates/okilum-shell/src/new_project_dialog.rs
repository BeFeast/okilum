//! «New project» (#1475): turn a vault folder into a project. Opened from a
//! folder's «Make this a project…» (the folder is fixed) and from
//! «+ New project…» under the Projects list (the folder is asked for,
//! starting with the folder selected in the tree).
//!
//! The dialog edits an `okilum_t3::projects::create::Draft`: name (the
//! folder's name until edited), kind, icon (T3's «Choose project icon»,
//! opened by the host over this dialog), for code and hybrid where the
//! code is, and «Create the project in T3» (on for code). The host writes
//! the project and closes the dialog: its `on_choice` handler is called
//! with the window the button was pressed in.
use crate::project_avatar;
use gpui::{
    div, prelude::*, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    IntoElement, Render, SharedString, Subscription, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex, ActiveTheme as _, Disableable as _,
};
use okilum_t3::projects::{
    create::{self, Draft, KINDS},
    icon::{self, ProjectIcon},
    Kind,
};
use std::{
    path::{Path, PathBuf},
    rc::Rc,
};

pub const TITLE: &str = "New project";
pub const T3_LABEL: &str = "Create the project in T3";

/// What the dialog's buttons ask the host for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewProjectEvent {
    /// Write the project in `folder` (absolute, inside the vault).
    Create {
        folder: PathBuf,
        draft: Draft,
    },
    /// Open «Choose project icon» over this dialog.
    ChooseIcon,
    Cancel,
}

/// The host's answer to a button.
pub type ChoiceHandler = Rc<dyn Fn(&NewProjectEvent, &mut Window, &mut App)>;

pub struct NewProjectDialog {
    vault: PathBuf,
    folder_fixed: bool,
    draft: Draft,
    /// The name was typed: a folder change no longer renames it.
    name_edited: bool,
    folder: Entity<InputState>,
    name: Entity<InputState>,
    source: Entity<InputState>,
    error: Option<String>,
    busy: bool,
    on_choice: Option<ChoiceHandler>,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<NewProjectEvent> for NewProjectDialog {}
impl Focusable for NewProjectDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.name.read(cx).focus_handle(cx)
    }
}

/// `HomeLab/Mesh/` → `HomeLab/Mesh`; `\` reads as `/`.
fn clean_relative(text: &str) -> String {
    text.trim()
        .replace('\\', "/")
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/")
}

impl NewProjectDialog {
    /// `folder` is vault-relative; `fixed` shows it without an input.
    pub fn new(
        vault: &Path,
        folder: &str,
        fixed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        project_avatar::init(cx);
        let relative = clean_relative(folder);
        let draft = Draft::for_folder(&vault.join(&relative));
        let folder_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("A folder in the vault"));
        folder_input.update(cx, |input, cx| {
            input.set_value(relative.clone(), window, cx)
        });
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Project name"));
        name.update(cx, |input, cx| {
            input.set_value(draft.name.clone(), window, cx)
        });
        let source = cx.new(|cx| {
            InputState::new(window, cx).placeholder("/home/me/src/app or https://git…/app.git")
        });
        let subscriptions = vec![
            cx.subscribe_in(&folder_input, window, |this, input, event, window, cx| {
                if matches!(event, InputEvent::Change) {
                    let relative = clean_relative(&input.read(cx).value());
                    this.folder_changed(&relative, window, cx);
                }
                this.enter(event, window, cx);
            }),
            cx.subscribe_in(&name, window, |this, input, event, window, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    if text != this.draft.name {
                        this.draft.name = text;
                        this.name_edited = true;
                        this.error = None;
                        cx.notify();
                    }
                }
                this.enter(event, window, cx);
            }),
            cx.subscribe_in(&source, window, |this, input, event, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.draft.source = input.read(cx).value().to_string();
                    this.error = None;
                    cx.notify();
                }
                this.enter(event, window, cx);
            }),
        ];
        Self {
            vault: vault.to_owned(),
            folder_fixed: fixed,
            draft,
            name_edited: false,
            folder: folder_input,
            name,
            source,
            error: None,
            busy: false,
            on_choice: None,
            _subscriptions: subscriptions,
        }
    }

    /// Enter in any field is Create project.
    fn enter(&mut self, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(event, InputEvent::PressEnter { .. }) {
            self.create(window, cx);
        }
    }

    /// Called for Create project, Choose icon… and Cancel, after the event
    /// is emitted, in the window of the press.
    pub fn on_choice(
        &mut self,
        handler: impl Fn(&NewProjectEvent, &mut Window, &mut App) + 'static,
    ) {
        self.on_choice = Some(Rc::new(handler));
    }

    /// Emits `event` and hands it to the host's handler, deferred, so the
    /// handler may close the dialog.
    pub fn choose(&mut self, event: NewProjectEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(event.clone());
        if let Some(handler) = self.on_choice.clone() {
            window.defer(cx, move |window, cx| handler(&event, window, cx));
        }
    }

    pub fn draft(&self) -> &Draft {
        &self.draft
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The folder as typed, vault-relative.
    pub fn relative(&self, cx: &App) -> String {
        clean_relative(&self.folder.read(cx).value())
    }

    /// The chosen folder, absolute; `None` while none is typed.
    pub fn folder(&self, cx: &App) -> Option<PathBuf> {
        let relative = self.relative(cx);
        (!relative.is_empty()).then(|| {
            relative
                .split('/')
                .fold(self.vault.clone(), |path, part| path.join(part))
        })
    }

    fn folder_changed(&mut self, relative: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        if !self.name_edited {
            let name = relative.rsplit('/').next().unwrap_or_default().to_owned();
            if name != self.draft.name {
                self.draft.name = name.clone();
                self.name
                    .update(cx, |input, cx| input.set_value(name, window, cx));
            }
        }
        cx.notify();
    }

    pub fn set_kind(&mut self, kind: Kind, cx: &mut Context<Self>) {
        self.draft.set_kind(kind);
        self.error = None;
        cx.notify();
    }

    pub fn set_create_in_t3(&mut self, on: bool, cx: &mut Context<Self>) {
        self.draft.set_create_in_t3(on);
        self.error = None;
        cx.notify();
    }

    /// The icon chosen in «Choose project icon»; `None` is automatic.
    pub fn set_icon(&mut self, icon: Option<ProjectIcon>, cx: &mut Context<Self>) {
        self.draft.icon = icon;
        cx.notify();
    }

    /// The host is writing the project, or failed to: the message shows
    /// under the fields and Create is enabled again.
    pub fn set_busy(&mut self, busy: bool, cx: &mut Context<Self>) {
        self.busy = busy;
        cx.notify();
    }

    pub fn set_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        self.error = error;
        self.busy = false;
        cx.notify();
    }

    /// Why Create cannot go ahead: the folder first, then the draft.
    pub fn problem(&self, cx: &App) -> Option<String> {
        let Some(folder) = self.folder(cx) else {
            return Some("Choose a folder in the vault.".into());
        };
        let relative = self.relative(cx);
        if relative
            .split('/')
            .any(|part| part.starts_with('.') || part == "..")
        {
            return Some(format!("{relative} cannot be a project."));
        }
        if !std::fs::symlink_metadata(&folder).is_ok_and(|m| m.is_dir()) {
            return Some(format!("{relative} is not a folder in this vault."));
        }
        if create::is_project(&folder) {
            return Some(format!("{relative} is already a project."));
        }
        self.draft.problem()
    }

    /// Create project: the host gets the folder and the draft, or the
    /// dialog says what is missing.
    pub fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.busy {
            return false;
        }
        // The fields as shown, whatever events reached the dialog.
        let name = self.name.read(cx).value().to_string();
        if name != self.draft.name {
            self.draft.name = name;
            self.name_edited = true;
        }
        self.draft.source = self.source.read(cx).value().to_string();
        if let Some(problem) = self.problem(cx) {
            self.error = Some(problem);
            cx.notify();
            return false;
        }
        let Some(folder) = self.folder(cx) else {
            return false;
        };
        let mut draft = self.draft.clone();
        draft.name = draft.name.trim().to_owned();
        self.busy = true;
        self.error = None;
        cx.notify();
        self.choose(NewProjectEvent::Create { folder, draft }, window, cx);
        true
    }

    /// The first field to type in: the folder when it is asked for.
    pub fn focus_first(&self, window: &mut Window, cx: &mut Context<Self>) {
        let input = if self.folder_fixed {
            &self.name
        } else {
            &self.folder
        };
        input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn label(text: &'static str, cx: &App) -> impl IntoElement {
        div()
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(cx.theme().muted_foreground)
            .child(text)
    }

    /// Code / Knowledge / Hybrid, the Projects list's segmented control.
    fn render_kinds(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (track, thumb) = (theme.muted, theme.background);
        let (on, off) = (theme.foreground, theme.muted_foreground);
        let current = self.draft.kind;
        h_flex()
            .id("new-project-kinds")
            .debug_selector(|| "new-project-kinds".into())
            .flex_none()
            .p(px(2.))
            .gap(px(2.))
            .rounded(px(7.))
            .bg(track)
            .children(KINDS.into_iter().enumerate().map(|(i, kind)| {
                let selected = current == kind;
                div()
                    .id(("new-project-kind", i))
                    .debug_selector(move || format!("new-project-kind-{i}"))
                    .h(px(24.))
                    .px_3()
                    .flex()
                    .items_center()
                    .rounded(px(5.))
                    .text_size(px(13.))
                    .cursor_pointer()
                    .text_color(if selected { on } else { off })
                    .when(selected, |d| {
                        d.bg(thumb).shadow_sm().font_weight(FontWeight::MEDIUM)
                    })
                    .when(!selected, |d| d.hover(move |d| d.text_color(on)))
                    .child(create::kind_label(kind))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_kind(kind, cx)))
            }))
    }

    fn render_icon(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let logo = self.folder(cx).and_then(|folder| icon::find_logo(&folder));
        let avatar = icon::resolve(self.draft.icon.as_ref(), logo.as_deref(), &self.draft.name);
        h_flex()
            .gap_3()
            .child(
                div()
                    .debug_selector(|| "new-project-icon-preview".into())
                    .child(project_avatar::render(&avatar, 32., cx)),
            )
            .child(
                Button::new("new-project-icon")
                    .debug_selector(|| "new-project-icon".into())
                    .outline()
                    .label("Choose icon…")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.choose(NewProjectEvent::ChooseIcon, window, cx)
                    })),
            )
            .when(self.draft.icon.is_some(), |d| {
                d.child(
                    Button::new("new-project-icon-auto")
                        .debug_selector(|| "new-project-icon-auto".into())
                        .ghost()
                        .label("Automatic")
                        .tooltip("Use the logo or the monogram")
                        .on_click(cx.listener(|this, _, _, cx| this.set_icon(None, cx))),
                )
            })
    }
}

impl Render for NewProjectDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let kind = self.draft.kind;
        let relative: SharedString = self.relative(cx).into();
        let t3_hint = if kind == Kind::Code {
            "T3 opens it in the checkout. The manager can be started later."
        } else {
            "T3 opens it in this folder. The manager can be started later."
        };
        v_flex()
            .id("new-project-dialog")
            .debug_selector(|| "new-project-dialog".into())
            .gap_3()
            .child(v_flex().gap_1().child(Self::label("Folder", cx)).map(|d| {
                if self.folder_fixed {
                    d.child(
                        div()
                            .debug_selector(|| "new-project-folder".into())
                            .text_sm()
                            .child(relative.clone()),
                    )
                } else {
                    d.child(
                        div()
                            .debug_selector(|| "new-project-folder-input".into())
                            .child(Input::new(&self.folder)),
                    )
                }
            }))
            .child(
                v_flex().gap_1().child(Self::label("Name", cx)).child(
                    div()
                        .debug_selector(|| "new-project-name".into())
                        .child(Input::new(&self.name)),
                ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(Self::label("Kind", cx))
                    .child(h_flex().child(self.render_kinds(cx))),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(Self::label("Icon", cx))
                    .child(self.render_icon(cx)),
            )
            .when(create::has_source(kind), |d| {
                d.child(
                    v_flex()
                        .gap_1()
                        .child(Self::label("Checkout or repository", cx))
                        .child(
                            div()
                                .debug_selector(|| "new-project-source".into())
                                .child(Input::new(&self.source)),
                        ),
                )
            })
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Checkbox::new("new-project-t3")
                            .debug_selector(|| "new-project-t3".into())
                            .label(T3_LABEL)
                            .checked(self.draft.create_in_t3())
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.set_create_in_t3(*checked, cx)
                            })),
                    )
                    .child(div().pl_6().text_sm().text_color(muted).child(t3_hint)),
            )
            .when_some(self.error.clone(), |d, error| {
                d.child(
                    div()
                        .debug_selector(|| "new-project-error".into())
                        .text_sm()
                        .text_color(danger)
                        .child(error),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .pt_2()
                    .child(div().flex_1())
                    .child(
                        Button::new("new-project-cancel")
                            .debug_selector(|| "new-project-cancel".into())
                            .outline()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.choose(NewProjectEvent::Cancel, window, cx)
                            })),
                    )
                    .child(
                        Button::new("new-project-create")
                            .debug_selector(|| "new-project-create".into())
                            .primary()
                            .label("Create project")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.create(window, cx);
                            })),
                    ),
            )
    }
}

#[cfg(test)]
mod tests;
