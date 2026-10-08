//! Isolated shared-editor acceptance fixture. Never reads or saves user notes.
#[allow(dead_code)] // Shared module contains app-only palette and diagnostics APIs.
#[path = "../src/source_presentation.rs"]
mod adapter;
use gpui::*;
use gpui_component::{
    input::{
        projection::{ProjectionProvider, SourceSnapshot},
        Editor, EditorState, WrappingIndent,
    },
    Root,
};
use std::sync::Arc;

const TEXT: &str = "abc שלום xyz\nשלום abc עולם\n**שלום** and русский текст\n";
actions!(bidi_fixture, [Record, Toggle]);
struct Fixture {
    editor: Entity<EditorState>,
    live: bool,
    _subscription: Subscription,
}
impl Fixture {
    fn record(&self, cx: &App) {
        let editor = self.editor.read(cx);
        println!(
            "{}",
            serde_json::json!({"cursor": editor.cursor(), "selection": editor.selected_range(),
            "source": editor.value(), "live": self.live})
        );
    }
}
impl Render for Fixture {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .p_4()
            .key_context("BidiFixture")
            .on_action(cx.listener(|this, _: &Record, _, cx| this.record(cx)))
            .on_action(cx.listener(|this, _: &Toggle, _, cx| {
                this.live = !this.live;
                let provider = if this.live {
                    let editor = this.editor.read(cx);
                    Some(Arc::new(adapter::CachedProvider::classify(SourceSnapshot {
                        stamp: editor.source_stamp(),
                        text: Arc::from(editor.value().as_ref()),
                    })) as Arc<dyn ProjectionProvider>)
                } else {
                    None
                };
                this.editor.update(cx, |editor, cx| {
                    editor.set_projection_provider(provider, cx);
                });
                this.record(cx);
            }))
            .child(
                Editor::new(&self.editor)
                    .font_family("Noto Sans")
                    .text_size(px(24.))
                    .h(relative(1.0)),
            )
    }
}
fn main() {
    gpui_platform::application()
        .with_assets(gpui_kit_assets::Assets)
        .run(|cx| {
            gpui_component::init(cx);
            cx.bind_keys([
                KeyBinding::new("ctrl-alt-r", Record, None),
                KeyBinding::new("ctrl-alt-l", Toggle, None),
            ]);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(60.), px(50.)),
                        size(px(800.), px(500.)),
                    ))),
                    ..Default::default()
                },
                |window, cx| {
                    window.set_window_title("Tessera bidi #737");
                    let editor = cx.new(|cx| {
                        let mut editor = EditorState::new(window, cx)
                            .line_number(false)
                            .folding(false)
                            .searchable(false)
                            .soft_wrap(true)
                            .wrapping_indent(WrappingIndent::None);
                        editor.set_value(TEXT, window, cx);
                        editor
                    });
                    editor.read(cx).focus_handle(cx).focus(window, cx);
                    let fixture = cx.new(|cx| {
                        let live = std::env::var_os("TESSERA_BIDI_LIVE").is_some();
                        if live {
                            let state = editor.read(cx);
                            let provider =
                                Arc::new(adapter::CachedProvider::classify(SourceSnapshot {
                                    stamp: state.source_stamp(),
                                    text: Arc::from(state.value().as_ref()),
                                }));
                            editor.update(cx, |state, cx| {
                                state.set_projection_provider(Some(provider), cx);
                            });
                        }
                        let subscription =
                            cx.observe(&editor, |this: &mut Fixture, _, cx| this.record(cx));
                        Fixture {
                            editor,
                            live,
                            _subscription: subscription,
                        }
                    });
                    let recorded = fixture.clone();
                    cx.on_action(move |_: &Record, cx| recorded.read(cx).record(cx));
                    cx.new(|cx| Root::new(fixture, window, cx))
                },
            )
            .unwrap();
            cx.activate(true);
        });
}
