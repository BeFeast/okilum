//! One lease spans the entire Sync configuration operation, including rollback
//! and authenticated readback. A managed runtime additionally serializes with
//! its autonomous launch and records folder authority before any REST effect.
use crate::daemon::DaemonIdentity;
use anyhow::Result;
use std::path::Path;

pub(crate) struct Configuration {
    _operation: Option<okilum_write_authority::OperationGuard>,
    #[cfg(target_os = "linux")]
    _runtime: Option<crate::managed_launch::Configuration>,
}
pub(crate) fn configuration(
    daemon: &DaemonIdentity,
    folder: &str,
    root: &Path,
) -> Result<Configuration> {
    #[cfg(not(target_os = "linux"))]
    let _ = (daemon, folder);
    #[cfg(target_os = "linux")]
    let runtime = crate::managed_launch::configuration(daemon, folder, root)?;
    // Acquire after the runtime gate, just like launch. The operation remains
    // admitted if begin_drain closes admission during effects/readback.
    #[cfg(target_os = "linux")]
    let operation = if runtime.is_none() {
        Some(okilum_write_authority::operation(root)?)
    } else {
        None
    };
    #[cfg(not(target_os = "linux"))]
    let operation = Some(okilum_write_authority::operation(root)?);
    Ok(Configuration {
        _operation: operation,
        #[cfg(target_os = "linux")]
        _runtime: runtime,
    })
}
