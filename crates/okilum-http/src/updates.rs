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
            response.status().is_success(),
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
                && !asset.FileName.contains(['/', '\\', ':'])
                && asset.FileName != "."
                && asset.FileName != "..",
            "Invalid package filename"
        );
        let url = self.base.join(&asset.FileName)?;
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
            .map_err(|error| velopack::Error::Other(format!("{error:#}")))
    }
    fn download_release_entry(
        &self,
        asset: &VelopackAsset,
        local_file: &Path,
        progress: Option<Sender<i16>>,
    ) -> Result<(), velopack::Error> {
        self.download(asset, local_file, progress)
            .map_err(|error| velopack::Error::Other(format!("{error:#}")))
    }
}
