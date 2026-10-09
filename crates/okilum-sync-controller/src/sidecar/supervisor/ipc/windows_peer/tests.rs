use super::*;
use std::{
    fs::{self, OpenOptions},
    os::windows::io::FromRawHandle,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use uuid::Uuid;
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{
            DuplicateHandle, DUPLICATE_SAME_ACCESS, ERROR_PIPE_CONNECTED, ERROR_PIPE_LISTENING,
        },
        Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX},
        System::{
            Pipes::{
                ConnectNamedPipe, CreateNamedPipeW, PIPE_NOWAIT, PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_TYPE_BYTE,
            },
            Threading::{
                GetCurrentProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
                PROCESS_SYNCHRONIZE,
            },
        },
    },
};

fn current_handle() -> Result<OwnedHandle> {
    // Fixture only: production takes a handle from trusted launch/verification.
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            std::process::id(),
        )?
    };
    Ok(unsafe { OwnedHandle::from_raw_handle(raw.0) })
}
fn duplicate(handle: &impl AsRawHandle) -> Result<OwnedHandle> {
    let mut copy = HANDLE::default();
    unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            HANDLE(handle.as_raw_handle()),
            GetCurrentProcess(),
            &mut copy,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        )?;
        Ok(OwnedHandle::from_raw_handle(copy.0))
    }
}
fn server() -> Result<(String, OwnedHandle)> {
    let name = format!(r"\\.\pipe\Okilum-test-peer-{}", Uuid::new_v4());
    let wide: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
    // Disposable fixture only. Default token DACL is NOT the production private
    // endpoint policy. NOWAIT makes connect polling bounded without a thread.
    let raw = unsafe {
        CreateNamedPipeW(
            PCWSTR(wide.as_ptr()),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            4096,
            4096,
            0,
            None,
        )
    };
    ensure!(
        !raw.is_invalid(),
        "fixture pipe creation: {}",
        windows::core::Error::from_win32()
    );
    Ok((name, unsafe { OwnedHandle::from_raw_handle(raw.0) }))
}
fn connected(pipe: &OwnedHandle) -> Result<bool> {
    match unsafe { ConnectNamedPipe(HANDLE(pipe.as_raw_handle()), None) } {
        Ok(()) => Ok(true),
        Err(e) if e.code() == windows::core::HRESULT::from_win32(ERROR_PIPE_CONNECTED.0) => {
            Ok(true)
        }
        Err(e) if e.code() == windows::core::HRESULT::from_win32(ERROR_PIPE_LISTENING.0) => {
            Ok(false)
        }
        Err(e) => Err(e.into()),
    }
}

#[test]
fn native_pipe_peer_matches_both_ends_and_rejects_wrong_sid_or_role() -> Result<()> {
    let sid = current_sid()?;
    ensure!(
        ProcessPeer::from_verified_process(current_handle()?, "S-1-0-0").is_err(),
        "foreign owner accepted"
    );
    let peer = ProcessPeer::from_verified_process(current_handle()?, &sid)?;
    let (name, server) = server()?;
    let client = OpenOptions::new().read(true).write(true).open(&name)?;
    ensure!(connected(&server)?, "local pair did not connect");
    peer.verify_pipe(&client, PeerEnd::Server)?;
    peer.verify_pipe(&server, PeerEnd::Client)?;
    ensure!(
        peer.verify_pipe(&client, PeerEnd::Client).is_err(),
        "client end accepted as server"
    );
    ensure!(
        peer.verify_pipe(&server, PeerEnd::Server).is_err(),
        "server end accepted as client"
    );
    let file = tempfile::tempfile()?;
    ensure!(
        peer.verify_pipe(&file, PeerEnd::Server).is_err(),
        "ordinary file accepted"
    );
    eprintln!("native pipe peer: both endpoint directions accepted; wrong SID/role/file rejected; sid={sid}");
    Ok(())
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
const FIXTURE: &str = r#"
use std::{fs::OpenOptions, io::Read, time::{Duration, Instant}};
fn main() {
    let name = std::env::args().nth(1).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let _pipe = loop {
        match OpenOptions::new().read(true).write(true).open(&name) {
            Ok(pipe) => break pipe,
            Err(error) if Instant::now() < deadline => {
                if !matches!(error.raw_os_error(), Some(2) | Some(231)) { panic!("{error}"); }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("{error}"),
        }
    };
    let _ = std::io::stdin().read(&mut [0u8]);
}
"#;

#[test]
fn native_pipe_peer_rejects_other_process_and_exited_handle() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("peer.rs");
    let executable = directory.path().join("peer.exe");
    fs::write(&source, FIXTURE)?;
    let compiled = Command::new("rustc")
        .arg("--edition=2021")
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()?;
    ensure!(
        compiled.status.success(),
        "fixture compilation: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let (name, server) = server()?;
    let mut child = ChildGuard(
        Command::new(&executable)
            .arg(&name)
            .stdin(Stdio::piped())
            .spawn()?,
    );
    let sid = current_sid()?;
    let peer = ProcessPeer::from_verified_process(duplicate(&child.0)?, &sid)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !connected(&server)? {
        ensure!(
            child.0.try_wait()?.is_none(),
            "fixture exited before connection"
        );
        ensure!(Instant::now() < deadline, "fixture connection timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
    let wrong = ProcessPeer::from_verified_process(current_handle()?, &sid)?;
    ensure!(
        wrong.verify_pipe(&server, PeerEnd::Client).is_err(),
        "same-user wrong process accepted"
    );
    peer.verify_pipe(&server, PeerEnd::Client)?; // positive control: correct live process accepted
    child.0.kill()?;
    child.0.wait()?;
    ensure!(
        peer.verify_pipe(&server, PeerEnd::Client).is_err(),
        "exited peer accepted"
    );
    eprintln!("native pipe peer: captured child accepted; same-user wrong process and exited peer rejected");
    Ok(())
}
