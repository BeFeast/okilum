//! Search palette. At most one query runs; intervening edits are coalesced.
use super::*;
use tessera_core::SearchHit;

use std::collections::HashMap;

fn is_markdown(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

/// Folder notes (`_index`, `index`, `README`) are titled by their folder.
fn is_folder_note(path: &str) -> bool {
    let stem = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    is_markdown(path)
        && ["_index", "index", "readme"]
            .iter()
            .any(|name| stem.eq_ignore_ascii_case(name))
}

/// Primary line (#645): the note title the backlinks panel uses (first H1,
/// then frontmatter title, then file name), never the `.md` file name. Other
/// files keep their extension. Ranking still uses the file name and path too.
/// Called on the query worker, never during rendering.
fn result_title(root: &Path, titles: &HashMap<String, String>, hit: &SearchHit) -> String {
    let path = root.join(&hit.path);
    if !is_markdown(&hit.path) {
        return hit.path.rsplit('/').next().unwrap_or(&hit.path).to_owned();
    }
    if is_folder_note(&hit.path) {
        if let Some(folder) = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|s| s.to_str())
        {
            return folder.to_owned();
        }
    }
    titles
        .get(&hit.path)
        .cloned()
        .or_else(|| display_title(&path))
        .unwrap_or_else(|| hit.title.clone())
}

/// Muted second line (#645): the containing folder, not the file path. A
/// folder note is already titled by its folder, so it shows that folder's
/// parent. Notes at the top level show the vault's name.
fn result_location(vault_name: &str, path: &str) -> String {
    let parent = |path: &str| {
        path.rsplit_once('/')
            .map_or("", |(folder, _)| folder)
            .to_owned()
    };
    let mut folder = parent(path);
    if is_folder_note(path) {
        folder = parent(&folder);
    }
    if folder.is_empty() {
        vault_name.to_owned()
    } else {
        folder.replace('/', " / ")
    }
}

pub(super) struct Palette {
    pub open: bool,
    full_text: bool,
    pub(super) input: Entity<InputState>,
    pub inventory: Option<Arc<Vec<tessera_core::vault::Note>>>,
    _subscription: Subscription,
    pub(super) rows: Vec<SearchHit>,
    selected: usize,
    generation: u64,
    running: bool,
    pending: bool,
    message: String,
    pub recent: Vec<String>,
    scroll: UniformListScrollHandle,
    #[cfg(test)]
    hold_query: Option<async_channel::Receiver<()>>,
}

impl Palette {
    pub fn new(window: &mut Window, cx: &mut Context<Reader>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search names and paths…"));
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                InputEvent::Change => this.refresh_quick_open(cx),
                InputEvent::PressEnter { .. } => this.accept_quick_open(window, cx),
                _ => {}
            });
        Self {
            open: false,
            full_text: false,
            input,
            inventory: None,
            _subscription: subscription,
            rows: Vec::new(),
            selected: 0,
            generation: 0,
            running: false,
            pending: false,
            message: String::new(),
            recent: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            #[cfg(test)]
            hold_query: None,
        }
    }

    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.rows.clear();
        self.selected = 0;
        self.pending = false;
    }

    pub fn remember(&mut self, path: &str) {
        self.recent.retain(|p| p != path);
        self.recent.push(path.to_string());
        if self.recent.len() > 100 {
            self.recent.remove(0);
        }
    }
}

impl Reader {
    pub(super) fn open_quick_open(
        &mut self,
        full_text: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.find_open {
            self.close_find(window, cx);
        }
        self.quick_open.open = true;
        self.quick_open.full_text = full_text;
        self.quick_open.input.update(cx, |input, cx| {
            input.set_placeholder(
                if full_text {
                    "Search note contents…"
                } else {
                    "Search names and paths…"
                },
                window,
                cx,
            );
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.refresh_quick_open(cx);
    }

    pub(super) fn close_quick_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_hover(cx);
        self.quick_open.open = false;
        self.quick_open.invalidate();
        if self.selected_file().is_empty() || self.file_preview.is_some() {
            self.focus_handle.focus(window, cx);
        } else {
            self.content
                .read(cx)
                .focus_handle()
                .clone()
                .focus(window, cx);
        }
        cx.notify();
    }

    pub(super) fn refresh_quick_open(&mut self, cx: &mut Context<Self>) {
        self.clear_hover(cx);
        self.quick_open.invalidate();
        if !self.quick_open.open {
            return;
        }
        self.quick_open.pending = true;
        self.quick_open.message = "Searching…".into();
        self.start_quick_open_query(cx);
        cx.notify();
    }

    fn start_quick_open_query(&mut self, cx: &mut Context<Self>) {
        if self.quick_open.running || !self.quick_open.pending {
            return;
        }
        self.quick_open.running = true;
        self.quick_open.pending = false;
        let generation = self.quick_open.generation;
        let root = self.vault_root.clone();
        let inventory = self.watcher_generation;
        let vault = self.vault.clone();
        let names = self.quick_open.inventory.clone();
        let searcher = self.searcher.clone();
        let prepared = self
            .loading
            .as_ref()
            .is_some_and(|load| !load.active && load.phase == "Ready");
        let query = self.quick_open.input.read(cx).value().trim().to_string();
        let recent = self.quick_open.recent.clone();
        let full_text = self.quick_open.full_text;
        #[cfg(test)]
        let hold = self.quick_open.hold_query.take();
        let title_root = root.clone();
        let titles = self.backlink_titles.clone();
        let task = cx.background_executor().spawn(async move {
            #[cfg(test)]
            if let Some(hold) = hold {
                let _ = hold.recv().await;
            }
            let result = if full_text {
                if query.is_empty() {
                    return Ok((vec![], "Type to search note contents".to_string()));
                }
                match searcher {
                    Some(searcher) => searcher
                        .search(&query, 100)
                        .map(|rows| (rows, String::new()))
                        .map_err(|error| error.to_string()),
                    None => Ok((
                        vec![],
                        if prepared {
                            "Content search is unavailable; see unreadable paths and Retry".into()
                        } else {
                            "Search index is not ready yet".into()
                        },
                    )),
                }
            } else {
                let rows = tessera_core::quick_open::search_titled(
                    names.as_deref().unwrap_or(&vault.notes),
                    &titles,
                    &query,
                    &recent,
                    100,
                )
                .into_iter()
                .map(|note| SearchHit {
                    path: note.path,
                    title: note.title,
                    score: 0.,
                    snippet_html: String::new(),
                })
                .collect();
                Ok((rows, String::new()))
            };
            result.map(|(mut rows, message)| {
                for hit in &mut rows {
                    hit.title = result_title(&title_root, &titles, hit);
                }
                (rows, message)
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.quick_open.running = false;
                if this.quick_open.open
                    && this.quick_open.generation == generation
                    && this.vault_root == root
                    && this.watcher_generation == inventory
                {
                    match result {
                        Ok((rows, message)) => {
                            this.quick_open.message = if rows.is_empty() && message.is_empty() {
                                "No matches".into()
                            } else {
                                message
                            };
                            this.quick_open.rows = rows;
                        }
                        Err(error) => this.quick_open.message = error,
                    }
                    this.quick_open
                        .scroll
                        .scroll_to_item(0, ScrollStrategy::Top);
                    cx.notify();
                }
                this.start_quick_open_query(cx);
            });
        })
        .detach();
    }

    pub(super) fn move_quick_open(&mut self, delta: isize, cx: &mut Context<Self>) {
        let palette = &mut self.quick_open;
        if !palette.open || palette.rows.is_empty() {
            return;
        }
        palette.selected =
            (palette.selected as isize + delta).rem_euclid(palette.rows.len() as isize) as usize;
        palette
            .scroll
            .scroll_to_item(palette.selected, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn accept_quick_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.quick_open.open {
            return;
        }
        let Some(hit) = self.quick_open.rows.get(self.quick_open.selected).cloned() else {
            return;
        };
        let jump = if self.quick_open.full_text {
            tessera_core::quick_open::matched_text(&hit.snippet_html)
        } else {
            None
        };
        self.close_quick_open(window, cx);
        self.open_note(&hit.path, jump.as_deref(), window, cx);
    }

    pub(super) fn render_quick_open(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = brand::palette(cx);
        let rows = self.quick_open.rows.clone();
        let selected = self.quick_open.selected;
        let full_text = self.quick_open.full_text;
        let selected_bg = palette.selected;
        let muted = palette.text_muted;
        let entity = cx.entity().downgrade();
        let generation = self.quick_open.generation;
        let root = self.vault_root.clone();
        let inventory = self.watcher_generation;
        let vault_name = self
            .vault_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let list = uniform_list("quick-open-results", rows.len(), move |range, _, _| {
            range
                .map(|ix| {
                    let hit = &rows[ix];
                    let entity = entity.clone();
                    let root = root.clone();
                    let hover_entity = entity.clone();
                    let hover_path = hit.path.clone();
                    let hover_root = root.clone();
                    v_flex()
                        .id(("quick-open-result", ix))
                        .on_hover(move |active, window, cx| {
                            let _ = hover_entity.update(cx, |this, cx| {
                                let key = format!("quick:{generation}:{hover_path}");
                                if *active
                                    && this.quick_open.open
                                    && this.quick_open.generation == generation
                                    && this.vault_root == hover_root
                                    && this.watcher_generation == inventory
                                {
                                    this.hover_note(
                                        key,
                                        reader_hover::Target {
                                            path: hover_path.clone(),
                                            heading: None,
                                        },
                                        window.mouse_position(),
                                        window,
                                        cx,
                                    );
                                } else {
                                    this.leave_hover(&key, cx);
                                }
                            });
                        })
                        .debug_selector(move || format!("quick-open-result-{ix}"))
                        .cursor_pointer()
                        .hover(|row| row.bg(selected_bg))
                        .on_click(move |_, window, cx| {
                            let _ = entity.update(cx, |this, cx| {
                                if this.quick_open.open
                                    && this.quick_open.generation == generation
                                    && this.vault_root == root
                                    && this.watcher_generation == inventory
                                {
                                    this.quick_open.selected = ix;
                                    this.accept_quick_open(window, cx);
                                }
                            });
                        })
                        .h(px(if full_text { 88. } else { 52. }))
                        .px_3()
                        .py_1()
                        .when(ix == selected, |row| row.bg(selected_bg))
                        .child(
                            div()
                                .text_sm()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(hit.title.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(result_location(&vault_name, &hit.path)),
                        )
                        .when(full_text, |row| {
                            row.child(div().text_xs().overflow_hidden().child(TextView::html(
                                SharedString::from(format!("quick-open-snippet-{ix}")),
                                hit.snippet_html.clone(),
                            )))
                        })
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.quick_open.scroll)
        .h(px(352.));
        div()
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .justify_center()
            .pt(px(56.))
            .child(
                v_flex()
                    .id("quick-open")
                    .key_context("QuickOpen")
                    .occlude()
                    .w(px(640.))
                    .max_w_full()
                    .h(px(456.))
                    .rounded(px(12.))
                    .bg(palette.surface)
                    .text_color(palette.text)
                    .border_1()
                    .border_color(palette.border)
                    .shadow_lg()
                    .child(div().px_3().py_2().text_sm().child(if full_text {
                        if cfg!(target_os = "macos") {
                            "Search contents · ⇧⌘F"
                        } else {
                            "Search contents · Ctrl+Shift+F"
                        }
                    } else {
                        if cfg!(target_os = "macos") {
                            "Quick open · ⌘K"
                        } else {
                            "Quick open · Ctrl+K"
                        }
                    }))
                    .child(
                        div()
                            .px_3()
                            .child(Input::new(&self.quick_open.input).cleanable(true)),
                    )
                    .child(list)
                    .child(div().px_3().text_xs().text_color(muted).child(
                        if self.quick_open.message.is_empty() {
                            if self.quick_open.input.read(cx).value().is_empty() {
                                "↑ ↓ Select · Enter Open · Esc Close".to_string()
                            } else {
                                "↑ ↓ Select · Enter Open · Esc Clear".to_string()
                            }
                        } else {
                            self.quick_open.message.clone()
                        },
                    )),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn result_titles_keep_file_identity_and_use_folder_for_indexes() {
        let root = std::env::temp_dir().join(format!("tessera-titles-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("Memory")).unwrap();
        for (path, body, expected) in [
            (
                "heading.md",
                "---\ntitle: Metadata\n---\n# Heading\n",
                "Heading",
            ),
            ("metadata.md", "---\ntitle: Metadata\n---\nBody", "Metadata"),
            ("plain.md", "Body", "plain"),
            ("Memory/_index.md", "# Ignored", "Memory"),
            ("Memory/index.md", "# Ignored", "Memory"),
            ("Memory/README.md", "# Ignored", "Memory"),
            ("Memory/Scan.pdf", "%PDF", "Scan.pdf"),
            ("Memory/Board.canvas", "{}", "Board.canvas"),
            ("Memory/readme.txt", "# Not a note", "readme.txt"),
        ] {
            std::fs::write(root.join(path), body).unwrap();
            let hit = SearchHit {
                path: path.into(),
                title: Vault::title_of(path),
                score: 0.,
                snippet_html: String::new(),
            };
            let title = result_title(&root, &HashMap::new(), &hit);
            assert_eq!(title, expected);
            assert!(!title.ends_with(".md"), "{path}");
            assert_eq!(hit.path, path);
        }
        // The shared title map wins over a disk read; folder notes keep their folder.
        let titles = HashMap::from([
            ("plain.md".to_string(), "Cached title".to_string()),
            ("Memory/_index.md".to_string(), "Ignored".to_string()),
        ]);
        for (path, expected) in [("plain.md", "Cached title"), ("Memory/_index.md", "Memory")] {
            let hit = SearchHit {
                path: path.into(),
                title: Vault::title_of(path),
                score: 0.,
                snippet_html: String::new(),
            };
            assert_eq!(result_title(&root, &titles, &hit), expected);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_location_shows_folder_without_file_name() {
        for (path, expected) in [
            ("plain.md", "Vault"),
            ("Projects/Roadmap.md", "Projects"),
            ("Projects/2026/Q4/Plan.md", "Projects / 2026 / Q4"),
            ("Memory/_index.md", "Vault"),
            ("Projects/Memory/README.md", "Projects"),
            ("Projects/Scan.pdf", "Projects"),
        ] {
            let location = result_location("Vault", path);
            assert_eq!(location, expected, "{path}");
            assert!(!location.contains(".md"));
        }
    }

    #[gpui::test]
    fn incremental_palette_5000_notes_and_superseded_results(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| Reader::new(Opts::default(), window, cx));
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            v.loading = None;
            v.vault = Arc::new(Vault::from_note_paths(
                (0..5000).map(|i| format!("area/Note {i}.md")),
            ));
            v.quick_open.remember("area/Note 4999.md");
            v.open_quick_open(false, window, cx);
        });
        visual.run_until_parked();
        reader.update(visual, |v, _| {
            assert_eq!(v.quick_open.rows[0].path, "area/Note 4999.md")
        });
        visual.simulate_keystrokes("down");
        reader.update(visual, |v, _| assert_eq!(v.quick_open.selected, 1));
        visual.simulate_keystrokes("up");
        reader.update(visual, |v, _| assert_eq!(v.quick_open.selected, 0));
        visual.simulate_keystrokes("escape");
        reader.update(visual, |v, _| assert!(!v.quick_open.open));
        visual.simulate_keystrokes("ctrl-k");
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            assert!(v.quick_open.open);
            assert!(v
                .quick_open
                .input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window));
        });
        for character in "Note 4999".chars() {
            visual.simulate_input(&character.to_string());
            visual.run_until_parked();
            reader.update(visual, |v, _| {
                assert!(
                    !v.quick_open.rows.is_empty(),
                    "each character publishes results"
                );
            });
        }
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            assert!(v.quick_open.open);
            assert!(v.quick_open.input.read(cx).value().is_empty());
            assert!(v
                .quick_open
                .input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window));
            assert!(
                !v.quick_open.rows.is_empty(),
                "empty query restores choices"
            );
        });
        visual.simulate_keystrokes("escape");
        reader.update_in(visual, |v, window, cx| {
            assert!(!v.quick_open.open);
            assert!(
                v.focus_handle.is_focused(window),
                "empty vault keeps a mounted focus target"
            );
            v.open_quick_open(false, window, cx);
        });
        visual.simulate_input("Note 4999");
        visual.run_until_parked();
        let (release, hold) = async_channel::bounded(1);
        reader.update_in(visual, |v, window, cx| {
            v.quick_open.hold_query = Some(hold);
            assert_eq!(v.quick_open.rows[0].path, "area/Note 4999.md");
            // A real task remains in flight while subsequent edits replace pending work.
            for text in ["Note 1", "Note 25", "Note 4998"] {
                v.quick_open
                    .input
                    .update(cx, |input, cx| input.set_value(text, window, cx));
                v.refresh_quick_open(cx);
            }
        });
        visual.run_until_parked();
        reader.update(visual, |v, cx| {
            assert_eq!(v.quick_open.input.read(cx).value().to_string(), "Note 4998");
            assert!(v.quick_open.running);
            assert!(v.quick_open.rows.is_empty());
        });
        release.try_send(()).unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |v, _, cx| {
            assert_eq!(v.quick_open.rows[0].path, "area/Note 4998.md");
            v.refresh_quick_open(cx);
            v.watcher_generation += 1;
        });
        visual.run_until_parked();
        reader.update(visual, |v, cx| {
            assert!(
                v.quick_open.rows.is_empty(),
                "old inventory result rejected"
            );
            v.refresh_quick_open(cx);
            v.vault_root = PathBuf::from("replacement-root");
        });
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            assert!(v.quick_open.rows.is_empty(), "old root result rejected");
            v.refresh_quick_open(cx);
            v.close_quick_open(window, cx);
        });
        visual.run_until_parked();
        reader.update(visual, |v, _| {
            assert!(!v.quick_open.open);
            assert!(
                v.quick_open.rows.is_empty(),
                "closed palette rejects in-flight result"
            );
        });
    }
    #[gpui::test]
    fn hidden_notes_and_new_directory_arrivals_reach_both_palettes(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp =
            std::env::temp_dir().join(format!("tessera-hidden-search-{}", uuid::Uuid::new_v4()));
        let root = temp.join("vault");
        let path = "_Assets/Daily Notes/2026/10/04/2026-10-04-полный-ai-стэк.md";
        std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
        std::fs::write(root.join("Target.md"), "# Target").unwrap();
        std::fs::write(
            root.join(path),
            "# Полный AI

canaryhidden [[Target]]",
        )
        .unwrap();
        for directory in [".git", ".obsidian", ".trash", "node_modules"] {
            std::fs::create_dir_all(root.join(directory)).unwrap();
            std::fs::write(
                root.join(directory).join("excluded.md"),
                "canaryhidden [[Target]]",
            )
            .unwrap();
        }
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("Target.md".into()),
                        index_dir: Some(temp.join("index")),
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
        visual.run_until_parked();
        for (new_note, expected_path, name, content) in [
            (false, path, "2026-10-04-полный-ai", "canaryhidden"),
            (
                true,
                "_Incoming/Новая заметка.md",
                "Новая заметка",
                "canaryarrival",
            ),
        ] {
            if new_note {
                let staged = temp.join("incoming");
                std::fs::create_dir_all(&staged).unwrap();
                std::fs::write(staged.join("Новая заметка.md"), "canaryarrival [[Target]]")
                    .unwrap();
                std::fs::rename(staged, root.join("_Incoming")).unwrap();
                reader.update_in(visual, |v, window, cx| {
                    let changes = v
                        .watcher
                        .as_mut()
                        .expect("live watcher")
                        .wait(Duration::from_secs(3))
                        .expect("directory arrival must trigger reconciliation");
                    assert!(!changes.rescan && changes.directories.contains("_Incoming"));
                    v.apply_vault_changes(changes, window, cx);
                });
                visual.run_until_parked();
            }
            reader.update(visual, |v, _| {
                assert!(v.searcher.is_some(), "published index positive control");
                assert!(v.vault.notes.iter().any(|note| note.path == expected_path));
                assert!(v
                    .vault
                    .backlinks("Target.md")
                    .iter()
                    .any(|link| link.path == expected_path));
                assert!(matches!(
                    v.vault.resolve(expected_path.trim_end_matches(".md")),
                    tessera_core::vault::Resolution::Resolved { .. }
                ));
                assert!(!v
                    .vault
                    .notes
                    .iter()
                    .any(|note| note.path.ends_with("excluded.md")));
            });
            reader.update(visual, |v, cx| {
                v.sync_tree();
                assert!(!v.tree.show_hidden());
                assert!(v.tree.rows.iter().any(|row| row.path == "Target.md"));
                assert!(!v.tree.rows.iter().any(|row| row.path.starts_with('_')));
                let inventory = v.vault.clone();
                // Exercise the same action as Show hidden files, without reveal:
                // revealing the current note would bypass the visibility filter.
                v.toggle_hidden_files(cx);
                assert!(v.tree.show_hidden());
                let mut directory = String::new();
                let components: Vec<_> = expected_path.split('/').collect();
                for component in &components[..components.len() - 1] {
                    if !directory.is_empty() {
                        directory.push('/');
                    }
                    directory.push_str(component);
                    let row = v
                        .tree
                        .rows
                        .iter()
                        .find(|row| row.path == directory)
                        .expect("every hidden ancestor must be browsable");
                    if !row.expanded {
                        v.tree.toggle(&directory);
                    }
                }
                assert!(v.tree.rows.iter().any(|row| row.path == expected_path));
                assert!(!v.tree.rows.iter().any(|row| [
                    ".git",
                    ".obsidian",
                    ".trash",
                    "node_modules"
                ]
                .contains(&row.path.as_str())));
                v.toggle_hidden_files(cx);
                assert!(!v.tree.rows.iter().any(|row| row.path.starts_with('_')));
                assert!(
                    Arc::ptr_eq(&inventory, &v.vault),
                    "visibility must not replace inventory"
                );
            });
            for (full_text, query) in [(false, name), (true, content)] {
                reader.update_in(visual, |v, window, cx| {
                    v.open_quick_open(full_text, window, cx);
                    v.quick_open
                        .input
                        .update(cx, |input, cx| input.set_value(query, window, cx));
                    v.refresh_quick_open(cx);
                });
                visual.run_until_parked();
                reader.update(visual, |v, _| {
                    assert_eq!(v.quick_open.rows.len(), 1, "query {query}");
                    assert_eq!(v.quick_open.rows[0].path, expected_path);
                });
            }
        }
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[gpui::test]
    fn mouse_and_keyboard_open_results_and_mark_content_match(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root =
            std::env::temp_dir().join(format!("tessera-quick-open-{}", uuid::Uuid::new_v4()));
        let index = root.with_extension("index");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("start.md"), "# Start\n\nOrdinary text.").unwrap();
        std::fs::create_dir_all(root.join("Memory")).unwrap();
        std::fs::write(
            root.join("Memory/2026-10-06-kickoff.md"),
            "---\ntitle: Ignored\n---\n# Quarterly planning\n",
        )
        .unwrap();
        std::fs::write(
            root.join("Memory/_index.md"),
            format!(
                "# Target\n\n{}\n\nUnique canaryword landing.",
                "Filler paragraph.\n\n".repeat(50)
            ),
        )
        .unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("start.md".into()),
                        index_dir: Some(index.clone()),
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
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            assert!(v.searcher.is_some(), "index must publish before searching");
            window.focus(&v.focus_handle, cx);
        });
        // The resolved title matches, fuzzily too, although no file name or
        // path contains it (#645).
        for query in ["Quarterly planning", "qplan"] {
            reader.update_in(visual, |v, window, cx| {
                v.open_quick_open(false, window, cx);
                v.quick_open
                    .input
                    .update(cx, |input, cx| input.set_value(query, window, cx));
                v.refresh_quick_open(cx);
            });
            visual.run_until_parked();
            reader.update_in(visual, |v, window, cx| {
                assert_eq!(v.quick_open.rows.len(), 1, "query {query}");
                assert_eq!(v.quick_open.rows[0].path, "Memory/2026-10-06-kickoff.md");
                assert_eq!(v.quick_open.rows[0].title, "Quarterly planning");
                v.close_quick_open(window, cx);
            });
        }
        // Name/path matching remains intact even though the displayed title differs.
        visual.simulate_keystrokes("ctrl-k");
        visual.run_until_parked();
        visual.simulate_input("Memory/_index");
        visual.run_until_parked();
        reader.update(visual, |v, _| {
            assert_eq!(v.quick_open.rows.len(), 1);
            assert_eq!(v.quick_open.rows[0].title, "Memory");
            assert_eq!(v.current_rel, "start.md");
        });
        let bounds = visual
            .debug_bounds("quick-open-result-0")
            .expect("visible name result");
        visual.simulate_click(bounds.center(), Modifiers::default());
        visual.run_until_parked();
        reader.update(visual, |v, _| {
            assert!(!v.quick_open.open);
            assert_eq!(v.current_rel, "Memory/_index.md");
        });
        for mouse in [false, true] {
            reader.update_in(visual, |v, window, cx| {
                v.open_note("start.md", None, window, cx);
            });
            visual.run_until_parked();
            if mouse {
                let search = visual
                    .debug_bounds("reader-search")
                    .expect("main toolbar search");
                visual.simulate_click(search.center(), Modifiers::default());
            } else {
                visual.simulate_keystrokes("ctrl-shift-f");
            }
            visual.run_until_parked();
            visual.simulate_input("canaryword");
            visual.run_until_parked();
            reader.update(visual, |v, cx| {
                assert_eq!(
                    v.quick_open.input.read(cx).value().to_string(),
                    "canaryword"
                );
                assert!(v.searcher.is_some(), "index publication positive control");
                assert!(v.quick_open.full_text);
                assert_eq!(v.quick_open.rows.len(), 1);
                assert_eq!(v.quick_open.rows[0].title, "Memory");
                assert_eq!(v.quick_open.rows[0].path, "Memory/_index.md");
                assert!(v.quick_open.rows[0]
                    .snippet_html
                    .contains("<b>canaryword</b>"));
            });
            if mouse {
                let bounds = visual
                    .debug_bounds("quick-open-result-0")
                    .expect("visible search result");
                visual.simulate_click(bounds.center(), Modifiers::default());
            } else {
                visual.simulate_keystrokes("enter");
            }
            visual.run_until_parked();
            visual.executor().advance_clock(Duration::from_millis(300));
            visual.run_until_parked();
            reader.update(visual, |v, cx| {
                assert!(!v.quick_open.open);
                assert_eq!(v.current_rel, "Memory/_index.md");
                assert_eq!(v.content.read(cx).search_status().1, 1);
                assert!(v.content.read(cx).list_state().logical_scroll_top().item_ix > 0);
            });
        }
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(index).unwrap();
    }
}
