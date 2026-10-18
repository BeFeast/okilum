//! User-defined text formats. Compiled once and shared by index workers.

use std::borrow::Cow;
use std::sync::Arc;

use regex::bytes::Regex;
use serde::{Deserialize, Serialize};

use super::record::{Escape, RawValue, ValueKind};

/// Durable user configuration; never part of the disposable log index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Definition {
    pub name: String,
    pub pattern: String,
}

impl Definition {
    pub fn compile(&self) -> Result<Arc<RegexParser>, regex::Error> {
        let regex = Regex::new(&self.pattern)?;
        Ok(Arc::new(RegexParser {
            definition: self.clone(),
            regex,
        }))
    }
}

#[derive(Debug)]
pub struct RegexParser {
    definition: Definition,
    regex: Regex,
}

impl PartialEq for RegexParser {
    fn eq(&self, other: &Self) -> bool {
        self.definition == other.definition
    }
}
impl Eq for RegexParser {}

impl RegexParser {
    pub fn named_groups(&self) -> impl Iterator<Item = &str> {
        self.regex.capture_names().flatten()
    }

    pub fn definition(&self) -> &Definition {
        &self.definition
    }

    pub(super) fn scan<'a>(
        &self,
        line: &'a [u8],
        visit: &mut dyn FnMut(&[Cow<'a, str>], RawValue<'a>),
    ) -> Result<(), ()> {
        let captures = self.regex.captures(line).ok_or(())?;
        for name in self.regex.capture_names().flatten() {
            if let Some(value) = captures.name(name) {
                visit(
                    &[Cow::Owned(name.to_owned())],
                    RawValue {
                        kind: ValueKind::String,
                        raw: value.as_bytes(),
                        escape: Escape::None,
                    },
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{
        detect, elapsed, export, filter, FilterGenerations, Format, Histogram, Level, LogFile,
        LogFilter, LogIndex, Query, Role,
    };

    const PATTERN: &str = r"^(?P<ts>[^|]+)\|(?P<level>[^|]+)\|(?P<worker>[^|]+)\|(?P<msg>.*)$";
    const DATA: &[u8] = include_bytes!("../../tests/fixtures/log/regex-pipe.log");

    fn parser(pattern: &str) -> Arc<RegexParser> {
        Definition {
            name: "Pipe log".into(),
            pattern: pattern.into(),
        }
        .compile()
        .unwrap()
    }

    #[test]
    fn invalid_regex_reports_the_compile_error() {
        let error = Definition {
            name: "Broken".into(),
            pattern: "(?P<ts>".into(),
        }
        .compile()
        .unwrap_err();
        assert!(error.to_string().contains("unclosed group"));
    }

    #[test]
    fn named_groups_and_counts_keep_every_unmatched_line() {
        assert_eq!(
            detect(DATA),
            Format::Plain,
            "positive control: unknown format"
        );
        let regex = parser(PATTERN);
        let index = LogIndex::build_regex(DATA, regex.clone());
        assert!(Arc::ptr_eq(index.regex().unwrap(), &regex));
        assert_eq!(index.regex_counts(), Some((3, 1)));
        assert_eq!(index.len(), 4, "unmatched line remains a row");
        assert_eq!(index.entries()[2].level(), Level::Unparsed);
        assert_eq!(index.raw(DATA, 2), Some(b"  worker restarted".as_slice()));
        assert!(index.record(index.raw(DATA, 2).unwrap()).is_none());
        let record = index.record(index.raw(DATA, 0).unwrap()).unwrap();
        assert_eq!(
            record.role(Role::Time).unwrap().value,
            "2025-10-01T00:00:00.120Z"
        );
        assert_eq!(record.role(Role::Level).unwrap().value, "INFO");
        assert_eq!(record.role(Role::Message).unwrap().value, "started");
        assert_eq!(
            record
                .rest()
                .map(|field| (&*field.key, &*field.value))
                .collect::<Vec<_>>(),
            [("worker", "alpha")]
        );
        let zero = LogIndex::build_regex(DATA, parser("^never$"));
        assert_eq!(zero.regex_counts(), Some((0, 4)));
        assert_eq!(zero.len(), 4);
        assert!(zero
            .entries()
            .iter()
            .all(|entry| entry.level() == Level::Unparsed));
        let full = LogIndex::build_regex(DATA, parser("(?P<msg>.*)"));
        assert_eq!(full.regex_counts(), Some((4, 0)));
        let blanks = LogIndex::build_regex(b"\n\r\nnoise\n", parser("^never$"));
        assert_eq!(blanks.regex_counts(), Some((0, 3)));
        assert_eq!(blanks.len(), 3, "even unmatched blank lines stay visible");
    }

    #[test]
    fn only_standard_named_groups_get_roles_and_optional_groups_can_be_absent() {
        let regex = parser(r"^(?P<time>\d+):(?P<logger>\w+):(?P<msg>[^:]+)(?::(?P<level>\w+))?$");
        let data = b"1759276800:worker:started\n1759276801:worker:failed:ERROR\n";
        let index = LogIndex::build_regex(data, regex);
        assert_eq!(index.regex_counts(), Some((2, 0)));
        assert_eq!(index.entries()[0].level(), Level::Missing);
        assert_eq!(index.entries()[1].level(), Level::Error);
        assert_eq!(
            index.stats().timestamped,
            0,
            "time is a field; only ts is the timestamp"
        );
        let record = index.record(index.raw(data, 0).unwrap()).unwrap();
        assert!(
            record.role(Role::Logger).is_none(),
            "logger remains a field"
        );
        assert_eq!(record.rest().count(), 2);
    }

    #[test]
    fn regex_fixture_queries_histogram_delta_and_export() {
        // #1155's existing files are all recognized. The adjacent pipe fixture
        // exercises the extension path without changing the owner's pack.
        let samples =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/log/samples");
        let mut files = 0;
        for entry in walkdir::WalkDir::new(samples) {
            let entry = entry.unwrap();
            if !entry.file_type().is_file()
                || entry.path().extension().is_some_and(|ext| ext == "md")
            {
                continue;
            }
            let file = LogFile::open(entry.path()).unwrap();
            assert_ne!(
                file.index().format(),
                Format::Plain,
                "{}",
                entry.path().display()
            );
            files += 1;
        }
        assert_eq!(files, 17, "positive control: inspected the sample pack");
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/log/regex-pipe.log");
        let file = LogFile::open(&path)
            .unwrap()
            .with_regex(parser(PATTERN))
            .unwrap();
        let index = file.index();
        let data = file.bytes();
        assert_eq!(index.format(), Format::Regex);
        let filter = LogFilter {
            query: Some(Query::parse("level>=warn and worker=beta").unwrap()),
            ..Default::default()
        };
        let result =
            filter::evaluate(&filter, index, data, &FilterGenerations::default().next()).unwrap();
        assert_eq!(result.rows, [1]);
        let text_filter = LogFilter {
            text: "worker=beta".into(),
            ..Default::default()
        };
        assert_eq!(
            filter::evaluate(
                &text_filter,
                index,
                data,
                &FilterGenerations::default().next()
            )
            .unwrap()
            .rows,
            [1]
        );
        let mut histogram = Histogram::new();
        histogram.add_index(index, None);
        assert_eq!(
            histogram.timed(),
            4,
            "the unmatched row inherits the preceding time for the histogram"
        );
        assert_eq!(histogram.untimed(), 0);
        let start = file.record(0).unwrap().stamp().unwrap();
        let end = file.record(1).unwrap().stamp().unwrap();
        assert_eq!(elapsed::between(start, end), "+1.234 s");
        let row = export::Row {
            window: Some(file.window()),
            data,
            entry: &index.entries()[1],
            format: index.format(),
            regex: index.regex().map(|regex| regex.as_ref()),
            source: None,
        };
        let json: serde_json::Value = serde_json::from_str(&export::json_line(&row)).unwrap();
        assert_eq!(json["fields"]["worker"], "beta");
    }

    #[test]
    fn regex_parallel_chunks_and_tail_preserve_the_definition() {
        use std::io::Write as _;
        let data = DATA.repeat(40_000);
        let regex = parser(PATTERN);
        let one = LogIndex::build_regex_with_threads(&data, Some(regex.clone()), 1);
        let four = LogIndex::build_regex_with_threads(&data, Some(regex.clone()), 4);
        assert_eq!(one, four, "same bytes and parser across parallel chunks");
        assert_eq!(four.regex_counts(), Some((120_000, 40_000)));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("custom.log");
        std::fs::write(&path, DATA).unwrap();
        let mut tail = crate::log::LogTail::open(&path)
            .unwrap()
            .with_regex(regex)
            .unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(DATA)
            .unwrap();
        tail.poll().unwrap();
        assert_eq!(tail.index().regex_counts(), Some((6, 2)));
        assert_eq!(
            tail.record(1).unwrap().role(Role::Message).unwrap().value,
            "slow response"
        );
        std::fs::write(&path, b"unmatched\n").unwrap();
        let event = tail.poll().unwrap();
        assert!(matches!(event, crate::log::tail::TailEvent::Reset { .. }));
        assert_eq!(
            tail.index().regex_counts(),
            Some((0, 1)),
            "parser survives truncation"
        );
    }
}
