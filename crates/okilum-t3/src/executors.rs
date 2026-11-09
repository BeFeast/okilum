//! Explicit area registry → live T3 projection. Top-level executor ownership
//! never comes from parentThreadId, which is null for ordinary manager launches.
use crate::{manifest::AreaBinding, ManagerGateway, ThreadSummary};
use anyhow::{ensure, Context, Result};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};

const MAX_REGISTRY_BYTES: usize = 256 * 1024;
const MAX_EXECUTORS: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub role: String,
    pub thread_id: String,
    /// Stable current issue number from the optional registry column.
    pub current_task: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registry {
    pub manager: String,
    pub executors: Vec<Entry>,
    manager_line: usize,
    references: BTreeSet<String>,
}

impl Registry {
    pub fn read(area: &Path) -> Result<Self> {
        Self::read_optional(area)?
            .context("This context has no executor registry in its selected service directory")
    }

    /// Absence is a normal project state; a present invalid/unreadable registry
    /// remains an error and cannot establish live executor ownership.
    pub fn read_optional(area: &Path) -> Result<Option<Self>> {
        let layout = crate::service_layout::ServiceLayout::resolve(area)?;
        let path = layout.path("thread-ids.md")?;
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && std::fs::symlink_metadata(&path)
                        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None);
            }
            Err(error) => {
                return Err(error).with_context(|| format!("Cannot read {}", path.display()));
            }
        };
        let mut source = String::new();
        file.take((MAX_REGISTRY_BYTES + 1) as u64)
            .read_to_string(&mut source)?;
        layout.check_current()?;
        Self::parse_at(&source, &path.display().to_string()).map(Some)
    }

    /// Supported existing registry: one Current threads / Текущие threads
    /// section, Role (or Роль) + Thread table, one Manager and one inline T3 ID
    /// per row. Historical sections are never candidates for a current binding.
    pub fn parse(source: &str) -> Result<Self> {
        Self::parse_at(source, "ops/thread-ids.md")
    }
    fn parse_at(source: &str, file: &str) -> Result<Self> {
        ensure!(
            source.len() <= MAX_REGISTRY_BYTES,
            "Executor registry exceeds 256 KiB; no partial registry was loaded"
        );
        let lines = source.lines().collect::<Vec<_>>();
        let sections = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| {
                line.starts_with("## Current threads") || line.starts_with("## Текущие threads")
            })
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        ensure!(
            sections.len() == 1,
            "{file}: expected one ## Current threads / ## Текущие threads section"
        );
        let section = &lines[sections[0] + 1..];
        let end = section
            .iter()
            .position(|line| line.starts_with("## "))
            .unwrap_or(section.len());
        let section = &section[..end];
        let tables = section
            .iter()
            .enumerate()
            .filter(|(_, line)| {
                let cells = cells(line);
                cells.len() >= 2 && matches!(cells[0], "Role" | "Роль") && cells[1] == "Thread"
            })
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        ensure!(
            tables.len() == 1,
            "{file}:{}: expected one table beginning | Role | Thread |",
            sections[0] + 1
        );
        let header = cells(section[tables[0]]);
        let task_column = header.iter().position(|cell| *cell == "Current task");
        let kind_column = header.iter().position(|cell| *cell == "Kind");
        let mut roles = BTreeSet::new();
        let mut manager = None;
        let mut manager_line = 0;
        let mut references = BTreeSet::new();
        let mut executors = Vec::new();
        let mut ids = BTreeSet::new();
        for (offset, line) in section.iter().enumerate().skip(tables[0] + 1) {
            if !line.trim().starts_with('|') {
                break;
            }
            let row = cells(line);
            if row
                .iter()
                .all(|s| !s.is_empty() && s.chars().all(|c| c == '-' || c == ':'))
            {
                continue;
            }
            let line_number = sections[0] + 2 + offset;
            ensure!(
                row.len() >= 2 && !row[0].is_empty(),
                "{file}:{line_number}: expected a Role and Thread cell"
            );
            let is_manager = row[0].eq_ignore_ascii_case("manager");
            let kind = kind_column
                .and_then(|column| row.get(column))
                .copied()
                .unwrap_or("");
            let reference = match kind.to_ascii_lowercase().as_str() {
                "" if kind_column.is_none() || is_manager => matches!(row[0].to_ascii_lowercase().as_str(), "decision-thread" | "decision thread" | "decisions"),
                "reference" if !is_manager => true,
                "executor" if !is_manager => false,
                "manager" if is_manager => false,
                _ => anyhow::bail!("{file}:{line_number}: role {:?}: expected Kind `manager` for Manager, or `executor` / `reference` for other rows", row[0]),
            };
            let leading = row[1]
                .strip_prefix('`')
                .and_then(|value| value.split_once('`'))
                .map(|(id, _)| id);
            let values = row[1]
                .split('`')
                .enumerate()
                .filter(|(i, _)| i % 2 == 1)
                .map(|(_, s)| s)
                .filter(|s| s.starts_with("mcp:"))
                .collect::<Vec<_>>();
            ensure!(
                values.len() == 1
                    && leading == values.first().copied()
                    && values[0].len() > 4
                    && !values[0].chars().any(char::is_whitespace),
                "{file}:{line_number}: role {:?}: start the Thread cell with exactly one inline ID in backticks, `mcp:<id>`; put the title or Markdown link after it", row[0]
            );
            let id = values[0].to_owned();
            ensure!(
                ids.insert(id.clone()),
                "{file}:{line_number}: role {:?}: duplicate thread ID in executor registry",
                row[0]
            );
            if is_manager {
                manager_line = line_number;
                ensure!(
                    manager.replace(id).is_none(),
                    "{file}:{line_number}: multiple Manager rows; keep exactly one"
                );
            } else if reference {
                references.insert(id);
            } else {
                ensure!(
                    executors.len() < MAX_EXECUTORS,
                    "Executor registry exceeds 128 entries; no partial registry was loaded"
                );
                ensure!(
                    roles.insert(row[0]),
                    "{file}:{line_number}: duplicate executor role {:?}",
                    row[0]
                );
                let current_task = task_column
                    .and_then(|column| row.get(column))
                    .map(|value| {
                        let raw=value.trim();
                        let value=raw.trim_matches('`');
                        if value.is_empty() || value == "—" || value == "-" {
                            return Ok(None);
                        }
                        let number = value
                            .strip_prefix('#')
                            .unwrap_or(value)
                            .parse::<u64>()
                            .with_context(||format!("{file}:{}: role {:?}, Current task {raw:?}: expected one positive issue number (#2020 or 2020) or blank; put PR, queue and history text in another column",sections[0]+2+offset,row[0]))?;
                        ensure!(number>0,"{file}:{}: role {:?}, Current task {raw:?}: issue number must be positive",sections[0]+2+offset,row[0]);
                        Ok(Some(number))
                    })
                    .transpose()?
                    .flatten();
                executors.push(Entry {
                    role: row[0].to_owned(),
                    thread_id: id,
                    current_task,
                });
            }
        }
        Ok(Self {
            manager: manager.with_context(|| {
                format!(
                    "{file}: add a Manager row with the current thread ID in backticks, `mcp:<id>`"
                )
            })?,
            executors,
            manager_line,
            references,
        })
    }
}
fn cells(line: &str) -> Vec<&str> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub entry: Entry,
    /// None is explicitly missing from the bound executor project's T3 listing.
    pub thread: Option<ThreadSummary>,
    pub pending_questions: Option<usize>,
    pub question_error: Option<String>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub manager: String,
    pub registered: Vec<Row>,
    /// Listing alone does not establish manager ownership. Keep these separate.
    pub unregistered: Vec<ThreadSummary>,
}
impl Snapshot {
    pub fn load(gateway: &dyn ManagerGateway, area: &Path, binding: &AreaBinding) -> Result<Self> {
        let registry = Registry::read(area)?;
        ensure!(
            Some(&registry.manager) == binding.manager.thread_id.as_ref(),
            "{}:{}: Manager row uses `{}` but this project is bound to `{}`. Set the Thread cell to the bound Manager ID in backticks, `mcp:<id>`, or correct the project manifest; executor rows were not replaced",
            crate::service_layout::ServiceLayout::resolve(area)?.path("thread-ids.md")?.display(), registry.manager_line,
            registry.manager, binding.manager.thread_id.as_deref().unwrap_or("no Manager")
        );
        Self::refresh(gateway, registry, &binding.executor.project_id)
    }
    pub fn refresh(
        gateway: &dyn ManagerGateway,
        registry: Registry,
        project: &str,
    ) -> Result<Self> {
        let mut threads = BTreeMap::new();
        for thread in gateway.list_threads(project)? {
            ensure!(
                threads.insert(thread.thread_id.clone(), thread).is_none(),
                "T3 returned a duplicate thread ID; executor list was not replaced"
            );
        }
        let mut registered = Vec::new();
        for entry in registry.executors {
            let thread = threads.remove(&entry.thread_id);
            let (pending_questions, question_error) = if thread.is_some() {
                match gateway.pending_requests(&entry.thread_id) {
                    Ok(ids) => (Some(ids.len()), None),
                    Err(error) => (None, Some(error.to_string())),
                }
            } else {
                (None, None)
            };
            registered.push(Row {
                entry,
                thread,
                pending_questions,
                question_error,
            });
        }
        threads.remove(&registry.manager); // Explicit manager is not an unregistered executor.
        for id in &registry.references {
            threads.remove(id);
        }
        Ok(Self {
            manager: registry.manager,
            registered,
            unregistered: threads.into_values().collect(),
        })
    }
}

#[cfg(test)]
mod tests;
