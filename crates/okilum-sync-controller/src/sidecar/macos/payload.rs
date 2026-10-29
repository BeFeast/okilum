//! Offline validation of an explicitly prepared managed payload. This is neither
//! preparation nor provenance: the trusted package layer supplies the selection.
use crate::{
    daemon::{DaemonIdentity, CLIENT_VERSION},
    sidecar::{store::Selection, supervisor::Launch, Binding},
};
use anyhow::{ensure, Result};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

pub struct Payload {
    binding: Binding,
    selection: Selection,
    identity: DaemonIdentity,
}
impl Payload {
    /// Keep the exact prepared receipt. A caller cannot substitute a new path,
    /// digest or device during re-enable; explicit preparation supplies a new one.
    pub fn new(binding: Binding, selection: Selection, identity: DaemonIdentity) -> Self {
        Self {
            binding,
            selection,
            identity,
        }
    }

    /// Registration must retain the exact selected receipt, not merely a valid
    /// binary left on disk after another selection has been committed.
    pub fn verify_selected(&self, binding: &Binding, selected: &Selection) -> Result<Launch> {
        ensure!(
            selected == &self.selection,
            "prepared runtime selection changed"
        );
        self.verify(binding)
    }

    /// Validate the same immutable selection and existing identity immediately
    /// before lifecycle effects. This never creates a certificate or starts REST.
    pub fn verify(&self, binding: &Binding) -> Result<Launch> {
        ensure!(binding == &self.binding, "prepared payload binding changed");
        ensure!(
            binding.owner == rustix::process::geteuid().as_raw().to_string(),
            "payload belongs to another user"
        );
        ensure!(
            !binding.installation.is_nil()
                && !binding.instance.is_nil()
                && !binding.device_identity.is_empty(),
            "prepared installation and device required"
        );
        ensure!(
            binding.device_identity == self.identity.device_id,
            "prepared device identity changed"
        );
        ensure!(
            [
                CLIENT_VERSION,
                CLIENT_VERSION.strip_prefix('v').unwrap_or(CLIENT_VERSION)
            ]
            .contains(&self.selection.version()),
            "unsupported prepared Syncthing version"
        );
        let state = Path::new(&binding.state_directory);
        ensure!(
            state.is_absolute()
                && state
                    .components()
                    .all(|c| !matches!(c, Component::ParentDir | Component::CurDir)),
            "plain absolute state path required"
        );
        let config = state.join("config");
        let data = state.join("data");
        ensure!(
            self.identity.config_file == config.join("config.xml"),
            "prepared configuration path changed"
        );
        let executable = self.selection.resolve(state)?;
        let parent = executable
            .parent()
            .ok_or_else(|| anyhow::anyhow!("runtime directory missing"))?;
        for directory in [&config, &data, parent] {
            self.private_directories(state, directory)?;
        }
        for (path, private) in [
            (self.identity.config_file.clone(), true),
            (config.join("key.pem"), true),
            (config.join("cert.pem"), false),
            (executable.clone(), false),
        ] {
            let metadata = fs::symlink_metadata(path)?;
            let forbidden = 0o7000 | if private { 0o077 } else { 0o022 };
            ensure!(
                metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.uid() == rustix::process::geteuid().as_raw()
                    && metadata.mode() & forbidden == 0,
                "prepared payload file is foreign, linked or has unsafe permissions"
            );
        }
        ensure!(
            fs::metadata(&executable)?.mode() & 0o111 != 0,
            "selected runtime is not executable"
        );
        self.selection.verify_file(state)?;
        self.identity.verify_local()?;
        let text = |path: PathBuf| -> Result<String> {
            path.into_os_string()
                .into_string()
                .map_err(|_| anyhow::anyhow!("prepared paths must be UTF-8"))
        };
        Ok(Launch {
            executable: text(executable)?,
            config: text(config)?,
            data: text(data)?,
        })
    }

    fn private_directories(&self, state: &Path, target: &Path) -> Result<()> {
        let relative = target.strip_prefix(state)?;
        let mut path = state.to_owned();
        let check = |path: &Path| -> Result<()> {
            let m = fs::symlink_metadata(path)?;
            ensure!(
                m.is_dir()
                    && !m.file_type().is_symlink()
                    && m.uid() == rustix::process::geteuid().as_raw()
                    && m.mode() & 0o7077 == 0,
                "private owned payload directory required"
            );
            Ok(())
        };
        check(&path)?;
        for component in relative.components() {
            ensure!(
                matches!(component, Component::Normal(_)),
                "plain runtime path required"
            );
            path.push(component);
            check(&path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
