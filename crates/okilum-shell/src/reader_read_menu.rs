//! Text and background context actions for the Read surface (#2025).
use super::*;
use gpui_component::menu::{PopupMenu, PopupMenuItem};

mod commands;
use commands::{commands, Command};

/// Copy payloads are captured when the menu opens. Navigation or an external
/// reload while it is open cannot substitute a different document's contents.
pub(super) fn menu(menu: PopupMenu, reader: &WeakEntity<Reader>, cx: &App) -> PopupMenu {
    build(menu, reader, cx, false)
}

/// Selection formats join an object's existing popup, rather than opening
/// another menu for the same press. Background commands stay on the surface.
pub(super) fn selection(menu: PopupMenu, reader: &WeakEntity<Reader>, cx: &App) -> PopupMenu {
    build(menu, reader, cx, true)
}

fn build(menu: PopupMenu, reader: &WeakEntity<Reader>, cx: &App, object: bool) -> PopupMenu {
    let Some(entity) = reader.upgrade() else {
        return menu;
    };
    let state = entity.read(cx);
    if state.editing.is_some()
        || state.file_preview.is_some()
        || (!object && state.pointer_on_link())
    {
        return menu;
    }
    let content = state.content.read(cx);
    let plain = content.selected_text_as(SelectionFormat::Plain);
    let markdown = content.selected_text_as(SelectionFormat::Source);
    let document = content.document_source();
    let has_selection = !plain.is_empty() || !markdown.is_empty();
    if object && !has_selection {
        return menu;
    }
    let mut menu = menu;
    for &command in commands(has_selection) {
        let payload = match command {
            Command::Plain => Some(plain.clone()),
            Command::Markdown => Some(markdown.clone()),
            Command::Document => document.as_ref().map(ToString::to_string),
            Command::SelectAll => None,
        };
        let reader = reader.clone();
        let generation = state.navigation.preparation_generation;
        menu = menu.item(
            PopupMenuItem::element(move |_, _| {
                div()
                    .debug_selector(move || format!("read-menu-{}", command.label()))
                    .child(command.label())
            })
            .disabled(command == Command::Document && payload.is_none())
            .on_click(move |_, _, cx| {
                if command == Command::SelectAll {
                    let _ = reader.update(cx, |reader, cx| {
                        if reader.navigation.preparation_generation == generation {
                            reader
                                .content
                                .update(cx, |content, cx| content.select_all(cx));
                        }
                    });
                } else if let Some(text) = &payload {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                }
            }),
        );
    }
    menu
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn background_and_selection_menu_write_captured_clipboard_bytes(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let body = "  **café 雪**\r\n\r\nTail.\r\n";
        std::fs::write(root.join("note.md"), body).unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("note.md".into()),
                        session_directory: Some(dir.path().join("state")),
                        index_dir: Some(dir.path().join("index")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.simulate_resize(size(px(1400.), px(960.)));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let bounds = visual
            .debug_bounds("reader-text")
            .expect("Read surface rendered");
        let at = point(bounds.center().x, bounds.bottom() - px(20.));
        let right_click = |visual: &mut gpui::VisualTestContext| {
            visual.simulate_mouse_move(at, None, Modifiers::default());
            visual.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
            visual.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
            visual.run_until_parked();
        };
        visual.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into())));
        right_click(visual);
        // Background: Select all, Copy entire document as Markdown.
        visual.simulate_keystrokes("down down enter");
        visual.run_until_parked();
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some(body)
            )
        });
        reader.read_with(visual, |reader, cx| {
            assert!(reader.content.read(cx).selected_text().is_empty())
        });

        right_click(visual);
        visual.simulate_keystrokes("down enter");
        visual.run_until_parked();
        let selected =
            reader.read_with(visual, |reader, cx| reader.content.read(cx).selected_text());
        assert!(
            !selected.is_empty(),
            "positive control: Select all selected the document"
        );
        right_click(visual);
        // Selection: Copy, Copy as Markdown.
        visual.simulate_keystrokes("down down enter");
        visual.run_until_parked();
        visual.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some(body)
            )
        });
        assert_eq!(
            reader.read_with(visual, |reader, cx| reader.content.read(cx).selected_text()),
            selected
        );
        assert_eq!(
            std::fs::read(root.join("note.md")).unwrap(),
            body.as_bytes()
        );
    }
}
