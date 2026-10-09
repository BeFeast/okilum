//! Disposable task projection with source revisions for explicit edit planning.
use comrak::{nodes::NodeValue, parse_document, Arena};
use std::{cmp::Ordering, collections::BTreeMap, sync::Arc};
use time::{Date, Duration};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub path: String,
    pub line: usize,
    pub block: usize,
    pub body_line: usize,
    pub text: String,
    /// Visible inline text when its projection is supported without guessing.
    pub display: Option<String>,
    pub checked: bool,
    pub due: Option<Date>,
    pub scheduled: Option<Date>,
    pub start: Option<Date>,
    pub done: Option<Date>,
    pub priority: u8,
}

fn date(s: &str) -> Option<Date> {
    Date::parse(
        s,
        &time::macros::format_description!("[year]-[month]-[day]"),
    )
    .ok()
}
fn metadata(text: &str, marker: char) -> Option<Date> {
    text.split_once(marker)
        .and_then(|(_, tail)| date(tail.split_whitespace().next()?))
}

pub fn parse(path: &str, source: &str) -> Vec<Task> {
    if !["[ ]", "[x]", "[X]"]
        .iter()
        .any(|marker| source.contains(marker))
    {
        return Vec::new();
    }
    let body = crate::render::without_frontmatter(source);
    let prefix_lines = source[..source.len() - body.len()]
        .bytes()
        .filter(|b| *b == b'\n')
        .count();
    let lines: Vec<_> = body.lines().collect();
    let arena = Arena::new();
    let root = parse_document(&arena, body, &crate::render::comrak_options());
    let mut tasks = Vec::new();
    for (block, top) in root.children().enumerate() {
        for node in top.descendants() {
            let data = node.data.borrow();
            let NodeValue::TaskItem(status) = data.value else {
                continue;
            };
            let body_line = data.sourcepos.start.line;
            let raw = lines
                .get(body_line.saturating_sub(1))
                .copied()
                .unwrap_or("");
            // The AST proves this is a checkbox, excluding fenced examples and YAML.
            let Some((_, text)) = raw.split_once(']') else {
                continue;
            };
            let text = text.trim().to_owned();
            tasks.push(Task {
                path: path.to_owned(),
                line: prefix_lines + body_line,
                block,
                body_line,
                display: node.children().next().and_then(inline_text),
                checked: status.is_some(),
                due: metadata(&text, '📅'),
                scheduled: metadata(&text, '⏳'),
                start: metadata(&text, '🛫'),
                done: metadata(&text, '✅'),
                priority: [('🔺', 0), ('⏫', 1), ('🔼', 2), ('🔽', 4), ('⏬', 5)]
                    .into_iter()
                    .find_map(|(c, p)| text.contains(c).then_some(p))
                    .unwrap_or(3),
                text,
            });
        }
    }
    tasks
}

fn inline_text<'a>(node: &'a comrak::nodes::AstNode<'a>) -> Option<String> {
    if !matches!(node.data.borrow().value, NodeValue::Paragraph) {
        return None;
    }
    let mut text = String::new();
    for child in node.descendants() {
        match &child.data.borrow().value {
            NodeValue::Text(value) => text.push_str(value),
            NodeValue::Code(value) => text.push_str(&value.literal),
            NodeValue::Paragraph
            | NodeValue::Emph
            | NodeValue::Strong
            | NodeValue::Strikethrough
            | NodeValue::Link(_) => {}
            // Images, HTML, multiline and extension nodes may be split or
            // replaced by the native renderer. Never use a lossy approximation
            // to jump to an unrelated occurrence elsewhere in the document.
            _ => return None,
        }
    }
    (!text.is_empty()).then_some(text)
}

#[derive(Clone, Debug)]
struct IndexedNote {
    revision: String,
    tasks: Vec<Task>,
}

#[derive(Clone, Debug, Default)]
pub struct Index {
    notes: BTreeMap<String, Arc<IndexedNote>>,
}
impl Index {
    /// Returns whether tasks or their full-source revision changed. Prose-only
    /// files remain irrelevant; prose edits in a task-bearing note invalidate
    /// edit evidence even when its visible tasks are identical.
    pub fn replace(&mut self, path: &str, source: &str) -> bool {
        let tasks = parse(path, source);
        if tasks.is_empty() {
            return self.remove(path);
        }
        let revision = crate::task_edit::revision(source);
        if self
            .notes
            .get(path)
            .is_some_and(|old| old.revision == revision)
        {
            return false;
        }
        self.notes
            .insert(path.to_owned(), Arc::new(IndexedNote { revision, tasks }));
        true
    }
    /// Tasks of one indexed note, in source order. Empty when the note has none
    /// or is not indexed; used by consumers that must not rescan the vault.
    pub fn note_tasks(&self, path: &str) -> Vec<Task> {
        self.notes
            .get(path)
            .map(|note| note.tasks.clone())
            .unwrap_or_default()
    }
    pub fn remove(&mut self, path: &str) -> bool {
        self.notes.remove(path).is_some()
    }
    /// Resolve on a worker against the immutable index used to display `task`,
    /// never a newer index fetched at click time. The caller reads canonical
    /// source; this method refuses any difference from the displayed revision.
    /// FileEditor still checks the resulting Target again when committing.
    pub fn edit_target(
        &self,
        task: &Task,
        source: &str,
    ) -> Result<crate::task_edit::Target, String> {
        let note = self
            .notes
            .get(&task.path)
            .ok_or("The task source is no longer indexed")?;
        if note.revision != crate::task_edit::revision(source) {
            return Err("This note changed. Refresh the dashboard before editing the task.".into());
        }
        if !note.tasks.contains(task) {
            return Err("The task does not belong to this displayed snapshot".into());
        }
        crate::task_edit::Target::capture(&task.path, source, task.line)
    }
    pub fn query(&self, query: &Query) -> Vec<Task> {
        if !query.unsupported.is_empty() {
            return Vec::new();
        }
        let mut tasks: Vec<_> = self
            .notes
            .values()
            .flat_map(|v| v.tasks.iter())
            .filter(|t| query.filters.iter().all(|f| f.matches(t)))
            .cloned()
            .collect();
        tasks.sort_by(|a, b| {
            for &(key, reverse) in &query.sorts {
                let order = match key {
                    Sort::Due => optional_date(a.due, b.due),
                    Sort::Done => optional_date(a.done, b.done),
                    Sort::Priority => a.priority.cmp(&b.priority),
                    Sort::Path => a.path.cmp(&b.path),
                };
                let order = if reverse { order.reverse() } else { order };
                if order != Ordering::Equal {
                    return order;
                }
            }
            a.path.cmp(&b.path).then(a.line.cmp(&b.line))
        });
        tasks
    }
}
fn optional_date(a: Option<Date>, b: Option<Date>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(&b),
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        _ => Ordering::Equal,
    }
}
#[derive(Clone, Copy, Debug)]
enum Sort {
    Due,
    Done,
    Priority,
    Path,
}
#[derive(Debug)]
enum Filter {
    Status(bool),
    NoDue,
    Date {
        done: bool,
        ordering: Ordering,
        value: Date,
    },
    /// `path includes` / `path does not include` (#880): a literal,
    /// case-insensitive substring of the vault-relative path with `.md`.
    Path {
        lowercase: String,
        include: bool,
    },
}
impl Filter {
    fn matches(&self, task: &Task) -> bool {
        match self {
            Self::Status(done) => task.checked == *done,
            Self::NoDue => task.due.is_none(),
            Self::Date {
                done,
                ordering,
                value,
            } => (if *done { task.done } else { task.due })
                .is_some_and(|d| d.cmp(value) == *ordering),
            Self::Path { lowercase, include } => {
                task.path.to_lowercase().contains(lowercase.as_str()) == *include
            }
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Group {
    #[default]
    Filename,
    Path,
    Due,
    Priority,
    Status,
}
impl Group {
    pub fn label(self, task: &Task) -> String {
        match self {
            Self::Filename => std::path::Path::new(&task.path)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            Self::Path => task
                .path
                .strip_suffix(".md")
                .unwrap_or(&task.path)
                .to_owned(),
            Self::Due => task
                .due
                .map_or_else(|| "No due date".into(), |d| d.to_string()),
            Self::Priority => [
                "Highest priority",
                "High priority",
                "Medium priority",
                "Normal priority",
                "Low priority",
                "Lowest priority",
            ][task.priority as usize]
                .into(),
            Self::Status => if task.checked { "Done" } else { "Open" }.into(),
        }
    }
}
#[derive(Debug, Default)]
pub struct Query {
    filters: Vec<Filter>,
    sorts: Vec<(Sort, bool)>,
    pub unsupported: Vec<String>,
    pub groups: Vec<Group>,
}
impl Query {
    pub fn parse(source: &str, today: Date) -> Self {
        let mut query = Self::default();
        for line in source
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            match line {
                "not done" => query.filters.push(Filter::Status(false)),
                "done" => query.filters.push(Filter::Status(true)),
                "no due date" => query.filters.push(Filter::NoDue),
                _ => {
                    if let Some(filter) = path_filter(line) {
                        query.filters.push(filter);
                        continue;
                    }
                    if let Some(group) = line.strip_prefix("group by ") {
                        let group = match group {
                            "filename" => Some(Group::Filename),
                            "path" => Some(Group::Path),
                            "due" => Some(Group::Due),
                            "priority" => Some(Group::Priority),
                            "status" => Some(Group::Status),
                            _ => None,
                        };
                        if let Some(group) = group {
                            query.groups.push(group);
                            continue;
                        }
                    } else if let Some(sort) = line.strip_prefix("sort by ") {
                        let reverse = sort.ends_with(" reverse");
                        let key = sort.strip_suffix(" reverse").unwrap_or(sort);
                        let key = match key {
                            "due" => Some(Sort::Due),
                            "done" => Some(Sort::Done),
                            "priority" => Some(Sort::Priority),
                            "path" => Some(Sort::Path),
                            _ => None,
                        };
                        if let Some(key) = key {
                            query.sorts.push((key, reverse));
                            continue;
                        }
                    } else if let Some((field, rest)) = line.split_once(' ') {
                        if matches!(field, "due" | "done") {
                            let (ordering, value) = if let Some(v) = rest.strip_prefix("before ") {
                                (Ordering::Less, v)
                            } else if let Some(v) = rest.strip_prefix("after ") {
                                (Ordering::Greater, v)
                            } else {
                                (Ordering::Equal, rest.strip_prefix("on ").unwrap_or(rest))
                            };
                            if let Some(value) = relative_date(value, today) {
                                query.filters.push(Filter::Date {
                                    done: field == "done",
                                    ordering,
                                    value,
                                });
                                continue;
                            }
                        }
                    }
                    query.unsupported.push(line.to_owned());
                }
            }
        }
        query
    }
}
/// Only the literal form. Placeholders (`{{query.file.path}}`), regex and
/// boolean combinations stay unsupported, so the query fails closed instead
/// of matching text the author did not mean.
fn path_filter(line: &str) -> Option<Filter> {
    let (include, value) = if let Some(value) = line.strip_prefix("path includes ") {
        (true, value)
    } else {
        (false, line.strip_prefix("path does not include ")?)
    };
    let value = value.trim();
    if value.is_empty() || value.contains("{{") {
        return None;
    }
    Some(Filter::Path {
        lowercase: value.to_lowercase(),
        include,
    })
}

fn relative_date(value: &str, today: Date) -> Option<Date> {
    if value == "today" {
        return Some(today);
    }
    if let Some(n) = value
        .strip_prefix("in ")
        .and_then(|s| s.strip_suffix(" days"))
    {
        return today.checked_add(Duration::days(n.parse::<u32>().ok()?.into()));
    }
    if let Some(n) = value.strip_suffix(" days ago") {
        return today.checked_sub(Duration::days(n.parse::<u32>().ok()?.into()));
    }
    date(value)
}

/// Bind a result to the same source task, then map its ordinal through the
/// Reader rewrite (expanded embeds are fenced and cannot add canonical tasks).
pub struct Target {
    pub block: usize,
    pub text: Option<String>,
}

pub fn target(task: &Task, original_body: &str, rendered: &str) -> anyhow::Result<Target> {
    let original = parse(&task.path, original_body);
    let ordinal = original
        .iter()
        .position(|t| {
            t.body_line == task.body_line && t.text == task.text && t.checked == task.checked
        })
        .ok_or_else(|| {
            anyhow::anyhow!("This task changed. Refresh the query before opening its source.")
        })?;
    let displayed = parse(&task.path, rendered);
    anyhow::ensure!(
        displayed.len() == original.len(),
        "Task location cannot be mapped in this document"
    );
    Ok(Target {
        block: displayed[ordinal].block,
        text: displayed[ordinal].display.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn today() -> Date {
        date("2026-10-06").unwrap()
    }
    fn index() -> Index {
        let mut i = Index::default();
        i.replace("a.md", "---\ntitle: Tasks\n---\n# Work\n- [ ] Old 📅 2026-10-05 ⏳ 2026-10-01 🛫 2026-09-30\n- [ ] Today 📅 2026-10-06 🔼\n- [ ] Soon 📅 2026-10-08 ⏫\n- [ ] Later 📅 2026-10-20\n- [ ] Undated\n- [x] Finished ✅ 2026-10-05\n- [x] Ancient ✅ 2026-09-01\n");
        i
    }
    fn run(i: &Index, q: &str) -> Vec<Task> {
        i.query(&Query::parse(q, today()))
    }
    #[test]
    fn dashboard_and_substituted_daily_queries() {
        let i = index();
        for (q, expected) in [
            ("not done\ndue before today\nsort by due", "Old"),
            ("not done\ndue today\nsort by priority", "Today"),
            (
                "not done\ndue after today\ndue before in 7 days\nsort by due",
                "Soon",
            ),
            (
                "not done\ndue after in 7 days\ndue before in 30 days\nsort by due",
                "Later",
            ),
            ("not done\nno due date\nsort by path", "Undated"),
            (
                "done\ndone after 7 days ago\nsort by done reverse",
                "Finished",
            ),
            ("not done\ndue on 2026-10-06", "Today"),
            ("done on 2026-10-05", "Finished"),
        ] {
            let tasks = run(&i, q);
            assert_eq!(tasks.len(), 1, "{q}");
            assert!(tasks[0].text.starts_with(expected), "{q}");
        }
        let old = &run(&i, "due before today")[0];
        assert_eq!(old.line, 5);
        assert_eq!(old.scheduled, date("2026-10-01"));
        assert_eq!(old.start, date("2026-09-30"));
    }
    #[test]
    fn syntax_is_not_tasks_and_unknown_queries_fail_closed() {
        let source = "---\nexample: '- [ ] YAML'\n---\n```md\n- [ ] Example\n```\n    - [ ] Indented code\n\n> - [ ] Quoted\n\n1. [x] Ordered\n   - [ ] Nested\n";
        let tasks = parse("n.md", source);
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].text, "Quoted");
        assert!(tasks[1].checked);
        assert_eq!(tasks[2].text, "Nested");
        for q in [
            "due on {{date:YYYY-MM-DD}}",
            "due before nonsense",
            "due on 2026-02-30",
            // Path filters are literal only (#880): these forms still fail.
            "not done\npath includes {{query.file.path}}",
            "path regex matches /private/",
            "path includes ",
            "(path includes a) OR (path includes b)",
            "sort by banana",
            "group by unknown",
        ] {
            let query = Query::parse(q, today());
            assert!(!query.unsupported.is_empty(), "{q}");
            assert!(index().query(&query).is_empty());
        }
    }
    /// #880: literal path filters, case-insensitive on the vault-relative
    /// path including `.md`, ANDed with the other lines.
    #[test]
    fn path_includes_and_does_not_include_filter_by_note_path() {
        let mut i = Index::default();
        i.replace("Projects/Launch Plan.md", "- [ ] Ship\n- [x] Draft\n");
        i.replace("Projects/Archive/Old plan.md", "- [ ] Revisit\n");
        i.replace("Home/Garden.md", "- [ ] Plant\n");
        i.replace("Personal plans.md", "- [ ] Rest\n");
        let texts = |q: &str| {
            let mut t: Vec<_> = run(&i, q).into_iter().map(|t| t.text).collect();
            t.sort();
            t
        };
        assert_eq!(texts("path includes launch plan.md"), ["Draft", "Ship"]);
        assert_eq!(
            texts("not done\npath includes PROJECTS/"),
            ["Revisit", "Ship"]
        );
        assert_eq!(texts("path includes archive/old plan"), ["Revisit"]);
        assert_eq!(
            texts("not done\npath does not include projects/"),
            ["Plant", "Rest"]
        );
        assert_eq!(
            texts("not done\npath includes plan\npath does not include archive"),
            ["Rest", "Ship"],
            "nested paths, spaces and AND semantics"
        );
        assert!(
            texts("path includes .MD").len() == 5,
            "extension is part of the path"
        );
        assert!(texts("path includes nowhere").is_empty());
        // Positive control: the same lines are supported, not silently dropped.
        assert!(
            Query::parse("path includes x\npath does not include y", today())
                .unsupported
                .is_empty()
        );
    }

    #[test]
    fn replacement_removal_ordering_and_boundaries() {
        let mut i = index();
        assert_eq!(run(&i, "not done").len(), 5);
        let sorted = run(&i, "not done\nsort by priority\nsort by due");
        assert!(sorted[0].text.starts_with("Soon"));
        assert!(sorted[1].text.starts_with("Today"));
        assert!(sorted.last().unwrap().text.starts_with("Undated"));
        i.replace("b.md", "- [ ] Boundary 📅 2026-10-13\n");
        assert_eq!(run(&i, "due before in 7 days").len(), 3);
        assert_eq!(run(&i, "due after in 7 days").len(), 1);
        i.replace("a.md", "- [x] Changed ✅ 2026-10-06\n");
        assert_eq!(run(&i, "not done").len(), 1);
        i.remove("b.md");
        assert!(run(&i, "not done").is_empty());
        assert_eq!(run(&i, "done").len(), 1);
        i.replace("a.md", "No tasks now");
        assert!(run(&i, "").is_empty());
    }
    #[test]
    fn projection_tracks_task_source_revisions() {
        let mut index = Index::default();
        assert!(!index.replace("prose.md", "Ordinary prose"));
        assert!(!index.remove("missing.md"));
        assert!(index.replace("task.md", "- [ ] Task\n\nProse"));
        assert!(!index.replace("task.md", "- [ ] Task\n\nProse"));
        assert!(index.replace("task.md", "- [ ] Task\n\nChanged prose"));
        assert!(
            index.replace("task.md", "New paragraph\n\n- [ ] Task\n"),
            "source line changed"
        );
        assert!(index.remove("task.md"));
    }

    #[test]
    fn edit_evidence_stays_bound_to_displayed_snapshot_and_occurrence() {
        let source =
            "\u{feff}---\r\ntype: Note\r\n---\r\n- [ ] Same\r\n- [ ] Same\r\n\r\nProse\r\n";
        let mut index = Index::default();
        index.replace("a.md", source);
        let shown = index.clone();
        let rows = run(&shown, "not done");
        let target = shown.edit_target(&rows[1], source).unwrap();
        let changed = target
            .plan(source, crate::task_edit::Change::Checked(true))
            .unwrap();
        assert_eq!(
            changed.after,
            source.replace("- [ ] Same\r\n\r\n", "- [x] Same\r\n\r\n")
        );
        let newer = source.replace("Prose", "Changed prose");
        assert!(index.replace("a.md", &newer));
        assert_eq!(run(&index, "not done"), rows);
        assert!(shown.edit_target(&rows[1], &newer).is_err());
        assert!(index.edit_target(&rows[1], &newer).is_ok());
        let mut invented = rows[1].clone();
        invented.line += 1;
        assert!(shown.edit_target(&invented, source).is_err());
        index.remove("a.md");
        assert!(index.edit_target(&rows[1], source).is_err());
        assert!(shown.edit_target(&rows[1], source).is_ok());
    }
    #[test]
    fn navigation_uses_rendered_inline_text_and_rejects_lossy_projection() {
        let source = "- [ ] Use `code` and **bold**\n";
        let task = parse("a.md", source).remove(0);
        assert_eq!(
            target(&task, source, source).unwrap().text.as_deref(),
            Some("Use code and bold")
        );
        let source = "- [ ] Read [[note|alias]]\n";
        let task = parse("a.md", source).remove(0);
        let rendered = "- [ ] Read [alias](tessera://open/note.md)\n";
        assert_eq!(
            target(&task, source, rendered).unwrap().text.as_deref(),
            Some("Read alias")
        );
        let source = "- [ ] ![Image](asset.png)\n";
        let task = parse("a.md", source).remove(0);
        assert!(target(&task, source, source).unwrap().text.is_none());
    }

    #[test]
    fn duplicate_task_navigation_is_source_bound_and_rejects_stale_results() {
        let source = "# Work\n\n- [ ] Same\n\nParagraph\n\n- [ ] Same\n";
        let tasks = parse("a.md", source);
        assert_eq!(target(&tasks[1], source, source).unwrap().block, 3);
        assert!(target(&tasks[1], "- [ ] Same\n", "- [ ] Same\n").is_err());
    }
}
