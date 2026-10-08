//! Exercise shared input navigation through the actual rendered component.
use gpui::*;
use gpui_component::{
    input::{Input, InputState},
    Root,
};

struct MaskedFixture(Entity<InputState>);
impl Render for MaskedFixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child(Input::new(&self.0))
    }
}

#[gpui::test]
fn masked_unicode_home_end_use_source_boundaries(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_reduce_motion(true);
        gpui_component::init(cx);
    });
    let mut input = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let state = cx.new(|cx| {
            let mut state = InputState::new(window, cx).masked(true);
            state.set_value("😀😀", window, cx);
            state.focus_handle(cx).focus(window, cx);
            state
        });
        input = Some(state.clone());
        let fixture = cx.new(|_| MaskedFixture(state));
        Root::new(fixture, window, cx)
    });
    let input = input.unwrap();
    visual.run_until_parked();
    visual.simulate_keystrokes("home end");
    input.read_with(visual, |state, _| {
        assert_eq!(state.cursor(), "😀😀".len());
        assert_eq!(state.value().as_ref(), "😀😀");
    });
    visual.simulate_input("X");
    input.read_with(visual, |state, _| {
        assert_eq!(state.value().as_ref(), "😀😀X")
    });
    visual.simulate_keystrokes("home");
    input.read_with(visual, |state, _| assert_eq!(state.cursor(), 0));
}
