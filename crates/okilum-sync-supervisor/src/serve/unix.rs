//! The Unix (macOS) half of the run loop.
use super::{Exit, Settings};
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
use std::{path::Path, time::Instant};
use uuid::Uuid;

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
    let cleared = close_and_clear(endpoint, &store, Instant::now() + settings.store);
    let exit = outcome?;
    cleared.context("clearing the generation hint failed")?;
    Ok(exit)
}

/// The endpoint goes before the hint. A controller that takes the state lock right after
/// the Stop reply may still be connecting to this generation (it finds the hint, and the
/// socket) while holding that lock; a listener nobody accepts from any more keeps that
/// attempt waiting for its whole budget, and the hint cannot be cleared until it is over
/// (#1083). Closing the listener ends the attempt at once.
fn close_and_clear(endpoint: UnixEndpoint, store: &UnixStore, deadline: Instant) -> Result<()> {
    drop(endpoint);
    store.clear_hint(deadline)
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

#[cfg(test)]
mod tests {
    use super::*;
    use okilum_sync_controller::sidecar::supervisor::ipc::Scope;
    use std::{io::Read, os::unix::{fs::PermissionsExt, net::UnixStream}, time::Duration};

    /// #1083: the controller holds the state lock and is connected to the generation that
    /// has just stopped. The supervisor must close its endpoint first, so that attempt ends
    /// by itself and the lock is released before the hint is cleared. The lock is taken
    /// before the supervisor starts, so the order of the two is not left to the scheduler.
    #[test]
    fn a_connection_pending_under_the_state_lock_does_not_block_clearing_the_hint() {
        let root = tempfile::tempdir().unwrap();
        let state = std::fs::canonicalize(root.path()).unwrap().join("state");
        std::fs::create_dir(&state).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = UnixStore::open_existing(&state).unwrap();
        let scope = Scope {
            installation: Uuid::from_u128(1),
            instance: Uuid::from_u128(2),
            generation: Uuid::from_u128(3),
        };
        let endpoint = UnixEndpoint::create(&state, &scope).unwrap();
        let path = endpoint.path().to_owned();
        let lock = store.begin(Instant::now() + Duration::from_secs(5)).unwrap();
        // Connected, never accepted: the state of a controller's `Status` exchange.
        let mut pending = UnixStream::connect(&path).unwrap();
        pending
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        let supervisor = {
            let store = store.clone();
            std::thread::spawn(move || {
                close_and_clear(endpoint, &store, Instant::now() + Duration::from_secs(3))
            })
        };
        let started = Instant::now();
        let mut byte = [0u8; 1];
        let read = pending.read(&mut byte);
        let waited = started.elapsed();
        // Positive control: this wait is what the old order made as long as the controller's
        // budget. A timeout here means the listener was still open.
        assert!(
            !matches!(&read, Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)),
            "the pending connection was not ended by the supervisor: {read:?}"
        );
        assert!(waited < Duration::from_secs(2), "it took {waited:?}");
        drop(lock);
        supervisor.join().unwrap().unwrap();
        assert!(!path.exists(), "the socket node is gone");
    }
}
