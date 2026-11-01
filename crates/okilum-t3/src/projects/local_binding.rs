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
        ensure!(
            projects
                .iter()
                .filter(|project| directory(Path::new(&project.workspace_root))
                    .is_ok_and(|root| root == checkout))
                .count()
                == 1,
            "Owner unresolved: source checkout is missing or has multiple T3 owners"
        );
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

/// Manifest revision retained while the explicit owner picker is open.
#[derive(Clone, Debug)]
pub struct Binding {
    area: PathBuf,
    document: crate::area_files::Document,
}

impl Binding {
    pub fn load(area: &Path) -> Result<Self> {
        let document =
            crate::area_files::read(area, "project")?.context("Create the local project first")?;
        ensure!(
            document.value.get("t3").is_none(),
            "Project already has a T3 binding; its retained recovery must be used"
        );
        let project = super::Project::read(area)?;
        ensure!(
            project.manager == super::Manager::NotStarted,
            "Project already has a Manager; its retained recovery must be used"
        );
        Ok(Self {
            area: area.into(),
            document,
        })
    }

    /// Persist an explicitly selected existing owner. No project/thread creation.
    pub fn save(
        &self,
        env: &crate::Environment,
        selection: &Selection,
        gateway: &dyn ManagerGateway,
    ) -> Result<super::Project> {
        let local = super::local_manager::environment(&env.origin())?;
        ensure!(local.origin() == env.origin(), "Local T3 origin changed");
        let actual = crate::area_files::read(&self.area, "project")?
            .context("Project disappeared; reopen setup")?;
        ensure!(
            actual.path == self.document.path && actual.source == self.document.source,
            "Project changed; reopen setup; draft retained"
        );
        let project = super::Project::read(&self.area)?;
        ensure!(
            project.manager == super::Manager::NotStarted && project.manager_project.is_none(),
            "Project owner changed; reopen the picker; draft retained"
        );
        let target = selection.verify(gateway)?;
        let mut document = self.document.clone();
        if project.kind != super::Kind::Knowledge {
            if let Some(checkout) = project.appearance.source_checkout.as_deref() {
                ensure!(
                    directory(Path::new(checkout))? == selection.checkout,
                    "Selected owner does not match the project's explicit source checkout"
                );
            } else {
                ensure!(document.value.get("execution").is_none(),
                    "Existing execution settings have no checkout; edit them explicitly before binding");
                document = append_field(
                    document,
                    "execution",
                    serde_yaml::to_value(
                        serde_json::json!({"source_checkout": selection.checkout.to_str().context("Checkout is not UTF-8")?}),
                    )?,
                )?;
            }
        }
        let root = selection
            .checkout
            .to_str()
            .context("Checkout is not UTF-8")?;
        document = append_field(
            document,
            "t3",
            serde_yaml::to_value(serde_json::json!({
                "schema_version": 1,
                "environment": env.alias,
                "origin": env.origin(),
                "executor_project_id": target.id,
                "manager": {"project_id": target.id, "thread_id": null, "workspace_mode": "local"},
                "local_workspace": {"environment_id": selection.environment_id, "root": root}
            }))?,
        )?;
        ensure!(
            document.source.len() <= 512 * 1024,
            "Project manifest exceeds 512 KiB; not written"
        );
        let owner = super::Project::read(&self.area)?;
        ensure!(
            owner.manager == super::Manager::NotStarted && owner.manager_project.is_none(),
            "Project Manager changed while checking T3; reopen setup; draft retained"
        );
        // replace uses the original source revision, not the staged document.
        self.document.replace(&document.source)?;
        let saved = super::Project::read(&self.area)?;
        ensure!(
            saved.manager_project.as_deref() == Some(selection.project_id.as_str()),
            "Saved Manager owner did not read back; reload before retrying"
        );
        Ok(saved)
    }
}

fn append_field(
    mut document: crate::area_files::Document,
    key: &str,
    value: serde_yaml::Value,
) -> Result<crate::area_files::Document> {
    ensure!(
        document.value.get(key).is_none(),
        "Existing {key} settings were not replaced"
    );
    document.source = match document.format {
        crate::area_files::Format::Toml => document.set_toml_field(key, Some(&value))?,
        crate::area_files::Format::Yaml => {
            let mut source = document.source.clone();
            if !source.ends_with('\n') {
                source.push('\n');
            }
            source.push_str(&serde_yaml::to_string(&serde_yaml::Mapping::from_iter([
                (serde_yaml::Value::from(key), value.clone()),
            ]))?);
            source
        }
    };
    let after = crate::area_files::parse(&document.source, document.format)?;
    let mut expected = document.value.clone();
    expected[key] = value;
    // TOML omits nullable Manager fields; area_files::parse restores them.
    ensure!(
        after == expected,
        "Binding would change unrelated project fields"
    );
    document.value = after;
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_binding_preserves_format_fields_and_refuses_stale_source() {
        for (format, source) in [
            (
                crate::area_files::Format::Yaml,
                "# keep\nname: Local\nkind: code\n",
            ),
            (
                crate::area_files::Format::Toml,
                "# keep\nname = \"Local\"\nkind = \"code\"\n",
            ),
        ] {
            let area =
                std::env::temp_dir().join(format!("okilum-binding-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(area.join("ops")).unwrap();
            let path = area
                .join("ops")
                .join(if format == crate::area_files::Format::Toml {
                    "project.toml"
                } else {
                    "project.yaml"
                });
            std::fs::write(&path, source).unwrap();
            let original = crate::area_files::read(&area, "project").unwrap().unwrap();
            let staged = append_field(original.clone(), "t3", serde_yaml::to_value(serde_json::json!({
                "schema_version": 1, "environment": "explicit", "origin": "http://localhost:3773",
                "executor_project_id": "chosen", "manager": {"project_id": "chosen", "thread_id": null, "workspace_mode": "local"},
                "local_workspace": {"environment_id": "verified", "root": "/checkout"}
            })).unwrap()).unwrap();
            assert!(staged.source.starts_with(source));
            assert_eq!(staged.value["name"], original.value["name"]);
            assert_eq!(
                staged.value["t3"]["local_workspace"]["environment_id"].as_str(),
                Some("verified")
            );
            original.replace(&staged.source).unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), staged.source);
            assert!(append_field(staged.clone(), "t3", serde_yaml::Value::Null).is_err());
            std::fs::write(&path, format!("{}# external edit\n", staged.source)).unwrap();
            assert!(original.replace(&staged.source).is_err());
            assert!(std::fs::read_to_string(&path)
                .unwrap()
                .ends_with("# external edit\n"));
            std::fs::remove_dir_all(area).unwrap();
        }
    }

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
        let mut other = project.clone();
        other.id = "other-owner".into();
        assert!(Selection::select(
            "verified-server",
            "chosen",
            &root,
            &[project.clone(), other]
        )
        .is_err());
        assert!(Selection::select(
            "verified-server",
            "chosen",
            &root,
            &[project.clone(), project]
        )
        .is_err());
    }
}
