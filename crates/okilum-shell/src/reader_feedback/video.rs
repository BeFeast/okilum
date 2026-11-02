//! Completion-time selection check. Never replaces the authored selection with
//! the file currently occupying its path; only explicit attachment can do that.
use super::model;

/// Read bounded container timing only; never launch a player, codec or process.
pub(super) fn duration_seconds(path: &std::path::Path) -> anyhow::Result<f64> {
    use anyhow::{ensure, Context};
    let mut file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= model::MAX_VIDEO,
        "Choose a bounded regular video file"
    );
    let mut budget = 64 * 1024;
    let mut nodes = 0;
    let duration = if model::video_mime(path) == Some("video/webm") {
        webm_duration(&mut file, metadata.len(), &mut budget, &mut nodes)?
    } else {
        mp4_duration(&mut file, 0, metadata.len(), false, &mut budget, &mut nodes)?
            .context("The video has no readable movie duration")?
    };
    ensure!(
        duration.is_finite() && duration > 0.,
        "The video duration is unknown or invalid"
    );
    Ok(duration)
}

fn read_at(
    file: &mut std::fs::File,
    offset: u64,
    length: usize,
    budget: &mut usize,
) -> anyhow::Result<Vec<u8>> {
    use std::io::{Read, Seek};
    anyhow::ensure!(
        length <= *budget,
        "Video timing metadata exceeds the inspection limit"
    );
    *budget -= length;
    file.seek(std::io::SeekFrom::Start(offset))?;
    let mut bytes = vec![0; length];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn mp4_duration(
    file: &mut std::fs::File,
    mut offset: u64,
    end: u64,
    movie: bool,
    budget: &mut usize,
    nodes: &mut usize,
) -> anyhow::Result<Option<f64>> {
    while offset < end {
        *nodes += 1;
        anyhow::ensure!(
            *nodes <= 1024 && end - offset >= 8,
            "Invalid or excessive movie metadata"
        );
        let header = read_at(file, offset, 8, budget)?;
        let length = u32::from_be_bytes(header[..4].try_into()?) as u64;
        let (length, header_size) = match length {
            0 => (end - offset, 8),
            1 => {
                anyhow::ensure!(end - offset >= 16, "Truncated extended movie box");
                (
                    u64::from_be_bytes(read_at(file, offset + 8, 8, budget)?.try_into().unwrap()),
                    16,
                )
            }
            n => (n, 8),
        };
        anyhow::ensure!(
            length >= header_size && length <= end - offset,
            "Movie box exceeds its container"
        );
        let content = offset + header_size;
        if !movie && &header[4..] == b"moov" {
            if let Some(duration) =
                mp4_duration(file, content, offset + length, true, budget, nodes)?
            {
                return Ok(Some(duration));
            }
        } else if movie && &header[4..] == b"mvhd" {
            anyhow::ensure!(length - header_size >= 4, "Missing movie timing version");
            let version = read_at(file, content, 4, budget)?[0];
            let needed = match version {
                0 => 20,
                1 => 32,
                _ => anyhow::bail!("Unsupported movie timing version"),
            };
            anyhow::ensure!(
                length - header_size >= needed as u64,
                "Truncated movie timing"
            );
            let timing = read_at(file, content, needed, budget)?;
            let (scale, duration) = if version == 0 {
                let scale = u32::from_be_bytes(timing[12..16].try_into()?);
                let duration = u32::from_be_bytes(timing[16..20].try_into()?);
                anyhow::ensure!(duration != u32::MAX, "Unknown movie duration");
                (scale, duration as u64)
            } else {
                let scale = u32::from_be_bytes(timing[20..24].try_into()?);
                let duration = u64::from_be_bytes(timing[24..32].try_into()?);
                anyhow::ensure!(duration != u64::MAX, "Unknown movie duration");
                (scale, duration)
            };
            anyhow::ensure!(scale > 0 && duration > 0, "Unknown movie duration");
            return Ok(Some(duration as f64 / scale as f64));
        }
        offset += length;
    }
    Ok(None)
}

fn vint(
    file: &mut std::fs::File,
    offset: &mut u64,
    end: u64,
    id: bool,
    budget: &mut usize,
) -> anyhow::Result<(u64, bool)> {
    anyhow::ensure!(*offset < end, "Truncated WebM metadata");
    let first = read_at(file, *offset, 1, budget)?[0];
    let width = first.leading_zeros() as usize + 1;
    anyhow::ensure!(
        width <= if id { 4 } else { 8 },
        "Invalid WebM element header"
    );
    anyhow::ensure!(
        width as u64 <= end - *offset,
        "Truncated WebM element header"
    );
    let mut value = if id {
        first as u64
    } else {
        (first & (0x7f >> (width - 1))) as u64
    };
    if width > 1 {
        for byte in read_at(file, *offset + 1, width - 1, budget)? {
            value = (value << 8) | byte as u64;
        }
    }
    *offset += width as u64;
    let unknown = !id && value == (1u64 << (7 * width)) - 1;
    Ok((value, unknown))
}

fn webm_duration(
    file: &mut std::fs::File,
    end: u64,
    budget: &mut usize,
    nodes: &mut usize,
) -> anyhow::Result<f64> {
    fn scan(
        file: &mut std::fs::File,
        mut offset: u64,
        end: u64,
        level: u8,
        budget: &mut usize,
        nodes: &mut usize,
    ) -> anyhow::Result<Option<f64>> {
        let mut scale = 1_000_000u64;
        let mut duration = None;
        while offset < end {
            *nodes += 1;
            anyhow::ensure!(*nodes <= 1024, "Excessive WebM metadata");
            let (id, _) = vint(file, &mut offset, end, true, budget)?;
            let (length, unknown) = vint(file, &mut offset, end, false, budget)?;
            anyhow::ensure!(
                !unknown || id == 0x18538067,
                "Unknown WebM timing element size"
            );
            let length = if unknown { end - offset } else { length };
            anyhow::ensure!(length <= end - offset, "WebM element exceeds its container");
            if (level == 0 && id == 0x18538067) || (level == 1 && id == 0x1549a966) {
                if let Some(value) = scan(file, offset, offset + length, level + 1, budget, nodes)?
                {
                    return Ok(Some(value));
                }
            } else if level == 2 && id == 0x2ad7b1 {
                anyhow::ensure!((1..=8).contains(&length), "Invalid WebM time scale");
                scale = read_at(file, offset, length as usize, budget)?
                    .iter()
                    .fold(0, |n, b| (n << 8) | *b as u64);
            } else if level == 2 && id == 0x4489 {
                duration = Some(match length {
                    4 => f32::from_be_bytes(read_at(file, offset, 4, budget)?.try_into().unwrap())
                        as f64,
                    8 => f64::from_be_bytes(read_at(file, offset, 8, budget)?.try_into().unwrap()),
                    _ => anyhow::bail!("Invalid WebM duration"),
                });
            }
            offset += length;
        }
        anyhow::ensure!(scale > 0, "Invalid WebM time scale");
        Ok(duration.map(|duration| duration * scale as f64 / 1_000_000_000.))
    }
    scan(file, 0, end, 0, budget, nodes)?
        .ok_or_else(|| anyhow::anyhow!("The video has no readable WebM duration"))
}

pub(super) fn recovery_error(selected: Option<&model::Video>) -> Option<String> {
    let selected = selected?;
    match model::video_file(&selected.path) {
        Ok(current) if current == *selected => None,
        _ => Some(
            "The selected video changed, was replaced or is unavailable. Your draft and original selection are kept. Select the video again or remove it before retrying."
                .into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback1575_limits_mp4_mov_duration_versions_and_malformed_timing() {
        fn movie(version: u8, ticks: u64) -> Vec<u8> {
            let mut timing = vec![0; if version == 0 { 20 } else { 32 }];
            timing[0] = version;
            if version == 0 {
                timing[12..16].copy_from_slice(&1000u32.to_be_bytes());
                timing[16..20].copy_from_slice(&(ticks as u32).to_be_bytes());
            } else {
                timing[20..24].copy_from_slice(&1000u32.to_be_bytes());
                timing[24..32].copy_from_slice(&ticks.to_be_bytes());
            }
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&((timing.len() + 16) as u32).to_be_bytes());
            bytes.extend_from_slice(b"moov");
            bytes.extend_from_slice(&((timing.len() + 8) as u32).to_be_bytes());
            bytes.extend_from_slice(b"mvhd");
            bytes.extend(timing);
            bytes
        }
        let directory = tempfile::tempdir().unwrap();
        for (version, name) in [(0, "clip.mp4"), (1, "clip.mov")] {
            let path = directory.path().join(name);
            std::fs::write(&path, movie(version, 12_500)).unwrap();
            assert_eq!(duration_seconds(&path).unwrap(), 12.5);
            let limits = model::Limits {
                video_seconds: Some(12),
                ..Default::default()
            };
            assert!(model::video_file_with_limits(&path, limits)
                .unwrap_err()
                .to_string()
                .contains("configured limit is 12 seconds"));
            assert!(model::video_file_with_limits(
                &path,
                model::Limits {
                    video_seconds: Some(13),
                    ..limits
                }
            )
            .is_ok());
            std::fs::write(
                &path,
                movie(
                    version,
                    if version == 0 {
                        u32::MAX as u64
                    } else {
                        u64::MAX
                    },
                ),
            )
            .unwrap();
            assert!(
                duration_seconds(&path).is_err(),
                "unknown duration must not pass an explicit duration cap"
            );
            std::fs::write(&path, [0, 0, 0, 7, b'm', b'o', b'o', b'v']).unwrap();
            assert!(duration_seconds(&path).is_err());
        }
    }

    #[test]
    fn feedback1575_limits_webm_scale_unknown_segment_and_invalid_duration() {
        fn element(id: &[u8], data: &[u8]) -> Vec<u8> {
            assert!(data.len() < 127);
            [id, &[0x80 | data.len() as u8], data].concat()
        }
        fn webm(duration: f64, scale: u32) -> Vec<u8> {
            let mut timing = element(&[0x44, 0x89], &duration.to_be_bytes());
            timing.extend(element(&[0x2a, 0xd7, 0xb1], &scale.to_be_bytes()));
            let info = element(&[0x15, 0x49, 0xa9, 0x66], &timing);
            // Eight-byte unknown Segment size is common in streaming WebM.
            [
                &[
                    0x1a, 0x45, 0xdf, 0xa3, 0x80, 0x18, 0x53, 0x80, 0x67, 0x01, 0xff, 0xff, 0xff,
                    0xff, 0xff, 0xff, 0xff,
                ][..],
                &info,
            ]
            .concat()
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clip.webm");
        for (raw, scale, seconds) in [(12_500., 1_000_000, 12.5), (25., 1_000_000_000, 25.)] {
            std::fs::write(&path, webm(raw, scale)).unwrap();
            assert_eq!(duration_seconds(&path).unwrap(), seconds);
        }
        for bytes in [
            webm(f64::NAN, 1_000_000),
            webm(100., 0),
            vec![0],
            vec![0x18, 0x53, 0x80, 0x67, 0xff],
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert!(duration_seconds(&path).is_err());
        }
    }

    #[test]
    fn failed_completion_retains_selection_and_refuses_changed_asset_until_reselected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recording.webm");
        std::fs::write(&path, b"original").unwrap();
        let selected = model::video_file(&path).unwrap();
        assert!(
            recovery_error(Some(&selected)).is_none(),
            "unchanged selection positive control"
        );
        let timestamp = std::fs::metadata(&path).unwrap().modified().unwrap();
        // Simulate an edit while the provider is running, even at the same
        // length and restored timestamp: the original bytes remain selected.
        std::fs::write(&path, b"replaced").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(timestamp))
            .unwrap();
        assert!(recovery_error(Some(&selected))
            .unwrap()
            .contains("before retrying"));
        assert_ne!(model::video_file(&path).unwrap(), selected);
        let explicitly_reselected = model::video_file(&path).unwrap();
        assert!(recovery_error(Some(&explicitly_reselected)).is_none());
        std::fs::remove_file(&path).unwrap();
        assert!(recovery_error(Some(&selected)).is_some());
        assert!(recovery_error(None).is_none(), "explicit remove recovers");
    }
}
