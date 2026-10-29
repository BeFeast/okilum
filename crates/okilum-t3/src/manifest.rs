//! Read-only binding of a vault area to its explicitly registered T3 manager.
//! Unrelated project metadata is left alone; this module never creates a
//! connection, chooses a manager, or writes the manifest.

use std::path::Path;

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{Environment, ModelSelection};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AreaBinding {
    pub schema_version: u32,
    pub environment: String,
    pub manager: ManagerBinding,
    pub executor: ExecutorBinding,
    pub default_models: DefaultModels,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagerBinding {
    // Both fields are required even when explicitly unbound (null).
    #[serde(deserialize_with = "required_nullable")]
    pub project_id: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub thread_id: Option<String>,
    pub workspace_mode: WorkspaceMode,
}

fn required_nullable<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Option::deserialize(d)
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMode {
    Local,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutorBinding {
    pub project_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultModels {
    pub manager: ModelSelection,
    pub executor: ModelSelection,
}

impl AreaBinding {
    /// Absent `t3:` means not connected. A present but invalid block is an
    /// error, including an explicit null. It never selects a fallback thread.
    pub fn parse(source: &str) -> Result<Option<Self>> {
        let document: serde_yaml::Value =
            serde_yaml::from_str(source).context("project.yaml is not valid YAML")?;
        let mapping = document
            .as_mapping()
            .context("project.yaml must be a mapping")?;
        let Some(block) = mapping.get(serde_yaml::Value::String("t3".into())) else {
            return Ok(None);
        };
        let binding: Self = serde_yaml::from_value(block.clone()).context("invalid t3: block")?;
        ensure!(
            binding.schema_version == 1,
            "unsupported t3 schema_version {}; expected 1",
            binding.schema_version
        );
        ensure!(
            !binding.environment.is_empty()
                && binding
                    .environment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)),
            "t3.environment must be a connection alias, not a URL or a secret"
        );
        match (&binding.manager.project_id, &binding.manager.thread_id) {
            (Some(project), Some(thread)) => {
                nonempty(project, "manager.project_id")?;
                nonempty(thread, "manager.thread_id")?;
            }
            (None, None) => {}
            _ => bail!("t3.manager project_id and thread_id must both be set or both null"),
        }
        nonempty(&binding.executor.project_id, "executor.project_id")?;
        for (name, model) in [
            ("manager", &binding.default_models.manager),
            ("executor", &binding.default_models.executor),
        ] {
            nonempty(
                &model.instance_id,
                &format!("default_models.{name}.instanceId"),
            )?;
            nonempty(&model.model, &format!("default_models.{name}.model"))?;
        }
        Ok(Some(binding))
    }

    /// Reads this area only. Missing manifests are an unconnected area;
    /// unreadable files and malformed data remain visible errors.
    pub fn read(area: &Path) -> Result<Option<Self>> {
        let Some(document) = crate::area_files::read(area, "project")? else {
            return Ok(None);
        };
        // Reuse the same schema validation; TOML nullable keys are normalized by
        // the manifest reader, while YAML keeps its existing strict contract.
        Self::parse(&serde_yaml::to_string(&document.value)?)
            .with_context(|| document.path.display().to_string())
    }

    /// Resolve the explicit alias in the device's saved connections. No
    /// network call or connection creation occurs during resolution.
    pub fn connection<'a>(&self, connections: &'a [Environment]) -> Result<&'a Environment> {
        let mut matches = connections
            .iter()
            .filter(|env| env.alias == self.environment);
        let found = matches.next().with_context(|| {
            format!(
                "T3 environment '{}' is not connected on this device",
                self.environment
            )
        })?;
        ensure!(
            matches.next().is_none(),
            "T3 environment alias '{}' has more than one connection",
            self.environment
        );
        Ok(found)
    }
}

fn nonempty(value: &str, field: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty() && value.trim() == value,
        "t3.{field} must be a nonempty identifier without surrounding spaces"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
