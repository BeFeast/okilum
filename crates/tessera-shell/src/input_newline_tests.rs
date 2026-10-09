//! Document newline policy through the rendered shared editor and history.
use ::core::prelude::v1::test;
use gpui::*;
use gpui_component::{
    input::{Editor, EditorState},
    Root,
};

struct NewlineFixture(Entity<EditorState>);
impl Render for NewlineFixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child(Editor::new(&self.0))
    }
}

#[gpui::test]
fn crlf_enter_paste_and_undo_preserve_document_bytes(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_reduce_motion(true);
        gpui_component::init(cx);
    });
    let original = "alpha\r\nשלום\r\n";
    let mut editor = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let state = cx.new(|cx| {
            let mut state = EditorState::new(window, cx);
            state.set_value(original, window, cx);
            state.focus_handle(cx).focus(window, cx);
            state
        });
        editor = Some(state.clone());
        let fixture = cx.new(|_| NewlineFixture(state));
        Root::new(fixture, window, cx)
    });
    let editor = editor.unwrap();
    visual.run_until_parked();
    visual.simulate_keystrokes("ctrl-end enter");
    editor.read_with(visual, |state, _| {
        assert_eq!(state.value().as_ref(), format!("{original}\r\n"));
    });
    visual.simulate_keystrokes("ctrl-z");
    editor.read_with(visual, |state, _| {
        assert_eq!(state.value().as_ref(), original)
    });
    visual.update(|_, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("first\n😀\r\nlast".to_owned()));
    });
    visual.simulate_keystrokes("ctrl-v");
    visual.run_until_parked();
    editor.read_with(visual, |state, _| {
        assert_eq!(
            state.value().as_ref(),
            format!("{original}first\r\n😀\r\nlast")
        );
    });
    visual.simulate_keystrokes("ctrl-z");
    editor.read_with(visual, |state, _| {
        assert_eq!(state.value().as_ref(), original)
    });
}
