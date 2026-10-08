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
        Foundation::{LocalFree, ERROR_ALREADY_EXISTS, HANDLE, HLOCAL},
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
            FILE_ALL_ACCESS, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY,
            FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL,
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
    let mut owner = PSID::default();
    let mut defaulted = BOOL::default();
    let mut control = 0;
    let mut revision = 0;
    let mut present = BOOL::default();
    let mut acl: *mut ACL = std::ptr::null_mut();
    unsafe {
        GetSecurityDescriptorOwner(descriptor.0, &mut owner, &mut defaulted)?;
        ensure!(sid_string(owner)? == sid, "state belongs to another user");
        GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision)?;
        ensure!(
            control & SE_DACL_PROTECTED.0 != 0,
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
            ace.Header.AceType == 0 && ace.Header.AceFlags == 3 && ace.Mask == FILE_ALL_ACCESS.0,
            "unexpected private-directory access rule"
        ); // ALLOW, OBJECT_INHERIT | CONTAINER_INHERIT
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
/// Keeps the directory open without FILE_SHARE_DELETE to prevent replacement
/// while a future journal holds its lock. Does not create files or start Sync.
pub struct PrivateDirectory {
    handle: OwnedHandle,
}
impl PrivateDirectory {
    /// Explicit opt-in only. Parent must already exist; never recursively creates
    /// or changes permissions on user-provided ancestors.
    pub fn prepare(path: &str) -> Result<Self> {
        super::path(path)?;
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
        super::path(path)?;
        let text: Vec<_> = path.encode_utf16().chain(Some(0)).collect();
        let raw = unsafe {
            CreateFileW(
                PCWSTR(text.as_ptr()),
                // Metadata-only access does not participate in Windows sharing checks.
                // Request directory read access so omitting FILE_SHARE_DELETE
                // actually prevents rename/replacement while this handle lives.
                FILE_LIST_DIRECTORY.0 | FILE_READ_ATTRIBUTES.0 | READ_CONTROL.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )?
        }; // READ_CONTROL
        let directory = Self {
            handle: unsafe { OwnedHandle::from_raw_handle(raw.0) },
        };
        directory.verify()?;
        Ok(directory)
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
