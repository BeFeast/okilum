use super::*;
use crate::sidecar::{
    supervisor::ipc::{exchange, Command, OwnedRuntime, Request, Server, Status},
    windows::security::current_sid,
};
use std::{
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};
use uuid::Uuid;
use windows::Win32::{
    Foundation::{ERROR_PIPE_CONNECTED, HANDLE},
    System::{
        Pipes::{ConnectNamedPipe, PeekNamedPipe},
        Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE},
    },
};
fn binding() -> Result<Binding> {
    Ok(Binding {
        installation: Uuid::new_v4(),
        instance: Uuid::new_v4(),
        owner: current_sid()?,
        supervisor: "fixture-supervisor".into(),
        state_directory: "fixture-private-state".into(),
        device_identity: "fixture-device".into(),
    })
}
fn scope(binding: &Binding) -> Scope {
    Scope {
        installation: binding.installation,
        instance: binding.instance,
        generation: Uuid::new_v4(),
    }
}
fn peer() -> Result<ProcessPeer> {
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
fn available(pipe: &PrivatePipe) -> Result<u32> {
    let mut count = 0;
    unsafe {
        PeekNamedPipe(
            HANDLE(pipe.as_raw_handle()),
            None,
            0,
            None,
            Some(&mut count),
            None,
        )?;
    }
    Ok(count)
}
fn pair() -> Result<(Binding, Scope, PrivatePipe, WindowsTransport)> {
    let b = binding()?;
    let s = scope(&b);
    let pipe = PrivatePipe::create(&s)?;
    let io = WindowsTransport::connect(
        b.clone(),
        s.clone(),
        peer()?,
        Instant::now() + Duration::from_secs(5),
    )?;
    let connected = unsafe { ConnectNamedPipe(HANDLE(pipe.as_raw_handle()), None) };
    ensure!(
        matches!(connected, Err(ref e) if e.code() == windows::core::HRESULT::from_win32(ERROR_PIPE_CONNECTED.0)),
        "fixture not connected"
    );
    Ok((b, s, pipe, io))
}

#[test]
fn native_transport_binding_scope_and_unverified_io_rejected_before_wire() -> Result<()> {
    for field in 0..7 {
        let (b, s, pipe, mut io) = pair()?;
        let mut changed = b.clone();
        match field {
            0 => changed.installation = Uuid::new_v4(),
            1 => changed.instance = Uuid::new_v4(),
            2 => changed.owner.push('x'),
            3 => changed.supervisor.push('x'),
            4 => changed.state_directory.push('x'),
            5 => changed.device_identity.push('x'),
            _ => {}
        }
        let mut altered_scope = s.clone();
        if field == 6 {
            altered_scope.generation = Uuid::new_v4();
        }
        ensure!(
            io.verify_peer(&changed, &altered_scope).is_err(),
            "altered prepared identity accepted"
        );
        ensure!(
            io.write(b"forbidden").is_err(),
            "poisoned transport wrote bytes"
        );
        ensure!(
            io.verify_peer(&b, &s).is_err(),
            "poisoned transport revived"
        );
        ensure!(available(&pipe)? == 0, "refusal emitted bytes");
    }
    let (_, _, pipe, mut io) = pair()?;
    ensure!(io.write(b"unverified").is_err(), "unverified wire access");
    ensure!(available(&pipe)? == 0, "unverified bytes escaped");
    drop(io);
    drop(pipe);
    // Positive control for the exact same kernel availability probe.
    let (b, s, pipe, mut io) = pair()?;
    io.verify_peer(&b, &s)?;
    io.write_all(b"visible")?;
    ensure!(
        available(&pipe)? == 7,
        "wire absence probe lacked positive control"
    );
    eprintln!("transport: all binding fields/generation and unverified I/O refused before wire; positive probe detected 7 bytes");
    Ok(())
}

#[test]
fn native_transport_constructor_refuses_wrong_owner_and_endpoint_scope() -> Result<()> {
    let b = binding()?;
    let s = scope(&b);
    let pipe = PrivatePipe::create(&s)?;
    let mut foreign = b.clone();
    foreign.owner = "S-1-0-0".into();
    ensure!(
        WindowsTransport::connect(
            foreign,
            s.clone(),
            peer()?,
            Instant::now() + Duration::from_secs(5)
        )
        .is_err(),
        "foreign prepared owner accepted"
    );
    let client = WindowsTransport::connect(
        b.clone(),
        s.clone(),
        peer()?,
        Instant::now() + Duration::from_secs(5),
    )?;
    let mut changed = s.clone();
    changed.generation = Uuid::new_v4();
    ensure!(
        WindowsTransport::accept(
            b,
            changed,
            pipe,
            peer()?,
            Instant::now() + Duration::from_secs(5)
        )
        .is_err(),
        "pipe from another generation accepted"
    );
    drop(client);
    eprintln!("transport: prepared owner refused before open with owner positive control; mismatched server endpoint refused");
    Ok(())
}

struct Runtime {
    probes: Arc<AtomicUsize>,
    stops: Arc<AtomicUsize>,
    intents: Arc<AtomicUsize>,
}
impl OwnedRuntime for Runtime {
    fn status(&mut self) -> Result<Status> {
        self.probes.fetch_add(1, Ordering::SeqCst);
        Ok(Status::Running)
    }
    fn verify_stop_intent(&mut self, _: &Binding) -> Result<()> {
        self.intents.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn stop_owned(&mut self) -> Result<Status> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        Ok(Status::Stopped)
    }
}
#[test]
fn native_transport_protocol_status_stop_and_idempotent_reply() -> Result<()> {
    let b = binding()?;
    let probes = Arc::new(AtomicUsize::new(0));
    let stops = Arc::new(AtomicUsize::new(0));
    let intents = Arc::new(AtomicUsize::new(0));
    let mut server = Server::new(
        b.clone(),
        Runtime {
            probes: probes.clone(),
            stops: stops.clone(),
            intents: intents.clone(),
        },
    );
    let s = server.scope().clone();
    let pipe = PrivatePipe::create(&s)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let expected = peer()?;
    let sb = b.clone();
    let ss = s.clone();
    let (release, released) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || -> Result<()> {
        let mut transport = WindowsTransport::accept(sb, ss, pipe, expected, deadline)?;
        for _ in 0..3 {
            server.serve_one(&mut transport)?;
        }
        let _ = released.recv_timeout(Duration::from_secs(5));
        Ok(())
    });
    let mut transport = WindowsTransport::connect(b.clone(), s.clone(), peer()?, deadline)?;
    ensure!(
        exchange(
            &mut transport,
            &b,
            &Request::new(s.clone(), Command::Status)
        )? == Status::Running,
        "status mismatch"
    );
    for _ in 0..2 {
        ensure!(
            exchange(&mut transport, &b, &Request::new(s.clone(), Command::Stop))?
                == Status::Stopped,
            "stop mismatch"
        );
    }
    release.send(())?;
    worker.join().expect("server fixture panicked")?;
    ensure!(
        probes.load(Ordering::SeqCst) == 1
            && stops.load(Ordering::SeqCst) == 1
            && intents.load(Ordering::SeqCst) == 2,
        "runtime side effects not correlated/idempotent"
    );
    eprintln!("transport: native framed Status/Stop/repeated Stop passed; fixture runtime probed once, stopped once, durable gate checked twice");
    Ok(())
}
