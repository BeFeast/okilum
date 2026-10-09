//! Bounded-frame supervisor control protocol, independent of the OS transport.
//! Native peer authentication, private endpoints and I/O deadlines are mandatory
//! transport duties. This module neither starts a process nor installs a service.
use crate::sidecar::{authority::StopToken, Binding};
use anyhow::{ensure, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::io::{Read, Write};
use uuid::Uuid;

#[cfg(target_os = "windows")]
pub mod windows_endpoint;
#[cfg(target_os = "windows")]
pub mod windows_io;
#[cfg(target_os = "windows")]
pub mod windows_peer;
#[cfg(target_os = "windows")]
pub mod windows_transport;

pub const VERSION: u16 = 2;
pub const MAX_FRAME: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub installation: Uuid,
    pub instance: Uuid,
    /// Fresh for each supervisor lifetime; never a PID or a persisted identity.
    pub generation: Uuid,
}
/// Command-specific fields are structural: Stop must carry exactly one token and
/// Status none, so a v1 `"Stop"` frame or a token-less Stop cannot deserialize.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    Status,
    Stop(StopToken),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u16,
    pub scope: Scope,
    pub id: Uuid,
    pub command: Command,
}
impl Request {
    pub fn new(scope: Scope, command: Command) -> Self {
        Self {
            version: VERSION,
            scope,
            id: Uuid::new_v4(),
            command,
        }
    }
    /// Effect-free checks shared by client and server. The token must name this
    /// request's own scope; nil identities never pass. A revision does not
    /// authenticate the caller, and the durable state decides authorization.
    pub fn validate(&self) -> Result<()> {
        ensure!(self.version == VERSION, "unsupported supervisor protocol");
        ensure!(
            !self.id.is_nil() && !self.scope.generation.is_nil(),
            "nil supervisor identity"
        );
        if let Command::Stop(token) = &self.command {
            let operation = &token.operation;
            ensure!(
                !token.journal_epoch.is_nil()
                    && !operation.operation_id.is_nil()
                    && operation.authorized_revision >= 1,
                "invalid stop token"
            );
            ensure!(operation.scope == self.scope, "stop token scope mismatch");
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Running,
    Stopping,
    Stopped,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub version: u16,
    pub scope: Scope,
    pub id: Uuid,
    pub status: Status,
    /// Echo of the full Stop token; absent for Status. Checked in `validate_for`.
    pub token: Option<StopToken>,
}
impl Response {
    /// Authenticate the remote endpoint separately before trusting this reply.
    pub fn validate_for(&self, request: &Request) -> Result<Status> {
        ensure!(
            self.version == VERSION && request.version == VERSION,
            "unsupported supervisor protocol"
        );
        ensure!(
            self.scope == request.scope && self.id == request.id,
            "supervisor reply mismatch"
        );
        match (&request.command, &self.token) {
            (Command::Stop(sent), Some(echoed)) => {
                ensure!(sent == echoed, "stop reply token mismatch");
                ensure!(
                    self.status != Status::Running,
                    "stop reply did not transition runtime"
                );
            }
            (Command::Status, None) => {}
            _ => anyhow::bail!("unexpected stop token in supervisor reply"),
        }
        Ok(self.status)
    }
}

/// Established local connection. Implementations must authenticate the native
/// peer and endpoint against the prepared binding, never against wire claims.
/// A single finite exchange deadline must cover authentication and all I/O,
/// including fragmented reads; resetting a timeout per byte is insufficient.
/// Implementations must compare the prepared binding and endpoint generation;
/// neither may be replaced by claims received from the wire.
pub trait Transport: Read + Write {
    fn verify_peer(&mut self, binding: &Binding, scope: &Scope) -> Result<()>;
}

/// One authenticated request/reply exchange. The caller discovers the generation
/// through the native verified endpoint, and persists stop intent before Stop.
/// A timeout, lost reply or mismatched response is an error, never proof of exit.
/// This function does not retry, unregister a service or change durable intent.
pub fn exchange(
    transport: &mut impl Transport,
    binding: &Binding,
    request: &Request,
) -> Result<Status> {
    request.validate()?;
    ensure!(
        request.scope.installation == binding.installation
            && request.scope.instance == binding.instance,
        "supervisor request binding mismatch"
    );
    transport.verify_peer(binding, &request.scope)?;
    write_frame(transport, request)?;
    let response: Response = read_frame(transport)?;
    response.validate_for(request)
}

/// Owns captured process/job handles, never PID lookup. This is not a default
/// native implementation: signature/state ownership must be checked by the
/// platform integration before stopping its owned tree.
pub trait OwnedRuntime {
    /// Authority for one Stop: the exclusive instance lock taken in
    /// `authorize_stop`. The server drops it, releasing the lock, before it
    /// writes any reply, and never calls back into a controller while it is held.
    type Lease;
    fn status(&mut self) -> Result<Status>;
    /// Under the instance lock, load authoritative state and require the exact
    /// Binding, epoch, current revision, stored token, reason/phase and this
    /// supervisor generation (`Envelope::authorize`). Failure or contention
    /// denies the effect, including for repeated requests. Implementations are
    /// built with the same absolute deadline as the accepted transport, so lock
    /// waiting cannot extend the exchange budget.
    fn authorize_stop(
        &mut self,
        binding: &Binding,
        scope: &Scope,
        token: &StopToken,
    ) -> Result<Self::Lease>;
    /// Bounded stop AND reap while `lease` is held. Stopped means all owned
    /// descendants exited; Stopping means a timeout and must not permit
    /// unregister/removal success.
    fn stop_owned(&mut self, lease: &Self::Lease) -> Result<Status>;
}

pub struct Server<R> {
    binding: Binding,
    scope: Scope,
    runtime: R,
    /// Completion is cached only for the exact token that produced it, in this
    /// generation. It is not durable and never stands in for authorization.
    completed: Option<StopToken>,
}
impl<R: OwnedRuntime> Server<R> {
    /// Only after explicit Enable and native verification/preparation. Creating
    /// this protocol object has no process, filesystem or registration effects.
    pub fn new(binding: Binding, runtime: R) -> Self {
        let scope = Scope {
            installation: binding.installation,
            instance: binding.instance,
            generation: Uuid::new_v4(),
        };
        Self {
            binding,
            scope,
            runtime,
            completed: None,
        }
    }
    /// The native listener sets the per-exchange deadline on its runtime here.
    pub fn runtime_mut(&mut self) -> &mut R {
        &mut self.runtime
    }
    /// Publish only through authenticated native endpoint discovery, bound to
    /// the captured supervisor process handle. Never trust a public PID file.
    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    /// Exactly one exchange; the native listener closes the connection on any
    /// error. No unbounded request loop, remote paths, Start, or shell command.
    pub fn serve_one(&mut self, transport: &mut impl Transport) -> Result<()> {
        transport.verify_peer(&self.binding, &self.scope)?;
        let request: Request = read_frame(transport)?;
        request.validate()?;
        ensure!(request.scope == self.scope, "supervisor scope changed");
        let (status, token) = match &request.command {
            Command::Status if self.completed.is_some() => (Status::Stopped, None),
            Command::Status => (self.runtime.status()?, None),
            Command::Stop(token) => {
                // The lease ends with this block: the lock is released before
                // the reply is written, and a repeat is authorized again first.
                let status = {
                    let lease = self
                        .runtime
                        .authorize_stop(&self.binding, &self.scope, token)?;
                    if self.completed.as_ref() == Some(token) {
                        Status::Stopped
                    } else {
                        let status = self.runtime.stop_owned(&lease)?;
                        ensure!(status != Status::Running, "stop did not transition runtime");
                        if status == Status::Stopped {
                            self.completed = Some(token.clone());
                        }
                        status
                    }
                };
                (status, Some(token.clone()))
            }
        };
        write_frame(
            transport,
            &Response {
                version: VERSION,
                scope: self.scope.clone(),
                id: request.id,
                status,
                token,
            },
        )
    }
}

/// Validate length before allocating or reading the body. Truncation, invalid
/// JSON, unknown fields and trailing JSON values fail instead of becoming a Stop.
pub fn read_frame<T: DeserializeOwned>(reader: &mut impl Read) -> Result<T> {
    let mut header = [0; 4];
    reader.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    ensure!(
        (1..=MAX_FRAME).contains(&length),
        "invalid supervisor frame length"
    );
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body)?)
}
pub fn write_frame(writer: &mut impl Write, message: &impl Serialize) -> Result<()> {
    let body = serde_json::to_vec(message)?;
    ensure!(
        (1..=MAX_FRAME).contains(&body.len()),
        "invalid supervisor frame length"
    );
    writer.write_all(&(body.len() as u32).to_be_bytes())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests;
