use super::{
    layout::{leading_date, PathFields},
    value::{self, absent},
    Field, FieldSchema, FieldType, Fields, Finding, NoteRecord, SectionRecord, Source, TaskRecord,
    Value,
};
use crate::{
    properties::{self, PropertyValue},
    render, tasks, Vault,
};
use comrak::{nodes::NodeValue, parse_document, Arena};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn project(schema: &FieldSchema, source: Source<'_>) -> NoteRecord {
    let text = String::from_utf8_lossy(source.bytes);
    let path = schema.layout.project(source.path);
    let mut row = NoteRecord {
        path: source.path.to_owned(),
        fields: BTreeMap::from([
            ("path".into(), Value::String(source.path.to_owned())),
            (
                "rev".into(),
                Value::String(format!("sha256:{:x}", Sha256::digest(source.bytes))),
            ),
        ]),
        tasks: Vec::new(),
        sections: Vec::new(),
        findings: Vec::new(),
    };

    if path.legacy {
        row.finding(
            "fields/legacy-path",
            "layer",
            "Legacy path has no lifecycle fields",
        );
    }
    let raw = properties::frontmatter_block(&text).map(properties::parse_for_records);
    let mut raw: BTreeMap<_, _> = match raw {
        Some(Ok(props)) => props
            .into_iter()
            .map(|prop| (prop.key, prop.value))
            .collect(),
        Some(Err(error)) => {
            row.finding("fields/invalid", "frontmatter", error);
            BTreeMap::new()
        }
        None => BTreeMap::new(),
    };

    project_mirror(schema, &mut row, &mut raw, "layer", path.layer, None);
    project_mirror(
        schema,
        &mut row,
        &mut raw,
        "context",
        path.context,
        path.layer,
    );
    project_mirror(
        schema,
        &mut row,
        &mut raw,
        "channel",
        path.channel,
        path.layer,
    );
    if !(path.context_root && path.kind.is_none()) {
        project_mirror(schema, &mut row, &mut raw, "kind", path.kind, path.layer);
    }

    let mut invalid = BTreeSet::new();
    let authored_type = raw.get("type").and_then(value::string);
    let is_decision = path.kind == Some("decisions")
        || authored_type.is_some_and(|kind| {
            schema
                .fields
                .get("type")
                .and_then(|field| field.enum_value(kind, path.layer, true))
                .as_deref()
                == Some("decision")
        });
    for (name, field) in &schema.fields {
        if row.fields.contains_key(name) || matches!(name.as_str(), "layer" | "context" | "channel")
        {
            continue;
        }
        if name == "status" && (is_decision || !matches!(path.layer, None | Some("state")))
            || name == "decision_status" && !is_decision
        {
            raw.remove(name);
            continue;
        }
        let raw_value = raw.get(name).filter(|raw| !absent(raw));
        let input = raw_value.or_else(|| {
            field
                .legacy_keys
                .iter()
                .find_map(|key| raw.get(key).filter(|raw| !absent(raw)))
        });
        let Some(input) = input else { continue };
        let normalized = normalize(name, field, input, &raw, raw_value.is_none());
        let enum_layer = if path.layer.is_none() {
            Some("legacy")
        } else {
            path.layer
        };
        let value = convert(schema, name, &normalized, field, enum_layer, &mut row);
        if let Some(value) = value {
            row.fields.insert(name.clone(), value);
        } else {
            invalid.insert(name.clone());
            row.finding("fields/invalid", name, format!("Invalid value {input:?}"));
        }
    }
    for (key, raw) in &raw {
        if schema.note_field(key).is_none() {
            if let Some(value) = value::inferred(raw, schema.timezone) {
                row.fields.insert(key.clone(), value);
            }
        }
    }

    if let Some(area) = path.area {
        match row.fields.get("area") {
            Some(Value::String(authored)) if authored != area => {
                row.finding(
                    "fields/conflict",
                    "area",
                    format!("Stored area {authored:?} disagrees with path domain {area:?}"),
                );
            }
            None if !invalid.contains("area") => {
                row.fields
                    .insert("area".into(), Value::String(area.to_owned()));
            }
            _ => {}
        }
    }

    let kind = match row.fields.get("kind") {
        Some(Value::String(kind)) => Some(kind.clone()),
        _ => None,
    };
    if let Some(mtime) = source.mtime {
        row.fields.insert("mtime".into(), Value::DateTime(mtime));
        if let Some(day) = value::local_day(mtime, schema.timezone) {
            fallback(&mut row.fields, &invalid, "updated", Some(Value::Date(day)));
        }
    }
    let body = render::without_frontmatter(&text);
    let heading_title = markdown(
        &mut row,
        body,
        text[..text.len() - body.len()]
            .bytes()
            .filter(|&b| b == b'\n')
            .count(),
    );
    if !row.fields.contains_key("title") {
        let title = heading_title.unwrap_or_else(|| Vault::title_of(source.path));
        row.fields.insert("title".into(), Value::String(title));
    }

    let file = source.path.rsplit('/').next().unwrap_or_default();
    let created = path
        .date
        .filter(|_| path.layer == Some("sources"))
        .or_else(|| leading_date(file));
    fallback(
        &mut row.fields,
        &invalid,
        "created",
        created.map(Value::Date),
    );
    fallback(
        &mut row.fields,
        &invalid,
        "date",
        path.date
            .filter(|_| path.layer == Some("log"))
            .map(Value::Date),
    );
    let inferred_type = match (path.layer, kind.as_deref(), path.channel) {
        (Some("state"), _, _) => Some("note"),
        (Some("sources"), _, _) if source.path.split('/').any(|part| part == "transcripts") => {
            Some("transcript")
        }
        (Some("sources"), _, Some("web")) => Some("clipping"),
        (Some("log"), Some(kind), _) => match kind {
            "decisions" => Some("decision"),
            "handovers" => Some("handover"),
            "incidents" => Some("incident"),
            "reports" => Some("report"),
            "specs" => Some("spec"),
            "research" => Some("research"),
            "daily" => Some("daily"),
            "final" => Some("archive"),
            _ => None,
        },
        _ => None,
    };
    fallback(
        &mut row.fields,
        &invalid,
        "type",
        inferred_type.map(|kind| Value::String(kind.into())),
    );
    required(
        schema,
        &mut row,
        &path,
        kind.as_deref(),
        &invalid,
        is_decision,
    );
    row
}

fn fallback(fields: &mut Fields, invalid: &BTreeSet<String>, name: &str, value: Option<Value>) {
    if !invalid.contains(name) && !fields.contains_key(name) {
        if let Some(value) = value {
            fields.insert(name.into(), value);
        }
    }
}

fn project_mirror(
    schema: &FieldSchema,
    row: &mut NoteRecord,
    raw: &mut BTreeMap<String, PropertyValue>,
    name: &str,
    derived: Option<&str>,
    layer: Option<&str>,
) {
    let Some(field) = schema.fields.get(name) else {
        return;
    };
    let expected = derived.and_then(|text| {
        value::convert(
            &PropertyValue::Text(text.into()),
            field,
            &field.field_type,
            layer,
            schema.timezone,
        )
    });
    if let Some(raw) = raw.remove(name).filter(|raw| !absent(raw)) {
        let stored = value::convert(&raw, field, &field.field_type, layer, schema.timezone);
        if stored.is_none() {
            row.finding(
                "fields/invalid",
                name,
                format!("Invalid stored mirror {raw:?}"),
            );
        }
        if stored != expected || stored.is_none() {
            row.finding(
                "fields/conflict",
                name,
                format!("Stored mirror {raw:?} disagrees with path value {derived:?}"),
            );
        }
    }
    if let Some(value) = expected {
        row.fields.insert(name.into(), value);
    } else if let Some(text) = derived {
        row.finding(
            "fields/invalid",
            name,
            format!("Invalid path value {text:?}"),
        );
    }
}

fn normalize(
    key: &str,
    field: &Field,
    raw: &PropertyValue,
    frontmatter: &BTreeMap<String, PropertyValue>,
    legacy: bool,
) -> PropertyValue {
    use PropertyValue::{List, Number, Text, Url};
    match key {
        "tags" => {
            let list = match raw {
                List(items) => items.as_slice(),
                raw => std::slice::from_ref(raw),
            };
            let mut seen = BTreeSet::new();
            List(
                list.iter()
                    .flat_map(|item| match item {
                        Text(text) => text
                            .split(',')
                            .map(str::trim)
                            .map(str::to_lowercase)
                            .map(Text)
                            .collect(),
                        Number(text) => vec![Text(text.clone())],
                        other => vec![other.clone()],
                    })
                    .filter(|item| match item {
                        Text(text) => !text.is_empty() && seen.insert(text.clone()),
                        _ => true,
                    })
                    .collect(),
            )
        }
        "related_to" => {
            if let Text(text) = raw {
                let items: Option<Vec<_>> = text
                    .split(',')
                    .map(|link| {
                        let inner = link.trim().strip_prefix("[[")?.strip_suffix("]]")?;
                        let (target, label) = inner.split_once('|').unwrap_or((inner, inner));
                        Some(PropertyValue::Link {
                            target: target.trim().into(),
                            label: label.trim().into(),
                        })
                    })
                    .collect();
                if let Some(items) = items {
                    return List(items);
                }
            }
            raw.clone()
        }
        "issue" | "pr" if legacy => {
            let repo =
                field
                    .legacy_repo_keys
                    .iter()
                    .find_map(|key| match frontmatter.get(key)? {
                        Url(url) => Some(url.trim_end_matches('/').trim_end_matches(".git")),
                        _ => None,
                    });
            let expand = |value: &PropertyValue| {
                if let Some((repo, number)) = repo.zip(match value {
                    Number(n) | Text(n) => n.parse::<u64>().ok().filter(|&n| n > 0),
                    _ => None,
                }) {
                    let endpoint = if key == "issue" { "issues" } else { "pulls" };
                    Url(format!("{repo}/{endpoint}/{number}"))
                } else {
                    value.clone()
                }
            };
            match raw {
                List(values) => List(values.iter().map(expand).collect()),
                value => expand(value),
            }
        }
        "aliases" => {
            let list = match raw {
                List(items) => items.as_slice(),
                raw => std::slice::from_ref(raw),
            };
            List(
                list.iter()
                    .map(|item| match item {
                        Text(s) if s.contains(['[', ']', '|', '#', '^', '\r', '\n']) => {
                            PropertyValue::Empty
                        }
                        item => item.clone(),
                    })
                    .collect(),
            )
        }
        _ => raw.clone(),
    }
}

fn convert(
    schema: &FieldSchema,
    key: &str,
    input: &PropertyValue,
    field: &Field,
    layer: Option<&str>,
    row: &mut NoteRecord,
) -> Option<Value> {
    let value = value::convert(input, field, &field.field_type, layer, schema.timezone)?;
    match &value {
        Value::LegacyDate { .. } => {
            row.finding("fields/legacy", key, "Datetime stored in a date field")
        }
        Value::String(text)
            if field.field_type == FieldType::Enum
                && !field.closed
                && field.enum_value(text, layer, false).is_none() =>
        {
            row.finding(
                "fields/unknown-value",
                key,
                format!("Unknown value {text:?}"),
            )
        }
        Value::List(tags) if key == "tags" => {
            for tag in tags {
                if let Value::String(tag) = tag {
                    if !field.values.contains(tag) {
                        row.finding("fields/unknown-value", key, format!("Unknown tag {tag:?}"));
                    }
                }
            }
        }
        _ => {}
    }
    Some(value)
}

fn required(
    schema: &FieldSchema,
    row: &mut NoteRecord,
    path: &PathFields<'_>,
    kind: Option<&str>,
    invalid: &BTreeSet<String>,
    is_decision: bool,
) {
    let note_type = match row.fields.get("type") {
        Some(Value::String(t)) => Some(t.clone()),
        _ => None,
    };
    for (name, field) in &schema.fields {
        if row.fields.contains_key(name)
            || invalid.contains(name)
            || name == "status" && (is_decision || !matches!(path.layer, None | Some("state")))
        {
            continue;
        }
        let rule = &field.required;
        let selected = rule
            .layers
            .iter()
            .any(|l| l == "all" || Some(l.as_str()) == path.layer)
            || note_type.as_ref().is_some_and(|t| rule.types.contains(t));
        let excepted = rule
            .except_layers
            .iter()
            .any(|l| Some(l.as_str()) == path.layer)
            || rule.except_kinds.iter().any(|k| Some(k.as_str()) == kind)
            || rule.except_domain_root_state && path.domain_root;
        if selected && !excepted {
            row.finding("fields/missing", name, "Required field is absent");
        }
    }
}

fn markdown(row: &mut NoteRecord, body: &str, prefix_lines: usize) -> Option<String> {
    let has_tasks = ["[ ]", "[x]", "[X]"]
        .iter()
        .any(|marker| body.contains(marker));
    let mut title = None;
    let mut tasks_open = 0;
    if has_tasks || body.contains(['#', '=', '-']) {
        let arena = Arena::new();
        let root = parse_document(&arena, body, &render::comrak_options());
        for top in root.children() {
            for node in top.descendants() {
                let data = node.data.borrow();
                let NodeValue::Heading(heading) = &data.value else {
                    continue;
                };
                let mut text = String::new();
                for child in node.descendants().skip(1) {
                    match &child.data.borrow().value {
                        NodeValue::Text(value) => text.push_str(value),
                        NodeValue::Code(value) => text.push_str(&value.literal),
                        NodeValue::SoftBreak | NodeValue::LineBreak => text.push(' '),
                        _ => {}
                    }
                }
                let text = text.trim();
                if heading.level == 1
                    && std::ptr::eq(top, node)
                    && title.is_none()
                    && !text.is_empty()
                {
                    title = Some(text.to_owned());
                }
                let line = prefix_lines + data.sourcepos.start.line;
                row.sections.push(SectionRecord {
                    line,
                    anchor: text.to_owned(),
                    fields: BTreeMap::from([
                        ("line".into(), Value::Number(line.into())),
                        ("heading".into(), Value::String(text.to_owned())),
                        ("heading_level".into(), Value::Number(heading.level.into())),
                        ("anchor".into(), Value::String(text.to_owned())),
                    ]),
                });
            }
        }
        if has_tasks {
            for task in tasks::parse_tree(&row.path, body, prefix_lines, root) {
                tasks_open += usize::from(!task.checked);
                row.add_task(task);
            }
        }
    }
    row.fields
        .insert("tasks_open".into(), Value::Number(tasks_open.into()));
    title
}

impl NoteRecord {
    fn finding(&mut self, code: &'static str, field: &str, message: impl Into<String>) {
        self.findings.push(Finding {
            code,
            field: field.to_owned(),
            message: message.into(),
            line: None,
        });
    }

    fn add_task(&mut self, task: tasks::Task) {
        let meta = task.metadata;
        let mut fields = BTreeMap::from([
            ("line".into(), Value::Number(task.line.into())),
            ("checked".into(), Value::Bool(task.checked)),
            ("text".into(), Value::String(meta.text)),
        ]);
        for (key, field) in [
            ("due", meta.due),
            ("scheduled", meta.scheduled),
            ("start", meta.start),
            ("done", meta.done),
            ("created", meta.created),
        ] {
            if let Some(date) = field {
                fields.insert(key.into(), Value::Date(date));
            }
        }
        for (key, field) in [("owner", meta.owner), ("thread", meta.thread)] {
            if let Some(text) = field {
                fields.insert(key.into(), Value::String(text));
            }
        }
        let mut invalid_priority = false;
        for issue in meta.issues {
            let code = match issue.kind {
                tasks::MetadataIssueKind::Duplicate => "fields/duplicate",
                tasks::MetadataIssueKind::Invalid => {
                    invalid_priority |= issue.key == "priority";
                    "fields/invalid"
                }
            };
            self.findings.push(Finding {
                code,
                field: issue.key,
                line: Some(task.line),
                message: format!("Task metadata value {:?}", issue.value),
            });
        }
        if !invalid_priority {
            fields.insert("priority".into(), Value::Number(task.priority.into()));
        }
        fields.extend(
            meta.other
                .into_iter()
                .map(|(key, text)| (key, Value::String(text))),
        );
        self.tasks.push(TaskRecord {
            line: task.line,
            fields,
        });
    }
}
