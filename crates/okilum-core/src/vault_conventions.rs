//! Vault naming conventions. These classify data, never service paths.
use std::path::Path;

pub fn hidden(path: &str) -> bool {
    path.split('/').any(|part| part.starts_with(['@', '.']))
}

pub fn index_name(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.eq_ignore_ascii_case("@index.md")
}

/// Preserve ambiguity between case spellings of the @ index.
pub fn index_candidates<'a>(paths: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    paths.filter(|path| index_name(path)).collect()
}

/// Only the root @Assets tree has the Markdown-as-asset convention.
/// Ordinary @ notes and every underscore path remain notes.
pub fn asset_content(path: &str) -> bool {
    path.starts_with("@Assets/")
}

/// Markdown assets never become note-list results, even through persisted history.
pub fn markdown_asset(path: &str) -> bool {
    asset_content(path)
        && Path::new(path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue2847_underscore_paths_are_ordinary_and_only_at_is_an_index() {
        for path in [
            "_Private/x.md",
            "_export/data.json",
            "Legacy/_index.md",
            "_Assets/Templates/Old.md",
            "_AgentContract.md",
            "_Schema/language.yaml",
        ] {
            assert!(!hidden(path), "ordinary: {path}");
            assert!(!index_name(path));
            assert!(!asset_content(path));
        }
        for path in [
            "@Private/x.md",
            "Nested/@Schema/Templates/Note.md",
            ".state/data.json",
            "Nested/.hidden.md",
        ] {
            assert!(hidden(path), "special: {path}");
        }
        assert_eq!(
            index_candidates(["_index.md", "_Index.md", "@INDEX.md"].into_iter()),
            ["@INDEX.md"]
        );
        assert_eq!(
            index_candidates(["_index.md", "@index.md", "@Index.md"].into_iter()),
            ["@index.md", "@Index.md"]
        );
        assert!(index_candidates(["_index.md", "_Index.md"].into_iter()).is_empty());
    }
}
