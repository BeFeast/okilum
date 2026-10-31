//! Read-mode context menu for rendered Markdown tables (#2025).
//!
//! A right-click anywhere on a rendered table offers to copy that table, either
//! as the Markdown it was written in or as tab-separated values. The menu acts
//! on the snapshot the renderer handed over with the press, so it copies the
//! clicked table even when a sibling table or a newer document occupies the
//! same source offsets. Opening the menu or copying never touches the document,
//! the selection or the scroll position.

use super::*;
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::text::{TableData, TableSecondaryPress};

/// One way of copying a table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TableCopy {
    Markdown,
    Tsv,
}

impl TableCopy {
    pub(crate) const ALL: [Self; 2] = [Self::Markdown, Self::Tsv];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Markdown => "Copy table as Markdown",
            Self::Tsv => "Copy table as TSV",
        }
    }

    /// The clipboard text for the pressed table, or `None` when the renderer
    /// could not locate the table's source (the item is then disabled rather
    /// than copying a reconstruction the author never wrote).
    pub(crate) fn content(self, press: &TableSecondaryPress) -> Option<String> {
        match self {
            Self::Markdown => press.source().map(str::to_owned),
            Self::Tsv => Some(tsv(press.data())),
        }
    }
}

/// Header first, then body rows; no delimiter row. Cells hold their visible
/// text, so Markdown syntax (emphasis markers, link targets, code ticks,
/// escapes) is gone. Empty cells stay empty.
pub(crate) fn tsv(table: &TableData) -> String {
    std::iter::once(&table.headers)
        .chain(table.rows.iter())
        .map(|row| {
            row.iter()
                .map(|cell| tsv_field(cell))
                .collect::<Vec<_>>()
                .join("\t")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A field containing a tab, line break or quote is double-quoted, with its
/// quotes doubled, as spreadsheets expect.
fn tsv_field(cell: &str) -> std::borrow::Cow<'_, str> {
    if cell.contains(['\t', '\n', '\r', '"']) {
        format!("\"{}\"", cell.replace('"', "\"\"")).into()
    } else {
        cell.into()
    }
}

/// Install the table menu on a Read-mode TextView. Live Preview is left alone:
/// a press there reveals the table's source in the editor.
pub(super) fn install(view: TextView, reader: Option<WeakEntity<Reader>>, live: bool) -> TextView {
    let Some(reader) = reader.filter(|_| !live) else {
        return view;
    };
    view.on_table_secondary_press(move |press, event, window, cx| {
        let Some(entity) = reader.upgrade() else {
            return;
        };
        // A link inside a cell keeps its own menu.
        if entity.read(cx).pointer_on_link() {
            return;
        }
        cx.stop_propagation();
        let press = press.clone();
        entity.update(cx, |reader, cx| {
            reader.table_menu(press, event.position, window, cx)
        });
    })
}

impl Reader {
    fn table_menu(
        &mut self,
        press: TableSecondaryPress,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let popup = PopupMenu::build(window, cx, move |mut menu, _, _| {
            for format in TableCopy::ALL {
                let press = press.clone();
                let available = format == TableCopy::Tsv || press.source().is_some();
                menu = menu.item(
                    PopupMenuItem::new(format.label())
                        .disabled(!available)
                        .on_click(move |_, _, cx| {
                            if let Some(text) = format.content(&press) {
                                cx.write_to_clipboard(ClipboardItem::new_string(text));
                            }
                        }),
                );
            }
            menu
        });
        cx.subscribe(&popup, |reader, _, _: &DismissEvent, cx| {
            reader.file_menu = None;
            cx.notify();
        })
        .detach();
        popup.focus_handle(cx).focus(window, cx);
        self.file_menu = Some((popup, position));
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn data(headers: &[&str], rows: &[&[&str]]) -> TableData {
        TableData {
            headers: headers.iter().map(|s| s.to_string()).collect(),
            rows: rows
                .iter()
                .map(|row| row.iter().map(|s| s.to_string()).collect())
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn reader_table_tsv_quotes_only_fields_that_need_it() {
        let table = data(
            &["Name", "Note", ""],
            &[
                &["a\tb", "say \"hi\"", ""],
                &["line\nbreak", "cr\rx", "plain"],
            ],
        );
        assert_eq!(
            tsv(&table).as_bytes(),
            b"Name\tNote\t\n\"a\tb\"\t\"say \"\"hi\"\"\"\t\n\"line\nbreak\"\t\"cr\rx\"\tplain"
        );
        assert_eq!(tsv(&data(&["only"], &[])), "only");
        assert_eq!(TableCopy::Markdown.label(), "Copy table as Markdown");
        assert_eq!(TableCopy::Tsv.label(), "Copy table as TSV");
    }

    const FIRST: &str = "| Name | Note | Code |\n\
| :--- | :---: | ---: |\n\
| שלום עולם | [site](https://example.com) and *emph* | `x` |\n\
| Ünïcode 雪 | **bold** a\\|b |  |\n\
| say \"hi\" | ~~gone~~ | end |";
    const SECOND: &str = "| Key | Value |\n| --- | --- |\n| k1 | v1 |";

    fn note(first: &str, second: &str, eol: &str) -> String {
        format!("# Tables\n\nIntro paragraph.\n\n{first}\n\nBetween the tables.\n\n{second}\n\nAfter.\n")
            .replace('\n', eol)
    }

    struct Fixture<'a> {
        _dir: tempfile::TempDir,
        root: PathBuf,
        reader: Entity<Reader>,
        visual: &'a mut gpui::VisualTestContext,
    }

    fn open<'a>(cx: &'a mut TestAppContext, notes: &[(&str, &str)]) -> Fixture<'a> {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        for (name, body) in notes {
            std::fs::write(root.join(name), body).unwrap();
        }
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
                        note: Some(notes[0].0.into()),
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
        fn bounds(&mut self, selector: &'static str) -> Bounds<Pixels> {
            self.visual.update(|window, cx| window.draw(cx).clear(cx));
            self.visual
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} rendered"))
        }

        fn menu_open(&mut self) -> bool {
            self.reader
                .read_with(self.visual, |reader, _| reader.file_menu.is_some())
        }

        fn right_click(&mut self, at: Point<Pixels>) {
            self.visual
                .simulate_mouse_move(at, None, Modifiers::default());
            self.visual
                .simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
            self.visual
                .simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
            self.visual.run_until_parked();
        }

        /// Right-click `at`, pick the menu item for `format` from the real
        /// popup and return the clipboard text it wrote.
        fn copy(&mut self, at: Point<Pixels>, format: TableCopy) -> String {
            self.visual.update(|_, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into()))
            });
            self.right_click(at);
            assert!(self.menu_open(), "right-click on a table opens its menu");
            // The items are listed in `TableCopy::ALL` order.
            let steps = match format {
                TableCopy::Markdown => "down enter",
                TableCopy::Tsv => "down down enter",
            };
            self.visual.simulate_keystrokes(steps);
            self.visual.run_until_parked();
            assert!(!self.menu_open(), "choosing an item closes the menu");
            let copied = self
                .visual
                .update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap());
            assert_ne!(copied, "sentinel", "the item wrote the clipboard");
            copied
        }

        /// What opening and copying must leave alone.
        fn untouched(&mut self) -> (Vec<u8>, String, usize, Pixels) {
            let bytes = std::fs::read(self.root.join("note.md")).unwrap();
            self.reader.read_with(self.visual, |reader, cx| {
                let content = reader.content.read(cx);
                let top = content.list_state().logical_scroll_top();
                (
                    bytes,
                    content.selected_text(),
                    top.item_ix,
                    top.offset_in_item,
                )
            })
        }
    }

    const FIRST_TSV: &str = "Name\tNote\tCode\n\
שלום עולם\tsite and emph\tx\n\
Ünïcode 雪\tbold a|b\t\n\
\"say \"\"hi\"\"\"\tgone\tend";

    #[gpui::test]
    fn reader_table_copy_menu_copies_the_clicked_table(cx: &mut TestAppContext) {
        let mut f = open(cx, &[("note.md", &note(FIRST, SECOND, "\n"))]);
        // Only the first table has a fourth row and a third column; the
        // second table is painted later, so shared selectors name it.
        let first = f.bounds("table-cell-3-2").center();
        let second = f.bounds("table-cell-1-1").center();
        let second_frame = f.bounds("table-content-frame");
        let before = f.untouched();

        assert_eq!(
            f.copy(first, TableCopy::Markdown).as_bytes(),
            FIRST.as_bytes()
        );
        assert_eq!(
            f.copy(first, TableCopy::Tsv).as_bytes(),
            FIRST_TSV.as_bytes()
        );
        assert_eq!(
            f.copy(second, TableCopy::Markdown).as_bytes(),
            SECOND.as_bytes()
        );
        assert_eq!(
            f.copy(second, TableCopy::Tsv).as_bytes(),
            "Key\tValue\nk1\tv1".as_bytes()
        );
        // Anywhere inside the frame counts, not only a cell's text.
        let edge = point(second_frame.left() + px(2.), second_frame.top() + px(2.));
        assert_eq!(
            f.copy(edge, TableCopy::Markdown).as_bytes(),
            SECOND.as_bytes()
        );
        // Back to the first table after the sibling was clicked.
        assert_eq!(
            f.copy(first, TableCopy::Markdown).as_bytes(),
            FIRST.as_bytes()
        );
        assert_eq!(
            f.untouched(),
            before,
            "document, selection and scroll unchanged"
        );

        // Negative control: the paragraph between the tables opens no table
        // menu and copies nothing, while the same gesture on a table did.
        let first_frame_bottom = f.bounds("table-cell-3-2").bottom();
        let between = point(
            first.x,
            first_frame_bottom + (second_frame.top() - first_frame_bottom) / 2.,
        );
        f.visual
            .update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into())));
        f.right_click(between);
        assert!(!f.menu_open(), "no table menu outside a table");
        f.visual.simulate_keystrokes("escape");
        f.visual.run_until_parked();
        assert_eq!(
            f.visual
                .update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap()),
            "sentinel"
        );
        assert_eq!(f.untouched(), before);
    }

    #[gpui::test]
    fn reader_table_copy_keeps_crlf_and_an_existing_selection(cx: &mut TestAppContext) {
        let mut f = open(cx, &[("note.md", &note(FIRST, SECOND, "\r\n"))]);
        let first = f.bounds("table-cell-3-2").center();
        f.reader.update(f.visual, |reader, cx| {
            reader
                .content
                .update(cx, |content, cx| content.select_all(cx))
        });
        f.visual.run_until_parked();
        let before = f.untouched();
        assert!(!before.1.is_empty(), "positive control: a selection exists");

        assert_eq!(
            f.copy(first, TableCopy::Markdown).as_bytes(),
            FIRST.replace('\n', "\r\n").as_bytes(),
            "authored CRLF line endings survive"
        );
        assert_eq!(
            f.copy(first, TableCopy::Tsv).as_bytes(),
            FIRST_TSV.as_bytes()
        );
        assert_eq!(
            f.untouched(),
            before,
            "selection kept while copying a table"
        );
    }

    #[gpui::test]
    fn reader_table_copy_follows_a_replacement_document(cx: &mut TestAppContext) {
        // Same lengths, so both notes' tables sit at identical offsets.
        let replaced = FIRST.replace("Name", "Nome").replace("שלום", "ברוך");
        assert_eq!(replaced.len(), FIRST.len());
        let mut f = open(
            cx,
            &[
                ("note.md", &note(FIRST, SECOND, "\n")),
                ("other.md", &note(&replaced, SECOND, "\n")),
            ],
        );
        let first = f.bounds("table-cell-3-2").center();
        assert_eq!(
            f.copy(first, TableCopy::Markdown).as_bytes(),
            FIRST.as_bytes()
        );
        f.reader.update_in(f.visual, |reader, window, cx| {
            reader.open_note("other.md", None, window, cx)
        });
        f.visual.run_until_parked();
        let first = f.bounds("table-cell-3-2").center();
        assert_eq!(
            f.copy(first, TableCopy::Markdown).as_bytes(),
            replaced.as_bytes()
        );
        assert!(f
            .copy(first, TableCopy::Tsv)
            .starts_with("Nome\tNote\tCode\nברוך עולם\t"));
    }
}
