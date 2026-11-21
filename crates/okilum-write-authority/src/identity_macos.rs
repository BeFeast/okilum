//! APFS device numbers change across boots. Read UUID and inode from one handle.
//! Network and FUSE volumes (NFS, SMB, macFUSE) report no volume UUID (#3202);
//! they are identified by their mount source instead, which survives remounts.
use super::*;
use std::os::{
    fd::AsRawFd,
    unix::fs::{MetadataExt, OpenOptionsExt},
};

#[derive(Clone, Debug, Serialize, Deserialize, Eq)]
pub struct Identity(
    pub(super) u64, // diagnostic st_dev hint only for UUID-bearing identities
    pub(super) u64,
    #[serde(default)] pub(super) Option<u64>,
    #[serde(default)] pub(super) Option<[u8; 16]>,
);
impl PartialEq for Identity {
    fn eq(&self, other: &Self) -> bool {
        match (self.3, other.3) {
            (Some(a), Some(b)) => (a, self.1, self.2) == (b, other.1, other.2),
            _ => (self.0, self.1, self.2, self.3) == (other.0, other.1, other.2, other.3),
        }
    }
}
impl Identity {
    pub(super) fn legacy_matches(&self, current: &Self) -> bool {
        self.3.is_none()
            && current.3.is_some()
            && self.1 == current.1
            && self.2.is_some()
            && self.2 == current.2
    }
}

pub fn read(path: &Path) -> Result<Identity> {
    let handle = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    from_file(&handle)
}

/// Stable APFS identity from the same already-open no-follow handle.
pub fn from_file(handle: &File) -> Result<Identity> {
    #[cfg(test)]
    if WITHOUT_VOLUME_UUID.with(std::cell::Cell::get) {
        // NFS answers ATTR_VOL_UUID with EINVAL (#3202, macOS 27 on NFSv3).
        return from_file_with(handle, |_| {
            Err(std::io::Error::from_raw_os_error(libc::EINVAL))
        });
    }
    from_file_with(handle, volume_uuid)
}

#[cfg(test)]
thread_local! {
    /// Models a volume that reports no UUID for every identity on this thread.
    pub(super) static WITHOUT_VOLUME_UUID: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

pub(super) fn from_file_with(
    handle: &File,
    native_uuid: impl FnOnce(&File) -> std::io::Result<Option<[u8; 16]>>,
) -> Result<Identity> {
    let meta = handle.metadata()?;
    let volume = match native_uuid(handle) {
        Ok(Some(uuid)) => uuid,
        // The filesystem has no volume UUID attribute; anything else stays an error.
        Ok(None) => mount_source_identity(handle)?,
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(libc::EINVAL | libc::ENOTSUP | libc::ENOATTR)
            ) =>
        {
            mount_source_identity(handle)?
        }
        Err(error) => return Err(error).context("Cannot read stable writer volume identity"),
    };
    Ok(Identity(
        meta.dev(),
        meta.ino(),
        meta.created()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|t| u64::try_from(t.as_nanos()).ok()),
        Some(volume),
    ))
}

/// Native APFS/HFS volume UUID; `None` when the volume reports no UUID.
fn volume_uuid(handle: &File) -> std::io::Result<Option<[u8; 16]>> {
    // Darwin attribute buffers have a four-byte length followed by the requested
    // UUID. ATTR_VOL_INFO selects volume attributes; it contributes no payload.
    #[repr(C)]
    #[derive(Default)]
    struct Volume {
        length: u32,
        uuid: [u8; 16],
    }
    let mut attributes: libc::attrlist = unsafe { std::mem::zeroed() };
    attributes.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    attributes.volattr = libc::ATTR_VOL_INFO | libc::ATTR_VOL_UUID;
    let mut volume = Volume::default();
    let result = unsafe {
        libc::fgetattrlist(
            handle.as_raw_fd(),
            (&mut attributes as *mut libc::attrlist).cast(),
            (&mut volume as *mut Volume).cast(),
            std::mem::size_of::<Volume>(),
            0,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(
        (volume.length as usize == std::mem::size_of::<Volume>() && volume.uuid != [0; 16])
            .then_some(volume.uuid),
    )
}

/// Volumes without a UUID: `st_dev` and `f_fsid` are assigned per mount (NFS
/// gives the same export a new device on every remount), so they cannot persist.
/// The filesystem type and mount source (`host:/export`, `//user@host/share`)
/// name the same export across remounts; inode and birth time then select the
/// object within it. The domain-separated digest cannot equal a native UUID.
fn mount_source_identity(handle: &File) -> Result<[u8; 16]> {
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatfs(handle.as_raw_fd(), &mut stat) } != 0 {
        return Err(std::io::Error::last_os_error())
            .context("Cannot read writer filesystem identity");
    }
    let field = |raw: &[libc::c_char]| -> Vec<u8> {
        raw.iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect()
    };
    mount_source_digest(&field(&stat.f_fstypename), &field(&stat.f_mntfromname))
}

pub(super) fn mount_source_digest(filesystem: &[u8], source: &[u8]) -> Result<[u8; 16]> {
    ensure!(
        !filesystem.is_empty() && !source.is_empty(),
        "Stable writer volume identity is unavailable"
    );
    let mut digest = Sha256::new();
    digest.update(b"okilum-writer-mount-source-v1\0");
    digest.update(filesystem);
    digest.update(b"\0");
    digest.update(source);
    let mut volume = [0; 16];
    volume.copy_from_slice(&digest.finalize()[..16]);
    Ok(volume)
}
