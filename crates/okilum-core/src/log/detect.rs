//! Format sniffing from the head of a file.

use super::{json, logfmt};

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
    /// Less than half the sample parses as records; every line is raw text.
    Plain,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::JsonLines => "JSON lines",
            Format::Logfmt => "logfmt",
            Format::Mixed => "JSON lines and logfmt",
            Format::Plain => "Plain text",
        }
    }
}

/// Decides from the first lines of `data`. The verdict is a property of the
/// file, so a stray non-record line later on becomes an unparsed row instead
/// of flipping the format.
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
    for line in sample.split(|&b| b == b'\n') {
        let line = line.trim_ascii();
        if line.is_empty() {
            continue;
        }
        total += 1;
        if json::scan(line, &mut |_, _| {}).is_ok() {
            json_lines += 1;
        } else if logfmt::scan(line, &mut |_, _| {}).is_ok() {
            logfmt_lines += 1;
        }
        if total == SAMPLE_LINES {
            break;
        }
    }
    if total == 0 || (json_lines + logfmt_lines) * 2 < total {
        Format::Plain
    } else if json_lines * 10 >= total && logfmt_lines * 10 >= total {
        Format::Mixed
    } else if json_lines >= logfmt_lines {
        Format::JsonLines
    } else {
        Format::Logfmt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
