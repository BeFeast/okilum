//! Display titles shared by reader labels and prepared embedded blocks.
use comrak::{nodes::NodeValue, parse_document, Arena, Options};

pub fn from_bytes(source: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(source);
    let body = crate::render::without_frontmatter(&text);
    let arena = Arena::new();
    let document = parse_document(&arena, body, &Options::default());
    for node in document.children() {
        if !matches!(&node.data.borrow().value, NodeValue::Heading(h) if h.level == 1) {
            continue;
        }
        let mut title = String::new();
        for part in node.descendants() {
            match &part.data.borrow().value {
                NodeValue::Text(text) => title.push_str(text),
                NodeValue::Code(code) => title.push_str(&code.literal),
                NodeValue::SoftBreak | NodeValue::LineBreak => title.push(' '),
                _ => {}
            }
        }
        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        if !title.is_empty() {
            return Some(title);
        }
    }
    let yaml = crate::properties::frontmatter_block(&text)?;
    let metadata: serde_yaml::Value = serde_yaml::from_str(yaml).ok()?;
    let title = metadata.get("title")?.as_str()?;
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    (!title.is_empty()).then_some(title)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_body_h1_precedes_frontmatter_and_skips_code_and_quotes() {
        let source = b"---\ntitle: Legacy\n---\n~~~md\n# Not a title\n~~~\n> # Quoted\n\n## Section\n\n# **First** [title](target) &amp; `code` ##\n# Later\n";
        assert_eq!(from_bytes(source).as_deref(), Some("First title & code"));
        assert_eq!(
            from_bytes(b"Setext title\n===\n").as_deref(),
            Some("Setext title")
        );
    }

    #[test]
    fn frontmatter_title_is_a_scalar_string_and_plain_notes_fall_back() {
        for source in [
            "---\ntitle: 'Legacy: title'\n---\nBody",
            "\u{feff}---\r\ntitle: \"Legacy: title\"\r\n---\r\nBody",
        ] {
            assert_eq!(
                from_bytes(source.as_bytes()).as_deref(),
                Some("Legacy: title")
            );
        }
        for source in [
            "Body",
            "---\ntitle: 123\n---\nBody",
            "---\ntitle: []\n---\nBody",
            "---\ntitle: ''\n---\nBody",
        ] {
            assert_eq!(from_bytes(source.as_bytes()), None);
        }
        assert_eq!(
            from_bytes(b"---\ntitle: [broken\n---\n# Valid body\n").as_deref(),
            Some("Valid body")
        );
    }
}
