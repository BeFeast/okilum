//! Fixture-level checks and the 100 MB throughput probe.

use super::*;

fn fixture(name: &str) -> LogFile {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/log")
        .join(name);
    LogFile::open(&path).unwrap()
}

fn levels(file: &LogFile) -> Vec<Level> {
    file.index().entries().iter().map(LogEntry::level).collect()
}

#[test]
fn okilum_diagnostic_log_has_no_level_and_nothing_is_hidden() {
    let file = fixture("reader-diagnostic.log");
    let index = file.index();
    assert_eq!(index.format(), Format::JsonLines);
    assert_eq!(index.len(), 7);
    assert_eq!(index.stats().unparsed(), 0);
    assert_eq!(index.stats().timestamped, 7, "epoch-second `time`");
    // No level field anywhere: every record says so, including the one with
    // an `error` key, which is not promoted to a guessed severity.
    assert!(levels(&file).iter().all(|level| *level == Level::Missing));
    assert!(!index.has_level_field());
    let failure = file.record(5).unwrap();
    assert_eq!(failure.fields[2].key, "error");
    assert!(failure.role(Role::Level).is_none());
    let warm = file.record(2).unwrap();
    let keys: Vec<_> = warm.fields.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "time",
            "launch",
            "phase",
            "elapsed_ms",
            "vault",
            "details.found",
            "details.reason",
            "details.startup_bytes",
            "details.source_bank_bytes",
        ]
    );
    assert_eq!(warm.fields[3].value, "14.2");
    assert_eq!(warm.fields[8].kind, ValueKind::Null);
    assert_eq!(
        warm.role(Role::Time).map(|f| f.value.as_str()),
        Some("1759276801")
    );
    let unreadable = file.record(3).unwrap();
    assert_eq!(unreadable.fields[2].kind, ValueKind::Array);
    assert!(unreadable.fields[2].value.starts_with("[{\"path\""));
}

#[test]
fn service_json_lines_keep_unparsed_rows_and_exact_text() {
    let file = fixture("service.jsonl");
    let index = file.index();
    assert_eq!(index.format(), Format::JsonLines);
    assert_eq!(
        levels(&file),
        [
            Level::Error,
            Level::Warn,
            Level::Info,
            Level::Unparsed,
            Level::Debug,
            Level::Trace,
            Level::Fatal,
            Level::Info,
        ]
    );
    let lines: Vec<_> = index.entries().iter().map(LogEntry::line).collect();
    assert_eq!(
        lines,
        [1, 2, 3, 4, 5, 7, 8, 9],
        "the blank line keeps numbering"
    );
    assert_eq!(index.stats().timestamped, 6, "`not a time` is not guessed");
    assert_eq!(
        file.raw(3).unwrap(),
        b"thread 'main' panicked at src/main.rs:12:5:",
        "CRLF is not part of the raw line"
    );
    let first = file.record(0).unwrap();
    assert_eq!(first.role(Role::Logger).unwrap().value, "vault.scan");
    let rest: Vec<_> = first
        .rest()
        .map(|f| format!("{}={}", f.key, f.value))
        .collect();
    assert_eq!(
        rest,
        [
            "request_id=97c0482241e0de67",
            "status=500",
            "path=/v1/notes/a",
            "user.id=2210",
            "user.role=owner",
        ]
    );
    assert_eq!(
        file.record(1).unwrap().fields[5].value,
        "812.50",
        "numbers keep their spelling"
    );
    assert_eq!(
        file.record(4).unwrap().role(Role::Message).unwrap().value,
        "line with \"quotes\" and \u{e9}"
    );
    let fatal = file.record(6).unwrap();
    assert_eq!(fatal.role(Role::Level).unwrap().value, "fatal");
    assert!(fatal.rest().any(|f| f.key == "Level"), "duplicates survive");
    assert!(file.record(3).is_none());
}

#[test]
fn logfmt_and_plain_text() {
    let file = fixture("service.logfmt");
    assert_eq!(file.index().format(), Format::Logfmt);
    assert_eq!(
        levels(&file),
        [
            Level::Info,
            Level::Warn,
            Level::Unparsed,
            Level::Error,
            Level::Debug
        ]
    );
    assert_eq!(
        file.record(3).unwrap().role(Role::Message).unwrap().value,
        "query failed: \"timeout\""
    );
    assert_eq!(
        file.record(4).unwrap().role(Role::Message).unwrap().value,
        ""
    );
    let plain = fixture("plain.log");
    assert_eq!(plain.index().format(), Format::Plain);
    assert_eq!(plain.index().stats().unparsed(), 3);
}

#[test]
fn log_extensions() {
    for name in ["a.log", "b.JSONL", "c.ndjson", "d.logfmt"] {
        assert!(is_log_path(Path::new(name)), "{name}");
    }
    for name in ["a.md", "log", "a.log.1", "a.log.gz", "a.json"] {
        assert!(!is_log_path(Path::new(name)), "{name}");
    }
}

/// `cargo test -p okilum-core --release -- --ignored --nocapture log::tests::throughput`
///
/// Builds a seeded ~100 MiB JSON-lines file, indexes it through the mapped
/// path, and checks the counts against what the generator wrote and against
/// a byte search that shares no code with the parser. Timings are printed,
/// not asserted: they only compare within one machine and session.
#[test]
#[ignore = "writes and indexes 100 MiB; run explicitly"]
fn throughput_100mb_with_known_counts() {
    const TARGET: usize = 100 * 1024 * 1024;
    const LEVELS: [&str; 4] = ["debug", "info", "warn", "error"];
    let mut seed = 602u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut data = Vec::with_capacity(TARGET + 1024);
    let (mut written, mut errors, mut unparsed) = (0u64, 0u64, 0u64);
    while data.len() < TARGET {
        let r = next();
        if r % 997 == 0 {
            data.extend_from_slice(b"    at okilum::reader::open (reader.rs:42)\n");
            unparsed += 1;
        } else {
            let level = match r % 20 {
                0..=5 => 0,
                6..=16 => 1,
                17..=18 => 2,
                _ => 3,
            };
            errors += u64::from(level == 3);
            let line = format!(
                "{{\"ts\":\"2025-10-01T{:02}:{:02}:{:02}.{:03}Z\",\"level\":\"{}\",\"logger\":\"vault.scan\",\"msg\":\"request \\\"{}\\\" done\",\"request_id\":\"{:016x}\",\"duration_ms\":{},\"status\":{},\"user\":{{\"id\":{},\"role\":\"owner\"}}}}\n",
                (r >> 8) % 24,
                (r >> 16) % 60,
                (r >> 24) % 60,
                (r >> 32) % 1000,
                LEVELS[level],
                r % 1000,
                next(),
                r % 2000,
                [200, 404, 500][(r % 3) as usize],
                r % 5000,
            );
            data.extend_from_slice(line.as_bytes());
        }
        written += 1;
    }
    let independent_errors =
        memchr::memmem::find_iter(&data, b"\"level\":\"error\"").count() as u64;
    assert_eq!(
        independent_errors, errors,
        "generator and byte search agree"
    );
    assert!(errors > 0 && unparsed > 0, "the controls are not vacuous");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.jsonl");
    std::fs::write(&path, &data).unwrap();
    drop(data);

    let started = std::time::Instant::now();
    let file = LogFile::open(&path).unwrap();
    let parallel = started.elapsed();
    assert!(file.is_mapped());
    let index = file.index();
    assert_eq!(index.format(), Format::JsonLines);
    assert_eq!(index.len() as u64, written);
    assert_eq!(index.stats().count(Level::Error), errors);
    assert_eq!(index.stats().unparsed(), unparsed);
    assert_eq!(index.stats().timestamped, written - unparsed);

    let started = std::time::Instant::now();
    let single = LogIndex::build_with_threads(file.bytes(), 1);
    let one_thread = started.elapsed();
    assert_eq!(&single, index, "thread count does not change the index");

    let mb = index.bytes() as f64 / (1024.0 * 1024.0);
    println!(
        "indexed {mb:.1} MiB, {} records ({} errors, {} unparsed): {:.3} s parallel ({:.0} MiB/s), {:.3} s one thread ({:.0} MiB/s), index {:.1} MiB",
        index.len(),
        errors,
        unparsed,
        parallel.as_secs_f64(),
        mb / parallel.as_secs_f64(),
        one_thread.as_secs_f64(),
        mb / one_thread.as_secs_f64(),
        (index.len() * std::mem::size_of::<LogEntry>()) as f64 / (1024.0 * 1024.0),
    );
}

fn sample(name: &str) -> LogFile {
    fixture(&format!("samples/native-formats/{name}"))
}

fn first_with(file: &LogFile, level: Level) -> Record {
    let at = levels(file).iter().position(|l| *l == level).unwrap();
    file.record(at).unwrap()
}

fn role<'r>(record: &'r Record, role: Role) -> Option<&'r str> {
    record.role(role).map(|field| field.value.as_str())
}

#[test]
fn spdlog_default_pattern_has_times_levels_and_loggers() {
    let file = sample("cpp/ordgw.spdlog-default.log");
    let index = file.index();
    assert_eq!(index.format(), Format::Spdlog);
    assert_eq!(index.format().label(), "spdlog");
    assert_eq!(index.len(), 35);
    // The backward-cpp trace after the crash stays raw until #1161.
    assert_eq!(index.stats().unparsed(), 14);
    assert_eq!(index.stats().timestamped, 21);
    let first = file.record(0).unwrap();
    assert_eq!(role(&first, Role::Time), Some("2026-10-10 06:04:00.000"));
    assert_eq!(
        index.get(0).unwrap().timestamp(),
        timestamp::parse_text(b"2026-10-10T06:04:00Z")
    );
    assert_eq!(role(&first, Role::Logger), Some("ordgw"));
    assert!(role(&first, Role::Message)
        .unwrap()
        .starts_with("ordgw 3.14.2"));
    let crash = first_with(&file, Level::Fatal);
    assert_eq!(role(&crash, Role::Level), Some("critical"));
    assert!(levels(&file).contains(&Level::Trace));
}

#[test]
fn glog_lines_and_header_are_records() {
    let file = sample("cpp/ordgw.glog.log");
    let index = file.index();
    assert_eq!(index.format(), Format::Glog);
    assert_eq!(index.len(), 29);
    // Four header lines and 21 log lines; the check-failure trace is raw.
    assert_eq!(index.stats().unparsed(), 4);
    assert_eq!(index.stats().timestamped, 21);
    assert_eq!(levels(&file)[..4], [Level::Missing; 4]);
    let header = file.record(1).unwrap();
    assert_eq!(
        role(&header, Role::Message),
        Some("Running on machine: trade-gw-01")
    );
    let fatal = first_with(&file, Level::Fatal);
    assert_eq!(role(&fatal, Role::Level), Some("FATAL"));
    let rest: Vec<_> = fatal
        .rest()
        .map(|f| (f.key.as_str(), f.value.as_str(), f.kind))
        .collect();
    assert_eq!(
        rest,
        [
            ("tid", "4414", ValueKind::Number),
            ("src", "crash_handler.cc:33", ValueKind::String),
        ]
    );
    assert_eq!(
        index.get(4).unwrap().timestamp(),
        timestamp::parse_text(b"2026-10-10T06:04:00.000279Z")
    );
}

#[test]
fn combined_access_log_has_fields_and_no_guessed_level() {
    let file = sample("apache-access-combined.log");
    let index = file.index();
    assert_eq!(index.format(), Format::AccessLog);
    assert_eq!(index.len(), 649);
    assert_eq!(index.stats().unparsed(), 0);
    assert_eq!(index.stats().timestamped, 649);
    // An access log has no severity; a status code is not turned into one.
    assert!(!index.has_level_field());
    let record = file.record(1).unwrap();
    let keys: Vec<_> = record.fields.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(
        keys,
        ["client", "time", "method", "path", "proto", "status", "referer", "ua"]
    );
    assert_eq!(record.fields[5].kind, ValueKind::Number);
    assert_eq!(
        index.get(0).unwrap().timestamp(),
        timestamp::parse_text(b"2026-10-10T06:00:01Z")
    );
}

#[test]
fn syslog_priority_becomes_the_level() {
    let file = fixture("syslog.log");
    let index = file.index();
    assert_eq!(index.format(), Format::Syslog);
    assert_eq!(index.stats().unparsed(), 0);
    assert_eq!(
        levels(&file),
        [
            Level::Info,
            Level::Info,
            Level::Info,
            Level::Error,
            Level::Fatal,
            Level::Info,
            Level::Warn,
            Level::Fatal,
        ]
    );
    // BSD times carry no year and stay as written.
    assert_eq!(index.stats().timestamped, 5);
    let sshd = file.record(2).unwrap();
    assert_eq!(role(&sshd, Role::Level), Some("info"));
    let keys: Vec<_> = sshd.fields.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "facility",
            "severity",
            "time",
            "host",
            "programname",
            "procid",
            "sd",
            "msg"
        ]
    );
    assert_eq!(sshd.fields[0].value, "authpriv");
}

#[test]
fn a_plain_file_is_not_claimed_by_a_text_format() {
    assert_eq!(fixture("plain.log").index().format(), Format::Plain);
    // Half the sample in one text format is enough; the rest stays raw.
    assert_eq!(
        detect(b"[2026-10-10 06:04:00.000] [x] [info] a\nnoise\n"),
        Format::Spdlog
    );
    assert_eq!(
        detect(b"[2026-10-10 06:04:00.000] [x] [info] a\nnoise\nmore noise\n"),
        Format::Plain
    );
}
