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
    pub launch_request: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registry {
    pub manager: String,
    pub executors: Vec<Entry>,
}

impl Registry {
    pub fn read(area: &Path) -> Result<Self> {
        let mut source = String::new();
        std::fs::File::open(area.join("ops/thread-ids.md"))
            .context("Cannot read this area's ops/thread-ids.md executor registry")?
            .take((MAX_REGISTRY_BYTES + 1) as u64)
            .read_to_string(&mut source)?;
        Self::parse(&source)
    }

    /// Supported existing registry: one Current threads / Текущие threads
    /// section, Role (or Роль) + Thread table, one Manager and one inline T3 ID
    /// per row. Historical sections are never candidates for a current binding.
    pub fn parse(source: &str) -> Result<Self> {
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
            "Expected one Current threads / Текущие threads registry section"
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
            "Expected one Role / Thread registry table"
        );
        let mut manager = None;
        let mut executors = Vec::new();
        let mut ids = BTreeSet::new();
        let header = cells(section[tables[0]]);
        let request_columns = header
            .iter()
            .enumerate()
            .filter(|(_, c)| **c == "Launch request")
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        ensure!(
            request_columns.len() <= 1,
            "Duplicate Launch request columns in executor registry"
        );
        let mut request_ids = BTreeSet::new();
        for line in section.iter().skip(tables[0] + 1) {
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
            ensure!(
                row.len() >= 2 && !row[0].is_empty(),
                "Malformed executor registry row"
            );
            let values = row[1]
                .split('`')
                .enumerate()
                .filter(|(i, _)| i % 2 == 1)
                .map(|(_, s)| s)
                .filter(|s| s.starts_with("mcp:"))
                .collect::<Vec<_>>();
            ensure!(
                values.len() == 1
                    && values[0].len() > 4
                    && !values[0].chars().any(char::is_whitespace),
                "Expected one inline mcp: thread ID per registry row"
            );
            let id = values[0].to_owned();
            let request = request_columns
                .first()
                .and_then(|i| row.get(*i))
                .copied()
                .unwrap_or("");
            let launch_request = if matches!(request, "" | "-" | "—") {
                None
            } else {
                let value = request
                    .strip_prefix('`')
                    .and_then(|s| s.strip_suffix('`'))
                    .context("Launch request must contain one inline okilum-launch UUID")?;
                ensure!(
                    crate::launch::valid_request_id(value),
                    "Invalid launch request identity in registry"
                );
                ensure!(
                    request_ids.insert(value.to_owned()),
                    "Duplicate launch request identity in registry; no thread was selected"
                );
                Some(value.to_owned())
            };
            ensure!(
                ids.insert(id.clone()),
                "Duplicate thread ID in executor registry"
            );
            if row[0].eq_ignore_ascii_case("manager") {
                ensure!(
                    launch_request.is_none(),
                    "Manager row cannot acknowledge an executor launch request"
                );
                ensure!(
                    manager.replace(id).is_none(),
                    "Multiple managers in current registry"
                );
            } else {
                ensure!(
                    executors.len() < MAX_EXECUTORS,
                    "Executor registry exceeds 128 entries; no partial registry was loaded"
                );
                executors.push(Entry {
                    role: row[0].to_owned(),
                    thread_id: id,
                    launch_request,
                });
            }
        }
        Ok(Self {
            manager: manager.context("Current executor registry has no Manager row")?,
            executors,
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

#[derive(Clone, Debug)]
pub struct Row {
    pub entry: Entry,
    /// None is explicitly missing from the bound executor project's T3 listing.
    pub thread: Option<ThreadSummary>,
    pub pending_questions: Option<usize>,
    pub question_error: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub manager: String,
    pub project_id: String,
    pub registered: Vec<Row>,
    /// Listing alone does not establish manager ownership. Keep these separate.
    pub unregistered: Vec<ThreadSummary>,
}
impl Snapshot {
    pub fn load(gateway: &dyn ManagerGateway, area: &Path, binding: &AreaBinding) -> Result<Self> {
        Self::load_with_questions(gateway, area, binding, true)
    }
    /// Lifecycle polling in Manager needs status, not up to128 question calls.
    /// The Executors segment still loads question counts through `load`.
    pub fn load_status(
        gateway: &dyn ManagerGateway,
        area: &Path,
        binding: &AreaBinding,
    ) -> Result<Self> {
        Self::load_with_questions(gateway, area, binding, false)
    }
    fn load_with_questions(
        gateway: &dyn ManagerGateway,
        area: &Path,
        binding: &AreaBinding,
        questions: bool,
    ) -> Result<Self> {
        let registry = Registry::read(area)?;
        ensure!(
            Some(&registry.manager) == binding.manager.thread_id.as_ref(),
            "Registry Manager differs from the area manifest; executor ownership was not inferred"
        );
        Self::refresh_with_questions(gateway, registry, &binding.executor.project_id, questions)
    }
    pub fn refresh(
        gateway: &dyn ManagerGateway,
        registry: Registry,
        project: &str,
    ) -> Result<Self> {
        Self::refresh_with_questions(gateway, registry, project, true)
    }
    fn refresh_with_questions(
        gateway: &dyn ManagerGateway,
        registry: Registry,
        project: &str,
        questions: bool,
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
            let (pending_questions, question_error) = if questions && thread.is_some() {
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
        Ok(Self {
            manager: registry.manager,
            project_id: project.into(),
            registered,
            unregistered: threads.into_values().collect(),
        })
    }
}

#[cfg(test)]
mod tests;
