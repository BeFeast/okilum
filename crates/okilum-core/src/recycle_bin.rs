//! The Windows Recycle Bin for Move to Trash with session Undo (#1124).
//!
//! The shell recycles the item (`SHFileOperationW` with `FOF_ALLOWUNDO`).
//! Undo finds the record the shell wrote for it — `$I…` in the drive's
//! `$Recycle.Bin\<SID>`, which names the original path — and moves the
//! matching `$R…` back without replacing anything. Only local NTFS folders are
//! accepted: elsewhere Windows deletes permanently instead of recycling. The
//! record parser is plain bytes and builds everywhere.
#[cfg(windows)]
pub use windows::*;

/// The original path in a Recycle Bin `$I` record: version 1 (Vista–8,
/// fixed 260 characters) or version 2 (Windows 10+, length-prefixed).
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn original_path(bytes: &[u8]) -> Option<String> {
    let version = i64::from_le_bytes(bytes.get(0..8)?.try_into().ok()?);
    let units = match version {
        1 => bytes.get(24..24 + 520)?,
        2 => {
            let count = u32::from_le_bytes(bytes.get(24..28)?.try_into().ok()?) as usize;
            bytes.get(28..28 + count.checked_mul(2)?)?
        }
        _ => return None,
    };
    let units: Vec<u16> = units
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .take_while(|&unit| unit != 0)
        .collect();
    String::from_utf16(&units).ok()
}

#[cfg(windows)]
mod windows {
    use super::original_path;
    use anyhow::{bail, ensure, Context, Result};
    use std::{
        collections::HashSet,
        fs,
        os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
        path::{Component, Path, PathBuf, Prefix},
    };
    use windows_sys::Win32::{
        Storage::FileSystem::{
            GetFileInformationByHandle, MoveFileExW, BY_HANDLE_FILE_INFORMATION,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        },
        UI::Shell::{
            SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
            FOF_WANTNUKEWARNING, FO_DELETE, SHFILEOPSTRUCTW,
        },
    };

    #[derive(Clone, Debug)]
    pub struct Trashed {
        pub root: PathBuf,
        pub relative: PathBuf,
        pub(crate) location: PathBuf,
        pub(crate) info: PathBuf,
        pub(crate) identity: (u64, u64),
    }

    /// Volume serial number and file index: what `(dev, ino)` is on Unix.
    pub fn identity(path: &Path) -> Result<(u64, u64)> {
        let file = fs::OpenOptions::new()
            .access_mode(0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: the handle is open for the lifetime of `file`; `info` is writable.
        let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) };
        ensure!(ok != 0, std::io::Error::last_os_error());
        Ok((
            u64::from(info.dwVolumeSerialNumber),
            (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        ))
    }

    /// What the confirmation inventory compares: identity, size and mtime.
    pub fn stamp(path: &Path, meta: &fs::Metadata) -> (u64, u64, u64, i64, i64) {
        let (volume, index) = identity(path).unwrap_or((0, 0));
        let modified = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .unwrap_or_default();
        (
            volume,
            index,
            meta.len(),
            modified.as_secs() as i64,
            i64::from(modified.subsec_nanos()),
        )
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain([0]).collect()
    }

    /// Paths as people and the Recycle Bin record spell them: no `\\?\`.
    fn plain(path: &Path) -> String {
        crate::vault::display_path(path)
    }

    fn checked(root: &Path, relative: &Path) -> Result<PathBuf> {
        let parts: Vec<_> = relative.components().collect();
        ensure!(
            !parts.is_empty() && parts.iter().all(|p| matches!(p, Component::Normal(_))),
            "Choose an item inside the vault"
        );
        ensure!(
            !crate::vault::service_path(relative),
            "Service files cannot be moved to the Recycle Bin"
        );
        Ok(root.join(relative))
    }

    /// `X:\$Recycle.Bin` for the drive holding `path`.
    fn recycle_bin(path: &Path) -> Result<PathBuf> {
        match path.components().next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                    Ok(PathBuf::from(format!("{}:\\$Recycle.Bin", letter as char)))
                }
                _ => bail!("Only vaults on a local drive can use the Recycle Bin"),
            },
            _ => bail!("Only vaults on a local drive can use the Recycle Bin"),
        }
    }

    /// Every `$I…` record this user can read on the drive.
    fn records(bin: &Path) -> HashSet<PathBuf> {
        let mut found = HashSet::new();
        for user in fs::read_dir(bin).into_iter().flatten().flatten() {
            for entry in fs::read_dir(user.path()).into_iter().flatten().flatten() {
                if entry.file_name().to_string_lossy().starts_with("$I") {
                    found.insert(entry.path());
                }
            }
        }
        found
    }

    pub fn move_to_trash(root: &Path, relative: &Path) -> Result<Trashed> {
        let root = root.canonicalize()?;
        let source = checked(&root, relative)?;
        let metadata = fs::symlink_metadata(&source)?;
        ensure!(
            (metadata.is_file() || metadata.is_dir()) && !metadata.file_type().is_symlink(),
            "Choose a regular file or folder, not a link"
        );
        // A network or non-NTFS location has no Recycle Bin: Windows would
        // delete the item for good. Refuse before touching it.
        crate::windows_files::Directory::open(source.parent().context("Missing source folder")?)?;
        let identity = identity(&source)?;
        let bin = recycle_bin(&root)?;
        let before = records(&bin);

        let from: Vec<u16> = wide(Path::new(&plain(&source)))
            .into_iter()
            .chain([0])
            .collect();
        let mut operation = SHFILEOPSTRUCTW {
            hwnd: std::ptr::null_mut(),
            wFunc: FO_DELETE,
            pFrom: from.as_ptr(),
            pTo: std::ptr::null(),
            // The nuke warning asks instead of deleting if recycling fails.
            fFlags: (FOF_ALLOWUNDO
                | FOF_NOCONFIRMATION
                | FOF_NOERRORUI
                | FOF_SILENT
                | FOF_WANTNUKEWARNING) as u16,
            fAnyOperationsAborted: 0,
            hNameMappings: std::ptr::null_mut(),
            lpszProgressTitle: std::ptr::null(),
        };
        // SAFETY: `from` is double-NUL terminated and outlives the call.
        let status = unsafe { SHFileOperationW(&mut operation) };
        ensure!(
            status == 0 && operation.fAnyOperationsAborted == 0,
            "Windows did not move the item to the Recycle Bin (code {status:#x}); the original was kept"
        );
        ensure!(
            fs::symlink_metadata(&source).is_err(),
            "Windows did not move the item to the Recycle Bin; the original was kept"
        );

        let original = plain(&source);
        let info = records(&bin)
            .difference(&before)
            .find(|record| {
                fs::read(record)
                    .ok()
                    .and_then(|bytes| original_path(&bytes))
                    .is_some_and(|path| path.to_lowercase() == original.to_lowercase())
            })
            .cloned()
            .context("The item is in the Recycle Bin, but Undo is unavailable. Restore it from the Recycle Bin")?;
        let location = info.with_file_name(format!(
            "$R{}",
            &info.file_name().unwrap().to_string_lossy()[2..]
        ));
        ensure!(
            identity == self::identity(&location)?,
            "The item is in the Recycle Bin, but Undo is unavailable. Restore it from the Recycle Bin"
        );
        Ok(Trashed {
            root,
            relative: relative.to_owned(),
            location,
            info,
            identity,
        })
    }

    impl Trashed {
        pub fn restore(&self) -> Result<()> {
            let identity =
                identity(&self.location).context("The item is no longer in the Recycle Bin")?;
            ensure!(
                identity == self.identity,
                "The item in the Recycle Bin changed; restore it from the Recycle Bin"
            );
            let destination = checked(&self.root, &self.relative)?;
            ensure!(
                destination.parent().is_some_and(Path::is_dir),
                "Cannot Undo: the original folder is gone. The item remains in the Recycle Bin"
            );
            // No MOVEFILE_REPLACE_EXISTING: Undo never replaces a newer item.
            // SAFETY: both paths are NUL-terminated wide strings.
            let ok = unsafe {
                MoveFileExW(
                    wide(&self.location).as_ptr(),
                    wide(&destination).as_ptr(),
                    0,
                )
            };
            ensure!(
                ok != 0,
                "Cannot Undo: the original path is occupied or unavailable. The item remains in the Recycle Bin"
            );
            let _ = fs::remove_file(&self.info);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use std::path::{Path, PathBuf};

    fn utf16(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    fn header(version: i64) -> Vec<u8> {
        let mut bytes = version.to_le_bytes().to_vec();
        bytes.extend(1234i64.to_le_bytes()); // size
        bytes.extend(0x01d9_0000_0000_0000i64.to_le_bytes()); // deletion time
        bytes
    }

    #[test]
    fn windows_version_2_records_name_the_original_path() {
        let path = r"C:\Users\qa\Notes\שלום עולם.md";
        let mut bytes = header(2);
        bytes.extend((path.encode_utf16().count() as u32 + 1).to_le_bytes());
        bytes.extend(utf16(path));
        bytes.extend([0, 0]);
        assert_eq!(original_path(&bytes).as_deref(), Some(path));
    }

    #[test]
    fn windows_version_1_records_use_a_fixed_field() {
        let path = r"D:\vault\note.md";
        let mut bytes = header(1);
        let mut field = utf16(path);
        field.resize(520, 0);
        bytes.extend(field);
        assert_eq!(original_path(&bytes).as_deref(), Some(path));
    }

    #[test]
    fn windows_truncated_or_unknown_records_name_nothing() {
        // Positive control above; these must not invent a path.
        assert_eq!(original_path(&header(3)), None);
        let mut short = header(2);
        short.extend(200u32.to_le_bytes());
        short.extend(utf16("C:\\a"));
        assert_eq!(original_path(&short), None);
        assert_eq!(original_path(&[1, 0, 0]), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_recycle_bin_move_and_undo_keep_bytes_and_never_replace() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let relative = PathBuf::from("שלום note.md");
        let bytes = b"\xef\xbb\xbf# original\r\n";
        std::fs::write(root.join(&relative), bytes).unwrap();

        let trashed = move_to_trash(&root, &relative).unwrap();
        assert!(!root.join(&relative).exists(), "the note left the vault");
        assert!(trashed.location.exists(), "the note is in the Recycle Bin");

        // Undo never replaces a newer note at the same path.
        std::fs::write(root.join(&relative), b"new").unwrap();
        assert!(trashed.restore().is_err());
        assert_eq!(std::fs::read(root.join(&relative)).unwrap(), b"new");

        // Positive control: once the path is free, Undo brings the bytes back
        // and removes the Recycle Bin record.
        std::fs::remove_file(root.join(&relative)).unwrap();
        trashed.restore().unwrap();
        assert_eq!(std::fs::read(root.join(&relative)).unwrap(), bytes);
        assert!(!trashed.info.exists());
        assert!(!trashed.location.exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_recycle_bin_refuses_service_files_and_outside_paths() {
        let temp = tempfile::tempdir().unwrap();
        for relative in [".obsidian/app.json", "../escape.md", ""] {
            assert!(
                move_to_trash(temp.path(), Path::new(relative)).is_err(),
                "{relative}"
            );
        }
    }
}
