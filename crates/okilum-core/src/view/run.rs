use super::{
    bind::{self, Clock, Params},
    builtin,
    compute::Computations,
    parse::{
        self, Action, ActionMapping, Block, Check, Open, OpenAction, Part, SectionDoc, Show,
        ViewDoc,
    },
    snapshot::{self, SnapshotJson},
    Finding,
};
use crate::{
    agent_imports::{Failure, RootedSource, Source as _},
    records::{Fields, NoteRecord, Projection, Value},
    search::{SearchDocument, Searcher},
    Resolution, Vault,
};
use chrono::{DateTime, FixedOffset, SecondsFormat};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::{
    cell::OnceCell,
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};

const MAX_JSONL_BYTES: usize = 4 * 1024 * 1024;
const MAX_JSONL_ROWS: usize = 64 * 1024;

pub trait NearProvider {
    /// Return matching vault-relative note paths for a semantic query.
    fn matches(&self, query: &str) -> Result<Vec<String>, String>;
}

type ReadSource<'a> = dyn Fn(&str) -> Option<String> + 'a;

pub struct RunContext<'a> {
    pub projection: &'a Projection,
    /// Link identities from the same publication as the projection.
    pub vault: &'a Vault,
    pub args: BTreeMap<String, Json>,
    pub as_of: DateTime<FixedOffset>,
    pub all_fields: bool,
    pub generation: u64,
    pub near: Option<&'a dyn NearProvider>,
    pub searcher: Option<&'a Searcher>,
    /// Optional exact source reader from a publication. Used for template prose,
    /// section excerpts and an in-memory search index when no searcher is given.
    /// JSONL is always read through the confined vault reader.
    pub read_source: Option<&'a ReadSource<'a>>,
    vault_snapshots: bool,
}

impl<'a> RunContext<'a> {
    pub fn new(projection: &'a Projection, vault: &'a Vault, as_of: DateTime<FixedOffset>) -> Self {
        Self {
            projection,
            vault,
            args: BTreeMap::new(),
            as_of,
            all_fields: false,
            generation: 0,
            near: None,
            searcher: None,
            read_source: None,
            vault_snapshots: false,
        }
    }

    /// Allow `near` to reuse snapshots published in this vault. Consumers must
    /// have access to the whole vault, including any sources in the snapshot.
    pub fn with_vault_snapshots(mut self) -> Self {
        self.vault_snapshots = true;
        self
    }
}

#[derive(Debug, Serialize)]
pub struct ViewResult {
    pub view_api: u8,
    pub view: ViewIdentity,
    pub params: BTreeMap<String, Json>,
    pub as_of: String,
    pub index: Index,
    pub findings: Vec<Finding>,
    pub sections: Vec<Section>,
    #[serde(skip)]
    pub(super) parts: Vec<Part>,
    #[serde(skip)]
    pub heading_labels: BTreeMap<String, String>,
}

/// Presentation order, including Markdown between executable sections.
pub enum ViewPart<'a> {
    Markdown(&'a str),
    Section {
        heading: &'a str,
        section: &'a Section,
    },
}

impl ViewResult {
    pub fn parts(&self) -> impl Iterator<Item = ViewPart<'_>> {
        self.parts.iter().map(|part| match part {
            Part::Markdown(markdown) => ViewPart::Markdown(markdown),
            Part::Section { index, heading } => ViewPart::Section {
                heading,
                section: &self.sections[*index],
            },
        })
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ViewIdentity {
    pub path: String,
    pub rev: String,
    pub extends: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Index {
    pub by: &'static str,
    pub notes: usize,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Error,
    Degraded,
}

#[derive(Debug, Serialize)]
pub struct Section {
    pub id: String,
    pub title: String,
    pub line: usize,
    pub show: Show,
    pub cols: Vec<String>,
    pub status: Status,
    pub source: &'static str,
    pub snapshot_at: Option<String>,
    pub diagnostics: Vec<Finding>,
    pub total: usize,
    pub truncated: bool,
    pub groups: Vec<Group>,
    pub rows: Vec<Row>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nodes: Option<Vec<TreeNode>>,
    #[serde(skip)]
    pub at: Option<String>,
    #[serde(skip)]
    pub node_view: Option<String>,
}

impl Section {
    pub fn resolve_node_view(&self, vault: &Vault, from: &str) -> Result<Option<String>, String> {
        self.node_view
            .as_deref()
            .map(|target| {
                if builtin::is_domain_reference(target) {
                    Ok(builtin::DOMAIN_PATH.to_owned())
                } else {
                    resolve(vault, from, target)
                }
            })
            .transpose()
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Row {
    pub path: String,
    pub line: Option<usize>,
    pub anchor: Option<String>,
    pub rev: String,
    pub group: Vec<Json>,
    pub fields: BTreeMap<String, Json>,
    pub actions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Group {
    pub key: Vec<Json>,
    pub count: usize,
    pub agg: BTreeMap<String, Json>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TreeNode {
    pub field: String,
    pub value: Json,
    pub label: String,
    pub count: usize,
    pub children: Vec<TreeNode>,
    /// Indices into this section's rows, on the last level only.
    pub rows: Vec<usize>,
}

struct Input {
    path: String,
    line: Option<usize>,
    anchor: Option<String>,
    rev: String,
    fields: Fields,
}

struct Runner<'a, 'ctx> {
    ctx: &'a RunContext<'ctx>,
    clock: Clock,
    rooted: OnceCell<Result<RootedSource, Failure>>,
    searcher: OnceCell<Result<Searcher, String>>,
    outgoing: OnceCell<BTreeMap<String, BTreeSet<String>>>,
    source_cache: BTreeMap<String, MarkdownSource>,
    coverage: Option<Vec<Coverage>>,
}

impl Runner<'_, '_> {
    fn read(&self, path: &str, max_bytes: usize) -> Result<String, String> {
        if !parse::relative_path(path) {
            return Err(format!("Invalid vault-relative path '{path}'"));
        }
        let root = self
            .rooted
            .get_or_init(|| RootedSource::new(&self.ctx.vault.root));
        let rooted = root
            .as_ref()
            .map_err(|error| format!("Cannot open vault ({error:?})"))?;
        rooted
            .read(&rooted.root().join(path), max_bytes)
            .map_err(|error| format!("Cannot read '{path}' ({error:?})"))
    }

    fn source(&self, path: &str) -> Result<String, String> {
        if let Some(read) = self.ctx.read_source {
            read(path).ok_or_else(|| format!("Source '{path}' is unavailable in the publication"))
        } else {
            self.read(path, usize::MAX - 1)
        }
    }

    fn template(
        &self,
        doc: &ViewDoc,
        target: &str,
    ) -> Result<Result<ViewDoc, Vec<Finding>>, String> {
        let domain = builtin::is_domain_reference(target);
        let path = if domain {
            builtin::DOMAIN_PATH.to_owned()
        } else {
            self.resolve(doc, target)?
        };
        if !path.starts_with("@views/") || self.ctx.projection.schema().excludes(&path) {
            return Err(format!("Template '{path}' must be a view under @views/"));
        }
        let source = if !domain {
            self.source(&path)?.into()
        } else if let Some(source) = self.ctx.read_source.and_then(|read| read(&path)) {
            source.into()
        } else {
            let source = builtin::domain_source(&self.ctx.vault.root)?;
            if self.ctx.read_source.is_some() && matches!(&source, std::borrow::Cow::Owned(_)) {
                return Err(format!("Source '{path}' is unavailable in the publication"));
            }
            source
        };
        Ok(parse::parse(&path, &source))
    }

    fn searcher(&self) -> Result<&Searcher, String> {
        if let Some(searcher) = self.ctx.searcher {
            return Ok(searcher);
        }
        self.searcher
            .get_or_init(|| {
                let mut documents = Vec::new();
                for note in self.ctx.projection.notes() {
                    let text = self.source(&note.path)?;
                    if rev(note) != parse::revision(text.as_bytes()) {
                        return Err(format!("Source '{}' changed since projection", note.path));
                    }
                    documents.push(SearchDocument {
                        path: note.path.clone(),
                        title: note.fields.get("title").map_or_else(
                            || Vault::title_of(&note.path),
                            |value| bind::value_text(value).into_owned(),
                        ),
                        text,
                    });
                }
                Searcher::build_snapshot_in_memory(self.ctx.vault, &documents, &mut |_, _| Ok(()))
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    fn resolve(&self, doc: &ViewDoc, target: &str) -> Result<String, String> {
        resolve(self.ctx.vault, &doc.path, target)
    }

    fn reachable(&self, start: &str, depth: usize, outgoing: bool) -> BTreeSet<String> {
        let mut seen = BTreeSet::from([start.to_owned()]);
        let mut frontier = vec![start.to_owned()];
        for _ in 0..depth {
            let mut layer = Vec::new();
            for path in frontier {
                let next = if outgoing {
                    self.outgoing
                        .get_or_init(|| {
                            let mut graph = BTreeMap::new();
                            for target in &self.ctx.vault.notes {
                                for link in self
                                    .ctx
                                    .vault
                                    .backlinks(&target.path)
                                    .into_iter()
                                    .filter(|link| !link.ambiguous)
                                {
                                    graph
                                        .entry(link.path)
                                        .or_insert_with(BTreeSet::new)
                                        .insert(target.path.clone());
                                }
                            }
                            graph
                        })
                        .get(&path)
                        .map_or_else(Vec::new, |targets| targets.iter().cloned().collect())
                } else {
                    self.ctx
                        .vault
                        .backlinks(&path)
                        .into_iter()
                        .filter(|link| !link.ambiguous)
                        .map(|link| link.path)
                        .collect()
                };
                layer.extend(next.into_iter().filter(|path| seen.insert(path.clone())));
            }
            frontier = layer;
        }
        seen.remove(start);
        seen
    }

    fn markdown(&mut self, input: &Input, block: &Block) -> Result<Option<String>, String> {
        if block.show != Show::Sections || block.jsonl() {
            return Ok(None);
        }
        if !self.source_cache.contains_key(&input.path) {
            let source = self.source(&input.path)?;
            if input.rev != parse::revision(source.as_bytes()) {
                return Err(format!("Source '{}' changed since projection", input.path));
            }
            self.source_cache
                .insert(input.path.clone(), MarkdownSource::new(source));
        }
        let source = &self.source_cache[&input.path];
        let markdown = if let Some(line) = input.line {
            source.heading(line).ok_or_else(|| {
                format!("Heading at line {line} is unavailable in '{}'", input.path)
            })?
        } else {
            crate::render::without_frontmatter(&source.text)
        };
        Ok(Some(markdown.trim_matches(['\r', '\n']).to_owned()))
    }

    fn jsonl(&self, authored_path: &str) -> Result<Vec<Input>, String> {
        let path = parse::normalized_path(authored_path);
        let schema = self.ctx.projection.schema();
        if schema.excludes(&path) || self.ctx.vault.nested_boundary(&path).is_some() {
            return Ok(vec![]);
        }
        let source = self.read(&path, MAX_JSONL_BYTES)?;
        let rev = parse::revision(source.as_bytes());
        let declared = schema.jsonl_fields.get(&path);
        let mut rows = Vec::new();
        for (index, line) in source.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            if rows.len() >= MAX_JSONL_ROWS {
                return Err(format!(
                    "'{path}' exceeds the {MAX_JSONL_ROWS}-row JSONL limit"
                ));
            }
            let line_no = index + 1;
            let raw: BTreeMap<String, Json> =
                serde_json::from_str(line).map_err(|error| format!("{path}:{line_no}: {error}"))?;
            let mut fields: Fields = raw
                .into_iter()
                .map(|(key, value)| (key, json_value(value)))
                .collect();
            if let Some(declared) = declared {
                for (key, field) in declared {
                    if field.field_type.is_opaque() {
                        continue;
                    }
                    if let Some(raw) = bind::get(&fields, key) {
                        let text = bind::value_text(raw);
                        let value = bind::field_literal(field, &text, self.clock.zone).ok_or_else(
                            || format!("{path}:{line_no}: Invalid value for '{key}'"),
                        )?;
                        fields.insert(key.clone(), value);
                    }
                }
            }
            rows.push(Input {
                path: path.clone(),
                line: Some(line_no),
                anchor: None,
                rev: rev.clone(),
                fields,
            });
        }
        Ok(rows)
    }

    fn projected(&self, block: &Block, allowed: impl Fn(&str) -> bool) -> Vec<Input> {
        let mut rows = Vec::new();
        let today = self.clock.today();
        for note in self
            .ctx
            .projection
            .notes()
            .filter(|note| allowed(&note.path))
        {
            let mut fields = note.fields.clone();
            for key in ["stale", "backlinks"] {
                if let Some(value) = today.and_then(|day| note.value(key, self.ctx.vault, day)) {
                    fields.insert(key.into(), value.into_owned());
                }
            }
            let rev = rev(note);
            let input = |line, anchor, fields| Input {
                path: note.path.clone(),
                line,
                anchor,
                rev: rev.clone(),
                fields,
            };
            if block.from == "notes" {
                rows.push(input(None, None, fields));
                continue;
            }
            let subrow = |line, anchor, child: &Fields| {
                let mut merged: Fields = fields
                    .iter()
                    .map(|(k, v)| (format!("note.{k}"), v.clone()))
                    .collect();
                for key in self.ctx.projection.schema().builtins.keys() {
                    if let Some(value) = fields.get(key) {
                        merged.insert(key.clone(), value.clone());
                    }
                }
                merged.extend(child.clone());
                input(Some(line), anchor, merged)
            };
            if block.from == "tasks" {
                rows.extend(
                    note.tasks
                        .iter()
                        .map(|task| subrow(task.line, None, &task.fields)),
                );
            } else {
                rows.extend(note.sections.iter().map(|section| {
                    subrow(section.line, Some(section.anchor.clone()), &section.fields)
                }));
            }
        }
        rows
    }

    fn execute(
        &mut self,
        doc: &ViewDoc,
        section: &SectionDoc,
        block: &Block,
        vars: &Params,
        budget: &mut Option<usize>,
        result: &mut Section,
    ) -> Result<(), Finding> {
        let failure = |key, message: String| section.finding(&doc.path, key, message);
        let query = if self.coverage.is_some() {
            None
        } else {
            bind::bind(
                doc,
                section,
                block,
                self.ctx.projection.schema(),
                vars,
                self.clock,
            )?
        };
        let mut computations = Computations::compile(block);
        computations
            .bind(block, self.ctx.projection.schema(), vars, self.clock)
            .map_err(|error| failure("let", error))?;
        let mut filters = Vec::new();
        if let Some(text) = &block.near {
            let text = text_arg(text, vars).map_err(|error| failure("near", error))?;
            let Some(near) = self.ctx.near else {
                result.status = Status::Degraded;
                result.diagnostics.push(bind::warn(failure(
                    "near",
                    "near needs the search service".into(),
                )));
                return Ok(());
            };
            filters.push(
                near.matches(&text)
                    .map_err(|error| failure("near", error))?
                    .into_iter()
                    .collect::<BTreeSet<_>>(),
            );
        }
        if let Some(links) = &block.links {
            for (target, outgoing) in [(&links.from, true), (&links.to, false)] {
                if let Some(target) = target {
                    let target = text_arg(target, vars)
                        .and_then(|target| self.resolve(doc, &target))
                        .map_err(|error| failure("links", error))?;
                    filters.push(self.reachable(&target, links.depth.unwrap_or(1), outgoing));
                }
            }
        }
        if let Some(text) = &block.matching {
            let text = text_arg(text, vars).map_err(|error| failure("match", error))?;
            let paths: Vec<_> = self
                .ctx
                .projection
                .notes()
                .map(|note| note.path.clone())
                .collect();
            filters.push(
                self.searcher()
                    .and_then(|searcher| {
                        searcher
                            .search_in_paths(&text, paths.len(), &paths)
                            .map_err(|error| error.to_string())
                    })
                    .map_err(|error| failure("match", error))?
                    .into_iter()
                    .map(|hit| hit.path)
                    .collect(),
            );
        }
        let mut rows = if block.jsonl() {
            self.jsonl(&block.from)
                .map_err(|error| failure("from", error))?
        } else {
            self.projected(block, |path| {
                filters.iter().all(|filter| filter.contains(path))
            })
        };
        let mut missing = BTreeMap::<String, BTreeSet<String>>::new();
        if self.coverage.is_some() {
            if let Some(filter) = &block.filter {
                let (_, syntax) =
                    crate::log::query::Query::parse_view(filter).map_err(|error| {
                        section.where_finding(&doc.path, filter, error.span.start, error.message)
                    })?;
                missing.extend(
                    syntax
                        .fields
                        .into_iter()
                        .map(|(field, _)| (field, BTreeSet::new())),
                );
            }
        }
        rows.retain_mut(|row| {
            computations.apply(&mut row.fields, self.clock);
            for (field, paths) in &mut missing {
                if bind::get(&row.fields, field).is_none() {
                    paths.insert(row.path.clone());
                }
            }
            query
                .as_ref()
                .is_none_or(|query| query.matches(&row.fields))
        });
        if let Some(coverage) = &mut self.coverage {
            coverage.extend(missing.into_iter().map(|(field, missing)| Coverage {
                view: doc.path.clone(),
                section: section.id.clone(),
                field,
                missing,
            }));
            return Ok(());
        }
        rows.sort_by(|left, right| {
            compare_keys(left, right, block.group(), Some(&block.order))
                .then_with(|| compare_keys(left, right, block.sort(), None))
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.line.cmp(&right.line))
        });
        result.total = rows.len();
        result.groups = groups(&rows, block, &computations, self.ctx.vault);
        if block.jsonl() && block.cols.is_none() {
            result.cols = rows
                .iter()
                .flat_map(|row| row.fields.keys())
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
        }
        let fields: BTreeSet<_> = result
            .cols
            .iter()
            .map(String::as_str)
            .chain(
                block
                    .group()
                    .iter()
                    .chain(block.sort())
                    .map(|key| parse::unsign(key)),
            )
            .chain(block.at.as_deref())
            .chain(block.lets.keys().map(String::as_str))
            .collect();

        // Commit this section's budget only once its entire output is valid.
        let mut remaining = *budget;
        let mut retained = Vec::new();
        let mut group_keys = result
            .groups
            .iter()
            .flat_map(|group| std::iter::repeat_n(&group.key, group.count));
        for input in rows.into_iter().take(block.limit.unwrap_or(usize::MAX)) {
            if remaining == Some(0) {
                result.truncated = true;
                break;
            }
            let markdown = self
                .markdown(&input, block)
                .map_err(|error| failure("show", error))?;
            let group = group_keys.next().cloned().unwrap_or_default();
            let row = output_row(&input, block, &fields, markdown, group, self.ctx);
            if let Some(bytes) = &mut remaining {
                let size = serde_json::to_vec(&row)
                    .expect("serializable view row")
                    .len();
                if size > *bytes {
                    *bytes = 0;
                    result.truncated = true;
                    break;
                }
                *bytes -= size;
            }
            if block.show == Show::Tree {
                retained.push(input);
            }
            result.rows.push(row);
        }
        if let Some(levels) = block.levels.as_ref().filter(|_| block.show == Show::Tree) {
            result.nodes = Some(tree(
                &retained,
                (0..retained.len()).collect(),
                levels,
                &block.order,
                self.ctx.vault,
            ));
        }
        *budget = remaining;
        Ok(())
    }
}

struct MarkdownSource {
    text: String,
    lines: Vec<usize>,
    headings: BTreeMap<usize, (usize, u8)>,
}

impl MarkdownSource {
    fn new(text: String) -> Self {
        use comrak::{nodes::NodeValue, parse_document, Arena};
        let body = crate::render::without_frontmatter(&text);
        let prefix = text[..text.len() - body.len()]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        let arena = Arena::new();
        let root = parse_document(&arena, body, &crate::render::comrak_options());
        let headings = root
            .descendants()
            .filter_map(|node| {
                let data = node.data.borrow();
                let NodeValue::Heading(heading) = &data.value else {
                    return None;
                };
                Some((
                    prefix + data.sourcepos.start.line,
                    (prefix + data.sourcepos.end.line, heading.level),
                ))
            })
            .collect();
        let lines = crate::render::markdown_line_offsets(&text);
        Self {
            text,
            lines,
            headings,
        }
    }

    fn heading(&self, line: usize) -> Option<&str> {
        use std::ops::Bound::{Excluded, Unbounded};
        let (after, level) = *self.headings.get(&line)?;
        let start = self.lines.get(after).copied().unwrap_or(self.text.len());
        let end = self
            .headings
            .range((Excluded(line), Unbounded))
            .find(|(_, (_, next))| *next <= level)
            .and_then(|(line, _)| self.lines.get(line - 1))
            .copied()
            .unwrap_or(self.text.len());
        self.text.get(start..end)
    }
}

fn rev(note: &NoteRecord) -> String {
    match note.fields.get("rev") {
        Some(Value::String(rev)) => rev.clone(),
        _ => unreachable!("projection always records the source revision"),
    }
}

fn json_value(value: Json) -> Value {
    match value {
        Json::Null => Value::Null,
        Json::Bool(value) => Value::Bool(value),
        Json::Number(value) => Value::Number(value),
        Json::String(value) => Value::String(value),
        Json::Array(values) => Value::List(values.into_iter().map(json_value).collect()),
        Json::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(key, value)| (key, json_value(value)))
                .collect(),
        ),
    }
}

fn text_arg(text: &str, vars: &Params) -> Result<String, String> {
    if let Some(name) = text.strip_prefix('$') {
        vars.get(name)
            .map(|value| bind::value_text(value).into_owned())
            .ok_or_else(|| format!("Missing parameter '{name}'"))
    } else {
        Ok(text.to_owned())
    }
}

fn compare_key(
    left: Option<&Value>,
    right: Option<&Value>,
    descending: bool,
    order: Option<&Vec<String>>,
) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (Some(left), Some(right)) => {
            if let Some(order) = order {
                let rank = |value: &Value| {
                    order
                        .iter()
                        .position(|item| item == bind::value_text(value).as_ref())
                        .unwrap_or(order.len())
                };
                return rank(left)
                    .cmp(&rank(right))
                    .then_with(|| bind::order(left, right));
            }
            let comparison = bind::order(left, right);
            if descending {
                comparison.reverse()
            } else {
                comparison
            }
        }
    }
}

fn compare_keys(
    left: &Input,
    right: &Input,
    keys: &[String],
    order: Option<&BTreeMap<String, Vec<String>>>,
) -> Ordering {
    keys.iter()
        .map(|key| {
            let name = parse::unsign(key);
            compare_key(
                bind::get(&left.fields, name),
                bind::get(&right.fields, name),
                key.starts_with('-'),
                order.and_then(|order| order.get(name)),
            )
        })
        .find(|ordering| !ordering.is_eq())
        .unwrap_or(Ordering::Equal)
}

fn group_key(row: &Input, keys: &[String], vault: &Vault) -> Vec<Json> {
    keys.iter()
        .map(|key| {
            bind::get(&row.fields, parse::unsign(key))
                .map_or(Json::Null, |value| value.to_json(vault, &row.path))
        })
        .collect()
}

fn groups(rows: &[Input], block: &Block, computed: &Computations, vault: &Vault) -> Vec<Group> {
    if block.group().is_empty() && computed.aggregates.is_empty() {
        return vec![];
    }
    let group = |rows: &[Input]| Group {
        key: rows
            .first()
            .map_or_else(Vec::new, |first| group_key(first, block.group(), vault)),
        count: rows.len(),
        agg: computed
            .aggregates
            .iter()
            .map(|(name, (aggregate, field))| {
                let value = aggregate.compute(
                    rows.len(),
                    rows.iter()
                        .filter_map(|row| bind::get(&row.fields, field.as_ref()?)),
                );
                (
                    name.clone(),
                    value.map_or(Json::Null, |value| {
                        value.to_json(vault, rows.first().map_or("", |first| &first.path))
                    }),
                )
            })
            .collect(),
    };
    if rows.is_empty() && block.group().is_empty() {
        return vec![group(rows)];
    }
    rows.chunk_by(|left, right| {
        compare_keys(left, right, block.group(), Some(&block.order)).is_eq()
    })
    .map(group)
    .collect()
}

fn resolve(vault: &Vault, from: &str, target: &str) -> Result<String, String> {
    let target = parse::wikilink(target).unwrap_or(target);
    match vault.resolve_from(target, from) {
        Resolution::Resolved { path } => Ok(path),
        Resolution::Unresolved => Err(format!("Unresolved link '{target}'")),
        Resolution::Ambiguous { mut candidates } => {
            candidates.sort();
            Err(format!(
                "Ambiguous link '{target}': {}",
                candidates.join(", ")
            ))
        }
    }
}

fn tree(
    rows: &[Input],
    mut members: Vec<usize>,
    levels: &[String],
    order: &BTreeMap<String, Vec<String>>,
    vault: &Vault,
) -> Vec<TreeNode> {
    let Some((key, levels)) = levels.split_first() else {
        return vec![];
    };
    let cmp = |left: &usize, right: &usize| {
        compare_key(
            bind::get(&rows[*left].fields, key),
            bind::get(&rows[*right].fields, key),
            false,
            order.get(key),
        )
    };
    members.sort_by(cmp);
    members
        .chunk_by(|left, right| cmp(left, right).is_eq())
        .map(|members| {
            let row = &rows[members[0]];
            let value = bind::get(&row.fields, key);
            TreeNode {
                field: key.clone(),
                value: value.map_or(Json::Null, |v| v.to_json(vault, &row.path)),
                label: value
                    .map_or_else(|| "(none)".to_owned(), |v| bind::value_text(v).into_owned()),
                count: members.len(),
                children: tree(rows, members.to_vec(), levels, order, vault),
                rows: if levels.is_empty() {
                    members.to_vec()
                } else {
                    vec![]
                },
            }
        })
        .collect()
}

fn output_row(
    input: &Input,
    block: &Block,
    fields: &BTreeSet<&str>,
    markdown: Option<String>,
    group: Vec<Json>,
    ctx: &RunContext<'_>,
) -> Row {
    let mut actions = vec!["open".to_owned()];
    for action in block.actions.iter().flatten() {
        match action {
            Action::Scalar(Check::Check) => actions.push("check".into()),
            Action::Mapping(ActionMapping::Set(action)) => {
                actions.push(format!("set:{}", action.set))
            }
            Action::Mapping(ActionMapping::Open(OpenAction { open: Open::Thread })) => {
                if input
                    .fields
                    .get("thread")
                    .or_else(|| input.fields.get("note.thread"))
                    .is_some_and(|v| matches!(v, Value::String(value) if !value.is_empty()))
                {
                    actions.push("open:thread".into());
                }
            }
        }
    }
    Row {
        path: input.path.clone(),
        line: input.line,
        anchor: input.anchor.clone(),
        rev: input.rev.clone(),
        group,
        fields: {
            let chosen = fields.iter().copied().chain(
                ctx.all_fields
                    .then_some(input.fields.keys().map(String::as_str))
                    .into_iter()
                    .flatten(),
            );
            chosen
                .filter_map(|key| {
                    bind::get(&input.fields, key)
                        .map(|value| (key.to_owned(), value.to_json(ctx.vault, &input.path)))
                })
                .collect()
        },
        actions,
        markdown,
    }
}

fn new_section(section: &SectionDoc, title: String) -> Section {
    let block = section.block.as_ref().ok();
    let cols = block.map_or_else(Vec::new, |block| {
        block.cols.clone().unwrap_or_else(|| {
            let key = match block.from.as_str() {
                "notes" => "title",
                "tasks" => "text",
                "sections" => "heading",
                _ => return vec![],
            };
            vec![key.to_owned()]
        })
    });
    Section {
        id: section.id.clone(),
        title,
        line: section.line,
        show: block.map_or(Show::Table, |b| b.show),
        cols,
        status: Status::Ok,
        source: "live",
        snapshot_at: None,
        diagnostics: Vec::new(),
        total: 0,
        truncated: false,
        groups: Vec::new(),
        rows: Vec::new(),
        nodes: None,
        at: block.and_then(|b| b.at.clone()),
        node_view: block.and_then(|b| b.node_view.clone()),
    }
}

/// Evaluate sections independently over one typed publication at a caller-fixed
/// time. This function reads sources and never changes the vault.
pub fn run(doc: &ViewDoc, ctx: &RunContext<'_>) -> ViewResult {
    execute(doc, &mut runner(ctx, None))
}

#[derive(Debug, Serialize)]
pub struct Coverage {
    pub view: String,
    pub section: String,
    pub field: String,
    pub missing: BTreeSet<String>,
}

/// Report field presence after source narrowing and computed fields, before
/// applying the where expression, row limit or output budget.
pub fn coverage(doc: &ViewDoc, ctx: &RunContext<'_>) -> Vec<Coverage> {
    let mut runner = runner(ctx, Some(Vec::new()));
    execute(doc, &mut runner);
    runner.coverage.expect("coverage was enabled")
}

fn runner<'a, 'ctx>(
    ctx: &'a RunContext<'ctx>,
    coverage: Option<Vec<Coverage>>,
) -> Runner<'a, 'ctx> {
    Runner {
        ctx,
        clock: Clock {
            at: ctx.as_of,
            zone: ctx.projection.schema().timezone,
        },
        rooted: OnceCell::new(),
        searcher: OnceCell::new(),
        outgoing: OnceCell::new(),
        source_cache: BTreeMap::new(),
        coverage,
    }
}

fn execute(doc: &ViewDoc, runner: &mut Runner<'_, '_>) -> ViewResult {
    let ctx = runner.ctx;
    let schema = ctx.projection.schema();
    let mut result = ViewResult {
        view_api: 0,
        view: ViewIdentity {
            path: doc.path.clone(),
            rev: doc.rev.clone(),
            extends: doc.front.extends.clone(),
        },
        params: BTreeMap::new(),
        as_of: ctx.as_of.to_rfc3339_opts(SecondsFormat::AutoSi, false),
        index: Index {
            by: concat!("okilum-core ", env!("CARGO_PKG_VERSION")),
            notes: ctx.projection.notes().count(),
            generation: ctx.generation,
        },
        findings: parse::front_checks(doc, schema),
        sections: Vec::new(),
        parts: Vec::new(),
        heading_labels: BTreeMap::new(),
    };
    let template = if let Some(target) = &doc.front.extends {
        match runner.template(doc, target) {
            Ok(Ok(template)) if template.front.extends.is_none() => Some(template),
            Ok(Err(findings)) => {
                result.findings.extend(findings);
                return result;
            }
            other => {
                let error = other
                    .err()
                    .unwrap_or_else(|| "View templates can extend only one level".into());
                result.findings.push(parse::finding(&doc.path, 1, 1, error));
                return result;
            }
        }
    } else {
        None
    };
    let definition = template.as_ref().unwrap_or(doc);
    if let Some(template) = &template {
        result.view.extends = Some(template.path.clone());
        result
            .findings
            .extend(parse::front_checks(template, schema));
    }
    let mut provided = definition.front.args.clone().unwrap_or_default();
    if template.is_some() {
        provided.extend(
            doc.front
                .args
                .iter()
                .flatten()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
    provided.extend(ctx.args.clone());
    let (vars, findings) = bind::params(definition, &provided, schema);
    result.findings.extend(findings);
    result.params = vars
        .iter()
        .map(|(key, value)| (key.clone(), value.to_json(ctx.vault, &doc.path)))
        .collect();
    result.heading_labels = super::render::bind_headings(doc, &vars, &mut result.findings);
    if template.is_some() {
        super::render::bind_headings(definition, &vars, &mut result.findings);
        result
            .parts
            .extend(super::render::substitute_parts(&doc.parts, &vars));
    }
    result
        .parts
        .extend(super::render::substitute_parts(&definition.parts, &vars));
    let mut budget = doc
        .front
        .budget
        .as_deref()
        .or(definition.front.budget.as_deref())
        .and_then(parse::budget);
    let mut ids = BTreeSet::new();
    let mut snapshot = None;
    for section in &definition.blocks {
        let title = super::render::substitute(&section.title, &vars, false);
        let mut output = new_section(section, title);
        output.diagnostics = section.check(definition, schema);
        if !ids.insert(&section.id) {
            output.diagnostics.push(section.finding(
                &definition.path,
                "id",
                format!("Duplicate section id '{}'", section.id),
            ));
        }
        if !output.diagnostics.iter().any(Finding::is_error) {
            let block = section.block.as_ref().expect("validated section");
            if let Err(finding) =
                runner.execute(definition, section, block, &vars, &mut budget, &mut output)
            {
                output.diagnostics.push(finding);
            }
        }
        if output.diagnostics.iter().any(Finding::is_error) {
            output.status = Status::Error;
            output.rows.clear();
            output.groups.clear();
            output.total = 0;
            output.truncated = false;
        }
        if output.status == Status::Degraded && ctx.vault_snapshots && runner.coverage.is_none() {
            let snapshot = snapshot.get_or_insert_with(|| {
                let path = snapshot::path(&result.view.path)?;
                let source = runner.read(&path, snapshot::MAX_BYTES).ok()?;
                serde_json::from_str::<SnapshotJson>(&source).ok()
            });
            if let Some(snapshot) = snapshot {
                snapshot.reuse(&result, &mut output);
            }
        }
        result.sections.push(output);
    }
    result
}
