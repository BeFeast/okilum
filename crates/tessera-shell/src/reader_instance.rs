//! One desktop Reader process per application state directory. The lock is OS
//! owned (a crash releases it); the authenticated loopback endpoint only carries
//! open intents. Canonical path selection remains in reader_open.
use super::*;
use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};

#[derive(serde::Serialize, serde::Deserialize)]
struct Endpoint {
    port: u16,
    pid: u32,
    nonce: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct Request {
    nonce: String,
    vault: Option<PathBuf>,
    path: Option<PathBuf>,
    note: Option<String>,
    index_dir: Option<PathBuf>,
    session_directory: Option<PathBuf>,
    query: Option<String>,
    use_html: bool,
    copy_source: bool,
    jump: bool,
}

pub(crate) enum Instance {
    Primary(std::fs::File, async_channel::Receiver<Request>),
    Forwarded,
}

fn absolute(path: &Option<PathBuf>, cwd: &Path) -> Option<PathBuf> {
    path.as_ref().map(|path| {
        if path.is_absolute() {
            path.clone()
        } else {
            cwd.join(path)
        }
    })
}

pub(crate) fn connect(opts: &Opts) -> anyhow::Result<Instance> {
    let directory = opts
        .session_directory
        .clone()
        .map(Ok)
        .unwrap_or_else(reader_history::state_directory)?;
    std::fs::create_dir_all(&directory)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(directory.join("reader-instance.lock"))?;
    match lock.try_lock() {
        Ok(()) => {
            let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
            let endpoint = Endpoint {
                port: listener.local_addr()?.port(),
                pid: std::process::id(),
                nonce: uuid::Uuid::new_v4().to_string(),
            };
            // Windows byte-range locks prohibit other processes from reading
            // the locked file. Publish the endpoint separately and atomically.
            let mut descriptor = tempfile::NamedTempFile::new_in(&directory)?;
            serde_json::to_writer(&mut descriptor, &endpoint)?;
            descriptor.flush()?;
            descriptor.persist(directory.join("reader-instance.json"))?;
            let (send, receive) = async_channel::unbounded();
            std::thread::Builder::new()
                .name("reader-open-intents".into())
                .spawn(move || {
                    for connection in listener.incoming() {
                        let Ok(mut stream) = connection else {
                            break;
                        };
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                        let mut line = String::new();
                        if std::io::BufReader::new((&mut stream).take(65536))
                            .read_line(&mut line)
                            .is_err()
                        {
                            continue;
                        }
                        let Ok(request) = serde_json::from_str::<Request>(&line) else {
                            continue;
                        };
                        if request.nonce != endpoint.nonce {
                            continue;
                        }
                        if send.try_send(request).is_err() {
                            break;
                        }
                        let _ = stream.write_all(b"OK\n");
                    }
                })?;
            Ok(Instance::Primary(lock, receive))
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            let cwd = std::env::current_dir()?;
            // The owner may still be writing its endpoint immediately after
            // winning the lock. Retry only that handoff; never start a duplicate.
            let mut last = anyhow::anyhow!("The existing Reader is not responding");
            for _ in 0..20 {
                let result = (|| -> anyhow::Result<()> {
                    let endpoint: Endpoint = serde_json::from_slice(&std::fs::read(
                        directory.join("reader-instance.json"),
                    )?)?;
                    #[cfg(windows)]
                    unsafe {
                        windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(
                            endpoint.pid,
                        );
                    }
                    let mut stream = TcpStream::connect_timeout(
                        &std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, endpoint.port)),
                        Duration::from_millis(250),
                    )?;
                    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                    let request = Request {
                        nonce: endpoint.nonce,
                        vault: absolute(&opts.vault, &cwd),
                        path: absolute(&opts.open_path, &cwd),
                        note: opts.note.clone(),
                        index_dir: absolute(&opts.index_dir, &cwd),
                        session_directory: Some(directory.clone()),
                        query: opts.query.clone(),
                        use_html: opts.use_html,
                        copy_source: opts.copy_source,
                        jump: opts.jump,
                    };
                    serde_json::to_writer(&mut stream, &request)?;
                    stream.write_all(b"\n")?;
                    let mut ack = [0; 3];
                    stream.read_exact(&mut ack)?;
                    anyhow::ensure!(&ack == b"OK\n", "Reader rejected the open request");
                    Ok(())
                })();
                match result {
                    Ok(()) => return Ok(Instance::Forwarded),
                    Err(error) => last = error,
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(last.context("Cannot contact the already running Reader"))
        }
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

pub(crate) fn receive(receiver: async_channel::Receiver<Request>, cx: &mut App) {
    cx.spawn(async move |cx| {
        while let Ok(request) = receiver.recv().await {
            cx.update(|cx| {
                if request.vault.is_some() || request.path.is_some() {
                    if let Err(error) = reader_open::open_window(
                        Opts {
                            vault: request.vault,
                            open_path: request.path,
                            note: request.note,
                            index_dir: request.index_dir,
                            session_directory: request.session_directory,
                            query: request.query,
                            use_html: request.use_html,
                            copy_source: request.copy_source,
                            jump: request.jump,
                            ..Default::default()
                        },
                        cx,
                    ) {
                        eprintln!("Cannot open requested document: {error:#}");
                    }
                } else {
                    cx.activate(true);
                    if let Some(window) =
                        cx.active_window().or_else(|| cx.windows().first().copied())
                    {
                        let _ = window.update(cx, |_, window, _| window.activate_window());
                    }
                }
            });
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn second_launch_delivers_vault_and_note_to_the_lock_owner() {
        let fixture = tempfile::tempdir().unwrap();
        let opts = Opts {
            session_directory: Some(fixture.path().join("state")),
            vault: Some(PathBuf::from("relative vault")),
            note: Some("note.md".into()),
            ..Default::default()
        };
        let Instance::Primary(_lock, requests) = connect(&opts).unwrap() else {
            panic!("first launch must own endpoint");
        };
        assert!(matches!(connect(&opts).unwrap(), Instance::Forwarded));
        let request = requests.try_recv().unwrap();
        assert_eq!(
            request.vault,
            Some(std::env::current_dir().unwrap().join("relative vault"))
        );
        assert_eq!(request.note.as_deref(), Some("note.md"));
        assert_eq!(request.session_directory, opts.session_directory);
        assert!(
            requests.try_recv().is_err(),
            "one launch creates one request"
        );
        let different = Opts {
            session_directory: Some(fixture.path().join("isolated-state")),
            ..Default::default()
        };
        assert!(
            matches!(connect(&different).unwrap(), Instance::Primary(_, _)),
            "isolated profiles remain independent"
        );
    }
}
