//! Read-only verification of a retained Windows managed preparation. Launch,
//! installation and explicit Connect never infer authority from a config path.
use crate::sidecar::{store::WindowsStore, Intent};
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
            .join("Okilum")
            .join("Sync"),
    )
}
fn selected_state(state: &Path) -> Result<PathBuf> {
    let text = state.to_str().context("Sync state path is not Unicode")?;
    let normal = text.strip_prefix(r"\\?\").unwrap_or(text);
    super::path(normal)?;
    let selected = active_state_directory()?;
    let selected_text = selected
        .to_str()
        .context("Sync state path is not Unicode")?;
    super::path(selected_text)?;
    if !normal.eq_ignore_ascii_case(selected_text) {
        ensure!(
            okilum_write_authority::filesystem_identity::read(Path::new(normal))?
                == okilum_write_authority::filesystem_identity::read(&selected)?,
            "Windows source requires its account-owned Sync state"
        );
    }
    Ok(selected)
}

fn store(state: &Path) -> Result<WindowsStore> {
    let selected = selected_state(state)?;
    WindowsStore::open_existing(
        selected
            .to_str()
            .context("Sync state path is not Unicode")?,
    )
}

pub(crate) fn managed_launch_reference(state: &Path) -> Result<Option<Reference>> {
    let selected = selected_state(state)?;
    let state = selected.as_path();
    let expected = if state.join("sidecar.json").try_exists()? {
        let store = store(state)?;
        let transaction = store.begin(Instant::now() + Duration::from_secs(5))?;
        transaction
            .current()?
            .and_then(|current| current.windows_launch())
            .cloned()
    } else {
        None
    };
    if !state.join("prepared-identity.json").try_exists()? {
        ensure!(
            expected.is_none()
                && !state.join("preparation.json").try_exists()?
                && !state.join("managed-launch").try_exists()?,
            "managed preparation receipt missing; recovery required"
        );
        // Exact legacy compatibility only; absence grants no first-merge fence.
        return Ok(None);
    }
    let receipt: Receipt =
        serde_json::from_slice(&private::read(&state.join("prepared-identity.json"))?)?;
    ensure!(
        expected
            .as_ref()
            .is_none_or(|saved| saved == &receipt.launch),
        "prepared Windows launch reference differs from its journal"
    );
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
        executable: executable.canonicalize()?,
    })?;
    Ok(Some(receipt.launch))
}
pub(crate) fn managed_transport_identity(state: &Path) -> Result<DaemonIdentity> {
    let selected = selected_state(state)?;
    let state = selected.as_path();
    let store = store(state)?;
    let saved = store
        .begin(Instant::now() + Duration::from_secs(5))?
        .current()?
        .context("Windows source launch receipt missing")?
        .clone();
    ensure!(
        saved.intent() == Intent::Enabled && saved.stop().is_none() && saved.update().is_none(),
        "Windows source requires an enabled Sync connection without pending stop/update"
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
    crate::managed_launch::live_generation(
        identity
            .config_file
            .parent()
            .context("native runtime home missing")?,
    )?;
    Ok(identity)
}

/// Explicit Connect only. Preparation is offline; the task's start occurs after
/// releasing the state transaction so the supervisor can read its durable intent.
/// Any uncertainty stays visible and cannot authorize source enrollment.
pub fn connect_current_app(confirmed: bool) -> Result<DaemonIdentity> {
    use crate::sidecar::{authority::Envelope, Platform, Registration};
    ensure!(confirmed, "explicit Connect consent required");
    ensure!(
        okilum_sync::NetworkAccess::current().allowed(),
        "Sync is disabled"
    );
    let sources = super::preparation::Sources::installed()?;
    let state = active_state_directory()?;
    let prepared =
        super::preparation::prepare(&state, &sources, Instant::now() + Duration::from_secs(90))?;
    let store = store(&state)?;
    let guard = super::guard::Guard::new(&prepared, &sources, store.clone(), &state)?;
    let mut platform = super::TaskScheduler(super::native::NativeTasks::connect(guard)?);
    let deadline = Instant::now() + Duration::from_secs(30);
    {
        let mut transaction = store.begin(deadline)?;
        let next = match transaction.current()? {
            None => Envelope::first(prepared.binding.clone()),
            Some(saved) => {
                ensure!(
                    saved.binding() == &prepared.binding && saved.intent() != Intent::Removed,
                    "existing native Sync identity changed or was removed; recovery required"
                );
                saved.enable()?
            }
        };
        platform.verify_payload(&prepared.binding)?;
        transaction.commit(next.bind_windows_launch(prepared.launch.clone())?)?;
        if platform.inspect(&prepared.binding)? == Registration::Absent {
            platform.register(&prepared.binding)?;
        }
    }
    ensure!(
        okilum_sync::NetworkAccess::current().allowed(),
        "Sync was disabled before task launch"
    );
    match platform.inspect(&prepared.binding)? {
        Registration::Stopped => {
            {
                let transaction = store.begin(deadline)?;
                let current = transaction
                    .current()?
                    .context("native Sync intent disappeared")?;
                ensure!(
                    current.binding() == &prepared.binding
                        && current.intent() == Intent::Enabled
                        && current.stop().is_none()
                        && current.update().is_none(),
                    "native Sync launch was cancelled or changed"
                );
                platform.verify_payload(&prepared.binding)?;
            }
            // No instance lock is held across the native dispatch/readiness wait.
            platform.start(&prepared.binding)?;
        }
        Registration::Running => (),
        _ => anyhow::bail!("native task registration is not ready"),
    }
    loop {
        if Instant::now() >= deadline {
            if let Some(problem) = store
                .read_supervisor_failure()?
                .filter(|text| !text.is_empty())
            {
                anyhow::bail!("Sync helper failed before readiness: {problem}");
            }
            anyhow::bail!("Sync is still starting; retry Connect to inspect the retained instance");
        }
        let transaction = store.begin(deadline)?;
        let current = transaction
            .current()?
            .context("native Sync intent disappeared")?;
        ensure!(
            current.binding() == &prepared.binding
                && current.intent() == Intent::Enabled
                && current.stop().is_none()
                && current.update().is_none(),
            "native Sync launch was cancelled or changed"
        );
        drop(transaction);
        if platform.inspect(&prepared.binding)? == Registration::Running {
            let identity = managed_source_identity(&state)?;
            ensure!(
                identity == prepared.identity,
                "native ready identity changed"
            );
            return Ok(identity);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The existing sidecar controller owns revision-bound Disable/Stop/unregister.
/// Stop succeeds only after the signed reply and captured supervisor exit.
pub fn disable_current_app() -> Result<()> {
    let state = active_state_directory()?;
    if !state.try_exists()? {
        return Ok(());
    }
    let sources = super::preparation::Sources::installed()?;
    let prepared =
        super::preparation::prepare(&state, &sources, Instant::now() + Duration::from_secs(30))?;
    let store = store(&state)?;
    let guard = super::guard::Guard::new(&prepared, &sources, store.clone(), &state)?;
    let platform = super::TaskScheduler(super::native::NativeTasks::connect(guard)?);
    let mut controller = crate::sidecar::Controller::new(store, platform);
    ensure!(
        controller.disable()? == crate::sidecar::State::Disabled,
        "Sync disable remains recovery pending"
    );
    Ok(())
}

#[cfg(test)]
mod signed_task_tests;
