//! Bounded, replayable runtime selection. Native updater hooks supply an already
//! staged runtime; no download, identity generation or vault writes happen here.
//! Progress lives in the revision-bound envelope (see `authority`): Stop and
//! Rollback use the same prepare / unlock / IPC / relock sequence as lifecycle
//! stops, and every phase commit is the exact next revision.
use super::{
    authority::StopToken, current_envelope, supervisor::ipc::Scope, Authority, Binding, Intent, Tx,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    pub version: String,
    pub digest: String,
    pub location: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Stop,
    Select,
    Start,
    Check,
    Rollback,
    Complete,
}
/// Pre-v2 `update.json` payload, read only by migration; the envelope owns update
/// state afterwards.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    pub binding: Binding,
    pub previous: Runtime,
    pub candidate: Runtime,
    pub intent: Intent,
    pub phase: Phase,
    pub rolled_back: bool,
}
pub trait Host {
    /// Signature/hash/version and original device identity; no effects.
    fn verify(&mut self, binding: &Binding, runtime: &Runtime) -> Result<()>;
    /// Verified generation of the live supervisor, or None when nothing is running
    /// or the platform has no authenticated IPC channel.
    fn supervisor_scope(&mut self, binding: &Binding) -> Result<Option<Scope>>;
    /// Native stop and reap, regardless of which runtime was last selected. Called
    /// under the instance lock, only when there is no authenticated IPC channel.
    fn stop(&mut self, binding: &Binding) -> Result<()>;
    /// Protocol-v2 Stop carrying `token`; the caller holds no lock. Confirm the
    /// supervisor AND its owned child exited before returning Ok.
    fn stop_supervisor(&mut self, binding: &Binding, token: &StopToken) -> Result<()>;
    /// Atomic durable selection. May be replayed after a lost successful reply.
    fn select(&mut self, binding: &Binding, runtime: &Runtime) -> Result<()>;
    /// Idempotent start, using the verified selection and existing identity.
    fn start(&mut self, binding: &Binding, runtime: &Runtime) -> Result<()>;
    /// Confirm actual REST version AND device identity, not merely process exit.
    fn healthy(&mut self, binding: &Binding, runtime: &Runtime) -> Result<()>;
}

/// Persist an update after verifying both runtimes. Needs an enrolled instance;
/// a retry of the pending update writes nothing and a different one is refused.
pub fn begin<A: Authority, H: Host>(
    authority: &mut A,
    host: &mut H,
    budget: Duration,
    previous: Runtime,
    candidate: Runtime,
) -> Result<()> {
    let mut tx = authority.begin(Instant::now() + budget)?;
    let saved =
        current_envelope(&mut tx)?.context("update requires explicit managed enrollment")?;
    let Some(next) = saved.begin_update(previous.clone(), candidate.clone())? else {
        return Ok(());
    };
    host.verify(saved.binding(), &previous)?;
    host.verify(saved.binding(), &candidate)?;
    tx.commit(next)
}

/// Perform one bounded phase. Host calls enforce hook deadlines; failure leaves
/// a durable phase to retry at next startup. Never loop or retry within a hook.
pub fn advance<A: Authority, H: Host>(
    authority: &mut A,
    host: &mut H,
    budget: Duration,
) -> Result<Phase> {
    let deadline = Instant::now() + budget;
    let mut tx = authority.begin(deadline)?;
    let Some(mut env) = current_envelope(&mut tx)? else {
        return Ok(Phase::Complete);
    };
    let Some(update) = env.update().cloned() else {
        return Ok(Phase::Complete);
    };
    if update.phase == Phase::Complete {
        return Ok(Phase::Complete);
    }
    let binding = env.binding().clone();
    // Disable/Remove supersedes the update: roll back, never restart.
    if env.intent() != Intent::Enabled && update.phase != Phase::Rollback {
        tx.commit(env.set_update_phase(Phase::Rollback, false)?)?;
        return Ok(Phase::Rollback);
    }
    let rollback = update.phase == Phase::Rollback;
    let mut phase = update.phase;
    if matches!(phase, Phase::Stop | Phase::Rollback) {
        match host.supervisor_scope(&binding)? {
            None => host.stop(&binding)?,
            Some(scope) => {
                // A stored operation for this generation is a retry; otherwise arm.
                let token = match env.stop() {
                    Some(op) if op.scope == scope => {
                        let token = StopToken {
                            journal_epoch: env.journal_epoch(),
                            operation: op.clone(),
                        };
                        env.authorize(&token, &scope)?;
                        token
                    }
                    _ => {
                        let (next, token) = if rollback {
                            env.arm_update_rollback(scope.clone())?
                        } else {
                            env.arm_update_stop(scope.clone())?
                        };
                        tx.commit(next)?;
                        token
                    }
                };
                drop(tx);
                host.stop_supervisor(&binding, &token)?;
                tx = authority.begin(deadline)?;
                let current =
                    current_envelope(&mut tx)?.context("journal disappeared during stop")?;
                // A stale updater (Enable, Disable, a newer operation) has no
                // authority to select, start or commit its old phase.
                current.authorize(&token, &scope)?;
                env = current.complete_stop(&token, &scope)?;
                tx.commit(env.clone())?;
                // UpdateStop already advanced to Select; rollback stays in Rollback.
                phase = env.update().context("update vanished")?.phase;
                if phase == Phase::Select {
                    return Ok(Phase::Select);
                }
            }
        }
    }
    let (next, rolled_back) = match phase {
        Phase::Stop => (Phase::Select, false),
        Phase::Select => {
            if host.verify(&binding, &update.candidate).is_err() {
                (Phase::Rollback, false)
            } else {
                host.select(&binding, &update.candidate)?;
                (Phase::Start, false)
            }
        }
        Phase::Start => {
            if host
                .verify(&binding, &update.candidate)
                .and_then(|()| host.start(&binding, &update.candidate))
                .is_err()
            {
                (Phase::Rollback, false)
            } else {
                (Phase::Check, false)
            }
        }
        Phase::Check => {
            if host.healthy(&binding, &update.candidate).is_ok() {
                (Phase::Complete, false)
            } else {
                (Phase::Rollback, false)
            }
        }
        Phase::Rollback => {
            host.verify(&binding, &update.previous)?;
            host.select(&binding, &update.previous)?;
            if env.intent() == Intent::Enabled {
                host.start(&binding, &update.previous)?;
                host.healthy(&binding, &update.previous)?;
            }
            (Phase::Complete, true)
        }
        Phase::Complete => unreachable!(),
    };
    tx.commit(env.set_update_phase(next, rolled_back)?)?;
    Ok(next)
}

#[cfg(test)]
mod tests;
