//! Native registration transport. The supervisor/verified-bundle boundary stays
//! explicit; this type alone cannot certify process ownership or reap a child.
use super::{Binding, SmApi, Status, PLIST};
use crate::sidecar::{
    authority::{launch::Dispatch, StopToken},
    supervisor::ipc::Scope,
};
use anyhow::{ensure, Result};
use objc2::rc::Retained;
use objc2_foundation::{NSProcessInfo, NSString};
use objc2_service_management::{SMAppService, SMAppServiceStatus};
use std::time::Instant;

/// Native supervisor implementation must authenticate its private IPC peer and
/// bind process identity to the saved installation/device, not only a PID.
pub trait OwnedSupervisor {
    fn verify_bundle(&mut self, binding: &Binding) -> Result<()>;
    fn verify_payload(&mut self, binding: &Binding) -> Result<()>;
    fn running(&mut self, binding: &Binding) -> Result<bool>;
    /// Atomically consume the fresh generation BEFORE native registration. No
    /// default permission: an unwired guard must never cause a launchd effect.
    /// Implementations must share the caller's transaction or run outside it,
    /// never nest a Store lock already held by a lifecycle controller.
    fn dispatch_registration(&mut self, _: &Binding) -> Result<Dispatch> {
        anyhow::bail!("recovery pending: native launch authority is not wired")
    }
    /// Await launchd's owned helper; never launch an unrelated child as fallback.
    fn await_running(&mut self, binding: &Binding) -> Result<()>;
    /// Request clean stop and await supervisor plus sidecar exit before success.
    fn stop_and_reap(&mut self, binding: &Binding) -> Result<()>;
    /// Authenticated live generation (`discovery::SupervisorLink`); default none.
    fn supervisor_scope(&mut self, _: &Binding) -> Result<Option<Scope>> {
        Ok(None)
    }
    fn stop_supervisor(&mut self, _: &Binding, _: &StopToken) -> Result<()> {
        anyhow::bail!("supervisor IPC is not wired on this guard")
    }
}

pub struct NativeSm<G> {
    guard: G,
}
impl<G: OwnedSupervisor> NativeSm<G> {
    /// No registration, process launch or filesystem effects.
    pub fn new(guard: G) -> Self {
        Self { guard }
    }
    fn service(&self, plist: &str) -> Result<Retained<SMAppService>> {
        service(plist)
    }
}
impl<G: OwnedSupervisor> SmApi for NativeSm<G> {
    fn major_version(&self) -> u32 {
        let version = NSProcessInfo::processInfo().operatingSystemVersion();
        u32::try_from(version.majorVersion).unwrap_or(0)
    }
    fn verify_bundle(&mut self, binding: &Binding, plist: &str) -> Result<()> {
        ensure!(
            plist == PLIST && self.major_version() >= 13,
            "unsupported Sync agent"
        );
        self.guard.verify_bundle(binding)
    }
    fn verify_payload(&mut self, binding: &Binding) -> Result<()> {
        self.guard.verify_payload(binding)
    }
    fn status(&mut self, plist: &str) -> Result<Status> {
        service_status(plist)
    }
    fn running(&mut self, binding: &Binding) -> Result<bool> {
        self.guard.running(binding)
    }
    fn register(&mut self, binding: &Binding, plist: &str) -> Result<()> {
        self.verify_bundle(binding, plist)?;
        self.verify_payload(binding)?;
        let service = self.service(plist)?;
        let dispatch = self.guard.dispatch_registration(binding)?;
        dispatch.execute(|_| {
            // An error/approval requirement consumes this attempt too. Neither
            // ServiceManagement status nor absence of a peer grants a retry.
            if let Err(error) = unsafe { service.registerAndReturnError() } {
                if self.status(plist)? != Status::RequiresApproval {
                    anyhow::bail!("ServiceManagement registration failed: {error}");
                }
            }
            Ok(())
        })
    }
    fn start_owned(&mut self, binding: &Binding) -> Result<()> {
        self.verify_bundle(binding, PLIST)?;
        ensure!(
            self.status(PLIST)? == Status::Enabled,
            "agent is not approved"
        );
        self.guard.await_running(binding)
    }
    fn stop_owned(&mut self, binding: &Binding) -> Result<()> {
        self.verify_bundle(binding, PLIST)?;
        self.guard.stop_and_reap(binding)
    }
    fn supervisor_scope(&mut self, binding: &Binding) -> Result<Option<Scope>> {
        self.guard.supervisor_scope(binding)
    }
    fn stop_supervisor(&mut self, binding: &Binding, token: &StopToken) -> Result<()> {
        self.guard.stop_supervisor(binding, token)
    }
    fn unregister(&mut self, binding: &Binding, plist: &str) -> Result<()> {
        self.verify_bundle(binding, plist)?;
        // Apple explicitly says unregister does not wait for a running service
        // to be reaped. Require the owned process tree to be stopped first.
        ensure!(
            !self.guard.running(binding)?,
            "owned helper is still running"
        );
        unsafe { self.service(plist)?.unregisterAndReturnError() }
            .map_err(|e| anyhow::anyhow!("ServiceManagement unregister failed: {e}"))?;
        ensure!(
            self.status(plist)? == Status::NotRegistered,
            "agent unregistration pending"
        );
        ensure!(
            !self.guard.running(binding)?,
            "owned helper exit not confirmed"
        );
        Ok(())
    }
}

fn service(plist: &str) -> Result<Retained<SMAppService>> {
    let version = NSProcessInfo::processInfo().operatingSystemVersion();
    ensure!(
        version.majorVersion >= 13,
        "Sync requires macOS 13 or later"
    );
    ensure!(plist == PLIST, "unexpected bundled agent");
    // Only our current app's fixed bundled plist, after checking runtime OS.
    Ok(unsafe { SMAppService::agentServiceWithPlistName(&NSString::from_str(plist)) })
}
fn service_status(plist: &str) -> Result<Status> {
    let status = unsafe { service(plist)?.status() };
    Ok(match status {
        SMAppServiceStatus::NotRegistered => Status::NotRegistered,
        SMAppServiceStatus::Enabled => Status::Enabled,
        SMAppServiceStatus::RequiresApproval => Status::RequiresApproval,
        SMAppServiceStatus::NotFound => Status::NotFound,
        _ => anyhow::bail!("unknown ServiceManagement status"),
    })
}

/// Not exported outside the macOS module. NativeRegistrationGuard alone composes
/// this raw effect port with the one-use journal dispatch capability.
pub(super) struct RegistrationService;
impl super::registration::RegistrationPort for RegistrationService {
    fn status(&mut self, deadline: Instant) -> Result<Status> {
        ensure!(Instant::now() < deadline, "registration deadline elapsed");
        service_status(PLIST)
    }
    fn register(&mut self, deadline: Instant) -> Result<()> {
        ensure!(Instant::now() < deadline, "registration deadline elapsed");
        let service = service(PLIST)?;
        // This native synchronous API has no cancellation interface; critically,
        // no journal lock is held across it, including approval/error handling.
        if let Err(error) = unsafe { service.registerAndReturnError() } {
            if service_status(PLIST)? != Status::RequiresApproval {
                anyhow::bail!("ServiceManagement registration failed: {error}");
            }
        }
        Ok(())
    }
}
