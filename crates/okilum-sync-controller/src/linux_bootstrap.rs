//! Explicit LAN bootstrap from a package-pinned runtime. Inventory/Settings reads
//! never call this module's Connect entry. No fallback to PATH or another daemon.
use crate::{
    daemon::{self, DaemonIdentity, Preparation},
    lifecycle::{Lifecycle, Systemd},
    private,
};
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub(crate) const BINARY_SHA: &str =
    "d72663e4f9efe0b8de5e1dacb13c1c8decc82e76623cf2fa47fd306f3e1d3a3e";
fn verify_runtime(path: &Path) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.mode() & 0o111 != 0
            && metadata.mode() & 0o022 == 0
            && (metadata.uid() == 0 || metadata.uid() == rustix::process::geteuid().as_raw())
            && metadata.len() <= 100_000_000,
        "unsafe packaged Sync runtime"
    );
    let mut digest = Sha256::new();
    let mut block = [0u8; 8192];
    loop {
        let n = file.read(&mut block)?;
        if n == 0 {
            break;
        }
        digest.update(&block[..n]);
    }
    ensure!(
        format!("{:x}", digest.finalize()) == BINARY_SHA,
        "packaged Sync runtime digest differs"
    );
    Ok(())
}
/// Conventional AppDir binary location, resolved from the running application,
/// never from PATH, HOME, a remote invitation, or caller-supplied executable.
pub fn packaged_runtime() -> Result<PathBuf> {
    let executable = std::env::current_exe()?.canonicalize()?;
    let bin = executable
        .parent()
        .context("application directory unavailable")?;
    let path = bin.join("../lib/okilum/syncthing/syncthing");
    verify_runtime(&path)?;
    Ok(path)
}
fn no_external(configs: &[PathBuf], owned: &Path) -> Result<()> {
    ensure!(
        configs.iter().all(|p| p == owned),
        "existing Sync configuration requires explicit connection selection"
    );
    let external: Vec<_> = configs
        .iter()
        .filter(|p| p.as_path() != owned)
        .cloned()
        .collect();
    let inventory = daemon::discover(&external);
    inventory.folders()?;
    ensure!(
        inventory
            .candidates
            .iter()
            .all(|c| c.identity.config_file == owned),
        "another Syncthing instance is already available"
    );
    Ok(())
}
/// Explicit user consent only. Retains preparation identity/ports on retry.
/// Returns a configuration only after the existing authenticated REST boundary
/// verifies the saved device and supported version. Never auto-selects an external home.
pub fn connect(
    confirmed: bool,
    state: &Path,
    units: &Path,
    configs: &[PathBuf],
    check: impl Fn() -> Result<()>,
) -> Result<DaemonIdentity> {
    ensure!(
        state.is_absolute() && units.is_absolute(),
        "absolute managed paths required"
    );
    ensure!(
        confirmed && okilum_sync::NetworkAccess::current().allowed(),
        "explicit Sync consent required"
    );
    ensure!(
        cfg!(target_arch = "x86_64"),
        "packaged Linux Sync architecture unavailable"
    );
    check()?;
    let owned_config = state.join("prepared/syncthing/config.xml");
    no_external(configs, &owned_config)?;
    let _lock = private::lock(state)?;
    let selection = state.join("connection.json");
    let request: Preparation = if selection.try_exists()? {
        serde_json::from_slice(&private::read(&selection)?)?
    } else {
        ensure!(
            !state.join("prepared").try_exists()?,
            "partial Sync preparation requires recovery"
        );
        let packaged = packaged_runtime()?;
        let runtime = state.join("syncthing");
        let mut input = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(packaged)?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o500)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&runtime)?;
        let copied = std::io::copy(
            &mut std::io::Read::by_ref(&mut input).take(100_000_001),
            &mut output,
        )?;
        ensure!(copied <= 100_000_000, "packaged runtime size changed");
        output.flush()?;
        output.sync_all()?;
        drop(output);
        verify_runtime(&runtime)?;
        let rest = TcpListener::bind("127.0.0.1:0")?;
        let lan = TcpListener::bind("0.0.0.0:0")?;
        let request = Preparation {
            executable: runtime,
            rest_address: rest.local_addr()?,
            listen_address: lan.local_addr()?,
        };
        private::write(&selection, &serde_json::to_vec(&request)?)?;
        request
    };
    ensure!(
        request.executable == state.join("syncthing"),
        "managed runtime path changed"
    );
    verify_runtime(&request.executable)?;
    check()?;
    let (instance, identity) = daemon::prepare_lan(&state.join("prepared"), &request)?;
    identity.verify_local()?;
    // Repeat complete discovery immediately before acquiring lifecycle authority.
    let fresh = crate::direct::configuration_paths()?;
    no_external(&fresh, &owned_config)?;
    ensure!(
        okilum_sync::NetworkAccess::current().allowed(),
        "Sync is disabled"
    );
    check()?;
    Lifecycle::new(state.join("service"), units.to_owned(), Systemd).enable(instance)?;
    identity.connect()?;
    check()?;
    Ok(identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_scope_rejects_other_configuration_before_discovery() {
        let owned = Path::new("/private/managed/config.xml");
        let external = vec![PathBuf::from("/external/config.xml")];
        let error = no_external(&external, owned).unwrap_err();
        assert!(error.to_string().contains("explicit connection selection"));
    }
    #[test]
    fn no_consent_or_revoked_access_never_creates_state() -> Result<()> {
        let root = tempfile::tempdir()?;
        let state = root.path().join("not-created");
        let units = root.path().join("not-registered");
        assert!(connect(false, &state, &units, &[], || Ok(())).is_err());
        assert!(!state.exists() && !units.exists());
        Ok(())
    }
}
