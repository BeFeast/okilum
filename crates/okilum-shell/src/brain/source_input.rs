//! One source engine chosen at view construction; mode changes never replace it.
use gpui::{
    px, AnyElement, App, AppContext, Entity, EntityId, EntityInputHandler, Focusable, IntoElement,
    SharedString, Styled, Window,
};
use gpui_component::input::projection::{
    ActiveSource, ProjectionProvider, SourceProjection, SourceSnapshot, SourceStamp,
};
use gpui_component::input::{Editor, EditorState, Textarea, TextareaState, WrappingIndent};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LinkSelection {
    pub bytes: std::ops::Range<usize>,
    pub utf16: std::ops::Range<usize>,
    pub reversed: bool,
    pub text: String,
}

#[derive(Clone)]
pub(super) enum SourceInput {
    Legacy(Entity<TextareaState>),
    Managed(Entity<EditorState>),
}
struct SourceOnly;
impl ProjectionProvider for SourceOnly {
    fn compose(&self, _: &SourceSnapshot, _: &ActiveSource) -> Option<Arc<dyn SourceProjection>> {
        None
    }
}
impl SourceInput {
    pub fn new(managed: bool, window: &mut Window, cx: &mut App) -> Self {
        if !managed {
            return Self::Legacy(cx.new(|cx| {
                TextareaState::new(window, cx)
                    .rows(18)
                    .soft_wrap(false)
                    .placeholder("Choose a note to view or edit")
            }));
        }
        let clipboard = cx
            .try_global::<crate::platform::ManagedClipboard>()
            .map(|provider| provider.0.clone());
        Self::Managed(cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .line_number(false)
                .folding(false)
                .searchable(false)
                .replaceable(false)
                .soft_wrap(true)
                .wrapping_indent(WrappingIndent::None)
                .placeholder("Choose a note to view or edit");
            // Source starts with the accepted native hooks, before any classifier result.
            state.set_projection_provider(Some(Arc::new(SourceOnly)), cx);
            state.set_exact_clipboard_provider(clipboard, cx);
            state.set_readonly(true, cx);
            state
        }))
    }
    pub fn managed(&self) -> Option<&Entity<EditorState>> {
        match self {
            Self::Managed(state) => Some(state),
            _ => None,
        }
    }
    pub fn link_selection(&self, window: &mut Window, cx: &mut App) -> Option<LinkSelection> {
        self.managed()?.update(cx, |state, cx| {
            if state.marked_text_range(window, cx).is_some() {
                return None;
            }
            let bytes = state.selected_range();
            let native = state.selected_text_range(false, window, cx)?;
            let text = state.value().get(bytes.clone())?.to_owned();
            Some(LinkSelection {
                bytes,
                utf16: native.range,
                reversed: native.reversed,
                text,
            })
        })
    }
    /// Caller must check the current BrainView write/recovery policy as well:
    /// the widget's programmatic replace method permits read-only edits.
    pub fn replace_link_selection(
        &self,
        expected: &LinkSelection,
        text: &str,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        if self.link_selection(window, cx).as_ref() != Some(expected) {
            return false;
        }
        let Some(editor) = self.managed() else {
            return false;
        };
        editor.update(cx, |state, cx| {
            if !state.is_editable() {
                return false;
            }
            let range = if expected.reversed {
                expected.bytes.end..expected.bytes.start
            } else {
                expected.bytes.clone()
            };
            state.set_selected_range(range, cx);
            state.replace(text.to_owned(), window, cx);
            true
        })
    }
    pub fn value(&self, cx: &App) -> SharedString {
        match self {
            Self::Legacy(state) => state.read(cx).value(),
            Self::Managed(state) => state.read(cx).value(),
        }
    }
    pub fn reset(&self, value: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
        let value = value.into();
        match self {
            Self::Legacy(state) => state.update(cx, |state, cx| state.set_value(value, window, cx)),
            Self::Managed(state) => {
                state.update(cx, |state, cx| state.set_value(value, window, cx))
            }
        }
    }
    pub fn stamp(&self, cx: &App) -> Option<SourceStamp> {
        self.managed().map(|state| state.read(cx).source_stamp())
    }
    pub fn entity_id(&self) -> EntityId {
        match self {
            Self::Legacy(state) => state.entity_id(),
            Self::Managed(state) => state.entity_id(),
        }
    }
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        match self {
            Self::Legacy(state) => state.focus_handle(cx).focus(window, cx),
            Self::Managed(state) => state.focus_handle(cx).focus(window, cx),
        }
    }
    pub fn set_readonly(&self, readonly: bool, cx: &mut App) {
        match self {
            Self::Legacy(state) => state.update(cx, |state, cx| state.set_readonly(readonly, cx)),
            Self::Managed(state) => state.update(cx, |state, cx| state.set_readonly(readonly, cx)),
        }
    }
    pub fn render(&self, readonly: bool) -> AnyElement {
        match self {
            Self::Legacy(state) => Textarea::new(state)
                .readonly(readonly)
                .h(px(300.))
                .into_any_element(),
            Self::Managed(state) => Editor::new(state)
                .readonly(readonly)
                // A note has no language server (#1010).
                .context_menu(move |menu, _, _| crate::reader_code_file::text_menu(menu, !readonly))
                .font_family(super::source_projection::BODY_FONT)
                .text_size(px(16.))
                .h(px(300.))
                .into_any_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::{Context, EntityInputHandler, Render, TestAppContext};

    struct Harness(SourceInput);
    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.0.render(false)
        }
    }

    #[gpui::test]
    fn managed_ordinary_paste_preserves_exact_bytes_and_one_undo(cx: &mut TestAppContext) {
        use gpui_component::input::clipboard::{
            ClipboardReadError, ClipboardReadRequest, ExactClipboardProvider,
        };
        struct Exact;
        impl ExactClipboardProvider for Exact {
            fn read_text(
                &self,
                _: ClipboardReadRequest,
                _: &App,
            ) -> gpui::Task<Result<Option<String>, ClipboardReadError>> {
                gpui::Task::ready(Ok(Some("Привет 🧠\r\nlast\n".into())))
            }
        }
        cx.update(gpui_component::init);
        cx.update(|cx| cx.set_global(crate::platform::ManagedClipboard(Arc::new(Exact))));
        let (view, cx) = cx.add_window_view(|window, cx| {
            let source = SourceInput::new(true, window, cx);
            source.reset("", window, cx);
            source.set_readonly(false, cx);
            source.focus(window, cx);
            window.activate_window();
            Harness(source)
        });
        cx.run_until_parked();
        #[cfg(not(target_os = "macos"))]
        cx.simulate_keystrokes("ctrl-v");
        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes("cmd-v");
        cx.run_until_parked();
        view.update_in(cx, |v, _, cx| {
            assert_eq!(v.0.value(cx).as_ref(), "Привет 🧠\r\nlast\n")
        });
        #[cfg(not(target_os = "macos"))]
        cx.simulate_keystrokes("ctrl-z");
        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes("cmd-z");
        view.update_in(cx, |v, _, cx| assert_eq!(v.0.value(cx).as_ref(), ""));
        #[cfg(not(target_os = "macos"))]
        cx.simulate_keystrokes("ctrl-y");
        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes("cmd-shift-z");
        view.update_in(cx, |v, _, cx| {
            assert_eq!(v.0.value(cx).as_ref(), "Привет 🧠\r\nlast\n")
        });
    }

    #[gpui::test]
    fn missing_raw_clipboard_never_inserts_normalized_sentinel_and_modes_keep_history(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let source = SourceInput::new(true, window, cx);
            source.reset("original\r\n", window, cx);
            source.set_readonly(false, cx);
            source.focus(window, cx);
            Harness(source)
        });
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let subscription = view.update_in(cx, |v, _, cx| {
            let events = events.clone();
            cx.subscribe(
                v.0.managed().unwrap(),
                move |_, _, event: &gpui_component::input::clipboard::ClipboardPasteEvent, _| {
                    events.borrow_mut().push(format!("{event:?}"));
                },
            )
        });
        cx.update(|window, cx| {
            window.activate_window();
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                "NORMALIZED SENTINEL".into(),
            ));
        });
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| {
            assert!(
                window.is_window_active(),
                "clipboard requires an active window"
            );
            assert!(v.0.managed().unwrap().read(cx).is_editable());
            assert!(v.0.managed().unwrap().focus_handle(cx).is_focused(window));
        });
        #[cfg(not(target_os = "macos"))]
        cx.simulate_keystrokes("ctrl-v");
        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes("cmd-v");
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| {
            assert_eq!(v.0.value(cx).as_ref(), "original\r\n");
            assert!(
                events
                    .borrow()
                    .iter()
                    .any(|event| event.contains("Unsupported")),
                "{events:?}"
            );
            v.0.managed().unwrap().update(cx, |s, cx| {
                s.replace_text_in_range(Some(0..0), "positive ", window, cx);
                let stamp = s.source_stamp();
                s.set_projection_provider(Some(Arc::new(SourceOnly)), cx);
                s.set_projection_provider(None, cx);
                assert_eq!(s.source_stamp(), stamp);
            });
        });
        #[cfg(not(target_os = "macos"))]
        cx.simulate_keystrokes("ctrl-z");
        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes("cmd-z");
        view.update_in(cx, |v, _, cx| {
            assert_eq!(v.0.value(cx).as_ref(), "original\r\n")
        });
        #[cfg(not(target_os = "macos"))]
        cx.simulate_keystrokes("ctrl-y");
        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes("cmd-shift-z");
        view.update_in(cx, |v, _, cx| {
            assert_eq!(v.0.value(cx).as_ref(), "positive original\r\n")
        });
        drop(subscription);
    }
    #[gpui::test]
    fn note_link_replaces_backward_unicode_selection_atomically_and_refuses_ime(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let original = "---\r\ntype: Note\r\n---\r\nBefore 😀План after\r\n";
        let (view, cx) = cx.add_window_view(|window, cx| {
            let source = SourceInput::new(true, window, cx);
            source.reset(original, window, cx);
            source.set_readonly(false, cx);
            source.focus(window, cx);
            Harness(source)
        });
        cx.run_until_parked();
        let selected = view.update_in(cx, |v, window, cx| {
            let start = original.find("😀").unwrap();
            let end = start + "😀План".len();
            v.0.managed()
                .unwrap()
                .update(cx, |s, cx| s.set_selected_range(end..start, cx));
            let selected = v.0.link_selection(window, cx).unwrap();
            assert_eq!(selected.text, "😀План");
            assert!(selected.reversed);
            assert_eq!(selected.utf16.end - selected.utf16.start, 6);
            assert!(v
                .0
                .replace_link_selection(&selected, "[[b/target.md|😀План]]", window, cx));
            assert_eq!(
                v.0.value(cx).as_ref(),
                original.replace("😀План", "[[b/target.md|😀План]]")
            );
            // The accepted source and projection share history across mode changes.
            v.0.managed().unwrap().update(cx, |s, cx| {
                s.set_projection_provider(None, cx);
                s.set_projection_provider(Some(Arc::new(SourceOnly)), cx);
            });
            selected
        });
        #[cfg(not(target_os = "macos"))]
        cx.simulate_keystrokes("ctrl-z");
        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes("cmd-z");
        view.update_in(cx, |v, window, cx| {
            assert_eq!(v.0.value(cx).as_ref(), original);
            assert_eq!(v.0.link_selection(window, cx), Some(selected.clone()));
        });
        #[cfg(not(target_os = "macos"))]
        cx.simulate_keystrokes("ctrl-y");
        #[cfg(target_os = "macos")]
        cx.simulate_keystrokes("cmd-shift-z");
        view.update_in(cx, |v, window, cx| {
            assert_eq!(
                v.0.value(cx).as_ref(),
                original.replace("😀План", "[[b/target.md|😀План]]")
            );
            assert!(!v.0.replace_link_selection(&selected, "WRONG", window, cx));
            let current = v.0.link_selection(window, cx).unwrap();
            v.0.set_readonly(true, cx);
            assert!(!v.0.replace_link_selection(&current, "READONLY", window, cx));
            v.0.set_readonly(false, cx);
            v.0.managed().unwrap().update(cx, |s, cx| {
                s.replace_and_mark_text_in_range(None, "中", Some(0..1), window, cx)
            });
            let composing = v.0.value(cx);
            assert!(v.0.link_selection(window, cx).is_none());
            assert!(!v.0.replace_link_selection(&current, "IME", window, cx));
            assert_eq!(v.0.value(cx), composing);
        });
    }
}
