//! App language configuration shared by Reader and projected Live code.
use gpui_component::highlighter::{LanguageConfig, LanguageRegistry};
use std::sync::Once;

pub(crate) fn install() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let registry = LanguageRegistry::singleton();
        // The pinned component ships the C# grammar with an empty query.
        // Keep its grammar/injection configuration and supply the upstream
        // grammar's query through the existing application registration API.
        if let Some(mut config) = registry.language("csharp") {
            if config.highlights.is_empty() {
                config.highlights =
                    include_str!("../../assets/highlighting/csharp/highlights.scm").into();
                registry.register("csharp", &config);
            }
        }
        registry.register(
            "xml",
            &LanguageConfig::new(
                "xml",
                tree_sitter_xml::LANGUAGE_XML.into(),
                vec![],
                tree_sitter_xml::XML_HIGHLIGHT_QUERY,
                "",
                "",
            ),
        );
        for &(alias, canonical) in okilum_core::code_language::ALIASES {
            if let Some(config) = registry.language(canonical) {
                registry.register(alias, &config);
            }
        }
    });
}
