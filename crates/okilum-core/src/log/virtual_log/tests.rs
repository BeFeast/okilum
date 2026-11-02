use super::*;

fn definition() -> Definition {
    Definition::parse(r#"{"version":1,"name":"Checkout","glob":"logs/**/checkout-*.jsonl","query":"service=checkout-api"}"#).unwrap()
}

#[test]
fn saved_criteria_merge_whole_files_and_report_exclusions_without_writes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("logs/pod")).unwrap();
    let definition_path = dir.path().join("checkout.okilog");
    std::fs::write(&definition_path, "{}").unwrap();
    let fixtures = [
        (
            "logs/checkout-a.jsonl",
            "{\"time\":\"2026-10-17T12:00:02Z\",\"service\":\"checkout-api\"}\n",
        ),
        (
            "logs/pod/checkout-b.jsonl",
            "{\"time\":\"2026-10-17T12:00:01Z\",\"service\":\"checkout-api\"}\n",
        ),
        ("logs/checkout-other.jsonl", "{\"service\":\"billing\"}\n"),
        ("logs/other.jsonl", "{}\n"),
        ("logs/checkout-ignore.txt", "service=checkout-api\n"),
    ];
    for (name, bytes) in fixtures {
        std::fs::write(dir.path().join(name), bytes).unwrap();
    }
    let mut progress = Vec::new();
    let selected = definition()
        .plan(dir.path(), &definition_path, 0)
        .unwrap()
        .select(|count, _| progress.push(count));
    assert!(!selected.incomplete);
    assert_eq!(progress, [1, 2, 3]);
    assert_eq!(selected.files.len(), 2);
    assert_eq!(selected.merged.len(), 2);
    assert!(selected
        .candidates
        .iter()
        .any(|c| c.reason == Reason::NoMatchingRecord));
    assert!(selected
        .candidates
        .iter()
        .any(|c| c.reason == Reason::NameMismatch));
    let indexes: Vec<_> = selected.files.iter().map(LogFile::index).collect();
    let first = selected
        .merged
        .entry(&indexes, 0)
        .unwrap()
        .timestamp()
        .unwrap();
    let last = selected
        .merged
        .entry(&indexes, 1)
        .unwrap()
        .timestamp()
        .unwrap();
    assert!(first < last);
    for (name, bytes) in fixtures {
        assert_eq!(
            std::fs::read_to_string(dir.path().join(name)).unwrap(),
            bytes
        );
    }
}

#[test]
fn injected_clock_uses_record_times_not_mtime() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("range.okilog");
    std::fs::write(&path, "{}").unwrap();
    std::fs::write(
        dir.path().join("new.jsonl"),
        "{\"time\":\"2026-10-17T12:00:00Z\"}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("old.jsonl"),
        "{\"time\":\"2026-10-17T10:00:00Z\"}\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("unknown.logfmt"), "service=checkout-api\n").unwrap();
    let now = super::super::LogIndex::build(b"{\"time\":\"2026-10-17T12:30:00Z\"}\n").entries()[0]
        .timestamp()
        .unwrap();
    let def = Definition {
        glob: "*".into(),
        query: None,
        time: Some(TimeRange::LastSeconds(3600)),
        ..definition()
    };
    let selected = def.plan(dir.path(), &path, now).unwrap().select(|_, _| {});
    assert_eq!(selected.files.len(), 1);
    assert!(selected
        .candidates
        .iter()
        .any(|c| c.reason == Reason::NoTimestamps));
    assert!(selected
        .candidates
        .iter()
        .any(|c| c.reason == Reason::OutsideTimeRange));
}

#[test]
fn rotation_membership_and_invalid_definitions_are_explicit() {
    for name in ["app.log", "app.log.1", "app.log.2.gz", "app.log-20261010"] {
        assert!(rotation_matches(Path::new(name), "app.log"), "{name}");
    }
    assert!(!rotation_matches(Path::new("app.logfmt"), "app.log"));
    assert!(!rotation_matches(
        Path::new("app.log.billing.jsonl"),
        "app.log"
    ));
    assert!(rotation_matches(
        Path::new("prog.host.user.log.INFO.20261010-060400.4411"),
        "prog.INFO"
    ));
    assert!(Definition::parse(r#"{"version":2,"name":"x","glob":"*"}"#).is_err());
    assert!(Definition::parse(r#"{"version":1,"name":"x","glob":"*","qurey":"oops"}"#).is_err());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.okilog");
    std::fs::write(&path, "{}").unwrap();
    let def = Definition {
        glob: "../*".into(),
        ..definition()
    };
    assert!(def.plan(dir.path(), &path, 0).is_err());
}

#[test]
fn copied_file_joins_reselection_and_removed_event_preserves_previous_snapshot() {
    use super::watcher::{classify, Change};
    use notify::{
        event::{CreateKind, RemoveKind},
        Event, EventKind,
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.okilog");
    std::fs::write(&path, "{}").unwrap();
    let def = Definition {
        glob: "*.jsonl".into(),
        query: None,
        ..definition()
    };
    let plan = def.plan(dir.path(), &path, 0).unwrap();
    assert_eq!(plan.select(|_, _| {}).files.len(), 0);
    let log = dir.path().join("new.jsonl");
    std::fs::write(&log, "{\"time\":\"2026-10-17T12:00:00Z\"}\n").unwrap();
    // Mock delivery positive control, not a claim of native watcher acceptance.
    assert!(matches!(
        classify(Ok(
            Event::new(EventKind::Create(CreateKind::File)).add_path(log.clone())
        )),
        Some(Change::Changed(_))
    ));
    let selected = plan.select(|_, _| {});
    assert_eq!(selected.merged.len(), 1);
    std::fs::remove_file(&log).unwrap();
    assert_eq!(
        classify(Ok(
            Event::new(EventKind::Remove(RemoveKind::File)).add_path(log.clone())
        )),
        Some(Change::Removed(vec![log]))
    );
    assert_eq!(selected.files[0].index().len(), 1);
    assert_eq!(plan.select(|_, _| {}).files.len(), 0);
}

#[test]
fn compressed_rotations_use_existing_decoder_and_chronological_merge() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.okilog");
    std::fs::write(&path, "{}").unwrap();
    std::fs::write(
        dir.path().join("app.log"),
        "{\"time\":\"2026-10-17T12:00:02Z\"}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app.log.1"),
        "{\"time\":\"2026-10-17T12:00:01Z\"}\n",
    )
    .unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(b"{\"time\":\"2026-10-17T12:00:00Z\"}\n")
        .unwrap();
    std::fs::write(dir.path().join("app.log.2.gz"), gz.finish().unwrap()).unwrap();
    let def = Definition {
        glob: "*".into(),
        query: None,
        rotation: Some("app.log".into()),
        ..definition()
    };
    let selected = def.plan(dir.path(), &path, 0).unwrap().select(|_, _| {});
    assert_eq!(selected.files.len(), 3);
    let indexes: Vec<_> = selected.files.iter().map(LogFile::index).collect();
    let times: Vec<_> = (0..3)
        .map(|i| {
            selected
                .merged
                .entry(&indexes, i)
                .unwrap()
                .timestamp()
                .unwrap()
        })
        .collect();
    assert!(times.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn oversized_file_is_excluded_before_opening() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.okilog");
    std::fs::write(&path, "{}").unwrap();
    let file = std::fs::File::create(dir.path().join("huge.log")).unwrap();
    file.set_len(MAX_FILE_BYTES + 1).unwrap();
    let def = Definition {
        glob: "*".into(),
        query: None,
        ..definition()
    };
    let selected = def
        .plan(dir.path(), &path, 0)
        .unwrap()
        .select(|_, _| panic!("oversized candidate must not be opened"));
    assert_eq!(selected.checked, 0);
    assert_eq!(selected.candidates[0].reason, Reason::SizeLimit);
}

#[cfg(unix)]
#[test]
fn glog_aliases_do_not_duplicate_records_and_outside_links_are_explained() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.okilog");
    std::fs::write(&path, "{}").unwrap();
    let target = dir
        .path()
        .join("prog.host.user.log.INFO.20261010-060400.4411");
    std::fs::write(&target, "{\"time\":\"2026-10-17T12:00:00Z\"}\n").unwrap();
    symlink(&target, dir.path().join("prog.INFO")).unwrap();
    let external = outside.path().join("private.log");
    std::fs::write(&external, "secret\n").unwrap();
    symlink(external, dir.path().join("external.log")).unwrap();
    let def = Definition {
        glob: "*".into(),
        query: None,
        ..definition()
    };
    let selected = def.plan(dir.path(), &path, 0).unwrap().select(|_, _| {});
    assert_eq!(selected.merged.len(), 1);
    assert!(selected
        .candidates
        .iter()
        .any(|c| c.reason == Reason::DuplicateSource));
    assert!(selected
        .candidates
        .iter()
        .any(|c| c.reason == Reason::OutsideRoot));
}

#[test]
fn hard_link_identities_are_indexed_once_within_one_membership_pass() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("view.okilog");
    std::fs::write(&path, "{}").unwrap();
    std::fs::write(dir.path().join("app.log"), "first\n").unwrap();
    std::fs::hard_link(dir.path().join("app.log"), dir.path().join("alias.log")).unwrap();
    let def = Definition {
        glob: "*.log".into(),
        query: None,
        ..definition()
    };
    let selection = def.plan(dir.path(), &path, 0).unwrap().select(|_, _| {});
    assert_eq!(selection.files.len(), 1);
    assert_eq!(
        selection.checked, 1,
        "the same physical content must not be opened/indexed twice"
    );
    assert_eq!(
        selection
            .candidates
            .iter()
            .filter(|c| c.reason == Reason::DuplicateSource)
            .count(),
        1
    );
}
