//! Linux user-service ownership. The caller must prepare and verify a private
//! Syncthing home before enable; external installations never enter this path.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};
use uuid::Uuid;

/// Injectable boundary for headless failure/recovery tests.
pub trait UserServices {
    fn reload(&self) -> Result<()>;
    fn verify_definition(&self, name: &str, path: &Path) -> Result<()>;
    fn enable_start(&self, name: &str) -> Result<()>;
    fn disable_stop(&self, name: &str) -> Result<()>;
    fn verify_stopped(&self, name: &str) -> Result<()>;
}
pub struct Systemd;
impl Systemd {
    fn run(args: &[&str]) -> Result<()> {
        let output = Command::new("systemctl")
            .arg("--user")
            .args(args)
            .output()?;
        ensure!(
            output.status.success(),
            "user service operation failed: {}",
            String::from_utf8_lossy(&output.stderr[..output.stderr.len().min(4096)]).trim()
        );
        Ok(())
    }
}
impl UserServices for Systemd {
    fn verify_definition(&self, name: &str, path: &Path) -> Result<()> {
        for (property, expected) in [
            (
                "FragmentPath",
                path.to_str().context("unit path is not UTF-8")?,
            ),
            ("DropInPaths", ""),
        ] {
            let output = Command::new("systemctl")
                .args(["--user", "show", name, "--property", property, "--value"])
                .output()?;
            ensure!(
                output.status.success() && std::str::from_utf8(&output.stdout)?.trim() == expected,
                "user service definition was overridden"
            );
        }
        Ok(())
    }

    fn verify_stopped(&self, name: &str) -> Result<()> {
        for (property, permitted) in [
            ("ActiveState", &["inactive", "failed"][..]),
            ("UnitFileState", &["", "disabled", "not-found"][..]),
        ] {
            let output = Command::new("systemctl")
                .args(["--user", "show", name, "--property", property, "--value"])
                .output()?;
            let value = std::str::from_utf8(&output.stdout)?.trim();
            ensure!(
                output.status.success() && permitted.contains(&value),
                "owned service state needs reconciliation"
            );
        }
        Ok(())
    }

    fn reload(&self) -> Result<()> {
        Self::run(&["daemon-reload"])
    }
    fn enable_start(&self, name: &str) -> Result<()> {
        Self::run(&["enable", "--now", name])
    }
    fn disable_stop(&self, name: &str) -> Result<()> {
        Self::run(&["disable", "--now", name])
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedInstance {
    /// Dedicated prepared Syncthing home, outside the vault and disposable index.
    pub home: PathBuf,
    /// Absolute path from package discovery, never a shell command.
    pub executable: PathBuf,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    id: Uuid,
    instance: ManagedInstance,
    unit_directory: PathBuf,
    desired_enabled: bool,
    certificate_hash: String,
}
/// Creating/reading this handle has no filesystem or service side effects.
pub struct Lifecycle<S> {
    state: PathBuf,
    units: PathBuf,
    services: S,
}
impl<S: UserServices> Lifecycle<S> {
    pub fn new(state: PathBuf, units: PathBuf, services: S) -> Self {
        Self {
            state,
            units,
            services,
        }
    }
    /// Only called as a consequence of explicit Enable Sync. The journal is
    /// committed first so a interrupted registration can be retried on reopening.
    pub fn enable(&self, instance: ManagedInstance) -> Result<String> {
        ensure!(
            self.state.is_absolute() && self.units.is_absolute(),
            "absolute state paths required"
        );
        private_dir(&self.state)?;
        let _lock = self.lock()?;
        let instance = canonical_instance(instance)?;
        let mut journal = match self.load()? {
            Some(j) => {
                ensure!(
                    j.instance.home == instance.home
                        && j.instance.executable == instance.executable,
                    "managed identity location changed"
                );
                j
            }
            None => Journal {
                id: Uuid::new_v4(),
                certificate_hash: certificate_hash(&instance)?,
                instance,
                unit_directory: self.units.clone(),
                desired_enabled: false,
            },
        };
        self.check_binding(&journal)?;
        ensure!(
            certificate_hash(&journal.instance)? == journal.certificate_hash,
            "managed device identity changed"
        );
        self.check_unit(&journal)?;
        journal.desired_enabled = true;
        self.save(&journal)?;
        self.reconcile_locked(&journal)?;
        Ok(unit_name(&journal))
    }
    /// No journal means no owned service: disabling must not create anything.
    pub fn disable(&self) -> Result<()> {
        if !self.state.exists() {
            return Ok(());
        }
        let _lock = self.lock()?;
        let Some(mut journal) = self.load()? else {
            return Ok(());
        };
        self.check_binding(&journal)?;
        self.check_unit(&journal)?;
        journal.desired_enabled = false;
        self.save(&journal)?;
        self.reconcile_locked(&journal)
    }
    /// Retry a durable intent after interruption. Never call before explicit
    /// enrollment, and never infer ownership from a running process or unit name.
    pub fn reconcile(&self) -> Result<()> {
        if !self.state.exists() {
            return Ok(());
        }
        let _lock = self.lock()?;
        if let Some(journal) = self.load()? {
            self.check_binding(&journal)?;
            self.reconcile_locked(&journal)?;
        }
        Ok(())
    }
    pub fn desired_enabled(&self) -> Result<bool> {
        Ok(self.load()?.is_some_and(|j| j.desired_enabled))
    }
    fn check_binding(&self, journal: &Journal) -> Result<()> {
        ensure!(
            journal.unit_directory == self.units && !journal.id.is_nil(),
            "service ownership scope changed"
        );
        Ok(())
    }
    fn check_unit(&self, journal: &Journal) -> Result<Option<File>> {
        let path = self.units.join(unit_name(journal));
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let meta = file.metadata()?;
        ensure!(
            meta.is_file() && meta.uid() == rustix::process::geteuid().as_raw(),
            "service owner changed"
        );
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        ensure!(
            contents == unit_contents(journal)?,
            "service modified externally"
        );
        unchanged(&path, &file)?;
        Ok(Some(file))
    }
    fn reconcile_locked(&self, journal: &Journal) -> Result<()> {
        let exists = self.check_unit(journal)?;
        let name = unit_name(journal);
        let path = self.units.join(&name);
        if journal.desired_enabled {
            canonical_instance(journal.instance.clone())?;
            ensure!(
                certificate_hash(&journal.instance)? == journal.certificate_hash,
                "managed device identity changed"
            );
            fs::create_dir_all(&self.units)?;
            ensure!(
                fs::symlink_metadata(&self.units)?.is_dir(),
                "unit directory replaced"
            );
            if exists.is_none() {
                let temporary = self.units.join(format!(
                    ".okilum-unit-{}-{}.tmp",
                    journal.id,
                    Uuid::new_v4()
                ));
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&temporary)?;
                file.write_all(unit_contents(journal)?.as_bytes())?;
                file.sync_all()?;
                // Publish only a complete unit, without replacing a concurrent file.
                let publish = fs::hard_link(&temporary, &path);
                let _ = fs::remove_file(&temporary);
                publish?;
                File::open(&self.units)?.sync_all()?;
            }
            self.services.reload()?;
            let file = self
                .check_unit(journal)?
                .context("service disappeared before start")?;
            self.services.verify_definition(&name, &path)?;
            unchanged(&path, &file)?;
            self.services.enable_start(&name)?;
            unchanged(&path, &file)?;
            self.check_unit(journal)?;
            self.services.verify_definition(&name, &path)?;
        } else if let Some(file) = exists {
            // Do not delete the file if stopping failed. The durable disabled
            // intent remains available for retry; no successful UI receipt yet.
            self.services.reload()?;
            self.services.verify_definition(&name, &path)?;
            unchanged(&path, &file)?;
            self.services.disable_stop(&name)?;
            unchanged(&path, &file)?;
            self.check_unit(journal)?
                .context("service disappeared during stop")?;
            fs::remove_file(&path)?;
            File::open(&self.units)?.sync_all()?;
            self.services.reload()?;
            self.services.verify_stopped(&name)?;
        } else {
            // A prior stop + unlink may have completed before daemon-reload.
            // Absence of a unit file alone is not proof that its process stopped.
            self.services.reload()?;
            self.services.verify_stopped(&name)?;
        }
        Ok(())
    }
    fn lock(&self) -> Result<File> {
        private_dir(&self.state)?;
        let path = self.state.join("lifecycle.lock");
        if path.exists() {
            regular(&path)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(path)?;
        let meta = file.metadata()?;
        ensure!(
            meta.is_file()
                && meta.permissions().mode() & 0o077 == 0
                && meta.uid() == rustix::process::geteuid().as_raw(),
            "invalid lock ownership"
        );
        file.try_lock()
            .context("another Sync operation is running")?;
        sweep(&self.state, "lifecycle-")?;
        if self.units.exists() {
            if let Some(journal) = self.load()? {
                self.check_binding(&journal)?;
                sweep(&self.units, &format!(".okilum-unit-{}-", journal.id))?;
            }
        }
        Ok(file)
    }
    fn load(&self) -> Result<Option<Journal>> {
        let path = self.state.join("lifecycle.json");
        if !path.try_exists()? {
            return Ok(None);
        }
        regular(&path)?;
        let j = serde_json::from_slice(&fs::read(path)?)?;
        Ok(Some(j))
    }
    fn save(&self, journal: &Journal) -> Result<()> {
        let path = self.state.join(format!("lifecycle-{}.tmp", Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(&serde_json::to_vec(journal)?)?;
        file.sync_all()?;
        fs::rename(path, self.state.join("lifecycle.json"))?;
        File::open(&self.state)?.sync_all()?;
        Ok(())
    }
}
fn unchanged(path: &Path, file: &File) -> Result<()> {
    let opened = file.metadata()?;
    let current = fs::symlink_metadata(path)?;
    ensure!(
        current.is_file() && current.dev() == opened.dev() && current.ino() == opened.ino(),
        "service path replaced during operation"
    );
    Ok(())
}
fn sweep(directory: &Path, prefix: &str) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(id) = name
            .to_str()
            .and_then(|s| s.strip_prefix(prefix))
            .and_then(|s| s.strip_suffix(".tmp"))
        else {
            continue;
        };
        if Uuid::parse_str(id).is_ok() {
            regular(&entry.path())?;
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}
fn certificate_hash(instance: &ManagedInstance) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(fs::read(instance.home.join("cert.pem"))?)
    ))
}
fn unit_name(journal: &Journal) -> String {
    format!("okilum-syncthing-{}.service", journal.id)
}
fn canonical_instance(instance: ManagedInstance) -> Result<ManagedInstance> {
    ensure!(
        instance.home.is_absolute() && instance.executable.is_absolute(),
        "absolute instance paths required"
    );
    ensure!(
        instance.home.canonicalize()? == instance.home,
        "home must be canonical"
    );
    private_dir(&instance.home)?;
    for name in ["config.xml", "cert.pem", "key.pem"] {
        regular(&instance.home.join(name))?;
    }
    ensure!(
        instance.executable.is_file(),
        "Syncthing executable missing"
    );
    Ok(instance)
}
fn unit_contents(j: &Journal) -> Result<String> {
    Ok(format!(
        "# Owned by Okilum; identity {}\n[Unit]\nDescription=Okilum folder sync\nStartLimitIntervalSec=60\nStartLimitBurst=3\n\n[Service]\nType=simple\nExecStart={} serve --no-browser --no-restart --no-upgrade --home={}\nRestart=on-failure\nRestartSec=5\nUMask=0077\nEnvironment=STMONITORED=1\n\n[Install]\nWantedBy=default.target\n",
        j.id, quote(&j.instance.executable)?, quote(&j.instance.home)?
    ))
}
fn quote(path: &Path) -> Result<String> {
    let s = path.to_str().context("service path is not UTF-8")?;
    ensure!(!s.chars().any(char::is_control), "invalid service path");
    // systemd specifier and environment expansion apply even inside quotes.
    Ok(format!(
        "\"{}\"",
        s.replace('%', "%%")
            .replace('$', "$$")
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    ))
}
fn regular(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file()
            && !meta.file_type().is_symlink()
            && meta.uid() == rustix::process::geteuid().as_raw(),
        "state file must be regular"
    );
    ensure!(
        meta.permissions().mode() & 0o077 == 0,
        "state file must be private"
    );
    Ok(())
}
fn private_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink() && meta.permissions().mode() & 0o077 == 0,
        "state directory must be private"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    #[derive(Default)]
    struct Services {
        active: Cell<bool>,
        registered: Cell<bool>,
        fail_stop: Cell<bool>,
        fail_start: Cell<bool>,
        replace_on_start: RefCell<Option<PathBuf>>,
        calls: RefCell<Vec<String>>,
    }
    impl UserServices for &Services {
        fn verify_definition(&self, _: &str, _: &Path) -> Result<()> {
            Ok(())
        }

        fn verify_stopped(&self, _: &str) -> Result<()> {
            ensure!(
                !self.active.get() && !self.registered.get(),
                "still running or registered"
            );
            Ok(())
        }

        fn reload(&self) -> Result<()> {
            self.calls.borrow_mut().push("reload".into());
            Ok(())
        }
        fn enable_start(&self, name: &str) -> Result<()> {
            self.calls.borrow_mut().push(format!("start:{name}"));
            self.registered.set(true);
            ensure!(
                !self.fail_start.get(),
                "simulated start failure after registration"
            );
            self.active.set(true);
            if let Some(path) = self.replace_on_start.borrow_mut().take() {
                fs::remove_file(&path)?;
                fs::write(path, "external replacement")?;
            }
            Ok(())
        }
        fn disable_stop(&self, name: &str) -> Result<()> {
            ensure!(!self.fail_stop.get(), "simulated systemd outage");
            self.calls.borrow_mut().push(format!("stop:{name}"));
            self.registered.set(false);
            self.active.set(false);
            Ok(())
        }
    }
    fn instance(root: &Path) -> Result<ManagedInstance> {
        let home = root.join("home");
        private_dir(&home)?;
        for name in ["config.xml", "cert.pem", "key.pem"] {
            let mut f = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(home.join(name))?;
            f.write_all(name.as_bytes())?;
        }
        Ok(ManagedInstance {
            home,
            executable: PathBuf::from("/bin/true"),
        })
    }
    #[test]
    fn absent_enable_disable_reenable_preserves_identity_and_unregisters() -> Result<()> {
        let root = tempfile::tempdir()?;
        let services = Services::default();
        let state = root.path().join("state");
        let units = root.path().join("units");
        let lifecycle = Lifecycle::new(state.clone(), units.clone(), &services);
        assert!(!lifecycle.desired_enabled()?);
        lifecycle.disable()?;
        lifecycle.reconcile()?;
        assert!(!state.exists() && !units.exists() && services.calls.borrow().is_empty());
        let instance = instance(root.path())?;
        let id = lifecycle.enable(instance.clone())?;
        assert!(services.active.get() && services.registered.get() && units.join(&id).exists());
        let cert = fs::read(instance.home.join("cert.pem"))?;
        lifecycle.disable()?;
        assert!(!services.active.get() && !services.registered.get() && !units.join(&id).exists());
        assert_eq!(fs::read(instance.home.join("cert.pem"))?, cert);
        assert_eq!(lifecycle.enable(instance)?, id);
        lifecycle.disable()?;
        let reopened = Lifecycle::new(state, units, &services);
        reopened.reconcile()?;
        assert!(!services.active.get() && !services.registered.get());
        Ok(())
    }
    #[test]
    fn failed_stop_is_retried_and_modified_units_are_never_stopped() -> Result<()> {
        let root = tempfile::tempdir()?;
        let services = Services::default();
        let state = root.path().join("state");
        let units = root.path().join("units");
        let lifecycle = Lifecycle::new(state.clone(), units.clone(), &services);
        let instance = instance(root.path())?;
        let name = lifecycle.enable(instance)?;
        services.fail_stop.set(true);
        assert!(lifecycle.disable().is_err());
        assert!(!lifecycle.desired_enabled()? && services.active.get());
        assert!(units.join(&name).exists());
        services.fail_stop.set(false);
        Lifecycle::new(state, units.clone(), &services).reconcile()?;
        assert!(!services.active.get() && !units.join(&name).exists());
        let name = lifecycle.enable(ManagedInstance {
            home: root.path().join("home"),
            executable: "/bin/true".into(),
        })?;
        fs::write(units.join(name), "[Service]\nExecStart=/bin/false")?;
        assert!(lifecycle.disable().is_err());
        assert!(
            services.active.get(),
            "must not stop an externally changed unit"
        );
        Ok(())
    }
    #[test]
    fn interrupted_enable_retries_the_same_owned_unit() -> Result<()> {
        let root = tempfile::tempdir()?;
        let services = Services::default();
        let units = root.path().join("units");
        let state = root.path().join("state");
        let controller = Lifecycle::new(state.clone(), units.clone(), &services);
        services.fail_start.set(true);
        assert!(controller.enable(instance(root.path())?).is_err());
        assert!(controller.desired_enabled()?);
        assert!(services.registered.get() && !services.active.get());
        let unit = fs::read_dir(&units)?.next().unwrap()?.file_name();
        services.fail_start.set(false);
        let reopened = Lifecycle::new(state, units.clone(), &services);
        reopened.reconcile()?;
        assert!(services.active.get());
        assert_eq!(fs::read_dir(&units)?.count(), 1);
        assert!(units.join(unit).exists());
        reopened.disable()?;
        Ok(())
    }
    #[test]
    fn replaced_identity_and_missing_active_unit_fail_closed() -> Result<()> {
        let root = tempfile::tempdir()?;
        let services = Services::default();
        let units = root.path().join("units");
        let lifecycle = Lifecycle::new(root.path().join("state"), units.clone(), &services);
        let instance = instance(root.path())?;
        let name = lifecycle.enable(instance.clone())?;
        lifecycle.disable()?;
        fs::write(instance.home.join("cert.pem"), "different identity")?;
        assert!(lifecycle.enable(instance).is_err());
        assert!(!services.active.get());
        services.active.set(true);
        assert!(
            lifecycle.disable().is_err(),
            "missing unit file must not claim a running service stopped"
        );
        assert!(!units.join(name).exists());
        Ok(())
    }
    #[test]
    fn replacement_during_start_is_detected_and_symlink_lock_rejected() -> Result<()> {
        let root = tempfile::tempdir()?;
        let services = Services::default();
        let state = root.path().join("state");
        let units = root.path().join("units");
        let controller = Lifecycle::new(state.clone(), units.clone(), &services);
        let instance = instance(root.path())?;
        let name = controller.enable(instance.clone())?;
        *services.replace_on_start.borrow_mut() = Some(units.join(name));
        assert!(controller.enable(instance.clone()).is_err());
        assert!(
            controller.disable().is_err(),
            "foreign replacement must not be removed"
        );
        let second = root.path().join("second");
        private_dir(&second)?;
        let sentinel = root.path().join("sentinel");
        fs::write(&sentinel, "external")?;
        std::os::unix::fs::symlink(&sentinel, second.join("lifecycle.lock"))?;
        let other = Lifecycle::new(second, units, &services);
        assert!(other.enable(instance).is_err());
        assert_eq!(fs::read_to_string(sentinel)?, "external");
        Ok(())
    }
    #[test]
    fn service_arguments_escape_systemd_expansion() -> Result<()> {
        assert_eq!(
            quote(Path::new("/tmp/vault %n $HOME"))?,
            "\"/tmp/vault %%n $$HOME\""
        );
        assert!(quote(Path::new("/tmp/line\nbreak")).is_err());
        Ok(())
    }
}
