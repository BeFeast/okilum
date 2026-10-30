//! Typed fence assistance over the existing exact-source/Undo hook (#1893).
use super::*;
use okilum_core::source_fences;

fn newline(text: &str) -> &'static str {
    match text.find('\n') {
        Some(at) if at > 0 && text.as_bytes()[at - 1] == b'\r' => "\r\n",
        _ => "\n",
    }
}
fn next_line(text: &str, end: usize) -> Option<(usize, &str)> {
    let start = end + text[end..].find('\n')? + 1;
    let end = lists::line_end(text, start);
    Some((start, &text[start..end]))
}

/// Insert the actual key first, then one separately undoable assistance edit.
pub(super) fn typed(text: &str, selection: Range<usize>, ch: char) -> Option<TypedEdit> {
    if !selection.is_empty() || !matches!(ch, '`' | '~') {
        return None;
    }
    let at = selection.start;
    let run = run_before(text, at, ch as u8);
    if run < 2 {
        return None;
    }
    let end = lists::line_end(text, lists::line_start(text, at));
    if !text[at..end].trim().is_empty() {
        return None;
    }
    // Prove the proposed opener without parsing unrelated content below it.
    let candidate = format!("{}{ch}{}", &text[..at], &text[at..end]);
    let opening = source_fences::opening_line(&candidate)?;
    if opening.character != ch || opening.length != run + 1 {
        return None;
    }
    let marker = ch.to_string().repeat(run + 1);
    let closing = format!("{}{marker}", opening.continuation);
    let edit = if run == 2 {
        if next_line(text, end).is_some_and(|(_, line)| line == closing) {
            return None;
        }
        (end + 1..end + 1, format!("{}{closing}", newline(text)))
    } else {
        // Extend only an adjacent empty matching block, never a code body.
        let (start, line) = next_line(text, end)?;
        if line != format!("{}{}", opening.continuation, ch.to_string().repeat(run)) {
            return None;
        }
        let start = start + opening.continuation.len() + 1;
        (start..start + run, marker)
    };
    Some(TypedEdit {
        insert_typed: true,
        edits: vec![edit],
        selection: at + 1..at + 1,
    })
}

/// Enter at an empty block's opener continues its container, not a new item.
pub(in crate::reader_editor) fn enter(text: &str, selection: Range<usize>) -> Option<ListEdit> {
    if !selection.is_empty() {
        return None;
    }
    let at = selection.start;
    let end = lists::line_end(text, lists::line_start(text, at));
    if at != end {
        return None;
    }
    let line = &text[lists::line_start(text, at)..end];
    if !line.contains("```") && !line.contains("~~~") {
        return None;
    }
    let opening = source_fences::opening_line(&text[..at])?;
    let (_, closing) = next_line(text, end)?;
    if closing
        != format!(
            "{}{}",
            opening.continuation,
            opening.character.to_string().repeat(opening.length)
        )
    {
        return None;
    }
    let insert = format!("{}{}", newline(text), opening.continuation);
    let caret = at + insert.len();
    Some(ListEdit {
        edits: vec![(at..at, insert)],
        selection: caret..caret,
        revert: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn type_at(marked: &str, ch: char) -> Option<String> {
        let at = marked.find('|').unwrap();
        let mut text = marked.replacen('|', "", 1);
        let edit = typed(&text, at..at, ch)?;
        assert!(
            edit.insert_typed,
            "native typing stays its own Undo transaction"
        );
        text.insert(at, ch);
        text = lists::apply(&text, &edit.edits);
        text.insert(edit.selection.start, '|');
        Some(text)
    }

    #[test]
    fn typed_openers_close_with_exact_nested_prefixes_and_line_endings() {
        for (before, ch, after) in [
            ("``|", '`', "```|\n```"),
            ("~~|", '~', "~~~|\n~~~"),
            ("  ``|", '`', "  ```|\n  ```"),
            ("> ``|", '`', "> ```|\n> ```"),
            ("- ``|", '`', "- ```|\n  ```"),
            ("1. ``|", '`', "1. ```|\n   ```"),
            ("- [ ] Task\n  ``|", '`', "- [ ] Task\n  ```|\n  ```"),
            ("> - ``|", '`', "> - ```|\n>   ```"),
            ("- > ``|", '`', "- > ```|\n  > ```"),
            ("- [ ] Task\n  > ``|", '`', "- [ ] Task\n  > ```|\n  > ```"),
            (
                "> - [x] Task\n>   ``|",
                '`',
                "> - [x] Task\n>   ```|\n>   ```",
            ),
            ("\u{feff}``|\r\nrest", '`', "\u{feff}```|\r\n```\r\nrest"),
            (
                "before\r\n~~|\r\nrest",
                '~',
                "before\r\n~~~|\r\n~~~\r\nrest",
            ),
        ] {
            assert_eq!(type_at(before, ch).as_deref(), Some(after), "{before:?}");
        }
    }

    #[test]
    fn long_fences_extend_only_the_adjacent_empty_matching_closer() {
        assert_eq!(type_at("```|\n```", '`').as_deref(), Some("````|\n````"));
        assert_eq!(
            type_at("> ~~~|\n> ~~~", '~').as_deref(),
            Some("> ~~~~|\n> ~~~~")
        );
        for marked in ["```|\nbody\n```", "```|\n~~~", "```|\n> ```"] {
            assert_eq!(type_at(marked, '`'), None, "{marked}");
        }
    }

    #[test]
    fn closing_fences_code_body_inline_text_and_existing_closers_are_untouched() {
        for marked in [
            "```\n``|",
            "````\n``|",
            "> ```\n> ``|",
            "- ```\n  ``|",
            "text ``|",
            "``|more",
            "``|\n```",
            "---\n``|",
            "    ``|",
            "- [ ] ``|",
            "> - [x] ``|",
            "- [ ] > ``|",
        ] {
            assert_eq!(type_at(marked, '`'), None, "{marked:?}");
        }
    }

    #[test]
    fn enter_keeps_the_new_empty_block_inside_its_container() {
        for (before, after) in [
            ("```rust|\n```", "```rust\n|\n```"),
            ("- ```|\n  ```", "- ```\n  |\n  ```"),
            ("> - ~~~|\n>   ~~~", "> - ~~~\n>   |\n>   ~~~"),
            ("- > ```|\n  > ```", "- > ```\n  > |\n  > ```"),
            ("- [ ] Task\n  ```|\n  ```", "- [ ] Task\n  ```\n  |\n  ```"),
        ] {
            let at = before.find('|').unwrap();
            let text = before.replacen('|', "", 1);
            let edit = enter(&text, at..at).unwrap();
            let mut actual = lists::apply(&text, &edit.edits);
            actual.insert(edit.selection.start, '|');
            assert_eq!(actual, after);
        }
    }
}
