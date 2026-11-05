//! Controls are supplied by this exact model's descriptors. Prompt-only Claude
//! choices follow T3's draft semantics, never an invented server wire option.
use super::*;
use crate::project::design as ui;
use okilum_t3::models::{ModelControl, ModelControlChoice, Snapshot};
use serde_json::Value;

fn has_ultrathink(text: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|word| word.eq_ignore_ascii_case("ultrathink"))
}
fn prefix_body(text: &str) -> Option<&str> {
    let (prefix, body) = text.split_once(':')?;
    prefix
        .trim()
        .eq_ignore_ascii_case("ultrathink")
        .then(|| body.strip_prefix('\n').unwrap_or(body))
}
fn prompt_edit(text: &str, injected: bool) -> Option<String> {
    if injected {
        let trimmed = text.trim();
        // This is the existing T3 Claude prompt control, not a generic effort
        // ladder. Prefixing a slash command would prevent its invocation.
        if trimmed.starts_with('/') && !trimmed.starts_with("//") {
            return None;
        }
        if has_ultrathink(text) {
            return Some(text.into());
        }
        Some(format!("Ultrathink:\n{text}"))
    } else if let Some(body) = prefix_body(text) {
        (!has_ultrathink(body)).then(|| body.into())
    } else {
        (!has_ultrathink(text)).then(|| text.into())
    }
}

impl ProjectView {
    pub(super) fn picker_controls(snapshot: &Snapshot) -> Vec<ModelControl> {
        snapshot
            .option_controls(&snapshot.configuration.model_selection)
            .into_iter()
            .filter_map(|mut control| {
                // Ordinary values come exclusively from this exact model's
                // descriptor. The only draft instruction implemented here is
                // Claude's advertised Ultrathink, not arbitrary injected ids.
                control.choices.retain(|choice| {
                    !choice.prompt_injected
                        || (Self::prompt_choice_supported(snapshot)
                            && choice.value.as_str() == Some("ultrathink"))
                });
                (!control.choices.is_empty()).then_some(control)
            })
            .collect()
    }

    pub(super) fn has_model_options(&self) -> bool {
        self.models
            .as_ref()
            .is_some_and(|snapshot| !Self::picker_controls(snapshot).is_empty())
    }
    fn prompt_choice_supported(snapshot: &Snapshot) -> bool {
        snapshot.catalog.providers.iter().any(|provider| {
            provider.provider_instance_id == snapshot.configuration.model_selection.instance_id
                && provider.driver_kind == "claudeAgent"
        })
    }
    fn prompt_controlled(&self, snapshot: &Snapshot, cx: &App) -> bool {
        Self::prompt_choice_supported(snapshot)
            && has_ultrathink(self.compose.read(cx).value().as_str())
    }
    fn option_selected(
        &self,
        snapshot: &Snapshot,
        control: &ModelControl,
        choice: &ModelControlChoice,
        cx: &App,
    ) -> bool {
        if control.choices.iter().any(|choice| choice.prompt_injected)
            && self.prompt_controlled(snapshot, cx)
        {
            return choice.prompt_injected;
        }
        control.current.as_ref() == Some(&choice.value)
    }
    pub(in crate::project::models) fn model_chip_summary(
        &self,
        snapshot: &Snapshot,
        cx: &App,
    ) -> String {
        Self::picker_controls(snapshot)
            .iter()
            .filter_map(|control| {
                let choice = control
                    .choices
                    .iter()
                    .find(|choice| self.option_selected(snapshot, control, choice, cx))?;
                if choice.value == Value::Bool(false) {
                    None
                } else if choice.value == Value::Bool(true) {
                    Some(control.label.clone())
                } else {
                    Some(choice.label.clone())
                }
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }
    fn choose_model_option(
        &mut self,
        id: String,
        value: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.model_busy || !self.can_write_models() {
            return;
        }
        let Some(snapshot) = &self.models else {
            return;
        };
        let selection = snapshot.configuration.model_selection.clone();
        let controls = Self::picker_controls(snapshot);
        let Some(control) = controls.iter().find(|control| control.id == id) else {
            return;
        };
        let Some(choice) = control.choices.iter().find(|choice| choice.value == value) else {
            return;
        };
        let controls_prompt = control.choices.iter().any(|choice| choice.prompt_injected);
        let injected = choice.prompt_injected;
        if injected && !Self::prompt_choice_supported(snapshot) {
            self.model_error = Some("This provider does not support that prompt control.".into());
            cx.notify();
            return;
        }
        let changed = if injected {
            None
        } else {
            match snapshot.change_option(&selection, &id, &value) {
                Ok(selection) => Some(selection),
                Err(_) => {
                    self.model_error =
                        Some("That model option is no longer available. Refresh models.".into());
                    cx.notify();
                    return;
                }
            }
        };
        if controls_prompt && Self::prompt_choice_supported(snapshot) {
            let original = self.compose.read(cx).value().to_string();
            let Some(text) = prompt_edit(&original, injected) else {
                self.model_error = Some(
                    "Edit the draft’s reasoning instruction before changing this choice.".into(),
                );
                cx.notify();
                return;
            };
            if text != original {
                self.compose
                    .update(cx, |input, cx| input.set_value(text, window, cx));
                #[cfg(native_manager)]
                self.draft_changed(window, cx);
            }
        }
        if let Some(changed) = changed {
            self.model_request(Some(changed), window, cx);
        }
        cx.notify();
    }
    pub(super) fn render_model_options(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(snapshot) = &self.models else {
            return div().into_any_element();
        };
        let selection = &snapshot.configuration.model_selection;
        let controls = Self::picker_controls(snapshot);
        let enabled = !self.busy && !self.model_busy && self.can_write_models();
        let palette = brand::palette(cx);
        let mut panel = v_flex()
            .w_full()
            .min_w_0()
            .text_size(px(ui::CHROME))
            .line_height(px(ui::LINE))
            .gap(px(ui::INSET))
            .debug_selector(|| "project-model-options".into())
            .child(
                div()
                    .flex_shrink_0()
                    .debug_selector(|| "project-model-settings-title".into())
                    .text_size(px(ui::LABEL))
                    .line_height(px(ui::LINE))
                    .text_color(palette.text_muted)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Model settings"),
            );
        let model_name = snapshot
            .model_info(selection)
            .map(|model| model.display_name())
            .unwrap_or(&selection.model);
        let provider = snapshot
            .catalog
            .providers
            .iter()
            .find(|provider| provider.provider_instance_id == selection.instance_id);
        let mut identity = h_flex().w_full().min_w_0().items_center().gap_2();
        if let Some(provider) = provider {
            identity = identity.child(visuals::provider_visual(&provider.driver_kind, px(18.)));
        }
        panel = panel.child(
            v_flex()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .gap_1()
                .pb_1()
                .child(
                    identity.child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .whitespace_normal()
                            .debug_selector(|| "project-model-inspector-title".into())
                            .text_size(px(ui::BODY))
                            .line_height(px(ui::LINE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(model_name.to_owned()),
                    ),
                )
                .child(
                    h_flex().min_w_0().items_center().gap_2().child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(ui::LABEL))
                            .line_height(px(20.))
                            .text_color(palette.text_muted)
                            .child(
                                provider
                                    .and_then(|provider| provider.display_name.as_deref())
                                    .unwrap_or(&selection.instance_id)
                                    .to_owned(),
                            ),
                    ),
                ),
        );
        if controls.is_empty() {
            panel = panel.child(
                div()
                    .debug_selector(|| "project-model-no-options".into())
                    .w_full()
                    .min_w_0()
                    .flex_shrink_0()
                    .whitespace_normal()
                    .text_size(px(ui::LABEL))
                    .line_height(px(18.))
                    .text_color(palette.text_muted)
                    .child("This model has no additional settings."),
            );
        }
        for (index, control) in controls.iter().enumerate() {
            let mut section = v_flex().w_full().min_w_0().flex_shrink_0().gap_1().child(
                div()
                    .debug_selector(move || format!("project-model-option-heading-{index}"))
                    .flex_shrink_0()
                    .min_h(px(20.))
                    .whitespace_normal()
                    .text_size(px(ui::LABEL))
                    .line_height(px(20.))
                    .text_color(palette.text_muted)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(control.label.clone()),
            );
            if let Some(description) = &control.description {
                section = section.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex_shrink_0()
                        .whitespace_normal()
                        .text_size(px(ui::LABEL))
                        .line_height(px(18.))
                        .text_color(palette.text_muted)
                        .child(description.clone()),
                );
            }
            for (choice_index, choice) in control.choices.iter().enumerate() {
                let id = control.id.clone();
                let value = choice.value.clone();
                let selected = self.option_selected(snapshot, control, choice, cx);
                let supported = !choice.prompt_injected || Self::prompt_choice_supported(snapshot);
                let tooltip = choice
                    .description
                    .clone()
                    .or_else(|| control.description.clone())
                    .unwrap_or_else(|| choice.label.clone());
                section = section.child(
                    Button::new(SharedString::from(format!(
                        "project-model-option-{index}-{choice_index}"
                    )))
                    .debug_selector(move || format!("project-model-option-{index}-{choice_index}"))
                    .ghost()
                    .small()
                    .w_full()
                    .h_auto()
                    .min_h(px(32.))
                    .flex_shrink_0()
                    .py_1()
                    .text_size(px(ui::BODY))
                    .line_height(px(ui::LINE))
                    .justify_start()
                    .selected(selected)
                    .toggled(selected)
                    .tooltip(tooltip)
                    .accessibility_label(format!("{}: {}", control.label, choice.label))
                    .disabled(!enabled || !supported)
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .min_h(px(18.))
                            .items_center()
                            .line_height(px(18.))
                            .gap_1()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(choice.label.clone()),
                            )
                            .when(choice.is_default, |row| {
                                row.child(
                                    div()
                                        .debug_selector(move || {
                                            format!("project-model-default-{index}-{choice_index}")
                                        })
                                        .flex_shrink_0()
                                        .px_1()
                                        .rounded_sm()
                                        .bg(palette.surface)
                                        .text_size(px(12.))
                                        .line_height(px(16.))
                                        .text_color(palette.text_muted)
                                        .child("Default"),
                                )
                            })
                            .child(div().w(px(16.)).flex_shrink_0().when(selected, |check| {
                                check.child(Icon::new(IconName::Check).small())
                            })),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose_model_option(id.clone(), value.clone(), window, cx)
                    })),
                );
            }
            panel = panel.child(section);
        }
        panel.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    #[test]
    fn prompt_only_effort_edits_visible_draft_without_encoding_an_option() {
        assert_eq!(
            prompt_edit("  Keep my whitespace  ", true).unwrap(),
            "Ultrathink:\n  Keep my whitespace  "
        );
        assert_eq!(prompt_edit("", true).unwrap(), "Ultrathink:\n");
        assert_eq!(
            prompt_edit("Ultrathink:\nHello", true).unwrap(),
            "Ultrathink:\nHello"
        );
        assert_eq!(prompt_edit("Ultrathink:\nHello", false).unwrap(), "Hello");
        assert!(prompt_edit("Ultrathink:\nKeep ultrathink in the body", false).is_none());
        assert!(prompt_edit("/compact", true).is_none());
        assert!(prompt_edit("Please ultrathink", false).is_none());
        assert!(!has_ultrathink("ultrathinking"));
    }
}
