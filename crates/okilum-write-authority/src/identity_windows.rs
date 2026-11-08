//! Windows identity from the same open no-reparse handle used for inspection.
//! The tuple remains schema-1 compatible with existing writer authority records.
use super::*;
use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Identity(
    pub(super) u64,
    pub(super) u64,
    #[serde(default)] pub(super) Option<u64>,
);

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
    Ok(Identity(
        info.dwVolumeSerialNumber as u64,
        ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
        Some(
            ((info.ftCreationTime.dwHighDateTime as u64) << 32)
                | info.ftCreationTime.dwLowDateTime as u64,
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_open_handle_identity_preserves_the_existing_serialized_shape_and_detects_replacement(
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
        assert_eq!(serde_json::to_value(&before)?, legacy);
        assert_eq!(serde_json::from_value::<Identity>(legacy)?, before);
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
