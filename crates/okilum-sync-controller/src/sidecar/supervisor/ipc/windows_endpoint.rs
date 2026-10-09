//! Explicit opt-in creation of one private local pipe instance. This does not
//! discover a supervisor, authenticate its peer, connect, or perform IPC I/O.
use super::{
    windows_peer::{PeerEnd, ProcessPeer},
    Scope,
};
use crate::sidecar::windows::security::{current_sid, sid_string};
use anyhow::{ensure, Context, Result};
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
            FILE_ALL_ACCESS, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
            PIPE_ACCESS_DUPLEX,
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

// Windows reports the configured remote-client rejection bit as well as end/type.
// Require it explicitly; do not mask away arbitrary extra bits.
fn validate_pipe_info(flags: NAMED_PIPE_MODE, instances: u32) -> Result<()> {
    let expected = PIPE_SERVER_END | PIPE_REJECT_REMOTE_CLIENTS;
    ensure!(
        flags == expected && instances == 1,
        "unexpected pipe type, end or instance limit: flags={:#010x}, max_instances={}, expected_flags={:#010x}, expected_max_instances=1",
        flags.0, instances, expected.0
    );
    Ok(())
}

/// A fixed local namespace; never accepts a caller-selected host or path.
pub fn endpoint_name(scope: &Scope) -> Result<String> {
    ensure!(
        !scope.installation.is_nil() && !scope.instance.is_nil() && !scope.generation.is_nil(),
        "nil supervisor scope"
    );
    Ok(format!(
        r"\\.\pipe\Okilum-Sync-{}-{}-{}",
        scope.installation, scope.instance, scope.generation
    ))
}

pub struct PrivatePipe {
    scope: Scope,
    handle: OwnedHandle,
    owner_sid: String,
}
impl PrivatePipe {
    pub(super) fn scope(&self) -> &Scope {
        &self.scope
    }
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
        // dwOpenMode accepts pipe access/creation flags, not READ_CONTROL.
        // DUPLEX grants generic read/write; keep the actual security read-back.
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(name.as_ptr()),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                super::MAX_FRAME as u32,
                super::MAX_FRAME as u32,
                0,
                Some(&attrs),
            )
        };
        if handle.is_invalid() {
            return Err(windows::core::Error::from_win32())
                .context("CreateNamedPipeW(private endpoint)");
        }
        let pipe = Self {
            scope: scope.clone(),
            handle: unsafe { OwnedHandle::from_raw_handle(handle.0) },
            owner_sid: sid,
        };
        pipe.verify().context("private endpoint read-back")?;
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
            .ok()
            .context("GetSecurityInfo(private pipe, SE_KERNEL_OBJECT)")?;
        }
        validate(&Descriptor(sd), &self.owner_sid)?;
        let mut flags = NAMED_PIPE_MODE::default();
        let mut instances = 0;
        unsafe {
            GetNamedPipeInfo(raw, Some(&mut flags), None, None, Some(&mut instances))
                .context("GetNamedPipeInfo(private endpoint)")?;
        }
        validate_pipe_info(flags, instances)?;
        Ok(())
    }
}
impl AsRawHandle for PrivatePipe {
    fn as_raw_handle(&self) -> RawHandle {
        self.handle.as_raw_handle()
    }
}

/// Connected client primitive, not a deadline-bounded Transport. The caller must
/// obtain scope and the expected process through authenticated discovery/launch.
/// No wire bytes are sent until both the descriptor and captured peer pass.
pub struct PrivateClient {
    handle: OwnedHandle,
    // Retain the verified process object for the entire connection lifetime.
    peer: ProcessPeer,
}
impl PrivateClient {
    /// One local open attempt, with no WaitNamedPipe/retry or fallback namespace.
    /// Missing, busy, shared or foreign endpoints fail closed. This does not
    /// impose an absolute deadline on the synchronous open/identity calls.
    pub fn connect(scope: &Scope, peer: ProcessPeer) -> Result<Self> {
        let client = Self {
            handle: Self::open(scope)?,
            peer,
        };
        client.verify()?;
        Ok(client)
    }
    /// Connect, then identify the server from the connected pipe itself (design:
    /// docs/sync-sidecar-discovery.md). `identify` receives the pipe and must return
    /// the verified, retained server process (see `windows_discovery::identify_server`);
    /// no wire byte is sent before it and the usual descriptor/direction checks pass.
    pub fn connect_discovering(
        scope: &Scope,
        identify: impl FnOnce(&OwnedHandle) -> Result<ProcessPeer>,
    ) -> Result<Self> {
        let handle = Self::open(scope)?;
        let peer = identify(&handle)?;
        let client = Self { handle, peer };
        client.verify()?;
        Ok(client)
    }
    fn open(scope: &Scope) -> Result<OwnedHandle> {
        use windows::Win32::{
            Foundation::{GENERIC_READ, GENERIC_WRITE},
            Storage::FileSystem::{
                CreateFileW, FILE_SHARE_MODE, OPEN_EXISTING, SECURITY_IDENTIFICATION,
                SECURITY_SQOS_PRESENT,
            },
        };
        let name = endpoint_name(scope)?;
        let name: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
        let raw = unsafe {
            // Identification prevents even a colliding server from using this
            // connection to impersonate the caller with execution privileges.
            CreateFileW(
                PCWSTR(name.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                None,
            )
        }
        .context("CreateFileW(private pipe client)")?;
        Ok(unsafe { OwnedHandle::from_raw_handle(raw.0) })
    }
    pub fn verify(&self) -> Result<()> {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        unsafe {
            GetSecurityInfo(
                HANDLE(self.handle.as_raw_handle()),
                SE_KERNEL_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                None,
                None,
                None,
                None,
                Some(&mut sd),
            )
            .ok()
            .context("GetSecurityInfo(private pipe client)")?;
        }
        validate(&Descriptor(sd), &current_sid()?).context("private client endpoint security")?;
        let mut flags = NAMED_PIPE_MODE::default();
        let mut instances = 0;
        unsafe {
            GetNamedPipeInfo(
                HANDLE(self.handle.as_raw_handle()),
                Some(&mut flags),
                None,
                None,
                Some(&mut instances),
            )
            .context("GetNamedPipeInfo(private client)")?;
        }
        ensure!(
            flags == PIPE_REJECT_REMOTE_CLIENTS && instances == 1,
            "unexpected client pipe metadata: flags={:#x}, instances={instances}",
            flags.0
        );
        self.peer
            .verify_pipe(self, PeerEnd::Server)
            .context("private client server identity")
    }
}
impl AsRawHandle for PrivateClient {
    fn as_raw_handle(&self) -> RawHandle {
        self.handle.as_raw_handle()
    }
}

#[cfg(test)]
mod tests;
