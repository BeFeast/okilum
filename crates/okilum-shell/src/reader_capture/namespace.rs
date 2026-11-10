//! App-private capture namespace, provisioned only by an explicit menu click.
//! This creates no notes. Canonical capture publication remains the core writer.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Binding {
    pub root: PathBuf,
    pub vault_root: PathBuf,
    pub session: Option<PathBuf>,
    pub indexes: Vec<PathBuf>,
    pub single_file: bool,
    pub ready: bool,
}
#[derive(Clone, Debug)]
pub(super) struct Snapshot {
    pub root: PathBuf,
    pub session: PathBuf,
    indexes: Vec<PathBuf>,
}
impl PartialEq for Snapshot {
    fn eq(&self, other: &Self) -> bool {
        self.root.as_os_str() == other.root.as_os_str()
            && self.session.as_os_str() == other.session.as_os_str()
            && self.indexes.len() == other.indexes.len()
            && self
                .indexes
                .iter()
                .zip(&other.indexes)
                .all(|(a, b)| a.as_os_str() == b.as_os_str())
    }
}
impl Eq for Snapshot {}
impl Snapshot {
    /// Pure menu-open snapshot: no disk access, path normalization or fallback.
    pub fn capture(root: PathBuf, binding: &Binding) -> Result<Self> {
        ensure!(
            !binding.single_file
                && binding.ready
                && binding.root.as_os_str() == root.as_os_str()
                && binding.vault_root.as_os_str() == root.as_os_str()
                && root.is_absolute(),
            "Quick capture requires the exact open vault; this menu is stale or document-only"
        );
        root.to_str()
            .context("Quick capture requires a UTF-8 vault identity")?;
        let session = binding.session.clone().context("Quick capture has no captured session storage; reopen the menu after recovery storage is available")?;
        ensure!(
            session.is_absolute(),
            "Capture session storage must be absolute"
        );
        session
            .to_str()
            .context("Capture session identity is not UTF-8")?;
        Ok(Self {
            root,
            session,
            indexes: binding.indexes.clone(),
        })
    }
    pub fn matches(&self, binding: &Binding) -> Result<()> {
        ensure!(
            Self::capture(self.root.clone(), binding)? == *self,
            "Reader root/session/index binding changed; open a fresh Quick capture menu"
        );
        Ok(())
    }
    pub fn prepare(&self) -> Result<PreparedNamespace> {
        let root = Directory::open(self.root.clone())?;
        let session = Directory::open(self.session.clone())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            ensure!(
                session.file.metadata()?.mode() & 0o022 == 0,
                "Capture session directory permits foreign writes"
            );
        }
        ensure!(
            std::fs::canonicalize(&self.root)?.as_os_str() == self.root.as_os_str(),
            "Capture vault root is not canonical; no retargeting is allowed"
        );
        ensure!(
            std::fs::canonicalize(&self.session)?.as_os_str() == self.session.as_os_str(),
            "Capture session root is not canonical; no retargeting is allowed"
        );
        let encoded_root =
            serde_json::to_vec(self.root.to_str().context("Non-UTF8 vault identity")?)?;
        let hash = format!("{:x}", Sha256::digest(&encoded_root));
        let base_path = self.session.join("capture-records-v1");
        let records = base_path.join(&hash);
        let drafts = self.session.join("editor-drafts");
        ensure!(
            !records.starts_with(&self.root) && !drafts.starts_with(&self.root),
            "Capture state must stay outside the canonical vault"
        );
        for index in &self.indexes {
            let index = resolve_index(index)?;
            ensure!(
                !records.starts_with(&index)
                    && !index.starts_with(&records)
                    && !drafts.starts_with(&index)
                    && !index.starts_with(&drafts),
                "Capture records/drafts overlap a deletable index; no fallback storage is selected"
            );
        }
        let (base, _) = session.child("capture-records-v1")?;
        let (directory, created) = base.child(&hash)?;
        let expected = serde_json::to_vec(&Identity {
            version: 1,
            root: self.root.to_str().unwrap().into(),
            session: self.session.to_str().unwrap().into(),
        })?;
        if created {
            directory.create_identity(&expected)?;
        }
        // An existing empty directory is unbound/colliding state, not permission
        // to invent an identity. Partial/corrupt files remain retained on error.
        let (identity, identity_file) = directory.identity()?;
        let _: Identity = serde_json::from_slice(&identity)
            .context("Capture namespace identity is corrupt; retained state was not rebound")?;
        ensure!(
            identity == expected,
            "Capture namespace full identity mismatch/collision; retained state was not rebound"
        );
        let (draft_directory, _) = session.child("editor-drafts")?;
        let prepared = PreparedNamespace {
            root: self.root.clone(),
            session: self.session.clone(),
            records,
            drafts,
            identity: expected,
            identity_file,
            directories: vec![root, session, base, directory, draft_directory],
        };
        prepared.validate()?;
        Ok(prepared)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    version: u32,
    root: String,
    session: String,
}

pub(super) struct PreparedNamespace {
    pub root: PathBuf,
    pub session: PathBuf,
    pub records: PathBuf,
    pub drafts: PathBuf,
    identity: Vec<u8>,
    identity_file: File,
    directories: Vec<Directory>,
}
impl PreparedNamespace {
    pub fn validate(&self) -> Result<()> {
        for directory in &self.directories {
            directory.validate()?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let owner = self.directories[1].file.metadata()?.uid();
            for directory in &self.directories[1..] {
                let metadata = directory.file.metadata()?;
                ensure!(
                    metadata.uid() == owner && metadata.mode() & 0o022 == 0,
                    "Capture storage permissions changed; draft retained"
                );
            }
        }
        let directory = &self.directories[3];
        let (bytes, file) = directory.identity()?;
        ensure!(
            bytes == self.identity && file_identity(&file)? == file_identity(&self.identity_file)?,
            "Capture namespace identity changed; draft retained and publication refused"
        );
        Ok(())
    }
}

fn resolve_index(path: &Path) -> Result<PathBuf> {
    ensure!(
        path.is_absolute(),
        "Index identity must be absolute for safe capture storage"
    );
    let mut existing = path;
    let mut tail = Vec::new();
    while match std::fs::symlink_metadata(existing) {
        Ok(_) => false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(error.into()),
    } {
        tail.push(
            existing
                .file_name()
                .context("Invalid index identity")?
                .to_owned(),
        );
        existing = existing
            .parent()
            .context("Index has no existing ancestor")?;
    }
    let guard = Directory::open(existing.into())?;
    guard.validate()?;
    let mut resolved = std::fs::canonicalize(existing)?;
    for name in tail.into_iter().rev() {
        resolved.push(name);
    }
    Ok(resolved)
}

struct Directory {
    path: PathBuf,
    #[cfg(unix)]
    file: File,
    #[cfg(windows)]
    pin: okilum_core::windows_files::Directory,
    #[cfg(unix)]
    identity: (u64, u64),
    #[cfg(windows)]
    identity: okilum_core::windows_files::Identity,
}
impl Directory {
    fn open(path: PathBuf) -> Result<Self> {
        ensure!(path.is_absolute(), "Use an absolute capture directory");
        #[cfg(unix)]
        {
            use rustix::fs::{open, openat, Mode, OFlags};
            let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            let mut file = File::from(open(Path::new("/"), flags, Mode::empty())?);
            for part in path.components() {
                match part {
                    Component::RootDir => (),
                    Component::Normal(name) => {
                        file = File::from(
                            openat(&file, name, flags, Mode::empty())
                                .context("Unsafe/symlink capture directory")?,
                        )
                    }
                    _ => anyhow::bail!("Capture directory has unsafe path components"),
                }
            }
            let identity = file_identity(&file)?;
            Ok(Self {
                path,
                file,
                identity,
            })
        }
        #[cfg(windows)]
        {
            let pin = okilum_core::windows_files::Directory::open(&path)?;
            let identity = okilum_core::windows_files::checked_identity(&path)?;
            Ok(Self {
                path,
                pin,
                identity,
            })
        }
    }
    fn validate(&self) -> Result<()> {
        let current = Self::open(self.path.clone())?;
        ensure!(
            current.identity == self.identity,
            "Capture directory was replaced; draft retained"
        );
        Ok(())
    }
    fn child(&self, name: &str) -> Result<(Self, bool)> {
        ensure!(
            !name.is_empty()
                && Path::new(name).components().count() == 1
                && matches!(
                    Path::new(name).components().next(),
                    Some(Component::Normal(_))
                ),
            "Invalid namespace component"
        );
        self.validate()?;
        #[cfg(unix)]
        {
            use rustix::fs::{mkdirat, openat, Mode, OFlags};
            use std::os::unix::fs::MetadataExt;
            let created = match mkdirat(&self.file, name, Mode::from_raw_mode(0o700)) {
                Ok(()) => {
                    self.file.sync_all()?;
                    true
                }
                Err(rustix::io::Errno::EXIST) => false,
                Err(error) => return Err(error.into()),
            };
            let file = File::from(openat(
                &self.file,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?);
            ensure!(
                file.metadata()?.mode() & 0o022 == 0
                    && file.metadata()?.uid() == self.file.metadata()?.uid(),
                "Capture namespace directory permits foreign ownership/writes"
            );
            let identity = file_identity(&file)?;
            Ok((
                Self {
                    path: self.path.join(name),
                    file,
                    identity,
                },
                created,
            ))
        }
        #[cfg(windows)]
        {
            let path = self.path.join(name);
            let created = match std::fs::symlink_metadata(&path) {
                Ok(_) => false,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                Err(error) => return Err(error.into()),
            };
            okilum_core::note_files::create_folders(&self.path, Path::new(name), created)?;
            Ok((Self::open(path)?, created))
        }
    }
    fn create_identity(&self, bytes: &[u8]) -> Result<()> {
        self.validate()?;
        #[cfg(unix)]
        {
            use rustix::fs::{openat, Mode, OFlags};
            use std::io::Write;
            // Private create-once namespace metadata only. No second canonical
            // writer: notes/assets continue through capture's existing writer.
            let mut file = File::from(openat(
                &self.file,
                "identity.json",
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )?);
            file.write_all(bytes)?;
            file.sync_all()?;
            self.file.sync_all()?;
        }
        #[cfg(windows)]
        self.pin
            .create(std::ffi::OsStr::new("identity.json"), bytes)?;
        Ok(())
    }
    fn identity(&self) -> Result<(Vec<u8>, File)> {
        #[cfg(unix)]
        let file = {
            use rustix::fs::{openat, Mode, OFlags};
            use std::os::unix::fs::MetadataExt;
            let file = File::from(
                openat(
                    &self.file,
                    "identity.json",
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .context("Capture namespace is unbound or identity is unsafe; no fallback")?,
            );
            ensure!(
                file.metadata()?.nlink() == 1
                    && file.metadata()?.mode() & 0o022 == 0
                    && file.metadata()?.uid() == self.file.metadata()?.uid(),
                "Capture identity has unsafe links/permissions"
            );
            file
        };
        #[cfg(windows)]
        let file = {
            use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
            };
            let file = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(self.path.join("identity.json"))?;
            ensure!(
                file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
                "Capture identity is a reparse point"
            );
            file
        };
        ensure!(
            file.metadata()?.is_file(),
            "Capture identity is not a regular file"
        );
        let mut bytes = Vec::new();
        (&file).take(32 * 1024 + 1).read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 32 * 1024, "Capture identity exceeds bounds");
        Ok((bytes, file))
    }
}
#[cfg(unix)]
fn file_identity(file: &File) -> Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}
#[cfg(windows)]
fn file_identity(file: &File) -> Result<okilum_core::windows_files::Identity> {
    ensure!(
        okilum_core::windows_files::information(file)?.nNumberOfLinks == 1,
        "Capture identity has unsafe hard links"
    );
    okilum_core::windows_files::file_identity(file)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::{ffi::OsStringExt, fs::symlink};

    fn fixture() -> (tempfile::TempDir, Binding, Snapshot) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let session = temp.path().join("session");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&session).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&session, std::fs::Permissions::from_mode(0o700)).unwrap();
        let binding = Binding {
            root: root.clone(),
            vault_root: root.clone(),
            session: Some(session),
            indexes: vec![temp.path().join("index")],
            single_file: false,
            ready: true,
        };
        let snapshot = Snapshot::capture(root, &binding).unwrap();
        (temp, binding, snapshot)
    }

    #[test]
    fn menu_snapshot_is_pure_and_rejects_rebinding_and_stale_completion() {
        let (_temp, binding, snapshot) = fixture();
        assert_eq!(std::fs::read_dir(&snapshot.session).unwrap().count(), 0);
        snapshot.matches(&binding).unwrap();
        let mut changed = binding.clone();
        changed.session = Some(snapshot.session.join("another-session"));
        assert!(snapshot.matches(&changed).is_err()); // click-time rebind
        let prepared = snapshot.prepare().unwrap();
        prepared.validate().unwrap(); // positive background completion
        changed = binding.clone();
        changed.root = snapshot.root.join("another-vault");
        assert!(snapshot.matches(&changed).is_err()); // stale background completion
        changed = binding.clone();
        changed.single_file = true;
        assert!(snapshot.matches(&changed).is_err());
        changed = binding.clone();
        changed.ready = false;
        assert!(snapshot.matches(&changed).is_err());
        changed = binding.clone();
        changed.session = None;
        assert!(snapshot.matches(&changed).is_err());
        changed = binding;
        changed.vault_root = snapshot.root.join("other");
        assert!(snapshot.matches(&changed).is_err());
    }

    #[test]
    fn exact_approved_paths_and_reopen_positive_control() {
        let (_temp, _binding, snapshot) = fixture();
        let first = snapshot.prepare().unwrap();
        let expected = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(snapshot.root.to_str().unwrap()).unwrap())
        );
        assert_eq!(
            first.records,
            snapshot.session.join("capture-records-v1").join(expected)
        );
        assert_eq!(first.drafts, snapshot.session.join("editor-drafts"));
        let second = snapshot.prepare().unwrap();
        assert_eq!(
            file_identity(&first.identity_file).unwrap(),
            file_identity(&second.identity_file).unwrap()
        );
        first.validate().unwrap();
        second.validate().unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&first.drafts, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(first.validate().is_err());
        std::fs::set_permissions(&first.drafts, std::fs::Permissions::from_mode(0o700)).unwrap();
        first.validate().unwrap();
        assert_eq!(std::fs::read_dir(&snapshot.root).unwrap().count(), 0);
    }

    #[test]
    fn full_identity_mismatch_corruption_and_unbound_collision_are_held() {
        let (_temp, _binding, snapshot) = fixture();
        let prepared = snapshot.prepare().unwrap();
        let path = prepared.records.join("identity.json");
        for bytes in [
            serde_json::to_vec(&Identity {
                version: 1,
                root: "different root".into(),
                session: snapshot.session.to_str().unwrap().into(),
            })
            .unwrap(),
            b"{partial".to_vec(),
        ] {
            std::fs::write(&path, &bytes).unwrap();
            assert!(snapshot.prepare().is_err());
            assert!(prepared.validate().is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        std::fs::remove_file(&path).unwrap();
        assert!(snapshot.prepare().is_err());
        assert!(!path.exists());
    }

    #[test]
    fn directory_and_identity_replacements_are_refused() {
        let (_temp, _binding, snapshot) = fixture();
        let prepared = snapshot.prepare().unwrap();
        let identity = prepared.records.join("identity.json");
        let old = prepared.records.join("old-identity");
        std::fs::rename(&identity, &old).unwrap();
        std::fs::write(&identity, &prepared.identity).unwrap();
        assert!(prepared.validate().is_err()); // same bytes, different inode
        std::fs::remove_file(&identity).unwrap();
        symlink(&old, &identity).unwrap();
        assert!(snapshot.prepare().is_err());
        std::fs::remove_file(&identity).unwrap();
        std::fs::hard_link(&old, &identity).unwrap();
        assert!(snapshot.prepare().is_err());
        std::fs::remove_file(&identity).unwrap();
        std::fs::rename(&old, &identity).unwrap();
        prepared.validate().unwrap();
        let moved = prepared.records.with_extension("retained");
        std::fs::rename(&prepared.records, &moved).unwrap();
        std::fs::create_dir(&prepared.records).unwrap();
        assert!(prepared.validate().is_err());
        assert!(snapshot.prepare().is_err());
        assert!(moved.join("identity.json").exists());
    }

    #[test]
    fn unsafe_session_root_index_and_non_utf8_refuse_without_fallback() {
        let (temp, binding, snapshot) = fixture();
        let alias = temp.path().join("alias");
        symlink(&snapshot.session, &alias).unwrap();
        let mut changed = binding.clone();
        changed.session = Some(alias);
        assert!(Snapshot::capture(snapshot.root.clone(), &changed)
            .unwrap()
            .prepare()
            .is_err());
        changed.session = Some(temp.path().join("missing"));
        assert!(Snapshot::capture(snapshot.root.clone(), &changed)
            .unwrap()
            .prepare()
            .is_err());
        changed = binding.clone();
        changed.indexes = vec![snapshot.session.clone()];
        assert!(Snapshot::capture(snapshot.root.clone(), &changed)
            .unwrap()
            .prepare()
            .is_err());
        let non_utf8 = PathBuf::from(std::ffi::OsString::from_vec(b"/invalid-\xff".to_vec()));
        changed = binding.clone();
        changed.session = Some(non_utf8.clone());
        assert!(Snapshot::capture(snapshot.root.clone(), &changed).is_err());
        changed = binding;
        changed.root = non_utf8.clone();
        changed.vault_root = non_utf8.clone();
        assert!(Snapshot::capture(non_utf8, &changed).is_err());
        assert_eq!(std::fs::read_dir(&snapshot.session).unwrap().count(), 0);
    }

    #[test]
    fn symlink_namespace_components_never_follow_or_rebind() {
        let (temp, _binding, snapshot) = fixture();
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let base = snapshot.session.join("capture-records-v1");
        symlink(&outside, &base).unwrap();
        assert!(snapshot.prepare().is_err());
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
        std::fs::remove_file(&base).unwrap();
        let prepared = snapshot.prepare().unwrap();
        std::fs::remove_dir(&prepared.drafts).unwrap();
        symlink(&outside, &prepared.drafts).unwrap();
        assert!(prepared.validate().is_err());
        assert!(snapshot.prepare().is_err());
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    }

    #[test]
    fn failed_checkpoint_preserves_authored_draft_and_original_attempt() {
        let (_temp, _binding, snapshot) = fixture();
        let prepared = snapshot.prepare().unwrap();
        let mut draft = okilum_core::capture::ComposerDraft {
            text: "retained authored text".into(),
            folder: Some("Inbox".into()),
            ..Default::default()
        };
        draft.prepare_save().unwrap();
        draft.persist(&snapshot.root, &prepared.records).unwrap();
        let frozen = draft.clone();
        let held = prepared.records.with_extension("held");
        std::fs::rename(&prepared.records, &held).unwrap();
        std::fs::write(&prepared.records, b"blocked directory").unwrap();
        assert!(draft.persist(&snapshot.root, &prepared.records).is_err());
        assert_eq!(draft, frozen); // UI may only emit CaptureClosed on Ok
        std::fs::remove_file(&prepared.records).unwrap();
        std::fs::rename(&held, &prepared.records).unwrap();
        prepared.validate().unwrap();
        let recovered =
            okilum_core::capture::recover_composers(&snapshot.root, &prepared.records).unwrap();
        assert!(recovered.contains(&frozen));
        draft.persist(&snapshot.root, &prepared.records).unwrap(); // retry positive control
    }
}
