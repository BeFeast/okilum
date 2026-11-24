//! Vault naming conventions. These classify data, never service paths.
use crate::vault_descriptor::{self, Layout, LayoutState};
use std::{
    path::Path,
    sync::{Arc, OnceLock},
};

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct Folders {
    state: LayoutState,
}

impl PartialEq for Folders {
    fn eq(&self, other: &Self) -> bool {
        match (&self.state, &other.state) {
            (LayoutState::Managed(a), LayoutState::Managed(b)) => {
                a.roots == b.roots && a.domains == b.domains
            }
            (LayoutState::Legacy, LayoutState::Legacy)
            | (LayoutState::Unsupported, LayoutState::Unsupported) => true,
            _ => false,
        }
    }
}

impl Eq for Folders {}

#[derive(serde::Deserialize)]
pub(crate) struct LegacyFolders {
    markdown_assets: String,
    archived: Vec<String>,
    buckets: Vec<String>,
    pub projects: String,
    pub image_attachments: String,
    pub file_attachments: String,
    pub domains: Vec<String>,
}

pub(crate) fn legacy() -> &'static LegacyFolders {
    static LEGACY: OnceLock<LegacyFolders> = OnceLock::new();
    LEGACY.get_or_init(|| {
        serde_json::from_str(include_str!("../config/legacy-vault-folders.json"))
            .expect("valid shipped layout fallback")
    })
}

pub const LEGACY_DAILY_FOLDER: &str = include_str!("../config/daily-notes-folder");

pub fn defaults() -> &'static Arc<Folders> {
    static DEFAULTS: OnceLock<Arc<Folders>> = OnceLock::new();
    DEFAULTS.get_or_init(Arc::default)
}

impl Folders {
    pub fn load(root: &Path) -> Arc<Self> {
        let state = LayoutState::load(root);
        if matches!(state, LayoutState::Legacy) {
            defaults().clone()
        } else {
            Arc::new(Self { state })
        }
    }

    pub fn layout(&self) -> Option<&Layout> {
        match &self.state {
            LayoutState::Managed(layout) => Some(layout),
            _ => None,
        }
    }

    pub fn is_legacy(&self) -> bool {
        matches!(self.state, LayoutState::Legacy)
    }

    pub fn layout_unavailable(&self) -> bool {
        matches!(self.state, LayoutState::Unsupported)
    }

    pub fn asset_content(&self, path: &str) -> bool {
        if let Some(layout) = self.layout() {
            return vault_descriptor::suffix(path, &layout.roots.sources)
                .and_then(|relative| relative.split('/').next())
                == Some("files")
                && crate::excalidraw::is_drawing(path);
        }
        self.is_legacy() && vault_descriptor::suffix(path, &legacy().markdown_assets).is_some()
    }

    pub fn markdown_asset(&self, path: &str) -> bool {
        self.asset_content(path)
            && Path::new(path)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    }

    pub fn archived(&self, path: &str) -> bool {
        if let Some(layout) = self.layout() {
            return layout
                .context(path)
                .is_some_and(|context| context.is_final());
        }
        self.is_legacy()
            && path.split('/').any(|segment| {
                let name = segment
                    .trim_start_matches(|c: char| {
                        c.is_ascii_digit() || matches!(c, '.' | '-' | ' ')
                    })
                    .to_lowercase();
                legacy()
                    .archived
                    .iter()
                    .any(|folder| folder.to_lowercase() == name)
            })
    }

    pub fn views_root(&self) -> Option<&str> {
        match &self.state {
            LayoutState::Managed(layout) => Some(&layout.roots.views),
            LayoutState::Legacy => Some("@views"),
            LayoutState::Unsupported => None,
        }
    }

    pub fn view_source(&self, path: &str) -> bool {
        self.views_root()
            .and_then(|root| vault_descriptor::suffix(path, root))
            .is_some_and(|relative| !relative.starts_with("out/"))
    }

    pub fn project_contains(&self, project: &str, path: &str) -> bool {
        if self.layout().is_some_and(|layout| {
            layout
                .context(project)
                .zip(layout.context(path))
                .is_some_and(|(project, path)| project.id == path.id)
        }) {
            return true;
        }
        project.is_empty()
            || vault_descriptor::suffix(path, project.trim_end_matches('/')).is_some()
    }

    pub fn legacy_attachment_folder(&self, image: bool) -> &'static str {
        let legacy = legacy();
        if image {
            &legacy.image_attachments
        } else {
            &legacy.file_attachments
        }
    }

    pub(crate) fn legacy_bucket(&self, name: &str) -> bool {
        self.is_legacy()
            && legacy()
                .buckets
                .iter()
                .any(|folder| folder.eq_ignore_ascii_case(name))
    }
}

impl LegacyFolders {
    pub(crate) fn bucket_exact(&self, name: &str) -> bool {
        self.buckets.iter().any(|folder| folder == name)
    }

    pub(crate) fn archive_bucket_exact(&self, name: &str) -> bool {
        self.archived.first().is_some_and(|folder| folder == name)
    }
}

pub fn obsidian_attachment_setting(root: &Path) -> Option<String> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct App {
        attachment_folder_path: String,
    }
    let settings = std::fs::read(root.join(".obsidian/app.json")).ok()?;
    serde_json::from_slice::<App>(&settings)
        .ok()
        .map(|app| app.attachment_folder_path)
}

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

/// Classification for source-only callers without a vault to configure it.
pub(crate) fn asset_content(path: &str) -> bool {
    defaults().asset_content(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_change_invalidates_search_generation_and_preserves_the_source() {
        use crate::vault::warm;
        let root = tempfile::tempdir().unwrap();
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/lifecycle-vault");
        std::fs::create_dir(root.path().join(vault_descriptor::DIRECTORY)).unwrap();
        std::fs::create_dir(root.path().join("@Assets")).unwrap();
        std::fs::write(root.path().join("@Assets/Note.md"), "# Contents").unwrap();

        let (_, mut snapshot, _) =
            warm::reconcile(root.path(), None, false, &mut |_, _| Ok(())).unwrap();
        assert!(snapshot.source("@Assets/Note.md").is_some());
        assert!(!snapshot.is_note_source("@Assets/Note.md"));
        snapshot.search_generation = Some("a".repeat(64));
        std::fs::copy(
            vault_descriptor::path(&fixture),
            vault_descriptor::path(root.path()),
        )
        .unwrap();

        let (vault, rescanned, stats) =
            warm::reconcile(root.path(), Some(&snapshot), false, &mut |_, _| Ok(())).unwrap();
        assert!(!stats.graph_reused);
        assert!(rescanned.search_generation.is_none());
        assert!(rescanned.is_note_source("@Assets/Note.md"));
        assert_eq!(rescanned.vault().folders(), vault.folders());
    }

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
