//! Binding/scope-checked adapter over owned native I/O. The caller still supplies
//! trusted discovery and signature/installation verification of captured peers.
use super::{
    windows_discovery::{identify_client, identify_server, ImagePolicy},
    windows_endpoint::PrivatePipe,
    windows_io::{ClientIo, ServerIo},
    windows_peer::ProcessPeer,
    Scope, Transport,
};
use crate::sidecar::{store::Hint, Binding};
use anyhow::{ensure, Result};
use std::{
    io::{self, Read, Write},
    sync::Arc,
    time::Instant,
};

enum Wire {
    Client(ClientIo),
    Server(ServerIo),
}

/// Captures the complete prepared Binding and Scope, never wire-derived identity.
/// Raw I/O is unavailable until verify_peer succeeds, and any error poisons it.
pub struct WindowsTransport {
    binding: Binding,
    scope: Scope,
    wire: Wire,
    verified: bool,
    failed: bool,
}
impl WindowsTransport {
    fn validate(binding: &Binding, scope: &Scope, peer: &ProcessPeer) -> Result<()> {
        ensure!(
            binding.owner == peer.owner_sid(),
            "prepared owner differs from captured peer owner"
        );
        Self::validate_scope(binding, scope)
    }
    fn validate_scope(binding: &Binding, scope: &Scope) -> Result<()> {
        ensure!(
            !scope.installation.is_nil() && !scope.instance.is_nil() && !scope.generation.is_nil(),
            "nil transport scope"
        );
        ensure!(
            binding.installation == scope.installation && binding.instance == scope.instance,
            "prepared scope differs from binding"
        );
        ensure!(
            !binding.supervisor.is_empty()
                && !binding.state_directory.is_empty()
                && !binding.device_identity.is_empty(),
            "incomplete prepared binding"
        );
        Ok(())
    }
    pub fn connect(
        binding: Binding,
        scope: Scope,
        peer: ProcessPeer,
        deadline: Instant,
    ) -> Result<Self> {
        Self::validate(&binding, &scope, &peer)?;
        let wire = Wire::Client(ClientIo::connect(scope.clone(), peer, deadline)?);
        Ok(Self {
            binding,
            scope,
            wire,
            verified: false,
            failed: false,
        })
    }
    /// Connect, then identify and verify the supervisor from the connected pipe
    /// (design: docs/sync-sidecar-discovery.md). The hint only names the generation
    /// and the start time to expect; the policy decides the signature; nothing is
    /// sent before the server process has been verified and retained.
    pub fn connect_discovering(
        binding: Binding,
        scope: Scope,
        hint: Hint,
        policy: Arc<dyn ImagePolicy + Send + Sync>,
        deadline: Instant,
    ) -> Result<Self> {
        Self::validate_scope(&binding, &scope)?;
        ensure!(
            hint.generation() == scope.generation,
            "hint names another supervisor generation"
        );
        let expected = binding.clone();
        let wire = Wire::Client(ClientIo::connect_discovering(
            scope.clone(),
            move |pipe| identify_server(pipe, &expected, &hint, &*policy),
            deadline,
        )?);
        Ok(Self {
            binding,
            scope,
            wire,
            verified: false,
            failed: false,
        })
    }
    /// The supervisor's accept when the client is the app: it connects, and only then is
    /// its process identified (same user, then the signature policy). Nothing is read
    /// or written before that and the retained peer is rechecked on the live pipe.
    pub fn accept_discovering(
        binding: Binding,
        scope: Scope,
        pipe: PrivatePipe,
        policy: Arc<dyn ImagePolicy + Send + Sync>,
        deadline: Instant,
    ) -> Result<Self> {
        Self::validate_scope(&binding, &scope)?;
        ensure!(
            pipe.scope() == &scope,
            "server endpoint belongs to another scope"
        );
        let owner = binding.owner.clone();
        let wire = Wire::Server(ServerIo::accept_discovering(
            pipe,
            move |pipe| identify_client(pipe, &owner, &*policy),
            deadline,
        )?);
        Ok(Self {
            binding,
            scope,
            wire,
            verified: false,
            failed: false,
        })
    }
    pub fn accept(
        binding: Binding,
        scope: Scope,
        pipe: PrivatePipe,
        peer: ProcessPeer,
        deadline: Instant,
    ) -> Result<Self> {
        Self::validate(&binding, &scope, &peer)?;
        ensure!(
            pipe.scope() == &scope,
            "server endpoint belongs to another scope"
        );
        let wire = Wire::Server(ServerIo::accept(pipe, peer, deadline)?);
        Ok(Self {
            binding,
            scope,
            wire,
            verified: false,
            failed: false,
        })
    }
    fn ready(&self) -> io::Result<()> {
        if self.failed || !self.verified {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "transport is unverified or poisoned",
            ));
        }
        Ok(())
    }
}
impl Transport for WindowsTransport {
    fn verify_peer(&mut self, binding: &Binding, scope: &Scope) -> Result<()> {
        let result = (|| {
            ensure!(!self.failed, "transport is poisoned");
            ensure!(
                binding == &self.binding,
                "prepared transport binding changed"
            );
            ensure!(scope == &self.scope, "prepared transport scope changed");
            match &mut self.wire {
                Wire::Client(io) => io.verify_peer()?,
                Wire::Server(io) => io.verify_peer()?,
            }
            Ok(())
        })();
        self.verified = result.is_ok();
        self.failed |= result.is_err();
        result
    }
}
impl Read for WindowsTransport {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let result = self.ready().and_then(|()| match &mut self.wire {
            Wire::Client(io) => io.read(bytes),
            Wire::Server(io) => io.read(bytes),
        });
        self.failed |= result.is_err();
        result
    }
}
impl Write for WindowsTransport {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let result = self.ready().and_then(|()| match &mut self.wire {
            Wire::Client(io) => io.write(bytes),
            Wire::Server(io) => io.write(bytes),
        });
        self.failed |= result.is_err();
        result
    }
    fn flush(&mut self) -> io::Result<()> {
        let result = self.ready().and_then(|()| match &mut self.wire {
            Wire::Client(io) => io.flush(),
            Wire::Server(io) => io.flush(),
        });
        self.failed |= result.is_err();
        result
    }
}
#[cfg(test)]
mod tests;
