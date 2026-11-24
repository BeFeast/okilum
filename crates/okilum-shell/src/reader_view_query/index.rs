use crate::sidebar_view_tree;
use anyhow::Context as _;
use okilum_core::{
    records::{FieldSchema, Projection},
    vault::warm::{incremental::Batch, Snapshot},
    Changes, Vault,
};
use std::sync::Arc;

pub(crate) type Index = Arc<Published>;

#[derive(Clone)]
pub(crate) struct Published {
    pub projection: Result<Arc<Projection>, String>,
    pub areas: sidebar_view_tree::Published,
}

/// The same accepted Markdown source bank used by the Reader and Now.
pub(crate) fn load(vault: &Vault, snapshot: &Snapshot) -> Index {
    let projection = FieldSchema::load_with_folders(&vault.root, vault.folders())
        .context("Could not load the view field contract in @Schema")
        .map(|schema| Arc::new(Projection::from_snapshot(Arc::new(schema), snapshot)))
        .map_err(|error| format!("{error:#}"));
    let areas = sidebar_view_tree::build(projection.as_deref(), vault, snapshot);
    Arc::new(Published { projection, areas })
}

pub(crate) fn update(
    previous: Option<Index>,
    vault: &Vault,
    snapshot: &Snapshot,
    batch: &Batch,
    changes: &Changes,
) -> Index {
    let Some(mut index) = previous
        .filter(|_| !changes.directories.contains("@Schema") && !changes.directories.contains(""))
    else {
        return load(vault, snapshot);
    };
    let published = Arc::make_mut(&mut index);
    if !batch.changed.is_empty() || !batch.removed.is_empty() {
        if let Ok(projection) = &mut published.projection {
            Arc::make_mut(projection).apply_snapshot(snapshot, batch);
        }
    }
    published.areas = sidebar_view_tree::build(published.projection.as_deref(), vault, snapshot);
    index
}
