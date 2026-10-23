#![cfg(target_os = "linux")]
//! Linux AppImage updater. Production trust and endpoint cannot be overridden.
use anyhow::{ensure, Context, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

pub mod desktop;

pub const FINGERPRINT: &str = "7FFE9F27ECC8E253E45BAEA08AACBFB2C9E47882";
const ORIGIN: &str = "https://updates.befeast.com/";
const IMAGE: &str = "Okilum-x86_64.AppImage";
const MAX_IMAGE: u64 = 2 * 1024 * 1024 * 1024;
const KEY: &str = include_str!("../../../scripts/arch/okilum-signing-key.asc");

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Release {
    pub schema: u64,
    pub platform: String,
    pub channel: String,
    pub build: u64,
    pub version: String,
    pub source: String,
    pub filename: String,
    pub sha256: String,
    pub size: u64,
    pub key: String,
    pub fingerprint: String,
    pub provenance_sha256: String,
}

impl Release {
    fn validate(&self, channel: &str) -> Result<()> {
        self.validate_authority(channel, FINGERPRINT)
    }
    fn validate_authority(&self, channel: &str, fingerprint: &str) -> Result<()> {
        ensure!(
            self.schema == 1 && self.platform == "linux-appimage",
            "Unsupported release format"
        );
        ensure!(
            self.channel == channel && matches!(channel, "beta" | "stable"),
            "Release channel mismatch"
        );
        ensure!(
            self.build > 5000 && self.version == format!("0.1.{}", self.build),
            "Release build/version mismatch"
        );
        ensure!(
            hex(&self.source, 40) && hex(&self.sha256, 64) && hex(&self.provenance_sha256, 64),
            "Invalid source/checksum"
        );
        ensure!(
            self.filename == IMAGE
                && self.key == format!("okilum/linux/builds/{}/{IMAGE}", self.build),
            "Invalid immutable payload path"
        );
        ensure!(
            self.fingerprint == fingerprint && self.size > 0 && self.size <= MAX_IMAGE,
            "Invalid signing authority or size"
        );
        Ok(())
    }
}
fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn sha(path: &Path) -> Result<String> {
    let mut hash = Sha256::new();
    std::io::copy(&mut File::open(path)?, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}

/// APPIMAGE must designate the original regular user-owned file, never the
/// extracted executable. Symlinks and hardlinked installs require manual updates.
#[derive(Debug)]
pub struct Target {
    pub path: PathBuf,
    dev: u64,
    ino: u64,
    mode: u32,
    hash: String,
}
impl Target {
    pub fn detect() -> Result<Self> {
        Self::open(&PathBuf::from(
            std::env::var_os("APPIMAGE").context("Not an AppImage launch")?,
        ))
    }
    pub fn open(path: &Path) -> Result<Self> {
        ensure!(path.is_absolute(), "APPIMAGE path must be absolute");
        let m = fs::symlink_metadata(path)?;
        ensure!(
            m.is_file() && m.nlink() == 1 && m.uid() == unsafe { libc::geteuid() },
            "AppImage must be a regular file owned by you, without links"
        );
        ensure!(
            m.permissions().mode() & 0o111 != 0 && m.permissions().mode() & 0o222 != 0,
            "AppImage is not writable/executable"
        );
        // Linux forbids write-opening a running executable (ETXTBSY), while
        // replacing its directory entry is supported. Never write-open it.
        // An actual create in the parent is necessary: access bits alone do not
        // establish whether rename is permitted (read-only mounts, ACLs).
        let probe = tempfile::NamedTempFile::new_in(path.parent().context("Missing parent")?)?;
        drop(probe);
        Ok(Self {
            path: path.to_owned(),
            dev: m.dev(),
            ino: m.ino(),
            mode: m.mode() & 0o777,
            hash: sha(path)?,
        })
    }
    fn unchanged(&self) -> Result<()> {
        let m = fs::symlink_metadata(&self.path)?;
        ensure!(
            m.is_file()
                && m.dev() == self.dev
                && m.ino() == self.ino
                && sha(&self.path)? == self.hash,
            "Original AppImage changed; check again"
        );
        Ok(())
    }
}

fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(300))
        .build()?)
}
fn download(key: &str, path: &Path, limit: u64) -> Result<()> {
    let response = client()?
        .get(format!("{ORIGIN}{key}"))
        .send()?
        .error_for_status()?;
    ensure!(
        response.content_length().is_none_or(|n| n <= limit),
        "Download exceeds size limit"
    );
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    let copied = std::io::copy(&mut response.take(limit + 1), &mut output)?;
    ensure!(copied <= limit, "Download exceeds size limit");
    output.sync_all()?;
    Ok(())
}

/// A private keyring and empty homedir exclude user keyrings and network key lookup.
fn verify(gpgv: &Path, data: &Path, sig: &Path, directory: &Path) -> Result<()> {
    verify_authority(gpgv, data, sig, directory, KEY, FINGERPRINT)
}
fn verify_authority(
    gpgv: &Path,
    data: &Path,
    sig: &Path,
    directory: &Path,
    public_key: &str,
    fingerprint: &str,
) -> Result<()> {
    let armored: String = public_key
        .lines()
        .skip_while(|l| !l.is_empty())
        .skip(1)
        .take_while(|l| !l.starts_with('=') && !l.starts_with('-'))
        .collect();
    let key = base64::engine::general_purpose::STANDARD.decode(armored)?;
    let keyring = directory.join("trusted.gpg");
    fs::write(&keyring, key)?;
    let output = Command::new(gpgv)
        .args(["--homedir"])
        .arg(directory)
        .args(["--keyring"])
        .arg(&keyring)
        .args(["--status-fd", "1"])
        .arg(sig)
        .arg(data)
        .env_remove("GNUPGHOME")
        .output()
        .context("Packaged signature verifier could not run")?;
    ensure!(
        output.status.success(),
        "Release signature verification failed"
    );
    let status = String::from_utf8(output.stdout)?;
    ensure!(
        status.lines().any(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            fields.len() >= 3
                && fields[0] == "[GNUPG:]"
                && fields[1] == "VALIDSIG"
                && (fields[2] == fingerprint || fields.last() == Some(&fingerprint))
        }),
        "Signature is not from the pinned Linux release authority"
    );
    Ok(())
}

pub fn check(beta: bool, current: u64, gpgv: &Path) -> Result<Option<Release>> {
    let channel = if beta { "beta" } else { "stable" };
    let dir = tempfile::tempdir()?;
    let descriptor = dir.path().join("release.json");
    let sig = dir.path().join("release.json.sig");
    download(
        &format!("okilum/linux/{channel}/latest.json"),
        &descriptor,
        64 * 1024,
    )?;
    download(
        &format!("okilum/linux/{channel}/latest.json.sig"),
        &sig,
        16 * 1024,
    )?;
    verify(gpgv, &descriptor, &sig, dir.path())?;
    let release: Release = serde_json::from_slice(&fs::read(descriptor)?)?;
    release.validate(channel)?;
    Ok((release.build > current).then_some(release))
}

/// Staging is ephemeral until an explicit restart. Crash before restart leaves
/// the installed file intact. The private sibling directory survives apply and
/// holds the old executable plus a recovery log.
pub struct Staged {
    directory: tempfile::TempDir,
    target: Target,
    release: Release,
    _lock: File,
}
impl Staged {
    pub fn prepare(target: Target, release: Release, beta: bool, gpgv: &Path) -> Result<Self> {
        release.validate(if beta { "beta" } else { "stable" })?;
        Self::prepare_with(target, release, gpgv, KEY, FINGERPRINT, download)
    }
    fn prepare_with(
        target: Target,
        release: Release,
        gpgv: &Path,
        public_key: &str,
        fingerprint: &str,
        fetch: impl Fn(&str, &Path, u64) -> Result<()>,
    ) -> Result<Self> {
        let parent = target.path.parent().context("Missing AppImage parent")?;
        let lockpath = parent.join(format!(
            ".{}.update-lock",
            target.path.file_name().unwrap().to_string_lossy()
        ));
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(lockpath)?;
        ensure!(
            unsafe {
                libc::flock(
                    std::os::fd::AsRawFd::as_raw_fd(&lock),
                    libc::LOCK_EX | libc::LOCK_NB,
                )
            } == 0,
            "Another AppImage updater is active"
        );
        target.unchanged()?;
        let directory = tempfile::Builder::new()
            .prefix(".okilum-update-")
            .tempdir_in(parent)?;
        let image = directory.path().join("new.AppImage");
        let sig = directory.path().join("payload.sig");
        fetch(&release.key, &image, release.size)?;
        fetch(&format!("{}.sig", release.key), &sig, 16 * 1024)?;
        verify_authority(
            gpgv,
            &image,
            &sig,
            directory.path(),
            public_key,
            fingerprint,
        )?;
        ensure!(
            fs::metadata(&image)?.len() == release.size && sha(&image)? == release.sha256,
            "AppImage size/checksum mismatch"
        );
        // Check the signed provenance as well as the descriptor's executable identity.
        let provenance = directory.path().join("provenance.json");
        let provenance_key = release.key.replace(IMAGE, "provenance.json");
        fetch(&provenance_key, &provenance, 64 * 1024)?;
        ensure!(
            sha(&provenance)? == release.provenance_sha256,
            "Provenance checksum mismatch"
        );
        let provenance_hash: serde_json::Value = serde_json::from_slice(&fs::read(&provenance)?)?;
        let expected_build = release.build.to_string();
        ensure!(
            provenance_hash["source_sha"] == release.source
                && provenance_hash["build_version"].as_str() == Some(expected_build.as_str())
                && provenance_hash["release_version"] == release.version
                && provenance_hash["sha256"] == release.sha256,
            "Payload provenance identity mismatch"
        );
        fs::set_permissions(&image, fs::Permissions::from_mode(target.mode))?;
        File::open(&image)?.sync_all()?;
        Ok(Self {
            directory,
            target,
            release,
            _lock: lock,
        })
    }
    /// Must only be called after editors and window state have been protected.
    /// A bounded external helper waits for normal application shutdown before
    /// replacing the file. No user command/path is interpolated into shell code.
    #[allow(clippy::zombie_processes)] // Helper deliberately outlives the quitting GUI.
    pub fn arm(self, args: &[std::ffi::OsString]) -> Result<PathBuf> {
        self.target.unchanged()?;
        let dir = self.directory.path();
        ensure!(
            sha(&dir.join("new.AppImage"))? == self.release.sha256,
            "Staged payload changed"
        );
        fs::hard_link(&self.target.path, dir.join("previous.AppImage"))?;
        fs::write(
            dir.join("expected.json"),
            serde_json::to_vec(&self.release)?,
        )?;
        File::open(dir.join("expected.json"))?.sync_all()?;
        File::open(dir)?.sync_all()?;
        File::open(self.target.path.parent().unwrap())?.sync_all()?;
        let log = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(dir.join("apply.log"))?;
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(APPLY)
            .arg("okilum-appimage-update")
            .arg(std::process::id().to_string())
            .arg(&self.target.path)
            .arg(dir)
            .arg(&self.release.sha256)
            .arg(&self.target.hash)
            .args(args)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .env_remove("APPIMAGE")
            .env_remove("APPDIR")
            .env_remove("LD_LIBRARY_PATH");
        // Keep the flock across shutdown until the helper has finished.
        use std::os::{fd::AsRawFd, unix::process::CommandExt};
        let lock_fd = self._lock.as_raw_fd();
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(lock_fd, 9) == -1 || libc::fcntl(9, libc::F_SETFD, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command.spawn().context("Could not arm AppImage updater")?;
        let path = self.directory.keep();
        Ok(path)
    }
}

const APPLY: &str = r#"
set -eu
pid=$1; target=$2; stage=$3; digest=$4; original=$5; shift 5
n=0
while kill -0 "$pid" 2>/dev/null; do
    n=$((n+1)); test "$n" -le 120 || { echo 'Shutdown timed out; original retained'; exit 1; }
    sleep 1
done
# Revalidate both the original inode and the staged bytes immediately before rename.
test "$target" -ef "$stage/previous.AppImage" || { echo 'Original changed; refusing apply'; exit 1; }
printf '%s  %s\n' "$original" "$stage/previous.AppImage" | sha256sum -c - || exit 1
printf '%s  %s\n' "$digest" "$stage/new.AppImage" | sha256sum -c - || exit 1
mv -f -- "$stage/new.AppImage" "$target"
sync -f "$target"
echo 'Applied; previous.AppImage is the manual rollback copy'
unset APPIMAGE APPDIR LD_LIBRARY_PATH APPIMAGE_EXTRACT_AND_RUN
# The application never inherits the transaction lock. A startup confirmation
# commits this transaction; a later session exit must never trigger rollback.
OKILUM_APPIMAGE_UPDATE_RECEIPT="$stage" "$target" --appimage-extract-and-run "$@" 9>&- &
child=$!
n=0
while test ! -f "$stage/started" && kill -0 "$child" 2>/dev/null; do
    n=$((n+1)); test "$n" -le 60 || {
        echo 'Startup confirmation timed out; application remains open, previous copy retained for manual recovery'
        exit 1
    }
    sleep 1
done
if test -f "$stage/started"; then
    printf 'committed\n' > "$stage/completed.tmp"
    sync -f "$stage/completed.tmp"
    mv -- "$stage/completed.tmp" "$stage/completed"
    sync -f "$stage"
    echo 'Startup confirmed; transaction committed and update lock released'
    exit 0
fi
wait "$child" && child_status=0 || child_status=$?
echo "Updated application exited before startup confirmation ($child_status); restoring previous AppImage"
# Do not clobber another install/update made while the child was starting.
actual=$(sha256sum "$target"); actual=${actual%% *}
test "$actual" = "$digest" || exit 1
mv -f -- "$stage/previous.AppImage" "$target"
sync -f "$target"

"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn release() -> Release {
        serde_json::from_value(serde_json::json!({
            "schema":1,"platform":"linux-appimage","channel":"beta",
            "build":5010,"version":"0.1.5010","source":"a".repeat(40),
            "filename":IMAGE,"sha256":"b".repeat(64),"size":100,
            "key":format!("okilum/linux/builds/5010/{IMAGE}"),
            "fingerprint":FINGERPRINT,"provenance_sha256":"c".repeat(64)
        }))
        .unwrap()
    }
    #[test]
    fn descriptor_rejects_wrong_channel_authority_identity_and_paths() {
        release().validate("beta").unwrap(); // positive control
        assert!(release().validate("stable").is_err());
        for field in [
            "schema",
            "platform",
            "channel",
            "version",
            "source",
            "filename",
            "sha256",
            "key",
            "fingerprint",
            "provenance_sha256",
        ] {
            let mut value = serde_json::json!({
                "schema":1,"platform":"linux-appimage","channel":"beta",
                "build":5010,"version":"0.1.5010","source":"a".repeat(40),
                "filename":IMAGE,"sha256":"b".repeat(64),"size":100,
                "key":format!("okilum/linux/builds/5010/{IMAGE}"),
                "fingerprint":FINGERPRINT,"provenance_sha256":"c".repeat(64)
            });
            value[field] = if field == "schema" {
                2.into()
            } else {
                "../untrusted".into()
            };
            let bad: Release = serde_json::from_value(value).unwrap();
            assert!(bad.validate("beta").is_err(), "{field}");
        }
    }
    fn executable(path: &Path, contents: &[u8]) {
        fs::write(path, contents).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[test]
    fn original_path_rejects_symlinks_readonly_hardlinks_and_changed_inode_or_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("original.AppImage");
        executable(&path, b"original");
        let target = Target::open(&path).unwrap();
        target.unchanged().unwrap();
        let alias = dir.path().join("alias");
        symlink(&path, &alias).unwrap();
        assert!(Target::open(&alias).is_err());
        fs::hard_link(&path, dir.path().join("hardlink")).unwrap();
        assert!(Target::open(&path).is_err());
        fs::remove_file(dir.path().join("hardlink")).unwrap();
        fs::write(&path, b"changed").unwrap();
        assert!(target.unchanged().is_err());
        executable(&dir.path().join("replacement"), b"original");
        fs::rename(dir.path().join("replacement"), &path).unwrap();
        assert!(target.unchanged().is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o555)).unwrap();
        assert!(Target::open(&path).is_err());
    }

    /// The production apply helper, real rename/rollback and real child process.
    /// Fake executable bodies are transaction controls, not native AppImage proof.
    fn transaction(payload: &[u8], tamper: bool, changed: bool) -> (Vec<u8>, String) {
        let dir = tempfile::tempdir().unwrap();
        let stage = dir.path().join("stage");
        fs::create_dir(&stage).unwrap();
        let original = dir.path().join("Original with spaces.AppImage");
        executable(&original, b"original");
        fs::hard_link(&original, stage.join("previous.AppImage")).unwrap();
        executable(&stage.join("new.AppImage"), payload);
        let digest = sha(&stage.join("new.AppImage")).unwrap();
        let old_digest = sha(&original).unwrap();
        if tamper {
            fs::write(stage.join("new.AppImage"), b"tampered").unwrap();
        }
        if changed {
            fs::write(&original, b"external replacement").unwrap();
        }
        let output = Command::new("/bin/sh")
            .args(["-c", APPLY, "test", "2147483647"])
            .arg(&original)
            .arg(&stage)
            .arg(digest)
            .arg(old_digest)
            .arg("--vault")
            .arg("path with spaces")
            .output()
            .unwrap();
        (
            fs::read(original).unwrap(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        )
    }
    #[test]
    fn apply_preserves_original_on_tamper_and_external_change_and_rolls_back_launch_failure() {
        let good = b"#!/bin/sh\ntouch \"$OKILUM_APPIMAGE_UPDATE_RECEIPT/started\"\ntest \"$1\" = --appimage-extract-and-run && test \"$2\" = --vault && test \"$3\" = 'path with spaces'\n";
        let (result, log) = transaction(good, false, false);
        assert_eq!(result, good);
        assert!(log.contains("Applied")); // positive control: helper actually applied
        assert_eq!(transaction(good, true, false).0, b"original");
        assert_eq!(transaction(good, false, true).0, b"external replacement");
        let (result, log) = transaction(b"#!/bin/sh\nexit 42\n", false, false);
        assert_eq!(result, b"original");
        assert!(log.contains("restoring previous AppImage"));
    }
    #[test]
    fn running_executable_is_replaceable_even_though_write_open_is_etxtbsy() {
        let path = std::env::current_exe().unwrap();
        let error = OpenOptions::new().write(true).open(&path).unwrap_err();
        assert_eq!(
            error.raw_os_error(),
            Some(libc::ETXTBSY),
            "positive kernel control"
        );
        Target::open(&path).unwrap().unchanged().unwrap();
    }

    #[test]
    fn confirmed_long_lived_child_does_not_retain_transaction_lock_or_trigger_late_rollback() {
        use std::os::{fd::AsRawFd, unix::process::CommandExt};
        let dir = tempfile::tempdir().unwrap();
        let stage = dir.path().join("stage");
        fs::create_dir(&stage).unwrap();
        let target = dir.path().join("original.AppImage");
        executable(&target, b"original");
        fs::hard_link(&target, stage.join("previous.AppImage")).unwrap();
        let payload = b"#!/bin/sh\necho $$ > \"$OKILUM_APPIMAGE_UPDATE_RECEIPT/child-pid\"\ntouch \"$OKILUM_APPIMAGE_UPDATE_RECEIPT/started\"\nexec sleep 30\n";
        executable(&stage.join("new.AppImage"), payload);
        let lockpath = dir.path().join("lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&lockpath)
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        let fd = lock.as_raw_fd();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", APPLY, "test", "2147483647"])
            .arg(&target)
            .arg(&stage)
            .arg(sha(&stage.join("new.AppImage")).unwrap())
            .arg(sha(&target).unwrap())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(fd, 9) == -1 || libc::fcntl(9, libc::F_SETFD, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut helper = command.spawn().unwrap();
        drop(lock);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = helper.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "helper waited for entire GUI session"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let pid: i32 = fs::read_to_string(stage.join("child-pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            0,
            "new application is still open"
        );
        let second = OpenOptions::new()
            .read(true)
            .write(true)
            .open(lockpath)
            .unwrap();
        let reacquired = unsafe { libc::flock(second.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        assert_eq!(
            reacquired, 0,
            "another update can stage while the new app is open"
        );
        assert_eq!(
            fs::read(&target).unwrap(),
            payload,
            "later session failure cannot roll back a committed update"
        );
    }

    #[test]
    fn production_verifier_rejects_disposable_key_and_tampering() {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let status = Command::new("gpg")
            .arg("--homedir")
            .arg(dir.path())
            .args([
                "--batch",
                "--pinentry-mode",
                "loopback",
                "--passphrase",
                "",
                "--quick-generate-key",
                "Disposable CI only",
                "ed25519",
                "sign",
                "0",
            ])
            .output()
            .unwrap();
        assert!(status.status.success());
        let data = dir.path().join("release.json");
        fs::write(&data, b"signed test bytes").unwrap();
        assert!(Command::new("gpg")
            .arg("--homedir")
            .arg(dir.path())
            .args(["--batch", "--detach-sign"])
            .arg(&data)
            .status()
            .unwrap()
            .success());
        let sig = dir.path().join("release.json.sig");
        // Positive control: generated signature verifies with its explicit keyring.
        assert!(Command::new("gpg")
            .arg("--homedir")
            .arg(dir.path())
            .arg("--verify")
            .arg(&sig)
            .arg(&data)
            .status()
            .unwrap()
            .success());
        let clean = tempfile::tempdir().unwrap();
        assert!(
            verify(Path::new("gpgv"), &data, &sig, clean.path()).is_err(),
            "normal mode never accepts test trust"
        );
        fs::write(&data, b"tampered").unwrap();
        assert!(!Command::new("gpg")
            .arg("--homedir")
            .arg(dir.path())
            .arg("--verify")
            .arg(&sig)
            .arg(&data)
            .status()
            .unwrap()
            .success());
    }
}

/// Acknowledge a replacement only after the new GUI has a window. This receipt
/// never supplies trust/feeds; it can only confirm this executable's identity.
pub fn confirm_startup(build: &str, source: &str) -> Result<bool> {
    let Some(directory) = std::env::var_os("OKILUM_APPIMAGE_UPDATE_RECEIPT") else {
        return Ok(false);
    };
    let directory = PathBuf::from(directory);
    let target = Target::detect()?;
    let metadata = fs::symlink_metadata(&directory)?;
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o777 == 0o700,
        "Unsafe update receipt directory"
    );
    ensure!(
        directory.parent() == target.path.parent()
            && directory
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(".okilum-update-")),
        "Update receipt is not beside the AppImage"
    );
    let expected: Release = serde_json::from_slice(&fs::read(directory.join("expected.json"))?)?;
    ensure!(
        expected.build.to_string() == build
            && expected.source == source
            && expected.sha256 == sha(&target.path)?,
        "Restarted AppImage identity mismatch"
    );
    atomic_receipt(&directory.join("started"), b"startup confirmed\n")?;
    Ok(true)
}

/// Publish a complete receipt only after its contents are durable.
fn atomic_receipt(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().context("Missing receipt parent")?;
    let mut receipt = tempfile::NamedTempFile::new_in(parent)?;
    receipt.write_all(bytes)?;
    receipt.as_file().sync_all()?;
    receipt.persist_noclobber(path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

/// Compiled callers supply their own immutable identity, never fixture metadata.
#[cfg(feature = "acceptance-harness")]
pub fn acceptance_identity(build: &str, source: &str) -> Result<serde_json::Value> {
    let target = Target::detect()?;
    Ok(
        serde_json::json!({"build":build,"source":source,"sha256":sha(&target.path)?,"path":target.path}),
    )
}

/// Explicit fixture authority for the non-shipping native GUI harness only.
/// Production check/prepare never call this function.
#[cfg(feature = "acceptance-harness")]
pub fn acceptance_prepare(
    fixture: &Path,
    fingerprint: &str,
    build: &str,
    source: &str,
) -> Result<Staged> {
    acceptance_prepare_inner(fixture, fingerprint, build, source, false)
}

/// Positive lock/staging control while the replaced native GUI remains open.
/// Reverify and stage its exact signed current payload, then discard it. This
/// does not fabricate a newer version or permit an equal-build installation.
#[cfg(feature = "acceptance-harness")]
pub fn acceptance_restage_current(
    fixture: &Path,
    fingerprint: &str,
    build: &str,
    source: &str,
) -> Result<()> {
    drop(acceptance_prepare_inner(
        fixture,
        fingerprint,
        build,
        source,
        true,
    )?);
    Ok(())
}

#[cfg(feature = "acceptance-harness")]
fn acceptance_prepare_inner(
    fixture: &Path,
    fingerprint: &str,
    build: &str,
    source: &str,
    current_control: bool,
) -> Result<Staged> {
    let target = Target::detect()?;
    ensure!(fixture.is_absolute(), "Fixture path must be absolute");
    ensure!(
        fingerprint != FINGERPRINT && fingerprint.len() == 40,
        "Fixture must use a disposable authority"
    );
    let key = fs::read_to_string(fixture.join("public.asc"))?;
    let gpgv = std::env::current_exe()?
        .parent()
        .context("Missing AppDir")?
        .join("gpgv");
    let private = tempfile::tempdir()?;
    let descriptor = fixture.join("release.json");
    let sig = fixture.join("release.json.sig");
    ensure!(
        verify(&gpgv, &descriptor, &sig, private.path()).is_err(),
        "Production verifier accepted fixture trust"
    );
    verify_authority(&gpgv, &descriptor, &sig, private.path(), &key, fingerprint)?;
    eprintln!("APPIMAGE_PRODUCTION_REJECTED_VALID_FIXTURE");
    let release: Release = serde_json::from_slice(&fs::read(descriptor)?)?;
    release.validate_authority("beta", fingerprint)?;
    if current_control {
        ensure!(
            release.build == build.parse::<u64>()? && release.source == source,
            "Restaging control must match this executable's compiled identity"
        );
    } else {
        ensure!(
            release.build > build.parse::<u64>()? && release.source != source,
            "Acceptance requires two genuinely different builds/sources"
        );
    }
    Staged::prepare_with(
        target,
        release,
        &gpgv,
        &key,
        fingerprint,
        |key, output, limit| {
            let input = fixture.join(key);
            ensure!(
                fs::metadata(&input)?.len() <= limit,
                "Fixture exceeds size limit"
            );
            fs::copy(input, output)?;
            Ok(())
        },
    )
}

/// Non-shipping, explicit native QA entry. The only alternate trust call sites
/// are compiled behind this feature; production check/prepare pin KEY/FINGERPRINT.
/// No flags, environment variables or fixture files alter normal updater trust.
#[cfg(feature = "acceptance-harness")]
pub fn acceptance_entry(build: &str, source: &str) -> Option<Result<()>> {
    let args: Vec<_> = std::env::args_os().collect();
    let action = args.get(1)?.to_str()?;
    if !matches!(
        action,
        "--appimage-update-acceptance" | "--appimage-update-acceptance-confirm"
    ) {
        return None;
    }
    Some((|| -> Result<()> {
        ensure!(
            args.len() == 5,
            "Expected fixture-directory fingerprint receipt, or receipt build source"
        );
        if action == "--appimage-update-acceptance-confirm" {
            ensure!(
                args[3] == build && args[4] == source,
                "Relaunched executable build/source mismatch"
            );
            let receipt = acceptance_identity(build, source)?;
            // Open the external destination before confirming: an invalid receipt
            // destination is an actual early-child failure/rollback control.
            let destination = PathBuf::from(&args[2]);
            let probe = tempfile::NamedTempFile::new_in(
                destination.parent().context("Missing receipt parent")?,
            )?;
            drop(probe);
            ensure!(
                confirm_startup(build, source)?,
                "Missing startup transaction"
            );
            atomic_receipt(&destination, &serde_json::to_vec(&receipt)?)?;
            println!("APPIMAGE_NATIVE_RESTART_IDENTITY_PASS {receipt}");
            return Ok(());
        }
        let fixture = PathBuf::from(&args[2]);
        let fingerprint = args[3].to_str().context("Invalid fixture fingerprint")?;
        let staged = acceptance_prepare(&fixture, fingerprint, build, source)?;
        let next_build = staged.release.build.to_string();
        let next_source = staged.release.source.clone();
        let directory = staged.arm(&[
            "--appimage-update-acceptance-confirm".into(),
            args[4].clone(),
            next_build.into(),
            next_source.into(),
        ])?;
        println!(
            "APPIMAGE_NATIVE_ARMED old_build={build} old_source={source} recovery={}",
            directory.display()
        );
        Ok(())
    })())
}
