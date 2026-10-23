//! Remote images of a rich-text paste, downloaded into the attachment folder
//! (#1611). Blocking; run on the background executor only. Platform-neutral.
//!
//! The pasted text already shows each image as a remote `![alt](url)`; a
//! download that fails leaves it that way. Limits: 15 s per image, 50 MB, an
//! `image/*` response (or image bytes when the server does not say), a name
//! from the URL without separators or a leading dot, its extension from the
//! content type.

use super::attachments::{pasted_name, sniff_image};
use super::files::{self, sanitize_name, CopyError};
use std::io::Read;
use std::path::Path;
use std::time::Duration;

/// Whole request, connect to last byte.
const TIMEOUT: Duration = Duration::from_secs(15);
/// Largest image downloaded.
pub const MAX_BYTES: u64 = 50 * 1024 * 1024;
/// Downloads at the same time.
pub const PARALLEL: usize = 4;
/// Remote images downloaded for one paste; the rest stay remote.
pub const MAX_IMAGES: usize = 100;

#[derive(Debug, PartialEq, Eq)]
pub enum DownloadError {
    Request,
    Status(u16),
    NotAnImage,
    TooLarge,
    Save(String),
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request => write!(f, "request failed or timed out"),
            Self::Status(status) => write!(f, "HTTP {status}"),
            Self::NotAnImage => write!(f, "not an image"),
            Self::TooLarge => write!(f, "larger than 50 MB"),
            Self::Save(error) => write!(f, "not saved: {error}"),
        }
    }
}

/// Download the image at `url` into `folder` and return the saved file name.
pub fn save_remote_image(url: &str, folder: &Path, stamp: &str) -> Result<String, DownloadError> {
    let client = reqwest::blocking::Client::builder()
        .timeout(TIMEOUT)
        .connect_timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent(concat!("Okilum/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| DownloadError::Request)?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "image/*")
        .send()
        .map_err(|_| DownloadError::Request)?;
    let status = response.status();
    if !status.is_success() {
        return Err(DownloadError::Status(status.as_u16()));
    }
    if response.content_length().is_some_and(|len| len > MAX_BYTES) {
        return Err(DownloadError::TooLarge);
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase());
    let mut bytes = Vec::new();
    response
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| DownloadError::Request)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(DownloadError::TooLarge);
    }
    let extension = image_extension(content_type.as_deref(), &bytes)?;
    let name = file_name(url, extension, stamp);
    files::write_into(folder, &name, &bytes).map_err(|error| match error {
        CopyError::Folder(error) | CopyError::Read(error) | CopyError::Write(error) => {
            DownloadError::Save(error.to_string())
        }
        CopyError::NoFreeName => DownloadError::Save("no free name".into()),
    })
}

/// The file extension for a response: from its `image/*` content type, or
/// from the bytes when the server sent no type or a generic one.
pub fn image_extension(
    content_type: Option<&str>,
    bytes: &[u8],
) -> Result<&'static str, DownloadError> {
    let mime = content_type
        .and_then(|ct| ct.split(';').next())
        .map(str::trim)
        .unwrap_or("");
    let by_type = match mime {
        "image/png" | "image/apng" => Some("png"),
        "image/jpeg" | "image/jpg" | "image/pjpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/svg+xml" => Some("svg"),
        "image/avif" => Some("avif"),
        "image/bmp" | "image/x-ms-bmp" => Some("bmp"),
        "image/tiff" => Some("tiff"),
        "image/heic" | "image/heif" => Some("heic"),
        "image/x-icon" | "image/vnd.microsoft.icon" => Some("ico"),
        _ => None,
    };
    if let Some(extension) = by_type {
        return Ok(extension);
    }
    let generic = mime.is_empty()
        || mime == "application/octet-stream"
        || mime == "binary/octet-stream"
        || mime.starts_with("image/");
    if generic {
        if let Some(kind) = sniff_image(&bytes[..bytes.len().min(16)]) {
            return Ok(kind.extension());
        }
    }
    Err(DownloadError::NotAnImage)
}

/// A safe file name from the URL's last path segment with `extension`, or a
/// pasted image name when the URL has none worth keeping.
pub fn file_name(url: &str, extension: &str, stamp: &str) -> String {
    let segment = url::Url::parse(url)
        .ok()
        .and_then(|url| {
            url.path_segments()
                .and_then(|mut segments| segments.next_back().map(str::to_owned))
        })
        .unwrap_or_default();
    let decoded = percent_encoding::percent_decode_str(&segment)
        .decode_utf8_lossy()
        .into_owned();
    let mapped: String = decoded
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '\0' | '[' | ']' | '#' | '^' | '|' => '-',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    // The URL's own extension goes; the content type names the real one.
    let stem = match mapped.rfind('.') {
        Some(dot)
            if dot > 0
                && mapped.len() - dot <= 6
                && mapped[dot + 1..].chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            &mapped[..dot]
        }
        _ => mapped.as_str(),
    };
    let stem = stem.trim().trim_start_matches('.').trim();
    if stem.is_empty() {
        return pasted_name(stamp, 0, extension);
    }
    let stem: String = stem.chars().take(100).collect();
    sanitize_name(&format!("{stem}.{extension}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_come_from_the_content_type_or_the_bytes() {
        assert_eq!(image_extension(Some("image/png"), b""), Ok("png"));
        assert_eq!(
            image_extension(Some("image/jpeg; charset=binary"), b""),
            Ok("jpg")
        );
        assert_eq!(
            image_extension(Some("application/octet-stream"), b"GIF89a.."),
            Ok("gif")
        );
        assert_eq!(image_extension(None, b"\x89PNG\r\n\x1a\nxx"), Ok("png"));
        assert_eq!(
            image_extension(Some("text/html"), b"\x89PNG\r\n\x1a\n"),
            Err(DownloadError::NotAnImage)
        );
        assert_eq!(
            image_extension(None, b"<html>"),
            Err(DownloadError::NotAnImage)
        );
    }

    #[test]
    fn names_are_safe_and_take_the_real_extension() {
        assert_eq!(
            file_name("https://x.org/img/My%20Chart.PNG?w=200", "png", "1"),
            "My Chart.png"
        );
        assert_eq!(
            file_name("https://x.org/a/photo.php?id=3", "jpg", "1"),
            "photo.jpg"
        );
        assert_eq!(
            file_name("https://x.org/", "png", "20261023120000"),
            "Pasted image 20261023120000.png"
        );
        assert_eq!(
            file_name("https://x.org/..%2F..%2Fetc%2Fpasswd", "gif", "1"),
            "-..-etc-passwd.gif"
        );
        assert_eq!(
            file_name("https://x.org/.hidden.png", "png", "7"),
            "hidden.png"
        );
        let long = format!("https://x.org/{}.png", "a".repeat(300));
        assert!(file_name(&long, "png", "1").chars().count() <= 120);
    }
}
