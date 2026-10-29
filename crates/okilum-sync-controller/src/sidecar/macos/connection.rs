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
        let deadline = Instant::now() + self.budget;
        self.bundle.verify(&self.binding)?;
        {
            let tx = self.store.begin(deadline)?;
            let saved = tx.current()?.context("managed Sync journal disappeared")?;
            ensure!(
                saved.binding() == &self.binding,
                "saved supervisor binding changed"
            );
            saved.authorize(token, &token.operation.scope)?;
        }
        self.link.stop_until(&self.binding, token, deadline)
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
            code_requirement::CodeRequirement, macos_peer::SignedPeer, unix_transport::PeerCheck,
        },
    };
    use std::path::Path;

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
}
#[cfg(target_os = "macos")]
pub use native::NativeConnection;

#[cfg(test)]
mod tests;
