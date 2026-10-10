//! The supervisor's generation hint (design: docs/sync-sidecar-discovery.md). It
//! says only which generation to look for and when that supervisor started; it is
//! a claim, verified against OS objects before use, and a missing, malformed,
//! stale or unconnectable hint means "no live supervisor". It is never part of the
//! lifecycle envelope, carries no revision, and its writer never touches
//! `sidecar.json`.
use super::{StateDir, Store};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Instant;
use uuid::Uuid;

pub(crate) const HINT: &str = "endpoint.json";
const SCHEMA: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hint {
    schema: u32,
    generation: Uuid,
    /// The supervisor's own process start time in the platform's native unit, so the
    /// verifier can reject a reused PID. Opaque here; compared only for equality.
    started: u64,
}
impl Hint {
    pub fn new(generation: Uuid, started: u64) -> Result<Self> {
        let hint = Self {
            schema: SCHEMA,
            generation,
            started,
        };
        hint.validate()?;
        Ok(hint)
    }
    pub fn generation(&self) -> Uuid {
        self.generation
    }
    pub fn started(&self) -> u64 {
        self.started
    }
    fn validate(&self) -> Result<()> {
        ensure!(self.schema == SCHEMA, "unknown endpoint hint schema");
        ensure!(!self.generation.is_nil(), "nil endpoint generation");
        ensure!(self.started > 0, "missing supervisor start time");
        Ok(())
    }
    fn from_slice(data: &[u8]) -> Result<Self> {
        let hint: Self = serde_json::from_slice(data).context("invalid endpoint hint")?;
        hint.validate()?;
        Ok(hint)
    }
}

impl<D: StateDir> Store<D> {
    /// Called by the supervisor after it created its endpoint. Takes the instance
    /// lock for the write only, with the exchange's absolute deadline.
    pub fn publish_hint(&self, deadline: Instant, hint: &Hint) -> Result<()> {
        hint.validate()?;
        let _lock = self.dir.lock(deadline)?;
        self.dir.write(HINT, &serde_json::to_vec(hint)?)
    }
    /// None when no supervisor published one. An unreadable or malformed hint is an
    /// error the caller treats as "no live supervisor", never as authority.
    pub fn read_hint(&self, deadline: Instant) -> Result<Option<Hint>> {
        let _lock = self.dir.lock(deadline)?;
        self.dir
            .read(HINT)?
            .map(|data| Hint::from_slice(&data))
            .transpose()
    }
    /// Read without the instance lock, for callers that already hold a transaction (the
    /// controller discovers the supervisor while it holds its own). Safe because the
    /// hint is replaced atomically; a malformed hint is an error, never authority.
    pub fn peek_hint(&self) -> Result<Option<Hint>> {
        self.dir
            .read(HINT)?
            .map(|data| Hint::from_slice(&data))
            .transpose()
    }
    /// Clean supervisor exit. Idempotent.
    pub fn clear_hint(&self, deadline: Instant) -> Result<()> {
        let _lock = self.dir.lock(deadline)?;
        self.dir.remove(HINT)
    }
}
