//! Window-local empty-column presentation. Source refresh never clears the
//! last completed emptiness decision or the user's explicit expansion.
use super::*;

#[derive(Default)]
pub(super) struct State {
    empty: [bool; 5],
    expanded: [bool; 5],
}
impl State {
    pub(super) fn invalidate(&mut self) {
        self.empty = [false; 5];
    }
    pub(super) fn observe(&mut self, counts: [usize; 5], complete: bool) {
        if complete {
            self.empty = counts.map(|count| count == 0);
        }
    }
    pub(super) fn known_empty(&self, column: Column) -> bool {
        self.empty[column as usize]
    }
    pub(super) fn collapsed(&self, column: Column, count: usize) -> bool {
        count == 0 && self.known_empty(column) && !self.expanded[column as usize]
    }
}

impl Board {
    pub(super) fn column_counts(&self, binding: &Binding) -> [usize; 5] {
        let mut counts = [0; 5];
        for scope in &binding.issues {
            if self
                .filter
                .as_ref()
                .is_none_or(|filter| filter == &scope.executor)
            {
                counts[self.delivery_column(scope) as usize] += 1;
            }
        }
        if self.filter.is_none() {
            for card in &self.local_cards.value.cards {
                counts[card.state.column() as usize] += 1;
            }
        }
        counts[Column::Queue as usize] += binding.card_problems.len();
        counts
    }

    pub(super) fn column_sources_ready(&self, binding: &Binding) -> bool {
        !self.binding_loading
            && !self.local_loading
            && !self.snapshot.busy()
            // Delivery follows Forgejo and can overlap the next poll. Its
            // retained completed snapshot still determines the visible columns;
            // requiring an idle delivery reader can starve the first collapse.
            // The config success/error gate below still rejects initial unknowns.
            && self.local_cards.last_success.is_some()
            && self.local_cards.error.is_none()
            && self.local_cards.value.problems.is_empty()
            // Even an annotation-free repository Board must finish its first
            // live read. A failed discovery is not evidence of zero issues.
            && (binding.repository.is_empty()
                || (self.snapshot.forgejo.last_success.is_some()
                    && self.snapshot.forgejo.error.is_none()))
            && (binding.issues.is_empty()
                || (self.snapshot.forgejo.last_success.is_some()
                    && self.snapshot.forgejo.error.is_none()
                    && self.delivery.config.last_success.is_some()
                    && self.delivery.config.error.is_none()
                    && (!binding
                        .issues
                        .iter()
                        .any(|scope| live::thread_id(scope).is_some())
                        || (self.snapshot.t3.last_success.is_some()
                            && self.snapshot.t3.error.is_none()))))
    }

    pub(super) fn expand_empty_column(
        &self,
        column: Column,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (color, glyph) = appearance::stage(column, cx);
        Button::new(("board-expand-empty", column as usize))
            .debug_selector(move || format!("board-expand-empty-{column:?}"))
            .ghost()
            .small()
            .accessibility_label(format!("Expand empty {} column, 0 cards", column.label()))
            .tooltip(format!("Expand {}", column.label()))
            .child(appearance::icon(glyph, color))
            .label(format!("{} 0", column.label()))
            .child(appearance::icon("chevron-right", color))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.columns.expanded[column as usize] = true;
                cx.notify();
            }))
    }

    pub(super) fn collapse_empty_column(
        &self,
        column: Column,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Button::new(("board-collapse-empty", column as usize))
            .debug_selector(move || format!("board-collapse-empty-{column:?}"))
            .ghost()
            .small()
            .accessibility_label(format!("Collapse empty {} column", column.label()))
            .tooltip(format!("Collapse empty {}", column.label()))
            .child(appearance::icon(
                "chevron-left",
                brand::palette(cx).text_muted,
            ))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.columns.expanded[column as usize] = false;
                if column == Column::Verified {
                    this.verified_open = false;
                }
                cx.notify();
            }))
    }

    pub(super) fn stage_scopes<'a>(
        &self,
        binding: &'a Binding,
        column: Column,
    ) -> Vec<&'a okilum_t3::board::Scope> {
        let filtered = binding.issues.iter().filter(|scope| {
            self.filter.as_ref().is_none_or(|f| f == &scope.executor)
                && self.delivery_column(scope) == column
        });
        okilum_t3::board::sorted_scope_refs(filtered, &self.snapshot.forgejo.value)
    }

    pub(super) fn queue_order_hint(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("board-queue-order-hint")
            .debug_selector(|| "board-queue-order-hint".into())
            .ml_auto()
            .min_w_0()
            .truncate()
            .text_size(px(12.))
            .text_color(brand::palette(cx).text_muted)
            .child(okilum_t3::board::QUEUE_ORDER_HINT)
    }

    pub(super) fn add_card_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        Button::new("board-local-add")
            .debug_selector(|| "board-local-add".into())
            .ghost()
            .small()
            .label("Add card")
            .disabled(self.editor_loading)
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_local_management(local::Mode::Add, window, cx)
            }))
    }

    #[cfg(feature = "project-ui-harness")]
    pub(in crate::project) fn order_fixture(
        area: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut board = Self::with_reader(area, okilum_t3::board::live::read, window, cx);
        let mut binding = Binding::parse(
            "version: 1\ntitle: Okilum Board v2\nrepository: https://git.oklabs.uk/BeFeast/okilum\nissues:\n  - {number: 2490, expected: Document Board v2 stage ordering rules, executor: Docs}\n  - {number: 2512, expected: Handle empty stage note path without warning, executor: Core}\n  - {number: 2518, expected: Fix column scroll anchor after live refresh, executor: UI}\n  - {number: 2520, expected: Preserve selected card when switching Board filter, executor: UI}\n  - {number: 2531, expected: Compact two-line cards in Board view without prose, executor: Board}\n  - {number: 2535, expected: Order issues inside each stage — from Oleg first, executor: Board}\n  - {number: 2524, expected: Keep narrow Manager splitter aligned beside Board, executor: UI}\n  - {number: 2532, expected: Stage strip in header and single-stage focus mode, executor: Board, agent_thread_id: thread-2532}\n  - {number: 2530, expected: List view as default with stages as collapsible groups, executor: Board, agent_thread_id: thread-2530}\n  - {number: 2515, expected: Avoid duplicate ETag revalidation on quick tab switch, executor: Core}\n  - {number: 2533, expected: Epics view with progress bar and collapsible child issue list, executor: Board}\n",
        )
        .expect("order fixture binding");
        for scope in &mut binding.issues {
            if matches!(scope.number, 2530 | 2532) {
                scope.current_task = true;
            }
        }
        let make_issue = |number: u64,
                          title: &str,
                          labels: &[&str],
                          updated_at: &str,
                          pull: Option<(u64, &str)>| {
            let mut issue: okilum_t3::board::Issue = serde_json::from_value(serde_json::json!({
                "number": number,
                "title": title,
                "state": "open",
                "updated_at": updated_at,
                "labels": labels.iter().map(|name| serde_json::json!({"name": name})).collect::<Vec<_>>(),
            }))
            .expect("order fixture issue");
            if let Some((pr, checks)) = pull {
                issue.open_pull_requests.push(pr);
                issue.live_pulls.push(okilum_t3::board::live::Pull {
                    number: pr,
                    merged_at: None,
                    checks: checks.into(),
                });
            }
            (number, issue)
        };
        let stamp = now();
        board.binding = Some(binding);
        board.local_loading = false;
        board.local_cards.last_success = Some(stamp);
        board.delivery.config.accept(Ok(None), stamp);
        board.snapshot.t3.accept(
            Ok(vec![
                serde_json::from_value(serde_json::json!({
                    "threadId": "thread-2530",
                    "title": "Board list view",
                    "status": "running",
                    "activeRunId": "run-2530",
                    "latestRunId": "run-2530",
                    "providerInstanceId": "codex",
                    "model": "gpt-6.1-sol",
                    "runtimeMode": "full-access",
                    "updatedAt": "2026-11-03T20:25:00Z"
                }))
                .unwrap(),
                serde_json::from_value(serde_json::json!({
                    "threadId": "thread-2532",
                    "title": "Board stage strip",
                    "status": "running",
                    "activeRunId": "run-2532",
                    "latestRunId": "run-2532",
                    "providerInstanceId": "codex",
                    "model": "gpt-6.1-sol",
                    "runtimeMode": "full-access",
                    "updatedAt": "2026-11-03T20:28:00Z"
                }))
                .unwrap(),
            ]),
            stamp,
        );
        board.snapshot.forgejo.accept(
            Ok([
                make_issue(
                    2490,
                    "Document Board v2 stage ordering rules",
                    &["task"],
                    "2026-11-03T18:00:00Z",
                    None,
                ),
                make_issue(
                    2512,
                    "Handle empty stage note path without warning",
                    &["bug"],
                    "2026-11-03T18:30:00Z",
                    None,
                ),
                make_issue(
                    2518,
                    "Fix column scroll anchor after live refresh",
                    &["bug"],
                    "2026-11-03T19:45:00Z",
                    None,
                ),
                make_issue(
                    2520,
                    "Preserve selected card when switching Board filter",
                    &["from-tester", "bug"],
                    "2026-11-03T19:15:00Z",
                    None,
                ),
                make_issue(
                    2531,
                    "Compact two-line cards in Board view without prose",
                    &["from-oleg", "fr"],
                    "2026-11-03T20:00:00Z",
                    None,
                ),
                make_issue(
                    2535,
                    "Order issues inside each stage — from Oleg first",
                    &["from-oleg", "fr"],
                    "2026-11-03T20:00:00Z",
                    None,
                ),
                make_issue(
                    2524,
                    "Keep narrow Manager splitter aligned beside Board",
                    &["from-tester", "bug"],
                    "2026-11-03T20:20:00Z",
                    Some((2539, "success")),
                ),
                make_issue(
                    2532,
                    "Stage strip in header and single-stage focus mode",
                    &["from-oleg", "fr"],
                    "2026-11-03T20:15:00Z",
                    None,
                ),
                make_issue(
                    2530,
                    "List view as default with stages as collapsible groups",
                    &["from-oleg", "fr"],
                    "2026-11-03T19:50:00Z",
                    Some((2540, "success")),
                ),
                make_issue(
                    2515,
                    "Avoid duplicate ETag revalidation on quick tab switch",
                    &["bug"],
                    "2026-11-03T20:10:00Z",
                    Some((2538, "pending")),
                ),
                make_issue(
                    2533,
                    "Epics view with progress bar and collapsible child issue list",
                    &["from-oleg", "fr"],
                    "2026-11-03T19:30:00Z",
                    Some((2541, "pending")),
                ),
            ]
            .into()),
            stamp,
        );
        board
    }
}

#[cfg(test)]
mod tests;
