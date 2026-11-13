//! Reader integration boundary: caller supplies current editor and policy snapshots.
//! No source writes, background requests or implicit vault traversal.
use okilum_core::agent_language::{self, Report, Status};

pub const POLICY_PATH: &str = "@Schema/language.yaml";

/// The parent reader owns loading the bounded policy snapshot and presenting
/// `status_text` plus range/hover decorations. Call on current source or policy
/// changes, and discard a result if the editor revision changed before display.
pub fn diagnostics(enabled: bool, path: &str, source: &str, policy: Result<&str, &str>) -> Report {
    if !enabled || !okilum_core::agent_imports::is_agent_document(path) {
        return Report {
            status: Status::NotApplicable,
            diagnostics: Vec::new(),
        };
    }
    match policy {
        Ok(policy) => agent_language::diagnose(path, source, Some(policy)),
        Err(reason) => Report::unknown(format!("Language policy unavailable: {reason}")),
    }
}

pub fn status_text(report: &Report) -> String {
    match &report.status {
        Status::NotApplicable => String::new(),
        Status::Unknown(reason) => format!("Language check unknown: {reason}"),
        Status::Checked if report.diagnostics.is_empty() => "Language check: no matches".into(),
        Status::Checked => format!("Language check: {} matches", report.diagnostics.len()),
    }
}

/// Byte ranges to mark in the editor, in source order.
pub fn mark_ranges(report: &Report) -> Vec<std::ops::Range<usize>> {
    report.diagnostics.iter().map(|d| d.range.clone()).collect()
}

/// Wavy underline in the warning color, the same mark as a missing link.
pub fn mark_style(color: gpui::Hsla) -> gpui::HighlightStyle {
    gpui::HighlightStyle {
        underline: Some(gpui::UnderlineStyle {
            thickness: gpui::px(1.),
            color: Some(color),
            wavy: true,
        }),
        ..Default::default()
    }
}

/// Hover text for the term under `offset`, with the matched byte range.
pub fn hover_at(report: &Report, offset: usize) -> Option<(std::ops::Range<usize>, String)> {
    report
        .diagnostics
        .iter()
        .find(|d| d.range.start <= offset && offset < d.range.end)
        .map(|d| {
            (
                d.range.clone(),
                format!(
                    "Transliterated term. Latin form: {}\n\nRule from the vault's language policy.",
                    d.replacement
                ),
            )
        })
}

/// Published ranges apply only to the exact captured text.
pub fn same_text(source: &str, text: &gpui_component::input::Rope) -> bool {
    text.len() == source.len() && text == source
}

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Condvar, Mutex,
};

/// Capture on the UI thread after each editor/policy change. Revision tokens
/// must identify the captured bytes, not a later disk read. `document` includes
/// vault identity so identical relative paths in different vaults cannot mix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub document: String,
    pub path: String,
    pub source_revision: String,
    pub schema_revision: String,
    pub source: Arc<str>,
    pub policy: Result<Arc<str>, Arc<str>>,
}

/// Ranges, hover replacements and status are published as one immutable unit.
pub struct Published {
    pub snapshot: Snapshot,
    pub report: Report,
}

struct Job {
    snapshot: Snapshot,
    cancelled: Arc<AtomicBool>,
}
struct Completion {
    value: Published,
    cancelled: Arc<AtomicBool>,
}
#[derive(Default)]
struct Mailbox {
    pending: Option<Job>,
    completed: Option<Completion>,
    stopped: bool,
}
#[derive(Default)]
struct Shared {
    mailbox: Mutex<Mailbox>,
    wake: Condvar,
}

/// One sleeping local worker per enabled reader, with one latest-only pending
/// capture. Core matching is bounded and unchanged: cancellation invalidates an
/// in-flight match before/after evaluation, rather than interrupting regex code.
/// `notify` must only enqueue a UI event (never access GPUI entities directly).
/// On that event call `apply_ready`, then render only `published`. No timer or
/// polling is needed. Drop/retire never waits for a running match on the UI thread.
pub struct Controller {
    shared: Arc<Shared>,
    notify: Arc<dyn Fn() + Send + Sync>,
    current: Option<Job>,
    published: Option<Published>,
}
impl Controller {
    pub fn new(notify: impl Fn() + Send + Sync + 'static) -> std::io::Result<Self> {
        Self::with_runner(notify, |snapshot| {
            diagnostics(
                true,
                &snapshot.path,
                &snapshot.source,
                snapshot
                    .policy
                    .as_ref()
                    .map(|p| p.as_ref())
                    .map_err(|e| e.as_ref()),
            )
        })
    }

    fn with_runner(
        notify: impl Fn() + Send + Sync + 'static,
        run: impl Fn(&Snapshot) -> Report + Send + 'static,
    ) -> std::io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(notify);
        let worker_notify = notify.clone();
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("agent-language".into())
            .spawn(move || {
                loop {
                    let job = {
                        let mut mailbox = worker.mailbox.lock().unwrap();
                        while mailbox.pending.is_none() && !mailbox.stopped {
                            mailbox = worker.wake.wait(mailbox).unwrap();
                        }
                        if mailbox.stopped {
                            return;
                        }
                        mailbox.pending.take().unwrap()
                    };
                    if job.cancelled.load(Ordering::Acquire) {
                        continue;
                    }
                    let report = run(&job.snapshot);
                    let mut mailbox = worker.mailbox.lock().unwrap();
                    if mailbox.stopped || job.cancelled.load(Ordering::Acquire) {
                        continue;
                    }
                    mailbox.completed = Some(Completion {
                        value: Published {
                            snapshot: job.snapshot,
                            report,
                        },
                        cancelled: job.cancelled,
                    });
                    drop(mailbox);
                    // A retire racing this wake is harmless: apply_ready verifies
                    // identity again, and the callback owns no document source.
                    worker_notify();
                }
            })?;
        Ok(Self {
            shared,
            notify,
            current: None,
            published: None,
        })
    }

    /// Returns true if the visible state was invalidated. Call with a new
    /// snapshot for either source OR schema changes (including policy errors).
    /// Same captures are idempotent; rapid edits replace pending work.
    pub fn observe(&mut self, snapshot: Snapshot) -> bool {
        if self
            .current
            .as_ref()
            .is_some_and(|job| job.snapshot == snapshot)
        {
            return false;
        }
        self.retire();
        if !okilum_core::agent_imports::is_agent_document(&snapshot.path) {
            return true;
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        self.current = Some(Job {
            snapshot: snapshot.clone(),
            cancelled: cancelled.clone(),
        });
        // An unavailable policy has no matching work. Publish its truthful
        // status on the observing thread, without waking the regex worker.
        if let Err(error) = &snapshot.policy {
            let report = diagnostics(true, &snapshot.path, &snapshot.source, Err(error));
            self.shared.mailbox.lock().unwrap().completed = Some(Completion {
                value: Published { snapshot, report },
                cancelled,
            });
            (self.notify)();
            return true;
        }
        self.shared.mailbox.lock().unwrap().pending = Some(Job {
            snapshot,
            cancelled,
        });
        self.shared.wake.notify_one();
        true
    }

    /// Call on disable, close, vault/document switch, or loss of source capture.
    /// Clearing decorations immediately prevents old byte ranges/hover text from
    /// being applied to edited bytes, even before the next result arrives.
    pub fn retire(&mut self) {
        if let Some(job) = self.current.take() {
            job.cancelled.store(true, Ordering::Release);
        }
        self.published = None;
        let mut mailbox = self.shared.mailbox.lock().unwrap();
        mailbox.pending = None;
        mailbox.completed = None;
    }

    /// UI-event delivery only; completion identity includes each observation's
    /// unique token, defeating retire/reopen and revision-token ABA races.
    pub fn apply_ready(&mut self) -> bool {
        let completion = self.shared.mailbox.lock().unwrap().completed.take();
        let Some(completion) = completion else {
            return false;
        };
        let current = self.current.as_ref().is_some_and(|job| {
            Arc::ptr_eq(&job.cancelled, &completion.cancelled)
                && !job.cancelled.load(Ordering::Acquire)
                && job.snapshot == completion.value.snapshot
        });
        if !current {
            return false;
        }
        self.published = Some(completion.value);
        true
    }

    pub fn published(&self) -> Option<&Published> {
        self.published.as_ref()
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.retire();
        self.shared.mailbox.lock().unwrap().stopped = true;
        self.shared.wake.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_policy_is_visible_only_for_enabled_agent_files() {
        assert_eq!(
            diagnostics(false, "AGENTS.md", "", Err("missing")).status,
            Status::NotApplicable
        );
        assert_eq!(
            diagnostics(true, "note.md", "", Err("missing")).status,
            Status::NotApplicable
        );
        let report = diagnostics(true, "AGENTS.md", "", Err("missing"));
        assert!(status_text(&report).contains("unknown"));
        assert!(status_text(&report).contains("missing"));
    }

    #[test]
    fn marks_and_hover_cover_transliterations_but_not_code_or_urls() {
        let policy = "backlog:\n  - pattern: 'воркер'\n    good: worker\n  - pattern: 'мерж\\w*'\n    good: merge\n";
        let source =
            "Воркер мержит.\n\n`воркер`\n\n```\nворкер\n```\n\nhttps://example.test/воркер\n";
        let report = diagnostics(true, "Dev/AGENTS.md", source, Ok(policy));
        let ranges = mark_ranges(&report);
        assert_eq!(ranges.len(), 2, "{report:?}");
        assert_eq!(&source[ranges[0].clone()], "Воркер");
        assert_eq!(&source[ranges[1].clone()], "мержит");
        let (range, text) = hover_at(&report, ranges[0].start + 2).unwrap();
        assert_eq!(range, ranges[0]);
        assert!(text.contains("Latin form: worker"));
        assert!(hover_at(&report, ranges[1].start)
            .unwrap()
            .1
            .contains("merge"));
        assert!(
            hover_at(&report, ranges[0].end).is_none(),
            "end is exclusive"
        );
        let style = mark_style(gpui::Hsla::default());
        assert!(style.underline.unwrap().wavy);
        let contract = diagnostics(true, "_AgentContract.md", "воркер", Ok(policy));
        assert_eq!(
            mark_ranges(&contract).len(),
            1,
            "root contract is an agent file"
        );
        let rope = gpui_component::input::Rope::from(source);
        assert!(same_text(source, &rope));
        assert!(!same_text("other", &rope));
    }

    fn capture(revision: &str, source: &str) -> Snapshot {
        Snapshot {
            document: "vault-a/AGENTS.md".into(),
            path: "AGENTS.md".into(),
            source_revision: revision.into(),
            schema_revision: "policy-1".into(),
            source: source.into(),
            policy: Ok("backlog:\n  - pattern: 'воркер'\n    good: worker\n".into()),
        }
    }
    fn received<T>(rx: &std::sync::mpsc::Receiver<T>) -> T {
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("worker positive control")
    }

    #[test]
    fn async_source_and_schema_refresh_replace_hover_and_ranges_atomically() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut controller = Controller::new(move || {
            tx.send(()).unwrap();
        })
        .unwrap();
        let original = capture("r1", "воркер");
        assert!(controller.observe(original.clone()));
        received(&rx);
        assert!(controller.apply_ready());
        let published = controller.published().unwrap();
        assert_eq!(published.report.diagnostics[0].range, 0..12);
        assert_eq!(published.report.diagnostics[0].replacement, "worker");
        assert!(!controller.observe(original));
        let mut changed = capture("r2", "x воркер");
        changed.schema_revision = "policy-2".into();
        changed.policy = Ok("backlog:\n  - pattern: 'воркер'\n    good: worker-new\n".into());
        assert!(controller.observe(changed.clone()));
        assert!(controller.published().is_none());
        received(&rx);
        assert!(controller.apply_ready());
        let published = controller.published().unwrap();
        assert_eq!(published.snapshot, changed);
        assert_eq!(published.report.diagnostics[0].range, 2..14);
        assert_eq!(published.report.diagnostics[0].replacement, "worker-new");
        // An unread stale schema completion is cleared by a policy-only change.
        let mut unavailable = capture("r2", "x воркер");
        unavailable.schema_revision = "policy-missing".into();
        unavailable.policy = Err("removed".into());
        controller.observe(unavailable);
        received(&rx);
        assert!(controller.apply_ready());
        assert!(matches!(
            controller.published().unwrap().report.status,
            Status::Unknown(_)
        ));
        assert!(controller
            .published()
            .unwrap()
            .report
            .diagnostics
            .is_empty());
    }

    #[test]
    fn running_job_is_cancelled_and_pending_edits_coalesce_without_stale_publication() {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (notify_tx, notify_rx) = std::sync::mpsc::channel();
        let mut controller = Controller::with_runner(
            move || {
                notify_tx.send(()).unwrap();
            },
            move |snapshot| {
                started_tx.send(snapshot.source_revision.clone()).unwrap();
                if snapshot.source_revision == "blocked" {
                    received(&release_rx);
                }
                diagnostics(
                    true,
                    &snapshot.path,
                    &snapshot.source,
                    Ok(snapshot.policy.as_ref().unwrap()),
                )
            },
        )
        .unwrap();
        controller.observe(capture("blocked", "воркер"));
        assert_eq!(received(&started_rx), "blocked");
        controller.observe(capture("coalesced", "воркер воркер"));
        let mut latest = capture("latest", "worker");
        latest.schema_revision = "latest-policy".into();
        controller.observe(latest.clone());
        release_tx.send(()).unwrap();
        assert_eq!(received(&started_rx), "latest");
        received(&notify_rx); // Only current work can notify after the gate.
        assert!(controller.apply_ready());
        assert_eq!(controller.published().unwrap().snapshot, latest);
        assert!(controller
            .published()
            .unwrap()
            .report
            .diagnostics
            .is_empty());
        assert!(started_rx.try_recv().is_err());
        assert!(notify_rx.try_recv().is_err());
    }

    #[test]
    fn stale_completed_results_and_retired_documents_cannot_return() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut controller = Controller::new(move || {
            tx.send(()).unwrap();
        })
        .unwrap();
        let first = capture("r1", "воркер");
        controller.observe(first.clone());
        received(&rx); // Completion exists, UI has not applied it.
        controller.retire();
        assert!(!controller.apply_ready());
        assert!(controller.published().is_none());
        // Reopening even the identical revision creates a distinct observation.
        controller.observe(first);
        received(&rx);
        controller.retire();
        assert!(!controller.apply_ready());
        let mut other_vault = capture("r1", "worker");
        other_vault.document = "vault-b/AGENTS.md".into();
        controller.observe(other_vault.clone());
        received(&rx);
        assert!(controller.apply_ready());
        assert_eq!(controller.published().unwrap().snapshot, other_vault);
        let mut ordinary = capture("r2", "воркер");
        ordinary.path = "note.md".into();
        controller.observe(ordinary);
        assert!(controller.published().is_none());
        assert!(controller.current.is_none());
    }

    #[test]
    fn retire_while_running_then_reopen_same_revision_drops_old_completion() {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (notify_tx, notify_rx) = std::sync::mpsc::channel();
        let first_run = AtomicBool::new(true);
        let mut controller = Controller::with_runner(
            move || {
                notify_tx.send(()).unwrap();
            },
            move |snapshot| {
                started_tx.send(()).unwrap();
                if first_run.swap(false, Ordering::Relaxed) {
                    received(&release_rx);
                }
                diagnostics(
                    true,
                    &snapshot.path,
                    &snapshot.source,
                    Ok(snapshot.policy.as_ref().unwrap()),
                )
            },
        )
        .unwrap();
        let snapshot = capture("same-revision", "воркер");
        controller.observe(snapshot.clone());
        received(&started_rx);
        controller.retire();
        assert!(controller.published().is_none());
        controller.observe(snapshot);
        release_tx.send(()).unwrap();
        received(&started_rx);
        received(&notify_rx);
        assert!(controller.apply_ready());
        assert_eq!(controller.published().unwrap().report.diagnostics.len(), 1);
        assert!(notify_rx.try_recv().is_err());
    }
}
