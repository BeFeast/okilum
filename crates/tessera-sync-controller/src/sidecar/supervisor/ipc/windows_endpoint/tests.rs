use super::*;
use std::fs::OpenOptions;
use uuid::Uuid;
use windows::Win32::{
    Foundation::{GetHandleInformation, ERROR_ACCESS_DENIED, HANDLE_FLAG_INHERIT},
    Security::{
        CreateRestrictedToken, CreateWellKnownSid, ImpersonateLoggedOnUser, RevertToSelf,
        WinWorldSid, DISABLE_MAX_PRIVILEGE, SID_AND_ATTRIBUTES, TOKEN_DUPLICATE, TOKEN_QUERY,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};
fn scope() -> Scope {
    Scope {
        installation: Uuid::new_v4(),
        instance: Uuid::new_v4(),
        generation: Uuid::new_v4(),
    }
}
fn read_descriptor(handle: HANDLE) -> Result<Descriptor> {
    let mut sd = PSECURITY_DESCRIPTOR::default();
    unsafe {
        GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            None,
            None,
            None,
            None,
            Some(&mut sd),
        )
        .ok()
        .context("GetSecurityInfo(fixture pipe, SE_KERNEL_OBJECT)")?;
    }
    Ok(Descriptor(sd))
}

fn sddl(sd: &Descriptor) -> Result<String> {
    use windows::{
        core::PWSTR,
        Win32::Security::Authorization::ConvertSecurityDescriptorToStringSecurityDescriptorW,
    };
    let mut text = PWSTR::null();
    unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd.0,
            1,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut text,
            None,
        )?;
        let result = text.to_string();
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        Ok(result?)
    }
}

#[test]
fn native_private_pipe_owner_acl_collision_and_lifetime() -> Result<()> {
    let scope = scope();
    let name = endpoint_name(&scope)?;
    ensure!(
        name.starts_with(r"\\.\pipe\Tessera-Sync-"),
        "not a fixed local name"
    );
    let pipe = PrivatePipe::create(&scope)?;
    pipe.verify()?; // positive read-back includes exact owner, DACL, type, max instances
    let mut flags = 0;
    unsafe {
        GetHandleInformation(HANDLE(pipe.as_raw_handle()), &mut flags)?;
    }
    ensure!(
        flags & HANDLE_FLAG_INHERIT.0 == 0,
        "pipe inherited by child"
    );
    ensure!(
        PrivatePipe::create(&scope).is_err(),
        "existing instance adopted"
    );
    pipe.verify()?;
    let client = OpenOptions::new().read(true).write(true).open(&name)?;
    drop(client);
    drop(pipe);
    let recreated = PrivatePipe::create(&scope)?;
    recreated.verify()?;
    let mut invalid = scope.clone();
    invalid.generation = Uuid::nil();
    ensure!(PrivatePipe::create(&invalid).is_err(), "nil scope admitted");
    eprintln!("private pipe: exact owner/DACL read-back, owner read/write, collision refusal, non-inheritance and recreate after release passed");
    Ok(())
}

struct Impersonation;
impl Drop for Impersonation {
    fn drop(&mut self) {
        // Never let a failed fixture leak an impersonated token into another test.
        if unsafe { RevertToSelf() }.is_err() {
            std::process::abort();
        }
    }
}
fn restricted_token() -> Result<OwnedHandle> {
    let mut raw = HANDLE::default();
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY, &mut raw)?;
    }
    let token = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let mut world = [0usize; 16];
    let mut size = std::mem::size_of_val(&world) as u32;
    let world_sid = PSID(world.as_mut_ptr().cast());
    unsafe {
        CreateWellKnownSid(WinWorldSid, None, Some(world_sid), &mut size)?;
    }
    let restricted = [SID_AND_ATTRIBUTES {
        Sid: world_sid,
        Attributes: 0,
    }];
    let mut output = HANDLE::default();
    unsafe {
        CreateRestrictedToken(
            HANDLE(token.as_raw_handle()),
            DISABLE_MAX_PRIVILEGE,
            None,
            None,
            Some(&restricted),
            &mut output,
        )?;
        Ok(OwnedHandle::from_raw_handle(output.0))
    }
}
#[test]
fn native_private_pipe_denies_restricted_token_with_owner_positive_control() -> Result<()> {
    let scope = scope();
    let pipe = PrivatePipe::create(&scope)?;
    let name = endpoint_name(&scope)?;
    let token = restricted_token()?;
    let attempt = {
        unsafe {
            ImpersonateLoggedOnUser(HANDLE(token.as_raw_handle()))?;
        }
        let _restore = Impersonation;
        OpenOptions::new().read(true).write(true).open(&name)
    };
    let error = attempt
        .err()
        .ok_or_else(|| anyhow::anyhow!("restricted token opened private pipe"))?;
    ensure!(
        error.raw_os_error() == Some(ERROR_ACCESS_DENIED.0 as i32),
        "expected ACL denial, got {error}"
    );
    // Same endpoint, normal owner token: ensures failure was not a missing/busy pipe.
    let client = OpenOptions::new().read(true).write(true).open(&name)?;
    pipe.verify()?;
    drop(client);
    eprintln!("private pipe: restricted-token read/write denied with ERROR_ACCESS_DENIED; normal owner read/write accepted");
    Ok(())
}

#[test]
fn native_private_pipe_preserves_shared_existing_endpoint() -> Result<()> {
    let scope = scope();
    let sid = current_sid()?;
    let sd = descriptor(&format!("O:{sid}D:P(A;;FA;;;WD)"))?;
    let attrs = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0 .0,
        bInheritHandle: BOOL(0),
    };
    let name = endpoint_name(&scope)?;
    let wide: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
    let raw = unsafe {
        CreateNamedPipeW(
            PCWSTR(wide.as_ptr()),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            4096,
            4096,
            0,
            Some(&attrs),
        )
    };
    if raw.is_invalid() {
        return Err(windows::core::Error::from_win32()).context("CreateNamedPipeW(shared fixture)");
    }
    let foreign = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let mut flags = NAMED_PIPE_MODE::default();
    let mut max_instances = 0;
    unsafe {
        GetNamedPipeInfo(
            HANDLE(foreign.as_raw_handle()),
            Some(&mut flags),
            None,
            None,
            Some(&mut max_instances),
        )
        .context("GetNamedPipeInfo(shared fixture)")?;
    }
    eprintln!(
        "shared fixture pipe read-back: flags={:#010x}, max_instances={max_instances}",
        flags.0
    );
    let before = read_descriptor(HANDLE(foreign.as_raw_handle()))?;
    ensure!(
        validate(&before, &sid).is_err(),
        "shared pipe treated as private"
    );
    ensure!(
        PrivatePipe::create(&scope).is_err(),
        "shared endpoint adopted"
    );
    let after = read_descriptor(HANDLE(foreign.as_raw_handle()))?;
    ensure!(
        sddl(&before)? == sddl(&after)?,
        "foreign security descriptor changed"
    );
    ensure!(
        validate(&after, &sid).is_err(),
        "foreign permissions repaired"
    );
    // Still usable by its owner; refusal did not disconnect/close the existing server.
    let client = OpenOptions::new().read(true).write(true).open(name)?;
    drop(client);
    eprintln!(
        "private pipe: shared collision refused, existing ACL still shared and server still usable"
    );
    Ok(())
}

#[test]
fn pipe_metadata_requires_server_byte_remote_rejection_and_single_instance() -> Result<()> {
    use windows::Win32::System::Pipes::PIPE_TYPE_MESSAGE;
    let expected = PIPE_SERVER_END | PIPE_REJECT_REMOTE_CLIENTS;
    validate_pipe_info(expected, 1)?;
    for flags in [
        PIPE_SERVER_END,
        PIPE_REJECT_REMOTE_CLIENTS,
        expected | PIPE_TYPE_MESSAGE,
        NAMED_PIPE_MODE(expected.0 | 0x100),
        NAMED_PIPE_MODE(0),
    ] {
        ensure!(
            validate_pipe_info(flags, 1).is_err(),
            "unexpected flags accepted: {:#x}",
            flags.0
        );
    }
    for instances in [0, 2, 255, u32::MAX] {
        ensure!(
            validate_pipe_info(expected, instances).is_err(),
            "invalid instance limit accepted"
        );
    }
    Ok(())
}

fn current_peer() -> Result<ProcessPeer> {
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            std::process::id(),
        )?
    };
    ProcessPeer::from_verified_process(
        unsafe { OwnedHandle::from_raw_handle(raw.0) },
        &current_sid()?,
    )
}

#[test]
fn native_private_client_accepts_owner_and_refuses_missing_busy_or_nil() -> Result<()> {
    let scope = scope();
    ensure!(
        PrivateClient::connect(&scope, current_peer()?).is_err(),
        "missing accepted"
    );
    let server = PrivatePipe::create(&scope)?;
    let client = PrivateClient::connect(&scope, current_peer()?)?;
    client.verify()?;
    let mut flags = 0;
    unsafe {
        GetHandleInformation(HANDLE(client.as_raw_handle()), &mut flags)?;
    }
    ensure!(flags & HANDLE_FLAG_INHERIT.0 == 0, "client inherited");
    ensure!(
        PrivateClient::connect(&scope, current_peer()?).is_err(),
        "busy accepted"
    );
    let mut invalid = scope.clone();
    invalid.generation = Uuid::nil();
    ensure!(
        PrivateClient::connect(&invalid, current_peer()?).is_err(),
        "nil accepted"
    );
    server.verify()?;
    eprintln!("private client: captured owner accepted; missing/busy/nil refused; non-inheritance confirmed");
    Ok(())
}

#[test]
fn native_private_client_rejects_shared_descriptor() -> Result<()> {
    let scope = scope();
    let sid = current_sid()?;
    let sd = descriptor(&format!("O:{sid}D:P(A;;FA;;;WD)"))?;
    let attrs = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0 .0,
        bInheritHandle: BOOL(0),
    };
    let name: Vec<_> = endpoint_name(&scope)?
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let raw = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            4096,
            4096,
            0,
            Some(&attrs),
        )
    };
    if raw.is_invalid() {
        return Err(windows::core::Error::from_win32()).context("shared client fixture");
    }
    let server = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let before = sddl(&read_descriptor(raw)?)?;
    let error = PrivateClient::connect(&scope, current_peer()?)
        .err()
        .ok_or_else(|| anyhow::anyhow!("shared endpoint accepted"))?;
    ensure!(
        format!("{error:#}").contains("private client endpoint security"),
        "wrong rejection: {error:#}"
    );
    ensure!(
        before == sddl(&read_descriptor(raw)?)?,
        "descriptor changed"
    );
    drop(server);
    let _private = PrivatePipe::create(&scope)?;
    let _client = PrivateClient::connect(&scope, current_peer()?)?;
    eprintln!("private client: shared descriptor refused unchanged; private replacement positive control accepted");
    Ok(())
}

#[test]
fn native_private_client_rejects_wrong_captured_server() -> Result<()> {
    use std::process::{Child, Command, Stdio};
    use windows::Win32::Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS};
    struct ChildGuard(Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let child = ChildGuard(
        Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 30",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let mut raw = HANDLE::default();
    unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            HANDLE(child.0.as_raw_handle()),
            GetCurrentProcess(),
            &mut raw,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        )?;
    }
    let wrong = ProcessPeer::from_verified_process(
        unsafe { OwnedHandle::from_raw_handle(raw.0) },
        &current_sid()?,
    )?;
    let scope = scope();
    let server = PrivatePipe::create(&scope)?;
    let error = PrivateClient::connect(&scope, wrong)
        .err()
        .ok_or_else(|| anyhow::anyhow!("wrong captured server accepted"))?;
    ensure!(
        format!("{error:#}").contains("pipe peer is not the captured process"),
        "wrong rejection: {error:#}"
    );
    server.verify()?;
    drop(server);
    let _server = PrivatePipe::create(&scope)?;
    let _client = PrivateClient::connect(&scope, current_peer()?)?;
    eprintln!(
        "private client: same-user wrong captured process refused; actual captured server accepted"
    );
    Ok(())
}
