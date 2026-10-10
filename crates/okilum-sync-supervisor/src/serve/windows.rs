//! The Windows half of the run loop: named pipe, Job Object. The pipe is created per
//! connection (one instance at a time, by design), the client is identified after it
//! connects, and the wait for a client never shortens the budget of one that arrives
//! late (`accept_discovering_within`).
use super::{Exit, Settings};
use crate::startup::Prepared;
use anyhow::{ensure, Result};
use okilum_sync_controller::sidecar::{
    store::{Hint, WindowsDir, WindowsStore},
    supervisor::{
        ipc::{
            windows_discovery::{own_start_time, ImagePolicy},
            windows_endpoint::PrivatePipe,
            windows_transport::WindowsTransport,
            OwnedRuntime, Scope, Server, Status,
        },
        runtime::StoreRuntime,
        windows::{JobChild, JobTree},
    },
};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use uuid::Uuid;

type Runtime = StoreRuntime<WindowsDir, JobTree>;

/// The previous connection's handle can outlive the transport for a moment; the next
/// pipe may only be created once it is gone, so retry briefly.
fn create_pipe(scope: &Scope) -> Result<PrivatePipe> {
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        match PrivatePipe::create(scope) {
            Ok(pipe) => return Ok(pipe),
            Err(error) if Instant::now() >= until => return Err(error),
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

pub fn run(
    state: &Path,
    store: WindowsStore,
    prepared: Prepared,
    settings: &Settings,
    policy: Arc<dyn ImagePolicy + Send + Sync>,
) -> Result<Exit> {
    let Prepared {
        binding,
        selection,
        launch,
    } = prepared;
    let generation = Uuid::new_v4();
    let scope = Scope {
        installation: binding.installation,
        instance: binding.instance,
        generation,
    };
    let first = PrivatePipe::create(&scope)?;
    // Checked right before the spawn and again right after it; on a mismatch after the
    // spawn the job is closed (every process in it killed) as the error unwinds.
    selection.verify_file(state)?;
    let child = JobChild::spawn(&launch)?;
    selection.verify_file(state)?;
    let tree = JobTree::new(child, settings.stop);
    store.publish_hint(
        Instant::now() + settings.store,
        &Hint::new(generation, own_start_time()?)?,
    )?;
    let mut server: Server<Runtime> = Server::with_generation(
        binding.clone(),
        StoreRuntime::new(store.clone(), tree),
        generation,
    );
    let outcome = serve(&mut server, first, &binding, &scope, settings, &policy);
    // Whatever ended the loop, the hint must not outlive the supervisor.
    let cleared = store.clear_hint(Instant::now() + settings.store);
    let exit = outcome?;
    cleared.map_err(|e| e.context("clearing the generation hint failed"))?;
    Ok(exit)
}

fn serve(
    server: &mut Server<Runtime>,
    first: PrivatePipe,
    binding: &okilum_sync_controller::sidecar::Binding,
    scope: &Scope,
    settings: &Settings,
    policy: &Arc<dyn ImagePolicy + Send + Sync>,
) -> Result<Exit> {
    let mut pipe = Some(first);
    loop {
        let current = match pipe.take() {
            Some(pipe) => pipe,
            None => create_pipe(scope)?,
        };
        // A failed or refused connection costs only that connection.
        if let Ok(mut transport) = WindowsTransport::accept_discovering_within(
            binding.clone(),
            scope.clone(),
            current,
            policy.clone(),
            settings.tick,
            settings.exchange,
        ) {
            server
                .runtime_mut()
                .begin_exchange(Instant::now() + settings.exchange);
            let _ = server.serve_one(&mut transport);
        }
        if server.has_stopped() {
            return Ok(Exit::Stopped);
        }
        // The tree is certified gone only by the Job Object accounting; a runtime that
        // died on its own is flushed here and reported.
        if server.runtime_mut().status()? == Status::Stopped {
            let status = server.runtime_mut().flush_tree()?;
            ensure!(
                status == Status::Stopped,
                "the crashed runtime's tree could not be flushed"
            );
            return Ok(Exit::RuntimeExited);
        }
    }
}
