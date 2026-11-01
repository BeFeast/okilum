//! Raw CRI envelopes are distinct from the Fluent Bit JSON sample corpus.
use okilum_core::log::{Format, Level, LogIndex, Role};

const DATA: &[u8] = include_bytes!("fixtures/log/cri.log");

#[test]
fn cri_assembles_payloads_and_preserves_source_and_first_timestamp() {
    let index = LogIndex::build_with_threads(DATA, 1);
    assert_eq!(index.format(), Format::Cri);
    assert_eq!(index.len(), 5);
    assert_eq!(index.stats().timestamped, 5);
    assert_eq!(
        index.get(0).unwrap().timestamp(),
        Some(1_791_612_000_123_456_789)
    );
    assert_eq!(index.get(0).unwrap().level(), Level::Missing);
    assert_eq!(index.get(1).unwrap().line(), 4);
    assert_eq!(
        index.get(1).unwrap().offset(),
        DATA.iter()
            .enumerate()
            .filter(|(_, b)| **b == b'\n')
            .nth(2)
            .unwrap()
            .0 as u64
            + 1
    );
    let raw = index.raw(DATA, 0).unwrap();
    assert!(raw.windows(2).any(|s| s == b"\r\n"));
    let record = index.record(raw).unwrap();
    assert_eq!(record.role(Role::Message).unwrap().value, "hello world!");
    assert_eq!(record.fields[1].value, "stdout");
    assert_eq!(record.fields[2].value, "F");
    let unfinished = index.record(index.raw(DATA, 2).unwrap()).unwrap();
    assert_eq!(unfinished.fields[2].value, "P");
    assert_eq!(unfinished.role(Role::Message).unwrap().value, "unfinished");
    assert_eq!(LogIndex::build_with_threads(DATA, 8), index);
}

#[test]
fn cri_rejects_invalid_envelopes_and_does_not_join_across_them() {
    for invalid in [
        "2026-10-10T06:00:00 stdout F local time",
        "2026-10-10T06:00:00Z stdin F wrong stream",
        "2026-10-10T06:00:00Z stdout X wrong tag",
        "2026-99-10T06:00:00Z stdout F invalid date",
    ] {
        assert_eq!(LogIndex::build(invalid.as_bytes()).format(), Format::Plain);
    }
    let bytes = b"2026-10-10T06:00:00Z stdout P a\nmalformed\n2026-10-10T06:00:00Z stdout F b\n";
    let index = LogIndex::build(bytes);
    assert_eq!(index.format(), Format::Cri);
    assert_eq!(index.len(), 3);
    assert_eq!(index.get(1).unwrap().level(), Level::Unparsed);
}

#[test]
fn cri_handles_empty_payloads_eof_and_bounded_decoding() {
    let bytes = b"2026-10-10T06:00:00Z stdout P first\n2026-10-10T06:00:00Z stdout P ";
    let index = LogIndex::build(bytes);
    assert_eq!(index.len(), 1);
    let record = index.record(index.raw(bytes, 0).unwrap()).unwrap();
    assert_eq!(record.role(Role::Message).unwrap().value, "first");
    assert_eq!(record.fields[2].value, "P");
    let mut large = b"2026-10-10T06:00:00Z stdout P ".to_vec();
    large.extend(std::iter::repeat_n(b'x', 180_000));
    large.extend_from_slice(b"\n2026-10-10T06:00:00Z stdout F end");
    let index = LogIndex::build(&large);
    assert!(index.record(index.raw(&large, 0).unwrap()).is_none());
    // Existing structured multiline grouping keeps its source identity.
    let trace = b"{\"level\":\"error\",\"msg\":\"failure\"}\n\tat service.rs:42";
    let index = LogIndex::build(trace);
    assert_eq!(index.len(), 1);
    assert_eq!(index.raw(trace, 0), Some(trace.as_slice()));
}
