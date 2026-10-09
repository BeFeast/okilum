use super::*;
use crate::{inbox as inbox_api, inbox_plan as api};
use okilum_core::source::WriteReceipt;
// The preserving maintenance build changes only this default.
pub(super) const NEW_PLANS_ENABLED: bool = true;
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Journal {
    operations: BTreeMap<String, Intent>,
    aliases: BTreeMap<String, String>,
    pub(super) recovery_required: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct Intent {
    request: api::Request,
    external_key: String,
    source_operation_id: String,
    goal_revision: String,
    outcome: api::Outcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delegation: Option<crate::proposals::ParentBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delegated_source_receipt: Option<WriteReceipt>,
}
impl Runner {
    pub(crate) fn inbox_plan_writable(&self) -> bool {
        self.inbox_planning_enabled
            && self.inbox_writable()
            && !self.state.inbox_plan_journal.recovery_required
    }
    pub(super) fn plan_identity_reserved(&self, operation: &str, external: &str) -> bool {
        let j = &self.state.inbox_plan_journal;
        j.operations.contains_key(operation)
            || j.aliases.contains_key(operation)
            || j.operations.values().any(|i| i.external_key == external)
    }
    pub(super) fn check_plan_inventory(&mut self) -> Result<()> {
        ensure!(
            self.source.required_proposal_inbox_adoption()
                || self
                    .state
                    .inbox_plan_journal
                    .operations
                    .values()
                    .all(|i| i.delegation.is_none() && i.delegated_source_receipt.is_none()),
            "delegated child state lacks Inbox adoption source fence"
        );
        let marker = self.state_dir.join("inbox-plan-enrollment.json");
        if marker.exists() {
            ensure!(
                serde_json::from_slice::<Value>(&fs::read(marker)?)? == self.plan_enrollment(),
                "inbox planning enrollment identity mismatch"
            );
            let state_path = self.state_dir.join("state.json");
            let disk: Value = if state_path.exists() {
                super::journal_value(&fs::read(state_path)?)?
            } else {
                Value::Null
            };
            if disk.get("inbox_plan_journal").is_none() {
                self.state.inbox_plan_journal.recovery_required = true;
            }
        }
        for entry in fs::read_dir(self.root.join(&self.state.records_dir))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("goal-") || !name.ends_with(".md") {
                continue;
            }
            if self
                .state
                .inbox_plan_journal
                .operations
                .values()
                .any(|i| i.delegation.is_some() && name == format!("goal-{}.md", i.outcome.goal_id))
            {
                continue;
            }
            let source = self
                .source
                .read(&format!("{}/{}", self.state.records_dir, name))?;
            let (metadata, _) = parse_document(&source)?;
            if let Some(origin) = metadata.get(serde_yaml::Value::String("origin_inbox".into())) {
                let origin: api::Origin = serde_yaml::from_value(origin.clone())?;
                let matching = self
                    .state
                    .inbox_plan_journal
                    .operations
                    .get(&origin.operation_id)
                    .is_some_and(|intent| {
                        self.path("goal", &intent.outcome.goal_id) == source.path
                            && serde_json::to_value(&intent.outcome.origin).ok()
                                == serde_json::to_value(&origin).ok()
                    });
                if !matching {
                    self.state.inbox_plan_journal.recovery_required = true;
                }
            }
        }
        for (alias, original) in &self.state.inbox_plan_journal.aliases {
            ensure!(
                !self.state.inbox_plan_journal.operations.contains_key(alias)
                    && self
                        .state
                        .inbox_plan_journal
                        .operations
                        .contains_key(original),
                "invalid inbox planning alias inventory"
            );
        }
        Ok(())
    }
    fn plan_enrollment(&self) -> Value {
        serde_json::json!({"schema":"okilum-inbox-plan-enrollment/v1","brain_id":self.state.brain_id,"root":self.root,"records_dir":self.state.records_dir})
    }
    pub(super) fn ensure_plan_enrollment(&self) -> Result<()> {
        let path = self.state_dir.join("inbox-plan-enrollment.json");
        if !path.exists() {
            let mut temp = tempfile::NamedTempFile::new_in(&self.state_dir)?;
            serde_json::to_writer(&mut temp, &self.plan_enrollment())?;
            temp.flush()?;
            temp.as_file().sync_all()?;
            temp.persist_noclobber(path)?;
            File::open(&self.state_dir)?.sync_all()?;
        }
        Ok(())
    }
    pub fn inbox_plan(&mut self, request: api::Request, actor: &str) -> Result<api::Outcome> {
        request.validate(actor)?;
        self.mutation(|r| r.inbox_plan_inner(request))
    }
    fn inbox_plan_inner(&mut self, request: api::Request) -> Result<api::Outcome> {
        let digest = request.digest(&self.state.brain_id)?;
        let external = request.source.external_key()?;
        if self.proposal_identity_reserved(&request.operation_id, &external)?
            || self.inbox_identity_reserved(&request.operation_id, &external)
            || self.attention_identity_reserved(&request.operation_id, &external)
        {
            return Err(inbox_api::error(
                "inbox_plan_operation_conflict",
                "identity belongs to another operation",
            ));
        }
        let journal = &self.state.inbox_plan_journal;
        let original = if journal.operations.contains_key(&request.operation_id) {
            Some(request.operation_id.clone())
        } else {
            journal.aliases.get(&request.operation_id).cloned()
        }
        .or_else(|| {
            journal
                .operations
                .iter()
                .find(|(_, i)| i.external_key == external)
                .map(|(id, _)| id.clone())
        });
        if let Some(original) = original {
            let intent = &self.state.inbox_plan_journal.operations[&original];
            if intent.outcome.receipt.request_sha256 != digest || intent.external_key != external {
                return Err(inbox_api::error(
                    "inbox_plan_operation_conflict",
                    "operation or upstream identity already has different planning input",
                ));
            }
            if request.operation_id != original
                && !self
                    .state
                    .inbox_plan_journal
                    .aliases
                    .contains_key(&request.operation_id)
            {
                if !self.inbox_plan_writable() {
                    return Err(inbox_api::error(
                        "inbox_plan_recovery_required",
                        "new aliases require original planning history",
                    ));
                }
                self.state
                    .inbox_plan_journal
                    .aliases
                    .insert(request.operation_id, original.clone());
                self.persist()?;
            }
            if self.state.inbox_plan_journal.operations[&original]
                .outcome
                .receipt
                .status
                != "committed"
            {
                self.flush_writes()
                    .map_err(|e| inbox_api::error("inbox_plan_recovery_required", e.to_string()))?;
            }
            let mut outcome = self.state.inbox_plan_journal.operations[&original]
                .outcome
                .clone();
            if outcome.receipt.status != "committed" {
                return Err(inbox_api::error(
                    "inbox_plan_recovery_required",
                    "retained goal projection needs recovery",
                ));
            }
            outcome.receipt.replayed = true;
            return Ok(outcome);
        }
        if !self.inbox_plan_writable() {
            return Err(inbox_api::error(
                "inbox_plan_recovery_required",
                "planning requires managed brain and original operation history",
            ));
        }
        let capture = self
            .inbox_get(&request.capture_id)
            .map_err(|e| inbox_api::error("inbox_plan_source_changed", e.to_string()))?;
        if capture.item.revision != request.expected_capture_revision {
            return Err(inbox_api::error(
                "inbox_plan_source_changed",
                "original capture revision changed; review current source",
            ));
        }
        if capture.text.len() > 65_536 {
            return Err(inbox_api::error(
                "inbox_plan_invalid_request",
                "original capture exceeds65536 UTF-8 bytes",
            ));
        }
        self.flush_writes()?;
        let origin = api::Origin {
            schema: "ai-brain/inbox-origin-v1".into(),
            brain_id: self.state.brain_id.clone(),
            capture_id: request.capture_id.clone(),
            path: capture.item.path,
            revision: capture.item.revision,
            text: capture.text,
            source_snapshot: capture.source,
            operation_id: request.operation_id.clone(),
            planned_by: request.source.clone(),
            planned_at: inbox_api::now()?,
        };
        let goal_id = Uuid::new_v4().to_string();
        let goal = Goal {
            id: goal_id.clone(),
            title: request.title.clone(),
            status: "active".into(),
            criteria: request.criteria.clone(),
            stage_ids: vec![],
            task_ref: None,
            extra: BTreeMap::from([("origin_inbox".into(), serde_json::to_value(&origin)?)]),
        };
        let body = format!(
            "# {}\n\nPlanned from [[{}]].\n\n{}",
            goal.title,
            origin.path,
            api::operator_input(&goal)?.context("planned goal origin absent")?
        );
        let mut record = serde_json::to_value(&goal)?;
        record["schema"] = serde_json::json!(SCHEMA);
        record["record_type"] = serde_json::json!("goal");
        record["brain_id"] = serde_json::json!(self.state.brain_id);
        let (goal_revision, source_operation_id) =
            self.queue_create_only("goal", &goal_id, &record, &body)?;
        if self.state.goal_id.is_none() {
            self.state.goal_id = Some(goal_id.clone());
            self.state.primary_goal_id = Some(goal_id.clone());
        } else {
            self.state.other_goals.insert(
                goal_id.clone(),
                GoalState {
                    goal_id: Some(goal_id.clone()),
                    ..GoalState::default()
                },
            );
        }
        let outcome = api::Outcome {
            goal_id,
            origin,
            receipt: inbox_api::Receipt {
                operation_id: request.operation_id.clone(),
                status: "pending".into(),
                request_sha256: digest,
                replayed: false,
            },
        };
        self.state.inbox_plan_journal.operations.insert(
            request.operation_id.clone(),
            Intent {
                request: request.clone(),
                external_key: external,
                source_operation_id,
                goal_revision,
                outcome,
                delegation: None,
                delegated_source_receipt: None,
            },
        );
        self.persist()?;
        #[cfg(test)]
        if std::mem::take(&mut self.interrupt_after_plan_intent) {
            anyhow::bail!("injected crash after planning intent");
        }
        self.flush_writes()
            .map_err(|e| inbox_api::error("inbox_plan_recovery_required", e.to_string()))?;
        Ok(
            self.state.inbox_plan_journal.operations[&request.operation_id]
                .outcome
                .clone(),
        )
    }
    pub(super) fn finalize_plan_write(
        &mut self,
        write: &SourceWrite,
        receipt: &WriteReceipt,
    ) -> Result<()> {
        let records = self.state.records_dir.clone();
        if let Some(intent) = self
            .state
            .inbox_plan_journal
            .operations
            .values_mut()
            .find(|i| i.source_operation_id == write.operation_id)
        {
            ensure!(
                receipt.path == format!("{}/goal-{}.md", records, intent.outcome.goal_id)
                    && receipt.revision == intent.goal_revision,
                "planned goal source receipt differs from retained intent"
            );
            intent.outcome.receipt.status = "committed".into();
        }
        Ok(())
    }
    pub(super) fn planned_goals(&self, capture_id: &str) -> Result<Vec<api::PlannedGoal>> {
        let mut goals = Vec::new();
        for goal in self.goals()? {
            if api::origin(&goal)?.is_some_and(|origin| {
                origin.capture_id == capture_id && origin.brain_id == self.state.brain_id
            }) {
                goals.push(api::PlannedGoal {
                    goal_id: goal.id,
                    title: goal.title,
                });
            }
        }
        Ok(goals)
    }
}

#[cfg(test)]
mod tests;

mod delegated;
