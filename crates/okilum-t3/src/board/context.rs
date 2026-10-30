//! Explicit project-note attention and next-stage context (#1769).
//! No current-stage UI, inferred decisions, canonical writes or execution controls.
use super::{delivery, relative_note, Source};
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};

const MAX_NOTE: usize = 256 * 1024;
const MAX_TOTAL: usize = 2 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub notes: Vec<String>,
    pub next_stage: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Attention,
    Decision,
    Blocker,
    Try,
    NextStage,
}
impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Attention => "Attention",
            Self::Decision => "Decision",
            Self::Blocker => "Blocked",
            Self::Try => "Try",
            Self::NextStage => "Next stage",
        }
    }
    fn key(self) -> &'static str {
        match self {
            Self::Attention => "attention",
            Self::Decision => "decision",
            Self::Blocker => "blocker",
            Self::Try => "try",
            Self::NextStage => "next_stage",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub kind: Kind,
    pub title: String,
    pub reason: String,
    pub consequence: String,
    pub issue: Option<u64>,
    pub platform: Option<delivery::Platform>,
    pub channel: Option<delivery::Channel>,
    /// Only an explicit resolution note can close this exact question revision.
    pub resolution: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub path: String,
    pub meta: Meta,
    pub revision: String,
    pub evidence: Vec<delivery::Evidence>,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resolution {
    pub decision: String,
    pub revision: String,
    pub by: String,
    pub at: String,
    pub outcome: String,
}
#[derive(Default)]
pub struct Snapshot {
    pub config: Source<Option<Config>>,
    pub notes: BTreeMap<String, Source<Option<Note>>>,
    pub resolutions: BTreeMap<String, Source<Option<Resolution>>>,
}
pub struct Update {
    pub config: Result<Option<Config>, String>,
    pub notes: delivery::Reads<String, Option<Note>>,
    pub resolutions: delivery::Reads<String, Option<Resolution>>,
}
pub struct Item {
    pub path: String,
    pub note: Option<Note>,
    pub problem: Option<String>,
    pub resolved: bool,
}
pub fn source_problem<T>(source: &Source<T>, at: u64, interval: u64) -> Option<String> {
    delivery::problem(source, at, interval).map(|error| {
        format!(
            "{error} · last success {}",
            source.last_success.map_or("never".into(), |last| format!(
                "{}s ago",
                at.saturating_sub(last)
            ))
        )
    })
}
impl Snapshot {
    pub fn enabled(&self) -> bool {
        self.config.value.is_some() || self.config.error.is_some()
    }
    pub fn accept(&mut self, update: Update, at: u64) {
        self.config.accept(update.config, at);
        if self.config.error.is_none() && self.config.value.is_none() {
            self.notes.clear();
            self.resolutions.clear();
            return;
        }
        for (path, value) in update.notes {
            self.notes.entry(path).or_default().accept(value, at);
        }
        for (path, value) in update.resolutions {
            self.resolutions.entry(path).or_default().accept(value, at);
        }
        if self.config.error.is_none() {
            if let Some(config) = &self.config.value {
                self.notes.retain(|path, _| {
                    config.notes.contains(path) || config.next_stage.as_ref() == Some(path)
                });
                let linked: BTreeSet<_> = self
                    .notes
                    .values()
                    .filter_map(|source| source.value.as_ref())
                    .filter_map(|note| note.meta.resolution.as_ref())
                    .collect();
                self.resolutions.retain(|path, _| linked.contains(path));
            }
        }
    }
    pub fn item(&self, path: &str, at: u64, interval: u64) -> Item {
        let source = self.notes.get(path);
        let note = source.and_then(|s| s.value.clone());
        let mut problem = source_problem(&self.config, at, interval).or_else(|| {
            source.map_or(Some("Note not read".into()), |s| {
                source_problem(s, at, interval)
            })
        });
        let mut resolved = false;
        if let Some(note) = &note {
            if let Some(path) = &note.meta.resolution {
                let proof = self.resolutions.get(path);
                let proof_problem = proof.map_or(Some("Resolution not read".into()), |s| {
                    source_problem(s, at, interval)
                });
                if let Some(detail) = proof_problem {
                    problem.get_or_insert_with(|| format!("{path}: {detail}"));
                }
                if let Some(proof) = proof.and_then(|s| s.value.as_ref()) {
                    if proof.decision != note.path
                        || !proof.revision.eq_ignore_ascii_case(&note.revision)
                    {
                        problem.get_or_insert(
                            "Resolution belongs to a different note or an earlier revision".into(),
                        );
                    } else if problem.is_none() {
                        resolved = true;
                    }
                }
            }
        }
        Item {
            path: path.into(),
            note,
            problem,
            resolved,
        }
    }
    pub fn active(&self, at: u64, interval: u64) -> Vec<Item> {
        self.config.value.as_ref().map_or(Vec::new(), |config| {
            config
                .notes
                .iter()
                .map(|path| self.item(path, at, interval))
                .filter(|item| !item.resolved)
                .collect()
        })
    }
    pub fn next(&self, at: u64, interval: u64) -> Option<Item> {
        self.config
            .value
            .as_ref()?
            .next_stage
            .as_ref()
            .map(|path| self.item(path, at, interval))
    }
    /// The current note and delivery snapshot must agree on the exact scenario
    /// record. Neither an old acceptance nor another OS grants Try availability.
    pub fn available(item: &Item, row: &delivery::Row) -> bool {
        item.problem.is_none()
            && item.note.as_ref().is_some_and(|note| {
                note.meta.kind == Kind::Try
                    && row.ready
                    && row.evidence.as_ref().is_some_and(|e| {
                        note.meta.platform == Some(row.track.0)
                            && note.meta.channel == Some(row.track.1)
                            && note.meta.issue == Some(e.issue)
                            && e.note == note.path
                            && note.evidence.contains(e)
                    })
            })
    }
}
fn text(area: &Path, path: &str, total: &mut usize) -> Result<String> {
    ensure!(*total < MAX_TOTAL, "Project-note refresh exceeds 2 MiB");
    ensure!(
        relative_note(path) || path == "ops/board-context.toml",
        "Expected a project-relative Markdown note"
    );
    let root = area.canonicalize()?;
    let file = area.join(path).canonicalize()?;
    ensure!(
        file.starts_with(root),
        "Project context path escapes through a symlink"
    );
    let limit = MAX_NOTE.min(MAX_TOTAL - *total);
    let mut bytes = Vec::new();
    std::fs::File::open(file)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    *total = total.saturating_add(bytes.len());
    ensure!(
        bytes.len() <= limit,
        "Project note exceeds metadata read budget"
    );
    Ok(String::from_utf8(bytes)?)
}
fn frontmatter(source: &str) -> Result<(serde_yaml::Value, String)> {
    let source = source.replace("\r\n", "\n");
    let rest = source
        .strip_prefix("---\n")
        .context("Expected explicit YAML frontmatter")?;
    let (front, body) = rest
        .split_once("\n---\n")
        .or_else(|| rest.strip_suffix("\n---").map(|front| (front, "")))
        .context("Missing frontmatter closing delimiter")?;
    Ok((serde_yaml::from_str(front)?, body.into()))
}
pub fn parse_note(path: &str, source: &str) -> Result<Note> {
    let (front, body) = frontmatter(source)?;
    let meta: Meta = serde_yaml::from_value(
        front
            .get("board_context")
            .cloned()
            .context("Missing board_context metadata")?,
    )?;
    ensure!(
        !meta.title.trim().is_empty()
            && !meta.reason.trim().is_empty()
            && !meta.consequence.trim().is_empty(),
        "Context needs title, reason and consequence"
    );
    ensure!(
        meta.issue.is_none_or(|issue| issue > 0),
        "Issue reference must be positive"
    );
    ensure!(
        meta.resolution
            .as_ref()
            .is_none_or(|path| relative_note(path)),
        "Resolution must be a project-relative Markdown note"
    );
    ensure!(
        meta.resolution.is_none()
            || matches!(meta.kind, Kind::Attention | Kind::Decision | Kind::Blocker),
        "Only attention/decision/blocker notes have resolution evidence"
    );
    ensure!(
        meta.kind != Kind::Try
            || (meta.issue.is_some() && meta.platform.is_some() && meta.channel.is_some()),
        "Try requires explicit issue, platform and channel"
    );
    let fields: BTreeMap<&str, serde_json::Value> = [
        ("kind", serde_json::json!(meta.kind.key())),
        ("title", serde_json::json!(meta.title)),
        ("reason", serde_json::json!(meta.reason)),
        ("consequence", serde_json::json!(meta.consequence)),
        ("issue", serde_json::json!(meta.issue)),
        (
            "platform",
            serde_json::json!(meta.platform.map(|p| p.label())),
        ),
        (
            "channel",
            serde_json::json!(meta.channel.map(|c| c.label())),
        ),
        ("body", serde_json::json!(body)),
    ]
    .into_iter()
    .collect();
    let revision = format!("{:x}", Sha256::digest(serde_json::to_vec(&fields)?));
    let evidence = if meta.kind == Kind::Try && front.get("delivery").is_some() {
        delivery::parse_evidence(source, path)?
    } else {
        Vec::new()
    };
    Ok(Note {
        path: path.into(),
        meta,
        revision,
        evidence,
    })
}
fn resolution(source: &str) -> Result<Resolution> {
    let (front, _) = frontmatter(source)?;
    let value: Resolution = serde_yaml::from_value(
        front
            .get("board_resolution")
            .cloned()
            .context("Missing board_resolution metadata")?,
    )?;
    ensure!(
        relative_note(&value.decision)
            && value.revision.len() == 64
            && value.revision.bytes().all(|b| b.is_ascii_hexdigit()),
        "Resolution needs exact decision note and SHA256 revision"
    );
    ensure!(
        !value.by.trim().is_empty()
            && !value.at.trim().is_empty()
            && !value.outcome.trim().is_empty(),
        "Resolution needs author, time and outcome"
    );
    Ok(value)
}
fn config(area: &Path, total: &mut usize) -> Result<Option<Config>> {
    match std::fs::symlink_metadata(area.join("ops/board-context.toml")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        result => {
            result?;
        }
    }
    let config: Config = toml::from_str(&text(area, "ops/board-context.toml", total)?)?;
    ensure!(
        config.version == 1 && config.notes.len() <= 64,
        "Unsupported context version or too many notes"
    );
    let mut seen = BTreeSet::new();
    for path in config.notes.iter().chain(config.next_stage.iter()) {
        ensure!(
            relative_note(path) && seen.insert(path),
            "Context note paths must be unique project-relative Markdown"
        );
    }
    Ok(Some(config))
}
pub fn refresh(area: &Path) -> Update {
    let mut total = 0;
    let config = config(area, &mut total).map_err(|e| format!("ops/board-context.toml: {e:#}"));
    let mut update = Update {
        config,
        notes: Vec::new(),
        resolutions: Vec::new(),
    };
    let Some(config) = update
        .config
        .as_ref()
        .ok()
        .and_then(|value| value.as_ref())
        .cloned()
    else {
        return update;
    };
    let mut resolved = BTreeSet::new();
    for path in config.notes.iter().chain(config.next_stage.iter()) {
        let result = text(area, path, &mut total)
            .and_then(|text| parse_note(path, &text))
            .and_then(|note| {
                ensure!(
                    (config.next_stage.as_ref() == Some(path))
                        == (note.meta.kind == Kind::NextStage),
                    "Next-stage notes belong only in the explicit next_stage slot"
                );
                Ok(note)
            });
        if let Ok(note) = &result {
            if let Some(path) = &note.meta.resolution {
                resolved.insert(path.clone());
            }
        }
        update.notes.push((
            path.clone(),
            result.map(Some).map_err(|e| format!("{path}: {e:#}")),
        ));
    }
    for path in resolved {
        let result = text(area, &path, &mut total).and_then(|text| resolution(&text));
        update.resolutions.push((
            path.clone(),
            result.map(Some).map_err(|e| format!("{path}: {e:#}")),
        ));
    }
    update
}

#[cfg(test)]
mod tests;
