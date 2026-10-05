//! Durable export receipts. Model execution is owned by the service worker,
//! while restart recovery never resends a provider request.
use crate::{
    context::{self, ReviewedPacketRef},
    context_export::{ExportInput, ExportSource, GeneratedPackage, Generation},
    Runner,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use tokio::sync::watch;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
pub struct Job {
    pub job_id: String,
    pub goal_id: String,
    pub packet: ReviewedPacketRef,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub error: Option<String>,
    pub package: Option<GeneratedPackage>,
}
impl Job {
    pub fn view(&self) -> Value {
        json!({"job_id":self.job_id,"goal_id":self.goal_id,"packet_id":self.packet.id,"packet_revision":self.packet.revision,"status":self.status,"markdown":self.package.as_ref().map(|p|&p.markdown),"error":self.error})
    }
}
pub struct Jobs {
    root: PathBuf,
    jobs: BTreeMap<String, Job>,
    cancel: BTreeMap<String, watch::Sender<bool>>,
}
impl Jobs {
    pub fn open(operational: &Path) -> Result<Self> {
        let root = operational.join("context-export-jobs");
        fs::create_dir_all(&root)?;
        let mut store = Self {
            root,
            jobs: BTreeMap::new(),
            cancel: BTreeMap::new(),
        };
        for entry in fs::read_dir(&store.root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json")
                || !entry.file_type()?.is_file()
            {
                continue;
            }
            ensure!(
                entry.metadata()?.len() <= 32 * 1024 * 1024,
                "export receipt exceeds bounded storage size"
            );
            let mut job: Job = serde_json::from_slice(&fs::read(path)?)?;
            Uuid::parse_str(&job.job_id)?;
            if job.status == "running" {
                job.status = "interrupted".into();
                job.error =
                    Some("backend_restarted; review and explicitly start a new export".into());
                job.updated_at = crate::retrieval::now();
                store.persist(&job)?;
            }
            store.jobs.insert(job.job_id.clone(), job);
        }
        Ok(store)
    }
    fn persist(&self, job: &Job) -> Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(&self.root)?;
        file.write_all(&serde_json::to_vec(job)?)?;
        file.as_file().sync_all()?;
        file.persist(self.root.join(format!("{}.json", job.job_id)))?;
        fs::File::open(&self.root)?.sync_all()?;
        Ok(())
    }
    pub fn get(&self, goal: &str, id: &str) -> Result<&Job> {
        let job = self.jobs.get(id).context("unknown context export job")?;
        ensure!(
            job.goal_id == goal,
            "context export belongs to another goal"
        );
        Ok(job)
    }
    pub fn latest(&self, goal: &str, packet: &str) -> Option<Value> {
        self.jobs
            .values()
            .filter(|j| j.goal_id == goal && j.packet.id == packet)
            .max_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then(a.job_id.cmp(&b.job_id))
            })
            .map(Job::view)
    }
    pub fn start(
        &mut self,
        goal: String,
        packet: ReviewedPacketRef,
    ) -> Result<(Job, watch::Receiver<bool>)> {
        ensure!(
            !self
                .jobs
                .values()
                .any(|j| j.goal_id == goal && j.status == "running"),
            "a context export is already running for this goal"
        );
        let at = crate::retrieval::now();
        let job = Job {
            job_id: Uuid::new_v4().to_string(),
            goal_id: goal,
            packet,
            status: "running".into(),
            created_at: at.clone(),
            updated_at: at,
            error: None,
            package: None,
        };
        self.persist(&job)?;
        let (send, receive) = watch::channel(false);
        self.cancel.insert(job.job_id.clone(), send);
        self.jobs.insert(job.job_id.clone(), job.clone());
        Ok((job, receive))
    }
    pub fn cancel(&mut self, goal: &str, id: &str) -> Result<Value> {
        let mut job = self.get(goal, id)?.clone();
        if job.status == "running" {
            if let Some(signal) = self.cancel.remove(id) {
                let _ = signal.send(true);
            }
            job.status = "interrupted".into();
            job.error = Some("cancelled_by_operator".into());
            job.updated_at = crate::retrieval::now();
            self.persist(&job)?;
            self.jobs.insert(id.into(), job.clone());
        }
        Ok(job.view())
    }
    pub fn finish(
        &mut self,
        goal: &str,
        id: &str,
        generation: Generation,
        fresh: Result<()>,
    ) -> Result<()> {
        let mut job = self.get(goal, id)?.clone();
        self.cancel.remove(id);
        if job.status != "running" {
            return Ok(());
        }
        match generation {
            Generation::Complete { package } => {
                job.package = Some(*package);
                job.status = "complete".into();
            }
            Generation::Interrupted { reason } => {
                job.status = "interrupted".into();
                job.error = Some(reason);
            }
            Generation::Error { code } => {
                job.status = "error".into();
                job.error = Some(code);
            }
        }
        if let Err(e) = fresh {
            job.status = "stale".into();
            job.error = Some(e.to_string());
        }
        job.updated_at = crate::retrieval::now();
        self.persist(&job)?;
        self.jobs.insert(id.into(), job);
        Ok(())
    }
    pub fn stale(&mut self, goal: &str, id: &str, error: String) -> Result<()> {
        let mut job = self.get(goal, id)?.clone();
        job.status = "stale".into();
        job.error = Some(error);
        job.updated_at = crate::retrieval::now();
        self.persist(&job)?;
        self.jobs.insert(id.into(), job);
        Ok(())
    }
}
pub fn capture(
    runner: &Runner,
    goal_id: &str,
    reference: &ReviewedPacketRef,
) -> Result<ExportInput> {
    let (packet, snapshots) = context::require_reviewed(runner, goal_id, reference)?;
    ensure!(!packet.citations.is_empty(),"AI context export requires at least one selected source; guidance alone has no source evidence");
    let sources = packet
        .citations
        .iter()
        .map(|c| {
            Ok(ExportSource {
                citation_id: c.citation_id.clone(),
                path: c.path.clone(),
                revision: c.revision.clone(),
                start_line: c.start_line,
                end_line: c.end_line,
                excerpt: c.excerpt.clone(),
                metadata: serde_json::from_value(serde_json::to_value(&c.metadata)?)?,
                content_base64: snapshots
                    .iter()
                    .find(|s| s.path == c.path)
                    .context("selected source missing")?
                    .content_base64
                    .clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ExportInput {
        goal_id: goal_id.into(),
        goal_title: runner.snapshot()?.goal.context("unknown goal")?.title,
        goal_revision: packet.goal_revision,
        packet_id: packet.id,
        packet_revision: packet.revision,
        reviewed_text: packet.text,
        sources,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reference() -> ReviewedPacketRef {
        ReviewedPacketRef {
            id: Uuid::new_v4().to_string(),
            revision: "sha256:reviewed".into(),
        }
    }
    #[test]
    fn restart_retains_identity_and_interrupts_without_replaying_provider_work() {
        let root = tempfile::tempdir().unwrap();
        let mut jobs = Jobs::open(root.path()).unwrap();
        let goal = Uuid::new_v4().to_string();
        let reference = reference();
        let (job, receive) = jobs.start(goal.clone(), reference.clone()).unwrap();
        assert!(!*receive.borrow());
        drop(jobs);
        let recovered = Jobs::open(root.path()).unwrap();
        let saved = recovered.get(&goal, &job.job_id).unwrap();
        assert_eq!(saved.status, "interrupted");
        assert_eq!(saved.packet, reference);
        assert!(saved.package.is_none());
        assert!(recovered.cancel.is_empty());
        assert_eq!(
            recovered.latest(&goal, &reference.id).unwrap()["job_id"],
            job.job_id
        );
        assert!(recovered.get("other-goal", &job.job_id).is_err());
        assert!(recovered.latest(&goal, "other-packet").is_none());
    }
    #[test]
    fn cancellation_is_durable_and_late_model_completion_cannot_replace_it() {
        let root = tempfile::tempdir().unwrap();
        let mut jobs = Jobs::open(root.path()).unwrap();
        let goal = Uuid::new_v4().to_string();
        let (job, receive) = jobs.start(goal.clone(), reference()).unwrap();
        assert!(jobs.start(goal.clone(), reference()).is_err());
        jobs.cancel(&goal, &job.job_id).unwrap();
        assert!(*receive.borrow());
        jobs.finish(
            &goal,
            &job.job_id,
            Generation::Error {
                code: "late_transport_result".into(),
            },
            Ok(()),
        )
        .unwrap();
        drop(jobs);
        let recovered = Jobs::open(root.path()).unwrap();
        let saved = recovered.get(&goal, &job.job_id).unwrap();
        assert_eq!(saved.status, "interrupted");
        assert_eq!(saved.error.as_deref(), Some("cancelled_by_operator"));
    }
    #[test]
    fn source_changes_during_generation_are_persisted_as_stale() {
        let root = tempfile::tempdir().unwrap();
        let mut jobs = Jobs::open(root.path()).unwrap();
        let goal = Uuid::new_v4().to_string();
        let (job, _) = jobs.start(goal.clone(), reference()).unwrap();
        jobs.finish(
            &goal,
            &job.job_id,
            Generation::Error {
                code: "response_invalid".into(),
            },
            Err(anyhow::anyhow!("selected source changed")),
        )
        .unwrap();
        drop(jobs);
        let recovered = Jobs::open(root.path()).unwrap();
        let saved = recovered.get(&goal, &job.job_id).unwrap();
        assert_eq!(saved.status, "stale");
        assert_eq!(saved.error.as_deref(), Some("selected source changed"));
        assert!(saved.package.is_none());
    }
}
