//! Windows identity from the same open no-reparse handle used for inspection.
//! Schema 2 retains the complete volume serial and 128-bit file identifier.
use super::*;
use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
use windows_sys::Win32::Storage::FileSystem::{
    FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Identity(
    pub(super) u64,
    pub(super) u64,
    #[serde(default)] pub(super) Option<u64>,
    #[serde(default)] pub(super) Option<FileIdentity>,
);

/// Persist all 128 file-ID bits. Legacy recovery records remain readable but
/// cannot authorize cleanup by comparing a truncated index to a current ID.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(untagged)]
pub enum FileIdentity {
    Full { volume: u64, file: [u8; 16] },
    Legacy((u64, u64)),
}
pub fn file_identity(file: &File) -> Result<FileIdentity> {
    let mut info = FILE_ID_INFO::default();
    ensure!(
        unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileIdInfo,
                (&mut info as *mut FILE_ID_INFO).cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        } != 0,
        "Cannot read full Windows file identity: {}",
        std::io::Error::last_os_error()
    );
    Ok(FileIdentity::Full {
        volume: info.VolumeSerialNumber,
        file: info.FileId.Identifier,
    })
}
impl Identity {
    pub fn same_volume(&self, other: &Self) -> bool {
        matches!((self.3, other.3), (Some(FileIdentity::Full {volume: a, ..}), Some(FileIdentity::Full {volume: b, ..})) if a == b)
    }
    pub(super) fn legacy_candidate(&self, current: &Self) -> bool {
        // A possible collision fences admission; it never proves an upgrade.
        self.3.is_none()
            && self.0 == current.0
            && self.1 == current.1
            && (self.2.is_none() || self.2 == current.2)
    }
    pub(super) fn legacy_matches(&self, current: &Self) -> bool {
        // Only identities whose full ID is representable by the old NTFS index
        // can be upgraded automatically. Never infer a ReFS ID's missing bits.
        let Some(FileIdentity::Full { volume, file }) = current.3 else {
            return false;
        };
        self.3.is_none()
            && self.2.is_some()
            && self.2 == current.2
            && self.0 == volume as u32 as u64
            && file[8..] == [0; 8]
            && self.1 == u64::from_le_bytes(file[..8].try_into().unwrap())
    }
}

pub fn read(path: &Path) -> Result<Identity> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    from_file(&file)
}
pub fn from_file(file: &File) -> Result<Identity> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    ensure!(
        unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } != 0,
        "Cannot verify writer authority identity"
    );
    ensure!(
        info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        "Reparse point in writer authority"
    );
    let full = file_identity(file)?;
    Ok(Identity(
        info.dwVolumeSerialNumber as u64,
        ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
        Some(
            ((info.ftCreationTime.dwHighDateTime as u64) << 32)
                | info.ftCreationTime.dwLowDateTime as u64,
        ),
        Some(full),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_open_handle_identity_preserves_all_file_id_bits_and_detects_replacement(
    ) -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("kept.bin");
        fs::write(&path, [0, 255, 42])?;
        let handle = File::open(&path)?;
        let before = from_file(&handle)?;
        assert_eq!(read(&path)?, before);
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        assert_ne!(
            unsafe { GetFileInformationByHandle(handle.as_raw_handle(), &mut info) },
            0
        );
        let legacy = serde_json::json!([
            info.dwVolumeSerialNumber as u64,
            ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
            ((info.ftCreationTime.dwHighDateTime as u64) << 32)
                | info.ftCreationTime.dwLowDateTime as u64
        ]);
        let old = serde_json::from_value::<Identity>(legacy)?;
        assert!(old.3.is_none());
        assert_eq!(before.3, Some(file_identity(&handle)?));
        assert_eq!(
            serde_json::from_slice::<Identity>(&serde_json::to_vec(&before)?)?,
            before
        );
        fs::rename(&path, temp.path().join("retained.bin"))?;
        fs::write(&path, b"replacement")?;
        assert_ne!(read(&path)?, before);
        assert_eq!(from_file(&handle)?, before);
        assert_eq!(fs::read(temp.path().join("retained.bin"))?, [0, 255, 42]);
        Ok(())
    }
    #[test]
    fn windows_identity_refuses_reparse_points_from_the_open_handle() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let target = temp.path().join("target");
        fs::write(&target, b"original")?;
        let link = temp.path().join("link");
        std::os::windows::fs::symlink_file(&target, &link)?;
        assert!(read(&link).is_err());
        let handle = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&link)?;
        assert!(from_file(&handle).is_err());
        assert_eq!(fs::read(&target)?, b"original");
        Ok(())
    }
}
