//! Exact regular-selection text for the isolated Linux Wayland editor candidate.
//! No clipboard ownership changes, core hooks, or external helper executable.
use gpui::{App, Task};
use gpui_component::input::clipboard::{
    ClipboardReadError as Error, ClipboardReadRequest, ExactClipboardProvider,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(target_os = "linux")]
const MAX_BYTES: usize = 8 * 1024 * 1024;

pub struct WaylandClipboard {
    // The caller must bind GPUI to this same named socket before initializing it.
    socket: Option<PathBuf>,
    seat: Option<String>,
}
impl WaylandClipboard {
    pub fn new(socket: Option<PathBuf>, seat: Option<String>) -> Self {
        Self { socket, seat }
    }
}
impl ExactClipboardProvider for WaylandClipboard {
    fn read_text(
        &self,
        request: ClipboardReadRequest,
        cx: &App,
    ) -> Task<Result<Option<String>, Error>> {
        let socket = self.socket.clone();
        let seat = self.seat.clone();
        let deadline = Instant::now() + TIMEOUT;
        cx.background_executor().spawn(async move {
            #[cfg(target_os = "linux")]
            {
                linux::read(socket.ok_or(Error::Unsupported)?, seat, request, deadline)
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (socket, seat, request, deadline);
                Err(Error::Unsupported)
            }
        })
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use rustix::{
        event::{poll, PollFd, PollFlags, Timespec},
        io::Errno,
        net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType},
        pipe::{pipe_with, PipeFlags},
    };
    use std::{
        collections::HashMap,
        os::{
            fd::{AsFd, OwnedFd},
            unix::net::UnixStream,
        },
    };
    use wayland_client::{
        backend::ObjectId,
        delegate_noop, event_created_child,
        protocol::{wl_callback, wl_registry, wl_seat},
        Connection, Dispatch, EventQueue, Proxy, QueueHandle,
    };
    use wayland_protocols_wlr::data_control::v1::client::{
        zwlr_data_control_device_v1::{self as device, ZwlrDataControlDeviceV1 as Device},
        zwlr_data_control_manager_v1::ZwlrDataControlManagerV1 as Manager,
        zwlr_data_control_offer_v1::{self as offer, ZwlrDataControlOfferV1 as Offer},
    };

    struct Seat {
        global: u32,
        proxy: wl_seat::WlSeat,
        name: Option<String>,
    }
    #[derive(Default)]
    struct State {
        manager: Option<(u32, Manager)>,
        seats: Vec<Seat>,
        selected_seat: Option<u32>,
        selection: Option<Offer>,
        pinned: Option<ObjectId>,
        offers: HashMap<ObjectId, Vec<String>>,
        failure: Option<Error>,
        synced: u64,
    }
    impl State {
        fn check(&self) -> Result<(), Error> {
            self.failure.clone().map_or(Ok(()), Err)
        }
        fn selection_changed(&mut self, next: Option<Offer>) {
            if let Some(pinned) = &self.pinned {
                if next.as_ref().map(Proxy::id).as_ref() != Some(pinned) {
                    self.failure = Some(Error::ChangedOffer);
                }
            }
            self.selection = next;
        }
    }
    impl Dispatch<wl_registry::WlRegistry, ()> for State {
        fn event(
            state: &mut Self,
            registry: &wl_registry::WlRegistry,
            event: wl_registry::Event,
            _: &(),
            _: &Connection,
            qh: &QueueHandle<Self>,
        ) {
            match event {
                wl_registry::Event::Global {
                    name,
                    interface,
                    version,
                } => {
                    if interface == "zwlr_data_control_manager_v1" {
                        state.manager = Some((name, registry.bind(name, version.min(2), qh, ())));
                    } else if interface == "wl_seat" {
                        let proxy = registry.bind(name, version.min(7), qh, name);
                        state.seats.push(Seat {
                            global: name,
                            proxy,
                            name: None,
                        });
                    }
                }
                wl_registry::Event::GlobalRemove { name } => {
                    if state.selected_seat == Some(name)
                        || state.manager.as_ref().is_some_and(|(id, _)| *id == name)
                    {
                        state.failure = Some(Error::ChangedOffer);
                    }
                    state.seats.retain(|seat| seat.global != name);
                }
                _ => {}
            }
        }
    }
    impl Dispatch<wl_seat::WlSeat, u32> for State {
        fn event(
            state: &mut Self,
            _: &wl_seat::WlSeat,
            event: wl_seat::Event,
            global: &u32,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let wl_seat::Event::Name { name } = event {
                if let Some(seat) = state.seats.iter_mut().find(|seat| seat.global == *global) {
                    seat.name = Some(name);
                }
            }
        }
    }
    impl Dispatch<wl_callback::WlCallback, u64> for State {
        fn event(
            state: &mut Self,
            _: &wl_callback::WlCallback,
            _: wl_callback::Event,
            sequence: &u64,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            state.synced = state.synced.max(*sequence);
        }
    }
    impl Dispatch<Device, ()> for State {
        fn event(
            state: &mut Self,
            _: &Device,
            event: device::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                device::Event::DataOffer { id } => {
                    state.offers.entry(id.id()).or_default();
                }
                device::Event::Selection { id } => state.selection_changed(id),
                device::Event::Finished => state.failure = Some(Error::ChangedOffer),
                _ => {}
            }
        }
        event_created_child!(State, Device, [0 => (Offer, ())]);
    }
    impl Dispatch<Offer, ()> for State {
        fn event(
            state: &mut Self,
            proxy: &Offer,
            event: offer::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let offer::Event::Offer { mime_type } = event {
                state.offers.entry(proxy.id()).or_default().push(mime_type);
            }
        }
    }
    delegate_noop!(State: ignore Manager);

    fn remaining(deadline: Instant, request: &ClipboardReadRequest) -> Result<Timespec, Error> {
        if request.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let duration = deadline
            .checked_duration_since(Instant::now())
            .ok_or(Error::Timeout)?
            .min(Duration::from_millis(20));
        Ok(Timespec {
            tv_sec: duration.as_secs() as _,
            tv_nsec: duration.subsec_nanos() as _,
        })
    }
    fn connect(
        path: &PathBuf,
        deadline: Instant,
        request: &ClipboardReadRequest,
    ) -> Result<Connection, Error> {
        remaining(deadline, request)?;
        let fd = rustix::net::socket_with(
            AddressFamily::UNIX,
            SocketType::STREAM,
            SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
            None,
        )
        .map_err(|_| Error::Io)?;
        let address = SocketAddrUnix::new(path).map_err(|_| Error::Unsupported)?;
        match rustix::net::connect(&fd, &address) {
            Ok(()) => {}
            Err(Errno::INPROGRESS) | Err(Errno::AGAIN) => loop {
                let mut fds = [PollFd::new(&fd, PollFlags::OUT)];
                match poll(&mut fds, Some(&remaining(deadline, request)?)) {
                    Ok(0) | Err(Errno::INTR) => continue,
                    Ok(_) => {
                        rustix::net::sockopt::socket_error(&fd)
                            .map_err(|_| Error::Io)?
                            .map_err(|_| Error::Io)?;
                        break;
                    }
                    Err(_) => return Err(Error::Io),
                }
            },
            Err(_) => return Err(Error::Io),
        }
        remaining(deadline, request)?;
        Connection::from_socket(UnixStream::from(fd)).map_err(|_| Error::Io)
    }

    /// Pump both transport and received data; every wait shares the same deadline.
    fn pump(
        connection: &Connection,
        queue: &mut EventQueue<State>,
        state: &mut State,
        pipe: Option<&OwnedFd>,
        bytes: &mut Vec<u8>,
        deadline: Instant,
        request: &ClipboardReadRequest,
    ) -> Result<bool, Error> {
        remaining(deadline, request)?;
        queue.dispatch_pending(state).map_err(|_| Error::Io)?;
        state.check()?;
        let needs_write = match connection.flush() {
            Ok(()) => false,
            Err(wayland_client::backend::WaylandError::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock =>
            {
                true
            }
            Err(_) => return Err(Error::Io),
        };
        let Some(guard) = queue.prepare_read() else {
            return Ok(false);
        };
        let mut fds = vec![PollFd::new(
            connection,
            PollFlags::IN
                | if needs_write {
                    PollFlags::OUT
                } else {
                    PollFlags::empty()
                },
        )];
        if let Some(pipe) = pipe {
            fds.push(PollFd::new(pipe, PollFlags::IN));
        }
        match poll(&mut fds, Some(&remaining(deadline, request)?)) {
            Ok(_) => {}
            Err(Errno::INTR) => return Ok(false),
            Err(_) => return Err(Error::Io),
        }
        let connection_ready = fds[0]
            .revents()
            .intersects(PollFlags::IN | PollFlags::ERR | PollFlags::HUP);
        let pipe_ready = fds.get(1).is_some_and(|fd| {
            fd.revents()
                .intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR)
        });
        drop(fds);
        if connection_ready {
            match guard.read() {
                Ok(_) => {}
                Err(wayland_client::backend::WaylandError::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return Err(Error::Io),
            }
        } else {
            drop(guard);
        }
        queue.dispatch_pending(state).map_err(|_| Error::Io)?;
        state.check()?;
        if pipe_ready {
            return drain_pipe(pipe.unwrap(), bytes, deadline, request, MAX_BYTES);
        }
        Ok(false)
    }
    fn drain_pipe(
        pipe: &OwnedFd,
        bytes: &mut Vec<u8>,
        deadline: Instant,
        request: &ClipboardReadRequest,
        limit: usize,
    ) -> Result<bool, Error> {
        loop {
            remaining(deadline, request)?;
            let mut chunk = [0u8; 16384];
            match rustix::io::read(pipe, &mut chunk) {
                Ok(0) => return Ok(true),
                Ok(count) => {
                    if bytes.len().saturating_add(count) > limit {
                        return Err(Error::TooLarge);
                    }
                    bytes.extend_from_slice(&chunk[..count]);
                }
                Err(Errno::AGAIN) => return Ok(false),
                Err(Errno::INTR) => continue,
                Err(_) => return Err(Error::Io),
            }
        }
    }
    fn decode(bytes: Vec<u8>) -> Result<Option<String>, Error> {
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| Error::InvalidUtf8)
    }
    fn barrier(
        connection: &Connection,
        queue: &mut EventQueue<State>,
        state: &mut State,
        deadline: Instant,
        request: &ClipboardReadRequest,
    ) -> Result<(), Error> {
        let target = state.synced + 1;
        connection.display().sync(&queue.handle(), target);
        while state.synced < target {
            pump(
                connection,
                queue,
                state,
                None,
                &mut Vec::new(),
                deadline,
                request,
            )?;
        }
        queue.dispatch_pending(state).map_err(|_| Error::Io)?;
        state.check()
    }

    pub(super) fn read(
        path: PathBuf,
        requested_seat: Option<String>,
        request: ClipboardReadRequest,
        deadline: Instant,
    ) -> Result<Option<String>, Error> {
        let connection = connect(&path, deadline, &request)?;
        let mut queue = connection.new_event_queue::<State>();
        let qh = queue.handle();
        let mut state = State::default();
        let _registry = connection.display().get_registry(&qh, ());
        barrier(&connection, &mut queue, &mut state, deadline, &request)?;
        // Seat name events follow bindings requested during the first barrier.
        barrier(&connection, &mut queue, &mut state, deadline, &request)?;
        let manager = state.manager.as_ref().ok_or(Error::Unsupported)?.1.clone();
        let eligible: Vec<_> = state
            .seats
            .iter()
            .filter(|seat| {
                requested_seat
                    .as_ref()
                    .is_none_or(|name| seat.name.as_ref() == Some(name))
            })
            .collect();
        let seat = match eligible.as_slice() {
            [seat] => *seat,
            [] => return Err(Error::Unavailable),
            _ => return Err(Error::AmbiguousSeat),
        };
        state.selected_seat = Some(seat.global);
        let _device = manager.get_data_device(&seat.proxy, &qh, ());
        barrier(&connection, &mut queue, &mut state, deadline, &request)?;
        let Some(offer) = state.selection.clone() else {
            return Ok(None);
        };
        let mimes = state.offers.get(&offer.id()).ok_or(Error::Unavailable)?;
        let mime = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"]
            .into_iter()
            .find(|mime| mimes.iter().any(|value| value == mime))
            .ok_or(Error::Unsupported)?;
        state.pinned = Some(offer.id());
        let (input, output) = pipe_with(PipeFlags::CLOEXEC).map_err(|_| Error::Io)?;
        rustix::fs::fcntl_setfl(&input, rustix::fs::OFlags::NONBLOCK).map_err(|_| Error::Io)?;
        offer.receive(mime.to_owned(), output.as_fd());
        drop(output);
        let mut bytes = Vec::new();
        while !pump(
            &connection,
            &mut queue,
            &mut state,
            Some(&input),
            &mut bytes,
            deadline,
            &request,
        )? {}
        // EOF alone does not prove the selection stayed current. Complete a
        // protocol barrier after draining queued changes on this connection.
        barrier(&connection, &mut queue, &mut state, deadline, &request)?;
        remaining(deadline, &request)?;
        decode(bytes)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Write;
        fn pipe_bytes(bytes: &[u8]) -> OwnedFd {
            let (input, output) = pipe_with(PipeFlags::CLOEXEC).unwrap();
            rustix::fs::fcntl_setfl(&input, rustix::fs::OFlags::NONBLOCK).unwrap();
            let mut writer = std::fs::File::from(output);
            writer.write_all(bytes).unwrap();
            input
        }
        /// Drain to EOF the way the reader does, polling until the deadline. A process that
        /// another test forks inherits the write end until it execs, so EOF can arrive a
        /// moment after the writer here is dropped.
        fn drain_to_eof(pipe: &OwnedFd, bytes: &mut Vec<u8>) -> Result<bool, Error> {
            let deadline = Instant::now() + TIMEOUT;
            loop {
                if drain_pipe(
                    pipe,
                    bytes,
                    deadline,
                    &ClipboardReadRequest::default(),
                    MAX_BYTES,
                )? {
                    return Ok(true);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        #[test]
        fn exact_clipboard_pipe_preserves_bytes_empty_invalid_and_limit() {
            let expected = "\u{feff}A e\u{301} 👩\u{200d}💻 🇮🇱\r\nB\nC\rD".as_bytes();
            let mut bytes = Vec::new();
            assert!(drain_to_eof(&pipe_bytes(expected), &mut bytes).unwrap());
            assert_eq!(decode(bytes).unwrap().unwrap().as_bytes(), expected);
            let mut bytes = Vec::new();
            assert!(drain_to_eof(&pipe_bytes(b""), &mut bytes).unwrap());
            assert_eq!(decode(bytes).unwrap(), Some(String::new()));
            let mut bytes = Vec::new();
            drain_to_eof(&pipe_bytes(&[0xff]), &mut bytes).unwrap();
            assert_eq!(decode(bytes), Err(Error::InvalidUtf8));
            assert!(matches!(
                drain_pipe(
                    &pipe_bytes(expected),
                    &mut Vec::new(),
                    Instant::now() + TIMEOUT,
                    &ClipboardReadRequest::default(),
                    expected.len() - 1
                ),
                Err(Error::TooLarge)
            ));
        }
        #[test]
        fn exact_clipboard_polls_idle_pipe_until_deadline() {
            let (client, _server) = UnixStream::pair().unwrap();
            let connection = Connection::from_socket(client).unwrap();
            let mut queue = connection.new_event_queue::<State>();
            let mut state = State::default();
            let (input, _held_writer) =
                pipe_with(PipeFlags::NONBLOCK | PipeFlags::CLOEXEC).unwrap();
            let deadline = Instant::now() + Duration::from_millis(30);
            let result = loop {
                match pump(
                    &connection,
                    &mut queue,
                    &mut state,
                    Some(&input),
                    &mut Vec::new(),
                    deadline,
                    &ClipboardReadRequest::default(),
                ) {
                    Ok(false) => continue,
                    other => break other,
                }
            };
            assert_eq!(result, Err(Error::Timeout));
        }
    }
}

/// Resolve the same explicit named endpoint GPUI uses without overriding an
/// inherited fd connection. Its compositor identity cannot be inferred safely.
#[allow(dead_code)] // Main application only; the isolated example binds explicitly.
pub fn existing_compositor() -> Option<std::path::PathBuf> {
    if std::env::var_os("WAYLAND_SOCKET").is_some() {
        return None;
    }
    named_compositor()
}

fn named_compositor() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::FileTypeExt;
        let display = std::path::PathBuf::from(std::env::var_os("WAYLAND_DISPLAY")?);
        let path = if display.is_absolute() {
            display
        } else {
            let runtime = std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
            if !runtime.is_absolute() {
                return None;
            }
            runtime.join(display)
        }
        .canonicalize()
        .ok()?;
        path.metadata()
            .ok()?
            .file_type()
            .is_socket()
            .then_some(path)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// The isolated fixture explicitly binds both clients before GPUI starts.
#[allow(dead_code)]
pub fn bind_compositor() -> Option<std::path::PathBuf> {
    let path = named_compositor()?;
    std::env::remove_var("WAYLAND_SOCKET");
    std::env::set_var("WAYLAND_DISPLAY", &path);
    Some(path)
}
