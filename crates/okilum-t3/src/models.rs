//! Explicit thread model controls. A mutation is not confirmed until T3's
//! configuration readback identifies the requested thread and selection.
use anyhow::{ensure, Result};

use crate::{Capabilities, ManagerGateway, ModelSelection, ThreadConfiguration, ThreadQuery};

pub const SCHEDULED_TASK_LIMIT: &str =
    "Scheduled tasks do not follow this model change. Manager migration remains blocked.";

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
    pub fn validate(&self, selection: &ModelSelection) -> Result<()> {
        let providers: Vec<_> = self
            .catalog
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
        Ok(())
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

fn selections_match(a: &ModelSelection, b: &ModelSelection) -> bool {
    a.instance_id == b.instance_id
        && a.model == b.model
        && options(a).is_some()
        && options(a) == options(b)
}
