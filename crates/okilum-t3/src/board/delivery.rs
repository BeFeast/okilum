//! Read-only, exact-build delivery facts. This module never publishes or accepts a build.
use super::{https_link, relative_note, Binding, Scope, Source};
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use std::{collections::BTreeMap, io::Read, path::Path};

mod read;
pub use read::refresh;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Macos,
    Windows,
    Linux,
}
impl Platform {
    pub const ALL: [Self; 3] = [Self::Macos, Self::Windows, Self::Linux];
    pub fn label(self) -> &'static str {
        match self {
            Self::Macos => "Mac",
            Self::Windows => "Windows",
            Self::Linux => "Linux",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Sprint,
    Beta,
    Stable,
}
impl Channel {
    pub const ALL: [Self; 3] = [Self::Sprint, Self::Beta, Self::Stable];
    pub fn label(self) -> &'static str {
        match self {
            Self::Sprint => "Sprint",
            Self::Beta => "Beta",
            Self::Stable => "Stable",
        }
    }
}
pub type Track = (Platform, Channel);
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub feeds: Vec<Feed>,
    /// Markdown notes containing explicit delivery frontmatter, never arbitrary prose.
    #[serde(default)]
    pub evidence: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Feed {
    pub platform: Platform,
    pub channel: Channel,
    pub url: String,
    pub format: Format,
    /// Required for Velopack: immutable release.json URL with a literal {build} slot.
    pub metadata: Option<String>,
    /// Exact artifact selected by the project owner in a Forgejo Actions run.
    pub artifact_name: Option<String>,
    #[serde(default)]
    pub next: String,
    pub blocker: Option<String>,
}
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Appcast,
    Velopack,
    Appimage,
    Receipt,
    Actions,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Build {
    pub platform: Platform,
    pub channel: Channel,
    pub build: String,
    pub source: String,
    pub artifact: String,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub issue: u64,
    pub pr: u64,
    pub platform: Platform,
    pub channel: Channel,
    pub build: String,
    pub source: String,
    pub artifact: String,
    pub scenario: String,
    /// Installation and acceptance are separate explicit assertions in this note.
    pub installed_at: Option<String>,
    pub accepted_by: Option<String>,
    pub accepted_at: Option<String>,
    #[serde(skip)]
    pub note: String,
}
impl Evidence {
    fn matches(&self, build: &Build) -> bool {
        self.platform == build.platform
            && self.channel == build.channel
            && self.build == build.build
            && self.source == build.source
            && self.artifact == build.artifact
    }
    pub fn accepted(&self) -> bool {
        self.accepted_by.as_deref() == Some("Oleg") && self.accepted_at.is_some()
    }
}
#[derive(Clone, Debug, Default)]
pub struct Ci {
    pub pr: u64,
    pub source: String,
    pub state: String,
    pub links: Vec<String>,
    /// A per-build proof; errors remain unknown, not a guessed ancestry relation.
    pub inclusion: BTreeMap<String, Result<bool, String>>,
}
#[derive(Clone, Debug, Default)]
pub struct Artifact {
    pub available: bool,
}
#[derive(Default)]
pub struct Snapshot {
    pub config: Source<Option<Config>>,
    pub feeds: BTreeMap<Track, Source<Option<Build>>>,
    pub artifacts: BTreeMap<String, Source<Artifact>>,
    pub ci: BTreeMap<u64, Source<Ci>>,
    pub evidence: BTreeMap<String, Source<Vec<Evidence>>>,
}
pub struct Update {
    pub config: Result<Option<Config>, String>,
    pub feeds: Reads<Track, Option<Build>>,
    pub artifacts: Reads<String, Artifact>,
    pub ci: Reads<u64, Ci>,
    pub evidence: Reads<String, Vec<Evidence>>,
}
pub type Reads<K, T> = Vec<(K, Result<T, String>)>;
impl Snapshot {
    pub fn accept(&mut self, update: Update, at: u64) {
        let previous = self.config.value.clone();
        self.config.accept(update.config, at);
        if self.config.error.is_none() {
            if let (Some(old), Some(new)) = (&previous, &self.config.value) {
                for feed in &new.feeds {
                    if old.feeds.iter().any(|f| {
                        f.platform == feed.platform
                            && f.channel == feed.channel
                            && (f.url != feed.url
                                || f.format != feed.format
                                || f.metadata != feed.metadata
                                || f.artifact_name != feed.artifact_name)
                    }) {
                        self.feeds.remove(&(feed.platform, feed.channel));
                    }
                }
            }
        }
        if self.config.error.is_none() && self.config.value.is_none() {
            self.feeds.clear();
            self.artifacts.clear();
            self.ci.clear();
            self.evidence.clear();
            return;
        }
        for (key, result) in update.feeds {
            self.feeds.entry(key).or_default().accept(result, at);
        }
        for (key, result) in update.artifacts {
            self.artifacts.entry(key).or_default().accept(result, at);
        }
        let active_prs: std::collections::BTreeSet<_> =
            update.ci.iter().map(|(pr, _)| *pr).collect();
        if self.config.error.is_none() {
            self.ci.retain(|pr, _| active_prs.contains(pr));
        }
        for (key, result) in update.ci {
            self.ci.entry(key).or_default().accept(result, at);
        }
        for (key, result) in update.evidence {
            self.evidence.entry(key).or_default().accept(result, at);
        }
        if self.config.error.is_none() {
            if let Some(config) = &self.config.value {
                self.feeds.retain(|track, _| {
                    config
                        .feeds
                        .iter()
                        .any(|feed| (feed.platform, feed.channel) == *track)
                });
                self.evidence
                    .retain(|note, _| config.evidence.contains(note));
                let artifacts: std::collections::BTreeSet<_> = self
                    .feeds
                    .values()
                    .filter_map(|s| s.value.as_ref().map(|b| b.artifact.clone()))
                    .collect();
                self.artifacts.retain(|url, _| artifacts.contains(url));
            }
        }
    }
    pub fn enabled(&self) -> bool {
        self.config.value.is_some() || self.config.error.is_some()
    }
    pub fn rows(&self, scope: &Scope, platform: Platform, at: u64, interval: u64) -> Vec<Row> {
        let Some(config) = &self.config.value else {
            return Vec::new();
        };
        config
            .feeds
            .iter()
            .filter(|f| f.platform == platform)
            .map(|feed| {
                let track = (feed.platform, feed.channel);
                let source = self.feeds.get(&track);
                let build = source.and_then(|s| s.value.as_ref());
                let mut row = Row {
                    track,
                    build: build.cloned(),
                    state: "Build unknown".into(),
                    evidence: None,
                    ready: false,
                    accepted: false,
                };
                let Some(build) = build else { return row };
                if problem(&self.config, at, interval).is_some()
                    || source.is_none_or(|s| problem(s, at, interval).is_some())
                {
                    row.state = "Feed unavailable or stale".into();
                    return row;
                }
                let mut included = false;
                let mut failed_ci = false;
                for pr in &scope.pull_requests {
                    let Some(ci) = self.ci.get(pr) else { continue };
                    if problem(ci, at, interval).is_some() {
                        continue;
                    }
                    if ci.value.inclusion.get(&build.source) == Some(&Ok(true)) {
                        included = true;
                        failed_ci |= matches!(ci.value.state.as_str(), "failure" | "error");
                    }
                }
                if !included {
                    row.state = "Source inclusion unknown".into();
                    return row;
                }
                let available = self
                    .artifacts
                    .get(&build.artifact)
                    .is_some_and(|s| problem(s, at, interval).is_none() && s.value.available);
                let records: Vec<_> = config
                    .evidence
                    .iter()
                    .filter_map(|note| self.evidence.get(note))
                    .filter(|s| problem(s, at, interval).is_none())
                    .flat_map(|s| &s.value)
                    .filter(|e| {
                        e.issue == scope.number
                            && scope.pull_requests.contains(&e.pr)
                            && e.matches(build)
                            && self.ci.get(&e.pr).is_some_and(|ci| {
                                problem(ci, at, interval).is_none()
                                    && ci.value.inclusion.get(&build.source) == Some(&Ok(true))
                            })
                    })
                    .collect();
                if records.iter().any(|e| e.scenario.trim().is_empty()) {
                    row.state = "Scenario missing".into();
                    return row;
                }
                // Multiple exact-build evidence records with different acceptance assertions are a conflict.
                if records.iter().any(|a| {
                    records
                        .iter()
                        .any(|b| a.accepted() != b.accepted() || a.scenario != b.scenario)
                }) {
                    row.state = "Conflicting acceptance evidence".into();
                    return row;
                }
                row.evidence = records.first().map(|e| (*e).clone());
                row.ready = available && row.evidence.is_some() && !failed_ci;
                row.accepted = records.first().is_some_and(|e| e.accepted());
                row.state = if failed_ci {
                    "CI failed; source is included"
                } else if !available {
                    "Artifact unavailable"
                } else if records.is_empty() {
                    if matches!(feed.format, Format::Actions | Format::Receipt) {
                        "Artifact metadata; scenario/evidence missing"
                    } else {
                        "Published; scenario/evidence missing"
                    }
                } else if row.accepted {
                    "Verified by Oleg on this build"
                } else {
                    "Available to try; not accepted"
                }
                .into();
                row
            })
            .collect()
    }
    pub fn problems(&self, at: u64, interval: u64) -> Vec<String> {
        let mut rows = Vec::new();
        let mut add = |name: String, last: Option<u64>, detail: Option<String>| {
            if let Some(detail) = detail {
                rows.push(format!(
                    "{name}: {detail} · last success {}",
                    last.map_or("never".into(), |last| format!(
                        "{}s ago",
                        at.saturating_sub(last)
                    ))
                ));
            }
        };
        add(
            "Delivery binding".into(),
            self.config.last_success,
            problem(&self.config, at, interval),
        );
        let Some(config) = &self.config.value else {
            return rows;
        };
        for feed in &config.feeds {
            if let Some(s) = self.feeds.get(&(feed.platform, feed.channel)) {
                add(
                    format!("{} {} feed", feed.platform.label(), feed.channel.label()),
                    s.last_success,
                    problem(s, at, interval),
                );
            }
        }
        for (pr, s) in &self.ci {
            add(
                format!("PR #{pr} CI"),
                s.last_success,
                problem(s, at, interval),
            );
        }
        for (note, s) in &self.evidence {
            if config.evidence.contains(note) {
                add(note.clone(), s.last_success, problem(s, at, interval));
            }
        }
        for (url, s) in &self.artifacts {
            add(
                format!("Artifact {url}"),
                s.last_success,
                problem(s, at, interval),
            );
        }
        rows
    }
}
#[derive(Clone, Debug)]
pub struct Row {
    pub track: Track,
    pub build: Option<Build>,
    pub state: String,
    pub evidence: Option<Evidence>,
    pub ready: bool,
    pub accepted: bool,
}
pub fn problem<T>(source: &Source<T>, at: u64, interval: u64) -> Option<String> {
    if let Some(error) = &source.error {
        return Some(error.clone());
    }
    match source.last_success {
        None => Some("not read".into()),
        Some(last) if at.saturating_sub(last) > interval.saturating_mul(2) => Some(format!(
            "stale · {}s since last success",
            at.saturating_sub(last)
        )),
        _ => None,
    }
}
pub fn sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn bounded(reader: impl Read) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 1024 * 1024,
        "Delivery metadata exceeds 1 MiB"
    );
    Ok(bytes)
}
fn local(area: &Path, note: &str) -> Result<Vec<u8>> {
    ensure!(
        relative_note(note) || note == "ops/board-delivery.toml",
        "Invalid evidence note path"
    );
    let canonical = area.canonicalize()?;
    let path = area.join(note).canonicalize()?;
    ensure!(path.starts_with(canonical), "Evidence path escapes project");
    bounded(std::fs::File::open(path)?)
}
pub fn config(area: &Path) -> Result<Option<Config>> {
    let path = area.join("ops/board-delivery.toml");
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        result => {
            result?;
        }
    }
    let bytes = local(area, "ops/board-delivery.toml")?;
    let value: Config = toml::from_str(std::str::from_utf8(&bytes)?)?;
    ensure!(value.version == 1, "Unsupported delivery binding version");
    ensure!(
        value.feeds.len() <= 9 && value.evidence.len() <= 100,
        "Too many delivery bindings"
    );
    let mut tracks = std::collections::BTreeSet::new();
    for feed in &value.feeds {
        ensure!(
            tracks.insert((feed.platform, feed.channel)),
            "Duplicate platform/channel feed"
        );
        ensure!(https_link(&feed.url), "Feed requires credential-free HTTPS");
        if let Some(url) = &feed.metadata {
            ensure!(
                (feed.format != Format::Velopack || url.matches("{build}").count() == 1)
                    && https_link(&url.replace("{build}", "1")),
                "Invalid archive URL template"
            );
        }
        ensure!(
            feed.format != Format::Velopack || feed.metadata.is_some(),
            "Velopack requires explicit archive metadata binding"
        );
        ensure!(
            feed.format != Format::Actions
                || feed
                    .artifact_name
                    .as_ref()
                    .is_some_and(|s| !s.trim().is_empty()),
            "Actions requires an explicit platform artifact name"
        );
        ensure!(
            feed.format != Format::Actions || feed.channel == Channel::Sprint,
            "Actions artifacts do not prove Beta/Stable publication"
        );
        ensure!(
            url::Url::parse(&feed.url)?.host_str() != Some("api.github.com"),
            "GitHub API is not a Board source"
        );
    }
    ensure!(
        value.evidence.iter().all(|n| relative_note(n)),
        "Evidence must name project Markdown notes"
    );
    Ok(Some(value))
}
pub fn evidence(area: &Path, note: &str) -> Result<Vec<Evidence>> {
    let bytes = local(area, note)?;
    parse_evidence(std::str::from_utf8(&bytes)?, note)
}
pub(crate) fn parse_evidence(text: &str, note: &str) -> Result<Vec<Evidence>> {
    let text = text.replace("\r\n", "\n");
    let front = text
        .strip_prefix("---\n")
        .and_then(|text| text.split_once("\n---").map(|p| p.0))
        .context("Delivery note needs explicit YAML frontmatter")?;
    let doc: serde_yaml::Value = serde_yaml::from_str(front)?;
    let mut rows: Vec<Evidence> = serde_yaml::from_value(
        doc.get("delivery")
            .cloned()
            .context("Missing delivery evidence list")?,
    )?;
    ensure!(rows.len() <= 100, "Too many evidence records");
    for row in &mut rows {
        ensure!(
            row.issue > 0
                && row.pr > 0
                && sha(&row.source)
                && !row.build.trim().is_empty()
                && https_link(&row.artifact),
            "Invalid exact-build evidence identity"
        );
        ensure!(!row.scenario.trim().is_empty(), "Evidence needs a scenario");
        ensure!(
            row.accepted_by.is_some() == row.accepted_at.is_some(),
            "Acceptance needs both author and time"
        );
        ensure!(
            [row.installed_at.as_deref(), row.accepted_at.as_deref()]
                .into_iter()
                .flatten()
                .all(|s| !s.trim().is_empty()),
            "Evidence timestamps cannot be empty"
        );
        row.note = note.into();
    }
    Ok(rows)
}

#[cfg(test)]
mod tests;
