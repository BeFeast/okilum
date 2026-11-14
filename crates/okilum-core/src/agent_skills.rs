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

/// Which agents read a skill folder (#2938).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Provider {
    /// `.agents/skills`: Codex and other agents.
    Agents,
    /// `.claude/skills`: Claude Code.
    Claude,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::Agents, Provider::Claude];

    /// Folder that holds this provider's skills inside an ancestor.
    pub fn folder(self) -> &'static str {
        match self {
            Provider::Agents => ".agents",
            Provider::Claude => ".claude",
        }
    }

    /// Who loads skills from this folder, for the row's source line.
    pub fn readers(self) -> &'static str {
        match self {
            Provider::Agents => "Codex and other agents",
            Provider::Claude => "Claude Code",
        }
    }
}

/// The same-named skill from the other provider's folder, shown on one row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairedSource {
    pub source: PathBuf,
    pub provider: Provider,
    /// The two `SKILL.md` files are not byte-identical.
    pub differs: bool,
}

#[derive(Clone, Debug)]
pub struct Skill {
    /// Exact vault-relative source identity, retained even for unavailable rows.
    pub source: PathBuf,
    pub provider: Provider,
    pub name: String,
    pub description: String,
    pub availability: Availability,
    /// Set when the same directory holds an available skill of this name in
    /// both `.agents/skills` and `.claude/skills`; `source` is then the
    /// `.agents` copy.
    pub paired: Option<PairedSource>,
    /// The command Claude Code runs this skill under, such as `/project` or
    /// `/Dev:project`, when a `.claude` source of this row is available.
    pub claude_command: Option<String>,
}

impl Skill {
    /// Every exact source of this row, primary first.
    pub fn sources(&self) -> impl Iterator<Item = (Provider, &Path)> {
        std::iter::once((self.provider, self.source.as_path())).chain(
            self.paired
                .iter()
                .map(|paired| (paired.provider, paired.source.as_path())),
        )
    }
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
    let mut contents = BTreeMap::<PathBuf, Vec<u8>>::new();
    'ancestors: for (depth, ancestor) in project.ancestors().enumerate() {
        if depth == MAX_ANCESTORS {
            result
                .diagnostics
                .push("Ancestor limit reached; discovery is incomplete".into());
            break;
        }
        for provider in Provider::ALL {
            let directory = ancestor.join(provider.folder()).join("skills");
            if scan_directory(
                &root,
                &directory,
                provider,
                disabled,
                &mut result,
                &mut contents,
            )
            .is_err()
            {
                break 'ancestors;
            }
        }
        if ancestor == root {
            break;
        }
    }
    // Codex and other agents see one flat set of `.agents` skills, so a name
    // found twice anywhere is a duplicate. Claude Code keeps same-named skills
    // from different directories (see `claude_commands`); only two of one name
    // inside a single `.claude/skills` folder clash.
    let key = |skill: &Skill| {
        let scope = match skill.provider {
            Provider::Agents => PathBuf::new(),
            Provider::Claude => scope_of(&skill.source).to_path_buf(),
        };
        (skill.provider, scope, skill.name.clone())
    };
    let mut names = BTreeMap::<(Provider, PathBuf, String), usize>::new();
    for skill in &result.skills {
        if skill.availability == Availability::Available {
            *names.entry(key(skill)).or_default() += 1;
        }
    }
    for skill in &mut result.skills {
        if skill.availability == Availability::Available && names[&key(skill)] > 1 {
            skill.availability = Availability::Duplicate;
        }
    }
    claude_commands(&mut result.skills);
    // An available skill of one name in both folders of the same directory is
    // one skill with two sources; anything else stays a separate, visible row.
    let mut claude = BTreeMap::<(PathBuf, String), usize>::new();
    for (index, skill) in result.skills.iter().enumerate() {
        if skill.provider == Provider::Claude && skill.availability == Availability::Available {
            claude.insert(
                (scope_of(&skill.source).to_path_buf(), skill.name.clone()),
                index,
            );
        }
    }
    let mut merged = vec![false; result.skills.len()];
    for index in 0..result.skills.len() {
        let skill = &result.skills[index];
        if skill.provider != Provider::Agents || skill.availability != Availability::Available {
            continue;
        }
        let Some(&other) = claude.get(&(scope_of(&skill.source).to_path_buf(), skill.name.clone()))
        else {
            continue;
        };
        let differs = contents.get(&skill.source) != contents.get(&result.skills[other].source);
        let paired = PairedSource {
            source: result.skills[other].source.clone(),
            provider: Provider::Claude,
            differs,
        };
        result.skills[index].claude_command = result.skills[other].claude_command.clone();
        result.skills[index].paired = Some(paired);
        merged[other] = true;
    }
    let mut index = 0;
    result.skills.retain(|_| {
        index += 1;
        !merged[index - 1]
    });
    result.skills.sort_by(|a, b| a.source.cmp(&b.source));
    result
}

/// Directory whose `<provider>/skills` folder holds `source`
/// (`<scope>/.claude/skills/<dir>/SKILL.md`), vault-relative; empty at the root.
fn scope_of(source: &Path) -> &Path {
    source
        .ancestors()
        .nth(4)
        .expect("discovered sources have four trailing components")
}

/// Claude Code loads `.claude/skills` from the start directory and every parent,
/// and same-named skills from different directories all stay available: the
/// one nearest the repository root keeps `/name`, the others run as
/// `/<directory>:name` (code.claude.com/docs/en/skills, nested skills). The
/// vault root stands in for the repository root.
fn claude_commands(skills: &mut [Skill]) {
    let mut by_name = BTreeMap::<String, Vec<usize>>::new();
    for (index, skill) in skills.iter().enumerate() {
        if skill.provider == Provider::Claude && skill.availability == Availability::Available {
            by_name.entry(skill.name.clone()).or_default().push(index);
        }
    }
    for (name, mut indices) in by_name {
        indices.sort_by_key(|&index| scope_of(&skills[index].source).components().count());
        for (rank, index) in indices.into_iter().enumerate() {
            let command = if rank == 0 {
                format!("/{name}")
            } else {
                let scope: Vec<_> = scope_of(&skills[index].source)
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().into_owned())
                    .collect();
                format!("/{}:{name}", scope.join("/"))
            };
            skills[index].claude_command = Some(command);
        }
    }
}

/// Scan one `<provider>/skills` folder. `Err` stops the ancestor walk, as a
/// directory that exists but cannot be read leaves coverage unknown.
fn scan_directory(
    root: &Path,
    directory: &Path,
    provider: Provider,
    disabled: &[PathBuf],
    result: &mut Snapshot,
    contents: &mut BTreeMap<PathBuf, Vec<u8>>,
) -> Result<(), ()> {
    // Do not enumerate symlinked roots outside the vault.
    match directory.canonicalize() {
        Ok(path) if !path.starts_with(root) => {
            result.diagnostics.push(format!(
                "Outside-vault skill directory: {}",
                directory.display()
            ));
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            result.diagnostics.push(error.to_string());
            return Err(());
        }
        _ => {}
    }
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            result.diagnostics.push(error.to_string());
            return Err(());
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
            paired: None,
            claude_command: None,
        };
        match read_metadata(root, &source, provider, &skill.name) {
            Ok((name, description, bytes)) => {
                skill.name = name;
                skill.description = description;
                contents.insert(relative.clone(), bytes);
            }
            Err(error) => skill.availability = Availability::Unavailable(error),
        }
        if disabled.contains(&relative) && skill.availability == Availability::Available {
            skill.availability = Availability::Disabled;
        }
        result.skills.push(skill);
    }
    Ok(())
}

fn read_metadata(
    root: &Path,
    source: &Path,
    provider: Provider,
    directory_name: &str,
) -> Result<(String, String, Vec<u8>), String> {
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
    let (name, description) = match provider {
        Provider::Agents => agents_metadata(text)?,
        Provider::Claude => claude_metadata(text, directory_name),
    };
    Ok((name, description, bytes))
}

/// Split `---` frontmatter from the body. `None` when the file does not open
/// with a closed frontmatter block.
fn split_frontmatter(text: &str) -> Option<(String, &str)> {
    let text = text.trim_start_matches('\u{feff}');
    let mut lines = text.split_inclusive('\n');
    let first = lines.next()?;
    if first.trim_end_matches(['\r', '\n']) != "---" {
        return None;
    }
    let mut offset = first.len();
    let mut frontmatter = String::new();
    for line in lines {
        offset += line.len();
        let line = line.trim_end_matches(['\r', '\n']);
        if line == "---" {
            return Some((frontmatter, &text[offset..]));
        }
        frontmatter.push_str(line);
        frontmatter.push('\n');
    }
    None
}

fn string_field(value: Option<&serde_yaml::Value>, key: &str) -> Option<String> {
    value?
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
}

/// Codex and other agents require frontmatter with `name` and `description`.
fn agents_metadata(text: &str) -> Result<(String, String), String> {
    let body = text.trim_start_matches('\u{feff}');
    if body.lines().next() != Some("---") {
        return Err("Missing YAML frontmatter".into());
    }
    let (frontmatter, _) = split_frontmatter(text).ok_or("Unclosed YAML frontmatter")?;
    let value: serde_yaml::Value = serde_yaml::from_str(&frontmatter).map_err(|e| e.to_string())?;
    let field = |key: &str| {
        string_field(Some(&value), key).ok_or_else(|| format!("Missing or invalid {key}"))
    };
    Ok((field("name")?, field("description")?))
}

/// Claude Code treats every field as optional: `name` defaults to the skill
/// directory name and `description` to the first non-empty line of the
/// content. Without an opening `---` line the whole file is content, and
/// frontmatter that does not parse loads the skill with no fields set.
fn claude_metadata(text: &str, directory_name: &str) -> (String, String) {
    let (value, content) = match split_frontmatter(text) {
        Some((frontmatter, content)) => (
            serde_yaml::from_str::<serde_yaml::Value>(&frontmatter).ok(),
            content,
        ),
        None => (None, text.trim_start_matches('\u{feff}')),
    };
    let name = string_field(value.as_ref(), "name").unwrap_or_else(|| directory_name.to_owned());
    let description = string_field(value.as_ref(), "description").unwrap_or_else(|| {
        content
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default()
            .to_owned()
    });
    (name, description)
}

/// Revalidate an explicit Open action; outside-root/non-file sources never open.
/// `source` must be one of the row's exact sources (`Skill::sources`).
pub fn open_source(root: &Path, source: &Path) -> Result<PathBuf, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    if source.is_absolute()
        || source
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        || source.file_name().is_none_or(|n| n != "SKILL.md")
    {
        return Err("Invalid skill source identity".into());
    }
    let components: Vec<_> = source.components().collect();
    let count = components.len();
    if count < 4
        || !Provider::ALL
            .iter()
            .any(|provider| components[count - 4].as_os_str() == provider.folder())
        || components[count - 3].as_os_str() != "skills"
    {
        return Err("Unsupported skill source identity".into());
    }
    let canonical = root
        .join(source)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !canonical.starts_with(&root) || !canonical.is_file() {
        return Err("Source is outside the vault or is not a regular file".into());
    }
    Ok(source.to_path_buf())
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
            open_source(root, &snapshot.skills[0].source).unwrap(),
            snapshot.skills[0].source
        );
    }
    fn write_body(root: &Path, folder: &str, name: &str, body: &str) {
        let path = root.join(folder).join("SKILL.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            format!("---\nname: {name}\ndescription: A useful skill\n---\n{body}\n"),
        )
        .unwrap();
    }

    #[test]
    fn claude_skills_are_listed_from_every_ancestor_with_their_provider() {
        // #2938: AI/.claude/skills/course-harvest exists only under .claude.
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("AI/course")).unwrap();
        write(root, "AI/.claude/skills/course-harvest", "course-harvest");
        write(root, ".claude/skills/media-download", "media-download");
        write(root, ".agents/skills/codex-only", "codex-only");
        let snapshot = discover(root, &root.join("AI/course"), &[]);
        assert!(snapshot.diagnostics.is_empty());
        let rows: Vec<_> = snapshot
            .skills
            .iter()
            .map(|s| (s.name.as_str(), s.provider, s.availability.clone()))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("codex-only", Provider::Agents, Availability::Available),
                ("media-download", Provider::Claude, Availability::Available),
                ("course-harvest", Provider::Claude, Availability::Available),
            ]
        );
        let harvest = &snapshot.skills[2];
        assert_eq!(
            harvest.source,
            PathBuf::from("AI/.claude/skills/course-harvest/SKILL.md")
        );
        assert!(harvest.paired.is_none());
        assert_eq!(open_source(root, &harvest.source).unwrap(), harvest.source);
        // A sibling project does not see AI/.claude.
        fs::create_dir_all(root.join("Dev")).unwrap();
        let sibling = discover(root, &root.join("Dev"), &[]);
        assert!(sibling.skills.iter().all(|s| s.name != "course-harvest"));
    }

    #[test]
    fn same_name_in_both_providers_is_one_row_that_says_when_files_differ() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write_body(
            root,
            ".agents/skills/grilling",
            "grilling",
            "Ask hard questions.",
        );
        write_body(
            root,
            ".claude/skills/grilling",
            "grilling",
            "Ask harder questions.",
        );
        write_body(root, ".agents/skills/project", "project", "Same body.");
        write_body(root, ".claude/skills/project", "project", "Same body.");
        let snapshot = discover(root, root, &[]);
        assert!(snapshot.diagnostics.is_empty());
        assert_eq!(snapshot.skills.len(), 2, "{:#?}", snapshot.skills);
        let grilling = &snapshot.skills[0];
        assert_eq!(grilling.name, "grilling");
        assert_eq!(grilling.availability, Availability::Available);
        assert_eq!(grilling.provider, Provider::Agents);
        assert_eq!(
            grilling.paired,
            Some(PairedSource {
                source: ".claude/skills/grilling/SKILL.md".into(),
                provider: Provider::Claude,
                differs: true,
            })
        );
        let project = &snapshot.skills[1];
        assert_eq!(project.name, "project");
        assert!(!project.paired.as_ref().unwrap().differs);
        // Each exact source opens on its own.
        let sources: Vec<_> = grilling.sources().collect();
        assert_eq!(
            sources,
            vec![
                (
                    Provider::Agents,
                    Path::new(".agents/skills/grilling/SKILL.md")
                ),
                (
                    Provider::Claude,
                    Path::new(".claude/skills/grilling/SKILL.md")
                ),
            ]
        );
        for (_, source) in sources {
            assert_eq!(open_source(root, source).unwrap(), source);
        }
    }

    type Row = (
        String,
        Provider,
        Availability,
        Option<String>,
        Option<String>,
    );
    fn rows(snapshot: &Snapshot) -> Vec<Row> {
        snapshot
            .skills
            .iter()
            .map(|s| {
                (
                    s.source.to_string_lossy().into_owned(),
                    s.provider,
                    s.availability.clone(),
                    s.paired
                        .as_ref()
                        .map(|p| p.source.to_string_lossy().into_owned()),
                    s.claude_command.clone(),
                )
            })
            .collect()
    }
    fn row(
        source: &str,
        provider: Provider,
        availability: Availability,
        paired: Option<&str>,
        command: Option<&str>,
    ) -> Row {
        (
            source.into(),
            provider,
            availability,
            paired.map(Into::into),
            command.map(Into::into),
        )
    }

    #[test]
    fn claude_same_name_in_parent_and_project_folders_both_stay_available() {
        // Vault today: root .claude/skills/project-onboarding and
        // Dev/.claude/skills/project-onboarding. Claude Code loads both; the
        // deeper one runs as /Dev:project-onboarding.
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("Dev/Areas/demo")).unwrap();
        write(root, ".claude/skills/onboarding", "onboarding");
        write(root, "Dev/.claude/skills/onboarding", "onboarding");
        write(
            root,
            "Dev/Areas/demo/.claude/skills/onboarding",
            "onboarding",
        );
        write(root, ".agents/skills/onboarding", "onboarding");
        let snapshot = discover(root, &root.join("Dev/Areas/demo"), &[]);
        assert!(snapshot.diagnostics.is_empty());
        use Availability::Available;
        use Provider::{Agents, Claude};
        assert_eq!(
            rows(&snapshot),
            vec![
                row(
                    ".agents/skills/onboarding/SKILL.md",
                    Agents,
                    Available,
                    Some(".claude/skills/onboarding/SKILL.md"),
                    Some("/onboarding"),
                ),
                row(
                    "Dev/.claude/skills/onboarding/SKILL.md",
                    Claude,
                    Available,
                    None,
                    Some("/Dev:onboarding"),
                ),
                row(
                    "Dev/Areas/demo/.claude/skills/onboarding/SKILL.md",
                    Claude,
                    Available,
                    None,
                    Some("/Dev/Areas/demo:onboarding"),
                ),
            ]
        );
    }

    #[test]
    fn duplicates_stay_per_provider_and_disabled_or_broken_copies_are_not_merged() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("Dev")).unwrap();
        // Codex sees one flat set: two .agents copies of a name clash even in
        // different directories, and the .claude copy is not paired.
        write(root, ".agents/skills/review", "review");
        write(root, "Dev/.agents/skills/review", "review");
        write(root, ".claude/skills/review", "review");
        // Two folders in one .claude/skills declaring the same name clash.
        write(root, ".claude/skills/one", "same");
        write(root, ".claude/skills/two", "same");
        // A disabled side and an unreadable side keep their own rows.
        write(root, ".agents/skills/off", "off");
        write(root, ".claude/skills/off", "off");
        write(root, ".agents/skills/broken", "broken");
        let broken = root.join(".claude/skills/broken");
        fs::create_dir_all(&broken).unwrap();
        fs::write(
            broken.join("SKILL.md"),
            vec![b'x'; MAX_SOURCE_BYTES as usize + 1],
        )
        .unwrap();
        let snapshot = discover(
            root,
            &root.join("Dev"),
            &[PathBuf::from(".claude/skills/off/SKILL.md")],
        );
        assert!(snapshot.diagnostics.is_empty());
        use Availability::{Available, Disabled, Duplicate};
        use Provider::{Agents, Claude};
        let too_large = Availability::Unavailable("Source exceeds 64 KiB".into());
        assert_eq!(
            rows(&snapshot),
            vec![
                row(
                    ".agents/skills/broken/SKILL.md",
                    Agents,
                    Available,
                    None,
                    None
                ),
                row(".agents/skills/off/SKILL.md", Agents, Available, None, None),
                row(
                    ".agents/skills/review/SKILL.md",
                    Agents,
                    Duplicate,
                    None,
                    None
                ),
                row(
                    ".claude/skills/broken/SKILL.md",
                    Claude,
                    too_large,
                    None,
                    None
                ),
                row(".claude/skills/off/SKILL.md", Claude, Disabled, None, None),
                row(".claude/skills/one/SKILL.md", Claude, Duplicate, None, None),
                row(
                    ".claude/skills/review/SKILL.md",
                    Claude,
                    Available,
                    None,
                    Some("/review"),
                ),
                row(".claude/skills/two/SKILL.md", Claude, Duplicate, None, None),
                row(
                    "Dev/.agents/skills/review/SKILL.md",
                    Agents,
                    Duplicate,
                    None,
                    None
                ),
            ]
        );
    }

    #[test]
    fn claude_frontmatter_is_optional_and_defaults_like_claude_code() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let put = |folder: &str, text: &str| {
            let path = root.join(folder).join("SKILL.md");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        put(".claude/skills/bare", "\n\n# Bare skill\nBody\n");
        put(
            ".claude/skills/partial",
            "---\nallowed-tools: Read\n---\n\n  Do the thing  \n",
        );
        put(
            ".claude/skills/named",
            "---\nname: renamed\n---\nFirst line\n",
        );
        put(
            ".claude/skills/broken-yaml",
            "---\nname: [unclosed\n---\nStill loads\n",
        );
        put(".claude/skills/unclosed", "---\nname: never\nText\n");
        put(".claude/skills/empty", "");
        // Codex keeps requiring frontmatter with both fields.
        put(".agents/skills/bare-agents", "# Bare skill\n");
        let snapshot = discover(root, root, &[]);
        assert!(snapshot.diagnostics.is_empty());
        let found: Vec<_> = snapshot
            .skills
            .iter()
            .map(|s| {
                (
                    s.source
                        .parent()
                        .unwrap()
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    s.name.clone(),
                    s.description.clone(),
                    s.availability.clone(),
                )
            })
            .collect();
        let ok = |dir: &str, name: &str, description: &str| {
            (
                dir.to_owned(),
                name.to_owned(),
                description.to_owned(),
                Availability::Available,
            )
        };
        assert_eq!(
            found,
            vec![
                (
                    "bare-agents".to_owned(),
                    "bare-agents".to_owned(),
                    String::new(),
                    Availability::Unavailable("Missing YAML frontmatter".into()),
                ),
                ok("bare", "bare", "# Bare skill"),
                ok("broken-yaml", "broken-yaml", "Still loads"),
                ok("empty", "empty", ""),
                ok("named", "renamed", "First line"),
                ok("partial", "partial", "Do the thing"),
                ok("unclosed", "unclosed", "---"),
            ]
        );
        assert_eq!(snapshot.skills[1].claude_command.as_deref(), Some("/bare"));
        assert_eq!(
            snapshot.skills[4].claude_command.as_deref(),
            Some("/renamed")
        );
    }

    #[test]
    fn open_rejects_sources_outside_the_two_skill_folders() {
        let temp = tempfile::tempdir().unwrap();
        write(temp.path(), ".codex/skills/other", "other");
        write(temp.path(), ".claude/commands/x", "x");
        for source in [
            ".codex/skills/other/SKILL.md",
            ".claude/commands/x/SKILL.md",
        ] {
            assert_eq!(
                open_source(temp.path(), Path::new(source)),
                Err("Unsupported skill source identity".into())
            );
        }
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
        assert!(open_source(temp.path(), &snapshot.skills[0].source).is_ok());
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
        assert!(open_source(temp.path(), &good.source).is_err());
        assert!(!discover(temp.path(), outside.path(), &[])
            .diagnostics
            .is_empty());
    }
}
