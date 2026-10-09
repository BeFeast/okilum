//! A read-only CSV/TSV projection. Canonical source is never serialized here.
use std::{
    io::{self, Read},
    ops::Range,
};

/// Resource bounds for an attachment projection, including pathological rows
/// with many empty fields. These never apply to canonical source editing.
#[derive(Clone, Copy, Debug)]
pub struct ReadLimits {
    pub bytes: usize,
    pub rows: usize,
    pub cells: usize,
    pub columns: usize,
}
impl ReadLimits {
    const UNLIMITED: Self = Self {
        bytes: usize::MAX,
        rows: usize::MAX,
        cells: usize::MAX,
        columns: usize::MAX,
    };
}

#[derive(Clone, Debug)]
pub struct Table {
    pub rows: Vec<Vec<String>>,
    pub columns: usize,
    pub delimiter: char,
    pub malformed: bool,
    pub ragged: bool,
    pub ambiguous: bool,
    /// More source exists, or a record/cell/column budget stopped parsing.
    pub limited: bool,
}

impl Table {
    pub fn parse(source: &str, tsv: bool) -> Self {
        Self::parse_limited(source, tsv, ReadLimits::UNLIMITED, true)
    }

    /// Read a bounded UTF-8 prefix, omitting a last incomplete record at our
    /// byte boundary. Actual EOF still retains malformed records for display.
    pub fn read(reader: impl Read, tsv: bool, limits: ReadLimits) -> io::Result<Self> {
        let probe = limits.bytes.checked_add(1).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "Invalid preview byte limit")
        })?;
        let mut bytes = Vec::new();
        reader.take(probe as u64).read_to_end(&mut bytes)?;
        let complete = bytes.len() <= limits.bytes;
        bytes.truncate(limits.bytes);
        let source = match std::str::from_utf8(&bytes) {
            Ok(source) => source,
            Err(error) if !complete && error.error_len().is_none() => {
                std::str::from_utf8(&bytes[..error.valid_up_to()]).expect("valid prefix")
            }
            Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
        };
        Ok(Self::parse_limited(source, tsv, limits, complete))
    }

    fn parse_limited(source: &str, tsv: bool, limits: ReadLimits, complete: bool) -> Self {
        let source = source.strip_prefix('\u{feff}').unwrap_or(source);
        let mut choices: Vec<_> = if tsv {
            vec!['\t']
        } else {
            vec![',', ';', '\t']
        }
        .into_iter()
        .map(|delimiter| {
            let (rows, _, _) = records(
                source,
                delimiter,
                ReadLimits {
                    rows: limits.rows.min(32),
                    ..limits
                },
                complete,
            );
            let mut counts = std::collections::BTreeMap::<usize, usize>::new();
            for row in &rows {
                if row.len() > 1 {
                    *counts.entry(row.len()).or_default() += 1;
                }
            }
            let score = counts.values().copied().max().unwrap_or(0);
            (delimiter, score)
        })
        .collect();
        choices.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
        let delimiter = choices[0].0;
        let ambiguous = !tsv && choices[0].1 > 0 && choices[0].1 == choices[1].1;
        let (rows, malformed, stopped) = records(source, delimiter, limits, complete);
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        let ragged = rows
            .first()
            .is_some_and(|first| rows.iter().any(|row| row.len() != first.len()));
        Self {
            rows,
            columns,
            delimiter,
            malformed,
            ragged,
            ambiguous,
            limited: !complete || stopped,
        }
    }

    pub fn cell(&self, row: usize, column: usize) -> &str {
        self.rows
            .get(row)
            .and_then(|row| row.get(column))
            .map_or("", String::as_str)
    }

    /// Clipboard projection only: a single cell stays plain; a rectangle is
    /// TSV with quoted tabs/newlines, including empty cells in ragged rows.
    pub fn copy(&self, rows: Range<usize>, columns: Range<usize>) -> String {
        if rows.len() == 1 && columns.len() == 1 {
            return self.cell(rows.start, columns.start).into();
        }
        rows.map(|row| {
            columns
                .clone()
                .map(|col| {
                    let value = self.cell(row, col);
                    if value.contains(['\t', '\r', '\n', '"']) {
                        format!("\"{}\"", value.replace('"', "\"\""))
                    } else {
                        value.into()
                    }
                })
                .collect::<Vec<_>>()
                .join("\t")
        })
        .collect::<Vec<_>>()
        .join("\n")
    }
}

fn records(
    source: &str,
    delimiter: char,
    limits: ReadLimits,
    complete: bool,
) -> (Vec<Vec<String>>, bool, bool) {
    let mut rows = vec![];
    let mut row = vec![];
    let mut field = String::new();
    let mut chars = source.chars().peekable();
    let mut quoted = false;
    let mut closed = false;
    let mut malformed = false;
    let mut active = false;
    let mut cells = 0;
    if limits.rows == 0 || limits.columns == 0 || limits.cells == 0 {
        return (rows, false, !source.is_empty());
    }
    while let Some(ch) = chars.next() {
        if quoted {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                    closed = true;
                }
            } else {
                field.push(ch);
            }
            active = true;
            continue;
        }
        if ch == delimiter {
            // A separator starts another field, so refuse before allocating
            // an unbounded Vec for a comma-only or extraordinarily wide row.
            if row.len() + 1 >= limits.columns || cells + row.len() + 1 >= limits.cells {
                return (rows, malformed, true);
            }
            row.push(std::mem::take(&mut field));
            closed = false;
            active = true;
        } else if ch == '\n' || ch == '\r' {
            if ch == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            row.push(std::mem::take(&mut field));
            cells += row.len();
            rows.push(std::mem::take(&mut row));
            closed = false;
            active = false;
            if rows.len() >= limits.rows || cells >= limits.cells {
                return (rows, malformed, chars.peek().is_some());
            }
        } else if ch == '"' && field.is_empty() && !closed {
            quoted = true;
            active = true;
        } else {
            malformed |= closed || ch == '"';
            field.push(ch);
            active = true;
        }
    }
    malformed |= quoted && complete;
    if active && complete {
        row.push(field);
        rows.push(row);
    }
    (rows, malformed, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, io::Cursor, rc::Rc};

    struct CountingReader {
        input: Cursor<Vec<u8>>,
        read: Rc<Cell<usize>>,
    }
    impl Read for CountingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let n = self.input.read(buffer)?;
            self.read.set(self.read.get() + n);
            Ok(n)
        }
    }

    #[test]
    fn delimited_preview_bounds_reads_records_and_empty_field_allocations() {
        let source = format!("name,value\n{}", "row,42\n".repeat(10_000));
        let read = Rc::new(Cell::new(0));
        let limits = ReadLimits {
            bytes: 1024,
            rows: 3,
            cells: 64,
            columns: 8,
        };
        let table = Table::read(
            CountingReader {
                input: Cursor::new(source.as_bytes().to_vec()),
                read: read.clone(),
            },
            false,
            limits,
        )
        .unwrap();
        assert!(table.limited && !table.malformed);
        assert_eq!(table.rows.len(), 3);
        assert_eq!(table.cell(2, 1), "42");
        assert_eq!(
            read.get(),
            limits.bytes + 1,
            "one bounded lookahead byte only"
        );
        assert_eq!(
            Table::parse(&source, false).rows.len(),
            10_001,
            "full-parse positive control"
        );
        let wide = format!("a,b\n{}\n", ",".repeat(50_000));
        assert_eq!(Table::parse(&wide, false).rows[1].len(), 50_001);
        let table = Table::read(
            Cursor::new(wide),
            false,
            ReadLimits {
                bytes: 100_000,
                rows: 10,
                cells: 100_000,
                columns: 8,
            },
        )
        .unwrap();
        assert!(table.limited);
        assert_eq!(
            table.rows,
            vec![vec!["a", "b"]],
            "a wide incomplete record cannot create unbounded cells"
        );
        let table = Table::read(
            Cursor::new("a,b\n1,2\n3,4\n"),
            false,
            ReadLimits {
                bytes: 64,
                rows: 10,
                cells: 4,
                columns: 8,
            },
        )
        .unwrap();
        assert!(table.limited);
        assert_eq!(table.rows.len(), 2);
    }

    #[test]
    fn delimited_preview_omits_cut_records_but_keeps_real_eof_errors() {
        let source = "a,b\r\n1,\"שלום\r\nnext\"\r\n2,3\r\n";
        let limits = ReadLimits {
            bytes: 11,
            rows: 10,
            cells: 64,
            columns: 8,
        };
        let table = Table::read(Cursor::new(source), false, limits).unwrap();
        assert!(table.limited && !table.malformed);
        assert_eq!(table.rows, vec![vec!["a", "b"]]);
        let table = Table::read(
            Cursor::new(source),
            false,
            ReadLimits {
                bytes: 1024,
                ..limits
            },
        )
        .unwrap();
        assert!(!table.limited && !table.malformed);
        assert_eq!(table.cell(1, 1), "שלום\r\nnext");
        let bad = Table::read(
            Cursor::new("a,b\n\"unfinished"),
            false,
            ReadLimits {
                bytes: 1024,
                ..limits
            },
        )
        .unwrap();
        assert!(!bad.limited && bad.malformed);
        assert_eq!(bad.cell(1, 0), "unfinished");
        assert!(Table::read(Cursor::new(b"a,b\n\xff,2"), false, limits).is_err());
    }
    #[test]
    fn delimited_quotes_newlines_bom_and_trailing_empty_are_read_only() {
        let source = "\u{feff}name,value,empty\r\n\"שלום, world\",\"a\r\nb \"\"quoted\"\"\",\r\n";
        let before = source.as_bytes().to_vec();
        let table = Table::parse(source, false);
        assert_eq!(table.delimiter, ',');
        assert!(!table.malformed && !table.ragged);
        assert_eq!(table.rows[1], ["שלום, world", "a\r\nb \"quoted\"", ""]);
        assert_eq!(source.as_bytes(), before);
    }
    #[test]
    fn delimited_detection_ignores_quoted_separators_and_tsv_is_explicit() {
        assert_eq!(
            Table::parse("a;b\n\"many, commas, here\";2\n", false).delimiter,
            ';'
        );
        assert_eq!(Table::parse("a\tb\nשלום\t123\n", false).delimiter, '\t');
        assert_eq!(Table::parse("a,b\n1,2\n", true).columns, 1);
        assert!(Table::parse("a,b;c\n1,2;3\n", false).ambiguous);
    }
    #[test]
    fn delimited_ragged_and_malformed_keep_parsed_values() {
        let table = Table::parse("a,b\n1\n2,3,4\n\"unfinished", false);
        assert!(table.ragged && table.malformed);
        assert_eq!(table.columns, 3);
        assert_eq!(table.cell(1, 1), "");
        assert_eq!(table.cell(3, 0), "unfinished");
        assert!(Table::parse("", false).rows.is_empty());
        assert_eq!(Table::parse("a,b,", false).rows[0], ["a", "b", ""]);
    }
    #[test]
    fn delimited_rectangular_copy_is_tsv_and_single_cell_is_plain() {
        let table = Table::parse("a,b\n\"line\nnext\",\"say \"\"hi\"\"\"\nshort\n", false);
        assert_eq!(table.copy(1..2, 0..1), "line\nnext");
        let copied = table.copy(0..3, 0..2);
        assert_eq!(
            Table::parse(&copied, true).rows,
            vec![
                vec!["a", "b"],
                vec!["line\nnext", "say \"hi\""],
                vec!["short", ""]
            ]
        );
    }
    #[test]
    fn delimited_ten_thousand_rows_keep_order_and_complete_values() {
        let source = format!(
            "name,value\n{}",
            (0..10_000)
                .map(|i| format!("שלום{i},{i}\n"))
                .collect::<String>()
        );
        let table = Table::parse(&source, false);
        assert_eq!(table.rows.len(), 10_001);
        assert_eq!(table.cell(10_000, 1), "9999");
        assert!(!table.malformed && !table.ragged);
    }
}
