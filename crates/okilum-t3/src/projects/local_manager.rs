//! Explicit local T3 enrollment. Checking a connection never launches a thread.
use super::{Kind, Manager, Project};
use crate::{
    area_files::{self, Document, Format},
    Environment, ManagerGateway,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use std::path::{Path, PathBuf};

/// An address on this device, never the saved remote app default.
pub fn environment(input: &str) -> Result<Environment> {
    let env = Environment::from_input("local-manager", input)?;
    let url = url::Url::parse(&env.origin())?;
    let local = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain("localhost")) => true,
        _ => false,
    };
    ensure!(local, "Use the local T3 address on this device (localhost or a loopback IP), not the remote server");
    Ok(env)
}

fn directory(path: &Path) -> Result<PathBuf> {
    ensure!(
        path.is_absolute(),
        "Choose an absolute workspace folder on this device"
    );
    let path = path.canonicalize().with_context(|| {
        format!(
            "Workspace {} is not available on this device",
            path.display()
        )
    })?;
    ensure!(path.is_dir(), "Workspace must be a folder on this device");
    Ok(path)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub environment_id: String,
    pub root: String,
}

/// Snapshot the file the form opened. A concurrent edit cannot be overwritten.
#[derive(Clone, Debug)]
pub struct Session {
    area: PathBuf,
    document: Document,
    pub project: Project,
}
impl Session {
    pub fn load(area: &Path) -> Result<Self> {
        let document =
            area_files::read(area, "project")?.context("Create the local project first")?;
        let project = Project::read(area)?;
        ensure!(
            document.value.get("t3").is_none_or(Value::is_mapping),
            "The existing t3 field must be a mapping; it was not replaced"
        );
        ensure!(project.manager == Manager::NotStarted && project.manager_project.is_none(),
            "This project already has a Manager binding; use its existing T3 connection and retained-launch recovery");
        ensure!(
            document.value["t3"].get("executor").is_none(),
            "This project already has an executor binding; keep its existing T3 connection"
        );
        if let Some(value) = document.value.get("t3") {
            field(&document, "t3", value.clone())?;
        }
        if project.kind != Kind::Knowledge {
            if let Some(value) = document.value.get("execution") {
                ensure!(
                    value.is_mapping(),
                    "The existing execution field must be a mapping; it was not replaced"
                );
                field(&document, "execution", value.clone())?;
            }
        }
        Ok(Self {
            area: area.into(),
            document,
            project,
        })
    }

    pub fn suggested_workspace(&self) -> String {
        self.project
            .appearance
            .source_checkout
            .clone()
            .unwrap_or_else(|| self.area.to_string_lossy().into_owned())
    }

    fn current(&self) -> Result<()> {
        let actual = area_files::read(&self.area, "project")?
            .context("Project disappeared; reopen setup")?;
        ensure!(
            actual.path == self.document.path && actual.source == self.document.source,
            "Project changed; reopen local Manager setup before saving"
        );
        let project = Project::read(&self.area)?;
        ensure!(
            project.manager == self.project.manager
                && project.manager_project == self.project.manager_project,
            "Manager changed; reopen local Manager setup"
        );
        Ok(())
    }

    /// Reuse an exact workspace, or create it once using T3's unique-root API.
    /// A lost create reply is reconciled by the next complete project listing.
    pub fn bind(
        &self,
        gateway: &dyn ManagerGateway,
        env: &Environment,
        workspace: &str,
    ) -> Result<Project> {
        ensure!(
            environment(&env.origin())? == *env,
            "Local T3 connection changed; check setup again"
        );
        let root = directory(Path::new(workspace.trim()))?;
        let root = root.to_str().context("Workspace path is not UTF-8")?;
        self.current()?;
        let identity = gateway.server_identity()?;
        ensure!(
            !identity.environment_id.trim().is_empty(),
            "Local T3 did not return its environment identity"
        );
        let listing = gateway.list_projects()?;
        let matches: Vec<_> = listing
            .iter()
            .filter(|p| directory(Path::new(&p.workspace_root)).is_ok_and(|p| p == Path::new(root)))
            .collect();
        ensure!(matches.len() <= 1, "Local T3 lists multiple projects at this workspace; choose the project in T3 before reconnecting");
        self.current()?;
        let target = match matches.first() {
            Some(project) => (*project).clone(),
            None => gateway.create_project(&self.project.name, root)?,
        };
        ensure!(
            !target.id.trim().is_empty(),
            "Local T3 project receipt has no id; check Projects in T3 before retrying"
        );
        let target_id = target.id.clone();
        let target = gateway.read_project(&target_id)?;
        ensure!(
            target.id == target_id,
            "Local T3 returned a different project; binding was not saved"
        );
        let verified = Workspace {
            environment_id: identity.environment_id,
            root: root.into(),
        };
        verified.check(gateway, &target)?;
        self.current()?;
        let mut t3 = self
            .document
            .value
            .get("t3")
            .and_then(Value::as_mapping)
            .cloned()
            .unwrap_or_default();
        for (key, value) in [
            ("schema_version", serde_yaml::to_value(1)?),
            ("environment", Value::from(env.alias.as_str())),
            ("origin", Value::from(env.origin())),
            ("executor_project_id", Value::from(target.id.as_str())),
            (
                "manager",
                serde_yaml::to_value(
                    serde_json::json!({"project_id":target.id,"thread_id":null,"workspace_mode":"local"}),
                )?,
            ),
            ("local_workspace", serde_yaml::to_value(&verified)?),
        ] {
            t3.insert(Value::from(key), value);
        }
        let mut updated = field(&self.document, "t3", Value::Mapping(t3))?;
        if self.project.kind != Kind::Knowledge {
            let mut document = self.document.clone();
            document.source = updated;
            document.value = area_files::parse(&document.source, document.format)?;
            ensure!(
                document
                    .value
                    .get("execution")
                    .is_none_or(Value::is_mapping),
                "The existing execution field must be a mapping; it was not replaced"
            );
            updated = checkout(&document, root)?;
        }
        ensure!(
            updated.len() as u64 <= area_files::MAX_BYTES,
            "Project manifest exceeds 512 KiB; not written"
        );
        self.current()?;
        self.document.replace(&updated).with_context(|| format!("Local T3 project {} exists, but the binding was not saved; reopen setup to reuse it", target.id))?;
        let project = Project::read(&self.area)?;
        ensure!(
            project.manager_project.as_deref() == Some(target.id.as_str()),
            "Manager binding did not read back"
        );
        Ok(project)
    }
}

impl Workspace {
    fn check(&self, gateway: &dyn ManagerGateway, project: &crate::Project) -> Result<()> {
        ensure!(
            !self.environment_id.trim().is_empty(),
            "Local T3 environment identity is missing; reopen setup"
        );
        ensure!(
            gateway.server_identity()?.environment_id == self.environment_id,
            "Local T3 environment changed; check the connection before starting a Manager"
        );
        ensure!(
            directory(Path::new(&project.workspace_root))? == directory(Path::new(&self.root))?,
            "T3 workspace changed; reconnect the Manager to the checkout on this device"
        );
        Ok(())
    }
}

/// Recheck the pinned local endpoint, server identity and exact workspace before
/// the existing retained thread launch. Legacy bindings keep their own path.
pub fn verify_launch(
    area: &Path,
    env: &Environment,
    gateway: &dyn ManagerGateway,
    project_id: &str,
) -> Result<Option<Workspace>> {
    let document = area_files::read(area, "project")?.context("Project disappeared")?;
    let Some(value) = document.value["t3"].get("local_workspace") else {
        return Ok(None);
    };
    let workspace: Workspace =
        serde_yaml::from_value(value.clone()).context("Invalid local Manager workspace binding")?;
    ensure!(
        environment(&env.origin())? == *env
            && document.value["t3"]["origin"].as_str() == Some(env.origin().as_str()),
        "Local Manager origin changed; reconnect before starting"
    );
    let project = gateway.read_project(project_id)?;
    ensure!(
        project.id == project_id,
        "Local T3 returned a different Manager project"
    );
    let owner = Project::read(area)?;
    if owner.kind != Kind::Knowledge {
        let checkout = owner
            .appearance
            .source_checkout
            .as_deref()
            .context("Local source checkout is missing; reopen setup")?;
        ensure!(
            directory(Path::new(checkout))? == directory(Path::new(&workspace.root))?,
            "Project checkout changed; reconnect the local Manager workspace"
        );
    }
    workspace.check(gateway, &project)?;
    Ok(Some(workspace))
}

pub fn workspace(area: &Path) -> Result<Option<Workspace>> {
    let document = area_files::read(area, "project")?.context("Project disappeared")?;
    document.value["t3"]
        .get("local_workspace")
        .map(|v| {
            serde_yaml::from_value(v.clone()).context("Invalid local Manager workspace binding")
        })
        .transpose()
}

fn checkout(document: &Document, root: &str) -> Result<String> {
    if document.value["execution"]["source_checkout"].as_str() == Some(root) {
        return Ok(document.source.clone());
    }
    let mut execution = document
        .value
        .get("execution")
        .and_then(Value::as_mapping)
        .cloned()
        .unwrap_or_default();
    execution.insert(Value::from("source_checkout"), Value::from(root));
    if document.format == Format::Toml {
        let mut doc = document.source.parse::<toml_edit::DocumentMut>()?;
        if doc.get("execution").is_none() {
            doc["execution"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        let mut value = toml_edit::Value::from(root);
        if let Some(old) = doc["execution"]
            .get("source_checkout")
            .and_then(toml_edit::Item::as_value)
        {
            *value.decor_mut() = old.decor().clone();
        }
        doc["execution"]["source_checkout"] = toml_edit::Item::Value(value);
        let source = doc.to_string();
        let after = area_files::parse(&source, Format::Toml)?;
        let mut expected = document.value.clone();
        expected["execution"] = Value::Mapping(execution);
        ensure!(
            after == expected,
            "Updating the checkout would change unrelated project fields"
        );
        return Ok(source);
    }
    // A single-field block has no unrelated siblings to preserve.
    if execution.len() == 1 {
        return field(document, "execution", Value::Mapping(execution));
    }
    let lines: Vec<_> = document.source.split_inclusive('\n').collect();
    let start=lines.iter().position(|l|l.strip_prefix("execution:").is_some_and(|s|s.trim().is_empty() || s.trim_start().starts_with('#'))).context("Use a block mapping for execution in the project manifest before changing its checkout")?;
    let end = (start + 1..lines.len())
        .find(|&i| {
            !lines[i].trim().is_empty()
                && !lines[i].starts_with(char::is_whitespace)
                && !lines[i].starts_with('#')
        })
        .unwrap_or(lines.len());
    let indent = lines[start + 1..end]
        .iter()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .context("Invalid execution mapping")?;
    let selected: Vec<_> = (start + 1..end)
        .filter(|&i| {
            lines[i].len() - lines[i].trim_start().len() == indent
                && lines[i].trim_start().starts_with("source_checkout:")
        })
        .collect();
    ensure!(
        selected.len() <= 1,
        "Ambiguous source_checkout layout; project manifest was not rewritten"
    );
    let (a, b) = match selected.first().copied() {
        Some(a) => {
            let b = (a + 1..end)
                .find(|&i| {
                    !lines[i].trim().is_empty()
                        && lines[i].len() - lines[i].trim_start().len() <= indent
                })
                .unwrap_or(end);
            (a, b)
        }
        None => (end, end),
    };
    let begin: usize = lines[..a].iter().map(|l| l.len()).sum();
    let finish: usize = lines[..b].iter().map(|l| l.len()).sum();
    let mut source = document.source.clone();
    source.replace_range(
        begin..finish,
        &format!(
            "{}source_checkout: {}\n",
            " ".repeat(indent),
            serde_json::to_string(root)?
        ),
    );
    let after = area_files::parse(&source, Format::Yaml)?;
    let mut expected = document.value.clone();
    expected["execution"] = Value::Mapping(execution);
    ensure!(
        after == expected,
        "Updating the checkout would change unrelated project fields"
    );
    Ok(source)
}

/// Replace only the selected root field. YAML keeps every other source byte;
/// unusual quoted/explicit-key layouts are refused instead of reformatted.
fn field(document: &Document, key: &str, value: Value) -> Result<String> {
    if document.format == Format::Toml {
        return document.set_toml_field(key, Some(&value));
    }
    let replacement =
        serde_yaml::to_string(&serde_yaml::Mapping::from_iter([(Value::from(key), value)]))?;
    let mut source = document.source.clone();
    if document.value.get(key).is_none() {
        if !source.ends_with('\n') {
            source.push('\n');
        }
        source.push_str(&replacement);
    } else {
        let lines: Vec<_> = source.split_inclusive('\n').collect();
        let starts: Vec<_> = lines
            .iter()
            .enumerate()
            .filter_map(|(i, l)| {
                l.strip_prefix(&format!("{key}:"))
                    .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
                    .map(|_| i)
            })
            .collect();
        ensure!(starts.len() == 1, "Edit {key} in {} as a plain root mapping before setup; unusual YAML layout was not rewritten", document.path.display());
        let start = starts[0];
        let mut end = start + 1;
        while end < lines.len()
            && (lines[end].trim().is_empty()
                || lines[end].starts_with(char::is_whitespace)
                || lines[end].starts_with('#'))
        {
            end += 1;
        }
        while end > start + 1
            && (lines[end - 1].trim().is_empty() || lines[end - 1].starts_with('#'))
        {
            end -= 1;
        }
        let a: usize = lines[..start].iter().map(|l| l.len()).sum();
        let b: usize = lines[..end].iter().map(|l| l.len()).sum();
        source.replace_range(a..b, &replacement);
    }
    let after = area_files::parse(&source, document.format)?;
    let mut before = document
        .value
        .as_mapping()
        .context("Project must be a mapping")?
        .clone();
    let mut actual = after
        .as_mapping()
        .context("Project must remain a mapping")?
        .clone();
    before.remove(Value::from(key));
    actual.remove(Value::from(key));
    ensure!(
        before == actual,
        "Updating {key} would change unrelated project fields"
    );
    Ok(source)
}

#[cfg(test)]
mod tests;
