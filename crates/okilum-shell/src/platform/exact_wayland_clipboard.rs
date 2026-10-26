//! Linux clipboard text for the note editor's paste: the regular selection's
//! exact bytes over the compositor's data-control where it has one, read in
//! the paste command itself (#1602); otherwise GPUI's own clipboard connection.
//! No clipboard ownership changes, core hooks, or external helper executable.
use gpui::{App, Task};
use gpui_component::input::clipboard::{
    ClipboardReadError as Error, ClipboardReadRequest, ExactClipboardProvider,
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(target_os = "linux")]
const MAX_BYTES: usize = 8 * 1024 * 1024;
// Keep the transport independent of the application crate: standalone native
// examples include this file too. The note adapter's PNG test checks this cap.
#[cfg(target_os = "linux")]
const MAX_NOTE_BYTES: usize = 64 * 1024 * 1024;

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

    /// The editor's paste reads here, inside the user's Ctrl+V or Paste
    /// command, and applies the text before the command returns (#1602). The
    /// asynchronous read above landed turns later, and an activation, focus,
    /// selection or layout change in between cancelled it without a word;
    /// with GPUI on X11, on GNOME's Mutter (no wlr data-control) or on an
    /// inherited socket it refused every paste as Unsupported.
    fn read_text_now(&self, cx: &App) -> Option<Result<Option<String>, Error>> {
        let exact = match &self.socket {
            #[cfg(target_os = "linux")]
            Some(socket) => linux::read_in_command(socket, self.seat.clone()),
            _ => Err(Error::Unsupported),
        };
        Some(settle(exact, || platform_text(cx)))
    }
}

/// What the compositor's data-control holds for an in-command read.
#[derive(Debug, PartialEq)]
enum Exact {
    /// The selection's exact text; `None` when there is no selection.
    Text(Option<String>),
    /// Okilum's own copy. Its bytes come from this UI thread, which is busy
    /// with the paste, so a pipe read would wait out the whole deadline.
    Own,
}

/// Exact text when data-control has it. Where that path cannot run (GPUI on
/// X11, GNOME's Mutter, an inherited socket, several seats, no connection) and
/// for Okilum's own copy, the text GPUI's clipboard connection reads: the same
/// bytes on X11; on Wayland GPUI folds CRLF into LF, which the note editor's
/// document newlines (#848) do to pasted text anyway. What the exact read
/// found wrong with the clipboard itself stands.
fn settle(
    exact: Result<Exact, Error>,
    platform: impl FnOnce() -> Result<Option<String>, Error>,
) -> Result<Option<String>, Error> {
    match exact {
        Ok(Exact::Text(text)) => Ok(text),
        Ok(Exact::Own) | Err(Error::Unsupported | Error::AmbiguousSeat | Error::Io) => platform(),
        Err(error) => Err(error),
    }
}

/// Text from GPUI's own clipboard connection, on the calling thread. An image
/// without text is no text (`None`); the editor says so.
fn platform_text(cx: &App) -> Result<Option<String>, Error> {
    Ok(cx.read_from_clipboard().and_then(|item| item.text()))
}

/// A stable native offer, shared by text-only fields and note smart paste.
#[cfg(target_os = "linux")]
pub(super) enum Selection {
    Empty,
    Own,
    Data(Vec<(String, Vec<u8>)>),
}

/// MIME selection used by the real data-control receive path. Reads share
/// one pinned offer/deadline and a total payload budget, never the primary selection.
#[cfg(target_os = "linux")]
pub(super) fn receive_selection(
    mimes: &[String],
    own: Option<&str>,
    note: bool,
    mut receive: impl FnMut(&str, usize) -> Result<Vec<u8>, Error>,
) -> Result<Selection, Error> {
    if own.is_some_and(|own| mimes.iter().any(|mime| mime == own)) {
        return Ok(Selection::Own);
    }
    let text = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"]
        .into_iter()
        .find(|mime| mimes.iter().any(|value| value == mime));
    let mut requested = Vec::new();
    if note {
        if mimes.iter().any(|mime| mime == "text/uri-list") {
            requested.push("text/uri-list");
        }
        if let Some(image) = [
            "image/png",
            "image/jpeg",
            "image/webp",
            "image/gif",
            "image/tiff",
            "image/bmp",
        ]
        .into_iter()
        .find(|mime| mimes.iter().any(|value| value == mime))
        {
            requested.push(image);
        }
    }
    requested.extend(text);
    if requested.is_empty() {
        return Err(Error::Unsupported);
    }
    let budget = if note { MAX_NOTE_BYTES } else { MAX_BYTES };
    let mut used = 0;
    let mut data = Vec::new();
    for mime in requested {
        let limit = (if mime.starts_with("image/") {
            budget
        } else {
            MAX_BYTES
        })
        .min(budget - used);
        let bytes = receive(mime, limit)?;
        if bytes.len() > limit {
            return Err(Error::TooLarge);
        }
        used += bytes.len();
        data.push((mime.to_owned(), bytes));
    }
    Ok(Selection::Data(data))
}

#[cfg(target_os = "linux")]
#[allow(dead_code)] // The standalone text-only examples do not install note paste.
pub(super) fn read_note_in_command(
    socket: Option<&Path>,
    seat: Option<String>,
) -> Result<Selection, Error> {
    linux::read_selection(
        socket.ok_or(Error::Unsupported)?,
        seat,
        &ClipboardReadRequest::default(),
        Instant::now() + TIMEOUT,
        Some(&own_mime()),
        true,
    )
}

/// GPUI offers this type with every selection it sets (its `self_mime`).
#[cfg(target_os = "linux")]
fn own_mime() -> String {
    format!("pid/{}", std::process::id())
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
        byte_limit: usize,
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
        path: &Path,
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
            return drain_pipe(
                pipe.unwrap(),
                bytes,
                deadline,
                request,
                state.byte_limit.max(1),
            );
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
        // Off the UI thread GPUI answers for Okilum's own copy, so no own
        // type is checked here and the pipe read covers it.
        match read_offer(&path, requested_seat, &request, deadline, None)? {
            Exact::Text(text) => Ok(text),
            Exact::Own => Err(Error::Unavailable),
        }
    }

    /// The same read on the UI thread, inside the paste command (#1602).
    /// Nothing can cancel it there; its own deadline still bounds it.
    pub(super) fn read_in_command(
        path: &Path,
        requested_seat: Option<String>,
    ) -> Result<Exact, Error> {
        read_offer(
            path,
            requested_seat,
            &ClipboardReadRequest::default(),
            Instant::now() + TIMEOUT,
            Some(&own_mime()),
        )
    }

    fn read_offer(
        path: &Path,
        requested_seat: Option<String>,
        request: &ClipboardReadRequest,
        deadline: Instant,
        own: Option<&str>,
    ) -> Result<Exact, Error> {
        match read_selection(path, requested_seat, request, deadline, own, false)? {
            Selection::Empty => Ok(Exact::Text(None)),
            Selection::Own => Ok(Exact::Own),
            Selection::Data(mut data) => decode(data.remove(0).1).map(Exact::Text),
        }
    }

    pub(super) fn read_selection(
        path: &Path,
        requested_seat: Option<String>,
        request: &ClipboardReadRequest,
        deadline: Instant,
        own: Option<&str>,
        note: bool,
    ) -> Result<Selection, Error> {
        let connection = connect(path, deadline, request)?;
        let mut queue = connection.new_event_queue::<State>();
        let qh = queue.handle();
        let mut state = State::default();
        let _registry = connection.display().get_registry(&qh, ());
        barrier(&connection, &mut queue, &mut state, deadline, request)?;
        // Seat name events follow bindings requested during the first barrier.
        barrier(&connection, &mut queue, &mut state, deadline, request)?;
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
        barrier(&connection, &mut queue, &mut state, deadline, request)?;
        let Some(offer) = state.selection.clone() else {
            return Ok(Selection::Empty);
        };
        let mimes = state
            .offers
            .get(&offer.id())
            .cloned()
            .ok_or(Error::ChangedOffer)?;
        state.pinned = Some(offer.id());
        receive_selection(&mimes, own, note, |mime, limit| {
            state.byte_limit = limit;
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
                request,
            )? {}
            // Verify the same selection after every MIME reaches EOF.
            barrier(&connection, &mut queue, &mut state, deadline, request)?;
            remaining(deadline, request)?;
            Ok(bytes)
        })
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

#[cfg(test)]
mod paste_tests {
    use super::*;
    use gpui::{
        div, AppContext, ClipboardItem, Context, Entity, Focusable, Image, ImageFormat,
        IntoElement, Keystroke, ParentElement, Render, Styled, TestAppContext, Window,
    };
    use gpui_component::input::{clipboard::ClipboardPasteEvent, Editor, EditorState};
    use std::{cell::RefCell, rc::Rc, sync::Arc};

    /// #1602: where data-control cannot run, and for Okilum's own copy, GPUI's
    /// clipboard answers; what the exact read found wrong stands.
    #[test]
    fn in_command_read_falls_back_only_where_the_exact_path_cannot_run() {
        assert_eq!(
            settle(Ok(Exact::Text(Some("a\r\nb".into()))), || panic!("exact")),
            Ok(Some("a\r\nb".into()))
        );
        assert_eq!(settle(Ok(Exact::Text(None)), || panic!("none")), Ok(None));
        for exact in [
            Ok(Exact::Own),
            Err(Error::Unsupported),
            Err(Error::AmbiguousSeat),
            Err(Error::Io),
        ] {
            let text = settle(exact, || Ok(Some("platform".to_owned())));
            assert_eq!(text, Ok(Some("platform".into())));
        }
        for error in [
            Error::Unavailable,
            Error::Busy,
            Error::Cancelled,
            Error::Timeout,
            Error::TooLarge,
            Error::InvalidUtf8,
            Error::ChangedOffer,
        ] {
            let text = settle(Err(error.clone()), || panic!("{error:?} stands"));
            assert_eq!(text, Err(error));
        }
    }

    struct Fixture(Entity<EditorState>);
    impl Render for Fixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(Editor::new(&self.0))
        }
    }

    /// #1602: with GPUI on X11 there is no compositor socket, and every Ctrl+V
    /// into a note was refused as Unsupported, silently. The text now lands
    /// inside the key command itself, before GPUI has recorded the window's
    /// activation, as one Undo step, and Shift+Insert pastes too. An empty
    /// clipboard or an image inserts nothing and reports Unavailable.
    #[gpui::test]
    fn x11_ctrl_v_lands_inside_the_command(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_component::init(cx);
        });
        let image = Image::from_bytes(ImageFormat::Png, b"\x89PNG\r\n\x1a\n".to_vec());
        // The test platform starts with an empty clipboard; nothing clears it.
        for (clipboard, expected) in [
            (None, "ac\n"),
            (Some(ClipboardItem::new_image(&image)), "ac\n"),
            (
                Some(ClipboardItem::new_string("first\r\n🧠 b\u{301}".into())),
                "afirst\n🧠 b\u{301}c\n",
            ),
        ] {
            let mut editor = None;
            let (_, visual) = cx.add_window_view(|window, cx| {
                let state = cx.new(|cx| {
                    let mut state = EditorState::new(window, cx).document_newlines(true);
                    let provider = WaylandClipboard::new(None, None);
                    state.set_exact_clipboard_provider(Some(Arc::new(provider)), cx);
                    state.set_value("ac\n", window, cx);
                    state.set_selected_range(1..1, cx);
                    state.focus_handle(cx).focus(window, cx);
                    window.activate_window();
                    state
                });
                editor = Some(state.clone());
                let fixture = cx.new(|_| Fixture(state));
                gpui_component::Root::new(fixture, window, cx)
            });
            let editor = editor.unwrap();
            let outcomes = Rc::new(RefCell::new(Vec::new()));
            let _subscription = visual.update(|_, cx| {
                let outcomes = outcomes.clone();
                cx.subscribe(&editor, move |_, event: &ClipboardPasteEvent, _| {
                    outcomes.borrow_mut().push(event.clone())
                })
            });
            visual.run_until_parked();
            if let Some(item) = clipboard.clone() {
                visual.update(|_, cx| cx.write_to_clipboard(item));
            }
            visual.deactivate_window();
            visual.update(|window, cx| {
                assert!(
                    !window.is_window_active(),
                    "positive control: GPUI flag is off"
                );
                window.dispatch_keystroke(Keystroke::parse("ctrl-v").unwrap(), cx);
                // Applied before the command returns, not on a later turn.
                assert_eq!(editor.read(cx).value().as_ref(), expected, "{clipboard:?}");
            });
            visual.run_until_parked();
            let outcome = outcomes.borrow().last().cloned();
            if expected == "ac\n" {
                assert_eq!(
                    outcome,
                    Some(ClipboardPasteEvent::Failed(Error::Unavailable)),
                    "{clipboard:?}"
                );
                continue;
            }
            assert_eq!(
                outcome,
                Some(ClipboardPasteEvent::Applied {
                    request: 1,
                    bytes: "first\n🧠 b\u{301}".len(),
                })
            );
            visual.simulate_keystrokes("ctrl-z");
            visual.update(|window, cx| {
                assert_eq!(editor.read(cx).value().as_ref(), "ac\n");
                editor.update(cx, |state, cx| state.set_selected_range(1..1, cx));
                // Clipboard managers and dictation (Omarchy, voxtype) paste
                // with Shift+Insert, which pasted nothing before.
                window.dispatch_keystroke(Keystroke::parse("shift-insert").unwrap(), cx);
                assert_eq!(editor.read(cx).value().as_ref(), expected);
            });
        }
    }
}
