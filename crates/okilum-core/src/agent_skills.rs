//! Bounded, read-only project skill inventory (#1591). No provider execution.
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

pub const MAX_SKILLS: usize = 256;
pub const MAX_SOURCE_BYTES: u64 = 64 * 1024;
pub const MAX_ANCESTORS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    Available,
    Disabled,
    Duplicate,
    Unavailable(String),
}

#[derive(Clone, Debug)]
pub struct Skill {
    /// Exact vault-relative source identity, retained even for unavailable rows.
    pub source: PathBuf,
    pub name: String,
    pub description: String,
    pub availability: Availability,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub skills: Vec<Skill>,
    /// Nonempty means coverage is incomplete; an empty list is not proof of absence.
    pub diagnostics: Vec<String>,
}

/// All inputs that can change the meaning of an inventory. The owner advances
/// `config_revision` even when a failed config reload retains the same paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryScope {
    pub root: PathBuf,
    pub project: PathBuf,
    pub disabled: Vec<PathBuf>,
    pub config_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Freshness {
    NotLoaded,
    Refreshing,
    Fresh,
    Incomplete,
    Stale(String),
    Failed(String),
}

/// Immutable worker input. Its private generation prevents a result for an old
/// project/configuration or an invalidated scan from replacing current state.
#[derive(Clone, Debug)]
pub struct RefreshRequest {
    generation: u64,
    scope: DiscoveryScope,
}

impl RefreshRequest {
    /// Run on the owner's background executor, never the UI thread.
    pub fn discover(&self) -> Snapshot {
        discover(&self.scope.root, &self.scope.project, &self.scope.disabled)
    }
}

/// Session-only projection; no timers, watchers, writes or provider calls.
/// Keep this instance across refreshes. Feed source/config events to `invalidate`
/// or `configure`, and submit a new request through the owner's executor.
pub struct Inventory {
    scope: Option<DiscoveryScope>,
    generation: u64,
    pending: bool,
    config_error: Option<String>,
    snapshot: Option<Snapshot>,
    freshness: Freshness,
}

impl Default for Inventory {
    fn default() -> Self {
        Self {
            scope: None,
            generation: 0,
            pending: false,
            config_error: None,
            snapshot: None,
            freshness: Freshness::NotLoaded,
        }
    }
}

impl Inventory {
    pub fn scope(&self) -> Option<&DiscoveryScope> {
        self.scope.as_ref()
    }
    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.snapshot.as_ref()
    }
    pub fn freshness(&self) -> &Freshness {
        &self.freshness
    }

    /// Returns true when a scan is needed. Project/vault switches clear old rows;
    /// config changes retain rows only as explicitly stale historical information.
    pub fn configure(&mut self, mut scope: DiscoveryScope) -> bool {
        scope.disabled.sort();
        scope.disabled.dedup();
        if self.scope.as_ref() == Some(&scope) && self.config_error.is_none() {
            return false;
        }
        let same_project = self
            .scope
            .as_ref()
            .is_some_and(|old| old.root == scope.root && old.project == scope.project);
        if !same_project {
            self.snapshot = None;
        }
        self.scope = Some(scope);
        self.config_error = None;
        self.invalidate("Project skill configuration changed");
        true
    }

    /// A failed config reload must not rescan using the old disabled policy and
    /// silently claim fresh availability. Reload config successfully and call
    /// `configure` before starting another scan, even if its revision is unchanged.
    pub fn configuration_failed(&mut self, error: impl Into<String>) {
        let error = format!("Skill configuration could not be loaded: {}", error.into());
        self.invalidate(error.clone());
        self.config_error = Some(error.clone());
        self.freshness = Freshness::Failed(error);
    }

    /// Call when leaving project scope or disabling the owning feature. Old
    /// completions are rejected and no previous project's sources remain visible.
    pub fn clear(&mut self) {
        self.invalidate("No project selected");
        self.scope = None;
        self.snapshot = None;
        self.config_error = None;
        self.freshness = Freshness::NotLoaded;
    }

    /// Also call on a watcher overflow, source rename/delete,
    /// or failed Open revalidation. Invalidates any scan already in flight.
    pub fn invalidate(&mut self, reason: impl Into<String>) {
        self.generation = self
            .generation
            .checked_add(1)
            .expect("Skill generation exhausted");
        self.pending = false;
        self.freshness = match &self.config_error {
            Some(error) => Freshness::Failed(error.clone()),
            None => Freshness::Stale(reason.into()),
        };
    }

    /// Explicit Refresh supersedes an earlier request, allowing recovery without
    /// waiting for an obsolete or failed worker to finish.
    pub fn begin_refresh(&mut self) -> Option<RefreshRequest> {
        if self.config_error.is_some() {
            return None;
        }
        let scope = self.scope.clone()?;
        self.invalidate("Refreshing project skills");
        self.pending = true;
        self.freshness = Freshness::Refreshing;
        Some(RefreshRequest {
            generation: self.generation,
            scope,
        })
    }

    /// Returns false for obsolete or repeated completions. Failed refreshes retain
    /// previous rows with a visible error; partial discovery is never called fresh.
    pub fn finish_refresh(
        &mut self,
        request: &RefreshRequest,
        result: Result<Snapshot, String>,
    ) -> bool {
        if !self.pending
            || self.generation != request.generation
            || self.scope.as_ref() != Some(&request.scope)
        {
            return false;
        }
        self.pending = false;
        match result {
            Ok(snapshot) => {
                self.freshness = if snapshot.diagnostics.is_empty() {
                    Freshness::Fresh
                } else {
                    Freshness::Incomplete
                };
                self.snapshot = Some(snapshot);
            }
            Err(error) => self.freshness = Freshness::Failed(error),
        }
        true
    }
}

/// Only immediate children of `.agents/skills` in project ancestors are supported.
/// Disabled paths are explicit vault-relative identities supplied by the caller;
/// no provider configuration or user-global directories are consulted.
pub fn discover(root: &Path, project: &Path, disabled: &[PathBuf]) -> Snapshot {
    let mut result = Snapshot::default();
    let scope = (|| {
        let root = root.canonicalize()?;
        let project = project.canonicalize()?;
        if !project.starts_with(&root) || !project.is_dir() {
            return Err(std::io::Error::other(
                "Project is outside the vault or is not a folder",
            ));
        }
        Ok((root, project))
    })();
    let (root, project) = match scope {
        Ok(scope) => scope,
        Err(error) => {
            result.diagnostics.push(error.to_string());
            return result;
        }
    };
    for (depth, ancestor) in project.ancestors().enumerate() {
        if depth == MAX_ANCESTORS {
            result
                .diagnostics
                .push("Ancestor limit reached; discovery is incomplete".into());
            break;
        }
        let directory = ancestor.join(".agents/skills");
        // Do not enumerate symlinked roots outside the vault.
        match directory.canonicalize() {
            Ok(path) if !path.starts_with(&root) => {
                result.diagnostics.push(format!(
                    "Outside-vault skill directory: {}",
                    directory.display()
                ));
                if ancestor == root {
                    break;
                }
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if ancestor == root {
                    break;
                }
                continue;
            }
            Err(error) => {
                result.diagnostics.push(error.to_string());
                break;
            }
            _ => {}
        }
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                result.diagnostics.push(error.to_string());
                break;
            }
        };
        // Bound enumeration as well as reads, including non-skill entries.
        for (index, entry) in entries.enumerate() {
            if index >= MAX_SKILLS || result.skills.len() >= MAX_SKILLS {
                result
                    .diagnostics
                    .push("Skill limit reached; discovery is incomplete".into());
                break;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    result.diagnostics.push(error.to_string());
                    continue;
                }
            };
            let source = entry.path().join("SKILL.md");
            if let Err(error) = fs::symlink_metadata(&source) {
                if error.kind() != std::io::ErrorKind::NotFound
                    && error.kind() != std::io::ErrorKind::NotADirectory
                {
                    result
                        .diagnostics
                        .push(format!("{}: {error}", source.display()));
                }
                continue;
            }
            let relative = source
                .strip_prefix(&root)
                .expect("ancestor stays in root")
                .to_path_buf();
            let mut skill = Skill {
                source: relative.clone(),
                name: entry.file_name().to_string_lossy().into_owned(),
                description: String::new(),
                availability: Availability::Available,
            };
            match read_metadata(&root, &source) {
                Ok((name, description)) => {
                    skill.name = name;
                    skill.description = description;
                }
                Err(error) => skill.availability = Availability::Unavailable(error),
            }
            if disabled.contains(&relative) && skill.availability == Availability::Available {
                skill.availability = Availability::Disabled;
            }
            result.skills.push(skill);
        }
        if ancestor == root {
            break;
        }
    }
    let mut names = BTreeMap::<String, usize>::new();
    for skill in &result.skills {
        if skill.availability == Availability::Available {
            *names.entry(skill.name.clone()).or_default() += 1;
        }
    }
    for skill in &mut result.skills {
        if skill.availability == Availability::Available && names[&skill.name] > 1 {
            skill.availability = Availability::Duplicate;
        }
    }
    result.skills.sort_by(|a, b| a.source.cmp(&b.source));
    result
}

fn read_metadata(root: &Path, source: &Path) -> Result<(String, String), String> {
    let canonical = source.canonicalize().map_err(|e| e.to_string())?;
    if !canonical.starts_with(root) {
        return Err("Source is outside the vault".into());
    }
    if !canonical.is_file() {
        return Err("Source is not a regular file".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(canonical)
        .map_err(|e| e.to_string())?
        .take(MAX_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err("Source exceeds 64 KiB".into());
    }
    let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
    let mut lines = text.trim_start_matches('\u{feff}').lines();
    if lines.next() != Some("---") {
        return Err("Missing YAML frontmatter".into());
    }
    let mut frontmatter = String::new();
    let mut closed = false;
    for line in lines {
        if line == "---" {
            closed = true;
            break;
        }
        frontmatter.push_str(line);
        frontmatter.push('\n');
    }
    if !closed {
        return Err("Unclosed YAML frontmatter".into());
    }
    let value: serde_yaml::Value = serde_yaml::from_str(&frontmatter).map_err(|e| e.to_string())?;
    let field = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("Missing or invalid {key}"))
    };
    Ok((field("name")?, field("description")?))
}

/// Revalidate an explicit Open action; outside-root/non-file sources never open.
pub fn open_source(root: &Path, skill: &Skill) -> Result<PathBuf, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    if skill.source.is_absolute()
        || skill
            .source
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        || skill.source.file_name().is_none_or(|n| n != "SKILL.md")
    {
        return Err("Invalid skill source identity".into());
    }
    let components: Vec<_> = skill.source.components().collect();
    let count = components.len();
    if count < 4
        || components[count - 4].as_os_str() != ".agents"
        || components[count - 3].as_os_str() != "skills"
    {
        return Err("Unsupported skill source identity".into());
    }
    let canonical = root
        .join(&skill.source)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !canonical.starts_with(&root) || !canonical.is_file() {
        return Err("Source is outside the vault or is not a regular file".into());
    }
    Ok(skill.source.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn write(root: &Path, folder: &str, name: &str) {
        let path = root.join(folder).join("SKILL.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            format!(
                "---\nname: {name}\ndescription: |\n  A useful skill\n---\nNever execute this.\n"
            ),
        )
        .unwrap();
    }
    fn scope(root: &Path) -> DiscoveryScope {
        DiscoveryScope {
            root: root.to_path_buf(),
            project: root.to_path_buf(),
            disabled: vec![],
            config_revision: 1,
        }
    }
    fn complete(inventory: &mut Inventory) {
        let request = inventory.begin_refresh().unwrap();
        let result = request.discover();
        assert!(inventory.finish_refresh(&request, Ok(result)));
    }

    #[test]
    fn config_refresh_changes_availability_and_rejects_old_completion() {
        let temp = tempfile::tempdir().unwrap();
        write(temp.path(), ".agents/skills/review", "review");
        let mut inventory = Inventory::default();
        let initial = scope(temp.path());
        assert!(inventory.configure(initial.clone()));
        complete(&mut inventory);
        assert_eq!(inventory.freshness(), &Freshness::Fresh);
        assert_eq!(
            inventory.snapshot().unwrap().skills[0].availability,
            Availability::Available
        );
        let obsolete = inventory.begin_refresh().unwrap();
        let obsolete_result = obsolete.discover();
        let mut changed = initial.clone();
        changed
            .disabled
            .push(PathBuf::from(".agents/skills/review/SKILL.md"));
        changed.config_revision += 1;
        assert!(inventory.configure(changed.clone()));
        assert!(matches!(inventory.freshness(), Freshness::Stale(_)));
        assert!(!inventory.finish_refresh(&obsolete, Ok(obsolete_result)));
        complete(&mut inventory);
        assert_eq!(
            inventory.snapshot().unwrap().skills[0].availability,
            Availability::Disabled
        );
        changed.disabled.extend(changed.disabled.clone());
        assert!(!inventory.configure(changed));
        assert!(inventory.configure(initial));
        complete(&mut inventory);
        assert_eq!(
            inventory.snapshot().unwrap().skills[0].availability,
            Availability::Available
        );
    }

    #[test]
    fn failed_config_cannot_publish_an_old_policy_as_fresh() {
        let temp = tempfile::tempdir().unwrap();
        write(temp.path(), ".agents/skills/review", "review");
        let mut inventory = Inventory::default();
        let scope = scope(temp.path());
        inventory.configure(scope.clone());
        complete(&mut inventory);
        let old = inventory.begin_refresh().unwrap();
        inventory.configuration_failed("Invalid disabled list");
        assert!(
            matches!(inventory.freshness(), Freshness::Failed(error) if error.contains("Invalid disabled list"))
        );
        assert!(inventory.snapshot().is_some());
        inventory.invalidate("Another source event");
        assert!(
            matches!(inventory.freshness(), Freshness::Failed(error) if error.contains("Invalid disabled list"))
        );
        assert!(inventory.begin_refresh().is_none());
        assert!(!inventory.finish_refresh(&old, Ok(old.discover())));
        assert!(inventory.configure(scope));
        complete(&mut inventory);
        assert_eq!(inventory.freshness(), &Freshness::Fresh);
    }

    #[test]
    fn source_events_failure_retry_and_partial_coverage_remain_visible() {
        let temp = tempfile::tempdir().unwrap();
        write(temp.path(), ".agents/skills/review", "review");
        let mut inventory = Inventory::default();
        inventory.configure(scope(temp.path()));
        complete(&mut inventory);
        let old = inventory.begin_refresh().unwrap();
        inventory.invalidate("Skill source changed");
        assert!(!inventory.finish_refresh(&old, Ok(old.discover())));
        let failed = inventory.begin_refresh().unwrap();
        assert!(inventory.finish_refresh(&failed, Err("Worker failed".into())));
        assert!(matches!(inventory.freshness(), Freshness::Failed(_)));
        assert_eq!(inventory.snapshot().unwrap().skills.len(), 1);
        let superseded = inventory.begin_refresh().unwrap();
        let retry = inventory.begin_refresh().unwrap();
        assert!(!inventory.finish_refresh(&superseded, Ok(superseded.discover())));
        let mut partial = retry.discover();
        partial.diagnostics.push("Skill limit reached".into());
        assert!(inventory.finish_refresh(&retry, Ok(partial)));
        assert_eq!(inventory.freshness(), &Freshness::Incomplete);
        assert!(!inventory.finish_refresh(&retry, Ok(Snapshot::default())));
        fs::remove_file(temp.path().join(".agents/skills/review/SKILL.md")).unwrap();
        inventory.invalidate("Skill removed");
        complete(&mut inventory);
        assert_eq!(inventory.freshness(), &Freshness::Fresh);
        assert!(inventory.snapshot().unwrap().skills.is_empty());
    }

    #[test]
    fn switching_projects_and_clearing_never_leaks_old_rows() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        write(first.path(), ".agents/skills/review", "review");
        let mut inventory = Inventory::default();
        inventory.configure(scope(first.path()));
        complete(&mut inventory);
        let old = inventory.begin_refresh().unwrap();
        inventory.configure(scope(second.path()));
        assert!(inventory.snapshot().is_none());
        assert!(!inventory.finish_refresh(&old, Ok(old.discover())));
        let pending = inventory.begin_refresh().unwrap();
        inventory.clear();
        assert!(inventory.scope().is_none());
        assert!(inventory.snapshot().is_none());
        assert!(inventory.begin_refresh().is_none());
        assert!(!inventory.finish_refresh(&pending, Ok(pending.discover())));
        inventory.configure(scope(second.path()));
        complete(&mut inventory);
        assert_eq!(inventory.freshness(), &Freshness::Fresh);
        assert!(inventory.snapshot().unwrap().skills.is_empty());
    }

    #[test]
    fn ancestors_duplicates_disabled_and_exact_sources() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("project/nested")).unwrap();
        write(root, ".agents/skills/root", "review");
        write(root, "project/.agents/skills/local", "review");
        write(root, "project/.agents/skills/off", "disabled");
        write(root, "project/.claude/skills/ignored", "ignored");
        let disabled = PathBuf::from("project/.agents/skills/off/SKILL.md");
        let snapshot = discover(root, &root.join("project/nested"), &[disabled]);
        assert!(snapshot.diagnostics.is_empty());
        assert_eq!(snapshot.skills.len(), 3);
        assert_eq!(
            snapshot
                .skills
                .iter()
                .filter(|s| s.availability == Availability::Duplicate)
                .count(),
            2
        );
        assert_eq!(
            snapshot
                .skills
                .iter()
                .filter(|s| s.availability == Availability::Disabled)
                .count(),
            1
        );
        assert!(snapshot.skills[0].description.contains("A useful skill"));
        assert_eq!(
            open_source(root, &snapshot.skills[0]).unwrap(),
            snapshot.skills[0].source
        );
    }
    #[test]
    fn exact_filename_and_size_limit_keep_inspectable_diagnostics() {
        let temp = tempfile::tempdir().unwrap();
        write(temp.path(), ".agents/skills/large", "large");
        let path = temp.path().join(".agents/skills/large/SKILL.md");
        fs::write(&path, vec![b'x'; MAX_SOURCE_BYTES as usize + 1]).unwrap();
        let ignored = temp.path().join(".agents/skills/wrong");
        fs::create_dir_all(&ignored).unwrap();
        fs::write(ignored.join("skill.md"), "ignored").unwrap();
        let snapshot = discover(temp.path(), temp.path(), &[]);
        assert_eq!(snapshot.skills.len(), 1);
        assert!(
            matches!(&snapshot.skills[0].availability, Availability::Unavailable(reason) if reason.contains("64 KiB"))
        );
        // Invalid metadata must remain inspectable with an explicit Open action.
        assert!(open_source(temp.path(), &snapshot.skills[0]).is_ok());
    }
    #[cfg(unix)]
    #[test]
    fn outside_sources_and_stale_open_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "secret", "secret");
        fs::create_dir_all(temp.path().join(".agents/skills")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret"),
            temp.path().join(".agents/skills/escape"),
        )
        .unwrap();
        write(temp.path(), ".agents/skills/good", "good");
        let snapshot = discover(temp.path(), temp.path(), &[]);
        assert!(snapshot
            .skills
            .iter()
            .any(|s| s.availability == Availability::Available));
        assert!(snapshot
            .skills
            .iter()
            .any(|s| matches!(s.availability, Availability::Unavailable(_))));
        let good = snapshot.skills.iter().find(|s| s.name == "good").unwrap();
        fs::remove_file(temp.path().join(&good.source)).unwrap();
        assert!(open_source(temp.path(), good).is_err());
        assert!(!discover(temp.path(), outside.path(), &[])
            .diagnostics
            .is_empty());
    }
}
