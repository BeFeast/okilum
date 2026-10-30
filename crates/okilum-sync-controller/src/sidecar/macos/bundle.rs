//! Read-only bundle ownership prerequisite for the native SMAppService guard.
//! This does not register, start, stop, or adopt a moved installation.
use super::{bundled_plist, Binding, PLIST};
use crate::sidecar::supervisor::ipc::code_requirement::CodeRequirement;
use anyhow::{ensure, Context, Result};
use std::{
    fs::{self, File},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
};

pub const HELPER: &str = "Contents/MacOS/okilum-sync-supervisor";
pub const SYNCTHING: &str = "Contents/MacOS/syncthing";
const SYNCTHING_PROVENANCE: &str = "Contents/Resources/Syncthing/provenance.json";

// System-wide installs are root-owned; per-user installs belong to this account.
// Read-only mode bits do not make a different file owner's code trustworthy.
fn trusted_owner(owner: u32, current: u32) -> bool {
    owner == 0 || owner == current
}

/// No permissive default: production must evaluate the supplied requirement
/// with Security.framework. Tests inject a recording/refusing verifier.
pub trait SignatureVerifier {
    fn verify(&self, code: &Path, requirement: &CodeRequirement) -> Result<()>;
}

pub struct BundleGuard<V> {
    root: PathBuf,
    binding: Binding,
    app_executable: String,
    app_requirement: CodeRequirement,
    helper_requirement: CodeRequirement,
    verifier: V,
}
impl<V: SignatureVerifier> BundleGuard<V> {
    /// The preparation layer can select only this runtime inside the verified
    /// installed app, never PATH or an arbitrary download. No process is started.
    /// The returned digest is of the Developer-ID-signed bytes, not the upstream
    /// unsigned payload; retain this provenance when copying into private state.
    pub fn bundled_runtime(
        &self,
        binding: &Binding,
        runtime_requirement: &CodeRequirement,
    ) -> Result<(PathBuf, String, Vec<u8>)> {
        use sha2::{Digest, Sha256};
        self.verify(binding)?;
        self.inspect(Path::new(SYNCTHING), false)?;
        self.inspect(Path::new(SYNCTHING_PROVENANCE), false)?;
        let runtime = self.root.join(SYNCTHING);
        self.verifier.verify(&runtime, runtime_requirement)?;
        let provenance = fs::read(self.root.join(SYNCTHING_PROVENANCE))?;
        let parsed: serde_json::Value = serde_json::from_slice(&provenance)?;
        ensure!(
            parsed["version"] == "2.1.6" && parsed["signed"] == true,
            "untrusted bundled runtime provenance"
        );
        let hash = format!("{:x}", Sha256::digest(fs::read(&runtime)?));
        ensure!(
            parsed["signed_binary_sha256"].as_str() == Some(hash.as_str()),
            "bundled runtime digest changed"
        );
        Ok((runtime, hash, provenance))
    }

    /// The current app supplies its own bundle, never one discovered by label.
    /// Resolution reads only; no state, registration, or signature is created.
    pub fn new(
        root: &Path,
        app_executable: &str,
        binding: Binding,
        app_requirement: CodeRequirement,
        helper_requirement: CodeRequirement,
        verifier: V,
    ) -> Result<Self> {
        ensure!(
            !binding.installation.is_nil()
                && !binding.instance.is_nil()
                && !binding.device_identity.is_empty(),
            "prepared installation and device identity required"
        );
        ensure!(root.is_absolute(), "absolute bundle path required");
        ensure!(
            !app_executable.is_empty()
                && app_executable.len() <= 255
                && app_executable != "okilum-sync-supervisor"
                && app_executable != "."
                && app_executable != ".."
                && app_executable
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_')),
            "invalid app executable name"
        );
        ensure!(
            root.components()
                .all(|c| !matches!(c, Component::ParentDir | Component::CurDir)),
            "plain bundle path required"
        );
        let root = root.canonicalize().context("bundle is unavailable")?;
        ensure!(
            root.extension().is_some_and(|e| e == "app"),
            "app bundle required"
        );
        Ok(Self {
            root,
            binding,
            app_executable: app_executable.to_owned(),
            app_requirement,
            helper_requirement,
            verifier,
        })
    }

    /// Recheck before every native registration/lifecycle mutation. A successful
    /// check is an observation, not an atomic lease over a replaceable app bundle.
    pub fn verify(&self, binding: &Binding) -> Result<()> {
        ensure!(binding == &self.binding, "prepared bundle binding changed");
        ensure!(
            binding.owner == rustix::process::geteuid().as_raw().to_string(),
            "bundle binding belongs to another user"
        );
        let helper = self.root.join(HELPER);
        ensure!(
            Path::new(&binding.supervisor) == helper,
            "binding names another bundle or supervisor"
        );
        ensure!(
            Path::new(&binding.state_directory).is_absolute(),
            "absolute state path required"
        );
        let state = Path::new(&binding.state_directory)
            .canonicalize()
            .context("private state is unavailable")?;
        ensure!(
            !state.starts_with(&self.root),
            "state must be outside the bundle"
        );
        self.inspect(Path::new(""), true)?;
        let app = PathBuf::from("Contents/MacOS").join(&self.app_executable);
        for relative in [app.as_path(), Path::new(HELPER)] {
            self.inspect(relative, false)?;
            ensure!(
                fs::metadata(self.root.join(relative))?.mode() & 0o111 != 0,
                "bundle code is not executable"
            );
        }
        let plist = PathBuf::from("Contents/Library/LaunchAgents").join(PLIST);
        self.inspect(&plist, false)?;
        let mut options = File::options();
        options.read(true).custom_flags(libc::O_NOFOLLOW);
        let mut bytes = vec![];
        options
            .open(self.root.join(plist))?
            .take(65537)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes == bundled_plist(HELPER)?.as_bytes(),
            "bundled agent definition changed"
        );
        // Verify the outer resource seal as well as the dedicated helper identity.
        // A valid helper alone cannot authorize an unsigned/modified outer app.
        self.verifier.verify(&self.root, &self.app_requirement)?;
        self.verifier.verify(&helper, &self.helper_requirement)
    }

    fn inspect(&self, relative: &Path, directory: bool) -> Result<()> {
        let mut path = self.root.clone();
        let check = |path: &Path, directory: bool| -> Result<()> {
            let m = fs::symlink_metadata(path)?;
            ensure!(
                !m.file_type().is_symlink() && if directory { m.is_dir() } else { m.is_file() },
                "bundle entry is missing, linked or has the wrong type"
            );
            ensure!(
                trusted_owner(m.uid(), rustix::process::geteuid().as_raw()),
                "bundle entry belongs to another user"
            );
            ensure!(
                m.mode() & 0o022 == 0,
                "bundle entry is writable by other users"
            );
            Ok(())
        };
        check(&path, true)?;
        let mut parts = relative.components().peekable();
        while let Some(part) = parts.next() {
            ensure!(
                matches!(part, Component::Normal(_)),
                "plain bundle-relative path required"
            );
            path.push(part);
            check(&path, parts.peek().is_some() || directory)?;
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
pub struct NativeSignature;
#[cfg(target_os = "macos")]
impl SignatureVerifier for NativeSignature {
    fn verify(&self, code: &Path, requirement: &CodeRequirement) -> Result<()> {
        crate::sidecar::supervisor::ipc::macos_peer::verify_static_code(code, requirement)
    }
}

#[cfg(test)]
mod tests;
