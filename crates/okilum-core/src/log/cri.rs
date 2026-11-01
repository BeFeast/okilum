//! CRI envelopes and contiguous P/F fragments. Source ranges remain lossless.
use std::borrow::Cow;

use super::{
    record::{Escape, RawValue, ValueKind, DECODE_BYTES},
    timestamp, Record,
};

#[derive(Clone, Copy)]
pub(super) struct Header<'a> {
    time: &'a [u8],
    pub stream: &'a [u8],
    pub tag: &'a [u8],
    message: &'a [u8],
}

pub(super) fn header(line: &[u8]) -> Option<Header<'_>> {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let mut parts = line.splitn(4, |b| *b == b' ');
    let time = parts.next()?;
    let stream = parts.next()?;
    let tag = parts.next()?;
    let message = parts.next()?;
    // CRI requires a zoned RFC3339 timestamp, never an epoch or local time.
    if time.get(10) != Some(&b'T')
        || !(time.ends_with(b"Z") || time.get(19..)?.iter().any(|b| matches!(b, b'+' | b'-')))
        || timestamp::parse_text(time).is_none()
        || !matches!(stream, b"stdout" | b"stderr")
        || !matches!(tag, b"P" | b"F")
    {
        return None;
    }
    Some(Header {
        time,
        stream,
        tag,
        message,
    })
}

pub(super) fn scan<'a>(
    line: &'a [u8],
    visit: &mut dyn FnMut(&[Cow<'a, str>], RawValue<'a>),
) -> Result<(), ()> {
    let h = header(line).ok_or(())?;
    for (key, raw) in [
        ("time", h.time),
        ("stream", h.stream),
        ("tag", h.tag),
        ("msg", h.message),
    ] {
        visit(
            &[Cow::Borrowed(key)],
            RawValue {
                kind: ValueKind::String,
                raw,
                escape: Escape::None,
            },
        );
    }
    Ok(())
}

/// Only adjacent fragments of the same stream belong to one source range.
/// A stream switch or malformed envelope leaves the preceding P visible.
/// Unterminated EOF fragments likewise retain tag=P rather than claiming F.
pub(super) fn assemble(source: &[u8], record: &mut Record) -> Option<()> {
    let mut message = Vec::new();
    let mut stream = None;
    let mut partial = true;
    let mut tag = b"P".as_slice();
    for line in source.split(|b| *b == b'\n') {
        let h = header(line)?;
        if !partial || stream.is_some_and(|s| s != h.stream) {
            return None;
        }
        if message
            .len()
            .saturating_add(h.message.len())
            .saturating_mul(6)
            > DECODE_BYTES
        {
            return None;
        }
        stream = Some(h.stream);
        message.extend_from_slice(h.message);
        partial = h.tag == b"P";
        tag = h.tag;
    }
    let message = super::ansi::strip(&message);
    record.fields[3].value = String::from_utf8_lossy(&message).into_owned();
    record.fields[2].value = String::from_utf8_lossy(tag).into_owned();
    Some(())
}
