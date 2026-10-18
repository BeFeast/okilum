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
    // Line 4 is a Rust panic line: a block at level error (#1161).
    assert_eq!(
        levels(&file),
        [
            Level::Error,
            Level::Warn,
            Level::Info,
            Level::Error,
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
    // Line 3 starts a goroutine dump: a block at level error (#1161).
    assert_eq!(
        levels(&file),
        [
            Level::Info,
            Level::Warn,
            Level::Error,
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
    for name in [
        "a.log",
        "b.JSONL",
        "c.ndjson",
        "d.logfmt",
        // Rotated (#1164): numbered, dated, logrotate dateext.
        "app.log.1",
        "app.log.2026-10-10",
        "app.log.20261010",
        "app.log-20261010",
        "events.jsonl.3",
        // glog's file and its newest-file link.
        "ordgw.trade-gw-01.god.log.INFO.20261010-060400.4411",
        "ordgw.WARNING",
        // Compressed, by the name inside.
        "app.log.gz",
        "app.log.1.gz",
        "app.log-20261010.bz2",
        "events.jsonl.zst",
        "APP.LOG.GZ",
    ] {
        assert!(is_log_path(Path::new(name)), "{name}");
    }
    for name in [
        "a.md",
        "log",
        "a.json",
        "a.log.bak",
        "a.log.1.tmp",
        "release.1",
        "app-20261010",
        "site.tar.gz",
        "a.gz",
        ".log",
        "info.INFO.md",
        "x.log.INFO.2026",
    ] {
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
    // Stack frames are indented continuation lines: each joins the record
    // before it (#1161), so they are not entries of their own.
    let (mut written, mut errors, mut frames) = (0u64, 0u64, 0u64);
    while data.len() < TARGET {
        let r = next();
        if r % 997 == 0 {
            data.extend_from_slice(b"    at okilum::reader::open (reader.rs:42)\n");
            frames += 1;
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
    assert!(errors > 0 && frames > 0, "the controls are not vacuous");
    assert_eq!(
        data.first(),
        Some(&b'{'),
        "every frame has a record before it"
    );

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.jsonl");
    std::fs::write(&path, &data).unwrap();
    drop(data);

    let started = std::time::Instant::now();
    let file = LogFile::open(&path).unwrap();
    let parallel = started.elapsed();
    assert!(!file.is_mapped() && file.window().tail);
    assert!(file.bytes().len() <= window::WINDOW_BYTES);
    let index = file.index();
    assert_eq!(index.format(), Format::JsonLines);
    assert!(index.len() <= window::WINDOW_LINES);
    let loaded_errors =
        memchr::memmem::find_iter(file.bytes(), b"\"level\":\"error\"").count() as u64;
    assert_eq!(index.stats().count(Level::Error), loaded_errors);
    assert_eq!(index.stats().unparsed(), 0);
    assert_eq!(index.stats().timestamped, index.len() as u64);

    let started = std::time::Instant::now();
    let single = LogIndex::build_with_threads(file.bytes(), 1);
    let one_thread = started.elapsed();
    assert_eq!(&single, index, "thread count does not change the index");

    let mb = index.bytes() as f64 / (1024.0 * 1024.0);
    println!(
        "indexed {mb:.1} MiB, {} records ({} errors, {} frames joined): {:.3} s parallel ({:.0} MiB/s), {:.3} s one thread ({:.0} MiB/s), index {:.1} MiB",
        index.len(),
        errors,
        frames,
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

fn role(record: &Record, role: Role) -> Option<&str> {
    record.role(role).map(|field| field.value.as_str())
}

#[test]
fn spdlog_default_pattern_has_times_levels_and_loggers() {
    let file = sample("cpp/ordgw.spdlog-default.log");
    let index = file.index();
    assert_eq!(index.format(), Format::Spdlog);
    assert_eq!(index.format().label(), "spdlog");
    // 21 records and the backward-cpp trace after the crash, as one block.
    assert_eq!(index.len(), 22);
    assert_eq!(index.stats().unparsed(), 0);
    assert_eq!(index.stats().blocks, 1);
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
    // Four header lines, 21 log lines and the check-failure trace.
    assert_eq!(index.len(), 26);
    assert_eq!(index.stats().unparsed(), 0);
    assert_eq!(index.stats().blocks, 1);
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

#[test]
fn crashes_are_one_entry_each_and_copy_exactly() {
    for (name, before, kind, lines) in [
        (
            "kubernetes/checkout-api-previous.log",
            31,
            BlockKind::GoPanic,
            14,
        ),
        ("cpp/ordgw.spdlog.jsonl", 21, BlockKind::StackTrace, 14),
        (
            "cpp/ordgw_tests-asan.log",
            35,
            BlockKind::SanitizerReport,
            27,
        ),
    ] {
        let file = fixture(&format!("samples/{name}"));
        let index = file.index();
        assert_eq!(index.format(), Format::JsonLines, "{name}");
        // The crash is the last entry, right after the records before it.
        assert_eq!(index.len(), before + 1, "{name}");
        assert_eq!(index.stats().blocks, 1, "{name}");
        let entry = *index.get(before).unwrap();
        assert_eq!(entry.level(), Level::Error, "{name}");
        assert_eq!(entry.timestamp(), None, "{name}");
        let raw = file.raw(before).unwrap();
        assert_eq!(block::block_kind(raw), Some(kind), "{name}");
        assert_eq!(block::line_count(raw), lines, "{name}");
        // Raw copy: the file's own bytes, every line, blank ones included.
        let rest = &file.bytes()[entry.offset() as usize..];
        assert_eq!(raw, rest.strip_suffix(b"\n").unwrap_or(rest), "{name}");
        assert!(file.record(before).is_none(), "{name}");
    }
}

#[test]
fn text_finds_words_inside_a_crash_and_a_trace() {
    fn hits(file: &LogFile, text: &str) -> Vec<u32> {
        let filter = LogFilter {
            text: text.to_owned(),
            ..Default::default()
        };
        let ticket = FilterGenerations::default().next();
        filter::evaluate(&filter, file.index(), file.bytes(), &ticket)
            .unwrap()
            .rows
    }
    let go = fixture("samples/kubernetes/checkout-api-previous.log");
    assert_eq!(hits(&go, "tax.(*Engine).Rate"), [31]);
    let spdlog = fixture("samples/cpp/ordgw.spdlog.jsonl");
    assert_eq!(hits(&spdlog, "PositionMap::find"), [21]);
    // A record's own continuation lines are searched too.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("java.jsonl");
    std::fs::write(
        &path,
        "{\"level\":\"error\",\"msg\":\"request failed\"}\n\tat com.example.Pool.take(Pool.java:88)\n{\"level\":\"info\",\"msg\":\"ok\"}\n",
    )
    .unwrap();
    let java = LogFile::open(&path).unwrap();
    assert_eq!(java.index().len(), 2);
    assert_eq!(hits(&java, "Pool.java:88"), [0]);
    assert!(hits(&java, "no such frame").is_empty());
}

#[test]
fn journald_macos_and_windows_exports_have_their_roles() {
    fn roles(file: &LogFile, index: usize) -> [Option<String>; 3] {
        let record = file.record(index).unwrap();
        [Role::Level, Role::Message, Role::Logger]
            .map(|role| record.role(role).map(|field| field.value.clone()))
    }
    let journal = sample("linux-journalctl-o-json.jsonl");
    assert_eq!(
        levels(&journal),
        [Level::Info, Level::Warn, Level::Error, Level::Fatal]
    );
    assert_eq!(journal.index().stats().timestamped, 4);
    assert_eq!(
        journal.index().get(0).unwrap().timestamp(),
        Some(1_791_612_010_915_171_000),
        "__REALTIME_TIMESTAMP is microseconds"
    );
    assert_eq!(
        roles(&journal, 0),
        [
            Some("6".into()),
            Some("Started nginx.service - A high performance web server.".into()),
            Some("systemd".into()),
        ]
    );

    let mac = sample("macos-log-show.ndjson");
    assert_eq!(
        levels(&mac),
        [
            Level::Info,
            Level::Error,
            Level::Info,
            Level::Fatal,
            Level::Debug,
            Level::Error,
        ]
    );
    assert_eq!(mac.index().stats().timestamped, 6);
    assert_eq!(
        mac.index().get(0).unwrap().timestamp(),
        timestamp::parse_text(b"2026-10-10T03:00:00.583433Z")
    );
    let [level, message, logger] = roles(&mac, 1);
    assert_eq!(level.as_deref(), Some("Error"));
    assert_eq!(message.as_deref(), Some("Security error has occurred."));
    assert_eq!(logger.as_deref(), Some("com.apple.securityd"));

    let windows = sample("windows-get-winevent.jsonl");
    assert_eq!(
        levels(&windows),
        [Level::Info, Level::Warn, Level::Error, Level::Fatal]
    );
    assert_eq!(windows.index().stats().timestamped, 4);
    assert_eq!(
        windows.index().get(0).unwrap().timestamp(),
        timestamp::parse_text(b"2026-10-10T03:00:51.256964Z")
    );
    let [level, message, logger] = roles(&windows, 1);
    assert_eq!(level.as_deref(), Some("Warning"));
    assert!(message
        .unwrap()
        .starts_with("Name resolution for the name wpad"));
    assert_eq!(logger.as_deref(), Some("Microsoft-Windows-DNS-Client"));
}

#[test]
fn colour_codes_are_gone_for_parsing_and_matching_but_not_from_raw() {
    // `kubectl logs` of an spdlog colour sink, codes as spdlog writes them.
    let file = fixture("kubectl-colour.log");
    let index = file.index();
    assert_eq!(index.format(), Format::Spdlog);
    assert_eq!(index.stats().unparsed(), 0);
    assert_eq!(
        levels(&file),
        [
            Level::Info,
            Level::Warn,
            Level::Error,
            Level::Error,
            Level::Fatal,
            Level::Debug,
        ]
    );
    let record = file.record(3).unwrap();
    assert_eq!(record.role(Role::Level).unwrap().value, "error");
    assert_eq!(
        record.role(Role::Message).unwrap().value,
        "connection reset, see runbook"
    );
    assert!(record.fields.iter().all(|f| !f.value.contains('\u{1b}')));
    // Raw copy keeps every byte.
    assert!(file.raw(3).unwrap().contains(&0x1b));
    let hits = |text: &str| {
        let filter = LogFilter {
            text: text.to_owned(),
            ..Default::default()
        };
        let ticket = FilterGenerations::default().next();
        filter::evaluate(&filter, index, file.bytes(), &ticket)
            .unwrap()
            .rows
    };
    assert_eq!(hits("level=error"), [2, 3]);
    // Positive control: a hyperlink code sits between the words in the raw
    // bytes, so only the stripped text can match.
    assert!(memchr::memmem::find(file.bytes(), b"see runbook").is_none());
    assert_eq!(hits("see runbook"), [3]);
}

#[test]
fn colour_codes_inside_json_strings_are_gone_too() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("zap.jsonl");
    std::fs::write(
        &path,
        "{\"level\":\"\\u001b[31mERROR\\u001b[0m\",\"msg\":\"\\u001b[1mboom\\u001b[0m\"}\n\u{1b}[33mplain warning\u{1b}[0m\n",
    )
    .unwrap();
    let file = LogFile::open(&path).unwrap();
    assert_eq!(levels(&file), [Level::Error, Level::Unparsed]);
    let record = file.record(0).unwrap();
    assert_eq!(record.role(Role::Message).unwrap().value, "boom");
    assert_eq!(record.role(Role::Level).unwrap().value, "ERROR");
}

#[test]
fn compressed_logs_decode_to_the_index_of_their_text() {
    // Packed by the system gzip, zstd and bzip2, not by the crates that read
    // them; one gzip file is two members back to back.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/log");
    for (packed, plain) in [
        (
            "names/access.log.1.gz",
            "samples/native-formats/apache-access-combined.log",
        ),
        (
            "names/ordgw.log.2026-10-10.zst",
            "samples/native-formats/cpp/ordgw.spdlog-default.log",
        ),
        ("names/syslog.log-20261010.bz2", "syslog.log"),
        ("names/two-members.log.gz", "syslog.log"),
    ] {
        let path = root.join(packed);
        assert!(is_log_path(&path), "{packed}");
        let file = LogFile::open(&path).unwrap();
        let text = fixture(plain);
        // Positive control: the file on disk is not the text.
        assert_ne!(std::fs::read(&path).unwrap(), text.bytes(), "{packed}");
        assert!(file.is_compressed() && !text.is_compressed(), "{packed}");
        assert_eq!(file.bytes(), text.bytes(), "{packed}");
        assert_eq!(file.index(), text.index(), "{packed}");
        let error = LogTail::open(&path)
            .err()
            .expect("a compressed log is not followed");
        assert!(error.to_string().contains("cannot be followed"), "{packed}");
    }
    let broken = tempfile::tempdir().unwrap();
    let path = broken.path().join("app.log.gz");
    std::fs::write(&path, b"not gzip at all").unwrap();
    let error = LogFile::open(&path).err().expect("garbage is refused");
    assert!(format!("{error:#}").contains("cannot be decoded"));
}

#[test]
fn a_glog_file_name_opens_as_glog() {
    let file = fixture("names/ordgw.trade-gw-01.god.log.INFO.20261010-060400.4411");
    assert_eq!(file.index().format(), Format::Glog);
    assert_eq!(file.index().len(), 26);
}
