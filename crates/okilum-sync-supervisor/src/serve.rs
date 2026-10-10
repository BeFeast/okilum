//! The supervisor's run loop (see docs/sync-supervisor-contract.md): endpoint, owned
//! tree, generation hint, then one exchange at a time until an authorized Stop ends it
//! or the runtime dies. The platform halves are `unix` (macOS) and `windows`.
use std::time::Duration;

pub struct Settings {
    /// Absolute budget of one exchange: authentication, I/O and any lock wait.
    pub exchange: Duration,
    /// Bound of one Stop of the owned tree.
    pub stop: Duration,
    /// How long to wait for a client before looking at the runtime again.
    pub tick: Duration,
    /// Budget of the hint and journal accesses outside an exchange.
    pub store: Duration,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            exchange: Duration::from_secs(10),
            stop: Duration::from_secs(10),
            // On Windows each wait recreates the pipe (one instance by design), so wait
            // longer there; this is also how long a dead runtime can go unnoticed.
            tick: if cfg!(windows) {
                Duration::from_secs(2)
            } else {
                Duration::from_millis(250)
            },
            store: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Exit {
    /// An authorized Stop returned `Stopped`: tree gone, hint cleared.
    Stopped,
    /// The runtime exited by itself. The tree was flushed and the hint cleared; the
    /// process should exit non-zero so the OS restart policy decides what happens.
    RuntimeExited,
}

/// Used by `main` to give the failure a readable context.
pub fn describe(exit: &Exit) -> &'static str {
    match exit {
        Exit::Stopped => "stopped by an authorized Stop",
        Exit::RuntimeExited => "the Syncthing runtime exited",
    }
}

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::run;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::run;
