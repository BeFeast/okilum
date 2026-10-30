//! Registration of ALREADY prepared, explicitly opted-in state. This coordinator
//! owns its short transactions; it is intentionally not `Platform` and cannot be
//! plugged into the generic lifecycle controller that holds a transaction across
//! registration. No enrollment, repair/re-arm, fallback spawn or Stop is exposed.
use super::{recovery::ManagedStatus, Status};
use crate::sidecar::{
    discovery::Discovered,
    store::{StateDir, Store},
    Binding,
};
use anyhow::{ensure, Context, Result};
use std::time::{Duration, Instant};

/// Bundle/payload validation and authenticated socket observation, independently
/// of ServiceManagement registration status. No mutation is permitted here.
pub trait RegistrationProof {
    fn verify(&self, binding: &Binding, deadline: Instant) -> Result<()>;
    fn observe(&self, binding: &Binding, deadline: Instant) -> Result<Discovered>;
}
/// Native service effects. Production uses the private ServiceManagement port;
/// injected implementations let tests exercise lock boundaries without changing
/// host registration. This port never launches a child as a fallback.
pub trait RegistrationPort {
    fn status(&mut self, deadline: Instant) -> Result<Status>;
    fn register(&mut self, deadline: Instant) -> Result<()>;
}

pub struct RegistrationGuard<D: StateDir, P, A> {
    binding: Binding,
    store: Store<D>,
    proof: P,
    service: A,
    budget: Duration,
}
impl<D: StateDir, P: RegistrationProof, A: RegistrationPort> RegistrationGuard<D, P, A> {
    /// No creation of state or native effects. Call outside any store transaction.
    pub fn new(
        binding: Binding,
        store: Store<D>,
        proof: P,
        service: A,
        budget: Duration,
    ) -> Result<Self> {
        ensure!(
            !budget.is_zero(),
            "finite nonzero registration budget required"
        );
        Instant::now()
            .checked_add(budget)
            .context("registration budget overflow")?;
        Ok(Self {
            binding,
            store,
            proof,
            service,
            budget,
        })
    }
    /// Read-only and conservative: service registration is never liveness. Do not
    /// hold the journal lock while authenticating or awaiting the socket reply.
    pub fn status(&self) -> ManagedStatus {
        self.status_until(Instant::now() + self.budget)
    }
    fn status_until(&self, deadline: Instant) -> ManagedStatus {
        let attempt = || -> Result<ManagedStatus> {
            ensure!(Instant::now() < deadline, "registration deadline elapsed");
            let saved = {
                let tx = self.store.begin(deadline)?;
                tx.current()?
                    .context("recovery pending: missing journal")?
                    .clone()
            };
            ensure!(
                saved.binding() == &self.binding,
                "recovery pending: binding changed"
            );
            self.proof.verify(&self.binding, deadline)?;
            let peer = self.proof.observe(&self.binding, deadline)?;
            ensure!(Instant::now() < deadline, "registration deadline elapsed");
            let tx = self.store.begin(deadline)?;
            ensure!(
                tx.current()? == Some(&saved),
                "recovery pending: authority changed"
            );
            Ok(ManagedStatus::observe(&saved, &peer))
        };
        attempt().unwrap_or(ManagedStatus::RecoveryPending)
    }
    /// Only a fresh Ready intent AND NotRegistered permit the one dispatch. An
    /// existing registration without a live signed generation stays pending.
    /// Failures consume any committed dispatch; never retry/re-arm automatically.
    pub fn register_prepared(&mut self) -> Result<ManagedStatus> {
        self.register_until(None)
    }
    /// User action for the exact prepared generation displayed by the UI. This
    /// never prepares missing state, repairs uncertainty or creates permission.
    pub fn enable_confirmed(&mut self, generation: uuid::Uuid) -> Result<ManagedStatus> {
        self.register_until(Some(generation))
    }
    fn register_until(&mut self, confirmed: Option<uuid::Uuid>) -> Result<ManagedStatus> {
        let deadline = Instant::now() + self.budget;
        self.proof.verify(&self.binding, deadline)?;
        ensure!(Instant::now() < deadline, "registration deadline elapsed");
        if self.service.status(deadline)? != Status::NotRegistered {
            return Ok(self.status_until(deadline));
        }
        ensure!(Instant::now() < deadline, "registration deadline elapsed");
        let dispatch = match confirmed {
            Some(generation) => {
                self.store
                    .dispatch_confirmed_launch(&self.binding, generation, deadline)?
            }
            None => self.store.dispatch_launch(&self.binding, deadline)?,
        };
        // dispatch_launch has returned and dropped its transaction BEFORE any
        // validation/native call: the helper may now take the same lock to claim.
        dispatch.execute(|_| {
            self.proof.verify(&self.binding, deadline)?;
            ensure!(Instant::now() < deadline, "registration deadline elapsed");
            ensure!(
                self.service.status(deadline)? == Status::NotRegistered,
                "recovery pending: registration changed after dispatch"
            );
            self.service.register(deadline)
        })?;
        Ok(self.status_until(deadline))
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use crate::sidecar::{
        macos::{
            account_state_directory,
            bundle::{BundleGuard, NativeSignature},
            connection::NativeSupervisor,
            native::RegistrationService,
            payload::Payload,
        },
        store::{UnixDir, UnixStore},
        supervisor::ipc::code_requirement::CodeRequirement,
    };
    use std::path::Path;

    struct NativeProof {
        bundle: BundleGuard<NativeSignature>,
        payload: Payload,
        supervisor: NativeSupervisor,
        store: UnixStore,
    }
    impl RegistrationProof for NativeProof {
        fn verify(&self, binding: &Binding, deadline: Instant) -> Result<()> {
            ensure!(Instant::now() < deadline, "registration deadline elapsed");
            self.bundle.verify(binding)?;
            let selection = self
                .store
                .read_selection(deadline)?
                .context("no selected runtime")?;
            self.payload.verify_selected(binding, &selection)?;
            Ok(())
        }
        fn observe(&self, _: &Binding, deadline: Instant) -> Result<Discovered> {
            self.supervisor.inspect_until(deadline)
        }
    }
    /// Concrete production composition. The native service port is private and
    /// reachable only after durable dispatch. No generic Controller/outer lock.
    pub struct NativeRegistrationGuard(
        RegistrationGuard<UnixDir, NativeProof, RegistrationService>,
    );
    impl NativeRegistrationGuard {
        pub fn open(
            app: &Path,
            app_executable: &str,
            binding: Binding,
            payload: Payload,
            app_requirement: CodeRequirement,
            helper_requirement: CodeRequirement,
            budget: Duration,
        ) -> Result<Self> {
            // SMAppService operates on the calling app's bundle, not an app
            // argument. Never verify one installation then register another.
            ensure!(
                std::env::current_exe()?.canonicalize()?
                    == app
                        .join("Contents/MacOS")
                        .join(app_executable)
                        .canonicalize()?,
                "registration requires the calling app's own executable"
            );
            let root = account_state_directory()?;
            ensure!(
                Path::new(&binding.state_directory) == root,
                "recovery pending: registration requires the account state directory"
            );
            let store = UnixStore::open_existing(&root)?;
            let bundle = BundleGuard::new(
                app,
                app_executable,
                binding.clone(),
                app_requirement.clone(),
                helper_requirement.clone(),
                NativeSignature,
            )?;
            let supervisor = NativeSupervisor::open(
                app,
                app_executable,
                binding.clone(),
                app_requirement,
                helper_requirement,
                budget,
            )?;
            let proof = NativeProof {
                bundle,
                payload,
                supervisor,
                store: store.clone(),
            };
            Ok(Self(RegistrationGuard::new(
                binding,
                store,
                proof,
                RegistrationService,
                budget,
            )?))
        }
        pub fn register_prepared(&mut self) -> Result<ManagedStatus> {
            self.0.register_prepared()
        }
        pub fn enable_confirmed(&mut self, generation: uuid::Uuid) -> Result<ManagedStatus> {
            self.0.enable_confirmed(generation)
        }
        pub fn status(&self) -> ManagedStatus {
            self.0.status()
        }
    }
}
#[cfg(target_os = "macos")]
pub use native::NativeRegistrationGuard;

#[cfg(all(test, unix))]
mod tests;
