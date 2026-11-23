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
    let projection = FieldSchema::load(&vault.root)
        .context("Could not load the view field contract in @Schema")
        .map(|schema| Arc::new(Projection::from_snapshot(Arc::new(schema), snapshot)))
        .map_err(|error| format!("{error:#}"));
    let areas = projection
        .as_ref()
        .ok()
        .and_then(|projection| sidebar_view_tree::build(projection, vault, snapshot));
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
    if let Ok(projection) = &mut published.projection {
        if !batch.changed.is_empty() || !batch.removed.is_empty() {
            Arc::make_mut(projection).apply_snapshot(snapshot, batch);
        }
        published.areas = sidebar_view_tree::build(projection, vault, snapshot);
    }
    index
}
