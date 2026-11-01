//! Reader integration model for agent imports. No editing or execution actions.
use okilum_core::agent_imports::{self, Expansion, Failure, Import, Limits, Source};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportLink {
    pub source: PathBuf,
    pub import: Import,
    pub destination: Result<PathBuf, Failure>,
}

/// Call only for a classified agent file. The supplied source is the displayed
/// snapshot; offsets must never be applied to a different revision.
pub fn links(store: &impl Source, root: &Path, source: &Path, snapshot: &str) -> Vec<ImportLink> {
    agent_imports::parse(snapshot)
        .into_iter()
        .map(|import| {
            let destination = agent_imports::resolve(store, root, source, &import.target);
            ImportLink {
                source: source.into(),
                import,
                destination,
            }
        })
        .collect()
}

/// Explicit read-only expansion action. The reader renders each child's original
/// text with `Imported::path` as its relative-link context and keeps errors visible.
/// This returns a separate tree, never replacement Markdown for the canonical note.
pub fn expand(
    store: &impl Source,
    root: &Path,
    source: &Path,
    snapshot: &str,
    limits: Limits,
) -> Result<Vec<Expansion>, Failure> {
    agent_imports::expand(store, root, source, snapshot, limits)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Mock;
    impl Source for Mock {
        fn canonicalize(&self, path: &Path) -> Result<PathBuf, Failure> {
            Ok(path.into())
        }
        fn read(&self, path: &Path, _: usize) -> Result<String, Failure> {
            assert_eq!(path, Path::new("/vault/sub/AGENTS.md"));
            Ok("Read-only imported source".into())
        }
    }
    #[test]
    fn link_and_expansion_keep_the_displayed_source_coordinates() {
        let source = Path::new("/vault/sub/CLAUDE.md");
        let snapshot = "@AGENTS.md\r\n";
        let links = links(&Mock, Path::new("/vault"), source, snapshot);
        assert_eq!(links[0].destination, Ok("/vault/sub/AGENTS.md".into()));
        assert_eq!(&snapshot[links[0].import.target_bytes.clone()], "AGENTS.md");
        let tree = expand(
            &Mock,
            Path::new("/vault"),
            source,
            snapshot,
            Limits::default(),
        )
        .unwrap();
        assert_eq!(tree[0].import, links[0].import);
        assert_eq!(
            tree[0].result.as_ref().unwrap().text,
            "Read-only imported source"
        );
    }
}
