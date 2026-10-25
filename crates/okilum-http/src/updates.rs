//! Velopack's supported UpdateSource extension; its checksum/install/restart logic stays intact.
use anyhow::{ensure, Context, Result};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
    sync::mpsc::Sender,
    time::Duration,
};
use velopack::{bundle::Manifest, sources::UpdateSource, VelopackAsset, VelopackAssetFeed};

#[derive(Clone)]
pub struct Source {
    base: url::Url,
    version: String,
}
impl Source {
    pub fn new(base: &str, version: &str) -> Result<Self> {
        let base = url::Url::parse(&format!("{}/", base.trim_end_matches('/')))?;
        ensure!(
            matches!(base.scheme(), "https" | "http"),
            "Unsupported feed scheme"
        );
        Ok(Self {
            base,
            version: version.into(),
        })
    }
    fn response(&self, url: &url::Url) -> Result<reqwest::blocking::Response> {
        let response = crate::client(url.as_str(), &self.version, Duration::from_secs(300))?
            .get(url.clone())
            .send()?
            .error_for_status()?;
        ensure!(
            response.status() == reqwest::StatusCode::OK,
            "{}",
            crate::http_reason(response.status().as_u16())
        );
        Ok(response)
    }
    pub fn feed(
        &self,
        channel: &str,
        app: &Manifest,
        staged_user_id: &str,
    ) -> Result<VelopackAssetFeed> {
        ensure!(
            !channel.is_empty()
                && channel
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
            "Invalid release channel"
        );
        let mut url = self.base.join(&format!("releases.{channel}.json"))?;
        url.query_pairs_mut()
            .append_pair("localVersion", &app.version.to_string())
            .append_pair("id", &app.id)
            .append_pair("stagingId", staged_user_id);
        // Bound remote metadata independently of Content-Length.
        let mut bytes = Vec::new();
        self.response(&url)?
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 4 * 1024 * 1024, "Release feed exceeds 4 MiB");
        serde_json::from_slice(&bytes).context("Invalid release feed")
    }
    pub fn download(
        &self,
        asset: &VelopackAsset,
        local_file: &Path,
        progress: Option<Sender<i16>>,
    ) -> Result<()> {
        // A package filename must not redirect a trusted feed download elsewhere.
        ensure!(
            !asset.FileName.is_empty()
                && asset
                    .FileName
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
                && asset.FileName != "."
                && asset.FileName != "..",
            "Invalid package filename"
        );
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("Feed URL cannot hold package paths"))?
            .pop_if_empty()
            .push(&asset.FileName);
        let mut response = self.response(&url)?;
        let mut file = File::create(local_file)?;
        let mut count = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        if let Some(sender) = &progress {
            let _ = sender.send(0);
        }
        loop {
            let read = response.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            count += read as u64;
            ensure!(count <= asset.Size, "Package exceeds its declared size");
            file.write_all(&buffer[..read])?;
            if let Some(sender) = &progress {
                let percent =
                    ((count as u128 * 100) / u128::from(asset.Size.max(1))).min(100) as i16;
                let _ = sender.send(percent);
            }
        }
        ensure!(
            count == asset.Size,
            "Incomplete package: expected {} bytes, received {count}",
            asset.Size
        );
        file.flush()?;
        // UpdateManager verifies SHA256 and size before renaming the partial package.
        Ok(())
    }
}
impl UpdateSource for Source {
    fn get_release_feed(
        &self,
        channel: &str,
        app: &Manifest,
        staged_user_id: &str,
    ) -> Result<VelopackAssetFeed, velopack::Error> {
        self.feed(channel, app, staged_user_id)
            .map_err(source_error)
    }
    fn download_release_entry(
        &self,
        asset: &VelopackAsset,
        local_file: &Path,
        progress: Option<Sender<i16>>,
    ) -> Result<(), velopack::Error> {
        self.download(asset, local_file, progress)
            .map_err(source_error)
    }
}

fn source_error(error: anyhow::Error) -> velopack::Error {
    let summary = if error.chain().any(|cause| cause.is::<reqwest::Error>()) {
        crate::reason(&error)
    } else {
        error.to_string()
    };
    velopack::Error::Other(format!("{summary}\nDetails: {error:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, thread};

    fn server(
        status: u16,
        body: Vec<u8>,
        extra: &'static str,
    ) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/updates", listener.local_addr().unwrap());
        let task = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            write!(socket, "HTTP/1.1 {status} Response\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n", body.len()).unwrap();
            // Feed limits may intentionally stop reading before the server finishes.
            let _ = socket.write_all(&body);
            String::from_utf8(request).unwrap()
        });
        (url, task)
    }
    fn app() -> Manifest {
        Manifest {
            id: "BeFeast + &?".into(),
            version: "0.1.123".parse().unwrap(),
            ..Default::default()
        }
    }
    fn asset(name: &str, size: u64) -> VelopackAsset {
        VelopackAsset {
            FileName: name.into(),
            Size: size,
            ..Default::default()
        }
    }
    #[test]
    fn feed_preserves_channel_query_and_user_agent() {
        for channel in ["stable", "beta"] {
            let (url, task) = server(200, br#"{"Assets":[]}"#.to_vec(), "");
            let source = Source::new(&url, "0.1.test").unwrap();
            assert!(source
                .get_release_feed(channel, &app(), "staging & ü")
                .unwrap()
                .Assets
                .is_empty());
            let request = task.join().unwrap();
            let target = request
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap();
            let parsed = url::Url::parse(&format!("http://localhost{target}")).unwrap();
            assert_eq!(parsed.path(), format!("/updates/releases.{channel}.json"));
            let query: std::collections::HashMap<_, _> =
                parsed.query_pairs().into_owned().collect();
            assert_eq!(query["localVersion"], "0.1.123");
            assert_eq!(query["id"], app().id);
            assert_eq!(query["stagingId"], "staging & ü");
            assert!(request
                .to_ascii_lowercase()
                .contains("user-agent: okilum/0.1.test ("));
            assert!(request.starts_with("GET "));
        }
    }
    #[test]
    fn package_streams_bytes_and_reports_progress() {
        let body = vec![42; 200_000];
        let (url, task) = server(200, body.clone(), "");
        let source = Source::new(&url, "test").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("package.partial");
        let (tx, rx) = std::sync::mpsc::channel();
        source
            .download_release_entry(
                &asset("App-0.1.2-full.nupkg", body.len() as u64),
                &file,
                Some(tx),
            )
            .unwrap();
        assert_eq!(std::fs::read(file).unwrap(), body);
        let progress: Vec<_> = rx.iter().collect();
        assert_eq!(progress.first(), Some(&0));
        assert_eq!(progress.last(), Some(&100));
        assert!(progress.windows(2).all(|v| v[0] <= v[1]));
        assert!(task
            .join()
            .unwrap()
            .starts_with("GET /updates/App-0.1.2-full.nupkg "));
    }
    #[test]
    fn size_mismatch_rejects_short_and_overlong_downloads() {
        for size in [2, 6] {
            let (url, task) = server(200, b"four".to_vec(), "");
            let dir = tempfile::tempdir().unwrap();
            let error = Source::new(&url, "test")
                .unwrap()
                .download_release_entry(
                    &asset("app.nupkg", size),
                    &dir.path().join("partial"),
                    None,
                )
                .unwrap_err();
            assert!(
                error.to_string().contains("size") || error.to_string().contains("Incomplete"),
                "{error}"
            );
            task.join().unwrap();
        }
    }
    #[test]
    fn rejection_and_redirect_never_overwrite_existing_destination() {
        for status in [302, 403, 407, 500] {
            let (url, task) = server(
                status,
                Vec::new(),
                "Location: https://example.invalid/package\r\n",
            );
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("partial");
            std::fs::write(&file, b"retained").unwrap();
            let error = Source::new(&url, "test")
                .unwrap()
                .download_release_entry(&asset("app.nupkg", 8), &file, None)
                .unwrap_err();
            assert!(error.to_string().contains(&status.to_string()), "{error}");
            assert_eq!(std::fs::read(file).unwrap(), b"retained");
            task.join().unwrap();
        }
    }
    #[test]
    fn feed_limit_and_invalid_json_are_not_accepted() {
        for body in [b"not JSON".to_vec(), vec![b' '; 4 * 1024 * 1024 + 1]] {
            let (url, task) = server(200, body, "");
            assert!(Source::new(&url, "test")
                .unwrap()
                .get_release_feed("stable", &app(), "staging")
                .is_err());
            task.join().unwrap();
        }
    }
    #[test]
    fn untrusted_path_components_are_rejected_before_network_or_file_io() {
        let source = Source::new("http://127.0.0.1:1/updates", "test").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("must-not-exist");
        for name in [
            "../outside",
            "https://other.invalid/app",
            "%2e%2e",
            "file?query",
            "file#fragment",
            "a\\b",
            ".",
            "..",
            "",
            "/absolute",
            "a\nb",
        ] {
            let error = source
                .download_release_entry(&asset(name, 1), &file, None)
                .unwrap_err();
            assert!(error.to_string().contains("filename"), "{name}: {error}");
            assert!(!file.exists());
        }
        for channel in ["../stable", "stable?query", "", "a/b"] {
            assert!(source
                .get_release_feed(channel, &app(), "id")
                .unwrap_err()
                .to_string()
                .contains("channel"));
        }
    }
    #[test]
    fn velopack_manager_still_checks_hash_before_final_package_rename() {
        for valid in [false, true] {
            let bytes = b"payload".to_vec();
            let (url, server) = server(200, bytes, "");
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let packages = root.join("packages");
            std::fs::create_dir(&packages).unwrap();
            let update_exe = root.join("Update.exe");
            std::fs::write(&update_exe, b"fixture only; never executed").unwrap();
            let manifest = root.join("sq.version");
            std::fs::write(&manifest, br#"<package><metadata><id>Okilum.NetworkRegression</id><version>0.1.1</version><mainExe>fixture.exe</mainExe><os>win</os><channel>stable</channel></metadata></package>"#).unwrap();
            let manager = velopack::UpdateManager::new(
                Source::new(&url, "test").unwrap(),
                Some(velopack::UpdateOptions {
                    ExplicitChannel: Some("stable".into()),
                    ..Default::default()
                }),
                Some(velopack::locator::VelopackLocatorConfig {
                    RootAppDir: root.into(),
                    UpdateExePath: update_exe,
                    PackagesDir: packages.clone(),
                    ManifestPath: manifest,
                    CurrentBinaryDir: root.into(),
                    IsPortable: false,
                }),
            )
            .unwrap();
            let mut release = asset("App-0.1.2-full.nupkg", 7);
            release.PackageId = "Okilum.NetworkRegression".into();
            release.Version = "0.1.2".into();
            release.Type = "Full".into();
            release.SHA256 = if valid {
                "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5"
            } else {
                "0000000000000000000000000000000000000000000000000000000000000000"
            }
            .into();
            let update = velopack::UpdateInfo {
                TargetFullRelease: release.clone(),
                ..Default::default()
            };
            let result = manager.download_updates(&update, None);
            assert_eq!(result.is_ok(), valid, "{result:?}");
            assert_eq!(packages.join(&release.FileName).exists(), valid);
            if !valid {
                assert!(matches!(result, Err(velopack::Error::ChecksumInvalid(..))));
            }
            server.join().unwrap();
        }
    }
}
