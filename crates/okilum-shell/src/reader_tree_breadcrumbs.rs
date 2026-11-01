//! One scroll-context row; document navigation never supplies its ancestry.
use super::*;
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use okilum_core::vault::EntryKind;
use std::collections::HashMap;

pub(crate) const HEIGHT: f32 = 28.;

/// Built with the virtual list's rows, not while handling individual pixels.
pub(crate) struct Ancestors {
    rows: Vec<Option<reader_tree::Row>>,
    folders: HashMap<String, (usize, reader_tree::Row)>,
}
impl Ancestors {
    pub(crate) fn new(rows: Vec<Option<reader_tree::Row>>) -> Self {
        let folders = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                row.as_ref()
                    .filter(|row| row.kind == EntryKind::Directory)
                    .map(|row| (row.path.clone(), (index, row.clone())))
            })
            .collect();
        Self { rows, folders }
    }

    pub(crate) fn at(&self, first: usize) -> Vec<reader_tree::Row> {
        if first == 0 {
            return vec![];
        }
        let Some(current) = self.rows.iter().skip(first).find_map(Option::as_ref) else {
            return vec![];
        };
        let mut result = Vec::new();
        let mut path = current.path.as_str();
        while let Some((parent, _)) = path.rsplit_once('/') {
            if let Some((index, row)) = self.folders.get(parent) {
                if *index < first {
                    result.push(row.clone());
                }
            }
            path = parent;
        }
        result.reverse();
        result
    }
}

/// Keep both ends; every omitted segment remains an explicit menu destination.
fn compact(widths: &[f32], available: f32) -> (Vec<usize>, Vec<usize>) {
    let total = widths.iter().sum::<f32>() + 16. * widths.len().saturating_sub(1) as f32;
    if widths.len() < 3 || total <= available {
        return ((0..widths.len()).collect(), vec![]);
    }
    let last = widths.len() - 1;
    let mut tail = last;
    let mut used = widths[0] + widths[last] + 60.;
    while tail > 2 && used + widths[tail - 1] + 16. <= available {
        tail -= 1;
        used += widths[tail] + 16.;
    }
    (
        std::iter::once(0).chain(tail..widths.len()).collect(),
        (1..tail).collect(),
    )
}

impl Reader {
    pub(crate) fn vault_index_path(&self) -> Option<String> {
        match self.tree.index_of("") {
            reader_tree::FolderIndex::One(path) => Some(path),
            _ => None,
        }
    }

    pub(crate) fn render_tree_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let path = self.vault_index_path();
        let root = self.vault_root.clone();
        let generation = self.watcher_generation;
        let reader = cx.entity().downgrade();
        let open_reader = reader.clone();
        let open_root = root.clone();
        let open_path = path.clone();
        let tooltip = path.as_ref().map_or_else(
            || "No unambiguous _index.md in this vault".to_owned(),
            |path| format!("{path}\n{}", root.join(path).display()),
        );
        let pinned = path
            .as_ref()
            .is_some_and(|path| self.sidebar.pinned.contains(path));
        let title = Button::new("tree-vault-index")
            .ghost()
            .small()
            .icon(IconName::FileText)
            .label("Vault Index")
            .tooltip(tooltip)
            .disabled(path.is_none())
            .min_w_0()
            .overflow_hidden()
            .debug_selector(|| "tree-vault-index".into())
            .on_click(move |_, window, cx| {
                let Some(path) = &open_path else { return };
                let _ = open_reader.update(cx, |this, cx| {
                    if this.vault_root == open_root && this.watcher_generation == generation {
                        this.select_panel_note(reader_layout::Panel::Notes, path, None, window, cx);
                    }
                });
            });
        let menu_reader = reader.clone();
        let title = div()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .child(title)
            .context_menu(move |menu, _, _| {
                let Some(path) = path.clone() else {
                    return menu;
                };
                let reveal_root = root.clone();
                let reveal_path = path.clone();
                let reveal_reader = menu_reader.clone();
                let pin_root = root.clone();
                let pin_reader = menu_reader.clone();
                menu.item(
                    PopupMenuItem::new(crate::platform::labels::Os::CURRENT.reveal()).on_click(
                        move |_, window, cx| {
                            let _ = reveal_reader.update(cx, |this, cx| {
                                if this.vault_root == reveal_root
                                    && this.watcher_generation == generation
                                {
                                    reader_files::run(
                                        reader_files::FileAction::Reveal,
                                        &reveal_root,
                                        &reveal_path,
                                        window,
                                        cx,
                                    );
                                }
                            });
                        },
                    ),
                )
                .item(
                    PopupMenuItem::new(if pinned { "Unpin" } else { "Pin" }).on_click(
                        move |_, _, cx| {
                            let _ = pin_reader.update(cx, |this, cx| {
                                if this.vault_root == pin_root
                                    && this.watcher_generation == generation
                                {
                                    this.toggle_pin(&path, cx);
                                }
                            });
                        },
                    ),
                )
            });
        let mut actions = h_flex().flex_none().gap_0p5();
        for action in [
            SidebarAction::NewNote,
            SidebarAction::NewFolder,
            SidebarAction::CollapseAll,
        ] {
            if !SidebarAction::available().contains(&action) {
                continue;
            }
            actions = actions.child(
                reader_icon_button(
                    action.id(),
                    action.icon(self.sidebar.show_hidden),
                    action.tooltip(cx),
                    cx,
                )
                .debug_selector(move || action.id().into())
                .on_click(sidebar_action_click(reader.clone(), action)),
            );
        }
        h_flex()
            .debug_selector(|| "tree-index-header".into())
            .h(px(32.))
            .flex_none()
            .w_full()
            .px_2()
            .gap_1()
            .child(title)
            .child(actions)
            .into_any_element()
    }

    fn scroll_to_tree_ancestor(&mut self, path: &str, cx: &mut Context<Self>) {
        let index = self
            .sidebar_items_named("")
            .iter()
            .position(|item| matches!(item, SideItem::Tree(row) if row.path == path));
        if let Some(index) = index {
            // Leave one real row above it, so the overlay cannot cover the target.
            self.tree_scroll
                .scroll_to_item_strict(index.saturating_sub(1), ScrollStrategy::Top);
            cx.notify();
        }
    }

    pub(crate) fn render_tree_breadcrumbs(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rows = &self.tree_sticky;
        if rows.is_empty() {
            return div().into_any_element();
        }
        let measured = f32::from(self.tree_scroll.0.borrow().base_handle.bounds().size.width);
        let width = if measured > 0. {
            measured
        } else {
            self.panel_widths.notes - 16.
        };
        let widths: Vec<_> = rows
            .iter()
            .map(|row| toolbar_text_width(&row.label, FontWeight::NORMAL, window) + 12.)
            .collect();
        let (visible, hidden) = compact(&widths, (width - 12.).max(0.));
        let root = self.vault_root.clone();
        let generation = self.watcher_generation;
        let reader = cx.entity().downgrade();
        let click = move |path: String| {
            let reader = reader.clone();
            let root = root.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                let _ = reader.update(cx, |this, cx| {
                    if this.vault_root == root && this.watcher_generation == generation {
                        this.scroll_to_tree_ancestor(&path, cx);
                    }
                });
            }
        };
        let mut strip = h_flex()
            .id("tree-scroll-breadcrumbs")
            .debug_selector(|| "tree-scroll-breadcrumbs".into())
            .absolute()
            .top_0()
            .left_0()
            .w_full()
            .h(px(HEIGHT))
            .px_1()
            .gap_0p5()
            .overflow_hidden()
            .bg(brand::palette(cx).sidebar)
            .occlude()
            .text_size(px(12.));
        for (position, index) in visible.iter().copied().enumerate() {
            if position > 0 {
                strip = strip.child(
                    Icon::new(IconName::ChevronRight)
                        .xsmall()
                        .flex_none()
                        .text_color(brand::palette(cx).text_muted),
                );
            }
            if position == 1 && !hidden.is_empty() {
                let omitted: Vec<_> = hidden.iter().map(|&index| rows[index].clone()).collect();
                let click = click.clone();
                strip = strip
                    .child(
                        Button::new("tree-breadcrumb-more")
                            .ghost()
                            .xsmall()
                            .label("…")
                            .tooltip("More parent folders")
                            .flex_none()
                            .debug_selector(|| "tree-breadcrumb-more".into())
                            .dropdown_menu(move |menu, _, _| {
                                omitted.iter().fold(menu, |menu, row| {
                                    menu.item(
                                        PopupMenuItem::new(row.label.clone())
                                            .on_click(click(row.path.clone())),
                                    )
                                })
                            }),
                    )
                    .child(
                        Icon::new(IconName::ChevronRight)
                            .xsmall()
                            .flex_none()
                            .text_color(brand::palette(cx).text_muted),
                    );
            }
            let row = &rows[index];
            strip = strip.child(
                Button::new(SharedString::from(format!("tree-breadcrumb-{}", row.path)))
                    .ghost()
                    .xsmall()
                    .label(row.label.clone())
                    .tooltip(row.path.clone())
                    .min_w_0()
                    .overflow_hidden()
                    .debug_selector({
                        let path = row.path.clone();
                        move || format!("tree-breadcrumb-{path}")
                    })
                    .on_click(click(row.path.clone())),
            );
        }
        strip.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn narrow_breadcrumbs_keep_both_ends_and_every_middle_destination() {
        let (visible, hidden) = compact(&[60., 60., 60., 60., 60.], 220.);
        assert_eq!(visible, [0, 4]);
        assert_eq!(hidden, [1, 2, 3]);
        let (visible, hidden) = compact(&[60.; 5], 500.);
        assert_eq!(visible, [0, 1, 2, 3, 4]);
        assert!(hidden.is_empty());
    }

    fn fixture(
        cx: &mut TestAppContext,
    ) -> (Entity<Reader>, &mut VisualTestContext, tempfile::TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let vault = temp.path().join("vault");
        std::fs::create_dir_all(vault.join("Area/LongDeepDirectory/Leaf")).unwrap();
        std::fs::write(vault.join("_Index.md"), "# Vault Index — Bird View\n").unwrap();
        std::fs::write(vault.join("Other.md"), "# Another document\n").unwrap();
        for index in 0..50 {
            std::fs::write(
                vault.join(format!("Area/LongDeepDirectory/Leaf/Note {index:02}.md")),
                "# Note\n",
            )
            .unwrap();
        }
        cx.update(|cx| {
            crate::test_init(cx);
            bind_keys(cx);
            reader_ui_state::install(&temp.path().join("state"), cx);
            cx.set_global(reader_history::TestSessionDirectory(
                temp.path().join("history"),
            ));
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(vault),
                        note: Some("Other.md".into()),
                        index_dir: Some(temp.path().join("index")),
                        panel_settings_override: Some(temp.path().join("panels.json")),
                        session_directory: Some(temp.path().join("session")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        visual.run_until_parked();
        visual.simulate_resize(size(px(1400.), px(600.)));
        let reader = reader.unwrap();
        reader.update(visual, |view, cx| {
            view.panels.open(reader_layout::Panel::Notes);
            view.panel_widths.notes = 225.;
            view.tree.set_subtree("Area", true);
            view.tree.cursor = None;
            cx.notify();
        });
        visual.run_until_parked();
        (reader, visual, temp)
    }

    #[gpui::test]
    fn index_header_uses_actual_file_and_permanent_creation_actions(cx: &mut TestAppContext) {
        let (reader, visual, temp) = fixture(cx);
        for selector in [
            "tree-vault-index",
            "sidebar-new-note",
            "sidebar-new-folder",
            "sidebar-collapse-all",
        ] {
            assert!(
                visual.debug_bounds(selector).is_some(),
                "missing permanent {selector}"
            );
        }
        let index = visual.debug_bounds("tree-vault-index").unwrap();
        visual.simulate_click(index.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |view, _| {
            assert_eq!(view.selected_file(), "_Index.md")
        });
        assert_eq!(
            std::fs::read_to_string(temp.path().join("vault/_Index.md")).unwrap(),
            "# Vault Index — Bird View\n"
        );
        reader.update(visual, |view, cx| {
            view.tree.cursor = Some("Area/LongDeepDirectory/Leaf".into());
            cx.notify();
        });
        visual.run_until_parked();
        let note = visual.debug_bounds("sidebar-new-note").unwrap();
        visual.simulate_click(note.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |view, _| {
            assert_eq!(
                view.creation.as_ref().unwrap().folder,
                "Area/LongDeepDirectory/Leaf"
            )
        });
        reader.update_in(visual, |view, window, cx| {
            view.cancel_creation(window, cx);
            view.tree.cursor = None;
        });
        visual.run_until_parked();
        let folder = visual.debug_bounds("sidebar-new-folder").unwrap();
        visual.simulate_click(folder.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |view, _| {
            assert_eq!(view.creation.as_ref().unwrap().folder, "")
        });
    }

    #[gpui::test]
    fn one_scroll_context_row_preserves_geometry_expansion_and_document(cx: &mut TestAppContext) {
        let (reader, visual, _temp) = fixture(cx);
        assert!(visual.debug_bounds("tree-scroll-breadcrumbs").is_none());
        let header = visual.debug_bounds("tree-index-header").unwrap();
        let viewport = visual.debug_bounds("sidebar-tree-body").unwrap();
        let expanded = reader.read_with(visual, |view, _| view.tree.expanded_paths().clone());
        reader.update(visual, |view, cx| {
            let first = view.sidebar_items_named("").iter().position(|item|
                matches!(item, SideItem::Tree(row) if row.path == "Area/LongDeepDirectory/Leaf/Note 10.md")
            ).unwrap();
            view.tree_scroll.scroll_to_item_strict(first, ScrollStrategy::Top);
            cx.notify();
        });
        visual.run_until_parked();
        let strip = visual
            .debug_bounds("tree-scroll-breadcrumbs")
            .expect("scrolled context");
        assert_eq!(strip.size.height, px(HEIGHT));
        assert!(visual.debug_bounds("tree-breadcrumb-more").is_some());
        assert_eq!(header, visual.debug_bounds("tree-index-header").unwrap());
        assert_eq!(viewport, visual.debug_bounds("sidebar-tree-body").unwrap());
        let offset = reader.read_with(visual, |view, _| {
            assert_eq!(
                view.selected_file(),
                "Other.md",
                "breadcrumbs follow scrolling, not document"
            );
            assert_eq!(
                view.tree_sticky
                    .iter()
                    .map(|row| row.path.as_str())
                    .collect::<Vec<_>>(),
                [
                    "Area",
                    "Area/LongDeepDirectory",
                    "Area/LongDeepDirectory/Leaf"
                ]
            );
            view.tree_scroll.0.borrow().base_handle.offset()
        });
        reader.update(visual, |_, cx| cx.notify());
        visual.run_until_parked();
        reader.read_with(visual, |view, _| {
            assert_eq!(view.tree_scroll.0.borrow().base_handle.offset(), offset)
        });
        let parent = visual
            .debug_bounds("tree-breadcrumb-Area/LongDeepDirectory/Leaf")
            .unwrap();
        visual.simulate_click(parent.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |view, _| {
            assert_eq!(view.tree.expanded_paths(), &expanded);
            assert_eq!(view.selected_file(), "Other.md");
        });
        let target = visual
            .debug_bounds("tree-row-Area/LongDeepDirectory/Leaf")
            .unwrap();
        assert!(
            target.top()
                >= visual
                    .debug_bounds("tree-scroll-breadcrumbs")
                    .map_or(viewport.top(), |strip| strip.bottom())
        );
        assert!(target.bottom() <= viewport.bottom());
        reader.update(visual, |view, cx| {
            view.tree_scroll
                .scroll_to_item_strict(0, ScrollStrategy::Top);
            cx.notify();
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("tree-scroll-breadcrumbs").is_none());
    }
}
