//! The delivered punctuation adapter through the mounted shared editor.
//! Recording delegates every decision to CachedProvider; it adds no policy.
use super::*;
use ::core::prelude::v1::test;
use gpui::*;
use gpui_component::{
    input::{Editor, EditorState, Redo, Undo, WrappingIndent},
    Root,
};
use okilum_core::file_editor::{FileEditor, Save};
use sha2::{Digest, Sha256};

type Frame = Arc<Mutex<Option<Arc<dyn SourceProjection>>>>;

struct Recording {
    provider: CachedProvider,
    frame: Frame,
}
impl ProjectionProvider for Recording {
    fn compose(
        &self,
        source: &SourceSnapshot,
        active: &ActiveSource,
    ) -> Option<Arc<dyn SourceProjection>> {
        let frame = self.provider.compose(source, active);
        *self.frame.lock().unwrap() = frame.clone();
        frame
    }
}

struct Surface(Entity<EditorState>);
impl Render for Surface {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(Editor::new(&self.0).size_full())
    }
}

const ORIGINAL: &str = "\u{feff}**\"Привет\" -- ...** `literal -- ...`\r\n\r\nTail\r\n";

fn editor(cx: &mut TestAppContext) -> (Entity<EditorState>, Frame, &mut VisualTestContext) {
    cx.update(|cx| {
        cx.set_reduce_motion(true);
        gpui_component::init(cx);
    });
    let frame = Arc::new(Mutex::new(None));
    let mut input = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let state = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .document_newlines(true)
                .keep_leading_bom(true)
                .soft_wrap(true)
                .searchable(false)
                .wrapping_indent(WrappingIndent::None)
                .line_number(false)
                .folding(false);
            state.set_value(ORIGINAL, window, cx);
            state.set_selected_range(ORIGINAL.len()..ORIGINAL.len(), cx);
            state.enable_layout_diagnostics();
            state.set_projection_provider(
                Some(Arc::new(Recording {
                    provider: CachedProvider::classify(SourceSnapshot {
                        stamp: state.source_stamp(),
                        text: Arc::from(ORIGINAL),
                    }),
                    frame: frame.clone(),
                })),
                cx,
            );
            state.focus_handle(cx).focus(window, cx);
            window.activate_window();
            state
        });
        input = Some(state.clone());
        Root::new(cx.new(|_| Surface(state)), window, cx)
    });
    visual.simulate_resize(size(px(900.), px(450.)));
    draw(visual);
    let input = input.unwrap();
    let current = frame
        .lock()
        .unwrap()
        .clone()
        .expect("actual editor compose");
    assert!(
        current.text().contains("«Привет» — …"),
        "rendered positive control"
    );
    assert!(
        current.text().contains("literal -- ..."),
        "code stays literal"
    );
    input.read_with(visual, |state, _| {
        let counts = state.layout_diagnostics().unwrap();
        assert!(counts.provider_applies > 0 && counts.projection_composes > 0);
        assert!(
            counts.layout_calls > 0,
            "the shared editor actually laid out a frame"
        );
        assert_eq!(current.source().stamp, state.source_stamp());
        assert_eq!(state.value().as_ref(), ORIGINAL);
        // Compose counters alone also increment before a native raw fallback.
        // The actual painted layout must collapse both authored bold markers
        // to the same point. Default Same wrapping disables projection.
        let marker = ORIGINAL.find("**").unwrap();
        let before = state.range_to_bounds(&(marker..marker)).unwrap();
        let after = state.range_to_bounds(&(marker + 2..marker + 2)).unwrap();
        assert_eq!(
            before.origin, after.origin,
            "actual projected layout positive control"
        );
    });
    (input, frame, visual)
}

fn draw(visual: &mut VisualTestContext) {
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.run_until_parked();
}

fn value(input: &Entity<EditorState>, visual: &mut VisualTestContext) -> String {
    input.read_with(visual, |state, _| state.value().to_string())
}

#[gpui::test]
fn punctuation_editor_keyboard_copy_safe_save_and_undo_keep_authored_bytes(
    cx: &mut TestAppContext,
) {
    let (input, _, visual) = editor(cx);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("note.md");
    std::fs::write(&path, ORIGINAL).unwrap();
    let original_hash = Sha256::digest(std::fs::read(&path).unwrap());
    let mut store = FileEditor::open(&path, &directory.path().join("editor-drafts")).unwrap();
    store.set_text(value(&input, visual)).unwrap();
    assert_eq!(store.save().unwrap(), Save::Saved);
    assert_eq!(Sha256::digest(std::fs::read(&path).unwrap()), original_hash);

    input.update(visual, |state, cx| {
        state.set_selected_range(0..ORIGINAL.len(), cx)
    });
    visual.update(|_, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("copy sentinel".to_owned()))
    });
    #[cfg(target_os = "macos")]
    visual.simulate_keystrokes("cmd-c");
    #[cfg(not(target_os = "macos"))]
    visual.simulate_keystrokes("ctrl-c");
    draw(visual);
    visual.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some(ORIGINAL)
        );
    });

    input.update(visual, |state, cx| {
        state.set_selected_range(ORIGINAL.len()..ORIGINAL.len(), cx)
    });
    visual.simulate_input(" -- ...");
    draw(visual);
    let authored = format!("{ORIGINAL} -- ...");
    assert_eq!(value(&input, visual), authored);
    store.set_text(value(&input, visual)).unwrap();
    assert_eq!(store.save().unwrap(), Save::Saved);
    assert_eq!(std::fs::read(&path).unwrap(), authored.as_bytes());
    visual.dispatch_action(Undo);
    draw(visual);
    assert_eq!(
        value(&input, visual),
        ORIGINAL,
        "projection and Save add no history step"
    );
    visual.dispatch_action(Redo);
    draw(visual);
    assert_eq!(value(&input, visual), authored);
    visual.dispatch_action(Undo);
    draw(visual);
    store.set_text(value(&input, visual)).unwrap();
    assert_eq!(store.save().unwrap(), Save::Saved);
    assert_eq!(Sha256::digest(std::fs::read(&path).unwrap()), original_hash);
}

#[gpui::test]
fn punctuation_editor_rendered_hit_reveals_and_edits_the_same_source_position(
    cx: &mut TestAppContext,
) {
    let executor = cx.executor();
    let (input, frame, visual) = editor(cx);
    let start = ORIGINAL.find("...").unwrap();
    let end = start + 3;
    let stamp = input.read_with(visual, |state, _| state.source_stamp());
    let bounds = input.read_with(visual, |state, _| state.input_bounds());
    let native_range =
        ORIGINAL[..start].encode_utf16().count()..ORIGINAL[..end].encode_utf16().count();
    let glyph = visual
        .update(|window, cx| {
            input.update(cx, |state, cx| {
                EntityInputHandler::bounds_for_range(state, native_range, bounds, window, cx)
            })
        })
        .expect("the projected punctuation has real layout geometry");
    assert!(glyph.size.width > px(0.) && glyph.size.height > px(0.));
    let hit = input.read_with(visual, |state, _| {
        state.source_offset_for_point(glyph.center())
    });
    assert!(
        hit == Some(start) || hit == Some(end),
        "the painted glyph maps to authored edges: {hit:?}"
    );
    visual.simulate_click(glyph.center(), Modifiers::none());
    draw(visual);
    let at = input.read_with(visual, |state, _| {
        assert_eq!(
            state.source_stamp(),
            stamp,
            "reveal must not mutate canonical text"
        );
        assert!(state.selected_range().is_empty());
        state.selected_range().start
    });
    assert!(
        at == start || at == end,
        "the glyph hit snaps to its authored edges: {at}"
    );
    // The shared editor pins the painted mapping for 500 ms after a press,
    // so double-clicks share one geometry. Reveal follows normal lease expiry.
    executor.advance_clock(std::time::Duration::from_millis(600));
    draw(visual);
    assert_eq!(
        frame.lock().unwrap().as_ref().unwrap().text(),
        ORIGINAL.replace("`literal -- ...`", "literal -- ..."),
        "punctuation reveals; untouched inline code retains its own policy"
    );
    visual.simulate_input("X");
    draw(visual);
    let mut expected = ORIGINAL.to_owned();
    expected.insert(at, 'X');
    assert_eq!(value(&input, visual), expected);
    visual.dispatch_action(Undo);
    draw(visual);
    assert_eq!(value(&input, visual), ORIGINAL);
}
