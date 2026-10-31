//! X11 CLIPBOARD for note smart paste (#2006). GPUI 0.3.3's X11 clipboard
//! reads only `image/*` and plain text (its `text/html` atom is commented out
//! upstream), so a page copied from a browser arrived as plain text. This
//! reads the regular selection's targets over Okilum's own short-lived X11
//! connection, inside the user's paste command, and hands them to the same
//! MIME negotiation the Wayland data-control reader uses. GPUI core is not
//! patched and clipboard ownership is never taken.
//!
//! Okilum's own copy is left to GPUI (its cached item keeps images and
//! metadata). The owner's process comes from the X-Resource extension; where
//! that cannot name it, the read is `Unsupported` and GPUI answers as before.
use super::exact_wayland_clipboard::{receive_selection, Selection};
use gpui_component::input::clipboard::ClipboardReadError as Error;
use rustix::event::{poll, PollFd, PollFlags, Timespec};
use std::time::{Duration, Instant};
use x11rb::{
    connection::{Connection as _, RequestConnection as _},
    errors::{ConnectionError, ReplyError},
    protocol::{
        res::{self, ClientIdMask, ClientIdSpec, ConnectionExt as _},
        xproto::{
            Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, Property, Window,
            WindowClass,
        },
        Event,
    },
    rust_connection::RustConnection,
    COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT, CURRENT_TIME, NONE,
};

/// Same budget as the Wayland note read: one deadline for the whole paste.
const TIMEOUT: Duration = Duration::from_secs(2);
/// Owners list a handful of targets; a longer list is not a clipboard.
const MAX_TARGETS: usize = 512;

/// Read the CLIPBOARD selection for a note paste on `display`. `own_pid` is
/// this process: its own copy is reported as [`Selection::Own`].
pub(super) fn read_note_in_command(display: &str, own_pid: u32) -> Result<Selection, Error> {
    read_note(display, own_pid, Instant::now() + TIMEOUT)
}

fn read_note(
    display: &str,
    own_pid: u32,
    deadline: Instant,
) -> Result<Selection, Error> {
    let (conn, screen) = RustConnection::connect(Some(display)).map_err(|_| Error::Unsupported)?;
    let reader = Reader::new(conn, screen, deadline)?;
    let result = reader.read(own_pid);
    // The connection closes on drop; destroying first keeps a reused display tidy.
    let _ = reader.conn.destroy_window(reader.window);
    let _ = reader.conn.flush();
    result
}

struct Reader {
    conn: RustConnection,
    window: Window,
    clipboard: Atom,
    property: Atom,
    incr: Atom,
    deadline: Instant,
}

fn io(_: impl Into<ReplyError>) -> Error {
    Error::Io
}

impl Reader {
    fn new(conn: RustConnection, screen: usize, deadline: Instant) -> Result<Self, Error> {
        let root = conn
            .setup()
            .roots
            .get(screen)
            .ok_or(Error::Unsupported)?
            .root;
        let window = conn.generate_id().map_err(|_| Error::Io)?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            COPY_FROM_PARENT,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .map_err(|_| Error::Io)?;
        let intern = |name: &[u8]| -> Result<Atom, Error> {
            Ok(conn
                .intern_atom(false, name)
                .map_err(io)?
                .reply()
                .map_err(io)?
                .atom)
        };
        let (clipboard, property, incr) = (
            intern(b"CLIPBOARD")?,
            intern(b"OKILUM_PASTE")?,
            intern(b"INCR")?,
        );
        Ok(Self {
            conn,
            window,
            clipboard,
            property,
            incr,
            deadline,
        })
    }

    fn read(&self, own_pid: u32) -> Result<Selection, Error> {
        let owner = self.owner()?;
        if owner == NONE {
            return Ok(Selection::Empty);
        }
        if self.owner_pid(owner)? == own_pid {
            return Ok(Selection::Own);
        }
        let targets = self.targets()?;
        self.same_owner(owner)?;
        receive_selection(&targets, None, true, |mime, limit| {
            let bytes = self.convert(mime, limit)?;
            self.same_owner(owner)?;
            Ok(if mime == "text/html" {
                html_utf8(bytes)
            } else {
                bytes
            })
        })
    }

    fn owner(&self) -> Result<Window, Error> {
        Ok(self
            .conn
            .get_selection_owner(self.clipboard)
            .map_err(io)?
            .reply()
            .map_err(io)?
            .owner)
    }

    /// Each conversion is answered by whoever owns the selection now; a
    /// different owner midway would mix two copies into one paste.
    fn same_owner(&self, owner: Window) -> Result<(), Error> {
        if self.owner()? == owner {
            Ok(())
        } else {
            Err(Error::ChangedOffer)
        }
    }

    /// The owning client's process id, or `Unsupported` when the server
    /// cannot name it (no X-Resource 1.2, a remote client).
    fn owner_pid(&self, owner: Window) -> Result<u32, Error> {
        if self
            .conn
            .extension_information(res::X11_EXTENSION_NAME)
            .map_err(|_| Error::Io)?
            .is_none()
        {
            return Err(Error::Unsupported);
        }
        let version = self
            .conn
            .res_query_version(1, 2)
            .map_err(io)?
            .reply()
            .map_err(io)?;
        if (version.server_major, version.server_minor) < (1, 2) {
            return Err(Error::Unsupported);
        }
        let reply = self
            .conn
            .res_query_client_ids(&[ClientIdSpec {
                client: owner,
                mask: ClientIdMask::LOCAL_CLIENT_PID,
            }])
            .map_err(io)?
            .reply()
            .map_err(io)?;
        reply
            .ids
            .iter()
            .find(|id| id.spec.mask == ClientIdMask::LOCAL_CLIENT_PID)
            .and_then(|id| id.value.first().copied())
            .ok_or(Error::Unsupported)
    }

    /// The owner's `TARGETS`, as MIME names.
    fn targets(&self) -> Result<Vec<String>, Error> {
        let targets = self.intern("TARGETS")?;
        let bytes = self.convert_atom(targets, MAX_TARGETS * 4)?;
        let atoms = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| u32::from_ne_bytes(*chunk))
            .filter(|atom| *atom != NONE)
            .collect::<Vec<_>>();
        let cookies = atoms
            .iter()
            .map(|atom| self.conn.get_atom_name(*atom))
            .collect::<Vec<_>>();
        let mut names = Vec::with_capacity(cookies.len());
        for cookie in cookies {
            // An atom the owner listed but the server forgot is not readable.
            if let Ok(reply) = cookie.map_err(io)?.reply() {
                if let Ok(name) = String::from_utf8(reply.name) {
                    names.push(name);
                }
            }
        }
        Ok(names)
    }

    fn intern(&self, name: &str) -> Result<Atom, Error> {
        Ok(self
            .conn
            .intern_atom(false, name.as_bytes())
            .map_err(io)?
            .reply()
            .map_err(io)?
            .atom)
    }

    fn convert(&self, mime: &str, limit: usize) -> Result<Vec<u8>, Error> {
        let target = self.intern(mime)?;
        self.convert_atom(target, limit)
    }

    /// ICCCM 2.4 conversion into our window's property, including the `INCR`
    /// transfer large owners use. Every wait shares the paste's deadline.
    fn convert_atom(&self, target: Atom, limit: usize) -> Result<Vec<u8>, Error> {
        self.conn
            .delete_property(self.window, self.property)
            .map_err(|_| Error::Io)?;
        self.conn
            .convert_selection(
                self.window,
                self.clipboard,
                target,
                self.property,
                CURRENT_TIME,
            )
            .map_err(|_| Error::Io)?;
        self.conn.flush().map_err(|_| Error::Io)?;
        let notified = self.wait(|event| match event {
            Event::SelectionNotify(event)
                if event.requestor == self.window
                    && event.selection == self.clipboard
                    && event.target == target =>
            {
                Some(event.property)
            }
            _ => None,
        })?;
        if notified == NONE {
            // The owner refused a target it listed: GPUI's read answers instead.
            return Err(Error::Io);
        }
        let (kind, first) = self.take_property(limit)?;
        if kind != self.incr {
            return Ok(first);
        }
        // Deleting the INCR property (done by the read above) starts the transfer.
        let mut data = Vec::new();
        loop {
            self.wait(|event| match event {
                Event::PropertyNotify(event)
                    if event.window == self.window
                        && event.atom == self.property
                        && event.state == Property::NEW_VALUE =>
                {
                    Some(())
                }
                _ => None,
            })?;
            let (_, chunk) = self.take_property(limit - data.len())?;
            if chunk.is_empty() {
                return Ok(data);
            }
            data.extend_from_slice(&chunk);
        }
    }

    /// Read and delete our property; more than `limit` bytes is `TooLarge`.
    fn take_property(&self, limit: usize) -> Result<(Atom, Vec<u8>), Error> {
        let units = u32::try_from(limit / 4 + 1).unwrap_or(u32::MAX);
        let reply = self
            .conn
            .get_property(true, self.window, self.property, AtomEnum::ANY, 0, units)
            .map_err(io)?
            .reply()
            .map_err(io)?;
        if reply.bytes_after > 0 || reply.value.len() > limit {
            let _ = self.conn.delete_property(self.window, self.property);
            return Err(Error::TooLarge);
        }
        self.conn.flush().map_err(|_| Error::Io)?;
        Ok((reply.type_, reply.value))
    }

    /// The first event `pick` accepts, before the deadline.
    fn wait<T>(&self, mut pick: impl FnMut(&Event) -> Option<T>) -> Result<T, Error> {
        loop {
            while let Some(event) = self.conn.poll_for_event().map_err(connection)? {
                if let Some(value) = pick(&event) {
                    return Ok(value);
                }
            }
            let left = self
                .deadline
                .checked_duration_since(Instant::now())
                .ok_or(Error::Timeout)?;
            let timeout = Timespec {
                tv_sec: left.as_secs() as _,
                tv_nsec: left.subsec_nanos() as _,
            };
            let stream = self.conn.stream();
            let mut fds = [PollFd::new(stream, PollFlags::IN)];
            match poll(&mut fds, Some(&timeout)) {
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(_) => return Err(Error::Io),
            }
        }
    }
}

fn connection(_: ConnectionError) -> Error {
    Error::Io
}

/// Browsers on X11 have offered `text/html` as UTF-16 with a byte order mark
/// (Firefox did for years); the converters take UTF-8.
fn html_utf8(bytes: Vec<u8>) -> Vec<u8> {
    let decode = |little: bool| {
        let units = bytes[2..].as_chunks::<2>().0.iter().map(|pair| {
            if little {
                u16::from_le_bytes(*pair)
            } else {
                u16::from_be_bytes(*pair)
            }
        });
        char::decode_utf16(units)
            .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect::<String>()
            .into_bytes()
    };
    match bytes.get(..2) {
        Some([0xFF, 0xFE]) => decode(true),
        Some([0xFE, 0xFF]) => decode(false),
        _ => bytes,
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::{
        process::{Child, Command, Stdio},
        sync::mpsc,
        thread,
    };
    use x11rb::wrapper::ConnectionExt as _;

    /// A private Xvfb for one test, or `None` (with a note) where none is installed.
    pub(in crate::platform) struct Xvfb {
        child: Child,
        pub display: String,
    }
    impl Drop for Xvfb {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
    impl Xvfb {
        pub fn start() -> Option<Self> {
            use std::io::BufRead;
            // Xvfb picks a free display itself and names it on stdout once it
            // accepts clients, so parallel tests never share a server.
            let mut child = match Command::new("Xvfb")
                .args(["-displayfd", "1", "-nolisten", "tcp", "-noreset"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(child) => child,
                Err(_) => {
                    eprintln!("#2006: Xvfb is not installed; X11 clipboard test skipped");
                    return None;
                }
            };
            let mut line = String::new();
            let _ = std::io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut line);
            let number = line.trim();
            let server = Self {
                child,
                display: format!(":{number}"),
            };
            assert!(
                !number.is_empty() && RustConnection::connect(Some(&server.display)).is_ok(),
                "#2006: Xvfb is installed but did not start a display"
            );
            Some(server)
        }
    }

    /// An X11 selection owner on its own connection and thread, serving
    /// `targets` (name, bytes) the ICCCM way, with `INCR` above `chunk` bytes.
    pub(in crate::platform) struct Owner {
        stop: mpsc::Sender<()>,
        thread: Option<thread::JoinHandle<()>>,
    }
    impl Drop for Owner {
        fn drop(&mut self) {
            let _ = self.stop.send(());
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
        }
    }
    impl Owner {
        pub fn serve(display: &str, targets: Vec<(&'static str, Vec<u8>)>, chunk: usize) -> Self {
            let (conn, screen) = RustConnection::connect(Some(display)).unwrap();
            let root = conn.setup().roots[screen].root;
            let window = conn.generate_id().unwrap();
            conn.create_window(
                COPY_DEPTH_FROM_PARENT,
                window,
                root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                COPY_FROM_PARENT,
                &CreateWindowAux::new(),
            )
            .unwrap();
            let intern = |name: &str| {
                conn.intern_atom(false, name.as_bytes())
                    .unwrap()
                    .reply()
                    .unwrap()
                    .atom
            };
            let clipboard = intern("CLIPBOARD");
            let targets_atom = intern("TARGETS");
            let incr = intern("INCR");
            let served = targets
                .into_iter()
                .map(|(name, bytes)| (intern(name), bytes))
                .collect::<Vec<_>>();
            conn.set_selection_owner(window, clipboard, CURRENT_TIME)
                .unwrap();
            conn.flush().unwrap();
            assert_eq!(
                conn.get_selection_owner(clipboard)
                    .unwrap()
                    .reply()
                    .unwrap()
                    .owner,
                window
            );
            let (stop, stopped) = mpsc::channel();
            let thread = thread::spawn(move || {
                use x11rb::protocol::xproto::{
                    PropMode, SelectionNotifyEvent, SELECTION_NOTIFY_EVENT,
                };
                // Pending INCR transfers: (requestor, property, rest).
                let mut pending: Vec<(Window, Atom, Vec<u8>)> = Vec::new();
                while stopped.try_recv().is_err() {
                    let Some(event) = conn.poll_for_event().unwrap() else {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    };
                    match event {
                        Event::SelectionRequest(request) => {
                            let mut property = request.property;
                            if request.target == targets_atom {
                                let mut atoms = vec![targets_atom];
                                atoms.extend(served.iter().map(|(atom, _)| *atom));
                                conn.change_property32(
                                    PropMode::REPLACE,
                                    request.requestor,
                                    property,
                                    AtomEnum::ATOM,
                                    &atoms,
                                )
                                .unwrap();
                            } else if let Some((_, bytes)) =
                                served.iter().find(|(atom, _)| *atom == request.target)
                            {
                                if bytes.len() > chunk {
                                    conn.change_window_attributes(
                                        request.requestor,
                                        &x11rb::protocol::xproto::ChangeWindowAttributesAux::new()
                                            .event_mask(EventMask::PROPERTY_CHANGE),
                                    )
                                    .unwrap();
                                    conn.change_property32(
                                        PropMode::REPLACE,
                                        request.requestor,
                                        property,
                                        incr,
                                        &[bytes.len() as u32],
                                    )
                                    .unwrap();
                                    pending.push((request.requestor, property, bytes.clone()));
                                } else {
                                    conn.change_property8(
                                        PropMode::REPLACE,
                                        request.requestor,
                                        property,
                                        request.target,
                                        bytes,
                                    )
                                    .unwrap();
                                }
                            } else {
                                property = NONE;
                            }
                            conn.send_event(
                                false,
                                request.requestor,
                                EventMask::NO_EVENT,
                                SelectionNotifyEvent {
                                    response_type: SELECTION_NOTIFY_EVENT,
                                    sequence: 0,
                                    time: request.time,
                                    requestor: request.requestor,
                                    selection: request.selection,
                                    target: request.target,
                                    property,
                                },
                            )
                            .unwrap();
                            conn.flush().unwrap();
                        }
                        Event::PropertyNotify(event) if event.state == Property::DELETE => {
                            if let Some(index) = pending
                                .iter()
                                .position(|(w, p, _)| *w == event.window && *p == event.atom)
                            {
                                let (window, property, rest) = &mut pending[index];
                                let take = rest.len().min(chunk);
                                let piece = rest.drain(..take).collect::<Vec<_>>();
                                let _ = conn.change_property8(
                                    PropMode::REPLACE,
                                    *window,
                                    *property,
                                    AtomEnum::STRING,
                                    &piece,
                                );
                                if piece.is_empty() {
                                    pending.remove(index);
                                }
                                conn.flush().unwrap();
                            }
                        }
                        _ => {}
                    }
                }
            });
            Self {
                stop,
                thread: Some(thread),
            }
        }
    }

    fn data(selection: Selection) -> Vec<(String, Vec<u8>)> {
        match selection {
            Selection::Data(data) => data,
            Selection::Empty => panic!("#2006: X11 selection read as empty"),
            Selection::Own => panic!("#2006: foreign X11 owner read as own"),
        }
    }
    const FOREIGN: u32 = u32::MAX;

    #[test]
    fn html_and_plain_text_come_from_a_foreign_x11_owner() {
        let Some(server) = Xvfb::start() else { return };
        // Positive control first: an empty CLIPBOARD reads as empty, not an error.
        assert!(matches!(
            read_note_in_command(&server.display, FOREIGN),
            Ok(Selection::Empty)
        ));
        let html = b"<p>Hello <strong>world</strong></p>".to_vec();
        let _owner = Owner::serve(
            &server.display,
            vec![
                ("UTF8_STRING", b"Hello world".to_vec()),
                ("text/html", html.clone()),
                ("text/plain", b"wrong preference".to_vec()),
            ],
            usize::MAX,
        );
        let data = data(read_note_in_command(&server.display, FOREIGN).unwrap());
        assert_eq!(
            data,
            vec![
                ("text/html".to_owned(), html),
                ("UTF8_STRING".to_owned(), b"Hello world".to_vec()),
            ],
            "#2006: X11 smart paste must receive text/html next to the plain text"
        );
    }

    #[test]
    fn incr_transfer_images_and_own_copy() {
        let Some(server) = Xvfb::start() else { return };
        let big = format!("<p>{}</p>", "x".repeat(300_000)).into_bytes();
        let png = vec![0x89, b'P', b'N', b'G', 1, 2, 3];
        let _owner = Owner::serve(
            &server.display,
            vec![
                ("text/html", big.clone()),
                ("image/png", png.clone()),
                ("UTF8_STRING", b"x".to_vec()),
            ],
            64 * 1024,
        );
        let data = data(read_note_in_command(&server.display, FOREIGN).unwrap());
        assert_eq!(data[0], ("text/html".to_owned(), big));
        assert_eq!(data[1], ("image/png".to_owned(), png));
        // The owner above is this process: its copy is GPUI's to answer.
        assert!(matches!(
            read_note_in_command(&server.display, std::process::id()),
            Ok(Selection::Own)
        ));
        // A payload above the budget is refused, not truncated.
        let reader = {
            let (conn, screen) = RustConnection::connect(Some(&server.display)).unwrap();
            Reader::new(conn, screen, Instant::now() + TIMEOUT).unwrap()
        };
        let html = reader.intern("text/html").unwrap();
        assert_eq!(reader.convert_atom(html, 1024), Err(Error::TooLarge));
    }

    /// The owner check names processes, not connections: another program's
    /// copy is read even with this process's real id (xclip, where installed).
    #[test]
    fn another_process_copy_is_read_with_the_real_own_pid() {
        use std::io::Write;
        let Some(server) = Xvfb::start() else { return };
        let html = "<p>From <em>xclip</em></p>";
        let Ok(mut xclip) = Command::new("xclip")
            .args(["-selection", "clipboard", "-t", "text/html", "-i"])
            .env("DISPLAY", &server.display)
            .stdin(Stdio::piped())
            .spawn()
        else {
            eprintln!("#2006: xclip is not installed; foreign-process check skipped");
            return;
        };
        xclip
            .stdin
            .take()
            .unwrap()
            .write_all(html.as_bytes())
            .unwrap();
        let start = Instant::now();
        let selection = loop {
            match read_note_in_command(&server.display, std::process::id()) {
                Ok(Selection::Empty) if start.elapsed() < Duration::from_secs(5) => {
                    thread::sleep(Duration::from_millis(20))
                }
                other => break other,
            }
        };
        let _ = xclip.kill();
        let _ = xclip.wait();
        assert_eq!(
            data(selection.unwrap()),
            vec![("text/html".to_owned(), html.into())]
        );
    }

    #[test]
    fn unreachable_display_is_unsupported_so_gpui_answers() {
        assert_eq!(
            read_note_in_command(":59000", FOREIGN).err(),
            Some(Error::Unsupported)
        );
    }

    #[test]
    fn utf16_html_is_decoded_and_utf8_html_is_untouched() {
        let mut le = vec![0xFF, 0xFE];
        le.extend("<b>Привет</b>".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(html_utf8(le), "<b>Привет</b>".as_bytes());
        let mut be = vec![0xFE, 0xFF];
        be.extend("<i>x</i>".encode_utf16().flat_map(u16::to_be_bytes));
        assert_eq!(html_utf8(be), b"<i>x</i>");
        assert_eq!(html_utf8(b"<p>a</p>".to_vec()), b"<p>a</p>");
    }
}
