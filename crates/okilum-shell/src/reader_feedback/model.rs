//! Feedback payloads and bounded public transport. No credentials or automatic retries.
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    io::{Cursor, Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
    time::Duration,
};

pub(super) const ENDPOINT: &str = "https://pomoi.co/api/feedback/okilum";
pub(super) const MAX_IMAGE: usize = 8 * 1024 * 1024;
const MAX_LOG: usize = 256 * 1024;
/// Client-side cap for the optional "video" part. Matches pomoi's
/// FEEDBACK_VIDEO_MAX_BYTES (90,000,000 bytes, below Cloudflare Free's 100 MB
/// request limit). The server's 413 text wins when stricter.
pub(super) const MAX_VIDEO: u64 = 90_000_000;

#[derive(Clone, Debug, Serialize)]
pub(super) struct Metadata {
    pub version: String,
    pub build: String,
    pub channel: String,
    pub source: String,
    pub os: String,
    pub os_version: String,
    pub distro: Option<String>,
    pub xdg_session_type: Option<String>,
    pub desktop: Option<String>,
    pub arch: String,
    pub scale: f32,
    pub window_width: f32,
    pub window_height: f32,
    pub decorations: String,
}
impl Metadata {
    pub fn summary(&self) -> String {
        let session = [
            self.distro.as_deref(),
            self.xdg_session_type.as_deref(),
            self.desktop.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        format!(
            "{} · Build {} · {} · {} · {} · {} · {}×{} @{}× · {}{}",
            self.version,
            self.build,
            self.channel,
            self.source,
            self.os_version,
            self.arch,
            self.window_width,
            self.window_height,
            self.scale,
            self.decorations,
            if session.is_empty() {
                String::new()
            } else {
                format!(" · {session}")
            }
        )
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Kind {
    #[default]
    Bug,
    Feature,
}
impl Kind {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Bug => "bug",
            Self::Feature => "fr",
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Report {
    pub text: String,
    pub kind: Kind,
    pub meta: Metadata,
    pub screenshot: Option<Arc<Vec<u8>>>,
    pub log: Option<String>,
    pub video: Option<Video>,
}

/// A user-chosen screen recording. Only the path is kept; the file is
/// streamed from disk on Send, so a 90 MB clip never sits in memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Video {
    pub path: std::path::PathBuf,
    pub name: String,
    pub size: u64,
    pub mime: &'static str,
}
impl Video {
    pub fn size_label(&self) -> String {
        human_size(self.size)
    }
}
/// Decimal units, like the server limit: 90,000,000 bytes is "90 MB".
pub(super) fn human_size(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        let label = format!("{:.1}", bytes as f64 / 1_000_000.);
        format!("{} MB", label.strip_suffix(".0").unwrap_or(&label))
    } else {
        format!("{} KB", bytes.div_ceil(1000))
    }
}
/// Says how far over the limit a video is, in bytes when the rounded sizes
/// would read the same ("90 MB; the limit is 90 MB").
fn over_limit(bytes: u64) -> String {
    let (size, limit) = (human_size(bytes), human_size(MAX_VIDEO));
    if size == limit {
        format!("{bytes} bytes; the limit is {MAX_VIDEO} bytes")
    } else {
        format!("{size}; the limit is {limit}")
    }
}
/// MIME type for a supported video extension (mp4/m4v, mov, webm).
pub(super) fn video_mime(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "m4v" => Some("video/mp4"),
        "mov" | "qt" => Some("video/quicktime"),
        "webm" => Some("video/webm"),
        _ => None,
    }
}
pub(super) fn video_file(path: &Path) -> Result<Video> {
    let mime = video_mime(path).context("Choose an MP4, MOV or WebM video.")?;
    let metadata = std::fs::metadata(path).context("Couldn't read the video file.")?;
    ensure!(metadata.is_file(), "Choose an MP4, MOV or WebM video file.");
    ensure!(metadata.len() > 0, "The video file is empty.");
    ensure!(
        metadata.len() <= MAX_VIDEO,
        "Video is {}. Trim or compress it and attach again.",
        over_limit(metadata.len())
    );
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "video".into());
    Ok(Video {
        path: path.to_path_buf(),
        name,
        size: metadata.len(),
        mime,
    })
}
impl Report {
    fn validate(&self) -> Result<()> {
        ensure!(
            !self.text.trim().is_empty(),
            "Describe your feedback before sending."
        );
        ensure!(
            self.text.chars().count() <= 10_000 && !self.text.contains('\0'),
            "Feedback must contain at most 10,000 characters and no NUL characters."
        );
        ensure!(
            self.screenshot
                .as_ref()
                .is_none_or(|b| b.len() <= MAX_IMAGE),
            "Screenshot must be at most 8 MB."
        );
        ensure!(
            self.log.as_ref().is_none_or(|s| s.len() <= MAX_LOG),
            "Log must be at most 256 KB."
        );
        if let Some(video) = &self.video {
            // Re-check on Send: the file may have changed since it was attached.
            video_file(&video.path)?;
        }
        Ok(())
    }
    pub fn copy_text(&self) -> String {
        let mut text = format!(
            "{}\n\nKind: {}\n{}",
            self.text,
            self.kind.wire(),
            self.meta.summary()
        );
        if self.screenshot.is_some() {
            text.push_str("\nScreenshot attached in the feedback draft.");
        }
        if let Some(video) = &self.video {
            text.push_str(&format!(
                "\nVideo attached in the feedback draft: {} ({}).",
                video.name,
                video.size_label()
            ));
        }
        if let Some(log) = &self.log {
            text.push_str("\n\nDiagnostic log:\n");
            text.push_str(log);
        }
        text
    }
}
#[derive(Clone, Debug, Deserialize)]
pub(super) struct Receipt {
    pub issue_number: u64,
    pub issue_url: String,
}

pub(super) fn send(report: Report) -> Result<Receipt> {
    send_to(ENDPOINT, &report)
}
fn send_to(endpoint: &str, report: &Report) -> Result<Receipt> {
    use reqwest::blocking::{
        multipart::{Form, Part},
        Client,
    };
    report.validate()?;
    // A large video needs far longer than the text-only 45 s budget.
    let timeout = if report.video.is_some() {
        Duration::from_secs(15 * 60)
    } else {
        Duration::from_secs(45)
    };
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut meta = serde_json::to_value(&report.meta)?;
    if super::sprint::enabled() {
        let context = super::sprint::context();
        for key in ["branch", "durations", "duration_seconds"] {
            if let Some(value) = context.get(key) {
                meta[key] = value.clone();
            }
        }
        meta["stage"] = "manual".into();
    }
    let mut form = Form::new()
        .text("text", report.text.clone())
        .text("kind", report.kind.wire())
        .text("meta", serde_json::to_string(&meta)?);
    if super::sprint::enabled() {
        form = form.text("title", super::sprint::title());
    }
    if let Some(bytes) = &report.screenshot {
        form = form.part(
            "screenshot",
            Part::bytes(bytes.as_ref().clone())
                .file_name("screenshot.png")
                .mime_str("image/png")?,
        );
    }
    if let Some(log) = &report.log {
        form = form.text("log", log.clone());
    }
    if let Some(video) = &report.video {
        let file = std::fs::File::open(&video.path)
            .context("Couldn't open the attached video. Your draft is kept.")?;
        let length = file.metadata()?.len();
        ensure!(
            length <= MAX_VIDEO,
            "Video is {}. Trim or compress it and attach again.",
            over_limit(length)
        );
        form = form.part(
            "video",
            Part::reader_with_length(file.take(length), length)
                .file_name(video.name.clone())
                .mime_str(video.mime)?,
        );
    }
    let response = client
        .post(endpoint)
        .multipart(form)
        .send()
        .context("Couldn't confirm delivery. Your feedback is kept.")?;
    let status = response.status();
    let bytes = bounded(response, 16 * 1024)?;
    if status != reqwest::StatusCode::CREATED {
        let message = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|v| {
                v.get("error")
                    .and_then(|s| s.as_str())
                    .map(|s| s.chars().take(512).collect::<String>())
            })
            .unwrap_or_else(|| match status.as_u16() {
                413 => "Attachments exceed the server's size limit. Your draft is kept.".into(),
                415 => "The server doesn't accept this attachment type. Your draft is kept.".into(),
                _ => "Feedback couldn't be sent. Your draft is kept.".into(),
            });
        bail!("{message} (HTTP {})", status.as_u16());
    }
    let receipt: Receipt = serde_json::from_slice(&bytes)
        .context("Couldn't confirm the created issue. Your draft is kept.")?;
    ensure!(
        receipt.issue_number > 0
            && receipt.issue_url
                == format!(
                    "https://git.oklabs.uk/BeFeast/okilum/issues/{}",
                    receipt.issue_number
                ),
        "Unexpected feedback issue link. Your draft is kept."
    );
    Ok(receipt)
}
fn bounded(reader: impl Read, maximum: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= maximum,
        "Attachment or response exceeds its size limit."
    );
    Ok(bytes)
}

pub(super) fn log_tail(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).context(
        "Couldn't read the diagnostic log. Uncheck the log attachment to send without it.",
    )?;
    let length = file.metadata()?.len();
    let start = length.saturating_sub(MAX_LOG as u64);
    file.seek(SeekFrom::Start(start))?;
    let bytes = bounded(file.take(length - start), MAX_LOG)?;
    // A byte cap may split a UTF-8 character or a line. Discard that first
    // partial line, then keep at most 250 complete trailing lines.
    let bytes = if start > 0 {
        &bytes[bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |i| i + 1)..]
    } else {
        &bytes[..]
    };
    let text =
        std::str::from_utf8(bytes).context("Diagnostic log isn't UTF-8; send without the log.")?;
    let lines = text.lines().collect::<Vec<_>>();
    Ok(lines[lines.len().saturating_sub(250)..].join("\n"))
}

/// Accept user-selected PNG/JPEG, plus the BMP representation native
/// clipboards use. The endpoint accepts PNG, so normalize before preview/send.
pub(super) fn image_bytes(
    bytes: &[u8],
    clipboard: bool,
) -> Result<(Arc<Vec<u8>>, image::RgbaImage)> {
    use image::ImageDecoder;
    let format = image::guess_format(bytes).context("Choose a PNG or JPEG screenshot.")?;
    let maximum = if clipboard && format == image::ImageFormat::Bmp {
        64 * 1024 * 1024
    } else {
        MAX_IMAGE
    };
    ensure!(
        bytes.len() <= maximum,
        "Screenshot must be at most 8 MB (PNG/JPEG)."
    );
    ensure!(
        matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg)
            || clipboard && format == image::ImageFormat::Bmp,
        "Choose a PNG or JPEG screenshot."
    );
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut image = image::DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    let mut png = Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png)?;
    ensure!(
        png.get_ref().len() <= MAX_IMAGE,
        "Screenshot exceeds 8 MB after PNG conversion. Choose a smaller image."
    );
    let mut preview = image
        .thumbnail(image.width().min(640), image.height().min(280))
        .into_rgba8();
    // RenderImage uses BGRA; the transmitted PNG retains its original colors.
    for pixel in preview.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Ok((Arc::new(png.into_inner()), preview))
}
pub(super) fn image_file(path: &Path) -> Result<(Arc<Vec<u8>>, image::RgbaImage)> {
    let metadata = std::fs::metadata(path)?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_IMAGE as u64,
        "Choose a PNG or JPEG file of at most 8 MB."
    );
    image_bytes(&bounded(std::fs::File::open(path)?, MAX_IMAGE)?, false)
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
