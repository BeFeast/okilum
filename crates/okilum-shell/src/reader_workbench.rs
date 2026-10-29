//! Now/Tree navigation and publication over accepted source snapshots (#1927).
use super::*;
use chrono::{Datelike as _, TimeZone as _};
use gpui_component::button::ButtonGroup;
use okilum_core::workbench::{self, Index, Unit};
use reader_sidebar::LeftMode;

fn clock() -> (u64, u64, u64) {
    let now = chrono::Local::now();
    let date = now.date_naive();
    let boundary = |date: chrono::NaiveDate| {
        chrono::Local
            .with_ymd_and_hms(date.year(), date.month(), date.day(), 0, 0, 0)
            .earliest()
            .map_or(now.timestamp(), |t| t.timestamp())
            .max(0) as u64
    };
    (
        now.timestamp().max(0) as u64,
        boundary(date),
        boundary(date.pred_opt().unwrap_or(date)),
    )
}

/// Manifest IO remains on the loading worker, never in render or mode switching.
fn units(root: &Path) -> Vec<Unit> {
    let discovery = okilum_t3::projects::discover(root);
    discovery
        .projects
        .into_iter()
        .filter_map(|project| {
            let path = project
                .folder
                .strip_prefix(root)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            let terminal = okilum_t3::area_files::read(&project.folder, "project")
                .ok()
                .flatten()
                .is_some_and(|doc| {
                    ["status", "lifecycle"].iter().any(|key| {
                        doc.value
                            .get(*key)
                            .and_then(serde_yaml::Value::as_str)
                            .is_some_and(workbench::terminal)
                    })
                });
            Some(Unit {
                path,
                title: project.name,
                terminal,
            })
        })
        .collect()
}

pub(super) fn from_snapshot(
    snapshot: &okilum_core::vault::warm::Snapshot,
    root: &Path,
) -> Arc<Index> {
    let (now, midnight, yesterday) = clock();
    Arc::new(Index::from_snapshot(
        snapshot,
        units(root),
        now,
        midnight,
        yesterday,
    ))
}

pub(super) fn update_index(
    index: &mut Index,
    snapshot: &okilum_core::vault::warm::Snapshot,
    removed: &std::collections::BTreeSet<String>,
    changed: &std::collections::BTreeSet<String>,
) {
    for path in removed {
        index.remove(path);
    }
    for path in changed {
        index.replace_snapshot(snapshot, path);
    }
    let (now, midnight, yesterday) = clock();
    index.refresh(now, midnight, yesterday);
}

fn shown_time(seconds: u64) -> String {
    chrono::Local
        .timestamp_opt(seconds.min(i64::MAX as u64) as i64, 0)
        .single()
        .map(|time| time.format("%H:%M").to_string())
        .unwrap_or_else(|| "—".into())
}

fn location(path: &str) -> String {
    path.trim_end_matches(".md").replace('/', " › ")
}

fn short_domain(domain: &str) -> &str {
    match domain {
        "Business" => "Bus",
        "Finance" => "Fin",
        "Health" => "Hea",
        "HomeLab" => "HL",
        "Music" => "Mus",
        "Video" => "Vid",
        name => name,
    }
}

impl Reader {
    pub(super) fn publish_workbench(&mut self, index: Arc<Index>, cx: &mut Context<Self>) {
        self.workbench = index.clone();
        self.workbench_pending = self.workbench_pending.wrapping_add(1);
        let pending = self.workbench_pending;
        let root = self.vault_root.clone();
        let generation = self.watcher_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.vault_root == root
                    && this.watcher_generation == generation
                    && this.workbench_pending == pending
                {
                    this.workbench_ready = Some(index);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn set_left_mode(
        &mut self,
        mode: LeftMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.single_file
            || self.project_navigation.active.is_some()
            || self.sidebar.left_mode == mode
        {
            return;
        }
        self.sidebar.left_mode = mode;
        // Keep the tree object, cursor and scroll handle intact. No reveal or rebuild.
        if mode == LeftMode::Now && self.tree_focus.contains_focused(window, cx) {
            self.focus_handle.focus(window, cx);
        }
        self.save_sidebar(cx);
        cx.notify();
    }

    pub(super) fn toggle_left_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mode = match self.sidebar.left_mode {
            LeftMode::Now => LeftMode::Tree,
            LeftMode::Tree => LeftMode::Now,
        };
        self.set_left_mode(mode, window, cx);
    }

    pub(super) fn render_left_modes(&self, cx: &mut Context<Self>) -> AnyElement {
        let reader = cx.entity().downgrade();
        h_flex()
            .w_full()
            .px_3()
            .py_1()
            .child(
                ButtonGroup::new("left-mode").w_full().children(
                    [
                        ("left-mode-now", "Now", LeftMode::Now),
                        ("left-mode-tree", "Tree", LeftMode::Tree),
                    ]
                    .into_iter()
                    .map(|(id, label, mode)| {
                        let reader = reader.clone();
                        Button::new(id)
                            .ghost()
                            .small()
                            .flex_1()
                            .label(label)
                            .selected(self.sidebar.left_mode == mode)
                            .debug_selector(move || id.into())
                            .tooltip(reader_shortcuts::hint("Now / Tree", &ToggleLeftMode, cx))
                            .on_click(move |_, window, cx| {
                                let _ = reader
                                    .update(cx, |this, cx| this.set_left_mode(mode, window, cx));
                            })
                    }),
                ),
            )
            .into_any_element()
    }

    fn toggle_now_section(&mut self, key: &str, expanded: bool, cx: &mut Context<Self>) {
        let store = if expanded {
            &mut self.sidebar.now_expanded
        } else {
            &mut self.sidebar.now_collapsed
        };
        if !store.remove(key) {
            store.insert(key.to_owned());
        }
        self.save_sidebar(cx);
        cx.notify();
    }

    fn now_heading(
        &self,
        key: String,
        title: String,
        expanded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let reader = cx.entity().downgrade();
        let open = if expanded {
            self.sidebar.now_expanded.contains(&key)
        } else {
            !self.sidebar.now_collapsed.contains(&key)
        };
        h_flex()
            .id(SharedString::from(format!("now-heading-{key}")))
            .w_full()
            .h(px(28.))
            .gap_1()
            .px_1()
            .cursor_pointer()
            .text_size(px(12.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(brand::palette(cx).text_muted)
            .child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .xsmall(),
            )
            .child(title)
            .on_click(move |_, _, cx| {
                let _ = reader.update(cx, |this, cx| this.toggle_now_section(&key, expanded, cx));
            })
            .into_any_element()
    }

    fn now_note_row(
        &self,
        path: &str,
        title: String,
        secondary: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let reader = cx.entity().downgrade();
        let root = self.vault_root.clone();
        let generation = self.watcher_generation;
        let path = path.to_owned();
        v_flex()
            .id(SharedString::from(format!("now-note-{path}")))
            .w_full()
            .min_w_0()
            .px_2()
            .py_1()
            .rounded(px(6.))
            .cursor_pointer()
            .hover(|style| style.bg(brand::reader_palette(cx).hover))
            .child(
                div()
                    .w_full()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(title),
            )
            .child(
                div()
                    .w_full()
                    .text_size(px(12.))
                    .text_color(brand::palette(cx).text_muted)
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(secondary),
            )
            .on_click(move |_, window, cx| {
                let _ = reader.update(cx, |this, cx| {
                    if this.vault_root != root || this.watcher_generation != generation {
                        return;
                    }
                    if this.vault.entries.iter().any(|entry| {
                        entry.path == path && entry.kind == okilum_core::vault::EntryKind::Directory
                    }) {
                        this.reveal_in_tree(&path, window, cx);
                    } else {
                        this.open_note(&path, None, window, cx);
                    }
                });
            })
            .into_any_element()
    }

    pub(super) fn render_workbench(&self, _: &Window, cx: &mut Context<Self>) -> AnyElement {
        let tokens = brand::palette(cx);
        let mut content = v_flex()
            .w_full()
            .min_w_0()
            .gap_1()
            .p_2()
            .text_size(px(brand::READER_CHROME_FONT_SIZE));
        let Some(index) = &self.workbench_ready else {
            return content
                .child(
                    div()
                        .px_2()
                        .text_color(tokens.text_muted)
                        .child("Preparing Now…"),
                )
                .into_any_element();
        };
        let view = &index.view;
        let network = self.loading.as_ref().is_some_and(|load| load.network);
        let time = shown_time(view.built_at);
        let freshness = if network {
            format!("NFS · snapshot {time}")
        } else {
            format!("● live · {time}")
        };
        let freshness_row = h_flex()
            .w_full()
            .justify_between()
            .px_1()
            .gap_1()
            .text_size(px(12.))
            .text_color(tokens.text_muted)
            .child(freshness)
            .when(network, |row| {
                row.child(
                    Button::new("now-rescan")
                        .ghost()
                        .small()
                        .label("Rescan")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.refresh_inventory_with_read(
                                okilum_core::Changes::default(),
                                true,
                                window,
                                cx,
                            )
                        })),
                )
            });
        content = content.child(freshness_row);
        content = content.child(self.now_heading("active".into(), "In progress".into(), false, cx));
        if !self.sidebar.now_collapsed.contains("active") {
            for activity in &view.active {
                content = content.child(self.now_note_row(
                    &activity.open,
                    activity.unit.title.clone(),
                    format!(
                        "{} · {} changes",
                        location(&activity.unit.path),
                        activity.count
                    ),
                    cx,
                ));
            }
            if view.active.is_empty() {
                content = content.child(div().px_2().text_color(tokens.text_muted).child("—"));
            }
            if view.older > 0 {
                content = content.child(
                    div()
                        .px_2()
                        .text_size(px(12.))
                        .text_color(tokens.text_muted)
                        .child(format!("{} more in 14 days", view.older)),
                );
            }
        }
        content = content.child(self.now_heading("changes".into(), "Changes".into(), false, cx));
        if !self.sidebar.now_collapsed.contains("changes") {
            for period in &view.days {
                let key = format!("day-{}", period.label);
                content = content.child(self.now_heading(
                    key.clone(),
                    format!("{} · {}", period.label, period.count),
                    false,
                    cx,
                ));
                if self.sidebar.now_collapsed.contains(&key) {
                    continue;
                }
                let more = format!("more-{}", period.label);
                let shown = if self.sidebar.now_expanded.contains(&more) {
                    period.places.len()
                } else {
                    8
                };
                for place in period.places.iter().take(shown) {
                    let key = format!("place-{}-{}", period.label, place.path);
                    let title = format!(
                        "{} · {}",
                        place.path.rsplit('/').next().unwrap_or(&place.path),
                        place.count
                    );
                    content = content
                        .child(self.now_heading(key.clone(), title, true, cx))
                        .child(
                            div()
                                .pl_5()
                                .text_size(px(12.))
                                .text_color(tokens.text_muted)
                                .child(location(&place.path)),
                        );
                    if self.sidebar.now_expanded.contains(&key) {
                        for row in &place.rows {
                            let title = if row.count > 1 {
                                format!("{} · {} files", row.title, row.count)
                            } else {
                                row.title.clone()
                            };
                            content = content.child(self.now_note_row(
                                &row.path,
                                title,
                                format!("{} · {}", shown_time(row.modified), location(&place.path)),
                                cx,
                            ));
                        }
                    }
                }
                if period.places.len() > shown {
                    let reader = cx.entity().downgrade();
                    content = content.child(
                        Button::new(SharedString::from(more.clone()))
                            .ghost()
                            .small()
                            .label(format!("{} more", period.places.len() - shown))
                            .on_click(move |_, _, cx| {
                                let _ = reader.update(cx, |this, cx| {
                                    this.toggle_now_section(&more, true, cx)
                                });
                            }),
                    );
                }
            }
        }
        content = content.child(self.now_heading("pins".into(), "Pinned".into(), false, cx));
        if !self.sidebar.now_collapsed.contains("pins") {
            for path in &self.sidebar.pinned {
                content = content.child(self.now_note_row(
                    path,
                    self.vault.note_title(path),
                    location(path),
                    cx,
                ));
            }
        }
        content = content.child(
            h_flex()
                .w_full()
                .flex_wrap()
                .gap_2()
                .px_1()
                .text_size(px(12.))
                .text_color(tokens.text_muted)
                .children(workbench::DOMAINS.into_iter().map(|domain| {
                    div().child(format!(
                        "{} {}",
                        short_domain(domain),
                        view.domains.get(domain).copied().unwrap_or(0)
                    ))
                })),
        );
        div()
            .id("now-sidebar")
            .debug_selector(|| "now-sidebar".into())
            .w_full()
            .h_full()
            .overflow_y_scroll()
            .track_scroll(&self.workbench_scroll)
            .child(content)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn fixture(
        cx: &mut TestAppContext,
    ) -> (Entity<Reader>, &mut VisualTestContext, tempfile::TempDir) {
        let temp = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        defer_loading: true,
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader.update(cx, |reader, cx| {
                reader.loading = None;
                reader.vault_root = temp.path().join("vault");
                reader.sidebar = reader_sidebar::State::default();
                reader.sidebar_path = Some(temp.path().join("state/sidebar.json"));
                reader.vault = Arc::new(Vault::from_note_paths(
                    (0..150).map(|i| format!("Dev/Projects/App/Note {i:03}.md")),
                ));
                reader.panels.open(reader_layout::Panel::Notes);
                reader.sync_tree();
                reader.tree.reveal("Dev/Projects/App/Note 090.md");
                reader.tree.cursor = Some("Dev/Projects/App/Note 090.md".into());
                reader
                    .tree_scroll
                    .scroll_to_item_strict(80, ScrollStrategy::Top);
                cx.notify();
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        visual.simulate_resize(size(px(1400.), px(800.)));
        visual.run_until_parked();
        (entity.unwrap(), visual, temp)
    }

    #[gpui::test]
    fn mode_switch_keeps_tree_cursor_expansion_scroll_and_persists(cx: &mut TestAppContext) {
        let (reader, visual, temp) = fixture(cx);
        assert!(visual.debug_bounds("left-mode-now").is_some());
        assert!(visual.debug_bounds("sidebar-new-folder").is_none());
        let toggle = visual.debug_bounds("left-mode-tree").unwrap();
        visual.simulate_click(toggle.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("sidebar-tree-body").is_some());
        let before = reader.read_with(visual, |r, _| {
            (
                r.tree
                    .rows
                    .iter()
                    .map(|row| (row.path.clone(), row.expanded))
                    .collect::<Vec<_>>(),
                r.tree.cursor.clone(),
                r.tree_scroll.0.borrow().base_handle.offset(),
            )
        });
        let key = if cfg!(target_os = "macos") {
            "cmd-|"
        } else {
            "ctrl-shift-\\"
        };
        visual.simulate_keystrokes(key);
        visual.run_until_parked();
        assert!(visual.debug_bounds("sidebar-tree-body").is_none());
        assert!(visual.debug_bounds("sidebar-collapse-all").is_none());
        visual.simulate_keystrokes(key);
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert_eq!(
                r.tree
                    .rows
                    .iter()
                    .map(|row| (row.path.clone(), row.expanded))
                    .collect::<Vec<_>>(),
                before.0
            );
            assert_eq!(r.tree.cursor, before.1);
            assert_eq!(r.tree_scroll.0.borrow().base_handle.offset(), before.2);
            assert_eq!(r.sidebar.left_mode, LeftMode::Tree);
        });
        let restored = reader_sidebar::State::load(&temp.path().join("state/sidebar.json"));
        assert_eq!(restored.left_mode, LeftMode::Tree);
    }

    #[gpui::test]
    fn projection_debounce_rejects_an_old_vault_and_preserves_source_bytes(
        cx: &mut TestAppContext,
    ) {
        let (reader, visual, temp) = fixture(cx);
        let note = temp.path().join("canonical.md");
        std::fs::write(&note, "# Keep these exact bytes\r\n").unwrap();
        let before = std::fs::read(&note).unwrap();
        let before_time = std::fs::metadata(&note).unwrap().modified().unwrap();
        let mut index = Index::default();
        index.replace("Dev/Projects/App/Plan.md", "", 100);
        index.refresh(101, 0, 0);
        reader.update(visual, |r, cx| {
            r.publish_workbench(Arc::new(index.clone()), cx)
        });
        visual.run_until_parked();
        reader.read_with(visual, |r, _| assert!(r.workbench_ready.is_none()));
        reader.update(visual, |r, _| r.watcher_generation += 1);
        visual.executor().advance_clock(Duration::from_millis(501));
        visual.run_until_parked();
        reader.read_with(visual, |r, _| {
            assert!(r.workbench_ready.is_none(), "old owner cannot publish")
        });
        reader.update(visual, |r, cx| r.publish_workbench(Arc::new(index), cx));
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(501));
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("now-sidebar").is_some(),
            "positive publication control"
        );
        reader.read_with(visual, |r, _| {
            assert_eq!(r.workbench_ready.as_ref().unwrap().view.active.len(), 1)
        });
        assert_eq!(std::fs::read(&note).unwrap(), before);
        assert_eq!(
            std::fs::metadata(&note).unwrap().modified().unwrap(),
            before_time
        );
    }

    #[test]
    fn mode_defaults_and_os_punctuation_preserve_the_physical_shortcut() {
        assert_eq!(reader_sidebar::State::default().left_mode, LeftMode::Now);
        let state: reader_sidebar::State = serde_json::from_str("{}").unwrap();
        assert_eq!(state.left_mode, LeftMode::Now);
        let mac = gpui::Keystroke::parse("cmd-|").unwrap();
        assert!(mac.should_match(&gpui::KeybindingKeystroke::parse("cmd-|").unwrap()));
        assert!(!mac.should_match(&gpui::KeybindingKeystroke::parse("cmd-\\").unwrap()));
        assert!(!mac.should_match(&gpui::KeybindingKeystroke::parse("cmd-shift-\\").unwrap()));
    }

    #[test]
    #[ignore = "read-only live census; set OKILUM_WORKBENCH_VAULT explicitly"]
    fn live_census_and_projection_latency() {
        let root = PathBuf::from(
            std::env::var_os("OKILUM_WORKBENCH_VAULT").expect("explicit census root"),
        );
        let mut pending = vec![root.clone()];
        let mut sources = Vec::new();
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                let rel = path
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if workbench::noise(&rel) {
                    continue;
                }
                let kind = entry.file_type().unwrap();
                if kind.is_dir() {
                    pending.push(path);
                } else if kind.is_file() && path.extension().is_some_and(|e| e == "md") {
                    let mtime = entry
                        .metadata()
                        .unwrap()
                        .modified()
                        .unwrap()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs();
                    if let Ok(source) = std::fs::read_to_string(path) {
                        sources.push((rel, source, mtime));
                    }
                }
            }
        }
        let candidates = units(&root);
        let (now, midnight, yesterday) = clock();
        let start = std::time::Instant::now();
        let mut index = Index::default();
        index.set_units(candidates);
        for (path, source, mtime) in &sources {
            index.replace(path, source, *mtime);
        }
        index.refresh(now, midnight, yesterday);
        let full = start.elapsed();
        let start = std::time::Instant::now();
        let mut batch = index.clone();
        for (path, source, mtime) in sources.iter().take(20) {
            batch.replace(path, source, *mtime);
        }
        batch.refresh(now, midnight, yesterday);
        let cost = start.elapsed();
        println!("WORKBENCH_LIVE notes={} active7={} older14={} today={} places={} full_ms={:.3} batch20_ms={:.3}", sources.len(), index.view.active.len(), index.view.older,
            index.view.days[0].count, index.view.days[0].places.len(), full.as_secs_f64()*1000., cost.as_secs_f64()*1000.);
        for activity in &index.view.active {
            println!("ACTIVE {}", activity.unit.path);
        }
    }
}
