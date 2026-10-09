//! Revision-bound lifecycle and update authority (design: docs/sync-sidecar-stop-operations.md).
//! Pure state transitions: no I/O, locking or native effects. A store must persist
//! each returned envelope atomically before anything acts on it; a failed flush
//! means the previous envelope stays authoritative and no effect may follow.
use super::{
    supervisor::ipc::Scope,
    update::{Phase, Runtime, Update},
    Binding, Intent, Journal,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const SCHEMA: u32 = 2;
const EXHAUSTED: &str = "revision exhausted; explicit operator repair required";

/// What a store holds for one instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stored {
    Absent,
    /// Pre-v2 `{binding, intent}` plus optional `update.json`; inert until migrated.
    Legacy {
        journal: Journal,
        update: Option<Update>,
    },
    Current(Envelope),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reason {
    Disable,
    Remove,
    UpdateStop(Uuid),
    UpdateRollback(Uuid),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopOperation {
    pub operation_id: Uuid,
    pub authorized_revision: u64,
    pub scope: Scope,
    pub reason: Reason,
}
/// What Stop carries on the wire. Not a PID or discovery record and never a
/// replacement for native peer authentication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopToken {
    pub journal_epoch: Uuid,
    pub operation: StopOperation,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateState {
    pub update_id: Uuid,
    pub previous: Runtime,
    pub candidate: Runtime,
    pub phase: Phase,
    pub rolled_back: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    schema: u32,
    journal_epoch: Uuid,
    revision: u64,
    binding: Binding,
    intent: Intent,
    update: Option<UpdateState>,
    stop: Option<StopOperation>,
}

impl Envelope {
    /// Explicit Enable of an instance that has no journal.
    pub fn first(binding: Binding) -> Self {
        Self {
            schema: SCHEMA,
            journal_epoch: Uuid::new_v4(),
            revision: 1,
            binding,
            intent: Intent::Enabled,
            update: None,
            stop: None,
        }
    }
    pub fn binding(&self) -> &Binding {
        &self.binding
    }
    pub fn intent(&self) -> Intent {
        self.intent
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn journal_epoch(&self) -> Uuid {
        self.journal_epoch
    }
    pub fn update(&self) -> Option<&UpdateState> {
        self.update.as_ref()
    }
    pub fn stop(&self) -> Option<&StopOperation> {
        self.stop.as_ref()
    }

    pub fn from_slice(data: &[u8]) -> Result<Self> {
        let envelope: Self =
            serde_json::from_slice(data).context("invalid sidecar journal; recovery required")?;
        envelope.validate()?;
        Ok(envelope)
    }
    pub fn to_vec(&self) -> Result<Vec<u8>> {
        self.validate()?;
        Ok(serde_json::to_vec(self)?)
    }
    fn validate(&self) -> Result<()> {
        ensure!(self.schema == SCHEMA, "unknown sidecar journal schema");
        ensure!(!self.journal_epoch.is_nil(), "nil journal epoch");
        ensure!(self.revision >= 1, "invalid journal revision");
        if let Some(update) = &self.update {
            ensure!(!update.update_id.is_nil(), "nil update id");
        }
        if let Some(op) = &self.stop {
            ensure!(!op.operation_id.is_nil(), "nil operation id");
            ensure!(
                op.authorized_revision == self.revision,
                "stop operation is not at the current revision"
            );
            self.check_scope(&op.scope)?;
            ensure!(
                self.reason_matches(&op.reason),
                "stop reason does not match journal state"
            );
        }
        Ok(())
    }
    fn check_scope(&self, scope: &Scope) -> Result<()> {
        ensure!(
            scope.installation == self.binding.installation
                && scope.instance == self.binding.instance
                && !scope.generation.is_nil(),
            "stop scope does not match binding"
        );
        Ok(())
    }
    fn reason_matches(&self, reason: &Reason) -> bool {
        let update = |id: &Uuid, phase: Phase| {
            self.update
                .as_ref()
                .is_some_and(|u| u.update_id == *id && u.phase == phase)
        };
        match reason {
            Reason::Disable => self.intent == Intent::Disabled,
            Reason::Remove => self.intent == Intent::Removed,
            Reason::UpdateStop(id) => self.intent == Intent::Enabled && update(id, Phase::Stop),
            Reason::UpdateRollback(id) => update(id, Phase::Rollback),
        }
    }

    /// Every authoritative mutation starts here: checked increment, old stop cleared.
    fn next(&self) -> Result<Self> {
        let mut next = self.clone();
        next.revision = self.revision.checked_add(1).context(EXHAUSTED)?;
        next.stop = None;
        Ok(next)
    }
    fn arm(mut next: Self, scope: Scope, reason: Reason) -> Result<(Self, StopToken)> {
        let operation = StopOperation {
            operation_id: Uuid::new_v4(),
            authorized_revision: next.revision,
            scope,
            reason,
        };
        next.stop = Some(operation.clone());
        next.validate()?;
        let token = StopToken {
            journal_epoch: next.journal_epoch,
            operation,
        };
        Ok((next, token))
    }

    /// Removed is terminal. Re-enable invalidates every outstanding operation.
    pub fn enable(&self) -> Result<Self> {
        ensure!(
            self.intent != Intent::Removed,
            "removed instance cannot be enabled"
        );
        let mut next = self.next()?;
        next.intent = Intent::Enabled;
        Ok(next)
    }
    /// New revision with the intent recorded but no stop armed, for when no
    /// authenticated supervisor generation exists. Removed stays Removed.
    pub fn set_intent(&self, intent: Intent) -> Result<Self> {
        ensure!(intent != Intent::Enabled, "use enable");
        let mut next = self.next()?;
        if self.intent != Intent::Removed {
            next.intent = intent;
        }
        Ok(next)
    }
    /// A new action: always a new revision and operation, even when the intent
    /// repeats. On a Removed instance this arms Remove, never un-removes.
    pub fn disable(&self, scope: Scope) -> Result<(Self, StopToken)> {
        let mut next = self.next()?;
        let reason = if self.intent == Intent::Removed {
            Reason::Remove
        } else {
            next.intent = Intent::Disabled;
            Reason::Disable
        };
        Self::arm(next, scope, reason)
    }
    pub fn remove(&self, scope: Scope) -> Result<(Self, StopToken)> {
        let mut next = self.next()?;
        next.intent = Intent::Removed;
        Self::arm(next, scope, Reason::Remove)
    }
    pub fn arm_update_stop(&self, scope: Scope) -> Result<(Self, StopToken)> {
        let id = self.pending_update(Phase::Stop)?;
        ensure!(self.intent == Intent::Enabled, "update stop needs Enabled");
        Self::arm(self.next()?, scope, Reason::UpdateStop(id))
    }
    /// Deliberately has no Enabled guard, unlike `arm_update_stop`: Disable/Remove
    /// during an update drive it to Rollback, and that stop must still be
    /// authorizable. Restarting the previous runtime is gated on Enabled elsewhere.
    pub fn arm_update_rollback(&self, scope: Scope) -> Result<(Self, StopToken)> {
        let id = self.pending_update(Phase::Rollback)?;
        Self::arm(self.next()?, scope, Reason::UpdateRollback(id))
    }
    fn pending_update(&self, phase: Phase) -> Result<Uuid> {
        let update = self.update.as_ref().context("no update in progress")?;
        ensure!(
            update.phase == phase,
            "update is not in the requested phase"
        );
        Ok(update.update_id)
    }
    /// Start an update. None when the same update is already pending (a retry
    /// writes nothing); a pending one with a different payload is refused.
    pub fn begin_update(&self, previous: Runtime, candidate: Runtime) -> Result<Option<Self>> {
        if let Some(saved) = &self.update {
            if saved.phase != Phase::Complete {
                ensure!(
                    saved.previous == previous && saved.candidate == candidate,
                    "another update is pending"
                );
                return Ok(None);
            }
            let selected = if saved.rolled_back {
                &saved.previous
            } else {
                &saved.candidate
            };
            ensure!(
                *selected == previous,
                "previous runtime differs from committed selection"
            );
        }
        ensure!(previous != candidate, "runtime is unchanged");
        ensure!(
            self.intent != Intent::Removed,
            "removed instance cannot update"
        );
        let mut next = self.next()?;
        next.update = Some(UpdateState {
            update_id: Uuid::new_v4(),
            previous,
            candidate,
            phase: Phase::Stop,
            rolled_back: false,
        });
        Ok(Some(next))
    }
    /// Record update progress in a new revision. Any outstanding stop dies with
    /// the old revision, so a stale updater cannot commit its old phase.
    pub fn set_update_phase(&self, phase: Phase, rolled_back: bool) -> Result<Self> {
        let mut next = self.next()?;
        let update = next.update.as_mut().context("no update in progress")?;
        update.phase = phase;
        update.rolled_back = rolled_back;
        Ok(next)
    }
    /// Recovery after a crash, a fresh supervisor generation or migration: the
    /// caller has verified native ownership of `scope`. The reason is derived
    /// from authoritative state, so terminal intent is preserved.
    pub fn arm_recovery(&self, scope: Scope) -> Result<(Self, StopToken)> {
        let reason = match (self.intent, &self.update) {
            (Intent::Disabled, _) => Reason::Disable,
            (Intent::Removed, _) => Reason::Remove,
            (Intent::Enabled, Some(u)) if u.phase == Phase::Stop => Reason::UpdateStop(u.update_id),
            (Intent::Enabled, Some(u)) if u.phase == Phase::Rollback => {
                Reason::UpdateRollback(u.update_id)
            }
            _ => anyhow::bail!("nothing to stop"),
        };
        Self::arm(self.next()?, scope, reason)
    }

    /// Server and controller both call this under the instance lock. A retry is
    /// exactly the stored token at the current revision; nothing else qualifies.
    pub fn authorize(&self, token: &StopToken, scope: &Scope) -> Result<()> {
        ensure!(
            token.journal_epoch == self.journal_epoch,
            "stop token is from another journal epoch"
        );
        let stored = self.stop.as_ref().context("no stop operation authorized")?;
        ensure!(
            *stored == token.operation
                && stored.authorized_revision == self.revision
                && stored.scope == *scope,
            "stale or mismatched stop operation"
        );
        self.validate()
    }
    /// Consume a confirmed stop in a new revision. UpdateStop advances to Select;
    /// other reasons leave their phase for the next authorized step.
    pub fn complete_stop(&self, token: &StopToken, scope: &Scope) -> Result<Self> {
        self.authorize(token, scope)?;
        let mut next = self.next()?;
        if let (Reason::UpdateStop(_), Some(update)) = (&token.operation.reason, &mut next.update) {
            update.phase = Phase::Select;
        }
        Ok(next)
    }

    /// A store accepts only the exact successor of what it holds: a fresh Enable
    /// when absent, the migration shape for a legacy journal, otherwise the next
    /// revision of the same epoch and binding.
    pub fn check_successor(stored: &Stored, next: &Envelope) -> Result<()> {
        match stored {
            Stored::Absent => ensure!(
                next.revision == 1
                    && next.intent == Intent::Enabled
                    && next.update.is_none()
                    && next.stop.is_none(),
                "first journal must be a fresh Enable"
            ),
            Stored::Legacy { journal, update } => ensure!(
                next.revision == 1
                    && next.binding == journal.binding
                    && next.intent == journal.intent
                    && next.stop.is_none()
                    && match (update, &next.update) {
                        (None, None) => true,
                        (Some(u), Some(n)) =>
                            u.previous == n.previous
                                && u.candidate == n.candidate
                                && u.phase == n.phase
                                && u.rolled_back == n.rolled_back,
                        _ => false,
                    },
                "legacy journal may only be migrated"
            ),
            Stored::Current(current) => ensure!(
                next.journal_epoch == current.journal_epoch
                    && current.revision.checked_add(1) == Some(next.revision)
                    && next.binding == current.binding,
                "stale or non-sequential journal revision"
            ),
        }
        Ok(())
    }

    /// Legacy `{binding, intent}` plus optional `update.json` into one envelope.
    /// Lifecycle intent wins; the update's stale intent is ignored. No stop is
    /// fabricated: the caller arms a verified operation in a later revision.
    pub fn migrate(journal: Journal, update: Option<Update>) -> Result<Self> {
        let update = update
            .map(|u| {
                ensure!(
                    u.binding == journal.binding,
                    "update binding differs from lifecycle"
                );
                Ok(UpdateState {
                    update_id: Uuid::new_v4(),
                    previous: u.previous,
                    candidate: u.candidate,
                    phase: u.phase,
                    rolled_back: u.rolled_back,
                })
            })
            .transpose()?;
        let envelope = Self {
            schema: SCHEMA,
            journal_epoch: Uuid::new_v4(),
            revision: 1,
            binding: journal.binding,
            intent: journal.intent,
            update,
            stop: None,
        };
        envelope.validate()?;
        Ok(envelope)
    }
    /// Explicit operator repair after revision exhaustion, with the supervisor
    /// verified stopped. A new epoch makes the counter reset safe: every token
    /// from the old epoch is refused.
    pub fn repair_epoch(&self) -> Self {
        Self {
            journal_epoch: Uuid::new_v4(),
            revision: 1,
            stop: None,
            ..self.clone()
        }
    }
}

#[cfg(test)]
mod tests;
