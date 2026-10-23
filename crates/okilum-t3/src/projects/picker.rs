//! Shared presentation model for the title-bar picker and command palette.
//! Discovery, manifest parsing and live manager checks stay in `projects`.
use super::{Discovery, Kind, Manager, Project, Status};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

pub const RECENT_LIMIT: usize = 5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KindFilter {
    #[default]
    All,
    Code,
    Knowledge,
    Hybrid,
}
impl KindFilter {
    pub const ALL: [Self; 4] = [Self::All, Self::Code, Self::Knowledge, Self::Hybrid];
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Code => "Code",
            Self::Knowledge => "Knowledge",
            Self::Hybrid => "Hybrid",
        }
    }
    fn matches(self, kind: Kind) -> bool {
        self == Self::All
            || matches!(
                (self, kind),
                (Self::Code, Kind::Code)
                    | (Self::Knowledge, Kind::Knowledge)
                    | (Self::Hybrid, Kind::Hybrid)
            )
    }
}
pub fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Code => "code",
        Kind::Knowledge => "knowledge",
        Kind::Hybrid => "hybrid",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagerIndicator {
    Running,
    NotStarted,
    Idle,
    Checking,
    Unreachable,
}
impl ManagerIndicator {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "Manager running",
            Self::NotStarted => "Manager not started",
            Self::Idle => "Manager idle",
            Self::Checking => "Checking manager…",
            Self::Unreachable => "Manager unreachable",
        }
    }
}
/// A status is valid only for the manager pointer used by that live listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagerObservation {
    pub manager: Manager,
    pub status: Status,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupKey {
    Recent,
    Domain(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowKey {
    pub group: GroupKey,
    pub folder: PathBuf,
}
#[derive(Clone, Debug)]
pub struct Row {
    pub key: RowKey,
    pub project: Project,
    pub domain: String,
    /// Human breadcrumbs distinguish identical names without exposing absolute paths.
    pub location: String,
    pub manager: ManagerIndicator,
}
#[derive(Clone, Debug)]
pub struct Group {
    pub key: GroupKey,
    pub label: String,
    /// Matching projects, including those hidden by collapse.
    pub count: usize,
    pub collapsed: bool,
    pub rows: Vec<Row>,
}

pub struct PickerModel {
    vault: PathBuf,
    projects: Vec<Project>,
    errors: Vec<(PathBuf, String)>,
    query: String,
    filter: KindFilter,
    collapsed: BTreeSet<String>,
    recent: Vec<PathBuf>,
    observations: BTreeMap<PathBuf, ManagerObservation>,
}
impl PickerModel {
    /// `recent` is most-recent-first, scoped to this vault. It records confirmed
    /// opens, not mere selection requests. The host owns preference persistence.
    pub fn new(vault: PathBuf, discovery: Discovery, recent: Vec<PathBuf>) -> Self {
        let mut model = Self {
            vault,
            projects: Vec::new(),
            errors: Vec::new(),
            query: String::new(),
            filter: KindFilter::All,
            collapsed: BTreeSet::new(),
            recent,
            observations: BTreeMap::new(),
        };
        model.refresh(discovery);
        model
    }
    pub fn refresh(&mut self, discovery: Discovery) {
        self.errors = discovery.errors;
        self.projects.clear();
        let mut seen = BTreeSet::new();
        for project in discovery.projects {
            if domain(&self.vault, &project.folder).is_none() {
                self.errors
                    .push((project.folder, "Project is outside this vault".into()));
            } else if seen.insert(project.folder.clone()) {
                self.projects.push(project);
            }
        }
        self.projects.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.folder.cmp(&b.folder))
        });
    }
    pub fn query(&self) -> &str {
        &self.query
    }
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
    }
    pub fn filter(&self) -> KindFilter {
        self.filter
    }
    pub fn set_filter(&mut self, filter: KindFilter) {
        self.filter = filter;
    }
    pub fn errors(&self) -> &[(PathBuf, String)] {
        &self.errors
    }
    pub fn recent(&self) -> &[PathBuf] {
        &self.recent
    }
    /// Unchanged observations need no UI work. A changed status can update
    /// dots without disturbing grouped results, selection or scroll position.
    pub fn set_observations(
        &mut self,
        observations: BTreeMap<PathBuf, ManagerObservation>,
    ) -> bool {
        if self.observations == observations {
            return false;
        }
        self.observations = observations;
        true
    }
    pub fn manager_indicator(&self, folder: &Path) -> Option<ManagerIndicator> {
        self.project(folder).map(|project| self.indicator(project))
    }
    pub fn toggle_domain(&mut self, domain: &str) {
        // Searching reveals matching rows; clearing restores the user's collapse state.
        if self.query.trim().is_empty() && !self.collapsed.remove(domain) {
            self.collapsed.insert(domain.to_owned());
        }
    }
    pub fn project(&self, folder: &Path) -> Option<&Project> {
        self.projects.iter().find(|p| p.folder == folder)
    }
    /// Call only after the host has successfully activated the selected project.
    pub fn record_opened(&mut self, folder: &Path) -> bool {
        if self.project(folder).is_none() {
            return false;
        }
        self.recent.retain(|p| p != folder);
        self.recent.insert(0, folder.to_owned());
        self.recent.truncate(32);
        true
    }
    fn indicator(&self, project: &Project) -> ManagerIndicator {
        if project.manager == Manager::NotStarted {
            return ManagerIndicator::NotStarted;
        }
        let Some(observation) = self
            .observations
            .get(&project.folder)
            .filter(|o| o.manager == project.manager)
        else {
            return ManagerIndicator::Checking;
        };
        match &observation.status {
            Status::Running(_) => ManagerIndicator::Running,
            Status::NotStarted | Status::Missing | Status::Settled => ManagerIndicator::NotStarted,
            Status::NotRunning(_) => ManagerIndicator::Idle,
            Status::Unreachable(_) => ManagerIndicator::Unreachable,
        }
    }
    fn matches(&self, project: &Project) -> bool {
        if !self.filter.matches(project.kind) {
            return false;
        }
        let relative = project
            .folder
            .strip_prefix(&self.vault)
            .unwrap_or(Path::new(""));
        let haystack = format!(
            "{} {} {}",
            project.name,
            relative.display(),
            kind_label(project.kind)
        )
        .to_lowercase();
        self.query
            .split_whitespace()
            .all(|word| haystack.contains(&word.to_lowercase()))
    }
    fn row(&self, project: &Project, group: GroupKey) -> Row {
        let relative = project
            .folder
            .strip_prefix(&self.vault)
            .expect("validated project");
        let location = relative
            .parent()
            .unwrap_or(Path::new(""))
            .components()
            .filter_map(|c| {
                if let Component::Normal(s) = c {
                    Some(s.to_string_lossy().into_owned())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(" › ");
        Row {
            key: RowKey {
                group,
                folder: project.folder.clone(),
            },
            project: project.clone(),
            domain: domain(&self.vault, &project.folder).expect("validated project"),
            location,
            manager: self.indicator(project),
        }
    }
    pub fn groups(&self) -> Vec<Group> {
        let mut groups = Vec::new();
        let mut seen = BTreeSet::new();
        let recent: Vec<_> = self
            .recent
            .iter()
            .filter(|p| seen.insert(*p))
            .filter_map(|p| self.project(p))
            .filter(|p| self.matches(p))
            .take(RECENT_LIMIT)
            .map(|p| self.row(p, GroupKey::Recent))
            .collect();
        if !recent.is_empty() {
            groups.push(Group {
                key: GroupKey::Recent,
                label: "Recent".into(),
                count: recent.len(),
                collapsed: false,
                rows: recent,
            });
        }
        let mut domains = BTreeMap::<String, Vec<&Project>>::new();
        for project in self.projects.iter().filter(|p| self.matches(p)) {
            domains
                .entry(domain(&self.vault, &project.folder).expect("validated project"))
                .or_default()
                .push(project);
        }
        let mut domains: Vec<_> = domains.into_iter().collect();
        domains.sort_by(|(a, _), (b, _)| a.to_lowercase().cmp(&b.to_lowercase()).then(a.cmp(b)));
        for (label, projects) in domains {
            let key = GroupKey::Domain(label.clone());
            let collapsed = self.query.trim().is_empty() && self.collapsed.contains(&label);
            groups.push(Group {
                key: key.clone(),
                count: projects.len(),
                collapsed,
                rows: if collapsed {
                    Vec::new()
                } else {
                    projects
                        .into_iter()
                        .map(|p| self.row(p, key.clone()))
                        .collect()
                },
                label,
            });
        }
        groups
    }
}

/// `Project.folder` is already obtained from its `ops/project.yaml` by discovery;
/// use its first vault-relative component, never infer a kind or PARA category.
fn domain(vault: &Path, folder: &Path) -> Option<String> {
    let relative = folder.strip_prefix(vault).ok()?;
    if relative
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return None;
    }
    match relative.components().next() {
        Some(Component::Normal(name)) => Some(name.to_string_lossy().into_owned()),
        None => Some("Vault".into()),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
