//! Verified bundle + private store + authenticated supervisor IPC, without service
//! registration or PID-based lifecycle control. Missing hints are not exit proof.
use super::bundle::{BundleGuard, SignatureVerifier};
use crate::sidecar::{
    authority::StopToken,
    discovery::{Connector, Discovered, SupervisorLink},
    store::{StateDir, Store},
    Binding,
};
use anyhow::{ensure, Context, Result};
use std::time::{Duration, Instant};

pub struct Connection<V: SignatureVerifier, D: StateDir, C: Connector> {
    binding: Binding,
    bundle: BundleGuard<V>,
    link: SupervisorLink<D, C>,
    store: Store<D>,
    budget: Duration,
}
impl<V: SignatureVerifier, D: StateDir, C: Connector> Connection<V, D, C> {
    /// Open outside a controller transaction: this initial journal check takes
    /// its lock. The state must already be prepared and explicitly enabled once.
    /// Inspection reads the atomic hint without locking. Stop validates its
    /// journal token in a short transaction, released before any IPC.
    pub fn new(
        binding: Binding,
        bundle: BundleGuard<V>,
        store: Store<D>,
        connector: C,
        budget: Duration,
    ) -> Result<Self> {
        ensure!(!budget.is_zero(), "finite nonzero IPC budget required");
        let deadline = Instant::now()
            .checked_add(budget)
            .context("IPC budget overflow")?;
        bundle.verify(&binding)?;
        {
            let tx = store.begin(deadline)?;
            let saved = tx.current()?.context("no prepared managed Sync journal")?;
            ensure!(
                saved.binding() == &binding,
                "saved supervisor binding changed"
            );
        }
        Ok(Self {
            binding,
            bundle,
            link: SupervisorLink::new(store.clone(), connector, budget),
            store,
            budget,
        })
    }

    /// Recheck the bundle before authenticating a live process. Absent means no
    /// hint; Unreachable remains visible, never converted into "stopped".
    pub fn inspect(&self) -> Result<Discovered> {
        let deadline = Instant::now() + self.budget;
        self.bundle.verify(&self.binding)?;
        Ok(self.link.detailed_until(&self.binding, deadline))
    }

    /// Request the already-journaled token against exactly its live generation.
    /// Success acknowledges the supervisor's owned-tree Stop reply, NOT that the
    /// supervisor process exited or that unregistering a service is now safe.
    /// The lifecycle controller must revalidate authority and confirm native exit.
    pub fn request_stop(&self, token: &StopToken) -> Result<()> {
        self.request_stop_until(token, Instant::now() + self.budget)
    }

    fn validate_stop(&self, token: &StopToken, deadline: Instant) -> Result<()> {
        ensure!(
            Instant::now() < deadline,
            "supervisor stop deadline elapsed"
        );
        let tx = self.store.begin(deadline)?;
        let saved = tx.current()?.context("managed Sync journal disappeared")?;
        ensure!(
            saved.binding() == &self.binding,
            "saved supervisor binding changed"
        );
        saved.authorize(token, &token.operation.scope)
    }

    fn request_stop_until(&self, token: &StopToken, deadline: Instant) -> Result<()> {
        self.bundle.verify(&self.binding)?;
        self.validate_stop(token, deadline)?;
        self.link.stop_until(&self.binding, token, deadline)
    }

    #[cfg(any(target_os = "macos", test))]
    fn stop_and_confirm(
        &self,
        token: &StopToken,
        deadline: Instant,
        confirm_exit: impl FnOnce(Instant) -> Result<bool>,
    ) -> Result<()> {
        self.request_stop_until(token, deadline)?;
        ensure!(
            confirm_exit(deadline)?,
            "supervisor process exit was not confirmed"
        );
        // The server's acknowledgement and a kernel exit event do not override
        // a concurrent Enable, new generation/epoch, or other durable operation.
        self.validate_stop(token, deadline)
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use crate::sidecar::{
        discovery::UnixConnector,
        macos::bundle::NativeSignature,
        store::{UnixDir, UnixStore},
        supervisor::ipc::{
            code_requirement::CodeRequirement,
            macos_exit::ExitWatch,
            macos_peer::SignedPeer,
            unix_transport::{PeerCheck, PeerEnd},
        },
    };
    use std::{cell::RefCell, os::unix::net::UnixStream, path::Path, rc::Rc};

    type PeerFactory = Box<dyn Fn() -> Box<dyn PeerCheck>>;
    pub type NativeConnection = Connection<NativeSignature, UnixDir, UnixConnector<PeerFactory>>;

    impl NativeConnection {
        /// No permissive authentication or fallback transport: both signature
        /// requirements come from build policy; state is opened, never created.
        pub fn open(
            app: &Path,
            app_executable: &str,
            binding: Binding,
            app_requirement: CodeRequirement,
            helper_requirement: CodeRequirement,
            budget: Duration,
        ) -> Result<Self> {
            let bundle = BundleGuard::new(
                app,
                app_executable,
                binding.clone(),
                app_requirement,
                helper_requirement.clone(),
                NativeSignature,
            )?;
            let state = Path::new(&binding.state_directory);
            let store = UnixStore::open_existing(state)?;
            let peer = SignedPeer::new(helper_requirement, Path::new(&binding.supervisor))?;
            let factory: PeerFactory = Box::new(move || Box::new(peer.clone()));
            let connector = UnixConnector::new(state.to_owned(), factory);
            Self::new(binding, bundle, store, connector, budget)
        }
    }

    struct WatchingPeer {
        peer: SignedPeer,
        captured: Rc<RefCell<Option<ExitWatch>>>,
    }
    impl PeerCheck for WatchingPeer {
        fn verify(&self, stream: &UnixStream, end: PeerEnd) -> Result<()> {
            self.captured.borrow_mut().take();
            ensure!(
                matches!(end, PeerEnd::Server),
                "exit capture requires the supervisor endpoint"
            );
            // Capture delegates to SignedPeer::verify both before and after
            // kernel registration (fresh audit-token/Security/path checks).
            let watch = ExitWatch::capture(stream, &self.peer)?;
            *self.captured.borrow_mut() = Some(watch);
            Ok(())
        }
    }

    /// Native Stop path: the process watch is captured on the very connection
    /// authenticated for the Stop request, before any frame is sent.
    pub struct NativeSupervisor {
        connection: NativeConnection,
        captured: Rc<RefCell<Option<ExitWatch>>>,
        budget: Duration,
    }
    impl NativeSupervisor {
        pub fn open(
            app: &Path,
            app_executable: &str,
            binding: Binding,
            app_requirement: CodeRequirement,
            helper_requirement: CodeRequirement,
            budget: Duration,
        ) -> Result<Self> {
            let bundle = BundleGuard::new(
                app,
                app_executable,
                binding.clone(),
                app_requirement,
                helper_requirement.clone(),
                NativeSignature,
            )?;
            let state = Path::new(&binding.state_directory);
            let store = UnixStore::open_existing(state)?;
            let peer = SignedPeer::new(helper_requirement, Path::new(&binding.supervisor))?;
            let captured = Rc::new(RefCell::new(None));
            let slot = captured.clone();
            let factory: PeerFactory = Box::new(move || {
                Box::new(WatchingPeer {
                    peer: peer.clone(),
                    captured: slot.clone(),
                })
            });
            let connector = UnixConnector::new(state.to_owned(), factory);
            let connection = NativeConnection::new(binding, bundle, store, connector, budget)?;
            Ok(Self {
                connection,
                captured,
                budget,
            })
        }
        pub fn inspect(&self) -> Result<Discovered> {
            self.connection.inspect()
        }
        /// One deadline covers authentication, owned-tree Stop, captured-process
        /// exit and final authority revalidation. No PID kill/unregister fallback.
        /// The lifecycle controller must still revalidate at its native mutation.
        pub fn stop_and_wait(&self, token: &StopToken) -> Result<()> {
            let deadline = Instant::now() + self.budget;
            self.captured.borrow_mut().take();
            self.connection
                .stop_and_confirm(token, deadline, |deadline| {
                    let mut watch = self
                        .captured
                        .borrow_mut()
                        .take()
                        .context("Stop did not capture an authenticated process")?;
                    watch.wait(deadline)
                })
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use std::process::Command;

        #[test]
        fn watching_peer_keeps_signed_authentication_and_clears_failed_captures() -> Result<()> {
            let executable = std::env::current_exe()?;
            let output = Command::new("codesign")
                .arg("-dvvv")
                .arg(&executable)
                .output()?;
            ensure!(output.status.success(), "test signature inspection failed");
            let text = String::from_utf8(output.stderr)?;
            let hash = text
                .lines()
                .find_map(|line| line.strip_prefix("CDHash="))
                .context("test code has no cdhash")?;
            let correct = SignedPeer::new(
                CodeRequirement::code_directory_hash(hash.trim())?,
                &executable,
            )?;
            let slot = Rc::new(RefCell::new(None));
            let (stream, _other) = UnixStream::pair()?;
            let right = WatchingPeer {
                peer: correct,
                captured: slot.clone(),
            };
            right.verify(&stream, PeerEnd::Server)?;
            assert!(
                slot.borrow().is_some(),
                "genuine signed peer did not capture exit"
            );
            let wrong = WatchingPeer {
                peer: SignedPeer::new(
                    CodeRequirement::team_and_identifier("ABCDE12345", "org.example.wrong")?,
                    &executable,
                )?,
                captured: slot.clone(),
            };
            assert!(wrong.verify(&stream, PeerEnd::Server).is_err());
            assert!(
                slot.borrow().is_none(),
                "failed authentication retained old exit evidence"
            );
            right.verify(&stream, PeerEnd::Server)?;
            assert!(right.verify(&stream, PeerEnd::Client).is_err());
            assert!(
                slot.borrow().is_none(),
                "wrong endpoint retained exit evidence"
            );
            Ok(())
        }
    }
}
#[cfg(target_os = "macos")]
pub use native::{NativeConnection, NativeSupervisor};

#[cfg(test)]
mod tests;
