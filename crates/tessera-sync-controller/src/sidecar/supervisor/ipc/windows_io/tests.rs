use super::*;
use crate::sidecar::{
    supervisor::ipc::{windows_endpoint::PrivatePipe, windows_peer::ProcessPeer, Scope},
    windows::security::current_sid,
};
use anyhow::{ensure, Result};
use uuid::Uuid;
use windows::Win32::{
    Foundation::ERROR_PIPE_CONNECTED,
    System::{
        Pipes::ConnectNamedPipe,
        Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE},
    },
};

fn pair() -> Result<(PrivatePipe, PrivateClient)> {
    let scope = Scope {
        installation: Uuid::new_v4(),
        instance: Uuid::new_v4(),
        generation: Uuid::new_v4(),
    };
    let server = PrivatePipe::create(&scope)?;
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            std::process::id(),
        )?
    };
    let peer = ProcessPeer::from_verified_process(
        unsafe { OwnedHandle::from_raw_handle(raw.0) },
        &current_sid()?,
    )?;
    let client = PrivateClient::connect(&scope, peer)?;
    // The client already connected; no pending connect or borrowed OVERLAPPED.
    let connected = unsafe { ConnectNamedPipe(HANDLE(server.as_raw_handle()), None) };
    ensure!(
        matches!(connected, Err(ref e) if e.code() == HRESULT::from_win32(ERROR_PIPE_CONNECTED.0)),
        "fixture did not establish the expected already-connected state: {connected:?}"
    );
    Ok((server, client))
}

#[test]
fn native_pipe_io_roundtrip_and_fragmented_reply() -> Result<()> {
    let (server, client) = pair()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let server_thread = std::thread::spawn(move || -> io::Result<()> {
        let request = perform(HANDLE(server.as_raw_handle()), Operation::Read(4), deadline)?;
        assert_eq!(&request.bytes[..request.count], b"ping");
        for byte in b"pong" {
            perform(
                HANDLE(server.as_raw_handle()),
                Operation::Write(vec![*byte]),
                deadline,
            )?;
        }
        // Keep the server alive until the client consumes its buffered reply.
        let ack = perform(HANDLE(server.as_raw_handle()), Operation::Read(1), deadline)?;
        assert_eq!(&ack.bytes[..ack.count], b"!");
        Ok(())
    });
    let mut io = ClientIo::new(client, deadline)?;
    io.write_all(b"ping")?;
    let mut reply = [0; 4];
    io.read_exact(&mut reply)?;
    ensure!(&reply == b"pong", "wire reply mismatch");
    io.write_all(b"!")?;
    io.flush()?;
    server_thread.join().expect("fixture panic")?;
    eprintln!("pipe I/O: actual write/read roundtrip and fragmented reply passed");
    Ok(())
}

#[test]
fn native_pipe_io_silent_peer_deadline_cancels_and_releases_worker() -> Result<()> {
    let (_server, client) = pair()?;
    let start = Instant::now();
    let mut io = ClientIo::new(client, start + Duration::from_millis(500))?;
    let error = io.read(&mut [0; 1]).unwrap_err();
    ensure!(
        error.kind() == io::ErrorKind::TimedOut,
        "wrong timeout: {error}"
    );
    ensure!(
        start.elapsed() < Duration::from_secs(2),
        "caller blocked on cancellation"
    );
    ensure!(
        io.read(&mut [0; 1]).unwrap_err().kind() == io::ErrorKind::BrokenPipe,
        "timeout reset connection"
    );
    io.completed.recv_timeout(Duration::from_secs(2))?;
    ensure!(
        io.evidence.pending.load(Ordering::Relaxed) > 0,
        "no actual pending read observed"
    );
    ensure!(
        io.evidence.cancelled.load(Ordering::Relaxed) > 0,
        "no aborted completion observed"
    );
    // Positive control is an actual readable peer, not only a no-traffic timer.
    let (server, client) = pair()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    perform(
        HANDLE(server.as_raw_handle()),
        Operation::Write(vec![42]),
        deadline,
    )?;
    let mut io = ClientIo::new(client, deadline)?;
    let mut byte = [0];
    io.read_exact(&mut byte)?;
    ensure!(byte == [42], "positive read failed");
    eprintln!("pipe I/O: silent peer timed out, worker cancellation drained; readable positive control passed");
    Ok(())
}

#[test]
fn native_pipe_io_fragments_do_not_reset_deadline() -> Result<()> {
    let (server, client) = pair()?;
    let start = Instant::now();
    let deadline = start + Duration::from_secs(1);
    let mut io = ClientIo::new(client, deadline)?;
    perform(
        HANDLE(server.as_raw_handle()),
        Operation::Write(vec![1]),
        deadline,
    )?;
    let mut byte = [0];
    io.read_exact(&mut byte)?;
    ensure!(byte == [1], "first fragment never arrived");
    std::thread::sleep(Duration::from_millis(600));
    perform(
        HANDLE(server.as_raw_handle()),
        Operation::Write(vec![2]),
        deadline,
    )?;
    io.read_exact(&mut byte)?;
    ensure!(byte == [2], "second fragment never arrived");
    let error = io.read(&mut byte).unwrap_err();
    ensure!(
        error.kind() == io::ErrorKind::TimedOut,
        "unexpected error: {error}"
    );
    ensure!(
        start.elapsed() < Duration::from_millis(1300),
        "fragments extended the original deadline"
    );
    io.completed.recv_timeout(Duration::from_secs(2))?;
    eprintln!("pipe I/O: two positive fragments did not reset original deadline");
    Ok(())
}

#[test]
fn native_pipe_io_blocked_write_cancels_after_positive_progress() -> Result<()> {
    let (_server, client) = pair()?;
    let mut io = ClientIo::new(client, Instant::now() + Duration::from_millis(500))?;
    let mut sent = 0;
    let error = loop {
        match io.write(&[7; MAX_FRAME]) {
            Ok(n) => {
                ensure!(n > 0, "empty write");
                sent += n;
            }
            Err(error) => break error,
        }
    };
    ensure!(sent > 0, "no successful write positive control");
    ensure!(
        error.kind() == io::ErrorKind::TimedOut,
        "wrong write error: {error}"
    );
    io.completed.recv_timeout(Duration::from_secs(2))?;
    ensure!(
        io.evidence.pending.load(Ordering::Relaxed) > 0,
        "no pending write observed"
    );
    ensure!(
        io.evidence.cancelled.load(Ordering::Relaxed) > 0,
        "write cancellation not completed"
    );
    eprintln!("pipe I/O: {sent} bytes accepted before blocked write; aborted completion and worker release confirmed");
    Ok(())
}
