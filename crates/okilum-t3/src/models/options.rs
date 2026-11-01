//! Model-scoped option presentation and explicit changes. No control choices
//! are guessed from a provider family, model id, or universal reasoning ladder.
use super::Snapshot;
use crate::{types::ModelOptionDescriptor, ModelInfo, ModelOption, ModelSelection};
use anyhow::{ensure, Result};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct ModelControlChoice {
    pub value: Value,
    pub label: String,
    pub description: Option<String>,
    pub is_default: bool,
    pub prompt_injected: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelControl {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub current: Option<Value>,
    pub choices: Vec<ModelControlChoice>,
}

impl Snapshot {
    /// A model is resolved only within its exact provider instance. Duplicate
    /// accounts/models remain ambiguous even when their display labels match.
    pub fn model_info(&self, selection: &ModelSelection) -> Option<&ModelInfo> {
        let mut providers = self
            .catalog
            .providers
            .iter()
            .filter(|provider| provider.provider_instance_id == selection.instance_id);
        let provider = providers.next()?;
        if providers.next().is_some() {
            return None;
        }
        let mut models = provider
            .models
            .iter()
            .filter(|model| model.id == selection.model);
        let model = models.next()?;
        models.next().is_none().then_some(model)
    }

    pub fn option_controls(&self, selection: &ModelSelection) -> Vec<ModelControl> {
        let Some(capabilities) = self
            .model_info(selection)
            .and_then(|model| model.capabilities.as_ref())
        else {
            return Vec::new();
        };
        capabilities
            .option_descriptors
            .iter()
            .filter_map(|descriptor| {
                let explicit = |id: &str| {
                    selection
                        .options
                        .iter()
                        .find(|option| option.id == id)
                        .map(|option| option.value.clone())
                };
                Some(match descriptor {
                    ModelOptionDescriptor::Select {
                        id,
                        label,
                        description,
                        options,
                        current_value,
                        prompt_injected_values,
                    } => {
                        let default = current_value.as_ref().or_else(|| {
                            options
                                .iter()
                                .find(|option| option.is_default)
                                .map(|option| &option.id)
                        });
                        ModelControl {
                            id: id.clone(),
                            label: label.clone(),
                            description: description.clone(),
                            current: explicit(id).or_else(|| default.cloned().map(Value::String)),
                            choices: options
                                .iter()
                                .map(|option| ModelControlChoice {
                                    value: Value::String(option.id.clone()),
                                    label: option.label.clone(),
                                    description: option.description.clone(),
                                    is_default: option.is_default,
                                    prompt_injected: prompt_injected_values.contains(&option.id),
                                })
                                .collect(),
                        }
                    }
                    ModelOptionDescriptor::Boolean {
                        id,
                        label,
                        description,
                        current_value,
                    } => ModelControl {
                        id: id.clone(),
                        label: label.clone(),
                        description: description.clone(),
                        current: explicit(id).or_else(|| current_value.map(Value::Bool)),
                        choices: [false, true]
                            .into_iter()
                            .map(|value| ModelControlChoice {
                                value: Value::Bool(value),
                                label: if value { "On" } else { "Off" }.into(),
                                description: None,
                                is_default: false,
                                prompt_injected: false,
                            })
                            .collect(),
                    },
                    ModelOptionDescriptor::Unsupported => return None,
                })
            })
            .collect()
    }

    /// Change one advertised ordinary option without discarding the other
    /// explicit choices. Prompt-injected values edit the visible draft through
    /// a separate UI path; this helper never encodes them as wire options.
    pub fn change_option(
        &self,
        selection: &ModelSelection,
        id: &str,
        value: &Value,
    ) -> Result<ModelSelection> {
        let controls = self.option_controls(selection);
        let matching = controls
            .iter()
            .filter(|control| control.id == id)
            .collect::<Vec<_>>();
        ensure!(
            matching.len() == 1,
            "This model does not expose that option unambiguously."
        );
        let choices = matching[0]
            .choices
            .iter()
            .filter(|choice| &choice.value == value)
            .collect::<Vec<_>>();
        ensure!(
            choices.len() == 1,
            "This model does not offer that option value."
        );
        ensure!(
            !choices[0].prompt_injected,
            "Prompt-only model values belong in the message draft."
        );
        let mut changed = selection.clone();
        changed.options.retain(|option| option.id != id);
        changed.options.push(ModelOption {
            id: id.into(),
            value: value.clone(),
        });
        self.validate(&changed)?;
        Ok(changed)
    }

    pub fn option_summary(&self, selection: &ModelSelection) -> String {
        self.option_controls(selection)
            .iter()
            .filter_map(|control| {
                let current = control.current.as_ref()?;
                let Some(choice) = control
                    .choices
                    .iter()
                    .find(|choice| &choice.value == current)
                else {
                    return Some(format!("{} · unavailable", control.label));
                };
                Some(if current.is_boolean() {
                    format!("{} {}", control.label, choice.label)
                } else {
                    choice.label.clone()
                })
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

pub(super) fn validate_model_options(model: &ModelInfo, selection: &ModelSelection) -> Result<()> {
    // Legacy MCP catalogs may omit capabilities entirely. Preserve their
    // existing selections, but expose no invented option controls.
    let Some(capabilities) = &model.capabilities else {
        return Ok(());
    };
    for option in &selection.options {
        let descriptors = capabilities
            .option_descriptors
            .iter()
            .filter(|descriptor| descriptor.id() == Some(option.id.as_str()))
            .collect::<Vec<_>>();
        ensure!(
            descriptors.len() == 1,
            "Model option is missing or ambiguous in the live catalog."
        );
        match descriptors[0] {
            ModelOptionDescriptor::Select {
                options,
                prompt_injected_values,
                ..
            } => {
                let value = option.value.as_str();
                ensure!(
                    value.is_some_and(|value| options
                        .iter()
                        .filter(|choice| choice.id == value)
                        .count()
                        == 1),
                    "This model does not offer that option value."
                );
                ensure!(
                    !prompt_injected_values
                        .iter()
                        .any(|injected| Some(injected.as_str()) == value),
                    "Prompt-only model values belong in the message draft."
                );
            }
            ModelOptionDescriptor::Boolean { .. } => ensure!(
                option.value.is_boolean(),
                "This model option requires a boolean value."
            ),
            ModelOptionDescriptor::Unsupported => {
                unreachable!("unsupported descriptors have no known id")
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
