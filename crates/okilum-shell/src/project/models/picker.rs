use super::*;
use crate::project::design as ui;

pub(super) fn surface_size(available: Size<Pixels>) -> Size<Pixels> {
    size(
        available.width.max(px(0.)).min(px(880.)),
        (available.height.max(px(0.)) * 0.8).min(px(500.)),
    )
}

#[derive(IntoElement)]
struct ModelSearch(Entity<InputState>);
impl RenderOnce for ModelSearch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focused = self.0.read(cx).focus_handle(cx).is_focused(window);
        Input::new(&self.0)
            .small()
            .w_full()
            .focus_bordered(false)
            // Keep a visible, single-pixel focus border without the heavy halo.
            .border_color(if focused {
                cx.theme().ring
            } else {
                brand::palette(cx).border_subtle
            })
            .prefix(Icon::new(IconName::Search).small())
            .aria_label("Search models")
    }
}

/// A normal same-frame component boundary keeps settings construction out of
/// the large picker builder's stack frame; it schedules no frames or requests.
#[derive(IntoElement)]
struct ModelSettings(WeakEntity<ProjectView>);
impl RenderOnce for ModelSettings {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        self.0
            .update(cx, |view, cx| view.render_model_options(cx))
            .unwrap_or_else(|_| div().into_any_element())
    }
}

impl ProjectView {
    pub(super) fn render_models(&self, cx: &mut Context<Self>) -> AnyElement {
        let measured = self.model_ui.panel_width.get();
        // The popup may overlay the workspace. Its limit is the visible window,
        // not the width of the Manager panel that owns the trigger.
        let available = if measured > px(0.) {
            measured
        } else {
            px(880.)
        };
        let measured_height = self.model_ui.panel_height.get();
        let geometry = surface_size(size(
            available,
            if measured_height > px(0.) {
                measured_height
            } else {
                px(625.)
            },
        ));
        let width = geometry.width;
        // Keep both names and descriptor labels readable; stack the options
        // only when the window cannot fit a useful model list beside them.
        let options_beside = width >= px(800.);
        let named_providers = width >= px(640.);
        let rail_width = if named_providers { px(208.) } else { px(68.) };
        let height = geometry.height;
        // Geometry is a function of the viewport only. Catalog length, current
        // descriptors, provider selection, favorites and search cannot resize
        // or reposition the surface underneath the pointer.
        let body_budget = (height - px(80.)).max(px(0.));
        let (list_height, option_height) = if options_beside {
            (body_budget, body_budget)
        } else {
            let stacked_budget = (body_budget - px(8.)).max(px(0.));
            (stacked_budget * 0.6, stacked_budget * 0.4)
        };
        let choices = Arc::new(self.visible_model_choices(cx));
        let palette = brand::palette(cx);
        let mut top_row = h_flex()
            .debug_selector(|| "project-model-header".into())
            .w_full()
            .min_w_0()
            .h(px(40.))
            .flex_shrink_0()
            .items_center()
            .gap(px(ui::GAP))
            .child(
                div()
                    .debug_selector(|| "project-model-title".into())
                    .flex_shrink_0()
                    .text_size(px(ui::BODY))
                    .line_height(px(20.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Select a model"),
            );
        if let Some(search) = &self.model_ui.search {
            top_row = top_row.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .debug_selector(|| "project-model-search".into())
                    .child(ModelSearch(search.clone())),
            );
        }
        let mut panel = v_flex()
            .debug_selector(|| "project-model-panel".into())
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                this.model_key(event, window, cx);
            }))
            .w(width)
            .max_w_full()
            .h(height)
            .flex_shrink_0()
            .text_size(px(ui::CHROME))
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .gap(px(ui::GAP))
            .child(
                top_row
                    .child(
                        Button::new("project-model-refresh")
                            .debug_selector(|| "project-model-refresh".into())
                            .ghost()
                            .small()
                            .icon(IconName::RotateCw)
                            .tooltip("Refresh models")
                            .disabled(self.model_busy || self.busy || !self.can_read_models())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.model_request(None, window, cx)
                            })),
                    )
                    .child(
                        Button::new("project-model-close")
                            .debug_selector(|| "project-model-close".into())
                            .ghost()
                            .small()
                            .icon(IconName::Close)
                            .accessibility_label("Close model selector")
                            .tooltip("Close model selector (Esc)")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.close_models(window, cx)),
                            ),
                    ),
            );
        #[cfg(native_manager)]
        let catalog_loading = self.native.active()
            && self
                .native
                .catalog
                .as_ref()
                .is_none_or(|catalog| catalog.read(cx).loading);
        #[cfg(not(native_manager))]
        let catalog_loading = false;
        let loading = self.model_busy || catalog_loading;
        let status = if self.model_error.is_some() {
            "Models are unavailable. Refresh to try again.".to_owned()
        } else if loading {
            if self.models.is_some() {
                "Updating models…"
            } else {
                "Loading models…"
            }
            .to_owned()
        } else if self.models.is_some() {
            format!(
                "{} models{}",
                choices.len(),
                if self.has_model_options() {
                    " · Settings available"
                } else {
                    ""
                }
            )
        } else {
            "Connect to see available models".to_owned()
        };
        panel = panel.child(
            div()
                .debug_selector(|| "project-model-status".into())
                .w_full()
                .h(px(24.))
                .flex_shrink_0()
                .truncate()
                .text_size(px(ui::LABEL))
                .line_height(px(20.))
                .text_color(if self.model_error.is_some() {
                    cx.theme().danger
                } else {
                    palette.text_muted
                })
                .when(self.model_error.is_some(), |status| {
                    status.debug_selector(|| "project-model-error".into())
                })
                .child(status),
        );
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
            let row_count = choices.len();
            let mut rail = v_flex()
                .debug_selector(|| "project-model-provider-rail".into())
                .w(rail_width)
                .h(list_height)
                .flex_shrink_0()
                .rounded(px(ui::RADIUS))
                .p_1()
                .gap_1()
                .child(
                    Button::new("project-model-favorites")
                        .debug_selector(|| "project-model-favorites".into())
                        .ghost()
                        .w_full()
                        .h(px(36.))
                        .flex_shrink_0()
                        .px_2()
                        .child(visuals::rail_label(
                            "favorites",
                            IconName::StarFill,
                            "Favorites",
                            named_providers,
                        ))
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
                .debug_selector(|| "project-model-providers".into())
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
                        .w_full()
                        .h_auto()
                        .min_h(px(36.))
                        .flex_shrink_0()
                        .px_2()
                        .py_1()
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .items_center()
                                .gap_2()
                                .child(self.render_picker_provider_mark(
                                    provider,
                                    &snapshot.catalog.providers,
                                    cx,
                                ))
                                .when(named_providers, |button| {
                                    button.child(
                                        div()
                                            .debug_selector(move || {
                                                format!("project-model-account-label-{index}")
                                            })
                                            .min_w_0()
                                            .flex_1()
                                            .whitespace_normal()
                                            .text_size(px(ui::LABEL))
                                            .line_height(px(ui::LINE))
                                            .child(title.clone()),
                                    )
                                })
                                .when_some(
                                    visuals::instance_badge(provider, &snapshot.catalog.providers),
                                    |button, badge| {
                                        button.child(
                                            div()
                                                .debug_selector(move || {
                                                    format!("project-model-account-suffix-{index}")
                                                })
                                                .flex_shrink_0()
                                                .text_size(px(ui::LABEL))
                                                .line_height(px(18.))
                                                .text_color(palette.text_muted)
                                                .child(badge),
                                        )
                                    },
                                ),
                        )
                        .tooltip(format!("{title} · {}", provider.provider_instance_id))
                        .accessibility_label(format!("Models from {title}"))
                        .selected(!searching && !self.model_ui.favorites && instance == id)
                        .toggled(!searching && !self.model_ui.favorites && instance == id)
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
                        .w_full()
                        .h(px(36.))
                        .flex_shrink_0()
                        .px_2()
                        .child(visuals::rail_label(
                            "unavailable",
                            IconName::TriangleAlert,
                            "Unavailable",
                            named_providers,
                        ))
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
                    .w_full()
                    .h(px(36.))
                    .flex_shrink_0()
                    .px_2()
                    .child(visuals::rail_label(
                        "current",
                        IconName::Check,
                        "Selected model",
                        named_providers,
                    ))
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
                .h(list_height)
                .flex_shrink_0()
                .gap_2()
                .child(rail)
                .child(list);
            if options_beside {
                body = body.child(
                    div()
                        .id("project-model-option-column")
                        .debug_selector(|| "project-model-option-column".into())
                        .w(px(260.))
                        .flex_shrink_0()
                        .h(option_height)
                        .min_h_0()
                        .rounded(px(ui::RADIUS))
                        .bg(palette.surface_raised)
                        .p(px(ui::INSET))
                        .overflow_y_scroll()
                        .child(ModelSettings(cx.weak_entity()))
                        .when(self.connection_details, |pane| {
                            pane.child(self.render_picker_diagnostics(cx))
                        }),
                );
            }
            panel = panel.child(body);
            if !options_beside {
                panel = panel.child(
                    div()
                        .id("project-model-option-column")
                        .debug_selector(|| "project-model-option-column".into())
                        .w_full()
                        .min_w_0()
                        .h(option_height)
                        .min_h_0()
                        .flex_shrink_0()
                        .rounded(px(ui::RADIUS))
                        .bg(palette.surface_raised)
                        .p(px(ui::INSET))
                        .overflow_y_scroll()
                        .child(ModelSettings(cx.weak_entity()))
                        .when(self.connection_details, |pane| {
                            pane.child(self.render_picker_diagnostics(cx))
                        }),
                );
            }
        } else if self.connection_details {
            panel = panel.child(
                div()
                    .id("project-model-option-column")
                    .debug_selector(|| "project-model-option-column".into())
                    .w_full()
                    .h(body_budget)
                    .min_h_0()
                    .flex_shrink_0()
                    .overflow_y_scroll()
                    .child(self.render_picker_diagnostics(cx)),
            );
        }
        panel.into_any_element()
    }

    fn render_picker_diagnostics(&self, cx: &App) -> AnyElement {
        let mut details = v_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .gap_2()
            .debug_selector(|| "project-model-diagnostics".into());
        if let Some(error) = &self.model_error {
            details = details.child(self.picker_diagnostic_line(
                "project-model-error-detail",
                error.clone(),
                cx,
            ));
        }
        details
            .child(self.picker_diagnostic_line(
                "project-model-scheduler-limit",
                okilum_t3::models::SCHEDULED_TASK_LIMIT.into(),
                cx,
            ))
            .into_any_element()
    }

    fn picker_diagnostic_line(&self, selector: &'static str, text: String, cx: &App) -> AnyElement {
        let line = div()
            .relative()
            .debug_selector(move || selector.into())
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .whitespace_normal()
            .text_size(px(ui::LABEL))
            .line_height(px(ui::LINE))
            .text_color(brand::palette(cx).text_muted)
            .child(text.clone());
        #[cfg(test)]
        let line = line.child({
            let painted = self.model_ui.diagnostics_painted.clone();
            gpui::canvas(
                |_, _, _| (),
                move |_, _, _, _| {
                    painted.borrow_mut().push(text);
                },
            )
            .absolute()
            .size_full()
        });
        line.into_any_element()
    }

    fn render_picker_provider_mark(
        &self,
        provider: &okilum_t3::ProviderCapability,
        _: &[okilum_t3::ProviderCapability],
        _: &App,
    ) -> AnyElement {
        // Original brand artwork remains unobscured. Account identities belong
        // beside the name (including the compact rail), never over the logo.
        let instance = provider.provider_instance_id.clone();
        div()
            .debug_selector(move || format!("project-model-provider-logo-{instance}"))
            .size(px(18.))
            .flex_shrink_0()
            .child(visuals::provider_visual(&provider.driver_kind, px(18.)))
            .into_any_element()
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
            .map(|provider| visuals::provider_visual(&provider.driver_kind, px(18.)))
            .unwrap_or_else(|| {
                Icon::new(IconName::SquareTerminal)
                    .size(px(18.))
                    .into_any_element()
            });
        let full_label = self
            .models
            .as_ref()
            .and_then(|snapshot| snapshot.model_info(&choice.selection))
            .and_then(|model| model.label.as_deref())
            .unwrap_or(&choice.label);
        let selection = choice.selection.clone();
        let favorite_selection = selection.clone();
        let favorite = self.model_is_favorite(&selection);
        let browsing_provider = !self.model_ui.favorites
            && self
                .model_ui
                .search
                .as_ref()
                .is_none_or(|input| input.read(cx).value().trim().is_empty());
        let sub_provider = self
            .models
            .as_ref()
            .and_then(|snapshot| snapshot.model_info(&choice.selection))
            .and_then(|model| model.sub_provider.as_deref())
            .filter(|name| !name.trim().is_empty());
        let multiple_accounts = provider.is_some_and(|provider| {
            self.models.as_ref().is_some_and(|snapshot| {
                snapshot
                    .catalog
                    .providers
                    .iter()
                    .filter(|account| account.driver_kind == provider.driver_kind)
                    .count()
                    > 1
            })
        });
        let secondary = if !choice.selectable {
            Some(sub_provider.map_or_else(
                || "Unavailable".to_owned(),
                |vendor| format!("{vendor} · Unavailable"),
            ))
        } else if let Some(vendor) = sub_provider {
            if !browsing_provider || multiple_accounts {
                if vendor.eq_ignore_ascii_case(&choice.provider_label) {
                    Some(vendor.to_owned())
                } else {
                    Some(format!("{vendor} · {}", choice.provider_label))
                }
            } else {
                Some(vendor.to_owned())
            }
        } else if !browsing_provider || multiple_accounts {
            Some(choice.provider_label.clone())
        } else {
            None
        };
        let mut title = v_flex()
            .flex_1()
            .min_w_0()
            .line_height(px(20.))
            .child(
                div()
                    .truncate()
                    .debug_selector(move || format!("project-model-label-{index}"))
                    .text_size(px(ui::BODY))
                    .line_height(px(ui::LINE))
                    .font_weight(FontWeight::MEDIUM)
                    .child(choice.label.clone()),
            )
            .when_some(secondary, |title, secondary| {
                title.child(
                    div()
                        .debug_selector(move || format!("project-model-provider-label-{index}"))
                        .truncate()
                        .text_size(px(ui::LABEL))
                        .line_height(px(16.))
                        .text_color(palette.text_muted)
                        .child(secondary),
                )
            });
        if !choice.selectable {
            title = title.text_color(palette.text_muted);
        }
        let mut content = h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .gap_2()
            .child(glyph)
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
            .h(px(44.))
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
                    .h(px(44.))
                    .px_2()
                    .py(px(2.))
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
