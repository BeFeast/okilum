use super::*;

impl ProjectView {
    pub(super) fn render_models(&self, cx: &mut Context<Self>) -> AnyElement {
        let has_options = self.has_model_options();
        let measured = self.model_ui.panel_width.get();
        // The popup may overlay the workspace. Its limit is the visible window,
        // not the width of the Manager panel that owns the trigger.
        let available = if measured > px(0.) {
            measured
        } else {
            px(760.)
        };
        let width = available.min(if has_options { px(760.) } else { px(680.) });
        // Keep both names and descriptor labels readable; stack the options
        // only when the window cannot fit a useful model list beside them.
        let options_beside = has_options && width >= px(640.);
        let measured_height = self.model_ui.panel_height.get();
        let height = if measured_height > px(0.) {
            measured_height
        } else {
            px(480.)
        };
        let palette = brand::palette(cx);
        let mut panel = v_flex()
            .debug_selector(|| "project-model-panel".into())
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                this.model_key(event, window, cx);
            }))
            .w(width)
            .max_w_full()
            .max_h(height)
            .text_size(px(brand::READER_CHROME_FONT_SIZE))
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .gap_2();
        let mut search_row = h_flex()
            .w_full()
            .min_w_0()
            .h(px(32.))
            .flex_shrink_0()
            .items_center()
            .gap_2();
        if let Some(search) = &self.model_ui.search {
            search_row = search_row.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .debug_selector(|| "project-model-search".into())
                    .child(
                        Input::new(search)
                            .small()
                            .w_full()
                            .aria_label("Search models"),
                    ),
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
        let loading = self.model_busy || catalog_loading;
        if loading {
            panel = panel.child(
                div()
                    .flex_shrink_0()
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
                    .flex_shrink_0()
                    .max_h(px(48.))
                    .overflow_hidden()
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
            // Search, status and errors keep their own line boxes. The body
            // spends only the remaining visible-window budget, and each column
            // owns its scroll rather than clipping the popup's final controls.
            let chrome = px(40.)
                + if loading { px(24.) } else { px(0.) }
                + if self.model_error.is_some() {
                    px(56.)
                } else {
                    px(0.)
                }
                + if self.connection_details {
                    px(40.)
                } else {
                    px(0.)
                };
            let body_budget = (height - chrome).max(px(0.));
            let rail_rows = snapshot.catalog.providers.len() as f32;
            let rail_natural = px(72. + rail_rows * 36.);
            let natural_height = px((row_count as f32 * 36.).clamp(108., 324.)).max(if searching {
                px(108.)
            } else {
                rail_natural.min(px(324.))
            });
            let (list_height, option_height) = if has_options && !options_beside {
                let column_budget = (body_budget - px(8.)).max(px(0.));
                let option_rows = snapshot
                    .option_controls(current)
                    .iter()
                    .map(|control| 28. * control.choices.len() as f32 + 28.)
                    .sum::<f32>();
                let option_natural = px(option_rows + 24.);
                // Use available height for ordinary supported controls before
                // asking the user to scroll; unusually long catalogs still own
                // an independent, bounded option viewport.
                let options = (column_budget * 0.5).min(option_natural).min(px(340.));
                ((column_budget - options).min(natural_height), options)
            } else {
                let shared = body_budget.min(if has_options {
                    natural_height.max(px(288.))
                } else {
                    natural_height
                });
                (shared, shared)
            };
            let mut rail = v_flex()
                .w(px(32.))
                .h(list_height)
                .flex_shrink_0()
                .gap_1()
                .child(
                    Button::new("project-model-favorites")
                        .debug_selector(|| "project-model-favorites".into())
                        .ghost()
                        .size(px(32.))
                        .flex_shrink_0()
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
                        .size(px(32.))
                        .flex_shrink_0()
                        .p_1()
                        .child(self.render_picker_provider_mark(
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
                        .size(px(32.))
                        .flex_shrink_0()
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
                    .flex_shrink_0()
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
                .flex_shrink_0()
                .gap_2()
                .when(!searching, |body| body.child(rail))
                .child(list);
            if options_beside {
                body = body.child(
                    div()
                        .id("project-model-option-column")
                        .debug_selector(|| "project-model-option-column".into())
                        .w(px(200.))
                        .flex_shrink_0()
                        .h(option_height)
                        .min_h_0()
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
                        .h(option_height)
                        .min_h_0()
                        .flex_shrink_0()
                        .overflow_y_scroll()
                        .child(self.render_model_options(cx)),
                );
            }
        }
        if self.connection_details {
            panel = panel.child(
                div()
                    .flex_shrink_0()
                    .max_h(px(32.))
                    .overflow_hidden()
                    .text_size(px(12.))
                    .text_color(palette.text_muted)
                    .debug_selector(|| "project-model-scheduler-limit".into())
                    .child(okilum_t3::models::SCHEDULED_TASK_LIMIT),
            );
        }
        panel.into_any_element()
    }

    fn render_picker_provider_mark(
        &self,
        provider: &okilum_t3::ProviderCapability,
        providers: &[okilum_t3::ProviderCapability],
        cx: &App,
    ) -> AnyElement {
        let mark = div()
            .relative()
            .size(px(20.))
            .flex_shrink_0()
            .child(visuals::provider_visual(&provider.driver_kind, px(20.)));
        if let Some(badge) = visuals::instance_badge(provider, providers) {
            let palette = brand::palette(cx);
            mark.child(
                div()
                    .absolute()
                    .right(px(-2.))
                    .bottom(px(-2.))
                    .min_w(px(12.))
                    .h(px(12.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(12.))
                    .line_height(px(12.))
                    .bg(palette.surface_raised)
                    .text_color(palette.text_muted)
                    .child(badge),
            )
            .into_any_element()
        } else {
            mark.into_any_element()
        }
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
            .line_height(px(16.))
            .child(
                div()
                    .truncate()
                    .debug_selector(move || format!("project-model-label-{index}"))
                    .child(choice.label.clone()),
            )
            .when_some(secondary, |title, secondary| {
                title.child(
                    div()
                        .debug_selector(move || format!("project-model-provider-label-{index}"))
                        .truncate()
                        .text_size(px(12.))
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
            .h(px(36.))
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
                    .h(px(36.))
                    .px_1()
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
