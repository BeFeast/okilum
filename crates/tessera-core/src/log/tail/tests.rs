use std::io::Write;
use std::time::Instant;

use super::*;
use crate::log::{Format, Level, LogEntry};

fn append(path: &Path, bytes: &[u8]) {
    let mut file = File::options().append(true).open(path).unwrap();
    file.write_all(bytes).unwrap();
}

fn json(level: &str, msg: &str) -> String {
    format!("{{\"level\":\"{level}\",\"msg\":\"{msg}\"}}\n")
}

/// The incremental index equals a fresh build over the same complete lines.
fn assert_matches_full_build(tail: &LogTail) {
    let indexed = &tail.bytes()[..tail.index().bytes() as usize];
    assert_eq!(tail.index(), &LogIndex::build_with_threads(indexed, 1));
}

fn appended(event: TailEvent) -> Range<usize> {
    match event {
        TailEvent::Appended {
            entries,
            reindexed: false,
        } => entries,
        other => panic!("expected a plain append, got {other:?}"),
    }
}

fn reset(event: TailEvent) -> (ResetReason, LogSegment, Range<usize>) {
    match event {
        TailEvent::Reset {
            reason,
            previous,
            entries,
        } => (reason, previous, entries),
        other => panic!("expected a reset, got {other:?}"),
    }
}

#[test]
fn appends_in_chunks_and_holds_a_split_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.jsonl");
    std::fs::write(&path, json("info", "first")).unwrap();
    let mut tail = LogTail::open(&path).unwrap();
    assert_eq!(tail.index().len(), 1);
    assert_eq!(tail.index().format(), Format::JsonLines);
    assert!(matches!(tail.poll().unwrap(), TailEvent::Unchanged));

    // A line cut in the middle is held, not indexed.
    let second = json("error", "second");
    let (head, rest) = second.split_at(10);
    append(&path, head.as_bytes());
    let generation = tail.generation();
    assert!(matches!(tail.poll().unwrap(), TailEvent::Unchanged));
    assert_ne!(tail.generation(), generation, "held bytes change the state");
    assert_eq!(tail.index().len(), 1);
    assert_eq!(tail.pending(), head.as_bytes());

    // Its newline arrives together with the next complete line.
    append(&path, format!("{rest}{}", json("warn", "third")).as_bytes());
    assert_eq!(appended(tail.poll().unwrap()), 1..3);
    assert!(tail.pending().is_empty());
    assert_eq!(tail.raw(1).unwrap(), second.trim_end().as_bytes());
    assert_eq!(tail.index().get(1).unwrap().level(), Level::Error);
    assert_eq!(tail.index().get(2).unwrap().line(), 3);
    assert_eq!(tail.record(2).unwrap().fields[1].value, "third");

    // Several polls, blank lines included, still match a full build.
    for i in 0..5 {
        append(
            &path,
            format!("\n{}", json("debug", &format!("m{i}"))).as_bytes(),
        );
        assert_eq!(appended(tail.poll().unwrap()), 3 + i..4 + i);
    }
    assert_eq!(tail.index().stats().lines, 13);
    assert_matches_full_build(&tail);
    assert_eq!(tail.epoch(), 0);
}

#[test]
fn crlf_endings_split_between_cr_and_lf() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.logfmt");
    std::fs::write(&path, b"level=info msg=one\r\nlevel=warn msg=two\r").unwrap();
    let mut tail = LogTail::open(&path).unwrap();
    assert_eq!(tail.index().len(), 1);
    assert_eq!(tail.pending(), b"level=warn msg=two\r");
    append(&path, b"\nlevel=error msg=three\r\n");
    assert_eq!(appended(tail.poll().unwrap()), 1..3);
    assert_eq!(tail.raw(1).unwrap(), b"level=warn msg=two");
    assert_eq!(tail.raw(2).unwrap(), b"level=error msg=three");
    assert_eq!(tail.index().format(), Format::Logfmt);
    assert_eq!(tail.index().get(2).unwrap().level(), Level::Error);
    assert_matches_full_build(&tail);
}

#[test]
fn truncation_finishes_the_old_content_and_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.jsonl");
    let old = format!("{}{}unfinished", json("info", "a"), json("info", "b"));
    std::fs::write(&path, &old).unwrap();
    let mut tail = LogTail::open(&path).unwrap();
    assert_eq!(tail.index().len(), 2);

    std::fs::write(&path, json("error", "new")).unwrap();
    let (reason, previous, entries) = reset(tail.poll().unwrap());
    assert_eq!(reason, ResetReason::Truncated);
    assert_eq!(previous.bytes(), old.as_bytes());
    // The unfinished line was read before the truncation; it is kept.
    assert_eq!(previous.index().len(), 3);
    assert_eq!(previous.raw(2).unwrap(), b"unfinished");
    assert_eq!(entries, 0..1);
    assert_eq!(tail.record(0).unwrap().fields[1].value, "new");
    assert_eq!(tail.index().get(0).unwrap().line(), 1);
    assert_eq!(tail.epoch(), 1);
    assert_matches_full_build(&tail);

    // Truncated and rewritten past the old length between two polls: the
    // size grew, but the bytes before the old offset changed.
    let held = tail.bytes().len();
    let rewrite = (0..4)
        .map(|i| json("warn", &format!("r{i}")))
        .collect::<String>();
    assert!(rewrite.len() > held && rewrite.as_bytes()[..held] != tail.bytes()[..]);
    std::fs::write(&path, &rewrite).unwrap();
    let (reason, previous, entries) = reset(tail.poll().unwrap());
    assert_eq!(reason, ResetReason::Truncated);
    assert_eq!(previous.index().len(), 1);
    assert_eq!(entries, 0..4);
    assert_eq!(tail.bytes(), rewrite.as_bytes());
    assert_eq!(tail.epoch(), 2);

    // Positive control for the guard: a plain append is not a truncation.
    append(&path, json("info", "after").as_bytes());
    assert_eq!(appended(tail.poll().unwrap()), 4..5);
}

#[test]
fn rotation_by_rename_and_recreate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.jsonl");
    let rotated = dir.path().join("app.jsonl.1");
    let mut writer = File::create(&path).unwrap();
    writer.write_all(json("info", "old-1").as_bytes()).unwrap();
    let mut tail = LogTail::open(&path).unwrap();
    assert_eq!(tail.index().len(), 1);

    std::fs::rename(&path, &rotated).unwrap();
    // The writer has not reopened yet: its lines still go to the old file.
    writer.write_all(json("info", "old-2").as_bytes()).unwrap();
    assert_eq!(appended(tail.poll().unwrap()), 1..2);
    assert!(tail.is_missing(), "the path names no file mid-rotation");
    writer.write_all(b"old-unfinished").unwrap();

    // The writer reopens the path: a new file.
    let mut writer = File::create(&path).unwrap();
    writer.write_all(json("warn", "new-1").as_bytes()).unwrap();
    assert_ne!(
        path_identity(&path).unwrap(),
        path_identity(&rotated).unwrap(),
        "positive control: the two files have distinct identities"
    );
    let (reason, previous, entries) = reset(tail.poll().unwrap());
    assert_eq!(reason, ResetReason::Rotated);
    assert!(!tail.is_missing());
    assert_eq!(previous.index().len(), 3);
    assert_eq!(previous.record(1).unwrap().fields[1].value, "old-2");
    assert_eq!(previous.raw(2).unwrap(), b"old-unfinished");
    assert_eq!(previous.bytes(), std::fs::read(&rotated).unwrap());
    assert_eq!(entries, 0..1);
    assert_eq!(tail.record(0).unwrap().fields[1].value, "new-1");

    // Only the new file is followed now.
    append(&rotated, json("info", "ignored").as_bytes());
    writer.write_all(json("info", "new-2").as_bytes()).unwrap();
    assert_eq!(appended(tail.poll().unwrap()), 1..2);
    assert_eq!(tail.record(1).unwrap().fields[1].value, "new-2");
    assert_matches_full_build(&tail);
}

#[test]
fn rotation_by_copy_and_truncate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    let old: String = (0..3).map(|i| json("info", &format!("old-{i}"))).collect();
    std::fs::write(&path, &old).unwrap();
    let mut tail = LogTail::open(&path).unwrap();
    let before = path_identity(&path).unwrap();

    // logrotate's copytruncate: copy aside, truncate in place, keep writing.
    std::fs::copy(&path, dir.path().join("app.log.1")).unwrap();
    let writer = File::options().write(true).open(&path).unwrap();
    writer.set_len(0).unwrap();
    drop(writer);
    append(&path, json("error", "fresh").as_bytes());
    assert_eq!(
        path_identity(&path).unwrap(),
        before,
        "same file: only the size tells"
    );

    let (reason, previous, entries) = reset(tail.poll().unwrap());
    assert_eq!(reason, ResetReason::Truncated);
    assert_eq!(previous.bytes(), old.as_bytes());
    assert_eq!(previous.index().len(), 3);
    assert_eq!(entries, 0..1);
    assert_eq!(tail.index().get(0).unwrap().level(), Level::Error);
}

#[test]
fn a_stale_read_never_overwrites_a_newer_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.jsonl");
    std::fs::write(&path, json("info", "a")).unwrap();
    let mut tail = LogTail::open(&path).unwrap();

    append(&path, json("info", "b").as_bytes());
    let slow = tail.begin_poll();
    let fast = tail.begin_poll();
    let slow = std::thread::spawn(move || slow.read().unwrap())
        .join()
        .unwrap();
    assert!(
        matches!(slow.change, Change::Grew(ref bytes) if !bytes.is_empty()),
        "positive control: the stale read did see the new bytes"
    );
    assert_eq!(appended(tail.apply(fast.read().unwrap())), 1..2);
    let state = (
        tail.generation(),
        tail.bytes().to_vec(),
        tail.index().clone(),
    );
    assert!(matches!(tail.apply(slow), TailEvent::Stale));
    assert_eq!(
        state,
        (
            tail.generation(),
            tail.bytes().to_vec(),
            tail.index().clone()
        ),
        "no line was appended twice"
    );

    // A read from another follower of the same file is stale as well.
    let mut other = LogTail::open(&path).unwrap();
    append(&path, json("info", "c").as_bytes());
    let foreign = other.begin_poll().read().unwrap();
    assert!(matches!(tail.apply(foreign), TailEvent::Stale));
    assert_eq!(appended(other.poll().unwrap()), 2..3);
    assert_eq!(appended(tail.poll().unwrap()), 2..3);
}

#[test]
fn the_format_verdict_follows_a_short_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    std::fs::write(&path, b"").unwrap();
    let mut tail = LogTail::open(&path).unwrap();
    assert!(tail.index().is_empty());

    append(&path, b"starting\n");
    assert_eq!(appended(tail.poll().unwrap()), 0..1);
    assert_eq!(tail.index().format(), Format::Plain);
    // Records arrive: the verdict and the first row change with them.
    let records: String = (0..3).map(|i| json("warn", &format!("m{i}"))).collect();
    append(&path, records.as_bytes());
    match tail.poll().unwrap() {
        TailEvent::Appended {
            entries,
            reindexed: true,
        } => assert_eq!(entries, 1..4),
        other => panic!("expected a reindex, got {other:?}"),
    }
    assert_eq!(tail.index().format(), Format::JsonLines);
    assert_eq!(tail.index().get(0).unwrap().level(), Level::Unparsed);
    assert_eq!(tail.index().get(1).unwrap().level(), Level::Warn);
    assert_matches_full_build(&tail);

    // The sample fills with plain lines: the verdict flips back, exactly as
    // a full build of the same bytes decides.
    let plain: String = (0..SAMPLE_LINES).map(|_| "plain text\n").collect();
    append(&path, plain.as_bytes());
    match tail.poll().unwrap() {
        TailEvent::Appended {
            entries,
            reindexed: true,
        } => assert_eq!(entries, 4..4 + SAMPLE_LINES),
        other => panic!("expected a reindex, got {other:?}"),
    }
    assert_eq!(tail.index().format(), Format::Plain);
    assert_matches_full_build(&tail);

    // Past the sample the verdict is final: later records do not flip it,
    // and the old entries are not indexed again.
    let records: String = (0..2 * SAMPLE_LINES)
        .map(|i| json("info", &format!("r{i}")))
        .collect();
    append(&path, records.as_bytes());
    let before = tail.index().len();
    assert_eq!(
        appended(tail.poll().unwrap()),
        before..before + 2 * SAMPLE_LINES
    );
    assert_eq!(tail.index().format(), Format::Plain);
    assert_eq!(tail.index().stats().unparsed(), tail.index().len() as u64);
    assert_matches_full_build(&tail);
}

#[test]
fn poll_interval_is_configurable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.log");
    std::fs::write(&path, b"").unwrap();
    let mut tail = LogTail::open(&path)
        .unwrap()
        .with_interval(NETWORK_POLL_INTERVAL);
    assert_eq!(tail.interval(), NETWORK_POLL_INTERVAL);
    let start = Instant::now();
    tail.begin_poll();
    assert!(!tail.is_due(start));
    assert!(tail.is_due(start + NETWORK_POLL_INTERVAL + Duration::from_millis(10)));
    assert_eq!(poll_interval_for(&path), LOCAL_POLL_INTERVAL);
}

#[test]
fn a_ten_thousand_line_append_indexes_only_the_new_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.jsonl");
    let batch = |from: usize| -> String {
        (from..from + 10_000)
            .map(|i| {
                format!(
                    "{{\"ts\":{},\"level\":\"{}\",\"msg\":\"m{i}\"}}\n",
                    1_759_276_800 + i,
                    ["debug", "info", "warn", "error"][i % 4]
                )
            })
            .collect()
    };
    std::fs::write(&path, batch(0)).unwrap();
    let mut tail = LogTail::open(&path).unwrap();
    let first: Vec<LogEntry> = tail.index().entries().to_vec();

    append(&path, batch(10_000).as_bytes());
    let started = Instant::now();
    let entries = appended(tail.poll().unwrap());
    let elapsed = started.elapsed();
    assert_eq!(entries, 10_000..20_000);
    assert_eq!(&tail.index().entries()[..10_000], &first[..]);
    assert_eq!(tail.index().stats().count(Level::Error), 5_000);
    assert_eq!(tail.index().stats().timestamped, 20_000);
    assert_matches_full_build(&tail);
    // Generous even for an unoptimised build; this catches a re-read or a
    // quadratic re-index, not a few percent.
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
}
