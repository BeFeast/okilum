use super::*;
use crate::attention::{self as api, Item, List, Mutation, Outcome};
use crate::inbox::{hash, now, Receipt};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use tessera_core::source::WriteReceipt;

#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Journal {
    observations: BTreeMap<String, Observation>,
    sequence: u64,
    operations: BTreeMap<String, Intent>,
    aliases: BTreeMap<String, String>,
    seen: BTreeMap<String, String>,
    pub(super) recovery_required: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct Observation {
    item: Item,
    sequence: u64,
}
#[derive(Clone, Serialize, Deserialize)]
struct Intent {
    external_key: String,
    source_operation_id: Option<String>,
    mutation: Mutation,
    action: String,
    outcome: Outcome,
}
#[derive(Serialize, Deserialize)]
struct Cursor {
    brain_id: String,
    actor: String,
    channel: String,
    generation: String,
    last: String,
}
fn item_key(item: &Item) -> String {
    format!("{}\n{}\n{}", item.goal_id, item.attention_id, item.revision)
}
fn identity_key(item: &Item) -> String {
    format!("{}\n{}", item.goal_id, item.attention_id)
}
fn seen_key(item: &Item, actor: &str, channel: &str) -> Result<String> {
    Ok(hash(&serde_json::to_vec(&(
        actor,
        channel,
        &item.goal_id,
        &item.attention_id,
        &item.revision,
    ))?))
}
impl Runner {
    pub(super) fn attention_decision_inventory(&self) -> Result<BTreeMap<String, String>> {
        let mut paths = BTreeMap::new();
        for intent in self.state.attention_journal.operations.values() {
            if intent.action != "save_decision" {
                continue;
            }
            if let Some(path) = &intent.outcome.path {
                let owner = &intent.mutation.target.goal_id;
                if let Some(previous) = paths.insert(path.clone(), owner.clone()) {
                    ensure!(previous == *owner, "ambiguous decision ownership inventory");
                }
            }
        }
        Ok(paths)
    }
    pub(super) fn check_attention_receipt_inventory(&mut self) -> Result<()> {
        let marker = self.state_dir.join("attention-enrollment.json");
        if marker.exists() {
            let value: Value = serde_json::from_slice(&fs::read(&marker)?)?;
            ensure!(
                value == self.attention_enrollment(),
                "attention enrollment identity mismatch; restore original operational state"
            );
            let path = self.state_dir.join("state.json");
            let journal: Value = if path.exists() {
                super::journal_value(&fs::read(path)?)?
            } else {
                Value::Null
            };
            if journal.get("attention_journal").is_none() {
                self.state.attention_journal.recovery_required = true;
            }
        }
        for entry in fs::read_dir(self.root.join(&self.state.records_dir))? {
            let entry = entry?;
            let filename = entry.file_name();
            let name = filename.to_string_lossy();
            if !name.starts_with("decision-") || !name.ends_with(".md") {
                continue;
            }
            let path = format!("{}/{}", self.state.records_dir, name);
            let source = self.source.read(&path)?;
            let (metadata, _) = parse_document(&source)?;
            if metadata.contains_key(serde_yaml::Value::String("attention_revision".into()))
                && !self
                    .state
                    .attention_journal
                    .operations
                    .values()
                    .any(|intent| intent.outcome.path.as_deref() == Some(&path))
            {
                self.state.attention_journal.recovery_required = true;
            }
        }
        for (alias, original) in &self.state.attention_journal.aliases {
            ensure!(
                !self.state.attention_journal.operations.contains_key(alias)
                    && self
                        .state
                        .attention_journal
                        .operations
                        .contains_key(original),
                "invalid attention receipt alias; restore original operational state"
            );
        }
        Ok(())
    }
    fn attention_enrollment(&self) -> Value {
        serde_json::json!({"schema":"tessera-attention-enrollment/v1", "brain_id":self.state.brain_id, "root":self.root, "records_dir":self.state.records_dir})
    }
    pub(super) fn ensure_attention_enrollment(&self) -> Result<()> {
        let path = self.state_dir.join("attention-enrollment.json");
        if !path.exists() {
            // The workspace checkpoint must already contain attention_journal.
            // No API can observe/send/ack before this marker is durable.
            let mut temporary = tempfile::NamedTempFile::new_in(&self.state_dir)?;
            serde_json::to_writer(&mut temporary, &self.attention_enrollment())?;
            temporary.flush()?;
            temporary.as_file().sync_all()?;
            temporary.persist_noclobber(path)?;
            File::open(&self.state_dir)?.sync_all()?;
        }
        Ok(())
    }
    pub(crate) fn attention_readable(&self) -> bool {
        !self.state.attention_journal.recovery_required
    }
    fn require_attention_history(&self) -> Result<()> {
        if !self.attention_readable() {
            return Err(api::error("attention_unsupported", "Original attention delivery history needs recovery; workspace attention and source reading remain available"));
        }
        Ok(())
    }
    pub(crate) fn attention_writable(&self) -> bool {
        self.managed
            && !self.state.attention_journal.recovery_required
            && !self.state.inbox_recovery_required
    }
    pub(super) fn attention_identity_reserved(&self, operation: &str, external: &str) -> bool {
        let journal = &self.state.attention_journal;
        journal.operations.contains_key(operation)
            || journal.aliases.contains_key(operation)
            || journal
                .operations
                .values()
                .any(|intent| intent.external_key == external)
    }
    fn attention_materialize(&self, raw: Vec<Value>) -> Result<Vec<Item>> {
        ensure!(
            raw.len() <= 10_000,
            "attention inventory exceeds 10000 items"
        );
        let mut items = Vec::new();
        for value in raw {
            let string = |key: &str| -> Result<String> {
                Ok(value[key]
                    .as_str()
                    .with_context(|| format!("attention {key} missing"))?
                    .into())
            };
            let goal_id = string("goal_id")?;
            let attention_id = string("attention_id")?;
            api::identity(&goal_id, &attention_id)?;
            let stage_id = value["stage_id"].as_str().map(str::to_owned);
            let result_id = value["result_id"].as_str().map(str::to_owned);
            let (_, goal_source) = self.record::<Goal>("goal", &goal_id)?;
            let stage_revision = if let Some(id) = &stage_id {
                let (stage, source) = self.record::<Stage>("stage", id)?;
                ensure!(
                    stage.goal_id == goal_id,
                    "attention stage belongs to another goal"
                );
                Some(source.revision)
            } else {
                ensure!(
                    result_id.is_none(),
                    "null stage cannot own an inferred result"
                );
                None
            };
            let result_revision = if let Some(id) = &result_id {
                let (result, source) = self.record::<ResultRecord>("result", id)?;
                ensure!(
                    result.goal_id == goal_id && Some(&result.stage_id) == stage_id.as_ref(),
                    "attention result belongs to another goal/stage"
                );
                Some(source.revision)
            } else {
                None
            };
            let kind = string("kind")?;
            let allowed_actions = match kind.as_str() {
                "decision" | "blocker" => vec!["save_decision".into(), "ack_seen".into()],
                "final" => vec!["ack_seen".into()],
                _ => vec![],
            };
            let mut item = Item {
                goal_id,
                attention_id,
                revision: String::new(),
                goal_title: string("goal_title")?,
                goal_status: string("goal_status")?,
                stage_id,
                result_id,
                kind,
                message: string("message")?,
                allowed_actions,
                current: true,
                seen: false,
                seen_at: None,
                actor_id: String::new(),
                channel: String::new(),
            };
            item.revision = format!(
                "sha256:{}",
                hash(&serde_json::to_vec(&(
                    &item,
                    goal_source.revision,
                    stage_revision,
                    result_revision
                ))?)
            );
            items.push(item);
        }
        items.sort_by_key(identity_key);
        ensure!(
            items
                .windows(2)
                .all(|pair| identity_key(&pair[0]) != identity_key(&pair[1])),
            "duplicate current attention identity"
        );
        Ok(items)
    }
    fn remember_attention(&mut self, items: &[Item]) -> Result<bool> {
        let mut changed = false;
        for item in items {
            let key = item_key(item);
            let journal = &mut self.state.attention_journal;
            if !journal.observations.contains_key(&key) {
                journal.sequence = journal
                    .sequence
                    .checked_add(1)
                    .context("attention sequence exhausted")?;
                journal.observations.insert(
                    key,
                    Observation {
                        item: item.clone(),
                        sequence: journal.sequence,
                    },
                );
                changed = true;
            }
        }
        Ok(changed)
    }
    fn attention_present(
        &self,
        mut item: Item,
        actor: &str,
        channel: &str,
        current: bool,
    ) -> Result<Item> {
        item.current = current;
        if !current {
            item.allowed_actions.clear();
        }
        item.seen_at = self
            .state
            .attention_journal
            .seen
            .get(&seen_key(&item, actor, channel)?)
            .cloned();
        item.seen = item.seen_at.is_some();
        item.actor_id = actor.into();
        item.channel = channel.into();
        Ok(item)
    }
    pub(crate) fn attention_list(
        &mut self,
        raw: Vec<Value>,
        actor: &str,
        channel: Option<&str>,
        limit: Option<usize>,
        cursor: Option<&str>,
    ) -> Result<List> {
        let channel = api::channel(channel)?;
        self.attention_list_scoped(raw, actor, channel, limit, cursor)
    }
    pub(crate) fn attention_list_connector(
        &mut self,
        raw: Vec<Value>,
        context: &crate::connector::Authorized,
        limit: Option<usize>,
        cursor: Option<&str>,
    ) -> Result<List> {
        self.attention_list_scoped(raw, context.actor(), "telegram", limit, cursor)
    }
    fn attention_list_scoped(
        &mut self,
        raw: Vec<Value>,
        actor: &str,
        channel: &str,
        limit: Option<usize>,
        cursor: Option<&str>,
    ) -> Result<List> {
        self.require_attention_history()?;
        self.mutation(|r| r.attention_list_inner(raw, actor, channel, limit, cursor))
    }
    fn attention_list_inner(
        &mut self,
        raw: Vec<Value>,
        actor: &str,
        channel: &str,
        limit: Option<usize>,
        cursor: Option<&str>,
    ) -> Result<List> {
        let limit = limit.unwrap_or(50);
        if !(1..=200).contains(&limit) {
            return Err(api::error(
                "attention_invalid_request",
                "Attention limit must be 1–200",
            ));
        }
        let items = self.attention_materialize(raw)?;
        if self.remember_attention(&items)? {
            self.persist()?;
        }
        let items: Vec<_> = items
            .into_iter()
            .map(|item| self.attention_present(item, actor, channel, true))
            .collect::<Result<_>>()?;
        let generation = hash(&serde_json::to_vec(&items)?);
        let start = if let Some(cursor) = cursor {
            let cursor: Cursor = (|| -> Result<_> {
                ensure!(cursor.len() <= 4096, "cursor is too large");
                Ok(serde_json::from_slice(&URL_SAFE_NO_PAD.decode(cursor)?)?)
            })()
            .map_err(|e| api::error("attention_invalid_request", e.to_string()))?;
            if cursor.brain_id != self.state.brain_id
                || cursor.actor != actor
                || cursor.channel != channel
                || cursor.generation != generation
            {
                return Err(api::error(
                    "attention_cursor_stale",
                    "Attention changed; reload the first page",
                ));
            }
            items
                .iter()
                .position(|item| identity_key(item) == cursor.last)
                .map(|i| i + 1)
                .ok_or_else(|| api::error("attention_invalid_request", "Unknown cursor position"))?
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
                actor: actor.into(),
                channel: channel.into(),
                generation: generation.clone(),
                last: identity_key(items.last().unwrap()),
            })?))
        };
        Ok(List {
            items,
            next_cursor,
            complete,
            generation,
            observed_at: now()?,
            delivery_cursor: self.state.attention_journal.sequence,
        })
    }
    pub(crate) fn attention_get(
        &mut self,
        raw: Vec<Value>,
        actor: &str,
        channel: Option<&str>,
        goal: &str,
        attention: &str,
        revision: Option<&str>,
    ) -> Result<Value> {
        let channel = api::channel(channel)?;
        self.require_attention_history()?;
        self.mutation(|r| r.attention_get_inner(raw, actor, channel, goal, attention, revision))
    }
    pub(crate) fn attention_get_connector(
        &mut self,
        raw: Vec<Value>,
        context: &crate::connector::Authorized,
        goal: &str,
        attention: &str,
        revision: Option<&str>,
    ) -> Result<Value> {
        self.require_attention_history()?;
        self.mutation(|r| {
            r.attention_get_inner(raw, context.actor(), "telegram", goal, attention, revision)
        })
    }
    fn attention_get_inner(
        &mut self,
        raw: Vec<Value>,
        actor: &str,
        channel: &str,
        goal: &str,
        attention: &str,
        revision: Option<&str>,
    ) -> Result<Value> {
        api::identity(goal, attention)
            .map_err(|e| api::error("attention_invalid_request", e.to_string()))?;
        let items = self.attention_materialize(raw)?;
        if self.remember_attention(&items)? {
            self.persist()?;
        }
        let current = items
            .iter()
            .find(|item| item.goal_id == goal && item.attention_id == attention);
        let (item, is_current) = if let Some(revision) = revision {
            if let Some(item) = current.filter(|item| item.revision == revision) {
                (item.clone(), true)
            } else {
                let key = format!("{goal}\n{attention}\n{revision}");
                let observation = self
                    .state
                    .attention_journal
                    .observations
                    .get(&key)
                    .ok_or_else(|| {
                        api::error("attention_not_found", "Attention revision was not observed")
                    })?;
                (observation.item.clone(), false)
            }
        } else {
            (
                current.cloned().ok_or_else(|| {
                    api::error("attention_not_found", "Attention is no longer current")
                })?,
                true,
            )
        };
        Ok(
            serde_json::json!({"item":self.attention_present(item,actor,channel,is_current)?,"observed_at":now()?}),
        )
    }
}
impl Runner {
    pub(super) fn finalize_attention_write(
        &mut self,
        write: &SourceWrite,
        receipt: &WriteReceipt,
    ) -> Result<()> {
        if let Some(intent) = self
            .state
            .attention_journal
            .operations
            .values_mut()
            .find(|intent| intent.source_operation_id.as_deref() == Some(&write.operation_id))
        {
            ensure!(
                intent.outcome.path.as_deref() == Some(&receipt.path)
                    && intent.outcome.revision.as_deref() == Some(&receipt.revision),
                "attention source receipt does not match persisted intent"
            );
            intent.outcome.receipt.status = "committed".into();
        }
        Ok(())
    }
    pub(crate) fn attention_mutate(
        &mut self,
        mutation: Mutation,
        action: &str,
        text: Option<&str>,
        actor: &str,
        current: impl FnOnce(&mut Self) -> Result<Vec<Value>>,
    ) -> Result<Outcome> {
        self.attention_mutate_authorized(
            mutation,
            action,
            text,
            &crate::inbox::SourceAuthority::Native(actor),
            current,
        )
    }
    pub(crate) fn attention_mutate_authorized(
        &mut self,
        mutation: Mutation,
        action: &str,
        text: Option<&str>,
        authority: &crate::inbox::SourceAuthority<'_>,
        current: impl FnOnce(&mut Self) -> Result<Vec<Value>>,
    ) -> Result<Outcome> {
        mutation.validate(authority)?;
        let actor = authority.actor();
        if action == "save_decision"
            && text.is_none_or(|text| text.trim().is_empty() || text.len() > 65_536)
        {
            return Err(api::error(
                "attention_invalid_request",
                "Decision text must contain 1–65536 UTF-8 bytes",
            ));
        }
        ensure!(
            matches!(action, "save_decision" | "ack_seen"),
            "unsupported attention action"
        );
        self.mutation(|r| r.attention_mutate_inner(mutation, action, text, actor, current))
    }
    fn attention_mutate_inner(
        &mut self,
        mutation: Mutation,
        action: &str,
        text: Option<&str>,
        actor: &str,
        current: impl FnOnce(&mut Self) -> Result<Vec<Value>>,
    ) -> Result<Outcome> {
        let digest = mutation.digest(&self.state.brain_id, action, text)?;
        let external = mutation.source.external_key()?;
        if self.proposal_identity_reserved(&mutation.operation_id, &external)?
            || self.inbox_identity_reserved(&mutation.operation_id, &external)
            || self.plan_identity_reserved(&mutation.operation_id, &external)
        {
            return Err(api::error(
                "attention_identity_conflict",
                "Identity is reserved for an inbox capture",
            ));
        }
        let journal = &self.state.attention_journal;
        let original = if journal.operations.contains_key(&mutation.operation_id) {
            Some(mutation.operation_id.clone())
        } else {
            journal.aliases.get(&mutation.operation_id).cloned()
        }
        .or_else(|| {
            journal
                .operations
                .iter()
                .find(|(_, intent)| intent.external_key == external)
                .map(|(id, _)| id.clone())
        });
        if let Some(original) = original {
            let intent = &journal.operations[&original];
            if intent.outcome.receipt.request_sha256 != digest || intent.external_key != external {
                return Err(api::error("attention_identity_conflict","Operation or upstream identity already has different action, target or content"));
            }
            if mutation.operation_id != original
                && !self
                    .state
                    .attention_journal
                    .aliases
                    .contains_key(&mutation.operation_id)
            {
                if !self.attention_writable() {
                    return Err(api::error(
                        "attention_unsupported",
                        "New receipt aliases require original operational history",
                    ));
                }
                self.state
                    .attention_journal
                    .aliases
                    .insert(mutation.operation_id, original.clone());
                self.persist()?;
            }
            if self.state.attention_journal.operations[&original]
                .outcome
                .receipt
                .status
                != "committed"
            {
                self.flush_writes()?;
            }
            let mut outcome = self.state.attention_journal.operations[&original]
                .outcome
                .clone();
            if outcome.receipt.status != "committed" {
                return Err(api::error(
                    "attention_projection_pending",
                    "Original decision projection needs recovery",
                ));
            }
            outcome.receipt.replayed = true;
            // A committed action's authority was frozen at acceptance. Failure
            // to read today's sources must not turn its replay into a new action.
            outcome.item = current(self)
                .and_then(|raw| self.attention_materialize(raw))
                .ok()
                .and_then(|items| {
                    items.into_iter().find(|item| {
                        item.goal_id == mutation.target.goal_id
                            && item.attention_id == mutation.target.attention_id
                    })
                })
                .map(|item| self.attention_present(item, actor, &mutation.source.channel, true))
                .transpose()?;
            return Ok(outcome);
        }
        if !self.attention_writable() {
            return Err(api::error(
                "attention_unsupported",
                "Attention writes require managed source and original operational receipts",
            ));
        }
        let raw = current(self)?;
        let items = self.attention_materialize(raw)?;
        let item = items.iter().find(|item| {
            item.goal_id == mutation.target.goal_id
                && item.attention_id == mutation.target.attention_id
        });
        if item.is_none_or(|item| {
            item.revision != mutation.target.expected_revision
                || item.stage_id != mutation.target.stage_id
        }) {
            let fresh = item
                .cloned()
                .map(|item| self.attention_present(item, actor, &mutation.source.channel, true))
                .transpose()?;
            return Err(api::stale(fresh.as_ref()));
        }
        let item = item.unwrap();
        if !item.allowed_actions.iter().any(|allowed| allowed == action) {
            return Err(api::error(
                "attention_unsupported",
                "This attention item does not support the requested action",
            ));
        }
        self.remember_attention(std::slice::from_ref(item))?;
        let mut outcome = Outcome {
            decision_id: None,
            path: None,
            revision: None,
            received_at: None,
            acknowledged_at: None,
            receipt: Receipt {
                operation_id: mutation.operation_id.clone(),
                status: "committed".into(),
                request_sha256: digest,
                replayed: false,
            },
            item: None,
        };
        let source_operation_id = if action == "save_decision" {
            let decision_id = Uuid::new_v4().to_string();
            let received_at = now()?;
            let record = serde_json::json!({"schema":SCHEMA,"record_type":"decision","brain_id":self.state.brain_id,"id":decision_id,"goal_id":mutation.target.goal_id,"stage_id":mutation.target.stage_id,"attention_id":mutation.target.attention_id,"attention_revision":mutation.target.expected_revision,"received_at":received_at,"verification":"unverified","actor_id":actor,"source":mutation.source});
            let (revision, source_operation) =
                self.queue_create_only("decision", &decision_id, &record, text.unwrap())?;
            self.stage_proposal_candidate(
                crate::proposals::TriggerKind::Decision,
                &decision_id,
                Some(mutation.target.goal_id.clone()),
                &received_at,
                std::slice::from_ref(&source_operation),
            )?;
            outcome.path = Some(self.path("decision", &decision_id));
            outcome.decision_id = Some(decision_id);
            outcome.revision = Some(revision);
            outcome.received_at = Some(received_at);
            outcome.receipt.status = "pending".into();
            Some(source_operation)
        } else {
            let key = seen_key(item, actor, &mutation.source.channel)?;
            let timestamp = if let Some(timestamp) = self.state.attention_journal.seen.get(&key) {
                timestamp.clone()
            } else {
                let timestamp = now()?;
                self.state
                    .attention_journal
                    .seen
                    .insert(key, timestamp.clone());
                timestamp
            };
            outcome.acknowledged_at = Some(timestamp);
            None
        };
        let operation = mutation.operation_id.clone();
        self.state.attention_journal.operations.insert(
            operation.clone(),
            Intent {
                external_key: external,
                source_operation_id,
                mutation: mutation.clone(),
                action: action.into(),
                outcome,
            },
        );
        self.persist()?;
        #[cfg(test)]
        if std::mem::take(&mut self.interrupt_after_inbox_intent) {
            anyhow::bail!("injected crash after attention intent before source/response");
        }
        self.flush_writes()?;
        let mut outcome = self.state.attention_journal.operations[&operation]
            .outcome
            .clone();
        outcome.item =
            Some(self.attention_present(item.clone(), actor, &mutation.source.channel, true)?);
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests;
