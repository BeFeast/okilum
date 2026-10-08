//! Explicit opt-in creation of one private local pipe instance. This does not
//! discover a supervisor, authenticate its peer, connect, or perform IPC I/O.
use super::Scope;
use crate::sidecar::windows::security::{current_sid, sid_string};
use anyhow::{ensure, Result};
use std::{
    mem::{offset_of, size_of},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
};
use windows::{
    core::{BOOL, PCWSTR},
    Win32::{
        Foundation::{LocalFree, HANDLE, HLOCAL},
        Security::{
            AclSizeInformation,
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
                SE_KERNEL_OBJECT,
            },
            GetAce, GetAclInformation, GetSecurityDescriptorControl, GetSecurityDescriptorDacl,
            GetSecurityDescriptorOwner, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION,
            DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
            SECURITY_ATTRIBUTES, SE_DACL_PROTECTED,
        },
        Storage::FileSystem::{
            FILE_ALL_ACCESS, FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_FIRST_PIPE_INSTANCE,
            FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX, READ_CONTROL,
        },
        System::Pipes::{
            CreateNamedPipeW, GetNamedPipeInfo, NAMED_PIPE_MODE, PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_SERVER_END, PIPE_TYPE_BYTE,
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
fn descriptor(sddl: &str) -> Result<Descriptor> {
    let wide: Vec<_> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut raw = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(wide.as_ptr()),
            1,
            &mut raw,
            None,
        )?;
    }
    Ok(Descriptor(raw))
}
fn validate(sd: &Descriptor, sid: &str) -> Result<()> {
    let mut owner = PSID::default();
    let mut defaulted = BOOL::default();
    let mut control = 0;
    let mut revision = 0;
    let mut present = BOOL::default();
    let mut acl: *mut ACL = std::ptr::null_mut();
    unsafe {
        GetSecurityDescriptorOwner(sd.0, &mut owner, &mut defaulted)?;
        ensure!(sid_string(owner)? == sid, "pipe has foreign owner");
        GetSecurityDescriptorControl(sd.0, &mut control, &mut revision)?;
        ensure!(
            control & SE_DACL_PROTECTED.0 != 0,
            "pipe DACL must be protected"
        );
        GetSecurityDescriptorDacl(sd.0, &mut present, &mut acl, &mut defaulted)?;
        ensure!(
            present.as_bool() && !acl.is_null(),
            "pipe requires non-null DACL"
        );
        let mut info = ACL_SIZE_INFORMATION::default();
        GetAclInformation(
            acl,
            (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )?;
        ensure!(info.AceCount == 1, "pipe requires one owner grant");
        let mut ptr = std::ptr::null_mut();
        GetAce(acl, 0, &mut ptr)?;
        let header = &*ptr.cast::<ACE_HEADER>();
        ensure!(
            header.AceType == 0 && usize::from(header.AceSize) >= size_of::<ACCESS_ALLOWED_ACE>(),
            "invalid pipe grant"
        );
        let ace = &*ptr.cast::<ACCESS_ALLOWED_ACE>();
        ensure!(
            ace.Header.AceFlags == 0 && ace.Mask == FILE_ALL_ACCESS.0,
            "unexpected pipe access mask or inheritance"
        );
        let granted = PSID(
            ptr.cast::<u8>()
                .add(offset_of!(ACCESS_ALLOWED_ACE, SidStart))
                .cast(),
        );
        ensure!(sid_string(granted)? == sid, "pipe grants another principal");
    }
    Ok(())
}

/// A fixed local namespace; never accepts a caller-selected host or path.
pub fn endpoint_name(scope: &Scope) -> Result<String> {
    ensure!(
        !scope.installation.is_nil() && !scope.instance.is_nil() && !scope.generation.is_nil(),
        "nil supervisor scope"
    );
    Ok(format!(
        r"\\.\pipe\Tessera-Sync-{}-{}-{}",
        scope.installation, scope.instance, scope.generation
    ))
}

pub struct PrivatePipe {
    handle: OwnedHandle,
    owner_sid: String,
}
impl PrivatePipe {
    /// Only after explicit Enable and verified supervisor preparation. Fails on
    /// any existing instance; never adopts, repairs, disconnects or replaces it.
    /// The returned overlapped handle is ready for a future bounded transport.
    pub fn create(scope: &Scope) -> Result<Self> {
        let name = endpoint_name(scope)?;
        let sid = current_sid()?;
        let sd = descriptor(&format!("O:{sid}D:P(A;;FA;;;{sid})"))?;
        let attrs = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0 .0,
            bInheritHandle: BOOL(0),
        };
        let name: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(name.as_ptr()),
                PIPE_ACCESS_DUPLEX
                    | FILE_FLAG_FIRST_PIPE_INSTANCE
                    | FILE_FLAG_OVERLAPPED
                    | FILE_FLAGS_AND_ATTRIBUTES(READ_CONTROL.0),
                PIPE_TYPE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                super::MAX_FRAME as u32,
                super::MAX_FRAME as u32,
                0,
                Some(&attrs),
            )
        };
        if handle.is_invalid() {
            return Err(windows::core::Error::from_win32().into());
        }
        let pipe = Self {
            handle: unsafe { OwnedHandle::from_raw_handle(handle.0) },
            owner_sid: sid,
        };
        pipe.verify()?;
        Ok(pipe)
    }
    /// Read-back only. The handle remains owned; no pathname reopen or ACL repair.
    pub fn verify(&self) -> Result<()> {
        ensure!(
            self.owner_sid == current_sid()?,
            "pipe owner no longer current user"
        );
        let raw = HANDLE(self.handle.as_raw_handle());
        let mut sd = PSECURITY_DESCRIPTOR::default();
        unsafe {
            GetSecurityInfo(
                raw,
                SE_KERNEL_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                None,
                None,
                None,
                None,
                Some(&mut sd),
            )
            .ok()?;
        }
        validate(&Descriptor(sd), &self.owner_sid)?;
        let mut flags = NAMED_PIPE_MODE::default();
        let mut instances = 0;
        unsafe {
            GetNamedPipeInfo(raw, Some(&mut flags), None, None, Some(&mut instances))?;
        }
        ensure!(
            flags == PIPE_SERVER_END && instances == 1,
            "unexpected pipe type, end or instance limit"
        );
        Ok(())
    }
}
impl AsRawHandle for PrivatePipe {
    fn as_raw_handle(&self) -> RawHandle {
        self.handle.as_raw_handle()
    }
}

#[cfg(test)]
mod tests;
