//! Global image import policy. Encoding and publication run on a background
//! executor; an embed is returned only after every requested file is durable.
use super::{attachments, files};
use image::ImageDecoder;
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub enabled: bool,
    pub quality: u32,
    pub keep_original: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            enabled: true,
            quality: 85,
            keep_original: false,
        }
    }
}

impl Options {
    pub fn normalized(self) -> Self {
        Self {
            quality: self.quality.clamp(1, 100),
            ..self
        }
    }

    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }
}

#[derive(Debug)]
pub struct Imported {
    pub name: String,
    /// Only files created by this operation, including the retained original.
    pub paths: Vec<PathBuf>,
}

const MAX_DECODE_BYTES: u64 = 128 * 1024 * 1024;

/// Already-WebP, animation, vectors and unsupported formats stay lossless.
/// In particular, never silently flatten an animation or a multi-page TIFF.
fn convertible(bytes: &[u8]) -> Result<Option<image::ImageFormat>, String> {
    let Ok(format) = image::guess_format(bytes) else {
        return Ok(None);
    };
    match format {
        image::ImageFormat::Jpeg | image::ImageFormat::Bmp => Ok(Some(format)),
        image::ImageFormat::Png => {
            let decoder = image::codecs::png::PngDecoder::new(Cursor::new(bytes))
                .map_err(|e| format!("The image could not be read: {e}"))?;
            Ok((!decoder.is_apng().map_err(|e| e.to_string())?).then_some(format))
        }
        image::ImageFormat::Tiff => {
            // Classic TIFF's first IFD ends with the next-page offset. Keep
            // unknown/BigTIFF layouts and multi-page documents unchanged.
            let little = bytes.starts_with(b"II*\0");
            let big = bytes.starts_with(b"MM\0*");
            if !little && !big {
                return Ok(None);
            }
            let u16_at = |at: usize| -> Option<u16> {
                let b = bytes.get(at..at.checked_add(2)?)?.try_into().ok()?;
                Some(if little {
                    u16::from_le_bytes(b)
                } else {
                    u16::from_be_bytes(b)
                })
            };
            let u32_at = |at: usize| -> Option<u32> {
                let b = bytes.get(at..at.checked_add(4)?)?.try_into().ok()?;
                Some(if little {
                    u32::from_le_bytes(b)
                } else {
                    u32::from_be_bytes(b)
                })
            };
            let single = (|| {
                let at = usize::try_from(u32_at(4)?).ok()?;
                let count = usize::from(u16_at(at)?);
                u32_at(at.checked_add(2)?.checked_add(count.checked_mul(12)?)?)
            })() == Some(0);
            Ok(single.then_some(format))
        }
        _ => Ok(None),
    }
}

fn encode(bytes: &[u8], format: image::ImageFormat, quality: u32) -> Result<Vec<u8>, String> {
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16383);
    limits.max_image_height = Some(16383);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) * 4 > MAX_DECODE_BYTES {
        return Err("The image is too large to convert safely.".into());
    }
    let orientation = decoder.orientation().map_err(|e| e.to_string())?;
    let mut decoded = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    decoded.apply_orientation(orientation);
    let rgba = decoded.to_rgba8();
    let mut config = webp::WebPConfig::new().map_err(|()| "WebP encoding is unavailable.")?;
    config.quality = quality.clamp(1, 100) as f32;
    config.alpha_quality = 100;
    webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height())
        .encode_advanced(&config)
        .map(|bytes| bytes.to_vec())
        .map_err(|e| format!("The image could not be converted to WebP: {e:?}"))
}

/// The incoming attachment is replaced by WebP, never an existing vault file
/// or the external source. Undo removes text only and retains these files.
pub fn save(
    folder: &Path,
    wanted: &str,
    bytes: &[u8],
    options: Options,
) -> Result<Imported, String> {
    if bytes.len() > super::offer::MAX_IMAGE_BYTES {
        return Err("The image is too large to import (64 MB at most).".into());
    }
    let options = options.normalized();
    if options.enabled
        && image::guess_format(bytes).is_err()
        && Path::new(wanted)
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|s| {
                matches!(
                    s.to_ascii_lowercase().as_str(),
                    "png" | "jpg" | "jpeg" | "bmp" | "tif" | "tiff"
                )
            })
    {
        return Err("The image could not be read, so nothing was inserted.".into());
    }
    let format = if options.enabled {
        convertible(bytes)?
    } else {
        None
    };
    let converted = format
        .map(|f| encode(bytes, f, options.quality))
        .transpose()?;
    let wanted = files::sanitize_name(wanted);
    let output_name = if converted.is_some() {
        Path::new(&wanted)
            .with_extension("webp")
            .to_string_lossy()
            .into_owned()
    } else {
        wanted.clone()
    };
    let mut paths = Vec::new();
    if converted.is_some() && options.keep_original {
        let original = Path::new(&wanted);
        let stem = original.file_stem().unwrap_or_default().to_string_lossy();
        let mut retained_name = format!("{stem}-original");
        if let Some(ext) = original.extension().filter(|ext| !ext.is_empty()) {
            retained_name.push('.');
            retained_name.push_str(&ext.to_string_lossy());
        }
        let name =
            files::write_into(folder, &retained_name, bytes).map_err(|e| e.user_text(&wanted))?;
        paths.push(folder.join(name));
    }
    match files::write_into(folder, &output_name, converted.as_deref().unwrap_or(bytes)) {
        Ok(name) => {
            paths.push(folder.join(&name));
            Ok(Imported { name, paths })
        }
        Err(error) => {
            for path in paths {
                let _ = std::fs::remove_file(path);
            }
            Err(error.user_text(&output_name))
        }
    }
}

/// `None` for a non-image; ordinary file copies keep their streaming path.
pub fn import_file(
    source: &Path,
    folder: &Path,
    wanted: &str,
    options: Options,
) -> Result<Option<Imported>, String> {
    let extension = Path::new(wanted)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    if !options.enabled || !attachments::is_image_extension(extension) {
        return Ok(None);
    }
    let file = std::fs::File::open(source).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(super::offer::MAX_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    save(folder, wanted, &bytes, options).map(Some)
}

#[cfg(test)]
pub(crate) fn fixture_png() -> Vec<u8> {
    let pixels = image::RgbaImage::from_fn(32, 16, |x, y| {
        image::Rgba([
            (x * 7) as u8,
            (y * 13) as u8,
            100,
            if x < 8 { 0 } else { 255 },
        ])
    });
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(pixels)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webp_import_preserves_dimensions_alpha_and_applies_quality() {
        let temp = tempfile::tempdir().unwrap();
        let png = fixture_png();
        let first = save(temp.path(), "picture.png", &png, Options::default()).unwrap();
        assert_eq!(first.name, "picture.webp");
        assert_eq!(first.paths.len(), 1);
        let bytes = std::fs::read(&first.paths[0]).unwrap();
        assert_eq!(
            image::guess_format(&bytes).unwrap(),
            image::ImageFormat::WebP
        );
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (32, 16));
        assert_eq!(decoded.get_pixel(0, 0)[3], 0);
        assert_eq!(decoded.get_pixel(31, 15)[3], 255);
        assert!(!temp.path().join("picture.png").exists());
        assert_eq!(bytes, encode(&png, image::ImageFormat::Png, 85).unwrap());
        assert_ne!(bytes, encode(&png, image::ImageFormat::Png, 20).unwrap());
    }

    #[test]
    fn retention_collisions_and_disabled_conversion_never_replace_existing_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let png = fixture_png();
        let options = Options {
            keep_original: true,
            ..Default::default()
        };
        std::fs::write(temp.path().join("picture.webp"), b"existing attachment").unwrap();
        std::fs::write(
            temp.path().join("picture-original.png"),
            b"existing original",
        )
        .unwrap();
        let saved = save(temp.path(), "picture.png", &png, options).unwrap();
        assert_eq!(saved.name, "picture-1.webp");
        assert_eq!(saved.paths.len(), 2);
        assert_eq!(std::fs::read(&saved.paths[0]).unwrap(), png);
        assert_eq!(
            std::fs::read(temp.path().join("picture.webp")).unwrap(),
            b"existing attachment"
        );
        assert_eq!(
            std::fs::read(temp.path().join("picture-original.png")).unwrap(),
            b"existing original"
        );
        let untouched = save(temp.path(), "off.png", &png, Options::disabled()).unwrap();
        assert_eq!(untouched.name, "off.png");
        assert_eq!(std::fs::read(&untouched.paths[0]).unwrap(), png);
    }

    #[test]
    fn retained_extensionless_images_have_portable_names_and_preserve_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let png = fixture_png();
        let options = Options {
            keep_original: true,
            ..Default::default()
        };
        for wanted in ["picture", "picture."] {
            let folder = temp.path().join(wanted.trim_end_matches('.'));
            let saved = save(&folder, wanted, &png, options).unwrap();
            assert_eq!(saved.paths.len(), 2);
            assert!(!saved.paths[0]
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with('.'));
            assert_eq!(std::fs::read(&saved.paths[0]).unwrap(), png);
            let image = image::open(saved.paths.last().unwrap()).unwrap();
            assert_eq!((image.width(), image.height()), (32, 16));
        }
        assert_eq!(
            std::fs::read(temp.path().join("picture/picture-original")).unwrap(),
            png
        );
        assert_eq!(
            std::fs::read(temp.path().join("picture/picture-original-1")).unwrap(),
            png
        );
    }

    #[test]
    fn imports_preserve_the_external_source_and_conversion_failure_publishes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.png");
        let png = fixture_png();
        std::fs::write(&source, &png).unwrap();
        let before = std::fs::metadata(&source).unwrap().modified().unwrap();
        let folder = temp.path().join("vault/attachments");
        let saved = import_file(&source, &folder, "source.png", Options::default())
            .unwrap()
            .unwrap();
        assert_eq!(saved.name, "source.webp");
        assert_eq!(std::fs::read(&source).unwrap(), png);
        assert_eq!(
            std::fs::metadata(&source).unwrap().modified().unwrap(),
            before
        );
        let failed = temp.path().join("failed");
        assert!(save(
            &failed,
            "bad.png",
            b"\x89PNG\r\n\x1a\nbroken",
            Options::default()
        )
        .is_err());
        assert!(!failed.exists());
        let blocker = temp.path().join("not-a-folder");
        std::fs::write(&blocker, b"keep").unwrap();
        assert!(save(&blocker, "image.png", &png, Options::default()).is_err());
        assert_eq!(std::fs::read(blocker).unwrap(), b"keep");
    }

    #[test]
    fn jpeg_orientation_is_applied_before_webp_encoding() {
        let temp = tempfile::tempdir().unwrap();
        let mut jpeg = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(12, 6)
            .write_to(&mut jpeg, image::ImageFormat::Jpeg)
            .unwrap();
        // APP1 Exif, little-endian IFD with Orientation=6 (90 degrees).
        let exif = b"Exif\0\0II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xe1];
        bytes.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        bytes.extend_from_slice(exif);
        bytes.extend_from_slice(&jpeg.into_inner()[2..]);
        let saved = save(temp.path(), "photo.jpg", &bytes, Options::default()).unwrap();
        let decoded = image::open(&saved.paths[0]).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (6, 12));
    }

    #[test]
    fn vector_and_animated_formats_are_not_flattened() {
        let temp = tempfile::tempdir().unwrap();
        for (name, bytes) in [
            ("vector.svg", b"<svg/>".as_slice()),
            ("animation.gif", b"GIF89a\x01\0\x01\0".as_slice()),
        ] {
            let saved = save(temp.path(), name, bytes, Options::default()).unwrap();
            assert_eq!(saved.name, name);
            assert_eq!(std::fs::read(saved.paths.last().unwrap()).unwrap(), bytes);
        }
        let webp = encode(&fixture_png(), image::ImageFormat::Png, 85).unwrap();
        let saved = save(temp.path(), "existing.webp", &webp, Options::default()).unwrap();
        assert_eq!(std::fs::read(&saved.paths[0]).unwrap(), webp);
    }
}
