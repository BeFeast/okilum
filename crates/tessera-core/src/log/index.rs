//! Parallel chunked line indexing. The index is derived data: it holds byte
//! ranges into the file plus what filtering and rows need first (level,
//! timestamp), and is rebuilt from the file whenever it is needed.

use super::{detect, record, Format, Level};

/// Below this many bytes per thread, another thread costs more than it saves.
const MIN_CHUNK_BYTES: usize = 1024 * 1024;
const NO_TIMESTAMP: i64 = i64::MIN;

/// One non-blank line. 32 bytes; the file bytes stay in the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogEntry {
    offset: u64,
    ts: i64,
    line: u32,
    len: u32,
    level: Level,
}

impl LogEntry {
    /// Byte offset of the line in the file.
    pub fn offset(&self) -> u64 {
        self.offset
    }
    /// Line length in bytes, without the line ending.
    pub fn len(&self) -> usize {
        self.len as usize
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// One-based line number in the file, blank lines included.
    pub fn line(&self) -> u64 {
        u64::from(self.line) + 1
    }
    pub fn level(&self) -> Level {
        self.level
    }
    /// Unix nanoseconds, when the record has a parseable time field.
    pub fn timestamp(&self) -> Option<i64> {
        (self.ts != NO_TIMESTAMP).then_some(self.ts)
    }
    pub fn range(&self) -> std::ops::Range<usize> {
        self.offset as usize..self.offset as usize + self.len as usize
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogStats {
    levels: [u64; Level::COUNT],
    /// Entries with a parseable timestamp.
    pub timestamped: u64,
    /// All lines, blank ones included.
    pub lines: u64,
}

impl LogStats {
    pub fn count(&self, level: Level) -> u64 {
        self.levels[level as usize]
    }
    pub fn unparsed(&self) -> u64 {
        self.count(Level::Unparsed)
    }
    /// Records that carry a level field, mapped or not.
    pub fn with_level_field(&self) -> u64 {
        Level::ALL
            .iter()
            .filter(|level| level.is_severity() || **level == Level::Unknown)
            .map(|level| self.count(*level))
            .sum()
    }
    fn add(&mut self, other: &LogStats) {
        for (mine, theirs) in self.levels.iter_mut().zip(other.levels) {
            *mine += theirs;
        }
        self.timestamped += other.timestamped;
        self.lines += other.lines;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogIndex {
    format: Format,
    entries: Vec<LogEntry>,
    stats: LogStats,
    bytes: u64,
}

impl LogIndex {
    /// Indexes with one thread per available core, as far as the size warrants.
    pub fn build(data: &[u8]) -> Self {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        Self::build_with_threads(data, threads)
    }

    /// The result is identical for every thread count.
    pub fn build_with_threads(data: &[u8], threads: usize) -> Self {
        let format = detect::detect(data);
        let threads = threads.clamp(1, (data.len() / MIN_CHUNK_BYTES).max(1));
        let bounds = chunk_bounds(data, threads);
        let chunks: Vec<Chunk> = if bounds.len() <= 2 {
            vec![index_chunk(data, 0, data.len(), format)]
        } else {
            std::thread::scope(|scope| {
                let workers: Vec<_> = bounds
                    .windows(2)
                    .map(|w| {
                        let (start, end) = (w[0], w[1]);
                        scope.spawn(move || index_chunk(data, start, end, format))
                    })
                    .collect();
                workers
                    .into_iter()
                    .map(|worker| worker.join().expect("log index worker panicked"))
                    .collect()
            })
        };
        let mut entries = Vec::with_capacity(chunks.iter().map(|c| c.entries.len()).sum());
        let mut stats = LogStats::default();
        for chunk in chunks {
            let base = stats.lines as u32;
            entries.extend(chunk.entries.into_iter().map(|mut entry| {
                entry.line += base;
                entry
            }));
            stats.add(&chunk.stats);
        }
        LogIndex {
            format,
            entries,
            stats,
            bytes: data.len() as u64,
        }
    }

    pub fn format(&self) -> Format {
        self.format
    }
    pub fn entries(&self) -> &[LogEntry] {
        &self.entries
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn get(&self, index: usize) -> Option<&LogEntry> {
        self.entries.get(index)
    }
    pub fn stats(&self) -> &LogStats {
        &self.stats
    }
    /// Size of the indexed bytes.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    /// The exact line bytes of entry `index`, without its line ending.
    pub fn raw<'a>(&self, data: &'a [u8], index: usize) -> Option<&'a [u8]> {
        data.get(self.entries.get(index)?.range())
    }
    /// Whether any record carries a level field. When none does, a level
    /// filter must not hide everything (see docs/research/602-hl-log-viewer.md).
    pub fn has_level_field(&self) -> bool {
        self.stats.with_level_field() > 0
    }
}

/// Chunk starts sit just after a newline, so no line is split between workers.
fn chunk_bounds(data: &[u8], threads: usize) -> Vec<usize> {
    let mut bounds = vec![0];
    for k in 1..threads {
        let nominal = (data.len() / threads * k).max(*bounds.last().unwrap());
        match memchr::memchr(b'\n', &data[nominal..]) {
            Some(at) if nominal + at + 1 < data.len() => bounds.push(nominal + at + 1),
            _ => break,
        }
    }
    bounds.push(data.len());
    bounds.dedup();
    bounds
}

struct Chunk {
    entries: Vec<LogEntry>,
    stats: LogStats,
}

fn index_chunk(data: &[u8], start: usize, end: usize, format: Format) -> Chunk {
    let mut entries = Vec::with_capacity((end - start) / 160);
    let mut stats = LogStats::default();
    let mut pos = start;
    let mut line = 0u32;
    while pos < end {
        let line_end = memchr::memchr(b'\n', &data[pos..end]).map_or(end, |at| pos + at);
        let mut text_end = line_end;
        if text_end > pos && data[text_end - 1] == b'\r' {
            text_end -= 1;
        }
        let text = &data[pos..text_end];
        if !text.trim_ascii().is_empty() {
            let (level, ts) = record::classify(text, format);
            stats.levels[level as usize] += 1;
            stats.timestamped += u64::from(ts.is_some());
            entries.push(LogEntry {
                offset: pos as u64,
                ts: ts.unwrap_or(NO_TIMESTAMP),
                line,
                len: (text_end - pos) as u32,
                level,
            });
        }
        line += 1;
        pos = line_end + 1;
    }
    stats.lines = u64::from(line);
    Chunk { entries, stats }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_stays_compact() {
        assert!(std::mem::size_of::<LogEntry>() <= 32);
    }

    #[test]
    fn lines_offsets_and_endings() {
        let data = b"{\"level\":\"info\"}\r\n\n  \n{\"level\":\"error\",\"ts\":1759276800}\nnot json\n{\"a\":1}";
        let index = LogIndex::build_with_threads(data, 1);
        assert_eq!(index.format(), Format::JsonLines);
        let summary: Vec<_> = index
            .entries()
            .iter()
            .map(|e| (e.line(), e.level(), e.timestamp().is_some()))
            .collect();
        assert_eq!(
            summary,
            [
                (1, Level::Info, false),
                (4, Level::Error, true),
                (5, Level::Unparsed, false),
                (6, Level::Missing, false),
            ]
        );
        assert_eq!(index.raw(data, 0).unwrap(), b"{\"level\":\"info\"}");
        assert_eq!(index.raw(data, 2).unwrap(), b"not json");
        assert_eq!(
            index.raw(data, 3).unwrap(),
            b"{\"a\":1}",
            "no final newline"
        );
        assert_eq!(index.stats().lines, 6);
        assert_eq!(index.stats().unparsed(), 1);
        assert_eq!(index.stats().timestamped, 1);
        assert!(index.has_level_field());
        assert!(LogIndex::build(b"").is_empty());
    }

    #[test]
    fn every_thread_count_gives_the_same_index() {
        let mut data = Vec::new();
        for i in 0..80_000 {
            match i % 7 {
                0 => data.extend_from_slice(b"stack trace line\n"),
                1 => data.push(b'\n'),
                _ => data.extend_from_slice(
                    format!(
                        "{{\"ts\":{},\"level\":\"{}\",\"msg\":\"m{i}\"}}\n",
                        1_759_276_800 + i,
                        ["debug", "info", "warn", "error"][i % 4]
                    )
                    .as_bytes(),
                ),
            }
        }
        let one = LogIndex::build_with_threads(&data, 1);
        assert!(data.len() > 2 * MIN_CHUNK_BYTES, "the parallel path runs");
        assert!(chunk_bounds(&data, 4).len() > 2);
        for threads in [2, 3, 4, 16] {
            assert_eq!(
                LogIndex::build_with_threads(&data, threads),
                one,
                "{threads}"
            );
        }
        assert_eq!(one.stats().unparsed(), 80_000 / 7 + 1);
        assert_eq!(one.stats().lines, 80_000);
    }
}
