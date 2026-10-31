//! The Manager's primary surface is a conversation. Operational detail is opt-in.
use super::*;

impl ProjectView {
    pub(super) fn render_chat_header(
        &self,
        status: &GatewayStatus,
        open: Option<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut row = h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .gap_1()
            .debug_selector(|| "project-chat-header".into());
        #[cfg(target_os = "linux")]
        {
            row = row.child(div().flex_1().min_w_0().child(div().truncate().child(
                if self.native.title().is_empty() {
                    "Manager".to_owned()
                } else {
                    self.native.title().to_owned()
                },
            )));
        }
        #[cfg(not(target_os = "linux"))]
        {
            row = row.child(div().flex_1().child("Manager"));
        }
        if let Some(target) = self.manager_model_target() {
            row = row.child(self.model_picker("project-manager-model", target, cx));
        }
        #[cfg(target_os = "linux")]
        if !self.native.active() && !self.threads.enabled {
            row = row.child(
                Button::new("project-pair")
                    .debug_selector(|| "project-pair".into())
                    .small()
                    .label("Pair")
                    .disabled(self.native_pairing_busy())
                    .on_click(cx.listener(|this, _, window, cx| this.pair_manager(window, cx))),
            );
        }
        row = row.child(self.connection_line(status, cx));
        let can_open = open.is_some();
        #[cfg(target_os = "linux")]
        let can_open = can_open || (self.native.active() && self.chat.is_some());
        if can_open {
            row = row.child(
                Button::new("project-open-t3")
                    .debug_selector(|| "project-open-t3".into())
                    .ghost()
                    .small()
                    .icon(IconName::ExternalLink)
                    .tooltip("Open in T3")
                    .disabled(self.opening_t3)
                    .on_click(cx.listener(|this, _, window, cx| this.open_in_t3(window, cx))),
            );
        }
        row.into_any_element()
    }

    pub(super) fn render_chat_diagnostics(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut panel = v_flex()
            .gap_2()
            .debug_selector(|| "project-diagnostics".into())
            .child(
                h_flex().child("Diagnostics").child(div().flex_1()).child(
                    Button::new("project-diagnostics-close")
                        .debug_selector(|| "project-diagnostics-close".into())
                        .ghost()
                        .small()
                        .icon(IconName::Close)
                        .tooltip("Close diagnostics")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.connection_details = false;
                            cx.notify();
                        })),
                ),
            );
        if let Some(target) = &self.model_target {
            panel = panel.child(format!("Thread: {}", target.thread_id));
        }
        if let Some(binding) = &self.binding {
            panel = panel.child(format!(
                "Bound thread: {}",
                binding.manager.thread_id.as_deref().unwrap_or("none")
            ));
        }
        if let Some(snapshot) = &self.models {
            panel = panel.child(format!(
                "Model: {} / {}; thread mode: {}; credential mode: {}",
                snapshot.configuration.model_selection.instance_id,
                snapshot.configuration.model_selection.model,
                snapshot.configuration.runtime_mode,
                snapshot.catalog.runtime_mode
            ));
        }
        if let Some(error) = &self.model_error {
            panel = panel.child(error.clone());
        }
        panel
            .child(
                div()
                    .debug_selector(|| "project-model-scheduler-limit".into())
                    .child(okilum_t3::models::SCHEDULED_TASK_LIMIT),
            )
            .into_any_element()
    }
}
