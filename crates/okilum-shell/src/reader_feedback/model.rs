//! Feedback payloads and bounded public transport. No credentials or automatic retries.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    io::{Cursor, Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
    time::Duration,
};

#[path = "draft.rs"]
pub(super) mod draft;

pub(super) const ENDPOINT: &str = "https://pomoi.co/api/feedback/okilum";
const FALLBACK_ENDPOINT: &str = "https://feedback.befeast.com/api/feedback/okilum";
pub(super) const MAX_IMAGE: usize = 8 * 1024 * 1024;
pub(super) const MAX_LOG: usize = 256 * 1024;
/// Client-side cap for the optional "video" part. Matches pomoi's
/// FEEDBACK_VIDEO_MAX_BYTES (90,000,000 bytes, below Cloudflare Free's 100 MB
/// request limit). The server's 413 text wins when stricter.
pub(super) const MAX_VIDEO: u64 = 90_000_000;

/// User preferences may tighten the endpoint's ceilings, never raise them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Limits {
    pub video_bytes: u64,
    pub video_seconds: Option<u64>,
    pub screenshot_bytes: u64,
    pub log_bytes: u64,
    pub total_bytes: u64,
    pub max_attachments: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            video_bytes: MAX_VIDEO,
            // The endpoint defines a byte cap, not a duration cap. A duration
            // restriction is explicit and requires readable container timing.
            video_seconds: None,
            screenshot_bytes: MAX_IMAGE as u64,
            log_bytes: MAX_LOG as u64,
            total_bytes: MAX_VIDEO + MAX_IMAGE as u64 + MAX_LOG as u64,
            max_attachments: 3,
        }
    }
}
impl Limits {
    pub fn validate(self) -> Result<Self> {
        for (label, value, ceiling) in [
            ("Video", self.video_bytes, MAX_VIDEO),
            ("Screenshot", self.screenshot_bytes, MAX_IMAGE as u64),
            ("Diagnostic log", self.log_bytes, MAX_LOG as u64),
            (
                "Total attachments",
                self.total_bytes,
                Self::default().total_bytes,
            ),
        ] {
            ensure!(
                value > 0 && value <= ceiling,
                "{label} limit must be greater than zero and at most {} ({ceiling} bytes).",
                human_size(ceiling)
            );
        }
        ensure!(
            self.video_seconds != Some(0),
            "Video duration must be positive, or blank for no duration limit."
        );
        ensure!(
            (1..=3).contains(&self.max_attachments),
            "Attachment count must be between 1 and 3 (one screenshot, one video and one log)."
        );
        Ok(self)
    }
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::File::open(path) {
            Ok(file) => serde_json::from_slice::<Self>(&bounded(file, 16 * 1024)?)
                .context("Read Feedback attachment limits")?
                .validate(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }
    pub fn save(self, path: &Path) -> Result<()> {
        use std::io::Write;
        self.validate()?;
        let parent = path
            .parent()
            .context("Feedback limits directory unavailable")?;
        std::fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&serde_json::to_vec_pretty(&self)?)?;
        temporary.as_file().sync_all()?;
        temporary.persist(path).map_err(|error| error.error)?;
        Ok(())
    }
    pub fn fields(self) -> [String; 6] {
        [
            decimal_units(self.video_bytes, 1_000_000),
            self.video_seconds
                .map(|n| n.to_string())
                .unwrap_or_default(),
            decimal_units(self.screenshot_bytes, 1_000_000),
            decimal_units(self.log_bytes, 1_000),
            decimal_units(self.total_bytes, 1_000_000),
            self.max_attachments.to_string(),
        ]
    }
    pub fn from_fields(fields: [String; 6]) -> Result<Self> {
        Self {
            video_bytes: parse_units(&fields[0], 1_000_000)?,
            video_seconds: if fields[1].trim().is_empty() {
                None
            } else {
                Some(fields[1].trim().parse().context(
                    "Video duration must be whole seconds, or blank for no duration limit.",
                )?)
            },
            screenshot_bytes: parse_units(&fields[2], 1_000_000)?,
            log_bytes: parse_units(&fields[3], 1_000)?,
            total_bytes: parse_units(&fields[4], 1_000_000)?,
            max_attachments: fields[5]
                .trim()
                .parse()
                .context("Attachment count must be a whole number from 1 to 3.")?,
        }
        .validate()
    }
    pub fn summary(self) -> String {
        let duration = self
            .video_seconds
            .map(|n| format!("{n} s maximum"))
            .unwrap_or_else(|| "no duration limit".into());
        format!(
            "Video up to {} ({duration}); screenshot up to {}; log up to {}; at most {} attachments, {} combined.",
            human_size(self.video_bytes),
            human_size(self.screenshot_bytes),
            human_size(self.log_bytes),
            self.max_attachments,
            human_size(self.total_bytes)
        )
    }
    pub fn check_sizes(
        self,
        screenshot: Option<u64>,
        video: Option<u64>,
        log: Option<u64>,
    ) -> Result<()> {
        self.validate()?;
        let mut count = 0;
        let mut total = 0u64;
        for (name, size, cap) in [
            ("Screenshot", screenshot, self.screenshot_bytes),
            ("Video", video, self.video_bytes),
            ("Diagnostic log", log, self.log_bytes),
        ] {
            if let Some(size) = size {
                ensure!(
                    size <= cap,
                    "{name} is {size} bytes; the configured limit is {cap} bytes ({}). Remove it or choose a smaller attachment before sending.",
                    human_size(cap)
                );
                count += 1;
                total = total
                    .checked_add(size)
                    .context("Attachment sizes overflowed")?;
            }
        }
        ensure!(
            count <= self.max_attachments,
            "This report has {count} attachments; the configured limit is {}. Remove an attachment before sending.",
            self.max_attachments
        );
        ensure!(
            total <= self.total_bytes,
            "Attachments total {total} bytes; the configured combined limit is {} bytes ({}). Remove an attachment or choose smaller files before sending.",
            self.total_bytes,
            human_size(self.total_bytes)
        );
        Ok(())
    }
    pub fn check_video(self, video: &Video) -> Result<()> {
        self.check_sizes(None, Some(video.size), None)?;
        let current = std::fs::metadata(&video.path).context(
            "The selected video is unavailable. Select it again or remove it before sending.",
        )?;
        ensure!(
            current.is_file(),
            "Choose a regular video file before sending."
        );
        self.check_sizes(None, Some(current.len()), None)?;
        ensure!(
            current.len() == video.size,
            "The selected video changed. Select it again or remove it before sending."
        );
        if let Some(maximum) = self.video_seconds {
            let duration = super::video::duration_seconds(&video.path)
                .context("Couldn't determine this video's duration. Choose an MP4/MOV/WebM with readable timing, or clear the duration limit before sending.")?;
            ensure!(
                duration <= maximum as f64,
                "Video is {duration:.2} seconds; the configured limit is {maximum} seconds. Trim it or choose another video before sending."
            );
        }
        Ok(())
    }
    pub fn check_report(self, report: &Report) -> Result<()> {
        ensure!(
            report.text.chars().count() <= 10_000 && !report.text.contains('\0'),
            "Feedback must contain at most 10,000 characters and no NUL characters."
        );
        self.check_sizes(
            report.screenshot.as_ref().map(|p| p.len() as u64),
            report.video.as_ref().map(|v| v.size),
            report.log.as_ref().map(|p| p.len() as u64),
        )?;
        if let Some(video) = &report.video {
            self.check_video(video)?;
        }
        Ok(())
    }
}
fn decimal_units(bytes: u64, unit: u64) -> String {
    let width = unit.ilog10() as usize;
    let whole = bytes / unit;
    let fraction = bytes % unit;
    if fraction == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{:0width$}", fraction)
            .trim_end_matches('0')
            .to_owned()
    }
}
fn parse_units(input: &str, unit: u64) -> Result<u64> {
    let (whole, fraction) = input.trim().split_once('.').unwrap_or((input.trim(), ""));
    ensure!(
        !whole.is_empty()
            && whole.bytes().all(|b| b.is_ascii_digit())
            && fraction.bytes().all(|b| b.is_ascii_digit())
            && fraction.len() <= unit.ilog10() as usize,
        "Enter a positive size in the displayed units, without an exponent or fractions of a byte."
    );
    let whole: u64 = whole.parse().context("Attachment limit is too large")?;
    let fraction: u64 = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<u64>()? * (unit / 10u64.pow(fraction.len() as u32))
    };
    whole
        .checked_mul(unit)
        .and_then(|n| n.checked_add(fraction))
        .context("Attachment limit is too large")
}

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
    pub identity: super::identity::Data,
    pub screenshot: Option<Arc<Vec<u8>>>,
    pub log: Option<String>,
    pub video: Option<Video>,
}

/// A user-chosen screen recording. Only the path is kept; the file is
/// streamed from disk on Send, so a 90 MB clip never sits in memory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Video {
    pub path: std::path::PathBuf,
    pub name: String,
    pub size: u64,
    #[serde(skip)]
    pub mime: &'static str,
    pub identity: VideoIdentity,
}
/// Content and filesystem identity captured at explicit selection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct VideoIdentity {
    digest: [u8; 32],
    modified: Option<std::time::SystemTime>,
    created: Option<std::time::SystemTime>,
    filesystem_file: Option<(u64, u64)>,
}
fn video_identity(file: &mut std::fs::File) -> Result<VideoIdentity> {
    use sha2::{Digest, Sha256};
    let before = file.metadata()?;
    ensure!(
        before.is_file() && before.len() > 0 && before.len() <= MAX_VIDEO,
        "The selected video is not a supported nonempty regular file. Select it again."
    );
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        ensure!(
            total <= MAX_VIDEO,
            "The selected video grew. Select it again."
        );
        digest.update(&buffer[..read]);
    }
    let after = file.metadata()?;
    ensure!(
        before.len() == total
            && after.len() == total
            && before.modified().ok() == after.modified().ok(),
        "The selected video changed. Select it again."
    );
    #[cfg(unix)]
    let filesystem_file = {
        use std::os::unix::fs::MetadataExt;
        Some((before.dev(), before.ino()))
    };
    #[cfg(windows)]
    let filesystem_file = {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: the File owns a live handle and information is writable.
        ensure!(
            unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } != 0,
            "Couldn't identify the selected video: {}",
            std::io::Error::last_os_error()
        );
        Some((
            u64::from(information.dwVolumeSerialNumber),
            (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
        ))
    };
    #[cfg(not(any(unix, windows)))]
    let filesystem_file = None;
    file.rewind()?;
    Ok(VideoIdentity {
        digest: digest.finalize().into(),
        modified: before.modified().ok(),
        created: before.created().ok(),
        filesystem_file,
    })
}
/// Freeze the verified selection before HTTP begins. An in-place source edit
/// during upload cannot change bytes already approved by the user.
fn selected_video_snapshot(video: &Video) -> Result<std::fs::File> {
    ensure!(
        std::fs::metadata(&video.path)?.is_file(),
        "The selected video is no longer a regular file. Select it again."
    );
    let mut source = std::fs::File::open(&video.path)
        .context("The selected video is unavailable. Select it again or remove it.")?;
    ensure!(
        video_identity(&mut source)? == video.identity,
        "The selected video changed or was replaced. Select it again or remove it."
    );
    let mut snapshot = tempfile::tempfile()?;
    std::io::copy(&mut source.take(MAX_VIDEO + 1), &mut snapshot)?;
    snapshot.rewind()?;
    let identity = video_identity(&mut snapshot)?;
    ensure!(
        identity.digest == video.identity.digest && snapshot.metadata()?.len() == video.size,
        "The selected video changed while preparing it. Select it again."
    );
    Ok(snapshot)
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
    ensure!(
        std::fs::metadata(path)
            .context("Couldn't read the video file.")?
            .is_file(),
        "Choose an MP4, MOV or WebM video file."
    );
    let mut file = std::fs::File::open(path).context("Couldn't read the video file.")?;
    let metadata = file.metadata()?;
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
        identity: video_identity(&mut file)?,
    })
}
pub(super) fn video_file_with_limits(path: &Path, limits: Limits) -> Result<Video> {
    limits.check_sizes(None, Some(std::fs::metadata(path)?.len()), None)?;
    let video = video_file(path)?;
    limits.check_video(&video)?;
    Ok(video)
}
impl Report {
    fn validate(&self) -> Result<()> {
        self.identity.validate()?;
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
            ensure!(
                video_file(&video.path)? == *video,
                "The selected video changed or was replaced. Select it again or remove it."
            );
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
    // Set from the successful local attempt; the server cannot supply this field.
    #[serde(skip)]
    pub delivery_endpoint: Option<String>,
}

/// Shared abort future spans both endpoints and the entire response body.
pub(super) type CancelSignal = futures_util::future::Shared<
    futures_util::future::BoxFuture<'static, Result<(), futures_util::future::Aborted>>,
>;

pub(super) fn send(report: Report) -> Result<Receipt> {
    send_using(ENDPOINT, FALLBACK_ENDPOINT, &report)
}
pub(super) fn send_cancellable(report: Report, cancel: CancelSignal) -> Result<Receipt> {
    send_using_cancellable(ENDPOINT, FALLBACK_ENDPOINT, &report, Some(cancel))
}
fn send_using(primary: &str, fallback: &str, report: &Report) -> Result<Receipt> {
    send_using_cancellable(primary, fallback, report, None)
}
fn send_using_cancellable(
    primary: &str,
    fallback: &str,
    report: &Report,
    cancel: Option<CancelSignal>,
) -> Result<Receipt> {
    okilum_http::primary_then_fallback(primary, fallback, |endpoint| {
        send_to_cancellable(endpoint, report, cancel.clone())
    })
}
#[cfg_attr(not(test), allow(dead_code, reason = "exercised only by tests"))]
fn send_to(endpoint: &str, report: &Report) -> Result<Receipt> {
    send_to_cancellable(endpoint, report, None)
}
fn send_to_cancellable(
    endpoint: &str,
    report: &Report,
    cancel: Option<CancelSignal>,
) -> Result<Receipt> {
    use futures_util::{
        FutureExt,
        future::{Either, select},
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("Create Feedback network runtime")?
        .block_on(async {
            let Some(cancel) = cancel else {
                return send_to_async(endpoint, report).await;
            };
            // Poll cancellation first: an already cancelled primary/fallback
            // must never open another connection. Dropping the losing request
            // future also drops the streaming file and response body.
            match select(cancel, send_to_async(endpoint, report).boxed()).await {
                Either::Left(_) => bail!("Sending cancelled. Your draft and attachments are kept. The server may already have received the report; check before retrying."),
                Either::Right((result, _)) => result,
            }
        })
}
async fn send_to_async(endpoint: &str, report: &Report) -> Result<Receipt> {
    use futures_util::TryStreamExt;
    use reqwest::multipart::{Form, Part};
    use tokio::io::AsyncReadExt;
    let attachment_error = Arc::new(std::sync::Mutex::new(None::<String>));
    report.validate()?;
    // A large video needs far longer than the text-only 45 s budget.
    let timeout = if report.video.is_some() {
        Duration::from_secs(15 * 60)
    } else {
        Duration::from_secs(45)
    };
    let failure = |error: anyhow::Error| {
        crate::reader_diagnostics::network_failure(&format!("Feedback via {endpoint}"), &error);
        let message = network_failure_message(endpoint, &okilum_http::reason(&error));
        error.context(message)
    };
    let client = okilum_http::async_client(endpoint, &report.meta.version, timeout)
        .map_err(|error| failure(okilum_http::transport_failure(error)))?;
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
        .text("install_id", report.identity.install_id.to_string())
        .text("meta", serde_json::to_string(&meta)?);
    if !report.identity.reporter_name.is_empty() {
        form = form.text("reporter_name", report.identity.reporter_name.clone());
    }
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
        let selected = video.clone();
        let file = tokio::task::spawn_blocking(move || selected_video_snapshot(&selected))
            .await
            .context("Prepare the selected video")??;
        let length = file.metadata()?.len();
        ensure!(
            length <= MAX_VIDEO,
            "Video is {}. Trim or compress it and attach again.",
            over_limit(length)
        );
        form = form.part(
            "video",
            Part::stream_with_length(
                reqwest::Body::wrap_stream(
                    tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file).take(length))
                        .map_err({
                            let attachment_error = attachment_error.clone();
                            move |error| {
                                *attachment_error.lock().unwrap_or_else(|e| e.into_inner()) =
                                    Some(error.to_string());
                                error
                            }
                        }),
                ),
                length,
            )
            .file_name(video.name.clone())
            .mime_str(video.mime)?,
        );
    }
    let response = client
        .post(endpoint)
        .multipart(form)
        .send()
        .await
        .map_err(|error| {
            let local = attachment_error
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            if let Some(local) = local {
                let error =
                    anyhow::Error::new(error).context(format!("Read the attached video: {local}"));
                crate::reader_diagnostics::network_failure(
                    &format!("Feedback via {endpoint}"),
                    &error,
                );
                error.context("Couldn't read the attached video. Your feedback is kept.")
            } else {
                failure(okilum_http::transport_failure(error.into()))
            }
        })?;
    let status = response.status();
    let bytes = bounded_response(response, 16 * 1024)
        .await
        .map_err(|error| {
            // A received HTTP status is authoritative. In particular, 201 may
            // already have created an issue: never retry an unreadable receipt.
            if status == reqwest::StatusCode::CREATED {
                let error = error.context("Feedback returned HTTP 201 with an unreadable receipt");
                crate::reader_diagnostics::network_failure(&format!("Feedback via {endpoint}"), &error);
                error.context("The server accepted your feedback, but its issue link could not be confirmed. Your draft is kept.")
            } else {
                let error = error.context(format!(
                    "Feedback returned HTTP {} with an unreadable body",
                    status.as_u16()
                ));
                crate::reader_diagnostics::network_failure(
                    &format!("Feedback via {endpoint}"),
                    &error,
                );
                error.context(network_failure_message(
                    endpoint,
                    &okilum_http::http_reason(status.as_u16()),
                ))
            }
        })?;
    if status != reqwest::StatusCode::CREATED {
        let error = anyhow::anyhow!("Feedback returned HTTP {}", status.as_u16());
        crate::reader_diagnostics::network_failure(&format!("Feedback via {endpoint}"), &error);
        if status.as_u16() == 407 {
            bail!(
                "{}",
                network_failure_message(endpoint, &okilum_http::http_reason(407))
            );
        }
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
        if status.as_u16() == 403 {
            bail!(
                "{}",
                network_failure_message(endpoint, &okilum_http::http_reason(403))
            );
        }
        bail!("{message} (HTTP {})", status.as_u16());
    }
    let mut receipt: Receipt = serde_json::from_slice(&bytes)
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
    receipt.delivery_endpoint = Some(endpoint.to_owned());
    crate::reader_diagnostics::feedback_delivery(endpoint);
    Ok(receipt)
}
/// Keep the actual cause visible and give the tester a next step even when
/// corporate filtering prevents Feedback itself from delivering diagnostics.
fn network_failure_message(endpoint: &str, reason: &str) -> String {
    let action = if reason.contains("HTTP 407") {
        "Ask IT for a proxy route that does not require integrated NTLM/Kerberos authentication."
    } else if reason.contains("TLS certificate") {
        if cfg!(windows) {
            "Ask IT to verify the corporate root CA in the Windows Trusted Root Certification Authorities store."
        } else {
            "Ask your network administrator to check the certificate chain."
        }
    } else if reason.contains("proxy") {
        if cfg!(windows) {
            "Check Windows Settings > Network & internet > Proxy, including the setup script (PAC), or contact IT."
        } else {
            "Check your proxy settings or contact your network administrator."
        }
    } else {
        "Check your connection and VPN, then retry. If it persists, ask IT to check HTTPS access to the Feedback host."
    };
    // Endpoints are app-owned, but never copy credentials/path/query into UI.
    let host = url::Url::parse(endpoint)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| "Feedback".into());
    let diagnostics = if cfg!(windows) {
        " Details: %LOCALAPPDATA%\\okilum\\reader-diagnostic.log."
    } else {
        " Details are in the app diagnostic log."
    };
    format!("{reason}. Your feedback is kept. {action} Host: {host}.{diagnostics}")
}
async fn bounded_response(mut response: reqwest::Response, maximum: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len().saturating_add(chunk.len()) <= maximum,
            "Attachment or response exceeds its size limit."
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
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

#[cfg_attr(not(test), allow(dead_code, reason = "exercised only by tests"))]
pub(super) fn log_tail(path: &Path) -> Result<String> {
    log_tail_with_limit(path, MAX_LOG)
}
pub(super) fn log_tail_with_limit(path: &Path, maximum: usize) -> Result<String> {
    ensure!(
        maximum > 0 && maximum <= MAX_LOG,
        "Invalid diagnostic log limit"
    );
    let mut file = std::fs::File::open(path).context(
        "Couldn't read the diagnostic log. Uncheck the log attachment to send without it.",
    )?;
    let length = file.metadata()?.len();
    let start = length.saturating_sub(maximum as u64);
    file.seek(SeekFrom::Start(start))?;
    let bytes = bounded(file.take(length - start), maximum)?;
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

#[cfg(test)]
mod limits_tests {
    use super::*;

    #[test]
    fn feedback1575_limits_config_roundtrip_refuses_server_ceiling_and_invalid_values() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences/feedback-limits.json");
        let defaults = Limits::default();
        assert_eq!(defaults.video_bytes, 90_000_000);
        assert_eq!(defaults.video_seconds, None);
        assert_eq!(Limits::from_fields(defaults.fields()).unwrap(), defaults);
        assert_eq!(Limits::load(&path).unwrap(), defaults);
        let custom = Limits {
            video_bytes: 12_000_000,
            video_seconds: Some(45),
            screenshot_bytes: 2_000_000,
            log_bytes: 4_000,
            total_bytes: 13_000_000,
            max_attachments: 2,
        };
        custom.save(&path).unwrap();
        assert_eq!(Limits::load(&path).unwrap(), custom);
        for invalid in [
            Limits {
                video_bytes: MAX_VIDEO + 1,
                ..custom
            },
            Limits {
                screenshot_bytes: MAX_IMAGE as u64 + 1,
                ..custom
            },
            Limits {
                log_bytes: MAX_LOG as u64 + 1,
                ..custom
            },
            Limits {
                total_bytes: defaults.total_bytes + 1,
                ..custom
            },
            Limits {
                video_seconds: Some(0),
                ..custom
            },
            Limits {
                max_attachments: 0,
                ..custom
            },
        ] {
            assert!(invalid.save(&path).is_err());
            assert_eq!(
                Limits::load(&path).unwrap(),
                custom,
                "invalid input cannot replace the saved policy"
            );
        }
        let mut fields = custom.fields();
        fields[0] = "1e9".into();
        assert!(Limits::from_fields(fields).is_err());
        std::fs::write(&path, br#"{"video_bytes":90000001}"#).unwrap();
        assert!(
            Limits::load(&path).is_err(),
            "invalid configuration must be visible, not silently accepted"
        );
    }

    #[test]
    fn feedback1575_limits_exact_bytes_count_and_combined_boundaries() {
        let limits = Limits {
            video_bytes: 32,
            screenshot_bytes: 8,
            log_bytes: 4,
            total_bytes: 40,
            max_attachments: 2,
            ..Limits::default()
        };
        assert!(limits.check_sizes(Some(8), Some(32), None).is_ok());
        assert!(
            limits
                .check_sizes(None, Some(33), None)
                .unwrap_err()
                .to_string()
                .contains("configured limit is 32 bytes")
        );
        assert!(limits.check_sizes(Some(9), None, None).is_err());
        assert!(limits.check_sizes(None, None, Some(5)).is_err());
        assert!(
            limits
                .check_sizes(Some(8), Some(32), Some(1))
                .unwrap_err()
                .to_string()
                .contains("3 attachments")
        );
        let combined = Limits {
            max_attachments: 3,
            ..limits
        };
        assert!(
            combined
                .check_sizes(Some(8), Some(32), Some(1))
                .unwrap_err()
                .to_string()
                .contains("combined limit")
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clip.webm");
        std::fs::write(&path, [0; 32]).unwrap();
        assert!(
            video_file_with_limits(&path, limits).is_ok(),
            "no duration cap does not invent a duration requirement"
        );
        std::fs::write(&path, [0; 33]).unwrap();
        assert!(video_file_with_limits(&path, limits).is_err());
    }

    #[test]
    fn feedback1575_limits_log_tail_and_report_preflight_use_configured_caps() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("diagnostics.log");
        std::fs::write(&path, b"old diagnostic line\nrecent\n").unwrap();
        assert_eq!(log_tail_with_limit(&path, 8).unwrap(), "recent");
        let mut report = Report {
            text: "Keep authored feedback".into(),
            kind: Kind::Bug,
            meta: tests::meta(),
            identity: super::super::identity::Data::anonymous(),
            screenshot: Some(Arc::new(vec![1; 8])),
            log: Some("log".into()),
            video: None,
        };
        let limits = Limits {
            screenshot_bytes: 8,
            log_bytes: 3,
            total_bytes: 11,
            max_attachments: 2,
            ..Limits::default()
        };
        assert!(limits.check_report(&report).is_ok());
        report.log = Some("four".into());
        assert!(limits.check_report(&report).is_err());
        report.log = None;
        report.text = "x".repeat(10_001);
        assert!(limits.check_report(&report).is_err());
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use futures_util::{FutureExt, future::AbortHandle};
    use std::net::TcpListener;

    fn signal() -> (AbortHandle, CancelSignal) {
        let (abort, registration) = AbortHandle::new_pair();
        (
            abort,
            futures_util::future::Abortable::new(std::future::pending(), registration)
                .boxed()
                .shared(),
        )
    }

    fn draft() -> Report {
        Report {
            text: "Keep this draft".into(),
            kind: Kind::Bug,
            meta: tests::meta(),
            identity: super::super::identity::Data::anonymous(),
            screenshot: Some(Arc::new(vec![1, 2, 3])),
            log: None,
            video: None,
        }
    }

    #[test]
    fn cancelled_before_send_contacts_neither_endpoint() {
        let primary = TcpListener::bind("127.0.0.1:0").unwrap();
        let fallback = TcpListener::bind("127.0.0.1:0").unwrap();
        let (abort, cancel) = signal();
        abort.abort();
        let error = send_using_cancellable(
            &format!("http://{}", primary.local_addr().unwrap()),
            &format!("http://{}", fallback.local_addr().unwrap()),
            &draft(),
            Some(cancel),
        )
        .unwrap_err();
        assert!(error.to_string().contains("Sending cancelled"));
        for listener in [primary, fallback] {
            listener.set_nonblocking(true).unwrap();
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }

    #[test]
    fn cancellation_drops_active_receipt_and_never_falls_back() {
        use std::io::Write;
        let primary = TcpListener::bind("127.0.0.1:0").unwrap();
        let fallback = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", primary.local_addr().unwrap());
        let alternate = format!("http://{}", fallback.local_addr().unwrap());
        let (abort, cancel) = signal();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = primary.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = [0; 4096];
            assert!(
                socket.read(&mut bytes).unwrap() > 0,
                "positive control: real request arrived"
            );
            socket
                .write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 1000\r\n\r\n{")
                .unwrap();
            abort.abort();
            // The body deliberately never completes. Cancellation must drop it.
            loop {
                match socket.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => break,
                    Err(error) => panic!("request was not closed: {error}"),
                }
            }
        });
        let report = draft();
        let error =
            send_using_cancellable(&endpoint, &alternate, &report, Some(cancel)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("server may already have received")
        );
        assert_eq!(report.text, "Keep this draft");
        assert_eq!(report.screenshot.as_deref().unwrap().as_slice(), &[1, 2, 3]);
        server.join().unwrap();
        fallback.set_nonblocking(true).unwrap();
        assert_eq!(
            fallback.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}
