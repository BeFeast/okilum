//! Editing comforts for short snippets in a Markdown note's fenced code
//! blocks (#1992). Inside the body of a fenced block, and only there:
//! brackets and quotes close themselves (Settings → Editor → brackets), wrap
//! a selection and type over their closer; Enter keeps the line's
//! indentation and opens a level after `{`, `(`, `[` (and `:` in Python);
//! Tab and Shift+Tab indent or outdent the selected lines; Cmd/Ctrl+/
//! toggles the line comment of the block's language.
//!
//! The fence lines and everything outside the block keep their Markdown
//! behaviour: a key that does not apply falls through to the list keys, the
//! formatting hook and the input's own handling. A selection that leaves
//! the block changes only the block's lines. A block whose language is
//! unknown or missing stays editable but gets no comment syntax: Okilum does
//! not guess one. Every change is one exact source edit set, one Undo step
//! through the editor's own history (patch 0087); nothing runs while an
//! input method composes or settles, or in a read-only editor.
use super::ime_guard::Trigger;
use super::lists::{self, Edit, ListEdit};
use super::*;
use gpui_base::input::{TypedEdit, TypedText, TypedTextHook};
use gpui_component::input::{Enter, Indent, IndentInline, Outdent, OutdentInline};
use std::ops::Range;

gpui::actions!(okilum, [ToggleCodeComment]);

/// The editor context the comment key is bound in.
const KEYS: &str = "ReaderSource > Input";

/// Marks that the comment key is in the keymap.
struct KeysBound;
impl Global for KeysBound {}

/// Bind Cmd/Ctrl+/ to the comment toggle in the note editor, once. Outside a
/// code block the action propagates, and the same key reaches the next
/// binding (the shortcut sheet) as before.
pub(super) fn ensure_keys(cx: &mut App) {
    if cx.has_global::<KeysBound>() {
        return;
    }
    cx.set_global(KeysBound);
    cx.bind_keys([KeyBinding::new(
        "secondary-/",
        ToggleCodeComment,
        Some(KEYS),
    )]);
}

/// The body of a fenced code block: whole lines between the fences.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Block {
    /// From the first body line's start to the last body line's end (before
    /// its newline). An unclosed fence runs to the end of the note, as in
    /// CommonMark.
    pub body: Range<usize>,
    /// The canonical language the info string names, if any.
    pub language: Option<String>,
}

impl Block {
    fn contains(&self, offset: usize) -> bool {
        self.body.start <= offset && offset <= self.body.end
    }
}

/// A fence line: its character, length and info string. The same rule as
/// [`lists::in_code`]: up to three spaces, three or more marks.
fn fence(line: &str) -> Option<(char, usize, &str)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent >= 4 {
        return None;
    }
    let body = line[indent..].trim_end_matches(['\r', ' ', '\t']);
    let ch = body.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let run = body.chars().take_while(|&c| c == ch).count();
    if run < 3 {
        return None;
    }
    Some((ch, run, body[run..].trim()))
}

/// The front matter's end: a `---` first line up to `---` or `...`.
fn front_matter_end(text: &str) -> usize {
    let from = lists::bom(text);
    if !(text[from..].starts_with("---") && lists::line_end(text, from) - from == 3) {
        return from;
    }
    let mut at = lists::line_end(text, from);
    for _ in 0..lists::SCAN_LINES {
        let Some(newline) = text[at..].find('\n') else {
            break;
        };
        let next = at + newline + 1;
        at = lists::line_end(text, next);
        let raw = text[next..at].trim_end();
        if raw == "---" || raw == "..." {
            return at;
        }
    }
    from
}

/// The language a fence's info string names, through the shared alias map
/// (`rs`, `py`, `cs`, `C#` …).
fn language(info: &str) -> Option<String> {
    let word = info.split_whitespace().next()?;
    let word = word.trim_start_matches('{').trim_start_matches('.');
    (!word.is_empty()).then(|| okilum_core::code_language::canonical(word))
}

/// The fenced block whose body holds `offset`, if any. Fence lines are not
/// body. Indented code, blockquote and list-marker fences are not seen as
/// fences here, as in [`lists::in_code`].
pub(super) fn block_at(text: &str, offset: usize) -> Option<Block> {
    let mut start = front_matter_end(text);
    if offset < start {
        return None;
    }
    // (character, length, body start, language)
    let mut open: Option<(char, usize, usize, Option<String>)> = None;
    loop {
        let end = lists::line_end(text, start);
        let line = &text[start..end];
        let next = text[end..].find('\n').map(|at| end + at + 1);
        match &open {
            Some((ch, len, body, lang)) => {
                if fence(line)
                    .is_some_and(|(c, run, info)| c == *ch && run >= *len && info.is_empty())
                {
                    if offset < start {
                        // The last body line ends before its newline.
                        let body_end = lists::line_end(text, lists::line_start(text, start - 1));
                        return (*body < start && offset >= *body).then(|| Block {
                            body: *body..body_end,
                            language: lang.clone(),
                        });
                    }
                    open = None;
                }
            }
            None => {
                if start > offset {
                    return None;
                }
                if let Some((ch, len, info)) = fence(line) {
                    // A backtick fence's info string has no backtick.
                    if !(ch == '`' && info.contains('`')) {
                        open = Some((ch, len, next.unwrap_or(text.len() + 1), language(info)));
                    }
                }
            }
        }
        match next {
            Some(next) => start = next,
            None => break,
        }
    }
    let (_, _, body, language) = open?;
    (body <= text.len() && offset >= body).then_some(Block {
        body: body..text.len(),
        language,
    })
}

/// The line comment of a language Okilum highlights, if it has one. Unknown
/// and comment-less languages (JSON, CSS, HTML, plain text) get none.
fn line_comment(language: &str) -> Option<&'static str> {
    let token = match language {
        "rust" | "c" | "cpp" | "csharp" | "go" | "java" | "javascript" | "typescript" | "tsx"
        | "kotlin" | "scala" | "swift" | "zig" | "proto" | "php" => "//",
        "python" | "bash" | "ruby" | "yaml" | "toml" | "elixir" | "cmake" | "make" | "graphql" => {
            "#"
        }
        "sql" | "lua" => "--",
        _ => return None,
    };
    // Only a language the embedded highlighter registry knows.
    crate::source_presentation::code_languages::install();
    gpui_component::highlighter::LanguageRegistry::singleton()
        .language(language)
        .map(|_| token)
}

/// One indentation level of the block: a tab when its lines use tabs, else
/// its smallest space indentation (2 to 8), else four spaces.
fn indent_unit(text: &str, block: &Block) -> String {
    let body = &text[block.body.clone()];
    if body.split('\n').any(|line| line.starts_with('\t')) {
        return "\t".into();
    }
    let width = body
        .split('\n')
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start_matches(' ').len())
        .filter(|&width| width > 0)
        .min()
        .map_or(4, |width| width.clamp(2, 8));
    " ".repeat(width)
}

/// The document's newline: the first one decides, as the editor does.
fn newline(text: &str) -> &'static str {
    match text.find('\n') {
        Some(at) if at > 0 && text.as_bytes()[at - 1] == b'\r' => "\r\n",
        _ => "\n",
    }
}

fn leading_whitespace(line: &str) -> &str {
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

fn closer(open: char) -> Option<char> {
    Some(match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '"' | '\'' => open,
        _ => return None,
    })
}

/// Characters the typed-text hook looks at in a code block.
fn code_pair_char(ch: char) -> bool {
    matches!(ch, '(' | '[' | '{' | ')' | ']' | '}' | '"' | '\'')
}

/// What typing `ch` at `selection` inside `block` does instead of plain
/// typing: wrap the selection, type over the closer at the caret, or close a
/// pair. One Undo step takes the whole change back.
pub(super) fn typed(
    text: &str,
    block: &Block,
    selection: Range<usize>,
    ch: char,
) -> Option<TypedEdit> {
    let width = ch.len_utf8();
    if !selection.is_empty() {
        let close = closer(ch)?;
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
    let next = text[at..]
        .chars()
        .next()
        .filter(|&c| c != '\r' && c != '\n');
    let over = TypedEdit {
        insert_typed: false,
        edits: Vec::new(),
        selection: at + width..at + width,
    };
    // An escaped character is literal: no pair, no type-over.
    let backslashes = before.chars().rev().take_while(|&c| c == '\\').count();
    if backslashes % 2 == 1 {
        return None;
    }
    if matches!(ch, ')' | ']' | '}') {
        return (next == Some(ch)).then_some(over);
    }
    let close = closer(ch)?;
    // A pair closes only before a space, the line end or closing punctuation.
    let free = next.is_none_or(|c| c.is_whitespace() || ")]},;:".contains(c));
    if ch == '"' || ch == '\'' {
        if next == Some(ch) {
            return Some(over);
        }
        // A quote after a word closes a string or is an apostrophe; a Rust
        // lifetime (`&'a`, `<'a>`) is not a quote.
        let rust_lifetime = ch == '\''
            && block.language.as_deref() == Some("rust")
            && matches!(prev, Some('&' | '<'));
        if prev.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == ch)
            || rust_lifetime
            || !free
        {
            return None;
        }
    } else if !free {
        return None;
    }
    Some(TypedEdit {
        insert_typed: false,
        edits: vec![(at..at, format!("{ch}{close}"))],
        selection: at + width..at + width,
    })
}

/// Enter in a code block: the new line keeps the indentation; after an
/// opening bracket (or `:` in Python) it goes one level deeper, and a closer
/// right at the caret moves to its own line at the old indentation.
pub(super) fn enter(text: &str, block: &Block, selection: Range<usize>) -> Option<ListEdit> {
    let start = lists::line_start(text, selection.start);
    let line_end_offset = lists::line_end(text, lists::line_start(text, selection.end));
    let before = &text[start..selection.start];
    let indent = leading_whitespace(before);
    let after = &text[selection.end..line_end_offset];
    let nl = newline(text);
    let opened = before.trim_end().chars().next_back();
    let deeper = matches!(opened, Some('{' | '(' | '['))
        || (opened == Some(':') && block.language.as_deref() == Some("python"));
    if !deeper {
        let insert = format!("{nl}{indent}");
        let caret = selection.start + insert.len();
        return Some(ListEdit {
            edits: vec![(selection, insert)],
            selection: caret..caret,
            revert: None,
        });
    }
    let inner = format!("{indent}{}", indent_unit(text, block));
    let gap = after.len() - after.trim_start_matches([' ', '\t']).len();
    let closes = opened
        .and_then(closer)
        .is_some_and(|close| after[gap..].starts_with(close));
    let (range, insert) = if closes {
        (
            selection.start..selection.end + gap,
            format!("{nl}{inner}{nl}{indent}"),
        )
    } else {
        (selection.clone(), format!("{nl}{inner}"))
    };
    let caret = selection.start + nl.len() + inner.len();
    Some(ListEdit {
        edits: vec![(range, insert)],
        selection: caret..caret,
        revert: None,
    })
}

/// The starts of the lines a command acts on: every line the selection
/// touches (not one it only ends at the start of), only inside the block.
fn selected_lines(text: &str, block: &Block, selection: &Range<usize>) -> Vec<usize> {
    let from = lists::line_start(text, selection.start.max(block.body.start));
    let mut last = selection.end.min(block.body.end);
    if selection.end > selection.start && last > from && lists::line_start(text, last) == last {
        last -= 1;
    }
    let mut lines = Vec::new();
    let mut start = from;
    loop {
        lines.push(start);
        let end = lists::line_end(text, start);
        let Some(newline) = text[end..].find('\n') else {
            break;
        };
        start = end + newline + 1;
        if start > last || start > block.body.end {
            break;
        }
    }
    lines
}

/// The selection after `edits`: it keeps covering the same text, the
/// inserted indentation or comment included.
fn mapped(edits: &[Edit], selection: &Range<usize>) -> Range<usize> {
    if selection.is_empty() {
        let at = lists::map_offset(edits, selection.start, true);
        return at..at;
    }
    lists::map_offset(edits, selection.start, false)..lists::map_offset(edits, selection.end, true)
}

/// Tab and Shift+Tab in a code block. With a caret, Tab inserts one level
/// and Shift+Tab outdents its line; with a selection both act on every
/// selected line of the block.
pub(super) fn indent(
    text: &str,
    block: &Block,
    selection: Range<usize>,
    outdent: bool,
) -> Option<ListEdit> {
    let unit = indent_unit(text, block);
    if selection.is_empty() && !outdent {
        let caret = selection.start + unit.len();
        return Some(ListEdit {
            edits: vec![(selection, unit)],
            selection: caret..caret,
            revert: None,
        });
    }
    let mut edits: Vec<Edit> = Vec::new();
    for start in selected_lines(text, block, &selection) {
        let line = &text[start..lists::line_end(text, start)];
        if outdent {
            let remove = if line.starts_with('\t') {
                1
            } else {
                let spaces = line.len() - line.trim_start_matches(' ').len();
                spaces.min(unit.len().max(1))
            };
            if remove > 0 {
                edits.push((start..start + remove, String::new()));
            }
        } else if !line.trim().is_empty() {
            edits.push((start..start, unit.clone()));
        }
    }
    if edits.is_empty() {
        // Nothing to outdent: the key is still the block's.
        return Some(ListEdit {
            edits,
            selection: selection.clone(),
            revert: None,
        });
    }
    Some(ListEdit {
        selection: mapped(&edits, &selection),
        edits,
        revert: None,
    })
}

/// Cmd/Ctrl+/ in a code block of a known language: comment the selected
/// lines at their common indentation, or uncomment them when every line
/// with text already is. Blank lines stay as they are.
pub(super) fn toggle_comment(
    text: &str,
    block: &Block,
    selection: Range<usize>,
) -> Option<ListEdit> {
    let token = line_comment(block.language.as_deref()?)?;
    let lines: Vec<(usize, &str)> = selected_lines(text, block, &selection)
        .into_iter()
        .map(|start| (start, &text[start..lists::line_end(text, start)]))
        .filter(|(_, line)| !line.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return None;
    }
    let commented = lines
        .iter()
        .all(|(_, line)| line.trim_start_matches([' ', '\t']).starts_with(token));
    let edits: Vec<Edit> = if commented {
        lines
            .iter()
            .map(|(start, line)| {
                let at = start + leading_whitespace(line).len();
                let mut end = at + token.len();
                if text[end..].starts_with(' ') {
                    end += 1;
                }
                (at..end, String::new())
            })
            .collect()
    } else {
        let column = lines
            .iter()
            .map(|(_, line)| leading_whitespace(line).len())
            .min()
            .unwrap_or(0);
        lines
            .iter()
            .map(|(start, _)| (start + column..start + column, format!("{token} ")))
            .collect()
    };
    Some(ListEdit {
        selection: mapped(&edits, &selection),
        edits,
        revert: None,
    })
}

/// The note editor's typed-text hook: code-block pairing inside a fenced
/// block's body, the Markdown `hook` everywhere else.
pub(super) fn typed_hook(markdown: TypedTextHook) -> TypedTextHook {
    Rc::new(move |input: &TypedText<'_>, cx: &App| {
        let mut chars = input.typed.chars();
        let ch = chars.next()?;
        if chars.next().is_none() && code_pair_char(ch) && reader_ui_state::auto_pair(cx).0 {
            let text = input.text.to_string();
            if let Some(block) = block_at(&text, input.selection.start)
                .filter(|block| block.contains(input.selection.end))
            {
                return typed(&text, &block, input.selection.clone(), ch);
            }
        }
        markdown(input, cx)
    })
}

impl Reader {
    /// The note editor's text, selection and the code block the selection
    /// starts in, when a code command may run now.
    fn code_block_input(
        &self,
        window: &Window,
        cx: &App,
    ) -> Option<(Entity<EditorState>, String, Range<usize>, Block)> {
        let input = self.list_input(window, cx)?;
        let (text, selection) = {
            let state = input.read(cx);
            (state.value().to_string(), state.selected_range())
        };
        let block = block_at(&text, selection.start)?;
        Some((input, text, selection, block))
    }

    fn code_enter(&mut self, action: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        if action.secondary || action.shift || !self.ime_allows(Trigger::Enter, cx) {
            return;
        }
        let Some((input, text, selection, block)) = self.code_block_input(window, cx) else {
            return;
        };
        if !block.contains(selection.end) {
            return;
        }
        if let Some(edit) = enter(&text, &block, selection) {
            if self.apply_list_edit(&input, edit, window, cx) {
                cx.stop_propagation();
            }
        }
    }

    fn code_indent(&mut self, outdent: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ime_allows(Trigger::Tab, cx) {
            return;
        }
        let Some((input, text, selection, block)) = self.code_block_input(window, cx) else {
            return;
        };
        let Some(edit) = indent(&text, &block, selection, outdent) else {
            return;
        };
        if edit.edits.is_empty() || self.apply_list_edit(&input, edit, window, cx) {
            cx.stop_propagation();
        }
    }

    fn code_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.list_input(window, cx) else {
            cx.propagate();
            return;
        };
        let (text, selection) = {
            let state = input.read(cx);
            (state.value().to_string(), state.selected_range())
        };
        // The block the selection starts in, or the one it ends in when it
        // starts in prose: only that block's lines change.
        let block = block_at(&text, selection.start).or_else(|| {
            let end = if selection.end > selection.start
                && lists::line_start(&text, selection.end) == selection.end
            {
                selection.end - 1
            } else {
                selection.end
            };
            block_at(&text, end)
        });
        let Some(block) = block else {
            cx.propagate();
            return;
        };
        if !self.ime_allows(Trigger::Text, cx) {
            return;
        }
        let selection = selection.start.max(block.body.start)..selection.end;
        // No known comment syntax (or only blank lines): nothing is guessed,
        // and the key stays the block's.
        if let Some(edit) = toggle_comment(&text, &block, selection) {
            self.apply_list_edit(&input, edit, window, cx);
        }
    }

    /// The code-block keys on the note editor's column, captured before the
    /// list keys and the input; each falls through outside a block's body.
    pub(super) fn code_comfort_actions(&self, column: Div, cx: &mut Context<Self>) -> Div {
        column
            .capture_action(
                cx.listener(|this, action: &Enter, window, cx| this.code_enter(action, window, cx)),
            )
            .capture_action(
                cx.listener(|this, _: &IndentInline, window, cx| {
                    this.code_indent(false, window, cx)
                }),
            )
            .capture_action(
                cx.listener(|this, _: &Indent, window, cx| this.code_indent(false, window, cx)),
            )
            .capture_action(
                cx.listener(|this, _: &OutdentInline, window, cx| {
                    this.code_indent(true, window, cx)
                }),
            )
            .capture_action(
                cx.listener(|this, _: &Outdent, window, cx| this.code_indent(true, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ToggleCodeComment, window, cx| {
                    this.code_comment(window, cx)
                }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use gpui_component::input::{Redo, Undo};

    /// The scanner only: which offsets are a block's body.
    #[test]
    fn the_body_is_between_the_fences_only() {
        let text = "intro\n```rust\nlet a = 1;\n```\nafter";
        let block = block_at(text, text.find("let").unwrap()).unwrap();
        assert_eq!(&text[block.body.clone()], "let a = 1;");
        assert_eq!(block.language.as_deref(), Some("rust"));
        assert_eq!(block_at(text, 0), None, "prose");
        assert_eq!(
            block_at(text, text.find("```rust").unwrap() + 3),
            None,
            "opener"
        );
        assert_eq!(block_at(text, text.rfind("```").unwrap()), None, "closer");
        assert_eq!(block_at(text, text.find("after").unwrap()), None);
        // Aliases and the alias map's canonical names.
        for (fence, language) in [
            ("rs", "rust"),
            ("py", "python"),
            ("cs", "csharp"),
            ("C#", "csharp"),
        ] {
            let text = format!("```{fence} title\nx\n```");
            assert_eq!(
                block_at(&text, text.len() - 5).unwrap().language.as_deref(),
                Some(language)
            );
        }
        // An unclosed fence runs to the end; two backticks are no fence.
        let text = "```js\nfoo(";
        assert_eq!(&text[block_at(text, text.len()).unwrap().body], "foo(");
        assert_eq!(block_at("``\nfoo", 5), None);
        // Front matter is not a fence; an empty block has no body.
        assert_eq!(block_at("---\na: 1\n---\nx", 9), None);
        assert_eq!(block_at("```\n```", 3), None);
        // CRLF: the body ends before the carriage return.
        let text = "```py\r\nx = 1\r\n```\r\n";
        assert_eq!(&text[block_at(text, 9).unwrap().body], "x = 1");
    }

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

    /// `|` is the caret; `[` and `]` a selection.
    fn marked(text: &str) -> (String, Range<usize>) {
        if let Some(at) = text.find('|') {
            return (text.replacen('|', "", 1), at..at);
        }
        let start = text.find('[').unwrap();
        let end = text.find(']').unwrap() - 1;
        (text.replacen('[', "", 1).replacen(']', "", 1), start..end)
    }

    /// Replace the note (no history) and place the marked caret or selection.
    fn set(note: &Note, visual: &mut VisualTestContext, text: &str) {
        let (text, selection) = marked(text);
        note.input
            .update_in(visual, |i, window, cx| i.set_value(text, window, cx));
        note.input
            .update(visual, |i, cx| i.set_selected_range(selection, cx));
        visual.run_until_parked();
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

    /// Type one character, as a key press delivers it.
    fn typed(visual: &mut VisualTestContext, ch: &str) {
        visual.simulate_input(ch);
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

    fn sheet_open(note: &Note, visual: &mut VisualTestContext) -> bool {
        note.reader.read_with(visual, |r, _| r.shortcut_sheet.open)
    }

    /// Typing pairs, wrapping, type-over and escapes inside a block; one
    /// Undo takes each back. Prose and the fence lines keep the Markdown
    /// hook (positive controls).
    fn pairs_in(note: &Note, visual: &mut VisualTestContext) {
        set(note, visual, "é ✓\n```rust\nfn main|\n```");
        typed(visual, "(");
        assert_eq!(shown(note, visual), "é ✓\n```rust\nfn main(|)\n```");
        typed(visual, ")");
        assert_eq!(
            shown(note, visual),
            "é ✓\n```rust\nfn main()|\n```",
            "type-over"
        );
        typed(visual, " ");
        typed(visual, "{");
        assert_eq!(shown(note, visual), "é ✓\n```rust\nfn main() {|}\n```");
        undo(visual);
        assert_eq!(
            shown(note, visual),
            "é ✓\n```rust\nfn main() |\n```",
            "one step"
        );
        redo(visual);
        assert_eq!(shown(note, visual), "é ✓\n```rust\nfn main() {|}\n```");

        // Strings with Unicode, the closing quote typed over, an escape.
        set(note, visual, "```py\ns = |\n```");
        typed(visual, "\"");
        assert_eq!(shown(note, visual), "```py\ns = \"|\"\n```");
        typed(visual, "日");
        typed(visual, "\\");
        typed(visual, "\"");
        assert_eq!(
            shown(note, visual),
            "```py\ns = \"日\\\"|\"\n```",
            "an escaped quote is literal"
        );
        typed(visual, "\"");
        assert_eq!(
            shown(note, visual),
            "```py\ns = \"日\\\"\"|\n```",
            "type-over"
        );
        // An apostrophe after a word stays one character.
        set(note, visual, "```py\n# it|\n```");
        typed(visual, "'");
        assert_eq!(shown(note, visual), "```py\n# it'|\n```");

        // A selection is wrapped, and stays selected.
        set(note, visual, "```js\nx = [a + b];\n```");
        typed(visual, "(");
        assert_eq!(shown(note, visual), "```js\nx = ([a + b]);\n```");
        undo(visual);
        assert_eq!(shown(note, visual), "```js\nx = [a + b];\n```");
        // Markdown wrappers are not code pairs: `*` replaces the selection.
        set(note, visual, "```js\nx = [a];\n```");
        typed(visual, "*");
        assert_eq!(shown(note, visual), "```js\nx = *|;\n```");
        // Positive control: in prose the Markdown hook still wraps.
        set(note, visual, "some [text]\n```js\nx\n```");
        typed(visual, "*");
        assert_eq!(shown(note, visual), "some *[text]*\n```js\nx\n```");
        // The closing fence line is not body: plain typing, no pair.
        set(note, visual, "```js\nx\n```|");
        typed(visual, "{");
        assert_eq!(shown(note, visual), "```js\nx\n```{|");

        // CRLF and an unclosed fence: still the block's body.
        set(note, visual, "a\r\n```c\r\nint f|\r\n```\r\n");
        typed(visual, "(");
        assert_eq!(shown(note, visual), "a\r\n```c\r\nint f(|)\r\n```\r\n");
        set(note, visual, "```js\nfoo|");
        typed(visual, "[");
        assert_eq!(shown(note, visual), "```js\nfoo[|]");
    }

    #[gpui::test]
    fn brackets_and_quotes_pair_inside_a_code_block(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        pairs_in(&note, visual);
    }

    fn enter_in(note: &Note, visual: &mut VisualTestContext) {
        set(note, visual, "```rust\nfn main() {|}\n```");
        keys(visual, "enter");
        assert_eq!(shown(note, visual), "```rust\nfn main() {\n    |\n}\n```");
        undo(visual);
        assert_eq!(
            shown(note, visual),
            "```rust\nfn main() {|}\n```",
            "one step"
        );
        redo(visual);
        assert_eq!(shown(note, visual), "```rust\nfn main() {\n    |\n}\n```");

        // The line's indentation stays; Python opens a level after `:`.
        set(note, visual, "```python\ndef f():\n  x = 1|\n```");
        keys(visual, "enter");
        assert_eq!(
            shown(note, visual),
            "```python\ndef f():\n  x = 1\n  |\n```"
        );
        set(note, visual, "```python\ndef f():|\n```");
        keys(visual, "enter");
        assert_eq!(shown(note, visual), "```python\ndef f():\n    |\n```");
        // CRLF keeps CRLF.
        set(note, visual, "```c\r\n\tif (x) {|\r\n```");
        keys(visual, "enter");
        assert_eq!(shown(note, visual), "```c\r\n\tif (x) {\r\n\t\t|\r\n```");

        // A list marker in code is code; outside it the list continues.
        set(note, visual, "```\n  - a|\n```");
        keys(visual, "enter");
        assert_eq!(shown(note, visual), "```\n  - a\n  |\n```");
        set(note, visual, "- a|\n```\nx\n```");
        keys(visual, "enter");
        assert_eq!(
            shown(note, visual),
            "- a\n- |\n```\nx\n```",
            "positive control"
        );
    }

    #[gpui::test]
    fn enter_keeps_and_opens_indentation_in_a_code_block(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        enter_in(&note, visual);
    }

    fn indent_in(note: &Note, visual: &mut VisualTestContext) {
        set(note, visual, "```python\nif x:\n[a = 1\nb = 2]\n```");
        keys(visual, "tab");
        assert_eq!(
            shown(note, visual),
            "```python\nif x:\n[    a = 1\n    b = 2]\n```"
        );
        keys(visual, "shift-tab");
        assert_eq!(shown(note, visual), "```python\nif x:\n[a = 1\nb = 2]\n```");
        undo(visual);
        assert_eq!(
            shown(note, visual),
            "```python\nif x:\n[    a = 1\n    b = 2]\n```",
            "one step"
        );
        undo(visual);
        assert_eq!(shown(note, visual), "```python\nif x:\n[a = 1\nb = 2]\n```");
        redo(visual);
        assert_eq!(
            shown(note, visual),
            "```python\nif x:\n[    a = 1\n    b = 2]\n```"
        );

        // A caret: Tab inserts the block's own level (two spaces here).
        set(note, visual, "```js\nif (x) {\n  y();\n|\n```");
        keys(visual, "tab");
        assert_eq!(shown(note, visual), "```js\nif (x) {\n  y();\n  |\n```");

        // A selection that leaves the block changes only the block's lines.
        set(note, visual, "```sh\n[echo a\n```\n- item]");
        keys(visual, "tab");
        let text = note.input.read_with(visual, |i, _| i.value().to_string());
        assert_eq!(text, "```sh\n    echo a\n```\n- item");
        set(note, visual, "```sh\n  [echo a\n```\nprose]");
        keys(visual, "shift-tab");
        let text = note.input.read_with(visual, |i, _| i.value().to_string());
        assert_eq!(text, "```sh\necho a\n```\nprose");
    }

    #[gpui::test]
    fn tab_and_shift_tab_indent_the_selected_code_lines(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        indent_in(&note, visual);
    }

    fn comment_in(note: &Note, visual: &mut VisualTestContext) {
        for (fence, open, done) in [
            (
                "rust",
                "[let a = 1;\n    let b = 2;]",
                "[// let a = 1;\n//     let b = 2;]",
            ),
            (
                "rs",
                "  [let a = 1;\n\n  let b = 2;]",
                "  [// let a = 1;\n\n  // let b = 2;]",
            ),
            ("python", "[x = 1\ny = 2]", "[# x = 1\n# y = 2]"),
            ("py", "if x:\n    pass|", "if x:\n    # pass|"),
            ("csharp", "[var a = 1;]", "[// var a = 1;]"),
            ("C#", "[var a = 1;]", "[// var a = 1;]"),
            ("cs", "[var a = 1;]", "[// var a = 1;]"),
            ("sql", "[select 1;]", "[-- select 1;]"),
        ] {
            set(note, visual, &format!("é\n```{fence}\n{open}\n```"));
            keys(visual, "ctrl-/");
            assert_eq!(
                shown(note, visual),
                format!("é\n```{fence}\n{done}\n```"),
                "{fence}"
            );
            keys(visual, "ctrl-/");
            assert_eq!(
                shown(note, visual),
                format!("é\n```{fence}\n{open}\n```"),
                "{fence} round trip"
            );
            undo(visual);
            assert_eq!(
                shown(note, visual),
                format!("é\n```{fence}\n{done}\n```"),
                "{fence} one step"
            );
            undo(visual);
            assert_eq!(shown(note, visual), format!("é\n```{fence}\n{open}\n```"));
            redo(visual);
            assert_eq!(shown(note, visual), format!("é\n```{fence}\n{done}\n```"));
            assert!(!sheet_open(note, visual), "{fence}: the key is the block's");
        }

        // Unknown or no language: no guessed syntax, still editable.
        for fence in ["", "foo", "json"] {
            set(note, visual, &format!("```{fence}\nvalue|\n```"));
            keys(visual, "ctrl-/");
            assert_eq!(
                shown(note, visual),
                format!("```{fence}\nvalue|\n```"),
                "{fence:?}"
            );
            typed(visual, "(");
            assert_eq!(
                shown(note, visual),
                format!("```{fence}\nvalue(|)\n```"),
                "{fence:?}"
            );
            assert!(!sheet_open(note, visual));
        }

        // A selection from prose into the block comments only code lines.
        set(note, visual, "[intro\n```rust\nlet a = 1;]\n```\n- item");
        keys(visual, "ctrl-/");
        let text = note.input.read_with(visual, |i, _| i.value().to_string());
        assert_eq!(text, "intro\n```rust\n// let a = 1;\n```\n- item");
        set(note, visual, "```rust\n[let a = 1;\n```\n- item]");
        keys(visual, "ctrl-/");
        let text = note.input.read_with(visual, |i, _| i.value().to_string());
        assert_eq!(text, "```rust\n// let a = 1;\n```\n- item");

        // Positive control: in prose the key still opens the shortcut sheet.
        set(note, visual, "prose|\n```rust\nx\n```");
        keys(visual, "ctrl-/");
        assert!(
            sheet_open(note, visual),
            "the key reaches the sheet in prose"
        );
        assert_eq!(
            note.input.read_with(visual, |i, _| i.value().to_string()),
            "prose\n```rust\nx\n```"
        );
    }

    #[gpui::test]
    fn the_comment_key_toggles_the_language_line_comment(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        comment_in(&note, visual);
    }

    /// Live Preview edits the same source through the same keys.
    #[gpui::test]
    fn code_comforts_work_in_live_preview(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        note.reader
            .update_in(visual, |r, window, cx| r.set_live_preview(true, window, cx));
        visual.run_until_parked();
        let live = |visual: &mut VisualTestContext| {
            note.reader.read_with(visual, |r, _| {
                r.editing.as_ref().unwrap().live_preview.enabled
            })
        };
        assert!(live(visual));
        pairs_in(&note, visual);
        enter_in(&note, visual);
        indent_in(&note, visual);
        comment_in(&note, visual);
        assert!(live(visual));
    }

    fn compose(note: &Note, visual: &mut VisualTestContext, text: &str) {
        note.input.update_in(visual, |i, window, cx| {
            i.replace_and_mark_text_in_range(None, text, None, window, cx)
        });
        visual.run_until_parked();
    }

    fn value(note: &Note, visual: &mut VisualTestContext) -> String {
        note.input.read_with(visual, |i, _| i.value().to_string())
    }

    /// No pairing, indentation or comment while an input method composes.
    /// After the settle the same keys work again (positive control).
    #[gpui::test]
    fn nothing_runs_during_a_composition(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        set(&note, visual, "```rust\nfn f|\n```");
        compose(&note, visual, "(");
        assert_eq!(
            value(&note, visual),
            "```rust\nfn f(\n```",
            "preedit pairs nothing"
        );

        set(&note, visual, "```rust\nfn f() {|\n```");
        compose(&note, visual, "か");
        keys(visual, "ctrl-/");
        assert!(
            !value(&note, visual).contains("//"),
            "no comment on preedit"
        );
        keys(visual, "tab");
        assert!(
            !value(&note, visual).contains("    "),
            "no indent on preedit"
        );

        set(&note, visual, "```rust\nfn f() {|\n```");
        compose(&note, visual, "か");
        note.input.update_in(visual, |i, window, cx| {
            let marked = i.marked_text_range(window, cx);
            i.replace_text_in_range(marked, "漢", window, cx)
        });
        visual.run_until_parked();
        keys(visual, "enter");
        assert!(
            !value(&note, visual).contains("\n    "),
            "the committing Enter opens no level: {:?}",
            value(&note, visual)
        );
        visual
            .executor()
            .advance_clock(super::super::ime_guard::IME_SETTLE + Duration::from_millis(100));
        visual.run_until_parked();
        set(&note, visual, "```rust\nfn f() {|\n```");
        keys(visual, "enter");
        assert_eq!(
            shown(&note, visual),
            "```rust\nfn f() {\n    |\n```",
            "after the settle"
        );
    }

    /// An editor that is not editable changes nothing: the comforts ask the
    /// same state the input does. The note's `Editor` element re-applies its
    /// editability on every frame, so each key arrives in the same update;
    /// the same keys without the lock are the positive control.
    #[gpui::test]
    fn a_read_only_editor_runs_no_comfort(cx: &mut TestAppContext) {
        let (note, visual) = open(cx);
        let start = "```rust\n  fn f() {[let a = 1;]\n```";
        let press = |visual: &mut VisualTestContext, locked: bool, key: &str| {
            set(&note, visual, start);
            let input = note.input.clone();
            visual.update(|window, cx| {
                if locked {
                    input.update(cx, |i, cx| i.set_readonly(true, cx));
                    assert!(!input.read(cx).is_editable());
                }
                // The actions the keys map to, dispatched on the input's
                // node in this frame, before a redraw.
                let action: Box<dyn Action> = match key {
                    "tab" => Box::new(IndentInline),
                    "shift-tab" => Box::new(OutdentInline),
                    "enter" => Box::new(Enter {
                        secondary: false,
                        shift: false,
                    }),
                    "comment" => Box::new(ToggleCodeComment),
                    _ => {
                        // What the platform's input handler does for a typed key.
                        input.update(cx, |i, cx| i.replace_text_in_range(None, key, window, cx));
                        return;
                    }
                };
                let focus = input.read(cx).focus_handle(cx);
                focus.dispatch_action(action.as_ref(), window, cx);
            });
            visual.run_until_parked();
            value(&note, visual)
        };
        for key in ["(", "tab", "shift-tab", "enter", "comment"] {
            let unlocked = press(visual, false, key);
            assert_ne!(
                unlocked, "```rust\n  fn f() {let a = 1;\n```",
                "{key}: control"
            );
            assert_eq!(
                press(visual, true, key),
                "```rust\n  fn f() {let a = 1;\n```",
                "{key}: read-only"
            );
        }
    }
}
