//! Native composer invalidates its own controls, never the history entity.
use super::*;
use crate::project::design;
use gpui_component::{button::ButtonCustomVariant, ThemeStyled as _};
pub(super) mod history;
use super::message_types::{
    corrected_message_with_author, filtered_messages, DraftMetadata, TypedDraft,
};
use okilum_t3::message_types::{
    DecisionMeaning, Message as TypedMessage, MessageKey, MessageType, Metadata as TypeMetadata,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TypedScope {
    pub environment: String,
    pub project: String,
    pub thread: String,
    /// Visible composer lease; never persisted or transported.
    pub generation: u64,
}
/// Parent saves this local payload with text/attachments under the same durable
/// conversation identity and revision. It must never enter a T3 wire request.
pub(super) struct TypedDraftChanged {
    pub scope: TypedScope,
    pub text: String,
    pub metadata: DraftMetadata,
}
impl EventEmitter<TypedDraftChanged> for Composer {}
/// Full expected source for revision-aware correction. Parent persists first,
/// then supplies the acknowledged projection through set_typed_messages.
pub(super) struct RetypeRequested {
    pub scope: TypedScope,
    pub expected: TypedMessage,
    pub replacement: TypedMessage,
}
impl EventEmitter<RetypeRequested> for Composer {}
pub(super) struct TypedMessageOpened(pub TypedScope, pub TypedMessage);
impl EventEmitter<TypedMessageOpened> for Composer {}
pub(super) struct TypedReloadRequested(pub TypedScope);
impl EventEmitter<TypedReloadRequested> for Composer {}
pub(super) struct TypedPanelChanged;
impl EventEmitter<TypedPanelChanged> for Composer {}

#[cfg(test)]
mod typed_1478_tests;

fn type_icon(kind: MessageType) -> IconName {
    match kind {
        MessageType::Chat => IconName::Bot,
        MessageType::FeatureRequest => IconName::Star,
        MessageType::Bug => IconName::TriangleAlert,
        MessageType::Decision => IconName::Replace,
        MessageType::Status => IconName::Info,
    }
}
/// Parent places this slot next to the chat feed, observing TypedPanelChanged.
#[derive(IntoElement)]
pub(super) struct TypedPanelSlot(pub Entity<Composer>);
impl RenderOnce for TypedPanelSlot {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        self.0
            .update(cx, |composer, cx| composer.render_typed_panel(cx))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Auto,
    Queue,
    Steer,
}
impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Auto => "Send",
            Self::Queue => "Queue",
            Self::Steer => "Steer",
        }
    }
}

#[derive(Clone, Copy)]
struct SendPresentation {
    enabled: bool,
    background: Hsla,
    foreground: Hsla,
    accessible_label: &'static str,
    tooltip: &'static str,
}

/// Keep explicit colors at the button instance and glyph: the toolkit's
/// disabled primary variant otherwise fades its own foreground independently
/// of an accent-filled ancestor.
fn send_presentation(mode: Mode, enabled: bool, palette: brand::Palette) -> SendPresentation {
    let (background, foreground) = if enabled {
        (palette.accent, palette.on_accent)
    } else {
        (palette.surface_raised, palette.text_muted)
    };
    let (accessible_label, tooltip) = match mode {
        Mode::Auto => (
            "Send message",
            "Send message (Enter) · New line (Shift+Enter)",
        ),
        Mode::Queue => (
            "Queue message",
            "Queue after current turn (Enter) · New line (Shift+Enter)",
        ),
        Mode::Steer => (
            "Steer current turn",
            "Steer current turn (Enter) · New line (Shift+Enter)",
        ),
    };
    SendPresentation {
        enabled,
        background,
        foreground,
        accessible_label,
        tooltip,
    }
}

pub(super) struct SendRequested;
impl EventEmitter<SendRequested> for Composer {}
pub(super) struct HistoryRecalled;
impl EventEmitter<HistoryRecalled> for Composer {}
pub(super) enum AttachmentRequested {
    Choose,
    Paths(Vec<PathBuf>),
    Paste,
}
impl EventEmitter<AttachmentRequested> for Composer {}

/// Build the parent's model controls during this component's layout, after
/// Composer::render has returned. Large debug render frames must not nest.
#[derive(IntoElement)]
struct ModelControlsSlot(WeakEntity<ProjectView>);
impl RenderOnce for ModelControlsSlot {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        self.0
            .update(cx, |view, cx| view.render_model_controls(window, cx))
            .unwrap_or_else(|_| div().into_any_element())
    }
}

pub(super) struct Composer {
    parent: Option<WeakEntity<ProjectView>>,
    _parent: Option<Subscription>,
    input: Entity<TextareaState>,
    enabled: bool,
    pub(super) mode: Mode,
    mode_open: bool,
    mode_focus: gpui::FocusHandle,
    mode_trigger_bounds: std::rc::Rc<std::cell::Cell<gpui::Bounds<Pixels>>>,
    native: bool,
    steer: bool,
    busy: bool,
    has_attachments: bool,
    can_attach: bool,
    history_feed: Option<Entity<feed::Feed>>,
    history_position: Option<history::Position>,
    history_blocked: bool,
    _input: Subscription,
    _typed_input: Option<Subscription>,
    typed_scope: Option<TypedScope>,
    typed_draft: TypedDraft,
    typed_messages: Vec<TypedMessage>,
    typed_authors: std::collections::HashMap<MessageKey, okilum_t3::message_types::Author>,
    typed_filter: Option<MessageType>,
    typed_panel_open: bool,
    typed_error: Option<String>,
    typed_edit_ready: bool,
}
impl Composer {
    pub(super) fn new(input: Entity<TextareaState>, cx: &mut Context<Self>) -> Self {
        // Match Reader's IME commit settle policy: some input methods deliver
        // the candidate-confirming Enter after the marked range is cleared.
        input.update(cx, |input, _| {
            input.set_ime_settle(Duration::from_millis(500))
        });
        let subscription = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { shift: false, .. })
                && this.enabled
                && !this.busy
                && this.prompt_validation(cx).is_none()
                && !this.input.read(cx).ime_settling(cx)
                && (this.has_attachments || !this.input.read(cx).value().trim().is_empty())
            {
                cx.emit(SendRequested);
            }
            cx.notify();
        });
        Self {
            parent: None,
            _parent: None,
            input,
            enabled: false,
            mode: Mode::Auto,
            mode_open: false,
            mode_focus: cx.focus_handle(),
            mode_trigger_bounds: Default::default(),
            native: false,
            steer: false,
            busy: false,
            has_attachments: false,
            can_attach: false,
            history_feed: None,
            history_position: None,
            history_blocked: false,
            _input: subscription,
            _typed_input: None,
            typed_scope: None,
            typed_draft: TypedDraft::default(),
            typed_messages: Vec::new(),
            typed_authors: Default::default(),
            typed_filter: None,
            typed_panel_open: false,
            typed_error: None,
            typed_edit_ready: false,
        }
    }
    /// Invoke atomically on conversation activation/restoration, before editing
    /// or sending. Legacy drafts use None. A malformed draft leaves current
    /// state intact so the parent can surface recovery rather than overwrite it.
    pub(super) fn restore_typed_context(
        &mut self,
        scope: TypedScope,
        text: String,
        retained: Option<DraftMetadata>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let draft = match retained {
            Some(retained) => TypedDraft::restore_metadata(text, retained)?,
            None => TypedDraft::from_text(text),
        };
        self.typed_scope = Some(scope);
        self.typed_draft = draft;
        self.typed_messages.clear();
        self.typed_authors.clear();
        self.typed_filter = None;
        self.typed_panel_open = false;
        self.typed_error = None;
        self.typed_edit_ready = false;
        self.input.update(cx, |input, cx| {
            input.set_value(self.typed_draft.visible_text().to_owned(), window, cx)
        });
        if self._typed_input.is_none() {
            self._typed_input = Some(cx.subscribe_in(
                &self.input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) && this.typed_scope.is_some() {
                        this.sync_typed_input(window, cx);
                        this.publish_typed_draft(cx);
                    }
                },
            ));
        }
        cx.emit(TypedPanelChanged);
        cx.notify();
        Ok(())
    }
    /// Called before switching away, clearing a connection, or disabling Agents.
    pub(super) fn clear_typed_context(&mut self, cx: &mut Context<Self>) {
        self.typed_scope = None;
        self.typed_draft = TypedDraft::default();
        self.typed_messages.clear();
        self.typed_authors.clear();
        self.typed_panel_open = false;
        self.typed_error = None;
        self.typed_edit_ready = false;
        cx.emit(TypedPanelChanged);
        cx.notify();
    }
    /// Original token bytes plus the current visible body. Parent uses this for
    /// send and durable capture instead of reading the pill-stripped textarea.
    pub(super) fn typed_source_text(&self, cx: &App) -> String {
        if self.typed_scope.is_none() {
            return self.input.read(cx).value().to_string();
        }
        let mut draft = self.typed_draft.clone();
        draft.edit_body(self.input.read(cx).value().as_str());
        draft.text
    }
    pub(super) fn typed_has_authored_metadata(&self) -> bool {
        self.typed_scope.is_some()
            && (self.typed_draft.selection.is_some() || self.typed_draft.conversion_suppressed)
    }
    /// Replace the source under the current exact owner without resetting
    /// panel state or reparsing an existing pill as an ordinary body edit.
    pub(super) fn replace_typed_draft(
        &mut self,
        text: String,
        metadata: Option<DraftMetadata>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let draft = match metadata {
            Some(metadata) => TypedDraft::restore_metadata(text, metadata)?,
            None => TypedDraft::from_text(text),
        };
        self.typed_draft = draft;
        self.input.update(cx, |input, cx| {
            input.set_value(self.typed_draft.visible_text().to_owned(), window, cx)
        });
        cx.notify();
        Ok(())
    }
    pub(super) fn typed_metadata(&self) -> Option<TypeMetadata> {
        self.typed_scope.as_ref()?;
        self.typed_draft.selection
    }
    pub(super) fn typed_draft_metadata(&self) -> DraftMetadata {
        self.typed_draft.retained_metadata()
    }
    pub(super) fn set_typed_messages(
        &mut self,
        scope: &TypedScope,
        messages: Vec<TypedMessage>,
        cx: &mut Context<Self>,
    ) {
        if self.typed_scope.as_ref() != Some(scope) {
            return;
        }
        self.typed_messages = messages
            .into_iter()
            .filter(|message| {
                message.key.environment == scope.environment
                    && message.key.project == scope.project
                    && message.key.thread == scope.thread
            })
            .collect();
        cx.emit(TypedPanelChanged);
        cx.notify();
    }
    pub(super) fn set_typed_sources(
        &mut self,
        scope: &TypedScope,
        messages: Vec<TypedMessage>,
        authors: std::collections::HashMap<MessageKey, okilum_t3::message_types::Author>,
        ready: bool,
        cx: &mut Context<Self>,
    ) {
        if self.typed_scope.as_ref() != Some(scope) {
            return;
        }
        self.typed_authors = authors;
        self.typed_edit_ready = ready;
        self.set_typed_messages(scope, messages, cx);
    }
    pub(super) fn typed_scope(&self) -> Option<&TypedScope> {
        self.typed_scope.as_ref()
    }
    pub(super) fn typed_write_succeeded(&mut self, scope: &TypedScope, cx: &mut Context<Self>) {
        if self.typed_scope.as_ref() == Some(scope) {
            self.typed_error = None;
            cx.emit(TypedPanelChanged);
            cx.notify();
        }
    }
    pub(super) fn typed_write_failed(
        &mut self,
        scope: &TypedScope,
        error: String,
        cx: &mut Context<Self>,
    ) {
        if self.typed_scope.as_ref() == Some(scope) {
            self.typed_error = Some(error);
            cx.emit(TypedPanelChanged);
            cx.notify();
        }
    }
    fn publish_typed_draft(&self, cx: &mut Context<Self>) {
        if let Some(scope) = self.typed_scope.clone() {
            cx.emit(TypedDraftChanged {
                scope,
                text: self.typed_draft.text.clone(),
                metadata: self.typed_draft.retained_metadata(),
            });
        }
        cx.notify();
    }
    fn sync_typed_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.typed_draft
            .edit_body(self.input.read(cx).value().as_str());
        if !self.input.read(cx).ime_settling(cx) && self.typed_draft.detect_token() {
            let body = self.typed_draft.visible_text().to_owned();
            self.input.update(cx, |input, cx| {
                input.set_value(body.clone(), window, cx);
                input.set_selected_range(body.len()..body.len(), cx);
            });
        }
    }
    fn undo_type_token(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.typed_draft
            .edit_body(self.input.read(cx).value().as_str());
        if !self.typed_draft.undo_token() {
            return false;
        }
        self.input.update(cx, |input, cx| {
            input.set_value(self.typed_draft.source_text().to_owned(), window, cx);
            input.set_selected_range(0..0, cx);
        });
        self.publish_typed_draft(cx);
        true
    }
    fn render_type_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.typed_scope.is_none() {
            return div().into_any_element();
        }
        let mut row = h_flex()
            .flex_wrap()
            .gap(px(4.))
            .debug_selector(|| "project-type-controls".into());
        for kind in MessageType::ALL {
            let painted_scope = self.typed_scope.clone();
            let selected = self
                .typed_draft
                .selection
                .map_or(kind == MessageType::Chat, |selection| {
                    selection.kind == kind
                });
            row = row.child(
                Button::new(SharedString::from(format!("project-type-{}", kind.label())))
                    .debug_selector(move || format!("project-type-{}", kind.label()))
                    .ghost()
                    .small()
                    .label(kind.label())
                    .child(Icon::new(type_icon(kind)).small())
                    .selected(selected)
                    .toggled(selected)
                    .accessibility_label(format!("Message type: {}", kind.label()))
                    .disabled(!self.enabled || self.busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.typed_scope != painted_scope {
                            return;
                        }
                        this.sync_typed_input(window, cx);
                        this.typed_draft.select(kind);
                        this.publish_typed_draft(cx);
                        this.input.read(cx).focus_handle(cx).focus(window, cx);
                    })),
            );
        }
        if self.typed_draft.has_token_pill() {
            let painted_scope = self.typed_scope.clone();
            row = row.child(
                Button::new("project-type-undo")
                    .debug_selector(|| "project-type-undo".into())
                    .ghost()
                    .small()
                    .label("Undo token")
                    .child(Icon::new(IconName::Undo2).small())
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.typed_scope != painted_scope {
                            return;
                        }
                        this.undo_type_token(window, cx);
                    })),
            );
        }
        let painted_scope = self.typed_scope.clone();
        row.child(
            Button::new("project-type-panel")
                .debug_selector(|| "project-type-panel".into())
                .ghost()
                .small()
                .label("Project items")
                .child(Icon::new(IconName::PanelRight).small())
                .selected(self.typed_panel_open)
                .toggled(self.typed_panel_open)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.typed_scope != painted_scope {
                        return;
                    }
                    this.typed_panel_open = !this.typed_panel_open;
                    cx.emit(TypedPanelChanged);
                    cx.notify();
                })),
        )
        .into_any_element()
    }
    /// The main chat keeps this one-line source link instead of losing typed
    /// items when the structured panel is filtered or closed.
    pub(super) fn render_typed_chat_chip(
        &self,
        key: &MessageKey,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(message) = self
            .typed_messages
            .iter()
            .find(|message| &message.key == key)
        else {
            return div().into_any_element();
        };
        let Some(metadata) = message
            .metadata
            .filter(|metadata| metadata.kind != MessageType::Chat)
        else {
            return div().into_any_element();
        };
        let kind = metadata.kind;
        let key = key.clone();
        let opened = message.clone();
        let painted_scope = self.typed_scope.clone();
        Button::new(SharedString::from(format!(
            "typed-chip-{}:{}:{}",
            key.thread.len(),
            key.thread,
            key.message
        )))
        .ghost()
        .small()
        .label(format!("{} → project item", kind.label()))
        .child(Icon::new(type_icon(kind)).small())
        .on_click(cx.listener(move |this, _, _, cx| {
            if this.typed_scope != painted_scope {
                return;
            }
            this.typed_panel_open = true;
            this.typed_filter = Some(kind);
            if let Some(scope) = painted_scope.clone() {
                cx.emit(TypedMessageOpened(scope, opened.clone()));
            }
            cx.emit(TypedPanelChanged);
            cx.notify();
        }))
        .into_any_element()
    }
    fn render_typed_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        if !self.typed_panel_open || self.typed_scope.is_none() {
            return div().into_any_element();
        }
        let mut tabs = h_flex().gap(px(4.)).flex_wrap();
        for filter in [
            None,
            Some(MessageType::FeatureRequest),
            Some(MessageType::Bug),
            Some(MessageType::Decision),
            Some(MessageType::Status),
            Some(MessageType::Chat),
        ] {
            let label = filter.map_or("All", MessageType::label);
            let painted_scope = self.typed_scope.clone();
            tabs = tabs.child(
                Button::new(SharedString::from(format!("project-type-filter-{label}")))
                    .debug_selector(move || {
                        format!("typed-filter-{}", filter.map_or("All", MessageType::label))
                    })
                    .ghost()
                    .small()
                    .label(label)
                    .selected(self.typed_filter == filter)
                    .toggled(self.typed_filter == filter)
                    .when_some(filter, |button, kind| {
                        button.child(Icon::new(type_icon(kind)).small())
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.typed_scope != painted_scope {
                            return;
                        }
                        this.typed_filter = filter;
                        cx.emit(TypedPanelChanged);
                        cx.notify();
                    })),
            );
        }
        let mut cards = v_flex()
            .id("project-typed-cards")
            .gap(px(8.))
            .overflow_y_scroll()
            .min_h_0()
            .flex_1();
        let mut count = 0;
        let scope = self.typed_scope.as_ref().expect("checked above");
        for message in filtered_messages(
            &self.typed_messages,
            &scope.environment,
            &scope.project,
            self.typed_filter,
        ) {
            let kind = message
                .metadata
                .map_or(MessageType::Chat, |metadata| metadata.kind);
            count += 1;
            let opened = message.clone();
            let source_scope = scope.clone();
            let expected = message.clone();
            let mut correction = h_flex().flex_wrap().gap(px(4.));
            for new_kind in MessageType::ALL {
                let expected = expected.clone();
                let painted_scope = scope.clone();
                correction = correction.child(
                    Button::new(SharedString::from(format!(
                        "retype-{}-{count}",
                        new_kind.label()
                    )))
                    .debug_selector(move || format!("typed-retype-{count}-{}", new_kind.label()))
                    .ghost()
                    .small()
                    .label(new_kind.label())
                    .child(Icon::new(type_icon(new_kind)).small())
                    .selected(kind == new_kind)
                    .toggled(kind == new_kind)
                    .disabled(
                        !self.typed_edit_ready
                            || (message.metadata.is_none()
                                && !self.typed_authors.contains_key(&message.key))
                            || kind == new_kind
                            || self.busy,
                    )
                    .tooltip(
                        if message.metadata.is_none()
                            && !self.typed_authors.contains_key(&message.key)
                        {
                            "Type correction unavailable: source author is unknown".to_owned()
                        } else {
                            format!("Change type to {}", new_kind.label())
                        },
                    )
                    .accessibility_label(format!("Retype message as {}", new_kind.label()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.typed_scope.as_ref() != Some(&painted_scope) {
                            return;
                        }
                        match corrected_message_with_author(
                            &this.typed_messages,
                            &expected,
                            new_kind,
                            this.typed_authors.get(&expected.key).copied(),
                        ) {
                            Ok(replacement) => {
                                if let Some(scope) = this.typed_scope.clone() {
                                    cx.emit(RetypeRequested {
                                        scope,
                                        expected: expected.clone(),
                                        replacement,
                                    });
                                }
                            }
                            Err(error) => {
                                this.typed_error = Some(error.into());
                                cx.emit(TypedPanelChanged);
                                cx.notify();
                            }
                        }
                    })),
                );
            }
            let meaning = message
                .metadata
                .and_then(|metadata| metadata.decision_meaning())
                .map(|meaning| match meaning {
                    DecisionMeaning::RecordedDecision => "Recorded decision",
                    DecisionMeaning::AwaitingAnswer => "Awaiting your answer",
                });
            cards = cards.child(
                v_flex()
                    .gap(px(8.))
                    .p(px(12.))
                    .border_1()
                    .border_color(brand::palette(cx).border_subtle)
                    .rounded(px(8.))
                    .child(
                        Button::new(SharedString::from(format!("typed-source-{count}")))
                            .debug_selector(move || format!("typed-source-{count}"))
                            .ghost()
                            .small()
                            .label(format!("{} · View message", kind.label()))
                            .child(Icon::new(type_icon(kind)).small())
                            .tooltip("Open original chat message")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.typed_scope.as_ref() == Some(&source_scope) {
                                    cx.emit(TypedMessageOpened(
                                        source_scope.clone(),
                                        opened.clone(),
                                    ));
                                }
                            })),
                    )
                    .when_some(message.metadata, |card, metadata| {
                        use okilum_t3::message_types::{Author, Provenance};
                        let origin = match metadata.author {
                            Author::User => "From you",
                            Author::Manager => "From manager",
                        };
                        let source = match metadata.provenance {
                            Provenance::ExplicitToken => "typed token",
                            Provenance::ExplicitSelection => "selected type",
                            Provenance::ManagerClassification => "manager classification",
                            Provenance::UserCorrection => "corrected by you",
                        };
                        card.child(
                            div()
                                .text_size(px(12.))
                                .child(format!("{origin} · {source}")),
                        )
                    })
                    .when_some(meaning, |card, meaning| card.child(div().child(meaning)))
                    .child(div().text_size(px(14.)).child(message.text.clone()))
                    .child(correction),
            );
        }
        if count == 0 {
            cards = cards.child(div().p(px(12.)).child(
                "No messages of this type. Use FR, Bug or Decision in the composer to add one.",
            ));
        }
        let painted_scope = self.typed_scope.clone();
        v_flex()
            .id("project-typed-panel")
            .debug_selector(|| "project-typed-panel".into())
            .w(px(320.))
            .max_w(relative(0.6))
            .min_h_0()
            .h_full()
            .gap(px(8.))
            .p(px(12.))
            .child(
                h_flex().child(div().flex_1().child("Project items")).child(
                    Button::new("typed-panel-close")
                        .ghost()
                        .small()
                        .label("Close")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.typed_scope != painted_scope {
                                return;
                            }
                            this.typed_panel_open = false;
                            cx.emit(TypedPanelChanged);
                            cx.notify();
                        })),
                ),
            )
            .child(tabs)
            .child(
                h_flex().gap(px(4.)).children(
                    [
                        MessageType::FeatureRequest,
                        MessageType::Bug,
                        MessageType::Decision,
                    ]
                    .into_iter()
                    .map(|kind| {
                        let painted_scope = self.typed_scope.clone();
                        Button::new(SharedString::from(format!(
                            "typed-panel-add-{}",
                            kind.label()
                        )))
                        .ghost()
                        .small()
                        .label(format!("+ {}", kind.label()))
                        .child(Icon::new(type_icon(kind)).small())
                        .disabled(!self.enabled || self.busy)
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                if this.typed_scope != painted_scope {
                                    return;
                                }
                                this.sync_typed_input(window, cx);
                                this.typed_draft.select(kind);
                                this.publish_typed_draft(cx);
                                this.input.read(cx).focus_handle(cx).focus(window, cx);
                            },
                        ))
                    }),
                ),
            )
            .when_some(self.typed_error.clone(), |panel, error| {
                let scope = self.typed_scope.clone();
                panel
                    .child(div().text_color(cx.theme().danger).child(error))
                    .child(
                        Button::new("typed-reload")
                            .ghost()
                            .small()
                            .label("Reload message types")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.typed_scope == scope {
                                    if let Some(scope) = &scope {
                                        cx.emit(TypedReloadRequested(scope.clone()));
                                    }
                                }
                            })),
                    )
            })
            .child(cards)
            .into_any_element()
    }
    pub(super) fn bind_parent(&mut self, parent: WeakEntity<ProjectView>, cx: &mut Context<Self>) {
        // Model state lives on ProjectView, while keystrokes stay local to this
        // entity. Observe its changes so a cached composer shows the confirmed
        // model and popup state without invalidating the message history.
        self._parent = parent
            .upgrade()
            .map(|parent| cx.observe(&parent, |_, _, cx| cx.notify()));
        self.parent = Some(parent);
    }
    pub(super) fn bind_history(&mut self, feed: Entity<feed::Feed>) {
        self.history_feed = Some(feed);
    }
    pub(super) fn reset_history(&mut self, cx: &mut Context<Self>) {
        if self.history_position.take().is_some() {
            cx.notify();
        }
    }
    pub(super) fn set_history_blocked(&mut self, blocked: bool) {
        self.history_blocked = blocked;
    }
    fn navigate_history(
        &mut self,
        direction: history::Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let modifiers = window.modifiers();
        if modifiers.shift
            || modifiers.alt
            || modifiers.control
            || modifiers.platform
            || modifiers.function
        {
            return false;
        }
        let Some(feed) = &self.history_feed else {
            return false;
        };
        let input = self.input.read(cx);
        if self.history_blocked
            || self.has_attachments
            || feed.read(cx).history_has_requests(cx)
            || !input.focus_handle(cx).is_focused(window)
            || input.presentation().is_disabled()
            || !input.is_editable()
            || input.ime_settling(cx)
            || input.completion_active(cx)
            || !input.selected_range().is_empty()
        {
            return false;
        }
        let prompt = input.value().to_string();
        // Avoid both a layout read and a history rebuild for ordinary typing.
        if self.history_position.is_none() && !prompt.is_empty() {
            return false;
        }
        if !input.is_caret_on_visual_edge(direction == history::Direction::Forward) {
            return false;
        }
        let entries = feed.read(cx).prompt_history();
        let Some(step) =
            history::step(direction, &entries, self.history_position.as_ref(), &prompt)
        else {
            return false;
        };
        self.history_position = step.position;
        // History recall is a replacement draft, never a body edit underneath
        // a token from the previous prompt.
        if self.typed_scope.is_some() {
            self.typed_draft = TypedDraft::from_text(step.prompt.clone());
            self.publish_typed_draft(cx);
        }
        self.input.update(cx, |input, cx| {
            let end = step.prompt.len();
            input.set_value(step.prompt, window, cx);
            input.set_selected_range(end..end, cx);
        });
        // Programmatic set_value deliberately emits no InputEvent. Publish one
        // explicit recall event so the parent's existing draft writer captures
        // it, without teaching durable draft storage about history position.
        cx.emit(HistoryRecalled);
        cx.stop_propagation();
        cx.notify();
        true
    }
    fn prompt_validation(&self, cx: &App) -> Option<String> {
        self.native
            .then(|| okilum_t3::actions::prompt_length_validation(&self.typed_source_text(cx)))
            .flatten()
    }
    pub(super) fn set_native(&mut self, native: bool, steer: bool, cx: &mut Context<Self>) {
        if (self.native, self.steer) != (native, steer) {
            self.native = native;
            self.steer = steer;
            if !native {
                self.mode_open = false;
            }
            if !steer && self.mode == Mode::Steer {
                self.mode = Mode::Auto;
            }
            cx.notify();
        }
    }
    pub(super) fn set_controls(&mut self, enabled: bool, busy: bool, cx: &mut Context<Self>) {
        if (self.enabled, self.busy) != (enabled, busy) {
            self.enabled = enabled;
            self.busy = busy;
            if busy {
                self.mode_open = false;
            }
            cx.notify();
        }
    }
    pub(super) fn set_attachments(
        &mut self,
        has_attachments: bool,
        can_attach: bool,
        cx: &mut Context<Self>,
    ) {
        if (self.has_attachments, self.can_attach) != (has_attachments, can_attach) {
            self.has_attachments = has_attachments;
            self.can_attach = can_attach;
            cx.notify();
        }
    }

    fn close_send_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mode_open = false;
        self.input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }
    fn render_send_mode(&self, cx: &mut Context<Self>) -> AnyElement {
        let bounds = self.mode_trigger_bounds.clone();
        let trigger = div()
            .id("project-send-mode-anchor")
            .relative()
            .on_prepaint(move |measured, _, _| bounds.set(measured))
            .child(
                Button::new("project-send-mode")
                    .debug_selector(|| "project-send-mode".into())
                    .ghost()
                    .small()
                    .size(px(32.))
                    .p_0()
                    .rounded(px(8.))
                    .child(Icon::new(IconName::ChevronDown).size(px(18.)))
                    .selected(self.mode_open)
                    .accessibility_label(format!("Choose send mode; current: {}", self.mode.name()))
                    .tooltip(format!("Send mode: {}", self.mode.name()))
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.mode_open {
                            this.close_send_mode(window, cx);
                        } else {
                            this.mode_open = true;
                            this.mode_focus.focus(window, cx);
                            cx.notify();
                        }
                    })),
            );
        if !self.mode_open {
            return trigger.into_any_element();
        }
        let mut menu = v_flex()
            .id("project-send-mode-menu")
            .debug_selector(|| "project-send-mode-menu".into())
            .role(gpui::Role::Menu)
            .track_focus(&self.mode_focus)
            .occlude()
            .on_prepaint(gpui_base::GlobalState::record_native_overlay)
            .popover_style(cx)
            .text_size(px(design::CHROME))
            .line_height(px(design::LINE))
            .w(px(200.))
            .p_1()
            .gap_0p5()
            .on_mouse_down_out(
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    // The trigger owns its completed click. A pointer-down outside
                    // the menu but on that same trigger must not close then reopen.
                    if !this.mode_trigger_bounds.get().contains(&event.position) {
                        this.close_send_mode(window, cx);
                    }
                }),
            )
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.close_send_mode(window, cx);
                    cx.stop_propagation();
                }
            }));
        for (mode, label, tooltip) in [
            (
                Mode::Auto,
                "Send",
                "Send normally; let the manager handle an active turn",
            ),
            (
                Mode::Queue,
                "Queue",
                "Queue this message after the current turn",
            ),
            (
                Mode::Steer,
                "Steer",
                "Send this message into the current turn",
            ),
        ] {
            menu = menu.child(
                Button::new(SharedString::from(format!("project-mode-{label}")))
                    .debug_selector(move || format!("project-mode-{label}"))
                    .ghost()
                    .small()
                    .w_full()
                    .justify_start()
                    .label(label)
                    .selected(self.mode == mode)
                    .toggled(self.mode == mode)
                    .tooltip(tooltip)
                    .disabled(self.busy || (mode == Mode::Steer && !self.steer))
                    .child(div().flex_1())
                    .child(div().w(px(14.)).when(self.mode == mode, |check| {
                        check.child(Icon::new(IconName::Check).small())
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.mode = mode;
                        this.close_send_mode(window, cx);
                    })),
            );
        }
        // The measured trigger stays in the composer flow; only the menu is
        // deferred. Opening must not change the trigger's bounds or its own
        // outside-click exclusion region. Side placement flips when the menu
        // cannot fit above, rather than clamping it over its own trigger.
        trigger
            .child(
                gpui::deferred(
                    gpui_base::Positioner::side(self.mode_trigger_bounds.get())
                        .placement(gpui_base::Placement::Top)
                        .align(gpui_base::Align::End)
                        .offset(px(4.))
                        .margin(px(12.))
                        .occlude()
                        .child(menu),
                )
                .with_priority(gpui_base::POPUP_PRIORITY),
            )
            .into_any_element()
    }
}
impl Render for Composer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(all(target_os = "linux", feature = "project-ui-harness"))]
        let _work = super::performance::Work::start(27);
        let validation = self.prompt_validation(cx);
        let palette = brand::palette(cx);
        let send = send_presentation(
            self.mode,
            !self.busy
                && validation.is_none()
                && self.enabled
                && (self.has_attachments || !self.input.read(cx).value().trim().is_empty()),
            palette,
        );
        let root = v_flex()
            .id("project-composer")
            .relative()
            .w_full()
            .min_w_0()
            .text_size(px(design::CHROME))
            .line_height(px(design::LINE))
            .gap(px(design::GAP))
            .child(self.render_type_controls(cx))
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                let input = this.input.read(cx);
                if this.typed_scope.is_none()
                    || this.mode_open
                    || this.busy
                    || !this.enabled
                    || !input.focus_handle(cx).is_focused(window)
                    || input.ime_settling(cx)
                    || input.completion_active(cx)
                {
                    return;
                }
                let backspace_at_start =
                    event.keystroke.key == "backspace" && input.selected_range() == (0..0);
                if (event.keystroke.key == "escape" || backspace_at_start)
                    && this.undo_type_token(window, cx)
                {
                    cx.stop_propagation();
                }
            }))
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::MoveUp, window, cx| {
                    if !this.navigate_history(history::Direction::Backward, window, cx) {
                        cx.propagate();
                    }
                }),
            )
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::MoveDown, window, cx| {
                    if !this.navigate_history(history::Direction::Forward, window, cx) {
                        cx.propagate();
                    }
                }),
            )
            // Textarea propagates submit-on-enter. Consume that action here so
            // the platform input fallback cannot insert a newline before send.
            .on_action(|_: &gpui_component::input::Enter, _, _| {})
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::Paste, window, cx| {
                    if this.native
                        && this.input.read(cx).focus_handle(cx).is_focused(window)
                        && !this.input.read(cx).presentation().is_disabled()
                    {
                        cx.stop_propagation();
                        cx.emit(AttachmentRequested::Paste);
                    } else {
                        cx.propagate();
                    }
                }),
            )
            .drag_over::<ExternalPaths>(|style, _, _, cx| style.bg(brand::palette(cx).selected))
            .on_drop(cx.listener(|_, paths: &ExternalPaths, _, cx| {
                cx.emit(AttachmentRequested::Paths(paths.0.to_vec()))
            }))
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .min_h(px(32.))
                    .debug_selector(|| "project-composer-input".into())
                    .child(
                        Textarea::new(&self.input)
                            .appearance(false)
                            .bordered(false)
                            .w_full()
                            .text_size(px(design::BODY))
                            .line_height(px(design::LINE))
                            .aria_label("Message your manager"),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .flex_wrap()
                    .gap(px(design::GAP))
                    .debug_selector(|| "project-composer-toolbar".into())
                    .child(
                        Button::new("project-attach")
                            .debug_selector(|| "project-attach".into())
                            .ghost()
                            .small()
                            .size(px(32.))
                            .rounded(px(8.))
                            .p_0()
                            .child(Icon::new(IconName::Plus).size(px(18.)))
                            .accessibility_label("Attach images or files")
                            .tooltip("Attach images or files")
                            .disabled(!self.can_attach)
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(AttachmentRequested::Choose)),
                            ),
                    )
                    .when_some(self.parent.clone(), |toolbar, parent| {
                        toolbar.child(
                            div()
                                .flex_1()
                                .min_w(px(120.))
                                .debug_selector(|| "project-composer-model-slot".into())
                                .child(ModelControlsSlot(parent)),
                        )
                    })
                    .child(div().flex_1().min_w_0())
                    .child(
                        h_flex()
                            .debug_selector(|| "project-send-controls".into())
                            .items_center()
                            .gap(px(4.))
                            .child(
                                Button::new("project-send")
                                    .debug_selector(|| "project-send".into())
                                    .custom(
                                        ButtonCustomVariant::new(cx)
                                            .color(send.background)
                                            .foreground(send.foreground)
                                            .hover(send.background)
                                            .active(send.background),
                                    )
                                    .small()
                                    .size(px(32.))
                                    .p_0()
                                    .rounded(px(8.))
                                    // Instance styles are replayed after semantic disabled styles.
                                    .bg(send.background)
                                    .text_color(send.foreground)
                                    .child(
                                        div()
                                            .debug_selector(|| "project-send-icon".into())
                                            .size(px(18.))
                                            .child(
                                                Icon::new(IconName::ArrowUp)
                                                    .size(px(18.))
                                                    .text_color(send.foreground),
                                            ),
                                    )
                                    .accessibility_label(send.accessible_label)
                                    .tooltip(send.tooltip)
                                    .disabled(!send.enabled)
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SendRequested))),
                            )
                            .when(self.native, |group| group.child(self.render_send_mode(cx))),
                    ),
            )
            .when_some(validation, |composer, message| {
                composer.child(
                    div()
                        .debug_selector(|| "project-prompt-length-validation".into())
                        .text_size(px(design::LABEL))
                        .line_height(px(design::LINE))
                        .text_color(cx.theme().danger)
                        .child(message),
                )
            })
            .into_any_element();
        #[cfg(all(target_os = "linux", feature = "project-ui-harness"))]
        let root = super::performance::Trace::new(root, 3).into_any_element();
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };

    fn contrast(a: Hsla, b: Hsla) -> f32 {
        fn luminance(color: Hsla) -> f32 {
            let color = gpui::Rgba::from(color);
            let channel = |value: f32| {
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
        }
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn send_icon_has_nontext_contrast_in_enabled_and_disabled_theme_states() {
        for theme in brand::ThemeId::ALL {
            for dark in [false, true] {
                let palette = brand::theme_palette(theme, dark);
                let enabled = send_presentation(Mode::Auto, true, palette);
                let disabled = send_presentation(Mode::Auto, false, palette);
                assert!(
                    contrast(enabled.foreground, enabled.background) >= 3.,
                    "enabled Send icon must have 3:1 contrast: {theme:?}, dark={dark}"
                );
                assert!(
                    contrast(disabled.foreground, disabled.background) >= 3.,
                    "disabled Send remains readable: {theme:?}, dark={dark}"
                );
                assert_ne!(
                    disabled.background, enabled.background,
                    "disabled control cannot retain a saturated enabled surface"
                );
            }
        }
    }

    #[gpui::test]
    fn icon_only_send_is_compact_grows_with_text_and_blocks_disabled_dispatch(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::test_init);
        for dark in [false, true] {
            cx.update(|cx| {
                gpui_component::Theme::change(
                    if dark {
                        gpui_component::ThemeMode::Dark
                    } else {
                        gpui_component::ThemeMode::Light
                    },
                    None,
                    cx,
                );
                brand::apply_theme(cx);
            });
            let sent = Rc::new(Cell::new(0));
            let observed = sent.clone();
            let (root, visual) = cx.add_window_view(|window, cx| {
                let input = cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .auto_grow(1, 10)
                        .submit_on_enter(true)
                });
                let composer = cx.new(|cx| {
                    let mut composer = Composer::new(input, cx);
                    composer.set_controls(true, false, cx);
                    composer.set_attachments(false, true, cx);
                    cx.subscribe(&cx.entity(), move |_, _, _: &SendRequested, _| {
                        observed.set(observed.get() + 1)
                    })
                    .detach();
                    composer
                });
                Root::new(composer, window, cx)
            });
            visual.simulate_resize(size(px(360.), px(600.)));
            visual.run_until_parked();
            let composer = root.read_with(visual, |root, _| {
                root.view().clone().downcast::<Composer>().unwrap()
            });
            let empty = visual.debug_bounds("project-composer-input").unwrap();
            let send = visual.debug_bounds("project-send").unwrap();
            let arrow = visual.debug_bounds("project-send-icon").unwrap();
            let attach = visual.debug_bounds("project-attach").unwrap();
            assert!(
                empty.size.height <= px(48.),
                "empty composer is a compact single line: {empty:?}"
            );
            assert_eq!(
                send.size,
                size(px(32.), px(32.)),
                "primary Send is icon-only, without reserved text width"
            );
            assert_eq!(
                arrow.size,
                size(px(18.), px(18.)),
                "arrow uses the shared optical icon size"
            );
            assert_eq!(
                send.center().y,
                attach.center().y,
                "attachment and Send share a baseline"
            );
            visual.simulate_click(send.center(), Modifiers::default());
            visual.run_until_parked();
            assert_eq!(sent.get(), 0, "empty disabled button cannot dispatch");
            composer.update_in(visual, |composer, window, cx| {
                composer.input.update(cx, |input, cx| {
                    input.set_value("First\nSecond\nThird\nFourth", window, cx);
                    input.focus(window, cx);
                });
                cx.notify();
            });
            visual.run_until_parked();
            let grown = visual.debug_bounds("project-composer-input").unwrap();
            assert!(
                grown.size.height > empty.size.height,
                "actual multiline content grows the input"
            );
            let send = visual.debug_bounds("project-send").unwrap();
            visual.simulate_click(send.center(), Modifiers::default());
            visual.run_until_parked();
            assert_eq!(sent.get(), 1, "enabled icon button dispatches exactly once");
            composer.update_in(visual, |composer, window, cx| {
                composer
                    .input
                    .update(cx, |input, cx| input.focus(window, cx))
            });
            visual.simulate_keystrokes("shift-enter");
            visual.run_until_parked();
            assert_eq!(
                sent.get(),
                1,
                "Shift+Enter inserts a newline rather than sending"
            );
            composer.read_with(visual, |composer, cx| {
                assert!(composer.input.read(cx).value().contains('\n'))
            });
            composer.update(visual, |composer, cx| composer.set_controls(true, true, cx));
            visual.run_until_parked();
            let send = visual.debug_bounds("project-send").unwrap();
            visual.simulate_click(send.center(), Modifiers::default());
            visual.simulate_keystrokes("enter");
            visual.run_until_parked();
            assert_eq!(
                sent.get(),
                1,
                "in-flight Send rejects both pointer and Enter dispatch"
            );
        }
    }

    #[gpui::test]
    fn send_mode_disclosure_stays_with_send_and_enter_retains_each_explicit_mode(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::test_init);
        let sent = Rc::new(RefCell::new(Vec::new()));
        let observed = sent.clone();
        let (root, visual) = cx.add_window_view(|window, cx| {
            let input = cx.new(|cx| TextareaState::new(window, cx).submit_on_enter(true));
            let composer = cx.new(|cx| {
                let mut composer = Composer::new(input, cx);
                composer.set_native(true, true, cx);
                composer.set_controls(true, false, cx);
                composer.set_attachments(false, true, cx);
                cx.subscribe(&cx.entity(), move |this, _, _: &SendRequested, _| {
                    observed.borrow_mut().push(this.mode);
                })
                .detach();
                composer
            });
            Root::new(composer, window, cx)
        });
        visual.simulate_resize(size(px(320.), px(600.)));
        visual.run_until_parked();
        let composer = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<Composer>().unwrap()
        });
        let input = visual.debug_bounds("project-composer-input").unwrap();
        let send = visual.debug_bounds("project-send").unwrap();
        let mode = visual.debug_bounds("project-send-mode").unwrap();
        let attach = visual.debug_bounds("project-attach").unwrap();
        assert!(
            input.size.width >= px(280.),
            "textarea retains the narrow-panel width"
        );
        assert!(attach.top() >= input.bottom() && send.top() >= input.bottom());
        assert_eq!(
            send.top(),
            mode.top(),
            "mode disclosure shares the Send baseline"
        );
        assert!(
            mode.left() >= send.right() && mode.left() - send.right() <= px(4.),
            "mode belongs to the send group"
        );
        assert!(visual.debug_bounds("project-mode-Send").is_none());
        assert!(visual.debug_bounds("project-mode-Queue").is_none());
        assert!(
            visual.debug_bounds("project-mode-Steer").is_none(),
            "idle composer has no always-visible mode strip"
        );
        for (index, (selector, expected)) in [
            ("project-mode-Send", Mode::Auto),
            ("project-mode-Queue", Mode::Queue),
            ("project-mode-Steer", Mode::Steer),
        ]
        .into_iter()
        .enumerate()
        {
            let trigger = visual.debug_bounds("project-send-mode").unwrap();
            visual.simulate_click(trigger.center(), Modifiers::default());
            visual.run_until_parked();
            let choice = visual
                .debug_bounds(selector)
                .expect("all supported modes remain reachable in the disclosure");
            visual.simulate_click(choice.center(), Modifiers::default());
            visual.run_until_parked();
            assert!(
                visual.debug_bounds("project-send-mode-menu").is_none(),
                "choosing mode closes its disclosure"
            );
            composer.update_in(visual, |composer, window, cx| {
                assert_eq!(composer.mode, expected);
                composer.input.update(cx, |input, cx| {
                    input.set_value(format!("Message {index}"), window, cx);
                    input.focus(window, cx);
                });
            });
            visual.run_until_parked();
            visual.simulate_keystrokes("enter");
            visual.run_until_parked();
            assert_eq!(
                *sent.borrow(),
                [Mode::Auto, Mode::Queue, Mode::Steer][..=index]
            );
        }
        let trigger = visual.debug_bounds("project-send-mode").unwrap();
        visual.simulate_click(trigger.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("project-send-mode-menu").is_some());
        let menu = visual.debug_bounds("project-send-mode-menu").unwrap();
        let open_trigger = visual.debug_bounds("project-send-mode").unwrap();
        assert!(
            menu.bottom() <= open_trigger.top() || menu.top() >= open_trigger.bottom(),
            "menu cannot cover its closing trigger: menu={menu:?}, trigger={open_trigger:?}"
        );
        assert_eq!(
            open_trigger, trigger,
            "opening cannot move the disclosure trigger"
        );
        composer.read_with(visual, |composer, _| {
            assert!(composer.mode_trigger_bounds.get().contains(&open_trigger.center()),
                "outside-click exclusion uses the real trigger bounds: measured={:?}, trigger={open_trigger:?}", composer.mode_trigger_bounds.get());
        });
        visual.simulate_click(open_trigger.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("project-send-mode-menu").is_none(),
            "a second completed trigger click closes without reopening"
        );
        composer.read_with(visual, |composer, _| {
            assert_eq!(
                composer.mode,
                Mode::Steer,
                "closing the disclosure never selects a mode under its trigger"
            )
        });
        assert_eq!(
            sent.borrow().len(),
            3,
            "disclosure interactions never dispatch a message"
        );
        composer.update(visual, |composer, cx| composer.set_native(true, false, cx));
        composer.read_with(visual, |composer, _| {
            assert_eq!(
                composer.mode,
                Mode::Auto,
                "a lost Steer capability returns to the safe existing mode"
            )
        });
    }

    #[gpui::test]
    fn real_model_control_and_send_share_the_composer_toolbar_across_panel_widths(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::test_init);
        let state = tempfile::tempdir().unwrap();
        let gateway = Arc::new(super::super::fixture::Fake::default());
        let (root, visual) = cx.add_window_view(|window, cx| {
            let project = cx.new(|cx| {
                super::super::fixture::view(state.path().to_owned(), gateway.clone(), window, cx)
            });
            Root::new(project, window, cx)
        });
        visual.run_until_parked();
        let project = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<ProjectView>().unwrap()
        });
        project.update_in(visual, |project, window, cx| {
            project.compose.update(cx, |input, cx| {
                input.set_value("Design layout keeps this draft.", window, cx)
            });
            project.composer.update(cx, |_, cx| cx.notify());
        });
        for width in [760., 360.] {
            visual.simulate_resize(size(px(width), px(700.)));
            visual.run_until_parked();
            let toolbar = visual.debug_bounds("project-composer-toolbar").unwrap();
            let input = visual.debug_bounds("project-composer-input").unwrap();
            let slot = visual.debug_bounds("project-composer-model-slot").unwrap();
            let model = visual.debug_bounds("project-manager-model").unwrap();
            let send = visual.debug_bounds("project-send").unwrap();
            assert!(
                toolbar.top() >= input.bottom(),
                "controls belong below the editable draft"
            );
            assert!(
                slot.contains(&model.center()),
                "the actual model control lives in the composer slot"
            );
            for selector in ["project-attach", "project-manager-model", "project-send"] {
                let control = visual.debug_bounds(selector).unwrap();
                assert!(control.left() >= toolbar.left() && control.right() <= toolbar.right()
                    && control.top() >= toolbar.top() && control.bottom() <= toolbar.bottom(),
                    "{selector} must stay inside the visible message toolbar at width {width}: control={control:?}, toolbar={toolbar:?}");
            }
            if width == 760. {
                assert!(
                    (model.center().y - send.center().y).abs() <= px(1.),
                    "model and Send share one aligned toolbar on a wide panel"
                );
            }
            project.read_with(visual, |project, cx| {
                assert_eq!(
                    project.compose.read(cx).value().as_str(),
                    "Design layout keeps this draft.",
                    "layout reflow preserves the draft verbatim"
                );
            });
        }
    }

    #[gpui::test]
    fn native_paste_consumes_the_action_once_and_legacy_paste_still_works(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let received = Rc::new(Cell::new(0));
        let insert = Rc::new(Cell::new(false));
        let dropped = Rc::new(RefCell::new(Vec::new()));
        let (root, visual) = cx.add_window_view(|window, cx| {
            let input = cx.new(|cx| TextareaState::new(window, cx));
            let composer = cx.new(|cx| {
                let mut composer = Composer::new(input, cx);
                composer.set_native(true, false, cx);
                composer.set_controls(true, false, cx);
                let received = received.clone();
                let insert = insert.clone();
                let dropped = dropped.clone();
                cx.subscribe_in(
                    &cx.entity(),
                    window,
                    move |this, _, event: &AttachmentRequested, window, cx| {
                        if let AttachmentRequested::Paths(paths) = event {
                            dropped.borrow_mut().extend(paths.clone());
                        }
                        if matches!(event, AttachmentRequested::Paste) {
                            received.set(received.get() + 1);
                            // File paste stages a row without inserting its URI.
                            // Text paste uses the same production InputState::insert.
                            if insert.get() {
                                let text = cx.read_from_clipboard().unwrap().text().unwrap();
                                this.input
                                    .update(cx, |input, cx| input.insert(text, window, cx));
                            }
                        }
                    },
                )
                .detach();
                composer
            });
            Root::new(composer, window, cx)
        });
        let composer = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<Composer>().unwrap()
        });
        composer.update_in(visual, |composer, window, cx| {
            composer
                .input
                .update(cx, |input, cx| input.focus(window, cx))
        });
        visual.run_until_parked();
        visual.update(|_, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                "file:///typed-file.txt".into(),
            ))
        });
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-v"
        } else {
            "ctrl-v"
        });
        visual.run_until_parked();
        assert_eq!(received.get(), 1);
        composer.read_with(visual, |composer, cx| {
            assert!(
                composer.input.read(cx).value().is_empty(),
                "file URI must not reach the default text Paste handler"
            )
        });
        insert.set(true);
        visual.update(|_, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                "First line\nSecond line".into(),
            ))
        });
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-v"
        } else {
            "ctrl-v"
        });
        visual.run_until_parked();
        assert_eq!(received.get(), 2);
        composer.update_in(visual, |composer, window, cx| {
            assert_eq!(
                composer.input.read(cx).value().as_str(),
                "First line\nSecond line"
            );
            composer
                .input
                .update(cx, |input, cx| input.set_value("", window, cx));
            composer.set_native(false, false, cx);
        });
        visual.run_until_parked();
        visual.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-v"
        } else {
            "ctrl-v"
        });
        visual.run_until_parked();
        assert_eq!(
            received.get(),
            2,
            "legacy text Paste must use its original handler"
        );
        composer.read_with(visual, |composer, cx| {
            assert_eq!(
                composer.input.read(cx).value().as_str(),
                "First line\nSecond line"
            )
        });
        composer.update(visual, |composer, cx| composer.set_native(true, false, cx));
        visual.run_until_parked();
        let center = visual
            .debug_bounds("project-composer-input")
            .unwrap()
            .center();
        let paths = vec![
            PathBuf::from("/qa/screenshot.png"),
            PathBuf::from("/qa/report.pdf"),
            PathBuf::from("/qa/notes.txt"),
        ];
        // A real file drag moves the pointer before entering the drop target.
        // FileDropEvent alone does not leave GPUI's keyboard modality.
        visual.simulate_event(gpui::MouseMoveEvent {
            position: center,
            pressed_button: Some(MouseButton::Left),
            ..Default::default()
        });
        visual.run_until_parked();
        visual.update(|window, _| assert!(!window.last_input_was_keyboard()));
        visual.simulate_event(gpui::FileDropEvent::Entered {
            position: center,
            paths: ExternalPaths(paths.clone().into_iter().collect()),
        });
        visual.run_until_parked();
        visual.simulate_event(gpui::FileDropEvent::Submit { position: center });
        visual.run_until_parked();
        assert_eq!(
            *dropped.borrow(),
            paths,
            "native file-drop events must retain all typed paths"
        );
    }
    #[gpui::test]
    fn oversized_prompt_blocks_enter_and_click_without_changing_the_draft(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let sends = Rc::new(Cell::new(0));
        let observed = sends.clone();
        let (root, visual) = cx.add_window_view(|window, cx| {
            let input = cx.new(|cx| TextareaState::new(window, cx).submit_on_enter(true));
            let composer = cx.new(|cx| {
                let mut composer = Composer::new(input, cx);
                composer.set_native(true, false, cx);
                composer.set_controls(true, false, cx);
                cx.subscribe(&cx.entity(), move |_, _, _: &SendRequested, _| {
                    observed.set(observed.get() + 1);
                })
                .detach();
                composer
            });
            Root::new(composer, window, cx)
        });
        let composer = root.read_with(visual, |root, _| {
            root.view().clone().downcast::<Composer>().unwrap()
        });
        let oversized = "a".repeat(120_001);
        composer.update_in(visual, |composer, window, cx| {
            composer.input.update(cx, |input, cx| {
                input.set_value(oversized.clone(), window, cx);
                input.focus(window, cx);
            });
            assert_eq!(composer.prompt_validation(cx).as_deref(), Some("Prompt is 1 character over the 120,000-character limit. Shorten or split it before sending."));
        });
        visual.run_until_parked();
        assert!(visual
            .debug_bounds("project-prompt-length-validation")
            .is_some());
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        let send = visual.debug_bounds("project-send").unwrap().center();
        visual.simulate_click(send, Modifiers::default());
        visual.run_until_parked();
        assert_eq!(sends.get(), 0);
        composer.read_with(visual, |composer, cx| {
            assert_eq!(composer.input.read(cx).value().as_str(), oversized);
        });
        composer.update_in(visual, |composer, window, cx| {
            composer.input.update(cx, |input, cx| {
                input.set_value("a".repeat(120_000), window, cx);
                input.focus(window, cx);
            });
        });
        visual.run_until_parked();
        assert!(visual
            .debug_bounds("project-prompt-length-validation")
            .is_none());
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert_eq!(
            sends.get(),
            1,
            "positive control: the boundary prompt dispatches once"
        );
    }
    #[gpui::test]
    fn prompt_history_keys_preserve_typed_selected_attached_and_modified_input(
        cx: &mut TestAppContext,
    ) {
        use okilum_t3::projection::{Changes, RowKey, TurnRow};
        cx.update(crate::test_init);
        let temp = tempfile::tempdir().unwrap();
        let mut project = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = super::super::fixture::view(
                    temp.path().to_owned(),
                    Arc::new(super::super::fixture::Fake::default()),
                    window,
                    cx,
                );
                view.active = false;
                view.compose.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                    input.focus(window, cx);
                });
                view
            });
            let composer = view.read(cx).composer.clone();
            project = Some(view);
            Root::new(composer, window, cx)
        });
        let project = project.unwrap();
        let composer = project.read_with(visual, |view, _| view.composer.clone());
        project.update(visual, |view, cx| view.feed.update(cx, |feed, cx| {
            let key = |id: &str| RowKey { thread: "history".into(), item: id.into() };
            let row = |id: &str, text: &str| Arc::new(TurnRow { key: key(id), local: true, item: Arc::new(serde_json::json!({"id":id,"type":"user_message","text":text,"status":"completed"})) });
            feed.apply_native(Changes { rows: vec![row("first", "first"), row("last", "second")], order: Some(vec![key("first"), key("last")]), ..Default::default() }, &[], cx);
        }));
        visual.simulate_resize(size(px(600.), px(600.)));
        visual.run_until_parked();
        composer.update_in(visual, |composer, window, cx| {
            composer
                .input
                .update(cx, |input, cx| input.focus(window, cx))
        });
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert_eq!(
                composer
                    .history_feed
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .prompt_history()
                    .len(),
                2,
                "positive control: visible thread has two recallable user prompts"
            );
            assert!(
                composer.input.read(cx).is_caret_on_visual_edge(false),
                "positive control: empty input is on its top visual edge"
            );
        });
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert_eq!(
                composer.input.read(cx).value().as_str(),
                "second",
                "positive control: unmodified Up recalled the latest prompt"
            )
        });
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert_eq!(composer.input.read(cx).value().as_str(), "first")
        });
        visual.simulate_keystrokes("down");
        visual.run_until_parked();
        visual.simulate_keystrokes("down");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert!(composer.input.read(cx).value().is_empty())
        });
        // Any modifier is native caret/selection navigation, never recall.
        for key in ["shift-up", "ctrl-up", "alt-up", "cmd-up"] {
            visual.simulate_keystrokes(key);
            visual.run_until_parked();
            composer.read_with(visual, |composer, cx| {
                assert!(
                    composer.input.read(cx).value().is_empty(),
                    "{key} recalled text"
                )
            });
        }
        composer.update_in(visual, |composer, window, cx| {
            composer
                .input
                .update(cx, |input, cx| input.set_value("typed", window, cx));
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert_eq!(composer.input.read(cx).value().as_str(), "typed")
        });
        composer.update_in(visual, |composer, window, cx| {
            composer
                .input
                .update(cx, |input, cx| input.set_value("", window, cx));
            composer.set_attachments(true, true, cx);
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert!(composer.input.read(cx).value().is_empty())
        });
        composer.update(visual, |composer, cx| {
            composer.set_attachments(false, true, cx);
            composer.set_history_blocked(true);
        });
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert!(
                composer.input.read(cx).value().is_empty(),
                "pending question must block recall"
            )
        });
        composer.update(visual, |composer, _| composer.set_history_blocked(false));
        project.update(visual, |view, cx| {
            view.native.actions.update(cx, |actions, _| {
                actions.state.requests.push(okilum_t3::actions::Request {
                    id: "pending".into(),
                    item: serde_json::json!({"type":"approval_request"}),
                    answerable: true,
                    dismissible: false,
                    response_mode: Some("live".into()),
                    approval_detail: None,
                    reason: None,
                });
            })
        });
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert!(
                composer.input.read(cx).value().is_empty(),
                "live native approval blocks recall without waiting for a parent render"
            )
        });
        project.update(visual, |view, cx| {
            view.native
                .actions
                .update(cx, |actions, _| actions.state.requests.clear())
        });
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        composer.update(visual, |composer, cx| {
            composer
                .input
                .update(cx, |input, cx| input.set_selected_range(0..3, cx))
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert_eq!(
                composer.input.read(cx).value().as_str(),
                "second",
                "selected recall must not be replaced with older text"
            )
        });
    }
    #[gpui::test]
    fn prompt_history_respects_soft_wrap_then_uses_resized_visual_edges(cx: &mut TestAppContext) {
        use gpui::EntityInputHandler as _;
        use okilum_t3::projection::{Changes, RowKey, TurnRow};
        cx.update(crate::test_init);
        let temp = tempfile::tempdir().unwrap();
        let long = "one two three four five six seven eight nine ten ".repeat(3);
        let mut project = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = super::super::fixture::view(
                    temp.path().to_owned(),
                    Arc::new(super::super::fixture::Fake::default()),
                    window,
                    cx,
                );
                view.active = false;
                view.compose.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                    input.focus(window, cx);
                });
                view
            });
            let composer = view.read(cx).composer.clone();
            project = Some(view);
            Root::new(composer, window, cx)
        });
        let project = project.unwrap();
        let composer = project.read_with(visual, |view, _| view.composer.clone());
        project.update(visual, |view, cx| view.feed.update(cx, |feed, cx| {
            let key = |id: &str| RowKey { thread: "wrap-history".into(), item: id.into() };
            let row = |id: &str, text: &str| Arc::new(TurnRow { key: key(id), local: true, item: Arc::new(serde_json::json!({"id":id,"type":"user_message","text":text,"status":"completed"})) });
            feed.apply_native(Changes { rows: vec![row("first", "first"), row("last", &long)], order: Some(vec![key("first"), key("last")]), ..Default::default() }, &[], cx);
        }));
        visual.simulate_resize(size(px(260.), px(600.)));
        visual.run_until_parked();
        composer.update_in(visual, |composer, window, cx| {
            composer
                .input
                .update(cx, |input, cx| input.focus(window, cx))
        });
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert!(
                composer.input.read(cx).is_caret_on_visual_edge(false),
                "positive control: empty input is on its top edge before recall"
            )
        });
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert_eq!(composer.input.read(cx).value().as_str(), long.trim());
            assert!(
                !composer.input.read(cx).is_caret_on_visual_edge(false),
                "positive control: recalled prompt ends below its first visual row"
            );
        });
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert_eq!(
                composer.input.read(cx).value().as_str(),
                long.trim(),
                "Up inside a wrapped prompt moves the caret instead of replacing text"
            )
        });
        visual.simulate_resize(size(px(4000.), px(600.)));
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert!(
                composer.input.read(cx).is_caret_on_visual_edge(false),
                "positive control: wide resize puts the same caret on the first row"
            )
        });
        visual.simulate_keystrokes("up");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert_eq!(composer.input.read(cx).value().as_str(), "first")
        });
        composer.update_in(visual, |composer, window, cx| {
            composer.input.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(Some(0..5), "first", Some(5..5), window, cx);
            })
        });
        visual.run_until_parked();
        visual.simulate_keystrokes("down");
        visual.run_until_parked();
        composer.read_with(visual, |composer, cx| {
            assert_eq!(
                composer.input.read(cx).value().as_str(),
                "first",
                "IME candidate navigation must not recall a newer prompt"
            )
        });
    }
}
