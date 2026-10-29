//! Fence container prefixes (#1893). List markers become continuation spaces;
//! quote markers and authored indentation keep their exact bytes.

/// Convert the syntax before an opening fence, not arbitrary paragraph text.
/// Each nesting level is consumed separately: list → quote and quote → list
/// obey the same rule. The caller must prove the line is a fence opener.
fn continuation_prefix(prefix: &str) -> Option<String> {
    let bytes = prefix.as_bytes();
    let mut at = 0;
    let mut column = 0;
    let mut result = String::new();
    while at < bytes.len() {
        match bytes[at] {
            b' ' | b'\t' => {
                result.push(bytes[at] as char);
                column = next_column(column, bytes[at]);
                at += 1;
            }
            b'>' => {
                result.push('>');
                column += 1;
                at += 1;
            }
            _ => {
                let start = at;
                let marker_end = match bytes[at] {
                    b'-' | b'+' | b'*' => at + 1,
                    b'0'..=b'9' => {
                        let digits = bytes[at..]
                            .iter()
                            .take_while(|b| b.is_ascii_digit())
                            .count();
                        if digits > 9 || !matches!(bytes.get(at + digits), Some(b'.' | b')')) {
                            return None;
                        }
                        at + digits + 1
                    }
                    _ => return None,
                };
                at = marker_end;
                while matches!(bytes.get(at), Some(b' ' | b'\t')) {
                    at += 1;
                }
                if at == marker_end {
                    return None;
                }
                // A task box is part of its list marker, including the gap.
                if matches!(bytes.get(at..at + 3), Some(b"[ ]" | b"[x]" | b"[X]"))
                    && matches!(bytes.get(at + 3), Some(b' ' | b'\t'))
                {
                    at += 3;
                    while matches!(bytes.get(at), Some(b' ' | b'\t')) {
                        at += 1;
                    }
                }
                let end_column = bytes[start..at]
                    .iter()
                    .fold(column, |col, b| next_column(col, *b));
                result.push_str(&" ".repeat(end_column - column));
                column = end_column;
            }
        }
    }
    Some(result)
}

/// CommonMark tab stops are four columns; ordinary indentation stays verbatim.
fn next_column(column: usize, byte: u8) -> usize {
    column + if byte == b'\t' { 4 - column % 4 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_container_level_has_its_own_continuation_prefix() {
        for (opening, closing) in [
            ("", ""),
            ("  ", "  "),
            ("- ", "  "),
            ("  - ", "    "),
            ("1. ", "   "),
            ("123) ", "     "),
            ("- [ ] ", "      "),
            ("- [x] ", "      "),
            ("> ", "> "),
            ("> > ", "> > "),
            ("> - ", ">   "),
            ("- > ", "  > "),
            ("> - [ ] ", ">       "),
            ("- [ ] > ", "      > "),
            ("> 1. - [x] > ", ">          > "),
            (" \t> - ", " \t>   "),
            ("-\t", "    "),
        ] {
            assert_eq!(
                continuation_prefix(opening).as_deref(),
                Some(closing),
                "{opening:?}"
            );
        }
    }

    #[test]
    fn paragraph_text_and_incomplete_markers_are_not_container_prefixes() {
        for prefix in ["a ", "-", "12.", "[ ] ", "~~", "> text ", "1234567890. "] {
            assert_eq!(continuation_prefix(prefix), None, "{prefix:?}");
        }
    }
}
