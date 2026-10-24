//! «Choose project icon» (#1627), copied from T3 Code's dialog: Icons /
//! Emoji / Monogram, T3's eighteen colours, a Lucide search, Cancel / Save
//! icon, and Reset to automatic. The state is `IconDraft`; this view only
//! draws it. The host stores what is chosen and closes the dialog: its
//! `on_choice` handler is called with the window the button was pressed
//! in, so the choice never depends on which window last drew the host.
use crate::project_avatar;
use gpui::{
    div, prelude::*, px, rgb, App, Context, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, IntoElement, Render, SharedString, Subscription, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    tooltip::Tooltip,
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
};
use okilum_t3::projects::icon::{
    filter_icons, Avatar, IconColor, IconDraft, PickerTab, ProjectIcon, PROJECT_EMOJIS,
};
use std::rc::Rc;

/// The host's answer to a button: store the icon, reset it, or just close.
pub type ChoiceHandler = Rc<dyn Fn(&IconDialogEvent, &mut Window, &mut App)>;

pub const TITLE: &str = "Choose project icon";
pub const SUBTITLE: &str = "Choose an icon, emoji, or monogram.";
const CELL: f32 = 36.;
const GRID_HEIGHT: f32 = 236.;

/// What the dialog's buttons ask the host for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IconDialogEvent {
    Save(ProjectIcon),
    /// Back to the logo or monogram: the manual icon is removed.
    Reset,
    Cancel,
}

pub struct ProjectIconDialog {
    draft: IconDraft,
    project_name: String,
    has_manual: bool,
    available: Vec<String>,
    search: Entity<InputState>,
    paste: Entity<InputState>,
    letters: Entity<InputState>,
    on_choice: Option<ChoiceHandler>,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<IconDialogEvent> for ProjectIconDialog {}
impl Focusable for ProjectIconDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search.read(cx).focus_handle(cx)
    }
}

impl ProjectIconDialog {
    pub fn new(
        project_name: &str,
        current: Option<&ProjectIcon>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        project_avatar::init(cx);
        let draft = IconDraft::new(current, project_name);
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search all Lucide icons"));
        let paste = cx.new(|cx| InputState::new(window, cx).placeholder("Paste an emoji"));
        let letters = cx.new(|cx| InputState::new(window, cx).placeholder("1–2 letters"));
        letters.update(cx, |input, cx| {
            input.set_value(draft.monogram.clone(), window, cx)
        });
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |_, _, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&paste, window, |this, input, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    if this.draft.paste_emoji(&text) {
                        cx.notify();
                    }
                }
            }),
            cx.subscribe_in(&letters, window, |this, input, event, window, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    this.draft.set_monogram(&text);
                    if this.draft.monogram != text {
                        let kept = this.draft.monogram.clone();
                        input.update(cx, |input, cx| input.set_value(kept, window, cx));
                    }
                    cx.notify();
                }
            }),
        ];
        Self {
            draft,
            project_name: project_name.to_owned(),
            has_manual: current.is_some(),
            available: project_avatar::lucide_names(cx).iter().cloned().collect(),
            search,
            paste,
            letters,
            on_choice: None,
            _subscriptions: subscriptions,
        }
    }

    /// Called for Save icon, Reset to automatic and Cancel, after the
    /// event is emitted, in the window of the press.
    pub fn on_choice(
        &mut self,
        handler: impl Fn(&IconDialogEvent, &mut Window, &mut App) + 'static,
    ) {
        self.on_choice = Some(Rc::new(handler));
    }

    /// Emits `event` and hands it to the host's handler. The handler runs
    /// deferred, outside this view's update, so it may close the dialog.
    pub fn choose(&mut self, event: IconDialogEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(event.clone());
        if let Some(handler) = self.on_choice.clone() {
            window.defer(cx, move |window, cx| handler(&event, window, cx));
        }
    }

    pub fn draft(&self) -> &IconDraft {
        &self.draft
    }

    pub fn set_tab(&mut self, tab: PickerTab, window: &mut Window, cx: &mut Context<Self>) {
        self.draft.tab = tab;
        match tab {
            PickerTab::Icons => self.search.update(cx, |i, cx| i.focus(window, cx)),
            PickerTab::Emoji => self.paste.update(cx, |i, cx| i.focus(window, cx)),
            PickerTab::Monogram => self.letters.update(cx, |i, cx| i.focus(window, cx)),
        }
        cx.notify();
    }

    pub fn set_color(&mut self, color: IconColor, cx: &mut Context<Self>) {
        self.draft.color = color;
        cx.notify();
    }

    pub fn select_icon(&mut self, name: &str, cx: &mut Context<Self>) {
        self.draft.lucide = name.to_owned();
        cx.notify();
    }

    pub fn select_emoji(&mut self, emoji: &str, cx: &mut Context<Self>) {
        self.draft.emoji = emoji.to_owned();
        cx.notify();
    }

    /// Save icon: chooses the draft's icon; nothing while it is not usable.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match self.draft.result() {
            Some(icon) => {
                self.choose(IconDialogEvent::Save(icon), window, cx);
                true
            }
            None => false,
        }
    }

    /// The icons the grid shows for the current search.
    pub fn visible_icons(&self, cx: &App) -> Vec<String> {
        filter_icons(&self.available, &self.search.read(cx).value())
    }

    fn preview(&self) -> Avatar {
        match self.draft.result() {
            Some(icon) => Avatar::Icon(icon),
            None => Avatar::Monogram {
                text: okilum_t3::projects::icon::monogram(&self.project_name),
                color: self.draft.color,
            },
        }
    }

    /// Icons / Emoji / Monogram, the Projects list's segmented control.
    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (track, thumb) = (theme.muted, theme.background);
        let (on, off) = (theme.foreground, theme.muted_foreground);
        let current = self.draft.tab;
        h_flex()
            .id("icon-tabs")
            .debug_selector(|| "icon-tabs".into())
            .flex_none()
            .p(px(2.))
            .gap(px(2.))
            .rounded(px(7.))
            .bg(track)
            .children(PickerTab::ALL.into_iter().enumerate().map(|(i, tab)| {
                let selected = current == tab;
                div()
                    .id(("icon-tab", i))
                    .debug_selector(move || format!("icon-tab-{i}"))
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
                    .child(tab.label())
                    .on_click(cx.listener(move |this, _, window, cx| this.set_tab(tab, window, cx)))
            }))
    }

    /// T3's eighteen swatches; the selected one is ringed.
    fn render_colors(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ring = cx.theme().foreground.opacity(0.64);
        let current = self.draft.color;
        v_flex()
            .gap_2()
            .child(
                div()
                    .text_size(px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(cx.theme().muted_foreground)
                    .child("Color"),
            )
            .child(
                h_flex()
                    .id("icon-colors")
                    .debug_selector(|| "icon-colors".into())
                    .flex_wrap()
                    .gap(px(6.))
                    .children(IconColor::ALL.into_iter().enumerate().map(|(i, color)| {
                        let label: SharedString = color.label().into();
                        div()
                            .id(("icon-color", i))
                            .debug_selector(move || format!("icon-color-{i}"))
                            .size(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .border_1()
                            .border_color(if current == color {
                                ring
                            } else {
                                gpui::transparent_black()
                            })
                            .cursor_pointer()
                            .child(div().size(px(16.)).rounded_full().bg(rgb(color.swatch())))
                            .tooltip(move |window, cx| {
                                Tooltip::new(label.clone()).build(window, cx)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| this.set_color(color, cx)))
                    })),
            )
    }

    fn cell(id: (&'static str, usize), selected: bool, cx: &App) -> gpui::Stateful<gpui::Div> {
        let theme = cx.theme();
        let hover = theme.accent;
        div()
            .id(id)
            .size(px(CELL))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .border_1()
            .border_color(if selected {
                theme.border
            } else {
                gpui::transparent_black()
            })
            .when(selected, |d| d.bg(theme.accent))
            .cursor_pointer()
            .hover(move |d| d.bg(hover))
    }

    fn render_icons(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let names = self.visible_icons(cx);
        let glyph = rgb(self.draft.color.glyph(cx.theme().is_dark()));
        let muted = cx.theme().muted_foreground;
        v_flex()
            .gap_2()
            .child(
                Input::new(&self.search)
                    .cleanable(true)
                    .prefix(Icon::new(IconName::Search).small()),
            )
            .child(
                h_flex()
                    .id("icon-grid")
                    .debug_selector(|| "icon-grid".into())
                    .flex_wrap()
                    .gap_1()
                    .max_h(px(GRID_HEIGHT))
                    .overflow_y_scroll()
                    .when(names.is_empty(), |d| {
                        d.child(
                            div()
                                .w_full()
                                .py_8()
                                .flex()
                                .justify_center()
                                .text_sm()
                                .text_color(muted)
                                .child("No icons found."),
                        )
                    })
                    .children(names.into_iter().enumerate().map(|(i, name)| {
                        let selected = self.draft.lucide == name;
                        let label: SharedString = name
                            .split('-')
                            .map(|part| {
                                let mut chars = part.chars();
                                chars
                                    .next()
                                    .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                                    .unwrap_or_default()
                            })
                            .collect::<Vec<_>>()
                            .join(" ")
                            .into();
                        Self::cell(("icon-choice", i), selected, cx)
                            .debug_selector(move || format!("icon-choice-{i}"))
                            .child(
                                project_avatar::lucide(&name, cx)
                                    .size(px(20.))
                                    .text_color(glyph),
                            )
                            .tooltip(move |window, cx| {
                                Tooltip::new(label.clone()).build(window, cx)
                            })
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.select_icon(&name, cx)),
                            )
                    })),
            )
    }

    fn render_emoji(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .id("emoji-grid")
                    .debug_selector(|| "emoji-grid".into())
                    .flex_wrap()
                    .gap_1()
                    .max_h(px(GRID_HEIGHT))
                    .overflow_y_scroll()
                    .children(
                        PROJECT_EMOJIS
                            .into_iter()
                            .enumerate()
                            .map(|(i, (emoji, label))| {
                                let selected = self.draft.emoji == emoji;
                                Self::cell(("emoji-choice", i), selected, cx)
                                    .debug_selector(move || format!("emoji-choice-{i}"))
                                    .text_size(px(20.))
                                    .child(emoji)
                                    .tooltip(move |window, cx| {
                                        Tooltip::new(label).build(window, cx)
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.select_emoji(emoji, cx)
                                    }))
                            }),
                    ),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(muted)
                    .child("Or paste any emoji"),
            )
            .child(Input::new(&self.paste))
    }

    fn render_monogram(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .gap_2()
            .child(
                div()
                    .text_size(px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(muted)
                    .child("Letters"),
            )
            .child(
                div()
                    .debug_selector(|| "icon-monogram-input".into())
                    .w(px(120.))
                    .child(Input::new(&self.letters)),
            )
    }
}

impl Render for ProjectIconDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let tab = self.draft.tab;
        let can_save = self.draft.result().is_some();
        let preview = project_avatar::render(&self.preview(), 32., cx);
        v_flex()
            .id("project-icon-dialog")
            .debug_selector(|| "project-icon-dialog".into())
            .gap_4()
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .debug_selector(|| "icon-preview".into())
                            .child(preview),
                    )
                    .child(div().text_sm().text_color(muted).child(SUBTITLE)),
            )
            .child(self.render_tabs(cx))
            .when(tab != PickerTab::Emoji, |d| d.child(self.render_colors(cx)))
            .map(|d| match tab {
                PickerTab::Icons => d.child(self.render_icons(cx)),
                PickerTab::Emoji => d.child(self.render_emoji(cx)),
                PickerTab::Monogram => d.child(self.render_monogram(cx)),
            })
            .child(
                h_flex()
                    .gap_2()
                    .pt_2()
                    .child(
                        Button::new("icon-reset")
                            .debug_selector(|| "icon-reset".into())
                            .ghost()
                            .label("Reset to automatic")
                            .disabled(!self.has_manual)
                            .tooltip("Use the logo or the monogram")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.choose(IconDialogEvent::Reset, window, cx)
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("icon-cancel")
                            .debug_selector(|| "icon-cancel".into())
                            .outline()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.choose(IconDialogEvent::Cancel, window, cx)
                            })),
                    )
                    .child(
                        Button::new("icon-save")
                            .debug_selector(|| "icon-save".into())
                            .primary()
                            .label("Save icon")
                            .disabled(!can_save)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save(window, cx);
                            })),
                    ),
            )
    }
}

#[cfg(test)]
mod tests;
