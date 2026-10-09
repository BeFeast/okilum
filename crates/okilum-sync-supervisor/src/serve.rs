//! The supervisor's run loop on Unix (macOS in production): endpoint, owned tree,
//! generation hint, then one exchange at a time until an authorized Stop ends it or the
//! runtime dies. Order and failure behaviour follow docs/sync-supervisor-contract.md.
use crate::startup::Prepared;
use anyhow::{Context, Result};
use okilum_sync_controller::sidecar::{
    store::{Hint, UnixDir, UnixStore},
    supervisor::{
        ipc::{
            unix_transport::{PeerCheck, PeerEnd, UnixEndpoint, UnixTransport},
            Server, Status,
        },
        process_group::{process_start_time, ProcessGroupTree},
        runtime::StoreRuntime,
    },
};
use std::{
    path::Path,
    time::{Duration, Instant},
};
use uuid::Uuid;

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
            tick: Duration::from_millis(250),
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

type Runtime = StoreRuntime<UnixDir, ProcessGroupTree>;

pub fn run<C: PeerCheck>(
    state: &Path,
    store: UnixStore,
    prepared: Prepared,
    settings: &Settings,
    peer_check: impl Fn() -> C,
) -> Result<Exit> {
    let Prepared {
        binding,
        selection,
        launch,
    } = prepared;
    let generation = Uuid::new_v4();
    let scope = okilum_sync_controller::sidecar::supervisor::ipc::Scope {
        installation: binding.installation,
        instance: binding.instance,
        generation,
    };
    let endpoint = UnixEndpoint::create(state, &scope)?;
    // The digest is checked right before the spawn and again right after it. On a
    // mismatch after the spawn the tree is dropped (killed) as the error unwinds.
    selection.verify_file(state)?;
    let tree = ProcessGroupTree::spawn(&launch, settings.stop)?;
    selection.verify_file(state)?;
    let started = process_start_time(std::process::id())?;
    store.publish_hint(
        Instant::now() + settings.store,
        &Hint::new(generation, started)?,
    )?;
    let mut server: Server<Runtime> = Server::with_generation(
        binding.clone(),
        StoreRuntime::new(store.clone(), tree),
        generation,
    );
    let outcome = serve(
        &mut server,
        &endpoint,
        &binding,
        &scope,
        settings,
        &peer_check,
    );
    // Whatever ended the loop, the hint must not outlive the supervisor.
    let cleared = store.clear_hint(Instant::now() + settings.store);
    let exit = outcome?;
    cleared.context("clearing the generation hint failed")?;
    Ok(exit)
}

fn serve<C: PeerCheck>(
    server: &mut Server<Runtime>,
    endpoint: &UnixEndpoint,
    binding: &okilum_sync_controller::sidecar::Binding,
    scope: &okilum_sync_controller::sidecar::supervisor::ipc::Scope,
    settings: &Settings,
    peer_check: &impl Fn() -> C,
) -> Result<Exit> {
    loop {
        if server.runtime_mut().tree().root_exited()? {
            // Flush stragglers (the leader is unreaped, so signalling is safe), then
            // report the crash. A tree that cannot be certified gone is an error too.
            let status = server.runtime_mut().flush_tree()?;
            anyhow::ensure!(
                status == Status::Stopped,
                "the crashed runtime's tree could not be flushed"
            );
            return Ok(Exit::RuntimeExited);
        }
        let Some(stream) = endpoint.try_accept(Instant::now() + settings.tick)? else {
            continue;
        };
        let deadline = Instant::now() + settings.exchange;
        // A connection that cannot even be set up, or whose exchange fails, costs
        // only that connection; the supervisor keeps serving.
        let Ok(mut transport) = UnixTransport::new(
            stream,
            PeerEnd::Client,
            binding.clone(),
            scope.clone(),
            deadline,
            peer_check(),
        ) else {
            continue;
        };
        server.runtime_mut().begin_exchange(deadline);
        let _ = server.serve_one(&mut transport);
        if server.has_stopped() {
            return Ok(Exit::Stopped);
        }
    }
}

/// Used by `main` to give the failure a readable context.
pub fn describe(exit: &Exit) -> &'static str {
    match exit {
        Exit::Stopped => "stopped by an authorized Stop",
        Exit::RuntimeExited => "the Syncthing runtime exited",
    }
}
