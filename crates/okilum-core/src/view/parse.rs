use super::{bind, compute, Finding, Severity};
use crate::records::{Field, FieldSchema, FieldType};
use comrak::{nodes::NodeValue, parse_document, Arena};
use serde::Deserialize;
use serde_json::Value as Json;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
};

#[derive(Debug)]
pub struct ViewDoc {
    pub(super) path: String,
    pub(super) rev: String,
    pub(super) front: Frontmatter,
    pub(super) front_text: String,
    pub(super) blocks: Vec<SectionDoc>,
    pub(super) parts: Vec<Part>,
    pub(super) headings: Vec<(usize, String, String)>,
}

impl ViewDoc {
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn is_sidebar_tree(&self) -> bool {
        self.front.sidebar
            && self.front.extends.is_none()
            && matches!(self.blocks.as_slice(), [section]
                if section.block.as_ref().is_ok_and(|block| block.show == Show::Tree))
    }

    /// Bind only values present on this branch, using the template's parameter
    /// types rather than assuming its parameter names match the tree fields.
    pub fn node_args(&self, branch: &[(String, Json)]) -> BTreeMap<String, Json> {
        self.front
            .params
            .iter()
            .filter_map(|(name, param)| {
                let field = param.kind().field()?;
                let (_, value) = branch.iter().rev().find(|(key, _)| key == field)?;
                (!value.is_null()).then(|| (name.clone(), value.clone()))
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
pub(super) enum Part {
    Markdown(String),
    Section { index: usize, heading: String },
}

#[derive(Debug)]
pub(super) struct SectionDoc {
    pub id: String,
    pub title: String,
    pub line: usize,
    pub yaml: String,
    pub block: Result<Block, Finding>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct Frontmatter {
    #[serde(rename = "type")]
    kind: String,
    view: Option<String>,
    #[serde(rename = "title")]
    _title: Option<String>,
    #[serde(default)]
    pub params: BTreeMap<String, Param>,
    pub extends: Option<String>,
    pub args: Option<BTreeMap<String, Json>>,
    pub budget: Option<String>,
    #[serde(rename = "materialize")]
    _materialize: Option<bool>,
    #[serde(default)]
    pub sidebar: bool,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(super) enum Param {
    Type(ParamType),
    Definition(ParamDefinition),
}

impl Param {
    pub fn kind(&self) -> ParamType {
        match self {
            Self::Type(kind) => *kind,
            Self::Definition(def) => def.kind,
        }
    }

    pub fn default_value(&self) -> Option<&Json> {
        match self {
            Self::Type(_) => None,
            Self::Definition(def) => def.default.as_ref(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ParamDefinition {
    #[serde(rename = "type")]
    kind: ParamType,
    default: Option<Json>,
    label: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(super) enum ParamType {
    Area,
    Context,
    Kind,
    Layer,
    Status,
    Text,
    Date,
    Number,
}

impl ParamType {
    pub fn field(self) -> Option<&'static str> {
        Some(match self {
            Self::Area => "area",
            Self::Context => "context",
            Self::Kind => "kind",
            Self::Layer => "layer",
            Self::Status => "status",
            Self::Text | Self::Number | Self::Date => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Show {
    #[default]
    Table,
    List,
    Cards,
    Board,
    Timeline,
    Graph,
    Sections,
    Log,
    Tree,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum KeyList {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Block {
    id: Option<String>,
    pub from: String,
    #[serde(rename = "where")]
    pub filter: Option<String>,
    #[serde(rename = "match")]
    pub matching: Option<String>,
    pub near: Option<String>,
    pub links: Option<Links>,
    #[serde(default, rename = "let")]
    pub lets: BTreeMap<String, String>,
    #[serde(default)]
    pub agg: BTreeMap<String, String>,
    group: Option<KeyList>,
    sort: Option<KeyList>,
    pub limit: Option<usize>,
    pub show: Show,
    pub cols: Option<Vec<String>>,
    pub at: Option<String>,
    pub levels: Option<Vec<String>>,
    #[serde(default)]
    pub order: BTreeMap<String, Vec<String>>,
    pub node_view: Option<String>,
    pub actions: Option<Vec<Action>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Links {
    pub to: Option<String>,
    pub from: Option<String>,
    pub depth: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(super) enum Action {
    Scalar(Check),
    Mapping(ActionMapping),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Check {
    Check,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(super) enum ActionMapping {
    Set(SetAction),
    Open(OpenAction),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SetAction {
    pub set: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OpenAction {
    pub open: Open,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Open {
    Thread,
}

impl Block {
    pub fn group(&self) -> &[String] {
        keys(&self.group)
    }
    pub fn sort(&self) -> &[String] {
        keys(&self.sort)
    }

    pub fn field<'a>(&self, schema: &'a FieldSchema, key: &str) -> Option<&'a Field> {
        match self.from.as_str() {
            "notes" => schema.note_field(key),
            kind @ ("tasks" | "sections") => {
                if let Some(parent) = key.strip_prefix("note.") {
                    schema.note_field(parent)
                } else {
                    let fields = if kind == "tasks" {
                        &schema.task_fields
                    } else {
                        &schema.section_fields
                    };
                    fields.get(key).or_else(|| schema.builtins.get(key))
                }
            }
            path => schema.jsonl_fields.get(&normalized_path(path))?.get(key),
        }
    }

    pub fn jsonl(&self) -> bool {
        !matches!(self.from.as_str(), "notes" | "tasks" | "sections")
    }
}

fn keys(list: &Option<KeyList>) -> &[String] {
    match list {
        Some(KeyList::One(one)) => std::slice::from_ref(one),
        Some(KeyList::Many(many)) => many,
        None => &[],
    }
}

pub(super) fn revision(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// Read a view definition without consulting the vault or evaluating any section.
/// Unreadable frontmatter and documents without sections return fatal findings.
pub fn parse(path: &str, source: &str) -> Result<ViewDoc, Vec<Finding>> {
    let (front, body) = crate::render::split_frontmatter(source)
        .or_else(|| {
            let (_, last) = source.rsplit_once('\n')?;
            if !matches!(last, "---" | "...") {
                return None;
            }
            let first_line = source.find('\n')? + 1;
            let opening = source[..first_line].trim_start_matches('\u{feff}');
            if !matches!(opening, "---\n" | "---\r\n") {
                return None;
            }
            Some((
                &source[first_line..source.len() - last.len()],
                &source[source.len()..],
            ))
        })
        .ok_or_else(|| vec![finding(path, 1, 1, "Expected view frontmatter")])?;
    let parsed: Frontmatter = deserialize(path, front, 1).map_err(|f| vec![f])?;
    let findings = front_findings(path, front, &parsed);
    if !findings.is_empty() {
        return Err(findings);
    }
    let prefix = source[..source.len() - body.len()]
        .bytes()
        .filter(|b| *b == b'\n')
        .count();
    let lines = crate::render::markdown_line_offsets(body);
    let arena = Arena::new();
    let root = parse_document(&arena, body, &crate::render::comrak_options());
    let mut blocks = Vec::new();
    let mut parts = Vec::new();
    let mut headings = Vec::new();
    let mut heading = None;
    let mut prose_start = 0;
    for node in root.children() {
        let data = node.data.borrow();
        let start = data.sourcepos.start.line;
        let Some(&start_at) = start.checked_sub(1).and_then(|line| lines.get(line)) else {
            heading = None;
            continue;
        };
        let end_at = lines
            .get(data.sourcepos.end.line)
            .copied()
            .unwrap_or(body.len());
        match &data.value {
            NodeValue::Heading(_) => {
                let mut title = String::new();
                for child in node.descendants().skip(1) {
                    match &child.data.borrow().value {
                        NodeValue::Text(text) => title.push_str(text),
                        NodeValue::Code(code) => title.push_str(&code.literal),
                        NodeValue::SoftBreak | NodeValue::LineBreak => title.push(' '),
                        _ => {}
                    }
                }
                let Some(markdown) = body.get(start_at..end_at) else {
                    heading = None;
                    continue;
                };
                if markdown.contains('$') {
                    headings.push((prefix + start, title.clone(), markdown.to_owned()));
                }
                heading = Some((start_at, title));
            }
            NodeValue::CodeBlock(code) if code.fenced && code.info.trim() == "view" => {
                let (heading_at, title) = heading.take().unwrap_or((start_at, String::new()));
                let Some(prose) = body.get(prose_start..heading_at) else {
                    continue;
                };
                let Some(heading) = body.get(heading_at..start_at) else {
                    continue;
                };
                push_prose(&mut parts, prose);
                prose_start = end_at;
                let line = prefix + start;
                let block = deserialize::<Block>(path, &code.literal, line);
                let id = block
                    .as_ref()
                    .ok()
                    .and_then(|b| b.id.clone())
                    .or_else(|| slug(&title))
                    .unwrap_or_else(|| format!("s{}", blocks.len() + 1));
                parts.push(Part::Section {
                    index: blocks.len(),
                    heading: heading.to_owned(),
                });
                blocks.push(SectionDoc {
                    id,
                    title,
                    line,
                    yaml: code.literal.clone(),
                    block,
                });
            }
            _ => heading = None,
        }
    }
    push_prose(&mut parts, &body[prose_start..]);
    if blocks.is_empty() && parsed.extends.is_none() {
        return Err(vec![finding(
            path,
            prefix + 1,
            1,
            "No top-level view blocks",
        )]);
    }
    if !blocks.is_empty() && parsed.extends.is_some() {
        return Err(vec![key_finding(
            path,
            front,
            1,
            "extends",
            "A view instance cannot contain view blocks",
        )]);
    }
    Ok(ViewDoc {
        path: path.into(),
        rev: revision(source.as_bytes()),
        front: parsed,
        front_text: front.into(),
        blocks,
        parts,
        headings,
    })
}

fn push_prose(parts: &mut Vec<Part>, text: &str) {
    if !text.trim().is_empty() {
        parts.push(Part::Markdown(text.to_owned()));
    }
}

fn slug(text: &str) -> Option<String> {
    let result = text
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join("-");
    (!result.is_empty()).then_some(result)
}

fn deserialize<T: for<'de> Deserialize<'de>>(
    path: &str,
    yaml: &str,
    offset: usize,
) -> Result<T, Finding> {
    use serde_yaml::Value as Yaml;
    fn invalid(value: &Yaml) -> bool {
        match value {
            Yaml::Null | Yaml::Tagged(_) => true,
            Yaml::Sequence(items) => items.iter().any(invalid),
            Yaml::Mapping(items) => items
                .iter()
                .any(|(key, value)| !key.is_string() || invalid(value)),
            _ => false,
        }
    }

    let input: Yaml =
        serde_yaml::from_str(yaml).map_err(|error| yaml_finding(path, yaml, offset, error))?;
    let Yaml::Mapping(mut map) = input else {
        return Err(finding(path, offset, 1, "Expected a YAML mapping"));
    };
    if offset == 1 {
        // Other frontmatter is owned by the note schema.
        map.retain(|key, _| {
            matches!(
                key.as_str(),
                Some(
                    "type"
                        | "view"
                        | "title"
                        | "params"
                        | "args"
                        | "extends"
                        | "budget"
                        | "materialize"
                        | "sidebar"
                )
            )
        });
    }
    for (key, value) in &map {
        let Some(key) = key.as_str() else {
            return Err(finding(path, offset + 1, 1, "Expected a string key"));
        };
        if invalid(value) {
            return Err(key_finding(
                path,
                yaml,
                offset,
                key,
                format!("Invalid value for '{key}'"),
            ));
        }
    }
    // JSON deserialization enforces the schema's scalar types. serde_yaml's
    // string visitor also accepts unquoted booleans and numbers as strings.
    let decoded = serde_json::to_value(map).and_then(serde_json::from_value::<T>);
    match decoded {
        Ok(value) => Ok(value),
        Err(strict) => match serde_yaml::from_str::<T>(yaml) {
            Err(error) => Err(yaml_finding(path, yaml, offset, error)),
            Ok(_) => Err(finding(path, offset + 1, 1, strict.to_string())),
        },
    }
}

fn yaml_finding(path: &str, yaml: &str, offset: usize, error: serde_yaml::Error) -> Finding {
    let (line, col) = error
        .location()
        .map_or((1, 1), |loc| (loc.line(), loc.column()));
    let line = line.clamp(1, yaml.lines().count().max(1));
    let col = col.clamp(
        1,
        yaml.lines().nth(line - 1).map_or(0, |s| s.chars().count()) + 1,
    );
    finding(path, offset + line, col, error.to_string())
}

fn front_findings(path: &str, yaml: &str, front: &Frontmatter) -> Vec<Finding> {
    let mut errors = Vec::new();
    let mut check = |valid: bool, key, message| {
        if !valid {
            errors.push(key_finding(path, yaml, 1, key, message));
        }
    };
    check(front.kind == "view", "type", "Expected type: view");
    check(
        front.view.as_deref().is_none_or(|view| view == "query"),
        "view",
        "Expected view: query",
    );
    check(
        front.params.len() <= 8,
        "params",
        "A view can declare up to eight params",
    );
    for (key, param) in &front.params {
        check(name(key), "params", "Invalid parameter name");
        if let Param::Definition(def) = param {
            check(
                def.label.as_ref().is_none_or(|s| s.chars().count() <= 80),
                "params",
                "Parameter labels may contain up to 80 characters",
            );
            check(
                def.default.as_ref().is_none_or(scalar_arg),
                "params",
                "Parameter defaults must be strings or numbers",
            );
        }
    }
    if let Some(extends) = &front.extends {
        check(
            wikilink(extends).is_some(),
            "extends",
            "Expected a template wikilink",
        );
        check(
            front.args.is_some(),
            "extends",
            "A view instance must supply args",
        );
    }
    if let Some(args) = &front.args {
        check(
            args.iter()
                .all(|(key, value)| name(key) && scalar_arg(value)),
            "args",
            "Arguments must have valid names and string or number values",
        );
    }
    if let Some(value) = &front.budget {
        check(
            budget(value).is_some(),
            "budget",
            "Expected a budget from 1kb to 9999kb",
        );
    }
    errors
}

pub(super) fn budget(text: &str) -> Option<usize> {
    let digits = text.strip_suffix("kb")?;
    if digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) || digits.len() > 4 {
        return None;
    }
    Some(digits.parse::<usize>().ok()? * 1024)
}

fn scalar_arg(value: &Json) -> bool {
    value.is_string() || value.is_number()
}

pub(super) fn name(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b == b'_')
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

pub(super) fn field_name(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_' || b == b'@')
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

pub(super) fn unsign(key: &str) -> &str {
    key.strip_prefix('-').unwrap_or(key)
}

pub(super) fn wikilink(text: &str) -> Option<&str> {
    let inside = text.strip_prefix("[[")?.strip_suffix("]]")?;
    let target = inside.split('|').next()?;
    (!target.is_empty() && !target.contains(['#', ']']) && !inside.contains(']')).then_some(target)
}

pub(super) fn relative_path(text: &str) -> bool {
    !text.is_empty()
        && !text.contains(['\\', ':', '\0'])
        && text.split('/').all(|p| p != "..")
        && Path::new(text)
            .components()
            .all(|p| matches!(p, Component::Normal(_) | Component::CurDir))
}

pub(super) fn normalized_path(path: &str) -> String {
    Path::new(path)
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

pub(super) fn finding(path: &str, line: usize, col: usize, message: impl Into<String>) -> Finding {
    Finding {
        path: path.into(),
        line,
        col,
        severity: Severity::Error,
        message: message.into(),
    }
}

fn key_finding(
    path: &str,
    yaml: &str,
    offset: usize,
    key: &str,
    message: impl Into<String>,
) -> Finding {
    let location = yaml.lines().enumerate().find_map(|(line, text)| {
        let trimmed = text.trim_start();
        let (authored, _) = trimmed.split_once(':')?;
        (authored.trim_matches(['\'', '"']) == key)
            .then(|| (offset + line + 1, text.len() - trimmed.len() + 1))
    });
    let (line, col) = location.unwrap_or((offset.max(1), 1));
    finding(path, line, col, message)
}

impl SectionDoc {
    pub fn finding(&self, path: &str, key: &str, message: impl Into<String>) -> Finding {
        key_finding(path, &self.yaml, self.line, key, message)
    }

    pub fn where_finding(
        &self,
        path: &str,
        query: &str,
        offset: usize,
        message: impl Into<String>,
    ) -> Finding {
        let prefix = &query[..offset];
        // Plain and single-line quoted scalars retain the authored offset. YAML
        // block scalars or escaped strings may have no one-to-one byte mapping.
        if let Some((line, start)) = self
            .yaml
            .lines()
            .enumerate()
            .filter(|(_, line)| line.trim_start().starts_with("where:"))
            .find_map(|(i, line)| {
                line.find(query)
                    .map(|start| (i, line[..start].chars().count()))
            })
        {
            return finding(
                path,
                self.line + line + 1,
                start + prefix.chars().count() + 1,
                message,
            );
        }
        self.finding(path, "where", message)
    }

    pub fn check(&self, doc: &ViewDoc, schema: &FieldSchema) -> Vec<Finding> {
        let block = match &self.block {
            Ok(b) => b,
            Err(finding) => return vec![finding.clone()],
        };
        let mut findings = Vec::new();
        let mut check = |valid: bool, key, message| {
            if !valid {
                findings.push(self.finding(&doc.path, key, message));
            }
        };
        check(
            block.id.as_deref().is_none_or(name),
            "id",
            "Invalid section id",
        );
        check(
            !block.jsonl() || (block.from.ends_with(".jsonl") && relative_path(&block.from)),
            "from",
            "Expected notes, tasks, sections or a vault-relative JSONL path",
        );
        check(
            !block.jsonl()
                || (block.matching.is_none() && block.near.is_none() && block.links.is_none()),
            "from",
            "JSONL sources cannot use match, near or links",
        );
        for (key, text, max) in [
            ("where", &block.filter, 4000),
            ("match", &block.matching, 500),
            ("near", &block.near, 500),
        ] {
            check(
                text.as_ref().is_none_or(|s| s.chars().count() <= max),
                key,
                "Query exceeds the maximum length",
            );
        }
        if let Some(links) = &block.links {
            check(
                links.to.is_some() || links.from.is_some(),
                "links",
                "Expected links.to or links.from",
            );
            check(
                links.depth.is_none_or(|depth| (1..=2).contains(&depth)),
                "links",
                "Link depth must be 1 or 2",
            );
        }
        check(
            block.lets.len() <= 8 && block.lets.keys().all(|s| name(s)),
            "let",
            "Expected up to eight named expressions",
        );
        check(
            block.agg.len() <= 6 && block.agg.keys().all(|s| name(s)),
            "agg",
            "Expected up to six named aggregates",
        );
        for (key, list) in [("group", &block.group), ("sort", &block.sort)] {
            check(
                list.is_none()
                    || (!keys(list).is_empty()
                        && keys(list).len() <= 4
                        && keys(list).iter().all(|s| field_name(unsign(s)))),
                key,
                "Expected one to four field names",
            );
        }
        for (key, list, max) in [("cols", &block.cols, 12), ("levels", &block.levels, 5)] {
            check(
                list.as_ref().is_none_or(|list| {
                    !list.is_empty() && list.len() <= max && list.iter().all(|s| field_name(s))
                }),
                key,
                "Invalid field list",
            );
        }
        check(
            block.at.as_deref().is_none_or(field_name),
            "at",
            "Invalid timeline field",
        );
        check(
            block.order.keys().all(|s| field_name(s)),
            "order",
            "Invalid order field",
        );
        check(
            block
                .node_view
                .as_deref()
                .is_none_or(|value| wikilink(value).is_some()),
            "node_view",
            "Expected a template wikilink",
        );
        check(
            block.limit.is_none_or(|limit| (1..=2000).contains(&limit)),
            "limit",
            "Limit must be from 1 to 2000",
        );
        match block.show {
            Show::Timeline => check(block.at.is_some(), "show", "A timeline requires at"),
            Show::Board => check(block.group.is_some(), "show", "A board requires group"),
            Show::Tree => check(block.levels.is_some(), "show", "A tree requires levels"),
            _ => {}
        }
        if let Some(actions) = &block.actions {
            check(
                (1..=3).contains(&actions.len()),
                "actions",
                "Expected one to three actions",
            );
            for action in actions {
                match action {
                    Action::Scalar(Check::Check) => check(
                        block.from == "tasks",
                        "actions",
                        "check requires from: tasks",
                    ),
                    Action::Mapping(ActionMapping::Set(action)) => {
                        let field = &action.set;
                        check(block.from == "notes", "actions", "set requires from: notes");
                        check(
                            matches!(field.as_str(), "status" | "owner" | "due")
                                && schema.fields.get(field).is_some_and(|f| {
                                    f.writable_by_actions
                                        && !matches!(f.field_type, FieldType::List(_))
                                }),
                            "actions",
                            "set requires a writable scalar note field (status, owner or due)",
                        );
                    }
                    Action::Mapping(ActionMapping::Open(OpenAction { open: Open::Thread })) => {}
                }
            }
        }
        findings.extend(compute::validate(doc, self, block, schema));
        findings.extend(bind::validate(doc, self, block, schema));
        findings
    }
}

pub(super) fn front_checks(doc: &ViewDoc, schema: &FieldSchema) -> Vec<Finding> {
    let mut findings = Vec::new();
    if doc.front.sidebar
        && (doc.blocks.len() != 1
            || doc.blocks[0]
                .block
                .as_ref()
                .is_ok_and(|block| block.show != Show::Tree))
    {
        findings.push(key_finding(
            &doc.path,
            &doc.front_text,
            1,
            "sidebar",
            "A sidebar view requires one tree section",
        ));
    }
    for (name, param) in &doc.front.params {
        if let Some(value) = param.default_value() {
            if let Err(error) = bind::param_value(param.kind(), value, schema) {
                findings.push(key_finding(
                    &doc.path,
                    &doc.front_text,
                    1,
                    "params",
                    format!("Parameter '{name}': {error}"),
                ));
            }
        }
    }
    findings
}

/// Validate without resolving a template on disk or requiring runtime arguments.
pub fn validate(doc: &ViewDoc, schema: &FieldSchema) -> Vec<Finding> {
    let mut findings = front_checks(doc, schema);
    let mut ids = BTreeSet::new();
    for section in &doc.blocks {
        if !ids.insert(&section.id) {
            findings.push(section.finding(
                &doc.path,
                "id",
                format!("Duplicate section id '{}'", section.id),
            ));
        }
        findings.extend(section.check(doc, schema));
    }
    findings
}
