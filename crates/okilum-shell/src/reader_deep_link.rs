//! Landing for external `okilum:` links (#1049, docs/deep-links.md): a line
//! scrolls the Reader to its block or puts the editor's caret on it; a
//! `#heading` or `#^block` uses the wikilink landing. A link never switches
//! between Reader and editing.

use super::*;
use gpui_component::input::{EditorState, RopeExt as _};
use okilum_core::deep_link::Position;

/// The «Copy link to heading» menu item for `heading` in this Reader's note.
pub(crate) fn heading_link_item(
    menu: gpui_component::menu::PopupMenu,
    reader: &WeakEntity<Reader>,
    heading: String,
) -> gpui_component::menu::PopupMenu {
    let reader = reader.clone();
    menu.item(
        gpui_component::menu::PopupMenuItem::new("Copy link to heading").on_click(
            move |_, window, cx| {
                let heading = heading.clone();
                let _ = reader.update(cx, |this, cx| this.copy_heading_link(heading, window, cx));
            },
        ),
    )
}

/// How long a landing highlight stays.
const FLASH: std::time::Duration = std::time::Duration::from_millis(1200);

/// The editor caret a link places once its note is open.
pub(crate) struct EditLanding {
    /// The note preparation it waits for; `None` waits for the window's
    /// restored editor (a new window that was left editing).
    generation: Option<u64>,
    rel: String,
    line: Option<u32>,
    column: Option<u32>,
    fragment: bool,
}

const FRAGMENT_IN_EDITOR: &str =
    "Headings and blocks are found in the reading view. The note is open for editing.";

impl Reader {
    /// Use the ordinary opener/save guard and its generation-bound editor handoff.
    pub(crate) fn open_created_note(
        &mut self,
        rel: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let before = self.navigation.preparation_generation;
        self.open_note(rel, None, window, cx);
        if self.navigation.preparation_generation != before {
            self.pending_link_edit = Some(EditLanding {
                generation: Some(self.navigation.preparation_generation),
                rel: rel.to_owned(),
                line: None,
                column: None,
                fragment: false,
            });
        }
    }

    /// Open `rel` at a link's position. A fragment and a line together: the
    /// line wins. While editing, the caret moves without asking: unsaved
    /// edits stay, nothing is lost.
    pub(crate) fn open_link(
        &mut self,
        rel: &str,
        position: Position,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.ui_state.restoring_workspace {
            self.cancel_workspace_restore();
        }
        let fragment = position
            .heading
            .clone()
            .or_else(|| position.block.as_ref().map(|block| format!("^{block}")));
        let line = position.line;
        if self.editing.is_some() {
            if self.selected_file() == rel {
                match line {
                    Some(line) => self.move_caret(line, position.column, window, cx),
                    None if fragment.is_some() => {
                        self.link_notice = Some(FRAGMENT_IN_EDITOR.into());
                        cx.notify();
                    }
                    None => {}
                }
                return;
            }
            // Another note: save this one, open the link's note, then edit it.
            self.open_note_landing(rel, None, None, None, window, cx);
            if self.editing.is_none() {
                self.pending_link_edit = Some(EditLanding {
                    generation: Some(self.navigation.preparation_generation),
                    rel: rel.to_owned(),
                    line,
                    column: position.column,
                    fragment: fragment.is_some(),
                });
            }
            return;
        }
        let fragment = fragment.filter(|_| line.is_none());
        let landing = match line {
            Some(line) => Some(prepared_links::Landing::Line(line)),
            None if fragment.is_none() => Some(prepared_links::Landing::Top),
            None => None,
        };
        self.open_note_landing(rel, None, fragment.as_deref(), landing, window, cx);
    }

    /// Called after a prepared note is accepted: an editing link returns to
    /// the editor on the link's note.
    pub(crate) fn finish_link_edit(
        &mut self,
        generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(landing) = self
            .pending_link_edit
            .take_if(|l| l.generation == Some(generation))
        else {
            return;
        };
        if self.editing.is_some() || self.pane.current_rel != landing.rel {
            return;
        }
        self.toggle_source(window, cx);
        self.land_in_editor(landing, window, cx);
    }

    /// A new window opened by a link: it opens as the vault was left. In the
    /// Reader the link lands now; a window restoring its editor lands the
    /// caret once the editor is back (`restore_ui_source`).
    pub(crate) fn open_first_link(
        &mut self,
        position: Position,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A bare note link is already where it should be.
        if position == Position::default() {
            return;
        }
        let rel = self.pane.current_rel.clone();
        if !self.retarget_restored_editor() {
            self.open_link(&rel, position, window, cx);
            return;
        }
        self.pending_link_edit = Some(EditLanding {
            generation: None,
            fragment: position.heading.is_some() || position.block.is_some(),
            rel,
            line: position.line,
            column: position.column,
        });
    }

    /// `restore_ui_source` brought the editor back.
    pub(crate) fn land_restored_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(landing) = self.pending_link_edit.take_if(|l| l.generation.is_none()) {
            if landing.rel == self.pane.current_rel {
                self.land_in_editor(landing, window, cx);
            }
        }
    }

    fn land_in_editor(
        &mut self,
        landing: EditLanding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing.is_none() {
            return;
        }
        match landing.line {
            Some(line) => self.move_caret(line, landing.column, window, cx),
            None if landing.fragment => {
                self.link_notice = Some(FRAGMENT_IN_EDITOR.into());
                cx.notify();
            }
            None => {}
        }
    }

    /// Highlight a Reader block once, fading out (#1049: a link's line
    /// shows where it landed). Reduced motion keeps it still, then clears.
    pub(crate) fn flash_block(&mut self, block: usize, cx: &mut Context<Self>) {
        let serial = self.navigation.flash.map_or(0, |(_, s)| s.wrapping_add(1));
        self.navigation.flash = Some((block, serial));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FLASH).await;
            let _ = this.update(cx, |this, cx| {
                if this.navigation.flash.is_some_and(|(_, s)| s == serial) {
                    this.navigation.flash = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The highlight over the flashed block, drawn where the list put it.
    pub(crate) fn render_landing_flash(&self, cx: &App) -> Option<AnyElement> {
        let (block, serial) = self.navigation.flash?;
        let bounds = self.content.read(cx).list_state().bounds_for_item(block)?;
        let color = cx.theme().primary.opacity(0.18);
        let mark = div()
            .debug_selector(|| "reader-landing-flash".into())
            .w(bounds.size.width)
            .h(bounds.size.height)
            .rounded(px(6.))
            .bg(color);
        let mark = if cx.reduce_motion() {
            mark.into_any_element()
        } else {
            mark.with_animation(
                ("reader-landing-flash", serial),
                Animation::new(FLASH),
                |mark, t| mark.opacity(1. - t),
            )
            .into_any_element()
        };
        Some(deferred(anchored().position(bounds.origin).child(mark)).into_any_element())
    }

    /// The heading under `position` in the Reader, by the list's real block
    /// bounds; `None` off a heading.
    pub(crate) fn heading_at(&self, position: Point<Pixels>, cx: &App) -> Option<String> {
        // A right-click on a link belongs to the link's own menu (#943).
        if self.editing.is_some() || self.pane.file_preview.is_some() || self.pointer_on_link() {
            return None;
        }
        let list = self.content.read(cx).list_state();
        self.pane
            .outline
            .iter()
            .find(|heading| {
                list.bounds_for_item(heading.target.block)
                    .is_some_and(|bounds| bounds.contains(&position))
            })
            .map(|heading| heading.text.clone())
    }

    /// «Copy link to heading»: this note at `#heading` (#1049); in a
    /// single-file view, this file's own link (#1439).
    pub(crate) fn copy_heading_link(
        &mut self,
        heading: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::reader_files::copy_link_to_clipboard(
            &self.vault_root,
            &self.pane.current_rel,
            self.link_scope(),
            &Position {
                heading: Some(heading),
                ..Default::default()
            },
            window,
            cx,
        );
    }

    /// «Copy link to line»: the caret's line and column in this note (#1049).
    pub(crate) fn copy_link_to_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.source_input() else {
            return;
        };
        let caret = input.read(cx).cursor_position();
        crate::reader_files::copy_link_to_clipboard(
            &self.vault_root,
            self.selected_file(),
            self.link_scope(),
            &Position {
                line: Some(caret.line + 1),
                column: Some(caret.character + 1),
                ..Default::default()
            },
            window,
            cx,
        );
    }

    /// Caret at a 1-based `line:column` (column in characters), scrolled into
    /// view. Out of range clamps to the last line or column.
    fn move_caret(
        &mut self,
        line: u32,
        column: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self.source_input() else {
            return;
        };
        let line = line.saturating_sub(1);
        let character = column.unwrap_or(1).saturating_sub(1);
        let place =
            move |input: &mut EditorState, window: &mut Window, cx: &mut Context<EditorState>| {
                let last = input.text().lines_len().saturating_sub(1) as u32;
                input.set_cursor_position(
                    gpui_component::input::Position {
                        line: line.min(last),
                        character,
                    },
                    window,
                    cx,
                );
            };
        // Now, so the caret is right at once; again after the next frame,
        // because the editor scrolls only once it has a layout.
        input.update(cx, |input, cx| place(input, window, cx));
        window.on_next_frame(move |window, cx| {
            input.update(cx, |input, cx| place(input, window, cx));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use std::time::Duration;

    fn at(line: u32, column: Option<u32>) -> Position {
        Position {
            line: Some(line),
            column,
            ..Default::default()
        }
    }

    #[gpui::test]
    fn copy_link_to_heading_names_the_heading_under_the_pointer(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Notes");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("Plan.md"),
            "# Plan\n\nIntro paragraph.\n\n## Next steps & risks\n\nBody.\n",
        )
        .unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Plan.md".into()),
                        index_dir: Some(temp.path().join("cache")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.simulate_resize(size(px(1000.), px(600.)));
        visual.run_until_parked();
        let (heading, paragraph) = reader.read_with(visual, |v, cx| {
            let list = v.content.read(cx).list_state();
            let heading = v
                .pane
                .outline
                .iter()
                .find(|h| h.text == "Next steps & risks")
                .unwrap();
            (
                list.bounds_for_item(heading.target.block).unwrap().center(),
                list.bounds_for_item(1).unwrap().center(),
            )
        });
        reader.read_with(visual, |v, cx| {
            assert_eq!(
                v.heading_at(heading, cx).as_deref(),
                Some("Next steps & risks")
            );
            assert_eq!(
                v.heading_at(paragraph, cx),
                None,
                "positive control: a paragraph"
            );
        });
        // #943: over a link inside the heading, only the link's menu opens.
        reader.update_in(visual, |v, _, cx| {
            v.pointer_link = Some((
                "okilum://open/Other.md".into(),
                v.navigation.preparation_generation,
            ));
            assert_eq!(
                v.heading_at(heading, cx),
                None,
                "a link claims the right-click"
            );
            v.pointer_link = None;
            assert!(
                v.heading_at(heading, cx).is_some(),
                "positive control: off the link"
            );
        });
        let identity = root.join(okilum_core::vault_link_identity::FILE);
        assert!(
            !root.join(".okilum").exists(),
            "finding the heading writes nothing"
        );
        reader.update_in(visual, |v, window, cx| {
            v.copy_heading_link("Next steps & risks".into(), window, cx);
            let link = cx.read_from_clipboard().unwrap().text().unwrap();
            // #1414: the folder name for older Okilum, the identity for this one.
            let id = okilum_core::vault_link_identity::read(&root)
                .unwrap()
                .expect("the first Copy creates the identity");
            assert_eq!(
                link,
                format!("okilum://v/Notes/Plan.md?vault_id={id}#Next%20steps%20%26%20risks")
            );
            let parsed = okilum_core::deep_link::parse(&link).unwrap();
            assert_eq!(
                parsed.position.heading.as_deref(),
                Some("Next steps & risks")
            );
            // Broken identity metadata: refused, clipboard and bytes kept.
            std::fs::write(&identity, "broken identity").unwrap();
            cx.write_to_clipboard(ClipboardItem::new_string("keep clipboard".into()));
            v.copy_heading_link("Plan".into(), window, cx);
            v.copy_document_link(window, cx);
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("keep clipboard")
            );
            assert_eq!(
                std::fs::read_to_string(&identity).unwrap(),
                "broken identity"
            );
        });
    }

    /// #1439: every Copy in a single-file view (the note, a heading, the
    /// caret, a link's address) copies `okilum://file/…` for this folder's
    /// files, which resolves to the file on its own; in a vault the same
    /// Copies carry the vault's identity. Nothing is written beside the file.
    #[gpui::test]
    fn every_copy_names_the_file_in_a_single_file_view_and_the_vault_otherwise(
        cx: &mut TestAppContext,
    ) {
        use okilum_core::deep_link::{parse, resolve, Address, Resolution};
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("Outside");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(
            folder.join("Landing.md"),
            "# Landing\n\nSee [[Other]].\n\n## Distant checkpoint\n\nBody.\n",
        )
        .unwrap();
        std::fs::write(folder.join("Other.md"), "# Other\n").unwrap();
        let folder = folder.canonicalize().unwrap();
        fn open<'a>(
            cx: &'a mut TestAppContext,
            folder: &Path,
            state: &Path,
            single_file: bool,
        ) -> (Entity<Reader>, &'a mut gpui::VisualTestContext) {
            let mut reader = None;
            let (_, visual) = cx.add_window_view(|window, cx| {
                let view = cx.new(|cx| {
                    Reader::new(
                        Opts {
                            vault: Some(folder.to_path_buf()),
                            note: Some("Landing.md".into()),
                            single_file,
                            index_dir: Some(state.join(format!("cache-{single_file}"))),
                            session_directory: Some(state.join(format!("state-{single_file}"))),
                            ..Default::default()
                        },
                        window,
                        cx,
                    )
                });
                reader = Some(view.clone());
                Root::new(view, window, cx)
            });
            visual.simulate_resize(size(px(1000.), px(600.)));
            visual.run_until_parked();
            (reader.unwrap(), visual)
        }
        // Every Copy entry point, as its menu item or command calls it.
        let copies = |reader: &Entity<Reader>, visual: &mut gpui::VisualTestContext| {
            reader.update_in(visual, |v, window, cx| {
                let mut copied = Vec::new();
                let take =
                    |cx: &mut Context<Reader>| cx.read_from_clipboard().unwrap().text().unwrap();
                v.copy_document_link(window, cx);
                copied.push(("note", take(cx)));
                v.copy_heading_link("Distant checkpoint".into(), window, cx);
                copied.push(("heading", take(cx)));
                let target = crate::reader_context_menu::LinkTarget::Document {
                    rel: "Other.md".into(),
                    position: Default::default(),
                };
                target.copy(&v.vault_root, v.link_scope(), window, cx);
                copied.push(("link address", take(cx)));
                v.toggle_source(window, cx);
                copied
            })
        };
        let caret = |reader: &Entity<Reader>, visual: &mut gpui::VisualTestContext| {
            visual.run_until_parked();
            reader.update_in(visual, |v, window, cx| {
                assert!(v.editing.is_some(), "the caret needs the editor");
                v.copy_link_to_line(window, cx);
                cx.read_from_clipboard().unwrap().text().unwrap()
            })
        };

        let (reader, visual) = open(cx, &folder, temp.path(), true);
        assert!(reader.read_with(visual, |v, _| v.single_file));
        let mut copied = copies(&reader, &mut *visual);
        copied.push(("caret", caret(&reader, &mut *visual)));
        for (entry, link) in &copied {
            let parsed = parse(link).unwrap_or_else(|e| panic!("{entry}: {link}: {e:?}"));
            assert!(
                matches!(parsed.address, Address::File(_)),
                "{entry}: {link}"
            );
            let want = if *entry == "link address" {
                "Other.md"
            } else {
                "Landing.md"
            };
            match resolve(&parsed, &[], &|p| p.is_file()) {
                Resolution::SingleFile { file, position } => {
                    assert_eq!(file, folder.join(want), "{entry}");
                    match *entry {
                        "heading" => {
                            assert_eq!(position.heading.as_deref(), Some("Distant checkpoint"))
                        }
                        "caret" => assert!(position.line.is_some(), "{link}"),
                        _ => assert_eq!(position, Default::default(), "{entry}"),
                    }
                }
                other => panic!("{entry}: {link}: {other:?}"),
            }
        }
        assert!(
            !folder.join(".okilum").exists(),
            "a single-file view writes nothing beside its file"
        );

        // Positive control: the same Copies in a vault window carry its identity.
        let (reader, visual) = open(cx, &folder, temp.path(), false);
        let mut copied = copies(&reader, &mut *visual);
        copied.push(("caret", caret(&reader, &mut *visual)));
        let id = okilum_core::vault_link_identity::read(&folder)
            .unwrap()
            .unwrap();
        for (entry, link) in &copied {
            match parse(link).unwrap().address {
                Address::VaultId {
                    id: copied_id,
                    vault,
                    ..
                } => {
                    assert_eq!(
                        (copied_id.as_str(), vault.as_str()),
                        (id.as_str(), "Outside"),
                        "{entry}"
                    )
                }
                other => panic!("{entry}: {link}: {other:?}"),
            }
        }
    }

    #[gpui::test]
    fn links_land_on_lines_in_reader_and_editor(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        // Paragraph i is block i-1 at file line 2i+2 (three frontmatter lines).
        let body: String = (1..=60).map(|i| format!("Paragraph {i}.\n\n")).collect();
        let target = format!("---\ntitle: Target\n---\n{body}");
        std::fs::write(root.join("target.md"), &target).unwrap();
        std::fs::write(root.join("start.md"), "# Start\n\nFirst line.\n").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("start.md".into()),
                        index_dir: Some(temp.path().join("cache")),
                        // Editing needs a draft-recovery directory.
                        session_directory: Some(temp.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.simulate_resize(size(px(1000.), px(600.)));
        visual.run_until_parked();
        let settle = |visual: &mut gpui::VisualTestContext| {
            visual.run_until_parked();
            visual.executor().advance_clock(Duration::from_millis(300));
            visual.run_until_parked();
        };
        let top =
            |v: &Reader, cx: &App| v.content.read(cx).list_state().logical_scroll_top().item_ix;

        // Reader: line 82 is paragraph 40, block 39.
        reader.update_in(visual, |v, window, cx| {
            v.open_link("target.md", at(82, None), window, cx)
        });
        settle(visual);
        reader.read_with(visual, |v, cx| {
            assert_eq!(v.pane.current_rel, "target.md");
            assert!(v.editing.is_none(), "a link never enters the editor");
            assert!(v.link_notice.is_none());
            assert_eq!(top(v, cx), 39);
            assert_eq!(
                v.navigation.flash.map(|(block, _)| block),
                Some(39),
                "landed block flashes"
            );
        });
        assert!(
            visual.debug_bounds("reader-landing-flash").is_some(),
            "the highlight is drawn"
        );
        visual.executor().advance_clock(Duration::from_millis(1500));
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert!(v.navigation.flash.is_none(), "it fades once")
        });
        // Positive control: the same note without a line opens at the top.
        reader.update_in(visual, |v, window, cx| {
            v.open_link("start.md", Position::default(), window, cx)
        });
        settle(visual);
        reader.update_in(visual, |v, window, cx| {
            v.open_link("target.md", Position::default(), window, cx)
        });
        settle(visual);
        reader.read_with(visual, |v, cx| {
            assert_eq!(top(v, cx), 0);
            assert!(
                v.navigation.flash.is_none(),
                "a bare link highlights nothing"
            );
        });

        // Editor, same note, unsaved edit: the caret moves, the edit stays.
        reader.update_in(visual, |v, window, cx| {
            v.toggle_source(window, cx);
            let edited = target.replacen("Paragraph 1.", "Paragraph one.", 1);
            v.editing.as_ref().unwrap().set_value(&edited, window, cx);
            v.open_link("target.md", at(10, Some(4)), window, cx);
        });
        settle(visual);
        reader.read_with(visual, |v, cx| {
            assert_eq!(v.editing_caret_line(cx), Some(10));
            let input = v.source_input().unwrap();
            let input = input.read(cx);
            assert_eq!(
                input.cursor_position().character,
                3,
                "column counts characters"
            );
            assert!(
                input.value().contains("Paragraph one."),
                "unsaved edit kept"
            );
        });
        assert_eq!(
            std::fs::read_to_string(root.join("target.md")).unwrap(),
            target
        );
        // Out of range clamps to the last line.
        reader.update_in(visual, |v, window, cx| {
            v.open_link("target.md", at(9999, None), window, cx)
        });
        settle(visual);
        reader.read_with(visual, |v, cx| {
            let last = v.source_input().unwrap().read(cx).text().lines_len();
            assert_eq!(v.editing_caret_line(cx), Some(last));
        });

        // Another note while editing: saved, opened, still in the editor.
        reader.update_in(visual, |v, window, cx| {
            v.open_link("start.md", at(3, Some(6)), window, cx)
        });
        settle(visual);
        reader.read_with(visual, |v, cx| {
            assert_eq!(v.pane.current_rel, "start.md");
            assert!(v.editing.is_some(), "the editor stays the mode");
            assert_eq!(v.editing_caret_line(cx), Some(3));
        });
        assert!(std::fs::read_to_string(root.join("target.md"))
            .unwrap()
            .contains("Paragraph one."));
    }
}
