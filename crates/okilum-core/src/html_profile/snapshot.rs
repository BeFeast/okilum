use super::{
    anchor, asset_link, finish_document, html, http_link, note_url, start_document, Fragment, Link,
};
use crate::{
    document_links::{self, Destination},
    view::{Finding, Group, Row, Status, ViewIdentity},
};
use chrono::{DateTime, FixedOffset};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::{collections::BTreeMap, fmt::Write};

/// A sealed C2 snapshot (snapshot_version 1, view_api 0). Deserialize it from
/// the published JSON. The gateway authorizes the snapshot and its targets.
#[derive(Debug, Deserialize)]
#[serde(try_from = "Envelope")]
pub struct SnapshotJson {
    generated_at: Timestamp,
    findings: Vec<Finding>,
    sections: Vec<Section>,
}

#[derive(Deserialize)]
struct Envelope {
    snapshot_version: u64,
    view_api: u64,
    generated_at: Timestamp,
    view: ViewIdentity,
    #[serde(rename = "params")]
    _params: Map<String, Value>,
    #[serde(rename = "as_of")]
    _as_of: Timestamp,
    #[serde(rename = "index")]
    _index: Index,
    findings: Vec<Finding>,
    sections: Vec<Section>,
}

#[derive(Deserialize)]
struct Index {
    #[serde(rename = "by")]
    _by: String,
    #[serde(rename = "notes")]
    _notes: usize,
    #[serde(rename = "generation")]
    _generation: u64,
}

impl TryFrom<Envelope> for SnapshotJson {
    type Error = String;

    fn try_from(wire: Envelope) -> Result<Self, Self::Error> {
        if (wire.snapshot_version, wire.view_api) != (1, 0) {
            return Err(format!(
                "Unsupported view snapshot version pair: snapshot_version {}, view_api {}",
                wire.snapshot_version, wire.view_api
            ));
        }
        if !valid_path(&wire.view.path)
            || !wire.view.path.starts_with("@views/")
            || wire.view.path.starts_with("@views/out/")
            || !wire.view.path.ends_with(".md")
            || !valid_revision(&wire.view.rev)
        {
            return Err("Invalid view snapshot identity".into());
        }
        for section in &wire.sections {
            if (section.source == Source::Snapshot)
                != (section.status == Status::Ok && section.snapshot_at.is_some())
                || (section.source == Source::Live && section.snapshot_at.is_some())
            {
                return Err(format!(
                    "Invalid provenance in view section '{}'",
                    section.id
                ));
            }
            if section.rows.iter().any(|row| {
                !valid_path(&row.path) || !valid_revision(&row.rev) || row.line == Some(0)
            }) {
                return Err(format!(
                    "Invalid row source in view section '{}'",
                    section.id
                ));
            }
        }
        Ok(Self {
            generated_at: wire.generated_at,
            findings: wire.findings,
            sections: wire.sections,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(try_from = "String")]
struct Timestamp {
    text: String,
    value: DateTime<FixedOffset>,
}

impl TryFrom<String> for Timestamp {
    type Error = chrono::ParseError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let value = DateTime::parse_from_rfc3339(&text)?;
        Ok(Self { text, value })
    }
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Source {
    Live,
    Snapshot,
}

#[derive(Debug, Deserialize)]
struct Section {
    id: String,
    title: String,
    #[serde(rename = "line")]
    _line: usize,
    show: String,
    cols: Vec<String>,
    status: Status,
    source: Source,
    snapshot_at: Option<Timestamp>,
    diagnostics: Vec<Finding>,
    total: usize,
    truncated: bool,
    groups: Vec<Group>,
    rows: Vec<Row>,
}

/// Render the already evaluated result without reading the vault or running
/// the view. Source links carry the sealed revision and optional navigation
/// hints. The native host decides how to open them if only newer bytes exist.
pub fn render_view_snapshot(snapshot: &SnapshotJson) -> String {
    let mut out = start_document();
    timestamp(&mut out, &snapshot.generated_at);
    findings(&mut out, &snapshot.findings);
    for (index, section) in snapshot.sections.iter().enumerate() {
        section.render(&mut out, index + 1);
    }
    finish_document(out)
}

fn timestamp(out: &mut String, time: &Timestamp) {
    let clock = time.value.with_timezone(&chrono_tz::Asia::Jerusalem);
    out.push_str("<p>snapshot <time datetime=\"");
    text(out, &time.text);
    writeln!(out, "\">{}</time></p>", clock.format("%H:%M")).unwrap();
}

fn findings(out: &mut String, findings: &[Finding]) {
    for finding in findings {
        out.push_str("<p>");
        text(out, &finding.path);
        write!(out, ":{}:{} · ", finding.line, finding.col).unwrap();
        text(out, &finding.message);
        out.push_str("</p>\n");
    }
}

impl Section {
    fn render(&self, out: &mut String, index: usize) {
        let shown = match self.show.as_str() {
            "table" => "table",
            "list" => "list",
            "sections" => "sections",
            "log" => "log",
            _ => "table",
        };
        writeln!(
            out,
            "<section id=\"section-{index}\" data-show=\"{shown}\">"
        )
        .unwrap();
        if !self.title.is_empty() {
            out.push_str("<h2>");
            text(out, &self.title);
            out.push_str("</h2>\n");
        }
        match self.status {
            Status::Ok => {}
            Status::Error => out.push_str("<p>Section error</p>\n"),
            Status::Degraded => out.push_str("<p>Unavailable</p>\n"),
        }
        findings(out, &self.diagnostics);
        if self.status == Status::Ok {
            self.render_results(out, shown);
        }
        out.push_str("</section>\n");
    }

    fn render_results(&self, out: &mut String, shown: &str) {
        if let Some(time) = &self.snapshot_at {
            timestamp(out, time);
        }
        if self.show != shown {
            let label = match self.show.as_str() {
                "board" => "Board shown",
                "timeline" => "Timeline shown",
                "cards" => "Cards shown",
                "graph" => "Graph shown",
                "tree" => "Tree shown",
                _ => "Shown",
            };
            writeln!(out, "<p>{label} as table</p>").unwrap();
        }
        if self.rows.is_empty() && self.total == 0 {
            out.push_str("<p>No results.</p>\n");
        }
        if self.truncated {
            out.push_str("<p>Output budget reached.</p>\n");
        }
        if self.rows.len() < self.total {
            writeln!(out, "<p>Showing {} of {}.</p>", self.rows.len(), self.total).unwrap();
        }
        let mut remaining = self.rows.as_slice();
        for group in &self.groups {
            if remaining.is_empty() {
                break;
            }
            let (rows, rest) = remaining.split_at(group.count.min(remaining.len()));
            remaining = rest;
            if rows.is_empty() {
                continue;
            }
            render_group_header(out, group);
            self.render_rows(out, shown, rows, !group.key.is_empty());
        }
        self.render_rows(out, shown, remaining, false);
    }

    fn render_rows(&self, out: &mut String, shown: &str, rows: &[Row], grouped: bool) {
        if rows.is_empty() {
            return;
        }
        match shown {
            "list" => {
                out.push_str("<ul>\n");
                for row in rows {
                    out.push_str("<li>");
                    if write_values(out, &self.cols, row) {
                        out.push_str(" — ");
                    }
                    render_source(out, &self.cols, row);
                    out.push_str("</li>\n");
                }
                out.push_str("</ul>\n");
            }
            "sections" => {
                let level = if grouped { 4 } else { 3 };
                for row in rows {
                    write!(out, "<article>\n<h{level}>").unwrap();
                    let has_values = write_values(out, &self.cols, row);
                    if !has_values {
                        render_source(out, &self.cols, row);
                    }
                    writeln!(out, "</h{level}>").unwrap();
                    if let Some(markdown) = &row.markdown {
                        Fragment::Snapshot.render(&row.path, markdown, out).unwrap();
                    }
                    if has_values {
                        out.push_str("<p>");
                        render_source(out, &self.cols, row);
                        out.push_str("</p>\n");
                    }
                    out.push_str("</article>\n");
                }
            }
            _ => self.render_table(out, rows),
        }
    }

    fn render_table(&self, out: &mut String, rows: &[Row]) {
        out.push_str("<table>\n<thead><tr>");
        for col in &self.cols {
            out.push_str("<th scope=\"col\">");
            text(out, col);
            out.push_str("</th>");
        }
        out.push_str("<th scope=\"col\">Source</th></tr></thead>\n<tbody>\n");
        for row in rows {
            out.push_str("<tr>");
            for col in &self.cols {
                out.push_str("<td>");
                if let Some(value) = lookup(&row.fields, col) {
                    cell(out, value);
                }
                out.push_str("</td>");
            }
            out.push_str("<td>");
            render_source(out, &self.cols, row);
            out.push_str("</td></tr>\n");
        }
        out.push_str("</tbody>\n</table>\n");
    }
}

fn render_group_header(out: &mut String, group: &Group) {
    if !group.key.is_empty() {
        out.push_str("<h3>");
        for (i, value) in group.key.iter().enumerate() {
            if i != 0 {
                out.push_str(" / ");
            }
            if value.is_null() {
                out.push_str("(none)");
            } else {
                cell(out, value);
            }
        }
        writeln!(out, " ({})</h3>", group.count).unwrap();
    }
    if !group.agg.is_empty() {
        out.push_str("<p>");
        for (i, (key, value)) in group.agg.iter().enumerate() {
            if i != 0 {
                out.push_str(" · ");
            }
            text(out, key);
            out.push(' ');
            cell(out, value);
        }
        out.push_str("</p>\n");
    }
}

fn write_values(out: &mut String, cols: &[String], row: &Row) -> bool {
    let start = out.len();
    for col in cols {
        let Some(value) = lookup(&row.fields, col) else {
            continue;
        };
        let before = out.len();
        if before != start {
            out.push_str(" · ");
        }
        let cell_start = out.len();
        cell(out, value);
        if out.len() == cell_start {
            out.truncate(before);
        }
    }
    out.len() != start
}

/// A projected dotted key wins over lookup inside a JSONL object.
fn lookup<'a>(fields: &'a BTreeMap<String, Value>, key: &str) -> Option<&'a Value> {
    let Some((first, rest)) = key.split_once('.') else {
        return fields.get(key);
    };
    fields.get(key).or_else(|| {
        let mut object = fields.get(first)?.as_object()?;
        let mut tail = rest;
        loop {
            if let Some(value) = object.get(tail) {
                return Some(value);
            }
            let (part, remainder) = tail.split_once('.')?;
            object = object.get(part)?.as_object()?;
            tail = remainder;
        }
    })
}

fn render_source(out: &mut String, cols: &[String], row: &Row) {
    // Task state is present when projected into the sealed row. Action names
    // are capabilities, so they do not tell us whether a task was checked.
    if !cols.iter().any(|col| col == "checked") {
        if let Some(checked) = row.fields.get("checked").and_then(Value::as_bool) {
            out.push_str(if checked { "Done · " } else { "Open · " });
        }
    }
    let mut url = note_url(&row.path);
    write!(url, "?revision={}", document_links::encode(&row.rev)).unwrap();
    let mut label = row.path.clone();
    if let Some(line) = row.line {
        write!(url, "&line={line}").unwrap();
        write!(label, ":{line}").unwrap();
    }
    append_anchor(&mut url, row.anchor.as_deref());
    link(out, &Link::Href(url), &label);
}

fn cell(out: &mut String, value: &Value) {
    match value {
        Value::Null => {}
        Value::String(value) => text(out, value),
        Value::Array(values) => {
            for (i, value) in values.iter().enumerate() {
                if i != 0 {
                    out.push_str(", ");
                }
                cell(out, value);
            }
        }
        Value::Object(object) => {
            if !cell_link(out, object) {
                let mut value = value.clone();
                value.sort_all_objects();
                text(out, &value.to_string());
            }
        }
        _ => text(out, &value.to_string()),
    }
}

fn cell_link(out: &mut String, object: &Map<String, Value>) -> bool {
    let Some(label) = object
        .get("link")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return false;
    };
    match (object.get("path"), object.get("ambiguous"), object.len()) {
        (Some(Value::String(path)), None, 2) if valid_path(path) => {
            let fragment = label.split_once('#').map(|(_, fragment)| fragment);
            link(out, &note_link(path, fragment), label);
        }
        (Some(Value::Null), None, 2) => {
            let target = if label
                .get(..8)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
            {
                http_link(label)
            } else {
                Link::Inert("unresolved")
            };
            link(out, &target, label);
        }
        (Some(Value::Null), Some(Value::Array(candidates)), 3) => {
            let Some(paths) = candidates
                .iter()
                .map(Value::as_str)
                .collect::<Option<Vec<_>>>()
            else {
                return false;
            };
            let mut unique = std::collections::HashSet::new();
            if paths.len() < 2
                || !paths
                    .iter()
                    .all(|path| valid_path(path) && unique.insert(*path))
            {
                return false;
            }
            link(
                out,
                &Link::Inert("ambiguous"),
                &format!("{label} (ambiguous)"),
            );
            let fragment = label.split_once('#').map(|(_, fragment)| fragment);
            out.push_str(" (");
            for (i, path) in paths.into_iter().enumerate() {
                if i != 0 {
                    out.push_str(", ");
                }
                link(out, &note_link(path, fragment), path);
            }
            out.push(')');
        }
        _ => return false,
    }
    true
}

fn text(out: &mut String, value: &str) {
    html::escape(out, value).unwrap();
}

fn link(out: &mut String, target: &Link, label: &str) {
    target.write_open(out, false).unwrap();
    out.push('>');
    text(out, label);
    out.push_str(target.close_tag());
}

fn note_link(path: &str, fragment: Option<&str>) -> Link {
    let mut url = note_url(path);
    append_anchor(&mut url, fragment);
    Link::Href(url)
}

fn append_anchor(url: &mut String, fragment: Option<&str>) {
    if let Some(fragment) = fragment.filter(|f| !f.trim().is_empty() && f.trim() != "^") {
        url.push('#');
        url.push_str(&anchor(fragment));
    }
}

fn valid_revision(revision: &str) -> bool {
    revision.strip_prefix("sha256:").is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn valid_path(path: &str) -> bool {
    !path.contains('\\')
        && !path.chars().any(|ch| ch.is_ascii_control())
        && !matches!(path.as_bytes(), [a, b':', ..] if a.is_ascii_alphabetic())
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

/// The sealed Markdown has no live link inventory. Resolve explicit paths
/// in the source row's directory (or vault root) and leave bare wiki names
/// unresolved. A partial list of result rows cannot establish uniqueness.
pub(super) fn excerpt_link(target: &str, wiki: bool, from: &str) -> Link {
    if !wiki && document_links::is_external_url(target) {
        return http_link(target);
    }
    let raw = target.split('#').next().unwrap_or(target);
    let path = if wiki {
        raw.trim().to_owned()
    } else {
        document_links::decode(raw)
    };
    if path.starts_with("//") || path.contains([':', '\\']) {
        return Link::Inert("unsupported");
    }
    if let Some(ext) = path
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, ext)| ext)
    {
        if !ext.eq_ignore_ascii_case("md") {
            return excerpt_path(&path, wiki, from)
                .map_or(Link::Inert("unresolved"), |path| asset_link(&path));
        }
    }
    match document_links::destination(target, wiki) {
        Destination::Note { path, heading } => {
            let Some(path) = excerpt_path(&path, wiki, from) else {
                return Link::Inert("unresolved");
            };
            link_excerpt_note(&path, heading.as_deref())
        }
        _ => Link::Inert("unsupported"),
    }
}

fn link_excerpt_note(path: &str, heading: Option<&str>) -> Link {
    if path.to_ascii_lowercase().ends_with(".md") {
        note_link(path, heading)
    } else {
        note_link(&format!("{path}.md"), heading)
    }
}

fn excerpt_path(path: &str, wiki: bool, from: &str) -> Option<String> {
    if path.is_empty() {
        return Some(from.to_owned());
    }
    let relative = path.starts_with("./") || path.starts_with("../");
    if wiki && !relative && !path.contains('/') {
        return None;
    }
    let rooted = path.starts_with('/') || wiki && !relative;
    let dir = from.rsplit_once('/').map_or("", |(dir, _)| dir);
    let mut parts: Vec<&str> = if rooted || dir.is_empty() {
        Vec::new()
    } else {
        dir.split('/').collect()
    };
    for part in path.strip_prefix('/').unwrap_or(path).split('/') {
        match part {
            "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    let path = parts.join("/");
    valid_path(&path).then_some(path)
}
