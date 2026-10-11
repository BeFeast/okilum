//! Format sniffing from the head of a file.

use super::{block, json, logfmt, text};

/// Bytes read for detection.
pub const SAMPLE_BYTES: usize = 64 * 1024;
/// Non-blank lines inspected for detection.
pub(super) const SAMPLE_LINES: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    JsonLines,
    Logfmt,
    /// Both JSON and logfmt records, each at least a tenth of the sample.
    Mixed,
    /// spdlog's default text pattern.
    Spdlog,
    /// glog's text lines and file header.
    Glog,
    /// Apache/nginx Common or Combined Log Format.
    AccessLog,
    /// syslog, RFC 5424 or the BSD form of RFC 3164.
    Syslog,
    /// Less than half the sample parses as records; every line is raw text.
    Plain,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::JsonLines => "JSON lines",
            Format::Logfmt => "logfmt",
            Format::Mixed => "JSON lines and logfmt",
            Format::Spdlog => "spdlog",
            Format::Glog => "glog",
            Format::AccessLog => "Access log",
            Format::Syslog => "syslog",
            Format::Plain => "Plain text",
        }
    }
}

/// The built-in text line formats, in the order a line is tried.
const TEXT: [Format; 4] = [
    Format::Spdlog,
    Format::Glog,
    Format::AccessLog,
    Format::Syslog,
];

/// Decides from the first lines of `data`. The verdict is a property of the
/// file, so a stray non-record line later on becomes an unparsed row instead
/// of flipping the format. A text format wins when it parses more of the
/// sample than JSON and logfmt together, and at least half of it. Stack
/// traces and crash reports after a line are not counted.
pub fn detect(data: &[u8]) -> Format {
    let mut sample = &data[..data.len().min(SAMPLE_BYTES)];
    if sample.len() < data.len() {
        // Never judge a line cut in half by the sample boundary; a first line
        // longer than the sample is read whole.
        let end = memchr::memrchr(b'\n', sample).unwrap_or_else(|| {
            memchr::memchr(b'\n', &data[sample.len()..]).map_or(data.len(), |at| sample.len() + at)
        });
        sample = &data[..end];
    }
    let (mut total, mut json_lines, mut logfmt_lines) = (0usize, 0usize, 0usize);
    let mut text_lines = [0usize; TEXT.len()];
    // Lines the index would attach to an entry (block.rs) say nothing about
    // the format: a JSON log that opens with a goroutine dump is JSON.
    let (mut seen, mut in_block) = (false, false);
    let mut rest = sample;
    while !rest.is_empty() {
        let end = memchr::memchr(b'\n', rest).unwrap_or(rest.len());
        let (raw, tail) = (&rest[..end], rest.get(end + 1..).unwrap_or_default());
        let lines = rest;
        rest = tail;
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let line = raw.trim_ascii();
        if line.is_empty() {
            continue;
        }
        let record = if json::scan(line, &mut |_, _| {}).is_ok() {
            json_lines += 1;
            true
        } else if let Some(at) = TEXT
            .iter()
            .position(|&format| text_scan(line, format).is_ok())
        {
            text_lines[at] += 1;
            true
        } else if logfmt::scan(line, &mut |_, _| {}).is_ok() {
            logfmt_lines += 1;
            true
        } else {
            false
        };
        if record {
            in_block = false;
        } else if (seen && (in_block || block::is_continuation(raw)))
            || block::block_kind(lines).is_some()
        {
            in_block |= block::block_kind(lines).is_some();
            continue;
        }
        seen = true;
        total += 1;
        if total == SAMPLE_LINES {
            break;
        }
    }
    // The first format wins a tie, so the verdict never depends on chance.
    let (text_format, text_count) =
        TEXT.iter()
            .zip(text_lines)
            .fold((Format::Plain, 0), |best, (&format, count)| {
                if count > best.1 {
                    (format, count)
                } else {
                    best
                }
            });
    if total > 0 && text_count > json_lines + logfmt_lines && text_count * 2 >= total {
        text_format
    } else if total == 0 || (json_lines + logfmt_lines) * 2 < total {
        Format::Plain
    } else if json_lines * 10 >= total && logfmt_lines * 10 >= total {
        Format::Mixed
    } else if json_lines >= logfmt_lines {
        Format::JsonLines
    } else {
        Format::Logfmt
    }
}

fn text_scan(line: &[u8], format: Format) -> Result<(), ()> {
    let visit = &mut |_: &[std::borrow::Cow<'_, str>], _| {};
    match format {
        Format::Spdlog => text::spdlog(line, visit),
        Format::Glog => text::glog(line, visit),
        Format::AccessLog => text::access(line, visit),
        Format::Syslog => text::syslog(line, visit),
        _ => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traces_do_not_count_against_the_format() {
        let mut data = b"{\"a\":1}\npanic: boom\n\n".to_vec();
        for i in 0..100 {
            data.extend_from_slice(format!("main.f{i}()\n\t/src/main.go:{i}\n").as_bytes());
        }
        data.extend_from_slice(b"{\"a\":2}\n\tat x\n\tat y\nnoise\n");
        assert_eq!(detect(&data), Format::JsonLines);
        // Without a record the same lines are plain text.
        assert_eq!(detect(b"one\n\ttwo\nthree\n"), Format::Plain);
    }

    #[test]
    fn verdicts() {
        assert_eq!(detect(b"{\"a\":1}\n{\"a\":2}\n"), Format::JsonLines);
        assert_eq!(detect(b"a=1 b=2\nc=3\n"), Format::Logfmt);
        assert_eq!(detect(b"{\"a\":1}\na=1\n"), Format::Mixed);
        assert_eq!(detect(b"hello world\nsecond line\n"), Format::Plain);
        assert_eq!(detect(b""), Format::Plain);
        // One stack trace line among records does not demote the file.
        assert_eq!(
            detect(b"{\"a\":1}\n  at main.rs:1\n{\"a\":2}\n"),
            Format::JsonLines
        );
        // CRLF endings.
        assert_eq!(detect(b"{\"a\":1}\r\n{\"a\":2}\r\n"), Format::JsonLines);
    }

    #[test]
    fn the_sample_boundary_never_cuts_a_line() {
        // A first record longer than the sample is judged whole.
        let long = format!("{{\"msg\":\"{}\"}}\n", "x".repeat(SAMPLE_BYTES * 2));
        let data = format!("{long}{long}");
        assert_eq!(detect(data.as_bytes()), Format::JsonLines);
        // Positive control: the same line truncated is not JSON.
        assert_eq!(detect(&long.as_bytes()[..SAMPLE_BYTES]), Format::Plain);
    }
}
