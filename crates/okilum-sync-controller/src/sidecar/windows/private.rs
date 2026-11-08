//! Explicit-Enable preparation of a private Windows state directory. No installer
//! or Reader startup calls this module. Existing directories are validated only.
use super::security::{current_sid, sid_string};
use anyhow::{ensure, Result};
use std::{
    mem::{offset_of, size_of},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
};
use windows::{
    core::{BOOL, PCWSTR},
    Win32::{
        Foundation::{LocalFree, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, HANDLE, HLOCAL},
        Security::{
            AclSizeInformation,
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
                SE_FILE_OBJECT,
            },
            GetAce, GetAclInformation, GetSecurityDescriptorControl, GetSecurityDescriptorDacl,
            GetSecurityDescriptorOwner, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION,
            DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
            SECURITY_ATTRIBUTES, SE_DACL_PROTECTED,
        },
        Storage::FileSystem::{
            CreateDirectoryW, CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
            CREATE_NEW, FILE_ALL_ACCESS, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
            FILE_SHARE_DELETE, FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
            READ_CONTROL,
        },
    },
};
struct Descriptor(PSECURITY_DESCRIPTOR);
impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0 .0)));
        }
    }
}
fn parse(sddl: &str) -> Result<Descriptor> {
    let text: Vec<_> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut raw = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(text.as_ptr()),
            1,
            &mut raw,
            None,
        )?;
    }
    Ok(Descriptor(raw))
}
fn validate(descriptor: &Descriptor, sid: &str) -> Result<()> {
    validate_grant(descriptor, sid, 3) // OBJECT_INHERIT | CONTAINER_INHERIT
}
fn validate_grant(descriptor: &Descriptor, sid: &str, flags: u8) -> Result<()> {
    validate_file_grant(descriptor, sid, flags, false)
}
fn validate_file_grant(
    descriptor: &Descriptor,
    sid: &str,
    flags: u8,
    generated: bool,
) -> Result<()> {
    let mut owner = PSID::default();
    let mut defaulted = BOOL::default();
    let mut control = 0;
    let mut revision = 0;
    let mut present = BOOL::default();
    let mut acl: *mut ACL = std::ptr::null_mut();
    unsafe {
        GetSecurityDescriptorOwner(descriptor.0, &mut owner, &mut defaulted)?;
        let owner = sid_string(owner)?;
        ensure!(
            owner == sid || (generated && owner == super::security::default_owner_sid()?),
            "state belongs to another user"
        );
        GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision)?;
        ensure!(
            generated || control & SE_DACL_PROTECTED.0 != 0,
            "state DACL must be protected from inheritance"
        ); // SE_DACL_PROTECTED
        GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut acl, &mut defaulted)?;
        ensure!(
            present.as_bool() && !acl.is_null(),
            "state requires a non-null DACL"
        );
        let mut info = ACL_SIZE_INFORMATION::default();
        GetAclInformation(
            acl,
            (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )?;
        ensure!(info.AceCount == 1, "state requires exactly one owner grant");
        let mut ptr = std::ptr::null_mut();
        GetAce(acl, 0, &mut ptr)?;
        let header = &*ptr.cast::<ACE_HEADER>();
        ensure!(
            header.AceType == 0 && usize::from(header.AceSize) >= size_of::<ACCESS_ALLOWED_ACE>(),
            "unexpected or truncated access rule"
        );
        let ace = &*ptr.cast::<ACCESS_ALLOWED_ACE>();
        ensure!(
            ace.Header.AceType == 0
                && (ace.Header.AceFlags == flags || (generated && ace.Header.AceFlags == 0x10))
                && ace.Mask == FILE_ALL_ACCESS.0,
            "unexpected private access rule"
        ); // ALLOW
        let ace_sid = PSID(
            ptr.cast::<u8>()
                .add(offset_of!(ACCESS_ALLOWED_ACE, SidStart))
                .cast(),
        );
        ensure!(
            sid_string(ace_sid)? == sid,
            "private grant belongs to another user"
        );
    }
    Ok(())
}
/// Open from the drive root outwards. Each successful open must retain a
/// non-delete-sharing handle and reject reparse points before the next lookup.
/// Keeping the handles alive prevents a checked prefix being renamed away while
/// a later full-path lookup resolves it again.
fn pin_ancestors<T>(path: &str, mut open: impl FnMut(&str) -> Result<T>) -> Result<Vec<T>> {
    let mut handles = Vec::new();
    for (index, _) in path.match_indices('\\') {
        let end = if index == 2 { index + 1 } else { index };
        handles.push(open(&path[..end])?);
    }
    Ok(handles)
}
fn open_ancestor(path: &str) -> Result<OwnedHandle> {
    let text: Vec<_> = path.encode_utf16().chain(Some(0)).collect();
    let raw = unsafe {
        CreateFileW(
            PCWSTR(text.as_ptr()),
            FILE_LIST_DIRECTORY.0 | FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )?
    };
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe {
        GetFileInformationByHandle(HANDLE(handle.as_raw_handle()), &mut info)?;
    }
    ensure!(
        info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0
            && info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
        "ordinary state ancestor directory required"
    );
    Ok(handle)
}
/// Keeps the directory open without FILE_SHARE_DELETE to prevent replacement
/// while a future journal holds its lock. Does not create files or start Sync.
pub struct PrivateDirectory {
    handle: OwnedHandle,
    // Keep every ancestor pinned until after the leaf handle is released.
    _ancestors: Vec<OwnedHandle>,
}
impl PrivateDirectory {
    /// Explicit opt-in only. Parent must already exist; never recursively creates
    /// or changes permissions on user-provided ancestors.
    pub fn prepare(path: &str) -> Result<Self> {
        super::path(path)?;
        let path = path.trim_end_matches('\\');
        super::path(path)?;
        // Pin before creation too: refusing a redirected path after creating a
        // directory there would already have modified the wrong location.
        let _ancestors = pin_ancestors(path, open_ancestor)?;
        let sid = current_sid()?;
        let descriptor = parse(&format!("O:{sid}D:P(A;OICI;FA;;;{sid})"))?;
        let attrs = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0 .0,
            bInheritHandle: BOOL(0),
        };
        let text: Vec<_> = path.encode_utf16().chain(Some(0)).collect();
        match unsafe { CreateDirectoryW(PCWSTR(text.as_ptr()), Some(&attrs)) } {
            Ok(()) => (),
            Err(e) if e.code() == windows::core::HRESULT::from_win32(ERROR_ALREADY_EXISTS.0) => (),
            Err(e) => return Err(e.into()),
        }
        Self::open_existing(path)
    }
    /// Read-only ownership validation; never repairs a foreign/shared DACL.
    pub fn open_existing(path: &str) -> Result<Self> {
        // Metadata-only access does not participate in Windows sharing checks.
        // Request directory read access so omitting FILE_SHARE_DELETE
        // actually prevents rename/replacement while this handle lives.
        Self::open_with(
            path,
            FILE_LIST_DIRECTORY.0 | FILE_READ_ATTRIBUTES.0 | READ_CONTROL.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
        )
    }
    /// The instance lock of the state store. Shares only write access: any other
    /// open that reads or lists the directory, including `open_existing` and
    /// another lock, fails with a sharing violation while this handle lives, in
    /// this process or another, and the directory cannot be renamed or deleted.
    /// Write sharing stays on because renaming a file into the directory opens it
    /// for write; with share mode none that rename fails with a sharing violation.
    pub fn lock_exclusive(path: &str) -> Result<Self> {
        Self::open_with(
            path,
            FILE_LIST_DIRECTORY.0 | FILE_READ_ATTRIBUTES.0 | READ_CONTROL.0,
            FILE_SHARE_WRITE,
        )
    }
    /// Validation and identity only. Neither blocks nor is blocked by a holder of
    /// the exclusive lock. The leaf may be replaced; its ancestors remain pinned.
    pub fn inspect(path: &str) -> Result<Self> {
        Self::open_with(
            path,
            FILE_READ_ATTRIBUTES.0 | READ_CONTROL.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        )
    }
    fn open_with(path: &str, access: u32, share: FILE_SHARE_MODE) -> Result<Self> {
        super::path(path)?;
        let path = path.trim_end_matches('\\');
        super::path(path)?;
        let ancestors = pin_ancestors(path, open_ancestor)?;
        let text: Vec<_> = path.encode_utf16().chain(Some(0)).collect();
        let raw = unsafe {
            CreateFileW(
                PCWSTR(text.as_ptr()),
                access,
                share,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )?
        };
        let directory = Self {
            handle: unsafe { OwnedHandle::from_raw_handle(raw.0) },
            _ancestors: ancestors,
        };
        directory.verify()?;
        Ok(directory)
    }
    /// Volume serial and file index: the same directory, not merely the same path.
    pub fn identity(&self) -> Result<(u32, u64)> {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe {
            GetFileInformationByHandle(HANDLE(self.handle.as_raw_handle()), &mut info)?;
        }
        Ok((
            info.dwVolumeSerialNumber,
            (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        ))
    }
    pub fn verify(&self) -> Result<()> {
        let raw = HANDLE(self.handle.as_raw_handle());
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe {
            GetFileInformationByHandle(raw, &mut info)?;
        }
        ensure!(
            info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0
                && info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
            "ordinary state directory required"
        );
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            GetSecurityInfo(
                raw,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                None,
                None,
                None,
                None,
                Some(&mut descriptor),
            )
            .ok()?;
        }
        validate(&Descriptor(descriptor), &current_sid()?)
    }
}
/// A new journal file, created exclusively with an explicit owner and a protected
/// single-grant DACL instead of whatever the token's default owner and the parent
/// would inherit (an elevated token's default owner is Administrators).
pub(crate) fn create_private_file(path: &str) -> Result<std::fs::File> {
    super::path(path)?;
    let sid = current_sid()?;
    let descriptor = parse(&format!("O:{sid}D:P(A;;FA;;;{sid})"))?;
    let attrs = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0 .0,
        bInheritHandle: BOOL(0),
    };
    let text: Vec<_> = path.encode_utf16().chain(Some(0)).collect();
    let raw = unsafe {
        CreateFileW(
            PCWSTR(text.as_ptr()),
            FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0,
            FILE_SHARE_MODE(0),
            Some(&attrs),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )?
    };
    Ok(unsafe { std::fs::File::from_raw_handle(raw.0) })
}
/// Open an existing journal file read-only without following a reparse point.
/// None when it does not exist.
pub(crate) fn open_private_file(path: &str) -> Result<Option<std::fs::File>> {
    super::path(path)?;
    let text: Vec<_> = path.encode_utf16().chain(Some(0)).collect();
    match unsafe {
        CreateFileW(
            PCWSTR(text.as_ptr()),
            FILE_GENERIC_READ.0,
            FILE_SHARE_READ,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    } {
        Ok(raw) => Ok(Some(unsafe { std::fs::File::from_raw_handle(raw.0) })),
        Err(e) if e.code() == windows::core::HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0) => {
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}
/// Owner, protected single-grant DACL, one hard link, no reparse point.
pub(crate) fn verify_private_file(file: &std::fs::File) -> Result<()> {
    verify_file(file, false)
}
/// Syncthing safe-saves config/certificate files with the single owner grant
/// inherited from the pinned protected config directory. Journals still require
/// their explicit protected DACL and never use this adapter.
pub(crate) fn verify_generated_file(file: &std::fs::File) -> Result<()> {
    verify_file(file, true)
}
fn verify_file(file: &std::fs::File, generated: bool) -> Result<()> {
    let raw = HANDLE(file.as_raw_handle());
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe {
        GetFileInformationByHandle(raw, &mut info)?;
    }
    ensure!(
        info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY.0 | FILE_ATTRIBUTE_REPARSE_POINT.0) == 0
            && info.nNumberOfLinks == 1,
        "private regular journal required"
    );
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        GetSecurityInfo(
            raw,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            None,
            None,
            None,
            None,
            Some(&mut descriptor),
        )
        .ok()?;
    }
    validate_file_grant(&Descriptor(descriptor), &current_sid()?, 0, generated)
        .map_err(|e| e.context("private regular journal required"))
}

#[cfg(test)]
mod ancestor_fixtures {
    use super::pin_ancestors;
    use std::{cell::Cell, rc::Rc};

    struct Pin(Rc<Cell<usize>>);
    impl Drop for Pin {
        fn drop(&mut self) {
            self.0.set(self.0.get() - 1);
        }
    }

    #[test]
    fn retains_every_prefix_until_the_directory_is_released() {
        let live = Rc::new(Cell::new(0));
        let mut visited = Vec::new();
        let pins = pin_ancestors(r"C:\Users\owner\state", |prefix| {
            // Before resolving the next prefix, every earlier one is pinned.
            assert_eq!(live.get(), visited.len());
            visited.push(prefix.to_string());
            live.set(live.get() + 1);
            Ok(Pin(live.clone()))
        })
        .unwrap();
        assert_eq!(visited, [r"C:\", r"C:\Users", r"C:\Users\owner"]);
        assert_eq!(live.get(), 3);
        drop(pins);
        assert_eq!(live.get(), 0);
    }

    #[test]
    fn refuses_reparse_ancestors_before_resolving_their_children() {
        let prefixes = [r"C:\", r"C:\Users", r"C:\Users\owner"];
        // An ordinary final directory would pass the old leaf-only check.
        // Inject a reparse refusal at every possible ancestor, including root.
        for refused in 0..prefixes.len() {
            let live = Rc::new(Cell::new(0));
            let mut visited = Vec::new();
            let result = pin_ancestors(r"C:\Users\owner\state", |prefix| {
                assert_eq!(live.get(), visited.len());
                visited.push(prefix.to_string());
                if prefix == prefixes[refused] {
                    return Err(std::io::Error::other(
                        "ordinary state ancestor directory required",
                    )
                    .into());
                }
                live.set(live.get() + 1);
                Ok(Pin(live.clone()))
            });
            assert!(result.is_err());
            assert_eq!(visited, prefixes[..=refused]);
            assert_eq!(live.get(), 0, "failed walks release all earlier pins");
        }
    }

    #[test]
    fn direct_drive_child_still_pins_the_root() {
        let pins = pin_ancestors(r"D:\state", |prefix| Ok(prefix.to_string())).unwrap();
        assert_eq!(pins, [r"D:\"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_only_protected_owner_grant() {
        let sid = current_sid().unwrap();
        validate(
            &parse(&format!("O:{sid}D:P(A;OICI;FA;;;{sid})")).unwrap(),
            &sid,
        )
        .unwrap();
        for sddl in [
            format!("O:{sid}D:(A;OICI;FA;;;{sid})"),
            format!("O:{sid}D:P(A;OICI;FA;;;WD)"),
            format!("O:{sid}D:P(A;OICI;FA;;;{sid})(A;;FR;;;WD)"),
            "O:SYD:P(A;OICI;FA;;;SY)".into(),
        ] {
            assert!(validate(&parse(&sddl).unwrap(), &sid).is_err());
        }
    }
    #[test]
    fn prepares_private_child_and_preserves_shared_existing_directory() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("managed");
        let directory = PrivateDirectory::prepare(path.to_str().unwrap()).unwrap();
        directory.verify().unwrap();
        let error = std::fs::rename(&path, parent.path().join("moved")).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(32), "expected sharing violation");
        assert!(path.is_dir());
        directory.verify().unwrap();
        drop(directory);
        std::fs::rename(&path, parent.path().join("moved")).unwrap();
        let shared = parent.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::write(shared.join("preserve"), "data").unwrap();
        assert!(PrivateDirectory::prepare(shared.to_str().unwrap()).is_err());
        assert_eq!(
            std::fs::read_to_string(shared.join("preserve")).unwrap(),
            "data"
        );
    }
}
