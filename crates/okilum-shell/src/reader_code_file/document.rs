//! Immutable display snapshots. Parsing and full-file Find run on workers.
//! A pathological minified line uses bounded pages rather than shaping megabytes
//! of glyphs on the UI thread. Copy node and Find still cover the whole file.
use std::{ops::Range, sync::Arc};

mod json_tree;

// Reserved, per-fixture counters prove the real parser ordering. Other tests
// cannot affect them, including concurrently running editor/highlighter work.
#[cfg(test)]
pub(super) static ORDERING_PARSES: [std::sync::atomic::AtomicUsize; 2] = [
    std::sync::atomic::AtomicUsize::new(0),
    std::sync::atomic::AtomicUsize::new(0),
];

const LONG_LINE: usize = 64 * 1024;
const PAGE_BYTES: usize = 32 * 1024;

pub(crate) struct Document {
    pub text: Arc<str>,
    pub pages: Vec<Range<usize>>,
    tree: Option<tree_sitter::Tree>,
    tree_offset: usize,
    pub json_tree: bool,
    pub invalid: Option<JsonError>,
    #[cfg(test)]
    pub parses: bool,
    #[cfg(test)]
    pub node_lookups: std::sync::atomic::AtomicUsize,
    pub foldable: bool,
    pub structure: Vec<StructureRow>,
}

/// Owned parser diagnostics belong to the whole raw snapshot, including
/// documents above the automatic formatting threshold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct JsonError {
    pub line: usize,
    pub column: usize,
    pub message: String,
    pub offset: usize,
}

/// A preorder AST index, with no copied node text or recursive UI elements.
#[derive(Debug)]
pub(crate) struct StructureRow {
    pub range: Range<usize>,
    pub subtree_end: usize,
    pub depth: usize,
    pub line: usize,
    pub parent: Option<usize>,
    label: json_tree::Label,
}

pub(super) fn structural(kind: &str) -> bool {
    matches!(
        kind,
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
    )
}

fn structure(tree: &tree_sitter::Tree) -> Vec<StructureRow> {
    let mut rows: Vec<StructureRow> = Vec::new();
    let mut cursor = tree.walk();
    let mut depth = 0;
    let mut parents: Vec<(usize, usize)> = Vec::new();
    loop {
        let node = cursor.node();
        if structural(node.kind()) {
            let index = rows.len();
            rows.push(StructureRow {
                range: node.byte_range(),
                subtree_end: index + 1,
                depth: parents.len(),
                line: node.start_position().row + 1,
                parent: parents.last().map(|&(_, index)| index),
                label: json_tree::Label::Source,
            });
            parents.push((depth, index));
        }
        if cursor.goto_first_child() {
            depth += 1;
            continue;
        }
        loop {
            if parents.last().is_some_and(|&(at, _)| at == depth) {
                let (_, index) = parents.pop().unwrap();
                rows[index].subtree_end = rows.len();
            }
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return rows;
            }
            depth -= 1;
        }
    }
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
    #[cfg(test)]
    pub fn new(text: String, language: &str) -> Self {
        Self::for_file(text, language, language == "json")
    }

    pub fn for_file(text: String, language: &str, json: bool) -> Self {
        #[cfg(test)]
        for (index, tag) in [
            b"code2809_ordering_large".as_slice(),
            b"code2809_ordering_small".as_slice(),
        ]
        .into_iter()
        .enumerate()
        {
            if text.as_bytes()[..text.len().min(128)]
                .windows(tag.len())
                .any(|part| part == tag)
            {
                ORDERING_PARSES[index].fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let first = first_page(&text);
        let mut pages = vec![first];
        while pages.last().unwrap().end < text.len() {
            let start = pages.last().unwrap().end;
            pages.push(start..page_end(&text, start));
        }
        let tree_offset = if json && text.starts_with('\u{feff}') {
            3
        } else {
            0
        };
        let parsed = &text[tree_offset..];
        let tree = gpui_component::highlighter::LanguageRegistry::singleton()
            .language(language)
            .and_then(|config| config.language)
            .and_then(|language| {
                let mut parser = tree_sitter::Parser::new();
                parser.set_language(&language).ok()?;
                parser.parse(parsed, None)
            });
        let invalid = if json {
            serde_json::from_str::<serde::de::IgnoredAny>(parsed)
                .err()
                .and_then(|error| {
                    // IgnoredAny is iterative, but serde still enforces its default
                    // nesting policy. That is a formatting refusal, not invalid JSON.
                    if error.to_string().starts_with("recursion limit exceeded") {
                        return None;
                    }
                    let line = error.line();
                    let column = error.column();
                    let start = parsed
                        .split_inclusive('\n')
                        .take(line.saturating_sub(1))
                        .map(str::len)
                        .sum::<usize>();
                    let mut offset =
                        (tree_offset + start + column.saturating_sub(1)).min(text.len());
                    while !text.is_char_boundary(offset) {
                        offset -= 1;
                    }
                    let message = error
                        .to_string()
                        .rsplit_once(" at line ")
                        .map_or_else(|| error.to_string(), |(message, _)| message.to_owned());
                    Some(JsonError {
                        line,
                        column,
                        message,
                        offset,
                    })
                })
        } else {
            None
        };
        let parses = tree
            .as_ref()
            .is_some_and(|tree| !tree.root_node().has_error());
        let json_tree = json && invalid.is_none() && parses;
        let structure = if json_tree {
            json_tree::index(tree.as_ref().unwrap(), tree_offset, text.len())
        } else if !json && pages.len() > 1 {
            tree.as_ref().map(structure).unwrap_or_default()
        } else {
            Vec::new()
        };
        let foldable = tree
            .as_ref()
            .filter(|_| text.contains('\n'))
            .is_some_and(|tree| {
                let mut cursor = tree.walk();
                loop {
                    let node = cursor.node();
                    if node.is_named()
                        && node.start_position().row < node.end_position().row
                        && node.parent().is_some()
                    {
                        break true;
                    }
                    if cursor.goto_first_child() {
                        continue;
                    }
                    loop {
                        if cursor.goto_next_sibling() {
                            break;
                        }
                        if !cursor.goto_parent() {
                            return false;
                        }
                    }
                }
            });
        Self {
            text: text.into(),
            pages,
            tree,
            tree_offset,
            json_tree,
            invalid,
            #[cfg(test)]
            parses,
            #[cfg(test)]
            node_lookups: std::sync::atomic::AtomicUsize::new(0),
            foldable,
            structure,
        }
    }

    pub fn node_range(&self, offset: usize) -> Option<Range<usize>> {
        #[cfg(test)]
        self.node_lookups
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tree = self.tree.as_ref()?;
        let offset = offset
            .min(self.text.len().saturating_sub(1))
            .saturating_sub(self.tree_offset);
        let mut node = tree
            .root_node()
            .named_descendant_for_byte_range(offset, offset)?;
        loop {
            if structural(node.kind()) {
                let range = node.byte_range();
                return Some(range.start + self.tree_offset..range.end + self.tree_offset);
            }
            node = node.parent()?;
        }
    }

    pub fn row_at(&self, range: &Range<usize>) -> Option<usize> {
        self.structure
            .iter()
            .rposition(|row| row.range.start <= range.start && row.range.end >= range.end)
    }

    pub fn row_label(&self, index: usize) -> String {
        let row = &self.structure[index];
        row.label.display(&self.text, &row.range)
    }

    pub fn row_hit_label(&self, index: usize, hit: &Range<usize>) -> String {
        let row = &self.structure[index];
        row.label.display_hit(&self.text, &row.range, hit)
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
    fn json2794_whole_file_tree_has_root_keys_and_scalar_array_items() {
        crate::source_presentation::code_languages::install();
        let values = "\"public OpenAPI fixture value\",".repeat(125_000);
        let source = format!("{{\"components\":{{\"schemas\":[{}]}},\"paths\":{{\"deep\":[1,true,null,\"NEEDLE\"]}}}}", values.trim_end_matches(','));
        let doc = Document::new(source.clone(), "json");
        assert!(doc.json_tree && doc.pages.len() > 100);
        assert_eq!(doc.row_label(0), "Object · 2 properties");
        let children: Vec<_> = doc
            .structure
            .iter()
            .enumerate()
            .filter(|(_, row)| row.parent == Some(0))
            .map(|(index, _)| doc.row_label(index))
            .collect();
        assert_eq!(
            children,
            [
                "\"components\": Object · 1 property",
                "\"paths\": Object · 1 property"
            ]
        );
        for label in ["[0] 1", "[1] true", "[2] null", "[3] \"NEEDLE\""] {
            assert!(
                (0..doc.structure.len()).any(|index| doc.row_label(index) == label),
                "{label}"
            );
        }
        let hit = doc.find("NEEDLE").pop().unwrap();
        assert!(hit.start > 3_500_000);
        assert_eq!(doc.row_label(doc.row_at(&hit).unwrap()), "[3] \"NEEDLE\"");
        let pretty = okilum_core::json_view::pretty(&source).unwrap();
        let pretty = Document::new(pretty, "json");
        assert_eq!(
            pretty.pages.len(),
            1,
            "Pretty is independent of long-line pages"
        );
        assert!(pretty.json_tree);
        assert_eq!(pretty.structure.len(), doc.structure.len());
    }

    #[test]
    fn json2794_tree_keeps_exact_bom_copy_ranges_and_rejects_invalid_json() {
        crate::source_presentation::code_languages::install();
        let text = "\u{feff}{\"z\":1.50,\"z\":2e3}";
        let doc = Document::new(text.into(), "json");
        assert!(doc.json_tree);
        assert_eq!(&doc.text[doc.structure[0].range.clone()], text);
        assert_eq!(&doc.text[doc.structure[2].range.clone()], "\"z\":2e3");
        assert_eq!(
            &doc.text[doc.node_range(text.find("2e3").unwrap()).unwrap()],
            "\"z\":2e3"
        );
        let invalid = Document::new(format!("{{\"x\":\"{}\",}}", "x".repeat(100_000)), "json");
        assert!(!invalid.json_tree && invalid.structure.is_empty());
        assert!(invalid.pages.len() > 1);
        // Index construction must remain iterative for deeply nested raw JSON.
        let deep = Document::new(format!("{}0{}", "[".repeat(512), "]".repeat(512)), "json");
        if deep.json_tree {
            assert_eq!(deep.structure.len(), 513);
        }
    }

    #[test]
    fn json2794_match_preview_reveals_a_hit_inside_a_long_unicode_value() {
        crate::source_presentation::code_languages::install();
        let source = format!(
            "{{\"description\":\"{}NEEDLE{}\"}}",
            "é".repeat(100_000),
            "界".repeat(100_000)
        );
        let doc = Document::new(source, "json");
        let hit = doc.find("NEEDLE").pop().unwrap();
        let index = doc.row_at(&hit).unwrap();
        assert!(
            !doc.row_label(index).contains("NEEDLE"),
            "positive control: default preview does not contain the distant hit"
        );
        let label = doc.row_hit_label(index, &hit);
        assert!(label.contains("NEEDLE"));
        assert!(
            label.len() < 500,
            "search preview remains bounded, UTF-8 safe text"
        );
    }

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
