//! The Manager's primary surface is a conversation. Operational detail is opt-in.
use super::*;

impl ProjectView {
    pub(super) fn render_chrome(&self, cx: &mut Context<Self>) -> AnyElement {
        let status = self
            .gateway
            .as_ref()
            .map(|gateway| gateway.status())
            .unwrap_or(GatewayStatus::NotConnected);
        let open = self
            .identity
            .as_ref()
            .zip(self.chat.as_ref())
            .map(|(identity, chat)| identity.thread_url(&chat.thread_id));
        let mut tabs = h_flex().id("project-sections").gap_2().p_2();
        for (id, label, section) in [
            ("project-section-manager", "Chat", ProjectSection::Manager),
            ("project-section-rules", "Rules", ProjectSection::Rules),
            ("project-section-skills", "Skills", ProjectSection::Skills),
        ] {
            tabs = tabs.child(
                Button::new(id)
                    .debug_selector(move || format!("project-section-label-{label}"))
                    .ghost()
                    .label(label)
                    .selected(self.section == section)
                    .on_click(cx.listener(move |this, _, _, cx| this.select_section(section, cx))),
            );
        }
        v_flex()
            .w_full()
            .child(self.render_chat_header(&status, open, cx))
            .child(tabs)
            .into_any_element()
    }

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
            .h(px(40.))
            .flex_none()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_ellipsis()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(px(design::CHROME))
                    .line_height(px(design::LINE))
                    .debug_selector(|| "project-manager-title".into())
                    .child("Manager"),
            );
        let mut actions = h_flex()
            .flex_none()
            .items_center()
            .gap_1()
            .child(self.connection_line(status, cx));
        let can_open = open.is_some();
        #[cfg(native_manager)]
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
        heading
            .px(px(design::INSET))
            .border_b_1()
            .border_color(brand::palette(cx).border_subtle)
            .debug_selector(|| "project-chat-header".into())
            .into_any_element()
    }

    pub(super) fn render_model_controls(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let controls = h_flex()
            .max_w_full()
            .min_w_0()
            .items_center()
            .flex_wrap()
            .gap_2();
        if let Some(target) = self.manager_model_target() {
            controls
                .child(self.model_picker("project-manager-model", target, window, cx))
                .into_any_element()
        } else {
            controls.into_any_element()
        }
    }

    pub(super) fn render_chat_diagnostics(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut panel = v_flex()
            .gap_2()
            .debug_selector(|| "project-diagnostics".into())
            .child(
                h_flex().child("Details").child(div().flex_1()).child(
                    Button::new("project-diagnostics-close")
                        .debug_selector(|| "project-diagnostics-close".into())
                        .ghost()
                        .small()
                        .icon(IconName::Close)
                        .tooltip("Close details")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.connection_details = false;
                            cx.notify();
                        })),
                ),
            );
        #[cfg(native_manager)]
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
