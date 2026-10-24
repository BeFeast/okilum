//! URLs on the clipboard and their titles (#1611): Paste as Titled Link and
//! a selection turned into a link. Platform-neutral and pure; the network
//! fetch lives with the background jobs. URL detection and the page title
//! parser are the converter crate's ([`okilum_paste::single_url`],
//! [`okilum_paste::extract_title`]).

use super::offer::decode_entities;

/// Bytes of a page read for its title.
pub const TITLE_READ_LIMIT: usize = 256 * 1024;

/// The URL when `text` is exactly one http(s), mailto, obsidian or file URL
/// (surrounding whitespace aside).
pub fn single_url(text: &str) -> Option<String> {
    okilum_paste::single_url(text).map(str::to_owned)
}

/// `[selection](url)` over a selection of the note's Markdown source. The
/// selection is already Markdown (`**bold**` stays bold), so it is kept as it
/// is; only one with unbalanced brackets, which would break the link, is
/// escaped as plain text by the converter.
pub fn selection_link(selected: &str, url: &str) -> String {
    let mut depth = 0i32;
    let balanced = selected.chars().all(|c| {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            _ => {}
        }
        depth >= 0
    }) && depth == 0;
    if balanced && !selected.contains(['\n', '\r']) {
        format!("[{}]({})", selected.trim(), destination(url))
    } else {
        okilum_paste::link_over_selection(selected, url)
    }
}

/// A link destination: an awkward URL (spaces, unbalanced parentheses) in
/// angle brackets.
fn destination(url: &str) -> String {
    let opens = url.matches('(').count();
    let closes = url.matches(')').count();
    if url.contains(' ') || url.contains('<') || opens != closes {
        format!("<{url}>")
    } else {
        url.to_string()
    }
}

/// Whether a fetched title can be looked up for this URL (no network for
/// mail, vault or file links).
pub fn fetchable(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// `[text](url)` with the text's brackets escaped and an awkward URL (spaces,
/// parentheses) in angle brackets.
pub fn markdown_link(text: &str, url: &str) -> String {
    let text = text
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace(['\n', '\r'], " ");
    format!("[{text}]({})", destination(url))
}

/// The visible text of a copied link: an HTML fragment that is a single
/// `<a href>` to `url` (a browser's Copy Link Address of a link, a chat
/// app's copied link).
pub fn anchor_text(html: &str, url: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<a ")?;
    if lower[start + 3..].contains("<a ") {
        return None;
    }
    let open_end = start + lower[start..].find('>')?;
    let close = open_end + lower[open_end..].find("</a>")?;
    let tag = &html[start..open_end];
    let href = attribute(tag, "href")?;
    if decode_entities(&href).trim_end_matches('/') != url.trim_end_matches('/') {
        return None;
    }
    let text = super::offer::html_to_text(&html[open_end + 1..close]);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty() && text != url).then_some(text)
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower[from..].find(name) {
        let at = from + found;
        from = at + name.len();
        let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        let rest = lower[at + name.len()..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let value_start = tag.len() - rest.len() + 1;
        let value = tag[value_start..].trim_start();
        return Some(match value.chars().next()? {
            quote @ ('"' | '\'') => value[1..].split(quote).next()?.to_string(),
            _ => value
                .split(|c: char| c.is_whitespace() || c == '>')
                .next()?
                .to_string(),
        });
    }
    None
}

/// Whether a URL that ends right before `rest` still ends there: `rest` is
/// empty or starts with something that cannot continue a URL (#1611: a bare
/// URL the user extended is not replaced by a link to the shorter one).
pub fn url_ends_before(rest: &str) -> bool {
    let mut chars = rest.chars();
    match chars.next() {
        None => true,
        // Sentence punctuation after a URL, not part of it.
        Some('.' | ',' | ';' | ':' | '!' | '?') => chars
            .next()
            .is_none_or(|c| c.is_whitespace() || matches!(c, ')' | ']' | '>' | '"' | '\'')),
        Some(c) => !(c.is_alphanumeric() || "/?#&=_%~+@-$*'".contains(c)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_urls_only() {
        assert_eq!(
            single_url("  https://example.com/a?b=1\n").as_deref(),
            Some("https://example.com/a?b=1")
        );
        assert!(single_url("mailto:a@b.c").is_some());
        assert!(single_url("obsidian://open?vault=x").is_some());
        assert!(single_url("file:///Users/o/a.txt").is_some());
        assert!(single_url("see https://example.com").is_none());
        assert!(single_url("https://a.com\nhttps://b.com").is_none());
        assert!(single_url("ftp://example.com").is_none());
        assert!(single_url("just words").is_none());
        assert!(single_url("").is_none());
    }

    #[test]
    fn markdown_links_escape_text_and_wrap_awkward_urls() {
        assert_eq!(
            markdown_link("A [b] c", "https://x.org/p"),
            "[A \\[b\\] c](https://x.org/p)"
        );
        assert_eq!(
            markdown_link("t", "https://en.wikipedia.org/wiki/Rust_(language)"),
            "[t](https://en.wikipedia.org/wiki/Rust_(language))"
        );
        assert_eq!(
            markdown_link("t", "https://x.org/a)b"),
            "[t](<https://x.org/a)b>)"
        );
    }

    #[test]
    fn anchor_text_names_a_copied_link() {
        let html = "<meta charset='utf-8'><a href=\"https://x.org/p\">The &amp; Page</a>";
        assert_eq!(
            anchor_text(html, "https://x.org/p").as_deref(),
            Some("The & Page")
        );
        assert_eq!(anchor_text(html, "https://other.org"), None);
        let two = "<a href='https://x.org/p'>a</a> <a href='https://x.org/q'>b</a>";
        assert_eq!(anchor_text(two, "https://x.org/p"), None);
    }

    #[test]
    fn a_selection_link_keeps_the_markdown_it_wraps() {
        assert_eq!(
            selection_link("**bold** word", "https://x.org/p"),
            "[**bold** word](https://x.org/p)"
        );
        assert_eq!(
            selection_link("a]b", "https://x.org/p"),
            "[a\\]b](https://x.org/p)"
        );
        assert_eq!(
            selection_link("t", "https://x.org/a b"),
            "[t](<https://x.org/a b>)"
        );
    }

    #[test]
    fn an_extended_url_does_not_end_at_the_old_one() {
        assert!(url_ends_before(""));
        assert!(url_ends_before(" and more"));
        assert!(url_ends_before(". Next"));
        assert!(url_ends_before(")"));
        assert!(!url_ends_before("/docs"));
        assert!(!url_ends_before("?q=1"));
        assert!(!url_ends_before(".html"));
    }
}
