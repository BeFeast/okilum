use super::*;
use crate::inbox::{self as api, Capture, CaptureRequest, Get, Item, List, Receipt};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use tessera_core::source::WriteReceipt;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Intent {
    external_key: String,
    source: api::SourceIdentity,
    source_operation_id: String,
    capture: Capture,
}
#[derive(Serialize, Deserialize)]
struct Cursor {
    brain_id: String,
    generation: String,
    last_id: String,
}
impl Runner {
    pub(super) fn check_inbox_receipt_inventory(&mut self) -> Result<()> {
        for entry in fs::read_dir(self.root.join(&self.state.records_dir))? {
            let entry = entry?;
            let filename = entry.file_name();
            let name = filename.to_string_lossy();
            if name.starts_with("inbox-") && name.ends_with(".md") {
                let path = format!("{}/{}", self.state.records_dir, name);
                if !self
                    .state
                    .inbox_operations
                    .values()
                    .any(|intent| intent.capture.path == path)
                {
                    // Canonical files restore knowledge, never delivery identity.
                    // Retain this condition even if a subsequent edit removes a file.
                    self.state.inbox_recovery_required = true;
                }
            }
        }
        for (alias, original) in &self.state.inbox_aliases {
            ensure!(
                !self.state.inbox_operations.contains_key(alias)
                    && self.state.inbox_operations.contains_key(original),
                "invalid inbox receipt alias; recover original operational state"
            );
        }
        Ok(())
    }

    pub(crate) fn inbox_writable(&self) -> bool {
        self.managed
            && !self.state.inbox_recovery_required
            && !self.state.attention_journal.recovery_required
    }
    pub fn inbox_capture(&mut self, request: CaptureRequest, actor: &str) -> Result<Capture> {
        self.inbox_capture_authorized(request, &crate::inbox::SourceAuthority::Native(actor))
    }
    pub(crate) fn inbox_capture_authorized(
        &mut self,
        request: CaptureRequest,
        authority: &crate::inbox::SourceAuthority<'_>,
    ) -> Result<Capture> {
        request.validate(authority)?;
        self.mutation(|r| r.inbox_capture_inner(request))
    }
    pub(super) fn inbox_identity_reserved(&self, operation: &str, external_key: &str) -> bool {
        self.state.inbox_operations.contains_key(operation)
            || self.state.inbox_aliases.contains_key(operation)
            || self
                .state
                .inbox_operations
                .values()
                .any(|intent| intent.external_key == external_key)
    }
    fn inbox_capture_inner(&mut self, request: CaptureRequest) -> Result<Capture> {
        let digest = request.digest(&self.state.brain_id)?;
        let external_key = request.source.external_key()?;
        if self.proposal_identity_reserved(&request.operation_id, &external_key)?
            || self.attention_identity_reserved(&request.operation_id, &external_key)
            || self.plan_identity_reserved(&request.operation_id, &external_key)
        {
            return Err(api::error(
                "inbox_identity_conflict",
                "Identity is already reserved for an attention action",
            ));
        }
        let by_operation = if self
            .state
            .inbox_operations
            .contains_key(&request.operation_id)
        {
            Some(request.operation_id.clone())
        } else {
            self.state.inbox_aliases.get(&request.operation_id).cloned()
        };
        let by_external = self
            .state
            .inbox_operations
            .iter()
            .find(|(_, intent)| intent.external_key == external_key)
            .map(|(id, _)| id.clone());
        if let Some(original) = by_operation.or(by_external) {
            let intent = self
                .state
                .inbox_operations
                .get(&original)
                .context("inbox alias has no intent")?;
            if intent.capture.receipt.request_sha256 != digest
                || intent.external_key != external_key
            {
                return Err(api::error(
                    "inbox_identity_conflict",
                    "Operation or upstream identity was already used with different content",
                ));
            }
            if request.operation_id != original
                && !self.state.inbox_aliases.contains_key(&request.operation_id)
            {
                if !self.inbox_writable() {
                    return Err(api::error(
                        "inbox_unsupported",
                        "New receipt aliases require original operational history",
                    ));
                }
                self.state
                    .inbox_aliases
                    .insert(request.operation_id, original.clone());
                self.persist()?;
            }
            if self.state.inbox_operations[&original]
                .capture
                .receipt
                .status
                != "committed"
            {
                self.flush_writes()?;
            }
            let mut capture = self.state.inbox_operations[&original].capture.clone();
            if capture.receipt.status != "committed" {
                return Err(api::error(
                    "inbox_projection_pending",
                    "Original capture projection needs recovery",
                ));
            }
            capture.receipt.replayed = true;
            return Ok(capture);
        }
        if !self.inbox_writable() {
            return Err(api::error(
                "inbox_unsupported",
                "Inbox capture requires a managed brain with its original operational receipts",
            ));
        }
        let capture_id = Uuid::new_v4().to_string();
        let received_at = api::now()?;
        let record = api::Record {
            schema: SCHEMA.into(),
            record_type: "inbox".into(),
            brain_id: self.state.brain_id.clone(),
            id: capture_id.clone(),
            status: "captured".into(),
            received_at: received_at.clone(),
            source: request.source.clone(),
        };
        let path = self.path("inbox", &capture_id);
        let (revision, source_operation_id) =
            self.queue_create_only("inbox", &capture_id, &record, &request.text)?;
        self.stage_proposal_candidate(
            crate::proposals::TriggerKind::Inbox,
            &capture_id,
            None,
            &received_at,
            std::slice::from_ref(&source_operation_id),
        )?;
        let capture = Capture {
            capture_id,
            path: path.clone(),
            revision,
            received_at,
            receipt: Receipt {
                operation_id: request.operation_id.clone(),
                status: "pending".into(),
                request_sha256: digest,
                replayed: false,
            },
        };
        self.state.inbox_operations.insert(
            request.operation_id.clone(),
            Intent {
                external_key,
                source: request.source,
                source_operation_id,
                capture,
            },
        );
        self.persist()?;
        #[cfg(test)]
        if std::mem::take(&mut self.interrupt_after_inbox_intent) {
            anyhow::bail!("injected crash after inbox intent before source write");
        }
        self.flush_writes()?;
        Ok(self.state.inbox_operations[&request.operation_id]
            .capture
            .clone())
    }
    pub(super) fn finalize_inbox_write(
        &mut self,
        write: &SourceWrite,
        receipt: &WriteReceipt,
    ) -> Result<()> {
        if let Some(intent) = self
            .state
            .inbox_operations
            .values_mut()
            .find(|intent| intent.source_operation_id == write.operation_id)
        {
            ensure!(
                intent.capture.path == receipt.path && intent.capture.revision == receipt.revision,
                "inbox source receipt does not match persisted intent"
            );
            intent.capture.receipt.status = "committed".into();
        }
        Ok(())
    }
    pub fn inbox_get(&self, capture_id: &str) -> Result<Get> {
        api::canonical_id(capture_id)
            .map_err(|e| api::error("inbox_invalid_request", e.to_string()))?;
        let path = self.path("inbox", capture_id);
        let source = self.source.read(&path).map_err(|e| {
            if e.code == tessera_core::source::ErrorCode::NotFound {
                api::error("inbox_not_found", "Inbox capture is no longer present")
            } else {
                api::error("inbox_record_invalid", e.to_string())
            }
        })?;
        let text = (|| -> Result<String> {
            Ok(String::from_utf8(STANDARD.decode(&source.content_base64)?)?)
        })()
        .map_err(|e| api::error("inbox_record_invalid", format!("{path}: {e}")))?;
        let (record, body) =
            api::validate_record(&text, &path, &self.state.records_dir, &self.state.brain_id)?;
        let title = body
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("")
            .trim()
            .chars()
            .take(160)
            .collect();
        Ok(Get {
            planned_goals: self.planned_goals(capture_id)?,
            text: body,
            item: Item {
                capture_id: record.id,
                path,
                revision: source.revision.clone(),
                received_at: record.received_at,
                status: record.status,
                title,
            },
            source,
        })
    }
    pub fn inbox_list(&self, limit: Option<usize>, cursor: Option<&str>) -> Result<List> {
        let limit = limit.unwrap_or(50);
        if !(1..=200).contains(&limit) {
            return Err(api::error(
                "inbox_invalid_request",
                "Inbox limit must be 1–200",
            ));
        }
        let mut items = Vec::new();
        for entry in fs::read_dir(self.root.join(&self.state.records_dir))? {
            let entry = entry?;
            let filename = entry.file_name();
            let name = filename.to_string_lossy();
            if !name.starts_with("inbox-") || !name.ends_with(".md") {
                continue;
            }
            if items.len() >= 10_000 {
                return Err(api::error(
                    "inbox_record_invalid",
                    "Inbox inventory exceeds 10000 records",
                ));
            }
            let id = &name[6..name.len() - 3];
            api::canonical_id(id).map_err(|e| api::error("inbox_record_invalid", e.to_string()))?;
            items.push(self.inbox_get(id)?.item);
        }
        items.sort_by(|a, b| a.capture_id.cmp(&b.capture_id));
        let generation = api::hash(&serde_json::to_vec(
            &items
                .iter()
                .map(|item| (&item.path, &item.revision))
                .collect::<Vec<_>>(),
        )?);
        let start = if let Some(cursor) = cursor {
            let parsed: Cursor = (|| -> Result<_> {
                ensure!(cursor.len() <= 4096, "cursor is too large");
                Ok(serde_json::from_slice(&URL_SAFE_NO_PAD.decode(cursor)?)?)
            })()
            .map_err(|e| api::error("inbox_invalid_request", e.to_string()))?;
            if parsed.brain_id != self.state.brain_id || parsed.generation != generation {
                return Err(api::error(
                    "inbox_cursor_stale",
                    "Inbox inventory changed; reload from the first page",
                ));
            }
            items
                .iter()
                .position(|item| item.capture_id == parsed.last_id)
                .map(|i| i + 1)
                .ok_or_else(|| {
                    api::error("inbox_invalid_request", "Unknown inbox cursor position")
                })?
        } else {
            0
        };
        let complete = start + limit >= items.len();
        let items: Vec<_> = items.into_iter().skip(start).take(limit).collect();
        let next_cursor = if complete {
            None
        } else {
            Some(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&Cursor {
                brain_id: self.state.brain_id.clone(),
                generation: generation.clone(),
                last_id: items.last().unwrap().capture_id.clone(),
            })?))
        };
        Ok(List {
            items,
            next_cursor,
            complete,
            observed_at: api::now()?,
            generation,
        })
    }
}

#[cfg(test)]
mod tests;
