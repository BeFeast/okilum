//! Read-only verification of a retained Windows managed preparation. Launch,
//! installation and explicit Connect never infer authority from a config path.
use crate::sidecar::{authority::launch::Phase, store::WindowsStore, Intent};
use crate::{
    daemon::DaemonIdentity,
    managed_launch::{ManagedInstance, Reference},
    private,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Receipt {
    pub identity: DaemonIdentity,
    pub launch: Reference,
}
pub fn active_state_directory() -> Result<PathBuf> {
    Ok(
        PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA unavailable")?)
            .join("Okilum/Sync"),
    )
}
fn store(state: &Path) -> Result<WindowsStore> {
    let normal = |path: &Path| {
        path.to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_lowercase()
    };
    ensure!(
        normal(state) == normal(&active_state_directory()?),
        "Windows source requires its account-owned Sync state"
    );
    WindowsStore::open_existing(
        state
            .to_str()
            .context("Sync state path is not Unicode")?
            .trim_start_matches(r"\\?\"),
    )
}
pub(crate) fn managed_launch_reference(state: &Path) -> Result<Option<Reference>> {
    if !state.join("prepared-identity.json").try_exists()? {
        ensure!(
            !state.join("sidecar.json").try_exists()?,
            "managed preparation receipt missing; recovery required"
        );
        return Ok(None);
    }
    let receipt: Receipt =
        serde_json::from_slice(&private::read(&state.join("prepared-identity.json"))?)?;
    let selection = store(state)?
        .read_selection(Instant::now() + Duration::from_secs(5))?
        .context("managed runtime selection missing")?;
    let executable = selection.verify_file(state)?;
    super::payload::verify(
        &executable,
        &super::signature::AuthenticodePolicy::from_build()?,
    )?;
    receipt.launch.validate(&ManagedInstance {
        home: state.join("config").canonicalize()?,
        executable,
    })?;
    Ok(Some(receipt.launch))
}
pub(crate) fn managed_transport_identity(state: &Path) -> Result<DaemonIdentity> {
    let store = store(state)?;
    let saved = store
        .begin(Instant::now() + Duration::from_secs(5))?
        .current()?
        .context("Windows source launch receipt missing")?
        .clone();
    ensure!(
        saved.intent() == Intent::Enabled
            && saved.stop().is_none()
            && saved.update().is_none()
            && saved
                .launch_registration()
                .is_some_and(|launch| launch.phase() == Phase::Claimed),
        "Windows source requires a ready claimed Sync connection"
    );
    let receipt: Receipt =
        serde_json::from_slice(&private::read(&state.join("prepared-identity.json"))?)?;
    let binding = saved.binding();
    ensure!(
        binding.owner == super::security::current_sid()?
            && binding.device_identity == receipt.identity.device_id
            && receipt.identity.config_file == state.join("config/config.xml").canonicalize()?,
        "Windows source identity binding changed"
    );
    receipt.identity.verify_local()?;
    managed_launch_reference(state)?;
    Ok(receipt.identity)
}
pub fn managed_source_identity(state: &Path) -> Result<DaemonIdentity> {
    let identity = managed_transport_identity(state)?;
    identity.connect()?;
    Ok(identity)
}
