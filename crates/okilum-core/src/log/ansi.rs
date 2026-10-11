//! ANSI escape sequences in log lines (#1164).
//!
//! Console encoders (zap, logrus, tracing, spdlog's colour sink) colour their
//! output with SGR codes such as `\x1b[31m`, and `kubectl logs` passes them
//! through. They are removed before a line is parsed, matched or shown, so
//! `level=error` and a text search see what a terminal shows. The file's
//! bytes, and therefore a raw copy, keep them.

use std::borrow::Cow;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

/// `line` without escape sequences; borrowed when it has none.
pub fn strip(line: &[u8]) -> Cow<'_, [u8]> {
    let Some(first) = memchr::memchr(ESC, line) else {
        return Cow::Borrowed(line);
    };
    let mut out = Vec::with_capacity(line.len());
    out.extend_from_slice(&line[..first]);
    let mut i = first;
    while i < line.len() {
        if line[i] != ESC {
            let next = memchr::memchr(ESC, &line[i..]).map_or(line.len(), |at| i + at);
            out.extend_from_slice(&line[i..next]);
            i = next;
            continue;
        }
        i = skip_sequence(line, i);
    }
    Cow::Owned(out)
}

/// Text without escape sequences. Sequences are ASCII, so what remains is
/// still the valid UTF-8 it was.
pub fn strip_text(text: Cow<'_, str>) -> Cow<'_, str> {
    if !text.as_bytes().contains(&ESC) {
        return text;
    }
    let bytes = strip(text.as_bytes()).into_owned();
    Cow::Owned(String::from_utf8(bytes).expect("removing ASCII keeps UTF-8 valid"))
}

/// The position after the sequence that starts with the ESC at `at`.
fn skip_sequence(line: &[u8], at: usize) -> usize {
    let mut i = at + 1;
    match line.get(i) {
        // CSI: parameters 0x30–0x3F, intermediates 0x20–0x2F, a final byte
        // 0x40–0x7E (`\x1b[1;31m`, `\x1b[2K`).
        Some(b'[') => {
            i += 1;
            while line.get(i).is_some_and(|c| (0x20..=0x3f).contains(c)) {
                i += 1;
            }
            if line.get(i).is_some_and(|c| (0x40..=0x7e).contains(c)) {
                i += 1;
            }
            i
        }
        // OSC (hyperlinks, titles): up to BEL or ESC `\`.
        Some(b']') => {
            i += 1;
            while i < line.len() {
                match line[i] {
                    BEL => return i + 1,
                    ESC if line.get(i + 1) == Some(&b'\\') => return i + 2,
                    _ => i += 1,
                }
            }
            i
        }
        // Other two-byte forms, with optional intermediates (`\x1b(B`).
        Some(_) => {
            while line.get(i).is_some_and(|c| (0x20..=0x2f).contains(c)) {
                i += 1;
            }
            if line.get(i).is_some_and(|c| (0x30..=0x7e).contains(c)) {
                i += 1;
            }
            i
        }
        None => i,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(line: &str) -> String {
        String::from_utf8(strip(line.as_bytes()).into_owned()).unwrap()
    }

    #[test]
    fn colours_and_controls_go_text_stays() {
        assert_eq!(
            clean("\x1b[36mINFO\x1b[0m[0000] listening \x1b[36mport\x1b[0m=8080"),
            "INFO[0000] listening port=8080"
        );
        assert_eq!(clean("\x1b[1;31mERROR\x1b[m boom"), "ERROR boom");
        assert_eq!(clean("\x1b[2K\rprogress"), "\rprogress");
        assert_eq!(
            clean("see \x1b]8;;https://x.test\x07link\x1b]8;;\x07 end"),
            "see link end"
        );
        assert_eq!(clean("\x1b]0;title\x1b\\text"), "text");
        assert_eq!(clean("\x1b(Bplain"), "plain");
        assert_eq!(clean("trailing \x1b"), "trailing ");
        assert_eq!(clean("héllo \x1b[32m✓\x1b[0m"), "héllo ✓");
        assert!(matches!(strip(b"no escapes"), Cow::Borrowed(_)));
    }

    #[test]
    fn decoded_json_escapes_are_stripped_too() {
        assert_eq!(
            strip_text(Cow::Borrowed("\u{1b}[31merror\u{1b}[0m")),
            "error"
        );
        assert!(matches!(strip_text(Cow::Borrowed("é")), Cow::Borrowed(_)));
    }
}
