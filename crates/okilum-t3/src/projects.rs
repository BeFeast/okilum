//! Vault-wide project discovery. Manifests and role registries remain canonical;
//! discovery never creates a thread or writes to the vault.
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Code,
    Knowledge,
    Hybrid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Manager {
    NotStarted,
    Thread(String),
}

#[derive(Clone, Debug)]
pub struct Project {
    pub folder: PathBuf,
    pub name: String,
    pub kind: Kind,
    pub environment: Option<String>,
    pub manager_project: Option<String>,
    pub manager: Manager,
}

#[derive(Default, Debug)]
pub struct Discovery {
    pub projects: Vec<Project>,
    /// A broken manifest must stay visible, without suppressing healthy projects.
    pub errors: Vec<(PathBuf, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    NotStarted,
    Running(String),
    NotRunning(String),
    Settled,
    Missing,
    Unreachable(String),
}
impl Status {
    pub fn restartable(&self) -> bool {
        matches!(self, Self::NotStarted | Self::Settled | Self::Missing)
    }
    pub fn label(&self) -> String {
        match self {
            Self::NotStarted | Self::Settled | Self::Missing => "Manager не запущен".into(),
            Self::Running(status) => format!("Running · {status}"),
            Self::NotRunning(status) => format!("Not running · {status}"),
            Self::Unreachable(error) => format!("Unreachable · {error}"),
        }
    }
}

/// One live listing per environment/project, shared across area rows.
pub fn statuses(
    projects: &[Project],
    gateways: &std::collections::BTreeMap<String, std::sync::Arc<dyn crate::ManagerGateway>>,
) -> std::collections::BTreeMap<PathBuf, Status> {
    let mut listings = std::collections::BTreeMap::<
        (String, String),
        std::result::Result<Vec<crate::ThreadSummary>, String>,
    >::new();
    let mut result = std::collections::BTreeMap::new();
    for project in projects {
        let state = match &project.manager {
            Manager::NotStarted => Status::NotStarted,
            Manager::Thread(thread) => {
                match project
                    .environment
                    .as_ref()
                    .zip(project.manager_project.as_ref())
                {
                    None => {
                        Status::Unreachable("T3 environment or project is not configured".into())
                    }
                    Some((alias, id)) => {
                        let listing =
                            listings
                                .entry((alias.clone(), id.clone()))
                                .or_insert_with(|| {
                                    let gateway = gateways.get(alias).ok_or_else(|| {
                                        "Connect to T3 to read manager status".to_owned()
                                    })?;
                                    gateway.list_threads(id).map_err(|e| e.to_string())
                                });
                        match listing {
                            Err(error) => Status::Unreachable(error.clone()),
                            Ok(threads) => match threads.iter().find(|t| &t.thread_id == thread) {
                                None => Status::Missing,
                                Some(thread) if thread.settled => Status::Settled,
                                Some(thread)
                                    if matches!(
                                        thread.status.as_str(),
                                        "running" | "preparing" | "queued" | "starting" | "waiting"
                                    ) =>
                                {
                                    Status::Running(thread.status.clone())
                                }
                                Some(thread) => Status::NotRunning(thread.status.clone()),
                            },
                        }
                    }
                }
            }
        };
        result.insert(project.folder.clone(), state);
    }
    result
}

impl Project {
    /// Native fallback for knowledge projects intentionally has no executor
    /// binding. Empty internal executor/model defaults are never actionable.
    pub fn native_binding(&self) -> Result<Option<crate::manifest::AreaBinding>> {
        let source = std::fs::read_to_string(self.folder.join("ops/project.yaml"))?;
        let document: serde_yaml::Value = serde_yaml::from_str(&source)?;
        if self.kind != Kind::Knowledge && document["t3"].get("executor").is_some() {
            let mut binding = crate::manifest::AreaBinding::read(&self.folder)?;
            if let Some(binding) = &mut binding {
                binding.manager.thread_id = match &self.manager {
                    Manager::Thread(id) => Some(id.clone()),
                    Manager::NotStarted => None,
                };
            }
            return Ok(binding);
        }
        let Some(environment) = self.environment.clone() else {
            return Ok(None);
        };
        ensure!(
            document["t3"]["schema_version"].as_u64() == Some(1),
            "unsupported T3 schema version"
        );
        let _: crate::manifest::ManagerBinding =
            serde_yaml::from_value(document["t3"]["manager"].clone())?;
        ensure!(
            !environment.is_empty()
                && environment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)),
            "invalid T3 environment alias"
        );
        let empty_model = || crate::ModelSelection {
            instance_id: String::new(),
            model: String::new(),
            options: Vec::new(),
        };
        Ok(Some(crate::manifest::AreaBinding {
            schema_version: 1,
            environment,
            manager: crate::manifest::ManagerBinding {
                project_id: self.manager_project.clone(),
                thread_id: match &self.manager {
                    Manager::Thread(id) => Some(id.clone()),
                    Manager::NotStarted => None,
                },
                workspace_mode: crate::manifest::WorkspaceMode::Local,
            },
            executor: crate::manifest::ExecutorBinding {
                project_id: String::new(),
            },
            default_models: crate::manifest::DefaultModels {
                manager: empty_model(),
                executor: empty_model(),
            },
        }))
    }

    pub fn read(folder: &Path) -> Result<Self> {
        ensure!(
            std::fs::symlink_metadata(folder.join("ops"))?.is_dir(),
            "Project ops must not be a symlink"
        );
        ensure!(
            std::fs::symlink_metadata(folder.join("ops/project.yaml"))?.is_file(),
            "Project manifest must not be a symlink"
        );
        if let Ok(metadata) = std::fs::symlink_metadata(folder.join("ops/thread-ids.md")) {
            ensure!(
                metadata.is_file(),
                "Manager registry must be a regular file, not a symlink"
            );
        }
        let source = std::fs::read_to_string(folder.join("ops/project.yaml"))?;
        let doc: serde_yaml::Value = serde_yaml::from_str(&source)?;
        ensure!(doc.is_mapping(), "project.yaml must be a mapping");
        let text = |value: &serde_yaml::Value| value.as_str().map(str::to_owned);
        let kind = match doc.get("kind") {
            Some(value) => serde_yaml::from_value(value.clone()).context("invalid project kind")?,
            None => {
                if doc.get("repo").is_some_and(|v| !v.is_null()) {
                    Kind::Code
                } else {
                    Kind::Knowledge
                }
            }
        };
        let manifest_manager = &doc["t3"]["manager"];
        // A present role registry is authoritative, including explicit not-started.
        let manager = match std::fs::read_to_string(folder.join("ops/thread-ids.md")) {
            Ok(registry) => manager_role(&registry)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match manifest_manager.get("thread_id").and_then(|v| v.as_str()) {
                    Some(id) => Manager::Thread(thread_id(id)?),
                    None => Manager::NotStarted,
                }
            }
            Err(e) => return Err(e).context("cannot read manager registry"),
        };
        Ok(Self {
            folder: folder.to_owned(),
            name: text(&doc["name"])
                .or_else(|| text(&doc["slug"]))
                .unwrap_or_else(|| {
                    folder
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                }),
            kind,
            environment: text(&doc["t3"]["environment"]),
            manager_project: text(&manifest_manager["project_id"]),
            manager,
        })
    }
}

/// The one source range used for both role reads and lossless replacements.
/// Fenced examples cannot become operational registry rows.
fn manager_cell(source: &str) -> Result<std::ops::Range<usize>> {
    let mut found = None;
    let mut fenced = false;
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        } else if !fenced && line.trim_start().starts_with('|') {
            let pipes: Vec<_> = line.match_indices('|').map(|(index, _)| index).collect();
            if pipes.len() >= 2 && line[pipes[0] + 1..pipes[1]].trim() == "Manager" {
                ensure!(
                    found.is_none(),
                    "more than one Manager role; no thread selected"
                );
                let end = pipes.get(2).context("Manager row has no thread cell")?;
                found = Some(offset + pipes[1] + 1..offset + end);
            }
        }
        offset += line.len();
    }
    found.context("Manager role is missing from thread-ids.md")
}

/// Exact role + leading inline-code id; links and historical IDs elsewhere in
/// the cell cannot select a thread. Duplicate roles are explicit ambiguity.
pub fn manager_role(registry: &str) -> Result<Manager> {
    let cell = registry[manager_cell(registry)?].trim();
    if cell == "— (not started)" {
        return Ok(Manager::NotStarted);
    }
    let value = cell
        .strip_prefix('`')
        .context("Manager cell must start with an inline-code thread id")?;
    let (id, _) = value
        .split_once('`')
        .context("unterminated Manager thread id")?;
    Ok(Manager::Thread(thread_id(id)?))
}

/// Replace only the current Manager cell, preserving all unrelated source bytes.
pub fn register_manager(source: &str, expected: &Manager, id: &str) -> Result<String> {
    thread_id(id)?;
    ensure!(
        &manager_role(source)? == expected,
        "Manager role changed; retain the created thread and reconcile explicitly"
    );
    let cell = manager_cell(source)?;
    let mut updated = source.to_owned();
    updated.replace_range(cell, &format!(" `{id}` "));
    Ok(updated)
}

fn thread_id(id: &str) -> Result<String> {
    let suffix = id
        .strip_prefix("mcp:")
        .context("Manager id must start with mcp:")?;
    ensure!(
        !suffix.is_empty()
            && suffix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "invalid Manager thread id"
    );
    Ok(id.to_owned())
}

/// Walk every domain, excluding symlinks and vault service directories. Never
/// follow a project manifest outside the canonical vault through a symlink.
pub fn discover(vault: &Path) -> Discovery {
    let mut result = Discovery::default();
    let mut pending = vec![vault.to_owned()];
    while let Some(folder) = pending.pop() {
        let manifest = folder.join("ops/project.yaml");
        if manifest.exists() {
            let read = (|| {
                ensure!(
                    std::fs::symlink_metadata(folder.join("ops"))?.is_dir(),
                    "project ops must be a directory, not a symlink"
                );
                ensure!(
                    std::fs::symlink_metadata(&manifest)?.is_file(),
                    "project manifest must be a file, not a symlink"
                );
                Project::read(&folder)
            })();
            match read {
                Ok(project) => result.projects.push(project),
                Err(e) => result.errors.push((folder.clone(), format!("{e:#}"))),
            }
        }
        let entries = match std::fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(e) => {
                result.errors.push((folder, e.to_string()));
                continue;
            }
        };
        for entry in entries {
            match entry {
                Ok(entry) => match entry.file_type() {
                    Ok(kind)
                        if kind.is_dir()
                            && !matches!(
                                entry.file_name().to_str(),
                                Some(
                                    ".git"
                                        | ".obsidian"
                                        | ".trash"
                                        | ".stfolder"
                                        | ".stversions"
                                        | ".okilum-index"
                                        | ".okilum"
                                        | "node_modules"
                                )
                            ) =>
                    {
                        pending.push(entry.path())
                    }
                    Ok(_) => {}
                    Err(e) => result.errors.push((entry.path(), e.to_string())),
                },
                Err(e) => result.errors.push((folder.clone(), e.to_string())),
            }
        }
    }
    result
        .projects
        .sort_by(|a, b| a.name.cmp(&b.name).then(a.folder.cmp(&b.folder)));
    result
}

/// Public T3 descriptor: browser login stays inside WKWebView and does not
/// require copying cookies or obtaining an MCP bearer token first.
pub fn browser_identity(environment: &crate::Environment) -> Result<crate::ServerIdentity> {
    let http = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let url = url::Url::parse(&environment.origin())?.join("/.well-known/t3/environment")?;
    http.get(url)
        .send()?
        .error_for_status()?
        .json()
        .context("T3 environment descriptor is unreadable")
}

/// Browser route, distinct from the desktop t3code: deep link.
pub fn manager_url(
    environment: &crate::Environment,
    identity: &crate::ServerIdentity,
    thread: &str,
) -> Result<url::Url> {
    thread_id(thread)?;
    ensure!(
        !identity.environment_id.is_empty(),
        "T3 environment identity is missing"
    );
    let mut url = url::Url::parse(&environment.origin())?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("invalid T3 origin"))?
        .push(&identity.environment_id)
        .push(thread);
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fenced_example_before_registry_never_receives_the_operational_id() {
        let source = "# Format example\r\n```markdown\r\n| Manager | `mcp:example` |\r\n```\r\n\r\n# Current registry\r\n| Manager | — (not started) | today |\r\n";
        assert_eq!(manager_role(source).unwrap(), Manager::NotStarted);
        let updated = register_manager(source, &Manager::NotStarted, "mcp:created").unwrap();
        assert_eq!(updated, source.replace("— (not started)", "`mcp:created`"));
        assert_eq!(
            manager_role(&updated).unwrap(),
            Manager::Thread("mcp:created".into())
        );
        assert!(updated.contains("| Manager | `mcp:example` |"));
    }

    #[test]
    fn browser_route_uses_the_configured_origin_and_server_environment() {
        let env = crate::Environment::new("test", "https://example.com").unwrap();
        let identity = crate::ServerIdentity {
            environment_id: "environment-1".into(),
            label: "test".into(),
        };
        assert_eq!(
            manager_url(&env, &identity, "mcp:manager")
                .unwrap()
                .as_str(),
            "https://example.com/environment-1/mcp:manager"
        );
        assert!(manager_url(&env, &identity, "../escape").is_err());
    }

    #[test]
    fn registration_preserves_other_cells_and_crlf_and_rejects_a_changed_role() {
        let source = "# Registry\r\n| Manager | — (not started) | today |\r\n| UI | `mcp:ui` | unchanged |\r\n";
        assert_eq!(
            register_manager(source, &Manager::NotStarted, "mcp:new").unwrap(),
            source.replace("— (not started)", "`mcp:new`")
        );
        assert!(register_manager(source, &Manager::Thread("mcp:other".into()), "mcp:new").is_err());
    }

    #[test]
    fn role_is_exact_current_and_unambiguous() {
        let registry = "| UI | `mcp:old` |\n| Manager | `mcp:current` [title](t3-thread://v1/mcp:current), previous `mcp:old` |";
        assert_eq!(
            manager_role(registry).unwrap(),
            Manager::Thread("mcp:current".into())
        );
        assert_eq!(
            manager_role("| Manager | — (not started) |").unwrap(),
            Manager::NotStarted
        );
        for bad in [
            "| Manager | [mcp:wrong](url) |",
            "| Manager | `mcp:one` |\n| Manager | `mcp:two` |",
            "| Former Manager | `mcp:old` |",
            "```\n| Manager | `mcp:example` |\n```",
            "| Manager | `mcp:../escape` |",
        ] {
            assert!(manager_role(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn discovery_crosses_domains_and_keeps_broken_projects_visible() {
        let vault = std::env::temp_dir().join(format!("okilum-projects-{}", uuid::Uuid::new_v4()));
        let write = |folder: &str, source: &str| {
            let path = vault.join(folder).join("ops");
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("project.yaml"), source).unwrap();
        };
        write("Music/Areas/Studio", "name: Studio\nkind: knowledge\nt3:\n  schema_version: 1\n  environment: maestro\n  manager: {project_id: brain, thread_id: null, workspace_mode: local}\n");
        write(
            "Dev/Areas/App",
            "name: App\nrepo: {name: app}\nt3:\n  manager: {thread_id: 'mcp:stale'}\n",
        );
        std::fs::write(
            vault.join("Dev/Areas/App/ops/thread-ids.md"),
            "| Manager | `mcp:rotated` |\n",
        )
        .unwrap();
        write("Life/Projects/Plan", "name: Plan\n");
        write("HomeLab/Projects/Broken", "kind: nonsense\n");
        std::fs::create_dir_all(vault.join("Dev/Areas/NotAProject")).unwrap();
        let result = discover(&vault);
        assert_eq!(result.projects.len(), 3);
        assert_eq!(result.errors.len(), 1);
        let app = &result.projects[0];
        assert_eq!(app.kind, Kind::Code);
        assert_eq!(app.manager, Manager::Thread("mcp:rotated".into()));
        assert_eq!(result.projects[1].kind, Kind::Knowledge);
        let studio = &result.projects[2];
        assert_eq!(studio.kind, Kind::Knowledge);
        assert_eq!(studio.manager, Manager::NotStarted);
        assert_eq!(studio.manager_project.as_deref(), Some("brain"));
        let binding = studio.native_binding().unwrap().unwrap();
        assert!(binding.executor.project_id.is_empty());
        assert_eq!(binding.manager.project_id.as_deref(), Some("brain"));
        assert!(binding.manager.thread_id.is_none());
        // An authoritative explicit empty row overrides the old manifest id.
        std::fs::write(
            vault.join("Dev/Areas/App/ops/thread-ids.md"),
            "| Manager | — (not started) |\n",
        )
        .unwrap();
        assert_eq!(
            Project::read(&app.folder).unwrap().manager,
            Manager::NotStarted
        );
        std::fs::remove_dir_all(vault).unwrap();
    }
}
