//! SMAppService adapter for macOS 13+. No legacy LaunchAgent installation.
//! The native bridge must bind this handle to one signed installed .app and
//! verify its bundled plist/helper before reporting ownership.
use super::{
    authority::StopToken, safe_text, supervisor::ipc::Scope, xml, Binding, Platform, Registration,
};
use anyhow::{ensure, Result};
pub const PLIST: &str = "uk.oklabs.tessera.sync.plist";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    NotRegistered,
    Enabled,
    RequiresApproval,
    NotFound,
}
pub trait SmApi {
    fn major_version(&self) -> u32;
    /// Check UID, installation identity, bundle path/signature, plist and helper.
    /// A copied/moved/changed bundle needs explicit recovery, never adoption.
    fn verify_bundle(&mut self, binding: &Binding, plist: &str) -> Result<()>;
    fn verify_payload(&mut self, binding: &Binding) -> Result<()>;
    fn status(&mut self, plist: &str) -> Result<Status>;
    fn running(&mut self, binding: &Binding) -> Result<bool>;
    fn register(&mut self, binding: &Binding, plist: &str) -> Result<()>;
    fn start_owned(&mut self, binding: &Binding) -> Result<()>;
    fn stop_owned(&mut self, binding: &Binding) -> Result<()>;
    /// Await SMAppService's unregister completion, not merely dispatch its call.
    fn unregister(&mut self, binding: &Binding, plist: &str) -> Result<()>;
}
pub struct SmAppService<A>(pub A);
/// Static bundle resource. Writing it at build time does not register an agent.
/// The supervisor locates owner-private Application Support state itself;
/// credentials and user-dependent state paths never enter the bundle plist.
pub fn bundled_plist(relative_supervisor: &str) -> Result<String> {
    safe_text(relative_supervisor)?;
    ensure!(
        relative_supervisor.starts_with("Contents/MacOS/")
            && !relative_supervisor
                .split('/')
                .any(|s| s == ".." || s == "." || s.is_empty()),
        "helper must be in the bundle code directory"
    );
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>Label</key><string>uk.oklabs.tessera.sync</string><key>BundleProgram</key><string>{}</string><key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict><key>ThrottleInterval</key><integer>30</integer><key>ProcessType</key><string>Background</string></dict></plist>"#,
        xml(relative_supervisor)
    ))
}
impl<A: SmApi> SmAppService<A> {
    fn verify(&mut self, binding: &Binding) -> Result<()> {
        ensure!(
            self.0.major_version() >= 13,
            "Sync requires macOS 13 or later; Reader remains available"
        );
        safe_text(&binding.owner)?;
        ensure!(
            binding.owner.bytes().all(|b| b.is_ascii_digit()),
            "numeric owner UID required"
        );
        for path in [&binding.supervisor, &binding.state_directory] {
            safe_text(path)?;
            ensure!(
                path.starts_with('/') && !path.split('/').any(|part| part == "." || part == ".."),
                "absolute macOS path required"
            );
        }
        self.0.verify_bundle(binding, PLIST)
    }
}
impl<A: SmApi> Platform for SmAppService<A> {
    fn inspect(&mut self, binding: &Binding) -> Result<Registration> {
        self.verify(binding)?;
        Ok(match self.0.status(PLIST)? {
            Status::NotRegistered => Registration::Absent,
            Status::RequiresApproval => Registration::ApprovalRequired,
            Status::NotFound => anyhow::bail!("bundled Sync helper is unavailable"),
            Status::Enabled => {
                if self.0.running(binding)? {
                    Registration::Running
                } else {
                    Registration::Stopped
                }
            }
        })
    }
    fn verify_payload(&mut self, binding: &Binding) -> Result<()> {
        self.verify(binding)?;
        self.0.verify_payload(binding)
    }
    fn register(&mut self, binding: &Binding) -> Result<()> {
        ensure!(
            self.inspect(binding)? == Registration::Absent,
            "agent already registered"
        );
        self.0.register(binding, PLIST)
    }
    fn start(&mut self, binding: &Binding) -> Result<()> {
        ensure!(
            self.inspect(binding)? == Registration::Stopped,
            "agent is not ready to start"
        );
        self.0.start_owned(binding)
    }
    fn stop(&mut self, binding: &Binding) -> Result<()> {
        if self.inspect(binding)? != Registration::Absent {
            self.0.stop_owned(binding)?;
        }
        Ok(())
    }
    /// No authenticated supervisor endpoint is wired on this adapter yet, so the
    /// controller stops natively under its lock instead of sending a token.
    fn supervisor_scope(&mut self, _: &Binding) -> Result<Option<Scope>> {
        Ok(None)
    }
    fn stop_supervisor(&mut self, _: &Binding, _: &StopToken) -> Result<()> {
        anyhow::bail!("supervisor IPC is not wired on this adapter")
    }
    fn unregister(&mut self, binding: &Binding) -> Result<()> {
        let state = self.inspect(binding)?;
        if state == Registration::Absent {
            return Ok(());
        }
        ensure!(
            state != Registration::Running,
            "owned process must stop before unregister"
        );
        self.0.unregister(binding, PLIST)
    }
}

#[cfg(target_os = "macos")]
pub mod native;
