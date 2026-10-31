//! Read-mode context menu by place (#2025).
//!
//! A right-click in the Reader offers what applies where it landed, in
//! separated groups: the selection (plain text or Markdown), the code block
//! under the pointer, the heading's link, a reminder for a selected date and
//! the whole note. Links, images and tables open their own menus; this one
//! then stays closed so a single press never shows two menus.
//!
//! The renderer reports right presses on code blocks and links (vendor patch
//! 0104). The press is kept with its position and the document generation, so
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

/// One Read-menu command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Item {
    Copy,
    CopyMarkdown,
    CopyCode,
    CopyHeadingLink(String),
    SelectAll,
    CopyNoteMarkdown,
}

impl Item {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Copy => "Copy",
            Self::CopyMarkdown => "Copy as Markdown",
            Self::CopyCode => "Copy code",
            Self::CopyHeadingLink(_) => "Copy link to heading",
            Self::SelectAll => "Select all",
            Self::CopyNoteMarkdown => "Copy note as Markdown",
        }
    }
}

/// What the Read menu knows about the press, as plain data.
#[derive(Clone, Debug, Default)]
pub(crate) struct Facts {
    pub(crate) selection: bool,
    /// The view can produce Markdown source for its selection.
    pub(crate) source: bool,
    pub(crate) code: bool,
    pub(crate) heading: Option<String>,
    /// A Markdown note is open (not an attachment preview), so the note can be
    /// copied whole.
    pub(crate) note: bool,
    /// The note's file text is known exactly.
    pub(crate) note_source: bool,
}

/// The menu's groups, in order, each with `(item, disabled)` entries. Empty
/// groups are left out; the caller puts a separator between groups.
pub(crate) fn groups(facts: &Facts) -> Vec<Vec<(Item, bool)>> {
    let mut groups = Vec::new();
    if facts.selection {
        groups.push(vec![
            (Item::Copy, false),
            (Item::CopyMarkdown, !facts.source),
        ]);
    }
    if facts.code {
        groups.push(vec![(Item::CopyCode, false)]);
    }
    if let Some(heading) = &facts.heading {
        groups.push(vec![(Item::CopyHeadingLink(heading.clone()), false)]);
    }
    let mut document = vec![(Item::SelectAll, false)];
    if facts.note {
        document.push((Item::CopyNoteMarkdown, !facts.note_source));
    }
    groups.push(document);
    groups
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

/// The Read menu for the press at the pointer.
pub(super) fn menu(
    menu: PopupMenu,
    reader: &WeakEntity<Reader>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Some(entity) = reader.upgrade() else {
        return menu;
    };
    let position = window.mouse_position();
    let Some(facts) = entity.read(cx).read_menu_facts(position, cx) else {
        return menu;
    };
    // Captured now: the click that picks an item clears the selection.
    let selection = facts.selection.then(|| {
        let content = entity.read(cx).content.clone();
        content.update(cx, |content, _| {
            (
                content.selected_text_as(SelectionFormat::Plain),
                content.selected_text_as(SelectionFormat::Source),
            )
        })
    });
    let mut menu = menu;
    for (ix, group) in groups(&facts).into_iter().enumerate() {
        if ix > 0 {
            menu = menu.separator();
        }
        for (item, disabled) in group {
            if let Item::CopyHeadingLink(heading) = item {
                menu = reader_deep_link::heading_link_item(menu, reader, heading);
                continue;
            }
            let copied = match (&item, &selection) {
                (Item::Copy, Some((plain, _))) => Some(plain.clone()),
                (Item::CopyMarkdown, Some((_, source))) => Some(source.clone()),
                _ => None,
            };
            let reader = reader.clone();
            let label = item.label();
            menu = menu.item(
                PopupMenuItem::element(move |_, _| {
                    div()
                        .debug_selector(move || format!("read-menu-{label}"))
                        .child(label)
                })
                .disabled(disabled)
                .on_click(move |_, window, cx| {
                    if let Some(text) = copied.clone().filter(|text| !text.is_empty()) {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        return;
                    }
                    let item = item.clone();
                    let _ = reader.update(cx, |reader, cx| {
                        reader.read_menu_command(item, position, window, cx)
                    });
                }),
            );
        }
        // A reminder needs a selected date: it joins the selection group.
        if ix == 0 && facts.selection {
            menu = reader_reminder::menu(menu, reader, cx);
        }
    }
    menu
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

    /// `None` when the press belongs to another menu: a link, an image or a
    /// table at the same place, or the source editor.
    fn read_menu_facts(&self, position: Point<Pixels>, cx: &App) -> Option<Facts> {
        let nothing_open =
            self.current_rel.is_empty() && self.file_preview.is_none() && !self.single_file;
        if nothing_open || self.editing.is_some() || self.pointer_on_link() {
            return None;
        }
        // Image and table menus open during the press itself, before this
        // deferred builder runs.
        if self
            .file_menu
            .as_ref()
            .is_some_and(|(_, at)| *at == position)
        {
            return None;
        }
        let content = self.content.read(cx);
        let note = self.file_preview.is_none() && !self.use_html;
        Some(Facts {
            selection: !content.selected_text().is_empty(),
            source: content.can_copy_source(),
            code: self.pressed_code(position).is_some(),
            heading: self.heading_at(position, cx),
            note,
            note_source: note && self.note_canonical_source.is_some(),
        })
    }

    fn read_menu_command(
        &mut self,
        item: Item,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = match item {
            // Copied from the selection captured when the menu opened.
            Item::Copy | Item::CopyMarkdown => None,
            Item::CopyCode => self.pressed_code(position).map(|code| code.to_string()),
            Item::CopyNoteMarkdown => self.note_canonical_source.as_deref().map(str::to_owned),
            Item::SelectAll => {
                self.content
                    .update(cx, |content, cx| content.select_all(cx));
                None
            }
            Item::CopyHeadingLink(heading) => {
                self.copy_heading_link(heading, window, cx);
                None
            }
        };
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
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
    fn read_menu_groups_combine_by_place() {
        let labels = |facts: &Facts| {
            groups(facts)
                .into_iter()
                .map(|group| {
                    group
                        .into_iter()
                        .map(|(item, disabled)| (item.label(), disabled))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        // Empty space in a note: the whole-note group only.
        let note = Facts {
            note: true,
            note_source: true,
            source: true,
            ..Default::default()
        };
        assert_eq!(
            labels(&note),
            [vec![
                ("Select all", false),
                ("Copy note as Markdown", false)
            ]]
        );
        // A selection inside a code block under a heading-free place.
        let everything = Facts {
            selection: true,
            code: true,
            heading: Some("Plan".into()),
            ..note.clone()
        };
        assert_eq!(
            labels(&everything),
            [
                vec![("Copy", false), ("Copy as Markdown", false)],
                vec![("Copy code", false)],
                vec![("Copy link to heading", false)],
                vec![("Select all", false), ("Copy note as Markdown", false)],
            ]
        );
        // An HTML view or a damaged file says what it cannot copy.
        let unsure = Facts {
            selection: true,
            source: false,
            note: true,
            note_source: false,
            ..Default::default()
        };
        assert_eq!(
            labels(&unsure),
            [
                vec![("Copy", false), ("Copy as Markdown", true)],
                vec![("Select all", false), ("Copy note as Markdown", true)],
            ]
        );
        // An attachment preview offers no note-wide copy.
        assert_eq!(labels(&Facts::default()), [vec![("Select all", false)]]);
    }

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
            f.pick(last, "Copy note as Markdown").as_bytes(),
            NOTE.as_bytes()
        );
        assert_eq!(
            f.pick(code, "Copy note as Markdown"),
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
        // Picking an item with the mouse ends the selection; select again.
        f.pick(last, "Select all");
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
    }
}
