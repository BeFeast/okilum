//! The app's side of discovery (design: docs/sync-sidecar-discovery.md): read the
//! supervisor's generation hint, connect, ask for `Status`, and only then name a live
//! generation; and send an authorized Stop to exactly that generation. Nothing here is
//! authority. The hint only says whom to look for, a failed or odd answer means "no
//! live supervisor", and the journal token alone authorizes a stop.
//!
//! The hint is read without the instance lock on purpose: the controller calls this
//! while it holds its transaction, and the hint is replaced atomically.
use super::{
    authority::StopToken,
    store::{Hint, StateDir, Store},
    supervisor::ipc::{exchange, Command, Request, Scope, Status, Transport},
    Binding,
};
use anyhow::{bail, ensure, Context, Result};
use std::time::{Duration, Instant};

/// Platform half: open an authenticated connection to the supervisor named by `hint`
/// within `deadline`. Windows verifies the server process from the connected pipe,
/// macOS the audit token and code requirement; both with an injected policy.
pub trait Connector {
    type Transport: Transport;
    fn connect(
        &self,
        binding: &Binding,
        hint: &Hint,
        scope: &Scope,
        deadline: Instant,
    ) -> Result<Self::Transport>;
}

/// What the app can say about the supervisor, with the reason when it cannot reach it.
#[derive(Debug, PartialEq, Eq)]
pub enum Discovered {
    /// No hint: nothing published one, or it was removed by a clean exit.
    Absent,
    /// A hint exists but did not lead to an authenticated, answering supervisor of
    /// that generation (stale after a crash, wrong peer, malformed, not answering).
    Unreachable(String),
    Live(Scope),
}

fn scope_of(binding: &Binding, hint: &Hint) -> Scope {
    Scope {
        installation: binding.installation,
        instance: binding.instance,
        generation: hint.generation(),
    }
}

pub fn discover_detailed<D: StateDir, C: Connector>(
    store: &Store<D>,
    binding: &Binding,
    connector: &C,
    budget: Duration,
) -> Discovered {
    let hint = match store.peek_hint() {
        Ok(Some(hint)) => hint,
        Ok(None) => return Discovered::Absent,
        Err(error) => return Discovered::Unreachable(format!("unreadable hint: {error:#}")),
    };
    let scope = scope_of(binding, &hint);
    let attempt = (|| -> Result<Status> {
        let deadline = Instant::now() + budget;
        let mut transport = connector.connect(binding, &hint, &scope, deadline)?;
        exchange(
            &mut transport,
            binding,
            &Request::new(scope.clone(), Command::Status),
        )
    })();
    match attempt {
        // The reply was validated against our request, so it names this generation.
        Ok(Status::Running) => Discovered::Live(scope),
        Ok(other) => Discovered::Unreachable(format!("supervisor reports {other:?}")),
        Err(error) => Discovered::Unreachable(format!("{error:#}")),
    }
}

/// `Some` only for a live, authenticated supervisor. Everything else is `None`, and the
/// controller then stops natively under its own lock instead of sending a token.
pub fn discover<D: StateDir, C: Connector>(
    store: &Store<D>,
    binding: &Binding,
    connector: &C,
    budget: Duration,
) -> Option<Scope> {
    match discover_detailed(store, binding, connector, budget) {
        Discovered::Live(scope) => Some(scope),
        _ => None,
    }
}

/// Send the journal's Stop token to the supervisor of the generation it names. Success
/// is a reply of `Stopped` that echoes the token; `Stopping`, a refusal, a lost reply or
/// a hint for another generation are errors and prove nothing about exit.
pub fn stop<D: StateDir, C: Connector>(
    store: &Store<D>,
    binding: &Binding,
    connector: &C,
    token: &StopToken,
    budget: Duration,
) -> Result<()> {
    let hint = store
        .peek_hint()?
        .context("no supervisor has published a generation")?;
    let scope = token.operation.scope.clone();
    ensure!(
        hint.generation() == scope.generation,
        "the live supervisor is another generation than the one this stop was armed for"
    );
    ensure!(
        scope == scope_of(binding, &hint),
        "the stop token names another installation or instance"
    );
    let deadline = Instant::now() + budget;
    let mut transport = connector.connect(binding, &hint, &scope, deadline)?;
    let status = exchange(
        &mut transport,
        binding,
        &Request::new(scope, Command::Stop(token.clone())),
    )?;
    if status != Status::Stopped {
        bail!("the supervisor did not finish stopping ({status:?})");
    }
    Ok(())
}

#[cfg(unix)]
pub use unix::UnixConnector;
#[cfg(unix)]
mod unix {
    use super::*;
    use crate::sidecar::supervisor::ipc::unix_transport::{
        connect, PeerCheck, PeerEnd, UnixTransport,
    };
    use std::path::PathBuf;

    /// The private socket in the state directory; `peer` builds the check applied to
    /// the supervisor's end (the audit-token and code-requirement check on macOS).
    pub struct UnixConnector<F> {
        directory: PathBuf,
        peer: F,
    }
    impl<F: Fn() -> Box<dyn PeerCheck>> UnixConnector<F> {
        pub fn new(directory: PathBuf, peer: F) -> Self {
            Self { directory, peer }
        }
    }
    impl<F: Fn() -> Box<dyn PeerCheck>> Connector for UnixConnector<F> {
        type Transport = UnixTransport<Box<dyn PeerCheck>>;
        fn connect(
            &self,
            binding: &Binding,
            _hint: &Hint,
            scope: &Scope,
            deadline: Instant,
        ) -> Result<Self::Transport> {
            let stream = connect(&self.directory, scope)?;
            UnixTransport::new(
                stream,
                PeerEnd::Server,
                binding.clone(),
                scope.clone(),
                deadline,
                (self.peer)(),
            )
        }
    }
}

#[cfg(windows)]
pub use win::WindowsConnector;
#[cfg(windows)]
mod win {
    use super::*;
    use crate::sidecar::supervisor::ipc::{
        windows_discovery::ImagePolicy, windows_transport::WindowsTransport,
    };
    use std::sync::Arc;

    /// Connect-then-verify over the supervisor's named pipe. The supervisor recreates
    /// its single pipe instance between connections, so a refused attempt is retried
    /// briefly (never past the deadline) before it counts as unreachable.
    pub struct WindowsConnector {
        policy: Arc<dyn ImagePolicy + Send + Sync>,
        retry_for: Duration,
    }
    impl WindowsConnector {
        pub fn new(policy: Arc<dyn ImagePolicy + Send + Sync>) -> Self {
            Self {
                policy,
                retry_for: Duration::from_millis(1500),
            }
        }
    }
    impl Connector for WindowsConnector {
        type Transport = WindowsTransport;
        fn connect(
            &self,
            binding: &Binding,
            hint: &Hint,
            scope: &Scope,
            deadline: Instant,
        ) -> Result<WindowsTransport> {
            let give_up = (Instant::now() + self.retry_for).min(deadline);
            loop {
                match WindowsTransport::connect_discovering(
                    binding.clone(),
                    scope.clone(),
                    hint.clone(),
                    self.policy.clone(),
                    deadline,
                ) {
                    Ok(transport) => return Ok(transport),
                    Err(error) if Instant::now() >= give_up => return Err(error),
                    Err(_) => std::thread::sleep(Duration::from_millis(50)),
                }
            }
        }
    }
}
