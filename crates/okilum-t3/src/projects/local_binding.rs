//! Explicit owner selection for a local project. Paths and labels are not identities.
use crate::{ManagerGateway, Project};
use anyhow::{ensure, Context, Result};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub environment_id: String,
    pub project_id: String,
    pub checkout: PathBuf,
}

impl Selection {
    /// Capture only an explicit picker choice against the verified server identity.
    /// This never creates a remote project or starts a Manager.
    pub fn select(
        environment_id: &str,
        project_id: &str,
        checkout: &Path,
        projects: &[Project],
    ) -> Result<Self> {
        ensure!(
            !environment_id.trim().is_empty(),
            "Owner unresolved: verify the T3 environment first"
        );
        ensure!(
            !project_id.trim().is_empty(),
            "Owner unresolved: select a T3 project explicitly"
        );
        let checkout = directory(checkout)?;
        let candidates: Vec<_> = projects.iter().filter(|p| p.id == project_id).collect();
        ensure!(
            candidates.len() == 1,
            "Owner unresolved: selected project is missing or ambiguous; reload Projects"
        );
        ensure!(
            directory(Path::new(&candidates[0].workspace_root))? == checkout,
            "Owner unresolved: selected T3 project does not own this source checkout"
        );
        Ok(Self {
            environment_id: environment_id.into(),
            project_id: project_id.into(),
            checkout,
        })
    }

    /// Revalidate a retained choice after reconnect, before any binding or launch.
    /// Identity is checked on both sides of the reads; a changed owner cannot be
    /// accepted merely because its hostname or workspace path stayed the same.
    pub fn verify(&self, gateway: &dyn ManagerGateway) -> Result<Project> {
        self.check_environment(&gateway.server_identity()?.environment_id)?;
        let projects = gateway.list_projects()?;
        let current = Self::select(
            &self.environment_id,
            &self.project_id,
            &self.checkout,
            &projects,
        )?;
        ensure!(
            current == *self,
            "Owner unresolved: source checkout changed; select it again"
        );
        let project = gateway.read_project(&self.project_id)?;
        ensure!(
            project.id == self.project_id,
            "Owner unresolved: T3 returned a different project"
        );
        ensure!(
            directory(Path::new(&project.workspace_root))? == self.checkout,
            "Owner unresolved: T3 project workspace changed; select it again"
        );
        self.check_environment(&gateway.server_identity()?.environment_id)?;
        Ok(project)
    }

    pub fn check_environment(&self, actual: &str) -> Result<()> {
        ensure!(
            !actual.trim().is_empty() && actual == self.environment_id,
            "Owner unresolved: T3 environment changed; select the owner again"
        );
        Ok(())
    }
}

fn directory(path: &Path) -> Result<PathBuf> {
    ensure!(
        path.is_absolute(),
        "Owner unresolved: choose an absolute local source checkout"
    );
    let canonical = path
        .canonicalize()
        .context("Owner unresolved: source checkout is unavailable")?;
    ensure!(
        canonical.is_dir(),
        "Owner unresolved: source checkout must be a directory"
    );
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_owner_rejects_missing_duplicate_and_replaced_environment() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let project = Project {
            id: "chosen".into(),
            workspace_root: root.to_str().unwrap().into(),
            ..Default::default()
        };
        let selection =
            Selection::select("verified-server", "chosen", &root, &[project.clone()]).unwrap();
        assert!(selection.check_environment("verified-server").is_ok());
        assert!(selection.check_environment("replacement-server").is_err());
        assert!(selection.check_environment("").is_err());
        assert!(Selection::select("verified-server", "", &root, &[project.clone()]).is_err());
        assert!(Selection::select("verified-server", "chosen", &root, &[]).is_err());
        assert!(Selection::select(
            "verified-server",
            "chosen",
            &root,
            &[project.clone(), project]
        )
        .is_err());
    }
}
