//! Checked immutable child destination. Restore has no Runner, clock or ID allocator.
use super::*;
use anyhow::Context;
use std::collections::BTreeMap;
use tessera_core::source::SourceWrite;
const SCHEMA: &str = "tessera-frozen-inbox-goal/v1";
const MAX_BYTES: usize = 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Stored {
    schema: String,
    brain_id: String,
    records_dir: String,
    request: Request,
    outcome: Outcome,
    write: SourceWrite,
    revision: String,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub(crate) struct FrozenGoal(Stored);
impl FrozenGoal {
    pub fn request(&self) -> &Request {
        &self.0.request
    }
    pub fn outcome(&self) -> &Outcome {
        &self.0.outcome
    }
    pub fn write(&self) -> &SourceWrite {
        &self.0.write
    }
    pub fn revision(&self) -> &str {
        &self.0.revision
    }
    pub fn restore(bytes: &[u8], brain: &str, records: &str) -> Result<Self> {
        ensure!(
            bytes.len() <= 2 * MAX_BYTES,
            "frozen Inbox goal exceeds bound"
        );
        let target = Self(serde_json::from_slice(bytes)?);
        target.validate(brain, records)?;
        Ok(target)
    }
    fn validate(&self, brain: &str, records: &str) -> Result<()> {
        let t = &self.0;
        let o = &t.outcome;
        let w = &t.write;
        t.request.validate(&t.request.source.actor_id)?;
        for id in [brain, &o.goal_id, &w.operation_id] {
            inbox::canonical_id(id)?;
        }
        ensure!(
            t.schema == SCHEMA
                && t.brain_id == brain
                && t.records_dir == records
                && w.brain_id == brain
                && w.schema == crate::SCHEMA
                && w.expected_revision.is_none(),
            "frozen goal binding mismatch"
        );
        ensure!(
            !records.is_empty()
                && records
                    .split('/')
                    .all(|part| !matches!(part, "" | "." | ".."))
                && !records.contains(['\\', ':'])
                && w.path == format!("{records}/goal-{}.md", o.goal_id),
            "invalid frozen goal path"
        );
        ensure!(
            o.receipt.operation_id == t.request.operation_id
                && o.receipt.request_sha256 == t.request.digest(brain)?
                && o.receipt.status == "pending"
                && !o.receipt.replayed,
            "frozen child receipt mismatch"
        );
        ensure!(
            o.origin.operation_id == t.request.operation_id
                && o.origin.capture_id == t.request.capture_id
                && o.origin.revision == t.request.expected_capture_revision
                && o.origin.planned_by == t.request.source
                && o.origin.brain_id == brain,
            "frozen capture origin mismatch"
        );
        crate::proposal::utc(&o.origin.planned_at)?;
        let bytes = canonical(brain, &t.request, o)?;
        ensure!(
            bytes.len() <= MAX_BYTES
                && STANDARD.encode(&bytes) == w.content_base64
                && t.revision == format!("sha256:{}", inbox::hash(&bytes)),
            "frozen goal bytes/revision mismatch"
        );
        Ok(())
    }
}
fn canonical(brain: &str, request: &Request, outcome: &Outcome) -> Result<Vec<u8>> {
    let goal = Goal {
        id: outcome.goal_id.clone(),
        title: request.title.clone(),
        status: "active".into(),
        criteria: request.criteria.clone(),
        stage_ids: vec![],
        task_ref: None,
        extra: BTreeMap::from([(
            "origin_inbox".into(),
            serde_json::to_value(&outcome.origin)?,
        )]),
    };
    let body = format!(
        "# {}\n\nPlanned from [[{}]].\n\n{}",
        goal.title,
        outcome.origin.path,
        operator_input(&goal)?.context("frozen origin absent")?
    );
    let mut metadata = serde_json::to_value(goal)?;
    metadata["schema"] = crate::SCHEMA.into();
    metadata["record_type"] = "goal".into();
    metadata["brain_id"] = brain.into();
    Ok(format!("---\n{}---\n{}", serde_yaml::to_string(&metadata)?, body).into_bytes())
}
pub(crate) fn prepare(runner: &crate::Runner, request: Request) -> Result<FrozenGoal> {
    request.validate(&request.source.actor_id)?;
    let capture = runner.inbox_get(&request.capture_id)?;
    ensure!(
        capture.item.revision == request.expected_capture_revision && capture.text.len() <= 65_536,
        "original capture changed or exceeds bound"
    );
    let workspace = runner.workspace_identity();
    let brain = workspace["brain_id"].as_str().context("brain absent")?;
    let records = workspace["records_dir"]
        .as_str()
        .context("records absent")?;
    let origin = Origin {
        schema: "ai-brain/inbox-origin-v1".into(),
        brain_id: brain.into(),
        capture_id: request.capture_id.clone(),
        path: capture.item.path,
        revision: capture.item.revision,
        text: capture.text,
        source_snapshot: capture.source,
        operation_id: request.operation_id.clone(),
        planned_by: request.source.clone(),
        planned_at: inbox::now()?,
    };
    let outcome = Outcome {
        receipt: Receipt {
            operation_id: request.operation_id.clone(),
            status: "pending".into(),
            request_sha256: request.digest(brain)?,
            replayed: false,
        },
        goal_id: uuid::Uuid::new_v4().to_string(),
        origin,
    };
    let bytes = canonical(brain, &request, &outcome)?;
    let write = SourceWrite {
        schema: crate::SCHEMA.into(),
        operation_id: uuid::Uuid::new_v4().to_string(),
        brain_id: brain.into(),
        path: format!("{records}/goal-{}.md", outcome.goal_id),
        expected_revision: None,
        content_base64: STANDARD.encode(&bytes),
    };
    let target = FrozenGoal(Stored {
        schema: SCHEMA.into(),
        brain_id: brain.into(),
        records_dir: records.into(),
        request,
        outcome,
        write,
        revision: format!("sha256:{}", inbox::hash(&bytes)),
    });
    target.validate(brain, records)?;
    Ok(target)
}
