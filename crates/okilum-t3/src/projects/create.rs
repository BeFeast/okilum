//! A new project from a vault folder (#1475): «Make this a project…» on a
//! folder in the tree and «+ New project…» under the Projects list.
//!
//! The dialog's state is a [`Draft`]: name, kind, icon and, for a code or
//! hybrid project, where its code is. [`create`] writes `ops/project.yaml`
//! (and `_index.md` when the folder has none), the manifest the Projects
//! list reads. Creating the T3 project is a separate, optional step
//! ([`create_in_t3`]); when it succeeds, [`link_t3`] adds the T3 binding to
//! the manifest. A failed T3 step leaves the written manifest alone.
//!
//! The manager thread and its rotation are not part of this step.
use super::{icon::ProjectIcon, Kind, Project};
use anyhow::{bail, ensure, Context, Result};
use std::io::Write as _;
use std::path::Path;

/// The manifest, relative to the project folder.
pub const MANIFEST: &str = "ops/project.yaml";
/// The folder's entry note.
pub const INDEX: &str = "_index.md";
/// The kinds in the dialog's order.
pub const KINDS: [Kind; 3] = [Kind::Code, Kind::Knowledge, Kind::Hybrid];

pub fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Code => "Code",
        Kind::Knowledge => "Knowledge",
        Kind::Hybrid => "Hybrid",
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Code => "code",
        Kind::Knowledge => "knowledge",
        Kind::Hybrid => "hybrid",
    }
}

/// «Create the project in T3» starts on for a code project, off otherwise.
pub fn t3_default(kind: Kind) -> bool {
    kind == Kind::Code
}

/// Only code and hybrid projects say where their code is.
pub fn has_source(kind: Kind) -> bool {
    kind != Kind::Knowledge
}

/// Where a code project's code is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// A checkout directory on the T3 host: T3's workspace root.
    Checkout(String),
    /// The repository's URL.
    Repo(String),
}

/// A URL (`https://`, `ssh://`, `git@host:`) is a repository; anything else
/// is a checkout path. Blank is none.
pub fn parse_source(text: &str) -> Option<Source> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let url = ["https://", "http://", "ssh://", "git://", "git@"]
        .iter()
        .any(|prefix| text.starts_with(prefix));
    Some(if url {
        Source::Repo(text.to_owned())
    } else {
        Source::Checkout(text.trim_end_matches('/').to_owned())
    })
}

/// What the «New project» dialog holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub name: String,
    pub kind: Kind,
    /// `None`: the automatic logo or monogram.
    pub icon: Option<ProjectIcon>,
    /// For code and hybrid: a checkout path on the T3 host or a repo URL.
    pub source: String,
    create_in_t3: bool,
    /// The user set the T3 checkbox; a kind change no longer moves it.
    t3_chosen: bool,
}

impl Draft {
    /// The defaults for `folder`: its name, a knowledge project, no T3.
    pub fn for_folder(folder: &Path) -> Self {
        let kind = Kind::Knowledge;
        Self {
            name: folder_name(folder),
            kind,
            icon: None,
            source: String::new(),
            create_in_t3: t3_default(kind),
            t3_chosen: false,
        }
    }

    pub fn create_in_t3(&self) -> bool {
        self.create_in_t3
    }

    /// A new kind; the T3 checkbox follows it until the user sets it.
    pub fn set_kind(&mut self, kind: Kind) {
        self.kind = kind;
        if !self.t3_chosen {
            self.create_in_t3 = t3_default(kind);
        }
    }

    pub fn set_create_in_t3(&mut self, on: bool) {
        self.create_in_t3 = on;
        self.t3_chosen = true;
    }

    /// The code's location, for the kinds that have one.
    pub fn source(&self) -> Option<Source> {
        has_source(self.kind)
            .then(|| parse_source(&self.source))
            .flatten()
    }

    /// What the T3 project's workspace root is, when it is not the folder.
    fn checkout(&self) -> Option<String> {
        match self.source() {
            Some(Source::Checkout(path)) if self.kind == Kind::Code => Some(path),
            _ => None,
        }
    }

    /// Why Create cannot go ahead, in the dialog's words.
    pub fn problem(&self) -> Option<String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Some("Enter a name.".into());
        }
        if name.chars().count() > 120 || name.chars().any(char::is_control) {
            return Some("Use a name of at most 120 characters on one line.".into());
        }
        match self.source() {
            Some(Source::Checkout(path)) if !path.starts_with('/') => {
                return Some(
                    "The checkout path is a folder on the T3 host, such as /home/me/src/app."
                        .into(),
                )
            }
            Some(Source::Checkout(path)) | Some(Source::Repo(path))
                if path.chars().any(|c| c.is_control()) =>
            {
                return Some("The checkout path or repository is one line.".into())
            }
            _ => {}
        }
        if self.kind == Kind::Code && self.create_in_t3 && self.checkout().is_none() {
            return Some(
                "T3 opens a code project in its checkout: enter the checkout path on the T3 host."
                    .into(),
            );
        }
        None
    }
}

pub fn folder_name(folder: &Path) -> String {
    folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A folder that already has `ops/project.yaml` is a project: the tree
/// does not offer «Make this a project…» for it.
pub fn is_project(folder: &Path) -> bool {
    ["project.toml", "project.yaml"]
        .iter()
        .any(|name| std::fs::symlink_metadata(folder.join("ops").join(name)).is_ok())
}

/// `HomeLab Mesh` → `homelab-mesh`: lowercase ASCII letters and digits,
/// everything else one `-`.
pub fn slug(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "project".into()
    } else {
        slug.into()
    }
}

/// A YAML double-quoted scalar (JSON's string syntax is a subset of it).
fn quoted(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into())
}

/// The manifest for `draft` in the vault folder `relative`: the fields the
/// Projects list reads, readable by hand.
pub fn manifest(draft: &Draft, relative: &str) -> String {
    let name = draft.name.trim();
    let domain = relative.split('/').next().unwrap_or(relative);
    let mut out = String::new();
    out.push_str("# Okilum project manifest, created in Okilum (okilum#1475).\n");
    out.push_str("schema_version: 1\n");
    out.push_str(&format!("slug: {}\n", slug(name)));
    out.push_str(&format!("name: {}\n", quoted(name)));
    out.push_str(&format!(
        "kind: {}   # code | knowledge | hybrid\n",
        kind_name(draft.kind)
    ));
    out.push_str(&format!("domain: {}\n", quoted(domain)));
    out.push_str(&format!(
        "index: {}\n",
        quoted(&format!("{relative}/{INDEX}"))
    ));
    if let Some(icon) = &draft.icon {
        out.push_str(&format!("icon: {}\n", icon.to_yaml_flow()));
    }
    match draft.source() {
        Some(Source::Checkout(path)) => {
            out.push_str("execution:\n");
            out.push_str(&format!("  source_checkout: {}\n", quoted(&path)));
        }
        Some(Source::Repo(url)) => {
            out.push_str("repo:\n");
            out.push_str(&format!("  clone_url: {}\n", quoted(&url)));
        }
        None => {}
    }
    out
}

/// What [`create`] wrote.
#[derive(Clone, Debug)]
pub struct Created {
    /// The project as the Projects list reads it.
    pub project: Project,
    /// `_index.md` was missing and is now there.
    pub wrote_index: bool,
}

/// Write a new file; never replace one. A file this call created but could
/// not finish is removed again.
fn write_new(path: &Path, text: &str) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all());
    if written.is_err() {
        drop(file);
        let _ = std::fs::remove_file(path);
    }
    written
}

/// Turn `folder` (inside `vault`) into a project: `ops/project.yaml`, and
/// `_index.md` with the name as its H1 when the folder has none. A folder
/// that already is a project is refused and nothing is written.
pub fn create(vault: &Path, folder: &Path, draft: &Draft) -> Result<Created> {
    if let Some(problem) = draft.problem() {
        bail!(problem);
    }
    let relative = super::icon::vault_relative(vault, folder)
        .filter(|relative| !relative.is_empty())
        .context("Choose a folder inside the vault, not the vault itself")?;
    ensure!(
        !relative.split('/').any(|part| part.starts_with('.')),
        "A hidden folder cannot be a project"
    );
    let metadata =
        std::fs::symlink_metadata(folder).with_context(|| format!("{relative} does not exist"))?;
    ensure!(metadata.is_dir(), "{relative} is not a folder");
    ensure!(
        !is_project(folder),
        "{relative} is already a project: it has {MANIFEST}"
    );
    let ops = folder.join("ops");
    let created_ops = match std::fs::symlink_metadata(&ops) {
        Ok(metadata) => {
            ensure!(
                metadata.is_dir(),
                "{relative}/ops is not a folder (a symlink or a file)"
            );
            false
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(&ops).with_context(|| format!("cannot create {relative}/ops"))?;
            true
        }
        Err(error) => return Err(error).with_context(|| format!("cannot read {relative}/ops")),
    };
    let rollback_ops = || {
        if created_ops {
            // Only an empty folder goes: never anything someone else put there.
            let _ = std::fs::remove_dir(&ops);
        }
    };
    let manifest_path = folder.join(MANIFEST);
    write_new(&manifest_path, &manifest(draft, &relative)).map_err(|error| {
        rollback_ops();
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            anyhow::anyhow!("{relative} is already a project: it has {MANIFEST}")
        } else {
            anyhow::Error::new(error).context(format!("cannot write {relative}/{MANIFEST}"))
        }
    })?;
    // From here a failure takes the manifest back, so the folder is not left
    // half made and refused as «already a project» on the next try.
    finish(folder, draft, &relative).inspect_err(|_| {
        let _ = std::fs::remove_file(&manifest_path);
        rollback_ops();
    })
}

/// The rest of [`create`] once the manifest is written: `_index.md` when the
/// folder has none, then the project as the Projects list reads it.
fn finish(folder: &Path, draft: &Draft, relative: &str) -> Result<Created> {
    let index = folder.join(INDEX);
    let wrote_index = match std::fs::symlink_metadata(&index) {
        Ok(_) => false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match write_new(&index, &format!("# {}\n", draft.name.trim())) {
                Ok(()) => true,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
                Err(error) => {
                    return Err(error).with_context(|| format!("cannot write {relative}/{INDEX}"))
                }
            }
        }
        Err(error) => return Err(error).with_context(|| format!("cannot read {relative}/{INDEX}")),
    };
    Ok(Created {
        project: Project::read(folder)?,
        wrote_index,
    })
}

/// The T3 project a vault project is bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct T3Link {
    /// The connection alias (`t3.environment`), a saved T3 connection.
    pub environment: String,
    pub project_id: String,
}

/// The `t3:` block for a project with a T3 project and no manager yet.
/// `executor_project_id` is what binds the icon to T3's; the manager pair
/// stays null until a manager is started.
pub fn t3_block(link: &T3Link) -> String {
    format!(
        "# T3 project created by Okilum (#1475); no manager yet.\n\
         t3:\n  schema_version: 1\n  environment: {}\n  executor_project_id: {}\n  \
         manager: {{project_id: null, thread_id: null, workspace_mode: local}}\n",
        quoted(&link.environment),
        quoted(&link.project_id),
    )
}

/// Add the `t3:` block to `folder`'s manifest, every other byte kept.
/// Refused when the manifest has a `t3:` block already.
pub fn link_t3(folder: &Path, link: &T3Link) -> Result<Project> {
    let document =
        crate::area_files::read(folder, "project")?.context("Area has no project manifest")?;
    ensure!(
        document.value.get("t3").is_none(),
        "Project manifest already has a t3 block"
    );
    let updated = match document.format {
        crate::area_files::Format::Yaml => {
            let mut updated = document.source.clone();
            if !updated.is_empty() && !updated.ends_with('\n') {
                updated.push('\n');
            }
            updated.push_str(&t3_block(link));
            let after: serde_yaml::Value = serde_yaml::from_str(&updated)?;
            let mut others = after.as_mapping().context("Expected a mapping")?.clone();
            others.remove(serde_yaml::Value::from("t3"));
            ensure!(
                Some(&others) == document.value.as_mapping(),
                "Adding T3 would change unrelated manifest fields"
            );
            updated
        }
        crate::area_files::Format::Toml => {
            let value: serde_yaml::Value = serde_yaml::from_str(&t3_block(link))?;
            document.set_toml_field("t3", value.get("t3"))?
        }
    };
    document.replace(&updated)?;
    let project = Project::read(folder)?;
    ensure!(
        project.environment.as_deref() == Some(link.environment.as_str())
            && project.appearance.executor_project.as_deref() == Some(link.project_id.as_str()),
        "The T3 project did not read back from the manifest"
    );
    Ok(project)
}

/// Where this vault is on the T3 host. T3 projects rooted in vault
/// folders (`…/Obsidian Vault/HomeLab`) name it: the part of the root in
/// front of a folder this vault has. The answer most roots agree on wins.
pub fn server_vault_root(vault: &Path, listing: &[crate::Project]) -> Option<String> {
    let mut votes = std::collections::BTreeMap::<String, usize>::new();
    for project in listing {
        let root = project
            .workspace_root
            .trim_end_matches(['/', '\\'])
            .replace('\\', "/");
        let parts: Vec<&str> = root.split('/').collect();
        for start in 1..parts.len() {
            let suffix = &parts[start..];
            if suffix
                .iter()
                .any(|part| part.is_empty() || part.starts_with('.'))
            {
                continue;
            }
            let prefix = parts[..start].join("/");
            if prefix.is_empty() {
                continue;
            }
            let local = suffix.iter().fold(vault.to_path_buf(), |p, s| p.join(s));
            if std::fs::symlink_metadata(&local).is_ok_and(|m| m.is_dir()) {
                *votes.entry(prefix).or_default() += 1;
                break;
            }
        }
    }
    votes
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.len().cmp(&a.0.len())))
        .map(|(prefix, _)| prefix)
}

/// The T3 project's workspace root: the checkout for a code project, the
/// vault folder on the T3 host otherwise. When no T3 project shows where
/// the vault is there, the folder's own path is used (T3 on this machine).
pub fn t3_workspace_root(
    draft: &Draft,
    vault: &Path,
    folder: &Path,
    listing: &[crate::Project],
) -> Result<String> {
    if draft.kind == Kind::Code {
        return draft
            .checkout()
            .context("T3 opens a code project in its checkout: enter the checkout path");
    }
    let relative = super::icon::vault_relative(vault, folder)
        .filter(|relative| !relative.is_empty())
        .context("The folder is outside the vault")?;
    if let Some(root) = server_vault_root(vault, listing) {
        return Ok(format!("{root}/{relative}"));
    }
    let local = folder.to_string_lossy().replace('\\', "/");
    ensure!(
        local.starts_with('/'),
        "Cannot tell where this vault is on the T3 host: no T3 project is rooted in it"
    );
    Ok(local)
}

/// What [`create_in_t3`] did.
#[derive(Clone, Debug)]
pub struct T3Created {
    pub project: crate::Project,
    /// T3 had a project at that root already; it was used, not duplicated.
    pub reused: bool,
    /// The project is there but T3 did not take the icon.
    pub icon_error: Option<String>,
}

fn same_root(a: &str, b: &str) -> bool {
    let trim = |s: &str| s.trim().trim_end_matches(['/', '\\']).to_owned();
    let (a, b) = (trim(a), trim(b));
    !a.is_empty() && a == b
}

/// Create the T3 project for `draft` with the paired connection: T3's
/// `project.create` at [`t3_workspace_root`], then the icon with T3's
/// `project.update`. A T3 project already at that root is used instead of
/// a second one. Blocks; run it off the UI thread.
pub fn create_in_t3(
    gateway: &dyn crate::ManagerGateway,
    draft: &Draft,
    vault: &Path,
    folder: &Path,
) -> Result<T3Created> {
    let listing = gateway
        .list_projects()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let root = t3_workspace_root(draft, vault, folder, &listing)?;
    let (project, reused) = match listing
        .iter()
        .find(|project| same_root(&project.workspace_root, &root))
    {
        Some(project) => (project.clone(), true),
        None => (
            gateway
                .create_project(draft.name.trim(), &root)
                .map_err(|error| anyhow::anyhow!("{error}"))?,
            false,
        ),
    };
    let mut created = T3Created {
        project,
        reused,
        icon_error: None,
    };
    if let Some(icon) = &draft.icon {
        if created.project.project_icon.as_ref() != Some(icon) {
            match gateway.set_project_icon(&created.project.id, Some(icon)) {
                Ok(project) => created.project = project,
                Err(error) => created.icon_error = Some(error.to_string()),
            }
        }
    }
    Ok(created)
}

/// The saved T3 connection a new project uses: the one most projects in
/// this vault already name, or the first saved one.
pub fn pick_environment<'a>(
    aliases: impl IntoIterator<Item = &'a str>,
    projects: &[Project],
) -> Option<String> {
    let aliases: Vec<&str> = aliases.into_iter().collect();
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for project in projects {
        if let Some(alias) = project.environment.as_deref() {
            if aliases.contains(&alias) {
                *counts.entry(alias).or_default() += 1;
            }
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(alias, _)| alias.to_owned())
        .or_else(|| aliases.first().map(|alias| (*alias).to_owned()))
}

#[cfg(test)]
mod tests;
