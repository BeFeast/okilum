//! Product-path regression controls, beyond the positional-reader unit tests.
use okilum_core::log::{
    export::{self, ExportFormat, Row},
    tail::TailEvent,
    window::{WINDOW_BYTES, WINDOW_LINES, WINDOW_LINE_BYTES},
    LogFile, LogTail, Role,
};
use std::{
    fs::{File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    sync::Mutex,
};
static HEAVY: Mutex<()> = Mutex::new(());
fn append(path: &std::path::Path, bytes: &[u8]) {
    OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}
fn message(id: impl std::fmt::Display) -> String {
    format!("{{\"level\":\"info\",\"msg\":\"{id}\"}}\n")
}

#[test]
fn five_gib_opens_real_suffix_and_shares_follow_snapshots() {
    let _serial = HEAVY.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("huge.ndjson");
    let mut source = File::create(&path).unwrap();
    source.write_all(message("HEAD").as_bytes()).unwrap();
    let tail = format!("\n{}{}", message("שלום🙂"), message("REAL_LAST_5G"));
    let len = 5 * 1024 * 1024 * 1024u64;
    source.set_len(len).unwrap();
    source
        .seek(SeekFrom::Start(len - tail.len() as u64))
        .unwrap();
    source.write_all(tail.as_bytes()).unwrap();
    drop(source);
    let file = LogFile::open(&path).unwrap();
    assert!(file.window().tail && file.window().offset >= len - WINDOW_BYTES as u64);
    assert_eq!(file.index().len(), 2);
    assert!(file
        .window()
        .file_line(file.index().get(0).unwrap())
        .is_none());
    let mut json = Vec::new();
    export::write_row(
        &mut json,
        &Row {
            data: file.bytes(),
            entry: file.index().get(1).unwrap(),
            format: file.index().format(),
            regex: None,
            window: Some(file.window()),
            source: None,
        },
        &ExportFormat::JsonLines,
    )
    .unwrap();
    let row: serde_json::Value = serde_json::from_slice(&json).unwrap();
    assert!(row["line"].is_null());
    assert_eq!(row["window_line"], 2);
    assert_eq!(
        row["byte_offset"],
        file.window().absolute_offset(file.index().get(1).unwrap())
    );
    assert_eq!(row["fields"]["msg"], "REAL_LAST_5G");
    let mut follow = LogTail::open(&path).unwrap();
    let held = follow.snapshot();
    append(&path, message("NEW_AFTER_OPEN").as_bytes());
    assert!(matches!(follow.poll().unwrap(), TailEvent::Appended { .. }));
    assert_eq!(held.index().len(), 2);
    assert_eq!(follow.index().len(), 3);
    assert!(!std::sync::Arc::ptr_eq(&held, &follow.snapshot()));
    assert!(
        okilum_core::log::LogSnapshot::memory_used()
            <= okilum_core::log::LogSnapshot::memory_limit()
    );
}

#[test]
fn append_crosses_budget_evicts_without_duplicates_and_keeps_absolute_offsets() {
    let _serial = HEAVY.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("growth.ndjson");
    File::create(&path).unwrap();
    let mut tail = LogTail::open(&path).unwrap();
    let pad = "x".repeat(1024);
    let mut slides = 0;
    let mut next = 0;
    for _ in 0..72 {
        let mut chunk = String::new();
        for _ in 0..1024 {
            chunk.push_str(&format!(
                "{{\"level\":\"info\",\"msg\":\"{next}\",\"pad\":\"{pad}\"}}\n"
            ));
            next += 1;
        }
        append(&path, chunk.as_bytes());
        match tail.poll().unwrap() {
            TailEvent::WindowChanged => slides += 1,
            TailEvent::Appended { .. } => (),
            other => panic!("{other:?}"),
        }
        assert!(tail.bytes().len() <= WINDOW_BYTES);
        assert!(tail.index().len() <= WINDOW_LINES);
        assert_eq!(
            tail.window().offset + tail.bytes().len() as u64,
            tail.window().end
        );
    }
    assert!(slides > 0 && tail.window().tail && tail.window().offset > 0);
    let mut seen = std::collections::HashSet::new();
    for i in 0..tail.index().len() {
        let record = tail.record(i).unwrap();
        let id: usize = record.role(Role::Message).unwrap().value.parse().unwrap();
        assert!(seen.insert(id), "duplicate {id}");
    }
    assert!(seen.contains(&(next - 1)) && !seen.contains(&0));
    let last = tail.index().get(tail.index().len() - 1).unwrap();
    let mut source = File::open(&path).unwrap();
    source
        .seek(SeekFrom::Start(tail.window().absolute_offset(last)))
        .unwrap();
    let mut actual = vec![0; last.len()];
    std::io::Read::read_exact(&mut source, &mut actual).unwrap();
    assert_eq!(actual, tail.raw(tail.index().len() - 1).unwrap());
}

#[test]
fn dense_lines_and_oversized_partial_records_cannot_expand_the_index_or_pending_buffer() {
    let _serial = HEAVY.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dense.log");
    std::fs::write(&path, b"x\n".repeat(WINDOW_LINES + 50)).unwrap();
    let file = LogFile::open(&path).unwrap();
    assert_eq!(file.index().len(), WINDOW_LINES);
    assert!(file.window().tail);
    drop(file);
    std::fs::write(&path, message("before")).unwrap();
    let mut tail = LogTail::open(&path).unwrap();
    append(&path, &vec![b'x'; WINDOW_LINE_BYTES + 1]);
    tail.poll().unwrap();
    assert!(tail.window().oversized);
    assert!(tail.pending().len() <= WINDOW_LINE_BYTES);
    append(
        &path,
        format!("orphan continuation\n{}", message("שלום🙂 AFTER_LONG_LINE")).as_bytes(),
    );
    tail.poll().unwrap();
    assert_eq!(tail.index().len(), 1);
    assert_eq!(
        tail.record(0).unwrap().role(Role::Message).unwrap().value,
        "שלום🙂 AFTER_LONG_LINE"
    );
    assert!(std::str::from_utf8(tail.bytes()).is_ok());
}

#[test]
fn split_utf8_and_regex_context_survive_append_truncate_and_rotation() {
    let _serial = HEAVY.lock().unwrap();
    use okilum_core::log::regex_text::Definition;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("custom.log");
    std::fs::write(&path, "info first\n").unwrap();
    let parser = Definition {
        name: "test".into(),
        pattern: r"^(?<level>\w+) (?<msg>.*)$".into(),
    }
    .compile()
    .unwrap();
    let mut tail = LogTail::open(&path)
        .unwrap()
        .with_regex(parser.clone())
        .unwrap();
    let line = "warn שלום🙂\n".as_bytes();
    let cut = line.len() - 3;
    append(&path, &line[..cut]);
    tail.poll().unwrap();
    assert_eq!(tail.index().len(), 1);
    append(&path, &line[cut..]);
    tail.poll().unwrap();
    assert_eq!(
        tail.record(1).unwrap().role(Role::Message).unwrap().value,
        "שלום🙂"
    );
    std::fs::write(&path, "error truncated\n").unwrap();
    assert!(matches!(tail.poll().unwrap(), TailEvent::Reset { .. }));
    assert!(std::sync::Arc::ptr_eq(
        tail.index().regex().unwrap(),
        &parser
    ));
    std::fs::rename(&path, dir.path().join("old.log")).unwrap();
    std::fs::write(&path, "info rotated\n").unwrap();
    assert!(matches!(tail.poll().unwrap(), TailEvent::Reset { .. }));
    assert_eq!(
        tail.record(0).unwrap().role(Role::Message).unwrap().value,
        "rotated"
    );
    assert!(std::sync::Arc::ptr_eq(
        tail.index().regex().unwrap(),
        &parser
    ));
}

#[test]
fn decoded_key_amplification_is_excluded_whole_before_field_allocation() {
    let _serial = HEAVY.lock().unwrap();
    use okilum_core::log::{Format, Record};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("amplified.ndjson");
    // A 48 KiB parent repeated by 1500 leaves would decode to >70 MiB,
    // although the source record still fits the 64 KiB record budget.
    let parent = "p".repeat(48 * 1024);
    let leaves = (0..1500)
        .map(|n| format!("\"k{n}\":0"))
        .collect::<Vec<_>>()
        .join(",");
    let bomb = format!("{{\"{parent}\":{{{leaves}}}}}");
    assert!(bomb.len() <= WINDOW_LINE_BYTES);
    assert!(Record::parse(bomb.as_bytes(), Format::JsonLines).is_none());
    let valid = r#"{"parent":{"left":1,"right":"שלום🙂"},"msg":"REAL_AFTER_AMPLIFIED"}"#.as_bytes();
    let control = Record::parse(valid, Format::JsonLines).unwrap();
    assert_eq!(control.fields[0].key, "parent.left");
    assert_eq!(control.fields[1].value, "שלום🙂");
    let input = format!(
        "{}{}\n{}\n",
        message("before"),
        bomb,
        std::str::from_utf8(valid).unwrap()
    );
    std::fs::write(&path, &input).unwrap();
    let file = LogFile::open(&path).unwrap();
    assert!(file.window().oversized && file.window().tail);
    assert_eq!(file.index().len(), 1);
    assert_eq!(file.record(0).unwrap(), control);
    let entry = file.index().get(0).unwrap();
    assert_eq!(
        file.window().absolute_offset(entry),
        (input.len() - valid.len() - 1) as u64
    );
    drop(file);
    // The same exclusion must run after Follow, not only at open.
    std::fs::write(&path, message("before")).unwrap();
    let mut follow = LogTail::open(&path).unwrap();
    append(
        &path,
        format!("{bomb}\n{}\n", std::str::from_utf8(valid).unwrap()).as_bytes(),
    );
    assert!(matches!(follow.poll().unwrap(), TailEvent::WindowChanged));
    assert_eq!(follow.index().len(), 1);
    assert_eq!(follow.record(0).unwrap(), control);
    assert!(follow.window().oversized);
}

#[test]
fn oversized_multiline_block_is_never_relabelled_as_a_complete_record() {
    let _serial = HEAVY.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blocks.ndjson");
    let continuation = "    stack frame\n".repeat(WINDOW_LINE_BYTES / 8);
    let input = format!(
        "{}{}{}",
        message("TOO_LONG_BLOCK"),
        continuation,
        message("WHOLE_NEXT")
    );
    std::fs::write(&path, &input).unwrap();
    let file = LogFile::open(&path).unwrap();
    assert!(file.window().tail && file.window().oversized);
    assert_eq!(file.index().len(), 1);
    assert_eq!(
        file.record(0).unwrap().role(Role::Message).unwrap().value,
        "WHOLE_NEXT"
    );
    assert_eq!(
        file.window().absolute_offset(file.index().get(0).unwrap()),
        (input.len() - message("WHOLE_NEXT").len()) as u64
    );
}

#[test]
fn compressed_expansion_is_refused_and_decoded_tail_offsets_are_explicit() {
    let _serial = HEAVY.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("packed.log.gz");
    let mut encoder =
        flate2::write::GzEncoder::new(File::create(&path).unwrap(), flate2::Compression::fast());
    let chunk = [b'x'; 64 * 1024];
    for _ in 0..=WINDOW_BYTES / chunk.len() {
        encoder.write_all(&chunk).unwrap();
    }
    encoder.finish().unwrap();
    assert!(std::fs::metadata(&path).unwrap().len() < 1024 * 1024);
    let error = LogFile::open(&path)
        .err()
        .expect("negative: reject decoded expansion before growth");
    assert!(format!("{error:#}").contains("64 MiB decoded budget"));

    let mut encoder =
        flate2::write::GzEncoder::new(File::create(&path).unwrap(), flate2::Compression::fast());
    encoder.write_all(&b"x\n".repeat(WINDOW_LINES + 3)).unwrap();
    encoder.finish().unwrap();
    let file = LogFile::open(&path).unwrap();
    assert!(file.window().tail && file.window().decoded);
    assert_eq!(file.index().len(), WINDOW_LINES);
    let mut json = Vec::new();
    export::write_row(
        &mut json,
        &Row {
            data: file.bytes(),
            entry: file.index().get(0).unwrap(),
            format: file.index().format(),
            regex: None,
            window: Some(file.window()),
            source: None,
        },
        &ExportFormat::JsonLines,
    )
    .unwrap();
    let row: serde_json::Value = serde_json::from_slice(&json).unwrap();
    assert!(row["line"].is_null());
    assert!(row.get("byte_offset").is_none());
    assert_eq!(row["decoded_byte_offset"], 6);
    assert_eq!(row["window_line"], 1);
}

#[test]
fn rotation_with_a_large_unread_backlog_keeps_the_real_final_old_record() {
    let _serial = HEAVY.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rotate.ndjson");
    std::fs::write(&path, message("old-first")).unwrap();
    let mut follow = LogTail::open(&path).unwrap();
    let record = message("x".repeat(1024));
    let mut writer = OpenOptions::new().append(true).open(&path).unwrap();
    for _ in 0..5000 {
        writer.write_all(record.as_bytes()).unwrap();
    }
    writer
        .write_all(message("ACTUAL_FINAL_OLD_RECORD").as_bytes())
        .unwrap();
    drop(writer);
    std::fs::rename(&path, dir.path().join("old.ndjson")).unwrap();
    std::fs::write(&path, message("first-new")).unwrap();
    let TailEvent::Reset { previous, .. } = follow.poll().unwrap() else {
        panic!("positive: rotation actually detected");
    };
    assert_eq!(
        previous
            .record(previous.index().len() - 1)
            .unwrap()
            .role(Role::Message)
            .unwrap()
            .value,
        "ACTUAL_FINAL_OLD_RECORD"
    );
    assert_eq!(
        follow.record(0).unwrap().role(Role::Message).unwrap().value,
        "first-new"
    );
    assert!(previous.bytes().len() <= WINDOW_BYTES);
}

#[test]
fn rotation_does_not_resurrect_the_remainder_of_an_excluded_unfinished_line() {
    let _serial = HEAVY.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("partial.log");
    std::fs::write(&path, vec![b'x'; WINDOW_LINE_BYTES + 1]).unwrap();
    let mut follow = LogTail::open(&path).unwrap();
    assert!(follow.window().oversized && follow.index().is_empty());
    append(&path, b"ORPHAN_REMAINDER\nactual-final-old\n");
    std::fs::rename(&path, dir.path().join("old.log")).unwrap();
    std::fs::write(&path, b"new\n").unwrap();
    let TailEvent::Reset { previous, .. } = follow.poll().unwrap() else {
        panic!("rotation detected");
    };
    assert_eq!(previous.index().len(), 1);
    assert_eq!(previous.raw(0).unwrap(), b"actual-final-old");
    assert_eq!(follow.raw(0).unwrap(), b"new");
}
