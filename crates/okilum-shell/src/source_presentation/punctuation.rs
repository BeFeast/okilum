//! Render-only punctuation candidates in a classifier-approved prose block.
//! Coordinates always refer to authored UTF-8; this module never edits source.
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

#[cfg(test)]
thread_local! {
    pub(super) static INDEXED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn replacements(
    source: &str,
    eligible: impl Fn(&Range<usize>) -> bool,
) -> Vec<(Range<usize>, &'static str)> {
    INDEXED_BYTES.with(|n| n.set(n.get() + source.len()));
    let boundaries: Vec<_> = source
        .grapheme_indices(true)
        .map(|(at, _)| at)
        .chain(std::iter::once(source.len()))
        .collect();
    replacements_indexed(source, eligible, |range| {
        boundaries.binary_search(&range.start).is_ok()
            && boundaries.binary_search(&range.end).is_ok()
    })
}

/// Immutable lexical summaries keep paragraph-wide quote/language decisions
/// separate from source inspection. An edit re-lexes complete changed logical
/// lines; pairing the cached events never reads unchanged source bytes.
#[derive(Clone)]
pub(super) struct Inventory {
    rows: Vec<Row>,
}
#[derive(Clone)]
struct Row {
    range: Range<usize>,
    blank: bool,
    russian: Vec<Range<usize>>,
    events: Vec<Event>,
}
#[derive(Clone)]
struct Event {
    range: Range<usize>,
    context: Range<usize>,
    boundary: bool,
    kind: EventKind,
}
#[derive(Clone, Copy)]
enum EventKind {
    Fixed(&'static str),
    Open(char),
    Close(char),
}
impl Inventory {
    pub(super) fn new(
        source: &str,
        origin: usize,
        boundary: impl Fn(&Range<usize>) -> bool,
    ) -> Self {
        let mut at = origin;
        let rows = source
            .split_inclusive('\n')
            .map(|line| {
                let row = lex(line, at, &boundary);
                at += line.len();
                row
            })
            .collect();
        Self { rows }
    }
    pub(super) fn remap(
        &self,
        old_lines: Range<usize>,
        new_lines: &str,
        edit: &okilum_core::source_projection::revision::Edit,
        boundary: impl Fn(&Range<usize>) -> bool,
    ) -> Self {
        let mut rows: Vec<_> = self
            .rows
            .iter()
            .filter(|r| r.range.end <= old_lines.start)
            .cloned()
            .collect();
        rows.extend(Self::new(new_lines, old_lines.start, boundary).rows);
        rows.extend(
            self.rows
                .iter()
                .filter(|r| r.range.start >= old_lines.end)
                .map(|r| {
                    let map = |r: &Range<usize>| edit.forward(r.start)..edit.forward(r.end);
                    Row {
                        range: map(&r.range),
                        blank: r.blank,
                        russian: r.russian.iter().map(map).collect(),
                        events: r
                            .events
                            .iter()
                            .map(|e| Event {
                                range: map(&e.range),
                                context: map(&e.context),
                                ..e.clone()
                            })
                            .collect(),
                    }
                }),
        );
        Self { rows }
    }
    pub(super) fn shifted(&self, map: impl Fn(usize) -> usize) -> Self {
        let range = |r: &Range<usize>| map(r.start)..map(r.end);
        Self {
            rows: self
                .rows
                .iter()
                .map(|r| Row {
                    range: range(&r.range),
                    blank: r.blank,
                    russian: r.russian.iter().map(range).collect(),
                    events: r
                        .events
                        .iter()
                        .map(|e| Event {
                            range: range(&e.range),
                            context: range(&e.context),
                            ..e.clone()
                        })
                        .collect(),
                })
                .collect(),
        }
    }
    pub(super) fn replacements(
        &self,
        eligible: impl Fn(&Range<usize>) -> bool,
    ) -> Vec<(Range<usize>, &'static str)> {
        let mut out = Vec::new();
        for paragraph in self.rows.split(|r| r.blank) {
            let russian = paragraph
                .iter()
                .any(|row| row.russian.iter().any(&eligible));
            let mut double = None;
            let mut single = None;
            for event in paragraph.iter().flat_map(|r| &r.events) {
                let admitted = event.boundary && eligible(&event.range) && eligible(&event.context);
                match event.kind {
                    EventKind::Fixed(glyph) if admitted => out.push((event.range.clone(), glyph)),
                    EventKind::Open(ch) => {
                        let pending = if ch == '"' { &mut double } else { &mut single };
                        *pending = Some((event.range.clone(), admitted));
                    }
                    EventKind::Close(ch) => {
                        let pending = if ch == '"' { &mut double } else { &mut single };
                        if let Some((open, true)) = pending.take() {
                            if admitted {
                                let (left, right) = if ch == '\'' {
                                    ("‘", "’")
                                } else if russian {
                                    ("«", "»")
                                } else {
                                    ("“", "”")
                                };
                                out.push((open, left));
                                out.push((event.range.clone(), right));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        out.sort_by_key(|(range, _)| range.start);
        out
    }
}

/// Production supplies the retained canonical source boundary index.
#[cfg(test)]
pub(super) fn replacements_indexed(
    source: &str,
    eligible: impl Fn(&Range<usize>) -> bool,
    boundary: impl Fn(&Range<usize>) -> bool,
) -> Vec<(Range<usize>, &'static str)> {
    Inventory::new(source, 0, boundary).replacements(eligible)
}

fn lex(source: &str, origin: usize, boundary: &impl Fn(&Range<usize>) -> bool) -> Row {
    let global = |r: Range<usize>| origin + r.start..origin + r.end;
    let mut row = Row {
        range: origin..origin + source.len(),
        blank: source.trim().is_empty(),
        russian: source
            .char_indices()
            .filter(|(_, ch)| {
                ('а'..='я').contains(ch) || ('А'..='Я').contains(ch) || *ch == 'ё' || *ch == 'Ё'
            })
            .map(|(at, ch)| global(at..at + ch.len_utf8()))
            .collect(),
        events: Vec::new(),
    };
    let mut at = 0;
    while at < source.len() {
        let tail = &source[at..];
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
        if !escaped {
            let event = if let Some(glyph) = glyph {
                Some((EventKind::Fixed(glyph), range.clone(), true))
            } else if ch == '"' || ch == '\'' {
                let admitted = boundary(&global(range.clone()));
                let before = source[..at]
                    .graphemes(true)
                    .rev()
                    .find(|cluster| !cluster.chars().all(direction_control))
                    .and_then(|cluster| cluster.chars().next());
                let after = source[at + len..].chars().find(|c| !direction_control(*c));
                let contraction = (ch == '\'')
                    .then(|| contraction_context(source, &(0..source.len()), &range))
                    .flatten();
                if let Some(context) = contraction {
                    Some((EventKind::Fixed("’"), context, admitted))
                } else if before.is_none_or(|c| !c.is_alphanumeric() && c != '"' && c != '\'')
                    && after.is_some_and(|c| !c.is_whitespace())
                {
                    Some((EventKind::Open(ch), range.clone(), admitted))
                } else if before.is_some_and(|c| !c.is_whitespace()) {
                    Some((EventKind::Close(ch), range.clone(), admitted))
                } else {
                    None
                }
            } else {
                None
            };
            if let Some((kind, context, boundary)) = event {
                row.events.push(Event {
                    range: global(range),
                    context: global(context),
                    boundary,
                    kind,
                });
            }
        }
        at += len;
    }
    row
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
    fn cached_lines_preserve_cross_line_quotes_and_paragraph_language() {
        let original =
            "Begin \"Hello\r\nworld\" -- ...\r\nРусский isn't changed.\r\n\r\nEnd \"English\".";
        assert_eq!(
            shown(original, |_| true),
            "Begin «Hello\r\nworld» — …\r\nРусский isn’t changed.\r\n\r\nEnd “English”."
        );
        for replacement in ["English", "Новый", "e\u{301}", "\"next\"", "new\r\nline"] {
            let start = original.find("Русский").unwrap();
            let end = start + "Русский".len();
            let changed = original.replacen("Русский", replacement, 1);
            let old_lines = start..start + original[start..].find('\n').unwrap() + 1;
            let new_end = old_lines.end - (end - start) + replacement.len();
            let bounds = |text: &str| {
                text.grapheme_indices(true)
                    .map(|(at, _)| at)
                    .chain(std::iter::once(text.len()))
                    .collect::<Vec<_>>()
            };
            let old_bounds = bounds(original);
            let new_bounds = bounds(&changed);
            let inventory = Inventory::new(original, 0, |r| {
                old_bounds.contains(&r.start) && old_bounds.contains(&r.end)
            });
            let edit = okilum_core::source_projection::revision::Edit {
                previous: 1,
                old: start..end,
                inserted: replacement.len(),
            };
            let next = inventory.remap(
                old_lines.clone(),
                &changed[old_lines.start..new_end],
                &edit,
                |r| new_bounds.contains(&r.start) && new_bounds.contains(&r.end),
            );
            assert_eq!(
                next.replacements(|_| true),
                replacements(&changed, |_| true),
                "{replacement:?}"
            );
        }
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
    fn quote_pairs_are_admitted_atomically_at_authored_grapheme_boundaries() {
        for quote in ['\'', '"'] {
            for raw in [
                format!("{quote}a{quote}\u{301}"),
                format!("{quote}\u{301}a{quote}"),
                format!("{quote}a{quote}\u{200d}"),
            ] {
                assert!(replacements(&raw, |_| true).is_empty(), "{raw:?}");
                assert_eq!(shown(&raw, |_| true), raw);
            }
            let raw = format!("{quote}a{quote}");
            for excluded in [0..1, 2..3] {
                assert!(replacements(&raw, |r| !(r.start < excluded.end
                    && excluded.start < r.end))
                .is_empty());
            }
            // Positive control: valid pairs following a refused pair still render.
            let raw = format!("{quote}a{quote}\u{301} {quote}ok{quote}");
            let candidates = replacements(&raw, |_| true);
            assert_eq!(candidates.len(), 2);
            let start = raw.find("ok").unwrap() - 1;
            assert_eq!(candidates[0].0, start..start + 1);
            assert_eq!(candidates[1].0, raw.len() - 1..raw.len());
            let boundaries: Vec<_> = raw
                .grapheme_indices(true)
                .map(|(at, _)| at)
                .chain(std::iter::once(raw.len()))
                .collect();
            assert!(candidates
                .iter()
                .all(|(r, _)| boundaries.contains(&r.start) && boundaries.contains(&r.end)));
        }
        assert_eq!(shown("'cafe\u{301}'", |_| true), "‘cafe\u{301}’");
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
