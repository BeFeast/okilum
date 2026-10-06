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
    use std::path::Path;
    use std::time::Duration;

    const SERVICE: &str = "org.freedesktop.FileManager1";
    const OBJECT: &str = "/org/freedesktop/FileManager1";
    /// Bounds service activation so a hung file manager still reaches the
    /// fallback.
    const TIMEOUT: Duration = Duration::from_secs(5);

    pub(super) async fn reveal(path: &Path) -> anyhow::Result<()> {
        let shown = async {
            let bus = zbus::connection::Builder::session()?
                .method_timeout(TIMEOUT)
                .build()
                .await?;
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

    pub(super) async fn show_items(bus: &zbus::Connection, path: &Path) -> anyhow::Result<()> {
        let uri = item_uri(path)?;
        bus.call_method(
            Some(SERVICE),
            OBJECT,
            Some(SERVICE),
            "ShowItems",
            &(vec![uri], ""),
        )
        .await?;
        Ok(())
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
    use super::linux::{fallback_folder, item_uri, show_items};
    use std::io::{BufRead as _, BufReader};
    use std::path::Path;
    use std::process::{Child, Command, Stdio};

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

    /// A private session bus, so the test never reaches the developer's own
    /// file manager.
    struct Bus(Child, String);
    impl Bus {
        fn start() -> Option<Self> {
            let mut child = Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address"])
                .stdout(Stdio::piped())
                .spawn()
                .ok()?;
            let mut address = String::new();
            BufReader::new(child.stdout.take()?)
                .read_line(&mut address)
                .ok()?;
            Some(Self(child, address.trim().to_owned()))
        }
        async fn connect(&self) -> zbus::Connection {
            zbus::connection::Builder::address(self.1.as_str())
                .unwrap()
                .build()
                .await
                .unwrap()
        }
    }
    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    struct FakeFileManager(async_channel::Sender<(Vec<String>, String)>);
    #[zbus::interface(name = "org.freedesktop.FileManager1")]
    impl FakeFileManager {
        async fn show_items(&self, uris: Vec<String>, startup_id: String) {
            let _ = self.0.send((uris, startup_id)).await;
        }
    }

    #[test]
    fn show_items_selects_the_item_and_fails_without_a_file_manager() {
        let Some(bus) = Bus::start() else {
            eprintln!("skipped: dbus-daemon is not installed");
            return;
        };
        zbus::block_on(async {
            let client = bus.connect().await;
            // Without a FileManager1 owner the call fails, which is what
            // routes Reveal to the xdg-open fallback.
            assert!(show_items(&client, Path::new("/vault/a.md")).await.is_err());

            // Positive control: the same call reaches a file manager that
            // owns the name, with the item's URI and an empty startup id.
            let (sender, receiver) = async_channel::unbounded();
            let server = bus.connect().await;
            server
                .object_server()
                .at("/org/freedesktop/FileManager1", FakeFileManager(sender))
                .await
                .unwrap();
            server
                .request_name("org.freedesktop.FileManager1")
                .await
                .unwrap();
            show_items(&client, Path::new("/vault/a note.md"))
                .await
                .unwrap();
            assert_eq!(
                receiver.recv().await.unwrap(),
                (vec!["file:///vault/a%20note.md".to_owned()], String::new())
            );
        });
    }
}
