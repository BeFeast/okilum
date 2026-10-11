//! Export of log rows (#1169): the raw lines exactly as they are in the file,
//! or one JSON object per row with the decoded record.
//!
//! Both write one row at a time to any `Write`, so a million rows stream to a
//! file without being held in memory.
//!
//! # Raw
//!
//! The bytes of the entry, then the line ending the file has after it: `\r\n`
//! or `\n`; a last line without one gets `\n`, so the exported lines never run
//! together. A multi-line entry (a stack trace) keeps its inner line endings.
//! Blank lines between entries are not entries and are not exported.
//!
//! # JSON lines
//!
//! One object per row:
//!
//! ```text
//! {"source":"a.log","line":12,"level":"error","time_ns":1759276800000000000,
//!  "fields":{"msg":"cache miss","status":500,"path":"/v1/notes"},
//!  "continuation":"  at frame one\n  at frame two"}
//! ```
//!
//! `source` is present only when asked for (a view of several files). `time_ns`
//! is the record's own time, absent without one. `fields` holds every field of
//! the record in source order, flattened to dotted keys as the viewer shows
//! them; a duplicate key is written again, not merged. Numbers, booleans and
//! null keep their JSON type, arrays and objects their structure, everything
//! else is a string. `continuation` is the entry's following lines. A row that
//! is not a record is `{"line":N,"level":"raw","raw":"..."}`.

use std::io::{self, Write};

use super::{block, Format, Level, LogEntry, Record, ValueKind};

/// How the rows are written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Raw,
    JsonLines,
}

/// One row to write: its entry, the file bytes the entry points into, and the
/// format that file was detected as.
#[derive(Clone, Copy, Debug)]
pub struct Row<'a> {
    pub data: &'a [u8],
    pub entry: &'a LogEntry,
    pub format: Format,
    /// The file the row came from, for a view of several files.
    pub source: Option<&'a str>,
}

pub fn write_row(out: &mut dyn Write, row: &Row, format: ExportFormat) -> io::Result<()> {
    match format {
        ExportFormat::Raw => write_raw(out, row),
        ExportFormat::JsonLines => write_json(out, row),
    }
}

/// The entry's bytes and the line ending that follows them in the file.
pub fn raw_with_ending<'a>(data: &'a [u8], entry: &LogEntry) -> (&'a [u8], &'static [u8]) {
    let range = entry.range();
    let bytes = data.get(range.clone()).unwrap_or_default();
    let ending: &'static [u8] = match data.get(range.end..) {
        Some(rest) if rest.starts_with(b"\r\n") => b"\r\n",
        _ => b"\n",
    };
    (bytes, ending)
}

pub fn write_raw(out: &mut dyn Write, row: &Row) -> io::Result<()> {
    let (bytes, ending) = raw_with_ending(row.data, row.entry);
    out.write_all(bytes)?;
    out.write_all(ending)
}

pub fn write_json(out: &mut dyn Write, row: &Row) -> io::Result<()> {
    out.write_all(json_line(row).as_bytes())?;
    out.write_all(b"\n")
}

/// The JSON object of a row, without the line ending.
pub fn json_line(row: &Row) -> String {
    let (bytes, _) = raw_with_ending(row.data, row.entry);
    let mut json = String::from("{");
    if let Some(source) = row.source {
        json.push_str(&format!("\"source\":{},", string(source)));
    }
    json.push_str(&format!("\"line\":{},", row.entry.line()));
    let record = (row.entry.level() != Level::Unparsed)
        .then(|| Record::parse(bytes, row.format))
        .flatten();
    let Some(record) = record else {
        json.push_str(&format!(
            "\"level\":\"raw\",\"raw\":{}}}",
            string(&String::from_utf8_lossy(bytes))
        ));
        return json;
    };
    json.push_str(&format!("\"level\":\"{}\"", level_name(row.entry.level())));
    if let Some(time) = row.entry.timestamp() {
        json.push_str(&format!(",\"time_ns\":{time}"));
    }
    json.push_str(",\"fields\":{");
    for (ix, field) in record.fields.iter().enumerate() {
        if ix > 0 {
            json.push(',');
        }
        json.push_str(&string(&field.key));
        json.push(':');
        json.push_str(&value(&field.value, field.kind));
    }
    json.push('}');
    if let Some(rest) = block::continuation(bytes) {
        json.push_str(&format!(
            ",\"continuation\":{}",
            string(&String::from_utf8_lossy(rest))
        ));
    }
    json.push('}');
    json
}

fn level_name(level: Level) -> &'static str {
    match level {
        Level::Trace => "trace",
        Level::Debug => "debug",
        Level::Info => "info",
        Level::Warn => "warn",
        Level::Error => "error",
        Level::Fatal => "fatal",
        Level::Unknown => "unknown",
        Level::Missing => "missing",
        Level::Unparsed => "raw",
    }
}

fn string(text: &str) -> String {
    serde_json::to_string(text).expect("a string is valid JSON")
}

/// A field value with its JSON type: what the source said, not a guess. A
/// number or an array that does not parse as JSON (a logfmt `status=2xx`
/// reads as a string anyway) is written as a string.
fn value(text: &str, kind: ValueKind) -> String {
    let typed = match kind {
        ValueKind::Number
        | ValueKind::Bool
        | ValueKind::Null
        | ValueKind::Array
        | ValueKind::Object => {
            serde_json::from_str::<serde_json::Value>(text)
                .ok()
                .filter(|parsed| match kind {
                    ValueKind::Number => parsed.is_number(),
                    ValueKind::Bool => parsed.is_boolean(),
                    ValueKind::Null => parsed.is_null(),
                    ValueKind::Array => parsed.is_array(),
                    _ => parsed.is_object(),
                })
        }
        ValueKind::String => None,
    };
    // `serde_json` rewrites a number's spelling (1.0 -> 1.0, 1e3 -> 1000.0);
    // the source's own digits are what a reader of the export expects.
    match (typed, kind) {
        (Some(_), ValueKind::Number | ValueKind::Bool | ValueKind::Null) => text.trim().to_owned(),
        (Some(parsed), _) => parsed.to_string(),
        (None, _) => string(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::LogIndex;

    fn rows<'a>(data: &'a [u8], index: &'a LogIndex) -> Vec<Row<'a>> {
        index
            .entries()
            .iter()
            .map(|entry| Row {
                data,
                entry,
                format: index.format(),
                source: None,
            })
            .collect()
    }

    fn raw(data: &[u8], which: &[usize]) -> Vec<u8> {
        let index = LogIndex::build_with_threads(data, 1);
        let rows = rows(data, &index);
        let mut out = Vec::new();
        for &at in which {
            write_raw(&mut out, &rows[at]).unwrap();
        }
        out
    }

    #[test]
    fn raw_rows_are_the_file_bytes_with_the_file_line_endings() {
        let data = b"{\"level\":\"info\",\"msg\":\"a\"}\r\n\r\n{\"level\":\"error\",\"msg\":\"b\"}\n{\"level\":\"warn\",\"msg\":\"c\"}";
        // CRLF stays CRLF, LF stays LF, a last line without one gets LF; the
        // blank line between entries is not an entry.
        assert_eq!(
            raw(data, &[0, 1, 2]),
            b"{\"level\":\"info\",\"msg\":\"a\"}\r\n{\"level\":\"error\",\"msg\":\"b\"}\n{\"level\":\"warn\",\"msg\":\"c\"}\n"
        );
        // A subset keeps the order it is given.
        assert_eq!(
            raw(data, &[2, 0]),
            b"{\"level\":\"warn\",\"msg\":\"c\"}\n{\"level\":\"info\",\"msg\":\"a\"}\r\n"
        );
    }

    #[test]
    fn a_multi_line_entry_keeps_its_inner_line_endings() {
        let data = b"{\"level\":\"error\",\"msg\":\"boom\"}\n  at a()\r\n  at b()\n{\"level\":\"info\",\"msg\":\"ok\"}\n";
        let index = LogIndex::build_with_threads(data, 1);
        assert_eq!(
            index.len(),
            2,
            "the indented lines belong to the first entry"
        );
        assert_eq!(
            raw(data, &[0, 1]),
            data.to_vec(),
            "the export of every row is the file"
        );
    }

    #[test]
    fn json_rows_decode_the_record_in_source_order_with_json_types() {
        let data = br#"{"ts":"2025-10-01T00:00:05Z","level":"error","msg":"cache \"miss\"","status":500,"ok":false,"ratio":1.50,"tags":["a","b"],"user":{"id":7},"none":null,"status":501}
"#;
        let index = LogIndex::build_with_threads(data, 1);
        let row = rows(data, &index).remove(0);
        let json = json_line(&row);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["line"], 1);
        assert_eq!(parsed["level"], "error");
        assert_eq!(parsed["time_ns"], 1_759_276_805_000_000_000i64);
        assert!(parsed.get("source").is_none() && parsed.get("continuation").is_none());
        let fields = &parsed["fields"];
        assert_eq!(fields["msg"], "cache \"miss\"");
        assert_eq!(fields["ok"], false);
        assert_eq!(fields["tags"], serde_json::json!(["a", "b"]));
        assert_eq!(fields["user.id"], 7);
        assert!(fields["none"].is_null());
        // The number keeps the source's spelling and a duplicate key is written again.
        assert!(json.contains("\"ratio\":1.50"), "{json}");
        assert!(
            json.contains("\"status\":500") && json.contains("\"status\":501"),
            "{json}"
        );
        // Key order is the source's.
        let order: Vec<_> = [
            "\"ts\"",
            "\"msg\"",
            "\"status\":500",
            "\"ratio\"",
            "\"user.id\"",
        ]
        .iter()
        .map(|key| json.find(key).unwrap_or_else(|| panic!("{key} in {json}")))
        .collect();
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{json}");
    }

    #[test]
    fn json_rows_carry_the_source_the_continuation_and_unparsed_lines_as_raw() {
        let data = b"{\"level\":\"error\",\"msg\":\"boom\",\"ts\":1759276800}\n  at a()\n  at b()\nplain words\n{\"level\":\"info\",\"msg\":\"ok\"}\n";
        let index = LogIndex::build_with_threads(data, 1);
        let mut rows = rows(data, &index);
        assert_eq!(rows.len(), 3);
        rows[0].source = Some("api.log");
        let first: serde_json::Value = serde_json::from_str(&json_line(&rows[0])).unwrap();
        assert_eq!(first["source"], "api.log");
        assert_eq!(first["level"], "error");
        assert_eq!(first["continuation"], "  at a()\n  at b()");
        assert_eq!(first["fields"]["msg"], "boom");
        let mut out = Vec::new();
        for row in &rows {
            write_row(&mut out, row, ExportFormat::JsonLines).unwrap();
        }
        let lines: Vec<serde_json::Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 3, "one object per row");
        assert_eq!(lines[1]["level"], "raw");
        assert_eq!(lines[1]["raw"], "plain words");
        assert_eq!(lines[1]["line"], 4);
        assert_eq!(lines[2]["fields"]["msg"], "ok");
    }

    #[test]
    fn a_line_that_is_not_a_record_is_exported_raw_in_json() {
        let data = b"{\"level\":\"info\",\"msg\":\"a\"}\nnot a record at all\n{\"level\":\"warn\",\"msg\":\"b\"}\n";
        let index = LogIndex::build_with_threads(data, 1);
        let rows = rows(data, &index);
        let object: serde_json::Value = serde_json::from_str(&json_line(&rows[1])).unwrap();
        assert_eq!(object["level"], "raw");
        assert_eq!(object["raw"], "not a record at all");
        assert_eq!(object["line"], 2);
        assert!(object.get("fields").is_none());
    }
}
