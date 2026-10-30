//! Now/Tree navigation and publication over accepted source snapshots (#1927).
use super::*;
use chrono::{Datelike as _, TimeZone as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use okilum_core::workbench::{self, Activity, Index, Unit};
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
            let doc = okilum_t3::area_files::read(&project.folder, "project")
                .ok()
                .flatten()
                .map(|doc| doc.value)
                .unwrap_or_default();
            let terminal = ["status", "lifecycle"].iter().any(|key| {
                doc.get(*key)
                    .and_then(serde_yaml::Value::as_str)
                    .is_some_and(workbench::terminal)
            });
            let status = doc
                .get("status")
                .and_then(serde_yaml::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let name = doc
                .get("slug")
                .and_then(serde_yaml::Value::as_str)
                .or_else(|| {
                    doc.get("repo")
                        .and_then(|r| r.get("name"))
                        .and_then(serde_yaml::Value::as_str)
                })
                .unwrap_or(&project.name);
            Some(Unit {
                title: workbench::short_project_name(&path, name),
                path,
                terminal,
                status,
                manager: matches!(project.manager, okilum_t3::projects::Manager::Thread(_)),
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
    index.observed_at = now;
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

impl Reader {
    pub(super) fn refresh_workbench_clock(&mut self, cx: &mut Context<Self>) {
        let minute = reader_sidebar::now() / 60;
        if self.workbench_ready.is_none() || minute == self.workbench_clock_minute {
            return;
        }
        self.workbench_clock_minute = minute;
        let original = self.workbench.clone();
        let root = self.vault_root.clone();
        let generation = self.watcher_generation;
        let index = original.clone();
        let task = cx.background_executor().spawn(async move {
            let mut index = (*index).clone();
            let (now, midnight, yesterday) = clock();
            index.refresh(now, midnight, yesterday);
            Arc::new(index)
        });
        cx.spawn(async move |this, cx| {
            let next = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.vault_root == root
                    && this.watcher_generation == generation
                    && Arc::ptr_eq(&this.workbench, &original)
                {
                    this.publish_workbench(next, cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn publish_workbench(&mut self, index: Arc<Index>, cx: &mut Context<Self>) {
        self.workbench = index.clone();
        self.workbench_clock_minute = index.view.built_at / 60;
        self.workbench_pending = self.workbench_pending.wrapping_add(1);
        let pending = self.workbench_pending;
        let root = self.vault_root.clone();
        let generation = self.watcher_generation;
        let prepared = index.clone();
        let labels = cx.background_executor().spawn(async move {
            // Local timezone lookup/formatting stays off the render path too.
            let mut labels = std::collections::BTreeMap::new();
            labels.insert(prepared.observed_at, shown_time(prepared.observed_at));
            for row in prepared
                .view
                .days
                .iter()
                .flat_map(|day| &day.places)
                .flat_map(|place| &place.rows)
            {
                labels
                    .entry(row.modified)
                    .or_insert_with(|| shown_time(row.modified));
            }
            labels
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let labels = labels.await;
            let _ = this.update(cx, |this, cx| {
                if this.vault_root == root
                    && this.watcher_generation == generation
                    && this.workbench_pending == pending
                {
                    if this.sidebar.initialize_now(index.view.project_count > 0) {
                        this.save_sidebar(cx);
                    }
                    this.workbench_ready = Some(index);
                    this.workbench_times = labels;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn workbench_time(&self, seconds: u64) -> String {
        self.workbench_times
            .get(&seconds)
            .cloned()
            .unwrap_or_else(|| "—".into())
    }

    pub(super) fn set_left_mode(
        &mut self,
        mode: LeftMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.single_file
            || self.project_navigation.active.is_some()
            || !self.sidebar.now_enabled()
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

    pub(super) fn set_now_enabled(
        &mut self,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.single_file {
            return;
        }
        self.sidebar.choose_now(enabled);
        if enabled && self.tree_focus.contains_focused(window, cx) {
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
        let tokens = brand::palette(cx);
        h_flex()
            .flex_none()
            .gap_1()
            .p(px(2.))
            .rounded(px(6.))
            .bg(tokens.border_subtle.opacity(0.5))
            .children(
                [
                    ("left-mode-now", "Now", LeftMode::Now),
                    ("left-mode-tree", "Tree", LeftMode::Tree),
                ]
                .into_iter()
                .map(|(id, label, mode)| {
                    let reader = reader.clone();
                    let active = self.sidebar.left_mode == mode;
                    let icon = match mode {
                        LeftMode::Now => Icon::default().path(brand::READER_CLOCK_ICON),
                        LeftMode::Tree => Icon::default().path("icons/list.svg"),
                    };
                    Button::new(id)
                        .ghost()
                        .small()
                        .w(px(62.))
                        .h(px(24.))
                        .px_1()
                        .rounded(px(4.))
                        .icon(icon.xsmall())
                        .label(label)
                        .selected(active)
                        .text_size(px(11.))
                        .text_color(if active {
                            tokens.text
                        } else {
                            tokens.text_muted
                        })
                        .when(active, |button| {
                            button
                                .bg(tokens.surface_raised)
                                .border_1()
                                .border_color(tokens.border_subtle)
                                .font_weight(FontWeight::SEMIBOLD)
                        })
                        .debug_selector(move || id.into())
                        .tooltip(reader_shortcuts::hint("Now / Tree", &ToggleLeftMode, cx))
                        .on_click(move |_, window, cx| {
                            let _ =
                                reader.update(cx, |this, cx| this.set_left_mode(mode, window, cx));
                        })
                }),
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
        count: usize,
        expanded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let reader = cx.entity().downgrade();
        let open = if expanded {
            self.sidebar.now_expanded.contains(&key)
        } else {
            !self.sidebar.now_collapsed.contains(&key)
        };
        let selector = format!("now-heading-{key}");
        h_flex()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .w_full()
            .flex_none()
            .h(px(28.))
            .gap_1()
            .px_2()
            .cursor_pointer()
            .text_size(px(10.))
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
            .child(div().flex_1())
            .child(count.to_string())
            .on_click(move |_, _, cx| {
                let _ = reader.update(cx, |this, cx| this.toggle_now_section(&key, expanded, cx));
            })
            .into_any_element()
    }

    fn now_project_row(
        &self,
        activity: &Activity,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = brand::palette(cx);
        let reader = cx.entity().downgrade();
        let root = self.vault_root.clone();
        let generation = self.watcher_generation;
        let open = activity.open.clone();
        let selector = if selected {
            "now-current-project".to_owned()
        } else {
            format!("now-project-{}", activity.unit.path)
        };
        let status = activity.unit.status.to_ascii_lowercase();
        let color = match status.as_str() {
            "paused" | "on-hold" | "blocked" => tokens.warning,
            "planned" | "planning" => tokens.text_muted,
            _ => tokens.accent,
        };
        let tooltip = format!(
            "{}\n{} files changed this week{}",
            location(&activity.unit.path),
            activity.count,
            if status.is_empty() {
                String::new()
            } else {
                format!(" · {status}")
            }
        );
        h_flex()
            .id(SharedString::from(format!(
                "now-project-{}",
                activity.unit.path
            )))
            .debug_selector(move || selector.clone())
            .w_full()
            .min_w_0()
            .h(px(26.))
            .px_3()
            .gap_1p5()
            .rounded(px(3.))
            .cursor_pointer()
            .when(selected, |row| row.bg(tokens.selected))
            .hover(|row| {
                row.bg(if selected {
                    tokens.selected
                } else {
                    brand::reader_palette(cx).hover
                })
            })
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
            })
            .child(div().size(px(6.)).flex_none().rounded_full().bg(color))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(activity.unit.title.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(10.))
                    .text_color(tokens.text_muted)
                    .child(activity.activity_label(
                        self.workbench_ready.as_ref().map_or(0, |i| i.view.built_at),
                    )),
            )
            .when(activity.unit.manager, |row| {
                row.child(
                    div()
                        .flex_none()
                        .px_1()
                        .rounded(px(2.))
                        .text_size(px(9.))
                        .text_color(tokens.text_muted)
                        .bg(tokens.border_subtle)
                        .child("M")
                        .debug_selector(|| "now-manager-badge".into()),
                )
            })
            .on_click(move |_, window, cx| {
                let _ = reader.update(cx, |this, cx| {
                    if this.vault_root == root && this.watcher_generation == generation {
                        this.open_note(&open, None, window, cx);
                    }
                });
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
        let selected = path == self.current_rel;
        let tooltip = location(path);
        let path = path.to_owned();
        v_flex()
            .id(SharedString::from(format!("now-note-{path}")))
            .w_full()
            .min_w_0()
            .px_2()
            .py_1()
            .rounded(px(6.))
            .cursor_pointer()
            .when(selected, |row| row.bg(brand::palette(cx).selected))
            .debug_selector(move || {
                if selected {
                    "now-current-note".into()
                } else {
                    "now-note".into()
                }
            })
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
            })
            .hover(|style| {
                style.bg(if selected {
                    brand::palette(cx).selected
                } else {
                    brand::reader_palette(cx).hover
                })
            })
            .child(
                div()
                    .w_full()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(title),
            )
            .when(!secondary.is_empty(), |row| {
                row.child(
                    div()
                        .w_full()
                        .text_size(px(10.))
                        .text_color(brand::palette(cx).text_muted)
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(secondary),
                )
            })
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
        let Some(index) = &self.workbench_ready else {
            return div()
                .p_2()
                .text_color(tokens.text_muted)
                .child("Preparing Now…")
                .into_any_element();
        };
        let view = &index.view;
        let reader = cx.entity().downgrade();
        let menu_reader = reader.clone();
        let settings_reader = reader.clone();
        let reveal_reader = reader.clone();
        let network = self.loading.as_ref().is_some_and(|load| load.network);
        let fresh = format!(
            "{} · snapshot {}",
            if network { "NFS" } else { "live" },
            self.workbench_time(index.observed_at)
        );
        let freshness =
            h_flex()
                .w_full()
                .flex_none()
                .h(px(28.))
                .px_3()
                .gap_1()
                .text_size(px(10.))
                .text_color(tokens.text_muted)
                .child(div().size(px(5.)).rounded_full().bg(if network {
                    tokens.text_muted
                } else {
                    tokens.success
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(fresh),
                )
                .child(
                    Button::new("now-rescan")
                        .ghost()
                        .xsmall()
                        .label("Rescan")
                        .debug_selector(|| "now-rescan".into())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.refresh_inventory_with_read(
                                okilum_core::Changes::default(),
                                true,
                                window,
                                cx,
                            )
                        })),
                )
                .child(
                    Button::new("now-menu")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Ellipsis)
                        .tooltip("Now actions")
                        .debug_selector(|| "now-menu".into())
                        .dropdown_menu(move |menu, _, _| {
                            let collapse_reader = menu_reader.clone();
                            let settings_reader = settings_reader.clone();
                            let reveal_reader = reveal_reader.clone();
                            menu.item(PopupMenuItem::new("New note").on_click(
                                sidebar_action_click(menu_reader.clone(), SidebarAction::NewNote),
                            ))
                            .item(PopupMenuItem::new("Reveal current note in Tree").on_click(
                                move |_, window, cx| {
                                    let _ = reveal_reader.update(cx, |this, cx| {
                                        let path = this.current_rel.clone();
                                        this.reveal_in_tree(&path, window, cx);
                                    });
                                },
                            ))
                            .item(PopupMenuItem::new("Collapse sections").on_click(
                                move |_, _, cx| {
                                    let _ = collapse_reader.update(cx, |this, cx| {
                                        this.sidebar.now_collapsed.extend(
                                            ["active", "changes", "pins"].map(str::to_owned),
                                        );
                                        this.save_sidebar(cx);
                                        cx.notify();
                                    });
                                },
                            ))
                            .item(
                                PopupMenuItem::new("Vault settings").on_click(move |_, _, cx| {
                                    reader_settings::show_vault(Some(settings_reader.clone()), cx)
                                }),
                            )
                        }),
                );
        let current_unit = index.unit_for(&self.current_rel);
        let mut groups: std::collections::BTreeMap<&str, Vec<&Activity>> = Default::default();
        for activity in &view.active {
            groups.entry(activity.domain()).or_default().push(activity);
        }
        let mut group_order: Vec<_> = groups.into_iter().collect();
        let domain_order = |domain: &str| match domain {
            "Dev" => 0,
            "Music" => 1,
            "AI · HomeLab" => 2,
            _ => 3,
        };
        group_order.sort_by(|a, b| domain_order(a.0).cmp(&domain_order(b.0)).then(a.0.cmp(b.0)));
        let mut active = v_flex().w_full().min_w_0().pb_2();
        for (domain, mut rows) in group_order {
            // Keep the current project visible even when the group is compact.
            // Stable sorting preserves recency among the other projects.
            rows.sort_by_key(|row| current_unit != Some(row.unit.path.as_str()));
            active = active.child(
                h_flex()
                    .px_3()
                    .h(px(24.))
                    .gap_1()
                    .text_size(px(10.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(tokens.text_muted)
                    .child(domain.to_owned())
                    .child(div().flex_1())
                    .child(rows.len().to_string()),
            );
            let key = format!("domain-{domain}");
            let shown = if self.sidebar.now_expanded.contains(&key) {
                rows.len()
            } else {
                4
            };
            for row in rows.iter().take(shown) {
                active = active.child(self.now_project_row(
                    row,
                    current_unit == Some(row.unit.path.as_str()),
                    cx,
                ));
            }
            if rows.len() > shown {
                let reader = reader.clone();
                let names = rows
                    .iter()
                    .skip(shown)
                    .take(3)
                    .map(|row| row.unit.title.as_str())
                    .collect::<Vec<_>>()
                    .join(" · ");
                let preview: String = names.chars().take(30).collect();
                let preview = if names.chars().count() > 30 {
                    format!("{preview}…")
                } else {
                    preview
                };
                active = active.child(
                    Button::new(SharedString::from(key.clone()))
                        .ghost()
                        .small()
                        .w_full()
                        .label(format!("{preview}  +{}", rows.len() - shown))
                        .tooltip(names)
                        .text_size(px(10.))
                        .debug_selector(|| "now-more-projects".into())
                        .on_click(move |_, _, cx| {
                            let _ = reader
                                .update(cx, |this, cx| this.toggle_now_section(&key, true, cx));
                        }),
                );
            }
        }
        if view.active.is_empty() {
            active = active.child(
                div()
                    .px_3()
                    .py_2()
                    .text_size(px(11.))
                    .text_color(tokens.text_muted)
                    .child("No project activity this week"),
            );
        }
        if view.older > 0 {
            active = active.child(self.now_heading(
                "older".into(),
                "More in the last 14 days".into(),
                view.older,
                true,
                cx,
            ));
            if self.sidebar.now_expanded.contains("older") {
                for row in &view.older_active {
                    active = active.child(self.now_project_row(
                        row,
                        current_unit == Some(row.unit.path.as_str()),
                        cx,
                    ));
                }
            }
        }
        let mut changes = v_flex().w_full().min_w_0().pb_2();
        for period in &view.days {
            let key = format!("day-{}", period.label);
            changes = changes.child(self.now_heading(
                key.clone(),
                period.label.into(),
                period.count,
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
                let title = place
                    .path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&place.path)
                    .to_owned();
                changes =
                    changes.child(self.now_heading(key.clone(), title, place.count, true, cx));
                if self.sidebar.now_expanded.contains(&key) {
                    for row in &place.rows {
                        let title = if row.count > 1 {
                            format!("{} · {} files", row.title, row.count)
                        } else {
                            row.title.clone()
                        };
                        changes = changes.child(self.now_note_row(
                            &row.path,
                            title,
                            format!(
                                "{} · {}",
                                self.workbench_time(row.modified),
                                location(&place.path)
                            ),
                            cx,
                        ));
                    }
                }
            }
            if period.places.len() > shown {
                let reader = reader.clone();
                changes = changes.child(
                    Button::new(SharedString::from(more.clone()))
                        .ghost()
                        .small()
                        .label(format!("+{} places", period.places.len() - shown))
                        .on_click(move |_, _, cx| {
                            let _ = reader
                                .update(cx, |this, cx| this.toggle_now_section(&more, true, cx));
                        }),
                );
            }
        }
        let mut pins = v_flex().w_full().min_w_0().pb_2();
        for path in &self.sidebar.pinned {
            pins =
                pins.child(self.now_note_row(path, self.vault.note_title(path), String::new(), cx));
        }
        let file_count = self
            .vault
            .entries
            .iter()
            .filter(|entry| entry.kind != okilum_core::vault::EntryKind::Directory)
            .count();
        let shortcut = if cfg!(target_os = "macos") {
            "⌘⇧\\"
        } else {
            "Ctrl+Shift+\\"
        };
        let footer = h_flex()
            .id("now-tree-footer")
            .w_full()
            .flex_none()
            .h(px(30.))
            .px_3()
            .gap_1()
            .border_t_1()
            .border_color(tokens.border_subtle)
            .cursor_pointer()
            .text_size(px(10.))
            .text_color(tokens.text_muted)
            .debug_selector(|| "now-tree-footer".into())
            .child(format!("Tree · {file_count} files"))
            .child(div().flex_1())
            .child(shortcut)
            .on_click(move |_, window, cx| {
                let _ = reader.update(cx, |this, cx| {
                    this.set_left_mode(LeftMode::Tree, window, cx)
                });
            });
        v_flex()
            .id("now-sidebar")
            .debug_selector(|| "now-sidebar".into())
            .w_full()
            .h_full()
            .min_h_0()
            .text_size(px(brand::READER_CHROME_FONT_SIZE))
            .child(freshness)
            .child(self.now_heading(
                "active".into(),
                "IN PROGRESS".into(),
                view.active.len(),
                false,
                cx,
            ))
            .when(!self.sidebar.now_collapsed.contains("active"), |panel| {
                panel.child(
                    div()
                        .id("now-project-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.workbench_scroll)
                        .child(active),
                )
            })
            .child(self.now_heading(
                "changes".into(),
                "CHANGES".into(),
                view.days.iter().map(|p| p.count).sum(),
                false,
                cx,
            ))
            .when(!self.sidebar.now_collapsed.contains("changes"), |panel| {
                panel.child(
                    div()
                        .id("now-change-scroll")
                        .min_h_0()
                        .max_h(px(200.))
                        .overflow_y_scroll()
                        .child(changes),
                )
            })
            .child(self.now_heading(
                "pins".into(),
                "PINNED".into(),
                self.sidebar.pinned.len(),
                false,
                cx,
            ))
            .when(!self.sidebar.now_collapsed.contains("pins"), |panel| {
                panel.child(
                    div()
                        .id("now-pin-scroll")
                        .min_h_0()
                        .max_h(px(120.))
                        .overflow_y_scroll()
                        .child(pins),
                )
            })
            .when(self.sidebar.now_collapsed.contains("active"), |panel| {
                panel.child(div().flex_1())
            })
            .child(footer)
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
                        vault: Some(temp.path().join("vault")),
                        session_directory: Some(temp.path().join("state")),
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
                reader.sidebar.initialize_now(true);
                reader.sidebar_path = Some(reader_sidebar::State::path(
                    &temp.path().join("state"),
                    &reader.vault_root,
                ));
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
    fn compact_groups_keep_all_section_headers_footer_and_current_project_visible(
        cx: &mut TestAppContext,
    ) {
        let (reader, visual, _temp) = fixture(cx);
        let now = reader_sidebar::now();
        let mut index = Index::default();
        index.set_units(vec![Unit {
            path: "Dev/Projects/App".into(),
            title: "App".into(),
            manager: true,
            ..Default::default()
        }]);
        index.replace("Dev/Projects/App/Note 090.md", "", now - 100);
        for i in 0..60 {
            index.replace(
                &format!("Dev/Projects/Project{i:02}/note.md"),
                "",
                now - i - 1,
            );
        }
        index.observed_at = now;
        index.refresh(now, now - 1000, now - workbench::DAY);
        reader.update(visual, |r, cx| {
            r.current_rel = "Dev/Projects/App/Note 090.md".into();
            r.workbench = Arc::new(index);
            r.workbench_ready = Some(r.workbench.clone());
            r.workbench_clock_minute = now / 60;
            r.sidebar.pinned = vec![r.current_rel.clone()];
            cx.notify();
        });
        visual.run_until_parked();
        let active = visual.debug_bounds("now-heading-active").unwrap();
        let changes = visual.debug_bounds("now-heading-changes").unwrap();
        let pins = visual.debug_bounds("now-heading-pins").unwrap();
        let footer = visual.debug_bounds("now-tree-footer").unwrap();
        assert!(
            active.origin.y < changes.origin.y
                && changes.origin.y < pins.origin.y
                && pins.origin.y < footer.origin.y
        );
        assert!(
            footer.bottom() <= px(800.),
            "the long project list cannot push navigation offscreen"
        );
        let current = visual.debug_bounds("now-current-project").unwrap();
        assert!(
            current.size.height <= px(28.),
            "one compact line per project"
        );
        assert!(
            visual.debug_bounds("now-manager-badge").is_some(),
            "actual manager-binding positive control"
        );
        assert!(
            visual
                .debug_bounds("now-project-Dev/Projects/Project03")
                .is_none(),
            "group tail starts collapsed"
        );
        let more = visual.debug_bounds("now-more-projects").unwrap();
        visual.simulate_click(more.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(visual
            .debug_bounds("now-project-Dev/Projects/Project03")
            .is_some());
        assert!(
            visual.debug_bounds("now-heading-changes").unwrap().origin.y
                < visual.debug_bounds("now-tree-footer").unwrap().origin.y
        );
        let pins = visual.debug_bounds("now-heading-pins").unwrap();
        visual.simulate_click(pins.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("now-current-note").is_some(),
            "current pinned note is also marked"
        );
    }

    #[gpui::test]
    fn plain_vault_has_only_tree_until_manual_vault_preference_enables_now(
        cx: &mut TestAppContext,
    ) {
        let (reader, visual, temp) = fixture(cx);
        let now = reader_sidebar::now();
        let mut index = Index::default();
        index.replace("Note.md", "# Note", now);
        index.refresh(now, now - 100, now - workbench::DAY);
        assert_eq!(index.view.project_count, 0);
        reader.update(visual, |r, cx| {
            r.vault = Arc::new(Vault::from_note_paths(["Note.md".into()]));
            r.current_rel = "Note.md".into();
            r.sidebar = reader_sidebar::State::default();
            r.sidebar.initialize_now(false);
            r.workbench = Arc::new(index);
            r.workbench_ready = Some(r.workbench.clone());
            r.workbench_clock_minute = now / 60;
            r.sync_tree();
            cx.notify();
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("sidebar-tree-body").is_some());
        assert!(visual.debug_bounds("left-mode-now").is_none());
        reader.update_in(visual, |r, window, cx| r.set_now_enabled(true, window, cx));
        visual.run_until_parked();
        assert!(visual.debug_bounds("left-mode-now").is_some());
        assert!(visual.debug_bounds("now-sidebar").is_some());
        assert!(visual.debug_bounds("sidebar-tree-body").is_none());
        let tree = visual.debug_bounds("left-mode-tree").unwrap();
        visual.simulate_click(tree.center(), Modifiers::default());
        visual.run_until_parked();
        let path =
            reader_sidebar::State::path(&temp.path().join("state"), &temp.path().join("vault"));
        let mut reopened = reader_sidebar::State::load(&path);
        assert!(!reopened.initialize_now(false));
        assert!(reopened.now_enabled());
        assert_eq!(reopened.left_mode, LeftMode::Tree);
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
        let path =
            reader_sidebar::State::path(&temp.path().join("state"), &temp.path().join("vault"));
        assert!(
            path.is_file(),
            "the actual per-vault application state was saved"
        );
        let restored = reader_sidebar::State::load(&path);
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
        let now = reader_sidebar::now();
        index.replace("Dev/Projects/App/Plan.md", "", now);
        index.refresh(
            now,
            now.saturating_sub(1000),
            now.saturating_sub(workbench::DAY),
        );
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
        let mut state: reader_sidebar::State = serde_json::from_str("{}").unwrap();
        assert_eq!(
            state.left_mode,
            LeftMode::Tree,
            "no pre-discovery Now flash"
        );
        assert!(!state.now_enabled());
        assert!(state.initialize_now(true));
        assert_eq!(state.left_mode, LeftMode::Now);
        let mac = gpui::Keystroke::parse("cmd-|").unwrap();
        let binding =
            |key| gpui::KeybindingKeystroke::from_keystroke(gpui::Keystroke::parse(key).unwrap());
        assert!(mac.should_match(&binding("cmd-|")));
        assert!(!mac.should_match(&binding("cmd-\\")));
        assert!(!mac.should_match(&binding("cmd-shift-\\")));
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
