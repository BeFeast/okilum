//! Text and background context actions for the Read surface (#2025).
use super::*;
use gpui_component::menu::{PopupMenu, PopupMenuItem};

gpui::actions!(reader_read_menu, [CopyAsMarkdown, CopyWithFormatting]);

pub(super) const COPY_MARKDOWN_KEY: &str = "secondary-shift-c";

impl Reader {
    pub(super) fn read_copy_available(&self) -> bool {
        self.editing.is_none()
            && self.file_preview.is_none()
            && !self.selected_file().is_empty()
            && !self.typed_navigation.active.get()
            && !self.tree_preview.is_open()
            && self
                .active_timeline()
                .is_none_or(|timeline| timeline.selected.is_none())
    }

    pub(super) fn copy_read_markdown(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self
            .editing
            .as_ref()
            .and_then(|editing| editing.selected_markdown(window, cx))
        {
            if !text.is_empty() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            return;
        }
        if !self.read_copy_available() {
            return;
        }
        if let Some(snapshot) = snapshot(self, cx) {
            snapshot.copy_format(false, window, cx);
        }
    }

    pub(super) fn copy_read_formatted(&self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.read_copy_available() {
            return;
        }
        if let Some(snapshot) = snapshot(self, cx) {
            snapshot.copy_format(true, window, cx);
        }
    }
}

mod commands;
#[cfg(test)]
mod rich_tests;
use commands::{commands, Command};

/// Copy payloads are captured when the menu opens. Navigation or an external
/// reload while it is open cannot substitute a different document's contents.
pub(super) fn menu(menu: PopupMenu, reader: &WeakEntity<Reader>, cx: &App) -> PopupMenu {
    let Some(entity) = reader.upgrade() else {
        return menu;
    };
    let state = entity.read(cx);
    if state.pointer_on_link() {
        return menu;
    }
    build(menu, reader, snapshot(state, cx), false)
}

/// Payloads can also be captured from an already-borrowed Reader while an
/// object popup is created. Its builder must not re-borrow the Reader entity.
#[derive(Clone)]
pub(super) struct Snapshot {
    plain: String,
    markdown: String,
    document: Option<SharedString>,
    generation: u64,
    rich_markdown: bool,
}

impl Snapshot {
    pub(super) fn copy_format(&self, formatted: bool, window: &mut Window, cx: &mut App) {
        if formatted {
            if self.rich_markdown && self.has_selection() {
                self.copy(Command::Rich, window, cx);
            }
        } else {
            self.copy(Command::Markdown, window, cx);
        }
    }

    fn has_selection(&self) -> bool {
        !self.plain.is_empty() || !self.markdown.is_empty()
    }

    fn copy(&self, command: Command, window: &mut Window, cx: &mut App) {
        let text = match command {
            Command::Plain | Command::Rich => Some(self.plain.as_str()),
            Command::Markdown if self.has_selection() => Some(self.markdown.as_str()),
            Command::Markdown | Command::Document => self.document.as_deref(),
            Command::SelectAll => None,
        };
        if let Some(text) = text {
            if command == Command::Rich {
                platform::rich_clipboard::copy(text.to_owned(), self.markdown.clone(), window, cx);
            } else {
                cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
            }
        }
    }
}

pub(super) fn snapshot(state: &Reader, cx: &App) -> Option<Snapshot> {
    if state.editing.is_some() || state.file_preview.is_some() {
        return None;
    }
    let content = state.content.read(cx);
    Some(Snapshot {
        plain: content.selected_text_as(SelectionFormat::Plain),
        markdown: content.selected_text_as(SelectionFormat::Source),
        document: content.document_source(),
        generation: state.navigation.preparation_generation,
        rich_markdown: !state.use_html,
    })
}

/// Selection formats join an object's existing popup, rather than opening
/// another menu for the same press. Background commands stay on the surface.
pub(super) fn selection(
    menu: PopupMenu,
    reader: &WeakEntity<Reader>,
    snapshot: Option<Snapshot>,
) -> PopupMenu {
    build(menu, reader, snapshot, true)
}

fn build(
    menu: PopupMenu,
    reader: &WeakEntity<Reader>,
    snapshot: Option<Snapshot>,
    object: bool,
) -> PopupMenu {
    let Some(snapshot) = snapshot else {
        return menu;
    };
    let has_selection = snapshot.has_selection();
    if object && !has_selection {
        return menu;
    }
    let mut menu = menu;
    for &command in commands(has_selection) {
        let snapshot = snapshot.clone();
        let reader = reader.clone();
        let hint_reader = reader.clone();
        menu = menu.item(
            PopupMenuItem::element(move |_, cx| {
                let editing = hint_reader
                    .upgrade()
                    .is_some_and(|reader| reader.read(cx).editing.is_some());
                let hints =
                    |action: &dyn Action| reader_shortcuts::content_shortcuts(action, editing, cx);
                h_flex()
                    .debug_selector(move || format!("read-menu-{}", command.label()))
                    .child(div().flex_1().child(command.label()))
                    .child(match command {
                        Command::Plain => hints(&gpui_component::input::Copy),
                        Command::Markdown | Command::Document => hints(&CopyAsMarkdown),
                        Command::Rich => hints(&CopyWithFormatting),
                        Command::SelectAll => hints(&gpui_component::input::SelectAll),
                    })
            })
            .disabled(
                (command == Command::Document && snapshot.document.is_none())
                    || (command == Command::Rich && !snapshot.rich_markdown),
            )
            .on_click(move |_, window, cx| {
                if command == Command::SelectAll {
                    let _ = reader.update(cx, |reader, cx| {
                        if reader.navigation.preparation_generation == snapshot.generation {
                            reader
                                .content
                                .update(cx, |content, cx| content.select_all(cx));
                        }
                    });
                } else {
                    snapshot.copy(command, window, cx);
                }
            }),
        );
    }
    menu
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

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
            .debug_bounds("reader-column")
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
