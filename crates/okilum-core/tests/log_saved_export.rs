//! Saved queries and the export of filtered rows over the sample pack (#1169).
//! The expectations are computed here from the file's own text, not from the
//! evaluator under test: the Apache rows are picked by reading `status=` off
//! each line.
use std::path::Path;

use okilum_core::log::export::{write_row, ExportFormat, Row};
use okilum_core::log::saved_query::{SavedQueries, SavedQuery};
use okilum_core::log::{filter, FilterGenerations, LogFile, LogFilter, Query};

fn sample(name: &str) -> LogFile {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/log/samples");
    LogFile::open(&root.join(name)).unwrap_or_else(|error| panic!("{name}: {error}"))
}

fn run(file: &LogFile, filter: &LogFilter) -> Vec<u32> {
    let ticket = FilterGenerations::default().next();
    filter::evaluate(filter, file.index(), file.bytes(), &ticket)
        .expect("a current ticket finishes")
        .rows
}

fn query(text: &str) -> LogFilter {
    LogFilter {
        query: Some(Query::parse(text).unwrap()),
        ..Default::default()
    }
}

fn export(file: &LogFile, rows: &[u32], format: ExportFormat) -> Vec<u8> {
    let mut out = Vec::new();
    for &row in rows {
        let row = Row {
            window: Some(file.window()),
            data: file.bytes(),
            entry: &file.index().entries()[row as usize],
            format: file.index().format(),
            regex: file.index().regex().map(|regex| regex.as_ref()),
            source: None,
        };
        write_row(&mut out, &row, &format).unwrap();
    }
    out
}

#[test]
fn a_saved_query_saved_on_one_log_applies_to_another() {
    let mut list = SavedQueries::new();
    // Saved while looking at Kibana, with a level choice and a time preset.
    let kibana = sample("kibana/kibana.ndjson");
    let mut filter = query("level>=warn and http.response.status_code>=500");
    filter.level.min = Some(okilum_core::log::Level::Info);
    filter.time = okilum_core::log::TimeWindow::LastHour;
    assert!(list.upsert(SavedQuery::capture("Server errors", &filter).unwrap()));
    let on_kibana = run(&kibana, &filter);
    assert!(
        !on_kibana.is_empty() && on_kibana.len() < kibana.index().len(),
        "positive control: the query keeps some of Kibana's rows and not all"
    );

    // Kept as JSON between launches, then applied to a file of another kind.
    let list = SavedQueries::from_json(&list.to_json()).unwrap();
    let saved = list.get("server errors").expect("found by name, any case");
    let restored = saved.to_filter().unwrap();
    assert_eq!(restored, filter, "everything the user chose comes back");
    assert_eq!(run(&kibana, &restored), on_kibana);

    let apache = sample("apache/apache-error.jsonl");
    let direct = run(&apache, &restored);
    // The same text typed by hand on that file gives the same rows.
    let mut typed = query("level>=warn and http.response.status_code>=500");
    typed.level = restored.level;
    typed.time = restored.time;
    assert_eq!(direct, run(&apache, &typed));
}

#[test]
fn a_saved_level_and_status_query_keeps_its_meaning_on_every_log() {
    // The query of the issue's acceptance text.
    let text = "level>=error";
    let mut list = SavedQueries::new();
    list.upsert(SavedQuery::capture("Errors", &query(text)).unwrap());
    let filter = list.get("Errors").unwrap().to_filter().unwrap();
    for (name, expected) in [
        ("kibana/kibana.ndjson", 8),
        ("apache/apache-error.jsonl", 35),
    ] {
        let file = sample(name);
        let rows = run(&file, &filter);
        assert_eq!(rows.len(), expected, "{name}");
        assert!(rows.len() < file.index().len(), "{name}: positive control");
    }
}

#[test]
fn exporting_the_21_filtered_rows_of_the_apache_sample_gives_exactly_those_raw_lines() {
    let file = sample("apache/apache-access.logfmt");
    let rows = run(&file, &query("status>=500"));
    assert_eq!(rows.len(), 21);
    assert!(rows.len() < file.index().len(), "positive control");

    // The expected lines, picked by reading the status off each line of the file.
    let text = std::str::from_utf8(file.bytes()).unwrap();
    assert!(!text.contains('\r'), "the sample uses LF");
    let expected: String = text
        .split_inclusive('\n')
        .filter(|line| {
            line.split(' ')
                .find_map(|token| token.strip_prefix("status="))
                .and_then(|status| status.parse::<u32>().ok())
                .is_some_and(|status| status >= 500)
        })
        .collect();
    assert_eq!(expected.lines().count(), 21);

    assert_eq!(
        export(&file, &rows, ExportFormat::Raw),
        expected.as_bytes(),
        "the export is exactly those lines, byte for byte"
    );

    // The same rows as JSON lines: one object each, the decoded fields typed.
    let json = String::from_utf8(export(&file, &rows, ExportFormat::JsonLines)).unwrap();
    let objects: Vec<serde_json::Value> = json
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(objects.len(), 21);
    for (object, &row) in objects.iter().zip(&rows) {
        let entry = &file.index().entries()[row as usize];
        assert_eq!(object["line"], entry.line());
        assert!(
            object["fields"]["status"].as_u64().unwrap() >= 500,
            "{object}"
        );
        assert!(object["fields"]["path"].is_string());
        assert!(object["time_ns"].is_i64(), "{object}");
    }
}

#[test]
fn exporting_every_row_of_a_file_reproduces_its_lines() {
    // Multi-line entries included: the C++ sample is an ASan report, whose blank
    // lines sit inside one entry. Blank lines between entries are not entries.
    for name in ["apache/apache-access.logfmt", "cpp/ordgw_tests-asan.log"] {
        let file = sample(name);
        let all: Vec<u32> = (0..file.index().len() as u32).collect();
        let exported = String::from_utf8(export(&file, &all, ExportFormat::Raw)).unwrap();
        let text = std::str::from_utf8(file.bytes()).unwrap();
        let lines = |text: &str| -> Vec<String> {
            text.lines()
                .filter(|line| !line.trim().is_empty())
                .map(str::to_owned)
                .collect()
        };
        assert_eq!(lines(&exported), lines(text), "{name}");
        assert!(exported.ends_with('\n'), "{name}: the last line is ended");
    }
    // A file without blank lines comes back byte for byte.
    let file = sample("apache/apache-access.logfmt");
    let all: Vec<u32> = (0..file.index().len() as u32).collect();
    assert_eq!(export(&file, &all, ExportFormat::Raw), file.bytes());
}
