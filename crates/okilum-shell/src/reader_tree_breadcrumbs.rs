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
    roots: Vec<(usize, reader_tree::Row)>,
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
        let roots = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                row.as_ref()
                    .filter(|row| row.kind == EntryKind::Directory && !row.path.contains('/'))
                    .map(|row| (index, row.clone()))
            })
            .collect();
        Self {
            rows,
            folders,
            roots,
        }
    }

    pub(crate) fn earlier_roots(&self, first: usize) -> Vec<reader_tree::Row> {
        let Some(current) = self.rows.iter().skip(first).find_map(Option::as_ref) else {
            return vec![];
        };
        let current_root = current.path.split('/').next().unwrap_or("");
        let end = self.roots.partition_point(|(index, _)| *index < first);
        self.roots[..end]
            .iter()
            .filter(|(_, row)| row.path != current_root)
            .map(|(_, row)| row.clone())
            .collect()
    }

    pub(crate) fn context(&self, first: usize) -> (Vec<reader_tree::Row>, Vec<reader_tree::Row>) {
        let earlier = self.earlier_roots(first);
        let mut ancestors = self.at(first);
        // Keep the root's name when the count overlays that root-folder row.
        if ancestors.is_empty() && !earlier.is_empty() {
            if let Some(row) = self
                .rows
                .iter()
                .skip(first)
                .find_map(Option::as_ref)
                .filter(|row| row.kind == EntryKind::Directory)
            {
                ancestors.push(row.clone());
            }
        }
        (ancestors, earlier)
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

const INDEX_CONTENT_HEIGHT: f32 = 20.;
const INDEX_GLYPH_SIZE: f32 = 14.;

/// The same compact Contents SVG as Icon::small, with its actual paint bounds
/// exposed to the header regression. Inherit the button's normal/disabled tint.
#[derive(IntoElement)]
struct IndexGlyph;
impl RenderOnce for IndexGlyph {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        svg()
            .path("icons/list.svg")
            .size(px(INDEX_GLYPH_SIZE))
            .flex_none()
            .text_color(window.text_style().color)
            .debug_selector(|| "tree-vault-index-glyph".into())
    }
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
            || "No unambiguous index note in this vault".to_owned(),
            |path| format!("{path}\n{}", root.join(path).display()),
        );
        let pinned = path
            .as_ref()
            .is_some_and(|path| self.sidebar.pinned.contains(path));
        let title = Button::new("tree-vault-index")
            .ghost()
            .small()
            .accessibility_label("Vault Index")
            .w_full()
            .justify_start()
            .px(px(4.))
            // Button's built-in label centers itself in the available width.
            // A full-width start-aligned child keeps Contents + title at the tree inset.
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .h(px(INDEX_CONTENT_HEIGHT))
                    .items_center()
                    .justify_start()
                    .gap_1()
                    .child(
                        div()
                            .flex_none()
                            .w(px(16.))
                            .h(px(INDEX_CONTENT_HEIGHT))
                            .flex()
                            .items_center()
                            .justify_center()
                            .debug_selector(|| "tree-vault-index-icon".into())
                            .child(IndexGlyph),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .h(px(INDEX_CONTENT_HEIGHT))
                            .line_height(px(INDEX_CONTENT_HEIGHT))
                            .text_ellipsis()
                            .debug_selector(|| "tree-vault-index-label".into())
                            .child("Vault Index"),
                    ),
            )
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
        let title = h_flex()
            .justify_start()
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
        for &action in SidebarAction::available() {
            let tooltip = if action == SidebarAction::ShowHidden {
                reader_shortcuts::hint(
                    if self.sidebar.show_hidden {
                        "Hide hidden files"
                    } else {
                        "Show hidden files"
                    },
                    action.action().as_ref(),
                    cx,
                )
            } else {
                action.tooltip(cx)
            };
            actions = actions.child(
                reader_icon_button(
                    action.id(),
                    action.icon(self.sidebar.show_hidden),
                    tooltip,
                    cx,
                )
                .selected(action == SidebarAction::ShowHidden && self.sidebar.show_hidden)
                .debug_selector(move || action.id().into())
                .on_click({
                    let reader = reader.clone();
                    move |_, window, cx| {
                        let _ = reader.update(cx, |this, cx| action.run(this, window, cx));
                    }
                }),
            );
        }
        h_flex()
            .debug_selector(|| "tree-index-header".into())
            .h(px(32.))
            .items_center()
            .flex_none()
            .w_full()
            .px_2()
            .gap_1()
            .child(title)
            .when_some(self.tree.rules_of(self.tree.scope()), |row, path| {
                row.child(reader_tree_rules::badge(
                    path,
                    reader.clone(),
                    self.vault_root.clone(),
                    generation,
                ))
            })
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
        let earlier = &self.tree_earlier_roots;
        if rows.is_empty() && earlier.is_empty() {
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
        let indicator = format!("↑ {}", earlier.len());
        let lead = if earlier.is_empty() {
            0.
        } else {
            toolbar_text_width(&indicator, FontWeight::NORMAL, window) + 24.
        };
        let (visible, hidden) = compact(&widths, (width - 12. - lead).max(0.));
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
        if !earlier.is_empty() {
            let tooltip = format!(
                "Earlier root folders\n{}",
                earlier
                    .iter()
                    .map(|row| row.label.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            let destinations = earlier.clone();
            let click = click.clone();
            strip = strip
                .child(
                    Button::new("tree-earlier-roots")
                        .ghost()
                        .xsmall()
                        .flex_none()
                        .label(indicator)
                        .accessibility_label(format!("{} earlier root folders", earlier.len()))
                        .tooltip(tooltip)
                        .debug_selector(|| "tree-earlier-roots".into())
                        .dropdown_menu(move |menu, _, _| {
                            destinations.iter().fold(menu, |menu, row| {
                                menu.item(
                                    PopupMenuItem::new(row.label.clone())
                                        .on_click(click(row.path.clone())),
                                )
                            })
                        }),
                )
                .when(!rows.is_empty(), |strip| {
                    strip.child(
                        div()
                            .flex_none()
                            .text_color(brand::palette(cx).text_muted)
                            .child("·"),
                    )
                });
        }
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
    fn earlier_roots_count_only_visible_past_root_folders() {
        let paths = [
            "AI",
            "AI/Notes",
            "Business",
            "Dev",
            "Finance",
            "Readme.md",
            "Health",
            "Health/Areas",
            "Health/Areas/Current",
            "Health/Areas/Current/note.md",
            "Healthcare",
            "Healthcare/else.md",
        ];
        let rows = paths
            .into_iter()
            .map(|path| {
                Some(reader_tree::Row {
                    path: path.into(),
                    label: path.rsplit('/').next().unwrap().into(),
                    kind: if path.ends_with(".md") {
                        EntryKind::Markdown
                    } else {
                        EntryKind::Directory
                    },
                    depth: path.matches('/').count(),
                    expanded: true,
                    archived: false,
                    hidden: false,
                    index: false,
                    agent_rules: None,
                    agent_shim: false,
                })
            })
            .collect();
        let source = Ancestors::new(rows);
        assert!(source.earlier_roots(0).is_empty());
        assert!(
            source.earlier_roots(1).is_empty(),
            "current root is not an earlier sibling"
        );
        let (parents, earlier) = source.context(9);
        assert_eq!(
            earlier
                .iter()
                .map(|row| row.path.as_str())
                .collect::<Vec<_>>(),
            ["AI", "Business", "Dev", "Finance"]
        );
        assert_eq!(
            parents
                .iter()
                .map(|row| row.path.as_str())
                .collect::<Vec<_>>(),
            ["Health", "Health/Areas", "Health/Areas/Current"]
        );
        let (parents, earlier) = source.context(6);
        assert_eq!(
            earlier.len(),
            4,
            "root boundary excludes that root and root-level files"
        );
        assert_eq!(parents[0].path, "Health");
        assert_eq!(
            source.earlier_roots(11).len(),
            5,
            "Health is a sibling of Healthcare, not its parent"
        );
        assert!(source.earlier_roots(paths.len()).is_empty());
    }

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
        fixture_with_branch(cx, "Area/LongDeepDirectory/Leaf", &[])
    }

    fn fixture_with_branch<'a>(
        cx: &'a mut TestAppContext,
        branch: &str,
        roots: &[&str],
    ) -> (Entity<Reader>, &'a mut VisualTestContext, tempfile::TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let vault = temp.path().join("vault");
        std::fs::create_dir_all(vault.join(branch)).unwrap();
        for folder in roots {
            std::fs::create_dir_all(vault.join(folder)).unwrap();
        }
        std::fs::write(vault.join("_Index.md"), "# Vault Index — Bird View\n").unwrap();
        std::fs::write(vault.join("Other.md"), "# Another document\n").unwrap();
        std::fs::write(vault.join("_hidden.md"), "# Hidden fixture\n").unwrap();
        for index in 0..50 {
            std::fs::write(
                vault.join(format!("{branch}/Note {index:02}.md")),
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
            view.tree
                .set_subtree(branch.split('/').next().unwrap(), true);
            view.tree.cursor = None;
            cx.notify();
        });
        visual.run_until_parked();
        (reader, visual, temp)
    }

    #[gpui::test]
    fn issue1587_folder_rules_badge_opens_hidden_rules_without_toggling_folder(
        cx: &mut TestAppContext,
    ) {
        let (reader, visual, temp) = fixture_with_branch(cx, "Project", &["Other"]);
        let vault = temp.path().join("vault");
        for (path, source) in [
            ("AGENTS.md", "# Root agent rules\n"),
            ("Project/AGENTS.md", "# Project agent rules\n"),
            ("Project/CLAUDE.md", "\u{feff} @./AGENTS.md\r\n"),
            ("Project/GEMINI.md", "# Real provider rules\n"),
        ] {
            std::fs::write(vault.join(path), source).unwrap();
        }
        for folder in [".claude", ".agents", ".codex"] {
            std::fs::create_dir(vault.join(format!("Project/{folder}"))).unwrap();
            std::fs::write(
                vault.join(format!("Project/{folder}/config.md")),
                "# Config\n",
            )
            .unwrap();
        }
        reader.update(visual, |view, cx| {
            view.vault = Arc::new(Vault::scan(&vault).unwrap());
            view.sync_tree();
            view.tree.set_expanded("Project", false);
            view.tree.cursor = Some("Other".into());
            cx.notify();
        });
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("tree-agent-rules-AGENTS.md").is_some(),
            "root rules are accessible from the header"
        );
        assert!(visual
            .debug_bounds("tree-agent-rules-Other/AGENTS.md")
            .is_none());
        reader.read_with(visual, |view, _| {
            assert!(!view
                .tree
                .rows
                .iter()
                .any(|r| r.path.ends_with("/AGENTS.md")));
            assert_eq!(
                view.tree.rules_of("Project").as_deref(),
                Some("Project/AGENTS.md")
            );
        });
        let badge = visual
            .debug_bounds("tree-agent-rules-Project/AGENTS.md")
            .expect("collapsed folder has its badge");
        let row = visual.debug_bounds("tree-row-Project").unwrap();
        assert!(badge.left() >= row.left() && badge.right() <= row.right());
        visual.simulate_click(badge.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |view, _| {
            assert_eq!(
                view.current_rel, "Project/AGENTS.md",
                "real click opens the hidden file"
            );
            assert!(
                !view.tree.expanded_paths().contains("Project"),
                "badge click must not expand the folder"
            );
            assert_ne!(
                view.tree.cursor.as_deref(),
                Some("Project"),
                "folder selection must not receive the click"
            );
        });
        let root = visual.debug_bounds("tree-agent-rules-AGENTS.md").unwrap();
        visual.simulate_click(root.center(), Modifiers::default());
        visual.run_until_parked();
        reader.read_with(visual, |view, _| assert_eq!(view.current_rel, "AGENTS.md"));
        reader.update(visual, |view, cx| {
            view.tree.set_subtree("Project", true);
            cx.notify();
        });
        visual.run_until_parked();
        reader.update(visual, |view, cx| {
            view.tree.set_show_hidden(true);
            view.sidebar.show_hidden = true;
            view.refresh_tree_rules(cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |view, _| {
            assert!(
                view.tree
                    .rows
                    .iter()
                    .find(|r| r.path == "Project/CLAUDE.md")
                    .unwrap()
                    .agent_shim
            )
        });
        std::fs::write(vault.join("Project/CLAUDE.md"), "# Own rules\n").unwrap();
        reader.update(visual, |view, cx| {
            // Source-only changes can reuse the same inventory Arc.
            view.source_publication_epoch += 1;
            view.refresh_tree_rules(cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |view, _| {
            assert!(
                !view
                    .tree
                    .rows
                    .iter()
                    .find(|r| r.path == "Project/CLAUDE.md")
                    .unwrap()
                    .agent_shim
            )
        });
        reader.update(visual, |view, cx| {
            view.tree.set_show_hidden(false);
            view.sidebar.show_hidden = false;
            cx.notify();
        });
        visual.run_until_parked();
        for enabled in [true, false] {
            let toggle = visual.debug_bounds("sidebar-show-hidden").unwrap();
            visual.simulate_click(toggle.center(), Modifiers::default());
            visual.run_until_parked();
            reader.read_with(visual, |view, _| {
                assert_eq!(view.tree.show_hidden(), enabled);
                for path in [
                    "Project/AGENTS.md",
                    "Project/CLAUDE.md",
                    "Project/GEMINI.md",
                    "Project/.claude",
                    "Project/.agents",
                    "Project/.codex",
                ] {
                    assert_eq!(
                        view.tree.rows.iter().any(|r| r.path == path),
                        enabled,
                        "{path}"
                    );
                }
            });
            assert!(
                visual
                    .debug_bounds("tree-agent-rules-Project/AGENTS.md")
                    .is_some(),
                "badge survives either visibility state"
            );
        }
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
            // #1512: a selected file creates beside itself, just like keyboard creation.
            view.tree.cursor = Some("Area/LongDeepDirectory/Leaf/Note 00.md".into());
        });
        visual.run_until_parked();
        let folder = visual.debug_bounds("sidebar-new-folder").unwrap();
        visual.simulate_click(folder.center(), Modifiers::default());
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
            assert_eq!(
                view.creation.as_ref().unwrap().folder,
                "",
                "no selection creates at root"
            );
        });
    }

    #[gpui::test]
    fn index_alignment_and_hidden_toggle_keep_the_same_header_row(cx: &mut TestAppContext) {
        let (reader, visual, temp) = fixture(cx);
        reader.update(visual, |view, _| {
            view.sidebar_path = Some(temp.path().join("sidebar-toggle.json"));
        });
        for width in [225., 400.] {
            reader.update(visual, |view, cx| {
                view.panel_widths.notes = width;
                cx.notify();
            });
            visual.run_until_parked();
            let header = visual.debug_bounds("tree-index-header").unwrap();
            let viewport = visual.debug_bounds("sidebar-tree-body").unwrap();
            let icon = visual.debug_bounds("tree-vault-index-icon").unwrap();
            let glyph = visual.debug_bounds("tree-vault-index-glyph").unwrap();
            let label = visual.debug_bounds("tree-vault-index-label").unwrap();
            assert_eq!(
                glyph.center().y,
                label.center().y,
                "actual SVG and text line box share a center"
            );
            assert_eq!(
                glyph.center().y,
                header.center().y,
                "compact content is centered in the permanent header row"
            );
            assert!(glyph.top() >= icon.top() && glyph.bottom() <= icon.bottom());
            assert!(label.top() >= header.top() && label.bottom() <= header.bottom());
            assert_eq!(
                icon.left(),
                viewport.left() + px(4.),
                "Contents icon follows the tree inset, never the remainder center"
            );
            assert_eq!(label.left(), icon.right() + px(4.));
            for selector in [
                "sidebar-new-note",
                "sidebar-new-folder",
                "sidebar-collapse-all",
                "sidebar-show-hidden",
            ] {
                let action = visual
                    .debug_bounds(selector)
                    .expect("permanent icon-only action");
                assert!(action.left() >= header.left() && action.right() <= header.right());
                assert!(action.top() >= header.top() && action.bottom() <= header.bottom());
                assert_eq!(action.center().y, header.center().y);
            }
        }
        reader.read_with(visual, |view, _| {
            assert!(!view.sidebar.show_hidden);
            assert!(!view.tree.rows.iter().any(|row| row.path == "_hidden.md"));
        });
        for enabled in [true, false] {
            let toggle = visual.debug_bounds("sidebar-show-hidden").unwrap();
            visual.simulate_click(toggle.center(), Modifiers::default());
            visual.run_until_parked();
            reader.read_with(visual, |view, _| {
                assert_eq!(view.sidebar.show_hidden, enabled);
                assert_eq!(
                    view.tree.rows.iter().any(|row| row.path == "_hidden.md"),
                    enabled
                );
                let saved = reader_sidebar::State::load(view.sidebar_path.as_ref().unwrap());
                assert_eq!(
                    saved.show_hidden, enabled,
                    "the existing per-vault setting persists"
                );
            });
        }
    }

    #[gpui::test]
    fn earlier_roots_indicator_preserves_viewport_and_folder_navigation(cx: &mut TestAppContext) {
        let (reader, visual, _temp) = fixture_with_branch(
            cx,
            "Health/Areas/Current",
            &["AI/Notes", "Business", "Dev", "Finance", "_filtered"],
        );
        let viewport = visual.debug_bounds("sidebar-tree-body").unwrap();
        assert!(visual.debug_bounds("tree-earlier-roots").is_none());
        reader.update(visual, |view, cx| {
            assert!(
                view.vault
                    .entries
                    .iter()
                    .any(|entry| entry.path == "_filtered"),
                "positive control: hidden root exists in inventory"
            );
            assert!(!view.tree.rows.iter().any(|row| row.path == "_filtered"));
            view.tree.set_subtree("AI", true);
            let first = view
                .sidebar_items_named("")
                .iter()
                .position(|item| {
                    matches!(item,
                        SideItem::Tree(row) if row.path == "Health/Areas/Current/Note 10.md"
                    )
                })
                .unwrap();
            view.tree_scroll
                .scroll_to_item_strict(first, ScrollStrategy::Top);
            cx.notify();
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("tree-earlier-roots").is_some());
        assert_eq!(
            visual
                .debug_bounds("tree-scroll-breadcrumbs")
                .unwrap()
                .size
                .height,
            px(HEIGHT)
        );
        assert_eq!(visual.debug_bounds("sidebar-tree-body").unwrap(), viewport);
        let (expanded, offset) = reader.read_with(visual, |view, _| {
            assert_eq!(
                view.tree_earlier_roots
                    .iter()
                    .map(|row| row.path.as_str())
                    .collect::<Vec<_>>(),
                ["AI", "Business", "Dev", "Finance"]
            );
            assert_eq!(view.selected_file(), "Other.md");
            (
                view.tree.expanded_paths().clone(),
                view.tree_scroll.0.borrow().base_handle.offset(),
            )
        });
        reader.update(visual, |_, cx| cx.notify());
        visual.run_until_parked();
        reader.update(visual, |view, cx| {
            assert_eq!(view.tree_scroll.0.borrow().base_handle.offset(), offset);
            // This is the same path-bound navigation callback used by the menu.
            view.scroll_to_tree_ancestor("AI", cx);
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("tree-earlier-roots").is_none());
        reader.read_with(visual, |view, _| {
            assert_eq!(view.tree.expanded_paths(), &expanded);
            assert_eq!(view.selected_file(), "Other.md");
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
