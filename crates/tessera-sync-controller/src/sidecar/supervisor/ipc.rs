//! Bounded-frame supervisor control protocol, independent of the OS transport.
//! Native peer authentication, private endpoints and I/O deadlines are mandatory
//! transport duties. This module neither starts a process nor installs a service.
use crate::sidecar::Binding;
use anyhow::{ensure, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::io::{Read, Write};
use uuid::Uuid;

pub const VERSION: u16 = 1;
pub const MAX_FRAME: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub installation: Uuid,
    pub instance: Uuid,
    /// Fresh for each supervisor lifetime; never a PID or a persisted identity.
    pub generation: Uuid,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    Status,
    Stop,
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
        ensure!(
            request.command != Command::Stop || self.status != Status::Running,
            "stop reply did not transition runtime"
        );
        Ok(self.status)
    }
}

/// Established local connection. Implementations must authenticate the native
/// peer and endpoint against the prepared binding, never against wire claims.
/// A single finite exchange deadline must cover authentication and all I/O,
/// including fragmented reads; resetting a timeout per byte is insufficient.
/// No permissive default or production native transport is provided here.
pub trait Transport: Read + Write {
    fn verify_peer(&mut self, binding: &Binding) -> Result<()>;
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
    ensure!(
        request.version == VERSION,
        "unsupported supervisor protocol"
    );
    ensure!(
        request.scope.installation == binding.installation
            && request.scope.instance == binding.instance,
        "supervisor request binding mismatch"
    );
    transport.verify_peer(binding)?;
    write_frame(transport, request)?;
    let response: Response = read_frame(transport)?;
    response.validate_for(request)
}

/// Owns captured process/job handles, never PID lookup. This is not a default
/// native implementation: signature/state ownership and durable intent must be
/// checked by the platform integration before stopping its owned tree.
pub trait OwnedRuntime {
    fn status(&mut self) -> Result<Status>;
    /// Re-read the durable Disable/Remove/update-stop intent for this exact
    /// binding under the controller's ordering/locking protocol. Failure denies
    /// the effect. Do not acquire a lock that the waiting controller still holds.
    fn verify_stop_intent(&mut self, binding: &Binding) -> Result<()>;
    /// Bounded stop AND reap. Stopped means all owned descendants exited;
    /// Stopping means a timeout and must not permit unregister/removal success.
    fn stop_owned(&mut self) -> Result<Status>;
}

pub struct Server<R> {
    binding: Binding,
    scope: Scope,
    runtime: R,
    stopped: bool,
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
            stopped: false,
        }
    }
    /// Publish only through authenticated native endpoint discovery, bound to
    /// the captured supervisor process handle. Never trust a public PID file.
    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    /// Exactly one exchange; the native listener closes the connection on any
    /// error. No unbounded request loop, remote paths, Start, or shell command.
    pub fn serve_one(&mut self, transport: &mut impl Transport) -> Result<()> {
        transport.verify_peer(&self.binding)?;
        let request: Request = read_frame(transport)?;
        ensure!(
            request.version == VERSION,
            "unsupported supervisor protocol"
        );
        ensure!(request.scope == self.scope, "supervisor scope changed");
        let status = match request.command {
            Command::Status if self.stopped => Status::Stopped,
            Command::Status => self.runtime.status()?,
            Command::Stop => {
                self.runtime.verify_stop_intent(&self.binding)?;
                if self.stopped {
                    Status::Stopped
                } else {
                    let status = self.runtime.stop_owned()?;
                    ensure!(status != Status::Running, "stop did not transition runtime");
                    self.stopped = status == Status::Stopped;
                    status
                }
            }
        };
        write_frame(
            transport,
            &Response {
                version: VERSION,
                scope: self.scope.clone(),
                id: request.id,
                status,
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
