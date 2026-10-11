//! The sample pack's "Try" queries (#1155, #1158) over its own files. The
//! expected counts were computed by a separate Python evaluation of the same
//! queries (see tests/fixtures/log/samples/README.md), so a disagreement is a
//! bug on one side, not a re-statement of what the Rust evaluator returns.
use std::path::Path;

use okilum_core::log::{filter, FilterGenerations, LogFile, LogFilter, Query};

const TRY: &[(&str, &str, usize)] = &[
    ("kibana/kibana.ndjson", "level>=error", 8),
    ("kibana/kibana.ndjson", "http.response.status_code>=500", 4),
    ("kibana/kibana.ndjson", "logger=plugins.alerting", 1),
    ("apache/apache-access.logfmt", "status>=500", 21),
    ("apache/apache-access.logfmt", "path~=.env", 6),
    ("apache/apache-access.logfmt", "duration_us>1000000", 6),
    ("apache/apache-error.jsonl", "level>=error", 35),
    ("apache/apache-error.jsonl", "module=proxy_http", 6),
    (
        "kubernetes/kubernetes-fluentbit.ndjson",
        "kubernetes.namespace_name=shop",
        38,
    ),
    (
        "kubernetes/kubernetes-fluentbit.ndjson",
        "kubernetes.pod_name~=checkout-api-85b4 and level>=warn",
        5,
    ),
    ("kubernetes/kubelet.jsonl", "exists(err)", 4),
    ("kubernetes/kubelet.jsonl", "err~=CrashLoopBackOff", 4),
    (
        "os/linux-rsyslog.jsonl",
        "programname=kernel and level>=error",
        6,
    ),
    ("os/linux-rsyslog.jsonl", "severity=alert", 1),
    ("cpp/ordgw.spdlog.jsonl", "level>=warn", 9),
    ("cpp/ordgw.spdlog.jsonl", "logger=fix.session", 6),
    ("cpp/ordgw.spdlog.jsonl", "tid=4414", 4),
    ("cpp/ordgw.spdlog.jsonl", "src~=risk_engine", 2),
    ("cpp/ordgw.quill.jsonl", "symbol=\"FDAX Z6\"", 6),
    ("cpp/ordgw.quill.jsonl", "exposure_eur>2000000", 1),
    ("cpp/ordgw_tests-asan.log", "level>=warn", 1),
    ("os/windows-winlogbeat.ndjson", "winlog.event_id=4625", 6),
    ("os/windows-winlogbeat.ndjson", "winlog.channel=Security", 8),
];

/// The text-format files of the pack (`native-formats/`, #1162). Counts are
/// from grep and awk over the same files, independent of the Rust scanners.
const NATIVE: &[(&str, &str, usize)] = &[
    (
        "native-formats/apache-access-combined.log",
        "status>=500",
        21,
    ),
    (
        "native-formats/apache-access-combined.log",
        "status>=400 and status<500",
        41,
    ),
    (
        "native-formats/apache-access-combined.log",
        "method=POST",
        132,
    ),
    (
        "native-formats/cpp/ordgw.spdlog-default.log",
        "level>=warn",
        10,
    ),
    (
        "native-formats/cpp/ordgw.spdlog-default.log",
        "logger=fix.session",
        6,
    ),
    ("native-formats/cpp/ordgw.glog.log", "level>=warn", 10),
    ("native-formats/cpp/ordgw.glog.log", "tid=4414", 4),
    ("native-formats/cpp/ordgw.glog.log", "src~=risk_engine", 2),
];

fn matches(file: &LogFile, query: &str) -> usize {
    let filter = LogFilter {
        query: Some(Query::parse(query).unwrap_or_else(|error| panic!("{query}: {error}"))),
        ..Default::default()
    };
    let ticket = FilterGenerations::default().next();
    filter::evaluate(&filter, file.index(), file.bytes(), &ticket)
        .expect("a current ticket finishes")
        .rows
        .len()
}

#[test]
fn every_try_query_returns_the_independently_counted_records() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/log/samples");
    let mut wrong = Vec::new();
    for (name, query, expected) in TRY {
        let file =
            LogFile::open(&root.join(name)).unwrap_or_else(|error| panic!("{name}: {error}"));
        // Positive control: the file has more records than the query keeps,
        // so a filter that passed everything could not meet the count.
        assert!(file.index().len() > *expected, "{name} is too small");
        let got = matches(&file, query);
        if got != *expected {
            wrong.push(format!("{name}: `{query}` -> {got}, expected {expected}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn text_format_files_answer_queries_on_their_fields() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/log/samples");
    let mut wrong = Vec::new();
    for (name, query, expected) in NATIVE {
        let file =
            LogFile::open(&root.join(name)).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(file.index().len() > *expected, "{name} is too small");
        let got = matches(&file, query);
        if got != *expected {
            wrong.push(format!("{name}: `{query}` -> {got}, expected {expected}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn a_blank_query_filters_nothing_and_a_typo_reports_its_column() {
    assert!(!LogFilter {
        query: Some(Query::parse("  ").unwrap()),
        ..Default::default()
    }
    .is_active());
    let source = "status>=500 and lvl ~ warn";
    let error = Query::parse(source).unwrap_err();
    assert_eq!(error.column(source), 20, "{error}");
}
