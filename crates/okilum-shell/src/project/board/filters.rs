//! Shared Board/List filtering. Only presentation state is persisted; sources
//! remain canonical and no filter writes to the project or its issues.
use super::*;
use okilum_t3::board::{Issue, Scope};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

mod ui;
pub(super) use ui::Controls;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum Chip {
    #[default]
    All,
    Oleg,
    Bugs,
    Testers,
    Active,
}
impl Chip {
    pub const ALL: [Self; 5] = [
        Self::All,
        Self::Oleg,
        Self::Bugs,
        Self::Testers,
        Self::Active,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "Все",
            Self::Oleg => "От Олега",
            Self::Bugs => "Баги",
            Self::Testers => "От testers",
            Self::Active => "Агенты сейчас",
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Oleg => "oleg",
            Self::Bugs => "bugs",
            Self::Testers => "testers",
            Self::Active => "active",
        }
    }
    fn matches(self, issue: Option<&Issue>, active: bool) -> bool {
        match self {
            Self::All => true,
            Self::Oleg => issue.is_some_and(|i| i.has_label("from-oleg")),
            Self::Bugs => issue.is_some_and(|i| i.has_label("bug")),
            Self::Testers => issue.is_some_and(|i| i.has_label("from-tester")),
            Self::Active => active,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Model {
    pub chip: Chip,
    pub labels: BTreeSet<String>,
    pub executor: Option<String>,
    pub search: String,
}
impl Model {
    pub fn is_active(&self) -> bool {
        self != &Self::default()
    }
    fn extras_match(&self, scope: &Scope, issue: Option<&Issue>, query: &str) -> bool {
        self.executor.as_ref().is_none_or(|e| e == &scope.executor)
            && self
                .labels
                .iter()
                .all(|name| issue.is_some_and(|i| i.has_label(name)))
            && search_matches(
                issue.map(|i| i.title.as_str()).unwrap_or(&scope.expected),
                Some(scope.number),
                query,
            )
    }
    pub fn matches_local(&self, card: &okilum_t3::board::local::Card) -> bool {
        self.chip == Chip::All
            && self.labels.is_empty()
            && self.executor.is_none()
            && search_matches(&card.title, None, &self.search.trim().to_lowercase())
    }
    pub fn load(directory: &Path, project: &Path) -> Self {
        std::fs::read(Self::path(directory, project))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, directory: &Path, project: &Path) -> anyhow::Result<()> {
        use std::io::Write as _;
        std::fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        file.write_all(&serde_json::to_vec(self)?)?;
        file.persist(Self::path(directory, project))?;
        Ok(())
    }
    fn path(directory: &Path, project: &Path) -> PathBuf {
        use sha2::{Digest, Sha256};
        let project = project
            .canonicalize()
            .unwrap_or_else(|_| project.to_path_buf());
        let hash = Sha256::digest(project.as_os_str().as_encoded_bytes());
        directory.join(format!("board-filters-{hash:x}.json"))
    }
}
fn search_matches(title: &str, number: Option<u64>, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let digits = query.strip_prefix('#').unwrap_or(query);
    let number_match = !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && digits
            .parse::<u64>()
            .ok()
            .zip(number)
            .is_some_and(|(a, b)| a == b);
    number_match || title.to_lowercase().contains(query)
}

/// Thread activity is evidence for this issue only when it is the explicit
/// current task. Ambiguous/settled/missing run identities cannot supply a match.
fn active(scope: &Scope, threads: &[okilum_t3::ThreadSummary]) -> bool {
    if !scope.current_task {
        return false;
    }
    let Some(id) = scope.agent_thread_id.as_deref() else {
        return false;
    };
    let mut matches = threads.iter().filter(|t| t.thread_id == id);
    let Some(thread) = matches.next() else {
        return false;
    };
    matches.next().is_none()
        && !thread.settled
        && thread
            .active_run_id
            .as_ref()
            .is_some_and(|run| !run.is_empty())
        && matches!(
            thread.status.as_str(),
            "preparing" | "starting" | "running" | "waiting"
        )
}

pub(super) struct Projection {
    /// The same narrowed binding feeds either presentation, including group counts.
    pub binding: Binding,
    pub chip_counts: [usize; 5],
    pub counts: [usize; 5],
    pub scopes: [Vec<usize>; 5],
    pub locals: [Vec<usize>; 5],
    pub labels: BTreeSet<String>,
    pub executors: BTreeSet<String>,
}
struct Cached {
    binding: Binding,
    issues: BTreeMap<u64, Issue>,
    threads: Vec<okilum_t3::ThreadSummary>,
    locals: Vec<okilum_t3::board::local::Card>,
    model: Model,
    discovered: bool,
    offline: bool,
    expires: u64,
    projection: Arc<Projection>,
}
#[derive(Default)]
pub(super) struct Cache(Option<Cached>);
impl Cache {
    pub fn invalidate(&mut self) {
        self.0 = None;
    }
    fn matches(&self, board: &Board, binding: &Binding, at: u64) -> bool {
        self.0.as_ref().is_some_and(|c| {
            c.binding == *binding
                && c.issues == board.snapshot.forgejo.value
                && c.threads == board.snapshot.t3.value
                && c.locals == board.local_cards.value.cards
                && c.model == board.filter
                && c.discovered == board.snapshot.forgejo.last_success.is_some()
                && c.offline == board.snapshot.forgejo.error.is_some()
                && at < c.expires
        })
    }
}
impl Board {
    pub(super) fn filtered_projection(&mut self) -> Arc<Projection> {
        let binding = self.binding.as_ref().expect("configured Board");
        let at = now();
        if !self.filter_cache.matches(self, binding, at) {
            let mut projected = if self.snapshot.forgejo.last_success.is_some() {
                okilum_t3::board::live::project(
                    binding,
                    &self.snapshot.forgejo.value,
                    self.projection_time(),
                )
            } else {
                binding.clone()
            };
            let mut result = Projection {
                binding: projected.clone(),
                chip_counts: [0; 5],
                counts: [0; 5],
                scopes: Default::default(),
                locals: Default::default(),
                labels: BTreeSet::new(),
                executors: BTreeSet::new(),
            };
            let query = self.filter.search.trim().to_lowercase();
            projected.issues.retain(|scope| {
                let issue = self.snapshot.forgejo.value.get(&scope.number);
                if let Some(issue) = issue {
                    result
                        .labels
                        .extend(issue.labels.iter().map(|l| l.name.clone()));
                }
                result.executors.insert(scope.executor.clone());
                let active = active(scope, &self.snapshot.t3.value);
                let extras = self.filter.extras_match(scope, issue, &query);
                if extras {
                    for (index, chip) in Chip::ALL.iter().enumerate() {
                        if chip.matches(issue, active) {
                            result.chip_counts[index] += 1;
                        }
                    }
                }
                extras && self.filter.chip.matches(issue, active)
            });
            result.binding = projected;
            for (index, scope) in result.binding.issues.iter().enumerate() {
                let column = self.delivery_column(scope) as usize;
                result.scopes[column].push(index);
                result.counts[column] += 1;
            }
            for (index, card) in self.local_cards.value.cards.iter().enumerate() {
                let mut all = self.filter.clone();
                all.chip = Chip::All;
                if all.matches_local(card) {
                    result.chip_counts[0] += 1;
                }
                if self.filter.matches_local(card) {
                    let column = card.state.column() as usize;
                    result.locals[column].push(index);
                    result.counts[column] += 1;
                }
            }
            // Invalid annotations remain visible, independently of filters.
            result.counts[Column::Queue as usize] += binding.card_problems.len();
            let mut expires = u64::MAX;
            if self.snapshot.forgejo.error.is_none() {
                for pull in self
                    .snapshot
                    .forgejo
                    .value
                    .values()
                    .flat_map(|i| &i.live_pulls)
                {
                    if let Some(merged) = pull.merged_at {
                        let expiry = merged.saturating_add(86401);
                        if expiry > at {
                            expires = expires.min(expiry);
                        }
                    }
                }
            }
            // Delivery evidence has freshness boundaries as well as source updates.
            if self.delivery.enabled() {
                expires = expires.min(at.saturating_add(15));
            }
            self.filter_cache.0 = Some(Cached {
                binding: binding.clone(),
                issues: self.snapshot.forgejo.value.clone(),
                threads: self.snapshot.t3.value.clone(),
                locals: self.local_cards.value.cards.clone(),
                model: self.filter.clone(),
                discovered: self.snapshot.forgejo.last_success.is_some(),
                offline: self.snapshot.forgejo.error.is_some(),
                expires,
                projection: Arc::new(result),
            });
        }
        self.filter_cache.0.as_ref().unwrap().projection.clone()
    }
}

#[cfg(test)]
mod tests;
