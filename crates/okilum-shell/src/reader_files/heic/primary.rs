//! Show the primary image of a multi-image HEIF file (#2177).
//!
//! libheif's `heif-convert` writes every non-hidden top-level image, in item
//! order, as `preview-1.png`, `preview-2.png`, …; nothing in the output says
//! which one is the primary (`pitm`). Nokia's C007 (grid), C008 and C014
//! (`iden`) conformance files list their primary last, so the first output
//! was the wrong picture. The snapshot handed to the converter therefore gets
//! the `infe` hidden flag on every other item: libheif still uses hidden
//! items as grid tiles, `iden` sources, alpha planes and thumbnails, but
//! lists only the primary as top-level, so it writes exactly `preview.png`.
//!
//! Only the least significant `infe` flags byte changes; no box size or
//! offset moves. The parse is bounded by the snapshot; any malformed or
//! unexpected structure leaves the bytes untouched for libheif to judge.
//! `scripts/appimage/heif_boxes.py` is the Python twin used by the
//! packaged-tool controls and the independent pixel reference.

struct Child {
    kind: [u8; 4],
    payload: usize,
    end: usize,
}

fn be(data: &[u8], at: usize, len: usize) -> Option<u64> {
    let bytes = data.get(at..at.checked_add(len)?)?;
    Some(
        bytes
            .iter()
            .fold(0, |value, byte| (value << 8) | u64::from(*byte)),
    )
}

/// The child boxes in `data[start..end]`, or `None` if any box overruns.
fn children(data: &[u8], start: usize, end: usize) -> Option<Vec<Child>> {
    if end > data.len() || start > end {
        return None;
    }
    let mut found = Vec::new();
    let mut offset = start;
    while offset < end {
        let kind: [u8; 4] = data
            .get(offset + 4..offset.checked_add(8)?)?
            .try_into()
            .ok()?;
        let (size, header) = match be(data, offset, 4)? {
            0 => (end - offset, 8),
            1 => (usize::try_from(be(data, offset + 8, 8)?).ok()?, 16),
            size => (usize::try_from(size).ok()?, 8),
        };
        let box_end = offset.checked_add(size)?;
        if size < header || box_end > end {
            return None;
        }
        found.push(Child {
            kind,
            payload: offset + header,
            end: box_end,
        });
        offset = box_end;
    }
    Some(found)
}

fn child<'a>(boxes: &'a [Child], kind: &[u8; 4]) -> Option<&'a Child> {
    boxes.iter().find(|child| &child.kind == kind)
}

/// Offsets of the `infe` flags bytes to set; `None` keeps the file as is.
fn hidden_flag_offsets(source: &[u8]) -> Option<Vec<usize>> {
    let top = children(source, 0, source.len())?;
    let meta = child(&top, b"meta")?;
    // `meta`, `pitm` and `iinf` are FullBoxes: version (1) + flags (3).
    let meta = children(source, meta.payload.checked_add(4)?, meta.end)?;
    let pitm = child(&meta, b"pitm")?;
    let primary = match source.get(pitm.payload)? {
        0 => be(source, pitm.payload + 4, 2)?,
        _ => be(source, pitm.payload + 4, 4)?,
    };
    let iinf = child(&meta, b"iinf")?;
    let entries = iinf.payload
        + if *source.get(iinf.payload)? == 0 {
            6
        } else {
            8
        };
    let mut offsets = Vec::new();
    let mut primary_seen = false;
    for infe in children(source, entries, iinf.end)? {
        if &infe.kind != b"infe" || infe.payload + 4 > infe.end {
            continue;
        }
        let id = match source[infe.payload] {
            0 | 1 => continue, // no item type before version 2
            2 => be(source, infe.payload + 4, 2)?,
            _ => be(source, infe.payload + 4, 4)?,
        };
        if id == primary {
            primary_seen = true;
        } else {
            offsets.push(infe.payload + 3);
        }
    }
    primary_seen.then_some(offsets)
}

/// Mark every non-primary item hidden in place. Returns false, leaving the
/// bytes unchanged, when the item structure cannot be read.
pub(super) fn primary_only(source: &mut [u8]) -> bool {
    let Some(offsets) = hidden_flag_offsets(source) else {
        return false;
    };
    for offset in offsets {
        source[offset] |= 1;
    }
    true
}
