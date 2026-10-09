//! macOS peer authentication for the private socket (design:
//! docs/sync-sidecar-discovery.md). After connect, the kernel's audit token for the
//! peer (`LOCAL_PEERTOKEN`, which carries the PID version so reuse cannot be mistaken
//! for the same process) is turned into a `SecCode`; that code must satisfy the
//! injected requirement (Team ID + identifier in releases, a pinned code directory
//! hash in development) and live at the expected executable path. The user is checked
//! first. Nothing here pins a signer: the requirement comes from policy.
use super::{
    code_requirement::CodeRequirement,
    unix_transport::{peer_uid, PeerCheck, PeerEnd},
};
use anyhow::{ensure, Context, Result};
use core_foundation_sys::{
    base::{kCFAllocatorDefault, CFIndex, CFRelease, CFTypeRef},
    data::CFDataCreate,
    dictionary::{
        kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks, CFDictionaryCreate,
        CFDictionaryRef,
    },
    string::{kCFStringEncodingUTF8, CFStringCreateWithBytes, CFStringRef},
    url::{CFURLGetFileSystemRepresentation, CFURLRef},
};
use std::{
    ffi::c_void,
    os::{fd::AsRawFd, unix::net::UnixStream},
    path::{Path, PathBuf},
};

type OSStatus = i32;
type SecCodeRef = *const c_void;
type SecRequirementRef = *const c_void;

#[link(name = "Security", kind = "framework")]
extern "C" {
    static kSecGuestAttributeAudit: CFStringRef;
    fn SecCodeCopyGuestWithAttributes(
        host: SecCodeRef,
        attributes: CFDictionaryRef,
        flags: u32,
        guest: *mut SecCodeRef,
    ) -> OSStatus;
    fn SecRequirementCreateWithString(
        text: CFStringRef,
        flags: u32,
        requirement: *mut SecRequirementRef,
    ) -> OSStatus;
    fn SecCodeCheckValidity(
        code: SecCodeRef,
        flags: u32,
        requirement: SecRequirementRef,
    ) -> OSStatus;
    fn SecCodeCopyStaticCode(
        code: SecCodeRef,
        flags: u32,
        static_code: *mut SecCodeRef,
    ) -> OSStatus;
    fn SecCodeCopyPath(static_code: SecCodeRef, flags: u32, path: *mut CFURLRef) -> OSStatus;
}

const SOL_LOCAL: libc::c_int = 0;
/// `LOCAL_PEERTOKEN` (sys/un.h): the peer's `audit_token_t`.
const LOCAL_PEERTOKEN: libc::c_int = 6;
const DEFAULT_FLAGS: u32 = 0; // kSecCSDefaultFlags
/// Error texts the tests assert on, so wording changes cannot silently decouple them.
pub(crate) const NOT_SATISFIED: &str = "peer does not satisfy the code requirement";
pub(crate) const DIFFERENT_EXECUTABLE: &str = "socket peer runs a different executable";

/// A retained Core Foundation / Security object, released on drop.
struct Owned(CFTypeRef);
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}
fn status(what: &str, code: OSStatus) -> Result<()> {
    ensure!(code == 0, "{what} failed (OSStatus {code})");
    Ok(())
}

/// The peer's audit token as the kernel reports it for this connection.
pub fn peer_audit_token(stream: &UnixStream) -> Result<[u8; 32]> {
    let mut token = [0u8; 32];
    let mut length = token.len() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            SOL_LOCAL,
            LOCAL_PEERTOKEN,
            token.as_mut_ptr().cast(),
            &mut length,
        )
    };
    ensure!(
        result == 0 && length as usize == token.len(),
        "cannot read the socket peer audit token"
    );
    Ok(token)
}
fn guest_code(token: &[u8; 32]) -> Result<Owned> {
    unsafe {
        let data = Owned(CFDataCreate(kCFAllocatorDefault, token.as_ptr(), 32 as CFIndex).cast());
        ensure!(!data.0.is_null(), "cannot wrap the audit token");
        let keys = [kSecGuestAttributeAudit as *const c_void];
        let values = [data.0];
        let attributes = Owned(
            CFDictionaryCreate(
                kCFAllocatorDefault,
                keys.as_ptr(),
                values.as_ptr(),
                1,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            )
            .cast(),
        );
        ensure!(!attributes.0.is_null(), "cannot build the guest attributes");
        let mut guest: SecCodeRef = std::ptr::null();
        status(
            "SecCodeCopyGuestWithAttributes",
            SecCodeCopyGuestWithAttributes(
                std::ptr::null(),
                attributes.0.cast(),
                DEFAULT_FLAGS,
                &mut guest,
            ),
        )?;
        Ok(Owned(guest))
    }
}
fn check_requirement(code: &Owned, requirement: &CodeRequirement) -> Result<()> {
    unsafe {
        let text = requirement.as_str();
        let text = Owned(
            CFStringCreateWithBytes(
                kCFAllocatorDefault,
                text.as_ptr(),
                text.len() as CFIndex,
                kCFStringEncodingUTF8,
                0,
            )
            .cast(),
        );
        ensure!(!text.0.is_null(), "cannot wrap the requirement text");
        let mut compiled: SecRequirementRef = std::ptr::null();
        status(
            "SecRequirementCreateWithString",
            SecRequirementCreateWithString(text.0.cast(), DEFAULT_FLAGS, &mut compiled),
        )?;
        let compiled = Owned(compiled);
        status(
            "SecCodeCheckValidity",
            SecCodeCheckValidity(code.0, DEFAULT_FLAGS, compiled.0),
        )
        .context(NOT_SATISFIED)
    }
}
fn code_path(code: &Owned) -> Result<PathBuf> {
    unsafe {
        let mut static_code: SecCodeRef = std::ptr::null();
        status(
            "SecCodeCopyStaticCode",
            SecCodeCopyStaticCode(code.0, DEFAULT_FLAGS, &mut static_code),
        )?;
        let static_code = Owned(static_code);
        let mut url: CFURLRef = std::ptr::null();
        status(
            "SecCodeCopyPath",
            SecCodeCopyPath(static_code.0, DEFAULT_FLAGS, &mut url),
        )?;
        let url = Owned(url.cast());
        let mut buffer = vec![0u8; libc::PATH_MAX as usize];
        ensure!(
            CFURLGetFileSystemRepresentation(
                url.0.cast(),
                1,
                buffer.as_mut_ptr(),
                buffer.len() as CFIndex
            ) != 0,
            "cannot read the peer's code path"
        );
        let end = buffer
            .iter()
            .position(|&b| b == 0)
            .context("unterminated path")?;
        Ok(PathBuf::from(std::ffi::OsStr::new(
            std::str::from_utf8(&buffer[..end]).context("non-UTF-8 code path")?,
        )))
    }
}

/// Peer must be the same user, satisfy `requirement`, and be the code at `executable`.
/// Both paths are resolved at each check, so an update that retargets a link is
/// followed and a long-lived value never compares against a stale resolution.
pub struct SignedPeer {
    requirement: CodeRequirement,
    executable: PathBuf,
}
impl SignedPeer {
    pub fn new(requirement: CodeRequirement, executable: &Path) -> Result<Self> {
        std::fs::canonicalize(executable).context("the expected executable does not exist")?;
        Ok(Self {
            requirement,
            executable: executable.to_path_buf(),
        })
    }
}
impl PeerCheck for SignedPeer {
    fn verify(&self, stream: &UnixStream, _: PeerEnd) -> Result<()> {
        ensure!(
            peer_uid(stream)? == rustix::process::geteuid().as_raw(),
            "socket peer belongs to another user"
        );
        let token = peer_audit_token(stream)?;
        let code = guest_code(&token)?;
        check_requirement(&code, &self.requirement)?;
        let path = std::fs::canonicalize(code_path(&code)?)?;
        let expected = std::fs::canonicalize(&self.executable)
            .context("the expected executable does not exist")?;
        ensure!(path == expected, DIFFERENT_EXECUTABLE);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
