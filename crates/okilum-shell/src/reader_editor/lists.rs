//! Lists that continue themselves (#1635). In a Markdown note Enter on a
//! list item starts the next one (`-`, `*`, `+`, `1.`, `1)`, `>`; a task
//! starts unchecked), Enter on an empty item outdents it and then leaves the
//! list, Shift+Enter breaks the line without a marker, and Tab / Shift+Tab
//! move the item with its children from anywhere in the line or a whole
//! multi-line selection. Ordered lists renumber below the edit.
//!
//! Every command is one minimal edit set applied as one Undo step (patch
//! 0087). Backspace right after a continuation takes back only the marker:
//! the line stays, as a soft break would have left it. Nothing runs while an
//! input method composes or settles ([`Reader::ime_allows`]), inside a code
//! fence or front matter, or while a completion owns Enter and Tab.
use super::ime_guard::Trigger;
use super::*;
use gpui_component::input::{Backspace, Enter, Indent, IndentInline, Outdent, OutdentInline};
use std::ops::Range;

/// Replace `range` of the text with the string.
pub(crate) type Edit = (Range<usize>, String);

/// What a list command does: minimal edits of the current source, sorted,
/// and the selection after them.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ListEdit {
    pub edits: Vec<Edit>,
    pub selection: Range<usize>,
    /// For a continuation, what Backspace right after it does.
    pub revert: Option<Revert>,
}

/// The marker a continuation inserted, in the text after it: Backspace with
/// the caret still right after it puts `soft` there instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Revert {
    pub prefix: Range<usize>,
    pub soft: String,
}

/// The revert of the last continuation, valid while the source and caret are
/// exactly as that continuation left them.
pub(super) struct PendingRevert {
    stamp: gpui_component::input::projection::SourceStamp,
    revert: Revert,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Marker {
    Bullet(u8),
    Ordered { number: u64, delim: u8 },
}

/// One line of the source, split into its Markdown prefix. Offsets are
/// absolute; `end` is before the newline (and a CR before it).
#[derive(Clone, Debug)]
pub(super) struct Line {
    pub(super) start: usize,
    pub(super) end: usize,
    /// After the `>` markers of block quotes and the space after each.
    pub(super) quote_end: usize,
    /// After the indentation inside the quote.
    pub(super) indent_end: usize,
    /// The marker characters: a bullet, or digits and their delimiter.
    marker: Option<(Marker, Range<usize>)>,
    task: bool,
    /// Where the text starts: after the marker, its spaces and a task box.
    pub(super) content: usize,
    /// Where an item's own content column is: after the marker and its
    /// spaces, before a task box.
    marker_end: usize,
}

impl Line {
    /// A list item: it has a bullet or a number.
    pub(super) fn item(&self) -> bool {
        self.marker.is_some()
    }

    fn depth(&self, text: &str) -> usize {
        text[self.start..self.quote_end].matches('>').count()
    }

    fn indent<'a>(&self, text: &'a str) -> &'a str {
        &text[self.quote_end..self.indent_end]
    }

    pub(super) fn width(&self, text: &str) -> usize {
        columns(self.indent(text))
    }

    pub(super) fn blank(&self, text: &str) -> bool {
        text[self.quote_end..self.end].trim().is_empty()
    }

    fn ordered(&self) -> Option<(u64, u8, Range<usize>)> {
        match self.marker {
            Some((Marker::Ordered { number, delim }, ref range)) => {
                Some((number, delim, range.start..range.end - 1))
            }
            _ => None,
        }
    }
}

/// Indentation width in columns, tabs to the next multiple of four.
fn columns(indent: &str) -> usize {
    indent.chars().fold(0, |col, ch| match ch {
        '\t' => col + 4 - col % 4,
        _ => col + 1,
    })
}

/// Lines scanned up or down from the edit before giving up: bounds the cost
/// of an Enter in a very long note.
pub(super) const SCAN_LINES: usize = 10_000;

/// Bytes of the UTF-8 byte order mark the note starts with, if any: the
/// first line starts after it.
pub(super) fn bom(text: &str) -> usize {
    if text.starts_with('\u{feff}') {
        '\u{feff}'.len_utf8()
    } else {
        0
    }
}

pub(super) fn line_start(text: &str, offset: usize) -> usize {
    text[..offset]
        .rfind('\n')
        .map_or(bom(text).min(offset), |at| at + 1)
}

pub(super) fn line_end(text: &str, start: usize) -> usize {
    let end = text[start..].find('\n').map_or(text.len(), |at| start + at);
    if end > start && text.as_bytes()[end - 1] == b'\r' {
        end - 1
    } else {
        end
    }
}

pub(super) fn line_at(text: &str, offset: usize) -> Line {
    let start = line_start(text, offset);
    parse(text, start, line_end(text, start))
}

fn previous_line(text: &str, line: &Line) -> Option<Line> {
    (line.start > bom(text)).then(|| line_at(text, line.start - 1))
}

fn next_line(text: &str, line: &Line) -> Option<Line> {
    let newline = text[line.end..].find('\n')? + line.end;
    let start = newline + 1;
    Some(parse(text, start, line_end(text, start)))
}

/// A `---`, `***` or `___` rule is not a list item.
fn thematic_break(rest: &[u8]) -> bool {
    let Some(&first) = rest.first() else {
        return false;
    };
    matches!(first, b'-' | b'*' | b'_')
        && rest.iter().filter(|&&b| b == first).count() >= 3
        && rest.iter().all(|&b| b == first || b == b' ' || b == b'\t')
}

fn parse(text: &str, start: usize, end: usize) -> Line {
    let bytes = &text.as_bytes()[start..end];
    let mut quote = 0;
    loop {
        let mut at = quote;
        while at < bytes.len() && at - quote < 3 && bytes[at] == b' ' {
            at += 1;
        }
        if bytes.get(at) != Some(&b'>') {
            break;
        }
        at += 1;
        if bytes.get(at) == Some(&b' ') {
            at += 1;
        }
        quote = at;
    }
    let indent = quote
        + bytes[quote..]
            .iter()
            .take_while(|b| matches!(b, b' ' | b'\t'))
            .count();
    let mut line = Line {
        start,
        end,
        quote_end: start + quote,
        indent_end: start + indent,
        marker: None,
        task: false,
        content: start + indent,
        marker_end: start + indent,
    };
    let rest = &bytes[indent..];
    if thematic_break(rest) {
        return line;
    }
    let (marker, len) = match rest.first() {
        Some(&bullet @ (b'-' | b'*' | b'+')) => (Marker::Bullet(bullet), 1),
        Some(b'0'..=b'9') => {
            let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
            match rest.get(digits) {
                Some(&delim @ (b'.' | b')')) if digits <= 9 => {
                    let number = text[start + indent..start + indent + digits]
                        .parse()
                        .unwrap_or(1);
                    (Marker::Ordered { number, delim }, digits + 1)
                }
                _ => return line,
            }
        }
        _ => return line,
    };
    let gap = rest[len..]
        .iter()
        .take_while(|b| matches!(b, b' ' | b'\t'))
        .count();
    if gap == 0 {
        return line;
    }
    let mut content = len + gap;
    let after = &rest[content..];
    let task = after.len() >= 3
        && after[0] == b'['
        && matches!(after[1], b' ' | b'x' | b'X')
        && after[2] == b']'
        && matches!(after.get(3), None | Some(b' ' | b'\t'));
    let marker_end = start + indent + content;
    if task {
        content += 3 + after[3..]
            .iter()
            .take_while(|b| matches!(b, b' ' | b'\t'))
            .count();
    }
    line.marker = Some((marker, start + indent..start + indent + len));
    line.task = task;
    line.content = start + indent + content;
    line.marker_end = marker_end;
    line
}

/// The document's newline: the first one decides, as the editor does.
fn newline(text: &str) -> &'static str {
    match text.find('\n') {
        Some(at) if at > 0 && text.as_bytes()[at - 1] == b'\r' => "\r\n",
        _ => "\n",
    }
}

/// True when the line at `start` is inside a code fence or the front matter:
/// no list there.
pub(super) fn in_code(text: &str, start: usize) -> bool {
    let mut from = bom(text);
    let body = &text[from..];
    // Front matter: a `---` first line up to a closing `---` or `...`.
    if body.starts_with("---") && line_end(text, from) - from == 3 {
        let mut at = line_end(text, from);
        for _ in 0..SCAN_LINES {
            let Some(newline) = text[at..].find('\n') else {
                break;
            };
            let next = at + newline + 1;
            at = line_end(text, next);
            let raw = text[next..at].trim_end();
            if raw == "---" || raw == "..." {
                if start < at {
                    return true;
                }
                from = at;
                break;
            }
        }
    }
    let mut fence: Option<(char, usize)> = None;
    for raw in text[from.min(start)..start].split('\n') {
        let indent = raw.len() - raw.trim_start_matches(' ').len();
        let body = raw
            .trim_start_matches(' ')
            .trim_end_matches(['\r', ' ', '\t']);
        let run = |ch: char| body.chars().take_while(|&c| c == ch).count();
        if let Some((ch, len)) = fence {
            if indent < 4 && run(ch) >= len && body.chars().all(|c| c == ch) {
                fence = None;
            }
        } else if indent < 4 {
            if let Some(ch) = ['`', '~'].into_iter().find(|&ch| run(ch) >= 3) {
                fence = Some((ch, run(ch)));
            }
        }
    }
    fence.is_some()
}

/// The nearest item above `line` in the same quote whose indentation is at
/// most `max` columns. Blank lines, deeper items and indented text are
/// passed over; a paragraph at the quote's own margin ends the list.
fn item_above(text: &str, line: &Line, max: usize) -> Option<Line> {
    let depth = line.depth(text);
    let mut current = line.clone();
    for _ in 0..SCAN_LINES {
        current = previous_line(text, &current)?;
        if current.depth(text) != depth {
            return None;
        }
        if current.blank(text) {
            continue;
        }
        let width = current.width(text);
        if current.marker.is_some() {
            if width <= max {
                return Some(current);
            }
        } else if width == 0 {
            return None;
        }
    }
    None
}

/// The item's children and continuation lines: what moves with it.
fn subtree(text: &str, item: &Line) -> Vec<Line> {
    let depth = item.depth(text);
    let width = item.width(text);
    let mut lines = Vec::new();
    let mut current = item.clone();
    for _ in 0..SCAN_LINES {
        let Some(next) = next_line(text, &current) else {
            break;
        };
        current = next;
        if current.depth(text) != depth {
            break;
        }
        if current.blank(text) || current.width(text) > width {
            lines.push(current.clone());
        } else {
            break;
        }
    }
    while lines.last().is_some_and(|line| line.blank(text)) {
        lines.pop();
    }
    lines
}

/// The ordered items of the list `item` is in: same quote, same indentation
/// and delimiter, above and below it, the item itself included. Deeper lines
/// are passed over; a shallower line or another kind of item ends the list.
fn siblings(text: &str, item: &Line) -> Vec<Line> {
    let Some((_, delim, _)) = item.ordered() else {
        return Vec::new();
    };
    let depth = item.depth(text);
    let width = item.width(text);
    let same = |line: &Line| match line.ordered() {
        Some((_, d, _)) => d == delim,
        None => false,
    };
    let mut items = Vec::new();
    let mut current = item.clone();
    for _ in 0..SCAN_LINES {
        let Some(line) = previous_line(text, &current) else {
            break;
        };
        current = line;
        if current.depth(text) != depth {
            break;
        }
        if current.blank(text) || current.width(text) > width {
            continue;
        }
        if current.width(text) < width || !same(&current) {
            break;
        }
        items.push(current.clone());
    }
    items.reverse();
    items.push(item.clone());
    let mut current = item.clone();
    for _ in 0..SCAN_LINES {
        let Some(line) = next_line(text, &current) else {
            break;
        };
        current = line;
        if current.depth(text) != depth {
            break;
        }
        if current.blank(text) || current.width(text) > width {
            continue;
        }
        if current.width(text) < width || !same(&current) {
            break;
        }
        items.push(current.clone());
    }
    items
}

/// The number every item of `item`'s list carries when the list is written
/// `1.` `1.` `1.` (any one number, at least two items): such a list is not
/// renumbered. `skip` (a line start) is the item an edit just placed, left out.
fn lazy_number(text: &str, item: &Line, skip: Option<usize>) -> Option<u64> {
    let numbers: Vec<u64> = siblings(text, item)
        .iter()
        .filter(|line| Some(line.start) != skip)
        .filter_map(|line| line.ordered().map(|(number, _, _)| number))
        .collect();
    numbers
        .first()
        .copied()
        .filter(|&first| numbers.len() >= 2 && numbers.iter().all(|&n| n == first))
}

/// Edits that number the ordered siblings after the item at `anchor` (a line
/// start) on from it; `first` renumbers the item itself. In a list numbered
/// `1.` throughout (`skip`, the item just placed, aside) every item gets that
/// number instead.
fn renumber(text: &str, anchor: usize, first: Option<u64>, skip: Option<usize>) -> Vec<Edit> {
    let item = line_at(text, anchor);
    let Some((number, delim, digits)) = item.ordered() else {
        return Vec::new();
    };
    let lazy = first
        .is_none()
        .then(|| lazy_number(text, &item, skip))
        .flatten();
    let step = if lazy.is_some() { 0 } else { 1 };
    let mut edits = Vec::new();
    let mut next = number;
    if let Some(first) = first.or(lazy) {
        if first != number {
            edits.push((digits, first.to_string()));
        }
        next = first;
    }
    let depth = item.depth(text);
    let width = item.width(text);
    let mut current = item;
    for _ in 0..SCAN_LINES {
        let Some(line) = next_line(text, &current) else {
            break;
        };
        current = line;
        if current.depth(text) != depth {
            break;
        }
        if current.blank(text) {
            continue;
        }
        let at = current.width(text);
        if at > width {
            continue;
        }
        if at < width {
            break;
        }
        match current.ordered() {
            Some((number, d, digits)) if d == delim => {
                next += step;
                if number != next {
                    edits.push((digits, next.to_string()));
                }
            }
            _ => break,
        }
    }
    edits
}

pub(super) fn apply(text: &str, edits: &[Edit]) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut at = 0;
    for (range, new) in edits {
        out.push_str(&text[at..range.start]);
        out.push_str(new);
        at = range.end;
    }
    out.push_str(&text[at..]);
    out
}

/// Where `offset` lands after sorted `edits`. `right`: an insertion at the
/// offset goes before it.
pub(super) fn map_offset(edits: &[Edit], offset: usize, right: bool) -> usize {
    let mut delta = 0isize;
    for (range, new) in edits {
        let grow = new.len() as isize - range.len() as isize;
        if range.is_empty() {
            if offset > range.start || (offset == range.start && right) {
                delta += grow;
                continue;
            }
            break;
        }
        if offset <= range.start {
            break;
        }
        if offset >= range.end {
            delta += grow;
            continue;
        }
        let start = range.start as isize + delta;
        return (start + if right { new.len() as isize } else { 0 }) as usize;
    }
    (offset as isize + delta) as usize
}

/// The edit without the bytes its old and new text share at either end.
fn minimal(text: &str, (range, new): Edit) -> Option<Edit> {
    let old = &text[range.clone()];
    if old == new {
        return None;
    }
    let prefix: usize = old
        .chars()
        .zip(new.chars())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum();
    let suffix: usize = old[prefix..]
        .chars()
        .rev()
        .zip(new[prefix..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum();
    Some((
        range.start + prefix..range.end - suffix,
        new[prefix..new.len() - suffix].to_string(),
    ))
}

/// Where a command renumbers: the line start of an ordered item in the text
/// after its primary edits, the number it gets (`None`: it keeps its own),
/// and the line start of the item the command placed, which does not decide
/// whether the list is numbered `1.` throughout.
type Anchor = (usize, Option<u64>, Option<usize>);

/// Combine a command's `primary` edits (of `text`) with the renumbering of
/// the ordered lists at `anchors` (line starts in the text after `primary`)
/// into one minimal edit set. `selection` is in the text after `primary`.
fn finish(
    text: &str,
    mut primary: Vec<Edit>,
    anchors: &[Anchor],
    selection: Range<usize>,
    revert: Option<Revert>,
) -> Option<ListEdit> {
    primary.sort_by_key(|(range, _)| (range.start, range.end));
    let after = apply(text, &primary);
    let mut numbers: Vec<Edit> = anchors
        .iter()
        .flat_map(|&(anchor, first, skip)| renumber(&after, anchor, first, skip))
        .collect();
    numbers.sort_by_key(|(range, _)| (range.start, range.end));
    numbers.dedup_by(|a, b| a.0 == b.0);
    let map = |offset: usize, right: bool| map_offset(&numbers, offset, right);
    let selection = if selection.is_empty() {
        let at = map(selection.start, true);
        at..at
    } else {
        map(selection.start, false)..map(selection.end, true)
    };
    let revert = revert.map(|revert| Revert {
        prefix: map(revert.prefix.start, false)..map(revert.prefix.end, true),
        soft: revert.soft,
    });
    // The primary edits' ranges in `after`.
    let mut delta = 0isize;
    let placed: Vec<Range<usize>> = primary
        .iter()
        .map(|(range, new)| {
            let start = (range.start as isize + delta) as usize;
            delta += new.len() as isize - range.len() as isize;
            start..start + new.len()
        })
        .collect();
    let mut folds: Vec<Vec<Edit>> = vec![Vec::new(); primary.len()];
    let mut edits = Vec::new();
    for (range, new) in numbers {
        if let Some(ix) = placed
            .iter()
            .position(|p| range.start < p.end && range.end > p.start)
        {
            let p = &placed[ix];
            if range.start < p.start || range.end > p.end {
                return None;
            }
            folds[ix].push((range.start - p.start..range.end - p.start, new));
            continue;
        }
        let shift: isize = primary
            .iter()
            .zip(&placed)
            .filter(|(_, p)| p.end <= range.start)
            .map(|((old, new), _)| new.len() as isize - old.len() as isize)
            .sum();
        let start = (range.start as isize - shift) as usize;
        edits.push((start..start + range.len(), new));
    }
    for ((_, new), fold) in primary.iter_mut().zip(folds) {
        for (range, digits) in fold.into_iter().rev() {
            new.replace_range(range, &digits);
        }
    }
    edits.extend(primary);
    edits.sort_by_key(|(range, _)| (range.start, range.end));
    if edits.windows(2).any(|pair| pair[0].0.end > pair[1].0.start) {
        return None;
    }
    let edits: Vec<Edit> = edits
        .into_iter()
        .filter_map(|edit| minimal(text, edit))
        .collect();
    (!edits.is_empty()).then_some(ListEdit {
        edits,
        selection,
        revert,
    })
}

fn marker_text(marker: Marker) -> String {
    match marker {
        Marker::Bullet(bullet) => (bullet as char).to_string(),
        Marker::Ordered { number, delim } => format!("{number}{}", delim as char),
    }
}

/// What a soft break leaves at the start of the new line: the quote, and
/// spaces up to where the item's text starts.
fn soft_prefix(text: &str, line: &Line) -> String {
    let mut prefix = text[line.start..line.indent_end].to_string();
    if line.marker.is_some() {
        prefix.push_str(&" ".repeat(text[line.indent_end..line.content].chars().count()));
    }
    prefix
}

/// Enter (or Shift+Enter) with `selection` in a Markdown note.
pub(crate) fn enter(text: &str, selection: Range<usize>, shift: bool) -> Option<ListEdit> {
    if selection.end > text.len()
        || !text.is_char_boundary(selection.start)
        || !text.is_char_boundary(selection.end)
        || text[selection.clone()].contains('\n')
    {
        return None;
    }
    let line = line_at(text, selection.start);
    if line.marker.is_none() && line.quote_end == line.start {
        return None;
    }
    if selection.start < line.content.min(line.end) || in_code(text, line.start) {
        return None;
    }
    let nl = newline(text);
    if shift {
        let insert = format!("{nl}{}", soft_prefix(text, &line));
        let caret = selection.start + insert.len();
        return finish(text, vec![(selection, insert)], &[], caret..caret, None);
    }
    let empty =
        selection.is_empty() && text[line.content.min(line.end)..line.end].trim().is_empty();
    if empty {
        return empty_item(text, &line);
    }
    let mut lazy = false;
    let next = match line.marker {
        Some((Marker::Ordered { number, delim }, _)) => {
            // `1.` on every item: keep it, renumber nothing.
            lazy = lazy_number(text, &line, None) == Some(number);
            let number = if lazy { number } else { number + 1 };
            Some(Marker::Ordered { number, delim })
        }
        Some((marker, _)) => Some(marker),
        None => None,
    };
    let mut prefix = text[line.start..line.indent_end].to_string();
    if let (Some(next), Some((_, range))) = (next, &line.marker) {
        prefix.push_str(&marker_text(next));
        prefix.push_str(&text[range.end..line.marker_end]);
        if line.task {
            prefix.push_str("[ ] ");
        }
    }
    let insert = format!("{nl}{prefix}");
    let new_line = selection.start + nl.len();
    let caret = new_line + prefix.len();
    let anchors = if line.ordered().is_some() && !lazy {
        vec![(new_line, None, Some(new_line))]
    } else {
        Vec::new()
    };
    let revert = Revert {
        prefix: new_line..caret,
        soft: soft_prefix(text, &line),
    };
    finish(
        text,
        vec![(selection, insert)],
        &anchors,
        caret..caret,
        Some(revert),
    )
}

/// Enter on an item with no text: a nested item moves out to its parent's
/// level, a top-level item leaves the list, an empty quote line leaves one
/// quote.
fn empty_item(text: &str, line: &Line) -> Option<ListEdit> {
    let Some((marker, _)) = line.marker else {
        let last = text[line.start..line.quote_end].rfind('>')? + line.start;
        let caret = last;
        return finish(
            text,
            vec![(last..line.end, String::new())],
            &[],
            caret..caret,
            None,
        );
    };
    let width = line.width(text);
    if width == 0 {
        let sibling = item_above(text, line, 0).filter(|above| above.width(text) == 0);
        let anchors: Vec<Anchor> = sibling
            .filter(|above| above.ordered().is_some())
            .map(|above| (above.start, None, None))
            .into_iter()
            .collect();
        let caret = line.quote_end;
        return finish(
            text,
            vec![(line.quote_end..line.end, String::new())],
            &anchors,
            caret..caret,
            None,
        );
    }
    let parent = item_above(text, line, width - 1);
    let (indent, marker) = match &parent {
        Some(parent) => (
            parent.indent(text).to_string(),
            match parent.marker {
                Some((Marker::Ordered { number, delim }, _)) => Marker::Ordered {
                    number: lazy_number(text, parent, None).unwrap_or(number + 1),
                    delim,
                },
                Some((marker, _)) => marker,
                None => marker,
            },
        ),
        None => (String::new(), marker),
    };
    let replacement = format!(
        "{indent}{} {}",
        marker_text(marker),
        if line.task { "[ ] " } else { "" }
    );
    let caret = line.quote_end + replacement.len();
    let anchors: Vec<Anchor> = parent
        .filter(|parent| parent.ordered().is_some())
        .map(|parent| (parent.start, None, Some(line.start)))
        .into_iter()
        .collect();
    finish(
        text,
        vec![(line.quote_end..line.end, replacement)],
        &anchors,
        caret..caret,
        None,
    )
}

/// Backspace right after a continuation: the marker it inserted becomes the
/// soft-break indentation, and the numbers below go back.
pub(crate) fn revert(text: &str, revert: &Revert) -> Option<ListEdit> {
    let Revert { prefix, soft } = revert;
    if prefix.end > text.len() || !text.is_char_boundary(prefix.start) {
        return None;
    }
    let caret = prefix.start + soft.len();
    let line = line_at(text, prefix.start);
    let anchors: Vec<Anchor> = previous_line(text, &line)
        .filter(|above| above.ordered().is_some())
        .map(|above| (above.start, None, None))
        .into_iter()
        .collect();
    finish(
        text,
        vec![(prefix.clone(), soft.clone())],
        &anchors,
        caret..caret,
        None,
    )
}

/// How many bytes at the start of `indent` cover `cols` columns: all of it
/// when it is narrower, and a whole tab when one crosses the limit.
fn indent_bytes(indent: &str, cols: usize) -> usize {
    let mut col = 0;
    let mut bytes = 0;
    for ch in indent.chars() {
        if col >= cols {
            break;
        }
        col = match ch {
            '\t' => col + 4 - col % 4,
            _ => col + 1,
        };
        bytes += ch.len_utf8();
    }
    bytes
}

/// Tab (`outdent`: Shift+Tab) with `selection` in a Markdown note: the item
/// on the caret line with its children, or every line of a multi-line
/// selection, one level in or out.
pub(crate) fn indent(text: &str, selection: Range<usize>, outdent: bool) -> Option<ListEdit> {
    if selection.end > text.len() {
        return None;
    }
    let first = line_at(text, selection.start);
    let mut last_offset = selection.end;
    if selection.end > selection.start && selection.end == line_start(text, selection.end) {
        last_offset -= 1;
    }
    let last = line_at(text, last_offset.max(first.start));
    if in_code(text, first.start) {
        return None;
    }
    let mut block = vec![first.clone()];
    let mut current = first.clone();
    while current.start < last.start {
        current = next_line(text, &current)?;
        block.push(current.clone());
    }
    let multi = block.len() > 1;
    if !multi && first.marker.is_none() {
        return None;
    }
    if let Some(item) = block.last().filter(|line| line.marker.is_some()).cloned() {
        block.extend(subtree(text, &item));
    }
    let lead = block.iter().find(|line| line.marker.is_some()).cloned();
    let tabs = block
        .iter()
        .any(|line| !line.blank(text) && line.indent(text).starts_with('\t'));
    let unit = if tabs { 4 } else { 2 };
    let mut old_sibling = None;
    let (add, remove) = match &lead {
        Some(lead) => {
            let width = lead.width(text);
            old_sibling = item_above(text, lead, width).filter(|above| above.width(text) == width);
            if outdent {
                let parent = if width > 0 {
                    item_above(text, lead, width - 1)
                } else {
                    None
                };
                let target = parent.map_or(0, |parent| parent.width(text));
                (0, width - target)
            } else {
                let step = old_sibling.as_ref().map_or(unit, |sibling| {
                    columns(&text[sibling.indent_end..sibling.marker_end])
                });
                (step, 0)
            }
        }
        None if outdent => (0, unit),
        None => (unit, 0),
    };
    let insert = if tabs {
        "\t".to_string()
    } else {
        " ".repeat(add)
    };
    let mut primary = Vec::new();
    for line in &block {
        if line.blank(text) {
            continue;
        }
        if outdent {
            let bytes = indent_bytes(line.indent(text), remove);
            if bytes > 0 {
                primary.push((line.quote_end..line.quote_end + bytes, String::new()));
            }
        } else {
            primary.push((line.quote_end..line.quote_end, insert.clone()));
        }
    }
    if primary.is_empty() {
        return None;
    }
    let selection = if selection.is_empty() {
        let at = map_offset(&primary, selection.start, true);
        at..at
    } else {
        map_offset(&primary, selection.start, false)..map_offset(&primary, selection.end, true)
    };
    let mut anchors = Vec::new();
    if let Some(lead) = &lead {
        let after = apply(text, &primary);
        let moved = line_at(&after, map_offset(&primary, lead.start, false));
        let width = moved.width(&after);
        let sibling =
            item_above(&after, &moved, width).filter(|above| above.width(&after) == width);
        match sibling {
            Some(sibling) if sibling.ordered().is_some() => {
                anchors.push((sibling.start, None, Some(moved.start)))
            }
            None if !outdent && moved.ordered().is_some() => {
                anchors.push((moved.start, Some(1), None))
            }
            _ => {}
        }
        // The level it left: the items after it close up.
        if let Some(old) = old_sibling.filter(|old| old.ordered().is_some()) {
            anchors.push((old.start, None, None));
        }
    }
    finish(text, primary, &anchors, selection, None)
}

impl Reader {
    /// The note editor, when a list command may run in it now: a Markdown
    /// note, focused and editable, no completion owning the key.
    pub(super) fn list_input(&self, window: &Window, cx: &App) -> Option<Entity<EditorState>> {
        let editing = self.editing.as_ref()?;
        let input = editing.input.read(cx);
        (editing.markdown
            && input.focus_handle(cx).is_focused(window)
            && input.is_editable()
            && !input.completion_active(cx))
        .then(|| editing.input.clone())
    }

    pub(super) fn apply_list_edit(
        &mut self,
        input: &Entity<EditorState>,
        edit: ListEdit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let applied = input.update(cx, |state, cx| {
            state.replace_source_edits(&edit.edits, edit.selection.clone(), window, cx)
        });
        if let Some(editing) = self.editing.as_mut() {
            editing.list_revert =
                applied
                    .then_some(edit.revert)
                    .flatten()
                    .map(|revert| PendingRevert {
                        stamp: input.read(cx).source_stamp(),
                        revert,
                    });
        }
        applied
    }

    /// Enter and Shift+Enter in the note editor (captured before the input).
    pub(super) fn list_enter(
        &mut self,
        action: &Enter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if action.secondary || !self.ime_allows(Trigger::Enter, cx) {
            return;
        }
        let Some(input) = self.list_input(window, cx) else {
            return;
        };
        let (text, selection) = {
            let state = input.read(cx);
            (state.value(), state.selected_range())
        };
        if let Some(edit) = enter(&text, selection, action.shift) {
            if self.apply_list_edit(&input, edit, window, cx) {
                cx.stop_propagation();
            }
        }
    }

    /// Tab and Shift+Tab, Cmd/Ctrl+] and Cmd/Ctrl+[ in the note editor.
    fn list_indent(&mut self, outdent: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ime_allows(Trigger::Tab, cx) {
            return;
        }
        let Some(input) = self.list_input(window, cx) else {
            return;
        };
        let (text, selection) = {
            let state = input.read(cx);
            (state.value(), state.selected_range())
        };
        if let Some(edit) = indent(&text, selection, outdent) {
            if self.apply_list_edit(&input, edit, window, cx) {
                cx.stop_propagation();
            }
        }
    }

    /// Backspace right after a continuation takes back only its marker.
    pub(super) fn list_backspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self
            .editing
            .as_mut()
            .and_then(|editing| editing.list_revert.take())
        else {
            return;
        };
        // Nothing runs on preedit; a commit has changed the source anyway.
        if !self.ime_allows(Trigger::Text, cx) {
            return;
        }
        let Some(input) = self.list_input(window, cx) else {
            return;
        };
        let (text, selection, stamp) = {
            let state = input.read(cx);
            (state.value(), state.selected_range(), state.source_stamp())
        };
        let caret = pending.revert.prefix.end;
        if stamp != pending.stamp || selection != (caret..caret) {
            return;
        }
        if let Some(edit) = revert(&text, &pending.revert) {
            if self.apply_list_edit(&input, edit, window, cx) {
                cx.stop_propagation();
            }
        }
    }

    /// The list keys on the note editor's column, captured before the input
    /// handles them; each falls through when no list command applies.
    pub(super) fn list_actions(&self, column: Div, cx: &mut Context<Self>) -> Div {
        column
            .capture_action(
                cx.listener(|this, action: &Enter, window, cx| this.list_enter(action, window, cx)),
            )
            .capture_action(
                cx.listener(|this, _: &Backspace, window, cx| this.list_backspace(window, cx)),
            )
            .capture_action(
                cx.listener(|this, _: &IndentInline, window, cx| {
                    this.list_indent(false, window, cx)
                }),
            )
            .capture_action(
                cx.listener(|this, _: &Indent, window, cx| this.list_indent(false, window, cx)),
            )
            .capture_action(
                cx.listener(|this, _: &OutdentInline, window, cx| {
                    this.list_indent(true, window, cx)
                }),
            )
            .capture_action(
                cx.listener(|this, _: &Outdent, window, cx| this.list_indent(true, window, cx)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    /// Apply the command at the `|` in `text`; the result with its caret.
    fn run(
        text: &str,
        command: impl FnOnce(&str, Range<usize>) -> Option<ListEdit>,
    ) -> Option<String> {
        let (text, selection) = marked(text);
        let edit = command(&text, selection)?;
        let mut out = apply(&text, &edit.edits);
        if edit.selection.is_empty() {
            out.insert(edit.selection.start, '|');
        } else {
            out.insert(edit.selection.end, ']');
            out.insert(edit.selection.start, '[');
        }
        Some(out)
    }

    /// `|` is the caret; `[` and `]` a selection.
    fn marked(text: &str) -> (String, Range<usize>) {
        if let Some(at) = text.find('|') {
            return (text.replacen('|', "", 1), at..at);
        }
        let start = text.find('[').unwrap();
        let end = text.find(']').unwrap() - 1;
        (text.replacen('[', "", 1).replacen(']', "", 1), start..end)
    }

    fn enter_at(text: &str) -> Option<String> {
        run(text, |t, s| enter(t, s, false))
    }

    fn tab(text: &str, outdent: bool) -> Option<String> {
        run(text, |t, s| indent(t, s, outdent))
    }

    #[test]
    fn enter_continues_every_prefix() {
        for (before, after) in [
            ("- a|", "- a\n- |"),
            ("* a|", "* a\n* |"),
            ("+ a|", "+ a\n+ |"),
            ("1. a|", "1. a\n2. |"),
            ("1) a|", "1) a\n2) |"),
            ("> a|", "> a\n> |"),
            ("- [x] a|", "- [x] a\n- [ ] |"),
            ("- [ ] a|", "- [ ] a\n- [ ] |"),
            ("3. [X] a|", "3. [X] a\n4. [ ] |"),
            ("  - a|", "  - a\n  - |"),
            ("> - a|", "> - a\n> - |"),
            ("> > 9. a|", "> > 9. a\n> > 10. |"),
            ("-   a|", "-   a\n-   |"),
            ("- a|bc", "- a\n- |bc"),
            ("- [a]bc", "- \n- |bc"),
        ] {
            assert_eq!(enter_at(before).as_deref(), Some(after), "{before:?}");
        }
    }

    /// A note that starts with a byte order mark: its first line is a list
    /// item like any other, and the mark stays only at the start.
    #[test]
    fn the_first_line_of_a_note_with_a_bom() {
        let bom = '\u{feff}';
        assert_eq!(
            enter_at(&format!("{bom}- a|")),
            Some(format!("{bom}- a\n- |"))
        );
        assert_eq!(
            enter_at(&format!("{bom}1. a\n2. |")),
            Some(format!("{bom}1. a\n|"))
        );
        assert_eq!(
            enter_at(&format!("{bom}- a\n  - b|")),
            Some(format!("{bom}- a\n  - b\n  - |"))
        );
        assert_eq!(
            tab(&format!("{bom}- a\n- b|"), false),
            Some(format!("{bom}- a\n  - b|"))
        );
        assert_eq!(
            tab(&format!("{bom}- a\n  - b|"), true),
            Some(format!("{bom}- a\n- b|"))
        );
        assert_eq!(enter_at(&format!("{bom}```\n- a|")), None, "code");
        assert_eq!(
            enter_at(&format!("{bom}---\n- a|\n---\n")),
            None,
            "front matter"
        );
    }

    #[test]
    fn enter_on_no_list_or_in_code_does_nothing() {
        for text in [
            "a|",
            "|- a",
            "-a|",
            "---|",
            "* * *|",
            "#1. a|",
            "```\n- a|",
            "~~~md\n> a|",
            "---\ntags:\n- a|\n---\n",
        ] {
            assert_eq!(enter_at(text), None, "{text:?}");
        }
        assert_eq!(
            enter_at("```\ncode\n```\n- a|").as_deref(),
            Some("```\ncode\n```\n- a\n- |"),
            "after the fence closes"
        );
        assert_eq!(
            enter_at("---\ntags: x\n---\n- a|").as_deref(),
            Some("---\ntags: x\n---\n- a\n- |"),
            "after the front matter"
        );
    }

    #[test]
    fn enter_on_an_empty_item_outdents_then_leaves() {
        assert_eq!(enter_at("- a\n  - |").as_deref(), Some("- a\n- |"));
        assert_eq!(enter_at("- a\n- |").as_deref(), Some("- a\n|"));
        assert_eq!(enter_at("1. a\n   - |").as_deref(), Some("1. a\n2. |"));
        assert_eq!(enter_at("- a\n  - [ ] |").as_deref(), Some("- a\n- [ ] |"));
        assert_eq!(enter_at("- [ ] |").as_deref(), Some("|"));
        assert_eq!(enter_at("> - a\n> - |").as_deref(), Some("> - a\n> |"));
        assert_eq!(enter_at("> a\n> |").as_deref(), Some("> a\n|"));
        assert_eq!(enter_at("> > |").as_deref(), Some("> |"));
        assert_eq!(
            enter_at("1. a\n   1. b\n   2. |\n2. c").as_deref(),
            Some("1. a\n   1. b\n2. |\n3. c"),
            "the outdented item joins its parent's numbering"
        );
    }

    #[test]
    fn shift_enter_breaks_without_a_marker() {
        let soft = |text: &str| run(text, |t, s| enter(t, s, true));
        assert_eq!(soft("- a|").as_deref(), Some("- a\n  |"));
        assert_eq!(soft("10. a|b").as_deref(), Some("10. a\n    |b"));
        assert_eq!(soft("- [ ] a|").as_deref(), Some("- [ ] a\n      |"));
        assert_eq!(soft("> a|").as_deref(), Some("> a\n> |"));
        assert_eq!(soft("a|"), None);
    }

    #[test]
    fn ordered_lists_renumber_below_the_edit() {
        assert_eq!(
            enter_at("1. a|\n2. b\n   - x\n3. c\n\n4. d\npara\n5. e").as_deref(),
            Some("1. a\n2. |\n3. b\n   - x\n4. c\n\n5. d\npara\n5. e")
        );
        assert_eq!(
            enter_at("1. a\n1. b|\n1. c").as_deref(),
            Some("1. a\n1. b\n1. |\n1. c"),
            "a list numbered 1. throughout stays so"
        );
        assert_eq!(
            enter_at("1. a|\n2) b").as_deref(),
            Some("1. a\n2. |\n2) b"),
            "another delimiter is another list"
        );
    }

    /// A list written `1.` on every item stays so whichever command runs in
    /// it, from its first item too; a single `1.` item still counts on.
    #[test]
    fn a_list_numbered_one_throughout_stays_so() {
        assert_eq!(
            enter_at("1. a|\n1. b\n1. c").as_deref(),
            Some("1. a\n1. |\n1. b\n1. c"),
            "Enter on the first item"
        );
        assert_eq!(enter_at("1. a|").as_deref(), Some("1. a\n2. |"));
        assert_eq!(
            enter_at("1. a\n   1. |\n1. c").as_deref(),
            Some("1. a\n1. |\n1. c"),
            "an empty nested item outdents into it"
        );
        assert_eq!(
            tab("1. a\n1. b|\n1. c", false).as_deref(),
            Some("1. a\n   1. b|\n1. c"),
            "Tab leaves the level it left alone"
        );
        assert_eq!(
            tab("1. a\n   1. b|\n1. c", true).as_deref(),
            Some("1. a\n1. b|\n1. c"),
            "Shift+Tab joins it as 1."
        );
        assert_eq!(
            tab("1. a\n   1. x\n   1. y\n2. b|", false).as_deref(),
            Some("1. a\n   1. x\n   1. y\n   1. b|"),
            "Tab into such a list takes its number"
        );
        let (text, selection) = marked("1. a\n1. b|");
        let edit = enter(&text, selection, false).unwrap();
        let after = apply(&text, &edit.edits);
        let back = super::revert(&after, &edit.revert.unwrap()).unwrap();
        assert_eq!(apply(&after, &back.edits), "1. a\n1. b\n   ");
    }

    #[test]
    fn edits_are_minimal() {
        let (text, selection) = marked("1. a|\n2. b\n3. c");
        let edit = enter(&text, selection, false).unwrap();
        assert_eq!(
            edit.edits,
            vec![
                (4..4, "\n2. ".to_string()),
                (5..6, "3".to_string()),
                (10..11, "4".to_string())
            ]
        );
        let (text, selection) = marked("- a\n    - |");
        let edit = enter(&text, selection, false).unwrap();
        assert_eq!(
            edit.edits,
            vec![(4..8, String::new())],
            "outdent removes only spaces"
        );
    }

    #[test]
    fn tab_moves_the_item_and_its_children_from_anywhere_in_the_line() {
        assert_eq!(tab("- a\n- b|c", false).as_deref(), Some("- a\n  - b|c"));
        assert_eq!(tab("- a\n  - b|c", true).as_deref(), Some("- a\n- b|c"));
        assert_eq!(
            tab("- a\n- |b\n  - c\n  text\n- d", false).as_deref(),
            Some("- a\n  - |b\n    - c\n    text\n- d")
        );
        assert_eq!(
            tab("- a\n  - |b\n    - c\n- d", true).as_deref(),
            Some("- a\n- |b\n  - c\n- d")
        );
        assert_eq!(
            tab("\t- a\n\t- |b", false).as_deref(),
            Some("\t- a\n\t\t- |b")
        );
        assert_eq!(
            tab("> - a\n> - b|", false).as_deref(),
            Some("> - a\n>   - b|")
        );
        assert_eq!(tab("- |a", true), None, "nothing to take out");
        assert_eq!(tab("plain| text", false), None, "Tab keeps its own meaning");
    }

    #[test]
    fn tab_renumbers_both_levels() {
        assert_eq!(
            tab("1. a\n2. b|\n3. c", false).as_deref(),
            Some("1. a\n   1. b|\n2. c")
        );
        assert_eq!(
            tab("1. a\n   1. b|\n2. c", true).as_deref(),
            Some("1. a\n2. b|\n3. c")
        );
        assert_eq!(
            tab("1. a\n   1. x\n2. b|", false).as_deref(),
            Some("1. a\n   1. x\n   2. b|")
        );
    }

    #[test]
    fn tab_on_a_multi_line_selection_moves_every_line() {
        assert_eq!(
            tab("- a\n[- b\n- c]\n- d", false).as_deref(),
            Some("- a\n[  - b\n  - c]\n- d")
        );
        assert_eq!(
            tab("- a\n[  - b\n\n  - c\n]- d", true).as_deref(),
            Some("- a\n[- b\n\n- c\n]- d"),
            "a selection ending at a line start leaves that line"
        );
        assert_eq!(
            tab("[para\nmore]", false).as_deref(),
            Some("[  para\n  more]")
        );
    }

    #[test]
    fn backspace_after_a_continuation_takes_back_only_the_marker() {
        let (text, selection) = marked("1. a|\n2. b");
        let edit = enter(&text, selection, false).unwrap();
        let after = apply(&text, &edit.edits);
        assert_eq!(after, "1. a\n2. \n3. b");
        let revert = edit.revert.unwrap();
        assert_eq!(edit.selection.start, revert.prefix.end);
        let back = super::revert(&after, &revert).unwrap();
        assert_eq!(apply(&after, &back.edits), "1. a\n   \n2. b");
        assert_eq!(back.selection, 8..8);
    }

    #[test]
    fn crlf_notes_keep_crlf() {
        assert_eq!(enter_at("x\r\n- a|").as_deref(), Some("x\r\n- a\r\n- |"));
    }
}

/// The keys in a real note editor: the capture on the editor column, the
/// input's own handling around it, and Undo.
#[cfg(test)]
mod key_tests {
    use super::*;
    // `super::*` brings gpui's `test`; `#[gpui::test]` expands to `#[test]`.
    use ::core::prelude::v1::test;
    use gpui_component::input::{Redo, Undo};
    use std::time::Duration;

    struct Note {
        reader: Entity<Reader>,
        input: Entity<EditorState>,
        _directory: tempfile::TempDir,
    }

    fn open(cx: &mut TestAppContext) -> (Note, &mut VisualTestContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("note.md"), "\n").unwrap();
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        open_path: Some(root.join("note.md")),
                        index_dir: Some(directory.path().join("index")),
                        session_directory: Some(directory.path().join("state")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |r, window, cx| r.toggle_source(window, cx));
        visual.run_until_parked();
        let input = reader.read_with(visual, |r, _| r.editing.as_ref().unwrap().input.clone());
        (
            Note {
                reader,
                input,
                _directory: directory,
            },
            visual,
        )
    }

    /// Replace the note with `text` (no history) and put the caret at `|` or
    /// the selection at `[` … `]`.
    fn set(note: &Note, visual: &mut VisualTestContext, text: &str) {
        let (text, selection) = if text.contains(['|', '[']) {
            tests_marked(text)
        } else {
            (text.to_string(), text.len()..text.len())
        };
        note.input
            .update_in(visual, |i, window, cx| i.set_value(text, window, cx));
        note.input
            .update(visual, |i, cx| i.set_selected_range(selection, cx));
        visual.run_until_parked();
    }

    fn tests_marked(text: &str) -> (String, Range<usize>) {
        if let Some(at) = text.find('|') {
            return (text.replacen('|', "", 1), at..at);
        }
        let start = text.find('[').unwrap();
        let end = text.find(']').unwrap() - 1;
        (text.replacen('[', "", 1).replacen(']', "", 1), start..end)
    }

    /// The note with its caret marked, as `set` takes it.
    fn shown(note: &Note, visual: &mut VisualTestContext) -> String {
        note.input.read_with(visual, |i, _| {
            let mut text = i.value().to_string();
            let range = i.selected_range();
            if range.is_empty() {
                text.insert(range.start, '|');
            } else {
                text.insert(range.end, ']');
                text.insert(range.start, '[');
            }
            text
        })
    }

    fn keys(visual: &mut VisualTestContext, keys: &str) {
        visual.simulate_keystrokes(keys);
        visual.run_until_parked();
    }

    fn undo(visual: &mut VisualTestContext) {
        visual.dispatch_action(Undo);
        visual.run_until_parked();
    }

    fn redo(visual: &mut VisualTestContext) {
        visual.dispatch_action(Redo);
        visual.run_until_parked();
    }

    /// Each prefix continues; one Undo takes the continuation back and
    /// leaves what was typed, Redo brings it again.
    #[gpui::test]
    fn every_prefix_continues_and_one_undo_takes_it_back(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        for (prefix, next) in [
            ("- ", "- "),
            ("* ", "* "),
            ("+ ", "+ "),
            ("1. ", "2. "),
            ("1) ", "2) "),
            ("> ", "> "),
            ("- [x] ", "- [ ] "),
            ("  - ", "  - "),
        ] {
            set(&note, visual, "");
            visual.simulate_input(&format!("{prefix}item"));
            visual.run_until_parked();
            keys(visual, "enter");
            assert_eq!(
                shown(&note, visual),
                format!("{prefix}item\n{next}|"),
                "{prefix:?} continues"
            );
            undo(visual);
            assert_eq!(
                shown(&note, visual),
                format!("{prefix}item|"),
                "{prefix:?}: one Undo keeps the typing"
            );
            redo(visual);
            assert_eq!(shown(&note, visual), format!("{prefix}item\n{next}|"));
        }
        // Not a list, or a list inside a fence: the input's own Enter.
        set(&note, visual, "text|");
        keys(visual, "enter");
        assert_eq!(shown(&note, visual), "text\n|");
        set(&note, visual, "```\n- a|");
        keys(visual, "enter");
        assert_eq!(shown(&note, visual), "```\n- a\n|", "no list in code");
    }

    #[gpui::test]
    fn typing_after_a_continuation_is_its_own_undo_step(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        set(&note, visual, "");
        visual.simulate_input("- a");
        keys(visual, "enter");
        visual.simulate_input("b");
        visual.run_until_parked();
        assert_eq!(shown(&note, visual), "- a\n- b|");
        undo(visual);
        assert_eq!(shown(&note, visual), "- a\n- |");
        undo(visual);
        assert_eq!(shown(&note, visual), "- a|");
    }

    /// The continuation and the renumbering below it are one Undo step.
    #[gpui::test]
    fn renumbering_is_part_of_the_same_undo_step(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        set(&note, visual, "1. a|\n2. b\n3. c");
        keys(visual, "enter");
        assert_eq!(shown(&note, visual), "1. a\n2. |\n3. b\n4. c");
        undo(visual);
        assert_eq!(shown(&note, visual), "1. a|\n2. b\n3. c");
        redo(visual);
        assert_eq!(shown(&note, visual), "1. a\n2. |\n3. b\n4. c");
    }

    #[gpui::test]
    fn enter_on_an_empty_item_outdents_then_leaves_the_list(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        set(&note, visual, "- a\n  - b\n  - |");
        keys(visual, "enter");
        assert_eq!(shown(&note, visual), "- a\n  - b\n- |", "outdent");
        keys(visual, "enter");
        assert_eq!(shown(&note, visual), "- a\n  - b\n|", "out of the list");
        undo(visual);
        assert_eq!(shown(&note, visual), "- a\n  - b\n- |");
        set(&note, visual, "> quote\n> |");
        keys(visual, "enter");
        assert_eq!(shown(&note, visual), "> quote\n|");
    }

    #[gpui::test]
    fn shift_enter_breaks_the_line_without_a_marker(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        set(&note, visual, "- [ ] a|");
        keys(visual, "shift-enter");
        assert_eq!(shown(&note, visual), "- [ ] a\n      |");
        undo(visual);
        assert_eq!(shown(&note, visual), "- [ ] a|");
    }

    #[gpui::test]
    fn tab_and_shift_tab_work_from_anywhere_and_on_a_selection(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        set(&note, visual, "1. a\n2. b|c\n3. d");
        keys(visual, "tab");
        assert_eq!(shown(&note, visual), "1. a\n   1. b|c\n2. d");
        keys(visual, "shift-tab");
        assert_eq!(shown(&note, visual), "1. a\n2. b|c\n3. d");
        undo(visual);
        assert_eq!(shown(&note, visual), "1. a\n   1. b|c\n2. d", "one step");
        undo(visual);
        assert_eq!(shown(&note, visual), "1. a\n2. b|c\n3. d", "one step");

        set(&note, visual, "- a\n[- b\n- c]\n- d");
        keys(visual, "tab");
        assert_eq!(shown(&note, visual), "- a\n[  - b\n  - c]\n- d");
        keys(visual, "shift-tab");
        assert_eq!(shown(&note, visual), "- a\n[- b\n- c]\n- d");
        undo(visual);
        undo(visual);
        assert_eq!(shown(&note, visual), "- a\n[- b\n- c]\n- d");

        // Plain text keeps the input's Tab.
        set(&note, visual, "ab|");
        keys(visual, "tab");
        assert_eq!(shown(&note, visual), "ab  |");
    }

    #[gpui::test]
    fn backspace_right_after_a_continuation_takes_back_only_the_marker(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        set(&note, visual, "1. a|\n2. b");
        keys(visual, "enter");
        assert_eq!(shown(&note, visual), "1. a\n2. |\n3. b");
        keys(visual, "backspace");
        assert_eq!(shown(&note, visual), "1. a\n   |\n2. b", "the line stays");
        keys(visual, "backspace");
        assert_eq!(
            shown(&note, visual),
            "1. a\n  |\n2. b",
            "then a plain delete"
        );

        // Not right after: the caret moved away and the text changed.
        set(&note, visual, "- a|");
        keys(visual, "enter");
        visual.simulate_input("x");
        keys(visual, "backspace");
        keys(visual, "backspace");
        assert_eq!(shown(&note, visual), "- a\n-|");
    }

    /// Live Preview edits the same source through the same keys: the list
    /// commands run there too, each one Undo step.
    #[gpui::test]
    fn lists_continue_and_indent_in_live_preview(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        note.reader
            .update_in(visual, |r, window, cx| r.set_live_preview(true, window, cx));
        visual.run_until_parked();
        assert!(note.reader.read_with(visual, |r, _| r
            .editing
            .as_ref()
            .unwrap()
            .live_preview
            .enabled));
        set(&note, visual, "- [x] a|");
        keys(visual, "enter");
        assert_eq!(shown(&note, visual), "- [x] a\n- [ ] |");
        keys(visual, "enter");
        assert_eq!(shown(&note, visual), "- [x] a\n|", "out of the list");
        undo(visual);
        assert_eq!(shown(&note, visual), "- [x] a\n- [ ] |");
        undo(visual);
        assert_eq!(shown(&note, visual), "- [x] a|");

        set(&note, visual, "1. a\n2. b|c");
        keys(visual, "tab");
        assert_eq!(shown(&note, visual), "1. a\n   1. b|c");
        undo(visual);
        assert_eq!(shown(&note, visual), "1. a\n2. b|c");
        assert!(note.reader.read_with(visual, |r, _| r
            .editing
            .as_ref()
            .unwrap()
            .live_preview
            .enabled));
    }

    fn compose(note: &Note, visual: &mut VisualTestContext, text: &str) {
        note.input.update_in(visual, |i, window, cx| {
            i.replace_and_mark_text_in_range(None, text, None, window, cx)
        });
        visual.run_until_parked();
    }

    /// While an input method composes nothing runs: no continuation, no list
    /// indent. The Enter that commits the composition continues no list;
    /// after the settle Enter continues again (positive control).
    #[gpui::test]
    fn no_list_command_runs_during_a_composition(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        set(&note, visual, "- a|");
        compose(&note, visual, "か");
        keys(visual, "enter");
        let text = note.input.read_with(visual, |i, _| i.value().to_string());
        assert!(
            !text.contains("\n- "),
            "no continuation on preedit: {text:?}"
        );

        set(&note, visual, "- a\n- b|");
        compose(&note, visual, "か");
        keys(visual, "tab");
        let text = note.input.read_with(visual, |i, _| i.value().to_string());
        assert!(
            text.starts_with("- a\n- b"),
            "no list indent on preedit: {text:?}"
        );

        set(&note, visual, "- a|");
        compose(&note, visual, "か");
        note.input.update_in(visual, |i, window, cx| {
            let marked = i.marked_text_range(window, cx);
            i.replace_text_in_range(marked, "漢", window, cx)
        });
        visual.run_until_parked();
        keys(visual, "enter");
        assert_eq!(shown(&note, visual), "- a漢\n|", "the committing Enter");
        visual
            .executor()
            .advance_clock(super::super::ime_guard::IME_SETTLE + Duration::from_millis(100));
        visual.run_until_parked();
        note.input
            .update(visual, |i, cx| i.set_selected_range(6..6, cx));
        keys(visual, "enter");
        assert_eq!(shown(&note, visual), "- a漢\n- |\n", "after the settle");
    }
}
