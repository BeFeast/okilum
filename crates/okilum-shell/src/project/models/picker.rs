use super::*;

impl ProjectView {
    pub(super) fn render_models(&self, cx: &mut Context<Self>) -> AnyElement {
        let enabled = !self.model_busy && !self.busy && self.can_send();
        let mut panel = v_flex()
            .debug_selector(|| "project-model-panel".into())
            .w(px(440.))
            .max_h(px(360.))
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child("Model")
                    .child(
                        Button::new("project-model-refresh")
                            .debug_selector(|| "project-model-refresh".into())
                            .ghost()
                            .small()
                            .icon(IconName::RotateCw)
                            .tooltip("Refresh models")
                            .disabled(!enabled)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.model_request(None, window, cx)
                            })),
                    ),
            );
        if let Some(search) = &self.model_ui.search {
            panel = panel.child(
                div()
                    .debug_selector(|| "project-model-search".into())
                    .child(Input::new(search).w_full().aria_label("Search models")),
            );
        }
        if self.model_busy {
            panel = panel.child("Loading…");
        }
        if let Some(error) = &self.model_error {
            panel = panel.child(
                div()
                    .debug_selector(|| "project-model-error".into())
                    .whitespace_normal()
                    .text_color(cx.theme().danger)
                    .child(if self.connection_details {
                        error.clone()
                    } else {
                        "Could not load or confirm the model. Refresh to try again.".into()
                    }),
            );
        }
        if let Some(snapshot) = &self.models {
            let current = &snapshot.configuration.model_selection;
            let instance = self
                .model_ui
                .provider
                .as_deref()
                .unwrap_or(&current.instance_id);
            let query = self
                .model_ui
                .search
                .as_ref()
                .map(|input| input.read(cx).value().to_string())
                .unwrap_or_default();
            let searching = !query.trim().is_empty();
            let choices = snapshot.picker_choices(instance, &query);
            let mut rail = v_flex()
                .id("project-model-providers")
                .w(px(128.))
                .flex_shrink_0()
                .max_h(px(240.))
                .overflow_y_scroll()
                .gap_1();
            for (index, provider) in snapshot.catalog.providers.iter().enumerate() {
                let id = provider.provider_instance_id.clone();
                let title = provider
                    .display_name
                    .as_deref()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or(&id)
                    .to_owned();
                rail = rail.child(
                    Button::new(("project-model-provider", index))
                        .debug_selector(move || format!("project-model-provider-{index}"))
                        .ghost()
                        .w_full()
                        .justify_start()
                        .label(title.clone())
                        .tooltip(format!("{title} · {id}"))
                        .selected(instance == id)
                        .disabled(self.model_busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.model_ui.provider = Some(id.clone());
                            this.model_ui.scroll.set_offset(point(px(0.), px(0.)));
                            cx.notify();
                        })),
                );
            }
            if !snapshot
                .catalog
                .providers
                .iter()
                .any(|provider| provider.provider_instance_id == current.instance_id)
            {
                let id = current.instance_id.clone();
                rail = rail.child(
                    Button::new("project-model-missing-provider")
                        .ghost()
                        .w_full()
                        .label(format!("{} · Unavailable", current.instance_id))
                        .selected(instance == current.instance_id)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.model_ui.provider = Some(id.clone());
                            cx.notify();
                        })),
                );
            }
            let mut list = v_flex()
                .id("project-model-choices")
                .debug_selector(|| "project-model-choices".into())
                .flex_1()
                .min_w_0()
                .min_h_0()
                .max_h(px(240.))
                .overflow_y_scroll()
                .track_scroll(&self.model_ui.scroll)
                .gap_1();
            for (index, choice) in choices.iter().enumerate() {
                let selection = choice.selection.clone();
                let mut text = v_flex().min_w_0().flex_1().child(
                    div()
                        .text_ellipsis()
                        .debug_selector(move || format!("project-model-label-{index}"))
                        .child(choice.label.clone()),
                );
                if searching {
                    text = text.child(
                        div()
                            .text_xs()
                            .text_color(brand::palette(cx).text_muted)
                            .child(choice.provider_label.clone()),
                    );
                }
                if !choice.selectable {
                    text = text.child(div().text_xs().child("Unavailable"));
                }
                let check = div()
                    .w(px(16.))
                    .flex_shrink_0()
                    .when(choice.current, |view| {
                        view.debug_selector(move || format!("project-model-current-{index}"))
                            .child(Icon::new(IconName::Check).small())
                    });
                list = list.child(
                    Button::new(("project-model-choice", index))
                        .debug_selector(move || format!("project-model-choice-{index}"))
                        .ghost()
                        .w_full()
                        .justify_start()
                        .overflow_hidden()
                        .accessibility_label(choice.label.clone())
                        .tooltip(format!("{} · {}", choice.label, choice.provider_label))
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .items_center()
                                .gap_2()
                                .child(check)
                                .child(text),
                        )
                        .selected(choice.current)
                        .disabled(!enabled || !choice.selectable)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.choose_model(selection.clone(), window, cx)
                        })),
                );
            }
            if choices.is_empty() {
                list = list.child(if searching {
                    "No models found"
                } else {
                    "No models available."
                });
            }
            panel = panel.child(
                h_flex()
                    .min_h_0()
                    .items_start()
                    .gap_2()
                    .when(!searching, |view| view.child(rail))
                    .child(list),
            );
        }
        if self.connection_details {
            panel = panel.child(
                div()
                    .debug_selector(|| "project-model-scheduler-limit".into())
                    .child(okilum_t3::models::SCHEDULED_TASK_LIMIT),
            );
        }
        panel.into_any_element()
    }
}
