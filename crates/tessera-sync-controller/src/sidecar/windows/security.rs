//! Native identity reads. Neither task names nor environment variables establish
//! ownership. Returned allocations remain owned until all SID reads finish.
use anyhow::{ensure, Result};
use std::{
    mem::size_of,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
};
use windows::{
    core::{BOOL, PCWSTR, PWSTR},
    Win32::{
        Foundation::{LocalFree, ERROR_INSUFFICIENT_BUFFER, HANDLE, HLOCAL},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            },
            GetSecurityDescriptorOwner, GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, PSID,
            TOKEN_QUERY, TOKEN_USER,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    },
};
struct LocalAllocation(HLOCAL);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(Some(self.0));
        }
    }
}
pub(super) fn sid_string(sid: PSID) -> Result<String> {
    ensure!(!sid.0.is_null(), "security descriptor has no owner SID");
    let mut text = PWSTR::null();
    unsafe {
        ConvertSidToStringSidW(sid, &mut text)?;
    }
    let _allocation = LocalAllocation(HLOCAL(text.0.cast()));
    Ok(unsafe { text.to_string()? })
}
/// Reads the process token, never USERNAME or a caller-supplied SID string.
pub fn current_sid() -> Result<String> {
    let mut raw = HANDLE::default();
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw)?;
    }
    let token = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let token = HANDLE(token.as_raw_handle());
    let mut required = 0;
    let sizing = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut required) };
    ensure!(
        sizing.as_ref().is_err_and(
            |e| e.code() == windows::core::HRESULT::from_win32(ERROR_INSUFFICIENT_BUFFER.0)
        ),
        "unexpected token sizing response"
    );
    ensure!(
        (size_of::<TOKEN_USER>() as u32..=65536).contains(&required),
        "invalid token buffer size"
    );
    // TOKEN_USER contains a pointer; a byte Vec does not guarantee its alignment.
    let mut storage = vec![0usize; (required as usize).div_ceil(size_of::<usize>())];
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(storage.as_mut_ptr().cast()),
            required,
            &mut required,
        )?;
        let user = &*storage.as_ptr().cast::<TOKEN_USER>();
        sid_string(user.User.Sid)
    }
}
/// Parse the scheduler's descriptor with Win32, including SDDL aliases. An
/// absent owner is an error, not permission to assume the current user.
pub fn descriptor_owner_sid(sddl: &str) -> Result<String> {
    ensure!(
        !sddl.contains('\0') && sddl.len() <= 65536,
        "invalid security descriptor text"
    );
    let wide: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(wide.as_ptr()),
            1,
            &mut descriptor,
            None,
        )?;
    }
    let _allocation = LocalAllocation(HLOCAL(descriptor.0));
    let mut owner = PSID::default();
    let mut defaulted = BOOL::default();
    unsafe {
        GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted)?;
    }
    sid_string(owner)
}
/// Resolve scheduler DOMAIN\user spelling through Windows, never string aliases.
pub fn resolve_account_sid(account: &str) -> Result<String> {
    if account.starts_with("S-1-") {
        ensure!(
            account.len() <= 256
                && account
                    .bytes()
                    .all(|c| c.is_ascii_digit() || c == b'S' || c == b'-'),
            "invalid SID spelling"
        );
        return descriptor_owner_sid(&format!("O:{account}"));
    }
    ensure!(
        !account.is_empty() && !account.contains('\0'),
        "invalid account name"
    );
    use windows::Win32::Security::{LookupAccountNameW, SID_NAME_USE};
    let name: Vec<_> = account.encode_utf16().chain(Some(0)).collect();
    let mut sid_size = 0;
    let mut domain_size = 0;
    let mut kind = SID_NAME_USE::default();
    let result = unsafe {
        LookupAccountNameW(
            PCWSTR::null(),
            PCWSTR(name.as_ptr()),
            None,
            &mut sid_size,
            None,
            &mut domain_size,
            &mut kind,
        )
    };
    ensure!(
        result.is_err_and(
            |e| e.code() == windows::core::HRESULT::from_win32(ERROR_INSUFFICIENT_BUFFER.0)
        ),
        "account SID sizing failed"
    );
    ensure!(
        (1..=65536).contains(&sid_size) && domain_size <= 65536,
        "invalid account SID size"
    );
    let mut sid = vec![0usize; (sid_size as usize).div_ceil(size_of::<usize>())];
    let mut domain = vec![0u16; domain_size as usize];
    let raw = PSID(sid.as_mut_ptr().cast());
    unsafe {
        LookupAccountNameW(
            PCWSTR::null(),
            PCWSTR(name.as_ptr()),
            Some(raw),
            &mut sid_size,
            Some(PWSTR(domain.as_mut_ptr())),
            &mut domain_size,
            &mut kind,
        )?;
    }
    sid_string(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn descriptor_aliases_are_resolved_and_missing_owner_is_rejected() {
        assert_eq!(descriptor_owner_sid("O:SY").unwrap(), "S-1-5-18");
        assert!(descriptor_owner_sid("D:").is_err());
        assert!(descriptor_owner_sid("O:SY\0O:BA").is_err());
        assert!(descriptor_owner_sid("not sddl").is_err());
    }
    #[test]
    fn process_token_roundtrips_through_descriptor() {
        let sid = current_sid().unwrap();
        assert_eq!(descriptor_owner_sid(&format!("O:{sid}")).unwrap(), sid);
    }
}
