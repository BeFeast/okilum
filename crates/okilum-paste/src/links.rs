//! URLs on the clipboard: link over a selection, titled links and page
//! titles from fetched bytes.

use html5ever::tendril::TendrilSink;
use html5ever::{parse_document, ParseOpts};
use markup5ever_rcdom::{Handle, NodeData, RcDom};

use crate::render::{escape_link_text, link_dest};

/// The clipboard text when it is exactly one http(s), mailto, obsidian or
/// file URL (trimmed); `None` otherwise.
pub fn single_url(text: &str) -> Option<&str> {
    let t = text.trim();
    if t.is_empty() || t.len() > 8192 || t.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let lower = t.get(..t.len().min(12))?.to_ascii_lowercase();
    let ok = if let Some(rest) = ["http://", "https://"]
        .iter()
        .find_map(|p| lower.strip_prefix(p).map(|_| &t[p.len()..]))
    {
        // A host is required.
        rest.chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '[')
    } else if lower.starts_with("mailto:") {
        t[7..].contains('@')
    } else if lower.starts_with("obsidian://") {
        t.len() > 11
    } else if lower.starts_with("file://") {
        t.len() > 7
    } else {
        false
    };
    if !ok || t.contains(['<', '>', '"']) {
        return None;
    }
    Some(t)
}

/// `[selection](url)`.
pub fn link_over_selection(selection: &str, url: &str) -> String {
    let text = escape_link_text(selection.trim());
    format!("[{text}]({})", link_dest(url.trim()))
}

/// `[title](url)`; the bare URL when the title is empty.
pub fn titled_link(title: &str, url: &str) -> String {
    let title = collapse_ws(title);
    if title.is_empty() {
        return url.trim().to_string();
    }
    link_over_selection(&title, url)
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The page title from the first bytes of a response: `og:title`, else
/// `<title>`. The charset comes from the Content-Type header, a BOM, or a
/// `<meta>` in the head; entities are decoded and whitespace collapsed.
pub fn extract_title(head: &[u8], content_type: Option<&str>) -> Option<String> {
    let encoding = content_type
        .and_then(charset_param)
        .and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
        .or_else(|| encoding_rs::Encoding::for_bom(head).map(|(e, _)| e))
        .or_else(|| sniff_meta_charset(head))
        .unwrap_or(encoding_rs::UTF_8);
    // A replacement for UTF-16 labels in <meta> is what browsers do too.
    let encoding = if encoding == encoding_rs::UTF_16LE || encoding == encoding_rs::UTF_16BE {
        if encoding_rs::Encoding::for_bom(head).is_some() {
            encoding
        } else {
            encoding_rs::UTF_8
        }
    } else {
        encoding
    };
    let (text, _, _) = encoding.decode(head);
    // Only the head matters; cut at </head> to keep the parse cheap.
    let lower_end = find_ci(&text, "</head");
    let html = match lower_end {
        Some(end) => &text[..end],
        None => &text[..],
    };
    let dom = parse_document(RcDom::default(), ParseOpts::default()).one(html);
    let mut title: Option<String> = None;
    let mut og: Option<String> = None;
    let mut stack: Vec<Handle> = vec![dom.document.clone()];
    while let Some(node) = stack.pop() {
        if let NodeData::Element { name, attrs, .. } = &node.data {
            match &*name.local {
                "title" if title.is_none() => {
                    let mut s = String::new();
                    for c in node.children.borrow().iter() {
                        if let NodeData::Text { contents } = &c.data {
                            s.push_str(&contents.borrow());
                        }
                    }
                    title = Some(s);
                }
                "meta" if og.is_none() => {
                    let attrs = attrs.borrow();
                    let get = |k: &str| {
                        attrs
                            .iter()
                            .find(|a| &*a.name.local == k)
                            .map(|a| a.value.to_string())
                    };
                    let key = get("property")
                        .or_else(|| get("name"))
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    if key == "og:title" {
                        og = get("content");
                    }
                }
                _ => {}
            }
        }
        stack.extend(node.children.borrow().iter().rev().cloned());
    }
    [og, title]
        .into_iter()
        .flatten()
        .map(|t| collapse_ws(&t.replace('\u{a0}', " ")))
        .find(|t| !t.is_empty())
        .map(|t| t.chars().take(300).collect())
}

fn charset_param(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| v.trim().trim_matches(['"', '\'']).to_string())
    })
}

fn find_ci(text: &str, needle: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    lower.find(needle)
}

/// `<meta charset=x>` or `<meta http-equiv content="...; charset=x">` in the
/// first 4 KB.
fn sniff_meta_charset(head: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    let window = &head[..head.len().min(4096)];
    let lower: Vec<u8> = window.iter().map(u8::to_ascii_lowercase).collect();
    let mut from = 0;
    while let Some(pos) = find_bytes(&lower[from..], b"<meta") {
        let start = from + pos;
        let end = lower[start..]
            .iter()
            .position(|b| *b == b'>')
            .map(|e| start + e)
            .unwrap_or(lower.len());
        let tag = &lower[start..end];
        if let Some(cs) = find_bytes(tag, b"charset") {
            let mut k = cs + 7;
            while k < tag.len() && (tag[k] == b' ' || tag[k] == b'=') {
                k += 1;
            }
            while k < tag.len() && (tag[k] == b'"' || tag[k] == b'\'') {
                k += 1;
            }
            let v_end = tag[k..]
                .iter()
                .position(|b| matches!(b, b'"' | b'\'' | b' ' | b';' | b'/' | b'>'))
                .map(|e| k + e)
                .unwrap_or(tag.len());
            if let Some(enc) = encoding_rs::Encoding::for_label(&tag[k..v_end]) {
                return Some(enc);
            }
        }
        from = end.max(start + 5);
        if from >= lower.len() {
            break;
        }
    }
    None
}

fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(
            single_url("  https://example.com/a?b=1 \n"),
            Some("https://example.com/a?b=1")
        );
        assert_eq!(single_url("mailto:a@b.org"), Some("mailto:a@b.org"));
        assert_eq!(
            single_url("obsidian://open?vault=V&file=N"),
            Some("obsidian://open?vault=V&file=N")
        );
        assert_eq!(
            single_url("file:///Users/o/a.pdf"),
            Some("file:///Users/o/a.pdf")
        );
        assert_eq!(single_url("https://a.com https://b.com"), None);
        assert_eq!(single_url("example.com"), None);
        assert_eq!(single_url("https://"), None);
        assert_eq!(single_url("ftp://x.org"), None);
    }

    #[test]
    fn links() {
        assert_eq!(
            link_over_selection("see [1]", "https://x.org/a b"),
            "[see \\[1\\]](https://x.org/a%20b)"
        );
        assert_eq!(
            link_over_selection("Rust", "https://en.wikipedia.org/wiki/Rust_(language)"),
            "[Rust](https://en.wikipedia.org/wiki/Rust_(language))"
        );
        assert_eq!(
            link_over_selection("x", "https://x.org/a)b"),
            "[x](https://x.org/a%29b)"
        );
        assert_eq!(
            titled_link("  A   *title*\n", "https://x.org"),
            "[A \\*title\\*](https://x.org)"
        );
        assert_eq!(titled_link("  ", "https://x.org"), "https://x.org");
    }
}
