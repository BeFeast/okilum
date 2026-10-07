//! Rebuildable sidebar projection over accepted Markdown snapshots; no filesystem IO.
use crate::vault::warm::Snapshot;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    Active,
    Planned,
    Other,
    Done,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    pub path: String,
    pub title: String,
    pub domain: Option<String>,
    pub status: Status,
    pub status_label: String,
    pub open_tasks: usize,
    pub modified: u128,
}
#[derive(Clone, Default)]
struct Note {
    project: Option<Project>,
    tasks: usize,
    modified: u128,
}
#[derive(Clone, Default)]
pub struct Index {
    notes: BTreeMap<String, Note>,
    rows: Vec<Project>,
}
pub fn archived(path: &str) -> bool {
    path.split('/').any(|segment| {
        let name = segment
            .trim_start_matches(|c: char| c.is_ascii_digit() || matches!(c, '.' | '-' | ' '))
            .to_lowercase();
        matches!(name.as_str(), "archive" | "архив")
    })
}
fn visible(path: &str) -> bool {
    !archived(path)
        && !path
            .split('/')
            .any(|p| p.starts_with('.') || (p.starts_with('_') && p != "_index.md"))
}
impl Index {
    pub fn from_snapshot(snapshot: &Snapshot) -> Self {
        let mut index = Self::default();
        for path in snapshot.source_paths() {
            index.replace_snapshot(snapshot, path);
        }
        index.refresh();
        index
    }
    pub fn replace_snapshot(&mut self, snapshot: &Snapshot, path: &str) {
        if let Some(source) = snapshot.source(path) {
            let modified = snapshot
                .source_revision(path)
                .map_or(0, |r| r.modified_nanoseconds());
            self.replace(path, &source, modified);
        } else {
            self.remove(path);
        }
    }
    pub fn remove(&mut self, path: &str) {
        self.notes.remove(path);
    }
    pub fn replace(&mut self, path: &str, source: &str, modified: u128) {
        if archived(path) {
            self.remove(path);
            return;
        }
        let project = path.strip_suffix("/_index.md").and_then(|folder| {
            let yaml = crate::properties::frontmatter_block(source)?;
            let map: serde_yaml::Mapping = serde_yaml::from_str(yaml).ok()?;
            let scalar = |key: &str| {
                map.get(serde_yaml::Value::String(key.into()))
                    .and_then(serde_yaml::Value::as_str)
            };
            if !scalar("type")?.eq_ignore_ascii_case("project") {
                return None;
            }
            let label = scalar("status").unwrap_or("Unspecified").trim();
            let status = match label.to_lowercase().as_str() {
                "active" | "in progress" | "in-progress" => Status::Active,
                "planned" | "planning" => Status::Planned,
                "done" | "closed" | "completed" | "cancelled" | "canceled" => Status::Done,
                _ => Status::Other,
            };
            let first = folder.split('/').next().unwrap_or(folder);
            let domain = (folder.contains('/')
                && !matches!(
                    first.to_lowercase().as_str(),
                    "projects" | "areas" | "resources"
                ))
            .then(|| first.to_owned());
            Some(Project {
                path: path.into(),
                title: scalar("title")
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or(folder.rsplit('/').next().unwrap_or(folder))
                    .into(),
                domain,
                status,
                status_label: label.into(),
                open_tasks: 0,
                modified,
            })
        });
        let tasks = if visible(path) {
            crate::tasks::parse(path, source)
                .iter()
                .filter(|t| !t.checked && !t.text.trim().is_empty())
                .count()
        } else {
            0
        };
        self.notes.insert(
            path.into(),
            Note {
                project,
                tasks,
                modified: if visible(path) { modified } else { 0 },
            },
        );
    }
    /// Run on the worker once after applying a batch, not from the render path.
    pub fn refresh(&mut self) {
        self.rows = self
            .notes
            .values()
            .filter_map(|n| n.project.clone())
            .collect();
        for project in &mut self.rows {
            let prefix = project.path.strip_suffix("_index.md").unwrap();
            for (_, note) in self
                .notes
                .range(prefix.to_owned()..)
                .take_while(|(p, _)| p.starts_with(prefix))
            {
                project.open_tasks += note.tasks;
                project.modified = project.modified.max(note.modified);
            }
        }
        self.rows.sort_by(|a, b| {
            a.status
                .cmp(&b.status)
                .then(b.modified.cmp(&a.modified))
                .then(a.title.to_lowercase().cmp(&b.title.to_lowercase()))
                .then(a.path.cmp(&b.path))
        });
    }
    pub fn rows(&self) -> &[Project] {
        &self.rows
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projects_are_derived_ordered_scoped_and_incremental() {
        let mut i = Index::default();
        for (path, status, time) in [
            ("Work/Projects/One/_index.md", "active", 1),
            ("Home/Projects/One/_index.md", "planned", 99),
            ("Work/Projects/Done/_index.md", "closed", 200),
            ("Work/4 Archive/Old/_index.md", "active", 300),
        ] {
            i.replace(
                path,
                &format!("---\r\ntype: Project\r\nstatus: {status}\r\n---\r\n- [ ] First\r\n"),
                time,
            );
        }
        i.replace("Work/Projects/One/a.md", "- [ ] Next\n- [x] Done\n", 10);
        i.replace("Work/Projects/OneOther/a.md", "- [ ] Wrong subtree", 1000);
        i.replace("Work/Projects/One/Archive/a.md", "- [ ] Archived", 1001);
        i.replace(
            "Work/Projects/One/_Assets/template.md",
            "- [ ] Template",
            1002,
        );
        i.refresh();
        assert_eq!(i.rows().len(), 3);
        assert_eq!(i.rows()[0].domain.as_deref(), Some("Work"));
        assert_eq!(i.rows()[0].open_tasks, 2);
        assert_eq!(i.rows()[0].modified, 10);
        assert_eq!(i.rows()[1].domain.as_deref(), Some("Home"));
        assert_eq!(i.rows()[2].status, Status::Done);
        i.remove("Work/Projects/One/a.md");
        i.refresh();
        assert_eq!(i.rows()[0].open_tasks, 1);
        i.replace("Work/Projects/One/_index.md", "---\ntype: Note\n---\n", 20);
        i.refresh();
        assert_eq!(i.rows().len(), 2);
    }
    #[test]
    fn unknown_status_stays_visible_and_invalid_types_are_not_guessed() {
        let mut i = Index::default();
        i.replace(
            "Work/P/_index.md",
            "---\ntype: project\nstatus: paused\n---\n",
            1,
        );
        i.replace("Work/Q/_index.md", "---\ntype: [project]\n---\n", 2);
        i.replace(
            "Work/R/_index.md",
            "---\ntype: project\ntype: Note\n---\n",
            3,
        );
        i.refresh();
        assert_eq!(i.rows().len(), 1);
        assert_eq!(i.rows()[0].status_label, "paused");
        i.replace(
            "Work/_Private/Hidden/_index.md",
            "---\ntype: project\n---\n",
            4,
        );
        i.refresh();
        assert_eq!(
            i.rows().len(),
            2,
            "Projects is independent of tree visibility"
        );
    }
}
