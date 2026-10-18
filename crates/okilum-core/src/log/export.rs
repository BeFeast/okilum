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
//!
//! # CSV
//!
//! The columns of the view, for a spreadsheet (#1387): a header row, then one
//! row per entry, RFC 4180: `CRLF` between rows, a field with a comma, a quote
//! or a line break in double quotes, a quote inside doubled. A multi-line entry
//! (a stack trace) is therefore one row. No byte order mark.
//!
//! ```text
//! time_utc,source,level,status,path,message
//! 2025-10-01 00:00:05.000,api.log,error,500,/v1/notes,"cache miss
//!   at frame one"
//! ```
//!
//! The columns are, in the order the viewer draws them: `time_utc` (the
//! record's own time, `YYYY-MM-DD HH:MM:SS.mmm`, empty without one), `source`
//! (only for a view of several files), `level` (as in the JSON export), the
//! field columns the view shows, each with the first field of that name
//! (empty where the record has none), and `message`: the record's message
//! field followed by the entry's remaining lines, or for a row that is not a
//! record its text without colour codes. Fields the view does not show as a
//! column are not exported; JSON lines has them all.
//!
//! Values are written as they are in the log. A spreadsheet reads a cell that
//! starts with `=`, `+`, `-` or `@` as a formula; the export does not alter a
//! value to prevent that.

use std::io::{self, Write};

use super::{ansi, block, timestamp, Format, Level, LogEntry, Record, Role, ValueKind};

/// How the rows are written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Raw,
    JsonLines,
    Csv(CsvColumns),
}

/// The columns of a CSV export: the field columns the view shows, in their
/// order, and whether the view merges several files (a `source` column).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CsvColumns {
    fields: Vec<String>,
    source: bool,
}

impl CsvColumns {
    pub fn new(fields: Vec<String>, source: bool) -> Self {
        Self { fields, source }
    }

    /// The header row's names.
    pub fn header(&self) -> Vec<&str> {
        let mut names = vec!["time_utc"];
        if self.source {
            names.push("source");
        }
        names.push("level");
        names.extend(self.fields.iter().map(String::as_str));
        names.push("message");
        names
    }
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

/// What a file of `format` starts with, before any row: the CSV header row;
/// nothing for the other formats.
pub fn write_header(out: &mut dyn Write, format: &ExportFormat) -> io::Result<()> {
    match format {
        ExportFormat::Csv(columns) => {
            let names = columns.header();
            out.write_all(csv_line(names.iter().copied()).as_bytes())
        }
        ExportFormat::Raw | ExportFormat::JsonLines => Ok(()),
    }
}

pub fn write_row(out: &mut dyn Write, row: &Row, format: &ExportFormat) -> io::Result<()> {
    match format {
        ExportFormat::Raw => write_raw(out, row),
        ExportFormat::JsonLines => write_json(out, row),
        ExportFormat::Csv(columns) => write_csv(out, row, columns),
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

pub fn write_csv(out: &mut dyn Write, row: &Row, columns: &CsvColumns) -> io::Result<()> {
    let (bytes, _) = raw_with_ending(row.data, row.entry);
    let record = record_of(row, bytes);
    let mut cells: Vec<String> = Vec::with_capacity(columns.fields.len() + 3);
    cells.push(
        row.entry
            .timestamp()
            .map_or_else(String::new, timestamp::sortable_utc),
    );
    if columns.source {
        cells.push(row.source.unwrap_or_default().to_owned());
    }
    cells.push(level_name(row.entry.level()).to_owned());
    for key in &columns.fields {
        // The viewer's own rule: the first field of that name.
        cells.push(
            record
                .as_ref()
                .and_then(|record| record.fields.iter().find(|field| &field.key == key))
                .map(|field| field.value.clone())
                .unwrap_or_default(),
        );
    }
    cells.push(match &record {
        Some(record) => {
            let mut message = record
                .role(Role::Message)
                .map(|field| field.value.clone())
                .unwrap_or_default();
            if let Some(rest) = block::continuation(bytes) {
                if !message.is_empty() {
                    message.push('\n');
                }
                message.push_str(&String::from_utf8_lossy(rest));
            }
            message
        }
        None => String::from_utf8_lossy(&ansi::strip(bytes)).into_owned(),
    });
    out.write_all(csv_line(cells.iter().map(String::as_str)).as_bytes())
}

/// One CSV row with its `CRLF`.
fn csv_line<'a>(cells: impl Iterator<Item = &'a str>) -> String {
    let mut line = String::new();
    for (at, cell) in cells.enumerate() {
        if at > 0 {
            line.push(',');
        }
        if cell.contains([',', '"', '\r', '\n']) {
            line.push('"');
            for ch in cell.chars() {
                if ch == '"' {
                    line.push('"');
                }
                line.push(ch);
            }
            line.push('"');
        } else {
            line.push_str(cell);
        }
    }
    line.push_str("\r\n");
    line
}

/// The decoded record of a row; `None` for a line that is not one.
fn record_of(row: &Row, bytes: &[u8]) -> Option<Record> {
    (row.entry.level() != Level::Unparsed)
        .then(|| Record::parse(bytes, row.format))
        .flatten()
}

/// The JSON object of a row, without the line ending.
pub fn json_line(row: &Row) -> String {
    let (bytes, _) = raw_with_ending(row.data, row.entry);
    let mut json = String::from("{");
    if let Some(source) = row.source {
        json.push_str(&format!("\"source\":{},", string(source)));
    }
    json.push_str(&format!("\"line\":{},", row.entry.line()));
    let Some(record) = record_of(row, bytes) else {
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
            write_row(&mut out, row, &ExportFormat::JsonLines).unwrap();
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

    /// The CSV file of every entry of `data`, header first.
    fn csv_file(data: &[u8], columns: &CsvColumns, source: Option<&str>) -> Vec<u8> {
        let index = LogIndex::build_with_threads(data, 1);
        let format = ExportFormat::Csv(columns.clone());
        let mut out = Vec::new();
        write_header(&mut out, &format).unwrap();
        for mut row in rows(data, &index) {
            row.source = source;
            write_row(&mut out, &row, &format).unwrap();
        }
        out
    }

    /// Reads a CSV file with the `csv` crate, strictly: every row must have as
    /// many fields as the header.
    fn parse_csv(bytes: &[u8]) -> Result<(Vec<String>, Vec<Vec<String>>), csv::Error> {
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .flexible(false)
            .from_reader(bytes);
        let header = reader.headers()?.iter().map(str::to_owned).collect();
        let mut rows = Vec::new();
        for record in reader.records() {
            rows.push(record?.iter().map(str::to_owned).collect());
        }
        Ok((header, rows))
    }

    fn strings(cells: &[&str]) -> Vec<String> {
        cells.iter().map(|cell| (*cell).to_owned()).collect()
    }

    #[test]
    fn csv_parses_back_to_the_same_values_one_row_per_entry() {
        let data = "\
{\"ts\":\"2025-10-01T00:00:05Z\",\"level\":\"error\",\"msg\":\"cache miss, \\\"cold\\\"\",\"status\":500,\"path\":\"/v1/notes\"}
  at frame one
  at frame two
{\"ts\":\"2025-10-01T00:00:06Z\",\"level\":\"info\",\"msg\":\"ok\",\"status\":200}
plain words, with a comma
{\"ts\":\"2025-10-01T00:00:07Z\",\"level\":\"warn\",\"msg\":\"two\\nlines\",\"path\":\"é ü ✓\",\"status\":\"404\"}
{\"level\":\"info\",\"msg\":\"no time\"}
"
        .as_bytes();
        let columns = CsvColumns::new(strings(&["status", "path"]), false);
        let file = csv_file(data, &columns, None);
        let (header, parsed) = parse_csv(&file).unwrap();
        assert_eq!(
            header,
            strings(&["time_utc", "level", "status", "path", "message"])
        );
        assert_eq!(
            parsed,
            vec![
                strings(&[
                    "2025-10-01 00:00:05.000",
                    "error",
                    "500",
                    "/v1/notes",
                    "cache miss, \"cold\"\n  at frame one\n  at frame two",
                ]),
                strings(&["2025-10-01 00:00:06.000", "info", "200", "", "ok"]),
                strings(&["", "raw", "", "", "plain words, with a comma"]),
                strings(&[
                    "2025-10-01 00:00:07.000",
                    "warn",
                    "404",
                    "é ü ✓",
                    "two\nlines"
                ]),
                strings(&["", "info", "", "", "no time"]),
            ],
            "one row per entry, the chosen columns in their order"
        );
        // RFC 4180: CRLF row ends, no byte order mark.
        assert!(file.ends_with(b"\r\n") && !file.starts_with(&[0xEF, 0xBB, 0xBF]));
        // Negative control: the stack trace and the two-line message are inside
        // quotes. Cut at line breaks the file has far more pieces than rows.
        let pieces = String::from_utf8(file).unwrap().lines().count();
        assert!(
            pieces > parsed.len() + 1,
            "{pieces} lines for {} rows",
            parsed.len()
        );
    }

    #[test]
    fn csv_names_the_source_of_a_merged_view_and_keeps_crlf_stack_traces_whole() {
        let data = b"{\"level\":\"error\",\"msg\":\"boom\",\"ts\":1759276800}\r\n  at a()\r\n  at b()\r\n{\"level\":\"info\",\"msg\":\"ok\"}\r\n";
        let columns = CsvColumns::new(Vec::new(), true);
        let file = csv_file(data, &columns, Some("api, v2.log"));
        let (header, parsed) = parse_csv(&file).unwrap();
        assert_eq!(header, strings(&["time_utc", "source", "level", "message"]));
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed[0][..3],
            strings(&["2025-10-01 00:00:00.000", "api, v2.log", "error"])[..]
        );
        assert!(
            parsed[0][3].starts_with("boom\n") && parsed[0][3].ends_with("  at b()"),
            "{:?}",
            parsed[0][3]
        );
        assert_eq!(parsed[1], strings(&["", "api, v2.log", "info", "ok"]));
    }

    #[test]
    fn csv_quotes_only_what_needs_it() {
        let cells = ["a,b", "say \"hi\"", "line\nbreak", "cr\rhere", "plain", ""];
        assert_eq!(
            csv_line(cells.into_iter()),
            "\"a,b\",\"say \"\"hi\"\"\",\"line\nbreak\",\"cr\rhere\",plain,\r\n"
        );
    }

    #[test]
    fn the_csv_reader_of_the_tests_rejects_an_unquoted_comma() {
        // The reader above is strict about the field count, so a quoting bug in
        // the export fails a test instead of passing through.
        assert!(parse_csv(b"a,b,c\r\n1,2\r\n").is_err());
        assert!(parse_csv(b"a,b\r\n1,x,y\r\n").is_err());
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
