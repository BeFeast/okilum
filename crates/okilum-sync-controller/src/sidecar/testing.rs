//! In-memory `Authority` with a real lock flag, shared by the sidecar tests.
use super::{
    authority::{Envelope, Stored},
    Authority, Tx,
};
use anyhow::{ensure, Result};
use std::{cell::RefCell, rc::Rc, time::Instant};

pub(crate) struct Memory {
    pub(crate) stored: Stored,
    pub(crate) writes: usize,
    pub(crate) fail: bool,
    pub(crate) locked: bool,
}
impl Default for Memory {
    fn default() -> Self {
        Self {
            stored: Stored::Absent,
            writes: 0,
            fail: false,
            locked: false,
        }
    }
}
#[derive(Clone, Default)]
pub(crate) struct Store(pub(crate) Rc<RefCell<Memory>>);
pub(crate) struct FakeTx {
    memory: Rc<RefCell<Memory>>,
    stored: Stored,
}
impl Authority for Store {
    type Tx<'a> = FakeTx;
    fn begin(&mut self, _: Instant) -> Result<FakeTx> {
        let mut memory = self.0.borrow_mut();
        ensure!(!memory.locked, "sidecar state is busy");
        memory.locked = true;
        Ok(FakeTx {
            memory: self.0.clone(),
            stored: memory.stored.clone(),
        })
    }
}
impl Drop for FakeTx {
    fn drop(&mut self) {
        self.memory.borrow_mut().locked = false;
    }
}
impl Tx for FakeTx {
    fn stored(&self) -> Result<&Stored> {
        Ok(&self.stored)
    }
    fn commit(&mut self, next: Envelope) -> Result<()> {
        let mut memory = self.memory.borrow_mut();
        ensure!(!memory.fail, "simulated flush failure");
        Envelope::check_successor(&self.stored, &next)?;
        memory.stored = Stored::Current(next.clone());
        memory.writes += 1;
        self.stored = Stored::Current(next);
        Ok(())
    }
    fn migrate(&mut self) -> Result<Envelope> {
        match self.stored.clone() {
            Stored::Legacy { journal, update } => {
                let envelope = Envelope::migrate(journal, update)?;
                self.commit(envelope.clone())?;
                Ok(envelope)
            }
            Stored::Current(envelope) => Ok(envelope),
            Stored::Absent => anyhow::bail!("no journal to migrate"),
        }
    }
}
pub(crate) fn envelope(store: &Store) -> Envelope {
    match &store.0.borrow().stored {
        Stored::Current(envelope) => envelope.clone(),
        other => panic!("expected a current journal, found {other:?}"),
    }
}
pub(crate) fn writes(store: &Store) -> usize {
    store.0.borrow().writes
}
