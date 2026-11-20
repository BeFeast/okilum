//! Crash-replayable Windows helper replacement. The marker is a write-ahead
//! plan of exact journal successors, not a revision range or permission inferred
//! from Disabled. Any unrelated journal action cancels it. Content alone cannot
//! tell every user Disable apart: with no live helper, `set_intent(Disabled)` is
//! byte-identical to a planned step. So user lifecycle actions first mark the
//! marker cancelled under the state lock, and every planned commit rechecks it.
//!
//! The plan never strands Sync in its own Disabled steps. The logon trigger or
//! RestartOnFailure may start the old helper again before its task is removed,
//! and its generation may change after a Stop was armed. Resume then re-arms:
//! it takes the live helper's authenticated scope and rewrites the plan from
//! the current planned step (`origin` keeps the Enabled state it started from).
//! A newer installed helper re-targets the steps that are not yet committed.
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
    /// The Enabled journal this replacement started from, once `from` is a
    /// re-armed planned Disabled step. None while `from` is that origin.
    #[serde(default)]
    origin: Option<Envelope>,
    /// Set by an explicit user lifecycle action before it commits. Never cleared.
    #[serde(default)]
    cancelled: bool,
}

impl Plan {
    fn validate(&self) -> Result<()> {
        ensure!(self.schema == 1, "unknown helper restage schema");
        let origin = self.origin.as_ref().unwrap_or(&self.from);
        ensure!(
            origin.intent() == Intent::Enabled
                && origin.stop().is_none()
                && origin.update().is_none(),
            "invalid helper restage origin"
        );
        if self.origin.is_some() {
            ensure!(
                self.from.intent() == Intent::Disabled
                    && self.from.update().is_none()
                    && self.from.journal_epoch() == origin.journal_epoch()
                    && self.from.revision() > origin.revision()
                    && self.from.binding() == origin.binding(),
                "invalid re-armed helper restage"
            );
        }
        for step in [
            origin,
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
    /// Wait for the started helper through the platform that started it. A new
    /// platform reads the runtime selection under the exclusive state lock, and
    /// the starting helper's managed-launch reads pin that directory without
    /// waiting, so they fail with a sharing violation (#3069).
    fn ready(
        &mut self,
        native: &mut Self::Native,
        binding: &Binding,
        deadline: Instant,
    ) -> Result<()>;
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

    /// The old helper runs again at a planned Disabled step (scheduler restart)
    /// or under another generation than the armed Stop. Rewrite the plan from
    /// `current` with that live, authenticated scope. Same target and origin.
    fn rearm(
        &self,
        plan: &Plan,
        current: &Envelope,
        scope: Option<crate::sidecar::supervisor::ipc::Scope>,
        deadline: Instant,
    ) -> Result<Plan> {
        let (stopping, stopped) = match scope {
            Some(scope) => {
                let (stopping, token) = current.arm_recovery(scope.clone())?;
                let stopped = stopping.complete_stop(&token, &scope)?;
                (stopping, stopped)
            }
            None => {
                let stopped = current.set_intent(Intent::Disabled)?;
                (stopped.clone(), stopped)
            }
        };
        let rebound = stopped.restage_supervisor(plan.rebound.binding().supervisor.clone())?;
        let next = Plan {
            schema: 1,
            origin: Some(plan.origin.clone().unwrap_or_else(|| plan.from.clone())),
            from: current.clone(),
            stopping,
            stopped,
            enabled: rebound.enable()?,
            rebound,
            hash: plan.hash.clone(),
            cancelled: false,
        };
        next.validate()?;
        let tx = self.begin(deadline)?;
        self.still_planned(&tx, current)?;
        self.dir.write(MARKER, &serde_json::to_vec(&next)?)?;
        Ok(next)
    }

    /// Another update installed a newer helper before the rebind committed.
    /// Staging the old hash would fail forever; aim the uncommitted steps at it.
    fn retarget(
        &self,
        plan: &Plan,
        current: &Envelope,
        target: &str,
        hash: &str,
        deadline: Instant,
    ) -> Result<Plan> {
        let rebound = plan.stopped.restage_supervisor(target.into())?;
        let next = Plan {
            enabled: rebound.enable()?,
            rebound,
            hash: hash.into(),
            ..plan.clone()
        };
        next.validate()?;
        let tx = self.begin(deadline)?;
        self.still_planned(&tx, current)?;
        self.dir.write(MARKER, &serde_json::to_vec(&next)?)?;
        Ok(next)
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
        let mut plan = {
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
                let rebound = stopped.restage_supervisor(target.clone())?;
                let enabled = rebound.enable()?;
                let plan = Plan {
                    schema: 1,
                    origin: None,
                    from,
                    stopping,
                    stopped,
                    rebound,
                    enabled,
                    hash: hash.clone(),
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
                let current = tx.current()?.context("Sync journal disappeared")?.clone();
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
            if plan.hash != hash && [&plan.from, &plan.stopping, &plan.stopped].contains(&&current)
            {
                plan = self.retarget(&plan, &current, &target, &hash, deadline)?;
                continue;
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
                    Registration::Running => match native.supervisor_scope(current.binding())? {
                        // Same authenticated Stop path as Controller::disable:
                        // no state lock while the server takes its Stop lease.
                        Some(scope) if scope == token.operation.scope => {
                            native.stop_supervisor(current.binding(), &token)?
                        }
                        // Restarted by the scheduler: a new generation, so a new Stop.
                        Some(scope) => {
                            plan = self.rearm(&plan, &current, Some(scope), deadline)?;
                            continue;
                        }
                        None => {
                            starting();
                            continue;
                        }
                    },
                    Registration::Stopped | Registration::Absent => (),
                    Registration::ApprovalRequired => {
                        anyhow::bail!("helper task requires approval")
                    }
                }
                let mut tx = self.begin(deadline)?;
                self.still_planned(&tx, &current)?;
                // Guard discovery must confirm absence, even after a crash that
                // lost the authenticated Stop reply. No scheduler/PID kill. If
                // the scheduler started it again, the next pass re-arms.
                if native.inspect(current.binding())? == Registration::Running {
                    continue;
                }
                native.unregister(current.binding())?;
                tx.commit(plan.stopped.clone())?;
                continue;
            }
            if current == plan.stopped {
                let mut native = host.platform(current.binding())?;
                if native.inspect(current.binding())? == Registration::Running {
                    // Started again by the logon trigger or RestartOnFailure
                    // before its task was removed: re-acquire Stop authority.
                    match native.supervisor_scope(current.binding())? {
                        Some(scope) => plan = self.rearm(&plan, &current, Some(scope), deadline)?,
                        None => starting(),
                    }
                    continue;
                }
                {
                    let tx = self.begin(deadline)?;
                    self.still_planned(&tx, &current)?;
                    if native.inspect(current.binding())? == Registration::Running {
                        continue;
                    }
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
            let stopped = {
                let tx = self.begin(deadline)?;
                self.still_planned(&tx, &plan.enabled)?;
                native.verify_payload(current.binding())?;
                if native.inspect(current.binding())? == Registration::Absent {
                    native.register(current.binding())?;
                }
                native.inspect(current.binding())? == Registration::Stopped
            };
            // Start only after the state lock is released (#3069): the
            // launched helper opens the state directory (its working directory
            // and startup reads) possibly before Task Scheduler returns, and the
            // exclusive transaction refuses every other open. A Disable that
            // commits in between still wins: the helper re-reads the journal
            // under the lock and stays idle unless it is Enabled, and readiness
            // rechecks the intent before this plan can finish.
            if stopped {
                native.start(current.binding())?;
            }
            host.ready(&mut native, current.binding(), deadline)?;
            let tx = self.begin(deadline)?;
            self.still_planned(&tx, &plan.enabled)?;
            // An update back to the retained image keeps its directory.
            if plan.from.binding().supervisor != plan.enabled.binding().supervisor {
                host.cleanup(plan.from.binding())?;
            }
            self.dir.remove(MARKER)?;
            return Ok(true);
        }
    }
}

/// A helper started by the scheduler has no authenticated endpoint yet. Wait
/// within the caller's deadline; never stop it any other way.
fn starting() {
    std::thread::sleep(std::time::Duration::from_millis(100));
}

#[cfg(test)]
mod tests;
