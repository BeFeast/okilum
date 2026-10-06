//! Read-only, generation-bound incoming body note links for managed sources.
use crate::retrieval::{self, IndexStatus, SearchScope, SourceMetadata};
use anyhow::{ensure, Result};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tessera_core::{
    document_links,
    source::SourceSnapshot,
    vault::{visit_wikilinks, ResolutionRef},
    Vault,
};

pub(crate) const MAX_EDGES: usize = 100_000;
pub(crate) const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_PATH: usize = 4096;
const MAX_EXCERPT: usize = 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub path: String,
    pub expected_revision: String,
    pub scope: SearchScope,
    #[serde(default = "default_limit")]
    pub limit: usize,
    pub cursor: Option<String>,
}
fn default_limit() -> usize {
    10
}
impl Request {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            retrieval::valid_path(&self.path) && self.path.len() <= MAX_PATH,
            "invalid backlinks target path"
        );
        ensure!(
            !self.expected_revision.is_empty() && self.expected_revision.len() <= 256,
            "invalid backlinks target revision"
        );
        ensure!(
            (1..=20).contains(&self.limit),
            "backlinks page budget exceeded"
        );
        if self.cursor.as_ref().is_some_and(|c| c.len() > 4096) {
            return Err(cursor_error());
        }
        retrieval::validate_scope(&self.scope)
    }
    fn binding(&self) -> String {
        retrieval::sha(
            serde_json::to_string(&(&self.path, &self.expected_revision, &self.scope))
                .unwrap()
                .as_bytes(),
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Row {
    pub path: String,
    pub title: String,
    pub revision: String,
    pub start_line: usize,
    pub end_line: usize,
    pub excerpt: String,
    pub excerpt_truncated: bool,
    pub link: String,
    pub link_truncated: bool,
    pub ambiguous: bool,
}
#[derive(Debug, Serialize)]
pub struct Target {
    pub path: String,
    pub revision: String,
}
#[derive(Debug, Serialize)]
pub struct Response {
    pub target: Target,
    pub index: IndexStatus,
    pub freshness: String,
    pub rows: Vec<Row>,
    pub next_cursor: Option<String>,
    pub warnings: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Edge {
    target: String,
    row: Row,
    metadata: SourceMetadata,
}
#[derive(Clone, Serialize, Deserialize)]
enum GraphFailure {
    Budget,
    Inventory,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Graph {
    edges: Vec<Edge>,
    #[serde(default)]
    complete: bool,
    unavailable: Option<GraphFailure>,
    #[serde(default)]
    inventory_revision: String,
}
fn prefix(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}
fn excerpt(line: &str, at: usize) -> (String, bool) {
    if line.len() <= MAX_EXCERPT {
        return (line.to_owned(), false);
    }
    let mut start = at
        .saturating_sub(MAX_EXCERPT / 3)
        .min(line.len().saturating_sub(MAX_EXCERPT));
    while !line.is_char_boundary(start) {
        start += 1;
    }
    (prefix(&line[start..], MAX_EXCERPT), true)
}
fn unavailable() -> Graph {
    Graph {
        edges: vec![],
        complete: false,
        unavailable: Some(GraphFailure::Budget),
        inventory_revision: String::new(),
    }
}
impl Graph {
    pub(crate) fn matches_inventory(&self, vault: &Vault) -> bool {
        self.inventory_revision == vault.link_inventory_revision()
    }
    pub(crate) fn build(
        sources: &[SourceSnapshot],
        records_dir: &str,
        markdown_vault: &Vault,
    ) -> Self {
        Self::build_bounded(sources, records_dir, markdown_vault, MAX_EDGES, MAX_BYTES)
    }
    pub(crate) fn build_bounded(
        sources: &[SourceSnapshot],
        records_dir: &str,
        markdown_vault: &Vault,
        edge_cap: usize,
        byte_cap: usize,
    ) -> Self {
        let budget_exceeded = || Self {
            inventory_revision: markdown_vault.link_inventory_revision(),
            ..unavailable()
        };
        if !markdown_vault.inventory_complete {
            return Self {
                unavailable: Some(GraphFailure::Inventory),
                inventory_revision: markdown_vault.link_inventory_revision(),
                ..Self::default()
            };
        }
        if sources.iter().any(|s| s.path.len() > MAX_PATH) {
            return budget_exceeded();
        }
        let resolver = Vault::from_note_paths(sources.iter().map(|s| s.path.clone()));
        let accepted: std::collections::HashSet<_> =
            sources.iter().map(|s| s.path.as_str()).collect();
        let mut edges = BTreeMap::<(String, String, usize), Edge>::new();
        // JSON array commas plus envelope allowance, accounted before each insertion.
        let mut stored_bytes = 256usize;
        let mut exceeded = false;
        for source in sources {
            let Ok(raw) = STANDARD.decode(&source.content_base64) else {
                return budget_exceeded();
            };
            let Ok(raw) = std::str::from_utf8(&raw) else {
                return budget_exceeded();
            };
            // Uses the same ownership validation as chunks. Refresh handles its error separately.
            if retrieval::source_owner(raw, &source.path, records_dir, &source.brain_id).is_err() {
                return budget_exceeded();
            }
            let metadata = retrieval::source_metadata(raw);
            let (_, body_line) = retrieval::metadata(raw);
            let lines: Vec<&str> = raw.split_inclusive('\n').collect();
            let mut starts = Vec::with_capacity(lines.len());
            let mut offset = 0;
            for line in &lines {
                starts.push(offset);
                offset += line.len();
            }
            let body_start: usize = lines.iter().take(body_line).map(|s| s.len()).sum();
            let title = lines
                .iter()
                .skip(body_line)
                .find_map(|l| l.strip_prefix("# "))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(&source.path);
            let title = prefix(title, 256);
            let body = &raw[body_start..];
            let mut occurrences = Vec::new();
            visit_wikilinks(body, |link, range| {
                occurrences.push((range, link.to_owned(), true));
                true
            });
            occurrences.extend(
                document_links::parse_in_vault(body, markdown_vault, &source.path)
                    .into_iter()
                    .filter(|link| !link.wiki)
                    // Attachments (including ambiguous attachment links) are not note edges.
                    .filter(|link| {
                        matches!(
                            document_links::destination(&link.target, false),
                            document_links::Destination::Note { .. }
                        )
                    })
                    .map(|link| (link.range, link.target, false)),
            );
            occurrences.sort_by_key(|(range, _, _)| range.start);
            for (range, link, wiki) in occurrences {
                let at = body_start + range.start;
                let line_index = starts
                    .partition_point(|start| *start <= at)
                    .saturating_sub(1);
                let line_start = starts[line_index];
                let line = lines[line_index].trim_end_matches(['\r', '\n']);
                let (excerpt, excerpt_truncated) = excerpt(line, at - line_start);
                let mut add = |target: &str, ambiguous: bool| {
                    if target == source.path || !accepted.contains(target) {
                        return true;
                    }
                    let key = (target.to_owned(), source.path.clone(), line_index + 1);
                    let previous = edges.get(&key);
                    if previous.is_some_and(|e| !e.row.ambiguous || ambiguous) {
                        return true;
                    }
                    if previous.is_none() && edges.len() >= edge_cap {
                        exceeded = true;
                        return false;
                    }
                    let edge = Edge {
                        target: target.to_owned(),
                        metadata: metadata.clone(),
                        row: Row {
                            path: source.path.clone(),
                            title: title.clone(),
                            revision: source.revision.clone(),
                            start_line: line_index + 1,
                            end_line: line_index + 1,
                            excerpt: excerpt.clone(),
                            excerpt_truncated,
                            link: prefix(&link, 512),
                            link_truncated: link.len() > 512,
                            ambiguous,
                        },
                    };
                    let old_bytes =
                        previous.map_or(0, |e| serde_json::to_vec(e).unwrap().len() + 1);
                    let new_bytes = serde_json::to_vec(&edge).unwrap().len() + 1;
                    let next_bytes = stored_bytes - old_bytes + new_bytes;
                    if next_bytes > byte_cap {
                        exceeded = true;
                        return false;
                    }
                    stored_bytes = next_bytes;
                    edges.insert(key, edge);
                    true
                };
                let keep_going = if wiki {
                    match resolver.resolve_from_ref(&link, &source.path) {
                        ResolutionRef::Resolved(path) => add(path, false),
                        ResolutionRef::Ambiguous(paths) => paths.iter().all(|path| add(path, true)),
                        ResolutionRef::Unresolved => true,
                    }
                } else {
                    let resolved =
                        document_links::resolve(&link, false, markdown_vault, &source.path);
                    match resolved.status {
                        "resolved" => resolved.candidates.iter().all(|path| add(path, false)),
                        "ambiguous" => resolved.candidates.iter().all(|path| add(path, true)),
                        _ => true,
                    }
                };
                if !keep_going {
                    break;
                }
            }
            if exceeded {
                return budget_exceeded();
            }
        }
        Self {
            edges: edges.into_values().collect(),
            complete: true,
            unavailable: None,
            inventory_revision: markdown_vault.link_inventory_revision(),
        }
    }
    pub(crate) fn page(
        &self,
        request: &Request,
        generation: &str,
    ) -> Result<(Vec<Row>, Option<String>)> {
        if let Some(failure) = &self.unavailable {
            let (code, message) = match failure {
                GraphFailure::Budget => ("backlinks_budget_exceeded", "Incoming references exceed the derived graph budget"),
                GraphFailure::Inventory => ("index_unavailable", "Incoming reference inventory is incomplete; refresh after unreadable entries are available"),
            };
            return Err(retrieval::error(code, message));
        }
        if !self.complete {
            return Err(retrieval::error(
                "index_unavailable",
                "Incoming references generation is incomplete; rebuild the index",
            ));
        }
        let start = self.edges.partition_point(|e| e.target < request.path);
        let end = self.edges.partition_point(|e| e.target <= request.path);
        let mut offset = start;
        if let Some(cursor) = &request.cursor {
            let parsed = URL_SAFE_NO_PAD
                .decode(cursor)
                .ok()
                .and_then(|b| serde_json::from_slice::<Cursor>(&b).ok())
                .ok_or_else(cursor_error)?;
            if parsed.generation != generation
                || parsed.binding != request.binding()
                || parsed.index < start
                || parsed.index >= end
            {
                return Err(cursor_error());
            }
            let edge = &self.edges[parsed.index];
            if parsed.key != edge_key(edge) || !edge.visible(&request.scope) {
                return Err(cursor_error());
            }
            offset = parsed.index + 1;
        }
        let mut selected = Vec::new();
        for (i, edge) in self.edges.iter().enumerate().take(end).skip(offset) {
            if edge.visible(&request.scope) {
                selected.push((i, edge));
            }
            if selected.len() > request.limit {
                break;
            }
        }
        let has_more = selected.len() > request.limit;
        selected.truncate(request.limit);
        let next_cursor = has_more.then(|| {
            let (index, edge) = selected.last().unwrap();
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&Cursor {
                    generation: generation.into(),
                    binding: request.binding(),
                    index: *index,
                    key: edge_key(edge),
                })
                .unwrap(),
            )
        });
        Ok((
            selected.into_iter().map(|(_, e)| e.row.clone()).collect(),
            next_cursor,
        ))
    }
}
impl Edge {
    fn visible(&self, scope: &SearchScope) -> bool {
        retrieval::source_in_scope(&self.row.path, &self.metadata, scope)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    generation: String,
    binding: String,
    index: usize,
    key: String,
}
fn edge_key(edge: &Edge) -> String {
    retrieval::sha(
        serde_json::to_string(&(&edge.target, &edge.row.path, edge.row.start_line))
            .unwrap()
            .as_bytes(),
    )
}
fn cursor_error() -> anyhow::Error {
    retrieval::error("backlinks_cursor_invalid", "Incoming references cursor no longer matches this request or generation; reload references")
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    fn source(path: &str, text: &str) -> SourceSnapshot {
        SourceSnapshot {
            schema: "ai-brain/v1".into(),
            brain_id: "01000000-0000-4000-8000-000000000001".into(),
            path: path.into(),
            revision: format!("sha256:{}", retrieval::sha(text.as_bytes())),
            content_base64: STANDARD.encode(text),
            media_type: "text/markdown".into(),
        }
    }
    fn request(path: &str) -> Request {
        Request {
            path: path.into(),
            expected_revision: "revision".into(),
            scope: SearchScope {
                goal_id: Uuid::new_v4().to_string(),
                mode: "project".into(),
                ..Default::default()
            },
            limit: 10,
            cursor: None,
        }
    }
    fn wiki_resolver(sources: &[SourceSnapshot]) -> Vault {
        let mut vault = Vault::from_note_paths(sources.iter().map(|s| s.path.clone()));
        // Existing wiki-only fixtures need no filesystem metadata.
        vault.inventory_complete = true;
        vault
    }
    #[test]
    fn markdown_matches_reader_destinations_without_expanding_accepted_sources() {
        let temp = tempfile::tempdir().unwrap();
        let body = concat!(
            "---\nrelated: '[ignored](../target.md)'\n---\n# Referrer\n",
            "[Sibling](target.md#Heading)\n",
            "[Root](../target.md) [[target]]\n",
            "[Reference][dest]\n",
            "\n",
            "[Maybe](shared.md) [[left/shared|precise]]\n",
            "[Self](./ref.md) [heading](#Here)\n",
            "`[Code](../target.md)`\n",
            "```md\n[Code](../target.md)\n```\n",
            "    [Indented](../target.md)\n",
            "[Broken](../target.md\n",
            "[Missing](./missing.md) [Remote](https://example.com/target.md)\n",
            "![Image](../target.md) [Asset](image.png)\n",
            "[Spaced](Схема %2520.md)\n",
            "[Outside corpus](../unaccepted.md)\n",
            "[Occupied](blocked.md)\n",
            "[Root slash](/target.md)\n",
            "\n[dest]: <./Схема%20%2520.md#Heading>\n",
        );
        let sources = vec![
            source("target.md", "# Heading"),
            source("notes/target.md", "# Heading"),
            source("notes/Схема %20.md", "# Heading"),
            source("left/shared.md", "# Left"),
            source("right/shared.md", "# Right"),
            source("blocked.md", "# Root namesake"),
            source("notes/ref.md", body),
        ];
        for source in &sources {
            let path = temp.path().join(&source.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, STANDARD.decode(&source.content_base64).unwrap()).unwrap();
        }
        std::fs::write(temp.path().join("unaccepted.md"), "# Not indexed").unwrap();
        std::fs::create_dir(temp.path().join("notes/blocked.md")).unwrap();
        let reader = Vault::scan(temp.path()).unwrap();
        let graph = Graph::build(&sources, "records", &reader);
        for (target, expected) in [
            ("target.md", vec![6, 22]),
            ("notes/target.md", vec![5]),
            ("notes/Схема %20.md", vec![7, 19]),
            ("left/shared.md", vec![9]),
            ("right/shared.md", vec![9]),
            ("notes/ref.md", vec![]),
            ("blocked.md", vec![]),
        ] {
            let rows = graph.page(&request(target), "g").unwrap().0;
            assert_eq!(
                rows.iter().map(|r| r.start_line).collect::<Vec<_>>(),
                expected,
                "{target}"
            );
            assert!(rows.iter().all(|r| r.path == "notes/ref.md"));
            let backlinks = reader.backlinks(target);
            assert_eq!(
                backlinks.len(),
                rows.len(),
                "Reader/managed mismatch for {target}"
            );
            for (backlink, row) in backlinks.iter().zip(&rows) {
                assert_eq!(backlink.path, row.path);
                assert_eq!(backlink.ambiguous, row.ambiguous);
                assert_eq!(backlink.context, row.excerpt);
            }
        }
        assert!(graph
            .page(&request("unaccepted.md"), "g")
            .unwrap()
            .0
            .is_empty());
        assert!(!graph.page(&request("left/shared.md"), "g").unwrap().0[0].ambiguous);
        assert!(graph.page(&request("right/shared.md"), "g").unwrap().0[0].ambiguous);
        let mut partial = reader.clone();
        partial.inventory_complete = false;
        let unavailable = Graph::build(&sources, "records", &partial);
        assert!(unavailable
            .page(&request("target.md"), "g")
            .unwrap_err()
            .to_string()
            .contains("incomplete"));
        let mut scoped = request("target.md");
        scoped.limit = 1;
        let (first, cursor) = graph.page(&scoped, "g").unwrap();
        assert_eq!(first[0].start_line, 6);
        assert!(cursor.is_some());
        scoped.cursor = cursor;
        assert_eq!(graph.page(&scoped, "g").unwrap().0[0].start_line, 22);
        assert!(graph.page(&scoped, "stale").is_err());
        scoped.cursor = None;
        scoped.scope.exclude_paths = vec!["notes/ref.md".into()];
        assert!(graph.page(&scoped, "g").unwrap().0.is_empty());
        let budget = Graph::build_bounded(&sources, "records", &reader, 1, MAX_BYTES);
        assert!(budget.matches_inventory(&reader));
        assert!(budget.page(&request("target.md"), "g").is_err());
        assert_eq!(
            std::fs::read_to_string(temp.path().join("notes/ref.md")).unwrap(),
            body
        );
    }

    #[test]
    fn full_source_identity_lines_code_frontmatter_and_precise_dedupe() {
        let long = format!("{} [[../left/shared#Heading|label]] tail", "я".repeat(5000));
        let body = format!("---\nrelated: '[[shared]]'\n---\n# Referrer\n`[[shared]]`\n```md\n[[shared]]\n```\n[[shared|maybe]] [[left/shared#H|precise]]\n![[../left/shared^block]]\n{long}\n");
        let sources = vec![
            source("left/shared.md", "# Left"),
            source("right/shared.md", "# Right"),
            source("notes/ref.md", &body),
            source("left/self.md", "[[./self]]"),
        ];
        let graph = Graph::build(&sources, "records", &wiki_resolver(&sources));
        let left = graph.page(&request("left/shared.md"), "g").unwrap().0;
        assert_eq!(
            left.iter().map(|r| r.start_line).collect::<Vec<_>>(),
            [9, 10, 11]
        );
        assert!(!left[0].ambiguous);
        assert_eq!(left[0].link, "left/shared");
        assert_eq!(left[0].title, "Referrer");
        assert!(left[2].excerpt_truncated && left[2].excerpt.len() <= 1024);
        assert!(left[2].excerpt.contains("[[../left/shared#Heading|label]]"));
        assert_eq!(left[2].excerpt.find('я').unwrap(), 0);
        let right = graph.page(&request("right/shared.md"), "g").unwrap().0;
        assert_eq!(right.len(), 1);
        assert!(right[0].ambiguous);
        assert!(graph
            .page(&request("left/self.md"), "g")
            .unwrap()
            .0
            .is_empty());
    }
    #[test]
    fn cursor_scope_binding_hidden_candidates_and_visibility() {
        let own = Uuid::new_v4().to_string();
        let other = Uuid::new_v4().to_string();
        let sources = vec![source("left/shared.md", "# Target"), source("records/shared.md", &format!("---\nrecord_type: session\ngoal_id: {other}\n---\n# Hidden target\n[[left/shared]]")),
            source("a.md","[[shared]]\n[[left/shared]]"), source("b.md",&format!("---\nrecord_type: decision\ngoal_id: {other}\n---\n[[left/shared]]")),
            source("c.md", &format!("---\ntype: Note\ngoal_id: {other}\n---\n[[left/shared]]"))];
        let graph = Graph::build(&sources, "records", &wiki_resolver(&sources));
        let mut req = request("left/shared.md");
        req.scope.goal_id = own;
        req.limit = 1;
        let (first, cursor) = graph.page(&req, "g").unwrap();
        assert!(first[0].ambiguous);
        assert_eq!(first[0].path, "a.md");
        req.cursor = cursor;
        let (second, cursor) = graph.page(&req, "g").unwrap();
        assert_eq!(second[0].start_line, 2);
        let mut bad = req.clone();
        bad.scope.mode = "goal".into();
        assert!(graph.page(&bad, "g").is_err());
        assert!(graph.page(&req, "other-generation").is_err());
        let mut bad = req.clone();
        bad.expected_revision = "changed".into();
        assert!(graph.page(&bad, "g").is_err());
        req.cursor = cursor;
        let (last, cursor) = graph.page(&req, "g").unwrap();
        assert_eq!(last[0].path, "b.md");
        assert!(cursor.is_none());
        req.cursor = None;
        req.limit = 20;
        req.scope.mode = "goal".into();
        assert_eq!(graph.page(&req, "g").unwrap().0.len(), 2);
        req.scope.exclude_paths = vec!["a.md".into()];
        assert!(graph.page(&req, "g").unwrap().0.is_empty());
        let empty = Graph::default();
        assert!(empty.page(&req, "g").is_err());
    }
    #[test]
    fn expanded_edges_and_escaped_serialized_bytes_fail_without_partial_rows() {
        let sources = vec![
            source("left/shared.md", "# L"),
            source("right/shared.md", "# R"),
            source("ref.md", "[[shared]]"),
        ];
        let failed =
            Graph::build_bounded(&sources, "records", &wiki_resolver(&sources), 1, MAX_BYTES);
        assert!(failed.edges.is_empty());
        assert!(failed
            .page(&request("left/shared.md"), "g")
            .unwrap_err()
            .to_string()
            .contains("budget"));
        let full = Graph::build(&sources, "records", &wiki_resolver(&sources));
        let serialized = serde_json::to_vec(&full).unwrap();
        assert!(serialized.len() < MAX_BYTES);
        let tiny = Graph::build_bounded(
            &sources,
            "records",
            &wiki_resolver(&sources),
            MAX_EDGES,
            128,
        );
        assert!(tiny.edges.is_empty());
        assert!(tiny.unavailable.is_some());
        let cache: Graph = serde_json::from_slice(&serde_json::to_vec(&failed).unwrap()).unwrap();
        assert!(cache.page(&request("left/shared.md"), "g").is_err());
        let oversized_target = "z".repeat(600);
        let sources = vec![
            source(&format!("{oversized_target}.md"), "# T"),
            source(
                "ref.md",
                &format!("# {}\n[[{oversized_target}]]", "я".repeat(200)),
            ),
        ];
        let graph = Graph::build(&sources, "records", &wiki_resolver(&sources));
        let rows = graph
            .page(&request(&format!("{oversized_target}.md")), "g")
            .unwrap()
            .0;
        assert_eq!(rows[0].title.len(), 256);
        assert_eq!(rows[0].link.len(), 512);
        assert!(rows[0].link_truncated);
    }
}
