//! Read-only, rebuildable Now sidebar. All inputs are accepted snapshots;
//! neither indexing nor rendering this projection reads or writes the filesystem.
use crate::{
    records::{FieldSchema, NoteRecord, Projection, Value},
    vault::warm::Snapshot,
    vault_conventions,
    vault_descriptor::{ContextLayer, Layout},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub const DAY: u64 = 86_400;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unit {
    pub path: String,
    pub title: String,
    pub terminal: bool,
    pub status: String,
    pub manager: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub path: String,
    pub group: Group,
    unit: Option<String>,
    pub mtime: u64,
    pub dated_name: Option<String>,
    pub noise: bool,
    pub title: String,
    terminal: bool,
    declared_unit: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Index {
    notes: BTreeMap<String, Row>,
    discovered: Vec<Unit>,
    linked_context: bool,
    managed: bool,
    layout: Option<Layout>,
    layout_unavailable: bool,
    schema: Option<Arc<FieldSchema>>,
    pub view: View,
    /// Time of the last accepted source publication, independent of window aging.
    pub observed_at: u64,
}

#[derive(Clone, Debug, Default)]
pub struct View {
    pub managed: bool,
    pub layout_unavailable: bool,
    pub suggest_now: bool,
    pub active: Vec<Activity>,
    pub older: usize,
    pub days: Vec<Period>,
    pub built_at: u64,
    /// All declared contexts, including those without recent edits.
    pub project_count: usize,
    pub older_active: Vec<Activity>,
}

#[derive(Clone, Debug)]
pub struct Activity {
    pub unit: Unit,
    pub group: Group,
    pub open: String,
    pub modified: u64,
    pub count: usize,
    pub today: usize,
}

/// Prefer a short project identity over an index note's document heading.
pub fn short_project_name(path: &str, suggested: &str) -> String {
    let folder = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".md");
    let dated = folder.as_bytes().get(4) == Some(&b'-')
        && folder
            .get(..4)
            .is_some_and(|s| s.bytes().all(|b| b.is_ascii_digit()))
        && folder
            .get(5..7)
            .is_some_and(|s| s.bytes().all(|b| b.is_ascii_digit()))
        && folder.as_bytes().get(7) == Some(&b'-');
    let short = if dated {
        let start = if folder
            .get(8..10)
            .is_some_and(|s| s.bytes().all(|b| b.is_ascii_digit()))
            && folder.as_bytes().get(10) == Some(&b'-')
        {
            11
        } else {
            8
        };
        &folder[start..]
    } else {
        folder
    };
    let short = if short.is_empty() { folder } else { short };
    if dated
        || suggested.trim().is_empty()
        || suggested.to_ascii_lowercase().ends_with("project manager")
        || suggested.eq_ignore_ascii_case("@index")
    {
        short.to_owned()
    } else {
        suggested.trim().to_owned()
    }
}

impl Activity {
    pub fn legacy_domain(&self) -> &str {
        let domain = self.unit.path.split('/').next().unwrap_or("Vault");
        if matches!(domain, "AI" | "HomeLab") {
            "AI · HomeLab"
        } else if vault_conventions::legacy()
            .domains
            .iter()
            .any(|name| name == domain)
        {
            domain
        } else {
            "Vault"
        }
    }

    pub fn activity_label(&self, now: u64) -> String {
        if self.today > 0 {
            format!("{} today", self.today)
        } else {
            let age = now.saturating_sub(self.modified);
            if age < DAY {
                format!("{}h ago", (age / 3600).max(1))
            } else {
                format!("{}d ago", age / DAY)
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Group {
    pub area: Option<String>,
    pub kind: Option<String>,
    pub status: Option<String>,
}

impl Group {
    pub fn label(&self) -> String {
        [&self.area, &self.kind, &self.status]
            .map(|value| value.as_deref().unwrap_or("—"))
            .join(" · ")
    }

    fn from_record(record: Option<&NoteRecord>) -> Self {
        let text = |name| record_text(record, name).map(str::to_owned);
        Self {
            area: text("area"),
            kind: text("kind"),
            status: text("status"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Period {
    pub label: &'static str,
    pub count: usize,
    pub groups: Vec<ChangeGroup>,
}

#[derive(Clone, Debug)]
pub struct ChangeGroup {
    pub group: Group,
    pub legacy_path: Option<String>,
    pub modified: u64,
    pub rows: Vec<Change>,
    pub count: usize,
}

#[derive(Clone, Debug)]
pub struct Change {
    pub path: String,
    pub title: String,
    pub modified: u64,
    /// A dense import is represented once, not as fifty individual edits.
    pub count: usize,
}

pub fn terminal(value: &str) -> bool {
    matches!(
        value.trim().to_lowercase().as_str(),
        "closed"
            | "resolved"
            | "done"
            | "completed"
            | "complete"
            | "superseded"
            | "archived"
            | "merged"
            | "shipped"
            | "promoted"
            | "retired"
            | "cancelled"
            | "canceled"
            | "abandoned"
    )
}

pub fn noise(path: &str) -> bool {
    is_noise(path, false)
}

fn is_noise(path: &str, managed: bool) -> bool {
    let file = path.rsplit('/').next().unwrap_or_default();
    (!managed
        && (vault_conventions::defaults().archived(path)
            || vault_conventions::defaults().asset_content(path)
            || path.split('/').any(|part| {
                part.eq_ignore_ascii_case("pokutan") || part.eq_ignore_ascii_case("transcripts")
            })))
        || path
            .split('/')
            .any(|part| part.starts_with('.') || part.eq_ignore_ascii_case("@schema"))
        || crate::quick_open::is_agent_file(path)
        || file.starts_with("._")
        || path.ends_with("ops/thread-ids.md")
        || path.ends_with("ops/board.md")
        || path.ends_with("ops/manager-state.md")
}

fn legacy_unit(path: &str) -> Option<String> {
    let parts: Vec<_> = path.split('/').collect();
    let index = parts
        .iter()
        .position(|part| *part == vault_conventions::legacy().projects)?;
    (parts.len() > index + 1).then(|| {
        parts[..index + 2]
            .join("/")
            .trim_end_matches(".md")
            .to_owned()
    })
}

fn legacy_place(path: &str) -> String {
    let parts: Vec<_> = path.split('/').collect();
    let depth = if parts
        .get(1)
        .is_some_and(|part| vault_conventions::legacy().bucket_exact(part))
    {
        3
    } else {
        2
    };
    parts[..parts.len().min(depth)]
        .join("/")
        .trim_end_matches(".md")
        .to_owned()
}

fn scalar<'a>(map: &'a serde_yaml::Value, key: &str) -> &'a str {
    map.get(key)
        .and_then(serde_yaml::Value::as_str)
        .unwrap_or_default()
}

fn legacy_metadata(
    path: &str,
    unit: Option<&str>,
    managed: bool,
    record: Option<&NoteRecord>,
) -> bool {
    (crate::vault_conventions::index_name(path)
        || unit.is_some_and(|unit| path.strip_suffix(".md") == Some(unit)))
        && (!managed || record_text(record, "layer").is_none())
}

fn record_text<'a>(record: Option<&'a NoteRecord>, key: &str) -> Option<&'a str> {
    match record?.fields.get(key)? {
        Value::String(text) => Some(text),
        _ => None,
    }
}

fn under(path: &str, unit: &str) -> bool {
    path.strip_prefix(unit)
        .is_some_and(|rest| rest.starts_with('/') || rest == ".md")
}

impl Index {
    pub fn from_snapshot(
        snapshot: &Snapshot,
        units: Vec<Unit>,
        projection: Option<&Projection>,
        now: u64,
        midnight: u64,
        yesterday: u64,
    ) -> Self {
        let mut index = Self {
            discovered: units,
            managed: !snapshot.folders().is_legacy(),
            layout: snapshot.folders().layout().cloned(),
            layout_unavailable: snapshot.folders().layout_unavailable(),
            linked_context: snapshot.has_note_links(100),
            observed_at: now,
            schema: projection.map(Projection::schema_arc),
            ..Self::default()
        };
        for path in snapshot.source_paths() {
            index.replace_snapshot(snapshot, path, projection);
        }
        index.refresh(now, midnight, yesterday);
        index
    }

    fn replace_snapshot(
        &mut self,
        snapshot: &Snapshot,
        path: &str,
        projection: Option<&Projection>,
    ) {
        if !snapshot.is_note_source(path) {
            self.remove(path);
            return;
        }
        let record = projection.and_then(|projection| projection.note(path));
        let legacy = (!self.managed).then(|| legacy_unit(path)).flatten();
        let source = legacy_metadata(path, legacy.as_deref(), self.managed, record)
            .then(|| snapshot.source(path))
            .flatten();
        let mtime = snapshot.source_revision(path).map_or(0, |stamp| {
            (stamp.modified_nanoseconds() / 1_000_000_000) as u64
        });
        self.replace_record(path, source.as_deref(), mtime, record);
    }

    pub fn apply_snapshot(
        &mut self,
        snapshot: &Snapshot,
        projection: Option<&Projection>,
        removed: &BTreeSet<String>,
        changed: &BTreeSet<String>,
    ) {
        let schema = projection.map(Projection::schema_arc);
        let managed = !snapshot.folders().is_legacy();
        self.layout_unavailable = snapshot.folders().layout_unavailable();
        self.layout = snapshot.folders().layout().cloned();
        let same_schema = match (&self.schema, &schema) {
            (Some(previous), Some(next)) => Arc::ptr_eq(previous, next),
            (None, None) => true,
            _ => false,
        };
        if same_schema && self.managed == managed {
            for path in removed {
                self.remove(path);
            }
            for path in changed {
                self.replace_snapshot(snapshot, path, projection);
            }
        } else {
            self.notes.clear();
            self.schema = schema;
            self.managed = managed;
            for path in snapshot.source_paths() {
                self.replace_snapshot(snapshot, path, projection);
            }
        }
        self.update_context(snapshot);
    }

    pub fn set_units(&mut self, units: Vec<Unit>) {
        self.discovered = units;
    }
    pub fn update_context(&mut self, snapshot: &Snapshot) {
        self.linked_context = snapshot.has_note_links(100);
    }
    pub fn remove(&mut self, path: &str) {
        self.notes.remove(path);
    }
    pub fn rows(&self) -> impl Iterator<Item = &Row> {
        self.notes.values()
    }
    pub fn unit_for(&self, path: &str) -> Option<&str> {
        self.notes.get(path)?.unit.as_deref()
    }

    pub fn replace(&mut self, path: &str, source: &str, mtime: u64) {
        self.replace_record(path, Some(source), mtime, None);
    }

    fn replace_record(
        &mut self,
        path: &str,
        source: Option<&str>,
        mtime: u64,
        record: Option<&NoteRecord>,
    ) {
        if !self.managed && vault_conventions::defaults().asset_content(path) {
            self.remove(path);
            return;
        }
        let is_index = crate::vault_conventions::index_name(path);
        let legacy = (!self.managed).then(|| legacy_unit(path)).flatten();
        let metadata = if legacy_metadata(path, legacy.as_deref(), self.managed, record) {
            source
                .and_then(crate::properties::frontmatter_block)
                .and_then(|yaml| serde_yaml::from_str::<serde_yaml::Value>(yaml).ok())
                .unwrap_or_default()
        } else {
            serde_yaml::Value::Null
        };
        let group = Group::from_record(record.filter(|_| self.managed));
        let typed_context = matches!(group.kind.as_deref(), Some("project" | "area"));
        let project_index = record_text(record, "type")
            .unwrap_or_else(|| scalar(&metadata, "type"))
            .eq_ignore_ascii_case("project");
        let declared_unit = if !self.managed {
            legacy
        } else if is_index && (typed_context || project_index) {
            path.rsplit_once('/').map(|(folder, _)| folder.to_owned())
        } else if typed_context {
            path.strip_suffix(".md").map(str::to_owned)
        } else {
            None
        };
        let terminal = group.status.as_deref().is_some_and(terminal)
            || terminal(scalar(&metadata, "status"))
            || terminal(scalar(&metadata, "lifecycle"));
        let name = path
            .rsplit('/')
            .next()
            .unwrap_or(path)
            .trim_end_matches(".md");
        let dated_name = (name.len() >= 10
            && name.as_bytes().get(4) == Some(&b'-')
            && name.as_bytes().get(7) == Some(&b'-'))
        .then(|| name.get(..10).map(str::to_owned))
        .flatten();
        self.notes.insert(
            path.to_owned(),
            Row {
                path: path.into(),
                group,
                unit: None,
                mtime,
                dated_name,
                noise: is_noise(path, self.managed),
                title: record_text(record.filter(|_| self.managed), "title")
                    .unwrap_or_else(|| {
                        let title = scalar(&metadata, "title");
                        if title.is_empty() {
                            name
                        } else {
                            title
                        }
                    })
                    .to_owned(),
                terminal,
                declared_unit,
            },
        );
    }

    /// Called once per worker batch. The UI consumes this ready view without IO.
    pub fn refresh(&mut self, now: u64, midnight: u64, yesterday: u64) {
        let mut units: BTreeMap<String, Unit> = self
            .discovered
            .iter()
            .cloned()
            .map(|u| (u.path.clone(), u))
            .collect();
        for row in self.notes.values().filter(|row| !row.noise) {
            if let Some(path) = &row.declared_unit {
                units.entry(path.clone()).or_insert_with(|| Unit {
                    title: short_project_name(path, ""),
                    path: path.clone(),
                    ..Unit::default()
                });
            }
        }
        let mut declarations = BTreeMap::new();
        for unit in units.values_mut() {
            let prefix = format!("{}/", unit.path);
            let candidates = crate::vault_conventions::index_candidates(
                self.notes
                    .range(prefix.clone()..)
                    .take_while(|(path, _)| path.starts_with(&prefix))
                    .map(|(path, _)| path.as_str())
                    .filter(|path| !path[prefix.len()..].contains('/')),
            );
            let row = match candidates.as_slice() {
                [path] => self.notes.get(*path),
                [] => self.notes.get(&format!("{}.md", unit.path)),
                _ => None,
            };
            if let Some(row) = row {
                unit.terminal |= row.terminal;
                if let Some(status) = &row.group.status {
                    unit.status.clone_from(status);
                }
                declarations.insert(unit.path.clone(), row.group.clone());
            }
            unit.title = short_project_name(&unit.path, &unit.title);
        }
        let contexts: BTreeMap<_, _> = self
            .layout
            .as_ref()
            .into_iter()
            .flat_map(|layout| {
                units.values().filter_map(move |unit| {
                    let path = layout.context(&unit.path)?;
                    (path.layer == ContextLayer::State).then_some((path.id, unit))
                })
            })
            .collect();
        let mut activity: BTreeMap<&str, Activity> = BTreeMap::new();
        let mut groups: BTreeMap<(usize, &Group, &str), Vec<&Row>> = BTreeMap::new();
        let mut view = View {
            managed: self.managed,
            layout_unavailable: self.layout_unavailable,
            built_at: now,
            project_count: units.len(),
            suggest_now: self.linked_context
                || self
                    .notes
                    .values()
                    .filter(|row| {
                        !row.noise
                            && row.dated_name.is_some()
                            && row
                                .path
                                .rsplit('/')
                                .next()
                                .and_then(|name| name.strip_suffix(".md"))
                                .is_some_and(|name| {
                                    name.len() == 10
                                        && name.bytes().enumerate().all(|(position, byte)| {
                                            if position == 4 || position == 7 {
                                                byte == b'-'
                                            } else {
                                                byte.is_ascii_digit()
                                            }
                                        })
                                })
                    })
                    .take(3)
                    .count()
                    == 3,
            ..View::default()
        };
        let week = now.saturating_sub(7 * DAY);
        for row in self
            .notes
            .values_mut()
            .filter(|r| !r.noise && r.mtime <= now)
        {
            // Look up ancestors rather than comparing every note to every unit.
            let mut ancestor = row.path.trim_end_matches(".md");
            let physical_owner = loop {
                if let Some(unit) = units.get(ancestor) {
                    if under(&row.path, &unit.path) {
                        break Some(unit);
                    }
                }
                let Some((parent, _)) = ancestor.rsplit_once('/') else {
                    break None;
                };
                ancestor = parent;
            };
            let context = self
                .layout
                .as_ref()
                .and_then(|layout| layout.context(&row.path));
            let archived = context.is_some_and(|path| path.is_final());
            let owner = physical_owner.or_else(|| {
                let path = context?;
                (path.layer == ContextLayer::Log && !archived)
                    .then(|| contexts.get(&path.id).copied())
                    .flatten()
            });
            let owner_path = owner.map(|unit| unit.path.as_str());
            if row.unit.as_deref() != owner_path || !self.managed {
                row.unit = owner_path.map(str::to_owned).or_else(|| {
                    (!self.managed).then(|| {
                        row.declared_unit
                            .clone()
                            .unwrap_or_else(|| legacy_place(&row.path))
                    })
                });
            }
            if let Some(unit) = owner {
                if !archived && !unit.terminal && !is_noise(&unit.path, self.managed) {
                    let a = activity
                        .entry(unit.path.as_str())
                        .or_insert_with(|| Activity {
                            unit: unit.clone(),
                            group: declarations.get(&unit.path).cloned().unwrap_or_default(),
                            open: row.path.clone(),
                            modified: 0,
                            count: 0,
                            today: 0,
                        });
                    if row.mtime > a.modified {
                        a.modified = row.mtime;
                        a.open.clone_from(&row.path);
                    }
                    if row.mtime >= week {
                        a.count += 1;
                    }
                    if row.mtime >= midnight {
                        a.today += 1;
                    }
                }
            }
            if row.mtime < week {
                continue;
            }
            let period = if row.mtime >= midnight {
                0
            } else if row.mtime >= yesterday {
                1
            } else {
                2
            };
            let place = row
                .unit
                .as_deref()
                .or_else(|| row.path.rsplit_once('/').map(|(parent, _)| parent))
                .unwrap_or("");
            groups
                .entry((period, &row.group, place))
                .or_default()
                .push(row);
        }
        for a in activity.into_values() {
            if a.modified >= week {
                view.active.push(a);
            } else if a.modified >= now.saturating_sub(14 * DAY) {
                view.older += 1;
                view.older_active.push(a);
            }
        }
        view.active.sort_by(|a, b| {
            b.modified
                .cmp(&a.modified)
                .then(a.unit.path.cmp(&b.unit.path))
        });
        view.older_active.sort_by(|a, b| {
            b.modified
                .cmp(&a.modified)
                .then(a.unit.path.cmp(&b.unit.path))
        });
        for (period, label) in ["Today", "Yesterday", "Week"].into_iter().enumerate() {
            let mut by_group = BTreeMap::<(&Group, Option<&str>), ChangeGroup>::new();
            while groups
                .first_key_value()
                .is_some_and(|((p, _, _), _)| *p == period)
            {
                let ((_, group, place), mut rows) = groups.pop_first().expect("group exists");
                rows.sort_by(|a, b| b.mtime.cmp(&a.mtime).then(a.path.cmp(&b.path)));
                let count = rows.len();
                let modified = rows.first().map_or(0, |r| r.mtime);
                let mut changes = Vec::new();
                let (mut start, mut end) = (0, 0);
                while start < rows.len() {
                    while end < rows.len()
                        && rows[start].mtime.saturating_sub(rows[end].mtime) <= 600
                    {
                        end += 1;
                    }
                    if end - start >= 50 {
                        changes.push(Change {
                            path: rows[start].path.clone(),
                            title: "Batch changes".into(),
                            modified: rows[start].mtime,
                            count: end - start,
                        });
                        start = end;
                    } else {
                        let r = rows[start];
                        changes.push(Change {
                            path: r.path.clone(),
                            title: r.title.clone(),
                            modified: r.mtime,
                            count: 1,
                        });
                        start += 1;
                    }
                }
                let legacy_path = (!self.managed).then_some(place);
                let grouped = by_group
                    .entry((group, legacy_path))
                    .or_insert_with(|| ChangeGroup {
                        group: group.clone(),
                        legacy_path: legacy_path.map(str::to_owned),
                        modified: 0,
                        count: 0,
                        rows: Vec::new(),
                    });
                grouped.modified = grouped.modified.max(modified);
                grouped.count += count;
                grouped.rows.extend(changes);
            }
            let mut changes_by_group: Vec<_> = by_group.into_values().collect();
            for group in &mut changes_by_group {
                group
                    .rows
                    .sort_by(|a, b| b.modified.cmp(&a.modified).then(a.path.cmp(&b.path)));
            }
            changes_by_group.sort_by(|a, b| {
                b.modified
                    .cmp(&a.modified)
                    .then(a.group.cmp(&b.group))
                    .then(a.legacy_path.cmp(&b.legacy_path))
            });
            view.days.push(Period {
                label,
                count: changes_by_group.iter().map(|group| group.count).sum(),
                groups: changes_by_group,
            });
        }
        self.view = view;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn context_offer_needs_links_or_daily_notes_not_project_discovery() {
        let mut index = super::Index::default();
        index.set_units(vec![super::Unit {
            path: "Dev/Projects/App".into(),
            ..Default::default()
        }]);
        index.replace("Dev/Projects/App/note.md", "Plain project", 100);
        index.refresh(100, 0, 0);
        assert_eq!(index.view.project_count, 1);
        assert!(!index.view.suggest_now);
        for day in 1..=2 {
            index.replace(&format!("Daily/2026-10-{day:02}.md"), "Daily", 100);
        }
        index.refresh(100, 0, 0);
        assert!(!index.view.suggest_now);
        index.replace("Daily/2026-10-03.md", "Daily", 100);
        index.refresh(100, 0, 0);
        assert!(index.view.suggest_now);
        index.remove("Daily/2026-10-03.md");
        index.refresh(100, 0, 0);
        assert!(!index.view.suggest_now);
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Target.md"), "Target").unwrap();
        for note in 0..100 {
            std::fs::write(root.path().join(format!("Note{note}.md")), "[[Target]]").unwrap();
        }
        let (_, snapshot, _) =
            crate::vault::warm::reconcile(root.path(), None, false, &mut |_, _| Ok(())).unwrap();
        let linked = super::Index::from_snapshot(&snapshot, vec![], None, 100, 0, 0);
        assert!(
            linked.view.suggest_now,
            "prepared note links provide a positive control"
        );
        std::fs::remove_file(root.path().join("Note0.md")).unwrap();
        let (_, snapshot, _) =
            crate::vault::warm::reconcile(root.path(), Some(&snapshot), false, &mut |_, _| Ok(()))
                .unwrap();
        index.update_context(&snapshot);
        index.refresh(100, 0, 0);
        assert!(
            !index.view.suggest_now,
            "falling below the link threshold clears relevance"
        );
    }

    use super::*;
    #[test]
    fn declared_legacy_units_keep_project_identity_and_recency() {
        for (path, name, expected) in [
            ("Dev/Areas/okilum", "Okilum", "Okilum"),
            ("Dev/Areas/karaoke", "Karaoke Project Manager", "karaoke"),
            (
                "Music/Projects/2026-10-axe-fx-linux-editor",
                "axe3",
                "axe-fx-linux-editor",
            ),
            (
                "AI/Projects/2026-10-second-brain-v2",
                "2026-10-second-brain-v2",
                "second-brain-v2",
            ),
            ("Dev/Projects/2026-10-29-demo", "", "demo"),
        ] {
            assert_eq!(short_project_name(path, name), expected);
        }
        let now = 30 * DAY;
        let mut index = Index::default();
        index.set_units(vec![
            Unit {
                path: "Dev/Areas/okilum".into(),
                title: "Okilum".into(),
                manager: true,
                ..Default::default()
            },
            Unit {
                path: "AI/Projects/2026-10-second-brain-v2".into(),
                ..Default::default()
            },
            Unit {
                path: "HomeLab/Projects/router".into(),
                ..Default::default()
            },
        ]);
        index.replace(
            "Dev/Areas/okilum/@index.md",
            "---\ntitle: Long Index Heading\n---\n",
            now,
        );
        index.replace("Dev/Areas/okilum/old.md", "", now - 2 * DAY);
        index.replace(
            "AI/Projects/2026-10-second-brain-v2/n.md",
            "",
            now - 4 * DAY,
        );
        index.replace("HomeLab/Projects/router/n.md", "", now - 8 * DAY);
        index.refresh(now, now - 100, now - DAY);
        let app = &index.view.active[0];
        assert_eq!(
            app.unit.title, "Okilum",
            "document headings cannot rename projects"
        );
        assert!(app.unit.manager);
        assert_eq!((app.count, app.today), (2, 1));
        assert_eq!(app.activity_label(now), "1 today");
        assert_eq!(app.group.label(), "— · — · —");
        assert_eq!(index.view.active[1].activity_label(now), "4d ago");
        assert_eq!(index.view.older_active.len(), 1);
        assert_eq!(
            index.unit_for("Dev/Areas/okilum/old.md"),
            Some("Dev/Areas/okilum")
        );
        assert_eq!(index.view.project_count, 3);
    }

    #[test]
    fn project_presence_uses_declarations_without_requiring_recent_activity() {
        let now = 30 * DAY;
        let mut index = Index {
            managed: true,
            ..Index::default()
        };
        index.replace("Notes/plain.md", "# Note", now);
        index.replace("Archive/Projects/Past/n.md", "", now);
        index.refresh(now, now - 100, now - DAY);
        assert_eq!(index.view.project_count, 0);
        assert_eq!(index.view.days[0].count, 2);
        index.set_units(vec![Unit {
            path: "Dev/Areas/quiet".into(),
            title: "Quiet".into(),
            ..Default::default()
        }]);
        index.refresh(now, now - 100, now - DAY);
        assert_eq!(index.view.project_count, 1);
        assert!(
            index.view.active.is_empty(),
            "quiet projects still enable the project sidebar"
        );
    }

    #[test]
    fn now_tracks_content_terminal_units_noise_and_incremental_removal() {
        let mut i = Index {
            managed: true,
            ..Index::default()
        };
        i.set_units(
            ["Dev/Areas/App", "Dev/Projects/Old", "Dev/Projects/Done"]
                .into_iter()
                .map(|path| Unit {
                    path: path.into(),
                    ..Unit::default()
                })
                .collect(),
        );
        let now = 20 * DAY;
        for (p, t) in [
            ("Dev/Areas/App/notes/a.md", now - 30),
            ("Dev/Areas/App/ops/thread-ids.md", now),
            ("Dev/Projects/Old/a.md", now - 8 * DAY),
        ] {
            i.replace(p, "", t);
        }
        i.replace(
            "Dev/Projects/Done/@index.md",
            "---\nstatus: shipped\n---\n",
            now,
        );
        i.replace("Dev/Projects/Done/body.md", "", now);
        for p in [
            "CLAUDE.md",
            "Dev/.private/a.md",
            "Dev/Pokutan/a.md",
            "Dev/transcripts/a.md",
            "Dev/Archive/a.md",
            "Dev/._a.md",
        ] {
            i.replace(p, "", now);
        }
        i.refresh(now, now - 100, now - DAY);
        assert_eq!(i.view.active.len(), 1);
        assert_eq!(i.view.active[0].modified, now - 30);
        assert_eq!(i.view.older, 1);
        assert_eq!(i.view.days[0].count, 6);
        assert_eq!(i.rows().filter(|r| r.noise).count(), 4);
        i.remove("Dev/Areas/App/notes/a.md");
        i.refresh(now, now - 100, now - DAY);
        assert!(i.view.active.is_empty());
    }

    #[test]
    fn batches_are_one_row_and_sibling_prefixes_do_not_leak() {
        let now = 20 * DAY;
        let mut i = Index {
            managed: true,
            ..Index::default()
        };
        i.set_units(
            ["Dev/Projects/App", "Dev/Projects/App2"]
                .into_iter()
                .map(|path| Unit {
                    path: path.into(),
                    ..Unit::default()
                })
                .collect(),
        );
        for n in 0..60 {
            i.replace(&format!("Dev/Projects/App/notes/{n}.md"), "", now - n);
        }
        i.replace("Dev/Projects/App2/n.md", "", now - 700);
        i.refresh(now, now - 1000, now - DAY);
        assert_eq!(i.view.active.len(), 2);
        let group = &i.view.days[0].groups[0];
        assert_eq!(group.count, 61);
        assert_eq!(group.rows.len(), 2);
        assert_eq!(group.rows[0].count, 60);
        assert_eq!(group.rows[1].path, "Dev/Projects/App2/n.md");
        i.replace(
            "Dev/Projects/App/@index.md",
            "---\nlifecycle: retired\n---\n",
            now,
        );
        i.refresh(now, now - 1000, now - DAY);
        assert_eq!(i.view.active.len(), 1);
        assert_eq!(i.view.active[0].unit.path, "Dev/Projects/App2");
    }

    #[test]
    fn corpus_scale_projection_reports_full_and_twenty_file_batch_cost() {
        let now = 20 * DAY;
        let start = std::time::Instant::now();
        let mut i = Index::default();
        i.set_units(
            (0..119)
                .map(|n| Unit {
                    path: format!("Dev/Projects/Unit{n}"),
                    ..Unit::default()
                })
                .collect(),
        );
        for n in 0..5400 {
            i.replace(
                &format!("Dev/Projects/Unit{}/notes/Note{n}.md", n % 119),
                "",
                now - n,
            );
        }
        i.refresh(now, now - 1000, now - DAY);
        let full = start.elapsed();
        assert_eq!(i.rows().count(), 5400);
        assert_eq!(i.view.active.len(), 119);
        let start = std::time::Instant::now();
        for n in 0..20 {
            i.replace(&format!("Dev/Projects/Unit{n}/notes/new.md"), "", now);
        }
        i.refresh(now, now - 1000, now - DAY);
        let batch = start.elapsed();
        assert_eq!(i.rows().count(), 5420);
        diagnostic_println!(
            "WORKBENCH_BENCH full_ms={:.3} batch20_ms={:.3}",
            full.as_secs_f64() * 1000.,
            batch.as_secs_f64() * 1000.
        );
    }

    #[test]
    fn lifecycle_groups_follow_the_projection_and_keep_missing_dimensions() {
        use crate::records::Source;

        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/lifecycle-vault");
        let schema = Arc::new(FieldSchema::load(&fixture).unwrap());
        let mut index = Index {
            managed: true,
            ..Index::default()
        };
        let now = 20 * DAY;
        let paths = [
            "state/health/sleep/@index.md",
            "log/health/sleep/decisions/2026-11-24-lights-out.md",
            "inbox/Capture.md",
            "sources/web/2026/11/24/Reference.md",
            "knowledge/evening-light.md",
        ];
        let bytes = paths.map(|path| std::fs::read(fixture.join(path)).unwrap());
        let projection = Projection::build(
            schema,
            paths.iter().zip(&bytes).map(|(path, bytes)| Source {
                path,
                bytes,
                mtime: None,
            }),
        );
        for path in paths {
            index.replace_record(path, None, now, projection.note(path));
        }
        index.refresh(now, now, now - DAY);
        assert_eq!(index.view.active.len(), 1);
        assert_eq!(
            index.view.active[0].group.label(),
            "Health · project · active"
        );
        assert_eq!(index.view.days[0].count, 5);
        let groups: BTreeMap<_, _> = index.view.days[0]
            .groups
            .iter()
            .map(|group| (group.group.label(), group.count))
            .collect();
        assert_eq!(groups["Health · decisions · —"], 1);
        assert_eq!(groups["Health · — · —"], 1);
        assert_eq!(groups["— · — · —"], 2);

        let updated = b"---\nkind: project\nstatus: parked\n---\n";
        let mut projection = projection;
        projection.apply(
            [Source {
                path: paths[0],
                bytes: updated,
                mtime: None,
            }],
            [],
        );
        index.replace_record(paths[0], None, now, projection.note(paths[0]));
        index.refresh(now, now, now - DAY);
        assert_eq!(
            index.view.active[0].group.label(),
            "Health · project · parked"
        );
    }

    #[test]
    fn active_log_and_state_share_a_context_but_final_leaves_activity() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/lifecycle-vault");
        let folders = vault_conventions::Folders::load(&root);
        let layout = folders.layout().cloned();
        let schema = Arc::new(FieldSchema::load(&root).unwrap());
        let now = 20 * DAY;
        let state = "state/dev/cedar/@index.md";
        let log = "log/dev/cedar/decisions/2026-11-24-use-cedar.md";
        let final_note = "log/dev/cedar/final/finished.md";
        let paths = [state, log, final_note, "state/dev/cedar/Archive/Plan.md"];
        let declaration = b"---\nkind: project\nstatus: active\n---\n";
        let projection = Projection::build(
            schema,
            paths.iter().map(|path| crate::records::Source {
                path,
                bytes: if *path == state {
                    declaration
                } else {
                    b"# Plan"
                },
                mtime: None,
            }),
        );
        let mut index = Index {
            managed: true,
            layout,
            ..Index::default()
        };
        for path in paths {
            index.replace_record(path, None, now, projection.note(path));
        }
        index.refresh(now, now, now.saturating_sub(DAY));
        assert_eq!(index.view.active.len(), 1);
        assert_eq!(index.view.active[0].today, 3);
        assert_eq!(index.unit_for(log), Some("state/dev/cedar"));
        assert_eq!(index.view.days[0].count, 4);
        assert!(folders.archived(final_note));
        assert!(!folders.archived("state/dev/cedar/Archive/Plan.md"));
    }

    #[test]
    fn vault_without_layout_keeps_path_groups_and_archive_and_asset_filters() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/legacy-workbench-vault");
        let (_, snapshot, _) =
            crate::vault::warm::reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
        let path = "Dev/Projects/Cedar/Plan.md";
        let now = (snapshot
            .source_revision(path)
            .unwrap()
            .modified_nanoseconds()
            / 1_000_000_000) as u64;
        let index = Index::from_snapshot(
            &snapshot,
            vec![],
            None,
            now.saturating_add(DAY),
            now.saturating_sub(DAY),
            0,
        );
        assert!(!index.view.managed);
        assert_eq!(index.view.active.len(), 1);
        let active = &index.view.active[0];
        assert_eq!(active.unit.title, "Cedar");
        assert_eq!(active.legacy_domain(), "Dev");
        assert_eq!(active.open, path);
        assert_eq!(index.view.days[0].count, 1);
        assert_eq!(
            index.view.days[0].groups[0].legacy_path.as_deref(),
            Some("Dev/Projects/Cedar")
        );
        assert!(snapshot.source("@Assets/Files/Reference.md").is_some());
        assert!(!snapshot.is_note_source("@Assets/Files/Reference.md"));
    }

    #[test]
    fn time_windows_age_without_a_new_file_or_a_new_snapshot_timestamp() {
        let now = 20 * DAY;
        let mut index = Index {
            observed_at: now,
            ..Index::default()
        };
        index.replace("Dev/Projects/App/Plan.md", "", now);
        index.refresh(now, now, now - DAY);
        assert_eq!(index.view.days[0].count, 1);
        index.refresh(now + DAY, now + DAY, now);
        assert_eq!(index.view.days[0].count, 0);
        assert_eq!(index.view.days[1].count, 1);
        assert_eq!(
            index.observed_at, now,
            "aging is not a new network snapshot"
        );
        index.refresh(now + 15 * DAY, now + 15 * DAY, now + 14 * DAY);
        assert!(index.view.active.is_empty());
        assert_eq!(index.view.older, 0);
        assert!(!is_noise("Dev/4 Archive/Old/plan.md", true));
    }
}

#[cfg(test)]
mod at_tests {
    use super::*;
    #[test]
    fn at_noise_metadata_and_ordinary_underscore_notes() {
        assert_eq!(
            short_project_name("Dev/Projects/Synthetic", "@INDEX"),
            "Synthetic"
        );
        assert!(noise("@Schema/note.md"));
        for path in [
            "@Assets/Clippings/clip.md",
            "@Assets/Daily Notes/day.md",
            "_Assets/Daily Notes/day.md",
            "@Private/ordinary.md",
        ] {
            assert!(!is_noise(path, true), "{path}");
        }
        let mut index = Index {
            managed: true,
            ..Index::default()
        };
        index.set_units(vec![Unit {
            path: "Dev/Projects/Synthetic".into(),
            ..Unit::default()
        }]);
        index.replace(
            "Dev/Projects/Synthetic/_index.md",
            "---\nstatus: active\n---\n",
            10,
        );
        index.replace(
            "Dev/Projects/Synthetic/@INDEX.md",
            "---\nstatus: done\n---\n",
            11,
        );
        index.replace("@Assets/Clippings/clip.md", "# Clipping", 100);
        assert!(index.rows().any(|row| row.path.starts_with("@Assets/")));
        index.refresh(100, 0, 0);
        assert!(
            index.view.active.is_empty(),
            "@ metadata must make this unit terminal"
        );
    }
}

#[cfg(test)]
mod underscore_tests {
    use super::*;

    #[test]
    fn issue2847_underscore_schema_assets_and_index_titles_are_ordinary() {
        for path in [
            "_schema/policy.md",
            "Notes/_Schema/plan.md",
            "_Assets/Templates/Note.md",
            "_Assets/Attachments/file.md",
            "_Assets/Daily Notes/day.md",
            "_Private/note.md",
            "Legacy/_index.md",
            "_AgentContract.md",
        ] {
            assert!(!noise(path), "ordinary workbench input: {path}");
        }
        assert!(noise("@Schema/policy.md"));
        assert!(noise("@Assets/Templates/Note.md"));
        assert_eq!(short_project_name("Work/Project", "_index"), "_index");
    }
}
