//! Admission and bounded snapshots for transient media; never writes source files.
use std::io::{self, Read};

pub(super) const IMAGE_BYTES: usize = 8 * 1024 * 1024;
pub(super) const PDF_BYTES: usize = 16 * 1024 * 1024;
pub(super) const IMAGE_SIDE: u32 = 4096;
pub(super) const IMAGE_PIXELS: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Image,
    Pdf,
}

pub(super) fn kind(rel: &str) -> Option<Kind> {
    let ext = std::path::Path::new(rel)
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => Some(Kind::Image),
        "pdf" => Some(Kind::Pdf),
        _ => None,
    }
}

pub(super) fn snapshot(reader: impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::other("File exceeds the Quick Look byte limit."));
    }
    Ok(bytes)
}

pub(super) fn dimensions_fit(width: u32, height: u32) -> bool {
    width > 0
        && height > 0
        && width <= IMAGE_SIDE
        && height <= IMAGE_SIDE
        && u64::from(width) * u64::from(height) <= IMAGE_PIXELS
}
