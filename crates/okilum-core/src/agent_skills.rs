//! Bounded, read-only project skill inventory (#1591). No provider execution.
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    ops::ControlFlow,
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

/// Skill folder convention a source was found in (#2938). Each provider loads
/// only its own folder, so the same name in both is not a duplicate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Provider {
    /// `.agents/skills`: Codex and other agents.
    Agents,
    /// `.claude/skills`: Claude Code.
    Claude,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::Agents, Provider::Claude];

    /// Folder name, also the row label.
    pub fn folder(self) -> &'static str {
        match self {
            Provider::Agents => ".agents",
            Provider::Claude => ".claude",
        }
    }

    pub fn audience(self) -> &'static str {
        match self {
            Provider::Agents => "Codex and other agents",
            Provider::Claude => "Claude Code",
        }
    }

    /// Provider of a `<folder>/skills/<name>/SKILL.md` identity, judged by its
    /// last four components only.
    pub fn of_source(source: &Path) -> Option<Provider> {
        let components: Vec<_> = source.components().collect();
        let count = components.len();
        if count < 4
            || components[count - 3].as_os_str() != "skills"
            || components[count - 1].as_os_str() != "SKILL.md"
        {
            return None;
        }
        Provider::ALL
            .into_iter()
            .find(|provider| components[count - 4].as_os_str() == provider.folder())
    }
}

/// A same-name source under the other provider's folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counterpart {
    pub source: PathBuf,
    pub provider: Provider,
    /// The two SKILL.md files are byte-for-byte equal.
    pub identical: bool,
}

#[derive(Clone, Debug)]
pub struct Skill {
    /// Exact vault-relative source identity, retained even for unavailable rows.
    pub source: PathBuf,
    pub provider: Provider,
    pub name: String,
    pub description: String,
    pub availability: Availability,
    /// Readable same-name sources of the other provider; empty when none.
    pub counterparts: Vec<Counterpart>,
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
pub struct RefreshRequest<R = u64> {
    generation: u64,
    scope: DiscoveryScope,
    inventory_revision: Option<R>,
}

impl<R> RefreshRequest<R> {
    /// Run on the owner's background executor, never the UI thread.
    pub fn discover(&self) -> Snapshot {
        discover(&self.scope.root, &self.scope.project, &self.scope.disabled)
    }
}

/// Session-only projection; no timers, watchers, writes or provider calls.
/// Keep this instance across refreshes. Feed source/config events to `invalidate`
/// or `configure`, and submit a new request through the owner's executor.
pub struct Inventory<R = u64> {
    scope: Option<DiscoveryScope>,
    generation: u64,
    pending: bool,
    config_error: Option<String>,
    inventory_revision: Option<R>,
    reload_diagnostics: Vec<String>,
    snapshot: Option<Snapshot>,
    freshness: Freshness,
}

impl<R> Default for Inventory<R> {
    fn default() -> Self {
        Self {
            scope: None,
            generation: 0,
            pending: false,
            config_error: None,
            inventory_revision: None,
            reload_diagnostics: Vec::new(),
            snapshot: None,
            freshness: Freshness::NotLoaded,
        }
    }
}

impl<R: Clone + Eq> Inventory<R> {
    /// Opaque revision supplied by the current published inventory owner. It may
    /// be a sequence or an exact retained publication token; never infer one.
    pub fn inventory_revision(&self) -> Option<&R> {
        self.inventory_revision.as_ref()
    }
    pub fn scope(&self) -> Option<&DiscoveryScope> {
        self.scope.as_ref()
    }
    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.snapshot.as_ref()
    }
    pub fn freshness(&self) -> &Freshness {
        &self.freshness
    }

    /// Diagnostics from the latest incomplete reload, separate from the retained
    /// last-good listing. These must remain visible until a successful reload.
    pub fn reload_diagnostics(&self) -> &[String] {
        &self.reload_diagnostics
    }

    /// Bind the accepted inventory revision and exact discovery policy before
    /// Reload. Call for every inventory publication, including an empty inventory.
    /// Paths are compared exactly; canonicalization must not hide a scope switch.
    pub fn configure_revision(&mut self, scope: DiscoveryScope, inventory_revision: R) -> bool {
        let revision_changed = self.inventory_revision.as_ref() != Some(&inventory_revision);
        self.inventory_revision = Some(inventory_revision);
        let changed = self.configure(scope);
        if revision_changed && !changed {
            self.invalidate("Project skill inventory changed");
        }
        changed || revision_changed
    }

    /// Explicit Reload entry point for owners with revision-aware inventory.
    /// The request captures root/project, inventory revision and disabled/config
    /// policy together; a later configure_revision invalidates its completion.
    pub fn begin_reload(
        &mut self,
        scope: DiscoveryScope,
        inventory_revision: R,
    ) -> Option<RefreshRequest<R>> {
        self.configure_revision(scope, inventory_revision);
        self.begin_refresh()
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
            self.reload_diagnostics.clear();
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
        self.inventory_revision = None;
        self.reload_diagnostics.clear();
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
    pub fn begin_refresh(&mut self) -> Option<RefreshRequest<R>> {
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
            inventory_revision: self.inventory_revision.clone(),
        })
    }

    pub fn matches_request(&self, request: &RefreshRequest<R>) -> bool {
        self.pending
            && self.generation == request.generation
            && self.scope.as_ref() == Some(&request.scope)
            && self.inventory_revision == request.inventory_revision
    }

    /// Returns false for obsolete or repeated completions. Failed refreshes retain
    /// previous rows with a visible error; partial discovery is never called fresh.
    pub fn finish_refresh(
        &mut self,
        request: &RefreshRequest<R>,
        result: Result<Snapshot, String>,
    ) -> bool {
        if !self.matches_request(request) {
            return false;
        }
        self.pending = false;
        match result {
            Ok(snapshot) => {
                self.reload_diagnostics = snapshot.diagnostics.clone();
                if snapshot.diagnostics.is_empty() {
                    self.freshness = Freshness::Fresh;
                    self.snapshot = Some(snapshot);
                } else {
                    self.freshness = Freshness::Incomplete;
                    // Partial/failed directory traversal cannot erase healthy
                    // last-good rows or publish a misleading empty inventory.
                    if self.snapshot.is_none() {
                        self.snapshot = Some(snapshot);
                    }
                }
            }
            Err(error) => self.freshness = Freshness::Failed(error),
        }
        true
    }
}

/// Only immediate children of `.agents/skills` and `.claude/skills` in project
/// ancestors are supported. Disabled paths are explicit vault-relative identities
/// supplied by the caller; no provider configuration or user-global directories
/// are consulted.
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
    // Bounded source bytes, parallel to `result.skills`, for the differs note.
    let mut contents = Vec::new();
    'ancestors: for (depth, ancestor) in project.ancestors().enumerate() {
        if depth == MAX_ANCESTORS {
            result
                .diagnostics
                .push("Ancestor limit reached; discovery is incomplete".into());
            break;
        }
        for provider in Provider::ALL {
            let scan = scan_directory(
                &root,
                ancestor,
                provider,
                disabled,
                &mut result,
                &mut contents,
            );
            if scan.is_break() {
                break 'ancestors;
            }
        }
        if ancestor == root {
            break;
        }
    }
    // Duplicates are counted per provider: each agent loads only its own folder.
    let mut names = BTreeMap::<(Provider, String), usize>::new();
    for skill in &result.skills {
        if skill.availability == Availability::Available {
            *names
                .entry((skill.provider, skill.name.clone()))
                .or_default() += 1;
        }
    }
    for skill in &mut result.skills {
        if skill.availability == Availability::Available
            && names[&(skill.provider, skill.name.clone())] > 1
        {
            skill.availability = Availability::Duplicate;
        }
    }
    let counterparts: Vec<Vec<Counterpart>> = result
        .skills
        .iter()
        .zip(&contents)
        .map(|(skill, bytes)| {
            let Some(bytes) = bytes else {
                return Vec::new();
            };
            result
                .skills
                .iter()
                .zip(&contents)
                .filter(|(other, _)| other.provider != skill.provider && other.name == skill.name)
                .filter_map(|(other, other_bytes)| {
                    Some(Counterpart {
                        source: other.source.clone(),
                        provider: other.provider,
                        identical: other_bytes.as_ref()? == bytes,
                    })
                })
                .collect()
        })
        .collect();
    for (skill, counterparts) in result.skills.iter_mut().zip(counterparts) {
        skill.counterparts = counterparts;
    }
    // Same-name rows sit together; within a name, the source decides.
    result
        .skills
        .sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.source.cmp(&b.source)));
    result
}

/// Read one provider's skill folder in one ancestor. `Break` stops discovery.
fn scan_directory(
    root: &Path,
    ancestor: &Path,
    provider: Provider,
    disabled: &[PathBuf],
    result: &mut Snapshot,
    contents: &mut Vec<Option<Vec<u8>>>,
) -> ControlFlow<()> {
    let directory = ancestor.join(provider.folder()).join("skills");
    // Do not enumerate symlinked roots outside the vault.
    match directory.canonicalize() {
        Ok(path) if !path.starts_with(root) => {
            result.diagnostics.push(format!(
                "Outside-vault skill directory: {}",
                directory.display()
            ));
            return ControlFlow::Continue(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ControlFlow::Continue(());
        }
        Err(error) => {
            result.diagnostics.push(error.to_string());
            return ControlFlow::Break(());
        }
        _ => {}
    }
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) => {
            result.diagnostics.push(error.to_string());
            return ControlFlow::Break(());
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
            .strip_prefix(root)
            .expect("ancestor stays in root")
            .to_path_buf();
        let mut skill = Skill {
            source: relative.clone(),
            provider,
            name: entry.file_name().to_string_lossy().into_owned(),
            description: String::new(),
            availability: Availability::Available,
            counterparts: Vec::new(),
        };
        let mut bytes = None;
        match read_metadata(root, &source) {
            Ok((name, description, source_bytes)) => {
                skill.name = name;
                skill.description = description;
                bytes = Some(source_bytes);
            }
            Err(error) => skill.availability = Availability::Unavailable(error),
        }
        if disabled.contains(&relative) && skill.availability == Availability::Available {
            skill.availability = Availability::Disabled;
        }
        result.skills.push(skill);
        contents.push(bytes);
    }
    ControlFlow::Continue(())
}

fn read_metadata(root: &Path, source: &Path) -> Result<(String, String, Vec<u8>), String> {
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
    Ok((field("name")?, field("description")?, bytes))
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
    if Provider::of_source(&skill.source).is_none() {
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
    fn write_body(root: &Path, folder: &str, name: &str, body: &str) {
        let path = root.join(folder).join("SKILL.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            format!("---\nname: {name}\ndescription: A useful skill\n---\n{body}"),
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
    fn reload_retains_last_good_and_accepts_only_current_revision_and_policy() {
        let temp = tempfile::tempdir().unwrap();
        write(temp.path(), ".agents/skills/review", "review");
        let mut inventory: Inventory = Inventory::default();
        let scope = scope(temp.path());
        let first = inventory.begin_reload(scope.clone(), 10).unwrap();
        assert!(inventory.finish_refresh(&first, Ok(first.discover())));
        let obsolete = inventory.begin_reload(scope.clone(), 10).unwrap();
        let next = inventory.begin_reload(scope.clone(), 11).unwrap();
        assert!(!inventory.finish_refresh(&obsolete, Ok(Snapshot::default())));
        let partial = Snapshot {
            skills: vec![],
            diagnostics: vec!["Skill directory unavailable".into()],
        };
        assert!(inventory.finish_refresh(&next, Ok(partial)));
        assert_eq!(inventory.snapshot().unwrap().skills[0].name, "review");
        assert_eq!(inventory.freshness(), &Freshness::Incomplete);
        assert_eq!(
            inventory.reload_diagnostics(),
            &["Skill directory unavailable"]
        );
        let old_policy = inventory.begin_reload(scope.clone(), 11).unwrap();
        let mut disabled = scope.clone();
        disabled
            .disabled
            .push(".agents/skills/review/SKILL.md".into());
        disabled.config_revision += 1;
        let current = inventory.begin_reload(disabled, 11).unwrap();
        assert!(!inventory.finish_refresh(&old_policy, Ok(old_policy.discover())));
        assert!(inventory.finish_refresh(&current, Ok(current.discover())));
        assert_eq!(
            inventory.snapshot().unwrap().skills[0].availability,
            Availability::Disabled
        );
        assert!(inventory.reload_diagnostics().is_empty());
        let stale = inventory.begin_refresh().unwrap();
        let other = tempfile::tempdir().unwrap();
        let switched = inventory
            .begin_reload(super::tests::scope(other.path()), 11)
            .unwrap();
        assert!(inventory.snapshot().is_none());
        assert!(!inventory.finish_refresh(&stale, Ok(stale.discover())));
        assert!(inventory.finish_refresh(&switched, Ok(switched.discover())));
        assert!(inventory.snapshot().unwrap().skills.is_empty());
    }

    #[test]
    fn manifest_failure_and_retry_never_erase_last_good_listing() {
        let temp = tempfile::tempdir().unwrap();
        write(temp.path(), ".agents/skills/review", "review");
        let mut inventory: Inventory = Inventory::default();
        let configured = scope(temp.path());
        let initial = inventory.begin_reload(configured.clone(), 1).unwrap();
        assert!(inventory.finish_refresh(&initial, Ok(initial.discover())));
        inventory.configuration_failed("Manifest is unreadable");
        assert!(inventory.begin_refresh().is_none());
        assert_eq!(inventory.snapshot().unwrap().skills.len(), 1);
        assert!(matches!(inventory.freshness(), Freshness::Failed(_)));
        let retry = inventory.begin_reload(configured, 2).unwrap();
        assert!(inventory.finish_refresh(&retry, Err("Source path is unavailable".into())));
        assert_eq!(inventory.snapshot().unwrap().skills.len(), 1);
        assert!(
            matches!(inventory.freshness(), Freshness::Failed(message) if message.contains("Source path"))
        );
        let retry = inventory.begin_refresh().unwrap();
        fs::remove_file(temp.path().join(".agents/skills/review/SKILL.md")).unwrap();
        assert!(inventory.finish_refresh(&retry, Ok(retry.discover())));
        assert_eq!(inventory.freshness(), &Freshness::Fresh);
        assert!(inventory.snapshot().unwrap().skills.is_empty());
    }

    #[test]
    fn config_refresh_changes_availability_and_rejects_old_completion() {
        let temp = tempfile::tempdir().unwrap();
        write(temp.path(), ".agents/skills/review", "review");
        let mut inventory: Inventory = Inventory::default();
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
        let mut inventory: Inventory = Inventory::default();
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
        let mut inventory: Inventory = Inventory::default();
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
        let mut inventory: Inventory = Inventory::default();
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
        write(root, "project/.claude/skills/claude", "claude");
        let disabled = PathBuf::from("project/.agents/skills/off/SKILL.md");
        let snapshot = discover(root, &root.join("project/nested"), &[disabled]);
        assert!(snapshot.diagnostics.is_empty());
        assert_eq!(snapshot.skills.len(), 4);
        let claude = snapshot.skills.iter().find(|s| s.name == "claude").unwrap();
        assert_eq!(claude.provider, Provider::Claude);
        assert_eq!(claude.availability, Availability::Available);
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
    fn claude_and_agents_providers_pair_by_name_and_note_byte_differences() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("project")).unwrap();
        write_body(root, ".agents/skills/review", "review", "Same body\n");
        write_body(root, ".claude/skills/review", "review", "Same body\n");
        write_body(root, ".agents/skills/handoff", "handoff", "Codex body\n");
        write_body(root, ".claude/skills/handoff", "handoff", "Claude body\n");
        write_body(root, ".claude/skills/twin", "twin", "Root\n");
        write_body(root, "project/.claude/skills/twin", "twin", "Local\n");
        write_body(root, "project/.claude/skills/only", "only", "Only\n");
        let snapshot = discover(root, &root.join("project"), &[]);
        assert!(
            snapshot.diagnostics.is_empty(),
            "{:?}",
            snapshot.diagnostics
        );
        assert_eq!(snapshot.skills.len(), 7);
        let row = |source: &str| {
            snapshot
                .skills
                .iter()
                .find(|s| s.source == Path::new(source))
                .unwrap_or_else(|| panic!("{source}"))
        };
        // Cross-provider same name is one logical skill, never a Duplicate.
        let agents = row(".agents/skills/review/SKILL.md");
        let claude = row(".claude/skills/review/SKILL.md");
        assert_eq!(agents.provider, Provider::Agents);
        assert_eq!(claude.provider, Provider::Claude);
        assert_eq!(agents.availability, Availability::Available);
        assert_eq!(claude.availability, Availability::Available);
        assert_eq!(
            agents.counterparts,
            vec![Counterpart {
                source: claude.source.clone(),
                provider: Provider::Claude,
                identical: true,
            }]
        );
        assert!(claude.counterparts[0].identical);
        let differs = row(".claude/skills/handoff/SKILL.md");
        assert_eq!(differs.availability, Availability::Available);
        assert_eq!(
            differs.counterparts,
            vec![Counterpart {
                source: PathBuf::from(".agents/skills/handoff/SKILL.md"),
                provider: Provider::Agents,
                identical: false,
            }]
        );
        assert!(!row(".agents/skills/handoff/SKILL.md").counterparts[0].identical);
        // Same name inside one provider keeps the Duplicate state.
        assert_eq!(
            row(".claude/skills/twin/SKILL.md").availability,
            Availability::Duplicate
        );
        assert_eq!(
            row("project/.claude/skills/twin/SKILL.md").availability,
            Availability::Duplicate
        );
        assert!(row("project/.claude/skills/twin/SKILL.md")
            .counterparts
            .is_empty());
        let only = row("project/.claude/skills/only/SKILL.md");
        assert_eq!(only.availability, Availability::Available);
        assert!(only.counterparts.is_empty());
        // Same-name rows are adjacent so the pair reads as one skill.
        let review = snapshot
            .skills
            .iter()
            .position(|s| s.name == "review")
            .unwrap();
        assert_eq!(snapshot.skills[review + 1].name, "review");
        for skill in &snapshot.skills {
            assert_eq!(open_source(root, skill).unwrap(), skill.source);
        }
    }

    #[test]
    fn open_source_accepts_only_provider_skill_identities() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let skill = |source: &str| Skill {
            source: PathBuf::from(source),
            provider: Provider::Claude,
            name: "x".into(),
            description: String::new(),
            availability: Availability::Available,
            counterparts: Vec::new(),
        };
        for folder in [
            "p/.claude/skills/x",
            "p/.agents/skills/x",
            "p/.codex/skills/x",
            "p/.claude/rules/x",
            "p/claude/skills/x",
        ] {
            write(root, folder, "x");
        }
        fs::write(root.join("p/.claude/skills/x/README.md"), "readme").unwrap();
        assert_eq!(
            open_source(root, &skill("p/.claude/skills/x/SKILL.md")).unwrap(),
            PathBuf::from("p/.claude/skills/x/SKILL.md")
        );
        assert!(open_source(root, &skill("p/.agents/skills/x/SKILL.md")).is_ok());
        for rejected in [
            "p/.codex/skills/x/SKILL.md",
            "p/.claude/rules/x/SKILL.md",
            "p/claude/skills/x/SKILL.md",
            "p/.claude/skills/x/README.md",
            "p/.claude/skills/../skills/x/SKILL.md",
            ".claude/skills/SKILL.md",
        ] {
            assert!(open_source(root, &skill(rejected)).is_err(), "{rejected}");
        }
        assert_eq!(
            Provider::of_source(Path::new("a/.claude/skills/x/SKILL.md")),
            Some(Provider::Claude)
        );
        assert_eq!(
            Provider::of_source(Path::new("a/.codex/skills/x/SKILL.md")),
            None
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
    #[cfg(unix)]
    #[test]
    fn outside_vault_claude_skill_folder_is_not_enumerated() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "skills/secret", "secret");
        fs::create_dir_all(temp.path().join(".claude")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("skills"),
            temp.path().join(".claude/skills"),
        )
        .unwrap();
        write(temp.path(), ".agents/skills/good", "good");
        let snapshot = discover(temp.path(), temp.path(), &[]);
        assert_eq!(snapshot.skills.len(), 1);
        assert_eq!(snapshot.skills[0].provider, Provider::Agents);
        assert!(snapshot
            .diagnostics
            .iter()
            .any(|d| d.contains("Outside-vault skill directory") && d.contains(".claude")));
    }
}
