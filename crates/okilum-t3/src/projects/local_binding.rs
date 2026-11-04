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
    project: super::Project,
    owner: Option<Owner>,
}

#[derive(Clone, Debug)]
struct Owner {
    environment: crate::Environment,
    selection: Selection,
}

impl Binding {
    pub fn load(area: &Path) -> Result<Self> {
        let document =
            crate::area_files::read(area, "project")?.context("Create the local project first")?;
        let project = super::Project::read(area)?;
        let owner = match document.value.get("t3") {
            None => {
                ensure!(
                    project.manager == super::Manager::NotStarted,
                    "Project already has a Manager; its retained recovery must be used"
                );
                None
            }
            Some(t3) => {
                ensure!(
                    t3["schema_version"].as_u64() == Some(1)
                        && t3["manager"]["workspace_mode"].as_str() == Some("local"),
                    "Saved local binding schema or workspace mode is unsupported; binding retained"
                );
                let workspace: super::local_manager::Workspace = serde_yaml::from_value(
                    t3.get("local_workspace").context(
                        "This is not a verified local binding; keep its existing connection and recovery"
                    )?.clone()
                ).context("Invalid saved local workspace; binding was not replaced")?;
                let origin = t3["origin"]
                    .as_str()
                    .context("Saved local origin is missing")?;
                super::local_manager::environment(origin)?;
                let alias = t3["environment"]
                    .as_str()
                    .context("Saved local environment alias is missing")?;
                let environment = crate::Environment::new(alias, origin)?;
                let project_id = t3["manager"]["project_id"]
                    .as_str()
                    .context("Saved local project owner is missing")?;
                ensure!(
                    !project_id.trim().is_empty() && !workspace.environment_id.trim().is_empty(),
                    "Saved local owner identity is missing; binding was not replaced"
                );
                ensure!(
                    project.environment.as_deref() == Some(alias)
                        && project.manager_project.as_deref() == Some(project_id),
                    "Saved local project owner is inconsistent; binding was not replaced"
                );
                let checkout = directory(Path::new(&workspace.root))?;
                ensure!(
                    checkout == Path::new(&workspace.root),
                    "Saved local checkout identity changed; binding was not replaced"
                );
                Some(Owner {
                    environment,
                    selection: Selection {
                        environment_id: workspace.environment_id,
                        project_id: project_id.into(),
                        checkout,
                    },
                })
            }
        };
        let binding = Self {
            area: area.into(),
            document,
            project,
            owner,
        };
        if let Some(owner) = &binding.owner {
            binding.check_checkout(&binding.project, &owner.selection)?;
        }
        Ok(binding)
    }

    /// Reconnect always uses the saved explicit endpoint, even if an alias in
    /// the app registry now names another server. Endpoint migration is separate.
    pub fn pinned_environment(&self) -> Option<&crate::Environment> {
        self.owner.as_ref().map(|owner| &owner.environment)
    }

    pub fn pinned_owner(&self) -> Option<&Selection> {
        self.owner.as_ref().map(|owner| &owner.selection)
    }

    /// A newly authenticated connection cannot silently become a new owner.
    pub fn check_selection(&self, env: &crate::Environment, selection: &Selection) -> Result<()> {
        super::local_manager::environment(&env.origin())?;
        if let Some(owner) = &self.owner {
            ensure!(env == &owner.environment && selection == &owner.selection,
                "Owner unresolved: reconnected project, environment or checkout differs from the saved owner; binding retained");
        }
        Ok(())
    }

    fn current(&self) -> Result<super::Project> {
        let actual = crate::area_files::read(&self.area, "project")?
            .context("Project disappeared; reopen setup")?;
        ensure!(
            actual.path == self.document.path && actual.source == self.document.source,
            "Project changed; reopen setup; draft retained"
        );
        let project = super::Project::read(&self.area)?;
        ensure!(
            project.manager == self.project.manager
                && project.manager_project == self.project.manager_project
                && project.environment == self.project.environment
                && project.kind == self.project.kind
                && project.appearance.source_checkout == self.project.appearance.source_checkout,
            "Project owner or checkout changed; reopen setup; draft retained"
        );
        // Project::read also reads the authoritative Manager registry. Recheck
        // the manifest after that read, including the reconnect no-write path.
        let latest = crate::area_files::read(&self.area, "project")?
            .context("Project disappeared; reopen setup")?;
        ensure!(
            latest.path == self.document.path && latest.source == self.document.source,
            "Project changed; reopen setup; draft retained"
        );
        Ok(project)
    }

    fn check_checkout(&self, project: &super::Project, selection: &Selection) -> Result<()> {
        ensure!(
            directory(&selection.checkout)? == selection.checkout,
            "Selected checkout identity changed; reopen setup; draft retained"
        );
        if project.kind != super::Kind::Knowledge {
            match project.appearance.source_checkout.as_deref() {
                Some(checkout) => ensure!(
                    directory(Path::new(checkout))? == selection.checkout,
                    "Selected owner does not match the project's explicit source checkout"
                ),
                None => ensure!(
                    self.owner.is_none(),
                    "Saved local source checkout is missing; binding retained"
                ),
            }
        }
        Ok(())
    }

    /// Persist an explicitly selected existing owner. No project/thread creation.
    pub fn save(
        &self,
        env: &crate::Environment,
        selection: &Selection,
        gateway: &dyn ManagerGateway,
    ) -> Result<super::Project> {
        self.check_selection(env, selection)?;
        let project = self.current()?;
        self.check_checkout(&project, selection)?;
        let target = selection.verify(gateway)?;
        // In particular, catch source/registry edits made while reconnecting.
        let current = self.current()?;
        self.check_checkout(&current, selection)?;
        if self.owner.is_some() {
            // Reconnect refreshes the authenticated transport, not the saved
            // Manager/executor or launch intent. Avoid a no-op replace: it skips
            // revision checking, which current() above deliberately performs.
            return Ok(current);
        }
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
        let owner = self.current()?;
        self.check_checkout(&owner, selection)?;
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

    use crate::gateway::GatewayResult;
    use crate::{
        Capabilities, GatewayStatus, ModelSelection, OutgoingMessage, SendReceipt, ServerIdentity,
        ThreadConfiguration, ThreadPage, ThreadQuery, ThreadSummary,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    };

    struct Reconnected {
        identity: Mutex<String>,
        project: Mutex<Project>,
        identity_after_read: Mutex<Option<String>>,
        edit_on_read: Mutex<Option<(PathBuf, String)>>,
        reads: AtomicUsize,
    }
    impl ManagerGateway for Reconnected {
        fn status(&self) -> GatewayStatus {
            GatewayStatus::Connected {
                expires_at: u64::MAX,
            }
        }
        fn server_identity(&self) -> GatewayResult<ServerIdentity> {
            Ok(ServerIdentity {
                environment_id: self.identity.lock().unwrap().clone(),
                label: "Same hostname".into(),
            })
        }
        fn list_projects(&self) -> GatewayResult<Vec<Project>> {
            Ok(vec![self.project.lock().unwrap().clone()])
        }
        fn read_project(&self, _: &str) -> GatewayResult<Project> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if let Some(identity) = self.identity_after_read.lock().unwrap().take() {
                *self.identity.lock().unwrap() = identity;
            }
            if let Some((path, source)) = self.edit_on_read.lock().unwrap().take() {
                std::fs::write(path, source).unwrap();
            }
            Ok(self.project.lock().unwrap().clone())
        }
        fn create_project(&self, _: &str, _: &str) -> GatewayResult<Project> {
            panic!("reconnect must not create a project")
        }
        fn start_manager(&self, _: &str, _: &str, _: &str) -> GatewayResult<String> {
            panic!("reconnect must not launch a thread")
        }
        fn list_threads(&self, _: &str) -> GatewayResult<Vec<ThreadSummary>> {
            unreachable!()
        }
        fn read_thread(&self, _: &ThreadQuery) -> GatewayResult<ThreadPage> {
            unreachable!()
        }
        fn send_message(&self, _: &OutgoingMessage) -> GatewayResult<SendReceipt> {
            unreachable!()
        }
        fn pending_requests(&self, _: &str) -> GatewayResult<Vec<String>> {
            unreachable!()
        }
        fn capabilities(&self) -> GatewayResult<Capabilities> {
            unreachable!()
        }
        fn thread_configuration(&self, _: &str) -> GatewayResult<ThreadConfiguration> {
            unreachable!()
        }
        fn configure_model(
            &self,
            _: &str,
            _: &ModelSelection,
        ) -> GatewayResult<ThreadConfiguration> {
            unreachable!()
        }
    }

    struct LocalFixture {
        area: PathBuf,
        path: PathBuf,
        checkout: PathBuf,
    }
    impl LocalFixture {
        fn new(toml: bool) -> Self {
            let area =
                std::env::temp_dir().join(format!("okilum-reconnect-{}", uuid::Uuid::new_v4()));
            let checkout = area.join("checkout");
            std::fs::create_dir_all(area.join("ops")).unwrap();
            std::fs::create_dir(&checkout).unwrap();
            let checkout = checkout.canonicalize().unwrap();
            let path = area
                .join("ops")
                .join(if toml { "project.toml" } else { "project.yaml" });
            std::fs::write(
                &path,
                if toml {
                    "# retained\nname = 'Local'\nkind = 'code'\ncustom = 'keep'\n"
                } else {
                    "# retained\nname: Local\nkind: code\ncustom: keep\n"
                },
            )
            .unwrap();
            let document = crate::area_files::read(&area, "project").unwrap().unwrap();
            let document = append_field(
                document,
                "execution",
                serde_yaml::to_value(serde_json::json!({
                    "source_checkout": checkout.to_str().unwrap(), "command": "keep"
                }))
                .unwrap(),
            )
            .unwrap();
            let document = append_field(document, "t3", serde_yaml::to_value(serde_json::json!({
                "schema_version": 1, "environment": "explicit-local", "origin": "http://localhost:3773",
                "executor_project_id": "chosen", "executor": {"project_id": "retained-executor"},
                "manager": {"project_id": "chosen", "thread_id": "mcp:existing", "workspace_mode": "local"},
                "local_workspace": {"environment_id": "verified", "root": checkout.to_str().unwrap()}
            })).unwrap()).unwrap();
            std::fs::write(&path, document.source).unwrap();
            Self {
                area,
                path,
                checkout,
            }
        }
        fn gateway(&self) -> Reconnected {
            Reconnected {
                identity: Mutex::new("verified".into()),
                project: Mutex::new(Project {
                    id: "chosen".into(),
                    workspace_root: self.checkout.to_str().unwrap().into(),
                    ..Default::default()
                }),
                identity_after_read: Mutex::new(None),
                edit_on_read: Mutex::new(None),
                reads: AtomicUsize::new(0),
            }
        }
    }
    impl Drop for LocalFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.area);
        }
    }

    #[test]
    fn initial_binding_still_persists_the_explicit_owner_before_reconnect() {
        for toml in [false, true] {
            let fixture = LocalFixture::new(toml);
            std::fs::write(
                &fixture.path,
                if toml {
                    "# retained\nname = 'Local'\nkind = 'code'\n"
                } else {
                    "# retained\nname: Local\nkind: code\n"
                },
            )
            .unwrap();
            let gateway = fixture.gateway();
            let binding = Binding::load(&fixture.area).unwrap();
            assert!(binding.pinned_owner().is_none());
            let env = crate::Environment::new("explicit-local", "http://localhost:3773").unwrap();
            let selection = Selection::select(
                "verified",
                "chosen",
                &fixture.checkout,
                &gateway.list_projects().unwrap(),
            )
            .unwrap();
            let saved = binding.save(&env, &selection, &gateway).unwrap();
            assert_eq!(saved.manager_project.as_deref(), Some("chosen"));
            assert_eq!(
                saved.appearance.source_checkout.as_deref(),
                fixture.checkout.to_str()
            );
            let source = std::fs::read(&fixture.path).unwrap();
            let reconnect = Binding::load(&fixture.area).unwrap();
            assert_eq!(reconnect.pinned_owner(), Some(&selection));
            reconnect.save(&env, &selection, &gateway).unwrap();
            assert_eq!(std::fs::read(&fixture.path).unwrap(), source);
        }
    }

    #[test]
    fn reconnect_keeps_existing_manager_and_exact_manifest_in_both_formats() {
        for toml in [false, true] {
            let fixture = LocalFixture::new(toml);
            let source = std::fs::read(&fixture.path).unwrap();
            let binding = Binding::load(&fixture.area).unwrap();
            let gateway = fixture.gateway();
            let project = binding
                .save(
                    binding.pinned_environment().unwrap(),
                    binding.pinned_owner().unwrap(),
                    &gateway,
                )
                .unwrap();
            assert_eq!(gateway.reads.load(Ordering::SeqCst), 1); // positive control: verification ran
            assert_eq!(
                project.manager,
                super::super::Manager::Thread("mcp:existing".into())
            );
            assert_eq!(project.manager_project.as_deref(), Some("chosen"));
            assert_eq!(
                project.appearance.executor_project.as_deref(),
                Some("retained-executor")
            );
            assert_eq!(std::fs::read(&fixture.path).unwrap(), source);
        }
    }

    #[test]
    fn reconnect_rejects_replaced_environment_project_and_checkout_without_rebinding() {
        let fixture = LocalFixture::new(false);
        let source = std::fs::read(&fixture.path).unwrap();
        let binding = Binding::load(&fixture.area).unwrap();
        let env = binding.pinned_environment().unwrap();
        let selection = binding.pinned_owner().unwrap();
        let gateway = fixture.gateway();
        for changed in [
            Selection {
                environment_id: "replacement".into(),
                ..selection.clone()
            },
            Selection {
                project_id: "replacement".into(),
                ..selection.clone()
            },
            Selection {
                checkout: fixture.area.clone(),
                ..selection.clone()
            },
        ] {
            assert!(binding.save(env, &changed, &gateway).is_err());
        }
        let other_origin =
            crate::Environment::new("explicit-local", "http://localhost:4773").unwrap();
        assert!(binding.save(&other_origin, selection, &gateway).is_err());
        assert_eq!(gateway.reads.load(Ordering::SeqCst), 0);
        gateway.project.lock().unwrap().id = "replacement".into();
        assert!(binding.save(env, selection, &gateway).is_err());
        gateway.project.lock().unwrap().id = "chosen".into();
        gateway.project.lock().unwrap().workspace_root = fixture.area.to_str().unwrap().into();
        assert!(binding.save(env, selection, &gateway).is_err());
        gateway.project.lock().unwrap().workspace_root = fixture.checkout.to_str().unwrap().into();
        // Same address/path and selected project, but a different authenticated server.
        *gateway.identity.lock().unwrap() = "replacement".into();
        assert!(binding.save(env, selection, &gateway).is_err());
        *gateway.identity.lock().unwrap() = "verified".into();
        *gateway.identity_after_read.lock().unwrap() = Some("replacement".into());
        assert!(binding.save(env, selection, &gateway).is_err());
        assert_eq!(gateway.reads.load(Ordering::SeqCst), 1);
        assert_eq!(std::fs::read(&fixture.path).unwrap(), source);
    }

    #[test]
    fn reconnect_revalidates_revision_and_manager_registry_after_remote_reads() {
        for registry_edit in [false, true] {
            let fixture = LocalFixture::new(false);
            let source = std::fs::read_to_string(&fixture.path).unwrap();
            let binding = Binding::load(&fixture.area).unwrap();
            let gateway = fixture.gateway();
            let (path, edited) = if registry_edit {
                (
                    fixture.area.join("ops/thread-ids.md"),
                    "| Manager | `mcp:rotated` |\n".into(),
                )
            } else {
                (fixture.path.clone(), format!("{source}# external edit\n"))
            };
            *gateway.edit_on_read.lock().unwrap() = Some((path.clone(), edited.clone()));
            assert!(binding
                .save(
                    binding.pinned_environment().unwrap(),
                    binding.pinned_owner().unwrap(),
                    &gateway
                )
                .is_err());
            assert_eq!(gateway.reads.load(Ordering::SeqCst), 1); // edit fired inside reconnect
            assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
            if registry_edit {
                assert_eq!(std::fs::read_to_string(&fixture.path).unwrap(), source);
            }
        }
    }

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
        let selection = Selection::select(
            "verified-server",
            "chosen",
            &root,
            std::slice::from_ref(&project),
        )
        .unwrap();
        assert!(selection.check_environment("verified-server").is_ok());
        assert!(selection.check_environment("replacement-server").is_err());
        assert!(selection.check_environment("").is_err());
        assert!(
            Selection::select("verified-server", "", &root, std::slice::from_ref(&project))
                .is_err()
        );
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
