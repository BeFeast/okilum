//! Transactional byte cursor: failed responses never consume bytes.
use std::time::Duration;

pub const CHUNK_BYTES: u64 = 1024 * 1024;
const MAX_FAILURES: u8 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Connected,
    Disconnected { retry_after: Duration },
    Stopped(Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    RangeUnsupported,
    InvalidRange,
    SourceChanged,
    Http(u16),
    RetryExhausted,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RangeUnsupported => {
                f.write_str("Server does not support HTTP Range; remote follow stopped")
            }
            Self::InvalidRange => f.write_str("Invalid HTTP Range response; cursor retained"),
            Self::SourceChanged => f.write_str("Remote log changed or shrank; reopen explicitly"),
            Self::Http(code) => write!(f, "Remote log rejected the request (HTTP {code})"),
            Self::RetryExhausted => {
                f.write_str("Remote log disconnected; reconnect limit reached; reopen to retry")
            }
        }
    }
}
impl std::error::Error for Error {}

pub struct Cursor {
    pub offset: u64,
    pub status: Status,
    failures: u8,
    /// Suffix length of the first request when starting near the end.
    tail: Option<u64>,
    /// File offset of the first shown byte; 0 when read from the start.
    pub started_at: u64,
    /// Bytes up to the first newline belong to a line cut by the tail start.
    pub skip_line: bool,
}
impl Default for Cursor {
    fn default() -> Self {
        Self {
            offset: 0,
            status: Status::Connected,
            failures: 0,
            tail: None,
            started_at: 0,
            skip_line: false,
        }
    }
}
impl Cursor {
    /// Start at the first full line of the last `bytes` of the file. One byte
    /// more is requested: if it is a newline, the first line is complete.
    pub fn from_tail(bytes: u64) -> Self {
        Self {
            tail: Some(bytes.min(CHUNK_BYTES - 1) + 1),
            ..Self::default()
        }
    }
    pub fn range(&self) -> String {
        if let Some(tail) = self.tail {
            return format!("bytes=-{tail}");
        }
        format!(
            "bytes={}-{}",
            self.offset,
            self.offset.saturating_add(CHUNK_BYTES - 1)
        )
    }
    pub fn disconnected(&mut self) {
        self.failures += 1;
        self.status = if self.failures >= MAX_FAILURES {
            Status::Stopped(Error::RetryExhausted)
        } else {
            Status::Disconnected {
                retry_after: Duration::from_secs(1 << (self.failures - 1)),
            }
        };
    }
    /// Validate headers before reading even one byte of an unsupported full response.
    /// An accepted suffix response moves the cursor to its start.
    pub fn expected(&mut self, status: u16, range: Option<&str>) -> Result<u64, Error> {
        if status == 200 {
            return Err(Error::RangeUnsupported);
        }
        let range = range.unwrap_or("");
        if let Some(tail) = self.tail {
            return self.suffix(tail, status, range);
        }
        if status == 416 {
            let total = range
                .strip_prefix("bytes */")
                .and_then(|s| s.parse::<u64>().ok())
                .ok_or(Error::InvalidRange)?;
            return if total == self.offset {
                Ok(0)
            } else {
                Err(Error::SourceChanged)
            };
        }
        if status != 206 {
            return Err(Error::Http(status));
        }
        let (start, end, _) = parse_content_range(range)?;
        if start != self.offset {
            return Err(Error::InvalidRange);
        }
        Ok(end - start + 1)
    }
    fn suffix(&mut self, tail: u64, status: u16, range: &str) -> Result<u64, Error> {
        let (start, length) = match status {
            // An empty file cannot satisfy a suffix range.
            416 if range == "bytes */0" => (0, 0),
            416 => return Err(Error::InvalidRange),
            206 => {
                let (start, end, total) = parse_content_range(range)?;
                if end + 1 != total || start != total.saturating_sub(tail) {
                    return Err(Error::InvalidRange);
                }
                (start, end - start + 1)
            }
            _ => return Err(Error::Http(status)),
        };
        self.tail = None;
        self.offset = start;
        self.skip_line = start > 0;
        self.started_at = if start > 0 { start + 1 } else { 0 };
        Ok(length)
    }
    pub fn commit(&mut self, bytes: u64) {
        self.offset += bytes;
        self.failures = 0;
        self.status = Status::Connected;
    }
}

/// `bytes start-end/total` with `start <= end < total`, at most one chunk.
fn parse_content_range(range: &str) -> Result<(u64, u64, u64), Error> {
    let (span, total) = range
        .strip_prefix("bytes ")
        .and_then(|s| s.split_once('/'))
        .ok_or(Error::InvalidRange)?;
    let (start, end) = span.split_once('-').ok_or(Error::InvalidRange)?;
    let start = start.parse::<u64>().map_err(|_| Error::InvalidRange)?;
    let end = end.parse::<u64>().map_err(|_| Error::InvalidRange)?;
    let total = total.parse::<u64>().map_err(|_| Error::InvalidRange)?;
    if end < start || end >= total || end - start >= CHUNK_BYTES {
        return Err(Error::InvalidRange);
    }
    Ok((start, end, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interrupted_response_retries_exact_cursor_and_recovers() {
        let mut c = Cursor::default();
        assert_eq!(c.expected(206, Some("bytes 0-3/4")), Ok(4));
        c.commit(4);
        c.disconnected(); // An interrupted body is never committed.
        assert!(matches!(c.status, Status::Disconnected { .. }));
        assert_eq!(c.range(), "bytes=4-1048579");
        assert_eq!(c.expected(206, Some("bytes 4-7/8")), Ok(4));
        c.commit(4);
        assert_eq!(c.offset, 8);
        assert_eq!(c.status, Status::Connected);
        assert_eq!(c.expected(416, Some("bytes */8")), Ok(0));
        assert_eq!(
            c.expected(416, Some("bytes */7")),
            Err(Error::SourceChanged)
        );
    }
    #[test]
    fn refuses_full_replays_gaps_and_malformed_ranges() {
        let mut c = Cursor::default();
        c.commit(4);
        assert_eq!(c.expected(200, None), Err(Error::RangeUnsupported));
        for range in [
            "bytes 0-7/8",
            "bytes 5-7/8",
            "bytes 4-8/8",
            "bytes 4-3/8",
            "bytes 4-7/*",
        ] {
            assert_eq!(c.expected(206, Some(range)), Err(Error::InvalidRange));
        }
        assert_eq!(c.offset, 4);
    }
    #[test]
    fn tail_start_accepts_only_the_exact_suffix_and_then_follows_normally() {
        let mut c = Cursor::from_tail(10);
        assert_eq!(c.range(), "bytes=-11");
        assert_eq!(c.expected(200, None), Err(Error::RangeUnsupported));
        for range in ["bytes 0-99/100", "bytes 88-98/100", "bytes 90-99/100"] {
            assert_eq!(c.expected(206, Some(range)), Err(Error::InvalidRange));
        }
        assert_eq!(
            c.expected(416, Some("bytes */100")),
            Err(Error::InvalidRange)
        );
        assert_eq!(c.offset, 0, "rejected responses move nothing");
        assert_eq!(c.expected(206, Some("bytes 89-99/100")), Ok(11));
        assert_eq!((c.offset, c.started_at, c.skip_line), (89, 90, true));
        c.commit(11);
        assert_eq!(c.range(), "bytes=100-1048675");
        // A smaller file is returned whole and read from its start.
        let mut c = Cursor::from_tail(10);
        assert_eq!(c.expected(206, Some("bytes 0-4/5")), Ok(5));
        assert_eq!((c.offset, c.started_at, c.skip_line), (0, 0, false));
        let mut c = Cursor::from_tail(10);
        assert_eq!(c.expected(416, Some("bytes */0")), Ok(0));
        assert_eq!(c.range(), "bytes=0-1048575");
    }
    #[test]
    fn reconnect_is_bounded_and_success_resets_budget() {
        let mut c = Cursor::default();
        for seconds in [1, 2, 4, 8] {
            c.disconnected();
            assert_eq!(
                c.status,
                Status::Disconnected {
                    retry_after: Duration::from_secs(seconds)
                }
            );
        }
        c.commit(0);
        c.disconnected();
        assert_eq!(
            c.status,
            Status::Disconnected {
                retry_after: Duration::from_secs(1)
            }
        );
        for _ in 0..4 {
            c.disconnected();
        }
        assert_eq!(c.status, Status::Stopped(Error::RetryExhausted));
    }
}
