//! An explicit user request to the area's manager, never a direct launch call.
//! The existing message journal retains the complete request before sending it.
use std::{
    io::Read,
    path::{Component, Path},
};

use crate::{
    manifest::AreaBinding, models::validate_selection, Capabilities, ManagerGateway,
    ModelSelection, Project, ThreadQuery,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

pub const PREFIX: &str = "Okilum executor launch request (v1)\n";
const MAX_BRIEF_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub struct LaunchContext {
    pub project: Project,
    pub catalog: Capabilities,
}

impl LaunchContext {
    pub fn load(gateway: &dyn ManagerGateway, binding: &AreaBinding) -> Result<Self> {
        let manager = binding
            .manager
            .thread_id
            .as_ref()
            .context("This area has no manager")?;
        let mut query = ThreadQuery::messages(manager);
        query.limit = Some(1);
        query.max_chars_per_item = Some(256);
        let thread = gateway.read_thread(&query)?.thread;
        ensure!(
            thread.thread_id == *manager
                && Some(&thread.project_id) == binding.manager.project_id.as_ref(),
            "The linked manager belongs to another T3 project; nothing was requested."
        );
        let mut projects = gateway
            .list_projects()?
            .into_iter()
            .filter(|p| p.id == binding.executor.project_id);
        let project = projects
            .next()
            .context("The area's executor project is missing from T3")?;
        ensure!(
            projects.next().is_none(),
            "The executor project is ambiguous in T3"
        );
        Ok(Self {
            project,
            catalog: gateway.capabilities()?,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Draft {
    pub role: String,
    /// Relative to the selected vault area on this device.
    pub brief_path: String,
    /// A path on the T3 server, never opened on this device.
    pub worktree_path: String,
    pub branch: String,
    pub model_override: Option<ModelSelection>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub request_id: String,
    pub role: String,
    pub brief_path: String,
    pub brief_source: String,
    pub executor_project_id: String,
    pub executor_project_root: String,
    pub worktree_path: String,
    pub branch: String,
    pub model_selection: ModelSelection,
    pub manager_instructions: String,
}

impl Request {
    pub fn prepare(
        area: &Path,
        binding: &AreaBinding,
        context: &LaunchContext,
        draft: &Draft,
    ) -> Result<Self> {
        ensure!(
            AreaBinding::read(area)?.as_ref() == Some(binding),
            "The area's manager binding changed. Reopen Project before requesting an executor."
        );
        ensure!(
            context.project.id == binding.executor.project_id,
            "Executor project does not match this area"
        );
        ensure!(
            !draft.role.trim().is_empty()
                && draft.role.len() <= 128
                && !draft.role.chars().any(char::is_control),
            "Enter a role of at most 128 bytes without control characters."
        );
        // The accepted T3 target is a Unix server. Do not validate remote paths
        // using this device's filesystem (which may be Windows).
        ensure!(
            draft.worktree_path.starts_with('/')
                && draft.worktree_path != "/"
                && !draft.worktree_path.chars().any(char::is_control),
            "Enter an absolute worktree path on the Unix T3 server, other than its root."
        );
        ensure!(
            !draft.branch.trim().is_empty()
                && !draft.branch.chars().any(char::is_whitespace)
                && !draft.branch.chars().any(char::is_control),
            "Enter the requested worktree branch without whitespace."
        );
        let relative = Path::new(&draft.brief_path);
        ensure!(
            !draft.brief_path.is_empty()
                && relative
                    .components()
                    .all(|p| matches!(p, Component::Normal(_))),
            "Brief must be a relative file path inside this vault area, without parent traversal."
        );
        let area = area.canonicalize()?;
        let brief = area
            .join(relative)
            .canonicalize()
            .context("Cannot find the vault brief")?;
        ensure!(
            brief.starts_with(&area),
            "Brief resolves outside this vault area; nothing was read or sent."
        );
        ensure!(brief.is_file(), "Brief must be a regular file");
        let mut brief_bytes = Vec::new();
        std::fs::File::open(brief)?
            .take((MAX_BRIEF_BYTES + 1) as u64)
            .read_to_end(&mut brief_bytes)
            .context("Cannot read the vault brief")?;
        ensure!(
            brief_bytes.len() <= MAX_BRIEF_BYTES,
            "Brief exceeds 64 KiB; nothing was truncated or requested."
        );
        let brief_source =
            String::from_utf8(brief_bytes).context("Cannot read the vault brief as UTF-8")?;
        ensure!(!brief_source.trim().is_empty(), "The vault brief is empty");
        let model = draft
            .model_override
            .clone()
            .or_else(|| context.project.default_model_selection.clone())
            .context("T3 project has no default model. Choose a model from its live catalog.")?;
        validate_selection(&context.catalog, &model)?;
        Ok(Self {
            request_id: format!("okilum-launch-{}", uuid::Uuid::new_v4()), role: draft.role.trim().into(),
            brief_path: draft.brief_path.clone(), brief_source,
            executor_project_id: context.project.id.clone(), executor_project_root: context.project.workspace_root.clone(),
            worktree_path: draft.worktree_path.clone(), branch: draft.branch.clone(), model_selection: model,
            manager_instructions: "This is an explicit user request for ONE executor. Recheck your area rules, brief and target project; create/select the requested worktree through t3_thread_launch workspaceStrategy and use the requested model. For a Codex executor, prepend a bootstrap that reads the canonical server absolute paths for _AgentContract.md, vault/Dev/area AGENTS.md, manifest and brief before work; resolve these from your own vault, never from device-local paths. Record the resulting thread in the area's registry. Report actual acceptance, preparing/running/completed state and the exact thread ID in manager chat; a message queue receipt is not launch acceptance. Keep this request_id for reconciliation. t3_thread_launch has no idempotency key: after an ambiguous response reconcile the project thread list before any further launch; NEVER blindly launch again. Scheduled-task model following remains unsupported; do not migrate the production manager.".into(),
        })
    }

    pub fn message(&self) -> Result<String> {
        Ok(format!("{PREFIX}{}", serde_json::to_string_pretty(self)?))
    }

    pub fn from_message(text: &str) -> Result<Option<Self>> {
        text.strip_prefix(PREFIX)
            .map(serde_json::from_str)
            .transpose()
            .map_err(Into::into)
    }
}
