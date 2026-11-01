use super::*;

impl ProjectView {
    pub(super) fn render_models(&self, cx: &mut Context<Self>) -> AnyElement {
        let has_options = self.has_model_options();
        let measured = self.model_ui.panel_width.get();
        let available = if measured > px(0.) {
            measured
        } else {
            px(440.)
        };
        let width = available.min(if has_options { px(560.) } else { px(440.) });
        let options_beside = has_options && width >= px(480.);
        let palette = brand::palette(cx);
        let mut panel = v_flex()
            .debug_selector(|| "project-model-panel".into())
            .w(width)
            .max_w_full()
            .max_h(px(420.))
            .min_w_0()
            .gap_2();
        let mut search_row = h_flex().w_full().min_w_0().items_center().gap_2();
        if let Some(search) = &self.model_ui.search {
            search_row = search_row.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .debug_selector(|| "project-model-search".into())
                    .child(Input::new(search).w_full().aria_label("Search models")),
            );
        }
        panel = panel.child(
            search_row.child(
                Button::new("project-model-refresh")
                    .debug_selector(|| "project-model-refresh".into())
                    .ghost()
                    .small()
                    .icon(IconName::RotateCw)
                    .tooltip("Refresh models")
                    .disabled(self.model_busy || self.busy || !self.can_read_models())
                    .on_click(
                        cx.listener(|this, _, window, cx| this.model_request(None, window, cx)),
                    ),
            ),
        );
        #[cfg(target_os = "linux")]
        let catalog_loading = self.native.active()
            && self
                .native
                .catalog
                .as_ref()
                .is_none_or(|catalog| catalog.read(cx).loading);
        #[cfg(not(target_os = "linux"))]
        let catalog_loading = false;
        if self.model_busy || catalog_loading {
            panel = panel.child(
                div()
                    .text_size(px(12.))
                    .text_color(palette.text_muted)
                    .child(if self.models.is_some() {
                        "Updating models…"
                    } else {
                        "Loading models…"
                    }),
            );
        }
        if let Some(error) = &self.model_error {
            panel = panel.child(
                div()
                    .debug_selector(|| "project-model-error".into())
                    .whitespace_normal()
                    .text_size(px(12.))
                    .text_color(cx.theme().danger)
                    .child(if self.connection_details {
                        error.clone()
                    } else {
                        "Models are unavailable. Refresh to try again.".into()
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
            let searching = self
                .model_ui
                .search
                .as_ref()
                .is_some_and(|input| !input.read(cx).value().trim().is_empty());
            let choices = Arc::new(self.visible_model_choices(cx));
            let row_count = choices.len();
            let list_height = if has_options && !options_beside {
                px(160.)
            } else {
                px((row_count as f32 * 40.).clamp(160., 280.))
            };
            let mut rail = v_flex()
                .w(px(40.))
                .h(list_height)
                .flex_shrink_0()
                .gap_1()
                .child(
                    Button::new("project-model-favorites")
                        .debug_selector(|| "project-model-favorites".into())
                        .ghost()
                        .size(px(40.))
                        .icon(IconName::StarFill)
                        .tooltip("Favorites")
                        .accessibility_label("Favorite models")
                        .selected(self.model_ui.favorites)
                        .toggled(self.model_ui.favorites)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.model_ui.favorites = true;
                            this.model_ui.follow_current = false;
                            this.model_ui.highlight = 0;
                            this.model_ui
                                .list_scroll
                                .0
                                .borrow()
                                .base_handle
                                .set_offset(point(px(0.), px(0.)));
                            cx.notify();
                        })),
                );
            let mut providers = v_flex()
                .id("project-model-providers")
                .w_full()
                .flex_1()
                .min_h_0()
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
                providers = providers.child(
                    Button::new(("project-model-provider", index))
                        .debug_selector(move || format!("project-model-provider-{index}"))
                        .ghost()
                        .size(px(40.))
                        .p_2()
                        .child(visuals::provider_mark(
                            provider,
                            &snapshot.catalog.providers,
                            cx,
                        ))
                        .tooltip(title.clone())
                        .accessibility_label(format!("Models from {title}"))
                        .selected(!self.model_ui.favorites && instance == id)
                        .toggled(!self.model_ui.favorites && instance == id)
                        .disabled(self.model_busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.model_ui.favorites = false;
                            this.model_ui.provider = Some(id.clone());
                            this.model_ui.follow_current = false;
                            this.model_ui.highlight = 0;
                            this.model_ui
                                .list_scroll
                                .0
                                .borrow()
                                .base_handle
                                .set_offset(point(px(0.), px(0.)));
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
                providers = providers.child(
                    Button::new("project-model-missing-provider")
                        .debug_selector(|| "project-model-missing-provider".into())
                        .ghost()
                        .size(px(40.))
                        .icon(IconName::TriangleAlert)
                        .tooltip("Current provider is unavailable")
                        .selected(!self.model_ui.favorites && instance == current.instance_id)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.model_ui.favorites = false;
                            this.model_ui.provider = Some(id.clone());
                            this.model_ui.highlight = 0;
                            cx.notify();
                        })),
                );
            }
            let current_instance = current.instance_id.clone();
            rail = rail.child(providers).child(
                Button::new("project-model-show-current")
                    .debug_selector(|| "project-model-show-current".into())
                    .ghost()
                    .size(px(32.))
                    .icon(IconName::Check)
                    .tooltip("Show current model")
                    .accessibility_label("Show current model")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.model_ui.favorites = false;
                        this.model_ui.provider = Some(current_instance.clone());
                        this.model_ui.follow_current = true;
                        this.settle_model_layout(false, window, cx);
                        cx.notify();
                    })),
            );
            let list = if row_count == 0 {
                div()
                    .w_full()
                    .h(list_height)
                    .p_2()
                    .text_size(px(12.))
                    .text_color(palette.text_muted)
                    .child(if searching {
                        "No models found"
                    } else if self.model_ui.favorites {
                        "Star a model to keep it here."
                    } else {
                        "No models available."
                    })
                    .into_any_element()
            } else {
                let rows = choices.clone();
                gpui::uniform_list(
                    "project-model-choice-list",
                    row_count,
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|index| {
                                this.render_model_choice(
                                    index,
                                    &rows[index],
                                    navigation::hint(&rows, index),
                                    cx,
                                )
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.model_ui.list_scroll)
                .w_full()
                .h(list_height)
                .into_any_element()
            };
            let list = div()
                .id("project-model-choices")
                .debug_selector(|| "project-model-choices".into())
                .flex_1()
                .min_w_0()
                .min_h_0()
                .overflow_hidden()
                .child(list);
            let mut body = h_flex()
                .w_full()
                .min_w_0()
                .min_h_0()
                .items_start()
                .gap_2()
                .when(!searching, |body| body.child(rail))
                .child(list);
            if options_beside {
                body = body.child(
                    div()
                        .id("project-model-option-column")
                        .debug_selector(|| "project-model-option-column".into())
                        .w(px(160.))
                        .flex_shrink_0()
                        .max_h(list_height)
                        .overflow_y_scroll()
                        .child(self.render_model_options(cx)),
                );
            }
            panel = panel.child(body);
            if has_options && !options_beside {
                panel = panel.child(
                    div()
                        .id("project-model-option-column")
                        .debug_selector(|| "project-model-option-column".into())
                        .w_full()
                        .min_w_0()
                        .max_h(px(160.))
                        .overflow_y_scroll()
                        .child(self.render_model_options(cx)),
                );
            }
        }
        if self.connection_details {
            panel = panel.child(
                div()
                    .text_size(px(12.))
                    .text_color(palette.text_muted)
                    .debug_selector(|| "project-model-scheduler-limit".into())
                    .child(okilum_t3::models::SCHEDULED_TASK_LIMIT),
            );
        }
        panel.into_any_element()
    }

    fn render_model_choice(
        &self,
        index: usize,
        choice: &okilum_t3::models::PickerChoice,
        hint: Option<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = brand::palette(cx);
        let enabled =
            !self.model_busy && !self.busy && self.can_write_models() && choice.selectable;
        let provider = self.models.as_ref().and_then(|snapshot| {
            snapshot
                .catalog
                .providers
                .iter()
                .find(|provider| provider.provider_instance_id == choice.selection.instance_id)
        });
        let glyph = provider
            .map(|provider| visuals::provider_glyph(&provider.driver_kind))
            .unwrap_or_else(|| Icon::new(IconName::SquareTerminal));
        let full_label = self
            .models
            .as_ref()
            .and_then(|snapshot| snapshot.model_info(&choice.selection))
            .and_then(|model| model.label.as_deref())
            .unwrap_or(&choice.label);
        let selection = choice.selection.clone();
        let favorite_selection = selection.clone();
        let favorite = self.model_is_favorite(&selection);
        let mut title = v_flex()
            .flex_1()
            .min_w_0()
            .line_height(px(16.))
            .child(
                div()
                    .truncate()
                    .debug_selector(move || format!("project-model-label-{index}"))
                    .child(choice.label.clone()),
            )
            .child(
                div()
                    .truncate()
                    .text_size(px(12.))
                    .text_color(palette.text_muted)
                    .child(if choice.selectable {
                        choice.provider_label.clone()
                    } else {
                        format!("{} · Unavailable", choice.provider_label)
                    }),
            );
        if !choice.selectable {
            title = title.text_color(palette.text_muted);
        }
        let mut content = h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .gap_2()
            .child(glyph.small())
            .child(title);
        content = content
            .when_some(hint, |content, hint| {
                content.child(
                    div()
                        .flex_shrink_0()
                        .px_1()
                        .rounded_sm()
                        .text_size(px(12.))
                        .text_color(palette.text_muted)
                        .bg(palette.surface_raised)
                        .child(hint),
                )
            })
            .child(
                div()
                    .w(px(16.))
                    .flex_shrink_0()
                    .when(choice.current, |check| {
                        check
                            .debug_selector(move || format!("project-model-current-{index}"))
                            .child(Icon::new(IconName::Check).small())
                    }),
            );
        h_flex()
            .id(("project-model-row", index))
            .debug_selector(move || format!("project-model-row-{index}"))
            .w_full()
            .h(px(40.))
            .min_w_0()
            .items_center()
            .gap_1()
            .rounded_md()
            .when(self.model_ui.highlight == index && !choice.current, |row| {
                row.bg(palette.surface_raised)
            })
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                if enabled && this.model_ui.highlight != index {
                    this.model_ui.highlight = index;
                    cx.notify();
                }
            }))
            .child(
                Button::new(("project-model-choice", index))
                    .debug_selector(move || format!("project-model-choice-{index}"))
                    .ghost()
                    .flex_1()
                    .min_w_0()
                    .h(px(40.))
                    .px_1()
                    .py_1()
                    .justify_start()
                    .overflow_hidden()
                    .accessibility_label(format!("{} · {}", choice.label, choice.provider_label))
                    .tooltip(format!("{full_label} · {}", choice.provider_label))
                    .child(content)
                    .selected(choice.current)
                    .disabled(!enabled)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose_model(selection.clone(), window, cx)
                    })),
            )
            .child(
                Button::new(("project-model-favorite", index))
                    .debug_selector(move || format!("project-model-favorite-{index}"))
                    .ghost()
                    .small()
                    .size(px(24.))
                    .icon(if favorite {
                        IconName::StarFill
                    } else {
                        IconName::Star
                    })
                    .tooltip(if favorite {
                        "Remove from favorites"
                    } else {
                        "Add to favorites"
                    })
                    .accessibility_label(if favorite {
                        "Remove model from favorites"
                    } else {
                        "Favorite model"
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_model_favorite(favorite_selection.clone(), cx);
                    })),
            )
            .into_any_element()
    }
}
