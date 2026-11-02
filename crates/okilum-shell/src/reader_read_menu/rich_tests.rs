use super::*;
use ::core::prelude::v1::test;
use platform::rich_clipboard::{self, Payload};
use std::{cell::RefCell, rc::Rc};

const NOTE: &str = "# Title\r\n\r\nA **café 雪** and *italic* with [Example](https://example.test).\r\n\r\n```rs\r\n    let a = \"<&>\";\r\n```\r\n\r\nAfter.\r\n";
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
    f.pick("read-menu-Copy with formatting");
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
        .debug_bounds("read-menu-Copy with formatting")
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
