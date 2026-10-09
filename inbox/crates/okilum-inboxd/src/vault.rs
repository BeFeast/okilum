//! Linux server-side fixture connector. Directory descriptors pin every traversal;
//! no symlinks, overwrite, provider-generated paths or arbitrary destination roots.
use crate::{
    publication::{valid_component, Publication, Publish},
    store::{Error, Store},
};
use okilum_inbox_domain::OwnerId;
use rustix::fs::{linkat, mkdirat, open, openat, unlinkat, AtFlags, Mode, OFlags};
use std::{
    fs::File,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::{Component, Path},
};
use uuid::Uuid;

pub struct Vault {
    root: File,
    folders: Vec<String>,
}
impl Vault {
    pub fn open(path: &Path, folders: Vec<String>) -> Result<Self, Error> {
        if !path.is_absolute() || folders.is_empty() || folders.iter().any(|s| !valid_component(s))
        {
            return Err(Error::InvalidPublication);
        }
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mut root = File::from(open("/", flags, Mode::empty()).map_err(io)?);
        for part in path.components() {
            match part {
                Component::RootDir => (),
                Component::Normal(name) => {
                    root = File::from(openat(&root, name, flags, Mode::empty()).map_err(io)?)
                }
                _ => return Err(Error::InvalidPublication),
            }
        }
        let vault = Self { root, folders };
        for folder in &vault.folders {
            vault.folder(folder)?;
        }
        Ok(vault)
    }
    pub fn folders(&self) -> &[String] {
        &self.folders
    }
    fn folder(&self, name: &str) -> Result<File, Error> {
        if !self.folders.iter().any(|s| s == name) {
            return Err(Error::InvalidPublication);
        }
        Ok(File::from(
            openat(
                &self.root,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(io)?,
        ))
    }
    /// Inspect legacy unacknowledged operations without publishing anything.
    /// An unrelated target proves occupancy, not whether an earlier link happened.
    pub fn inspect(&self, publication: &mut Publication) {
        if publication.state == "published" || publication.conflict.is_some() {
            return;
        }
        let request = &publication.request;
        let (parent, name) = request
            .filename
            .rsplit_once('/')
            .unwrap_or(("", &request.filename));
        let Ok(folder) = self
            .folder(&request.folder)
            .and_then(|f| nested_folder(f, parent, false))
        else {
            return;
        };
        let Ok(target) = rustix::fs::statat(&folder, name, AtFlags::SYMLINK_NOFOLLOW) else {
            return;
        };
        let stage = format!(".okilum-inbox-{}.tmp", request.operation_id);
        let Ok(staged) = rustix::fs::statat(&folder, stage.as_str(), AtFlags::SYMLINK_NOFOLLOW)
        else {
            return;
        };
        if target.st_ino != staged.st_ino || target.st_dev != staged.st_dev {
            publication.conflict = Some("occupied".into());
        }
    }
    pub fn publish(
        &self,
        store: &mut Store,
        owner: OwnerId,
        item: Uuid,
        request: &Publish,
    ) -> Result<Publication, Error> {
        // Validate the allowlist before committing any intent.
        let folder = self.folder(&request.folder)?;
        let mut publication = store.prepare_publication(owner, item, request)?;
        if publication.conflict.is_some() {
            return Err(Error::PublicationConflict);
        }
        let (parent, filename) = request
            .filename
            .rsplit_once('/')
            .unwrap_or(("", &request.filename));
        let folder = nested_folder(folder, parent, publication.state != "published")?;
        let stage = format!(".okilum-inbox-{}.tmp", request.operation_id);
        if publication.state == "published" {
            matching(&folder, filename, &request.content)?;
            // Cleanup can have been interrupted after journal commit.
            let _ = unlinkat(&folder, stage.as_str(), AtFlags::empty());
            return Ok(publication);
        }
        let first_attempt = publication.state == "queued";
        if first_attempt {
            if let Err(error) = create_stage(&folder, &stage, &request.content) {
                if error != rustix::io::Errno::EXIST {
                    return Err(io(error));
                }
                match matching(&folder, &stage, &request.content) {
                    Ok(file) => file.sync_all().map_err(|_| Error::VaultUnavailable)?,
                    Err(Error::PublicationConflict) => {
                        // A queued intent has never linked its stage. Recover a
                        // partial write only in this reserved, private staging name;
                        // refuse symlinks, non-files and stages already hard-linked.
                        let partial = File::from(
                            openat(
                                &folder,
                                stage.as_str(),
                                OFlags::RDONLY
                                    | OFlags::NOFOLLOW
                                    | OFlags::NONBLOCK
                                    | OFlags::CLOEXEC,
                                Mode::empty(),
                            )
                            .map_err(|_| Error::PublicationConflict)?,
                        );
                        let meta = partial.metadata().map_err(|_| Error::VaultUnavailable)?;
                        if !meta.is_file() || meta.nlink() != 1 {
                            return Err(Error::PublicationConflict);
                        }
                        unlinkat(&folder, stage.as_str(), AtFlags::empty()).map_err(io)?;
                        create_stage(&folder, &stage, &request.content).map_err(io)?;
                    }
                    Err(error) => return Err(error),
                }
            }
            folder.sync_all().map_err(|_| Error::VaultUnavailable)?;
            store.advance_publication(owner, request.operation_id, "queued", "prepared")?;
            publication.state = "prepared".into();
        }
        let staged = matching(&folder, &stage, &request.content)?;
        match linkat(&folder, stage.as_str(), &folder, filename, AtFlags::empty()) {
            Ok(()) => (),
            Err(rustix::io::Errno::EXIST) => {
                // Retained staging identity distinguishes a lost acknowledgement
                // from an unrelated occupied filename (even with equal bytes).
                let same = matching(&folder, filename, &request.content).and_then(|existing| {
                    let a = staged.metadata().map_err(|_| Error::VaultUnavailable)?;
                    let b = existing.metadata().map_err(|_| Error::VaultUnavailable)?;
                    Ok(a.ino() == b.ino() && a.dev() == b.dev())
                });
                match same {
                    Ok(true) => (),
                    Ok(false) | Err(Error::PublicationConflict) => {
                        if first_attempt {
                            store.mark_publication_conflict(owner, request.operation_id)?;
                        }
                        return Err(Error::PublicationConflict);
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(e) => return Err(io(e)),
        }
        let delivered = matching(&folder, filename, &request.content)?;
        let a = staged.metadata().map_err(|_| Error::VaultUnavailable)?;
        let b = delivered.metadata().map_err(|_| Error::VaultUnavailable)?;
        if a.ino() != b.ino() || a.dev() != b.dev() {
            return Err(Error::PublicationConflict);
        }
        folder.sync_all().map_err(|_| Error::VaultUnavailable)?;
        store.advance_publication(owner, request.operation_id, "prepared", "published")?;
        publication.state = "published".into();
        let _ = unlinkat(&folder, stage.as_str(), AtFlags::empty());
        let _ = folder.sync_all();
        Ok(publication)
    }
}
fn io(_: rustix::io::Errno) -> Error {
    Error::VaultUnavailable
}
fn matching(folder: &File, name: &str, content: &str) -> Result<File, Error> {
    let mut file = File::from(
        openat(
            folder,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| Error::PublicationConflict)?,
    );
    let metadata = file.metadata().map_err(|_| Error::VaultUnavailable)?;
    if !metadata.is_file() || metadata.len() != content.len() as u64 {
        return Err(Error::PublicationConflict);
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::VaultUnavailable)?;
    if bytes != content.as_bytes() {
        return Err(Error::PublicationConflict);
    }
    Ok(file)
}

fn create_stage(folder: &File, name: &str, content: &str) -> Result<(), rustix::io::Errno> {
    let mut file = File::from(openat(
        folder,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?);
    file.write_all(content.as_bytes())
        .map_err(|_| rustix::io::Errno::IO)?;
    file.sync_all().map_err(|_| rustix::io::Errno::IO)
}

// Traverse only validated relative components, pinning each directory and never
// following symlinks. Persist the parent entry before proceeding to its child.
fn nested_folder(mut folder: File, parent: &str, create: bool) -> Result<File, Error> {
    for component in parent.split('/').filter(|s| !s.is_empty()) {
        if create {
            match mkdirat(&folder, component, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => (),
                Err(error) => return Err(io(error)),
            }
            folder.sync_all().map_err(|_| Error::VaultUnavailable)?;
        }
        folder = File::from(
            openat(
                &folder,
                component,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(io)?,
        );
    }
    Ok(folder)
}
