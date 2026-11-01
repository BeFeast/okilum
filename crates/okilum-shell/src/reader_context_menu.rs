//! Read-mode context menu by place (#2025).
//!
//! A right-click in the Reader offers what applies where it landed, in
//! separated groups: the selection (plain text or Markdown), the code block
//! under the pointer, the heading's link, a reminder for a selected date and
//! the whole note. Links, images and tables open their own menus; this one
//! then stays closed so a single press never shows two menus.
//!
//! The renderer reports right presses on code blocks and links (vendor patch
//! 0113). The press is kept with its position and the document generation, so
//! a menu built later never acts on a block from another place or note.
//! Nothing here runs while typing or rendering: work happens on the press.

use super::*;
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::text::LinkSecondaryPress;
use okilum_core::deep_link::Position;

/// What a right press in the Reader landed on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReadPress {
    position: Point<Pixels>,
    generation: u64,
    target: PressTarget,
}

#[derive(Clone, Debug, PartialEq)]
enum PressTarget {
    /// The code of a fenced or indented block, as its Copy button copies it.
    Code(SharedString),
    Link {
        url: SharedString,
        text: SharedString,
    },
}

/// Record right presses on links and code blocks in a Read-mode TextView.
/// Live Preview is left alone; presses always propagate.
pub(super) fn install(view: TextView, reader: Option<WeakEntity<Reader>>, live: bool) -> TextView {
    let Some(reader) = reader.filter(|_| !live) else {
        return view;
    };
    let code_reader = reader.clone();
    view.on_code_block_secondary_press(move |block, event, _, cx| {
        let Some(code) = block.copy_code() else {
            return;
        };
        let _ = code_reader.update(cx, |reader, _| {
            reader.record_press(event.position, PressTarget::Code(code))
        });
    })
    .on_link_secondary_press(move |press: &LinkSecondaryPress, event, _, cx| {
        let target = PressTarget::Link {
            url: press.url().clone(),
            text: press.text().clone(),
        };
        let _ = reader.update(cx, |reader, _| reader.record_press(event.position, target));
    })
}

/// One Read surface hook. Text snapshots come from the retained #2262
/// packet; code is captured for this press, before another render can move it.
pub(super) fn menu(
    menu: PopupMenu,
    reader: &WeakEntity<Reader>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Some(entity) = reader.upgrade() else {
        return menu;
    };
    let state = entity.read(cx);
    let position = window.mouse_position();
    if state.editing.is_some()
        || state.pointer_on_link()
        || state
            .file_menu
            .as_ref()
            .is_some_and(|(_, at)| *at == position)
    {
        return menu;
    }
    let code = state.pressed_code(position);
    let heading = state.heading_at(position, cx);
    let mut menu = reader_read_menu::menu(menu, reader, cx);
    if let Some(code) = code {
        menu = menu.separator().item(
            PopupMenuItem::element(|_, _| {
                div()
                    .debug_selector(|| "read-menu-Copy code".into())
                    .child("Copy code")
            })
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
            }),
        );
    }
    let menu = reader_reminder::menu(menu.separator(), reader, cx);
    match heading {
        Some(heading) => reader_deep_link::heading_link_item(menu, reader, heading),
        None => menu,
    }
}

impl Reader {
    fn record_press(&mut self, position: Point<Pixels>, target: PressTarget) {
        self.read_press = Some(ReadPress {
            position,
            generation: self.navigation.preparation_generation,
            target,
        });
    }

    /// The code block pressed at `position` in this document.
    fn pressed_code(&self, position: Point<Pixels>) -> Option<SharedString> {
        let press = self.read_press.as_ref().filter(|press| {
            press.position == position && press.generation == self.navigation.preparation_generation
        })?;
        match &press.target {
            PressTarget::Code(code) => Some(code.clone()),
            PressTarget::Link { .. } => None,
        }
    }

    /// The visible text of the link `url` pressed last in this document.
    pub(crate) fn pressed_link_text(&self, url: &str) -> Option<SharedString> {
        let press = self
            .read_press
            .as_ref()
            .filter(|press| press.generation == self.navigation.preparation_generation)?;
        match &press.target {
            PressTarget::Link { url: pressed, text } if pressed.as_ref() == url => {
                Some(text.clone())
            }
            _ => None,
        }
    }

    /// The full address a link points at, for «Copy link address»: a web
    /// address as written, a note, heading, block or file in this vault as an
    /// `okilum:` link (a relative link or `#anchor` is resolved, never copied
    /// as written). `None` when the link does not resolve to one place.
    pub(crate) fn link_address(&self, url: &str) -> Option<String> {
        use okilum_core::document_links::prepared::LinkStatus;
        let state = self.prepared_links.get(url);
        if state.is_some_and(|state| {
            !matches!(state.status, LinkStatus::Resolved | LinkStatus::External)
        }) {
            return None;
        }
        let action = state
            .and_then(|state| state.action_url.as_deref())
            .unwrap_or(url);
        link_address(&self.vault_root, &self.current_rel, action)
    }
}

/// [`Reader::link_address`] for a link's action URL.
pub(crate) fn link_address(root: &Path, current: &str, action: &str) -> Option<String> {
    if let Some(rest) = action.strip_prefix(WIKI_SCHEME) {
        let (rel, fragment) = split_open_url(rest);
        let rel = if rel.is_empty() {
            current.to_owned()
        } else {
            rel
        };
        if rel.is_empty() {
            return None;
        }
        let position = match fragment {
            Some(fragment) => match fragment.strip_prefix('^') {
                Some(block) => Position {
                    block: Some(block.to_owned()),
                    ..Default::default()
                },
                None => Position {
                    heading: Some(fragment),
                    ..Default::default()
                },
            },
            None => Position::default(),
        };
        return Some(reader_files::okilum_link(root, &rel, &position));
    }
    if let Some(rest) = action.strip_prefix("okilum://attachment/") {
        let rel = okilum_core::document_links::decode(rest);
        return Some(reader_files::okilum_link(root, &rel, &Position::default()));
    }
    if action.starts_with("okilum://") {
        return None;
    }
    prepared_links::external_tooltip(action).map(|_| action.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn link_addresses_are_full_not_as_written() {
        let root = Path::new("/vaults/My Brain");
        let note = |rel: &str, position: Position| reader_files::okilum_link(root, rel, &position);
        assert_eq!(
            link_address(root, "a/b.md", "https://example.com/x?y=1#z").as_deref(),
            Some("https://example.com/x?y=1#z")
        );
        assert_eq!(
            link_address(root, "a/b.md", "okilum://open/notes/other.md").unwrap(),
            note("notes/other.md", Position::default())
        );
        // `[[#Plan]]` and `[text](#plan)`: this note at the heading.
        let heading = note(
            "a/b.md",
            Position {
                heading: Some("Plan".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            link_address(root, "a/b.md", "okilum://open/#Plan").unwrap(),
            heading
        );
        assert!(heading.starts_with("okilum://"), "{heading}");
        assert_eq!(
            link_address(root, "a/b.md", "okilum://open/x.md#%5Eabc").unwrap(),
            note(
                "x.md",
                Position {
                    block: Some("abc".into()),
                    ..Default::default()
                }
            )
        );
        assert_eq!(
            link_address(root, "a/b.md", "okilum://attachment/files/r%201.pdf").unwrap(),
            note("files/r 1.pdf", Position::default())
        );
        // Positive control above; links that do not name one place copy nothing.
        for unresolved in [
            "okilum://unresolved/nowhere",
            "okilum://ambiguous/twice",
            "okilum://missing-file/gone.png",
        ] {
            assert_eq!(
                link_address(root, "a/b.md", unresolved),
                None,
                "{unresolved}"
            );
        }
    }

    const NOTE: &str = "# Plan\n\n[Example](https://example.com/page) and [jump](#plan) then **bold** words.\n\n```rust\nfn main() {}\n```\n\nLast *plain* paragraph.\n";

    struct Fixture<'a> {
        _dir: tempfile::TempDir,
        root: PathBuf,
        reader: Entity<Reader>,
        visual: &'a mut gpui::VisualTestContext,
    }

    fn open(cx: &mut TestAppContext) -> Fixture<'_> {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("note.md"), NOTE).unwrap();
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
        visual.simulate_resize(size(px(1400.), px(960.)));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        Fixture {
            _dir: dir,
            root,
            reader: entity.unwrap(),
            visual,
        }
    }

    impl Fixture<'_> {
        fn block(&mut self, ix: usize) -> Bounds<Pixels> {
            self.visual.update(|window, cx| window.draw(cx).clear(cx));
            self.reader.read_with(self.visual, |reader, cx| {
                reader
                    .content
                    .read(cx)
                    .list_state()
                    .bounds_for_item(ix)
                    .expect("block rendered")
            })
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

        fn offered(&mut self, label: &str) -> Option<Bounds<Pixels>> {
            self.visual
                .debug_bounds(Box::leak(format!("read-menu-{label}").into_boxed_str()))
        }

        fn close(&mut self) {
            self.visual.simulate_keystrokes("escape");
            self.visual.run_until_parked();
        }

        fn clipboard(&mut self) -> String {
            self.visual
                .update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap())
        }

        fn seed(&mut self) {
            self.visual.update(|_, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into()))
            });
        }

        /// Right-click `at`, choose `label` in the Read menu, return the clipboard.
        fn pick(&mut self, at: Point<Pixels>, label: &str) -> String {
            self.seed();
            self.right_click(at);
            let item = self
                .offered(label)
                .unwrap_or_else(|| panic!("{label} offered"));
            self.visual
                .simulate_click(item.center(), Modifiers::default());
            self.visual.run_until_parked();
            self.clipboard()
        }

        /// The link under the pointer while scanning the first paragraph.
        fn find_link(&mut self, url: &str) -> Point<Pixels> {
            let paragraph = self.block(1);
            let y = paragraph.top() + px(BODY_FONT_SIZE * 0.8);
            (0..120)
                .map(|step| point(paragraph.left() + px(step as f32 * 6.), y))
                .find(|at| {
                    self.visual
                        .simulate_mouse_move(*at, None, Modifiers::default());
                    self.visual.run_until_parked();
                    self.reader.read_with(self.visual, |reader, _| {
                        reader
                            .pointer_link
                            .as_ref()
                            .is_some_and(|(link, _)| link.contains(url))
                    })
                })
                .unwrap_or_else(|| panic!("positive control: hovering {url} is reported"))
        }

        fn link_menu(&mut self, at: Point<Pixels>, steps: &str) -> String {
            self.seed();
            self.right_click(at);
            assert!(
                self.reader
                    .read_with(self.visual, |r, _| r.file_menu.is_some()),
                "the link opens its menu"
            );
            assert!(self.offered("Select all").is_none(), "one menu per press");
            self.visual.simulate_keystrokes(steps);
            self.visual.run_until_parked();
            self.clipboard()
        }
    }

    #[gpui::test]
    fn read_menu_copies_code_note_and_selection_by_place(cx: &mut TestAppContext) {
        let mut f = open(cx);
        let code = f.block(2).center();
        let last = f.block(3).center();

        // Exactly what the block's Copy button copies.
        assert_eq!(f.pick(code, "Copy code"), "fn main() {}\n");
        // Negative control: a paragraph offers no code item.
        f.right_click(last);
        assert!(f.offered("Copy code").is_none());
        assert!(
            f.offered("Copy").is_none(),
            "no selection, no selection group"
        );
        f.close();

        assert_eq!(
            f.pick(last, "Copy entire document as Markdown").as_bytes(),
            NOTE.as_bytes()
        );
        assert_eq!(
            f.pick(code, "Copy entire document as Markdown"),
            NOTE,
            "groups combine"
        );

        f.pick(last, "Select all");
        let selected = f
            .reader
            .read_with(f.visual, |r, cx| r.content.read(cx).selected_text());
        assert!(selected.contains("Last"), "{selected:?}");

        let plain = f.pick(last, "Copy");
        assert!(
            plain.contains("then bold words") && plain.contains("Last plain"),
            "{plain:?}"
        );
        assert!(!plain.contains("**"), "{plain:?}");
        // The clipboard payload is snapshotted without changing selection.
        f.reader.update(f.visual, |r, cx| {
            r.content.update(cx, |c, cx| c.select_all(cx))
        });
        f.visual.run_until_parked();
        let markdown = f.pick(last, "Copy as Markdown");
        assert!(
            markdown.contains("**bold**")
                && markdown.contains("*plain*")
                && markdown.contains("[jump](#plan)"),
            "{markdown:?}"
        );
        assert_eq!(
            std::fs::read_to_string(f.root.join("note.md")).unwrap(),
            NOTE,
            "the note is never written"
        );
    }

    #[gpui::test]
    fn read_menu_copies_a_links_full_address_and_text(cx: &mut TestAppContext) {
        let mut f = open(cx);
        let web = f.find_link("example.com");
        assert_eq!(f.link_menu(web, "down enter"), "https://example.com/page");
        assert_eq!(f.link_menu(web, "down down enter"), "Example");

        let anchor = f.find_link("plan");
        let address = f.link_menu(anchor, "down enter");
        let expected = reader_files::okilum_link(
            &f.root,
            "note.md",
            &Position {
                heading: Some("Plan".into()),
                ..Default::default()
            },
        );
        assert_eq!(address.to_lowercase(), expected.to_lowercase(), "{address}");
        assert_eq!(f.link_menu(anchor, "down down enter"), "jump");

        // With text selected the link keeps its own menu and its text.
        f.reader.update(f.visual, |r, cx| {
            r.content.update(cx, |c, cx| c.select_all(cx))
        });
        f.visual.run_until_parked();
        assert_eq!(f.link_menu(web, "down down enter"), "Example");
        f.right_click(web);
        let copy = f
            .offered("Copy as Markdown")
            .expect("selection group joins the link popup");
        f.visual.simulate_click(copy.center(), Modifiers::default());
        f.visual.run_until_parked();
        assert_eq!(
            f.clipboard(),
            NOTE,
            "the accepted source snapshot is copied"
        );
        assert_eq!(
            std::fs::read_to_string(f.root.join("note.md")).unwrap(),
            NOTE
        );
    }
}
