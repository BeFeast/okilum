//! Unified Now feed (#3296).
//!
//! The vault generator owns urgency. This module only reads `@views/out/now.json`,
//! checks the path contract, and folds live manager questions on top.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde::Deserialize;

pub const SNAPSHOT_PATH: &str = "@views/out/now.json";
pub const MAX_BYTES: usize = 2 * 1024 * 1024;

const GROUPS: [&str; 5] = ["urgent", "answer", "hands", "available", "waiting"];
const GROUP_TITLES: [&str; 5] = [
    "Urgent",
    "Reply",
    "Do manually",
    "Available",
    "Waiting on others",
];
const SUBGROUPS: [(&str, &str); 4] = [
    ("intent", "Intentions"),
    ("soon", "Due soon"),
    ("review", "Ready to review"),
    ("profile", "Profile checks"),
];

#[derive(Clone, Debug, PartialEq)]
pub enum LoadOutcome {
    Absent,
    Ready(NowFeed),
    Invalid(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct NowFeed {
    pub version: u32,
    pub generated_at: String,
    pub groups: Vec<FeedGroup>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct FeedGroup {
    pub id: String,
    pub items: Vec<FeedItem>,
    #[serde(default)]
    pub hidden_count: usize,
    #[serde(default)]
    pub hidden_items: Vec<FeedItem>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct FeedItem {
    pub id: String,
    pub group: String,
    #[serde(default)]
    pub subgroup: Option<String>,
    pub kind: String,
    pub title: String,
    pub area: String,
    #[serde(default)]
    pub project: Option<String>,
    pub source_path: String,
    #[serde(default)]
    pub due: Option<String>,
    #[serde(default)]
    pub next_check: Option<String>,
    pub overdue: bool,
    #[serde(default)]
    pub waiting_on: Option<String>,
    #[serde(default)]
    pub decision_options: Vec<String>,
    #[serde(default, deserialize_with = "answer_target")]
    pub answer_target: Option<AnswerTarget>,
    #[serde(default)]
    pub note_meta: BTreeMap<String, serde_json::Value>,
    /// `"answer"`, `"act"`, or absent. Presentation does not re-bucket on it.
    #[serde(default)]
    pub response: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnswerTarget {
    Case {
        path: String,
    },
    T3Manager {
        project: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveQuestion {
    pub id: String,
    pub project: Option<String>,
    pub title: String,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveProblem {
    pub source: String,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoadedSnapshot {
    pub outcome: LoadOutcome,
    pub areas: BTreeMap<String, String>,
    pub stamp: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct T3Banner {
    pub summary: String,
    pub tooltip: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresentedFeed {
    pub banner: Option<T3Banner>,
    pub groups: Vec<PresentedGroup>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresentedGroup {
    pub id: &'static str,
    pub title: &'static str,
    pub count: usize,
    pub sections: Vec<PresentedSection>,
    pub hidden: Vec<PresentedItem>,
    pub hidden_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresentedSection {
    pub title: Option<&'static str>,
    pub rows: Vec<PresentedRow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresentedRow {
    Item(PresentedItem),
    Fold {
        project: String,
        count: usize,
        items: Vec<PresentedItem>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresentedItem {
    pub id: String,
    pub title: String,
    pub secondary: String,
    pub tooltip: String,
    pub open: FeedOpen,
    pub response: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedOpen {
    Note(String),
    Live(String),
}

fn answer_target<'de, D>(deserializer: D) -> Result<Option<AnswerTarget>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    Ok(Some(
        serde_json::from_value(value).unwrap_or(AnswerTarget::Other),
    ))
}

impl NowFeed {
    pub fn load(vault_root: &Path) -> LoadOutcome {
        match read_snapshot(vault_root) {
            Ok(outcome) => outcome,
            Err(message) => LoadOutcome::Invalid(message),
        }
    }

    pub fn group(&self, id: &str) -> Option<&FeedGroup> {
        self.groups.iter().find(|group| group.id == id)
    }
}

pub fn load_presentation(vault_root: &Path) -> LoadedSnapshot {
    let outcome = NowFeed::load(vault_root);
    let areas = match crate::vault_descriptor::LayoutState::load(vault_root) {
        crate::vault_descriptor::LayoutState::Managed(layout) => layout
            .domains
            .iter()
            .map(|domain| (domain.id.clone(), domain.name.clone()))
            .collect(),
        _ => BTreeMap::new(),
    };
    let stamp = match &outcome {
        LoadOutcome::Ready(feed) => snapshot_clock(&feed.generated_at),
        _ => String::new(),
    };
    LoadedSnapshot {
        outcome,
        areas,
        stamp,
    }
}

pub fn snapshot_clock(generated_at: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(generated_at)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| "—".into())
}

/// A coalesced watch batch that can change the feed file.
pub fn snapshot_changed(changes: &crate::watch::Changes) -> bool {
    if changes.rescan {
        return true;
    }
    let parent = |path: &str| path == "@views" || path == "@views/out" || path == SNAPSHOT_PATH;
    changes.directories.iter().any(|path| parent(path))
        || changes
            .changed
            .iter()
            .chain(changes.removed.iter())
            .any(|path| path == SNAPSHOT_PATH)
}

pub fn area_label(area: &str, names: &BTreeMap<String, String>) -> String {
    if let Some(name) = names.get(area) {
        return name.clone();
    }
    let words: Vec<String> = area
        .split(['-', '_', '/'])
        .filter(|part| !part.is_empty())
        .map(capitalize)
        .collect();
    if words.is_empty() {
        area.to_owned()
    } else {
        words.join(" ")
    }
}

pub fn build_feed(
    feed: &NowFeed,
    questions: &[LiveQuestion],
    problems: &[LiveProblem],
    areas: &BTreeMap<String, String>,
) -> PresentedFeed {
    let banner = t3_banner(problems);
    let groups = GROUPS
        .iter()
        .enumerate()
        .map(|(index, id)| present_group(feed, id, GROUP_TITLES[index], questions, areas))
        .collect();
    PresentedFeed { banner, groups }
}

fn t3_banner(problems: &[LiveProblem]) -> Option<T3Banner> {
    if problems.is_empty() {
        return None;
    }
    Some(T3Banner {
        summary: format!("T3 unavailable ({} sources)", problems.len()),
        tooltip: problems
            .iter()
            .map(|problem| format!("{}: {}", problem.source, problem.detail))
            .collect::<Vec<_>>()
            .join("\n"),
    })
}

fn present_group(
    feed: &NowFeed,
    id: &str,
    title: &'static str,
    questions: &[LiveQuestion],
    areas: &BTreeMap<String, String>,
) -> PresentedGroup {
    let source = feed.group(id);
    let items = source.map(|group| group.items.as_slice()).unwrap_or(&[]);
    let (sections, count, hidden, hidden_count) = if id == "answer" {
        let rows = fold_answers(items, questions, areas);
        let count = items.len() + questions.len();
        (
            vec![PresentedSection { title: None, rows }],
            count,
            Vec::new(),
            0,
        )
    } else if id == "available" {
        let sections = available_sections(items, areas);
        (sections, items.len(), Vec::new(), 0)
    } else if id == "waiting" {
        let hidden_count = source.map(|group| group.hidden_count).unwrap_or(0);
        let hidden = source
            .map(|group| {
                group
                    .hidden_items
                    .iter()
                    .map(|item| present_item(item, areas))
                    .collect()
            })
            .unwrap_or_default();
        let rows = items
            .iter()
            .map(|item| PresentedRow::Item(present_item(item, areas)))
            .collect();
        (
            vec![PresentedSection { title: None, rows }],
            items.len() + hidden_count,
            hidden,
            hidden_count,
        )
    } else {
        let rows = items
            .iter()
            .map(|item| PresentedRow::Item(present_item(item, areas)))
            .collect();
        (
            vec![PresentedSection { title: None, rows }],
            items.len(),
            Vec::new(),
            0,
        )
    };
    PresentedGroup {
        id: match id {
            "urgent" => "urgent",
            "answer" => "answer",
            "hands" => "hands",
            "available" => "available",
            _ => "waiting",
        },
        title,
        count,
        sections,
        hidden,
        hidden_count,
    }
}

fn available_sections(
    items: &[FeedItem],
    areas: &BTreeMap<String, String>,
) -> Vec<PresentedSection> {
    let mut sections = Vec::new();
    let loose: Vec<PresentedRow> = items
        .iter()
        .filter(|item| item.subgroup.is_none())
        .map(|item| PresentedRow::Item(present_item(item, areas)))
        .collect();
    if !loose.is_empty() {
        sections.push(PresentedSection {
            title: None,
            rows: loose,
        });
    }
    for (id, title) in SUBGROUPS {
        let rows: Vec<PresentedRow> = items
            .iter()
            .filter(|item| item.subgroup.as_deref() == Some(id))
            .map(|item| PresentedRow::Item(present_item(item, areas)))
            .collect();
        if !rows.is_empty() {
            sections.push(PresentedSection {
                title: Some(title),
                rows,
            });
        }
    }
    let mut seen = BTreeSet::new();
    for item in items {
        let Some(name) = item.subgroup.as_deref() else {
            continue;
        };
        if SUBGROUPS.iter().any(|(id, _)| *id == name) || !seen.insert(name.to_owned()) {
            continue;
        }
        let rows = items
            .iter()
            .filter(|item| item.subgroup.as_deref() == Some(name))
            .map(|item| PresentedRow::Item(present_item(item, areas)))
            .collect();
        sections.push(PresentedSection { title: None, rows });
    }
    sections
}

enum Piece {
    Vault(usize),
    Live(usize),
}

fn fold_answers(
    items: &[FeedItem],
    questions: &[LiveQuestion],
    areas: &BTreeMap<String, String>,
) -> Vec<PresentedRow> {
    let mut pieces = Vec::new();
    for index in 0..items.len() {
        pieces.push(Piece::Vault(index));
    }
    for index in 0..questions.len() {
        pieces.push(Piece::Live(index));
    }
    let project_of = |piece: &Piece| -> Option<&str> {
        match piece {
            Piece::Vault(index) => items[*index].project.as_deref(),
            Piece::Live(index) => questions[*index].project.as_deref(),
        }
    };
    let render = |piece: &Piece| -> PresentedItem {
        match piece {
            Piece::Vault(index) => present_item(&items[*index], areas),
            Piece::Live(index) => present_live(&questions[*index]),
        }
    };
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for piece in &pieces {
        if let Some(project) = project_of(piece) {
            *counts.entry(project).or_default() += 1;
        }
    }
    let mut emitted = BTreeSet::new();
    let mut rows = Vec::new();
    for piece in &pieces {
        let Some(project) = project_of(piece) else {
            rows.push(PresentedRow::Item(render(piece)));
            continue;
        };
        if counts[project] < 2 {
            rows.push(PresentedRow::Item(render(piece)));
            continue;
        }
        if !emitted.insert(project.to_owned()) {
            continue;
        }
        let folded = pieces
            .iter()
            .filter(|other| project_of(other) == Some(project))
            .map(|other| render(other))
            .collect::<Vec<_>>();
        rows.push(PresentedRow::Fold {
            project: project.to_owned(),
            count: folded.len(),
            items: folded,
        });
    }
    rows
}

fn present_live(question: &LiveQuestion) -> PresentedItem {
    PresentedItem {
        id: question.id.clone(),
        title: question.title.clone(),
        secondary: question.detail.clone(),
        tooltip: match &question.project {
            Some(project) => format!("{}\n{project}", question.title),
            None => question.title.clone(),
        },
        open: FeedOpen::Live(question.id.clone()),
        response: None,
    }
}

fn present_item(item: &FeedItem, areas: &BTreeMap<String, String>) -> PresentedItem {
    let mut secondary = Vec::new();
    if !item.area.is_empty() {
        secondary.push(area_label(&item.area, areas));
    }
    if let Some(project) = &item.project {
        if !project.is_empty() {
            secondary.push(project.clone());
        }
    }
    if item.overdue {
        secondary.push("overdue".into());
    }
    if let Some(due) = &item.due {
        secondary.push(format!("due {due}"));
    }
    if let Some(waiting) = &item.waiting_on {
        secondary.push(format!("waiting on {waiting}"));
    }
    let mut tooltip = vec![
        item.title.clone(),
        item.source_path.clone(),
        item.kind.clone(),
    ];
    if let Some(response) = &item.response {
        tooltip.push(format!("response: {response}"));
    }
    PresentedItem {
        id: item.id.clone(),
        title: item.title.clone(),
        secondary: secondary.join(" · "),
        tooltip: tooltip.join("\n"),
        open: FeedOpen::Note(item.source_path.clone()),
        response: item.response.clone(),
    }
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn read_snapshot(vault_root: &Path) -> Result<LoadOutcome, String> {
    let views = vault_root.join("@views");
    let out = views.join("out");
    let file = out.join("now.json");
    if !exists_dir(&views, "@views")? || !exists_dir(&out, "@views/out")? {
        return Ok(LoadOutcome::Absent);
    }
    let meta = match std::fs::symlink_metadata(&file) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err("symlink: @views/out/now.json".into());
        }
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LoadOutcome::Absent);
        }
        Err(error) => return Err(one_line(&error)),
    };
    if !meta.is_file() {
        return Err("now.json is not a file".into());
    }
    if meta.len() > MAX_BYTES as u64 {
        return Err("now.json exceeds 2 MiB".into());
    }
    let mut bytes = Vec::new();
    File::open(&file)
        .and_then(|handle| handle.take((MAX_BYTES as u64) + 1).read_to_end(&mut bytes))
        .map_err(|error| one_line(&error))?;
    if bytes.len() > MAX_BYTES {
        return Err("now.json exceeds 2 MiB".into());
    }
    parse_snapshot(&bytes)
}

fn exists_dir(path: &Path, label: &str) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(format!("symlink: {label}")),
        Ok(meta) if meta.is_dir() => Ok(true),
        Ok(_) => Err(format!("{label} is not a directory")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(one_line(&error)),
    }
}

fn parse_snapshot(bytes: &[u8]) -> Result<LoadOutcome, String> {
    let raw: RawFeed = serde_json::from_slice(bytes).map_err(|error| one_line(&error))?;
    if raw.version != 1 {
        return Err(format!("unsupported feed version {}", raw.version));
    }
    if chrono::DateTime::parse_from_rfc3339(&raw.generated_at).is_err() {
        return Err("generated_at is not RFC 3339".into());
    }
    if raw.groups.len() > GROUPS.len() {
        return Err("too many feed groups".into());
    }
    let mut seen = BTreeSet::new();
    let mut groups = Vec::new();
    for group in raw.groups {
        if !GROUPS.contains(&group.id.as_str()) {
            return Err(format!("unknown feed group {}", group.id));
        }
        if !seen.insert(group.id.clone()) {
            return Err(format!("duplicate feed group {}", group.id));
        }
        if !group.hidden_items.is_empty() && group.hidden_items.len() != group.hidden_count {
            return Err(format!(
                "{} hidden_items length {} does not match hidden_count {}",
                group.id,
                group.hidden_items.len(),
                group.hidden_count
            ));
        }
        check_items(&group.id, &group.items)?;
        check_items(&group.id, &group.hidden_items)?;
        groups.push(group);
    }
    Ok(LoadOutcome::Ready(NowFeed {
        version: raw.version,
        generated_at: raw.generated_at,
        groups,
    }))
}

fn check_items(group_id: &str, items: &[FeedItem]) -> Result<(), String> {
    for item in items {
        if item.group != group_id {
            return Err(format!(
                "item {} group {} is outside {group_id}",
                item.id, item.group
            ));
        }
        if !crate::view::snapshot::valid_path(&item.source_path) {
            return Err(format!("invalid source_path {}", item.source_path));
        }
        if !valid_response(item.response.as_deref()) {
            return Err(format!("invalid response on {}", item.id));
        }
        if item
            .subgroup
            .as_deref()
            .is_some_and(|value| value.is_empty() || value.chars().any(|ch| ch.is_ascii_control()))
        {
            return Err(format!("invalid subgroup on {}", item.id));
        }
    }
    Ok(())
}

fn valid_response(value: Option<&str>) -> bool {
    matches!(value, None | Some("answer") | Some("act"))
}

fn one_line(error: &dyn std::fmt::Display) -> String {
    error
        .to_string()
        .replace(['\n', '\r'], " ")
        .chars()
        .take(200)
        .collect()
}

#[derive(Deserialize)]
struct RawFeed {
    version: u32,
    generated_at: String,
    groups: Vec<FeedGroup>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(groups: Vec<serde_json::Value>) -> String {
        serde_json::json!({
            "version": 1,
            "generated_at": "2026-11-27T18:05:00Z",
            "groups": groups,
        })
        .to_string()
    }

    fn ready(json: &str) -> NowFeed {
        match parse_snapshot(json.as_bytes()).unwrap() {
            LoadOutcome::Ready(feed) => feed,
            other => panic!("expected ready, got {other:?}"),
        }
    }

    #[test]
    fn parses_v1_groups_targets_meta_and_response() {
        let json = document(vec![
            serde_json::json!({
                "id": "urgent",
                "items": [{
                    "id": "u-act",
                    "group": "urgent",
                    "kind": "task",
                    "title": "Ship patch",
                    "area": "lab",
                    "project": "alpha",
                    "source_path": "notes/u-act.md",
                    "due": null,
                    "next_check": null,
                    "overdue": true,
                    "waiting_on": null,
                    "decision_options": ["ship"],
                    "answer_target": {"kind": "case", "path": "notes/u-act.md"},
                    "note_meta": {"rank": 1},
                    "response": "act"
                }]
            }),
            serde_json::json!({
                "id": "answer",
                "items": [{
                    "id": "a1",
                    "group": "answer",
                    "kind": "question",
                    "title": "Reply draft",
                    "area": "lab",
                    "project": null,
                    "source_path": "notes/a1.md",
                    "due": null,
                    "next_check": null,
                    "overdue": false,
                    "waiting_on": null,
                    "answer_target": {"kind": "t3_manager", "project": "alpha"},
                    "response": "answer"
                }]
            }),
            serde_json::json!({
                "id": "hands",
                "items": [{
                    "id": "m1",
                    "group": "hands",
                    "kind": "task",
                    "title": "Rotate key",
                    "area": "lab",
                    "project": null,
                    "source_path": "notes/m1.md",
                    "overdue": false
                }]
            }),
            serde_json::json!({
                "id": "available",
                "items": [
                    {"id":"i1","group":"available","subgroup":"intent","kind":"note","title":"Draft note","area":"ops-west","project":null,"source_path":"notes/i1.md","due":null,"next_check":"2026-12-01","overdue":false,"waiting_on":null},
                    {"id":"s1","group":"available","subgroup":"soon","kind":"note","title":"Due soon","area":"lab","project":null,"source_path":"notes/s1.md","due":"2026-11-30","next_check":null,"overdue":false,"waiting_on":null},
                    {"id":"r1","group":"available","subgroup":"review","kind":"note","title":"Ready review","area":"lab","project":null,"source_path":"notes/r1.md","due":null,"next_check":null,"overdue":false,"waiting_on":null},
                    {"id":"p1","group":"available","subgroup":"profile","kind":"note","title":"Profile check","area":"lab","project":null,"source_path":"notes/p1.md","due":null,"next_check":null,"overdue":false,"waiting_on":null}
                ]
            }),
            serde_json::json!({
                "id": "waiting",
                "hidden_count": 1,
                "hidden_items": [{
                    "id": "h1",
                    "group": "waiting",
                    "kind": "note",
                    "title": "Later",
                    "area": "lab",
                    "project": null,
                    "source_path": "notes/h1.md",
                    "due": null,
                    "next_check": null,
                    "overdue": false,
                    "waiting_on": "vendor"
                }],
                "items": [{
                    "id": "w1",
                    "group": "waiting",
                    "kind": "note",
                    "title": "Vendor reply",
                    "area": "lab",
                    "project": null,
                    "source_path": "notes/w1.md",
                    "due": null,
                    "next_check": null,
                    "overdue": true,
                    "waiting_on": "vendor"
                }]
            }),
        ]);
        let feed = ready(&json);
        assert_eq!(feed.groups.len(), 5);
        let urgent = feed.group("urgent").unwrap();
        assert_eq!(urgent.items[0].response.as_deref(), Some("act"));
        assert_eq!(
            urgent.items[0].answer_target,
            Some(AnswerTarget::Case {
                path: "notes/u-act.md".into()
            })
        );
        assert_eq!(
            urgent.items[0].note_meta.get("rank"),
            Some(&serde_json::json!(1))
        );
        assert!(matches!(
            feed.group("answer").unwrap().items[0].answer_target,
            Some(AnswerTarget::T3Manager { .. })
        ));
        let waiting = feed.group("waiting").unwrap();
        assert_eq!(waiting.hidden_count, 1);
        assert_eq!(waiting.hidden_items.len(), waiting.hidden_count);
        assert_eq!(waiting.hidden_items[0].id, "h1");
        let names = BTreeMap::from([("lab".into(), "Laboratory".into())]);
        let presented = build_feed(&feed, &[], &[], &names);
        let urgent_rows = &presented.groups[0];
        assert_eq!(urgent_rows.title, "Urgent");
        let PresentedRow::Item(row) = &urgent_rows.sections[0].rows[0] else {
            panic!("urgent row");
        };
        assert_eq!(row.response.as_deref(), Some("act"));
        assert_eq!(row.title, "Ship patch");
        assert!(row.secondary.contains("Laboratory"));
        let available = presented
            .groups
            .iter()
            .find(|group| group.id == "available")
            .unwrap();
        let labels: Vec<_> = available
            .sections
            .iter()
            .filter_map(|section| section.title)
            .collect();
        assert_eq!(
            labels,
            [
                "Intentions",
                "Due soon",
                "Ready to review",
                "Profile checks"
            ]
        );
    }

    #[test]
    fn rejects_version_date_symlink_size_and_traversal() {
        assert!(matches!(
            parse_snapshot(br#"{"version":2,"generated_at":"2026-11-27T18:05:00Z","groups":[]}"#),
            Err(message) if message.contains("version")
        ));
        assert!(matches!(
            parse_snapshot(br#"{"version":1,"generated_at":"yesterday","groups":[]}"#),
            Err(message) if message.contains("RFC 3339")
        ));
        assert!(matches!(
            parse_snapshot(
                br#"{"version":1,"generated_at":"2026-11-27T18:05:00Z","groups":[{"id":"urgent","items":[{"id":"x","group":"urgent","kind":"note","title":"Bad","area":"lab","source_path":"../secret.md","overdue":false}]}]}"#
            ),
            Err(message) if message.contains("source_path")
        ));
        assert!(matches!(
            parse_snapshot(
                br#"{"version":1,"generated_at":"2026-11-27T18:05:00Z","groups":[{"id":"urgent","items":[{"id":"x","group":"urgent","kind":"note","title":"Bad","area":"lab","source_path":"notes/x.md","overdue":false,"response":"person"}]}]}"#
            ),
            Err(message) if message.contains("response")
        ));
        let root = tempfile::tempdir().unwrap();
        assert!(matches!(NowFeed::load(root.path()), LoadOutcome::Absent));
        std::fs::create_dir_all(root.path().join("@views/out")).unwrap();
        let payload = root.path().join("payload.json");
        std::fs::write(&payload, b"{}").unwrap();
        std::os::unix::fs::symlink(&payload, root.path().join("@views/out/now.json")).unwrap();
        assert!(
            matches!(NowFeed::load(root.path()), LoadOutcome::Invalid(message) if message.contains("symlink"))
        );
        std::fs::remove_file(root.path().join("@views/out/now.json")).unwrap();
        std::fs::write(
            root.path().join("@views/out/now.json"),
            vec![b'x'; MAX_BYTES + 1],
        )
        .unwrap();
        assert!(matches!(
            NowFeed::load(root.path()),
            LoadOutcome::Invalid(message) if message.contains("2 MiB")
        ));
    }

    #[test]
    fn folds_project_questions_and_keeps_singletons() {
        let feed = ready(&document(vec![serde_json::json!({
            "id": "answer",
            "items": [
                {
                    "id": "a1",
                    "group": "answer",
                    "kind": "note",
                    "title": "First",
                    "area": "lab",
                    "project": "alpha",
                    "source_path": "notes/a1.md",
                    "overdue": false,
                    "response": "answer"
                },
                {
                    "id": "b1",
                    "group": "answer",
                    "kind": "note",
                    "title": "Only beta",
                    "area": "lab",
                    "project": "beta",
                    "source_path": "notes/b1.md",
                    "overdue": false,
                    "response": "answer"
                },
                {
                    "id": "n1",
                    "group": "answer",
                    "kind": "note",
                    "title": "Loose",
                    "area": "lab",
                    "project": null,
                    "source_path": "notes/n1.md",
                    "overdue": false,
                    "response": "answer"
                }
            ]
        })]));
        let questions = vec![
            LiveQuestion {
                id: "t3-alpha".into(),
                project: Some("alpha".into()),
                title: "Second alpha".into(),
                detail: "open the manager thread".into(),
            },
            LiveQuestion {
                id: "t3-gamma-1".into(),
                project: Some("gamma".into()),
                title: "Gamma one".into(),
                detail: "open the manager thread".into(),
            },
            LiveQuestion {
                id: "t3-gamma-2".into(),
                project: Some("gamma".into()),
                title: "Gamma two".into(),
                detail: "open the manager thread".into(),
            },
            LiveQuestion {
                id: "t3-delta".into(),
                project: Some("delta".into()),
                title: "Delta only".into(),
                detail: "open the manager thread".into(),
            },
        ];
        let presented = build_feed(&feed, &questions, &[], &BTreeMap::new());
        let rows = &presented
            .groups
            .iter()
            .find(|group| group.id == "answer")
            .unwrap()
            .sections[0]
            .rows;
        assert!(
            matches!(&rows[0], PresentedRow::Fold { project, count, .. } if project == "alpha" && *count == 2)
        );
        assert!(matches!(&rows[1], PresentedRow::Item(item) if item.title == "Only beta"));
        assert!(matches!(&rows[2], PresentedRow::Item(item) if item.title == "Loose"));
        assert!(
            matches!(&rows[3], PresentedRow::Fold { project, count, .. } if project == "gamma" && *count == 2)
        );
        assert!(matches!(&rows[4], PresentedRow::Item(item) if item.title == "Delta only"));
    }

    #[test]
    fn one_t3_banner_keeps_vault_groups() {
        let feed = ready(&document(vec![
            serde_json::json!({
                "id": "urgent",
                "items": [{
                    "id": "u1",
                    "group": "urgent",
                    "kind": "note",
                    "title": "Ship patch",
                    "area": "lab",
                    "project": null,
                    "source_path": "notes/u1.md",
                    "overdue": false,
                    "response": "act"
                }]
            }),
            serde_json::json!({"id": "waiting", "items": [], "hidden_count": 0}),
        ]));
        let problems = vec![
            LiveProblem {
                source: "alpha".into(),
                detail: "unreachable".into(),
            },
            LiveProblem {
                source: "beta".into(),
                detail: "auth".into(),
            },
        ];
        let presented = build_feed(&feed, &[], &problems, &BTreeMap::new());
        let banner = presented.banner.unwrap();
        assert_eq!(banner.summary, "T3 unavailable (2 sources)");
        assert!(banner.tooltip.contains("alpha: unreachable"));
        assert!(banner.tooltip.contains("beta: auth"));
        assert_eq!(presented.groups.len(), 5);
        let urgent = &presented.groups[0];
        assert_eq!(urgent.count, 1);
        let PresentedRow::Item(row) = &urgent.sections[0].rows[0] else {
            panic!("row");
        };
        assert_eq!(row.response.as_deref(), Some("act"));
    }
}
