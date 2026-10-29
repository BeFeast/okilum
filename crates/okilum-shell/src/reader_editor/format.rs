//! Formatting keys and typed autoformat (#1636). In a Markdown note
//! Cmd/Ctrl+B, Cmd/Ctrl+I, Cmd/Ctrl+Shift+X and Cmd+` (Ctrl+` elsewhere)
//! toggle bold, italic, strikethrough and inline code; Cmd/Ctrl+1…6 set the
//! heading level and Cmd/Ctrl+Alt+0 makes a paragraph (Cmd/Ctrl+0 stays the
//! text zoom reset). Typing `*` `_` `~` `=` or `` ` `` with a selection wraps
//! it; brackets and quotes close themselves (Settings → Editor), `*` `_` and
//! `` ` `` only when that is switched on.
//!
//! A toggle finds the wrapper around the selection or the caret through the
//! same classifier that conceals it in Live Preview, so a hidden `**` is
//! removed like a visible one. Every command and every typed autoformat is
//! one Undo step; Backspace right after a typed autoformat takes back only
//! the autoformat (patch 0088). Keys are matched by key position, so Russian
//! and Hebrew layouts reach them too.
pub(super) mod fences;

use super::ime_guard::Trigger;
use super::lists::{self, Edit, ListEdit};
use super::*;
use gpui_base::input::{TypedEdit, TypedText, TypedTextHook};
use okilum_core::source_classifier::{self, Style};
use okilum_core::source_projection::Snapshot;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

gpui::actions!(
    okilum,
    [
        ToggleBold,
        ToggleItalic,
        ToggleStrikethrough,
        ToggleInlineCode,
        Heading1,
        Heading2,
        Heading3,
        Heading4,
        Heading5,
        Heading6,
        Paragraph,
    ]
);

/// The note editor, in source and in Live Preview.
const KEYS: &str = "ReaderSource > Input";

pub(crate) fn bind_keys(cx: &mut App) {
    let keys = Some(KEYS);
    cx.bind_keys([
        KeyBinding::new("secondary-b", ToggleBold, keys),
        KeyBinding::new("secondary-i", ToggleItalic, keys),
        // Letters keep shift on macOS too, so one string fits.
        KeyBinding::new("secondary-shift-x", ToggleStrikethrough, keys),
        // Cmd+E is Edit / Read (#916): inline code is Cmd+` (Oleg, 24.10).
        // macOS may keep Cmd+` for "Move focus to next window"; Ctrl+`
        // (Typora's key) works there too. The Cmd binding comes later, so
        // it is the one menus and the shortcut sheet show.
        #[cfg(target_os = "macos")]
        KeyBinding::new("ctrl-`", ToggleInlineCode, keys),
        KeyBinding::new("secondary-`", ToggleInlineCode, keys),
        KeyBinding::new("secondary-1", Heading1, keys),
        KeyBinding::new("secondary-2", Heading2, keys),
        KeyBinding::new("secondary-3", Heading3, keys),
        KeyBinding::new("secondary-4", Heading4, keys),
        KeyBinding::new("secondary-5", Heading5, keys),
        KeyBinding::new("secondary-6", Heading6, keys),
        // Cmd/Ctrl+0 resets the text zoom, as in browsers (#1652).
        KeyBinding::new("secondary-alt-0", Paragraph, keys),
    ]);
}

/// The app menu's Format menu; each item is enabled while a note editor
/// has focus.
pub(crate) fn format_menu() -> Menu {
    Menu {
        name: "Format".into(),
        disabled: false,
        items: vec![
            MenuItem::action("Bold", ToggleBold),
            MenuItem::action("Italic", ToggleItalic),
            MenuItem::action("Strikethrough", ToggleStrikethrough),
            MenuItem::action("Inline Code", ToggleInlineCode),
            MenuItem::separator(),
            MenuItem::action("Heading 1", Heading1),
            MenuItem::action("Heading 2", Heading2),
            MenuItem::action("Heading 3", Heading3),
            MenuItem::action("Heading 4", Heading4),
            MenuItem::action("Heading 5", Heading5),
            MenuItem::action("Heading 6", Heading6),
            MenuItem::action("Paragraph", Paragraph),
        ],
    }
}

/// An inline style a key toggles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Inline {
    Bold,
    Italic,
    Strike,
    Code,
}

impl Inline {
    /// What a new wrapper is written with.
    fn wrapper(self) -> &'static str {
        match self {
            Inline::Bold => "**",
            Inline::Italic => "*",
            Inline::Strike => "~~",
            Inline::Code => "`",
        }
    }

    fn style(self) -> Style {
        match self {
            Inline::Bold => Style::Strong,
            Inline::Italic => Style::Emphasis,
            Inline::Strike => Style::Strike,
            Inline::Code => Style::Code,
        }
    }

    /// The characters an existing wrapper may be written with.
    fn marks(self) -> &'static [u8] {
        match self {
            Inline::Bold | Inline::Italic => b"*_",
            Inline::Strike => b"~",
            Inline::Code => b"`",
        }
    }

    /// Whether runs of `left` and `right` marks around a text make this
    /// style: `*` italic is an odd run, `**` bold at least two.
    fn fits(self, left: usize, right: usize) -> bool {
        match self {
            Inline::Italic => left % 2 == 1 && right % 2 == 1,
            Inline::Bold | Inline::Strike => left >= 2 && right >= 2,
            Inline::Code => left == right && left >= 1,
        }
    }

    /// Marks one side of the wrapper removes from runs of `left` and `right`.
    fn width(self, left: usize) -> usize {
        match self {
            Inline::Italic => 1,
            Inline::Bold | Inline::Strike => 2,
            Inline::Code => left,
        }
    }
}

/// Lines and bytes a paragraph is looked at around the selection.
const PARAGRAPH_LINES: usize = 200;
const PARAGRAPH_BYTES: usize = 32 * 1024;

/// The paragraph around `selection`: whole lines, out to the blank lines
/// around it (bounded).
fn paragraph(text: &str, selection: &Range<usize>) -> Range<usize> {
    let first = lists::line_start(text, selection.start);
    let mut start = first;
    for _ in 0..PARAGRAPH_LINES {
        if start <= lists::bom(text) {
            break;
        }
        let above = lists::line_start(text, start - 1);
        if text[above..start].trim().is_empty() || first - above > PARAGRAPH_BYTES {
            break;
        }
        start = above;
    }
    let last = lists::line_end(text, lists::line_start(text, selection.end));
    let mut end = last;
    for _ in 0..PARAGRAPH_LINES {
        let Some(newline) = text[end..].find('\n') else {
            break;
        };
        let next = end + newline + 1;
        let next_end = lists::line_end(text, next);
        if text[next..next_end].trim().is_empty() || next_end - last > PARAGRAPH_BYTES {
            break;
        }
        end = next_end;
    }
    start..end
}

/// The styled ranges (their content, without the marks) the Live Preview
/// classifier finds in `block`: what it conceals is what a toggle sees.
fn styles(text: &str, block: Range<usize>) -> Vec<(Style, Range<usize>)> {
    let snapshot = Snapshot::new("format", 0, &text[block.clone()]);
    let classification = source_classifier::classify(&snapshot);
    classification
        .styles_for(&snapshot)
        .map(|styles| {
            styles
                .iter()
                .map(|span| {
                    (
                        span.style,
                        span.range.start + block.start..span.range.end + block.start,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn run_before(text: &str, at: usize, mark: u8) -> usize {
    text.as_bytes()[..at]
        .iter()
        .rev()
        .take_while(|&&b| b == mark)
        .count()
}

fn run_after(text: &str, at: usize, mark: u8) -> usize {
    text.as_bytes()[at..]
        .iter()
        .take_while(|&&b| b == mark)
        .count()
}

/// The wrapper of `kind` around the selection or the caret: its whole range
/// and its content. The innermost classified span wins; an empty wrapper
/// around the caret, or in a paragraph the classifier keeps raw the marks
/// right at the selection, count too.
fn existing(
    text: &str,
    selection: &Range<usize>,
    kind: Inline,
) -> Option<(Range<usize>, Range<usize>)> {
    let styles = styles(text, paragraph(text, selection));
    let raw = styles.is_empty();
    let mut best: Option<(Range<usize>, Range<usize>)> = None;
    for (style, content) in styles {
        if style != kind.style() {
            continue;
        }
        let width = match kind {
            Inline::Code => run_before(text, content.start, b'`'),
            _ => kind.width(0),
        };
        if width == 0 || content.start < width || content.end + width > text.len() {
            continue;
        }
        let outer = content.start - width..content.end + width;
        let inside = if selection.is_empty() {
            outer.start < selection.start && selection.start < outer.end
        } else {
            outer.start <= selection.start && selection.end <= outer.end
        };
        if inside && best.as_ref().is_none_or(|(o, _)| outer.len() < o.len()) {
            best = Some((outer, content));
        }
    }
    best.or_else(|| {
        (raw || selection.is_empty())
            .then(|| adjacent(text, selection, kind))
            .flatten()
    })
}

/// Marks right at the selection: outside it (`**|text|**`), inside it
/// (`|**text**|`), or an empty wrapper around the caret (`**|**`).
fn adjacent(
    text: &str,
    selection: &Range<usize>,
    kind: Inline,
) -> Option<(Range<usize>, Range<usize>)> {
    let (start, end) = (selection.start, selection.end);
    for &mark in kind.marks() {
        let (left, right) = (run_before(text, start, mark), run_after(text, end, mark));
        let empty = start == end;
        let exact = match kind {
            Inline::Italic => left == 1 && right == 1,
            Inline::Bold | Inline::Strike => left == 2 && right == 2,
            Inline::Code => left == right && left >= 1,
        };
        if (!empty && kind.fits(left, right)) || (empty && exact) {
            let width = kind.width(left);
            return Some((start - width..end + width, start..end));
        }
        if empty {
            continue;
        }
        let inner = &text[start..end];
        let left = run_after(inner, 0, mark);
        let right = run_before(inner, inner.len(), mark);
        if left + right < inner.len() && kind.fits(left, right) {
            let width = kind.width(left);
            return Some((start..end, start + width..end - width));
        }
    }
    None
}

/// The word the caret is strictly inside, on its line.
fn word_at(text: &str, caret: usize) -> Option<Range<usize>> {
    let start = lists::line_start(text, caret);
    let end = lists::line_end(text, start);
    text[start..end]
        .split_word_bound_indices()
        .map(|(at, word)| (start + at..start + at + word.len(), word))
        .find(|(range, word)| {
            range.start < caret && caret < range.end && word.chars().any(char::is_alphanumeric)
        })
        .map(|(range, _)| range)
}

/// The backticks around inline code `content`: one more than its longest
/// run, with a space inside when it starts or ends with one.
fn code_marks(content: &str) -> (String, String) {
    let longest = content.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let ticks = "`".repeat(longest + 1);
    if content.starts_with('`') || content.ends_with('`') {
        (format!("{ticks} "), format!(" {ticks}"))
    } else {
        (ticks.clone(), ticks)
    }
}

/// Cmd/Ctrl+B, I, Shift+X, `: remove the wrapper around the selection or
/// the caret, or wrap the selection (its spaces stay outside), the word the
/// caret is in, or an empty pair at the caret. The selection stays on the
/// same text.
pub(crate) fn toggle(text: &str, selection: Range<usize>, kind: Inline) -> Option<ListEdit> {
    let selection = past_bom(text, selection);
    if lists::in_code(text, lists::line_start(text, selection.start)) {
        return None;
    }
    if let Some((outer, content)) = existing(text, &selection, kind) {
        let edits: Vec<Edit> = vec![
            (outer.start..content.start, String::new()),
            (content.end..outer.end, String::new()),
        ];
        let start = lists::map_offset(&edits, selection.start, false);
        let end = lists::map_offset(&edits, selection.end, false);
        return Some(ListEdit {
            edits,
            selection: start..end,
            revert: None,
        });
    }
    let target = if !selection.is_empty() {
        let raw = &text[selection.clone()];
        let lead = raw.len() - raw.trim_start().len();
        let trail = raw.len() - raw.trim_end().len();
        if lead == raw.len() {
            selection.clone()
        } else {
            selection.start + lead..selection.end - trail
        }
    } else {
        word_at(text, selection.start).unwrap_or_else(|| selection.clone())
    };
    let (open, close) = match kind {
        Inline::Code => code_marks(&text[target.clone()]),
        _ => (kind.wrapper().to_string(), kind.wrapper().to_string()),
    };
    let shift = open.len();
    let selection = if !selection.is_empty() {
        target.start + shift..target.end + shift
    } else if target.is_empty() {
        target.start + shift..target.start + shift
    } else {
        selection.start + shift..selection.start + shift
    };
    let edits = if target.is_empty() {
        vec![(target.clone(), format!("{open}{close}"))]
    } else {
        vec![
            (target.start..target.start, open),
            (target.end..target.end, close),
        ]
    };
    Some(ListEdit {
        edits,
        selection,
        revert: None,
    })
}

/// `selection` moved past the note's byte order mark: marks go after it.
fn past_bom(text: &str, selection: Range<usize>) -> Range<usize> {
    let bom = lists::bom(text);
    selection.start.max(bom)..selection.end.max(bom)
}

/// Cmd/Ctrl+1…6 (`level`) and Cmd/Ctrl+Alt+0 (0): every line of the
/// selection becomes a heading of that level, or a paragraph. A list item
/// keeps its marker, a quote its `>`.
pub(crate) fn heading(text: &str, selection: Range<usize>, level: usize) -> Option<ListEdit> {
    let first = lists::line_start(text, selection.start);
    if lists::in_code(text, first) {
        return None;
    }
    // A selection that ends at a line start leaves that line alone.
    let mut last = selection.end;
    if !selection.is_empty() && last > first && text[..last].ends_with('\n') {
        last -= 1;
    }
    let mut edits: Vec<Edit> = Vec::new();
    let mut start = first;
    let single = lists::line_start(text, last) == first;
    for _ in 0..lists::SCAN_LINES {
        let line = lists::line_at(text, start);
        if single || !line.blank(text) {
            edits.extend(heading_edit(text, &line, level));
        }
        let Some(newline) = text[line.end..].find('\n') else {
            break;
        };
        start = line.end + newline + 1;
        if start > last {
            break;
        }
    }
    if edits.is_empty() {
        return None;
    }
    let selection = if selection.is_empty() {
        let at = lists::map_offset(&edits, selection.start, true);
        at..at
    } else {
        lists::map_offset(&edits, selection.start, false)
            ..lists::map_offset(&edits, selection.end, true)
    };
    Some(ListEdit {
        edits,
        selection,
        revert: None,
    })
}

fn heading_edit(text: &str, line: &lists::Line, level: usize) -> Option<Edit> {
    // Four columns of indentation outside a list is a code block.
    if !line.item() && line.width(text) >= 4 {
        return None;
    }
    let base = if line.item() {
        line.content
    } else {
        line.indent_end
    };
    let rest = &text[base..line.end];
    let hashes = rest.bytes().take_while(|&b| b == b'#').count();
    let marker = if (1..=6).contains(&hashes)
        && rest
            .as_bytes()
            .get(hashes)
            .is_none_or(|b| matches!(b, b' ' | b'\t'))
    {
        hashes
            + rest[hashes..]
                .bytes()
                .take_while(|b| matches!(b, b' ' | b'\t'))
                .count()
    } else {
        0
    };
    let new = if level == 0 {
        String::new()
    } else {
        format!("{} ", "#".repeat(level))
    };
    let old = &text[base..base + marker];
    if old == new {
        return None;
    }
    // Only the bytes that differ: `## ` → `### ` inserts one `#`.
    let common = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    Some((base + common..base + marker, new[common..].to_string()))
}

/// Typed autoformat switches (Settings → Editor).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pairing {
    /// Code fences close independently of inline Markdown pairing (#1893).
    pub fences: bool,
    /// `(` `[` `{` `"` `'` `«` close themselves.
    pub brackets: bool,
    /// `*` `_` `` ` `` close themselves (off by default, as in Typora).
    pub markdown: bool,
}

/// Typing one of these with a selection wraps it (always on).
const WRAP: [char; 5] = ['*', '_', '~', '=', '`'];
const MARKDOWN_PAIRS: [char; 3] = ['*', '_', '`'];

/// The closing character of an auto-paired opening one.
fn closer(open: char) -> Option<char> {
    Some(match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '«' => '»',
        '"' | '\'' | '*' | '_' | '`' => open,
        _ => return None,
    })
}

fn opener(close: char) -> Option<char> {
    Some(match close {
        ')' => '(',
        ']' => '[',
        '}' => '{',
        '»' => '«',
        _ => return None,
    })
}

/// Whether a typed character can start a typed autoformat at all: the
/// hook's cheap first test, before it reads the note.
fn may_format(ch: char, selected: bool, pairing: Pairing) -> bool {
    if !selected && pairing.fences && matches!(ch, '`' | '~') {
        return true;
    }
    if selected {
        return WRAP.contains(&ch) || (pairing.brackets && closer(ch).is_some());
    }
    if MARKDOWN_PAIRS.contains(&ch) {
        return pairing.markdown;
    }
    pairing.brackets && (closer(ch).is_some() || opener(ch).is_some())
}

/// What typing `ch` at `selection` does instead of plain typing, if
/// anything: wrap the selection, type over the closer a pair put there, or
/// close a pair.
pub(crate) fn typed(
    text: &str,
    selection: Range<usize>,
    ch: char,
    pairing: Pairing,
) -> Option<TypedEdit> {
    let selection = past_bom(text, selection);
    if pairing.fences {
        if let Some(edit) = fences::typed(text, selection.clone(), ch) {
            return Some(edit);
        }
    }
    if !may_format(ch, !selection.is_empty(), pairing)
        || lists::in_code(text, lists::line_start(text, selection.start))
    {
        return None;
    }
    let width = ch.len_utf8();
    if !selection.is_empty() {
        let close = if WRAP.contains(&ch) { ch } else { closer(ch)? };
        return Some(TypedEdit {
            insert_typed: false,
            edits: vec![
                (selection.start..selection.start, ch.to_string()),
                (selection.end..selection.end, close.to_string()),
            ],
            selection: selection.start + width..selection.end + width,
        });
    }
    let at = selection.start;
    let before = &text[lists::line_start(text, at)..at];
    let prev = before.chars().next_back();
    let next = text[at..].chars().next();
    let over = TypedEdit {
        insert_typed: false,
        edits: Vec::new(),
        selection: at + width..at + width,
    };
    let pair = |close: char| TypedEdit {
        insert_typed: true,
        edits: vec![(at + width..at + width, close.to_string())],
        selection: at + width..at + width,
    };
    // A pair closes only before a space, the line end or closing punctuation.
    let free = next.is_none_or(|c| c.is_whitespace() || ")]}».,;:!?".contains(c));
    if let Some(open) = opener(ch) {
        // A closing bracket types over the one its pair put there.
        let unclosed = before.matches(open).count() > before.matches(ch).count();
        return (next == Some(ch) && unclosed).then_some(over);
    }
    if MARKDOWN_PAIRS.contains(&ch) {
        let mark = ch as u8;
        let (left, right) = (run_before(text, at, mark), run_after(text, at, mark));
        if right > 0 {
            // `*|*` + `*` → `**|**`; `*a|*` + `*` → `*a*|`.
            let outside = text[..at - left]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric());
            if left == right && outside && ch != '`' {
                return Some(pair(ch));
            }
            return (left <= right).then_some(over);
        }
        // Not after a letter (snake_case, 2*3), not a bullet or a fence.
        let start = before.trim().is_empty();
        if prev.is_some_and(|c| c.is_alphanumeric() || c == ch) || (start && ch != '`') || !free {
            return None;
        }
        return Some(pair(ch));
    }
    if ch == '"' || ch == '\'' {
        // Inside a quote, the closing one types over.
        if next == Some(ch) && quotes(before, next, ch) % 2 == 1 {
            return Some(over);
        }
        // An apostrophe in a word is not a quote.
        if prev.is_some_and(|c| c.is_alphanumeric() || c == ch) || !free || next == Some(ch) {
            return None;
        }
        return Some(pair(ch));
    }
    let close = closer(ch)?;
    free.then(|| pair(close))
}

/// The quotes `ch` in `before` (the line up to the caret, `after` the
/// character at it) that open or close a quotation. One inside a word is an
/// apostrophe, a geresh or gershayim (it's, ג'ירפה, צה"ל): it pairs nothing.
fn quotes(before: &str, after: Option<char>, ch: char) -> usize {
    let chars: Vec<char> = before.chars().collect();
    (0..chars.len())
        .filter(|&ix| {
            let in_word = ix > 0
                && chars[ix - 1].is_alphanumeric()
                && chars
                    .get(ix + 1)
                    .copied()
                    .or(after)
                    .is_some_and(char::is_alphanumeric);
            chars[ix] == ch && !in_word
        })
        .count()
}

/// The typed-text hook of a Markdown note's editor.
pub(super) fn typed_hook() -> TypedTextHook {
    Rc::new(|input: &TypedText<'_>, cx: &App| {
        let mut chars = input.typed.chars();
        let ch = chars.next()?;
        if chars.next().is_some() {
            return None;
        }
        let (brackets, markdown) = reader_ui_state::auto_pair(cx);
        let pairing = Pairing {
            brackets,
            markdown,
            fences: reader_ui_state::close_code_fences(cx),
        };
        if !may_format(ch, !input.selection.is_empty(), pairing) {
            return None;
        }
        typed(
            &input.text.to_string(),
            input.selection.clone(),
            ch,
            pairing,
        )
    })
}

impl Reader {
    /// Run a formatting command on the note editor's text and selection.
    fn format_command(
        &mut self,
        command: impl FnOnce(&str, Range<usize>) -> Option<ListEdit>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Nothing changes the text under a composition.
        if !self.ime_allows(Trigger::Text, cx) {
            return;
        }
        let Some(input) = self.list_input(window, cx) else {
            return;
        };
        let (text, selection) = {
            let state = input.read(cx);
            (state.value(), state.selected_range())
        };
        if let Some(edit) = command(&text, selection) {
            self.apply_list_edit(&input, edit, window, cx);
        }
    }

    fn format_inline(&mut self, kind: Inline, window: &mut Window, cx: &mut Context<Self>) {
        self.format_command(|text, selection| toggle(text, selection, kind), window, cx);
    }

    fn format_heading(&mut self, level: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.format_command(
            |text, selection| heading(text, selection, level),
            window,
            cx,
        );
    }

    /// The formatting keys and menu items on the note editor's column.
    pub(super) fn format_actions(&self, column: Div, cx: &mut Context<Self>) -> Div {
        column
            .on_action(cx.listener(|this, _: &ToggleBold, window, cx| {
                this.format_inline(Inline::Bold, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleItalic, window, cx| {
                this.format_inline(Inline::Italic, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleStrikethrough, window, cx| {
                this.format_inline(Inline::Strike, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleInlineCode, window, cx| {
                this.format_inline(Inline::Code, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &Heading1, window, cx| this.format_heading(1, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &Heading2, window, cx| this.format_heading(2, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &Heading3, window, cx| this.format_heading(3, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &Heading4, window, cx| this.format_heading(4, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &Heading5, window, cx| this.format_heading(5, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &Heading6, window, cx| this.format_heading(6, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &Paragraph, window, cx| this.format_heading(0, window, cx)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    /// `|` is the caret; `[` and `]` a selection.
    fn marked(text: &str) -> (String, Range<usize>) {
        if let Some(at) = text.find('|') {
            return (text.replacen('|', "", 1), at..at);
        }
        let start = text.find('[').unwrap();
        let end = text.find(']').unwrap() - 1;
        (text.replacen('[', "", 1).replacen(']', "", 1), start..end)
    }

    fn show(mut text: String, selection: Range<usize>) -> String {
        if selection.is_empty() {
            text.insert(selection.start, '|');
        } else {
            text.insert(selection.end, ']');
            text.insert(selection.start, '[');
        }
        text
    }

    fn run(
        text: &str,
        command: impl FnOnce(&str, Range<usize>) -> Option<ListEdit>,
    ) -> Option<String> {
        let (text, selection) = marked(text);
        let edit = command(&text, selection)?;
        Some(show(lists::apply(&text, &edit.edits), edit.selection))
    }

    fn inline(text: &str, kind: Inline) -> Option<String> {
        run(text, |t, s| toggle(t, s, kind))
    }

    fn level(text: &str, level: usize) -> Option<String> {
        run(text, |t, s| heading(t, s, level))
    }

    const ON: Pairing = Pairing {
        fences: true,
        brackets: true,
        markdown: true,
    };
    const DEFAULT: Pairing = Pairing {
        fences: true,
        brackets: true,
        markdown: false,
    };

    /// Type `ch` at the marked selection, the way the hook applies it.
    fn type_char(text: &str, ch: char, pairing: Pairing) -> Option<String> {
        let (text, selection) = marked(text);
        let edit = typed(&text, selection.clone(), ch, pairing)?;
        let mut text = text;
        if edit.insert_typed {
            text.replace_range(selection, &ch.to_string());
        }
        Some(show(lists::apply(&text, &edit.edits), edit.selection))
    }

    /// A note that starts with a byte order mark (common from Windows): the
    /// first line is formatted after it, and nothing panics there.
    #[test]
    fn the_first_line_of_a_note_with_a_bom() {
        let bom = '\u{feff}';
        assert_eq!(
            inline(&format!("{bom}a [word] b"), Inline::Bold),
            Some(format!("{bom}a **[word]** b"))
        );
        assert_eq!(
            inline(&format!("{bom}a wo|rd"), Inline::Italic),
            Some(format!("{bom}a *wo|rd*"))
        );
        assert_eq!(
            level(&format!("{bom}Title|\n\nbody"), 2),
            Some(format!("{bom}## Title|\n\nbody"))
        );
        assert_eq!(
            level(&format!("{bom}# Title|"), 0),
            Some(format!("{bom}Title|"))
        );
        assert_eq!(
            type_char(&format!("{bom}|"), '(', DEFAULT),
            Some(format!("{bom}(|)"))
        );
        assert_eq!(
            type_char(&format!("{bom}[x]"), '*', DEFAULT),
            Some(format!("{bom}*[x]*"))
        );
        // Select All starts before the mark; the wrapper goes after it.
        assert_eq!(
            inline(&format!("[{bom}word]"), Inline::Bold),
            Some(format!("{bom}**[word]**"))
        );
        assert_eq!(
            inline(&format!("{bom}```\n[x]\n```"), Inline::Bold),
            None,
            "code"
        );
    }

    #[test]
    fn bold_wraps_the_selection_the_word_or_an_empty_pair() {
        use Inline::Bold;
        for (before, after) in [
            ("a [word] b", "a **[word]** b"),
            ("a[ word ]b", "a **[word]** b"),
            ("a wo|rd b", "a **wo|rd** b"),
            ("a | b", "a **|** b"),
            ("word|", "word**|**"),
            ("[слово]", "**[слово]**"),
            ("при|вет мир", "**при|вет** мир"),
            ("[שלום]", "**[שלום]**"),
        ] {
            assert_eq!(inline(before, Bold).as_deref(), Some(after), "{before:?}");
        }
    }

    #[test]
    fn bold_finds_its_wrapper_around_the_selection_or_the_caret() {
        use Inline::Bold;
        for (before, after) in [
            ("a **[word]** b", "a [word] b"),
            ("a [**word**] b", "a [word] b"),
            ("a **bo|ld** b", "a bo|ld b"),
            ("a **b[ol]d** b", "a b[ol]d b"),
            ("a **two [words]** b", "a two [words] b"),
            ("__a|b__", "a|b"),
            ("**|**", "|"),
            ("- **[item]**", "- [item]"),
            ("> ## **[quote]**", "> ## [quote]"),
            ("**[слово]**", "[слово]"),
        ] {
            assert_eq!(inline(before, Bold).as_deref(), Some(after), "{before:?}");
        }
    }

    #[test]
    fn italic_and_bold_tell_their_marks_apart() {
        assert_eq!(
            inline("**[x]**", Inline::Italic).as_deref(),
            Some("***[x]***"),
            "italic inside bold wraps"
        );
        assert_eq!(inline("***[x]***", Inline::Bold).as_deref(), Some("*[x]*"));
        assert_eq!(
            inline("***[x]***", Inline::Italic).as_deref(),
            Some("**[x]**")
        );
        assert_eq!(inline("*[x]*", Inline::Italic).as_deref(), Some("[x]"));
        assert_eq!(inline("_a|b_ c", Inline::Italic).as_deref(), Some("a|b c"));
        assert_eq!(
            inline("*[x]*", Inline::Bold).as_deref(),
            Some("***[x]***"),
            "bold around italic wraps"
        );
    }

    #[test]
    fn strikethrough_and_inline_code_toggle() {
        assert_eq!(
            inline("a [x] b", Inline::Strike).as_deref(),
            Some("a ~~[x]~~ b")
        );
        assert_eq!(
            inline("a ~~[x]~~ b", Inline::Strike).as_deref(),
            Some("a [x] b")
        );
        assert_eq!(inline("[code]", Inline::Code).as_deref(), Some("`[code]`"));
        assert_eq!(
            inline("`co|de` x", Inline::Code).as_deref(),
            Some("co|de x")
        );
        assert_eq!(
            inline("[a`b]", Inline::Code).as_deref(),
            Some("``[a`b]``"),
            "a backtick inside takes a longer fence"
        );
        assert_eq!(inline("[`a]", Inline::Code).as_deref(), Some("`` [`a] ``"));
    }

    #[test]
    fn a_paragraph_the_classifier_keeps_raw_still_toggles_marks_at_the_selection() {
        // Inline HTML keeps its paragraph out of Live Preview.
        assert_eq!(
            inline("<kbd>k</kbd> **[b]**", Inline::Bold).as_deref(),
            Some("<kbd>k</kbd> [b]")
        );
    }

    #[test]
    fn nothing_toggles_in_code_or_front_matter() {
        assert_eq!(inline("```\n[x]\n```", Inline::Bold), None);
        assert_eq!(inline("---\ntitle: [x]\n---\n", Inline::Bold), None);
        assert_eq!(
            inline("```\nc\n```\n[x]", Inline::Bold).as_deref(),
            Some("```\nc\n```\n**[x]**"),
            "after the fence"
        );
    }

    #[test]
    fn heading_keys_set_the_level_or_a_paragraph() {
        for (before, n, after) in [
            ("Title|", 2, Some("## Title|")),
            ("## Title|", 3, Some("### Title|")),
            ("### Ti|tle", 1, Some("# Ti|tle")),
            ("### T|", 0, Some("T|")),
            ("|", 1, Some("# |")),
            ("#tag|", 1, Some("# #tag|")),
            ("- item|", 2, Some("- ## item|")),
            ("> quote|", 2, Some("> ## quote|")),
            ("[a\n\nb]", 1, Some("[# a\n\n# b]")),
            ("[a\n]b", 1, Some("[# a\n]b")),
            ("## T|", 2, None),
            ("x|", 0, None),
            ("    code|", 1, None),
            ("```\nx|", 1, None),
            ("Заголовок|", 1, Some("# Заголовок|")),
        ] {
            assert_eq!(level(before, n).as_deref(), after, "{before:?} → {n}");
        }
    }

    #[test]
    fn typing_a_mark_with_a_selection_wraps_it() {
        for ch in ['*', '_', '~', '=', '`'] {
            assert_eq!(
                type_char("a [word] b", ch, DEFAULT),
                Some(format!("a {ch}[word]{ch} b")),
                "{ch}"
            );
        }
        assert_eq!(type_char("[x]", '(', DEFAULT).as_deref(), Some("([x])"));
        assert_eq!(type_char("[x]", '«', DEFAULT).as_deref(), Some("«[x]»"));
        let off = Pairing {
            fences: true,
            brackets: false,
            markdown: false,
        };
        assert_eq!(type_char("[x]", '(', off), None, "brackets off");
        assert_eq!(
            type_char("[x]", '*', off).as_deref(),
            Some("*[x]*"),
            "wrapping stays on"
        );
        assert_eq!(type_char("[x]", 'a', ON), None);
        assert_eq!(type_char("```\n[x]", '*', ON), None, "code");
    }

    #[test]
    fn brackets_and_quotes_close_themselves() {
        for (before, ch, after) in [
            ("|", '(', Some("(|)")),
            ("f|", '(', Some("f(|)")),
            ("| x", '[', Some("[|] x")),
            ("|x", '(', None),
            ("(|)", ')', Some("()|")),
            ("(a|)", ')', Some("(a)|")),
            ("|)", ')', None),
            ("|", '"', Some("\"|\"")),
            ("\"a|\"", '"', Some("\"a\"|")),
            ("don|", '\'', None),
            ("it's 'a|'", '\'', Some("it's 'a'|")),
            ("צה\"ל \"א|\"", '"', Some("צה\"ל \"א\"|")),
            ("|", '«', Some("«|»")),
            ("«a|»", '»', Some("«a»|")),
        ] {
            assert_eq!(
                type_char(before, ch, DEFAULT).as_deref(),
                after,
                "{before:?} + {ch}"
            );
        }
        let off = Pairing {
            fences: true,
            brackets: false,
            markdown: false,
        };
        assert_eq!(type_char("|", '(', off), None);
        assert_eq!(type_char("(|)", ')', off), None);
    }

    #[test]
    fn markdown_marks_close_themselves_only_when_switched_on() {
        assert_eq!(type_char("a |", '*', DEFAULT), None, "off by default");
        assert_eq!(type_char("a |", '_', DEFAULT), None);
        for (before, ch, after) in [
            ("a |", '*', Some("a *|*")),
            ("a *|*", '*', Some("a **|**")),
            ("a **b|**", '*', Some("a **b*|*")),
            ("a **b*|*", '*', Some("a **b**|")),
            ("|", '*', None),
            ("snake|", '_', None),
            ("2|", '*', None),
            ("a |", '`', Some("a `|`")),
            ("a `|`", '`', Some("a ``|")),
            ("a ``|", '`', None),
        ] {
            assert_eq!(
                type_char(before, ch, ON).as_deref(),
                after,
                "{before:?} + {ch}"
            );
        }
    }

    /// Plain typing of a whole construct ends with the same text: every
    /// closer a pair added is typed over.
    #[test]
    fn typing_through_pairs_gives_the_typed_text() {
        for typed_text in [
            "[link](url) and (a [b] {c}) \"q\" «r»",
            "f(x) - [x] done",
            // An apostrophe in a word before a quote: no stray closer.
            "it's 'fine' and \"don't\" too",
            // Hebrew geresh and gershayim inside words.
            "צה\"ל אמר \"שלום\" ו-ג'ירפה 'כן'",
        ] {
            let mut text = String::new();
            let mut at = 0;
            for ch in typed_text.chars() {
                match typed(&text, at..at, ch, DEFAULT) {
                    Some(edit) => {
                        if edit.insert_typed {
                            text.insert(at, ch);
                        }
                        text = lists::apply(&text, &edit.edits);
                        at = edit.selection.start;
                    }
                    None => {
                        text.insert(at, ch);
                        at += ch.len_utf8();
                    }
                }
            }
            assert_eq!(text, typed_text);
        }
    }
}

/// The keys in a real note editor, in source and in Live Preview: one Undo
/// step each, Russian and Hebrew layouts, the IME guard.
#[cfg(test)]
mod key_tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui::{Keystroke, Modifiers};
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
            crate::bind_keys(cx);
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

    fn set(note: &Note, visual: &mut VisualTestContext, text: &str) {
        let (text, selection) = if text.contains(['|', '[']) {
            tests_marked(text)
        } else {
            (text.to_string(), text.len()..text.len())
        };
        note.input
            .update_in(visual, |i, window, cx| i.set_value(text, window, cx));
        select(note, visual, selection);
    }

    fn select(note: &Note, visual: &mut VisualTestContext, selection: Range<usize>) {
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

    /// A key as the platform reports it on another layout: the key by its
    /// position (Latin), the character the layout would type.
    fn layout_key(visual: &mut VisualTestContext, modifiers: Modifiers, key: &str, typed: &str) {
        let keystroke = Keystroke {
            modifiers,
            key: key.into(),
            key_char: Some(typed.into()),
        };
        visual.update(|window, cx| {
            window.dispatch_keystroke(keystroke, cx);
        });
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

    #[gpui::test]
    fn each_toggle_key_wraps_and_unwraps_in_one_undo_step(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        for (key, mark) in [
            ("secondary-b", "**"),
            ("secondary-i", "*"),
            ("secondary-shift-x", "~~"),
            ("secondary-`", "`"),
        ] {
            set(&note, visual, "a [word] b");
            keys(visual, key);
            let wrapped = format!("a {mark}[word]{mark} b");
            assert_eq!(shown(&note, visual), wrapped, "{key} wraps");
            keys(visual, key);
            assert_eq!(shown(&note, visual), "a [word] b", "{key} unwraps");
            undo(visual);
            assert_eq!(shown(&note, visual), wrapped, "{key}: one step");
            undo(visual);
            assert_eq!(shown(&note, visual), "a [word] b", "{key}: one step");
            redo(visual);
            assert_eq!(shown(&note, visual), wrapped);
        }
        // The word under the caret, then the caret inside the wrapper.
        set(&note, visual, "a wo|rd b");
        keys(visual, "secondary-b");
        assert_eq!(shown(&note, visual), "a **wo|rd** b");
        keys(visual, "secondary-b");
        assert_eq!(shown(&note, visual), "a wo|rd b");
        // Code blocks keep their text.
        set(&note, visual, "```\n[x]\n```");
        keys(visual, "secondary-b");
        assert_eq!(shown(&note, visual), "```\n[x]\n```");
    }

    #[gpui::test]
    fn heading_keys_set_the_level_and_alt_zero_makes_a_paragraph(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        set(&note, visual, "Title|\n\nbody");
        keys(visual, "secondary-2");
        assert_eq!(shown(&note, visual), "## Title|\n\nbody");
        keys(visual, "secondary-1");
        assert_eq!(shown(&note, visual), "# Title|\n\nbody");
        keys(visual, "secondary-6");
        assert_eq!(shown(&note, visual), "###### Title|\n\nbody");
        keys(visual, "secondary-alt-0");
        assert_eq!(shown(&note, visual), "Title|\n\nbody");
        undo(visual);
        assert_eq!(shown(&note, visual), "###### Title|\n\nbody", "one step");
        // Cmd/Ctrl+0 stays the text zoom reset (#1652): the text stays.
        keys(visual, "secondary-0");
        assert_eq!(shown(&note, visual), "###### Title|\n\nbody");
    }

    /// Russian and Hebrew layouts reach the keys by position: the platform
    /// reports the Latin key and the layout's character beside it.
    #[gpui::test]
    fn russian_and_hebrew_layouts_reach_the_formatting_keys(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        let secondary = Modifiers::secondary_key();
        set(&note, visual, "мир [слово] мир");
        layout_key(visual, secondary, "b", "и");
        assert_eq!(shown(&note, visual), "мир **[слово]** мир", "Russian Б");
        layout_key(visual, secondary, "b", "נ");
        assert_eq!(shown(&note, visual), "мир [слово] мир", "Hebrew נ");
        layout_key(visual, secondary, "i", "ш");
        assert_eq!(shown(&note, visual), "мир *[слово]* мир", "Russian Ш");
        undo(visual);
        let shift_secondary = Modifiers {
            shift: true,
            ..secondary
        };
        layout_key(visual, shift_secondary, "x", "Ч");
        assert_eq!(shown(&note, visual), "мир ~~[слово]~~ мир", "Russian Ч");
        undo(visual);
        layout_key(visual, secondary, "`", "ё");
        assert_eq!(shown(&note, visual), "мир `[слово]` мир", "Russian Ё");
        undo(visual);
        layout_key(visual, secondary, "1", "1");
        assert_eq!(shown(&note, visual), "# мир [слово] мир");
        undo(visual);
        // Shift+8 types `*` on the Russian layout too: it wraps.
        layout_key(visual, Modifiers::shift(), "8", "*");
        assert_eq!(shown(&note, visual), "мир *[слово]* мир");
        // A letter of the layout is plain typing.
        set(&note, visual, "|");
        layout_key(visual, Modifiers::default(), "b", "и");
        assert_eq!(shown(&note, visual), "и|");
    }

    /// In Live Preview the `**` of a bold word are hidden; the toggle finds
    /// them and the selection stays on the word.
    #[gpui::test]
    fn concealed_bold_toggles_off_and_keeps_the_selection(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        note.reader
            .update_in(visual, |r, window, cx| r.set_live_preview(true, window, cx));
        visual.run_until_parked();
        set(&note, visual, "a **bold** b\n\nother|");
        visual.run_until_parked();
        let concealed = note.reader.read_with(visual, |r, _| {
            r.editing.as_ref().unwrap().live_preview.concealed_text()
        });
        let concealed = concealed.expect("Live Preview classified the note");
        assert!(
            concealed.contains("a bold b") && !concealed.contains("**"),
            "positive control: the marks are concealed: {concealed:?}"
        );
        // What a double-click on the shown word selects: the word alone.
        select(&note, visual, 4..8);
        keys(visual, "secondary-b");
        assert_eq!(shown(&note, visual), "a [bold] b\n\nother");
        undo(visual);
        assert_eq!(shown(&note, visual), "a **[bold]** b\n\nother");
        // The caret alone in the hidden-marked word.
        select(&note, visual, 6..6);
        keys(visual, "secondary-b");
        assert_eq!(shown(&note, visual), "a bo|ld b\n\nother");
        assert!(note.reader.read_with(visual, |r, _| r
            .editing
            .as_ref()
            .unwrap()
            .live_preview
            .enabled));
    }

    #[gpui::test]
    fn code_fences_close_by_default_and_undo_keeps_the_typed_opener(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        for live in [false, true] {
            note.reader.update_in(visual, |reader, window, cx| {
                if reader.editing.as_ref().unwrap().live_preview.enabled != live {
                    reader.toggle_live_preview(window, cx);
                }
            });
            for (prefix, continued) in [("", ""), ("- ", "  "), ("> ", "> "), ("- > ", "  > ")] {
                set(&note, visual, &format!("{prefix}|"));
                for _ in 0..3 {
                    visual.simulate_input("`");
                }
                visual.run_until_parked();
                assert_eq!(
                    shown(&note, visual),
                    format!("{prefix}```|\n{continued}```")
                );
                undo(visual);
                assert_eq!(shown(&note, visual), format!("{prefix}```|"));
                visual.dispatch_action(Redo);
                visual.run_until_parked();
                keys(visual, "enter");
                assert_eq!(
                    shown(&note, visual),
                    format!("{prefix}```\n{continued}|\n{continued}```")
                );
            }
        }
    }

    #[gpui::test]
    fn disabling_fence_assistance_leaves_literal_typing(cx: &mut TestAppContext) {
        let state = tempfile::tempdir().unwrap();
        cx.update(|cx| reader_ui_state::install(state.path(), cx));
        let (note, visual) = open(cx);
        visual.update(|_, cx| reader_ui_state::set_close_code_fences(false, cx));
        set(&note, visual, "|");
        for _ in 0..3 {
            visual.simulate_input("`");
        }
        visual.run_until_parked();
        assert_eq!(shown(&note, visual), "```|");
    }

    /// A typed autoformat is its own Undo step, and Backspace right after it
    /// takes back only the autoformat.
    #[gpui::test]
    fn backspace_right_after_a_typed_autoformat_takes_back_only_it(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        set(&note, visual, "a [word] b");
        visual.simulate_input("*");
        assert_eq!(shown(&note, visual), "a *[word]* b", "typing wraps");
        keys(visual, "backspace");
        assert_eq!(shown(&note, visual), "a [word] b", "Backspace unwraps");

        set(&note, visual, "x |");
        visual.simulate_input("(");
        assert_eq!(shown(&note, visual), "x (|)", "the pair closes");
        keys(visual, "backspace");
        assert_eq!(shown(&note, visual), "x (|", "the typed ( stays");
        keys(visual, "backspace");
        assert_eq!(shown(&note, visual), "x |", "then a plain delete");

        set(&note, visual, "x |");
        visual.simulate_input("(");
        undo(visual);
        assert_eq!(shown(&note, visual), "x (|", "Undo takes the closer");
        undo(visual);
        assert_eq!(shown(&note, visual), "x |");

        // Typing through a pair gives the typed text.
        set(&note, visual, "|");
        visual.simulate_input("see [link](url) \"q\"");
        assert_eq!(shown(&note, visual), "see [link](url) \"q\"|");

        // Not right after: the caret moved on, Backspace is plain.
        set(&note, visual, "x |");
        visual.simulate_input("(");
        select(&note, visual, 4..4);
        keys(visual, "backspace");
        assert_eq!(shown(&note, visual), "x (|");
    }

    /// While an input method composes no key formats; for the settle after
    /// a commit typed autoformat waits too.
    #[gpui::test]
    fn no_formatting_during_a_composition_or_its_settle(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        set(&note, visual, "ab|");
        note.input.update_in(visual, |i, window, cx| {
            i.replace_and_mark_text_in_range(None, "か", None, window, cx)
        });
        visual.run_until_parked();
        keys(visual, "secondary-b");
        let text = note.input.read_with(visual, |i, _| i.value().to_string());
        assert!(!text.contains("**"), "no toggle on preedit: {text:?}");
        note.input.update_in(visual, |i, window, cx| {
            let marked = i.marked_text_range(window, cx);
            i.replace_text_in_range(marked, "漢", window, cx)
        });
        visual.run_until_parked();
        visual.simulate_input(" (");
        assert_eq!(shown(&note, visual), "ab漢 (|", "no pair in the settle");
        visual
            .executor()
            .advance_clock(super::super::ime_guard::IME_SETTLE + Duration::from_millis(100));
        visual.run_until_parked();
        visual.simulate_input(" (");
        assert_eq!(shown(&note, visual), "ab漢 ( (|)", "after the settle");
    }
}
