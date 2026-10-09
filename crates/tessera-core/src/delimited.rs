//! A read-only CSV/TSV projection. Canonical source is never serialized here.
use std::ops::Range;

#[derive(Clone, Debug)]
pub struct Table {
    pub rows: Vec<Vec<String>>,
    pub columns: usize,
    pub delimiter: char,
    pub malformed: bool,
    pub ragged: bool,
    pub ambiguous: bool,
}

impl Table {
    pub fn parse(source: &str, tsv: bool) -> Self {
        let source = source.strip_prefix('\u{feff}').unwrap_or(source);
        let mut choices: Vec<_> = if tsv {
            vec!['\t']
        } else {
            vec![',', ';', '\t']
        }
        .into_iter()
        .map(|delimiter| {
            let (rows, _) = records(source, delimiter, Some(32));
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
        let (rows, malformed) = records(source, delimiter, None);
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

fn records(source: &str, delimiter: char, limit: Option<usize>) -> (Vec<Vec<String>>, bool) {
    let mut rows = vec![];
    let mut row = vec![];
    let mut field = String::new();
    let mut chars = source.chars().peekable();
    let mut quoted = false;
    let mut closed = false;
    let mut malformed = false;
    let mut active = false;
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
            row.push(std::mem::take(&mut field));
            closed = false;
            active = true;
        } else if ch == '\n' || ch == '\r' {
            if ch == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            row.push(std::mem::take(&mut field));
            rows.push(std::mem::take(&mut row));
            closed = false;
            active = false;
            if limit.is_some_and(|limit| rows.len() >= limit) {
                return (rows, malformed);
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
    malformed |= quoted;
    if active {
        row.push(field);
        rows.push(row);
    }
    (rows, malformed)
}

#[cfg(test)]
mod tests {
    use super::*;
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
