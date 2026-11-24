use super::{Finding, Group, Row, Section, Status, TreeNode, ViewIdentity, ViewResult};
use chrono::{DateTime, FixedOffset};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

pub(super) const MAX_BYTES: usize = 512 * 1024;

pub(super) fn path(view_path: &str) -> Option<String> {
    let stem = view_path.strip_prefix("@views/")?.strip_suffix(".md")?;
    let mut bytes = stem.bytes();
    let alphanumeric = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    if stem.len() > 64
        || !bytes.next().is_some_and(alphanumeric)
        || !bytes.all(|b| alphanumeric(b) || matches!(b, b'-' | b'_'))
    {
        return None;
    }
    Some(format!("@views/out/{stem}.json"))
}

/// A sealed C2 snapshot (snapshot_version 1, view_api 0). Deserialize it from
/// the published JSON. The gateway authorizes the snapshot and its targets.
#[derive(Debug, Deserialize)]
#[serde(try_from = "Envelope")]
pub struct SnapshotJson {
    pub(crate) generated_at: Timestamp,
    view: ViewIdentity,
    params: BTreeMap<String, Value>,
    as_of: Timestamp,
    pub(crate) findings: Vec<Finding>,
    pub(crate) sections: Vec<SnapshotSection>,
}

#[derive(Deserialize)]
struct Envelope {
    snapshot_version: u64,
    view_api: u64,
    generated_at: Timestamp,
    view: ViewIdentity,
    params: BTreeMap<String, Value>,
    as_of: Timestamp,
    #[serde(rename = "index")]
    _index: Index,
    findings: Vec<Finding>,
    sections: Vec<SnapshotSection>,
}

#[derive(Deserialize)]
struct Index {
    #[serde(rename = "by")]
    _by: String,
    #[serde(rename = "notes")]
    _notes: usize,
    #[serde(rename = "generation")]
    _generation: u64,
}

impl TryFrom<Envelope> for SnapshotJson {
    type Error = String;

    fn try_from(wire: Envelope) -> Result<Self, Self::Error> {
        if (wire.snapshot_version, wire.view_api) != (1, 0) {
            return Err(format!(
                "Unsupported view snapshot version pair: snapshot_version {}, view_api {}",
                wire.snapshot_version, wire.view_api
            ));
        }
        if !valid_path(&wire.view.path)
            || !wire.view.path.starts_with("@views/")
            || wire.view.path.starts_with("@views/out/")
            || !wire.view.path.ends_with(".md")
            || !valid_revision(&wire.view.rev)
        {
            return Err("Invalid view snapshot identity".into());
        }
        for section in &wire.sections {
            if (section.source == Source::Snapshot)
                != (section.status == Status::Ok && section.snapshot_at.is_some())
                || (section.source == Source::Live && section.snapshot_at.is_some())
            {
                return Err(format!(
                    "Invalid provenance in view section '{}'",
                    section.id
                ));
            }
            if section.rows.iter().any(|row| {
                !valid_path(&row.path) || !valid_revision(&row.rev) || row.line == Some(0)
            }) {
                return Err(format!(
                    "Invalid row source in view section '{}'",
                    section.id
                ));
            }
        }
        Ok(Self {
            generated_at: wire.generated_at,
            view: wire.view,
            params: wire.params,
            as_of: wire.as_of,
            findings: wire.findings,
            sections: wire.sections,
        })
    }
}

impl SnapshotJson {
    pub(super) fn reuse(&mut self, view: &ViewResult, section: &mut Section) {
        if self.view.path != view.view.path
            || self.view.rev != view.view.rev
            || self.view.extends != view.view.extends
            || self.params != view.params
        {
            return;
        }
        let show = serde_json::to_value(section.show).expect("serializable display");
        let Some(prior) = self.sections.iter_mut().find(|prior| {
            prior.id == section.id
                && prior.line == section.line
                && prior.title == section.title
                && show.as_str() == Some(&prior.show)
                && prior.status == Status::Ok
                && !prior.diagnostics.iter().any(Finding::is_error)
                && prior
                    .groups
                    .iter()
                    .try_fold(0_usize, |total, group| total.checked_add(group.count))
                    .is_some()
        }) else {
            return;
        };
        section.status = Status::Ok;
        section.source = "snapshot";
        section.snapshot_at = Some(
            prior
                .snapshot_at
                .as_ref()
                .unwrap_or(&self.as_of)
                .text
                .clone(),
        );
        section.cols = std::mem::take(&mut prior.cols);
        section.diagnostics = std::mem::take(&mut prior.diagnostics);
        section.total = prior.total;
        section.truncated = prior.truncated;
        section.groups = std::mem::take(&mut prior.groups);
        section.rows = std::mem::take(&mut prior.rows);
        section.nodes = prior.nodes.take();
    }
}

impl Section {
    /// Display the original evaluation time in the vault views' calendar zone.
    pub fn snapshot_clock(&self) -> Option<String> {
        let stamp = DateTime::parse_from_rfc3339(self.snapshot_at.as_deref()?).ok()?;
        Some(
            stamp
                .with_timezone(&chrono_tz::Asia::Jerusalem)
                .format("%H:%M")
                .to_string(),
        )
    }
}

#[derive(Debug, Deserialize)]
#[serde(try_from = "String")]
pub(crate) struct Timestamp {
    pub(crate) text: String,
    pub(crate) value: DateTime<FixedOffset>,
}

impl TryFrom<String> for Timestamp {
    type Error = chrono::ParseError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let value = DateTime::parse_from_rfc3339(&text)?;
        Ok(Self { text, value })
    }
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Source {
    Live,
    Snapshot,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SnapshotSection {
    id: String,
    pub(crate) title: String,
    line: usize,
    pub(crate) show: String,
    pub(crate) cols: Vec<String>,
    pub(crate) status: Status,
    source: Source,
    pub(crate) snapshot_at: Option<Timestamp>,
    pub(crate) diagnostics: Vec<Finding>,
    pub(crate) total: usize,
    pub(crate) truncated: bool,
    pub(crate) groups: Vec<Group>,
    pub(crate) rows: Vec<Row>,
    nodes: Option<Vec<TreeNode>>,
}

fn valid_revision(revision: &str) -> bool {
    revision.strip_prefix("sha256:").is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

pub(crate) fn valid_path(path: &str) -> bool {
    !path.contains('\\')
        && !path.chars().any(|ch| ch.is_ascii_control())
        && !matches!(path.as_bytes(), [a, b':', ..] if a.is_ascii_alphabetic())
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}
