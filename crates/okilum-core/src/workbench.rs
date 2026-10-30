//! Read-only, rebuildable Now sidebar. All inputs are accepted snapshots;
//! neither indexing nor rendering this projection reads or writes the filesystem.
use crate::vault::warm::Snapshot;
use std::collections::BTreeMap;

pub const DAY: u64 = 86_400;
pub const DOMAINS: [&str; 9] = [
    "AI", "Business", "Dev", "Finance", "Health", "HomeLab", "Life", "Music", "Video",
];

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
    pub domain: String,
    pub unit: String,
    pub mtime: u64,
    pub dated_name: Option<String>,
    pub family: String,
    pub noise: bool,
    pub title: String,
    terminal: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Index {
    notes: BTreeMap<String, Row>,
    discovered: Vec<Unit>,
    pub view: View,
    /// Time of the last accepted source publication, independent of window aging.
    pub observed_at: u64,
}

#[derive(Clone, Debug, Default)]
pub struct View {
    pub active: Vec<Activity>,
    pub older: usize,
    pub days: Vec<Period>,
    pub domains: BTreeMap<String, usize>,
    pub built_at: u64,
    /// All discovered/structural units, including those without recent edits.
    pub project_count: usize,
    pub older_active: Vec<Activity>,
}

#[derive(Clone, Debug)]
pub struct Activity {
    pub unit: Unit,
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
        || suggested == "_index"
    {
        short.to_owned()
    } else {
        suggested.trim().to_owned()
    }
}

impl Activity {
    pub fn domain(&self) -> &str {
        let domain = self.unit.path.split('/').next().unwrap_or("Vault");
        if matches!(domain, "AI" | "HomeLab") {
            "AI · HomeLab"
        } else if DOMAINS.contains(&domain) {
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

#[derive(Clone, Debug)]
pub struct Period {
    pub label: &'static str,
    pub count: usize,
    pub places: Vec<Place>,
}

#[derive(Clone, Debug)]
pub struct Place {
    pub path: String,
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
    let parts: Vec<_> = path.split('/').collect();
    let file = parts.last().copied().unwrap_or_default();
    crate::projects::archived(path)
        || parts.iter().any(|p| {
            p.starts_with('.')
                || matches!(
                    p.to_lowercase().as_str(),
                    "pokutan" | "transcripts" | "archive" | "архив" | "_schema"
                )
        })
        || matches!(
            file,
            "CLAUDE.md" | "AGENTS.md" | "GEMINI.md" | "ANTIGRAVITY.md" | "CONTEXT.md"
        )
        || file.starts_with("._")
        || path.ends_with("ops/thread-ids.md")
        || path.ends_with("ops/board.md")
        || path.ends_with("ops/manager-state.md")
        || path.starts_with("_Assets/") && !path.starts_with("_Assets/Daily Notes/")
}

fn scalar(map: &serde_yaml::Value, key: &str) -> String {
    map.get(key)
        .and_then(serde_yaml::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn para_unit(path: &str) -> Option<String> {
    let parts: Vec<_> = path.split('/').collect();
    let projects = parts.iter().position(|part| *part == "Projects")?;
    (parts.len() > projects + 1).then(|| {
        parts[..projects + 2]
            .join("/")
            .trim_end_matches(".md")
            .to_owned()
    })
}

fn under(path: &str, unit: &str) -> bool {
    path.strip_prefix(unit)
        .is_some_and(|rest| rest.starts_with('/') || rest == ".md")
}

pub fn place(path: &str) -> String {
    let parts: Vec<_> = path.split('/').collect();
    let count = if parts
        .get(1)
        .is_some_and(|s| matches!(*s, "Projects" | "Areas" | "Resources"))
    {
        3
    } else {
        2
    };
    parts[..parts.len().min(count)]
        .join("/")
        .trim_end_matches(".md")
        .to_owned()
}

impl Index {
    pub fn from_snapshot(
        snapshot: &Snapshot,
        units: Vec<Unit>,
        now: u64,
        midnight: u64,
        yesterday: u64,
    ) -> Self {
        let mut index = Self {
            discovered: units,
            observed_at: now,
            ..Self::default()
        };
        for path in snapshot.source_paths() {
            index.replace_snapshot(snapshot, path);
        }
        index.refresh(now, midnight, yesterday);
        index
    }

    pub fn replace_snapshot(&mut self, snapshot: &Snapshot, path: &str) {
        if let Some(source) = snapshot.source(path) {
            let mtime = snapshot
                .source_revision(path)
                .map_or(0, |s| (s.modified_nanoseconds() / 1_000_000_000) as u64);
            self.replace(path, &source, mtime);
        } else {
            self.remove(path);
        }
    }

    pub fn set_units(&mut self, units: Vec<Unit>) {
        self.discovered = units;
    }
    pub fn remove(&mut self, path: &str) {
        self.notes.remove(path);
    }
    pub fn rows(&self) -> impl Iterator<Item = &Row> {
        self.notes.values()
    }
    pub fn unit_for(&self, path: &str) -> Option<&str> {
        self.notes.get(path).map(|row| row.unit.as_str())
    }

    pub fn replace(&mut self, path: &str, source: &str, mtime: u64) {
        // Only unit metadata needs YAML parsing, not every body on a watcher batch.
        let metadata = if path.ends_with("/_index.md")
            || para_unit(path).is_some_and(|unit| path == format!("{unit}.md"))
        {
            crate::properties::frontmatter_block(source)
                .and_then(|yaml| serde_yaml::from_str::<serde_yaml::Value>(yaml).ok())
                .unwrap_or_default()
        } else {
            serde_yaml::Value::Null
        };
        let title = scalar(&metadata, "title");
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
                domain: path
                    .split('/')
                    .next()
                    .filter(|s| DOMAINS.contains(s))
                    .unwrap_or("Vault")
                    .into(),
                unit: para_unit(path).unwrap_or_else(|| place(path)),
                mtime,
                dated_name,
                family: path
                    .rsplit_once('/')
                    .map_or("Notes", |(parent, _)| {
                        parent.rsplit('/').next().unwrap_or("Notes")
                    })
                    .into(),
                noise: noise(path),
                title: if title.is_empty() {
                    name.to_owned()
                } else {
                    title
                },
                terminal: terminal(&scalar(&metadata, "status"))
                    || terminal(&scalar(&metadata, "lifecycle")),
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
            if let Some(path) = para_unit(&row.path) {
                units.entry(path.clone()).or_insert_with(|| Unit {
                    title: short_project_name(&path, ""),
                    path,
                    ..Unit::default()
                });
            }
        }
        for unit in units.values_mut() {
            if let Some(row) = self
                .notes
                .get(&format!("{}/_index.md", unit.path))
                .or_else(|| self.notes.get(&format!("{}.md", unit.path)))
            {
                unit.terminal |= row.terminal;
            }
            unit.title = short_project_name(&unit.path, &unit.title);
        }
        let mut activity: BTreeMap<String, Activity> = BTreeMap::new();
        let mut groups: BTreeMap<(usize, String), Vec<&Row>> = BTreeMap::new();
        let mut view = View {
            built_at: now,
            project_count: units.len(),
            ..View::default()
        };
        let week = now.saturating_sub(7 * DAY);
        for row in self
            .notes
            .values_mut()
            .filter(|r| !r.noise && r.mtime <= now)
        {
            row.unit = para_unit(&row.path).unwrap_or_else(|| place(&row.path));
            // Look up ancestors rather than comparing every note to every unit.
            let mut ancestor = row.path.trim_end_matches(".md");
            let owner = loop {
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
            if let Some(unit) = owner {
                row.unit.clone_from(&unit.path);
                if !unit.terminal && !noise(&unit.path) {
                    let a = activity
                        .entry(unit.path.clone())
                        .or_insert_with(|| Activity {
                            unit: unit.clone(),
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
            *view.domains.entry(row.domain.clone()).or_default() += 1;
            let period = if row.mtime >= midnight {
                0
            } else if row.mtime >= yesterday {
                1
            } else {
                2
            };
            groups
                .entry((period, row.unit.clone()))
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
            let mut places = Vec::new();
            for ((_, path), mut rows) in groups
                .iter()
                .filter(|((p, _), _)| *p == period)
                .map(|(k, v)| (k, v.clone()))
            {
                rows.sort_by(|a, b| b.mtime.cmp(&a.mtime).then(a.path.cmp(&b.path)));
                let count = rows.len();
                let modified = rows.first().map_or(0, |r| r.mtime);
                let mut changes = Vec::new();
                let mut start = 0;
                while start < rows.len() {
                    let end = rows[start..]
                        .iter()
                        .take_while(|r| rows[start].mtime.saturating_sub(r.mtime) <= 600)
                        .count()
                        + start;
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
                places.push(Place {
                    path: path.clone(),
                    modified,
                    count,
                    rows: changes,
                });
            }
            places.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.path.cmp(&b.path)));
            view.days.push(Period {
                label,
                count: places.iter().map(|p| p.count).sum(),
                places,
            });
        }
        self.view = view;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn short_project_identity_activity_and_domains_use_existing_snapshot_data() {
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
        index.set_units(vec![Unit {
            path: "Dev/Areas/okilum".into(),
            title: "Okilum".into(),
            manager: true,
            ..Default::default()
        }]);
        index.replace(
            "Dev/Areas/okilum/_index.md",
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
        assert_eq!(app.domain(), "Dev");
        assert_eq!(index.view.active[1].domain(), "AI · HomeLab");
        assert_eq!(index.view.active[1].activity_label(now), "4d ago");
        assert_eq!(index.view.older_active.len(), 1);
        assert_eq!(
            index.unit_for("Dev/Areas/okilum/old.md"),
            Some("Dev/Areas/okilum")
        );
        assert_eq!(index.view.project_count, 3);
    }

    #[test]
    fn project_presence_does_not_depend_on_recent_activity_or_archived_noise() {
        let now = 30 * DAY;
        let mut index = Index::default();
        index.replace("Notes/plain.md", "# Note", now);
        index.replace("Archive/Projects/Past/n.md", "", now);
        index.refresh(now, now - 100, now - DAY);
        assert_eq!(index.view.project_count, 0);
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
        let mut i = Index::default();
        i.set_units(vec![Unit {
            path: "Dev/Areas/App".into(),
            title: "App".into(),
            ..Unit::default()
        }]);
        let now = 20 * DAY;
        for (p, t) in [
            ("Dev/Areas/App/notes/a.md", now - 30),
            ("Dev/Areas/App/ops/thread-ids.md", now),
            ("Dev/Projects/Old/a.md", now - 8 * DAY),
        ] {
            i.replace(p, "", t);
        }
        i.replace(
            "Dev/Projects/Done/_index.md",
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
        assert_eq!(i.view.days[0].count, 3);
        assert_eq!(i.rows().filter(|r| r.noise).count(), 7);
        i.remove("Dev/Areas/App/notes/a.md");
        i.refresh(now, now - 100, now - DAY);
        assert!(i.view.active.is_empty());
    }

    #[test]
    fn batches_are_one_row_and_sibling_prefixes_do_not_leak() {
        let now = 20 * DAY;
        let mut i = Index::default();
        for n in 0..60 {
            i.replace(&format!("Dev/Projects/App/notes/{n}.md"), "", now - n);
        }
        i.replace("Dev/Projects/App2/n.md", "", now - 700);
        i.refresh(now, now - 1000, now - DAY);
        assert_eq!(i.view.active.len(), 2);
        let p = &i.view.days[0].places[0];
        assert_eq!(p.count, 60);
        assert_eq!(p.rows.len(), 1);
        assert_eq!(p.rows[0].count, 60);
        assert_eq!(i.view.domains["Dev"], 61);
        i.replace(
            "Dev/Projects/App/_index.md",
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
        println!(
            "WORKBENCH_BENCH full_ms={:.3} batch20_ms={:.3}",
            full.as_secs_f64() * 1000.,
            batch.as_secs_f64() * 1000.
        );
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
        assert!(noise("Dev/4 Archive/Old/plan.md"));
    }
}
