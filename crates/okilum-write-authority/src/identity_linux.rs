//! Linux device numbers (notably btrfs anonymous devices) are boot-local hints.
//! Read filesystem identity and inode/birthtime from the same no-follow handle.
use super::*;
use std::os::{
    fd::AsRawFd,
    unix::fs::{MetadataExt, OpenOptionsExt},
};

#[derive(Clone, Debug, Serialize, Deserialize, Eq)]
pub struct Identity(
    pub(super) u64,
    pub(super) u64,
    #[serde(default)] pub(super) Option<u64>,
    #[serde(default)] pub(super) Option<[u8; 16]>,
    #[serde(default)] pub(super) Option<u64>, // btrfs subvolume tree ID
);
impl PartialEq for Identity {
    fn eq(&self, other: &Self) -> bool {
        match (self.3, other.3) {
            (Some(a), Some(b)) => (a, self.1, self.2, self.4) == (b, other.1, other.2, other.4),
            _ => {
                (self.0, self.1, self.2, self.3, self.4)
                    == (other.0, other.1, other.2, other.3, other.4)
            }
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

// Schema 1 did not store a filesystem UUID. Besides the inode/birthtime proof,
// retain its same-filesystem relationships across all five independent objects.
// Once migrated, UUID/subvolume equality is mandatory on every validation.
pub(super) fn legacy_filesystems_match(pairs: &[(&Identity, &Identity)]) -> bool {
    pairs.iter().all(|(old, current)| {
        pairs.iter().all(|(other_old, other_current)| {
            old.0 != other_old.0 || (current.3, current.4) == (other_current.3, other_current.4)
        })
    })
}

pub(super) fn read(path: &Path) -> Result<Identity> {
    let handle = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    from_file(&handle)
}

/// Stable persisted identity; callers retain their own descriptor/path guards.
pub fn from_file(handle: &File) -> Result<Identity> {
    let meta = handle.metadata()?;
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatfs(handle.as_raw_fd(), &mut stat) } != 0 {
        return Err(std::io::Error::last_os_error())
            .context("Cannot read writer filesystem identity");
    }
    // Linux fsid_t is two native 32-bit words. Encode explicitly for durable JSON.
    let fsid: [i32; 2] = unsafe { std::mem::transmute(stat.f_fsid) };
    ensure!(
        fsid != [0; 2],
        "Stable writer filesystem identity is unavailable"
    );
    let mut filesystem = [0; 16];
    #[allow(clippy::unnecessary_cast)] // f_type is 32-bit on 32-bit Linux.
    let filesystem_type = stat.f_type as i64;
    filesystem[..8].copy_from_slice(&filesystem_type.to_le_bytes());
    filesystem[8..12].copy_from_slice(&fsid[0].to_le_bytes());
    filesystem[12..].copy_from_slice(&fsid[1].to_le_bytes());
    let subvolume = if stat.f_type == 0x9123683e {
        // Full btrfs filesystem UUID and subvolume ID: snapshots can share inode
        // numbers AND birthtimes. Both queries are unprivileged and fd-scoped.
        #[repr(C)]
        struct FsInfo {
            max_id: u64,
            num_devices: u64,
            fsid: [u8; 16],
            remaining: [u8; 992],
        }
        #[repr(C)]
        struct Lookup {
            treeid: u64,
            objectid: u64,
            name: [u8; 4080],
        }
        let mut info: FsInfo = unsafe { std::mem::zeroed() };
        let mut lookup: Lookup = unsafe { std::mem::zeroed() };
        // BTRFS_FIRST_FREE_OBJECTID requests only the containing subvolume ID,
        // without a privileged path lookup (linux/btrfs.h).
        lookup.objectid = 256;
        let fs_info = rustix::ioctl::opcode::read::<FsInfo>(0x94, 31);
        let ino_lookup = rustix::ioctl::opcode::read_write::<Lookup>(0x94, 18);
        if unsafe { libc::ioctl(handle.as_raw_fd(), fs_info as _, &mut info) } != 0
            || unsafe { libc::ioctl(handle.as_raw_fd(), ino_lookup as _, &mut lookup) } != 0
        {
            return Err(std::io::Error::last_os_error())
                .context("Cannot read stable btrfs filesystem/subvolume identity");
        }
        ensure!(
            info.fsid != [0; 16] && lookup.treeid != 0,
            "Invalid btrfs identity"
        );
        filesystem = info.fsid;
        Some(lookup.treeid)
    } else {
        None
    };
    Ok(Identity(
        meta.dev(),
        meta.ino(),
        meta.created()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|time| u64::try_from(time.as_nanos()).ok()),
        Some(filesystem),
        subvolume,
    ))
}
