//! Rebuildable human-attention projection. Call on source changes, never per frame.
use super::{context, delivery, Binding, Issue};
use anyhow::{ensure, Result};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Author {
    pub id: Option<u64>,
    #[serde(default)]
    pub login: String,
}
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Report {
    pub created_at: Option<String>,
    pub user: Option<Author>,
    pub comments: Option<u64>,
    /// Set only after the existing Forgejo reader checked every comment page.
    #[serde(skip)]
    pub filing_bot_only: bool,
}
pub fn timestamp(value: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .and_then(|date| u64::try_from(date.timestamp()).ok())
}
pub fn recent_report(issue: &Issue, at: u64) -> bool {
    issue.state == "open"
        && (issue.has_label("from-tester") || issue.has_label("from-oleg"))
        && issue
            .report
            .created_at
            .as_deref()
            .and_then(timestamp)
            .and_then(|created| at.checked_sub(created))
            .is_some_and(|age| age < 24 * 3600)
}
pub fn new_report(issue: &Issue, at: u64) -> bool {
    recent_report(issue, at) && (issue.report.comments == Some(0) || issue.report.filing_bot_only)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub owner: String,
    pub title: String,
    pub modified_at: Option<u64>,
}
/// Only top-level owner bullets in the exact Open section are questions.
/// A malformed source is a visible failure, never a successful empty list.
pub fn parse_questions(text: &str, modified_at: Option<u64>) -> Result<Vec<Question>> {
    let mut open = false;
    let mut found = false;
    let mut fence = None;
    let mut questions = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_end();
        let leading = trimmed.trim_start();
        if leading.starts_with("```") || leading.starts_with("~~~") {
            let marker = &leading[..3];
            if fence == Some(marker) {
                fence = None;
            } else if fence.is_none() {
                fence = Some(marker);
            }
            continue;
        }
        if fence.is_some() {
            continue;
        }
        if trimmed == "## Open" {
            ensure!(!found, "Duplicate Open section");
            found = true;
            open = true;
            continue;
        }
        if trimmed.starts_with("# ") || trimmed.starts_with("## ") {
            open = false;
        }
        if !open {
            continue;
        }
        let Some(bullet) = trimmed.strip_prefix("- ") else {
            continue;
        };
        let Some((owner, rest)) = bullet.strip_prefix('[').and_then(|s| s.split_once("] ")) else {
            anyhow::bail!("Open question needs [owner] and question text");
        };
        let title = rest
            .split_once(" — ")
            .map_or(rest, |(question, _)| question)
            .trim();
        ensure!(
            !owner.trim().is_empty() && !title.is_empty(),
            "Empty question owner or text"
        );
        questions.push(Question {
            owner: owner.trim().into(),
            title: title.into(),
            modified_at,
        });
    }
    ensure!(found, "Missing ## Open section");
    Ok(questions)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Decision,
    Try,
    New,
}
impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Decision => "Решить",
            Self::Try => "Попробуй",
            Self::New => "Новое",
        }
    }
    pub fn action(self) -> &'static str {
        match self {
            Self::Decision => "Ответить",
            Self::Try => "Открыть",
            Self::New => "Разобрать",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub kind: Kind,
    pub title: String,
    pub source: String,
    pub target: String,
    pub video: Option<String>,
    pub at: Option<u64>,
}
#[derive(Default)]
pub struct Projection {
    pub items: Vec<Item>,
    pub problem: Option<String>,
}
impl Projection {
    pub fn visible(&self) -> bool {
        !self.items.is_empty()
    }
}
pub fn project(
    context: &context::Snapshot,
    delivery: &delivery::Snapshot,
    binding: &Binding,
    issues: &BTreeMap<u64, Issue>,
    at: u64,
    issue_at: u64,
) -> Projection {
    let mut projection = Projection::default();
    let mut problems = Vec::new();
    if context.enabled() {
        if let Some(error) = context::source_problem(&context.config, at, binding.refresh_seconds) {
            problems.push(error);
        }
        if let Some(path) = context
            .config
            .value
            .as_ref()
            .and_then(|c| c.questions_note.as_ref())
        {
            if let Some(source) = context.questions.get(path) {
                if let Some(error) = context::source_problem(source, at, binding.refresh_seconds) {
                    problems.push(format!("{path}: {error}"));
                }
                projection
                    .items
                    .extend(source.value.iter().map(|question| Item {
                        kind: Kind::Decision,
                        title: question.title.clone(),
                        source: question.owner.clone(),
                        target: path.clone(),
                        video: None,
                        at: question.modified_at,
                    }));
            } else {
                problems.push(format!("{path}: questions not read"));
            }
        }
        for item in context.active(at, binding.refresh_seconds) {
            if let Some(error) = &item.problem {
                problems.push(error.clone());
            }
            let Some(note) = &item.note else {
                continue;
            };
            let kind = match note.meta.kind {
                context::Kind::Decision | context::Kind::Attention | context::Kind::Blocker => {
                    Kind::Decision
                }
                context::Kind::Try => Kind::Try,
                context::Kind::NextStage => continue,
            };
            let mut source = note.path.clone();
            let mut time = note.modified_at;
            if kind == Kind::Try {
                let row = binding
                    .issues
                    .iter()
                    .find(|s| Some(s.number) == note.meta.issue)
                    .and_then(|scope| {
                        Some(delivery.rows(scope, note.meta.platform?, at, binding.refresh_seconds))
                    })
                    .and_then(|rows| {
                        rows.into_iter()
                            .find(|row| Some(row.track.1) == note.meta.channel)
                    });
                if let Some(row) = row.filter(|row| context::Snapshot::available(&item, row)) {
                    if let Some(build) = row.build {
                        source = format!(
                            "{} {} {}",
                            build.platform.label(),
                            build.channel.label(),
                            build.build
                        );
                        time = build.published_at.as_deref().and_then(timestamp).or(time);
                    }
                } else {
                    source = "Try unavailable · build/scenario evidence unconfirmed".into();
                }
            }
            projection.items.push(Item {
                kind,
                title: note.meta.title.clone(),
                source,
                target: note.path.clone(),
                video: note.meta.video.clone(),
                at: time,
            });
        }
    }
    let mut reports: Vec<_> = issues
        .values()
        .filter(|issue| new_report(issue, issue_at))
        .collect();
    reports.sort_by_key(|issue| {
        (
            std::cmp::Reverse(issue.report.created_at.as_deref().and_then(timestamp)),
            issue.number,
        )
    });
    projection
        .items
        .extend(reports.into_iter().map(|issue| Item {
            kind: Kind::New,
            title: format!("#{} {}", issue.number, issue.title),
            source: {
                let origin = if issue.has_label("from-tester") {
                    "tester"
                } else {
                    "Oleg"
                };
                issue
                    .report
                    .user
                    .as_ref()
                    .filter(|user| !user.login.is_empty())
                    .map_or_else(
                        || origin.into(),
                        |user| format!("{origin} · {}", user.login),
                    )
            },
            target: binding.issue_url(issue.number),
            video: None,
            at: issue.report.created_at.as_deref().and_then(timestamp),
        }));
    projection.problem = (!problems.is_empty()).then(|| problems.join(" · "));
    projection
}

#[cfg(test)]
mod tests;
