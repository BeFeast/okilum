//! Transient desktop search scope, shared by explicit Agent files and ordinary search.
use okilum_core::{agent_search, quick_open::FileFilter, SearchHit, Searcher};

/// Store in the search palette, independently of Tree's hidden-file setting and
/// the optional Agents executor module. Explicit palette open must call `open`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentSearch {
    filter: FileFilter,
}

impl AgentSearch {
    pub(crate) fn open(&mut self) {
        self.filter = FileFilter::Default;
    }

    /// Returns whether callers must invalidate/reissue the current query.
    pub(crate) fn set_filter(&mut self, filter: FileFilter) -> bool {
        let changed = self.filter != filter;
        self.filter = filter;
        changed
    }

    pub(crate) fn filter(self) -> FileFilter {
        self.filter
    }

    pub(crate) fn label(self) -> &'static str {
        match self.filter {
            FileFilter::Default => "Files",
            FileFilter::AgentFiles => "Agent files",
        }
    }

    /// Run off the UI thread with current canonical inventory paths. Keep the
    /// returned hit path for navigation; duplicate titles are not identities.
    pub(crate) fn search<'a>(
        self,
        searcher: &Searcher,
        inventory: impl IntoIterator<Item = &'a str>,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<SearchHit>> {
        agent_search::search(searcher, inventory, self.filter, query, limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_open_resets_filter_but_query_refresh_does_not() {
        let mut state = AgentSearch::default();
        assert!(state.set_filter(FileFilter::AgentFiles));
        assert!(!state.set_filter(FileFilter::AgentFiles));
        let background_query = state;
        assert_eq!(background_query.filter(), FileFilter::AgentFiles);
        assert_eq!(state.label(), "Agent files");
        state.open();
        assert_eq!(state.filter(), FileFilter::Default);
        assert_eq!(state.label(), "Files");
    }
}
