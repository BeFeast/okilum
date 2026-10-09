use super::*;
use crate::log::Format;

const SECOND: i64 = 1_000_000_000;
const BASE: i64 = 1_759_276_800;

/// A JSON-lines index; `None` is a record without a time field.
fn json(times: &[Option<i64>]) -> LogIndex {
    let mut data = String::new();
    for (i, time) in times.iter().enumerate() {
        match time {
            Some(t) => data.push_str(&format!("{{\"ts\":{},\"msg\":\"m{i}\"}}\n", BASE + t)),
            None => data.push_str(&format!("{{\"msg\":\"untimed {i}\"}}\n")),
        }
    }
    LogIndex::build_with_threads(data.as_bytes(), 1)
}

fn pairs(merged: &MergedLog) -> Vec<(usize, usize)> {
    merged
        .rows()
        .iter()
        .map(|row| (row.file().index(), row.entry()))
        .collect()
}

/// Every entry of every file appears exactly once.
fn assert_complete<F: MergeSource>(merged: &MergedLog, files: &[F]) {
    let mut seen: Vec<Vec<bool>> = files
        .iter()
        .map(|f| vec![false; f.merge_entries().len()])
        .collect();
    for row in merged.rows() {
        let slot = &mut seen[row.file().index()][row.entry()];
        assert!(!*slot, "{row:?} twice");
        *slot = true;
    }
    assert!(seen.iter().flatten().all(|s| *s), "every entry merged");
}

#[test]
fn interleaved_files_merge_by_time() {
    let a = json(&[Some(0), Some(2), Some(4), Some(6)]);
    let b = json(&[Some(1), Some(3), Some(5)]);
    let files = [&a, &b];
    let merged = MergedLog::build(&files);
    assert_eq!(
        pairs(&merged),
        [(0, 0), (1, 0), (0, 1), (1, 1), (0, 2), (1, 2), (0, 3)]
    );
    assert_eq!(merged.file_count(), 2);
    assert_eq!(
        merged.entry(&files, 1).unwrap().timestamp(),
        Some((BASE + 1) * SECOND)
    );
    assert!(merged.entry(&files, 7).is_none());
    assert_complete(&merged, &files);
}

#[test]
fn equal_timestamps_keep_file_then_line_order() {
    let a = json(&[Some(1), Some(1), Some(2)]);
    let b = json(&[Some(0), Some(1), Some(1)]);
    let c = json(&[Some(1)]);
    let merged = MergedLog::build(&[&a, &b, &c]);
    assert_eq!(
        pairs(&merged),
        [(1, 0), (0, 0), (0, 1), (1, 1), (1, 2), (2, 0), (0, 2)]
    );
    // The same files in another order give the tie to the new first file.
    let swapped = MergedLog::build(&[&c, &b, &a]);
    assert_eq!(
        pairs(&swapped),
        [(1, 0), (0, 0), (1, 1), (1, 2), (2, 0), (2, 1), (2, 2)]
    );
}

#[test]
fn untimestamped_runs_stay_after_their_record() {
    let a = json(&[Some(1), None, None, Some(5), None]);
    let b = json(&[Some(1), Some(2), Some(5), Some(9)]);
    let merged = MergedLog::build(&[&b, &a]);
    // a's run after 1 stays glued to a:1 although b:2 is earlier than a:5,
    // and b wins the tie at 1 and at 5, so a's runs follow a's records.
    assert_eq!(
        pairs(&merged),
        [
            (0, 0),
            (1, 0),
            (1, 1),
            (1, 2),
            (0, 1),
            (0, 2),
            (1, 3),
            (1, 4),
            (0, 3)
        ]
    );
}

#[test]
fn leading_runs_and_files_without_any_timestamp() {
    let a = json(&[None, None, Some(3), Some(4)]);
    let b = json(&[Some(1), Some(3)]);
    let c = json(&[None, None]);
    let files = [&a, &b, &c];
    let merged = MergedLog::build(&files);
    assert_eq!(
        pairs(&merged),
        [
            (2, 0),
            (2, 1),
            (1, 0),
            (0, 0),
            (0, 1),
            (0, 2),
            (1, 1),
            (0, 3)
        ],
        "a's leading run sits right before a's first record; c starts the view"
    );
    assert_complete(&merged, &files);
}

#[test]
fn backwards_time_in_one_file_is_sorted_with_its_runs() {
    let a = json(&[Some(5), None, Some(1), None, Some(3)]);
    let b = json(&[Some(2), Some(4)]);
    let merged = MergedLog::build(&[&a, &b]);
    assert_eq!(
        pairs(&merged),
        [(0, 2), (0, 3), (1, 0), (0, 4), (1, 1), (0, 0), (0, 1)]
    );
}

#[test]
fn empty_files() {
    let a = json(&[Some(2), Some(1)]);
    let empty = LogIndex::build(b"");
    let merged = MergedLog::build(&[&empty, &a, &empty]);
    assert_eq!(pairs(&merged), [(1, 1), (1, 0)]);
    assert_eq!(merged.file_count(), 3);
    let none = MergedLog::build(&[&empty]);
    assert!(none.is_empty());
    assert!(MergedLog::build::<LogIndex>(&[]).is_empty());
}

#[test]
fn json_and_logfmt_merge_on_one_clock() {
    let logfmt = LogIndex::build_with_threads(
        b"time=2025-10-01T00:00:00Z level=info msg=a\n\
          goroutine 7 [running]:\n\
          time=2025-10-01T00:00:02Z level=error msg=c\n",
        1,
    );
    let jsonl = LogIndex::build_with_threads(
        b"{\"ts\":\"2025-10-01T00:00:01.5Z\",\"level\":\"warn\",\"msg\":\"b\"}\n\
          {\"time\":1759276803000,\"level\":\"info\",\"msg\":\"d\"}\n",
        1,
    );
    assert_eq!(logfmt.format(), Format::Logfmt);
    assert_eq!(jsonl.format(), Format::JsonLines);
    let merged = MergedLog::build(&[&logfmt, &jsonl]);
    assert_eq!(pairs(&merged), [(0, 0), (0, 1), (1, 0), (0, 2), (1, 1)]);
}

/// Three files of 100k entries each. A quadratic merge would take minutes;
/// the bound is loose enough for a debug build on a slow host.
#[test]
fn three_files_of_100k_entries() {
    const N: usize = 100_000;
    let mut seed = 602u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let files: Vec<LogIndex> = (0..3)
        .map(|_| {
            let mut t = 0;
            let times: Vec<_> = (0..N)
                .map(|_| {
                    let r = next();
                    t += (r % 4) as i64;
                    (r % 50 != 0).then_some(t)
                })
                .collect();
            json(&times)
        })
        .collect();
    assert!(files.iter().all(|f| f.len() == N));

    let started = std::time::Instant::now();
    let merged = MergedLog::build(&files);
    let elapsed = started.elapsed();
    assert_eq!(merged.len(), 3 * N);
    assert_complete(&merged, &files);
    let keys: Vec<_> = (0..merged.len())
        .map(|row| merged.entry(&files, row).unwrap())
        .filter_map(LogEntry::timestamp)
        .collect();
    assert!(is_sorted(&keys), "timestamps never go backwards");
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "merged 300k entries in {elapsed:?}"
    );

    // Three followed services writing in turns: every batch is newer than
    // the merged tail, so following them never rebuilds.
    const BATCH: usize = 100;
    let mut clock = 0;
    let mut times: Vec<Vec<Option<i64>>> = (0..3).map(|_| Vec::with_capacity(N)).collect();
    for _ in 0..N / BATCH {
        for file in &mut times {
            for i in 0..BATCH {
                clock += i64::from(i % 3 == 0);
                file.push((i % 10 != 9).then_some(clock));
            }
        }
    }
    let services: Vec<LogIndex> = times.iter().map(|t| json(t)).collect();
    let started = std::time::Instant::now();
    let mut followed = MergedLog::build(&[&services[0].entries()[..0]; 3]);
    let mut appended = 0;
    for end in (BATCH..=N).step_by(BATCH) {
        let prefixes: Vec<&[LogEntry]> = services.iter().map(|f| &f.entries()[..end]).collect();
        for file in 0..3 {
            match followed.append(&prefixes, FileId::new(file)) {
                MergeUpdate::Appended(n) => appended += n,
                MergeUpdate::Rebuilt => panic!("batch ending at {end} of file {file} rebuilt"),
            }
        }
    }
    let elapsed = started.elapsed();
    assert_eq!(appended, 3 * N);
    assert_eq!(followed, MergedLog::build(&services));
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "followed 3000 batches in {elapsed:?}"
    );
}

#[test]
fn append_newer_entries_in_place() {
    let a = json(&[Some(0), None, Some(4), Some(7), None, Some(9)]);
    let b = json(&[Some(1), Some(5), Some(8)]);
    let full = MergedLog::build(&[&a, &b]);

    let mut merged = MergedLog::build(&[&a.entries()[..3], &b.entries()[..2]]);
    let before = merged.rows().to_vec();
    let grown = [&a.entries()[..5], &b.entries()[..2]];
    assert_eq!(
        merged.append(&grown, FileId::new(0)),
        MergeUpdate::Appended(2)
    );
    assert_eq!(&merged.rows()[..before.len()], before, "rows stay put");
    let grown = [&a.entries()[..5], &b.entries()[..3]];
    assert_eq!(
        merged.append(&grown, FileId::new(1)),
        MergeUpdate::Appended(1)
    );
    assert_eq!(
        merged.append(&grown, FileId::new(1)),
        MergeUpdate::Appended(0)
    );
    let all = [a.entries(), b.entries()];
    assert_eq!(
        merged.append(&all, FileId::new(0)),
        MergeUpdate::Appended(1)
    );
    assert_eq!(merged, full);
}

#[test]
fn append_falls_back_to_a_rebuild() {
    let a = json(&[None, Some(2), Some(6), None]);
    let b = json(&[Some(1), Some(3), Some(4), Some(6)]);
    let all = [a.entries(), b.entries()];
    let full = MergedLog::build(&all);

    // Older than the tail.
    let mut merged = MergedLog::build(&[&a.entries()[..3], &b.entries()[..2]]);
    assert_eq!(merged.append(&all, FileId::new(1)), MergeUpdate::Rebuilt);
    assert_eq!(merged, full);

    // Equal to the tail but from an earlier file: the tie belongs before it.
    let mut merged = MergedLog::build(&[&a.entries()[..3], &b.entries()[..3]]);
    assert_eq!(
        merged.append(&all, FileId::new(1)),
        MergeUpdate::Appended(1)
    );
    let mut merged = MergedLog::build(&[&a.entries()[..2], &b.entries()[..4]]);
    assert_eq!(merged.append(&all, FileId::new(0)), MergeUpdate::Rebuilt);
    assert_eq!(merged, full);

    // The first timestamp of a file moves its leading run.
    let mut merged = MergedLog::build(&[&a.entries()[..1], &b.entries()[..0]]);
    let grown = [a.entries(), &b.entries()[..0]];
    assert_eq!(merged.append(&grown, FileId::new(0)), MergeUpdate::Rebuilt);
    assert_eq!(merged, MergedLog::build(&grown));
    assert_eq!(pairs(&merged), [(0, 0), (0, 1), (0, 2), (0, 3)]);

    // A shrunk file and a changed file count.
    let mut merged = full.clone();
    let shrunk = [&a.entries()[..2], b.entries()];
    assert_eq!(merged.append(&shrunk, FileId::new(0)), MergeUpdate::Rebuilt);
    assert_eq!(merged, MergedLog::build(&shrunk));
    let mut merged = full.clone();
    let three = [a.entries(), b.entries(), b.entries()];
    assert_eq!(merged.append(&three, FileId::new(2)), MergeUpdate::Rebuilt);
    assert_eq!(merged, MergedLog::build(&three));
    let mut merged = full.clone();
    assert_eq!(merged.append(&all, FileId::new(5)), MergeUpdate::Rebuilt);
    assert_eq!(merged, full);
}

/// Whatever path `append` takes, the view equals a fresh build.
#[test]
fn any_append_sequence_equals_a_build() {
    let mut seed = 8u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let (mut in_place, mut rebuilt) = (0, 0);
    for _ in 0..40 {
        let files: Vec<LogIndex> = (0..3)
            .map(|_| {
                let len = (next() % 30) as usize;
                let mut t = (next() % 10) as i64;
                let times: Vec<_> = (0..len)
                    .map(|_| {
                        let r = next();
                        t += (r % 5) as i64 - i64::from(r % 13 == 0) * 6;
                        (r % 4 != 0).then_some(t)
                    })
                    .collect();
                json(&times)
            })
            .collect();
        let mut ends = [0usize; 3];
        let mut merged = MergedLog::build(&[&files[0].entries()[..0]; 3]);
        while ends.iter().zip(&files).any(|(end, f)| *end < f.len()) {
            let file = (next() % 3) as usize;
            ends[file] = (ends[file] + (next() % 6) as usize).min(files[file].len());
            let prefixes: Vec<&[LogEntry]> = files
                .iter()
                .zip(ends)
                .map(|(f, end)| &f.entries()[..end])
                .collect();
            match merged.append(&prefixes, FileId::new(file)) {
                MergeUpdate::Appended(0) => {}
                MergeUpdate::Appended(_) => in_place += 1,
                MergeUpdate::Rebuilt => rebuilt += 1,
            }
            assert_eq!(merged, MergedLog::build(&prefixes));
        }
        assert_complete(&merged, &files);
    }
    assert!(
        in_place > 0 && rebuilt > 0,
        "both paths ran: {in_place} / {rebuilt}"
    );
}
