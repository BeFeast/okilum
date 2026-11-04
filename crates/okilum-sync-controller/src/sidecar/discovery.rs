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
    discover_detailed_until(store, binding, connector, Instant::now() + budget)
}

fn discover_detailed_until<D: StateDir, C: Connector>(
    store: &Store<D>,
    binding: &Binding,
    connector: &C,
    deadline: Instant,
) -> Discovered {
    let hint = match store.peek_hint() {
        Ok(Some(hint)) => hint,
        Ok(None) => return Discovered::Absent,
        Err(error) => return Discovered::Unreachable(format!("unreadable hint: {error:#}")),
    };
    let scope = scope_of(binding, &hint);
    let attempt = (|| -> Result<Status> {
        ensure!(
            Instant::now() < deadline,
            "supervisor exchange deadline elapsed"
        );
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
    stop_until(store, binding, connector, token, Instant::now() + budget)
}

fn stop_until<D: StateDir, C: Connector>(
    store: &Store<D>,
    binding: &Binding,
    connector: &C,
    token: &StopToken,
    deadline: Instant,
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
    ensure!(
        Instant::now() < deadline,
        "supervisor exchange deadline elapsed"
    );
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

/// What a platform guard embeds to implement `supervisor_scope` and `stop_supervisor`:
/// the store whose hint it reads (without the instance lock), the platform connector
/// and the time one call may take. Cloning the store shares its lock, so the link and
/// the controller can use the same one.
pub struct SupervisorLink<D: StateDir, C: Connector> {
    store: Store<D>,
    connector: C,
    budget: Duration,
}
impl<D: StateDir, C: Connector> SupervisorLink<D, C> {
    pub fn new(store: Store<D>, connector: C, budget: Duration) -> Self {
        Self {
            store,
            connector,
            budget,
        }
    }
    pub fn scope(&self, binding: &Binding) -> Option<Scope> {
        discover(&self.store, binding, &self.connector, self.budget)
    }
    pub fn detailed(&self, binding: &Binding) -> Discovered {
        discover_detailed(&self.store, binding, &self.connector, self.budget)
    }
    /// Share the caller's whole-operation deadline, without renewing its budget.
    pub fn detailed_until(&self, binding: &Binding, deadline: Instant) -> Discovered {
        discover_detailed_until(&self.store, binding, &self.connector, deadline)
    }
    pub fn stop_until(
        &self,
        binding: &Binding,
        token: &StopToken,
        deadline: Instant,
    ) -> Result<()> {
        stop_until(&self.store, binding, &self.connector, token, deadline)
    }
    pub fn stop(&self, binding: &Binding, token: &StopToken) -> Result<()> {
        stop(&self.store, binding, &self.connector, token, self.budget)
    }
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
        windows_discovery::ImagePolicy, windows_endpoint::OPEN_CONTEXT,
        windows_transport::WindowsTransport,
    };
    use std::sync::Arc;

    /// Connect-then-verify over the supervisor's named pipe. The supervisor recreates
    /// its single pipe instance between connections, so an attempt that could not even
    /// open the pipe is retried briefly (never past the deadline); an attempt that
    /// reached the pipe and failed verification is not.
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
                    // Only "the pipe is not there / is busy right now" is worth another
                    // try; a failed verification is final and must not cost latency.
                    Err(error)
                        if Instant::now() >= give_up
                            || !format!("{error:#}").contains(OPEN_CONTEXT) =>
                    {
                        return Err(error)
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(50)),
                }
            }
        }
    }
}

/// Inventory the existing managed receiver configuration after an app restart.
/// This is only a candidate path, never proof of a live or authenticated daemon.
/// Callers must still inspect its identity before use. No state is created and
/// no supervisor connection, preparation, registration or process start occurs.
/// Only an absent state directory means no managed candidate; incomplete or
/// unsafe existing state must remain a visible discovery error.
#[cfg(unix)]
pub fn managed_configuration_path(state: &std::path::Path) -> Result<Option<std::path::PathBuf>> {
    use std::{fs, os::unix::fs::MetadataExt, path::Component};
    ensure!(
        state.is_absolute()
            && state
                .components()
                .all(|c| !matches!(c, Component::ParentDir | Component::CurDir)),
        "plain absolute managed state path required"
    );
    let metadata = match fs::symlink_metadata(state) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let config = state.join("config");
    let file = config.join("config.xml");
    // An interrupted first preparation leaves no configuration yet (#2589).
    let config_metadata = match fs::symlink_metadata(&config) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    for metadata in [metadata, config_metadata] {
        ensure!(
            metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && metadata.uid() == rustix::process::geteuid().as_raw()
                && metadata.mode() & 0o7077 == 0,
            "private owned managed configuration directory required"
        );
    }
    let metadata = match fs::symlink_metadata(&file) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.uid() == rustix::process::geteuid().as_raw()
            && metadata.mode() & 0o7077 == 0,
        "private owned managed configuration file required"
    );
    Ok(Some(file))
}

#[cfg(all(test, unix))]
mod managed_configuration_tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{symlink, PermissionsExt},
    };

    #[test]
    fn restart_rediscovers_offline_configuration_without_preparing_state() -> Result<()> {
        let root = tempfile::tempdir()?;
        let state = root.path().join("managed");
        assert_eq!(managed_configuration_path(&state)?, None);
        assert!(!state.exists());
        fs::create_dir(&state)?;
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
        let config = state.join("config");
        fs::create_dir(&config)?;
        fs::set_permissions(&config, fs::Permissions::from_mode(0o700))?;
        let file = config.join("config.xml");
        // An inventory fixture only: it cannot authenticate or start a daemon.
        fs::write(&file, b"offline configuration fixture")?;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
        for _ in 0..2 {
            assert_eq!(managed_configuration_path(&state)?, Some(file.clone()));
        }
        assert_eq!(fs::read(&file)?, b"offline configuration fixture");
        assert_eq!(fs::read_dir(&state)?.count(), 1);
        assert_eq!(fs::read_dir(&config)?.count(), 1);
        Ok(())
    }

    #[test]
    fn incomplete_unsafe_and_linked_configuration_is_not_absence() -> Result<()> {
        let root = tempfile::tempdir()?;
        let state = root.path();
        fs::set_permissions(state, fs::Permissions::from_mode(0o700))?;
        assert!(managed_configuration_path(state).is_err());
        let config = state.join("config");
        fs::create_dir(&config)?;
        fs::set_permissions(&config, fs::Permissions::from_mode(0o700))?;
        assert!(managed_configuration_path(state).is_err());
        let file = config.join("config.xml");
        fs::write(&file, b"fixture")?;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
        assert_eq!(managed_configuration_path(state)?, Some(file.clone()));
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644))?;
        assert!(managed_configuration_path(state).is_err());
        fs::remove_file(&file)?;
        symlink("missing.xml", &file)?;
        assert!(managed_configuration_path(state).is_err());
        fs::remove_file(&file)?;
        fs::remove_dir(&config)?;
        let outside = root.path().join("outside");
        fs::create_dir(&outside)?;
        symlink(&outside, &config)?;
        assert!(managed_configuration_path(state).is_err());
        assert!(managed_configuration_path(std::path::Path::new("relative")).is_err());
        Ok(())
    }
}
