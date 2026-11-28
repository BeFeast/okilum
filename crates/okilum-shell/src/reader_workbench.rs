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
            // Manifests park a project with `status` or its delivery `profile`.
            let parked = ["status", "profile"].iter().any(|key| {
                doc.get(*key)
                    .and_then(serde_yaml::Value::as_str)
                    .is_some_and(|value| value.trim().eq_ignore_ascii_case("parked"))
            });
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
                parked,
            })
        })
        .collect()
}

pub(super) fn from_snapshot(
    snapshot: &okilum_core::vault::warm::Snapshot,
    root: &Path,
    projection: &reader_view_query::Index,
) -> Arc<Index> {
    let (now, midnight, yesterday) = clock();
    Arc::new(Index::from_snapshot(
        snapshot,
        units(root),
        projection.projection.as_deref().ok(),
        now,
        midnight,
        yesterday,
    ))
}

pub(super) fn update_index(
    index: &mut Index,
    snapshot: &okilum_core::vault::warm::Snapshot,
    projection: &reader_view_query::Index,
    removed: &std::collections::BTreeSet<String>,
    changed: &std::collections::BTreeSet<String>,
) {
    index.apply_snapshot(
        snapshot,
        projection.projection.as_deref().ok(),
        removed,
        changed,
    );
    let (now, midnight, yesterday) = clock();
    index.observed_at = now;
    index.refresh(now, midnight, yesterday);
}

pub(super) fn shown_time(seconds: u64) -> String {
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
    /// T3 activity of the discovered projects, keyed by vault-relative path.
    /// Empty while T3 is unreadable: Now then orders by folder activity.
    pub(super) fn now_t3_activity(&self) -> std::collections::BTreeMap<String, u64> {
        self.project_navigation
            .activity
            .iter()
            .filter_map(|(folder, at)| {
                let path = folder.strip_prefix(&self.vault_root).ok()?;
                Some((path.to_string_lossy().replace('\\', "/"), *at))
            })
            .collect()
    }

    /// Rank of a project folder in Now: projects in progress by their latest
    /// activity first, then folders Now does not track, then Other.
    pub(super) fn now_rank(&self) -> impl Fn(&Path) -> (u8, std::cmp::Reverse<u64>) {
        let mut ranks = std::collections::BTreeMap::new();
        if let Some(index) = &self.workbench_ready {
            let order = index.view.ordered(&self.now_t3_activity());
            for (tier, rows) in [(0, &order.active), (0, &order.older), (2, &order.other)] {
                for row in rows {
                    ranks.insert(
                        row.unit.path.clone(),
                        (tier, std::cmp::Reverse(row.modified)),
                    );
                }
            }
        }
        let root = self.vault_root.clone();
        move |folder: &Path| {
            folder
                .strip_prefix(&root)
                .ok()
                .and_then(|path| ranks.get(&path.to_string_lossy().replace('\\', "/")))
                .copied()
                .unwrap_or((1, std::cmp::Reverse(0)))
        }
    }

    pub(super) fn refresh_workbench_clock(&mut self, cx: &mut Context<Self>) {
        let minute = reader_sidebar::now() / 60;
        if !self.sidebar.now_enabled()
            || self.workbench_ready.is_none()
            || minute == self.workbench_clock_minute
        {
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
            let retired = next.clone();
            let _ = this.update(cx, |this, cx| {
                if this.vault_root == root
                    && this.watcher_generation == generation
                    && Arc::ptr_eq(&this.workbench, &original)
                {
                    this.publish_workbench(next, cx);
                }
            });
            cx.background_executor()
                .spawn(async move { drop((original, retired)) })
                .detach();
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
                .flat_map(|day| &day.groups)
                .flat_map(|group| &group.rows)
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
            #[cfg(test)]
            let _work = reader_editor::work::scope();
            // A superseded or orphaned completion still owns a whole index.
            // Keep it outside the UI update until acceptance is established.
            let mut publication = Some((index, labels));
            let mut publish = |cx: &mut gpui::AsyncApp| {
                this.update(cx, |this, cx| {
                    // Measure this completion's own work. The test-support
                    // flush after the update redraws dirty windows; that frame
                    // belongs to whoever measures rendering, not to this
                    // publication (it inflated #2405's autosave accounting).
                    #[cfg(test)]
                    let _work = reader_editor::work::scope();
                    if this.vault_root == root
                        && this.watcher_generation == generation
                        && this.workbench_pending == pending
                    {
                        let initialized_now = this.sidebar.initialize_now();
                        if initialized_now {
                            this.save_sidebar(cx);
                        }
                        let (index, labels) = publication.take().unwrap();
                        // This is the delayed owner of the previous published
                        // index; releasing it also walks vault-sized metadata.
                        let retired = (
                            this.workbench_ready.replace(index),
                            std::mem::replace(&mut this.workbench_times, labels),
                        );
                        cx.background_executor()
                            .spawn(async move { drop(retired) })
                            .detach();
                        if initialized_now || this.sidebar.left_mode == LeftMode::Now {
                            this.refresh_now_feed(cx);
                            cx.notify();
                        }
                    }
                })
            };
            #[cfg(test)]
            let _ = reader_editor::work::unscoped(|| publish(cx));
            #[cfg(not(test))]
            let _ = publish(cx);
            cx.background_executor()
                .spawn(async move { drop(publication) })
                .detach();
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
            || (mode == LeftMode::Areas && self.areas_tree().is_none())
        {
            return;
        }
        if mode == LeftMode::Now && !self.sidebar.now_enabled() {
            self.set_now_enabled(true, window, cx);
            return;
        }
        if !self.sidebar.select_left_mode(mode) {
            return;
        }
        // Keep the tree object, cursor and scroll handle intact. No reveal or rebuild.
        if mode != LeftMode::Tree && self.tree_focus.contains_focused(window, cx) {
            self.focus_handle.focus(window, cx);
        }
        self.save_sidebar(cx);
        if mode == LeftMode::Now {
            self.refresh_waiting(window, cx);
            self.refresh_now_feed(cx);
        }
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
        if enabled {
            self.refresh_waiting(window, cx);
            self.refresh_now_feed(cx);
        } else {
            // Disabling Now stops Waiting reads at once; nothing is written.
            self.waiting.cancel();
        }
        cx.notify();
    }

    pub(super) fn toggle_left_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mode = match self.left_sidebar_mode() {
            LeftMode::Now | LeftMode::Areas => LeftMode::Tree,
            LeftMode::Tree => LeftMode::Now,
        };
        self.set_left_mode(mode, window, cx);
    }

    pub(super) fn render_left_modes(&self, cx: &mut Context<Self>) -> AnyElement {
        let views: Vec<_> = [
            ("left-mode-tree", "Tree", LeftMode::Tree),
            ("left-mode-now", "Now", LeftMode::Now),
            ("left-mode-areas", "Areas", LeftMode::Areas),
        ]
        .into_iter()
        .filter(|(_, _, mode)| *mode != LeftMode::Areas || self.areas_tree().is_some())
        .map(|(id, label, mode)| {
            let reader = cx.entity().downgrade();
            let icon = match mode {
                LeftMode::Now => Icon::default().path(brand::READER_CLOCK_ICON),
                LeftMode::Tree => Icon::default().path("icons/list.svg"),
                LeftMode::Areas => Icon::default().path("icons/view-tree.svg"),
            };
            let mut action = reader_viewer_chrome::ViewerAction::new(
                id,
                icon,
                label,
                std::rc::Rc::new(move |window, cx| {
                    let _ = reader.update(cx, |this, cx| this.set_left_mode(mode, window, cx));
                }),
            );
            if mode != LeftMode::Areas {
                action.tooltip = reader_shortcuts::hint("Now / Tree", &ToggleLeftMode, cx);
            }
            reader_viewer_chrome::ViewerView {
                action,
                active: self.left_sidebar_mode() == mode,
            }
        })
        .collect();
        reader_segmented::render(&views, reader_segmented::Form::Full, cx)
    }

    pub(super) fn render_now_offer(&self, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .id("second-brain-offer")
            .debug_selector(|| "second-brain-offer".into())
            .px_3()
            .py_1()
            .gap_1()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(11.))
                    .child("Make Now your default view?"),
            )
            .child(
                Button::new("second-brain-enable")
                    .ghost()
                    .small()
                    .label("Use Now")
                    .tooltip("Enable Second brain for this vault")
                    .debug_selector(|| "second-brain-enable".into())
                    .on_click(
                        cx.listener(|this, _, window, cx| this.set_now_enabled(true, window, cx)),
                    ),
            )
            .child(
                Button::new("second-brain-dismiss")
                    .ghost()
                    .small()
                    .label("Not now")
                    .debug_selector(|| "second-brain-dismiss".into())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar.now_offer_answered = true;
                        this.save_sidebar(cx);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    pub(super) fn toggle_now_section(&mut self, key: &str, expanded: bool, cx: &mut Context<Self>) {
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

    pub(super) fn now_heading(
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
            "parked" | "paused" | "on-hold" | "blocked" => tokens.warning,
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
            .when_some(activity.group.status.clone(), |row, status| {
                row.child(
                    div()
                        .flex_none()
                        .text_size(px(10.))
                        .text_color(tokens.text_muted)
                        .child(status),
                )
            })
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
        let selected = path == self.pane.current_rel;
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

    pub(super) fn now_toolbar(
        &self,
        label: String,
        live: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = brand::palette(cx);
        let reader = cx.entity().downgrade();
        let menu_reader = reader.clone();
        let settings_reader = reader.clone();
        let reveal_reader = reader.clone();
        h_flex()
            .w_full()
            .flex_none()
            .h(px(28.))
            .px_3()
            .gap_1()
            .text_size(px(10.))
            .text_color(tokens.text_muted)
            .child(div().size(px(5.)).rounded_full().bg(if live {
                tokens.success
            } else {
                tokens.text_muted
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(label),
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
                        );
                        this.refresh_waiting(window, cx);
                        this.refresh_now_feed(cx);
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
                        menu.item(
                            PopupMenuItem::new("New note").on_click(sidebar_action_click(
                                menu_reader.clone(),
                                SidebarAction::NewNote,
                            )),
                        )
                        .item(
                            PopupMenuItem::new("Today's note").on_click(sidebar_action_click(
                                menu_reader.clone(),
                                SidebarAction::DailyNote,
                            )),
                        )
                        .item(PopupMenuItem::new("Reveal current note in Tree").on_click(
                            move |_, window, cx| {
                                let _ = reveal_reader.update(cx, |this, cx| {
                                    let path = this.pane.current_rel.clone();
                                    this.reveal_in_tree(&path, window, cx);
                                });
                            },
                        ))
                        .item(
                            PopupMenuItem::new("Collapse sections").on_click(move |_, _, cx| {
                                let _ = collapse_reader.update(cx, |this, cx| {
                                    let keys: &[&str] = if this.now_feed_ready() {
                                        &[
                                            "feed-urgent",
                                            "feed-answer",
                                            "feed-hands",
                                            "feed-available",
                                            "feed-waiting",
                                        ]
                                    } else {
                                        &["waiting", "active", "changes", "pins"]
                                    };
                                    this.sidebar
                                        .now_collapsed
                                        .extend(keys.iter().map(|key| (*key).to_owned()));
                                    this.save_sidebar(cx);
                                    cx.notify();
                                });
                            }),
                        )
                        .item(
                            PopupMenuItem::new("Vault settings").on_click(move |_, _, cx| {
                                reader_settings::show_vault(Some(settings_reader.clone()), cx)
                            }),
                        )
                    }),
            )
            .into_any_element()
    }

    pub(super) fn render_workbench(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = brand::palette(cx);
        if self.now_feed_ready() {
            return self.render_now_feed(window, cx);
        }
        let Some(index) = &self.workbench_ready else {
            return div()
                .p_2()
                .text_color(tokens.text_muted)
                .child("Preparing Now…")
                .into_any_element();
        };
        let view = &index.view;
        if view.layout_unavailable {
            return div()
                .p_2()
                .text_color(tokens.text_muted)
                .child("Vault layout is unavailable")
                .into_any_element();
        }
        let reader = cx.entity().downgrade();
        let network = self.loading.as_ref().is_some_and(|load| load.network);
        let fresh = format!(
            "{} · snapshot {}",
            if network { "NFS" } else { "live" },
            self.workbench_time(index.observed_at)
        );
        let freshness = self.now_toolbar(fresh, !network, cx);
        let current_unit = index.unit_for(&self.pane.current_rel);
        let order = view.ordered(&self.now_t3_activity());
        // Groups follow their newest project: `order.active` is newest first,
        // so a group's first appearance is its rank. No alphabetical pinning.
        // (context label, optional (domain, context domain)).
        type GroupKey<'a> = (&'a str, Option<(&'a str, &'a str)>);
        let mut groups: Vec<(GroupKey, Vec<&Activity>)> = Vec::new();
        for activity in &order.active {
            let group = if view.managed {
                match index.activity_context(activity) {
                    Some((context, domain)) => (context.name, Some((domain, context.domain))),
                    None => ("—", None),
                }
            } else {
                (activity.legacy_domain(), None)
            };
            match groups.iter_mut().find(|(key, _)| *key == group) {
                Some((_, rows)) => rows.push(activity),
                None => groups.push((group, vec![activity])),
            }
        }
        // Equally recent groups: named contexts before unassigned, by name.
        groups.sort_by(|(a, a_rows), (b, b_rows)| {
            let tie = |&(label, domain): &GroupKey| {
                (view.managed && domain.is_none(), String::from(label))
            };
            b_rows[0]
                .modified
                .cmp(&a_rows[0].modified)
                .then_with(|| tie(a).cmp(&tie(b)))
        });
        let mut active = v_flex().w_full().min_w_0().pb_2();
        for ((label, domain), rows) in groups {
            active = active.child(
                h_flex()
                    .px_3()
                    .h(px(24.))
                    .gap_1()
                    .text_size(px(10.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(tokens.text_muted)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(label.to_owned()),
                    )
                    .when_some(domain, |header, (name, _)| {
                        header.child(
                            div()
                                .flex_none()
                                .max_w(px(100.))
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(name.to_owned()),
                        )
                    })
                    .child(rows.len().to_string()),
            );
            let key = if let Some((_, domain_id)) = domain {
                format!("active-context-{domain_id}:{label}")
            } else if view.managed {
                "active-context-unassigned".into()
            } else {
                format!("domain-{label}")
            };
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
            // The current context stays visible in a compact group, in its
            // place after the newest rows rather than above them.
            if let Some(row) = rows
                .iter()
                .skip(shown)
                .find(|row| current_unit == Some(row.unit.path.as_str()))
            {
                active = active.child(self.now_project_row(row, true, cx));
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
        if order.active.is_empty() {
            active = active.child(
                div()
                    .px_3()
                    .py_2()
                    .text_size(px(11.))
                    .text_color(tokens.text_muted)
                    .child(if view.managed {
                        "No context activity this week"
                    } else {
                        "No project activity this week"
                    }),
            );
        }
        if !order.older.is_empty() {
            active = active.child(self.now_heading(
                "older".into(),
                "More in the last 14 days".into(),
                order.older.len(),
                true,
                cx,
            ));
            if self.sidebar.now_expanded.contains("older") {
                for row in &order.older {
                    active = active.child(self.now_project_row(
                        row,
                        current_unit == Some(row.unit.path.as_str()),
                        cx,
                    ));
                }
            }
        }
        // Parked, finished and dormant projects: collapsed at the bottom.
        if !order.other.is_empty() {
            active = active.child(self.now_heading(
                "other".into(),
                "OTHER".into(),
                order.other.len(),
                true,
                cx,
            ));
            if self.sidebar.now_expanded.contains("other") {
                for row in &order.other {
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
                period.groups.len()
            } else {
                8
            };
            for group in period.groups.iter().take(shown) {
                let (key, title) = if let Some(path) = &group.legacy_path {
                    (
                        format!("place-{}-{path}", period.label),
                        path.rsplit('/').next().unwrap_or(path).to_owned(),
                    )
                } else {
                    let title = group.group.label();
                    (format!("change-group-{}-{title}", period.label), title)
                };
                changes =
                    changes.child(self.now_heading(key.clone(), title, group.count, true, cx));
                if self.sidebar.now_expanded.contains(&key) {
                    for row in &group.rows {
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
                                location(group.legacy_path.as_deref().unwrap_or_else(|| {
                                    row.path.rsplit_once('/').map_or("", |(parent, _)| parent)
                                }))
                            ),
                            cx,
                        ));
                    }
                }
            }
            if period.groups.len() > shown {
                let reader = reader.clone();
                changes = changes.child(
                    Button::new(SharedString::from(more.clone()))
                        .ghost()
                        .small()
                        .label(format!(
                            "+{} {}",
                            period.groups.len() - shown,
                            if view.managed { "groups" } else { "places" }
                        ))
                        .on_click(move |_, _, cx| {
                            let _ = reader
                                .update(cx, |this, cx| this.toggle_now_section(&more, true, cx));
                        }),
                );
            }
        }
        let mut pins = v_flex().w_full().min_w_0().pb_2();
        // Missing pins are listed in the pinned row only (#2789).
        let present: Vec<&String> = self
            .sidebar
            .pinned
            .iter()
            .filter(|path| !self.pin_missing(path))
            .collect();
        for path in present.iter().copied() {
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
            .children(self.render_now_feed_warning(cx))
            .children(self.render_now_waiting(cx))
            .child(self.now_heading(
                "active".into(),
                "IN PROGRESS".into(),
                order.active.len(),
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
            .child(self.now_heading("pins".into(), "PINNED".into(), present.len(), false, cx))
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
                reader.sidebar.initialize_now();
                reader.sidebar.choose_now(true);
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
        index.set_units(
            std::iter::once(Unit {
                path: "Dev/Projects/App".into(),
                title: "App".into(),
                manager: true,
                ..Default::default()
            })
            .chain((0..60).map(|i| Unit {
                path: format!("Dev/Projects/Project{i:02}"),
                ..Default::default()
            }))
            .collect(),
        );
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
            r.pane.current_rel = "Dev/Projects/App/Note 090.md".into();
            r.workbench = Arc::new(index);
            r.workbench_ready = Some(r.workbench.clone());
            r.workbench_clock_minute = now / 60;
            r.sidebar.pinned = vec![r.pane.current_rel.clone()];
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
                .is_some(),
            "the newest projects lead the compact group"
        );
        assert!(
            visual
                .debug_bounds("now-project-Dev/Projects/Project04")
                .is_none(),
            "group tail starts collapsed; the older current project is shown after it"
        );
        let more = visual.debug_bounds("now-more-projects").unwrap();
        visual.simulate_click(more.center(), Modifiers::default());
        visual.run_until_parked();
        assert!(visual
            .debug_bounds("now-project-Dev/Projects/Project04")
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
    fn contextual_now_offer_accept_and_dismiss_survive_reopen(cx: &mut TestAppContext) {
        for accept in [false, true] {
            let (reader, visual, temp) = fixture(cx);
            let now = reader_sidebar::now();
            let mut index = Index::default();
            for day in 1..=3 {
                index.replace(&format!("Daily/2026-10-{day:02}.md"), "Daily", now);
            }
            index.refresh(now, now - 100, now - workbench::DAY);
            assert!(index.view.suggest_now);
            reader.update(visual, |r, cx| {
                r.sidebar = reader_sidebar::State::default();
                r.sidebar.initialize_now();
                r.workbench = Arc::new(index);
                r.workbench_ready = Some(r.workbench.clone());
                r.workbench_clock_minute = now / 60;
                cx.notify();
            });
            visual.run_until_parked();
            assert!(visual.debug_bounds("sidebar-tree-body").is_some());
            assert!(visual.debug_bounds("left-mode-now").is_some());
            assert!(visual.debug_bounds("second-brain-offer").is_some());
            let button = visual
                .debug_bounds(if accept {
                    "second-brain-enable"
                } else {
                    "second-brain-dismiss"
                })
                .unwrap();
            visual.simulate_click(button.center(), Modifiers::default());
            visual.run_until_parked();
            let path =
                reader_sidebar::State::path(&temp.path().join("state"), &temp.path().join("vault"));
            let reopened = reader_sidebar::State::load(&path);
            assert_eq!(reopened.now_enabled(), accept);
            assert_eq!(
                reopened.left_mode,
                if accept {
                    LeftMode::Now
                } else {
                    LeftMode::Tree
                }
            );
            assert!(reopened.now_offer_answered);
            reader.update(visual, |r, cx| {
                r.sidebar = reopened;
                cx.notify();
            });
            visual.run_until_parked();
            assert!(visual.debug_bounds("second-brain-offer").is_none());
            assert_eq!(visual.debug_bounds("now-sidebar").is_some(), accept);
        }
    }

    #[gpui::test]
    fn plain_vault_keeps_tree_and_visible_now_tab_until_explicit_activation(
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
            r.pane.current_rel = "Note.md".into();
            r.sidebar = reader_sidebar::State::default();
            r.sidebar.initialize_now();
            r.workbench = Arc::new(index);
            r.workbench_ready = Some(r.workbench.clone());
            r.workbench_clock_minute = now / 60;
            r.sync_tree();
            cx.notify();
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("sidebar-tree-body").is_some());
        assert!(visual.debug_bounds("left-mode-now").is_some());
        assert!(visual.debug_bounds("second-brain-offer").is_none());
        assert!(!reader.read_with(visual, |r, _| r.sidebar.now_enabled()));
        let now_tab = visual.debug_bounds("left-mode-now").unwrap();
        visual.simulate_click(now_tab.center(), Modifiers::default());
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
        assert!(!reopened.initialize_now());
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
        // Now / Tree has no default chord since #773 gave it to Notes.
        visual.dispatch_action(ToggleLeftMode);
        visual.run_until_parked();
        assert!(visual.debug_bounds("sidebar-tree-body").is_none());
        assert!(visual.debug_bounds("sidebar-collapse-all").is_none());
        visual.dispatch_action(ToggleLeftMode);
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
    fn navigation_keeps_saved_now_until_tree_is_explicitly_selected(cx: &mut TestAppContext) {
        let (reader, visual, temp) = fixture(cx);
        let note = "Dev/Projects/App/Note 090.md";
        let path =
            reader_sidebar::State::path(&temp.path().join("state"), &temp.path().join("vault"));
        type Navigate = fn(&mut Reader, &mut Window, &mut Context<Reader>);
        let navigation: [Navigate; 4] = [
            Reader::focus_sidebar_search,
            Reader::focus_current_folder,
            Reader::show_vault_files,
            |reader, window, cx| reader.reveal_in_tree("Dev/Projects/App", window, cx),
        ];
        for navigate in navigation {
            reader.update_in(visual, |r, window, cx| {
                r.sidebar.choose_now(true);
                r.pane.current_rel = note.into();
                navigate(r, window, cx);
                assert_eq!(r.sidebar.left_mode, LeftMode::Tree);
                // An unrelated save while viewing Tree must preserve Now.
                r.save_sidebar(cx);
            });
            visual.run_until_parked();
            assert_eq!(reader_sidebar::State::load(&path).left_mode, LeftMode::Now);
        }

        let tab = visual.debug_bounds("left-mode-tree").unwrap();
        visual.simulate_click(tab.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(reader_sidebar::State::load(&path).left_mode, LeftMode::Tree);
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
        index.replace(
            "Dev/Projects/App/@index.md",
            "---\ntype: project\n---\n",
            now,
        );
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
        assert!(state.initialize_now());
        assert_eq!(state.left_mode, LeftMode::Tree);
        state.choose_now(true);
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
        diagnostic_println!("WORKBENCH_LIVE notes={} active7={} older14={} today={} places={} full_ms={:.3} batch20_ms={:.3}", sources.len(), index.view.active.len(), index.view.older,
            index.view.days[0].count, index.view.days[0].groups.len(), full.as_secs_f64()*1000., cost.as_secs_f64()*1000.);
        for activity in &index.view.active {
            diagnostic_println!("ACTIVE {}", activity.unit.path);
        }
        // Now's order from folder activity alone, then merged with a JSON map
        // of T3 activity (vault-relative project path -> Unix seconds).
        let t3: std::collections::BTreeMap<String, u64> = std::env::var_os("OKILUM_WORKBENCH_T3")
            .map(|path| serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap())
            .unwrap_or_default();
        for (source, external) in [("folder", Default::default()), ("folder+t3", t3)] {
            let order = index.view.ordered(&external);
            for (part, rows) in [
                ("active", &order.active),
                ("older", &order.older),
                ("other", &order.other),
            ] {
                let names: Vec<_> = rows.iter().map(|row| row.unit.title.as_str()).collect();
                diagnostic_println!(
                    "ORDER {source} {part} {}: {}",
                    names.len(),
                    names.join(", ")
                );
            }
        }
    }

    fn waiting_source(
        reader: &Reader,
        name: &str,
        thread: &str,
    ) -> okilum_t3::projects::waiting::Source {
        okilum_t3::projects::waiting::Source {
            folder: reader.vault_root.join("Dev/Areas").join(name),
            environment: "home".into(),
            project_id: "managers".into(),
            thread_id: thread.into(),
        }
    }

    fn waiting_read(
        source: &okilum_t3::projects::waiting::Source,
        ids: Result<Vec<&str>, &str>,
    ) -> (PathBuf, okilum_t3::projects::waiting::Read) {
        (
            source.folder.clone(),
            okilum_t3::projects::waiting::Read {
                source: Some(source.clone()),
                questions: Default::default(),
                texts: ids
                    .as_ref()
                    .map(|ids| {
                        ids.iter()
                            .map(|id| (id.to_string(), format!("Ship {id}?")))
                            .collect()
                    })
                    .unwrap_or_default(),
                ids: ids
                    .map(|ids| ids.into_iter().map(str::to_owned).collect())
                    .map_err(str::to_owned),
            },
        )
    }

    fn ready_now(reader: &Entity<Reader>, visual: &mut VisualTestContext) -> u64 {
        let now = reader_sidebar::now();
        let mut index = Index::default();
        index.observed_at = now;
        index.refresh(now, now - 1000, now - workbench::DAY);
        reader.update(visual, |r, cx| {
            r.workbench = Arc::new(index);
            r.workbench_ready = Some(r.workbench.clone());
            r.workbench_clock_minute = now / 60;
            cx.notify();
        });
        now
    }

    /// #2346: rows are T3's pending questions of an exact manager thread; a
    /// failed read keeps the confirmed rows marked stale and names the source.
    #[gpui::test]
    fn waiting_for_me_shows_exact_questions_stale_rows_and_unavailable_sources(
        cx: &mut TestAppContext,
    ) {
        let (reader, visual, _temp) = fixture(cx);
        let now = ready_now(&reader, visual);
        reader.update(visual, |r, cx| {
            let root = r.vault_root.clone();
            let app = waiting_source(r, "App", "mcp:app");
            let down = waiting_source(r, "Down", "mcp:down");
            let titles = std::collections::BTreeMap::from([
                (app.folder.clone(), "App".to_owned()),
                (down.folder.clone(), "Down".to_owned()),
            ]);
            let (generation, permit) = r.waiting.begin(&root, titles);
            assert!(permit.allowed() && r.waiting.checking());
            let reads = std::collections::BTreeMap::from([
                waiting_read(&app, Ok(vec!["q1"])),
                waiting_read(&down, Err("T3 unreachable")),
            ]);
            assert!(r.accept_waiting(&root, generation, reads, now, "12:00".into(), cx));
            assert!(!r.waiting.checking());
            let view = r.waiting.view();
            assert_eq!(
                r.waiting_question_lines(&view.rows[0]),
                (
                    "Ship q1?".to_owned(),
                    "App · open the manager thread".to_owned()
                ),
                "the row states the question T3 reports, then its project"
            );
            let mut unread = view.rows[0].clone();
            unread.text = None;
            assert_eq!(
                r.waiting_question_lines(&unread).0,
                "Question · text unavailable"
            );
        });
        visual.run_until_parked();
        let heading = visual.debug_bounds("now-heading-waiting").unwrap();
        assert!(heading.origin.y < visual.debug_bounds("now-heading-active").unwrap().origin.y);
        assert!(visual.debug_bounds("now-waiting-row").is_some());
        assert!(visual.debug_bounds("now-waiting-unavailable").is_some());
        assert!(visual.debug_bounds("now-waiting-status").is_some());
        assert!(visual.debug_bounds("now-waiting-stale-row").is_none());

        reader.update(visual, |r, cx| {
            let root = r.vault_root.clone();
            let app = waiting_source(r, "App", "mcp:app");
            let down = waiting_source(r, "Down", "mcp:down");
            let (generation, _) = r.waiting.begin(&root, Default::default());
            let reads = std::collections::BTreeMap::from([
                waiting_read(&app, Err("offline")),
                waiting_read(&down, Err("T3 unreachable")),
            ]);
            assert!(r.accept_waiting(&root, generation, reads, now + 60, "12:01".into(), cx));
            let view = r.waiting.view();
            assert_eq!(view.rows.len(), 1);
            assert!(view.rows[0].stale);
            assert_eq!(
                view.rows[0].text.as_deref(),
                Some("Ship q1?"),
                "a stale row keeps its confirmed question text"
            );
            assert_eq!(
                view.rows[0].observed_at, now,
                "stale rows keep their own time"
            );
            assert_eq!(view.unavailable.len(), 2);
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("now-waiting-stale-row").is_some());
        assert!(visual.debug_bounds("now-waiting-row").is_none());
    }

    /// Waiting rows follow Now's activity order, not folder names: a parked
    /// project's question comes last, and T3 activity reorders the rest.
    #[gpui::test]
    fn waiting_rows_follow_project_activity_with_parked_last(cx: &mut TestAppContext) {
        let (reader, visual, _temp) = fixture(cx);
        let now = reader_sidebar::now();
        let mut index = Index::default();
        index.set_units(
            [("Alpha", false), ("Parked", true), ("Zed", false)]
                .into_iter()
                .map(|(name, parked)| Unit {
                    path: format!("Dev/Areas/{name}"),
                    title: name.into(),
                    parked,
                    ..Default::default()
                })
                .collect(),
        );
        index.replace("Dev/Areas/Alpha/a.md", "", now - 3 * workbench::DAY);
        index.replace("Dev/Areas/Parked/a.md", "", now);
        index.replace("Dev/Areas/Zed/a.md", "", now - 60);
        index.observed_at = now;
        index.refresh(now, now - 1000, now - workbench::DAY);
        let order = |r: &Reader| {
            r.ordered_waiting()
                .rows
                .iter()
                .map(|row| row.request_id.clone())
                .collect::<Vec<_>>()
        };
        reader.update(visual, |r, cx| {
            r.workbench = Arc::new(index);
            r.workbench_ready = Some(r.workbench.clone());
            let root = r.vault_root.clone();
            let sources = ["Alpha", "Parked", "Zed"]
                .map(|name| waiting_source(r, name, &format!("mcp:{}", name.to_lowercase())));
            let (generation, _) = r.waiting.begin(&root, Default::default());
            let reads = std::collections::BTreeMap::from([
                waiting_read(&sources[0], Ok(vec!["alpha"])),
                waiting_read(&sources[1], Ok(vec!["parked"])),
                waiting_read(&sources[2], Ok(vec!["zed-1", "zed-2"])),
            ]);
            assert!(r.accept_waiting(&root, generation, reads, now, "12:00".into(), cx));
            assert_eq!(
                order(r),
                ["zed-1", "zed-2", "alpha", "parked"],
                "folder activity alone: newest first, parked last"
            );
            r.project_navigation
                .activity
                .insert(sources[0].folder.clone(), now - 5);
            r.project_navigation
                .activity
                .insert(sources[1].folder.clone(), now);
            assert_eq!(
                order(r),
                ["alpha", "zed-1", "zed-2", "parked"],
                "T3 activity reorders projects but never lifts a parked one"
            );
        });
    }

    /// #2346: a reply belongs to the vault, generation and enabled Now that
    /// dispatched it. Disabling Now revokes the in-flight dispatch.
    #[gpui::test]
    fn late_waiting_replies_from_another_vault_an_older_read_or_a_disabled_now_are_refused(
        cx: &mut TestAppContext,
    ) {
        let (reader, visual, _temp) = fixture(cx);
        ready_now(&reader, visual);
        reader.update_in(visual, |r, window, cx| {
            let root = r.vault_root.clone();
            let app = waiting_source(r, "App", "mcp:app");
            let reads = || std::collections::BTreeMap::from([waiting_read(&app, Ok(vec!["q1"]))]);
            let (old, old_permit) = r.waiting.begin(&root, Default::default());
            r.vault_root = root.join("other-vault");
            assert!(
                !r.accept_waiting(&root, old, reads(), 1, "—".into(), cx),
                "a reply for the previous vault"
            );
            r.vault_root = root.clone();
            let (current, permit) = r.waiting.begin(&root, Default::default());
            assert!(
                !old_permit.allowed(),
                "a newer read revokes the older dispatch"
            );
            assert!(
                !r.accept_waiting(&root, old, reads(), 1, "—".into(), cx),
                "a superseded reply"
            );
            r.set_now_enabled(false, window, cx);
            assert!(!permit.allowed(), "disabling Now revokes the dispatch");
            assert!(!r.waiting.checking());
            assert!(
                !r.accept_waiting(&root, current, reads(), 1, "—".into(), cx),
                "a reply after Now was disabled"
            );
            assert!(r.waiting.view().rows.is_empty());
            assert_eq!(r.waiting.view().sources, 0);
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("now-heading-waiting").is_none());
    }

    /// #2346: a row opens only its exact, still-current manager binding.
    #[gpui::test]
    fn a_waiting_row_opens_only_its_current_manager_binding(cx: &mut TestAppContext) {
        let (reader, visual, _temp) = fixture(cx);
        ready_now(&reader, visual);
        visual.update(|_, cx| reader_features::enable_test_modules(cx));
        reader.update_in(visual, |r, window, cx| {
            let root = r.vault_root.clone();
            let app = waiting_source(r, "App", "mcp:app");
            r.project_navigation.discovery.projects = vec![okilum_t3::projects::Project {
                folder: app.folder.clone(),
                name: "App".into(),
                kind: okilum_t3::projects::Kind::Code,
                environment: Some(app.environment.clone()),
                manager_project: Some(app.project_id.clone()),
                manager: okilum_t3::projects::Manager::Thread(app.thread_id.clone()),
                appearance: Default::default(),
            }];
            let (generation, _) = r.waiting.begin(&root, Default::default());
            let reads = std::collections::BTreeMap::from([waiting_read(&app, Ok(vec!["q1"]))]);
            assert!(r.accept_waiting(&root, generation, reads, 1, "—".into(), cx));
            assert!(
                !r.open_waiting(&app, Some("q2"), window, cx),
                "unknown question"
            );
            let other = waiting_source(r, "App", "mcp:previous");
            assert!(
                !r.open_waiting(&other, Some("q1"), window, cx),
                "another thread"
            );
            let opened = r.project_navigation.generation;
            assert!(r.open_waiting(&app, Some("q1"), window, cx));
            assert_ne!(
                r.project_navigation.generation, opened,
                "the project entry pipeline started"
            );
            r.project_navigation.discovery.projects[0].manager =
                okilum_t3::projects::Manager::Thread("mcp:rebound".into());
            assert!(
                !r.open_waiting(&app, Some("q1"), window, cx),
                "a rebound manager no longer owns the question"
            );
        });
    }
}
