//! The selected Syncthing runtime (design: docs/sync-supervisor-contract.md). The
//! payload layer records which staged runtime the supervisor must launch, as the
//! durable result of `update::Host::select`; the supervisor launches nothing else and
//! never falls back to another binary. A claim like the hint, strict like it, and
//! additionally checked against the file's SHA-256 right before the spawn.
use super::{StateDir, Store};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Component, Path, PathBuf},
    time::Instant,
};

pub(crate) const SELECTION: &str = "runtime.json";

/// A plain label that is also safe as one path component (staging directory name).
pub(crate) fn version_label_ok(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 64
        && version != "."
        && version != ".."
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
}
const SCHEMA: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    schema: u32,
    /// Plain version label; also the staging directory name under `<state>/runtime/`.
    version: String,
    /// Lower-case hex SHA-256 of the executable.
    digest: String,
    /// Absolute path of the executable, inside `<state>/runtime/<version>/`.
    location: PathBuf,
}
impl Selection {
    pub fn new(version: &str, digest: &str, location: &Path) -> Result<Self> {
        let selection = Self {
            schema: SCHEMA,
            version: version.to_string(),
            digest: digest.to_string(),
            location: location.to_path_buf(),
        };
        selection.validate()?;
        Ok(selection)
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    fn validate(&self) -> Result<()> {
        ensure!(self.schema == SCHEMA, "unknown runtime selection schema");
        ensure!(
            version_label_ok(&self.version),
            "invalid runtime version label"
        );
        ensure!(
            self.digest.len() == 64
                && self
                    .digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid runtime digest"
        );
        ensure!(
            self.location.is_absolute()
                && self
                    .location
                    .components()
                    .all(|c| !matches!(c, Component::ParentDir | Component::CurDir)),
            "runtime location must be a plain absolute path"
        );
        Ok(())
    }
    fn from_slice(data: &[u8]) -> Result<Self> {
        let selection: Self = serde_json::from_slice(data).context("invalid runtime selection")?;
        selection.validate()?;
        Ok(selection)
    }
    /// The executable path, which must lie strictly inside
    /// `<state>/runtime/<version>/` (lexically; links are refused at verify time).
    pub fn resolve(&self, state: &Path) -> Result<PathBuf> {
        let staging = state.join("runtime").join(&self.version);
        ensure!(
            self.location.starts_with(&staging) && self.location != staging,
            "runtime location is outside its staging directory"
        );
        Ok(self.location.clone())
    }
    /// Resolve, then check the file: a regular file (not a link) whose SHA-256 equals
    /// the recorded digest. Call it right before the spawn and again right after.
    pub fn verify_file(&self, state: &Path) -> Result<PathBuf> {
        let path = self.resolve(state)?;
        // Open without following a link, then judge and hash the same open file, so
        // a swap after the check cannot change what was verified.
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
        }
        let mut file = match options.open(&path) {
            Ok(file) => file,
            #[cfg(unix)]
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
                anyhow::bail!("runtime executable must be a regular file, not a link")
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                anyhow::bail!("runtime executable {} is missing", path.display())
            }
            Err(e) => return Err(e.into()),
        };
        ensure!(
            file.metadata()?.is_file(),
            "runtime executable must be a regular file, not a link"
        );
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1 << 16];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let actual: String = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        ensure!(
            actual == self.digest,
            "runtime executable does not match the selected digest"
        );
        Ok(path)
    }
}

impl<D: StateDir> Store<D> {
    /// The payload layer records the selection under the instance lock.
    pub fn publish_selection(&self, deadline: Instant, selection: &Selection) -> Result<()> {
        selection.validate()?;
        let _lock = self.dir.lock(deadline)?;
        self.dir.write(SELECTION, &serde_json::to_vec(selection)?)
    }
    /// None when nothing was selected. A malformed record is an error: the supervisor
    /// then refuses to start instead of guessing a binary.
    pub fn read_selection(&self, deadline: Instant) -> Result<Option<Selection>> {
        let _lock = self.dir.lock(deadline)?;
        self.dir
            .read(SELECTION)?
            .map(|data| Selection::from_slice(&data))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const GOOD: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn staged(state: &Path, version: &str, bytes: &[u8]) -> (Selection, PathBuf) {
        let directory = state.join("runtime").join(version);
        fs::create_dir_all(&directory).unwrap();
        let executable = directory.join("syncthing");
        fs::write(&executable, bytes).unwrap();
        let digest: String = Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        (
            Selection::new(version, &digest, &executable).unwrap(),
            executable,
        )
    }

    #[test]
    fn only_plain_well_formed_selections_are_accepted() {
        let state = tempfile::tempdir().unwrap();
        let location = state
            .path()
            .join("runtime")
            .join("v2.1.6")
            .join("syncthing");
        assert!(Selection::new("v2.1.6", GOOD, &location).is_ok()); // control
        for version in ["", ".", "..", "a/b", "a b", "é", &"v".repeat(65)] {
            assert!(
                Selection::new(version, GOOD, &location).is_err(),
                "{version:?}"
            );
        }
        for digest in [
            "",
            &GOOD.to_uppercase(),
            &GOOD[..63],
            &format!("{GOOD}0"),
            &GOOD.replace('0', "g"),
        ] {
            assert!(
                Selection::new("v2.1.6", digest, &location).is_err(),
                "{digest:?}"
            );
        }
        assert!(Selection::new("v2.1.6", GOOD, Path::new("runtime/v2.1.6/syncthing")).is_err());
        let with_parent = state.path().join("runtime").join("..").join("syncthing");
        assert!(Selection::new("v2.1.6", GOOD, &with_parent).is_err());
        // Unknown fields (a command line, an env) cannot ride along.
        let mut value =
            serde_json::to_value(Selection::new("v2.1.6", GOOD, &location).unwrap()).unwrap();
        assert!(Selection::from_slice(&serde_json::to_vec(&value).unwrap()).is_ok());
        value["args"] = serde_json::json!(["--evil"]);
        assert!(Selection::from_slice(&serde_json::to_vec(&value).unwrap()).is_err());
        assert!(Selection::from_slice(b"{").is_err());
    }

    #[test]
    fn the_location_must_lie_inside_its_own_staging_directory() {
        let state = tempfile::tempdir().unwrap();
        let (selection, executable) = staged(state.path(), "v1", b"binary");
        assert_eq!(selection.resolve(state.path()).unwrap(), executable);
        let elsewhere = tempfile::tempdir().unwrap();
        assert!(
            selection.resolve(elsewhere.path()).is_err(),
            "another state directory"
        );
        let other_version = Selection::new("v2", selection.digest(), &executable).unwrap();
        assert!(
            other_version.resolve(state.path()).is_err(),
            "another version's directory"
        );
        let staging = state.path().join("runtime").join("v1");
        let itself = Selection::new("v1", selection.digest(), &staging).unwrap();
        assert!(
            itself.resolve(state.path()).is_err(),
            "the directory itself"
        );
    }

    #[test]
    fn the_digest_is_checked_against_the_actual_file() {
        let state = tempfile::tempdir().unwrap();
        let (selection, executable) = staged(state.path(), "v1", b"genuine syncthing");
        assert_eq!(selection.verify_file(state.path()).unwrap(), executable); // control
        fs::write(&executable, b"genuine syncthing, tampered").unwrap();
        let error = selection.verify_file(state.path()).unwrap_err().to_string();
        assert!(error.contains("does not match"), "{error}");
        fs::remove_file(&executable).unwrap();
        assert!(selection
            .verify_file(state.path())
            .unwrap_err()
            .to_string()
            .contains("missing"));
        // A directory in its place is not a runtime.
        fs::create_dir(&executable).unwrap();
        assert!(selection.verify_file(state.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_place_of_the_executable_is_refused_even_with_the_right_content() {
        let state = tempfile::tempdir().unwrap();
        let (selection, executable) = staged(state.path(), "v1", b"genuine syncthing");
        let real = state.path().join("elsewhere");
        fs::rename(&executable, &real).unwrap();
        std::os::unix::fs::symlink(&real, &executable).unwrap();
        let error = selection.verify_file(state.path()).unwrap_err().to_string();
        assert!(error.contains("not a link"), "{error}");
    }
}
