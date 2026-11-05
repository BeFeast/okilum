use super::*;
use ::core::prelude::v1::test;
use platform::rich_clipboard::{self, Payload};
use std::{cell::RefCell, rc::Rc};

const NOTE: &str = "# Title\r\n\r\nA **café 雪** and *italic* with [Example](https://example.test).\r\n\r\n```rs\r\n    let a = \"<&>\";\r\n```\r\n\r\nAfter.\r\n\r\n- First\r\n- Second\r\n";
struct Fixture<'a> {
    reader: Entity<Reader>,
    visual: &'a mut VisualTestContext,
    seen: Rc<RefCell<Vec<Payload>>>,
    root: PathBuf,
    _dir: tempfile::TempDir,
}
fn open(cx: &mut TestAppContext) -> Fixture<'_> {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("vault");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("note.md"), NOTE).unwrap();
    let seen = Rc::new(RefCell::new(Vec::new()));
    cx.update(|cx| {
        gpui_component::init(cx);
        bind_keys(cx);
        cx.set_reduce_motion(true);
        let capture = seen.clone();
        rich_clipboard::test_sink(
            move |payload| {
                capture.borrow_mut().push(payload.clone());
                Ok(())
            },
            cx,
        );
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
    visual.simulate_resize(size(px(1400.), px(960.)));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    Fixture {
        reader: entity.unwrap(),
        visual,
        seen,
        root,
        _dir: dir,
    }
}
impl Fixture<'_> {
    fn seed_clipboard(&mut self) {
        self.visual
            .update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into())));
    }
    fn focus_text(&mut self) {
        self.reader.update_in(self.visual, |r, window, cx| {
            let focus = r.content.read(cx).focus_handle().clone();
            focus.focus(window, cx);
        });
    }
    fn palette_copy(&mut self, formatted: bool) {
        self.focus_text();
        self.visual.simulate_keystrokes("secondary-k");
        self.visual.simulate_input(if formatted {
            "copy as rich text"
        } else {
            "copy as markdown"
        });
        self.visual.run_until_parked();
        self.seed_clipboard();
        self.visual.simulate_keystrokes("secondary-shift-c");
        self.visual.run_until_parked();
        assert_eq!(
            self.clipboard(),
            "sentinel",
            "Read shortcut excludes palette input"
        );
        self.visual.update(|window, cx| window.draw(cx).clear(cx));
        self.pick(if formatted {
            "palette-copy-formatted"
        } else {
            "palette-copy-markdown"
        });
        assert!(!self.reader.read_with(self.visual, |r, _| r.quick_open.open));
    }
    fn background(&mut self) -> Point<Pixels> {
        let bounds = self
            .visual
            .debug_bounds("reader-column")
            .expect("Read column mounted");
        point(bounds.center().x, bounds.bottom() - px(20.))
    }
    fn right_click(&mut self, at: Point<Pixels>) {
        self.visual
            .simulate_mouse_move(at, None, Modifiers::default());
        self.visual.run_until_parked();
        self.visual
            .simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
        self.visual
            .simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
        self.visual.run_until_parked();
        self.visual.update(|window, cx| window.draw(cx).clear(cx));
    }
    fn pick(&mut self, selector: &'static str) {
        let bounds = self.visual.debug_bounds(selector).expect(selector);
        self.visual
            .simulate_click(bounds.center(), Modifiers::default());
        self.visual.run_until_parked();
    }
    fn select_all(&mut self) -> String {
        // Keyboard and palette copy can retain selection. Start each independent
        // menu scenario on the background, which offers Select all.
        self.reader.update(self.visual, |r, cx| {
            r.content
                .update(cx, |content, cx| content.clear_selection(cx));
        });
        self.visual.run_until_parked();
        let at = self.background();
        self.right_click(at);
        self.pick("read-menu-Select all");
        self.reader.read_with(self.visual, |r, cx| {
            r.content.read(cx).selected_text_as(SelectionFormat::Plain)
        })
    }
    fn clipboard(&mut self) -> String {
        self.visual
            .update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap())
    }
    fn link(&mut self) -> Point<Pixels> {
        let paragraph = self.reader.read_with(self.visual, |r, cx| {
            r.content.read(cx).list_state().bounds_for_item(1).unwrap()
        });
        let y = paragraph.top() + px(BODY_FONT_SIZE * 0.8);
        (0..140)
            .map(|i| point(paragraph.left() + px(i as f32 * 6.), y))
            .find(|at| {
                self.visual
                    .simulate_mouse_move(*at, None, Modifiers::default());
                self.visual.run_until_parked();
                self.reader.read_with(self.visual, |r, _| {
                    r.pointer_link
                        .as_ref()
                        .is_some_and(|(url, _)| url.contains("example.test"))
                })
            })
            .expect("positive control: rendered link hit detected")
    }
}

#[gpui::test]
fn markdown_copy_uses_a_partial_rendered_selection_instead_of_the_document(
    cx: &mut TestAppContext,
) {
    let mut f = open(cx);
    let paragraph = f.reader.read_with(f.visual, |r, cx| {
        r.content.read(cx).list_state().bounds_for_item(1).unwrap()
    });
    let start = point(
        paragraph.left() + px(2.),
        paragraph.top() + px(BODY_FONT_SIZE * 0.8),
    );
    let end = point(paragraph.right() - px(2.), start.y);
    let select = |f: &mut Fixture<'_>| {
        f.visual
            .simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        f.visual
            .simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
        f.visual
            .simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        f.visual.run_until_parked();
    };
    select(&mut f);
    let (plain, markdown) = f.reader.read_with(f.visual, |r, cx| {
        let content = r.content.read(cx);
        (
            content.selected_text_as(SelectionFormat::Plain),
            content.selected_text_as(SelectionFormat::Source),
        )
    });
    assert!(
        plain.contains("café 雪") && !plain.contains("Title") && !plain.contains("After"),
        "positive control: paragraph-only selection: {plain:?}"
    );
    assert!(markdown.contains("**café 雪**") && markdown.contains("https://example.test"));
    assert_ne!(markdown, NOTE);
    let at = f.background();
    f.right_click(at);
    f.pick("read-menu-Copy as Markdown");
    assert_eq!(f.clipboard(), markdown);
    select(&mut f);
    f.visual.simulate_keystrokes("secondary-c");
    f.visual.run_until_parked();
    let plain_copy = f.clipboard();
    // The existing keyboard Copy trims the renderer's final block separator;
    // the explicit context-menu snapshot intentionally keeps it.
    assert_eq!(plain_copy, plain.trim_end_matches('\n'));
    f.seed_clipboard();
    f.visual.simulate_keystrokes("secondary-shift-c");
    f.visual.run_until_parked();
    assert_eq!(f.clipboard(), markdown);
    f.visual.simulate_keystrokes("secondary-c");
    f.visual.run_until_parked();
    assert_eq!(f.clipboard(), plain_copy);
    select(&mut f);
    f.palette_copy(false);
    assert_eq!(f.clipboard(), markdown);
}

#[gpui::test]
fn markdown_shortcut_and_palette_match_menu_and_plain_copy_stays_plain(cx: &mut TestAppContext) {
    let mut f = open(cx);
    let plain = f.select_all();
    assert!(plain.contains("Title") && plain.contains("First") && !plain.contains("**"));
    let at = f.background();
    f.right_click(at);
    f.pick("read-menu-Copy as Markdown");
    let markdown = f.clipboard();
    assert_eq!(markdown, NOTE);

    f.select_all();
    f.focus_text();
    f.visual.simulate_keystrokes("secondary-c");
    f.visual.run_until_parked();
    let plain_copy = f.clipboard();
    assert_eq!(plain_copy, plain.trim_end_matches('\n'));
    f.seed_clipboard();
    f.visual.simulate_keystrokes("secondary-shift-c");
    f.visual.run_until_parked();
    assert_eq!(f.clipboard(), markdown);
    f.visual.simulate_keystrokes("secondary-c");
    f.visual.run_until_parked();
    assert_eq!(f.clipboard(), plain_copy);

    f.select_all();
    f.seed_clipboard();
    f.palette_copy(false);
    assert_eq!(f.clipboard(), markdown);
    f.select_all();
    f.palette_copy(true);
    assert_eq!(f.clipboard(), plain);
    assert_eq!(
        f.seen.borrow().len(),
        1,
        "formatted palette command reaches the native writer boundary"
    );
    assert_eq!(
        std::fs::read(f.root.join("note.md")).unwrap(),
        NOTE.as_bytes()
    );
}

#[gpui::test]
fn source_and_live_markdown_shortcuts_copy_the_exact_selected_source(cx: &mut TestAppContext) {
    let mut f = open(cx);
    for live in [false, true] {
        f.reader.update_in(f.visual, |reader, window, cx| {
            if reader.editing.is_none() {
                reader.toggle_source(window, cx);
            }
            reader.set_live_preview(live, window, cx);
        });
        f.visual.run_until_parked();
        let input = f.reader.read_with(f.visual, |reader, _| {
            reader.editing.as_ref().unwrap().input().clone()
        });
        input.update(f.visual, |input, cx| {
            input.set_selected_range(0..NOTE.len(), cx)
        });
        f.visual
            .update(|window, cx| input.read(cx).focus_handle(cx).focus(window, cx));
        f.seed_clipboard();
        f.visual.simulate_keystrokes("secondary-shift-c");
        f.visual.run_until_parked();
        assert_eq!(f.clipboard().as_bytes(), NOTE.as_bytes(), "live={live}");
        assert!(
            f.seen.borrow().is_empty(),
            "Markdown copy never writes rich text"
        );
        f.seed_clipboard();
        let at = input.read_with(f.visual, |input, _| input.input_bounds().origin)
            + point(px(20.), px(12.));
        f.right_click(at);
        // Native-menu fallback starts unselected: Cut, Copy, Copy as Markdown.
        f.visual.simulate_keystrokes("down down down enter");
        f.visual.run_until_parked();
        assert_eq!(
            f.clipboard().as_bytes(),
            NOTE.as_bytes(),
            "context menu, live={live}"
        );
        assert_eq!(
            std::fs::read(f.root.join("note.md")).unwrap(),
            NOTE.as_bytes()
        );
        f.seed_clipboard();
        f.reader.update_in(f.visual, |reader, window, cx| {
            reader.focus_handle.focus(window, cx);
            reader.copy_read_markdown(window, cx);
        });
        assert_eq!(
            f.clipboard(),
            "sentinel",
            "an inactive editor cannot supply a stale selection"
        );
    }
}

#[gpui::test]
fn read_rich_text_shortcut_reaches_the_real_multiformat_boundary(cx: &mut TestAppContext) {
    let mut f = open(cx);
    let plain = f.select_all();
    f.focus_text();
    f.seed_clipboard();
    f.visual.simulate_keystrokes("secondary-alt-c");
    f.visual.run_until_parked();
    assert_eq!(f.clipboard(), plain);
    let seen = f.seen.borrow();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].plain, plain);
    assert!(seen[0].html.contains("<strong>café 雪</strong>"));
}

#[gpui::test]
fn markdown_shortcut_and_palette_without_selection_match_document_menu(cx: &mut TestAppContext) {
    let mut f = open(cx);
    let at = f.background();
    f.right_click(at);
    f.pick("read-menu-Copy entire document as Markdown");
    let markdown = f.clipboard();
    assert_eq!(markdown, NOTE);
    f.focus_text();
    f.seed_clipboard();
    f.visual.simulate_keystrokes("secondary-shift-c");
    f.visual.run_until_parked();
    assert_eq!(f.clipboard(), markdown);
    f.seed_clipboard();
    f.palette_copy(false);
    assert_eq!(f.clipboard(), markdown);
    assert!(f.reader.read_with(f.visual, |r, cx| r
        .content
        .read(cx)
        .selected_text()
        .is_empty()));

    // History can be open beside the current Read document without a version
    // preview. Its keyboard context must retain the Read copy shortcut.
    f.reader
        .update_in(f.visual, |r, window, cx| r.open_timeline(window, cx));
    f.visual.run_until_parked();
    f.reader.read_with(f.visual, |r, _| {
        assert!(r
            .active_timeline()
            .is_some_and(|timeline| timeline.selected.is_none()));
        assert!(r.read_copy_available());
    });
    assert!(
        f.visual.debug_bounds("reader-column").is_some(),
        "current Read document remains visible"
    );
    f.focus_text();
    f.seed_clipboard();
    f.visual.simulate_keystrokes("secondary-shift-c");
    f.visual.run_until_parked();
    assert_eq!(f.clipboard(), markdown);
}
#[gpui::test]
fn rich_read_menu_publishes_html_and_plain_without_changing_other_copy_formats(
    cx: &mut TestAppContext,
) {
    let mut f = open(cx);
    let plain = f.select_all();
    assert!(plain.contains("café 雪") && !plain.contains("**"));
    let at = f.background();
    f.right_click(at);
    assert_eq!(
        f.reader.read_with(f.visual, |r, cx| r
            .content
            .read(cx)
            .selected_text_as(SelectionFormat::Plain)),
        plain,
        "menu snapshot leaves the selection intact"
    );
    f.pick("read-menu-Copy as Rich Text");
    assert_eq!(f.clipboard(), plain);
    let seen = f.seen.borrow();
    assert_eq!(
        seen.len(),
        1,
        "real hook reaches the native multi-format boundary"
    );
    assert_eq!(seen[0].plain, plain);
    assert!(
        seen[0].html.contains("<strong>café 雪</strong>")
            && seen[0].html.contains("<em>italic</em>")
    );
    assert!(seen[0].html.contains("    let a = ") && seen[0].html.contains("&lt;&amp;&gt;"));
    drop(seen);
    // Choosing a menu item with the mouse has the existing selection-clear
    // behavior. Select again for each independent copy-format action.
    f.select_all();
    f.right_click(at);
    f.pick("read-menu-Copy");
    assert_eq!(f.clipboard(), plain);
    f.select_all();
    f.right_click(at);
    f.pick("read-menu-Copy as Markdown");
    assert_eq!(f.clipboard().as_bytes(), NOTE.as_bytes());
    assert_eq!(
        f.seen.borrow().len(),
        1,
        "plain/Markdown actions stay plain"
    );
    assert_eq!(
        std::fs::read(f.root.join("note.md")).unwrap(),
        NOTE.as_bytes()
    );
}
#[gpui::test]
fn rich_link_group_uses_captured_selection_after_the_displayed_revision_changes(
    cx: &mut TestAppContext,
) {
    let mut f = open(cx);
    let plain = f.select_all();
    let link = f.link();
    f.right_click(link);
    assert!(
        f.reader.read_with(f.visual, |r, _| r.file_menu.is_some()),
        "one link popup owns the press"
    );
    let rich = f
        .visual
        .debug_bounds("read-menu-Copy as Rich Text")
        .expect("rich selection action joins link group");
    // A later accepted render cannot replace the clipboard payload already
    // captured for this menu. The source file itself remains read-only.
    f.reader.update(f.visual, |r, cx| {
        r.content.update(cx, |content, cx| {
            content.set_text_with_source(
                "Later **revision**",
                Some("Later **revision**".into()),
                cx,
            );
        })
    });
    f.visual.run_until_parked();
    f.visual.simulate_click(rich.center(), Modifiers::default());
    f.visual.run_until_parked();
    assert_eq!(f.clipboard(), plain);
    assert_eq!(f.seen.borrow().len(), 1);
    let seen = f.seen.borrow();
    assert!(
        seen[0].html.contains("<strong>café 雪</strong>")
            && seen[0].html.contains("href=\"https://example.test\"")
    );
    assert!(!seen[0].html.contains("Later"));
    assert_eq!(
        std::fs::read(f.root.join("note.md")).unwrap(),
        NOTE.as_bytes()
    );
}
