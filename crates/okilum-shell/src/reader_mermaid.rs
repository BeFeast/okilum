//! Mermaid fences highlight as code in the Reader, Source and Live Preview
//! (S7e, option C). Rendered diagrams wait: merman 0.8 drew 28 of 33 real
//! vault diagrams correctly against the 30 the gate requires.
use gpui_component::highlighter::{LanguageConfig, LanguageRegistry};

/// The language name a ```` ```mermaid ```` fence resolves to.
pub(super) const MERMAID: &str = "mermaid";

/// Register the Mermaid grammar with the shared highlighter registry. Code
/// blocks look languages up by name, and Markdown injects fenced code the same
/// way, so this one registration covers every view. Idempotent.
pub(super) fn register() {
    LanguageRegistry::singleton().register(
        MERMAID,
        &LanguageConfig::new(
            MERMAID,
            tree_sitter::Language::new(tree_sitter_mermaid::LANGUAGE),
            vec![],
            tree_sitter_mermaid::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_mermaid::LOCALS_QUERY,
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mermaid_fences_have_a_grammar_and_highlights() {
        // Control: before registration the language is unknown.
        assert!(LanguageRegistry::singleton()
            .language("mermaid-unregistered")
            .is_none());
        register();
        let config = LanguageRegistry::singleton().language(MERMAID).unwrap();
        let language = config.language.clone().unwrap();
        // The query compiles against this grammar, and a real diagram parses
        // without errors and yields highlight captures.
        let query = tree_sitter::Query::new(&language, &config.highlights).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let source = "flowchart TD\n  A[Start] -->|yes| B{Decision}\n  %% comment\n";
        let tree = parser.parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut captures = std::collections::BTreeSet::new();
        use tree_sitter::StreamingIterator as _;
        let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
        while let Some(m) = matches.next() {
            for capture in m.captures {
                captures.insert(query.capture_names()[capture.index as usize]);
            }
        }
        assert!(captures.contains("keyword"), "{captures:?}");
        assert!(captures.contains("comment"), "{captures:?}");
    }
}
