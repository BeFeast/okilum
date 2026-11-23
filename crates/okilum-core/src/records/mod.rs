//! Rebuildable typed records over canonical Markdown source publications.
mod layout;
mod note;
mod schema;
pub(crate) mod value;

pub use schema::{Field, FieldSchema, FieldType};
pub use value::{Link, Value};

use crate::vault::warm::{incremental::Batch, Snapshot};
use crate::Vault;
use chrono::{DateTime, FixedOffset};
use rayon::prelude::*;
use std::{borrow::Cow, collections::BTreeMap, sync::Arc};
use time::Date;

pub type Fields = BTreeMap<String, Value>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub code: &'static str,
    pub field: String,
    pub line: Option<usize>,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskRecord {
    pub line: usize,
    pub fields: Fields,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionRecord {
    pub line: usize,
    pub anchor: String,
    pub fields: Fields,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteRecord {
    pub path: String,
    pub fields: Fields,
    pub tasks: Vec<TaskRecord>,
    pub sections: Vec<SectionRecord>,
    pub findings: Vec<Finding>,
}

impl NoteRecord {
    /// Time- and graph-dependent built-ins use the current publication.
    pub fn value(&self, key: &str, vault: &Vault, today: Date) -> Option<Cow<'_, Value>> {
        match key {
            "backlinks" => Some(Cow::Owned(Value::Number(
                vault.resolved_backlink_count(&self.path).into(),
            ))),
            "stale" => Some(Cow::Owned(Value::Bool(
                self.fields.get("stale_after")?.as_date()? < today,
            ))),
            _ => self.fields.get(key).map(Cow::Borrowed),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Source<'a> {
    pub path: &'a str,
    pub bytes: &'a [u8],
    pub mtime: Option<DateTime<FixedOffset>>,
}

#[derive(Clone, Debug)]
pub struct Projection {
    schema: Arc<FieldSchema>,
    notes: BTreeMap<String, Arc<NoteRecord>>,
}

impl Projection {
    pub fn build<'a>(
        schema: Arc<FieldSchema>,
        sources: impl IntoIterator<Item = Source<'a>>,
    ) -> Self {
        let sources: Vec<_> = sources
            .into_iter()
            .filter(|source| schema.includes(source.path))
            .collect();
        let rows = project_sources(&sources, |source| record(&schema, *source));
        Self {
            schema,
            notes: rows.into_iter().collect(),
        }
    }

    pub fn from_snapshot(schema: Arc<FieldSchema>, snapshot: &Snapshot) -> Self {
        let paths: Vec<_> = snapshot
            .source_paths()
            .filter(|path| schema.includes(path))
            .collect();
        let rows = project_sources(&paths, |path| snapshot_record(&schema, snapshot, path));
        Self {
            schema,
            notes: rows.into_iter().flatten().collect(),
        }
    }

    pub fn schema(&self) -> &FieldSchema {
        &self.schema
    }

    pub fn notes(&self) -> impl Iterator<Item = &NoteRecord> {
        self.notes.values().map(Arc::as_ref)
    }

    pub fn note(&self, path: &str) -> Option<&NoteRecord> {
        self.notes.get(path).map(Arc::as_ref)
    }

    /// Consume the upserts and removals from a completed vault publication.
    /// Renames are a removal and an upsert. Upserts take precedence on overlap.
    pub fn apply<'a>(
        &mut self,
        changed: impl IntoIterator<Item = Source<'a>>,
        removed: impl IntoIterator<Item = &'a str>,
    ) {
        for path in removed {
            self.notes.remove(path);
        }
        let changed: Vec<_> = changed
            .into_iter()
            .filter(|source| self.schema.includes(source.path))
            .collect();
        let rows = project_sources(&changed, |source| record(&self.schema, *source));
        self.notes.extend(rows);
    }

    /// Update from the exact source bank accepted by the incremental worker.
    /// Reload the schema and rebuild when a contract document changes.
    pub fn apply_snapshot(&mut self, snapshot: &Snapshot, batch: &Batch) {
        let paths: Vec<_> = batch
            .changed
            .iter()
            .filter(|path| self.schema.includes(path))
            .collect();
        let rows = project_sources(&paths, |path| snapshot_record(&self.schema, snapshot, path));
        for path in &batch.removed {
            self.notes.remove(path);
        }
        for (path, row) in paths.into_iter().zip(rows) {
            if let Some((path, row)) = row {
                self.notes.insert(path, row);
            } else {
                self.notes.remove(path);
            }
        }
    }

    /// Test field presence over the rows already narrowed to a view's scope.
    pub fn missing<'a>(rows: impl IntoIterator<Item = &'a NoteRecord>, key: &str) -> Vec<&'a str> {
        rows.into_iter()
            .filter(|row| match key {
                "backlinks" => false,
                "stale" => !row.fields.contains_key("stale_after"),
                _ => !row.fields.contains_key(key),
            })
            .map(|row| row.path.as_str())
            .collect()
    }

    pub fn legacy_paths(&self) -> impl Iterator<Item = &str> {
        self.notes()
            .filter(|note| note.findings.iter().any(|f| f.code == "fields/legacy-path"))
            .map(|note| note.path.as_str())
    }
}

fn project_sources<T: Sync, R: Send>(
    sources: &[T],
    project: impl Fn(&T) -> R + Sync + Send,
) -> Vec<R> {
    // Small edits usually finish faster than a worker handoff. Larger batches
    // preserve the input order while scheduling expensive files independently.
    if sources.len() < 16 {
        sources.iter().map(project).collect()
    } else {
        sources.par_iter().map(project).collect()
    }
}

fn record(schema: &FieldSchema, source: Source<'_>) -> (String, Arc<NoteRecord>) {
    (
        source.path.to_owned(),
        Arc::new(note::project(schema, source)),
    )
}

fn snapshot_record(
    schema: &FieldSchema,
    snapshot: &Snapshot,
    path: &str,
) -> Option<(String, Arc<NoteRecord>)> {
    let bytes = snapshot.source_bytes(path)?;
    let mtime = snapshot
        .source_revision(path)
        .and_then(|stamp| stamp.modified_nanoseconds().try_into().ok())
        .map(DateTime::from_timestamp_nanos)
        .map(|t| t.fixed_offset());
    Some(record(
        schema,
        Source {
            path,
            bytes: &bytes,
            mtime,
        },
    ))
}

pub(crate) fn is_slug(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
