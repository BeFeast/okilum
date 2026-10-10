//! Staging of the supervisor outside the application folder (design:
//! docs/sync-supervisor-shape.md, "Windows updates"). Velopack replaces `current` on
//! every update and a background process running from it would block update and
//! uninstall (#974), so the package carries the helper and Enable copies it to a
//! versioned folder under the instance's private state. The copy is digest-verified,
//! never overwrites a different file of the same version, and a new app release stages
//! a new version beside the old one (the old one is stopped through the normal Stop).
//! Registration of the staged path is the caller's, after this returns.
use super::store::version_label_ok;
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub struct Staged {
    /// The staged executable: `<root>/supervisor/<version>/<file name>`.
    pub path: PathBuf,
    /// Lower-case hex SHA-256 of its content, to record and to check before each start.
    pub digest: String,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn digest_of(file: &mut File) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(hex(&hasher.finalize()));
        }
        hasher.update(&buffer[..read]);
    }
}

/// The root must be a private directory of the current user. On Windows that is the
/// protected owner-only DACL; elsewhere an owner-only mode.
fn ensure_private(root: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let text = root
            .to_str()
            .context("the staging root must be valid UTF-8")?;
        super::windows::private::PrivateDirectory::inspect(text)?;
        Ok(())
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(root)?;
        ensure!(
            metadata.is_dir()
                && metadata.uid() == rustix::process::geteuid().as_raw()
                && metadata.mode() & 0o077 == 0,
            "the staging root must be a private user-owned directory"
        );
        Ok(())
    }
}

/// Copy `payload` (the helper inside the application folder) to
/// `<root>/supervisor/<version>/<file name>`.
///
/// `root` must be a private directory outside the application folder: it may not lie
/// inside the folder that holds `payload` and may have no `current` component, because
/// that is the folder the updater replaces.
pub fn stage_supervisor(payload: &Path, root: &Path, version: &str) -> Result<Staged> {
    ensure!(
        version_label_ok(version),
        "invalid supervisor version label"
    );
    ensure!(root.is_absolute(), "the staging root must be absolute");
    ensure!(
        !root
            .components()
            .any(|c| matches!(c, Component::Normal(name)
            if name.to_string_lossy().eq_ignore_ascii_case("current"))),
        "the staging root is inside an application `current` folder"
    );
    let source_dir = std::fs::canonicalize(payload)
        .context("the packaged supervisor is missing")?
        .parent()
        .context("the packaged supervisor has no folder")?
        .to_path_buf();
    ensure!(
        !std::fs::canonicalize(root)
            .context("the staging root does not exist")?
            .starts_with(&source_dir),
        "the staging root is inside the application folder"
    );
    ensure_private(root)?;
    let name = payload
        .file_name()
        .context("the packaged supervisor has no file name")?;
    let directory = root.join("supervisor").join(version);
    std::fs::create_dir_all(&directory)?;
    let destination = directory.join(name);

    // Hash what is actually copied, from the one open source file.
    let mut source = File::open(payload)?;
    ensure!(
        source.metadata()?.is_file(),
        "the packaged supervisor is not a file"
    );
    let temporary = directory.join(format!(".stage-{}.tmp", Uuid::new_v4()));
    let result = (|| -> Result<String> {
        let mut copy = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1 << 16];
        loop {
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            copy.write_all(&buffer[..read])?;
        }
        copy.sync_all()?;
        drop(copy);
        // Keep the source's permissions (the executable bit) before locking it down.
        std::fs::set_permissions(&temporary, source.metadata()?.permissions())?;
        let digest = hex(&hasher.finalize());
        // Publish without replacing: a hard link fails if the name exists, atomically,
        // so two racing stagings of different content cannot overwrite each other.
        match std::fs::hard_link(&temporary, &destination) {
            Ok(()) => {
                // Drop the temporary name before locking the file down (a read-only file
                // cannot be removed on Windows), then make the staged one read-only.
                std::fs::remove_file(&temporary)?;
                let mut permissions = std::fs::metadata(&destination)?.permissions();
                permissions.set_readonly(true);
                std::fs::set_permissions(&destination, permissions)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                // Already staged: it must be exactly this content, and is left alone (it
                // may be the running supervisor).
                let existing = digest_of(&mut File::open(&destination)?)?;
                ensure!(
                    existing == digest,
                    "a different supervisor is already staged under this version"
                );
            }
            Err(error) => return Err(error.into()),
        }
        ensure!(
            digest_of(&mut File::open(&destination)?)? == digest,
            "the staged supervisor does not match its source"
        );
        Ok(digest)
    })();
    let _ = std::fs::remove_file(&temporary);
    Ok(Staged {
        path: destination,
        digest: result?,
    })
}

#[cfg(test)]
mod tests;
