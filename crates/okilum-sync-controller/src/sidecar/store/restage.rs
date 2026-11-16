//! Crash-replayable Windows helper replacement. The marker is a write-ahead
//! plan of exact journal successors, not a revision range or permission inferred
//! from Disabled. Any unrelated journal action cancels it. Content alone cannot
//! tell every user Disable apart: with no live helper, `set_intent(Disabled)` is
//! byte-identical to a planned step. So user lifecycle actions first mark the
//! marker cancelled under the state lock, and every planned commit rechecks it.
use super::{StateDir, Store};
use crate::sidecar::{
    authority::{Envelope, StopToken},
    Binding, Intent, Platform, Registration,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Instant;

const MARKER: &str = "restage.json";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema: u32,
    from: Envelope,
    stopping: Envelope,
    stopped: Envelope,
    rebound: Envelope,
    enabled: Envelope,
    hash: String,
    /// Set by an explicit user lifecycle action before it commits. Never cleared.
    #[serde(default)]
    cancelled: bool,
}

impl Plan {
    fn validate(&self) -> Result<()> {
        ensure!(self.schema == 1, "unknown helper restage schema");
        ensure!(
            self.from.intent() == Intent::Enabled
                && self.from.stop().is_none()
                && self.from.update().is_none(),
            "invalid helper restage origin"
        );
        for step in [
            &self.from,
            &self.stopping,
            &self.stopped,
            &self.rebound,
            &self.enabled,
        ] {
            Envelope::from_slice(&step.to_vec()?)?;
        }
        Envelope::check_successor(
            &crate::sidecar::authority::Stored::Current(self.from.clone()),
            &self.stopping,
        )?;
        let expected = if let Some(operation) = self.stopping.stop() {
            ensure!(
                self.stopping.binding() == self.from.binding()
                    && self.stopping.intent() == Intent::Disabled
                    && self.stopping.update().is_none(),
                "invalid helper restage Stop"
            );
            let token = StopToken {
                journal_epoch: self.stopping.journal_epoch(),
                operation: operation.clone(),
            };
            self.stopping.complete_stop(&token, &operation.scope)?
        } else {
            ensure!(
                self.stopping == self.from.set_intent(Intent::Disabled)?,
                "invalid helper restage intent"
            );
            self.stopping.clone()
        };
        ensure!(
            self.stopped == expected
                && self.rebound
                    == self
                        .stopped
                        .restage_supervisor(self.rebound.binding().supervisor.clone())?
                && self.enabled == self.rebound.enable()?,
            "invalid helper restage successors"
        );
        Ok(())
    }
}

pub(crate) trait Host {
    type Native: Platform;
    /// Load retained identity/runtime, but bind the guard to the journal's helper
    /// (preparation can be ahead of the journal after a crash).
    fn platform(&mut self, binding: &Binding) -> Result<Self::Native>;
    /// Verified, idempotent staging and preparation publication. Never generates
    /// a device identity, changes configuration, peers or runtime selection.
    fn select_helper(&mut self, binding: &Binding, hash: &str) -> Result<()>;
    fn ready(&mut self, binding: &Binding, deadline: Instant) -> Result<()>;
    /// Only after new readiness. Deletion failure retains the replay marker.
    fn cleanup(&mut self, old: &Binding) -> Result<()>;
}

impl<D: StateDir> Store<D> {
    pub(crate) fn helper_restage_pending(&self) -> Result<bool> {
        Ok(self.dir.read(MARKER)?.is_some())
    }

    /// Called by explicit Disable/Connect before its own journal commit. Revokes
    /// the plan's permission to reconnect, even when the user's next envelope is
    /// identical to a planned step. Settling (preparation repair, removal) is left
    /// to the next refresh, which never re-enables a cancelled plan.
    pub(crate) fn cancel_helper_restage(&self, deadline: Instant) -> Result<()> {
        let _tx = self.begin(deadline)?;
        let Some(bytes) = self.dir.read(MARKER)? else {
            return Ok(());
        };
        let mut plan: Plan = serde_json::from_slice(&bytes)?;
        if !plan.cancelled {
            plan.cancelled = true;
            self.dir.write(MARKER, &serde_json::to_vec(&plan)?)?;
        }
        Ok(())
    }

    fn plan(&self) -> Result<Option<Plan>> {
        self.dir
            .read(MARKER)?
            .map(|bytes| {
                let plan: Plan = serde_json::from_slice(&bytes)?;
                plan.validate()?;
                Ok(plan)
            })
            .transpose()
    }

    /// Under the held state lock: the journal is still `expected` and the plan
    /// was neither cancelled nor replaced since this step was chosen.
    fn still_planned(&self, tx: &super::Transaction<D>, expected: &Envelope) -> Result<()> {
        ensure!(
            tx.current()? == Some(expected),
            "Sync changed during helper restage; retry next launch"
        );
        ensure!(
            self.plan()?.is_some_and(|plan| !plan.cancelled),
            "helper restage cancelled by a user lifecycle action"
        );
        Ok(())
    }

    pub(crate) fn restage_helper(
        &self,
        host: &mut impl Host,
        target: String,
        hash: String,
        deadline: Instant,
    ) -> Result<bool> {
        // Caller serializes refresh attempts with prepare's separate operation
        // lock. User lifecycle actions remain free to cancel during IPC waits.
        let plan = {
            let tx = self.begin(deadline)?;
            if let Some(plan) = self.plan()? {
                plan
            } else {
                let Some(from) = tx.current()? else {
                    return Ok(false);
                };
                if from.intent() != Intent::Enabled
                    || from.stop().is_some()
                    || from.update().is_some()
                    || from.binding().supervisor == target
                {
                    return Ok(false);
                }
                // Prepare native guard outside the state transaction on Windows.
                let from = from.clone();
                drop(tx);
                let mut native = host.platform(from.binding())?;
                let scope = native.supervisor_scope(from.binding())?;
                let tx = self.begin(deadline)?;
                ensure!(
                    tx.current()? == Some(&from),
                    "Sync changed before helper restage"
                );
                let (stopping, stopped) = if let Some(scope) = scope {
                    let (stopping, token) = from.disable(scope.clone())?;
                    let stopped = stopping.complete_stop(&token, &scope)?;
                    (stopping, stopped)
                } else {
                    ensure!(
                        native.inspect(from.binding())? != Registration::Running,
                        "running helper has no authenticated Stop scope"
                    );
                    let stopped = from.set_intent(Intent::Disabled)?;
                    (stopped.clone(), stopped)
                };
                let rebound = stopped.restage_supervisor(target)?;
                let enabled = rebound.enable()?;
                let plan = Plan {
                    schema: 1,
                    from,
                    stopping,
                    stopped,
                    rebound,
                    enabled,
                    hash,
                    cancelled: false,
                };
                self.dir.write(MARKER, &serde_json::to_vec(&plan)?)?;
                plan
            }
        };
        loop {
            ensure!(
                okilum_sync::NetworkAccess::current().allowed(),
                "Sync access revoked during helper refresh"
            );
            ensure!(
                Instant::now() < deadline,
                "helper restage deadline elapsed; retry next launch"
            );
            let (current, cancelled) = {
                let tx = self.begin(deadline)?;
                let current = tx
                    .current()?
                    .context("Sync journal disappeared")?
                    .clone();
                let cancelled = self.plan()?.is_none_or(|plan| plan.cancelled);
                (current, cancelled)
            };
            let known = [
                &plan.from,
                &plan.stopping,
                &plan.stopped,
                &plan.rebound,
                &plan.enabled,
            ]
            .contains(&&current);
            if cancelled || !known {
                // A user's newer intent wins even when it repeats Disabled.
                // Repair a preparation/journal publication gap before forgetting
                // the plan; do not undo that intent or touch a running helper.
                let hash = std::path::Path::new(&current.binding().supervisor)
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                    .context("retained helper hash missing")?;
                host.select_helper(current.binding(), hash)?;
                let tx = self.begin(deadline)?;
                ensure!(
                    tx.current()? == Some(&current),
                    "Sync changed while cancelling restage"
                );
                self.dir.remove(MARKER)?;
                return Ok(false);
            }
            if current == plan.from {
                let mut tx = self.begin(deadline)?;
                self.still_planned(&tx, &current)?;
                tx.commit(plan.stopping.clone())?;
                continue;
            }
            if current == plan.stopping && current != plan.stopped {
                let mut native = host.platform(current.binding())?;
                let token = StopToken {
                    journal_epoch: current.journal_epoch(),
                    operation: current.stop().context("helper Stop missing")?.clone(),
                };
                match native.inspect(current.binding())? {
                    Registration::Running => {
                        ensure!(
                            native.supervisor_scope(current.binding())?
                                == Some(token.operation.scope.clone()),
                            "helper generation changed; retry after lifecycle recovery"
                        );
                        // Same authenticated Stop path as Controller::disable:
                        // no state lock while the server takes its Stop lease.
                        native.stop_supervisor(current.binding(), &token)?;
                    }
                    Registration::Stopped | Registration::Absent => (),
                    Registration::ApprovalRequired => {
                        anyhow::bail!("helper task requires approval")
                    }
                }
                let mut tx = self.begin(deadline)?;
                self.still_planned(&tx, &current)?;
                // Guard discovery must confirm absence, even after a crash that
                // lost the authenticated Stop reply. No scheduler/PID kill.
                ensure!(
                    native.inspect(current.binding())? != Registration::Running,
                    "old helper has not exited"
                );
                native.unregister(current.binding())?;
                tx.commit(plan.stopped.clone())?;
                continue;
            }
            if current == plan.stopped {
                let mut native = host.platform(current.binding())?;
                {
                    let tx = self.begin(deadline)?;
                    self.still_planned(&tx, &current)?;
                    ensure!(
                        native.inspect(current.binding())? != Registration::Running,
                        "old helper still running; retry next launch"
                    );
                    native.unregister(current.binding())?;
                }
                host.select_helper(plan.rebound.binding(), &plan.hash)?;
                let mut tx = self.begin(deadline)?;
                self.still_planned(&tx, &current)?;
                tx.commit(plan.rebound.clone())?;
                continue;
            }
            if current == plan.rebound {
                // Idempotently refresh prepared-identity after a partial publish.
                host.select_helper(current.binding(), &plan.hash)?;
                let mut native = host.platform(current.binding())?;
                let mut tx = self.begin(deadline)?;
                self.still_planned(&tx, &current)?;
                native.verify_payload(current.binding())?;
                tx.commit(plan.enabled.clone())?;
                continue;
            }
            let mut native = host.platform(current.binding())?;
            {
                let tx = self.begin(deadline)?;
                self.still_planned(&tx, &plan.enabled)?;
                native.verify_payload(current.binding())?;
                if native.inspect(current.binding())? == Registration::Absent {
                    native.register(current.binding())?;
                }
                if native.inspect(current.binding())? == Registration::Stopped {
                    native.start(current.binding())?;
                }
            }
            host.ready(current.binding(), deadline)?;
            let tx = self.begin(deadline)?;
            self.still_planned(&tx, &plan.enabled)?;
            host.cleanup(plan.from.binding())?;
            self.dir.remove(MARKER)?;
            return Ok(true);
        }
    }
}

#[cfg(test)]
mod tests;
