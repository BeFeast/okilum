//! Native registration transport. The supervisor/verified-bundle boundary stays
//! explicit; this type alone cannot certify process ownership or reap a child.
use super::{Binding, SmApi, Status, PLIST};
use crate::sidecar::{authority::StopToken, supervisor::ipc::Scope};
use anyhow::{ensure, Result};
use objc2::rc::Retained;
use objc2_foundation::{NSProcessInfo, NSString};
use objc2_service_management::{SMAppService, SMAppServiceStatus};

/// Native supervisor implementation must authenticate its private IPC peer and
/// bind process identity to the saved installation/device, not only a PID.
pub trait OwnedSupervisor {
    fn verify_bundle(&mut self, binding: &Binding) -> Result<()>;
    fn verify_payload(&mut self, binding: &Binding) -> Result<()>;
    fn running(&mut self, binding: &Binding) -> Result<bool>;
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
        ensure!(
            self.major_version() >= 13,
            "Sync requires macOS 13 or later"
        );
        ensure!(plist == PLIST, "unexpected bundled agent");
        // SMAppService is referenced only after checking the runtime OS version.
        // The selector uses our fixed plist in the caller's own signed bundle.
        Ok(unsafe { SMAppService::agentServiceWithPlistName(&NSString::from_str(plist)) })
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
        // The retained service lives through the message send.
        let status = unsafe { self.service(plist)?.status() };
        Ok(match status {
            SMAppServiceStatus::NotRegistered => Status::NotRegistered,
            SMAppServiceStatus::Enabled => Status::Enabled,
            SMAppServiceStatus::RequiresApproval => Status::RequiresApproval,
            SMAppServiceStatus::NotFound => Status::NotFound,
            _ => anyhow::bail!("unknown ServiceManagement status"),
        })
    }
    fn running(&mut self, binding: &Binding) -> Result<bool> {
        self.guard.running(binding)
    }
    fn register(&mut self, binding: &Binding, plist: &str) -> Result<()> {
        self.verify_bundle(binding, plist)?;
        self.verify_payload(binding)?;
        let service = self.service(plist)?;
        // No error text is parsed as authorization. A user-approval requirement
        // is accepted only if independently reported by ServiceManagement.
        if let Err(error) = unsafe { service.registerAndReturnError() } {
            if self.status(plist)? != Status::RequiresApproval {
                anyhow::bail!("ServiceManagement registration failed: {error}");
            }
        }
        Ok(())
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
