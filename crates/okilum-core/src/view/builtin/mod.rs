//! Shipped declarative views for the project workspace.
use super::{parse, run, Finding, RunContext, Section, Show, Status};
use crate::{
    agent_imports::{Failure, RootedSource, Source as _},
    records::{self, FieldSchema, Projection, Value},
    Vault,
};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, OnceLock},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectKind {
    Decisions,
    Reports,
    Handovers,
}

impl ProjectKind {
    pub const ALL: [Self; 3] = [Self::Decisions, Self::Reports, Self::Handovers];

    pub fn label(self) -> &'static str {
        match self {
            Self::Decisions => "Decisions",
            Self::Reports => "Reports",
            Self::Handovers => "Handovers",
        }
    }

    pub fn directories(self) -> &'static [&'static str] {
        match self {
            Self::Decisions => &["ops/decisions", "decisions"],
            Self::Reports => &["ops/daily", "ops/reports", "reports"],
            Self::Handovers => &["handovers", "ops/handovers"],
        }
    }

    fn template(self) -> (&'static str, &'static str) {
        match self {
            Self::Decisions => (
                "@views/project-decisions.md",
                include_str!("project-decisions.md"),
            ),
            Self::Reports => (
                "@views/project-reports.md",
                include_str!("project-reports.md"),
            ),
            Self::Handovers => (
                "@views/project-handovers.md",
                include_str!("project-handovers.md"),
            ),
        }
    }
}

/// A bounded source from the current project's record folders, with its
/// existing display title, authored status and recorded date or filename date.
pub struct ProjectRecord<'a> {
    pub source: records::Source<'a>,
    pub title: &'a str,
    pub date: Option<&'a str>,
    pub status: Option<&'a str>,
}

pub fn project_records<'a>(
    root: &Path,
    vault: &Vault,
    kind: ProjectKind,
    records: impl IntoIterator<Item = ProjectRecord<'a>>,
) -> Result<Section, String> {
    let (path, builtin) = kind.template();
    let override_source = read_override(root, path)?;
    let doc = parse(path, override_source.as_deref().unwrap_or(builtin))
        .map_err(|findings| messages(findings.iter()))?;
    let [section] = doc.blocks.as_slice() else {
        return Err(format!("{path}: Expected one view block"));
    };
    let block = section
        .block
        .as_ref()
        .map_err(|finding| messages([finding]))?;
    if doc.front.extends.is_some()
        || block.from != "notes"
        || !matches!(block.show, Show::List | Show::Table)
    {
        return Err(format!(
            "{path}: Project record views require from: notes and show: list or table"
        ));
    }

    let schema = if override_source.is_some() {
        FieldSchema::load(root).map_or_else(|_| project_schema(), Arc::new)
    } else {
        project_schema()
    };
    let records: BTreeMap<_, _> = records
        .into_iter()
        .filter(|record| !schema.excludes(record.source.path))
        .map(|record| (record.source.path, record))
        .collect();
    let projection = Projection::from_records(
        schema.clone(),
        records.values().map(|record| {
            let mut note = records::project_note(&schema, record.source);
            note.fields
                .insert("title".into(), Value::String(record.title.to_owned()));
            if override_source.is_none() {
                // The legacy list sorts all authored date strings verbatim.
                note.fields.remove("date");
            }
            if let Some(date) = record.date {
                note.fields.entry("date".into()).or_insert_with(|| {
                    if override_source.is_some() {
                        if let Some(day) = records::value::date(date) {
                            return Value::Date(day);
                        }
                    }
                    Value::String(date.to_owned())
                });
            }
            if let Some(status) = record.status {
                note.fields
                    .entry("status".into())
                    .or_insert_with(|| Value::String(status.to_owned()));
            }
            note
        }),
    );
    let read = |path: &str| {
        let bytes = records.get(path)?.source.bytes;
        std::str::from_utf8(bytes).ok().map(str::to_owned)
    };
    let mut context = RunContext::new(&projection, vault, chrono::Local::now().fixed_offset());
    context.read_source = Some(&read);
    let mut result = run(&doc, &context);
    if result.findings.iter().any(Finding::is_error) {
        return Err(messages(result.findings.iter().filter(|f| f.is_error())));
    }
    let mut section = result.sections.remove(0);
    if section.status != Status::Ok {
        return Err(messages(&section.diagnostics));
    }
    section.diagnostics.extend(result.findings);
    Ok(section)
}

fn read_override(root: &Path, path: &str) -> Result<Option<String>, String> {
    for source in [root.join("@views"), root.join(path)] {
        match source.symlink_metadata() {
            Ok(meta) if meta.is_symlink() => return Err(format!("Cannot read {path} (symlink)")),
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("Cannot read {path} ({error})")),
        }
    }
    let rooted = RootedSource::new(root).map_err(|error| format!("{path}: {error:?}"))?;
    match rooted.read(&rooted.root().join(path), 256 * 1024) {
        Ok(source) => Ok(Some(source)),
        Err(Failure::Missing) => Ok(None),
        Err(error) => Err(format!("Cannot read {path} ({error:?})")),
    }
}

fn messages<'a>(findings: impl IntoIterator<Item = &'a Finding>) -> String {
    findings
        .into_iter()
        .map(|f| format!("{}:{}: {}", f.path, f.line, f.message))
        .collect::<Vec<_>>()
        .join("\n")
}

fn project_schema() -> Arc<FieldSchema> {
    static SCHEMA: OnceLock<Arc<FieldSchema>> = OnceLock::new();
    SCHEMA
        .get_or_init(|| {
            let schema = FieldSchema::from_documents(|name| {
                Ok(match name {
                    "fields.yaml" => include_str!("project-fields.yaml").to_owned(),
                    "layout.yaml" => "layers: {}\ndomains: []".into(),
                    "frontmatter-schema.yaml" => "ignore_globs: []".into(),
                    _ => anyhow::bail!("Unexpected project schema document {name}"),
                })
            })
            .expect("shipped project record schema must load");
            Arc::new(schema)
        })
        .clone()
}
