//! Source context for the Remind me action (#724): the sentence around a
//! selected date and the heading it sits under. Pure and IO-free.
//!
//! The selection comes from rendered text, so it is matched against the
//! canonical source only when it identifies exactly one place. A selection that
//! occurs twice, or not at all, yields no context rather than a guessed one;
//! the caller then falls back to the note itself.
use regex::Regex;
use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Located {
    /// Plain-text sentence containing the selection, whitespace-collapsed.
    pub sentence: Option<String>,
    /// Nearest preceding ATX heading, only when it is unique in the note and
    /// has no inline markup that would make a `#heading` link unresolvable.
    pub heading: Option<String>,
}

const MAX_SENTENCE_CHARS: usize = 200;

struct Block {
    text: String,
    heading: Option<String>,
}

static LIST_MARKER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:[-*+]|\d{1,9}[.)])\s+(?:\[[ xX]\]\s+)?").unwrap());
static ATX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^#{1,6}\s+(.*?)(?:\s+#+)?\s*$").unwrap());
static WIKI_ALIAS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!?\[\[[^\]|]*\|([^\]]*)\]\]").unwrap());
static WIKI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"!?\[\[([^\]]*)\]\]").unwrap());
static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"!?\[([^\]]*)\]\([^)]*\)").unwrap());
static TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>\n]*>").unwrap());
static BLOCK_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s\^[A-Za-z0-9-]+$").unwrap());

pub fn locate(source: &str, selection: &str) -> Located {
    let needle = selection.split_whitespace().collect::<Vec<_>>().join(" ");
    if needle.is_empty() {
        return Located::default();
    }
    let (blocks, headings) = blocks(source);
    let mut hits = blocks.iter().flat_map(|block| {
        block
            .text
            .match_indices(&needle)
            .map(move |(at, _)| (block, at))
    });
    let (Some((block, at)), None) = (hits.next(), hits.next()) else {
        return Located::default();
    };
    Located {
        sentence: sentence(&block.text, at, needle.len()),
        heading: block
            .heading
            .clone()
            .filter(|h| headings.get(h) == Some(&1) && plain_heading(h)),
    }
}

fn blocks(source: &str) -> (Vec<Block>, HashMap<String, usize>) {
    let mut lines = source.lines().peekable();
    if lines.peek().is_some_and(|l| l.trim_end() == "---") {
        lines.next();
        for line in lines.by_ref() {
            if matches!(line.trim_end(), "---" | "...") {
                break;
            }
        }
    }
    let (mut out, mut headings) = (Vec::new(), HashMap::new());
    let (mut current, mut heading, mut fence) = (String::new(), None::<String>, false);
    let mut quote_depth = 0;
    let flush = |current: &mut String, heading: &Option<String>, out: &mut Vec<Block>| {
        let text = current.split_whitespace().collect::<Vec<_>>().join(" ");
        current.clear();
        if !text.is_empty() {
            out.push(Block {
                text,
                heading: heading.clone(),
            });
        }
    };
    for line in lines {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            flush(&mut current, &heading, &mut out);
            fence = !fence;
            continue;
        }
        if fence {
            continue;
        }
        let (mut body, mut depth) = (trimmed, 0);
        while let Some(rest) = body.strip_prefix('>') {
            body = rest.trim_start();
            depth += 1;
        }
        // A quote starts or ends a block even without a blank line.
        if depth != quote_depth {
            flush(&mut current, &heading, &mut out);
            quote_depth = depth;
        }
        if body.is_empty() {
            flush(&mut current, &heading, &mut out);
        } else if let Some(h) = ATX.captures(body).filter(|_| trimmed == body) {
            flush(&mut current, &heading, &mut out);
            let text = h[1].trim().to_owned();
            *headings.entry(text.clone()).or_insert(0) += 1;
            heading = Some(text.clone()).filter(|t| !t.is_empty());
            current = text;
            flush(&mut current, &heading, &mut out);
        } else if let Some(marker) = LIST_MARKER.find(body) {
            flush(&mut current, &heading, &mut out);
            current.push_str(&body[marker.end()..]);
        } else {
            current.push(' ');
            current.push_str(body);
        }
    }
    flush(&mut current, &heading, &mut out);
    (out, headings)
}

fn plain_heading(heading: &str) -> bool {
    !heading.is_empty()
        && !heading.contains([
            '*', '_', '`', '~', '=', '<', '>', '[', ']', '|', '\\', '#', '^', '%', '!',
        ])
}

/// The sentence of `text` containing the match at `at..at + len`.
fn sentence(text: &str, at: usize, len: usize) -> Option<String> {
    let end_of = |from: usize| {
        let mut chars = text[from..].char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if matches!(c, '.' | '!' | '?' | '…' | '。' | '؟')
                && chars.peek().is_none_or(|(_, n)| n.is_whitespace())
            {
                return from + i + c.len_utf8();
            }
        }
        text.len()
    };
    let mut start = 0;
    loop {
        let end = end_of(start);
        if end > at + len - 1 || end >= text.len() {
            let raw = text[start..end].trim();
            return clean(raw).filter(|s| !s.is_empty());
        }
        start = end;
    }
}

fn clean(raw: &str) -> Option<String> {
    let text = WIKI_ALIAS.replace_all(raw, "$1");
    let text = WIKI.replace_all(&text, "$1");
    let text = LINK.replace_all(&text, "$1");
    let text = TAGS.replace_all(&text, "");
    let text = BLOCK_ID.replace(&text, "");
    let text = text
        .replace("**", "")
        .replace("__", "")
        .replace("~~", "")
        .replace("==", "")
        .replace('`', "");
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= MAX_SENTENCE_CHARS {
        return Some(text);
    }
    // Cut on a word boundary; the reminder links back to the full sentence.
    let cut: String = text.chars().take(MAX_SENTENCE_CHARS).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head);
    Some(format!("{}…", cut.trim_end_matches([',', ';', ':', ' ']))).filter(|s| s.len() > 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = "---\ntitle: DST\n---\n# Calendar\n\nIntro text.\n\n## Winter time\n\nClocks go back on **1 ноября**. Do it early.\nNext line of the same paragraph.\n\n- Call [[People/Dana|Dana]] before 2026-12-01.\n\n```\n01.11 in code\n```\n\n## Summer time\n\nIt starts in spring.\n";

    #[test]
    fn finds_the_sentence_and_unique_heading() {
        // Wiki links are reduced to their visible alias.
        let found = locate(NOTE, "2026-12-01");
        assert_eq!(
            found.sentence.as_deref(),
            Some("Call Dana before 2026-12-01.")
        );
        assert_eq!(found.heading.as_deref(), Some("Winter time"));
    }

    #[test]
    fn emphasis_around_the_selection_is_dropped_from_the_sentence() {
        let found = locate(NOTE, "1 ноября");
        assert_eq!(
            found.sentence.as_deref(),
            Some("Clocks go back on 1 ноября.")
        );
        assert_eq!(found.heading.as_deref(), Some("Winter time"));
    }

    #[test]
    fn ambiguous_missing_and_code_only_selections_have_no_context() {
        let twice = "# A\n\nPay on 1 Nov.\n\n# B\n\nCall on 1 Nov.\n";
        assert_eq!(locate(twice, "1 Nov"), Located::default());
        assert_eq!(locate(NOTE, "2027-01-01"), Located::default());
        assert_eq!(locate(NOTE, "01.11"), Located::default());
        assert_eq!(locate(NOTE, "   "), Located::default());
        // Frontmatter is not note text.
        assert_eq!(locate(NOTE, "DST"), Located::default());
    }

    #[test]
    fn duplicate_or_decorated_headings_fall_back_to_the_note() {
        let source = "## Plan\n\nA on 2026-11-01.\n\n## Plan\n\nB later.\n\n## **Bold** head\n\nC on 2026-12-05.\n";
        let a = locate(source, "2026-11-01");
        assert_eq!(a.sentence.as_deref(), Some("A on 2026-11-01."));
        assert_eq!(a.heading, None);
        let c = locate(source, "2026-12-05");
        assert_eq!(c.sentence.as_deref(), Some("C on 2026-12-05."));
        assert_eq!(c.heading, None);
    }

    #[test]
    fn selection_spanning_a_soft_line_break_and_unicode_sentences() {
        let source = "# Заметка\n\nПервое предложение. Перевод часов\n1 ноября в Израиле!\nИ ещё.\n\nבשבוע הבא 05.11 נפגשים.\n";
        let ru = locate(source, "1 ноября");
        assert_eq!(
            ru.sentence.as_deref(),
            Some("Перевод часов 1 ноября в Израиле!")
        );
        assert_eq!(ru.heading.as_deref(), Some("Заметка"));
        let he = locate(source, "05.11");
        assert_eq!(he.sentence.as_deref(), Some("בשבוע הבא 05.11 נפגשים."));
    }

    #[test]
    fn a_date_with_dots_does_not_split_the_sentence_and_long_text_is_cut() {
        let dotted = locate("Send it by 01.11.2026 please.\n", "01.11.2026");
        assert_eq!(
            dotted.sentence.as_deref(),
            Some("Send it by 01.11.2026 please.")
        );
        let long = format!("{} on 2026-11-01.\n", "word ".repeat(80).trim_end());
        let cut = locate(&long, "2026-11-01").sentence.unwrap();
        assert!(cut.ends_with('…') && cut.chars().count() <= MAX_SENTENCE_CHARS + 1);
    }

    #[test]
    fn list_items_are_separate_sentences_and_quotes_are_unwrapped() {
        let source = "- first 2026-11-01 item\n- second item\n> quoted 2026-12-02 line\n";
        assert_eq!(
            locate(source, "2026-11-01").sentence.as_deref(),
            Some("first 2026-11-01 item")
        );
        assert_eq!(
            locate(source, "2026-12-02").sentence.as_deref(),
            Some("quoted 2026-12-02 line")
        );
    }
}
