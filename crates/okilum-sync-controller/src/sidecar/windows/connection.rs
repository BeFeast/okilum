//! Stop on the authenticated connected pipe, retaining that exact process object
//! before the request and awaiting its kernel exit after the complete-tree reply.
use super::signature::AuthenticodePolicy;
use crate::sidecar::{
    authority::StopToken,
    discovery::{Connector, WindowsConnector},
    store::WindowsStore,
    supervisor::ipc::{self, windows_transport::WindowsTransport, Command, Request, Status},
    Binding,
};
use anyhow::{ensure, Context, Result};
use std::{sync::Arc, time::Instant};

pub(super) fn stop_and_wait(
    store: &WindowsStore,
    binding: &Binding,
    token: &StopToken,
    policy: Arc<AuthenticodePolicy>,
    deadline: Instant,
) -> Result<()> {
    let hint = store
        .peek_hint_until(deadline)?
        .context("no authenticated supervisor generation; recovery pending")?;
    let scope = token.operation.scope.clone();
    ensure!(
        scope.installation == binding.installation
            && scope.instance == binding.instance
            && scope.generation == hint.generation(),
        "stop token names another supervisor generation"
    );
    let connector = WindowsConnector::new(policy);
    let mut transport: WindowsTransport = connector.connect(binding, &hint, &scope, deadline)?;
    let exit = transport.capture_exit()?;
    ensure!(
        ipc::exchange(
            &mut transport,
            binding,
            &Request::new(scope, Command::Stop(token.clone()))
        )? == Status::Stopped,
        "supervisor did not finish stopping its owned Job Object; recovery pending"
    );
    drop(transport);
    exit.wait(deadline)
}
