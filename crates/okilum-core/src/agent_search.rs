//! Reader file-class scoping over canonical vault-relative source identities.
use crate::{quick_open::FileFilter, search::Searcher, SearchHit};

/// Select paths before querying so excluded instructions cannot consume top-N
/// slots or prevent the existing engine's scoped fuzzy fallback. Pass the current
/// inventory (intersected with project scope, if any), never display labels.
pub fn paths<'a>(inventory: impl IntoIterator<Item = &'a str>, filter: FileFilter) -> Vec<String> {
    inventory
        .into_iter()
        .filter(|path| filter.admits(path))
        .map(str::to_owned)
        .collect()
}

/// Keep the existing query language, errors, scores and source-confirmed snippets.
/// This is an explicit desktop scope; it does not change the Reader/MCP protocol.
pub fn search<'a>(
    searcher: &Searcher,
    inventory: impl IntoIterator<Item = &'a str>,
    filter: FileFilter,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<SearchHit>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    searcher.search_in_paths(query, limit, &paths(inventory, filter))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::SearchDocument;

    #[test]
    fn exact_source_scope_precedes_limit_and_preserves_query_errors() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let identities = [
            "a/AGENTS.md",
            "b/AGENTS.md",
            "notes/my-AGENTS.md",
            "notes/agents.md",
        ];
        let documents: Vec<_> = identities
            .iter()
            .map(|path| SearchDocument {
                path: (*path).into(),
                title: "Shared title".into(),
                text: "needle".into(),
            })
            .collect();
        let engine = Searcher::build_documents(&documents, dir.path())?;
        assert_eq!(engine.search("needle", 10)?.len(), 4, "positive control");
        for filter in [FileFilter::Default, FileFilter::AgentFiles] {
            let hits = search(&engine, identities, filter, "needle", 1)?;
            assert_eq!(hits.len(), 1, "excluded hits must not starve the limit");
            assert!(filter.admits(&hits[0].path));
            assert_eq!(hits[0].title, "Shared title");
            assert!(search(&engine, identities, filter, "unknownfield:needle", 1).is_err());
        }
        let hits = search(&engine, identities, FileFilter::AgentFiles, "needle", 10)?;
        let mut actual: Vec<_> = hits.into_iter().map(|hit| hit.path).collect();
        actual.sort();
        assert_eq!(actual, ["a/AGENTS.md", "b/AGENTS.md"]);
        assert!(search(
            &engine,
            ["absent/AGENTS.md"],
            FileFilter::AgentFiles,
            "needle",
            10
        )?
        .is_empty());
        Ok(())
    }
}
