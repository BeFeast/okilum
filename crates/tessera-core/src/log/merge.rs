//! Chronological merge of several indexed log files (#602 slice 8).
//!
//! A [`MergedLog`] is a list of `(file, entry)` references into N per-file
//! indexes; no entry is copied. It is derived data like the indexes it points
//! into and is rebuilt from them whenever needed.
//!
//! # Order
//!
//! Every entry gets a time key, and rows are ordered by
//! `(time key, file, entry)`:
//!
//! - An entry with a timestamp is keyed by that timestamp.
//! - An entry without one (a stack-trace line, a record without a time
//!   field) is keyed by the previous timestamped entry *of the same file*, so
//!   it stays attached right after that entry: no other file's row can sort
//!   between them, because ties go to the lower file and then to the lower
//!   entry.
//! - A leading run without timestamps has no previous entry. It is keyed by
//!   the file's first timestamp, so it sits right before that record. A file
//!   with no timestamp at all sorts at the very start of the view, in file
//!   order.
//! - Equal keys keep file order (the order the files were given in), then
//!   line order.
//!
//! A file whose timestamps go backwards is sorted by key (stably, so runs
//! without timestamps move with the record they are attached to); the
//! common, already ordered file is merged as it is.
//!
//! # Updating
//!
//! [`MergedLog::append`] takes the newly indexed entries of one file. When
//! they all sort after the current tail of the merged view (the usual case
//! for a followed log), they are appended in place; otherwise the view is
//! rebuilt. Either way the result equals [`MergedLog::build`] over the same
//! entries.

use std::cmp::Reverse;
use std::collections::binary_heap::PeekMut;
use std::collections::BinaryHeap;

use super::{LogEntry, LogIndex};

/// Position of a file in the list given to [`MergedLog::build`]. Stable for
/// the lifetime of the view, so it can pick the file's colour stripe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId(u32);

impl FileId {
    pub fn new(index: usize) -> Self {
        FileId(u32::try_from(index).expect("file count fits in u32"))
    }
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// One row of the merged view: entry `entry` of file `file`. 8 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MergedRow {
    file: FileId,
    entry: u32,
}

impl MergedRow {
    pub fn file(&self) -> FileId {
        self.file
    }
    /// Index into that file's entries.
    pub fn entry(&self) -> usize {
        self.entry as usize
    }
}

/// What [`MergedLog::append`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeUpdate {
    /// This many rows were added at the end; existing rows kept their place.
    Appended(usize),
    /// The view was rebuilt; row positions may have changed.
    Rebuilt,
}

/// Anything that holds one file's indexed entries.
pub trait MergeSource {
    fn merge_entries(&self) -> &[LogEntry];
}

impl MergeSource for LogIndex {
    fn merge_entries(&self) -> &[LogEntry] {
        self.entries()
    }
}

impl MergeSource for [LogEntry] {
    fn merge_entries(&self) -> &[LogEntry] {
        self
    }
}

impl MergeSource for Vec<LogEntry> {
    fn merge_entries(&self) -> &[LogEntry] {
        self
    }
}

impl<T: MergeSource + ?Sized> MergeSource for &T {
    fn merge_entries(&self) -> &[LogEntry] {
        (**self).merge_entries()
    }
}

/// `(time key, file, entry)`: the total order of the merged view.
type SortKey = (i64, u32, u32);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FileState {
    /// Entries of the file already in the view.
    merged: usize,
    /// Last timestamp seen in file order: the key of the next entry without
    /// one.
    carry: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergedLog {
    rows: Vec<MergedRow>,
    files: Vec<FileState>,
    tail: Option<SortKey>,
}

impl MergedLog {
    /// k-way merge of `files`, O(N log k) for N entries in k files.
    pub fn build<F: MergeSource>(files: &[F]) -> Self {
        assert!(u32::try_from(files.len()).is_ok(), "file count fits in u32");
        let mut streams = Vec::with_capacity(files.len());
        let mut states = Vec::with_capacity(files.len());
        for source in files {
            let entries = source.merge_entries();
            let (keys, carry) = time_keys(entries, None);
            let order = (!is_sorted(&keys)).then(|| {
                let mut order: Vec<u32> = (0..entries.len() as u32).collect();
                order.sort_by_key(|&entry| keys[entry as usize]);
                order
            });
            streams.push(Stream {
                keys,
                order,
                next: 0,
            });
            states.push(FileState {
                merged: entries.len(),
                carry,
            });
        }

        let mut rows = Vec::with_capacity(streams.iter().map(|s| s.keys.len()).sum());
        let mut heap: BinaryHeap<Reverse<SortKey>> = streams
            .iter()
            .enumerate()
            .filter_map(|(file, stream)| stream.peek(file as u32).map(Reverse))
            .collect();
        let mut tail = None;
        while let Some(mut top) = heap.peek_mut() {
            let key = top.0;
            let (_, file, entry) = key;
            rows.push(MergedRow {
                file: FileId(file),
                entry,
            });
            tail = Some(key);
            let stream = &mut streams[file as usize];
            stream.next += 1;
            match stream.peek(file) {
                // Replacing the top sifts once instead of a pop and a push.
                Some(next) => *top = Reverse(next),
                None => {
                    PeekMut::pop(top);
                }
            }
        }

        MergedLog {
            rows,
            files: states,
            tail,
        }
    }

    /// Brings the view up to date after `file` gained entries at its end.
    ///
    /// `files` are all files' current entries, in the order given to
    /// [`build`](Self::build); the entries already merged must be unchanged
    /// (an index replaced after truncation or rotation calls `build`). The
    /// view is rebuilt when the new entries do not all sort after its tail,
    /// when the file shrank, when the file count changed, or when a file whose
    /// merged entries had no timestamp gains its first one (that moves its
    /// leading run).
    pub fn append<F: MergeSource>(&mut self, files: &[F], file: FileId) -> MergeUpdate {
        match self.try_append(files, file) {
            Some(added) => MergeUpdate::Appended(added),
            None => {
                *self = Self::build(files);
                MergeUpdate::Rebuilt
            }
        }
    }

    fn try_append<F: MergeSource>(&mut self, files: &[F], file: FileId) -> Option<usize> {
        if files.len() != self.files.len() {
            return None;
        }
        let entries = files.get(file.index())?.merge_entries();
        let state = self.files[file.index()];
        let fresh = entries.get(state.merged..)?;
        if fresh.is_empty() {
            return Some(0);
        }
        if state.merged > 0 && state.carry.is_none() && fresh.iter().any(has_timestamp) {
            return None;
        }
        let (keys, carry) = time_keys(fresh, state.carry);
        if !is_sorted(&keys) {
            return None;
        }
        let first = (keys[0], file.0, state.merged as u32);
        if self.tail.is_some_and(|tail| first <= tail) {
            return None;
        }
        let start = state.merged as u32;
        self.rows
            .extend((start..entries.len() as u32).map(|entry| MergedRow { file, entry }));
        self.tail = Some((keys[keys.len() - 1], file.0, entries.len() as u32 - 1));
        self.files[file.index()] = FileState {
            merged: entries.len(),
            carry,
        };
        Some(fresh.len())
    }

    pub fn rows(&self) -> &[MergedRow] {
        &self.rows
    }
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub fn get(&self, row: usize) -> Option<MergedRow> {
        self.rows.get(row).copied()
    }
    pub fn file_count(&self) -> usize {
        self.files.len()
    }
    /// The entry behind `row`, looked up in the same `files` the view was
    /// built from.
    pub fn entry<'a, F: MergeSource>(&self, files: &'a [F], row: usize) -> Option<&'a LogEntry> {
        let row = self.get(row)?;
        files
            .get(row.file.index())?
            .merge_entries()
            .get(row.entry())
    }
}

struct Stream {
    keys: Vec<i64>,
    /// Entry order by key, only when the file's keys go backwards.
    order: Option<Vec<u32>>,
    next: usize,
}

impl Stream {
    fn peek(&self, file: u32) -> Option<SortKey> {
        let entry = match &self.order {
            Some(order) => *order.get(self.next)? as usize,
            None => self.next,
        };
        Some((*self.keys.get(entry)?, file, entry as u32))
    }
}

fn has_timestamp(entry: &LogEntry) -> bool {
    entry.timestamp().is_some()
}

/// Time keys for `entries`, continuing after `carry` (see the module docs),
/// and the carry after the last entry.
fn time_keys(entries: &[LogEntry], carry: Option<i64>) -> (Vec<i64>, Option<i64>) {
    let mut current = carry
        .or_else(|| entries.iter().find_map(LogEntry::timestamp))
        .unwrap_or(i64::MIN);
    let mut carry = carry;
    let keys = entries
        .iter()
        .map(|entry| {
            if let Some(ts) = entry.timestamp() {
                current = ts;
                carry = Some(ts);
            }
            current
        })
        .collect();
    (keys, carry)
}

fn is_sorted(keys: &[i64]) -> bool {
    keys.windows(2).all(|pair| pair[0] <= pair[1])
}

#[cfg(test)]
mod tests;
