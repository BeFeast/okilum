//! Exact-source table operations. The caller admits only a current classifier
//! Table range; this parser never decides whether prose or code is a table.
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Alignment {
    Default,
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Command {
    Next,
    Previous,
    InsertRow,
    DeleteRow,
    InsertColumn,
    DeleteColumn,
    Align(Alignment),
    Format,
}

#[derive(Debug)]
pub(super) struct Change {
    pub source: String,
    pub selection: Range<usize>,
}

pub(super) struct Table {
    pub(super) rows: Vec<Vec<String>>,
    pub(super) alignment: Vec<Alignment>,
    pub(super) ranges: Vec<Vec<Range<usize>>>,
    indent: String,
    newline: &'static str,
    trailing_newline: bool,
}

// GFM splits even code spans at unescaped pipes. Backslash parity matters.
fn cells(line: &str, start: usize) -> Option<Vec<Range<usize>>> {
    cell_spans(line, start, true)
}

fn cell_spans(line: &str, start: usize, trim: bool) -> Option<Vec<Range<usize>>> {
    let trimmed = line.trim_matches([' ', '\t']);
    let from = line.len() - line.trim_start_matches([' ', '\t']).len();
    let end = from + trimmed.len();
    let mut pipes = Vec::new();
    let mut escaped = false;
    for (at, byte) in line.bytes().enumerate() {
        if byte == b'|' && !escaped {
            pipes.push(at);
        }
        escaped = byte == b'\\' && !escaped;
    }
    if pipes.is_empty() {
        return None;
    }
    let leading = pipes.first() == Some(&from);
    let trailing = pipes.last().is_some_and(|&at| at + 1 == end);
    let mut bounds = vec![from];
    bounds.extend(pipes.iter().map(|at| at + 1));
    bounds.push(end + 1);
    let mut result = Vec::new();
    for pair in bounds.windows(2).skip(usize::from(leading)) {
        let a = pair[0];
        let b = pair[1] - 1;
        if a > end || b < a || (trailing && a == end) {
            continue;
        }
        if !trim {
            result.push(start + a..start + b);
            continue;
        }
        let raw = &line[a..b];
        let content = raw.trim_matches([' ', '\t']);
        let lead = raw.len() - raw.trim_start_matches([' ', '\t']).len();
        let pos = a + if content.is_empty() {
            raw.len().min(1)
        } else {
            lead
        };
        result.push(start + pos..start + pos + content.len());
    }
    (!result.is_empty()).then_some(result)
}

impl Table {
    pub(super) fn parse(source: &str) -> Option<Self> {
        if source.len() > 64 * 1024 || source.contains('\0') {
            return None;
        }
        let mut all = Vec::new();
        let mut offset = 0;
        let indent_len = source.len() - source.trim_start_matches(' ').len();
        if indent_len > 3 {
            return None;
        }
        let indent = source[..indent_len].to_owned();
        let newline = if source.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        for line in source.split_inclusive('\n') {
            // Formatting must not silently normalize mixed newline styles.
            if line.ends_with('\n') && line.ends_with("\r\n") != (newline == "\r\n") {
                return None;
            }
            let raw = line
                .trim_end_matches('\n')
                .strip_suffix('\r')
                .unwrap_or(line.trim_end_matches('\n'));
            if !raw.starts_with(&indent) || raw.contains('\r') {
                return None;
            }
            all.push(cells(raw, offset)?);
            offset += line.len();
        }
        if all.len() < 2 {
            return None;
        }
        let width = all[0].len();
        // Refuse ragged rows: GFM may discard surplus cells. Never format away
        // authored text, and do not silently invent cells in malformed input.
        if width > 128 || all.iter().any(|row| row.len() != width) {
            return None;
        }
        let alignment = all[1]
            .iter()
            .map(|range| {
                let raw = &source[range.clone()];
                let dashes = raw.trim_matches(':');
                if dashes.is_empty() || !dashes.bytes().all(|b| b == b'-') {
                    return None;
                }
                Some(match (raw.starts_with(':'), raw.ends_with(':')) {
                    (true, true) => Alignment::Center,
                    (true, false) => Alignment::Left,
                    (false, true) => Alignment::Right,
                    _ => Alignment::Default,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        all.remove(1);
        let rows = all
            .iter()
            .map(|row| {
                row.iter()
                    .map(|range| source[range.clone()].to_owned())
                    .collect()
            })
            .collect();
        Some(Self {
            rows,
            alignment,
            ranges: all,
            indent,
            newline,
            trailing_newline: source.ends_with('\n'),
        })
    }

    fn locate(&self, selection: &Range<usize>) -> Option<(usize, usize)> {
        if selection.start > selection.end {
            return None;
        }
        for (r, row) in self.ranges.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                if cell.start <= selection.start && selection.end <= cell.end {
                    return Some((r, c));
                }
            }
        }
        None
    }

    fn render(&self, row: usize, column: usize) -> Change {
        let widths: Vec<_> = (0..self.alignment.len())
            .map(|col| {
                self.rows
                    .iter()
                    .map(|row| row[col].chars().count())
                    .max()
                    .unwrap_or(0)
                    .max(3)
            })
            .collect();
        let mut source = String::new();
        let mut selection = 0..0;
        for (r, cells) in self.rows.iter().enumerate() {
            source.push_str(&self.indent);
            source.push('|');
            for (c, cell) in cells.iter().enumerate() {
                source.push(' ');
                let start = source.len();
                source.push_str(cell);
                if r == row && c == column {
                    selection = start..source.len();
                }
                source.push_str(&" ".repeat(widths[c] - cell.chars().count()));
                source.push_str(" |");
            }
            if r == 0 {
                source.push_str(self.newline);
                source.push_str(&self.indent);
                source.push('|');
                for (c, &alignment) in self.alignment.iter().enumerate() {
                    source.push(' ');
                    let left = matches!(alignment, Alignment::Left | Alignment::Center);
                    let right = matches!(alignment, Alignment::Right | Alignment::Center);
                    if left {
                        source.push(':');
                    }
                    source
                        .push_str(&"-".repeat(widths[c] - usize::from(left) - usize::from(right)));
                    if right {
                        source.push(':');
                    }
                    source.push_str(" |");
                }
            }
            if r + 1 < self.rows.len() || self.trailing_newline {
                source.push_str(self.newline);
            }
        }
        Change { source, selection }
    }
}

/// Cell ranges retain the bytes authored during a typing session, including
/// leading/trailing spaces. Reparsing on every keystroke would mistake those
/// spaces for original Markdown padding and gradually duplicate them.
#[derive(Clone)]
pub(super) struct CellGrid {
    source: String,
    ranges: Vec<Vec<Range<usize>>>,
}

impl CellGrid {
    pub(super) fn new(source: &str) -> Option<Self> {
        Some(Self {
            source: source.to_owned(),
            ranges: Table::parse(source)?.ranges,
        })
    }

    pub(super) fn value(&self, row: usize, column: usize) -> Option<&str> {
        self.source.get(self.ranges.get(row)?.get(column)?.clone())
    }

    /// Exact-source guard supplements the caller's document revision guard.
    /// Return the next session separately; publish it only after the editor
    /// accepts the transaction, so a failed write cannot advance cell offsets.
    pub(super) fn replace(
        &self,
        current: &str,
        row: usize,
        column: usize,
        value: &str,
    ) -> Option<(Self, Change)> {
        if current != self.source || value.contains(['\n', '\r', '\0']) {
            return None;
        }
        let mut escaped = false;
        for byte in value.bytes() {
            if byte == b'|' && !escaped {
                return None;
            }
            escaped = byte == b'\\' && !escaped;
        }
        let range = self.ranges.get(row)?.get(column)?.clone();
        let mut next = self.clone();
        next.source.replace_range(range.clone(), value);
        let before = Table::parse(current)?;
        let after = Table::parse(&next.source)?;
        if after.rows.len() != before.rows.len() || after.alignment != before.alignment {
            return None;
        }
        for (r, cells) in after.rows.iter().enumerate() {
            for (c, content) in cells.iter().enumerate() {
                let expected = if (r, c) == (row, column) {
                    value.trim_matches([' ', '\t'])
                } else {
                    before.rows[r][c].as_str()
                };
                if content != expected {
                    return None;
                }
            }
        }
        let end = range.start + value.len();
        for (r, cells) in next.ranges.iter_mut().enumerate() {
            for (c, cell) in cells.iter_mut().enumerate() {
                if (r, c) == (row, column) {
                    *cell = range.start..end;
                } else if cell.start >= range.end {
                    cell.start = cell.start - range.len() + value.len();
                    cell.end = cell.end - range.len() + value.len();
                }
            }
        }
        let change = Change {
            source: next.source.clone(),
            selection: range.start..end,
        };
        Some((next, change))
    }
}

fn line_range(source: &str, cell: &Range<usize>) -> Range<usize> {
    let start = source[..cell.start].rfind('\n').map_or(0, |n| n + 1);
    let end = source[cell.end..]
        .find('\n')
        .map_or(source.len(), |n| cell.end + n + 1);
    start..end
}

pub(super) fn edit(source: &str, selection: Range<usize>, command: Command) -> Option<Change> {
    if !source.is_char_boundary(selection.start) || !source.is_char_boundary(selection.end) {
        return None;
    }
    let table = Table::parse(source)?;
    let (mut row, mut col) = table.locate(&selection)?;
    if matches!(command, Command::Format) {
        return Some(table.render(row, col));
    }
    let mut updated = source.to_owned();
    let width = table.alignment.len();
    match command {
        Command::Next => {
            col += 1;
            if col == width {
                col = 0;
                row += 1;
            }
        }
        Command::Previous => {
            if col > 0 {
                col -= 1;
            } else if row > 0 {
                row -= 1;
                col = width - 1;
            }
        }
        Command::InsertRow => {
            row += 1;
        }
        Command::DeleteRow => {
            if row == 0 {
                return None;
            }
            updated.replace_range(line_range(source, &table.ranges[row][0]), "");
            row = row.min(table.rows.len() - 2);
        }
        Command::InsertColumn | Command::DeleteColumn => {
            if matches!(command, Command::DeleteColumn) && width == 1 {
                return None;
            }
            // Include the delimiter row, retaining optional outer pipes and padding.
            let mut offset = 0;
            let mut edits = Vec::new();
            for line in source.split_inclusive('\n') {
                let raw = line.trim_end_matches(['\r', '\n']);
                let ranges = cell_spans(raw, offset, false)?;
                let range = &ranges[col];
                if matches!(command, Command::InsertColumn) {
                    let delimiter = offset == line_range(source, &table.ranges[0][0]).end;
                    let no_border = col + 1 == width
                        && range.end == offset + raw.trim_end_matches([' ', '\t']).len();
                    let value = match (delimiter, no_border) {
                        (true, true) => "| ---|",
                        (true, false) => "| ---",
                        (false, true) => "| |",
                        (false, false) => "| ",
                    };
                    edits.push((range.end..range.end, value));
                } else if col + 1 < width {
                    edits.push((range.start..ranges[col + 1].start, ""));
                } else {
                    edits.push((ranges[col - 1].end..range.end, ""));
                }
                offset += line.len();
            }
            for (range, value) in edits.into_iter().rev() {
                updated.replace_range(range, value);
            }
            if matches!(command, Command::InsertColumn) {
                col += 1;
            } else {
                col = col.min(width - 2);
            }
        }
        Command::Align(alignment) => {
            let header_end = line_range(source, &table.ranges[0][0]).end;
            let line = source[header_end..]
                .split('\n')
                .next()?
                .trim_end_matches('\r');
            let range = cells(line, header_end)?.get(col)?.clone();
            let old = &source[range.clone()];
            let dashes = old.trim_matches(':');
            let left = matches!(alignment, Alignment::Left | Alignment::Center);
            let right = matches!(alignment, Alignment::Right | Alignment::Center);
            let value = format!(
                "{}{}{}",
                if left { ":" } else { "" },
                dashes,
                if right { ":" } else { "" }
            );
            updated.replace_range(range, &value);
        }
        Command::Format => unreachable!(),
    }
    if matches!(command, Command::InsertRow) || row == table.rows.len() {
        let physical = if row == 1 {
            line_range(source, &table.ranges[0][0]).end
                + source[line_range(source, &table.ranges[0][0]).end..]
                    .find('\n')
                    .map_or(
                        source.len() - line_range(source, &table.ranges[0][0]).end,
                        |n| n + 1,
                    )
        } else {
            line_range(source, &table.ranges[row - 1][0]).end
        };
        let blank = format!("{}|{}{}", table.indent, " |".repeat(width), table.newline);
        let prefix = if physical > 0 && !source[..physical].ends_with('\n') {
            table.newline
        } else {
            ""
        };
        let mut value = format!("{prefix}{blank}");
        if physical == source.len() && !table.trailing_newline {
            value.truncate(value.len() - table.newline.len());
        }
        updated.insert_str(physical, &value);
    }
    let parsed = Table::parse(&updated)?;
    let selection = parsed.ranges.get(row)?.get(col)?.clone();
    Some(Change {
        source: updated,
        selection,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "| Name | Count |\n| :--- | ---: |\n| apple | 3 |\n| pear | 12 |";
    fn at(source: &str, content: &str) -> Range<usize> {
        let start = source.find(content).unwrap();
        start..start + content.len()
    }
    fn selected(change: &Change) -> &str {
        &change.source[change.selection.clone()]
    }

    #[test]
    fn cell_typing_preserves_authored_spaces_and_neighbor_bytes() {
        let source = "  Name\t| Count\r\n  :----- | ---:\r\n  apple  |  3\r\n";
        let original = CellGrid::new(source).unwrap();
        let (typing, first) = original.replace(source, 1, 0, "red ").unwrap();
        assert_eq!(first.source, source.replacen("apple", "red ", 1));
        let (typing, second) = typing.replace(&first.source, 1, 0, "red pear ").unwrap();
        assert_eq!(second.source, source.replacen("apple", "red pear ", 1));
        assert_eq!(typing.value(1, 0), Some("red pear "));
        let (_, neighbor) = typing.replace(&second.source, 1, 1, "42").unwrap();
        assert_eq!(
            neighbor.source,
            source
                .replacen("apple", "red pear ", 1)
                .replace("  3", "  42")
        );
        assert_eq!(selected(&neighbor), "42");
        // An old transaction cannot be replayed after another edit or Undo.
        assert!(original.replace(&neighbor.source, 1, 0, "stale").is_none());
        assert!(typing.replace(source, 1, 0, "stale").is_none());
        let (_, restored) = CellGrid::new(source)
            .unwrap()
            .replace(source, 1, 0, "fresh")
            .unwrap();
        assert_eq!(restored.source, source.replacen("apple", "fresh", 1));
    }

    #[test]
    fn cell_edits_refuse_structure_changes_without_losing_escaped_source() {
        let source = "|Name|Count|\n|---|---|\n|a\\|b|3|";
        let grid = CellGrid::new(source).unwrap();
        for value in ["x|y", "x\ny", "x\r\ny", "x\0y", "x\\"] {
            assert!(grid.replace(source, 1, 0, value).is_none(), "{value:?}");
        }
        let (_, change) = grid.replace(source, 1, 0, r"c\|d").unwrap();
        assert_eq!(change.source, source.replace(r"a\|b", r"c\|d"));
        let (_, change) = grid.replace(source, 1, 0, r"x\\").unwrap();
        assert_eq!(change.source, source.replace(r"a\|b", r"x\\"));
    }

    #[test]
    fn actions_change_only_the_requested_source() {
        let source = "  A\t| B  \r\n  ----- | ---:\r\n  one  |two\r\n  three | four\r\n";
        assert_eq!(
            edit(source, at(source, "one"), Command::Next)
                .unwrap()
                .source,
            source
        );
        let aligned = edit(source, at(source, "one"), Command::Align(Alignment::Center)).unwrap();
        assert_eq!(aligned.source, source.replacen("-----", ":-----:", 1));
        let deleted = edit(source, at(source, "three"), Command::DeleteRow).unwrap();
        assert_eq!(deleted.source, source.replace("  three | four\r\n", ""));
        let inserted = edit(source, at(source, "one"), Command::InsertRow).unwrap();
        assert!(inserted
            .source
            .starts_with("  A\t| B  \r\n  ----- | ---:\r\n  one  |two\r\n"));
        assert!(inserted.source.ends_with("  three | four\r\n"));
        let restored = edit(
            &inserted.source,
            inserted.selection.clone(),
            Command::DeleteRow,
        )
        .unwrap();
        assert_eq!(restored.source, source);
        let inserted = edit(source, at(source, "one"), Command::InsertColumn).unwrap();
        let restored = edit(
            &inserted.source,
            inserted.selection.clone(),
            Command::DeleteColumn,
        )
        .unwrap();
        assert_eq!(restored.source, source);
    }

    #[test]
    fn navigation_skips_delimiter_wraps_and_appends_one_row() {
        let next = edit(TABLE, at(TABLE, "Name"), Command::Next).unwrap();
        assert_eq!(selected(&next), "Count");
        let next = edit(&next.source, next.selection.clone(), Command::Next).unwrap();
        assert_eq!(selected(&next), "apple");
        let back = edit(&next.source, next.selection.clone(), Command::Previous).unwrap();
        assert_eq!(selected(&back), "Count");
        let last = edit(TABLE, at(TABLE, "12"), Command::Next).unwrap();
        assert_eq!(selected(&last), "");
        assert_eq!(last.source.lines().count(), 5);
        assert!(
            Table::parse(&last.source).is_some(),
            "empty cells remain navigable"
        );
    }

    #[test]
    fn structural_edits_preserve_other_cells_and_alignment() {
        let inserted = edit(TABLE, at(TABLE, "apple"), Command::InsertRow).unwrap();
        assert_eq!(inserted.source.lines().count(), 5);
        let removed = edit(
            &inserted.source,
            inserted.selection.clone(),
            Command::DeleteRow,
        )
        .unwrap();
        let formatted = Change {
            source: TABLE.to_owned(),
            selection: 0..0,
        };
        assert_eq!(removed.source, formatted.source);
        let inserted = edit(TABLE, at(TABLE, "apple"), Command::InsertColumn).unwrap();
        assert_eq!(Table::parse(&inserted.source).unwrap().alignment.len(), 3);
        let removed = edit(
            &inserted.source,
            inserted.selection.clone(),
            Command::DeleteColumn,
        )
        .unwrap();
        assert_eq!(removed.source, formatted.source);
        let aligned = edit(TABLE, at(TABLE, "Count"), Command::Align(Alignment::Center)).unwrap();
        assert_eq!(
            Table::parse(&aligned.source).unwrap().alignment[1],
            Alignment::Center
        );
        assert_eq!(selected(&aligned), "Count");
        assert!(edit(TABLE, at(TABLE, "Name"), Command::DeleteRow).is_none());
        assert!(edit("| a |\n|---|", 2..3, Command::DeleteColumn).is_none());
    }

    #[test]
    fn exact_text_crlf_escapes_unicode_and_optional_borders_survive() {
        let source =
            "  Имя | a\\|b | `c\\|d`\r\n  --- | :---: | ---:\r\n  שלום | x\\\\ | конец\r\n";
        let change = edit(source, at(source, "Имя"), Command::Next).unwrap();
        assert_eq!(selected(&change), "a\\|b");
        assert!(change.source.contains("`c\\|d`"));
        assert!(change.source.contains("שלום"));
        assert!(change.source.contains("x\\\\"));
        assert_eq!(
            change.source.matches('\n').count(),
            change.source.matches("\r\n").count()
        );
        assert!(change.source.ends_with("\r\n"));
        assert_eq!(change.source, source, "navigation preserves exact source");
        let formatted = edit(&change.source, change.selection.clone(), Command::Format).unwrap();
        let again = edit(
            &formatted.source,
            formatted.selection.clone(),
            Command::Format,
        )
        .unwrap();
        assert_eq!(formatted.source, again.source, "formatting is idempotent");
    }

    #[test]
    fn ambiguous_or_non_cell_selections_do_not_rewrite_source() {
        for source in [
            "| a | b |\n|---|---|\n| lost | extra | text |",
            "| a |\n|wat|\n| b |",
            "| a | b |\n|---|---|\n| missing |",
        ] {
            assert!(edit(source, 2..3, Command::Next).is_none());
        }
        let mixed = "| a |\r\n|---|\n| b |";
        assert!(edit(mixed, 2..3, Command::Format).is_none());
        assert!(edit(TABLE, 0..TABLE.len(), Command::Next).is_none());
        assert!(edit(TABLE, at(TABLE, ":---"), Command::Next).is_none());
        assert!(edit(TABLE, 0..0, Command::Next).is_none());
        assert!(edit(TABLE, 1000..1000, Command::Next).is_none());
        let source = "| שלום |\n|---|";
        assert!(
            edit(source, 3..3, Command::Next).is_none(),
            "UTF-8 boundary"
        );
    }
}
