//! Launch progress from T3's own run and thread status (#1420).
//!
//! Okilum never calls `t3_thread_launch`. After the manager receives a launch
//! request, progress is read back from T3: the manager run that received the
//! message (acceptance), then the executor thread whose first message carries
//! this request's marker line (preparing → running → completed, or failed /
//! cancelled). Every observation is a blocking gateway read and runs off the UI
//! thread; [`Tracker::apply`] drops observations of any other launch or of an
//! older state of this one.
use super::Request;
use crate::{ManagerGateway, SendReceipt, ThreadQuery, ThreadSummary};
use anyhow::{Context, Result};
use std::collections::BTreeSet;

/// The first line of the `t3_thread_launch` message the manager is asked to
/// send. Okilum identifies the executor thread only by this line in T3's own
/// timeline, never by title, registry prose or the manager's chat reply.
pub const MARKER: &str = "Okilum launch request: ";
/// New threads inspected per observation; each thread is read at most once.
const MAX_READS_PER_OBSERVATION: usize = 8;
/// Enough for the marker line; the brief itself is never read back.
const FIRST_ITEM_CHARS: u32 = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    /// Delivered to the manager conversation; T3 has not started its run yet.
    Requested,
    /// T3 started the manager run that received this request.
    Accepted,
    /// The executor thread exists; its run has not started yet.
    Preparing,
    Running,
}

impl Stage {
    const ALL: [Stage; 4] = [
        Stage::Requested,
        Stage::Accepted,
        Stage::Preparing,
        Stage::Running,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Stage::Requested => "requested",
            Stage::Accepted => "accepted",
            Stage::Preparing => "preparing",
            Stage::Running => "running",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Failed,
    Cancelled,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Completed => "completed",
            Outcome::Failed => "failed",
            Outcome::Cancelled => "cancelled",
        }
    }
}

/// T3's thread status → launch step. Unknown future statuses advance nothing
/// and stay visible as the raw value.
pub fn classify(status: &str) -> Option<std::result::Result<Stage, Outcome>> {
    Some(match status {
        "idle" | "preparing" | "queued" | "starting" => Ok(Stage::Preparing),
        "running" | "waiting" => Ok(Stage::Running),
        "completed" => Err(Outcome::Completed),
        "failed" | "rolled_back" => Err(Outcome::Failed),
        "cancelled" | "interrupted" => Err(Outcome::Cancelled),
        _ => return None,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExecutorThread {
    pub thread_id: String,
    pub title: String,
    /// T3's raw thread status, shown beside the step.
    pub status: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Progress {
    /// The furthest non-final step T3 has confirmed. Never moves back.
    pub reached: Stage,
    pub outcome: Option<Outcome>,
    pub executor: Option<ExecutorThread>,
    /// Facts that do not advance a step: ambiguity, a missing thread, a manager
    /// run that ended without a matching executor, an unknown status.
    pub note: Option<String>,
}

impl Progress {
    /// `requested → accepted → [preparing] → running → completed`; the current
    /// step is bracketed. A final outcome replaces the remaining steps, and
    /// only steps T3 actually confirmed are listed before it.
    pub fn line(&self) -> String {
        let mut parts = Vec::new();
        for stage in Stage::ALL {
            match self.outcome {
                Some(_) if stage <= self.reached => parts.push(stage.label().to_owned()),
                Some(_) => {}
                None if stage == self.reached => parts.push(format!("[{}]", stage.label())),
                None => parts.push(stage.label().to_owned()),
            }
        }
        parts.push(match self.outcome {
            Some(outcome) => format!("[{}]", outcome.label()),
            None => Outcome::Completed.label().to_owned(),
        });
        parts.join(" → ")
    }

    pub fn finished(&self) -> bool {
        self.outcome.is_some()
    }
}

/// One read of T3 for one launch. Produced off the UI thread.
#[derive(Clone, Debug)]
pub struct Observation {
    request_id: String,
    revision: u64,
    manager_run_active: Option<bool>,
    manager_status: Option<String>,
    listed: Vec<ThreadSummary>,
    inspected: Vec<String>,
    matched: Vec<String>,
}

/// In-memory progress of one delivered launch request.
#[derive(Clone, Debug)]
pub struct Tracker {
    pub request_id: String,
    /// The journal's `clientRequestId` of the manager message.
    pub client_request_id: String,
    manager_thread_id: String,
    executor_project_id: String,
    /// Executor-project threads that existed before the request was sent.
    baseline: Option<BTreeSet<String>>,
    inspected: BTreeSet<String>,
    matched: BTreeSet<String>,
    manager_run_ended: Option<String>,
    revision: u64,
    pub progress: Progress,
}

impl Tracker {
    /// `baseline` is the executor project's listing taken before sending; when
    /// it could not be read, new threads are found by inspection alone.
    pub fn new(
        request: &Request,
        client_request_id: String,
        manager_thread_id: String,
        baseline: Option<&[ThreadSummary]>,
    ) -> Self {
        Self {
            request_id: request.request_id.clone(),
            client_request_id,
            manager_thread_id,
            executor_project_id: request.executor_project_id.clone(),
            baseline: baseline.map(|threads| threads.iter().map(|t| t.thread_id.clone()).collect()),
            inspected: BTreeSet::new(),
            matched: BTreeSet::new(),
            manager_run_ended: None,
            revision: 0,
            progress: Progress {
                reached: Stage::Requested,
                outcome: None,
                executor: None,
                note: None,
            },
        }
    }

    pub fn manager_thread_id(&self) -> &str {
        &self.manager_thread_id
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Blocking T3 reads for one observation. `receipt` is the manager
    /// message's send receipt; without it nothing is read (delivery is not
    /// confirmed, and the journal's Retry owns that state).
    pub fn observe(
        &self,
        gateway: &dyn ManagerGateway,
        receipt: Option<&SendReceipt>,
    ) -> Result<Option<Observation>> {
        let Some(receipt) = receipt else {
            return Ok(None);
        };
        let mut observation = Observation {
            request_id: self.request_id.clone(),
            revision: self.revision,
            manager_run_active: None,
            manager_status: None,
            listed: Vec::new(),
            inspected: Vec::new(),
            matched: Vec::new(),
        };
        if self.matched.is_empty() {
            let mut query = ThreadQuery::messages(&self.manager_thread_id);
            query.limit = Some(1);
            query.max_chars_per_item = Some(1);
            let manager = gateway
                .read_thread(&query)
                .context("Cannot read the manager thread status")?
                .thread;
            observation.manager_run_active =
                Some(manager.active_run_id.as_deref() == Some(receipt.run_id.as_str()));
            observation.manager_status = Some(manager.status);
        }
        let mut listed = gateway
            .list_threads(&self.executor_project_id)
            .context("Cannot list the executor project's T3 threads")?;
        // Newest first, so a fresh launch is inspected before older threads.
        listed.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        let candidates = listed
            .iter()
            .filter(|t| t.thread_id != self.manager_thread_id)
            .filter(|t| !self.inspected.contains(&t.thread_id))
            .filter(|t| {
                !self
                    .baseline
                    .as_ref()
                    .is_some_and(|ids| ids.contains(&t.thread_id))
            })
            .take(MAX_READS_PER_OBSERVATION)
            .map(|t| t.thread_id.clone())
            .collect::<Vec<_>>();
        let marker = format!("{MARKER}{}", self.request_id);
        for id in candidates {
            let mut query = ThreadQuery::messages(&id);
            query.limit = Some(1);
            query.max_chars_per_item = Some(FIRST_ITEM_CHARS);
            let page = gateway
                .read_thread(&query)
                .with_context(|| format!("Cannot read new T3 thread {id}"))?;
            let first = page
                .items
                .iter()
                .min_by_key(|item| item.position)
                .and_then(|item| item.text.as_deref());
            // An empty new thread may not have its first message yet: read it
            // again on the next observation instead of ruling it out.
            let Some(first) = first else {
                continue;
            };
            if first.lines().next().map(str::trim_end) == Some(marker.as_str()) {
                observation.matched.push(id.clone());
            }
            observation.inspected.push(id);
        }
        observation.listed = listed;
        Ok(Some(observation))
    }

    /// Applies an observation of this launch taken at its current revision.
    /// Returns whether anything visible changed.
    pub fn apply(&mut self, observation: Observation) -> bool {
        if observation.request_id != self.request_id || observation.revision != self.revision {
            return false;
        }
        self.revision += 1;
        if self.progress.finished() {
            return false;
        }
        let before = self.progress.clone();
        self.inspected.extend(observation.inspected);
        self.matched.extend(observation.matched);
        if observation.manager_run_active == Some(true) {
            self.progress.reached = self.progress.reached.max(Stage::Accepted);
        }
        let mut note = None;
        match self.matched.len() {
            0 => {
                if self.progress.reached >= Stage::Accepted
                    && observation.manager_run_active == Some(false)
                {
                    self.manager_run_ended = observation.manager_status.clone();
                }
                if let Some(status) = &self.manager_run_ended {
                    note = Some(format!(
                        "The manager's run for this request is no longer active (manager thread: {status}) and no T3 thread carries this request yet. Check the manager chat; do not request another executor."
                    ));
                }
            }
            1 => {
                let id = self.matched.iter().next().unwrap();
                match observation.listed.iter().find(|t| &t.thread_id == id) {
                    Some(thread) => {
                        self.progress.reached = self.progress.reached.max(Stage::Accepted);
                        match classify(&thread.status) {
                            Some(Ok(stage)) => {
                                self.progress.reached = self.progress.reached.max(stage)
                            }
                            Some(Err(outcome)) => self.progress.outcome = Some(outcome),
                            None => {
                                note = Some(format!(
                                    "T3 reports an unknown executor status \"{}\".",
                                    thread.status
                                ))
                            }
                        }
                        self.progress.executor = Some(ExecutorThread {
                            thread_id: thread.thread_id.clone(),
                            title: thread.title.clone(),
                            status: thread.status.clone(),
                        });
                    }
                    None => {
                        note = Some(format!(
                        "Executor thread {id} is missing from the executor project's T3 listing."
                    ))
                    }
                }
            }
            _ => {
                // Never pick a winner between two launches of one request.
                self.progress.reached = self.progress.reached.max(Stage::Accepted);
                self.progress.executor = None;
                note = Some(format!(
                    "More than one T3 thread carries this request: {}. Resolve it in the manager chat.",
                    self.matched.iter().cloned().collect::<Vec<_>>().join(", ")
                ));
            }
        }
        self.progress.note = note;
        self.progress != before
    }

    /// A receipt that started or steered a manager run is T3's own acceptance.
    pub fn record_receipt(&mut self, receipt: &SendReceipt) -> bool {
        if matches!(
            receipt.delivery.as_str(),
            "started" | "steered" | "restarted"
        ) && self.progress.reached < Stage::Accepted
            && !self.progress.finished()
        {
            self.progress.reached = Stage::Accepted;
            return true;
        }
        false
    }
}

#[cfg(test)]
#[path = "progress_tests.rs"]
mod tests;
