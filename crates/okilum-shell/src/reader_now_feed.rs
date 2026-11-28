//! Unified cross-area feed in Now (#3296).
//! Snapshot reads run on the background executor. Render only draws the
//! accepted [`okilum_core::now_feed::NowFeed`].
mod actions;
use super::*;
use okilum_core::now_feed::{
    self, FeedOpen, LoadOutcome, PresentedGroup, PresentedItem, PresentedRow,
};
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct Cache {
    root: PathBuf,
    generation: u64,
    inflight: bool,
    phase: Phase,
    areas: BTreeMap<String, String>,
    role_names: BTreeMap<String, String>,
    stamp: String,
    targets: okilum_core::now_actions::Targets,
    actions: actions::Actions,
}

#[derive(Clone, Debug, Default)]
enum Phase {
    #[default]
    Idle,
    Absent,
    Invalid(String),
    Ready(now_feed::NowFeed),
}

impl Cache {
    fn covers(&self, root: &Path) -> bool {
        self.root == root && (self.inflight || !matches!(self.phase, Phase::Idle))
    }

    fn begin(&mut self, root: &Path) -> u64 {
        if self.root != root {
            self.phase = Phase::Idle;
            self.areas.clear();
            self.role_names.clear();
            self.stamp.clear();
            self.targets = Default::default();
            self.actions = Default::default();
            self.root = root.to_owned();
        }
        self.inflight = true;
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    fn accept(
        &mut self,
        root: &Path,
        generation: u64,
        loaded: now_feed::LoadedSnapshot,
        targets: okilum_core::now_actions::Targets,
    ) -> bool {
        if self.root != root || self.generation != generation {
            return false;
        }
        self.inflight = false;
        self.areas = loaded.areas;
        self.role_names = loaded.role_names;
        self.stamp = loaded.stamp;
        self.targets = targets;
        if !matches!((&self.phase, &loaded.outcome),
            (Phase::Ready(old), LoadOutcome::Ready(new)) if old == new)
        {
            self.actions.accept_snapshot();
        }
        self.phase = match loaded.outcome {
            LoadOutcome::Absent => Phase::Absent,
            LoadOutcome::Invalid(error) => Phase::Invalid(error),
            LoadOutcome::Ready(feed) => Phase::Ready(feed),
        };
        true
    }
}

impl Reader {
    /// Start a snapshot read when Now is showing and this root has no result yet.
    pub(super) fn ensure_now_feed(&mut self, cx: &mut Context<Self>) {
        self.start_now_feed(false, cx);
    }

    /// Vault load, Rescan, switching to Now, or a watch event for the snapshot.
    pub(super) fn refresh_now_feed(&mut self, cx: &mut Context<Self>) {
        self.start_now_feed(true, cx);
    }

    fn start_now_feed(&mut self, force: bool, cx: &mut Context<Self>) {
        if self.single_file
            || self.vault_root.as_os_str().is_empty()
            || !self.sidebar.now_enabled()
            || self.sidebar.left_mode != reader_sidebar::LeftMode::Now
        {
            return;
        }
        if !force && self.now_feed.covers(&self.vault_root) {
            return;
        }
        let generation = self.now_feed.begin(&self.vault_root);
        let root = self.vault_root.clone();
        cx.spawn(async move |this, cx| {
            let load_root = root.clone();
            let (loaded, targets) = cx
                .background_executor()
                .spawn(async move {
                    let loaded = now_feed::load_presentation(&load_root);
                    let targets =
                        okilum_core::now_actions::load_targets(&load_root, &loaded.outcome);
                    (loaded, targets)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.now_feed.accept(&root, generation, loaded, targets) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn now_feed_ready(&self) -> bool {
        self.now_feed.root == self.vault_root && matches!(self.now_feed.phase, Phase::Ready(_))
    }

    pub(super) fn now_feed_warning(&self) -> Option<String> {
        match &self.now_feed.phase {
            Phase::Invalid(error) if self.now_feed.root == self.vault_root => Some(error.clone()),
            _ => None,
        }
    }

    pub(super) fn render_now_feed(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Phase::Ready(feed) = &self.now_feed.phase else {
            return div().into_any_element();
        };
        let feed = feed.without_answered_cases(&self.now_feed.actions.answered);
        let (questions, problems) = self.feed_live();
        let presented = now_feed::build_feed(
            &feed,
            &questions,
            &problems,
            &self.now_feed.areas,
            &self.now_feed.role_names,
        );
        let tokens = brand::palette(cx);
        let stamp = if self.now_feed.stamp.is_empty() {
            "—".to_owned()
        } else {
            self.now_feed.stamp.clone()
        };
        let mut body = v_flex().w_full().min_w_0().pb_2();
        if let Some(banner) = &presented.warnings {
            body = body.child(self.feed_banner(
                "now-feed-warnings",
                banner.summary.clone(),
                banner.tooltip.clone(),
                true,
                cx,
            ));
        }
        if let Some(banner) = &presented.banner {
            body = body.child(self.feed_banner(
                "now-feed-t3",
                banner.summary.clone(),
                banner.tooltip.clone(),
                true,
                cx,
            ));
        }
        for group in &presented.groups {
            body = body.child(self.feed_group(group, window, cx));
        }
        let reader = cx.entity().downgrade();
        let shortcut = if cfg!(target_os = "macos") {
            "⌘⇧\\"
        } else {
            "Ctrl+Shift+\\"
        };
        v_flex()
            .id("now-sidebar")
            .debug_selector(|| "now-feed".into())
            .w_full()
            .h_full()
            .min_h_0()
            .text_size(px(brand::READER_CHROME_FONT_SIZE))
            .child(self.now_toolbar(format!("Feed · snapshot {stamp}"), true, cx))
            .child(
                div()
                    .id("now-feed-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            )
            .child(
                h_flex()
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
                    .child("Tree")
                    .child(div().flex_1())
                    .child(shortcut)
                    .on_click(move |_, window, cx| {
                        let _ = reader.update(cx, |this, cx| {
                            this.set_left_mode(reader_sidebar::LeftMode::Tree, window, cx)
                        });
                    }),
            )
            .into_any_element()
    }

    pub(super) fn render_now_feed_warning(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let error = self.now_feed_warning()?;
        let line = error.chars().take(120).collect::<String>();
        Some(self.feed_banner(
            "now-feed-invalid",
            format!("Feed snapshot: {line}"),
            error,
            true,
            cx,
        ))
    }

    fn feed_group(
        &self,
        group: &PresentedGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = format!("feed-{}", group.id);
        let open = !self.sidebar.now_collapsed.contains(&key);
        let mut section = v_flex().w_full().min_w_0().child(self.now_heading(
            key,
            group.title.to_owned(),
            group.count,
            false,
            cx,
        ));
        if !open {
            return section.into_any_element();
        }
        if group.id == "available" {
            section = section.child(self.now_intention_form(window, cx));
        }
        for block in &group.sections {
            if let Some(title) = block.title {
                section = section.child(
                    div()
                        .px_3()
                        .h(px(22.))
                        .text_size(px(10.))
                        .text_color(brand::palette(cx).text_muted)
                        .child(title.to_owned()),
                );
            }
            for row in &block.rows {
                section = section.child(self.feed_presented_row(row, window, cx));
            }
        }
        section = section.child(self.feed_hidden(group, window, cx));
        section.into_any_element()
    }

    fn feed_hidden(
        &self,
        group: &PresentedGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if group.id != "waiting" || group.hidden_count == 0 {
            return div().into_any_element();
        }
        let key = "feed-waiting-hidden";
        let expanded = self.sidebar.now_expanded.contains(key);
        if expanded && !group.hidden.is_empty() {
            let reader = cx.entity().downgrade();
            let mut list = v_flex().w_full().min_w_0();
            for item in &group.hidden {
                list = list.child(self.feed_item(item, "now-feed-hidden", window, cx));
            }
            list = list.child(
                div()
                    .id("now-feed-show-less")
                    .debug_selector(|| "now-feed-show-less".into())
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .text_size(px(11.))
                    .text_color(brand::palette(cx).text_muted)
                    .on_click(move |_, _, cx| {
                        let _ = reader.update(cx, |this, cx| {
                            this.toggle_now_section(key, true, cx);
                        });
                    })
                    .child("Show less"),
            );
            return list.into_any_element();
        }
        let count = group.hidden_count;
        let expandable = !group.hidden.is_empty();
        let reader = cx.entity().downgrade();
        let label = format!("+{count} more");
        div()
            .id("now-feed-hidden-count")
            .debug_selector(|| "now-feed-hidden-count".into())
            .px_3()
            .py_1()
            .text_size(px(11.))
            .text_color(brand::palette(cx).text_muted)
            .when(expandable, |row| {
                row.cursor_pointer().on_click(move |_, _, cx| {
                    let _ = reader.update(cx, |this, cx| {
                        this.toggle_now_section(key, true, cx);
                    });
                })
            })
            .child(label)
            .into_any_element()
    }

    fn feed_presented_row(
        &self,
        row: &PresentedRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match row {
            PresentedRow::Item(item) => self.feed_item(item, "now-feed-row", window, cx),
            PresentedRow::Fold {
                project,
                count,
                items,
            } => {
                let key = format!("feed-fold-{project}");
                let open = self.sidebar.now_expanded.contains(&key);
                let reader = cx.entity().downgrade();
                let toggle_key = key.clone();
                let hover = brand::reader_palette(cx).hover;
                let mut block = v_flex().w_full().min_w_0().child(
                    div()
                        .id(SharedString::from(format!("now-feed-fold-{project}")))
                        .debug_selector(|| "now-feed-project".into())
                        .w_full()
                        .px_3()
                        .py_1()
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .on_click(move |_, _, cx| {
                            let key = toggle_key.clone();
                            let _ = reader.update(cx, |this, cx| {
                                this.toggle_now_section(&key, true, cx);
                            });
                        })
                        .child(format!("{project}: {count} items")),
                );
                if open {
                    for item in items {
                        block = block.child(self.feed_item(item, "now-feed-row", window, cx));
                    }
                }
                block.into_any_element()
            }
        }
    }

    fn feed_item(
        &self,
        item: &PresentedItem,
        selector: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = brand::palette(cx);
        let hover = brand::reader_palette(cx).hover;
        let open = item.open.clone();
        let reader = cx.entity().downgrade();
        let tooltip = item.tooltip.clone();
        let control = match &item.open {
            FeedOpen::Live(id) => self
                .waiting
                .view()
                .rows
                .into_iter()
                .find(|row| reader_now_waiting::feed_id(row) == *id)
                .and_then(|row| self.now_live_form(row, window, cx)),
            FeedOpen::Note(_) => item
                .case_reply_path
                .as_deref()
                .and_then(|path| self.now_case_form(path, &item.id, window, cx)),
        };
        let row = v_flex()
            .id(SharedString::from(format!("now-feed-item-{}", item.id)))
            .debug_selector(move || selector.into())
            .w_full()
            .min_w_0()
            .px_3()
            .py_1()
            .rounded(px(3.))
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
            })
            .on_click(move |_, window, cx| {
                let open = open.clone();
                let _ = reader.update(cx, |this, cx| this.open_feed(&open, window, cx));
            })
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(item.title.clone()),
            )
            .when(!item.secondary.is_empty(), |row| {
                let secondary = item.secondary.clone();
                row.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_size(px(10.))
                        .text_color(tokens.text_muted)
                        .child(secondary),
                )
            });
        v_flex()
            .child(row)
            .children(control.map(|form| div().px_3().pb_1().child(form)))
            .into_any_element()
    }

    fn feed_banner(
        &self,
        selector: &'static str,
        line: String,
        tooltip: String,
        warning: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let color = if warning {
            brand::palette(cx).warning
        } else {
            brand::palette(cx).text_muted
        };
        h_flex()
            .id(SharedString::from(selector))
            .debug_selector(move || selector.into())
            .w_full()
            .px_3()
            .py_1()
            .text_size(px(11.))
            .text_color(color)
            .overflow_hidden()
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
            })
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(line),
            )
            .into_any_element()
    }

    fn open_feed(&mut self, open: &FeedOpen, window: &mut Window, cx: &mut Context<Self>) {
        match open {
            FeedOpen::Note(path) => self.open_note(path, None, window, cx),
            FeedOpen::Live(id) => {
                let view = self.waiting.view();
                let Some(row) = view
                    .rows
                    .iter()
                    .find(|row| reader_now_waiting::feed_id(row) == *id)
                else {
                    return;
                };
                let source = row.source.clone();
                let request = row.request_id.clone();
                self.open_waiting(&source, Some(&request), window, cx);
            }
        }
    }
}
