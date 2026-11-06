//! Local control channel for `okilum sync` (#2589). The process that owns this
//! computer's Sync state, `okilum sync serve` or the running app, runs the
//! commands other processes of the same user send it, so pairing another
//! computer does not stop the hub. A Unix-domain socket in the private Sync
//! journal folder (0700): the socket is 0600 and each end checks that the other
//! runs as this user. Local only, never TCP. Sync, and with it this channel,
//! exists only on Linux and macOS.
use super::*;
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::{
    io::{Read as _, Write as _},
    os::unix::{
        fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _},
        net::{UnixListener, UnixStream},
    },
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, PoisonError,
    },
};

/// Changes whenever a call or answer changes shape; an updated command asks a
/// process started from the previous build to restart.
const PROTOCOL: u32 = 4;
const SOCKET: &str = "control.sock";
const MAX_MESSAGE: u64 = 16 * 1024 * 1024;
const MAX_CALLS: usize = 8;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Call {
    /// The Settings/CLI action path, on the owner's controller.
    Perform(Box<Operation>),
    RefreshPairing,
    Approve(uuid::Uuid),
    Reject(uuid::Uuid),
    ApprovalDelivered,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Answer {
    Step(Box<cli::Step>),
    Snapshot(Box<Snapshot>),
    Delivered(bool),
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    protocol: u32,
    body: T,
}

fn socket_path(state: &Path) -> PathBuf {
    state.join(SOCKET)
}

/// Runs one forwarded call on the controller this process owns.
pub(super) fn answer(
    controller: &Controller,
    call: Call,
    perform: impl FnOnce(Controller, Operation) -> anyhow::Result<Output>,
) -> anyhow::Result<Answer> {
    let call = match call {
        Call::Perform(operation) => {
            let output = perform(controller.clone(), *operation)?;
            return Ok(Answer::Step(Box::new(cli::Step::from(output))));
        }
        call => call,
    };
    // Pairing polls also wait for the owner's own refresh: two concurrent
    // redeem polls would race to record the same approval.
    let _serial = serial_actions();
    Ok(match call {
        Call::RefreshPairing => Answer::Snapshot(Box::new(controller.refresh_pairing()?)),
        Call::Approve(id) => Answer::Snapshot(Box::new(controller.approve(id)?)),
        Call::Reject(id) => Answer::Snapshot(Box::new(controller.reject(id)?)),
        Call::ApprovalDelivered => Answer::Delivered(controller.approval_delivered()?),
        Call::Perform(_) => unreachable!("performed above"),
    })
}
/// The owner's ordinary action path, as Settings and `okilum sync` run it.
pub(super) fn perform_owned(
    controller: Controller,
    operation: Operation,
) -> anyhow::Result<Output> {
    perform_scoped(Some(controller), operation, &Epoch::default().ticket())
}

/// The running app's channel and what it answers with: the app's controller
/// and its revocable Sync permission. The channel lives while the app owns
/// Sync; disabling Sync closes it.
type Owned = Arc<Mutex<(Controller, okilum_t3::NetworkAccess)>>;
static APP: Mutex<Option<(Owned, Option<Host>)>> = Mutex::new(None);

/// Called off the UI thread, in the action's Sync permission scope, with each
/// controller an app action returns.
#[cfg_attr(test, allow(dead_code))]
pub(super) fn app_owns(controller: &Controller) {
    let owned = (controller.clone(), okilum_t3::NetworkAccess::current());
    let mut app = APP.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((current, _)) = app.as_ref() {
        *current.lock().unwrap_or_else(PoisonError::into_inner) = owned;
        return;
    }
    let current = Arc::new(Mutex::new(owned));
    let answering = current.clone();
    let host = state_location()
        .and_then(|state| {
            Host::start(&state, move |call| {
                let (controller, access) = answering
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                let _scope = access.enter();
                answer(&controller, call, perform_owned)
            })
        })
        // One attempt until Sync is turned off; commands then refuse as before.
        .map_err(|error| eprintln!("okilum: `okilum sync` cannot reach this app: {error:#}"))
        .ok();
    *app = Some((current, host));
}
pub(super) fn app_released() {
    let released = APP.lock().unwrap_or_else(PoisonError::into_inner).take();
    drop(released);
}

/// Listens on a state's control socket until dropped.
pub(super) struct Host {
    path: PathBuf,
    identity: (u64, u64),
    stop: Arc<AtomicBool>,
}
impl Host {
    /// Only the holder of `state`'s journal lock calls this, so a socket that
    /// is already there belongs to an owner that has exited.
    pub(super) fn start(
        state: &Path,
        handler: impl Fn(Call) -> anyhow::Result<Answer> + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let path = socket_path(state);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(&path)?,
            Ok(_) => anyhow::bail!("{} is not a Sync control socket", path.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let listener =
            UnixListener::bind(&path).with_context(|| format!("listen on {}", path.display()))?;
        // The journal folder is already private; the socket is too.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let metadata = std::fs::symlink_metadata(&path)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let handler = Arc::new(handler);
        let active = Arc::new(AtomicUsize::new(0));
        std::thread::Builder::new()
            .name("okilum-sync-control".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if stopping.load(Ordering::Acquire) {
                        break;
                    }
                    let Ok(stream) = stream else {
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    };
                    if active
                        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                            (n < MAX_CALLS).then_some(n + 1)
                        })
                        .is_err()
                    {
                        continue;
                    }
                    struct Slot(Arc<AtomicUsize>);
                    impl Drop for Slot {
                        fn drop(&mut self) {
                            self.0.fetch_sub(1, Ordering::AcqRel);
                        }
                    }
                    let slot = Slot(active.clone());
                    let handler = handler.clone();
                    let _ = std::thread::Builder::new()
                        .name("okilum-sync-call".into())
                        .spawn(move || {
                            let _slot = slot;
                            let _ = respond(stream, &*handler);
                        });
                }
            })?;
        Ok(Self {
            path,
            identity: (metadata.dev(), metadata.ino()),
            stop,
        })
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake the blocking accept, so its thread ends and closes the socket.
        drop(UnixStream::connect(&self.path));
        if std::fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| (metadata.dev(), metadata.ino()) == self.identity)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
fn respond(
    mut stream: UnixStream,
    handler: &dyn Fn(Call) -> anyhow::Result<Answer>,
) -> anyhow::Result<()> {
    check_peer(&stream)?;
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    let request: serde_json::Value = serde_json::from_slice(&read_message(&stream)?)?;
    let protocol = request["protocol"].as_u64();
    let body = if protocol != Some(u64::from(PROTOCOL)) {
        Err(format!(
            "this computer's Sync is run by another Okilum build (control protocol {PROTOCOL}, \
             command {}); restart `okilum sync serve` or Okilum after updating",
            protocol.map_or_else(|| "unknown".into(), |p| p.to_string())
        ))
    } else {
        serde_json::from_value::<Envelope<Call>>(request)
            .map_err(anyhow::Error::from)
            .and_then(|request| handler(request.body))
            .map_err(|error| format!("{error:#}"))
    };
    stream.write_all(&serde_json::to_vec(&Envelope {
        protocol: PROTOCOL,
        body,
    })?)?;
    Ok(())
}
fn read_message(stream: &UnixStream) -> anyhow::Result<Vec<u8>> {
    let mut message = Vec::new();
    stream.take(MAX_MESSAGE + 1).read_to_end(&mut message)?;
    anyhow::ensure!(
        message.len() as u64 <= MAX_MESSAGE,
        "Sync control message too large"
    );
    Ok(message)
}
/// Both ends: the other process runs as this user.
fn check_peer(stream: &UnixStream) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    let uid = rustix::net::sockopt::socket_peercred(stream)?.uid.as_raw();
    #[cfg(target_os = "macos")]
    let uid = {
        use std::os::fd::AsRawFd as _;
        let (mut uid, mut gid) = (0, 0);
        // SAFETY: getpeereid writes two integers owned by this frame.
        let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
        anyhow::ensure!(
            result == 0,
            "Sync control peer: {}",
            std::io::Error::last_os_error()
        );
        uid
    };
    anyhow::ensure!(
        uid == rustix::process::geteuid().as_raw(),
        "the Sync control socket's other end runs as another user"
    );
    Ok(())
}

/// A command's way to the process that owns this computer's Sync state.
pub(super) struct Client {
    path: PathBuf,
}
impl Client {
    /// The owner, when one answers on `state`'s control socket.
    pub(super) fn find(state: &Path) -> anyhow::Result<Option<Self>> {
        let path = socket_path(state);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        anyhow::ensure!(
            metadata.file_type().is_socket()
                && metadata.uid() == rustix::process::geteuid().as_raw(),
            "{} is not this user's Sync control socket",
            path.display()
        );
        match UnixStream::connect(&path) {
            Ok(stream) => check_peer(&stream)?,
            // An owner that exited leaves its socket behind.
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("reach the process that owns Sync"),
        }
        Ok(Some(Self { path }))
    }
    pub(super) fn call(&self, call: Call) -> anyhow::Result<Answer> {
        let mut stream = UnixStream::connect(&self.path)
            .context("reach the process that owns this computer's Sync")?;
        check_peer(&stream)?;
        stream.write_all(&serde_json::to_vec(&Envelope {
            protocol: PROTOCOL,
            body: call,
        })?)?;
        stream.shutdown(std::net::Shutdown::Write)?;
        let reply = read_message(&stream)?;
        anyhow::ensure!(
            !reply.is_empty(),
            "the process that owns this computer's Sync closed the request"
        );
        let reply: Envelope<Result<Answer, String>> = serde_json::from_slice(&reply)?;
        reply.body.map_err(anyhow::Error::msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn state(root: &Path) -> anyhow::Result<PathBuf> {
        use std::os::unix::fs::DirBuilderExt as _;
        let state = root.join("direct");
        std::fs::DirBuilder::new().mode(0o700).create(&state)?;
        Ok(state)
    }

    #[test]
    fn issue2589_control_socket_is_private_and_leaves_no_owner_behind() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let state = state(root.path())?;
        assert!(Client::find(&state)?.is_none(), "nobody owns Sync yet");
        // A crashed owner's socket: present, but nobody answers.
        drop(UnixListener::bind(socket_path(&state))?);
        assert!(Client::find(&state)?.is_none(), "stale socket");
        let host = Host::start(&state, |call| match call {
            Call::ApprovalDelivered => Ok(Answer::Delivered(true)),
            _ => anyhow::bail!("no invitation"),
        })?;
        let metadata = std::fs::symlink_metadata(socket_path(&state))?;
        assert!(metadata.file_type().is_socket());
        assert_eq!(metadata.mode() & 0o777, 0o600);
        let owner = Client::find(&state)?.context("the owner answers")?;
        assert!(matches!(
            owner.call(Call::ApprovalDelivered)?,
            Answer::Delivered(true)
        ));
        let Err(error) = owner.call(Call::RefreshPairing) else {
            panic!("the owner's error reaches the command");
        };
        assert_eq!(format!("{error:#}"), "no invitation");
        drop(host);
        assert!(!socket_path(&state).exists(), "closed with its owner");
        assert!(Client::find(&state)?.is_none());
        Ok(())
    }

    #[test]
    fn issue2589_control_refuses_foreign_files_and_other_builds() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let state = state(root.path())?;
        std::fs::write(socket_path(&state), b"not a socket")?;
        assert!(Host::start(&state, |_| anyhow::bail!("unused")).is_err());
        assert!(Client::find(&state).is_err());
        std::fs::remove_file(socket_path(&state))?;
        let _host = Host::start(&state, |_| Ok(Answer::Delivered(false)))?;
        let mut stream = UnixStream::connect(socket_path(&state))?;
        stream.write_all(br#"{"protocol":0,"body":"something_newer"}"#)?;
        stream.shutdown(std::net::Shutdown::Write)?;
        let reply: serde_json::Value = serde_json::from_slice(&read_message(&stream)?)?;
        let message = reply["body"]["Err"].as_str().unwrap_or_default();
        assert!(message.contains("restart `okilum sync serve`"), "{reply}");
        Ok(())
    }
}
