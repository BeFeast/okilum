//! Immutable display snapshots. Parsing and full-file Find run on workers.
//! A pathological minified line uses bounded pages rather than shaping megabytes
//! of glyphs on the UI thread. Copy node and Find still cover the whole file.
use std::{ops::Range, sync::Arc};

const LONG_LINE: usize = 64 * 1024;
const PAGE_BYTES: usize = 32 * 1024;

pub(super) struct Document {
    pub text: Arc<str>,
    pub pages: Vec<Range<usize>>,
    tree: Option<tree_sitter::Tree>,
}

pub(super) fn first_page(text: &str) -> Range<usize> {
    if text.split('\n').any(|line| line.len() > LONG_LINE) {
        0..page_end(text, 0)
    } else {
        0..text.len()
    }
}

fn page_end(text: &str, start: usize) -> usize {
    let mut end = (start + PAGE_BYTES).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

impl Document {
    pub fn new(text: String, language: &str) -> Self {
        let first = first_page(&text);
        let mut pages = vec![first];
        while pages.last().unwrap().end < text.len() {
            let start = pages.last().unwrap().end;
            pages.push(start..page_end(&text, start));
        }
        let tree = gpui_component::highlighter::LanguageRegistry::singleton()
            .language(language)
            .and_then(|config| config.language)
            .and_then(|language| {
                let mut parser = tree_sitter::Parser::new();
                parser.set_language(&language).ok()?;
                parser.parse(&text, None)
            });
        Self {
            text: text.into(),
            pages,
            tree,
        }
    }

    pub fn node_range(&self, offset: usize) -> Option<Range<usize>> {
        let tree = self.tree.as_ref()?;
        let offset = offset.min(self.text.len().saturating_sub(1));
        let mut node = tree
            .root_node()
            .named_descendant_for_byte_range(offset, offset)?;
        loop {
            if matches!(
                node.kind(),
                "object"
                    | "array"
                    | "pair"
                    | "element"
                    | "empty_element"
                    | "table"
                    | "table_array_element"
                    | "block_mapping_pair"
                    | "block_sequence_item"
                    | "flow_mapping"
                    | "flow_sequence"
            ) {
                return Some(node.byte_range());
            }
            node = node.parent()?;
        }
    }

    pub fn page_at(&self, offset: usize) -> usize {
        self.pages
            .partition_point(|page| page.end <= offset)
            .min(self.pages.len() - 1)
    }

    /// Literal Unicode Find, counted over the complete snapshot, never just a page.
    pub fn find(&self, query: &str) -> Vec<Range<usize>> {
        if query.is_empty() {
            return Vec::new();
        }
        let pattern = regex::escape(query);
        regex::RegexBuilder::new(&pattern)
            .case_insensitive(true)
            .build()
            .map(|regex| regex.find_iter(&self.text).map(|m| m.range()).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn data_view_2669_pages_cover_every_utf8_byte_and_find_crosses_page_boundaries() {
        let text = format!(
            "{}Needle{}שלום",
            "界".repeat(PAGE_BYTES / 3 - 1),
            "x".repeat(100_000)
        );
        let doc = Document::new(text.clone(), "text");
        assert!(doc.pages.len() > 1);
        assert_eq!(
            doc.pages
                .iter()
                .map(|r| &doc.text[r.clone()])
                .collect::<String>(),
            text
        );
        assert!(doc.pages.iter().all(|r| r.len() <= PAGE_BYTES));
        let found = doc.find("needle");
        assert_eq!(found.len(), 1);
        assert_eq!(&doc.text[found[0].clone()], "Needle");
        assert!(found[0].start < doc.pages[0].end && found[0].end > doc.pages[0].end);
        assert_eq!(doc.page_at(doc.text.len()), doc.pages.len() - 1);
    }

    #[test]
    fn data_view_2669_copy_node_uses_exact_nested_source_for_every_data_grammar() {
        crate::source_presentation::code_languages::install();
        for (language, text, needle, expected) in [
            (
                "json",
                "{\"outer\": {\"answer\": 42}}",
                "42",
                "\"answer\": 42",
            ),
            (
                "xml",
                "<?xml version=\"1.0\"?><root><a key=\"x\">שלום</a></root>",
                "שלום",
                "<a key=\"x\">שלום</a>",
            ),
            ("toml", "[outer]\nanswer = 42\n", "42", "answer = 42"),
            ("yaml", "outer:\n  answer: 42\n", "42", "answer: 42"),
        ] {
            let doc = Document::new(text.into(), language);
            let tree = doc.tree.as_ref().expect(language);
            assert!(
                !tree.root_node().has_error(),
                "{language}: {}",
                tree.root_node().to_sexp()
            );
            let range = doc.node_range(text.find(needle).unwrap()).expect(language);
            assert_eq!(&doc.text[range], expected, "{language}");
        }
        let doc = Document::new("plain text".into(), "text");
        assert!(doc.node_range(0).is_none());
    }
}
