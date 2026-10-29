//! Explicit stage membership and independent read-only Board source snapshots (#1767).
use crate::ThreadSummary;
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Component, Path},
    time::Duration,
};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub version: u32,
    pub title: String,
    pub goal: String,
    pub repository: String,
    pub stage_note: String,
    #[serde(default = "refresh_default")]
    pub refresh_seconds: u64,
    pub issues: Vec<Scope>,
    #[serde(default)]
    pub attention: Vec<AttentionItem>,
    #[serde(default = "stuck_default")]
    pub stuck_after_seconds: u64,
    #[serde(skip)]
    pub registry_error: Option<String>,
}
/// Human-owned next actions. No issue/thread state creates these entries.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttentionItem {
    pub kind: AttentionKind,
    pub title: String,
    pub subline: String,
    /// Public HTTPS destination or an existing area-relative Markdown note.
    pub link: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    Decision,
    Try,
    Hands,
}
impl AttentionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Decision => "Decision",
            Self::Try => "Try",
            Self::Hands => "Your help",
        }
    }
}
fn attention_link(value: &str) -> bool {
    relative_note(value)
        || url::Url::parse(value).is_ok_and(|url| {
            url.scheme() == "https"
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
        })
}
fn stuck_default() -> u64 {
    30 * 60
}
fn refresh_default() -> u64 {
    30
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub number: u64,
    pub expected: String,
    pub executor: String,
    /// Stable issue ownership; independent of the executor's changing run IDs.
    #[serde(skip)]
    pub current_task: bool,
    /// The agent link is literal, never resolved by matching a display name.
    #[serde(default)]
    pub agent_thread_id: Option<String>,
    /// Human-maintained delivery/acceptance, never inferred from issue closure.
    #[serde(default)]
    pub stage: Option<HumanStage>,
    /// Explicit blocked marker until the live-status slice supplies elapsed time.
    #[serde(default)]
    pub stuck: bool,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub pull_requests: Vec<u64>,
    #[serde(default)]
    pub builds: Vec<BuildLink>,
    /// Only an explicit issue-to-run binding may establish execution.
    pub execution: Option<Execution>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanStage {
    Ready,
    Verified,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildLink {
    pub label: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Execution {
    pub thread_id: String,
    pub run_id: String,
}

pub fn relative_note(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('\\')
        && value.ends_with(".md")
        && Path::new(value)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}
impl Binding {
    pub fn parse(source: &str) -> Result<Self> {
        let binding: Self = serde_yaml::from_str(source).context("Invalid ops/board.yaml")?;
        ensure!(binding.version == 1, "Unsupported Board binding version");
        ensure!(
            !binding.title.trim().is_empty()
                && !binding.goal.trim().is_empty()
                && relative_note(&binding.stage_note),
            "Board requires a title and area-relative stage note"
        );
        ensure!(
            binding.refresh_seconds > 0,
            "Board refresh_seconds must be positive (default 30)"
        );
        let url = url::Url::parse(&binding.repository)?;
        ensure!(
            url.scheme() == "https"
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "Board requires a public HTTPS repository without credentials"
        );
        let parts = url.path().trim_matches('/').split('/').collect::<Vec<_>>();
        ensure!(
            parts.len() == 2
                && parts.iter().all(|p| !p.is_empty()
                    && p.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))),
            "Expected a Forgejo owner/repository URL"
        );
        ensure!(
            !binding.issues.is_empty(),
            "Board stage has no explicit issues"
        );
        for item in &binding.attention {
            ensure!(
                !item.title.trim().is_empty()
                    && !item.subline.trim().is_empty()
                    && attention_link(&item.link),
                "Attention items require kind, title, subline and a safe note/HTTPS link"
            );
        }
        ensure!(
            binding.stuck_after_seconds > 0,
            "stuck_after_seconds must be positive"
        );
        let mut seen = std::collections::BTreeSet::new();
        for scope in &binding.issues {
            ensure!(
                scope.number > 0 && seen.insert(scope.number),
                "Board issue references must be positive and unique"
            );
            ensure!(
                !scope.expected.trim().is_empty() && !scope.executor.trim().is_empty(),
                "Each issue requires an expected result and executor binding"
            );
            if let Some(id) = &scope.agent_thread_id {
                ensure!(
                    !id.trim().is_empty() && !id.chars().any(char::is_whitespace),
                    "agent_thread_id must name one exact T3 thread"
                );
            }
            ensure!(
                scope.notes.iter().all(|n| relative_note(n)),
                "Related notes must be area-relative Markdown paths"
            );
            for build in &scope.builds {
                let url = url::Url::parse(&build.url)?;
                ensure!(
                    !build.label.trim().is_empty()
                        && url.scheme() == "https"
                        && url.host_str().is_some()
                        && url.username().is_empty()
                        && url.password().is_none(),
                    "Build links require a label and credential-free HTTPS URL"
                );
            }
            if let Some(run) = &scope.execution {
                ensure!(
                    !run.thread_id.trim().is_empty() && !run.run_id.trim().is_empty(),
                    "Execution requires both thread and run identity"
                );
            }
        }
        Ok(binding)
    }
    pub fn read(area: &Path) -> Result<Option<Self>> {
        let path = area.join("ops/board.yaml");
        match std::fs::symlink_metadata(&path) {
            Ok(meta) => ensure!(
                meta.is_file(),
                "Board binding must be a regular file, not a symlink"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).context("Cannot read ops/board.yaml"),
        };
        let binding = Self::parse(&text)?;
        let project: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(area.join("ops/project.yaml"))?)?;
        let repo = url::Url::parse(&binding.repository)?;
        ensure!(
            project["repo"]["host"].as_str() == repo.host_str()
                && project["repo"]["full_name"].as_str() == Some(repo.path().trim_matches('/')),
            "Board repository differs from explicit project manifest"
        );
        let canonical = area.canonicalize()?;
        for note in std::iter::once(&binding.stage_note)
            .chain(binding.issues.iter().flat_map(|s| &s.notes))
            .chain(
                binding
                    .attention
                    .iter()
                    .filter(|i| relative_note(&i.link))
                    .map(|i| &i.link),
            )
        {
            if area.join(note).exists() {
                ensure!(
                    area.join(note).canonicalize()?.starts_with(&canonical),
                    "Board note escapes the project through a symlink"
                );
            }
        }
        // The current registry supersedes literal IDs retained in old cards.
        // An unreadable registry cannot establish live executor ownership.
        let mut binding = binding;
        let registry = match crate::executors::Registry::read(area) {
            Ok(registry) => Some(registry),
            Err(error) => {
                binding.registry_error = Some(format!("{error:#}"));
                None
            }
        };
        for scope in &mut binding.issues {
            let entry = registry.as_ref().and_then(|registry| {
                registry
                    .executors
                    .iter()
                    .find(|entry| entry.role == scope.executor)
                    .or_else(|| {
                        registry
                            .executors
                            .iter()
                            .find(|entry| Some(&entry.thread_id) == scope.agent_thread_id.as_ref())
                    })
            });
            scope.current_task =
                entry.is_some_and(|entry| entry.current_task == Some(scope.number));
            scope.agent_thread_id = entry.map(|entry| entry.thread_id.clone());
        }
        Ok(Some(binding))
    }
    /// Presentation or attention edits retain issue snapshots and card selection.
    pub fn same_issue_source(&self, other: &Self) -> bool {
        self.repository == other.repository
            && self
                .issues
                .iter()
                .map(|i| i.number)
                .eq(other.issues.iter().map(|i| i.number))
    }
    pub fn issue_url(&self, number: u64) -> String {
        format!("{}/issues/{number}", self.repository.trim_end_matches('/'))
    }
    pub fn pull_url(&self, number: u64) -> String {
        format!("{}/pulls/{number}", self.repository.trim_end_matches('/'))
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub state: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub labels: Vec<IssueLabel>,
    /// Verified open references from this refresh, never inferred from a URL badge.
    #[serde(skip)]
    pub open_pull_requests: Vec<u64>,
}
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct IssueLabel {
    pub name: String,
}
impl Issue {
    pub fn has_label(&self, name: &str) -> bool {
        self.labels.iter().any(|label| label.name == name)
    }
}
/// Public requests only: no token, client credential store or GitHub bridge.
pub fn read_issues(binding: &Binding) -> Vec<(u64, Result<Issue, String>)> {
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Okilum-Board/1")
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return binding
                .issues
                .iter()
                .map(|s| (s.number, Err(e.to_string())))
                .collect()
        }
    };
    let repo = url::Url::parse(&binding.repository).expect("validated Board URL");
    let pulls = read_open_pulls(&client, &repo);
    binding
        .issues
        .iter()
        .map(|scope| {
            let result = (|| -> Result<Issue> {
                let endpoint = format!(
                    "{}/api/v1/repos{}/issues/{}",
                    repo.origin().ascii_serialization(),
                    repo.path().trim_end_matches('/'),
                    scope.number
                );
                let response = client
                    .get(endpoint)
                    .header("Accept", "application/json")
                    .send()?
                    .error_for_status()?;
                use std::io::Read;
                let mut bytes = Vec::new();
                response.take(512 * 1024 + 1).read_to_end(&mut bytes)?;
                ensure!(
                    bytes.len() <= 512 * 1024,
                    "Issue response exceeds 512 KiB; not truncated"
                );
                let mut issue: Issue = serde_json::from_slice(&bytes)?;
                issue.open_pull_requests = pulls
                    .as_ref()
                    .map_err(|e| anyhow::anyhow!("{e}"))?
                    .iter()
                    .filter(|pull| {
                        scope.pull_requests.contains(&pull.number)
                            || pull.references(&repo, scope.number)
                    })
                    .map(|pull| pull.number)
                    .collect();
                ensure!(
                    issue.number == scope.number,
                    "Forgejo returned a different issue"
                );
                Ok(issue)
            })();
            (scope.number, result.map_err(|e| format!("{e:#}")))
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct PullRequest {
    number: u64,
    state: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    merged: bool,
}
impl PullRequest {
    fn references(&self, repo: &url::Url, number: u64) -> bool {
        let text = format!("{}\n{}", self.title, self.body);
        let marker = format!("#{number}");
        let short = text.match_indices(&marker).any(|(at, _)| {
            text[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric() && !"_/.-".contains(c))
                && text[at + marker.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric() && c != '_')
        });
        let full = format!("{}/issues/{number}", repo.as_str().trim_end_matches('/'));
        short
            || text.match_indices(&full).any(|(at, _)| {
                text[at + full.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_ascii_digit())
            })
    }
}
fn read_open_pulls(
    client: &reqwest::blocking::Client,
    repo: &url::Url,
) -> Result<Vec<PullRequest>, String> {
    let read = || -> Result<Vec<PullRequest>> {
        let endpoint = format!(
            "{}/api/v1/repos{}/pulls?state=open&limit=50&page=1",
            repo.origin().ascii_serialization(),
            repo.path().trim_end_matches('/')
        );
        let response = client
            .get(endpoint)
            .header("Accept", "application/json")
            .send()?
            .error_for_status()?;
        ensure!(
            !response
                .headers()
                .get("Link")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.contains("rel=\"next\"")),
            "Open PR list exceeds one bounded page; working state was not guessed"
        );
        use std::io::Read;
        let mut bytes = Vec::new();
        response.take(512 * 1024 + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= 512 * 1024,
            "Open PR response exceeds 512 KiB; not truncated"
        );
        let pulls: Vec<PullRequest> = serde_json::from_slice(&bytes)?;
        Ok(pulls
            .into_iter()
            .filter(|p| p.state == "open" && !p.merged)
            .collect())
    };
    read().map_err(|error| format!("Cannot read open PR references: {error:#}"))
}

#[derive(Clone, Debug, Default)]
pub struct Source<T> {
    pub value: T,
    pub last_success: Option<u64>,
    pub error: Option<String>,
}
impl<T> Source<T> {
    pub fn accept(&mut self, result: Result<T, String>, now: u64) {
        match result {
            Ok(value) => {
                self.value = value;
                self.last_success = Some(now);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Queue,
    Working,
    Checks,
    Ready,
    Verified,
}
impl Column {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queue => "Queue",
            Self::Working => "In progress",
            Self::Checks => "Checks",
            Self::Ready => "Can try",
            Self::Verified => "Verified",
        }
    }
}
pub fn column(scope: &Scope, issue: Option<&Issue>, threads: &[ThreadSummary]) -> Column {
    match scope.stage {
        Some(HumanStage::Ready) => return Column::Ready,
        Some(HumanStage::Verified) => return Column::Verified,
        None => {}
    }
    if issue.is_some_and(|i| i.state == "closed") {
        return Column::Checks;
    }
    if !issue.is_some_and(|issue| issue.state == "open") {
        return Column::Queue;
    }
    if issue.is_some_and(|issue| !issue.open_pull_requests.is_empty()) {
        return Column::Working;
    }
    if scope.current_task {
        if let Some(id) = scope.agent_thread_id.as_deref() {
            let mut matching = threads.iter().filter(|thread| thread.thread_id == id);
            if let Some(thread) = matching.next() {
                if matching.next().is_none()
                    && !thread.settled
                    && thread
                        .active_run_id
                        .as_ref()
                        .is_some_and(|run| !run.is_empty())
                    && matches!(
                        thread.status.as_str(),
                        "preparing" | "starting" | "running" | "waiting"
                    )
                {
                    return Column::Working;
                }
            }
        }
    }
    Column::Queue
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ticket {
    pub owner: String,
    pub generation: u64,
}
#[derive(Default)]
pub struct Snapshot {
    pub forgejo: Source<BTreeMap<u64, Issue>>,
    pub t3: Source<Vec<ThreadSummary>>,
    owner: String,
    generation: u64,
    pending: u8,
}
impl Snapshot {
    pub fn new(owner: String) -> Self {
        Self {
            owner,
            ..Default::default()
        }
    }
    pub fn begin(&mut self) -> Option<Ticket> {
        if self.pending != 0 {
            return None;
        }
        self.pending = 2;
        Some(Ticket {
            owner: self.owner.clone(),
            generation: self.generation,
        })
    }
    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = 0;
    }
    pub fn matches(&self, ticket: &Ticket) -> bool {
        ticket.owner == self.owner && ticket.generation == self.generation
    }
    pub fn busy(&self) -> bool {
        self.pending != 0
    }
    pub fn issues(&mut self, ticket: &Ticket, values: Vec<(u64, Result<Issue, String>)>, now: u64) {
        if !self.matches(ticket) {
            return;
        }
        let mut errors = Vec::new();
        let mut complete = true;
        for (number, value) in values {
            match value {
                Ok(issue) => {
                    self.forgejo.value.insert(number, issue);
                }
                Err(error) => {
                    complete = false;
                    errors.push(format!("#{number}: {error}"));
                }
            }
        }
        if complete {
            self.forgejo.last_success = Some(now);
        }
        self.forgejo.error = (!errors.is_empty()).then(|| errors.join("; "));
        self.pending = self.pending.saturating_sub(1);
    }
    pub fn threads(
        &mut self,
        ticket: &Ticket,
        value: Result<Vec<ThreadSummary>, String>,
        now: u64,
    ) {
        if !self.matches(ticket) {
            return;
        }
        let value = value.and_then(|threads| {
            let mut ids = std::collections::BTreeSet::new();
            if threads.iter().all(|t| ids.insert(t.thread_id.clone())) {
                Ok(threads)
            } else {
                Err("T3 returned duplicate thread IDs; previous source retained".into())
            }
        });
        self.t3.accept(value, now);
        self.pending = self.pending.saturating_sub(1);
    }
}
#[cfg(test)]
mod tests;
