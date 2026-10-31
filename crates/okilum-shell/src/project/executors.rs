use super::*;
use okilum_t3::ThreadSummary;

impl ProjectView {
    pub(super) fn render_executors(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut list = v_flex()
            .id("project-executors-list")
            .debug_selector(|| "project-executors-list".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap_4();
        if let Some(error) = &self.executors_error {
            list = list.child(div().text_color(cx.theme().danger).child(format!(
                "Cannot refresh executors; previous rows are retained. {error}"
            )));
        }
        let Some(snapshot) = &self.executors else {
            return list.child("Executor status has not loaded. Connect to T3; this view refreshes with manager polling.").into_any_element();
        };
        list = list.child(div().child(format!(
            "Manager · {} registered executors",
            snapshot.registered.len()
        )));
        for (index, row) in snapshot.registered.iter().enumerate() {
            let mut entry = v_flex()
                .gap_1()
                .pl_4()
                .debug_selector(move || format!("project-executor-{index}"))
                .child(div().child(row.entry.role.clone()));
            if let Some(thread) = &row.thread {
                entry = entry.child(self.executor_thread(thread, index, cx));
                if let Some(binding) = &self.binding {
                    let target = okilum_t3::models::Target {
                        thread_id: thread.thread_id.clone(),
                        project_id: binding.executor.project_id.clone(),
                    };
                    entry = entry.child(self.model_picker(("executor-model", index), target, cx));
                }
                entry = entry.child(match row.pending_questions {
                    Some(0) => "No pending questions".to_owned(),
                    Some(count) => format!("Question in T3 · {count} pending"),
                    None => format!(
                        "Question status unavailable: {}",
                        row.question_error.as_deref().unwrap_or("not checked")
                    ),
                });
            } else {
                entry = entry.child(div().text_color(cx.theme().danger).child(format!(
                    "{} · missing from the bound T3 project",
                    row.entry.thread_id
                )));
            }
            list = list.child(entry);
        }
        if !snapshot.unregistered.is_empty() {
            list = list.child(div().child("T3 threads not named in this area's registry"));
            for (offset, thread) in snapshot.unregistered.iter().enumerate() {
                list = list.child(
                    v_flex()
                        .gap_1()
                        .child(self.executor_thread(thread, snapshot.registered.len() + offset, cx))
                        .child("Ownership not established · question status not checked"),
                );
            }
        }
        list.into_any_element()
    }

    fn executor_thread(
        &self,
        thread: &ThreadSummary,
        index: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        let mut row = v_flex()
            .id(("executor-thread", index))
            .gap_1()
            .child(thread.title.clone())
            .child(
                div()
                    .text_color(brand::palette(cx).text_muted)
                    .child(format!("{} · {} items", thread.status, thread.item_count)),
            );
        let mut actions = h_flex().gap_2();
        if let Some(identity) = &self.identity {
            let url = identity.thread_url(&thread.thread_id);
            actions = actions.child(
                Button::new(("executor-open-t3", index))
                    .ghost()
                    .label("Open in T3")
                    .icon(IconName::ExternalLink)
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            );
        }
        for (pr, url) in thread.pull_request_urls().into_iter().enumerate() {
            if okilum_t3::is_http_link(url) {
                let url = url.to_owned();
                actions = actions.child(
                    Button::new(("executor-open-pr", pr))
                        .ghost()
                        .label(format!("PR {}", pr + 1))
                        .tooltip(url.clone())
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                );
            } else {
                actions = actions.child("PR link unavailable: unsupported URL");
            }
        }
        row = row.child(actions);
        row.into_any_element()
    }
}
