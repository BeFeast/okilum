//! Show a file selected in the system file manager (#612).
//!
//! macOS and Windows use GPUI's native reveal (Finder, `explorer /select`).
//! On Linux the freedesktop `org.freedesktop.FileManager1.ShowItems` D-Bus
//! call opens the containing folder with the item selected; when no file
//! manager implements it, `xdg-open` opens the parent folder instead.

use gpui::{App, Task};
use std::path::Path;

/// The returned task fails only when every route failed, so callers can say
/// so instead of leaving the user with a silent no-op.
pub fn reveal_path(path: &Path, cx: &App) -> Task<anyhow::Result<()>> {
    #[cfg(target_os = "linux")]
    {
        let path = path.to_owned();
        cx.background_executor()
            .spawn(async move { linux::reveal(&path).await })
    }
    #[cfg(not(target_os = "linux"))]
    {
        cx.reveal_path(path);
        Task::ready(Ok(()))
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use anyhow::Context as _;
    use std::future::Future;
    use std::path::Path;
    use std::time::Duration;

    const SERVICE: &str = "org.freedesktop.FileManager1";
    const OBJECT: &str = "/org/freedesktop/FileManager1";
    /// Bounds service activation so a hung file manager still reaches the
    /// fallback.
    const TIMEOUT: Duration = Duration::from_secs(5);

    pub(super) async fn reveal(path: &Path) -> anyhow::Result<()> {
        let shown = async {
            let bus = connect(zbus::connection::Builder::session()?).await?;
            show_items(&bus, path).await
        }
        .await;
        let Err(dbus) = shown else {
            return Ok(());
        };
        let folder = fallback_folder(path);
        // Blocking is fine: this runs on a background thread.
        let status = std::process::Command::new("xdg-open")
            .arg(folder)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .with_context(|| {
                format!("No file manager answered over D-Bus ({dbus}) and xdg-open is unavailable")
            })?;
        anyhow::ensure!(
            status.success(),
            "No file manager answered over D-Bus ({dbus}) and xdg-open could not open {}",
            folder.display()
        );
        Ok(())
    }

    /// zbus's method timeout only covers the reply wait, not connection/auth or
    /// sending the call. Bound the whole operation on either executor backend.
    pub(super) async fn deadline<T>(
        stage: &str,
        operation: impl Future<Output = anyhow::Result<T>>,
    ) -> anyhow::Result<T> {
        let timer = async_io::Timer::after(TIMEOUT);
        futures::pin_mut!(operation, timer);
        match futures::future::select(operation, timer).await {
            futures::future::Either::Left((result, _)) => result,
            futures::future::Either::Right(_) => {
                anyhow::bail!("{stage} timed out after {}s", TIMEOUT.as_secs())
            }
        }
    }

    pub(super) async fn connect(
        builder: zbus::connection::Builder<'_>,
    ) -> anyhow::Result<zbus::Connection> {
        deadline("D-Bus connection/authentication", async {
            Ok(builder.method_timeout(TIMEOUT).build().await?)
        })
        .await
    }

    pub(super) async fn show_items(bus: &zbus::Connection, path: &Path) -> anyhow::Result<()> {
        let uri = item_uri(path)?;
        deadline("FileManager1.ShowItems", async {
            bus.call_method(
                Some(SERVICE),
                OBJECT,
                Some(SERVICE),
                "ShowItems",
                &(vec![uri], ""),
            )
            .await?;
            Ok(())
        })
        .await
    }

    pub(super) fn item_uri(path: &Path) -> anyhow::Result<String> {
        url::Url::from_file_path(path)
            .map(String::from)
            .map_err(|()| anyhow::anyhow!("Not an absolute path: {}", path.display()))
    }

    /// The folder that contains the item, as the issue asks for the fallback.
    pub(super) fn fallback_folder(path: &Path) -> &Path {
        path.parent().unwrap_or(path)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::linux::{connect, deadline, fallback_folder, item_uri, show_items};
    use std::io::Write as _;
    use std::path::Path;
    use std::process::{Child, Command};

    #[test]
    fn item_uri_is_an_encoded_file_url() {
        assert_eq!(
            item_uri(Path::new("/vault/a note, #1.md")).unwrap(),
            "file:///vault/a%20note,%20%231.md"
        );
        assert!(item_uri(Path::new("relative.md")).is_err());
    }

    #[test]
    fn fallback_opens_the_containing_folder() {
        assert_eq!(
            fallback_folder(Path::new("/vault/notes/a.md")),
            Path::new("/vault/notes")
        );
        assert_eq!(
            fallback_folder(Path::new("/vault/notes")),
            Path::new("/vault")
        );
        assert_eq!(fallback_folder(Path::new("/")), Path::new("/"));
    }

    /// A private session bus with no service directories: `--session` would
    /// still discover and activate the host's installed file manager (#2458).
    struct Bus(Child, String);
    impl Bus {
        fn start() -> Option<Self> {
            let mut config = tempfile::NamedTempFile::new().unwrap();
            // Do not include session.conf or standard_session_servicedirs.
            // An abstract socket needs no filesystem path or socket cleanup.
            writeln!(
                config,
                r#"<busconfig>
  <type>session</type>
  <listen>unix:abstract=okilum-reveal-{}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*"/>
    <allow receive_sender="*"/>
    <allow own="*"/>
  </policy>
</busconfig>"#,
                uuid::Uuid::new_v4()
            )
            .unwrap();
            // A regular file avoids a read_line/reader-join wait if a sibling
            // process fixture temporarily inherits a pipe writer before exec.
            let address = tempfile::NamedTempFile::new().unwrap();
            let child = match Command::new("dbus-daemon")
                .arg("--config-file")
                .arg(config.path())
                .args(["--nofork", "--print-address"])
                .stdout(address.reopen().unwrap())
                .spawn()
            {
                Ok(child) => child,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
                Err(error) => panic!("could not start private dbus-daemon: {error}"),
            };
            // Install the cleanup guard before any fallible startup checks.
            let mut bus = Self(child, String::new());
            let started = std::time::Instant::now();
            loop {
                bus.1 = std::fs::read_to_string(address.path()).unwrap();
                if bus.1.ends_with('\n') {
                    break;
                }
                assert!(
                    bus.0.try_wait().unwrap().is_none(),
                    "private dbus-daemon exited before printing its address"
                );
                assert!(
                    started.elapsed() < std::time::Duration::from_secs(5),
                    "private dbus-daemon startup timed out"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            bus.1 = bus.1.trim().to_owned();
            assert!(
                !bus.1.is_empty(),
                "private dbus-daemon did not print an address"
            );
            Some(bus)
        }
        async fn connect(&self) -> zbus::Connection {
            connect(zbus::connection::Builder::address(self.1.as_str()).unwrap())
                .await
                .unwrap()
        }
    }
    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            self.0.wait().expect("could not reap private dbus-daemon");
            eprintln!("reveal: private dbus-daemon reaped");
        }
    }

    #[test]
    fn silent_bus_authentication_times_out() {
        use std::io::Read as _;
        use std::os::unix::net::UnixListener;
        use std::time::{Duration, Instant};

        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("silent-bus");
        let listener = UnixListener::bind(&socket).unwrap();
        let address = format!("unix:path={}", socket.display());
        let started = Instant::now();
        let error = zbus::block_on(connect(
            zbus::connection::Builder::address(address.as_str()).unwrap(),
        ))
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("connection/authentication timed out"));
        assert!(started.elapsed() < Duration::from_secs(10));
        // Control: the connection really reached the socket and sent AUTH;
        // refusal before the handshake cannot count as a passing timeout test.
        listener.set_nonblocking(true).unwrap();
        let (mut client, _) = listener.accept().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut auth = [0; 128];
        let count = client.read(&mut auth).unwrap();
        assert!(String::from_utf8_lossy(&auth[..count]).contains("AUTH"));
    }

    struct FakeFileManager {
        received: async_channel::Sender<(Vec<String>, String)>,
        release: Option<async_channel::Receiver<()>>,
    }
    #[zbus::interface(name = "org.freedesktop.FileManager1")]
    impl FakeFileManager {
        async fn show_items(&self, uris: Vec<String>, startup_id: String) {
            let _ = self.received.send((uris, startup_id)).await;
            if let Some(release) = &self.release {
                let _ = release.recv().await;
            }
        }
    }

    #[test]
    fn show_items_times_out_when_manager_never_answers() {
        use std::time::{Duration, Instant};

        let bus = Bus::start().expect("dbus-daemon is required for the timeout control");
        zbus::block_on(async {
            // Deliberately omit zbus's method timeout: this must exercise the
            // product ShowItems deadline, not a second, test-only safety net.
            let client = deadline("control connection", async {
                Ok(zbus::connection::Builder::address(bus.1.as_str())?
                    .build()
                    .await?)
            })
            .await
            .unwrap();
            let server = bus.connect().await;
            let (received, receipt) = async_channel::unbounded();
            let (release, blocked) = async_channel::unbounded();
            server
                .object_server()
                .at(
                    "/org/freedesktop/FileManager1",
                    FakeFileManager {
                        received,
                        release: Some(blocked),
                    },
                )
                .await
                .unwrap();
            server
                .request_name("org.freedesktop.FileManager1")
                .await
                .unwrap();
            let started = Instant::now();
            let error = show_items(&client, Path::new("/vault/a.md"))
                .await
                .unwrap_err();
            assert!(
                error.to_string().contains("ShowItems timed out"),
                "{error:#}"
            );
            assert!(started.elapsed() < Duration::from_secs(10));
            assert_eq!(receipt.try_recv().unwrap().0, vec!["file:///vault/a.md"]);
            drop(release);
        });
    }

    #[test]
    fn show_items_selects_the_item_and_fails_without_a_file_manager() {
        eprintln!("reveal: starting private bus");
        let Some(bus) = Bus::start() else {
            eprintln!("skipped: dbus-daemon is not installed");
            return;
        };
        zbus::block_on(deadline("private bus test", async {
            eprintln!("reveal: connecting client");
            let client = bus.connect().await;
            eprintln!("reveal: listing activatable names");
            let daemon = zbus::fdo::DBusProxy::new(&client).await.unwrap();
            // A missing-manager error alone could also mean activation failed.
            // Prove that no host service can be activated in the first place.
            assert!(daemon
                .list_activatable_names()
                .await
                .unwrap()
                .iter()
                .all(|name| name.as_str() == "org.freedesktop.DBus"));
            // Without a FileManager1 owner the call fails, which is what
            // routes Reveal to the xdg-open fallback.
            eprintln!("reveal: calling missing FileManager1");
            let missing = show_items(&client, Path::new("/vault/a.md"))
                .await
                .unwrap_err();
            assert!(
                matches!(missing.downcast_ref::<zbus::Error>(),
                    Some(zbus::Error::MethodError(name, _, _))
                        if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown"),
                "expected ServiceUnknown from the private bus, got: {missing:#}"
            );
            eprintln!("reveal: missing-manager error received");

            // Positive control: the same call reaches a file manager that
            // owns the name, with the item's URI and an empty startup id.
            let (sender, receiver) = async_channel::unbounded();
            eprintln!("reveal: connecting fake manager");
            let server = bus.connect().await;
            eprintln!("reveal: registering fake manager object");
            server
                .object_server()
                .at(
                    "/org/freedesktop/FileManager1",
                    FakeFileManager {
                        received: sender,
                        release: None,
                    },
                )
                .await
                .unwrap();
            eprintln!("reveal: requesting manager name");
            server
                .request_name("org.freedesktop.FileManager1")
                .await
                .unwrap();
            eprintln!("reveal: calling fake manager");
            show_items(&client, Path::new("/vault/a note.md"))
                .await
                .unwrap();
            eprintln!("reveal: receiving positive control");
            assert_eq!(
                deadline("fake manager receipt", async { Ok(receiver.recv().await?) })
                    .await
                    .unwrap(),
                (vec!["file:///vault/a%20note.md".to_owned()], String::new())
            );
            Ok(())
        }))
        .unwrap();
        eprintln!("reveal: finished, reaping private bus");
    }
}
