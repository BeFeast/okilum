use super::*;
use std::io::Write as _;

/// A synthetic capture shaped like Chrome DevTools' "Save all as HAR".
const CHROME: &str = r#"{
  "log": {
    "version": "1.2",
    "creator": { "name": "WebInspector", "version": "537.36" },
    "pages": [{ "startedDateTime": "2026-11-13T08:00:00.000Z", "id": "page_1", "title": "https://example.test/", "pageTimings": {} }],
    "entries": [
      {
        "_initiator": { "type": "other" },
        "_priority": "VeryHigh",
        "_resourceType": "document",
        "cache": {},
        "connection": "443",
        "pageref": "page_1",
        "request": {
          "method": "GET",
          "url": "https://example.test/app?x=1&y=two",
          "httpVersion": "http/2.0",
          "headers": [
            { "name": ":authority", "value": "example.test" },
            { "name": "cookie", "value": "session=s3cr3t; theme=dark" },
            { "name": "user-agent", "value": "Synthetic/1.0" }
          ],
          "queryString": [ { "name": "x", "value": "1" }, { "name": "y", "value": "two" } ],
          "cookies": [ { "name": "session", "value": "s3cr3t", "path": "/", "httpOnly": true, "secure": true } ],
          "headersSize": -1,
          "bodySize": 0
        },
        "response": {
          "status": 200,
          "statusText": "",
          "httpVersion": "http/2.0",
          "headers": [ { "name": "content-type", "value": "text/html; charset=utf-8" } ],
          "cookies": [],
          "content": { "size": 27, "mimeType": "text/html", "text": "<html><b>hello</b></html>\n" },
          "redirectURL": "",
          "headersSize": -1,
          "bodySize": -1,
          "_transferSize": 412,
          "_error": null
        },
        "serverIPAddress": "192.0.2.10",
        "startedDateTime": "2026-11-13T08:00:00.100Z",
        "time": 120.5,
        "timings": { "blocked": 1.5, "dns": -1, "ssl": -1, "connect": -1, "send": 0.2, "wait": 100.1, "receive": 18.7, "_blocked_queueing": 0.9 }
      },
      {
        "_resourceType": "fetch",
        "request": {
          "method": "POST",
          "url": "https://api.example.test/v1/login",
          "httpVersion": "http/2.0",
          "headers": [ { "name": "content-type", "value": "application/json" } ],
          "queryString": [],
          "cookies": [],
          "headersSize": -1,
          "bodySize": 37,
          "postData": { "mimeType": "application/json", "text": "{\"user\":\"ann\",\"password\":\"hunter2\"}" }
        },
        "response": {
          "status": 401,
          "statusText": "Unauthorized",
          "httpVersion": "http/2.0",
          "headers": [],
          "cookies": [],
          "content": { "size": 18, "mimeType": "application/json", "text": "eyJlcnJvciI6ImRlbmllZCJ9", "encoding": "base64" },
          "redirectURL": "",
          "headersSize": -1,
          "bodySize": -1,
          "_transferSize": 90
        },
        "startedDateTime": "2026-11-13T08:00:00.300Z",
        "time": 40,
        "timings": { "blocked": 0, "dns": -1, "connect": -1, "send": 0, "wait": 39, "receive": 1, "ssl": -1 }
      },
      {
        "_resourceType": "image",
        "request": { "method": "GET", "url": "https://cdn.example.test/logo.png", "httpVersion": "", "headers": [], "queryString": [], "cookies": [], "headersSize": -1, "bodySize": 0 },
        "response": {
          "status": 0, "statusText": "", "httpVersion": "", "headers": [], "cookies": [],
          "content": { "size": 0, "mimeType": "x-unknown" },
          "redirectURL": "", "headersSize": -1, "bodySize": -1, "_transferSize": 0,
          "_error": "net::ERR_BLOCKED_BY_CLIENT"
        },
        "startedDateTime": "2026-11-13T08:00:00.350Z",
        "time": 0.8,
        "timings": { "blocked": 0.8, "dns": -1, "connect": -1, "send": 0, "wait": 0, "receive": 0, "ssl": -1 }
      }
    ]
  }
}"#;

/// Shaped like Firefox's export: `browser`, offset timestamps, sizes from
/// headersSize/bodySize, no `_transferSize`.
const FIREFOX: &str = r#"{"log":{"version":"1.2","creator":{"name":"Firefox","version":"145.0"},"browser":{"name":"Firefox","version":"145.0"},"pages":[],
"entries":[{"pageref":"page_1","startedDateTime":"2026-11-13T10:00:00.000+02:00","request":{"bodySize":0,"method":"GET","url":"http://localhost:8080/","httpVersion":"HTTP/1.1","headers":[{"name":"Host","value":"localhost:8080"}],"cookies":[],"queryString":[],"headersSize":312},"response":{"status":304,"statusText":"Not Modified","httpVersion":"HTTP/1.1","headers":[],"cookies":[],"content":{"mimeType":"text/html; charset=utf-8","size":0,"text":""},"redirectURL":"","headersSize":250,"bodySize":0},"cache":{},"timings":{"blocked":0,"dns":0,"connect":0,"ssl":0,"send":0,"wait":5,"receive":0},"time":5,"_securityState":"insecure","serverIPAddress":"127.0.0.1","connection":"8080"}]}}"#;

fn index_text(text: &[u8]) -> (Vec<Entry>, Outcome) {
    let mut entries = Vec::new();
    let outcome = index(text, &AtomicBool::new(false), &mut |batch| {
        entries.extend(batch.entries)
    })
    .unwrap();
    (entries, outcome)
}

fn write(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn chrome_capture_lists_requests_with_list_fields() {
    let (entries, outcome) = index_text(CHROME.as_bytes());
    assert_eq!(outcome.problem, None);
    assert_eq!(outcome.creator.as_deref(), Some("WebInspector 537.36"));
    assert_eq!(entries.len(), 3);
    let doc = &entries[0];
    assert_eq!(&*doc.method, "GET");
    assert_eq!(doc.host(), "example.test");
    assert_eq!(doc.path(), "/app?x=1&y=two");
    assert_eq!(doc.status, Some(200));
    assert_eq!(doc.kind(), Kind::Document);
    assert_eq!(doc.type_label(), "document");
    assert_eq!(doc.size, Some(27));
    assert_eq!(doc.transferred, Some(412));
    assert_eq!(doc.time, Some(120.5));
    assert!(!doc.failed());
    assert_eq!(&CHROME.as_bytes()[doc.span.start as usize], &b'{');
    assert_eq!(&CHROME.as_bytes()[doc.span.end as usize - 1], &b'}');
    let login = &entries[1];
    assert_eq!((&*login.method, login.status), ("POST", Some(401)));
    assert!(login.failed());
    assert_eq!(login.kind(), Kind::Fetch);
    let blocked = &entries[2];
    assert_eq!(&*blocked.error, "net::ERR_BLOCKED_BY_CLIENT");
    assert!(blocked.failed());
    let gap = entries[1].started_ms.unwrap() - entries[0].started_ms.unwrap();
    assert!((gap - 200.).abs() < 0.001, "{gap}");
}

#[test]
fn firefox_capture_uses_header_and_body_sizes_and_offsets() {
    let (entries, outcome) = index_text(FIREFOX.as_bytes());
    assert_eq!(outcome.problem, None);
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e.host(), "localhost:8080");
    assert_eq!(e.path(), "/");
    assert_eq!(e.transferred, Some(250));
    assert_eq!(e.type_label(), "html");
    assert_eq!(e.kind(), Kind::Document);
    // 10:00+02:00 is 08:00Z.
    let utc = parse_time("2026-11-13T08:00:00Z").unwrap();
    assert_eq!(e.started_ms, Some(utc));
}

#[test]
fn details_read_one_entry_from_its_range() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(&dir, "c.har", CHROME.as_bytes());
    let (entries, _) = index_text(CHROME.as_bytes());
    let cancel = AtomicBool::new(false);
    let d = details(&path, entries[0].span.clone(), None, None, &cancel).unwrap();
    assert_eq!(d.method, "GET");
    assert_eq!(d.url.shown(), "https://example.test/app?x=1&y=two");
    assert_eq!(d.request_headers.len(), 3);
    assert_eq!(
        d.request_headers[1].value.shown(),
        "session=s3cr3t; theme=dark"
    );
    assert_eq!(d.query.len(), 2);
    assert_eq!(d.request_cookies[0].name, "session");
    assert!(d.request_cookies[0]
        .attributes
        .contains(&"httpOnly=true".to_owned()));
    assert_eq!(d.server, "192.0.2.10");
    // -1 phases are not applicable and omitted; custom phases are kept.
    let names: Vec<_> = d.timings.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        ["blocked", "send", "wait", "receive", "_blocked_queueing"]
    );
    let body = d.response_body.unwrap();
    assert_eq!(body.mime, "text/html");
    let bytes = body_bytes(
        &path,
        body.text.as_ref().unwrap(),
        false,
        1024,
        None,
        &cancel,
    )
    .unwrap();
    assert_eq!(bytes.bytes, b"<html><b>hello</b></html>\n");
    assert!(bytes.complete);

    let d = details(&path, entries[1].span.clone(), None, None, &cancel).unwrap();
    let post = d.request_body.unwrap();
    assert_eq!(
        post.text.unwrap().shown(),
        r#"{"user":"ann","password":"hunter2"}"#
    );
    let response = d.response_body.unwrap();
    assert!(response.base64());
    let decoded = body_bytes(
        &path,
        response.text.as_ref().unwrap(),
        true,
        1024,
        None,
        &cancel,
    )
    .unwrap();
    assert_eq!(decoded.bytes, br#"{"error":"denied"}"#);
    assert!(decoded.complete && !decoded.invalid_base64);
}

#[test]
fn base64_prefix_decodes_whole_quanta_and_reports_invalid_text() {
    use base64::Engine as _;
    let cancel = AtomicBool::new(false);
    let payload: Vec<u8> = (0..=255u8).cycle().take(10_000).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(&payload);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("unused");
    let text = Text::Inline(encoded);
    let cut = body_bytes(&path, &text, true, 1000, None, &cancel).unwrap();
    assert_eq!(cut.bytes, payload[..1000]);
    assert!(!cut.complete);
    let all = body_bytes(&path, &text, true, 20_000, None, &cancel).unwrap();
    assert_eq!(all.bytes, payload);
    assert!(all.complete);
    let bad = body_bytes(
        &path,
        &Text::Inline("not base64!".into()),
        true,
        100,
        None,
        &cancel,
    )
    .unwrap();
    assert!(bad.invalid_base64);
    assert_eq!(bad.bytes, b"notbase64!");
}

#[test]
fn escapes_and_surrogates_decode_and_lone_surrogates_do_not_panic() {
    let har = br#"{"log":{"entries":[{"request":{"method":"GET","url":"https://e.test/\u00e9\ud83d\ude00\ud800x\"q\\"}}]}}"#;
    let (entries, outcome) = index_text(har);
    assert_eq!(outcome.problem, None);
    assert_eq!(&*entries[0].url, "https://e.test/é😀\u{FFFD}x\"q\\");
}

#[test]
fn non_utf8_bytes_are_replaced_not_fatal() {
    let mut har =
        br#"{"log":{"entries":[{"request":{"method":"GET","url":"https://e.test/"#.to_vec();
    har.extend_from_slice(&[0xFF, 0xFE, b'a']);
    har.extend_from_slice(
        br#""},"response":{"status":200,"content":{"mimeType":"text/plain","text":""#,
    );
    har.extend_from_slice(&[0xC3, 0x28]);
    har.extend_from_slice(br#""}}}]}}"#);
    let (entries, outcome) = index_text(&har);
    assert_eq!(outcome.problem, None);
    assert_eq!(&*entries[0].url, "https://e.test/\u{FFFD}\u{FFFD}a");
    let dir = tempfile::tempdir().unwrap();
    let path = write(&dir, "n.har", &har);
    let d = details(
        &path,
        entries[0].span.clone(),
        None,
        None,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(d
        .response_body
        .unwrap()
        .text
        .unwrap()
        .shown()
        .contains('\u{FFFD}'));
}

#[test]
fn truncated_capture_keeps_complete_requests_and_reports_the_offset() {
    // Cut inside the third entry, as an interrupted export leaves it.
    let cut = CHROME.find("cdn.example.test").unwrap();
    let (entries, outcome) = index_text(&CHROME.as_bytes()[..cut]);
    assert_eq!(entries.len(), 2);
    match outcome.problem {
        Some(Problem::Damaged(error)) => {
            assert_eq!(error.offset, cut as u64);
            assert_eq!(error.message, "The file ends inside a string");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn malformed_and_foreign_json_are_named_not_panicking() {
    for (text, expected) in [
        (&b""[..], Problem::NotHar("The file is empty")),
        (b"[1,2]", Problem::NotHar("The file is not a JSON object")),
        (
            br#"{"a":1}"#,
            Problem::NotHar("There is no log.entries list"),
        ),
        (
            br#"{"log":{"entries":{}}}"#,
            Problem::NotHar("There is no log.entries list"),
        ),
        (
            br#"{"log":{"entries":[{"time":12x}]}}"#,
            Problem::Damaged(SyntaxError {
                offset: 27,
                message: "Invalid number",
            }),
        ),
        (
            br#"{"log":{"entries":[{"time":1} {"time":2}]}}"#,
            Problem::Damaged(SyntaxError {
                offset: 30,
                message: "Expected a comma or ]",
            }),
        ),
        (
            br#"{"log":{"entries":[]}} trailing"#,
            Problem::Damaged(SyntaxError {
                offset: 23,
                message: "Unexpected text after the end of the JSON value",
            }),
        ),
        (
            br#"{"log":{"entries":[{"x":"\q"}]}}"#,
            Problem::Damaged(SyntaxError {
                offset: 27,
                message: "Invalid escape in a string",
            }),
        ),
    ] {
        let (_, outcome) = index_text(text);
        assert_eq!(
            outcome.problem,
            Some(expected),
            "{}",
            String::from_utf8_lossy(text)
        );
    }
    // Deep nesting is bounded without recursion.
    let mut deep = br#"{"log":{"entries":[{"x":"#.to_vec();
    deep.extend(std::iter::repeat_n(b'[', 100_000));
    let (_, outcome) = index_text(&deep);
    assert!(
        matches!(&outcome.problem, Some(Problem::Damaged(e)) if e.message == "The JSON is nested too deeply"),
        "{outcome:?}"
    );
    // A valid empty capture is not a problem.
    let (entries, outcome) = index_text(br#"{"log":{"version":"1.2","entries":[]}}"#);
    assert!(entries.is_empty() && outcome.problem.is_none());
}

#[test]
fn deep_values_inside_an_entry_do_not_recurse_without_bound() {
    let mut har = br#"{"log":{"entries":[{"request":{"method":"GET","url":"u"},"x":"#.to_vec();
    har.extend(std::iter::repeat_n(b'[', 3000));
    har.extend(std::iter::repeat_n(b']', 3000));
    har.extend_from_slice(b"}]}}");
    let (entries, outcome) = index_text(&har);
    assert_eq!(outcome.problem, None);
    let dir = tempfile::tempdir().unwrap();
    let path = write(&dir, "deep.har", &har);
    let d = details(
        &path,
        entries[0].span.clone(),
        None,
        None,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(d.method, "GET");
}

#[test]
fn long_values_stay_on_disk_as_ranges() {
    let body = "x".repeat(INLINE_TEXT * 3) + "\\n" + &"y".repeat(10);
    let url = format!("https://e.test/{}", "p".repeat(LIST_TEXT * 2));
    let har = format!(
        r#"{{"log":{{"entries":[{{"request":{{"method":"GET","url":"{url}"}},"response":{{"status":200,"content":{{"mimeType":"text/plain","size":1,"text":"{body}"}}}}}}]}}}}"#
    );
    let (entries, _) = index_text(har.as_bytes());
    let e = &entries[0];
    assert!(e.url_cut);
    assert_eq!(e.url.len(), LIST_TEXT);
    let dir = tempfile::tempdir().unwrap();
    let path = write(&dir, "l.har", har.as_bytes());
    let cancel = AtomicBool::new(false);
    let d = details(&path, e.span.clone(), None, None, &cancel).unwrap();
    // The whole URL is in the details even though the list keeps a prefix.
    assert_eq!(d.url, Text::Inline(url.clone()));
    let text = d.response_body.unwrap().text.unwrap();
    let Text::Long { prefix, span } = &text else {
        panic!("body must stay on disk");
    };
    assert_eq!(prefix.len(), INLINE_TEXT);
    assert_eq!(&har.as_bytes()[span.start as usize], &b'"');
    let cut = body_bytes(&path, &text, false, 10, None, &cancel).unwrap();
    assert_eq!(
        (cut.bytes.as_slice(), cut.complete),
        (&b"xxxxxxxxxx"[..], false)
    );
    let all = body_bytes(&path, &text, false, usize::MAX, None, &cancel).unwrap();
    assert!(all.complete);
    assert_eq!(all.bytes.len(), INLINE_TEXT * 3 + 1 + 10);
    assert!(all.bytes.ends_with(b"x\nyyyyyyyyyy"));
}

#[test]
fn cancellation_stops_the_index_between_reads() {
    let mut har = String::from(r#"{"log":{"entries":["#);
    let entry = r#"{"request":{"method":"GET","url":"https://e.test/"},"response":{"status":200}}"#;
    for i in 0..20_000 {
        if i > 0 {
            har.push(',');
        }
        har.push_str(entry);
    }
    har.push_str("]}}");
    let cancel = AtomicBool::new(false);
    let mut seen = 0;
    let result = index(har.as_bytes(), &cancel, &mut |batch| {
        seen += batch.entries.len();
        cancel.store(true, Ordering::Relaxed);
    });
    assert_eq!(result, Err(Cancelled));
    assert!(seen < 20_000, "cancel must stop before the end: {seen}");
    // Control: the same input completes without cancellation.
    let (entries, outcome) = index_text(har.as_bytes());
    assert_eq!((entries.len(), outcome.problem), (20_000, None));
}

#[test]
fn the_list_stops_at_the_entry_limit() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(br#"{"log":{"entries":["#).unwrap();
    let entry = br#"{"request":{"method":"GET","url":"/"}}"#;
    for i in 0..MAX_ENTRIES + 10 {
        if i > 0 {
            file.write_all(b",").unwrap();
        }
        file.write_all(entry).unwrap();
    }
    file.write_all(b"]}}").unwrap();
    let (mut count, mut batches) = (0, 0);
    let outcome = index_file(file.path(), &AtomicBool::new(false), &mut |batch| {
        count += batch.entries.len();
        batches += 1;
    })
    .unwrap();
    assert!(outcome.limited);
    // Requests arrive in bounded batches while the file is read.
    assert!(batches >= MAX_ENTRIES / BATCH, "{batches}");
    assert_eq!((count, outcome.entries), (MAX_ENTRIES, MAX_ENTRIES));
    assert_eq!(outcome.problem, None);
}

/// A 100 MB capture: every body is large, none is kept by the index, and
/// batches arrive before the end so the list can show progress.
#[test]
fn a_100_mb_capture_indexes_with_bounded_memory_and_progressive_batches() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    let body = "Q".repeat(200_000);
    file.write_all(
        br#"{"log":{"version":"1.2","creator":{"name":"synthetic","version":"1"},"entries":["#,
    )
    .unwrap();
    let count = 520;
    for i in 0..count {
        if i > 0 {
            file.write_all(b",").unwrap();
        }
        write!(
            file,
            r#"{{"startedDateTime":"2026-11-13T08:00:{:02}.000Z","time":{i},"request":{{"method":"GET","url":"https://big.example.test/{i}","headers":[{{"name":"authorization","value":"Bearer t{i}"}}]}},"response":{{"status":200,"content":{{"size":200000,"mimeType":"application/octet-stream","encoding":"base64","text":"{body}"}},"_transferSize":150000}}}}"#,
            i % 60
        )
        .unwrap();
    }
    file.write_all(b"]}}").unwrap();
    file.flush().unwrap();
    let bytes = file.as_file().metadata().unwrap().len();
    assert!(bytes > 100_000_000, "{bytes}");
    let mut batches = Vec::new();
    let mut entries = Vec::new();
    let outcome = index_file(file.path(), &AtomicBool::new(false), &mut |batch| {
        batches.push((batch.entries.len(), batch.offset));
        entries.extend(batch.entries);
    })
    .unwrap();
    assert_eq!(outcome.problem, None);
    assert_eq!(entries.len(), count);
    assert!(batches.windows(2).all(|w| w[0].1 < w[1].1));
    // Bounded: the index holds list text only, nowhere near the file size.
    let kept: usize = entries
        .iter()
        .map(|e| {
            std::mem::size_of::<Entry>()
                + e.url.len()
                + e.method.len()
                + e.mime.len()
                + e.started.len()
                + e.resource_type.len()
                + e.error.len()
        })
        .sum();
    assert!(
        kept < 200_000,
        "{kept} bytes kept for {bytes} bytes of file"
    );
    // The last entry's details load lazily and keep the body on disk.
    let last = entries.last().unwrap();
    let d = details(
        file.path(),
        last.span.clone(),
        None,
        None,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        d.url.shown(),
        format!("https://big.example.test/{}", count - 1)
    );
    assert!(matches!(
        d.response_body.unwrap().text,
        Some(Text::Long { .. })
    ));
}

#[test]
fn split_url_handles_relative_and_credentials() {
    assert_eq!(
        split_url("https://u:p@h.test:8443/a?b#c"),
        ("h.test:8443", "/a?b#c")
    );
    assert_eq!(split_url("https://h.test"), ("h.test", "/"));
    assert_eq!(split_url("https://h.test?q"), ("h.test", "?q"));
    assert_eq!(split_url("/relative"), ("", "/relative"));
    assert_eq!(
        split_url("data:image/png;base64,AAA"),
        ("", "data:image/png;base64,AAA")
    );
}

#[test]
fn kinds_follow_resource_type_then_mime() {
    assert_eq!(Kind::of("xhr", "text/html"), Kind::Fetch);
    assert_eq!(
        Kind::of("", "application/javascript; charset=utf-8"),
        Kind::Script
    );
    assert_eq!(Kind::of("", "image/svg+xml"), Kind::Image);
    assert_eq!(Kind::of("", "font/woff2"), Kind::Font);
    assert_eq!(Kind::of("", "application/json"), Kind::Fetch);
    assert_eq!(Kind::of("ping", ""), Kind::Other);
    assert_eq!(Kind::of("", ""), Kind::Other);
}

// --- Review of PR 2878 (#2836): each case fails on head 9482eed4. ---

/// P1: one ignored field holding 5,000 arrays of 5,000 zeros (~50 MB) must not
/// become 25 M values; cookies with huge attribute lists hit the aggregate
/// budget, which the details report.
#[test]
fn review2878_detail_parse_skips_unknown_fields_and_has_an_aggregate_budget() {
    let mut har = String::from(
        r#"{"log":{"entries":[{"request":{"method":"GET","url":"https://e.test/","cookies":["#,
    );
    let attributes: String = (0..5000).map(|i| format!(r#","a{i}":"v""#)).collect();
    for i in 0..60 {
        if i > 0 {
            har.push(',');
        }
        har.push_str(&format!(r#"{{"name":"c{i}","value":"x"{attributes}}}"#));
    }
    har.push_str(r#"]},"_junk":["#);
    let zeros = format!("[{}0]", "0,".repeat(4999));
    for i in 0..5000 {
        if i > 0 {
            har.push(',');
        }
        har.push_str(&zeros);
    }
    har.push_str("]}]}}");
    assert!(har.len() > 50_000_000);
    let (entries, outcome) = index_text(har.as_bytes());
    assert_eq!(outcome.problem, None);
    let dir = tempfile::tempdir().unwrap();
    let path = write(&dir, "budget.har", har.as_bytes());
    let d = details(
        &path,
        entries[0].span.clone(),
        None,
        None,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(d.method, "GET");
    assert!(d.nodes <= MAX_DETAIL_NODES, "{} values built", d.nodes);
    assert!(d.incomplete, "the budget ran out and the details say so");
    // Control: the ignored field alone builds almost nothing.
    let small =
        r#"{"log":{"entries":[{"request":{"method":"GET","url":"u"},"_junk":[[0,0],[0,0]]}]}}"#;
    let (entries, _) = index_text(small.as_bytes());
    let path = write(&dir, "small.har", small.as_bytes());
    let d = details(
        &path,
        entries[0].span.clone(),
        None,
        None,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(d.nodes < 10 && !d.incomplete, "{}", d.nodes);
}

/// P1: decoded strings count against the byte budget.
#[test]
fn review2878_inline_strings_share_a_byte_budget() {
    let value = "v".repeat(INLINE_TEXT - 1);
    let headers: Vec<String> = (0..400)
        .map(|i| format!(r#"{{"name":"h{i}","value":"{value}"}}"#))
        .collect();
    let har = format!(
        r#"{{"log":{{"entries":[{{"request":{{"method":"GET","url":"u","headers":[{}]}}}}]}}}}"#,
        headers.join(",")
    );
    let (entries, _) = index_text(har.as_bytes());
    let dir = tempfile::tempdir().unwrap();
    let path = write(&dir, "bytes.har", har.as_bytes());
    let d = details(
        &path,
        entries[0].span.clone(),
        None,
        None,
        &AtomicBool::new(false),
    )
    .unwrap();
    let kept: usize = d
        .request_headers
        .iter()
        .map(|h| h.value.shown().len())
        .sum();
    assert!(kept <= MAX_DETAIL_BYTES, "{kept}");
    assert!(d.incomplete);
}

/// P1: non-finite and out-of-range numbers are unknown, never a panic or a
/// wrapped sum (debug test builds check overflow).
#[test]
fn review2878_huge_numbers_are_unknown_not_panics() {
    let har = br#"{"log":{"entries":[{"time":1e999,"request":{"method":"GET","url":"u"},"response":{"status":1e300,"headersSize":1e19,"bodySize":1e19,"content":{"size":-1e400}},"timings":{"send":1e999,"wait":1,"receive":1e300}}]}}"#;
    let (entries, outcome) = index_text(har);
    assert_eq!(outcome.problem, None);
    let e = &entries[0];
    assert_eq!(
        (e.time, e.status, e.transferred, e.size),
        (None, None, None, None)
    );
    let near = br#"{"log":{"entries":[{"response":{"headersSize":9007199254740992,"bodySize":9007199254740992}}]}}"#;
    let (entries, _) = index_text(near);
    assert_eq!(entries[0].transferred, Some(2 * 9_007_199_254_740_992));
    let dir = tempfile::tempdir().unwrap();
    let path = write(&dir, "n.har", har);
    let d = details(&path, e.span.clone(), None, None, &AtomicBool::new(false)).unwrap();
    assert_eq!(d.timings, [("wait".to_owned(), 1.)]);
    assert_eq!((d.time, d.status), (None, None));
    assert_eq!(d.response_body.unwrap().size, None);
}

/// P2: details refuse a file that is not the indexed revision.
#[test]
fn review2878_details_refuse_a_replaced_file_of_equal_length() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(&dir, "r.har", CHROME.as_bytes());
    let mut revision = None;
    let mut entries = Vec::new();
    let outcome = index_file(&path, &AtomicBool::new(false), &mut |batch| {
        revision = batch.revision;
        entries.extend(batch.entries)
    })
    .unwrap();
    assert_eq!(outcome.revision, revision);
    let cancel = AtomicBool::new(false);
    // Control: the same revision reads.
    assert!(details(&path, entries[0].span.clone(), revision, None, &cancel).is_ok());
    let replaced = CHROME.replace("app?x=1", "zzz?x=1");
    assert_eq!(replaced.len(), CHROME.len());
    std::fs::write(&path, &replaced).unwrap();
    let file = std::fs::File::options().write(true).open(&path).unwrap();
    file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000))
        .unwrap();
    assert_eq!(
        details(&path, entries[0].span.clone(), revision, None, &cancel),
        Err(DetailError::Changed)
    );
    let body = Text::Long {
        prefix: String::new(),
        span: entries[0].span.clone(),
    };
    assert_eq!(
        body_bytes(&path, &body, false, 10, revision, &cancel),
        Err(DetailError::Changed)
    );
}

/// P2: duplicate keys follow one rule (last wins) in the list and details.
#[test]
fn review2878_duplicate_keys_agree_between_list_and_details() {
    let har = br#"{"log":{"entries":[{"request":{"method":"GET","method":"DELETE","url":"https://a.test/1"},"request":{"url":"https://b.test/2","method":"PUT"},"response":{"status":200,"_error":"x","content":{"size":5}},"response":{"status":404},"_error":null}]}}"#;
    let (entries, _) = index_text(har);
    let e = &entries[0];
    let dir = tempfile::tempdir().unwrap();
    let path = write(&dir, "dup.har", har);
    let d = details(&path, e.span.clone(), None, None, &AtomicBool::new(false)).unwrap();
    assert_eq!((&*e.method, d.method.as_str()), ("PUT", "PUT"));
    assert_eq!(
        (&*e.url, d.url.shown()),
        ("https://b.test/2", "https://b.test/2")
    );
    assert_eq!((e.status, d.status), (Some(404), Some(404)));
    // The later response has no content and no error.
    assert_eq!(e.size, None);
    assert!(d.response_body.is_none());
    assert_eq!((&*e.error, d.error.as_str()), ("", ""));
    let one = br#"{"log":{"entries":[{"request":{"method":"GET","method":"DELETE","url":"u"}}]}}"#;
    let (entries, _) = index_text(one);
    let path = write(&dir, "one.har", one);
    let d = details(
        &path,
        entries[0].span.clone(),
        None,
        None,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        (&*entries[0].method, d.method.as_str()),
        ("DELETE", "DELETE")
    );
}

/// P2: JSON grammar, not Rust float syntax; raw control bytes are damage.
#[test]
fn review2878_json_grammar_is_enforced() {
    for (text, offset, message) in [
        (
            &br#"{"log":{"entries":[{"time":01}]}}"#[..],
            27,
            "Invalid number",
        ),
        (
            br#"{"log":{"entries":[{"time":1.}]}}"#,
            27,
            "Invalid number",
        ),
        (
            br#"{"log":{"entries":[{"time":.5}]}}"#,
            27,
            "Expected a value",
        ),
        (
            br#"{"log":{"entries":[{"time":+1}]}}"#,
            27,
            "Expected a value",
        ),
        (
            br#"{"log":{"entries":[{"time":1e}]}}"#,
            27,
            "Invalid number",
        ),
        (
            b"{\"log\":{\"entries\":[{\"x\":\"a\tb\"}]}}",
            26,
            "Unescaped control character in a string",
        ),
    ] {
        let (_, outcome) = index_text(text);
        assert_eq!(
            outcome.problem,
            Some(Problem::Damaged(SyntaxError { offset, message })),
            "{}",
            String::from_utf8_lossy(text)
        );
    }
    for valid in ["0", "-0", "1.5e-3", "1E+2", "123", "-0.0e0"] {
        assert!(json_number(valid.as_bytes()), "{valid}");
    }
    let (entries, outcome) = index_text(br#"{"log":{"entries":[{"time":1.5e1}]}}"#);
    assert_eq!((outcome.problem, entries[0].time), (None, Some(15.)));
}

/// Cancellation stops a detail read and a body decode before they finish.
#[test]
fn review2878_cancelled_detail_and_body_reads_stop() {
    let body = "B".repeat(20_000_000);
    let har = format!(
        r#"{{"log":{{"entries":[{{"request":{{"method":"GET","url":"u"}},"response":{{"status":200,"content":{{"mimeType":"text/plain","text":"{body}"}}}}}}]}}}}"#
    );
    let (entries, _) = index_text(har.as_bytes());
    let dir = tempfile::tempdir().unwrap();
    let path = write(&dir, "c.har", har.as_bytes());
    let cancelled = AtomicBool::new(true);
    assert_eq!(
        details(&path, entries[0].span.clone(), None, None, &cancelled),
        Err(DetailError::Cancelled)
    );
    let text = Text::Long {
        prefix: String::new(),
        span: (har.find("BBB").unwrap() as u64 - 1)
            ..(har.find("BBB").unwrap() + body.len() + 1) as u64,
    };
    assert_eq!(
        body_bytes(&path, &text, false, usize::MAX, None, &cancelled),
        Err(DetailError::Cancelled)
    );
    // Control: uncancelled, the same reads complete.
    let open = AtomicBool::new(false);
    assert!(details(&path, entries[0].span.clone(), None, None, &open).is_ok());
    let all = body_bytes(&path, &text, false, usize::MAX, None, &open).unwrap();
    assert_eq!(all.bytes.len(), body.len());
}

/// A detail parse notices cancellation between reads: flipping the flag after
/// the first buffer of a 20 MB entry stops it after at most one more read.
#[test]
fn review2878_detail_parse_stops_mid_read_when_cancelled() {
    struct Flip<'a> {
        data: &'a [u8],
        read: usize,
        reads: usize,
        cancel: &'a AtomicBool,
    }
    impl Read for Flip<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.reads += 1;
            if self.reads == 1 {
                // The worker is cancelled while it is in the middle of the entry.
                self.cancel.store(true, Ordering::Relaxed);
            }
            let n = buf.len().min(self.data.len() - self.read);
            buf[..n].copy_from_slice(&self.data[self.read..self.read + n]);
            self.read += n;
            Ok(n)
        }
    }
    let entry = format!(
        r#"{{"request":{{"method":"GET","url":"u","postData":{{"text":"{}"}}}}}}"#,
        "P".repeat(20_000_000)
    );
    let cancel = AtomicBool::new(false);
    let mut reader = Flip {
        data: entry.as_bytes(),
        read: 0,
        reads: 0,
        cancel: &cancel,
    };
    assert_eq!(
        parse_details(&mut reader, 0, &cancel),
        Err(DetailError::Cancelled)
    );
    assert_eq!(reader.reads, 1, "no read after the cancellation");
    assert!(
        reader.read < entry.len() / 10,
        "{} of {} bytes read",
        reader.read,
        entry.len()
    );
    // Control: the same entry parses completely without cancellation.
    let open = AtomicBool::new(false);
    let d = parse_details(entry.as_bytes(), 0, &open).unwrap();
    assert_eq!(d.method, "GET");
}

// --- Second review of PR 2878: each case fails on head a17c4319. ---

/// Syncthing keeps the source mtime: same length, same mtime, different
/// bytes must not show the new entry under the old row.
#[test]
fn review2878b_entry_hash_catches_a_replacement_with_the_same_size_and_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(&dir, "same.har", CHROME.as_bytes());
    let mut revision = None;
    let mut entries = Vec::new();
    index_file(&path, &AtomicBool::new(false), &mut |batch| {
        revision = batch.revision;
        entries.extend(batch.entries)
    })
    .unwrap();
    let cancel = AtomicBool::new(false);
    let first = &entries[0];
    // Control: the indexed bytes pass and hash the same at read time.
    let d = details(
        &path,
        first.span.clone(),
        revision,
        Some(first.hash),
        &cancel,
    )
    .unwrap();
    assert_eq!(d.method, "GET");
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    let replaced = CHROME.replacen(r#""method": "GET""#, r#""method": "PUT""#, 1);
    assert_ne!(replaced, CHROME);
    assert_eq!(replaced.len(), CHROME.len());
    std::fs::write(&path, &replaced).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    let file = std::fs::File::open(&path).unwrap();
    assert_eq!(
        Revision::of(&file).ok(),
        revision,
        "precondition: size and time unchanged"
    );
    assert_eq!(
        details(
            &path,
            first.span.clone(),
            revision,
            Some(first.hash),
            &cancel
        ),
        Err(DetailError::Changed)
    );
    // Other entries are untouched and still read.
    let second = &entries[1];
    assert!(details(
        &path,
        second.span.clone(),
        revision,
        Some(second.hash),
        &cancel
    )
    .is_ok());
}

/// Duplicate keys past MAX_ITEMS: last wins all the way, as in the list.
#[test]
fn review2878b_duplicates_past_the_item_limit_still_agree() {
    let mut har =
        String::from(r#"{"log":{"entries":[{"request":{"method":"GET","url":"u"},"response":{"#);
    for _ in 0..MAX_ITEMS {
        har.push_str(r#""status":200,"#);
    }
    har.push_str(r#""status":404}}]}}"#);
    let (entries, _) = index_text(har.as_bytes());
    let dir = tempfile::tempdir().unwrap();
    let path = write(&dir, "dups.har", har.as_bytes());
    let d = details(
        &path,
        entries[0].span.clone(),
        None,
        None,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!((entries[0].status, d.status), (Some(404), Some(404)));
    assert!(!d.incomplete, "duplicates take no extra fields");
    // More distinct fields than details keep: marked incomplete, not silent.
    let attributes: String = (0..=MAX_ITEMS).map(|i| format!(r#","a{i}":"v""#)).collect();
    let har = format!(
        r#"{{"log":{{"entries":[{{"request":{{"method":"GET","url":"u","cookies":[{{"name":"c","value":"x"{attributes}}}]}}}}]}}}}"#
    );
    let (entries, _) = index_text(har.as_bytes());
    let path = write(&dir, "wide.har", har.as_bytes());
    let d = details(
        &path,
        entries[0].span.clone(),
        None,
        None,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(d.incomplete);
}

// --- #2836 part 2: Save as writes whole bodies. ---

#[test]
fn part2_write_body_streams_whole_bodies_and_verifies_the_entry() {
    use base64::Engine as _;
    let payload: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(&payload);
    let text_body = format!("line1\\nline\\\"2\\u00e9{}", "t".repeat(400_000));
    let har = format!(
        r#"{{"log":{{"entries":[{{"request":{{"method":"GET","url":"u"}},"response":{{"status":200,"content":{{"mimeType":"image/png","encoding":"base64","text":"{encoded}"}}}}}},{{"request":{{"method":"GET","url":"v"}},"response":{{"status":200,"content":{{"mimeType":"text/plain","text":"{text_body}"}}}}}}]}}}}"#
    );
    let dir = tempfile::tempdir().unwrap();
    let path = write(&dir, "save.har", har.as_bytes());
    let mut revision = None;
    let mut entries = Vec::new();
    index_file(&path, &AtomicBool::new(false), &mut |batch| {
        revision = batch.revision;
        entries.extend(batch.entries)
    })
    .unwrap();
    let cancel = AtomicBool::new(false);
    let body = |index: usize| {
        let e = &entries[index];
        details(&path, e.span.clone(), revision, Some(e.hash), &cancel)
            .unwrap()
            .response_body
            .unwrap()
    };
    // Binary (base64) beyond the 256 KB preview, written whole.
    let image = body(0);
    assert!(matches!(image.text, Some(Text::Long { .. })));
    let mut out = Vec::new();
    let e = &entries[0];
    let written = write_body(
        &path,
        image.text.as_ref().unwrap(),
        image.base64(),
        Some((&e.span, revision, e.hash)),
        &mut out,
        &cancel,
    )
    .unwrap();
    assert_eq!((written as usize, out == payload), (payload.len(), true));
    // Text with escapes, written decoded and whole.
    let text = body(1);
    let mut out = Vec::new();
    write_body(
        &path,
        text.text.as_ref().unwrap(),
        false,
        None,
        &mut out,
        &cancel,
    )
    .unwrap();
    let expected = format!("line1\nline\"2\u{e9}{}", "t".repeat(400_000));
    assert_eq!(String::from_utf8(out).unwrap(), expected);
    // A changed request is refused before anything is written.
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::fs::write(&path, har.replacen("image/png", "image/pnh", 1)).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    let mut out = Vec::new();
    assert_eq!(
        write_body(
            &path,
            image.text.as_ref().unwrap(),
            true,
            Some((&e.span, revision, e.hash)),
            &mut out,
            &cancel
        ),
        Err(DetailError::Changed)
    );
    assert!(out.is_empty());
}
