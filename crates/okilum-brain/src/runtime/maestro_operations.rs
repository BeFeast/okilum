//! Local intention/rejection ledger, using the existing Maestro operation journal.
use super::*;
use crate::maestro_operations::{self as api, Disposition, Rejection, Request};

impl Runner {
    fn validate_maestro_operation(&self, request: &Request) -> Result<()> {
        uuid(request.operation_id())?;
        uuid(request.goal_id())?;
        ensure!(
            self.goal_ids().iter().any(|id| id == request.goal_id()),
            "unknown goal"
        );
        ensure!(
            self.managed,
            "Maestro operation writes require a managed brain"
        );
        ensure!(
            !self.state.maestro_journal.recovery_required,
            "Maestro recovery must be reconciled"
        );
        ensure!(
            serde_json::to_vec(&request.body())?.len() <= 256 * 1024,
            "Maestro operation request exceeds limit"
        );
        match request {
            Request::Link(r) => {
                uuid(&r.project_id)?;
            }
            Request::ApprovalDecision(r) => r.validate()?,
            Request::Unlink(r) => {
                uuid(&r.expected_link_id)?;
            }
        }
        Ok(())
    }
    fn enroll_maestro_operations(&self, decision: bool) -> Result<()> {
        let path = self.state_dir.join("maestro-links-enrollment.json");
        let previous = if path.exists() {
            Some(fs::read(&path)?)
        } else {
            None
        };
        let mut marker = self.workspace_identity();
        marker[api::ENROLLMENT_FIELD] = serde_json::json!(1);
        if decision {
            marker[crate::maestro_control::ENROLLMENT_FIELD] = serde_json::json!(1);
        }
        if let Some(bytes) = &previous {
            let value: Value = serde_json::from_slice(bytes)?;
            ensure!(
                api::enrollment_identity(value.clone())? == self.workspace_identity(),
                "Maestro enrollment identity mismatch"
            );
            if value
                .get(crate::maestro_control::ENROLLMENT_FIELD)
                .is_some()
            {
                marker[crate::maestro_control::ENROLLMENT_FIELD] = serde_json::json!(1);
            }
            if value == marker {
                return Ok(());
            }
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&self.state_dir)?;
        serde_json::to_writer(&mut temporary, &marker)?;
        temporary.flush()?;
        temporary.as_file().sync_all()?;
        ensure!(
            if let Some(bytes) = previous {
                fs::read(&path)? == bytes
            } else {
                !path.exists()
            },
            "Maestro enrollment changed during upgrade"
        );
        temporary.persist(path)?;
        File::open(&self.state_dir)?.sync_all()?;
        Ok(())
    }
    pub(crate) fn maestro_operation_get(&self, goal: &str, operation: &str) -> Result<Disposition> {
        uuid(goal)?;
        uuid(operation)?;
        ensure!(self.goal_ids().iter().any(|id| id == goal), "unknown goal");
        ensure!(
            !self.state.maestro_journal.recovery_required,
            "Maestro recovery must be reconciled"
        );
        let Some(record) = self.state.maestro_journal.operations.get(operation) else {
            return Ok(Disposition::unknown(goal, operation));
        };
        let body = record
            .get("request")
            .context("Maestro operation request is missing")?;
        ensure!(
            body["operation_id"] == operation && body["goal_id"] == goal,
            "Maestro operation belongs to another goal or request"
        );
        let kind = record["kind"]
            .as_str()
            .or_else(|| {
                if record["receipt"]["linked"] == true {
                    Some("link")
                } else if record["receipt"]["unlinked"] == true {
                    Some("unlink")
                } else {
                    None
                }
            })
            .context("Unknown Maestro operation kind")?;
        match kind {
            "link" => {
                serde_json::from_value::<crate::maestro_links::LinkRequest>(body.clone())?;
            }
            "unlink" => {
                serde_json::from_value::<crate::maestro_links::UnlinkRequest>(body.clone())?;
            }
            "approval_decision" => {
                serde_json::from_value::<crate::maestro_control::Request>(body.clone())?
                    .validate()?;
            }
            _ => anyhow::bail!("Unknown Maestro operation kind"),
        }
        let status = record["status"].as_str().unwrap_or("committed");
        ensure!(
            ["pending", "committed", "rejected"].contains(&status),
            "Unknown Maestro operation status"
        );
        let receipt = record
            .get("receipt")
            .filter(|value| !value.is_null())
            .cloned();
        let rejection = record
            .get("rejection")
            .filter(|value| !value.is_null())
            .cloned()
            .map(serde_json::from_value::<Rejection>)
            .transpose()?;
        match status {
            "committed" => {
                let value = receipt
                    .as_ref()
                    .context("Committed Maestro receipt is missing")?;
                ensure!(
                    value["operation_id"] == operation
                        && value["goal_id"] == goal
                        && match kind {
                            "link" => value["linked"] == true,
                            "unlink" => value["unlinked"] == true,
                            "approval_decision" => serde_json::from_value::<
                                crate::maestro_control::Request,
                            >(body.clone())?
                            .matches_local_receipt(value),
                            _ => false,
                        }
                        && rejection.is_none(),
                    "Invalid committed Maestro receipt"
                );
            }
            "rejected" => ensure!(
                receipt.is_none()
                    && rejection.as_ref().is_some_and(
                        |r| !r.code.is_empty() && (kind != "approval_decision" || r.never_sent)
                    ),
                "Invalid rejected Maestro operation"
            ),
            _ => ensure!(
                receipt.is_none() && rejection.is_none(),
                "Pending Maestro operation contains a terminal result"
            ),
        }
        Ok(Disposition {
            schema: api::SCHEMA.into(),
            operation_id: operation.into(),
            goal_id: goal.into(),
            kind: Some(kind.into()),
            request: Some(body.clone()),
            status: status.into(),
            receipt,
            rejection,
        })
    }
    pub(crate) fn maestro_operation_replay(&self, request: &Request) -> Result<Option<Value>> {
        let disposition = self.maestro_operation_get(request.goal_id(), request.operation_id())?;
        if disposition.status == "unknown" {
            return Ok(None);
        }
        ensure!(
            disposition.kind.as_deref() == Some(request.kind())
                && disposition.request.as_ref() == Some(&request.body()),
            "Maestro operation ID reused with different input"
        );
        match disposition.status.as_str() {
            "committed" => Ok(disposition.receipt),
            "rejected" => Err(api::Error {
                message: disposition.rejection.as_ref().unwrap().message.clone(),
                disposition,
            }
            .into()),
            _ => Ok(None),
        }
    }
    pub(crate) fn maestro_operation_begin(&mut self, request: &Request) -> Result<Option<Value>> {
        self.mutation(|runner| {
            runner.validate_maestro_operation(request)?;
            if let Some(receipt) = runner.maestro_operation_replay(request)? { return Ok(Some(receipt)) }
            if runner.state.maestro_journal.operations.contains_key(request.operation_id()) { return Ok(None) }
            ensure!(runner.state.maestro_journal.operations.len() < 2048, "Maestro operation history limit reached");
            runner.enroll_maestro_operations(matches!(request, Request::ApprovalDecision(_)))?;
            runner.state.maestro_journal.operations.insert(request.operation_id().into(), serde_json::json!({
                "kind": request.kind(), "request": request.body(), "status": "pending", "receipt": null, "rejection": null,
            }));
            ensure!(serde_json::to_vec(&runner.state.maestro_journal)?.len() <= 32 * 1024 * 1024, "Maestro retained operation budget reached");
            runner.persist()?;
            Ok(None)
        })
    }
    /// Atomic proof that no remote send began: a pending/committed record always wins.
    pub(crate) fn maestro_decision_refuse_unsent(
        &mut self,
        request: &crate::maestro_control::Request,
        message: &str,
    ) -> Result<Disposition> {
        let operation = Request::ApprovalDecision(Box::new(request.clone()));
        self.mutation(|runner| {
            runner.validate_maestro_operation(&operation)?;
            let prior = runner.maestro_operation_get(&request.goal_id, &request.operation_id)?;
            if prior.status != "unknown" {
                ensure!(prior.kind.as_deref() == Some(operation.kind()) && prior.request.as_ref() == Some(&operation.body()), "Maestro operation ID reused with different input");
                return Ok(prior);
            }
            ensure!(runner.state.maestro_journal.operations.len() < 2048, "Maestro operation history limit reached");
            runner.enroll_maestro_operations(true)?;
            runner.state.maestro_journal.operations.insert(request.operation_id.clone(),serde_json::json!({"kind":"approval_decision","request":request,"status":"rejected","receipt":null,"rejection":{"code":"approval_not_sent","message":message,"never_sent":true}}));
            ensure!(serde_json::to_vec(&runner.state.maestro_journal)?.len() <= 32 * 1024 * 1024, "Maestro retained operation budget reached");
            runner.persist()?;
            runner.maestro_operation_get(&request.goal_id,&request.operation_id)
        })
    }
    pub(crate) fn maestro_operation_reject(
        &mut self,
        request: &Request,
        code: &str,
        message: &str,
    ) -> Result<Disposition> {
        ensure!(!matches!(request, Request::ApprovalDecision(_)), "A remote decision cannot be abandoned or locally rejected; reconcile its exact receipt");
        self.mutation(|runner| {
            runner.validate_maestro_operation(request)?;
            let prior = runner.maestro_operation_get(request.goal_id(), request.operation_id())?;
            if prior.status != "unknown" {
                ensure!(prior.kind.as_deref() == Some(request.kind()) && prior.request.as_ref() == Some(&request.body()), "Maestro operation ID reused with different input");
                if prior.status == "committed" || prior.status == "rejected" { return Ok(prior) }
            } else {
                ensure!(runner.state.maestro_journal.operations.len() < 2048, "Maestro operation history limit reached");
            }
            runner.enroll_maestro_operations(matches!(request, Request::ApprovalDecision(_)))?;
            runner.state.maestro_journal.operations.insert(request.operation_id().into(), serde_json::json!({
                "kind": request.kind(), "request": request.body(), "status": "rejected", "receipt": null,
                "rejection": { "code": code, "message": message },
            }));
            ensure!(serde_json::to_vec(&runner.state.maestro_journal)?.len() <= 32 * 1024 * 1024, "Maestro retained operation budget reached");
            runner.persist()?;
            runner.maestro_operation_get(request.goal_id(), request.operation_id())
        })
    }
    pub(crate) fn maestro_reject_result(
        &mut self,
        request: &Request,
        code: &str,
        message: &str,
    ) -> Result<Value> {
        let disposition = self.maestro_operation_reject(request, code, message)?;
        if disposition.status == "committed" {
            return Ok(disposition.receipt.unwrap());
        }
        Err(api::Error {
            message: disposition.rejection.as_ref().unwrap().message.clone(),
            disposition,
        }
        .into())
    }
    pub(crate) fn maestro_operation_error(
        &self,
        request: &Request,
        error: anyhow::Error,
    ) -> anyhow::Error {
        if error.downcast_ref::<api::Error>().is_some() {
            return error;
        }
        let disposition = self
            .maestro_operation_get(request.goal_id(), request.operation_id())
            .ok()
            .filter(|d| {
                d.status == "unknown"
                    || (d.kind.as_deref() == Some(request.kind())
                        && d.request.as_ref() == Some(&request.body()))
            })
            .unwrap_or_else(|| Disposition::unknown(request.goal_id(), request.operation_id()));
        api::Error {
            disposition,
            message: error.to_string(),
        }
        .into()
    }
}
