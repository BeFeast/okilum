//! The Manager's primary surface is a conversation. Operational detail is opt-in.
use super::*;

impl ProjectView {
    pub(super) fn render_chat_header(
        &self,
        status: &GatewayStatus,
        open: Option<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Keep conversation identity separate from configuration: a long model
        // name must not squeeze the title or push connection actions offscreen.
        let mut heading = h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .flex_wrap()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w(px(72.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .debug_selector(|| "project-manager-title".into())
                    .child("Manager"),
            );
        let mut actions = h_flex()
            .items_center()
            .gap_1()
            .child(self.connection_line(status, cx));
        let mut controls = h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .flex_wrap()
            .gap_2();
        let mut has_controls = false;
        if let Some(target) = self.manager_model_target() {
            controls = controls.child(self.model_picker("project-manager-model", target, cx));
            has_controls = true;
        }
        #[cfg(target_os = "linux")]
        if !self.native.active() {
            controls = controls.child(
                Button::new("project-pair")
                    .debug_selector(|| "project-pair".into())
                    .ghost()
                    .small()
                    .label("Pair")
                    .disabled(self.native_pairing_busy())
                    .on_click(cx.listener(|this, _, window, cx| this.pair_manager(window, cx))),
            );
            has_controls = true;
        }
        let can_open = open.is_some();
        #[cfg(target_os = "linux")]
        let can_open = can_open || (self.native.active() && self.chat.is_some());
        if can_open {
            actions = actions.child(
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
        heading = heading.child(actions);
        let header = v_flex()
            .w_full()
            .min_w_0()
            .gap_1()
            .debug_selector(|| "project-chat-header".into())
            .child(heading);
        if has_controls {
            header.child(controls).into_any_element()
        } else {
            header.into_any_element()
        }
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
        #[cfg(target_os = "linux")]
        if !self.native.title().is_empty() {
            panel = panel.child(format!("Title: {}", self.native.title()));
        }
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
