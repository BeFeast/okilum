//! APFS device numbers change across boots. Read UUID and inode from one handle.
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

pub(super) fn read(path: &Path) -> Result<Identity> {
    let handle = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    from_file(&handle)
}

/// Stable persisted identity read from the caller's held no-follow descriptor.
pub fn from_file(handle: &File) -> Result<Identity> {
    let meta = handle.metadata()?;
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
        return Err(std::io::Error::last_os_error())
            .context("Cannot read stable writer volume identity");
    }
    ensure!(
        volume.length as usize == std::mem::size_of::<Volume>() && volume.uuid != [0; 16],
        "Stable writer volume identity is unavailable"
    );
    Ok(Identity(
        meta.dev(),
        meta.ino(),
        meta.created()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|t| u64::try_from(t.as_nanos()).ok()),
        Some(volume.uuid),
    ))
}
