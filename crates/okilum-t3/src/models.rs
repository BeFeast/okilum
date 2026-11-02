//! Explicit thread model controls. A mutation is not confirmed until T3's
//! configuration readback identifies the requested thread and selection.
use anyhow::{ensure, Result};

use crate::{Capabilities, ManagerGateway, ModelSelection, ThreadConfiguration, ThreadQuery};

pub const SCHEDULED_TASK_LIMIT: &str =
    "Changing a conversation’s model does not change models used by scheduled tasks.";

#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub thread_id: String,
    pub project_id: String,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub target: Target,
    pub catalog: Capabilities,
    pub configuration: ThreadConfiguration,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PickerChoice {
    pub selection: ModelSelection,
    pub label: String,
    pub provider_label: String,
    pub sub_provider: Option<String>,
    pub current: bool,
    pub selectable: bool,
}

mod options;
mod search;
pub use options::{ModelControl, ModelControlChoice};

impl Target {
    fn verify(&self, gateway: &dyn ManagerGateway) -> Result<()> {
        let mut query = ThreadQuery::messages(&self.thread_id);
        query.limit = Some(1);
        query.max_chars_per_item = Some(1);
        let page = gateway.read_thread(&query)?;
        ensure!(
            page.thread.thread_id == self.thread_id && page.thread.project_id == self.project_id,
            "The model target no longer matches the linked T3 project; nothing was configured."
        );
        Ok(())
    }

    pub fn load(&self, gateway: &dyn ManagerGateway) -> Result<Snapshot> {
        self.verify(gateway)?;
        let catalog = gateway.capabilities()?;
        let configuration = gateway.thread_configuration(&self.thread_id)?;
        ensure!(
            configuration.thread_id == self.thread_id,
            "T3 returned another thread's model configuration."
        );
        Ok(Snapshot {
            target: self.clone(),
            catalog,
            configuration,
        })
    }

    pub fn configure(
        &self,
        gateway: &dyn ManagerGateway,
        selection: &ModelSelection,
    ) -> Result<Snapshot> {
        // Revalidate ownership and the live catalog immediately before sending.
        let mut snapshot = self.load(gateway)?;
        snapshot.validate(selection)?;
        let observed = gateway.configure_model(&self.thread_id, selection)?;
        ensure!(
            observed.thread_id == self.thread_id
                && selections_match(&observed.model_selection, selection),
            "Model change is unconfirmed: T3's readback differs from the requested selection. Refresh to read the current configuration; no automatic retry was made."
        );
        snapshot.configuration = observed;
        Ok(snapshot)
    }
}

impl Snapshot {
    /// An empty query browses one configured instance; search spans instances.
    /// Current is the server's instance/model pair, independent of option order.
    pub fn picker_choices(&self, instance: &str, query: &str) -> Vec<PickerChoice> {
        let current = &self.configuration.model_selection;
        let mut choices = Vec::new();
        for provider in &self.catalog.providers {
            if query.trim().is_empty() && provider.provider_instance_id != instance {
                continue;
            }
            let provider_label = provider
                .display_name
                .as_deref()
                .filter(|label| !label.trim().is_empty())
                .unwrap_or(&provider.provider_instance_id);
            for model in &provider.models {
                let label = model.display_name();
                let Some(score) = search::score(
                    query,
                    &[
                        label,
                        model.label.as_deref().unwrap_or(""),
                        &model.id,
                        model.sub_provider.as_deref().unwrap_or(""),
                        &provider.driver_kind,
                        provider_label,
                    ],
                    false,
                ) else {
                    continue;
                };
                let selection = self.selection(&provider.provider_instance_id, &model.id);
                let selectable = self.validate(&selection).is_ok();
                choices.push((
                    score,
                    PickerChoice {
                        current: selectable
                            && current.instance_id == selection.instance_id
                            && current.model == selection.model,
                        selection,
                        label: label.into(),
                        provider_label: provider_label.into(),
                        sub_provider: model.sub_provider.clone(),
                        selectable,
                    },
                ));
            }
        }
        // A missing current is explicit, never substituted by another account
        // which happens to advertise the same slug.
        if self.validate(current).is_err()
            && (!query.trim().is_empty() || current.instance_id == instance)
        {
            if let Some(score) =
                search::score(query, &[&current.model, &current.instance_id], false)
            {
                choices.insert(
                    0,
                    (
                        score,
                        PickerChoice {
                            selection: current.clone(),
                            label: current.model.clone(),
                            provider_label: current.instance_id.clone(),
                            sub_provider: None,
                            current: true,
                            selectable: false,
                        },
                    ),
                );
            }
        }
        if !query.trim().is_empty() {
            choices.sort_by(|(left_score, left), (right_score, right)| {
                left_score
                    .cmp(right_score)
                    .then_with(|| left.label.to_lowercase().cmp(&right.label.to_lowercase()))
                    .then_with(|| left.selection.instance_id.cmp(&right.selection.instance_id))
                    .then_with(|| left.selection.model.cmp(&right.selection.model))
            });
        }
        choices.into_iter().map(|(_, choice)| choice).collect()
    }

    pub fn validate(&self, selection: &ModelSelection) -> Result<()> {
        validate_selection(&self.catalog, selection)
    }

    /// Keep the current options when reselecting the current model. A different
    /// model starts with no overrides; T3 supplies its defaults, never options
    /// belonging to another provider/model.
    pub fn selection(&self, instance: &str, model: &str) -> ModelSelection {
        let current = &self.configuration.model_selection;
        ModelSelection {
            instance_id: instance.into(),
            model: model.into(),
            options: if current.instance_id == instance && current.model == model {
                current.options.clone()
            } else {
                Vec::new()
            },
        }
    }
}

pub(crate) fn validate_selection(catalog: &Capabilities, selection: &ModelSelection) -> Result<()> {
    let providers: Vec<_> = catalog
        .providers
        .iter()
        .filter(|p| p.provider_instance_id == selection.instance_id)
        .collect();
    ensure!(
        providers.len() == 1,
        "Provider is missing or ambiguous in the live catalog. Refresh models."
    );
    ensure!(
        providers[0]
            .models
            .iter()
            .filter(|m| m.id == selection.model)
            .count()
            == 1,
        "Model is missing or ambiguous in the live catalog. Refresh models."
    );
    ensure!(
        options(selection).is_some(),
        "Model options contain duplicate identifiers."
    );
    ensure!(
        selection
            .options
            .iter()
            .all(|option| option.value.is_boolean()
                || option
                    .value
                    .as_str()
                    .is_some_and(|value| !value.trim().is_empty())),
        "Model options must be nonempty strings or boolean values."
    );
    let model = providers[0]
        .models
        .iter()
        .find(|model| model.id == selection.model)
        .unwrap();
    options::validate_model_options(model, selection)?;
    Ok(())
}

fn options(
    selection: &ModelSelection,
) -> Option<std::collections::BTreeMap<&str, &serde_json::Value>> {
    let mut result = std::collections::BTreeMap::new();
    for option in &selection.options {
        if option.id.is_empty() || result.insert(option.id.as_str(), &option.value).is_some() {
            return None;
        }
    }
    Some(result)
}

pub(crate) fn selections_match(a: &ModelSelection, b: &ModelSelection) -> bool {
    a.instance_id == b.instance_id
        && a.model == b.model
        && options(a).is_some()
        && options(a) == options(b)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod help_tests {
    #[test]
    fn scheduled_model_help_explains_the_effect_without_internal_policy_terms() {
        let help = super::SCHEDULED_TASK_LIMIT;
        assert!(help.contains("scheduled tasks") && help.contains("model"));
        for internal in [
            "migration",
            "blocked",
            "credential",
            "readback",
            "full-access",
        ] {
            assert!(
                !help.contains(internal),
                "internal term leaked into model help: {internal}"
            );
        }
    }
}
