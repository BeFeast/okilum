//! «Waiting for me» in Now (#2346): pending manager questions with their
//! exact source identity. Only T3's own pending-request list is evidence; file
//! modification times and activity never create a row. A failed read keeps the
//! last confirmed rows, marked stale with their observation time. The text of
//! each listed question is read from T3 too; a question whose text cannot be
//! read stays a row and says so.
use super::{Manager, Project};
use crate::{NetworkAccess, PendingQuestion};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// At most this many questions per manager thread become rows. The rest is
/// counted, never silently dropped.
pub const ROWS_PER_SOURCE: usize = 20;

/// The exact manager thread a question belongs to.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Source {
    pub folder: PathBuf,
    pub environment: String,
    pub project_id: String,
    pub thread_id: String,
}

/// A project whose manifest names a manager thread. `Err` explains why that
/// thread can never be read (incomplete manifest).
pub fn sources(projects: &[Project]) -> Vec<(PathBuf, Result<Source, String>)> {
    projects
        .iter()
        .filter_map(|project| {
            let Manager::Thread(thread) = &project.manager else {
                return None;
            };
            let source = match (&project.environment, &project.manager_project) {
                (Some(environment), Some(project_id)) => Ok(Source {
                    folder: project.folder.clone(),
                    environment: environment.clone(),
                    project_id: project_id.clone(),
                    thread_id: thread.clone(),
                }),
                _ => Err(super::picker::manifest_problem(project)
                    .unwrap_or_else(|| "T3 environment or project is not configured".into())),
            };
            Some((project.folder.clone(), source))
        })
        .collect()
}

/// One read of every source. A revoked `permit` stops dispatch before the next
/// request; the sources not read are reported as cancelled, not as empty.
pub fn read(
    sources: &[(PathBuf, Result<Source, String>)],
    gateways: &BTreeMap<String, std::sync::Arc<dyn crate::ManagerGateway>>,
    permit: &NetworkAccess,
) -> BTreeMap<PathBuf, Read> {
    let mut result = BTreeMap::new();
    for (folder, source) in sources {
        let read = match source {
            Err(problem) => Read {
                source: None,
                ids: Err(problem.clone()),
                texts: BTreeMap::new(),
            },
            Ok(source) => {
                let mut texts = BTreeMap::new();
                let ids = if !permit.allowed() {
                    Err(CANCELLED.into())
                } else {
                    match gateways.get(&source.environment) {
                        None => Err("Connect to T3 to read manager questions".into()),
                        Some(gateway) => {
                            let ids = {
                                let _scope = permit.enter();
                                gateway.pending_requests(&source.thread_id)
                            };
                            ids.map(|mut ids| {
                                ids.sort();
                                ids.dedup();
                                // Only the ids that become rows are read.
                                for id in ids.iter().take(ROWS_PER_SOURCE) {
                                    if !permit.allowed() {
                                        break;
                                    }
                                    let _scope = permit.enter();
                                    if let Some(text) = gateway
                                        .pending_request_questions(&source.thread_id, id)
                                        .ok()
                                        .as_deref()
                                        .and_then(summary)
                                    {
                                        texts.insert(id.clone(), text);
                                    }
                                }
                                ids
                            })
                            .map_err(|error| error.to_string())
                        }
                    }
                };
                Read {
                    source: Some(source.clone()),
                    ids,
                    texts,
                }
            }
        };
        result.insert(folder.clone(), read);
    }
    result
}

pub const CANCELLED: &str = "Read cancelled";

/// The one-line text of a pending request: its first question (or header),
/// with a count of the further questions it asks. `None` when T3 gave no text.
pub fn summary(questions: &[PendingQuestion]) -> Option<String> {
    let line = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let first = questions.iter().find_map(|question| {
        [question.question.as_str(), question.header.as_str()]
            .into_iter()
            .map(line)
            .find(|text| !text.is_empty())
    })?;
    Some(match questions.len() {
        0 | 1 => first,
        n => format!("{first} (+{} more)", n - 1),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    pub source: Option<Source>,
    pub ids: Result<Vec<String>, String>,
    /// Question text per pending id, for the ids whose text could be read.
    pub texts: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Entry {
    source: Option<Source>,
    /// Last confirmed pending ids of exactly `source`.
    ids: Vec<String>,
    /// Question text of `ids`, where known.
    texts: BTreeMap<String, String>,
    /// When `ids` were confirmed. `None`: never read successfully.
    observed_at: Option<u64>,
    problem: Option<String>,
}

/// The accepted Waiting state of one vault, merged from successive reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ledger {
    entries: BTreeMap<PathBuf, Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub source: Source,
    pub request_id: String,
    /// The question as T3 states it; `None` when its text could not be read.
    pub text: Option<String>,
    /// The source's latest read failed; this row is the last confirmed state.
    pub stale: bool,
    pub observed_at: u64,
}

/// A source that currently cannot be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unavailable {
    pub folder: PathBuf,
    pub problem: String,
    /// Last successful read, when one exists (its rows are shown stale).
    pub observed_at: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct View {
    pub rows: Vec<Row>,
    pub unavailable: Vec<Unavailable>,
    /// Questions beyond [`ROWS_PER_SOURCE`], per manager thread.
    pub omitted: Vec<(Source, usize)>,
    pub sources: usize,
    /// The latest successful read of any source.
    pub checked_at: Option<u64>,
}
impl View {
    /// Some sources are unreadable: the rows are not the whole picture.
    pub fn partial(&self) -> bool {
        !self.unavailable.is_empty()
    }
}

impl Ledger {
    /// Merge a read taken at `at`. The read must cover the current discovery:
    /// projects missing from it are gone and lose their rows. A changed
    /// manager identity never keeps the old thread's questions.
    pub fn apply(&mut self, reads: BTreeMap<PathBuf, Read>, at: u64) {
        let mut entries = BTreeMap::new();
        for (folder, read) in reads {
            let previous = self
                .entries
                .remove(&folder)
                .filter(|entry| entry.source.is_some() && entry.source == read.source);
            let entry = match read.ids {
                Ok(mut ids) => {
                    ids.sort();
                    ids.dedup();
                    // A question asked once keeps its text when only the text
                    // read failed this time; withdrawn questions drop theirs.
                    let mut texts = previous.map(|entry| entry.texts).unwrap_or_default();
                    texts.retain(|id, _| ids.contains(id));
                    texts.extend(read.texts);
                    Entry {
                        source: read.source,
                        ids,
                        texts,
                        observed_at: Some(at),
                        problem: None,
                    }
                }
                Err(problem) => {
                    let previous = previous.unwrap_or_default();
                    Entry {
                        source: read.source,
                        ids: previous.ids,
                        texts: previous.texts,
                        observed_at: previous.observed_at,
                        problem: Some(problem),
                    }
                }
            };
            entries.insert(folder, entry);
        }
        self.entries = entries;
    }

    /// Whether the exact question is still the accepted pending state.
    pub fn contains(&self, source: &Source, request_id: &str) -> bool {
        self.entries.get(&source.folder).is_some_and(|entry| {
            entry.source.as_ref() == Some(source) && entry.ids.iter().any(|id| id == request_id)
        })
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn view(&self) -> View {
        let mut view = View {
            sources: self.entries.len(),
            checked_at: self.entries.values().filter_map(|e| e.observed_at).max(),
            ..View::default()
        };
        for (folder, entry) in &self.entries {
            if let Some(problem) = &entry.problem {
                view.unavailable.push(Unavailable {
                    folder: folder.clone(),
                    problem: problem.clone(),
                    observed_at: entry.observed_at,
                });
            }
            let (Some(source), Some(observed_at)) = (&entry.source, entry.observed_at) else {
                continue;
            };
            for id in entry.ids.iter().take(ROWS_PER_SOURCE) {
                view.rows.push(Row {
                    source: source.clone(),
                    request_id: id.clone(),
                    text: entry.texts.get(id).cloned(),
                    stale: entry.problem.is_some(),
                    observed_at,
                });
            }
            if entry.ids.len() > ROWS_PER_SOURCE {
                view.omitted
                    .push((source.clone(), entry.ids.len() - ROWS_PER_SOURCE));
            }
        }
        view
    }
}

#[cfg(test)]
mod tests;
