use anyhow::Context as _;
use okilum_core::{
    records::{FieldSchema, Projection},
    vault::warm::{incremental::Batch, Snapshot},
    Changes,
};
use std::{path::Path, sync::Arc};

pub(crate) type Index = Arc<Result<Projection, String>>;

/// The same accepted Markdown source bank used by the Reader and Now.
pub(crate) fn load(root: &Path, snapshot: &Snapshot) -> Index {
    Arc::new(
        FieldSchema::load(root)
            .context("Could not load the view field contract in @Schema")
            .map(|schema| Projection::from_snapshot(Arc::new(schema), snapshot))
            .map_err(|error| format!("{error:#}")),
    )
}

pub(crate) fn update(
    previous: Option<Index>,
    root: &Path,
    snapshot: &Snapshot,
    batch: &Batch,
    changes: &Changes,
) -> Index {
    let Some(mut index) = previous
        .filter(|_| !changes.directories.contains("@Schema") && !changes.directories.contains(""))
    else {
        return load(root, snapshot);
    };
    if (!batch.changed.is_empty() || !batch.removed.is_empty()) && index.is_ok() {
        Arc::make_mut(&mut index)
            .as_mut()
            .expect("checked projection")
            .apply_snapshot(snapshot, batch);
    }
    index
}
