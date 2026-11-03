//! The UI owns a not-yet-started journal job. Cancelling an async Task alone
//! releases its captures only when the executor next polls it; that is too late
//! for close/move to synchronously transfer the editor's file lock.
use okilum_core::file_editor::DraftWrite;
use std::{cell::RefCell, rc::Rc};
pub(super) type Ticket = Rc<RefCell<Option<DraftWrite>>>;
pub(super) struct Pending(Ticket);
impl Pending {
    pub(super) fn new(write: DraftWrite) -> Self {
        Self(Rc::new(RefCell::new(Some(write))))
    }
    pub(super) fn ticket(&self) -> Ticket {
        self.0.clone()
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        // A running worker has already taken its job and keeps exclusion until
        // completion. A timer-only job releases exclusion synchronously here.
        self.0.borrow_mut().take();
    }
}
