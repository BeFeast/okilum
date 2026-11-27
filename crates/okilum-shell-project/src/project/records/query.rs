use okilum_core::{
    records::Source,
    vault::note_path,
    view::{builtin, Show},
    Vault,
};
use okilum_t3::records::{self, Kind, Record};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path, sync::Arc};

#[derive(Default)]
pub struct Snapshot {
    pub sources: Arc<[Record]>,
    pub rows: Vec<Row>,
    pub columns: Vec<String>,
    pub table: bool,
    pub problems: Vec<String>,
}

pub(super) struct Row {
    pub source: usize,
    pub cells: Vec<String>,
}

pub(super) fn load(
    root: &Path,
    vault: &Vault,
    project: &Path,
    kind: Kind,
    previous: &[Record],
) -> Snapshot {
    let Ok(project_path) = project.strip_prefix(root) else {
        return Snapshot {
            problems: vec!["Project records are outside the active vault".into()],
            ..Snapshot::default()
        };
    };
    let mut scan = records::read(project, kind);
    if !scan.problems.is_empty() {
        for previous in previous {
            if !scan
                .records
                .iter()
                .any(|record| record.path == previous.path)
            {
                scan.records.push(previous.clone());
                scan.problems.push(format!(
                    "{}: previous snapshot retained; current source unavailable",
                    previous.path
                ));
            }
        }
    }
    let mut snapshot = Snapshot {
        sources: scan.records.into(),
        problems: scan.problems,
        ..Snapshot::default()
    };
    let source_indices: BTreeMap<_, _> = snapshot
        .sources
        .iter()
        .enumerate()
        .map(|(index, record)| (note_path(&project_path.join(&record.path)), index))
        .collect();
    let records = source_indices.iter().map(|(path, &index)| {
        let record = &snapshot.sources[index];
        builtin::ProjectRecord {
            source: Source {
                path,
                bytes: record.source.as_bytes(),
                mtime: record
                    .modified
                    .map(chrono::DateTime::<chrono::Utc>::from)
                    .map(|at| at.fixed_offset()),
            },
            title: &record.title,
            date: record.date.as_deref(),
            status: record.status.as_deref(),
        }
    });
    let section = match builtin::project_records(root, vault, kind, records) {
        Ok(section) => section,
        Err(error) => {
            snapshot.problems.push(error);
            return snapshot;
        }
    };
    snapshot.table = section.show == Show::Table;
    snapshot.columns = section.cols;
    if snapshot.columns.is_empty() {
        snapshot.columns.push("title".to_owned());
    }
    snapshot.rows = section
        .rows
        .iter()
        .filter_map(|row| {
            let &source = source_indices.get(&row.path)?;
            let record = &snapshot.sources[source];
            let cells = snapshot
                .columns
                .iter()
                .map(|column| match column.as_str() {
                    "path" => record.path.clone(),
                    "date" if !snapshot.table && !row.fields.contains_key(column) => {
                        "Date not specified".into()
                    }
                    _ => row.fields.get(column).map_or_else(
                        || {
                            if column == "title" {
                                record.title.clone()
                            } else {
                                String::new()
                            }
                        },
                        cell_text,
                    ),
                })
                .collect();
            Some(Row { source, cells })
        })
        .collect();
    snapshot.problems.extend(
        section
            .diagnostics
            .into_iter()
            .map(|finding| format!("{}:{}: {}", finding.path, finding.line, finding.message)),
    );
    if section.truncated {
        snapshot.problems.push(format!(
            "Showing {} of {} project records.",
            snapshot.rows.len(),
            section.total
        ));
    }
    snapshot
}

fn cell_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(cell_text).collect::<Vec<_>>().join(", "),
        Value::Object(object) if object.get("link").and_then(Value::as_str).is_some() => {
            let link = object["link"].as_str().unwrap_or_default();
            match object.get("ambiguous").and_then(Value::as_array) {
                Some(paths) => format!(
                    "{link} (ambiguous: {})",
                    paths.iter().map(cell_text).collect::<Vec<_>>().join(", ")
                ),
                None => link.to_owned(),
            }
        }
        value => value.to_string(),
    }
}
