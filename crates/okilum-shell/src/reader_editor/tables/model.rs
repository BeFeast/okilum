//! Exact-source table operations. The caller admits only a current classifier
//! Table range; this parser never decides whether prose or code is a table.
use std::ops::Range;
use unicode_width::UnicodeWidthStr;

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
    MoveRowUp,
    MoveRowDown,
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

    fn locate(&self, source: &str, selection: &Range<usize>) -> Option<(usize, usize)> {
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
        // A caret in Markdown cell padding still belongs to that cell. Do
        // not extend this to cross-cell selections or the delimiter row.
        if selection.is_empty() {
            for (r, row) in self.ranges.iter().enumerate() {
                let line = line_range(source, &row[0]);
                let raw = source[line.clone()].trim_end_matches(['\r', '\n']);
                if let Some(spans) = cell_spans(raw, line.start, false) {
                    if let Some(c) = spans.iter().position(|span| {
                        span.start <= selection.start && selection.start <= span.end
                    }) {
                        return Some((r, c));
                    }
                }
            }
        }
        None
    }

    fn render(&self, row: usize, column: usize) -> Option<Change> {
        self.render_cells(row, column).map(|(change, _)| change)
    }

    fn render_cells(&self, row: usize, column: usize) -> Option<(Change, Vec<Vec<Range<usize>>>)> {
        let widths: Vec<_> = (0..self.alignment.len())
            .map(|col| {
                self.rows
                    .iter()
                    .map(|row| UnicodeWidthStr::width(row[col].as_str()))
                    .max()
                    .unwrap_or(0)
                    .max(3)
            })
            .collect();
        // Alignment can multiply a long cell's padding across many rows.
        // Keep the output under the same admission bound before allocating it.
        let line_width = self.indent.len()
            + 1
            + widths.iter().map(|width| width + 3).sum::<usize>()
            + self.newline.len();
        let estimate = line_width * (self.rows.len() + 1)
            + self
                .rows
                .iter()
                .flatten()
                .map(|cell| cell.len() - UnicodeWidthStr::width(cell.as_str()).min(cell.len()))
                .sum::<usize>();
        if estimate > 64 * 1024 {
            return None;
        }
        let mut source = String::new();
        let mut selection = 0..0;
        let mut ranges = Vec::new();
        for (r, cells) in self.rows.iter().enumerate() {
            let mut row_ranges = Vec::new();
            source.push_str(&self.indent);
            source.push('|');
            for (c, cell) in cells.iter().enumerate() {
                source.push(' ');
                let start = source.len();
                source.push_str(cell);
                row_ranges.push(start..source.len());
                if r == row && c == column {
                    selection = start..source.len();
                }
                source.push_str(&" ".repeat(widths[c] - UnicodeWidthStr::width(cell.as_str())));
                source.push_str(" |");
            }
            ranges.push(row_ranges);
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
        Some((Change { source, selection }, ranges))
    }
}

/// A lossless cell presentation: GFM pipe/backslash escapes and HTML line
/// breaks have editable text equivalents. Boundary pairs keep untouched source
/// spellings (including mixed <br>, <BR/> and <br />) byte for byte.
struct CellText {
    text: String,
    boundaries: Vec<(usize, usize)>,
    newline: String,
}

impl CellText {
    fn read(raw: &str) -> Self {
        let mut result = Self {
            text: String::new(),
            boundaries: vec![(0, 0)],
            newline: String::new(),
        };
        let mut at = 0;
        let mut ticks = None;
        while at < raw.len() {
            let tail = &raw[at..];
            let mut len = tail.chars().next().unwrap().len_utf8();
            let mut decoded = None;
            if tail.starts_with('`') {
                let run = tail.bytes().take_while(|&b| b == b'`').count();
                ticks = match ticks {
                    None => Some(run),
                    Some(n) if n == run => None,
                    other => other,
                };
                for _ in 0..run {
                    at += 1;
                    result.text.push('`');
                    result.boundaries.push((result.text.len(), at));
                }
                continue;
            }
            if tail.starts_with('\\') {
                match tail.as_bytes().get(1) {
                    Some(b'|') => {
                        len = 2;
                        decoded = Some('|');
                    }
                    Some(b'\\') if ticks.is_none() => {
                        len = 2;
                        decoded = Some('\\');
                    }
                    _ => {}
                }
            } else if ticks.is_none() {
                for spelling in ["<br>", "<br/>", "<br />"] {
                    if tail
                        .get(..spelling.len())
                        .is_some_and(|text| text.eq_ignore_ascii_case(spelling))
                    {
                        len = spelling.len();
                        decoded = Some('\n');
                        if result.newline.is_empty() {
                            result.newline = tail[..len].to_owned();
                        }
                        break;
                    }
                }
            }
            result
                .text
                .push(decoded.unwrap_or_else(|| tail.chars().next().unwrap()));
            at += len;
            result.boundaries.push((result.text.len(), at));
        }
        if result.newline.is_empty() {
            result.newline = "<br>".to_owned();
        }
        result
    }

    fn rewrite(raw: &str, value: &str) -> Option<String> {
        if value.contains(['\r', '\0']) {
            return None;
        }
        let before = Self::read(raw);
        if before.text == value {
            return Some(raw.to_owned());
        }
        let mut prefix = before
            .text
            .chars()
            .zip(value.chars())
            .take_while(|(a, b)| a == b)
            .map(|(c, _)| c.len_utf8())
            .sum::<usize>();
        // Include a neighboring backslash run in the replacement so newly
        // typed pipes cannot turn its parity into a structural delimiter.
        while prefix > 0 && before.text[..prefix].ends_with('\\') {
            prefix -= 1;
        }
        let suffix = before.text[prefix..]
            .chars()
            .rev()
            .zip(value[prefix..].chars().rev())
            .take_while(|(a, b)| a == b)
            .map(|(c, _)| c.len_utf8())
            .sum::<usize>();
        let boundary = |at| {
            before
                .boundaries
                .iter()
                .find(|(display, _)| *display == at)
                .map(|(_, raw)| *raw)
        };
        let start = boundary(prefix)?;
        let end = boundary(before.text.len() - suffix)?;
        let mut encoded = String::new();
        for ch in value[prefix..value.len() - suffix].chars() {
            match ch {
                '|' => encoded.push_str(r"\|"),
                '\\' => encoded.push_str(r"\\"),
                '\n' => encoded.push_str(&before.newline),
                _ => encoded.push(ch),
            }
        }
        let mut result = raw.to_owned();
        result.replace_range(start..end, &encoded);
        // Refuse an ambiguous escape/code-span join rather than normalizing
        // unrelated Markdown elsewhere in the cell to make it fit.
        (Self::read(&result).text == value).then_some(result)
    }
}

pub(super) fn cell_text(raw: &str) -> String {
    CellText::read(raw).text
}

/// Cell ranges retain the bytes authored during a typing session, including
/// leading/trailing spaces. Reparsing on every keystroke would mistake those
/// spaces for original Markdown padding and gradually duplicate them.
#[derive(Clone)]
pub(super) struct CellGrid {
    source: String,
    ranges: Vec<Vec<Range<usize>>>,
}

fn has_trailing_pipe(line: &str) -> bool {
    line.trim_end_matches([' ', '\t'])
        .strip_suffix('|')
        .is_some_and(|before| {
            before
                .bytes()
                .rev()
                .take_while(|&byte| byte == b'\\')
                .count()
                % 2
                == 0
        })
}

// Cell input is literal with respect to block structure. A leading pipe
// protects edited bare rows from becoming headings, lists, fences or HTML.
fn bare_row_needs_pipe(line: &str, header: bool) -> bool {
    let text = line.trim_start_matches([' ', '\t']);
    if text.starts_with('|') {
        return false;
    }
    let indent = &line[..line.len() - text.len()];
    if header || indent.len() > 3 || indent.contains('\t') {
        return true;
    }
    let bytes = text.as_bytes();
    if matches!(
        bytes.first(),
        Some(b'#' | b'>' | b'<' | b'`' | b'~' | b'-' | b'+' | b'*' | b'[')
    ) {
        return true;
    }
    let digits = bytes
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    digits > 0
        && matches!(bytes.get(digits), Some(b'.' | b')'))
        && bytes.get(digits + 1).is_some_and(u8::is_ascii_whitespace)
}

impl CellGrid {
    pub(super) fn new(source: &str) -> Option<Self> {
        Some(Self {
            source: source.to_owned(),
            ranges: Table::parse(source)?.ranges,
        })
    }

    pub(super) fn format(&self, row: usize, column: usize) -> Option<(Self, Change)> {
        let mut table = Table::parse(&self.source)?;
        table.rows = self
            .ranges
            .iter()
            .map(|cells| {
                cells
                    .iter()
                    .map(|range| self.source[range.clone()].to_owned())
                    .collect()
            })
            .collect();
        let (change, ranges) = table.render_cells(row, column)?;
        let next = Self {
            source: change.source.clone(),
            ranges,
        };
        Some((next, change))
    }

    pub(super) fn value(&self, row: usize, column: usize) -> Option<&str> {
        self.source.get(self.ranges.get(row)?.get(column)?.clone())
    }

    pub(super) fn replace_text(
        &self,
        current: &str,
        row: usize,
        column: usize,
        value: &str,
    ) -> Option<(Self, Change)> {
        let raw = self.value(row, column)?;
        let encoded = CellText::rewrite(raw, value)?;
        self.replace(current, row, column, &encoded)
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
        let before = Table::parse(current)?;
        let mut next = self.clone();
        next.source.replace_range(range.clone(), value);
        let mut prefix = None;
        let mut suffix = None;
        if current.get(range.clone()) != Some(value) {
            let physical = line_range(current, &range);
            let old_line = current[physical.clone()].trim_end_matches(['\r', '\n']);
            let start = physical.start;
            let line = next.source[start..]
                .split('\n')
                .next()?
                .trim_end_matches('\r');
            if column == 0
                && !old_line.trim_start_matches([' ', '\t']).starts_with('|')
                && (bare_row_needs_pipe(line, row == 0)
                    || line.trim_start_matches([' ', '\t']).starts_with('|'))
            {
                let at = start + before.indent.len();
                next.source.insert(at, '|');
                prefix = Some(at);
            }
            if column + 1 == before.alignment.len()
                && value.trim_matches([' ', '\t']).is_empty()
                && !has_trailing_pipe(old_line)
            {
                let line = next.source[start..]
                    .split('\n')
                    .next()?
                    .trim_end_matches('\r');
                let at = start + line.len();
                next.source.insert(at, '|');
                suffix = Some(at);
            }
        }
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
        if let Some(at) = prefix {
            for cells in &mut next.ranges {
                for cell in cells {
                    if cell.start >= at {
                        cell.start += 1;
                        cell.end += 1;
                    }
                }
            }
        }
        if let Some(at) = suffix {
            for (r, cells) in next.ranges.iter_mut().enumerate() {
                for (c, cell) in cells.iter_mut().enumerate() {
                    if cell.start > at || (cell.start == at && (r, c) != (row, column)) {
                        cell.start += 1;
                        cell.end += 1;
                    }
                }
            }
        }
        let change = Change {
            source: next.source.clone(),
            selection: next.ranges[row][column].clone(),
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

// Physical row/column mapping includes the delimiter row and padding, so an
// idle format also works after typing an alignment colon or a trailing space.
fn map_format_position(before: &str, after: &str, at: usize) -> Option<usize> {
    let mut from = 0;
    let mut to = 0;
    for (old, new) in before
        .split_inclusive('\n')
        .zip(after.split_inclusive('\n'))
    {
        if at < from + old.len() || (at == before.len() && from + old.len() == before.len()) {
            let old_raw = old.trim_end_matches(['\r', '\n']);
            let new_raw = new.trim_end_matches(['\r', '\n']);
            let offset = at.checked_sub(from)?;
            if offset >= old_raw.len() {
                return Some(
                    to + new_raw.len() + (offset - old_raw.len()).min(new.len() - new_raw.len()),
                );
            }
            let spans = cell_spans(old_raw, 0, false)?;
            if let Some(column) = spans
                .iter()
                .position(|span| span.start <= offset && offset <= span.end)
            {
                let old_cell = cells(old_raw, 0)?.get(column)?.clone();
                let new_cell = cells(new_raw, 0)?.get(column)?.clone();
                return Some(
                    to + new_cell.start + offset.saturating_sub(old_cell.start).min(new_cell.len()),
                );
            }
            return Some(to + offset.min(new_raw.len()));
        }
        from += old.len();
        to += new.len();
    }
    None
}

pub(super) fn edit(source: &str, selection: Range<usize>, command: Command) -> Option<Change> {
    if selection.start > selection.end
        || !source.is_char_boundary(selection.start)
        || !source.is_char_boundary(selection.end)
    {
        return None;
    }
    let table = Table::parse(source)?;
    if matches!(command, Command::Format) {
        let mut change = table.render(0, 0)?;
        change.selection = map_format_position(source, &change.source, selection.start)?
            ..map_format_position(source, &change.source, selection.end)?;
        return Some(change);
    }
    let (mut row, mut col) = table.locate(source, &selection)?;
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
        Command::MoveRowUp | Command::MoveRowDown => {
            let other = if matches!(command, Command::MoveRowUp) {
                row.checked_sub(1)?
            } else {
                row + 1
            };
            // The header and delimiter are structural, never movable data rows.
            if row == 0 || other == 0 || other >= table.rows.len() {
                return None;
            }
            let a = row.min(other);
            let b = row.max(other);
            let first = line_range(source, &table.ranges[a][0]);
            let second = line_range(source, &table.ranges[b][0]);
            let body = |range: Range<usize>| source[range].trim_end_matches(['\r', '\n']);
            let ending = if source[second.clone()].ends_with('\n') {
                table.newline
            } else {
                ""
            };
            updated.replace_range(
                first.start..second.end,
                &format!("{}{}{}{}", body(second), table.newline, body(first), ending),
            );
            row = other;
        }
        Command::DeleteRow => {
            if row == 0 {
                return None;
            }
            let removed = line_range(source, &table.ranges[row][0]);
            let at_end = removed.end == source.len();
            updated.replace_range(removed, "");
            // The preceding line separator becomes the new table ending.
            // A classifier range excludes the note's final newline, so do
            // not leave that separator inside the new grid lease.
            if at_end && !table.trailing_newline {
                updated.truncate(updated.len() - table.newline.len());
            }
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
                    // Removing a bare first cell exposes its separator as an
                    // apparent outer border. Supply a real border so an empty
                    // next cell or literal block prefix retains its identity.
                    let border =
                        if col == 0 && !raw.trim_start_matches([' ', '\t']).starts_with('|') {
                            "|"
                        } else {
                            ""
                        };
                    edits.push((range.start..ranges[col + 1].start, border));
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
                if width == 2 {
                    // A one-column GFM table requires an outer pipe. Bare
                    // two-column rows lose their only delimiter on deletion;
                    // add borders around their exact remaining cell bytes.
                    let mut bordered = String::new();
                    for (physical, line) in updated.split_inclusive('\n').enumerate() {
                        let raw = line.trim_end_matches(['\r', '\n']);
                        if cells(raw, 0).is_none() {
                            let (indent, content) = raw.split_at(table.indent.len());
                            bordered.push_str(indent);
                            bordered.push('|');
                            bordered.push_str(content);
                            // Do not let an authored trailing backslash turn
                            // the new border into an escaped content pipe.
                            if content
                                .bytes()
                                .rev()
                                .take_while(|&byte| byte == b'\\')
                                .count()
                                % 2
                                == 1
                            {
                                bordered.push(' ');
                            }
                            bordered.push('|');
                        } else if bare_row_needs_pipe(raw, physical == 0) {
                            let (indent, content) = raw.split_at(table.indent.len());
                            bordered.push_str(indent);
                            bordered.push('|');
                            bordered.push_str(content);
                        } else {
                            bordered.push_str(raw);
                        }
                        bordered.push_str(&line[raw.len()..]);
                    }
                    updated = bordered;
                }
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
    fn display_edits_preserve_escaped_pipes_and_mixed_break_spellings() {
        let source =
            "  Name | Value\r\n  --- | ---:\r\n  left\\|right<BR/>line<br />end  |  `raw<br>`\r\n";
        let grid = CellGrid::new(source).unwrap();
        let display = "left|right\nline\nend";
        assert_eq!(cell_text(grid.value(1, 0).unwrap()), display);
        assert_eq!(cell_text(grid.value(1, 1).unwrap()), "`raw<br>`");
        let (same, unchanged) = grid.replace_text(source, 1, 0, display).unwrap();
        assert_eq!(
            unchanged.source, source,
            "opening and leaving a cell never normalizes its source"
        );
        let (_, change) = same
            .replace_text(source, 1, 0, "left|right\nline!\nend")
            .unwrap();
        assert_eq!(change.source, source.replace("line<br />", "line!<br />"));
        let grid = CellGrid::new(&change.source).unwrap();
        let (_, change) = grid
            .replace_text(&change.source, 1, 0, "left|right\nline!\nend\nשלום|😀")
            .unwrap();
        assert!(change
            .source
            .contains("end<BR/>שלום\\|😀  |  `raw<br>`\r\n"));
        assert_eq!(
            change.source.matches('\n').count(),
            source.matches('\n').count(),
            "multiline text stays in one GFM source row"
        );
    }

    #[test]
    fn typed_pipes_and_newlines_are_encoded_without_touching_neighbor_source() {
        let source = "| A | B |\n|---|---|\n| old | other\\|cell |";
        let grid = CellGrid::new(source).unwrap();
        let (next, change) = grid.replace_text(source, 1, 0, "one|two\nthree").unwrap();
        assert_eq!(change.source, source.replace("old", "one\\|two<br>three"));
        let (_, more) = next
            .replace_text(&change.source, 1, 0, "one|two\nthree ")
            .unwrap();
        assert_eq!(more.source, source.replace("old", "one\\|two<br>three "));
        assert_eq!(cell_text(r"a\\b\|c"), r"a\b|c");
        // Existing authored backslashes retain their meaning when a pipe is
        // appended to the same run rather than becoming an extra column.
        let source = "| A | B |\n|---|---|\n| slash\\ | neighbor |";
        let grid = CellGrid::new(source).unwrap();
        let (_, change) = grid.replace_text(source, 1, 0, "slash\\|end").unwrap();
        assert_eq!(cell_text(&change.source[change.selection]), "slash\\|end");
        assert!(grid.replace_text(source, 1, 0, "bad\0text").is_none());
        assert!(grid.replace_text(source, 1, 0, "bad\r\ntext").is_none());
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

#[cfg(test)]
mod remaining_scope_tests {
    use super::*;

    #[test]
    fn format_aligns_cyrillic_cjk_emoji_sequences_and_retains_the_caret() {
        let source = "| Имя | 值 |\r\n|:--|--:|\r\n|😀👨‍👩‍👧‍👦|é|\r\n|длинное|東京|";
        let caret = source.find("👨").unwrap();
        let change = edit(source, caret..caret, Command::Format).unwrap();
        assert_ne!(change.source, source, "positive control: formatting ran");
        assert!(change.source[change.selection.start..].starts_with("👨‍👩‍👧‍👦"));
        assert!(change.selection.is_empty());
        let lines: Vec<_> = change.source.split("\r\n").collect();
        let positions = |line: &str| {
            cells(line, 0)
                .unwrap()
                .iter()
                .map(|cell| {
                    let delimiter = line[cell.end..].find('|').unwrap() + cell.end;
                    UnicodeWidthStr::width(&line[..delimiter])
                })
                .collect::<Vec<_>>()
        };
        for line in &lines[1..] {
            assert_eq!(positions(line), positions(lines[0]));
        }
        assert!(!change.source.replace("\r\n", "").contains('\n'));
        let again = edit(&change.source, change.selection, Command::Format).unwrap();
        assert_eq!(again.source, change.source, "formatting is idempotent");
    }

    #[test]
    fn moving_rows_preserves_bytes_escapes_and_newlines_at_eof() {
        for newline in ["\n", "\r\n"] {
            for ending in [false, true] {
                let source = ["Name|値", "---|---:", r"one\|😀 | 東京  ", "two | Привет"]
                    .join(newline)
                    + if ending { newline } else { "" };
                let at = source.find("one").unwrap();
                let moved = edit(&source, at..at, Command::MoveRowDown).unwrap();
                assert_ne!(moved.source, source, "positive control: row moved");
                assert_eq!(&moved.source[moved.selection.clone()], r"one\|😀");
                let restored = edit(&moved.source, moved.selection, Command::MoveRowUp).unwrap();
                assert_eq!(restored.source, source);
                assert!(
                    edit(&source, at..at, Command::MoveRowUp).is_none(),
                    "header stays fixed"
                );
                let header = source.find("Name").unwrap();
                assert!(edit(&source, header..header, Command::MoveRowDown).is_none());
            }
        }
    }

    #[test]
    fn idle_cell_format_keeps_typed_spaces_and_escape_spellings() {
        let source = "|A|B|\n|--|--:|\n|old|a\\|b<BR/>😀|";
        let grid = CellGrid::new(source).unwrap();
        let (grid, _) = grid.replace_text(source, 1, 0, "  новый ").unwrap();
        let (formatted, change) = grid.format(1, 0).unwrap();
        assert!(change.source.contains("a\\|b<BR/>😀"));
        assert_eq!(formatted.value(1, 0), Some("  новый "));
        let (_, change) = formatted
            .replace_text(&change.source, 1, 0, "  новый !")
            .unwrap();
        assert!(change.source.contains("  новый !"));
    }
    #[test]
    fn a_raw_caret_in_padding_formats_and_navigates_in_the_same_cell() {
        let source = "|A| B |\n|--|--|\n| x    | y |";
        let at = source.find("x    ").unwrap() + 4;
        let formatted = edit(source, at..at, Command::Format).unwrap();
        assert_ne!(
            formatted.source, source,
            "positive control: formatter accepted padding"
        );
        assert!(formatted.source[..formatted.selection.start].ends_with('x'));
        let next = edit(source, at..at, Command::Next).unwrap();
        assert_eq!(&next.source[next.selection], "y");
    }
    #[test]
    fn formatting_maps_delimiter_carets_and_multi_cell_selections() {
        let source = "| Name | 值 |\r\n|:---------|--:|\r\n| 😀 | 東京 |";
        let at = source.find(":---------").unwrap() + 2;
        let changed = edit(source, at..at, Command::Format).unwrap();
        assert_ne!(
            changed.source, source,
            "positive control: delimiter edit formats"
        );
        assert!(changed.selection.is_empty());
        assert_eq!(
            changed.source[..changed.selection.start]
                .matches('\n')
                .count(),
            1
        );
        let from = source.find("Name").unwrap();
        let to = source.find("東京").unwrap() + "東京".len();
        let changed = edit(source, from..to, Command::Format).unwrap();
        assert!(changed.source[changed.selection].starts_with("Name"));
    }
    #[test]
    fn alignment_refuses_pathological_padding_expansion_before_allocation() {
        let small = "|a|b|\n|--|--|\n|x|y|";
        assert!(
            edit(small, 2..2, Command::Format).is_some(),
            "positive control: ordinary format accepted"
        );
        let large = format!(
            "|{}|b|\n|--|--|\n{}",
            "x".repeat(60000),
            "|x|y|\n".repeat(100)
        );
        assert!(
            Table::parse(&large).is_some(),
            "authored input is within the bound"
        );
        assert!(edit(&large, 2..2, Command::Format).is_none());
        assert!(CellGrid::new(&large).unwrap().format(0, 0).is_none());
    }
    #[test]
    fn deleting_the_last_data_row_keeps_the_original_table_ending() {
        for newline in ["\n", "\r\n"] {
            for ending in [false, true] {
                let head = ["A | B", "--- | ---:"].join(newline);
                let source = format!(
                    "{head}{newline}😀 | 東京{}",
                    if ending { newline } else { "" }
                );
                let at = source.find("😀").unwrap();
                let changed = edit(&source, at..at, Command::DeleteRow).unwrap();
                assert_ne!(changed.source, source, "positive control: last row deleted");
                assert_eq!(changed.source, head + if ending { newline } else { "" });
                assert_eq!(Table::parse(&changed.source).unwrap().rows.len(), 1);
            }
        }
    }

    #[test]
    fn deleting_from_a_bare_two_column_table_keeps_one_valid_lossless_column() {
        for newline in ["\n", "\r\n"] {
            for ending in [false, true] {
                let source = ["A|B", "---|---:", r"one\|😀|東京", "two|Привет"].join(newline)
                    + if ending { newline } else { "" };
                let at = source.find("東京").unwrap();
                let changed = edit(&source, at..at, Command::DeleteColumn).unwrap();
                let expected = ["|A|", "|---|", r"|one\|😀|", "|two|"].join(newline)
                    + if ending { newline } else { "" };
                assert_eq!(
                    changed.source, expected,
                    "the remaining cell source is unchanged"
                );
                assert_eq!(Table::parse(&changed.source).unwrap().alignment.len(), 1);
                assert_eq!(&changed.source[changed.selection.clone()], r"one\|😀");
                assert!(
                    edit(&changed.source, changed.selection, Command::Format).is_some(),
                    "positive control: the result remains formatable"
                );
            }
        }
    }
    #[test]
    fn adding_a_one_column_border_never_reinterprets_trailing_backslashes() {
        for count in 1..=4 {
            let raw = format!("trail{}", "\\".repeat(count));
            let source = format!("A|B\n---|---\nkeep|{raw}");
            let at = source.find("keep").unwrap();
            let changed = edit(&source, at..at, Command::DeleteColumn).unwrap();
            assert_eq!(Table::parse(&changed.source).unwrap().rows[1][0], raw);
            assert_eq!(
                &changed.source[changed.selection], raw,
                "remaining source bytes are exact"
            );
        }
    }
}
