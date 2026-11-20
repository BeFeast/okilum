//! «Waiting for me» in Now (#2346): pending manager questions read from T3.
//! Rows come only from T3's pending-request list of each project's bound
//! manager thread, never from file times or activity. Reads run only while Now
//! is enabled, are dropped when the vault, Now or a newer read supersedes them,
//! and never write to the vault.
use super::*;
use okilum_t3::projects::waiting::{self, Ledger, Read, Source};
use std::collections::BTreeMap;

/// The accepted Waiting state of one vault root.
#[derive(Default)]
pub(super) struct Waiting {
    root: PathBuf,
    generation: u64,
    ledger: Ledger,
    /// The in-flight read's network permission; revoked on supersession.
    permit: Option<okilum_t3::NetworkAccess>,
    titles: BTreeMap<PathBuf, String>,
    times: BTreeMap<u64, String>,
}

impl Waiting {
    /// Start a read for `root`: any older read loses its permission and its
    /// reply will be refused. A different root starts from nothing.
    pub(super) fn begin(
        &mut self,
        root: &Path,
        titles: BTreeMap<PathBuf, String>,
    ) -> (u64, okilum_t3::NetworkAccess) {
        if self.root != root {
            self.cancel();
            self.root = root.to_owned();
        }
        if let Some(permit) = self.permit.take() {
            permit.revoke();
        }
        self.generation = self.generation.wrapping_add(1);
        let permit = okilum_t3::NetworkAccess::new(true);
        self.permit = Some(permit.clone());
        self.titles = titles;
        (self.generation, permit)
    }

    /// Now disabled, Agents off or another vault: stop dispatch, forget rows.
    pub(super) fn cancel(&mut self) {
        if let Some(permit) = self.permit.take() {
            permit.revoke();
        }
        self.generation = self.generation.wrapping_add(1);
        self.ledger = Ledger::default();
        self.titles.clear();
        self.times.clear();
    }

    pub(super) fn checking(&self) -> bool {
        self.permit.is_some()
    }

    pub(super) fn view(&self) -> waiting::View {
        self.ledger.view()
    }

    fn time(&self, at: u64) -> String {
        self.times.get(&at).cloned().unwrap_or_else(|| "—".into())
    }
}

/// One presented Waiting row; `open` names the exact target of a click.
struct WaitingRow {
    selector: &'static str,
    id: String,
    title: String,
    secondary: String,
    tooltip: String,
    warning: bool,
    open: Option<(Source, Option<String>)>,
}

impl Reader {
    /// One read of every bound manager's pending questions while Now is the
    /// shown view. Called on project discovery, Rescan and showing Now; there
    /// is no poller, and Tree or a disabled Now never contacts T3 for it.
    pub(super) fn refresh_waiting(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.single_file
            || !self.sidebar.now_enabled()
            || self.sidebar.left_mode != reader_sidebar::LeftMode::Now
            || !reader_features::enabled(reader_features::Feature::Agents, cx)
            || !self.project_navigation.discovered
        {
            return;
        }
        let projects = self.project_navigation.discovery.projects.clone();
        let root = self.vault_root.clone();
        let titles = projects
            .iter()
            .map(|project| {
                let path = project
                    .folder
                    .strip_prefix(&root)
                    .map(|path| path.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|_| project.name.clone());
                (
                    project.folder.clone(),
                    okilum_core::workbench::short_project_name(&path, &project.name),
                )
            })
            .collect();
        let sources = waiting::sources(&projects);
        let (generation, permit) = self.waiting.begin(&root, titles);
        cx.notify();
        if sources.is_empty() {
            let at = reader_sidebar::now();
            let label = reader_workbench::shown_time(at);
            self.accept_waiting(&root, generation, BTreeMap::new(), at, label, cx);
            return;
        }
        let state = self.session_directory.clone();
        cx.spawn_in(window, async move |this, cx| {
            let gateways = reader_project_navigation::t3_gateways(&this, state, cx)
                .await
                .unwrap_or_default();
            let (reads, at, label) = cx
                .background_executor()
                .spawn(async move {
                    let reads = waiting::read(&sources, &gateways, &permit);
                    let at = reader_sidebar::now();
                    (reads, at, reader_workbench::shown_time(at))
                })
                .await;
            let _ = this.update_in_own_window(cx, |this, _, cx| {
                this.accept_waiting(&root, generation, reads, at, label, cx);
            });
        })
        .detach();
    }

    /// Accept a read only for the root and generation that dispatched it,
    /// while Now is still enabled. Returns whether it was accepted.
    pub(super) fn accept_waiting(
        &mut self,
        root: &Path,
        generation: u64,
        reads: BTreeMap<PathBuf, Read>,
        at: u64,
        label: String,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.vault_root != root
            || self.waiting.root != root
            || self.waiting.generation != generation
            || !self.sidebar.now_enabled()
        {
            return false;
        }
        let waiting = &mut self.waiting;
        waiting.permit = None;
        waiting.ledger.apply(reads, at);
        waiting.times.insert(at, label);
        let view = waiting.ledger.view();
        let kept: std::collections::BTreeSet<u64> = view
            .rows
            .iter()
            .map(|row| row.observed_at)
            .chain(view.unavailable.iter().filter_map(|u| u.observed_at))
            .chain(view.checked_at)
            .collect();
        waiting.times.retain(|at, _| kept.contains(at));
        cx.notify();
        true
    }

    /// Open the project whose bound manager thread holds the question. A row
    /// whose question or manager binding is no longer current opens nothing.
    pub(super) fn open_waiting(
        &mut self,
        source: &Source,
        request: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.waiting.root != self.vault_root {
            return false;
        }
        if request.is_some_and(|id| !self.waiting.ledger.contains(source, id)) {
            return false;
        }
        let bound = waiting::sources(&self.project_navigation.discovery.projects)
            .into_iter()
            .any(|(_, current)| current.as_ref() == Ok(source));
        if !bound {
            self.refresh_waiting(window, cx);
            return false;
        }
        self.enter_project_folder(source.folder.clone(), window, cx);
        true
    }

    fn waiting_title(&self, folder: &Path) -> String {
        self.waiting.titles.get(folder).cloned().unwrap_or_else(|| {
            folder
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
    }

    fn waiting_location(&self, folder: &Path) -> String {
        folder
            .strip_prefix(&self.vault_root)
            .map(|path| path.to_string_lossy().replace(['/', '\\'], " › "))
            .unwrap_or_default()
    }

    /// Title and second line of a question row: the question as T3 states
    /// it, then its project and freshness.
    pub(super) fn waiting_question_lines(&self, row: &waiting::Row) -> (String, String) {
        let title = row
            .text
            .clone()
            .unwrap_or_else(|| "Question · text unavailable".to_owned());
        let project = self.waiting_title(&row.source.folder);
        let secondary = if row.stale {
            format!(
                "{project} · stale, last confirmed {}",
                self.waiting.time(row.observed_at)
            )
        } else {
            format!("{project} · open the manager thread")
        };
        (title, secondary)
    }

    fn waiting_row(&self, row: WaitingRow, cx: &mut Context<Self>) -> AnyElement {
        let WaitingRow {
            selector,
            id,
            title,
            secondary,
            tooltip,
            warning,
            open,
        } = row;
        let tokens = brand::palette(cx);
        let hover = brand::reader_palette(cx).hover;
        let reader = cx.entity().downgrade();
        v_flex()
            .id(SharedString::from(id))
            .debug_selector(move || selector.into())
            .w_full()
            .min_w_0()
            .px_3()
            .py_1()
            .rounded(px(3.))
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
            })
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1p5()
                    .child(
                        div()
                            .size(px(6.))
                            .flex_none()
                            .rounded_full()
                            .bg(if warning {
                                tokens.warning
                            } else {
                                tokens.accent
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(title),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(px(10.))
                    .text_color(if warning {
                        tokens.warning
                    } else {
                        tokens.text_muted
                    })
                    .child(secondary),
            )
            .when_some(open, |row, (source, request)| {
                row.cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .on_click(move |_, window, cx| {
                        let _ = reader.update(cx, |this, cx| {
                            this.open_waiting(&source, request.as_deref(), window, cx);
                        });
                    })
            })
            .into_any_element()
    }

    /// The Waiting for me section, or nothing before the first read of this
    /// vault. Render performs no IO and no formatting of times.
    pub(super) fn render_now_waiting(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let waiting = &self.waiting;
        if waiting.root != self.vault_root || (waiting.ledger.is_empty() && !waiting.checking()) {
            return None;
        }
        let view = waiting.view();
        let mut status = Vec::new();
        if waiting.checking() {
            status.push("Checking managers…".to_owned());
        }
        if view.partial() {
            status.push(format!(
                "{} of {} sources unavailable",
                view.unavailable.len(),
                view.sources
            ));
        }
        if let Some(at) = view.checked_at {
            status.push(if view.rows.is_empty() && !view.partial() {
                format!("Nothing waiting · checked {}", waiting.time(at))
            } else {
                format!("checked {}", waiting.time(at))
            });
        } else if !waiting.checking() && view.sources == 0 {
            status.push("No project has a manager thread".to_owned());
        }
        let tokens = brand::palette(cx);
        let mut body = v_flex().w_full().min_w_0().pb_1().child(
            div()
                .debug_selector(|| "now-waiting-status".into())
                .px_3()
                .pb_1()
                .text_size(px(10.))
                .text_color(tokens.text_muted)
                .child(status.join(" · ")),
        );
        for row in &view.rows {
            let selector = if row.stale {
                "now-waiting-stale-row"
            } else {
                "now-waiting-row"
            };
            let (title, secondary) = self.waiting_question_lines(row);
            let tooltip = format!(
                "{}\n{}\nManager thread {} · T3 {} / {}\nPending request {}",
                title,
                self.waiting_location(&row.source.folder),
                row.source.thread_id,
                row.source.environment,
                row.source.project_id,
                row.request_id,
            );
            body = body.child(self.waiting_row(
                WaitingRow {
                    selector,
                    id: format!(
                        "now-waiting-{}-{}",
                        row.source.folder.display(),
                        row.request_id
                    ),
                    title,
                    secondary,
                    tooltip,
                    warning: false,
                    open: Some((row.source.clone(), Some(row.request_id.clone()))),
                },
                cx,
            ));
        }
        for (source, count) in &view.omitted {
            body = body.child(self.waiting_row(
                WaitingRow {
                    selector: "now-waiting-omitted",
                    id: format!("now-waiting-omitted-{}", source.folder.display()),
                    title: format!("+{count} more in {}", self.waiting_title(&source.folder)),
                    secondary: "Open the manager thread for the rest".into(),
                    tooltip: self.waiting_location(&source.folder),
                    warning: false,
                    open: Some((source.clone(), None)),
                },
                cx,
            ));
        }
        for unavailable in &view.unavailable {
            let secondary = match unavailable.observed_at {
                Some(at) => format!(
                    "Unavailable · last confirmed {} · {}",
                    waiting.time(at),
                    unavailable.problem
                ),
                None => format!("Unavailable · {}", unavailable.problem),
            };
            body = body.child(self.waiting_row(
                WaitingRow {
                    selector: "now-waiting-unavailable",
                    id: format!("now-waiting-unavailable-{}", unavailable.folder.display()),
                    title: self.waiting_title(&unavailable.folder),
                    secondary,
                    tooltip: format!(
                        "{}\n{}",
                        self.waiting_location(&unavailable.folder),
                        unavailable.problem
                    ),
                    warning: true,
                    open: None,
                },
                cx,
            ));
        }
        let count = view.rows.len() + view.omitted.iter().map(|(_, n)| n).sum::<usize>();
        Some(
            v_flex()
                .w_full()
                .min_w_0()
                .flex_none()
                .child(self.now_heading(
                    "waiting".into(),
                    "WAITING FOR ME".into(),
                    count,
                    false,
                    cx,
                ))
                .when(!self.sidebar.now_collapsed.contains("waiting"), |section| {
                    section.child(
                        div()
                            .id("now-waiting-scroll")
                            .min_h_0()
                            .max_h(px(180.))
                            .overflow_y_scroll()
                            .child(body),
                    )
                })
                .into_any_element(),
        )
    }
}
