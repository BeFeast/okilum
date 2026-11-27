//! Read-mode context menu for rendered Markdown tables (#2025).
//!
//! A right-click anywhere on a rendered table offers to copy that table, either
//! as the Markdown it was written in or as tab-separated values. The menu acts
//! on the snapshot the renderer handed over with the press, so it copies the
//! clicked table even when a sibling table or a newer document occupies the
//! same source offsets. Opening the menu or copying never touches the document,
//! the selection or the scroll position.

use gpui_component::text::{TableData, TableSecondaryPress};

/// One way of copying a table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableCopy {
    Markdown,
    Tsv,
}

impl TableCopy {
    pub const ALL: [Self; 2] = [Self::Markdown, Self::Tsv];

    pub fn label(self) -> &'static str {
        match self {
            Self::Markdown => "Copy table as Markdown",
            Self::Tsv => "Copy table as TSV",
        }
    }

    /// The clipboard text for the pressed table, or `None` when the renderer
    /// could not locate the table's source (the item is then disabled rather
    /// than copying a reconstruction the author never wrote).
    pub fn content(self, press: &TableSecondaryPress) -> Option<String> {
        match self {
            Self::Markdown => press.source().map(str::to_owned),
            Self::Tsv => Some(tsv(press.data())),
        }
    }
}

/// Header first, then body rows; no delimiter row. Cells hold their visible
/// text, so Markdown syntax (emphasis markers, link targets, code ticks,
/// escapes) is gone. Empty cells stay empty.
pub fn tsv(table: &TableData) -> String {
    std::iter::once(&table.headers)
        .chain(table.rows.iter())
        .map(|row| {
            row.iter()
                .map(|cell| tsv_field(cell))
                .collect::<Vec<_>>()
                .join("\t")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A field containing a tab, line break or quote is double-quoted, with its
/// quotes doubled, as spreadsheets expect.
fn tsv_field(cell: &str) -> std::borrow::Cow<'_, str> {
    if cell.contains(['\t', '\n', '\r', '"']) {
        format!("\"{}\"", cell.replace('"', "\"\"")).into()
    } else {
        cell.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn data(headers: &[&str], rows: &[&[&str]]) -> TableData {
        TableData {
            headers: headers.iter().map(|s| s.to_string()).collect(),
            rows: rows
                .iter()
                .map(|row| row.iter().map(|s| s.to_string()).collect())
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn reader_table_tsv_quotes_only_fields_that_need_it() {
        let table = data(
            &["Name", "Note", ""],
            &[
                &["a\tb", "say \"hi\"", ""],
                &["line\nbreak", "cr\rx", "plain"],
            ],
        );
        assert_eq!(
            tsv(&table).as_bytes(),
            b"Name\tNote\t\n\"a\tb\"\t\"say \"\"hi\"\"\"\t\n\"line\nbreak\"\t\"cr\rx\"\tplain"
        );
        assert_eq!(tsv(&data(&["only"], &[])), "only");
        assert_eq!(TableCopy::Markdown.label(), "Copy table as Markdown");
        assert_eq!(TableCopy::Tsv.label(), "Copy table as TSV");
    }
}
