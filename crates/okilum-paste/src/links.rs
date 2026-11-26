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
    // Schemes are ASCII: comparing bytes never splits a codepoint, and an
    // ASCII match ends on a char boundary.
    let scheme = |p: &str| {
        t.as_bytes()
            .get(..p.len())
            .is_some_and(|b| b.eq_ignore_ascii_case(p.as_bytes()))
    };
    let ok = if let Some(p) = ["http://", "https://"].into_iter().find(|p| scheme(p)) {
        // A host is required.
        t[p.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '[')
    } else if scheme("mailto:") {
        t[7..].contains('@')
    } else if scheme("obsidian://") {
        t.len() > 11
    } else if scheme("file://") {
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

/// Structured preview metadata extracted from an HTML document's `<head>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageMetadata {
    pub title: Option<String>,
    pub description: Option<String>,
    pub site_name: Option<String>,
    pub image_url: Option<String>,
    pub favicon_url: Option<String>,
}

fn decode_head<'a>(head: &'a [u8], content_type: Option<&str>) -> std::borrow::Cow<'a, str> {
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
    text
}

fn clean_text(raw: Option<String>, max_chars: usize) -> Option<String> {
    let cleaned = collapse_ws(&raw?.replace('\u{a0}', " "));
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.chars().take(max_chars).collect())
    }
}

fn resolve_http_url(raw: &str, base_url: Option<&url::Url>) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 4096 {
        return None;
    }
    let parsed = match base_url {
        Some(base) => base.join(trimmed).ok()?,
        None => url::Url::parse(trimmed).ok()?,
    };
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return None;
    }
    Some(parsed.into())
}

/// Extract Open Graph, Twitter Card, standard HTML title/description, and
/// favicon metadata from the head bytes of an HTML response.
pub fn extract_page_metadata(
    head: &[u8],
    content_type: Option<&str>,
    base_url: Option<&url::Url>,
) -> PageMetadata {
    if head.is_empty() {
        return PageMetadata::default();
    }
    let text = decode_head(head, content_type);
    // Only the head matters; cut at </head> to keep the parse cheap.
    let html = match find_ci(&text, "</head") {
        Some(end) => &text[..end],
        None => &text[..],
    };
    let dom = parse_document(RcDom::default(), ParseOpts::default()).one(html);
    let mut doc_title: Option<String> = None;
    let mut og_title: Option<String> = None;
    let mut twitter_title: Option<String> = None;
    let mut og_description: Option<String> = None;
    let mut twitter_description: Option<String> = None;
    let mut meta_description: Option<String> = None;
    let mut og_site_name: Option<String> = None;
    let mut og_image: Option<String> = None;
    let mut twitter_image: Option<String> = None;
    let mut preferred_icon: Option<String> = None;
    let mut fallback_icon: Option<String> = None;
    let mut apple_icon: Option<String> = None;

    let mut stack: Vec<Handle> = vec![dom.document.clone()];
    while let Some(node) = stack.pop() {
        if let NodeData::Element { name, attrs, .. } = &node.data {
            match &*name.local {
                "title" if doc_title.is_none() => {
                    let mut s = String::new();
                    for c in node.children.borrow().iter() {
                        if let NodeData::Text { contents } = &c.data {
                            s.push_str(&contents.borrow());
                        }
                    }
                    doc_title = Some(s);
                }
                "meta" => {
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
                        .trim()
                        .to_ascii_lowercase();
                    let content = get("content");
                    match key.as_str() {
                        "og:title" if og_title.is_none() => og_title = content,
                        "twitter:title" if twitter_title.is_none() => twitter_title = content,
                        "og:description" if og_description.is_none() => og_description = content,
                        "twitter:description" if twitter_description.is_none() => {
                            twitter_description = content
                        }
                        "description" if meta_description.is_none() => meta_description = content,
                        "og:site_name" if og_site_name.is_none() => og_site_name = content,
                        "og:image" | "og:image:secure_url" | "og:image:url"
                            if og_image.is_none() =>
                        {
                            og_image = content
                        }
                        "twitter:image" | "twitter:image:src" if twitter_image.is_none() => {
                            twitter_image = content
                        }
                        _ => {}
                    }
                }
                "link" => {
                    let attrs = attrs.borrow();
                    let get = |k: &str| {
                        attrs
                            .iter()
                            .find(|a| &*a.name.local == k)
                            .map(|a| a.value.to_string())
                    };
                    if let (Some(rel), Some(href)) = (get("rel"), get("href")) {
                        let rel_lower = rel.to_ascii_lowercase();
                        let tokens: Vec<&str> = rel_lower.split_ascii_whitespace().collect();
                        let mime = get("type").unwrap_or_default().to_ascii_lowercase();
                        let is_raster_or_svg = mime.contains("png")
                            || mime.contains("svg")
                            || mime.contains("webp")
                            || mime.contains("jpeg")
                            || href.to_ascii_lowercase().ends_with(".png")
                            || href.to_ascii_lowercase().ends_with(".svg")
                            || href.to_ascii_lowercase().ends_with(".webp");
                        if tokens.iter().any(|t| *t == "apple-touch-icon") {
                            if apple_icon.is_none() {
                                apple_icon = Some(href);
                            }
                        } else if tokens.iter().any(|t| *t == "icon") {
                            if is_raster_or_svg && preferred_icon.is_none() {
                                preferred_icon = Some(href);
                            } else if fallback_icon.is_none() {
                                fallback_icon = Some(href);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        stack.extend(node.children.borrow().iter().rev().cloned());
    }

    let title = [og_title, twitter_title, doc_title]
        .into_iter()
        .find_map(|candidate| clean_text(candidate, 300));
    let description = [og_description, twitter_description, meta_description]
        .into_iter()
        .find_map(|candidate| clean_text(candidate, 600));
    let site_name = clean_text(og_site_name, 120);
    let image_url = [og_image, twitter_image]
        .into_iter()
        .flatten()
        .find_map(|raw| resolve_http_url(&raw, base_url));
    let favicon_url = [preferred_icon, apple_icon, fallback_icon]
        .into_iter()
        .flatten()
        .find_map(|raw| resolve_http_url(&raw, base_url));

    PageMetadata {
        title,
        description,
        site_name,
        image_url,
        favicon_url,
    }
}

/// The page title from the first bytes of a response: `og:title`, else
/// `<title>`. The charset comes from the Content-Type header, a BOM, or a
/// `<meta>` in the head; entities are decoded and whitespace collapsed.
pub fn extract_title(head: &[u8], content_type: Option<&str>) -> Option<String> {
    let text = decode_head(head, content_type);
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

    /// Byte 12 of each URL falls inside a multi-byte codepoint.
    #[test]
    fn unicode_urls() {
        for url in [
            "https://café.fr/menu",
            "http://пример.рф",
            "https://日本語.jp",
            "mailto:andrü@x.de",
        ] {
            assert_eq!(single_url(url), Some(url), "{url}");
        }
        assert_eq!(
            single_url("HTTPS://Example.com"),
            Some("HTTPS://Example.com")
        );
        assert_eq!(single_url("MailTo:a@b.org"), Some("MailTo:a@b.org"));
        // Rejections stay: invalid scheme, empty host, whitespace or
        // control characters, overlong input.
        assert_eq!(single_url("ftp://café.fr"), None);
        assert_eq!(single_url("httpé://x.org"), None);
        assert_eq!(single_url("пример.рф"), None);
        assert_eq!(single_url("http://"), None);
        assert_eq!(single_url("https:///пример"), None);
        assert_eq!(single_url("mailto:andrü"), None);
        assert_eq!(single_url("http://пример.рф/a b"), None);
        assert_eq!(single_url("http://пример.рф/\u{7}x"), None);
        assert_eq!(single_url("http://пример.рф/\u{85}x"), None);
        let long = format!("https://café.fr/{}", "é".repeat(4096));
        assert!(long.len() > 8192);
        assert_eq!(single_url(&long), None);
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

    #[test]
    fn page_metadata_extracts_og_twitter_fallback_and_resolves_urls() {
        let base = url::Url::parse("https://docs.example.org/guides/start.html").unwrap();
        let html = br#"<!doctype html>
        <html>
        <head>
            <title>Fallback Title</title>
            <meta property="og:title" content="  Rich &amp; Fast&#x2014;Preview  ">
            <meta property="og:description" content="  Local-first   knowledge base. ">
            <meta property="og:site_name" content="Example Docs">
            <meta property="og:image" content="/assets/card.png">
            <link rel="icon" type="image/png" href="../icons/site.png">
        </head>
        <body><p>Body</p></body>
        </html>"#;
        let meta = extract_page_metadata(html, Some("text/html; charset=utf-8"), Some(&base));
        assert_eq!(meta.title.as_deref(), Some("Rich & Fast\u{2014}Preview"));
        assert_eq!(
            meta.description.as_deref(),
            Some("Local-first knowledge base.")
        );
        assert_eq!(meta.site_name.as_deref(), Some("Example Docs"));
        assert_eq!(
            meta.image_url.as_deref(),
            Some("https://docs.example.org/assets/card.png")
        );
        assert_eq!(
            meta.favicon_url.as_deref(),
            Some("https://docs.example.org/icons/site.png")
        );
    }

    #[test]
    fn page_metadata_falls_back_to_twitter_and_rejects_non_http_images() {
        let base = url::Url::parse("https://example.org/post").unwrap();
        let html = br#"<html><head>
            <title>HTML Title</title>
            <meta name="description" content="Plain meta description">
            <meta property="og:image" content="javascript:alert(1)">
            <meta name="twitter:image" content="//cdn.example.org/thumb.webp">
            <link rel="shortcut icon" href="file:///etc/passwd">
            <link rel="apple-touch-icon" href="/apple-icon.png">
        </head></html>"#;
        let meta = extract_page_metadata(html, None, Some(&base));
        assert_eq!(meta.title.as_deref(), Some("HTML Title"));
        assert_eq!(meta.description.as_deref(), Some("Plain meta description"));
        assert_eq!(
            meta.image_url.as_deref(),
            Some("https://cdn.example.org/thumb.webp")
        );
        assert_eq!(
            meta.favicon_url.as_deref(),
            Some("https://example.org/apple-icon.png")
        );
    }

    #[test]
    fn page_metadata_handles_malformed_and_empty_html() {
        assert_eq!(
            extract_page_metadata(b"", None, None),
            PageMetadata::default()
        );
        let malformed =
            b"<head><title>Broken <b>Tag</title><meta name=description content='Still works";
        let meta = extract_page_metadata(malformed, None, None);
        assert_eq!(meta.title.as_deref(), Some("Broken <b>Tag"));
    }
}
