//! Render-only punctuation candidates in a classifier-approved prose block.
//! Coordinates always refer to authored UTF-8; this module never edits source.
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

pub(super) fn replacements(
    source: &str,
    eligible: impl Fn(&Range<usize>) -> bool,
) -> Vec<(Range<usize>, &'static str)> {
    let mut out = Vec::new();
    // A blank line resets language and quote pairing, including CRLF notes.
    let mut paragraph = 0;
    let mut at = 0;
    for line in source.split_inclusive('\n') {
        if line.trim().is_empty() {
            scan(source, paragraph..at, &eligible, &mut out);
            paragraph = at + line.len();
        }
        at += line.len();
    }
    scan(source, paragraph..source.len(), &eligible, &mut out);
    out.sort_by_key(|(range, _)| range.start);
    out
}

fn scan(
    source: &str,
    paragraph: Range<usize>,
    eligible: &impl Fn(&Range<usize>) -> bool,
    out: &mut Vec<(Range<usize>, &'static str)>,
) {
    let text = &source[paragraph.clone()];
    let russian = text.char_indices().any(|(at, ch)| {
        (('а'..='я').contains(&ch) || ('А'..='Я').contains(&ch) || ch == 'ё' || ch == 'Ё')
            && eligible(&(paragraph.start + at..paragraph.start + at + ch.len_utf8()))
    });
    let mut double = None;
    let mut single = None;
    let mut at = paragraph.start;
    while at < paragraph.end {
        let tail = &source[at..paragraph.end];
        let ch = tail.chars().next().unwrap();
        let escaped = source[..at]
            .bytes()
            .rev()
            .take_while(|b| *b == b'\\')
            .count()
            % 2
            == 1;
        let (len, glyph) = if tail.starts_with("...") {
            (3, Some("…"))
        } else if tail.starts_with("---") {
            (3, Some("—"))
        } else if tail.starts_with("--") {
            (2, Some("—"))
        } else {
            (ch.len_utf8(), None)
        };
        let range = at..at + len;
        if !escaped && eligible(&range) {
            if let Some(glyph) = glyph {
                out.push((range, glyph));
            } else if ch == '"' || ch == '\'' {
                // Bidi formatting controls affect visual order, not quote
                // boundaries. Keep every authored byte and pair in logical
                // source order, including isolates around embedded LTR text.
                let before = source[paragraph.start..at]
                    .graphemes(true)
                    .rev()
                    .find(|cluster| !cluster.chars().all(direction_control))
                    .and_then(|cluster| cluster.chars().next());
                let after = source[at + len..paragraph.end]
                    .chars()
                    .find(|c| !direction_control(*c));
                let contraction = (ch == '\'')
                    .then(|| contraction_context(source, &paragraph, &range))
                    .flatten();
                if let Some(context) = contraction {
                    // Context must belong to the same approved prose span.
                    // The replacement itself covers only the ASCII apostrophe,
                    // never a neighbouring letter, mark or bidi control.
                    if eligible(&context) {
                        out.push((range, "’"));
                    }
                } else {
                    let pending = if ch == '"' { &mut double } else { &mut single };
                    if before.is_none_or(|c| !c.is_alphanumeric() && c != '"' && c != '\'')
                        && after.is_some_and(|c| !c.is_whitespace())
                    {
                        *pending = Some(range);
                    } else if before.is_some_and(|c| !c.is_whitespace()) {
                        if let Some(open) = pending.take() {
                            let (left, right) = if ch == '\'' {
                                ("‘", "’")
                            } else if russian {
                                ("«", "»")
                            } else {
                                ("“", "”")
                            };
                            out.push((open, left));
                            out.push((range, right));
                        }
                    }
                }
            }
        }
        at += len;
    }
}

/// The adjacent logical word clusters, including intervening bidi controls.
/// Grapheme context handles decomposed accents and RTL vowel marks without
/// widening the replacement range or borrowing text from another paragraph.
fn contraction_context(
    source: &str,
    paragraph: &Range<usize>,
    apostrophe: &Range<usize>,
) -> Option<Range<usize>> {
    // A mark attached to the apostrophe is not a word cluster after it.
    if source[apostrophe.start..paragraph.end]
        .graphemes(true)
        .next()?
        != "'"
    {
        return None;
    }
    let (left, before) = source[paragraph.start..apostrophe.start]
        .grapheme_indices(true)
        .rev()
        .find(|(_, cluster)| !cluster.chars().all(direction_control))?;
    let (right, after) = source[apostrophe.end..paragraph.end]
        .grapheme_indices(true)
        .find(|(_, cluster)| !cluster.chars().all(direction_control))?;
    if !before.chars().next()?.is_alphanumeric() || !after.chars().next()?.is_alphanumeric() {
        return None;
    }
    Some(paragraph.start + left..apostrophe.end + right + after.len())
}

// Unicode Bidi_Control characters only: ZWJ/ZWNJ and combining marks
// participate in authored words and must not be discarded as formatting.
fn direction_control(ch: char) -> bool {
    matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

#[cfg(test)]
mod tests {
    use super::*;
    fn shown(source: &str, eligible: impl Fn(&Range<usize>) -> bool) -> String {
        let mut text = source.to_owned();
        for (range, glyph) in replacements(source, eligible).into_iter().rev() {
            text.replace_range(range, glyph);
        }
        text
    }
    #[test]
    fn english_russian_and_contractions_keep_authored_bytes() {
        let source = "\"Привет\" -- ...";
        let original = source.as_bytes().to_vec();
        assert_eq!(shown(source, |_| true), "«Привет» — …");
        assert_eq!(source.as_bytes(), original);
        assert_eq!(
            shown("\"Hello\" isn't 'bad'...", |_| true),
            "“Hello” isn’t ‘bad’…"
        );
    }
    #[test]
    fn paragraph_language_and_excluded_code_are_independent() {
        assert_eq!(
            shown("\"Hello\"\r\n\r\n\"Привет\"", |_| true),
            "“Hello”\r\n\r\n«Привет»"
        );
        let source = "`Привет` \"Hello\"";
        assert_eq!(
            shown(source, |r| r.start >= "`Привет`".len()),
            "`Привет` “Hello”"
        );
    }
    #[test]
    fn mixed_direction_quotes_keep_controls_and_logical_source_ranges() {
        for control in [
            '\u{061c}', '\u{200e}', '\u{200f}', '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}',
            '\u{202e}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
        ] {
            let raw = format!("עברית \"{control}Hello{control}\" -- ... שלום");
            let expected = format!("עברית “{control}Hello{control}” — … שלום");
            assert_eq!(shown(&raw, |_| true), expected);
            let replacements = replacements(&raw, |_| true);
            assert_eq!(
                replacements.len(),
                4,
                "positive control: both quotes, dash, ellipsis"
            );
            assert_eq!(&raw[replacements[0].0.clone()], "\"");
            assert_eq!(&raw[replacements[1].0.clone()], "\"");
        }
        let raw = "\u{2067}\"שלום\u{200f}\"\u{2069} and \"Hello\"";
        assert_eq!(
            shown(raw, |_| true),
            "\u{2067}“שלום\u{200f}”\u{2069} and “Hello”"
        );
    }

    #[test]
    fn contractions_use_word_clusters_but_replace_only_the_ascii_apostrophe() {
        for raw in [
            "isn't",
            "cafe\u{301}'s",
            "שָׁלוֹם's",
            "α\u{301}'s",
            "can\u{200f}'\u{200e}t",
        ] {
            let at = raw.find('\'').unwrap();
            let candidates = replacements(raw, |_| true);
            assert_eq!(candidates, vec![(at..at + 1, "’")], "{raw:?}");
            assert_eq!(shown(raw, |_| true), raw.replacen('\'', "’", 1));
        }
        assert_eq!(
            shown("'cafe\u{301}' isn't", |_| true),
            "‘cafe\u{301}’ isn’t"
        );
    }

    #[test]
    fn contraction_context_cannot_cross_excluded_source_or_paragraph_boundaries() {
        let raw = "can\u{200f}'\u{200e}t";
        let apostrophe = raw.find('\'').unwrap();
        // Even if the punctuation is eligible, an excluded neighbour/control
        // must not supply the evidence that it is an in-word apostrophe.
        for excluded in [
            0..3,
            3..apostrophe,
            apostrophe..apostrophe + 1,
            apostrophe + 1..raw.len() - 1,
            raw.len() - 1..raw.len(),
        ] {
            assert_eq!(
                shown(raw, |r| !(r.start < excluded.end && excluded.start < r.end)),
                raw
            );
        }
        assert_eq!(shown("can'\r\n\r\n't", |_| true), "can'\r\n\r\n't");
        assert_eq!(shown("can\\'t", |_| true), "can\\'t");
        assert_eq!(shown("a'\u{301}b", |_| true), "a'\u{301}b");
    }

    #[test]
    fn excluded_and_escaped_source_and_unpaired_quotes_stay_literal() {
        assert_eq!(shown("\"code\" -- ...", |_| false), "\"code\" -- ...");
        assert_eq!(
            shown("\\\"literal\\\" \\... \"unfinished", |_| true),
            "\\\"literal\\\" \\... \"unfinished"
        );
    }
}
