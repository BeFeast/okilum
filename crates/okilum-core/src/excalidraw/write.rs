//! Writing an edited scene back into its drawing file (#478).
//!
//! `write` is a pure function from the opened file bytes plus an edited scene
//! to new file bytes. It changes only what the edit changed:
//!
//! - Unchanged elements keep the file's own JSON object, so plugin-only fields
//!   such as `rawText` survive an editor that drops them.
//! - `## Text Elements` and `## Element Links` entries are rewritten only for
//!   elements whose text or link changed. The Obsidian Excalidraw plugin reads
//!   those Markdown entries as authoritative over the JSON, so leaving a stale
//!   entry would silently revert the edit the next time Obsidian opens the file.
//! - The Drawing block keeps its encoding (`json` or `compressed-json`).
//! - Every other byte (frontmatter, BOM, back-of-note Markdown, `## Embedded
//!   Files`, line endings) is copied from the opened file.
//!
//! A save that changes nothing returns [`WriteOutcome::Unchanged`]: no write.
use super::{lz, Scene, MAX_SOURCE_BYTES};
use anyhow::{bail, ensure, Context, Result};
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::LazyLock;

const MAX_ELEMENTS: usize = 10_000;
/// The plugin splits compressed scenes into 256-character chunks.
const CHUNK: usize = 256;
/// App state the editor lets the user change; everything else stays as opened.
const USER_APP_STATE: &[&str] = &[
    "viewBackgroundColor",
    "gridSize",
    "gridStep",
    "gridModeEnabled",
];

/// What a save must do to the drawing file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The edited scene equals the opened one; write nothing.
    Unchanged,
    /// The complete new file contents.
    Updated(String),
}

impl WriteOutcome {
    /// The file contents after the save, given the opened contents.
    pub fn contents<'a>(&'a self, base: &'a str) -> &'a str {
        match self {
            Self::Unchanged => base,
            Self::Updated(text) => text,
        }
    }
}

/// Builds the new contents of a drawing file from an edited scene.
///
/// - `base`: the file contents as opened.
/// - `baseline`: the scene the editor reported right after loading `base`, if
///   any. Editors normalise a scene on load (defaults, versions), so changes are
///   judged against this baseline. Without one they are judged against the file.
/// - `edited`: the scene to save.
///
/// The result is parsed back before it is returned; a scene that would not
/// read back exactly is an error, never a written file.
pub fn write(base: &str, baseline: Option<&Value>, edited: &Value) -> Result<WriteOutcome> {
    ensure!(base.len() <= MAX_SOURCE_BYTES, "Drawing file is too large");
    let original = Scene::parse(base)?.json;
    let edited_elements = scene_elements(edited, "Edited scene")?;
    let baseline_elements = baseline
        .map(|scene| scene_elements(scene, "Baseline scene"))
        .transpose()?;
    let markdown = !base
        .trim_start_matches('\u{feff}')
        .trim_start()
        .starts_with('{');
    let layout = markdown.then(|| Layout::parse(base)).transpose()?;
    let embedded: HashSet<&str> = layout
        .as_ref()
        .map(|layout| layout.embedded_ids(base))
        .unwrap_or_default();

    let merge = merge_elements(
        original["elements"]
            .as_array()
            .context("Drawing has no element array")?,
        baseline_elements,
        edited_elements,
        markdown,
    )?;
    let mut scene = original.clone();
    scene["elements"] = Value::Array(merge.elements);
    merge_app_state(&mut scene, &original, baseline, edited);
    merge_files(&mut scene, edited, &embedded);
    if scene == original {
        return Ok(WriteOutcome::Unchanged);
    }

    let text = match layout {
        Some(layout) => layout.render(base, &scene, &merge.text, &merge.links)?,
        None => render_plain(base, &scene)?,
    };
    ensure!(text.len() <= MAX_SOURCE_BYTES, "Drawing file is too large");
    let reread = Scene::parse(&text).context("Saved drawing does not read back")?;
    ensure!(reread.json == scene, "Saved drawing does not read back");
    Ok(WriteOutcome::Updated(text))
}

fn scene_elements<'a>(scene: &'a Value, what: &str) -> Result<&'a [Value]> {
    ensure!(
        scene.get("type").and_then(Value::as_str) == Some("excalidraw"),
        "{what} is not an Excalidraw scene"
    );
    let elements = scene
        .get("elements")
        .and_then(Value::as_array)
        .with_context(|| format!("{what} has no element array"))?;
    ensure!(
        elements.len() <= MAX_ELEMENTS,
        "{what} has too many elements"
    );
    let mut ids = HashSet::new();
    for element in elements {
        let id =
            element_id(element).with_context(|| format!("{what} has an element without id"))?;
        ensure!(ids.insert(id), "{what} repeats element id {id}");
    }
    Ok(elements.as_slice())
}

fn ids(elements: &[Value]) -> HashMap<String, usize> {
    elements
        .iter()
        .enumerate()
        .filter_map(|(index, element)| Some((element_id(element)?.to_owned(), index)))
        .collect()
}

fn element_id(element: &Value) -> Option<&str> {
    element
        .as_object()?
        .get("id")?
        .as_str()
        .filter(|id| !id.is_empty())
}

fn is_deleted(element: &Value) -> bool {
    element.get("isDeleted").and_then(Value::as_bool) == Some(true)
}

fn is_text(element: &Value) -> bool {
    element.get("type").and_then(Value::as_str) == Some("text")
}

/// The unwrapped text the user typed (`originalText`, older scenes: `text`).
fn original_text(element: &Value) -> Option<&str> {
    element
        .get("originalText")
        .or_else(|| element.get("text"))
        .and_then(Value::as_str)
}

fn link(element: &Value) -> Option<&str> {
    element
        .get("link")
        .and_then(Value::as_str)
        .filter(|link| !link.is_empty())
}

/// Element equality ignoring `rawText`, which upstream Excalidraw deletes on load.
fn same_element(a: &Value, b: &Value) -> bool {
    match (a.as_object(), b.as_object()) {
        (Some(a), Some(b)) => {
            let fields =
                |map: &Map<String, Value>| map.iter().filter(|(key, _)| *key != "rawText").count();
            fields(a) == fields(b)
                && a.iter()
                    .filter(|(key, _)| *key != "rawText")
                    .all(|(key, value)| b.get(key) == Some(value))
        }
        _ => a == b,
    }
}

/// A requested change to one entry of a Markdown section.
#[derive(Debug, PartialEq)]
enum Entry {
    Set(String),
    Remove,
}

struct Merge {
    elements: Vec<Value>,
    /// `## Text Elements` changes in scene order, by element id.
    text: Vec<(String, Entry)>,
    /// `## Element Links` changes in scene order, by element id.
    links: Vec<(String, Entry)>,
}

/// The plugin's block references are exactly 8 characters (`\s\^(.{8})\n+`).
/// A longer id would be read as part of the next entry's text, so elements with
/// upstream's 21-character ids get no entry; the plugin adopts them from the
/// JSON (and shortens their ids) when it next opens the file.
fn block_ref_id(id: &str) -> bool {
    id.len() == 8
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Links the plugin keeps in `## Element Links` (Markdown wiki links only).
fn markdown_link(link: Option<&str>) -> Option<&str> {
    link.filter(|link| link.starts_with("[[") && link.ends_with("]]") && !link.contains('\n'))
}

/// `markdown`: whether the file is an Obsidian `.excalidraw.md`, whose text
/// elements carry the plugin's `rawText`.
fn merge_elements(
    original: &[Value],
    baseline: Option<&[Value]>,
    edited: &[Value],
    markdown: bool,
) -> Result<Merge> {
    let original_ids = ids(original);
    let baseline_ids = baseline.map(ids);
    // What the editor started from: the baseline when given, otherwise the file.
    let reference = |id: &str| -> Option<&Value> {
        match (&baseline_ids, baseline) {
            (Some(ids), Some(baseline)) => ids.get(id).map(|&index| &baseline[index]),
            _ => original_ids.get(id).map(|&index| &original[index]),
        }
    };
    let mut merge = Merge {
        elements: Vec::with_capacity(edited.len()),
        text: Vec::new(),
        links: Vec::new(),
    };
    let mut seen = HashSet::new();
    for element in edited {
        let id = element_id(element).context("Edited element without id")?;
        seen.insert(id);
        let file = original_ids.get(id).map(|&index| &original[index]);
        let before = reference(id);
        if let (Some(file), Some(before)) = (file, before) {
            if same_element(element, before) {
                merge.elements.push(file.clone());
                continue;
            }
        }
        let mut element = element.clone();
        if let Some(file) = file {
            bump_version(&mut element, file);
        }
        let deleted = is_deleted(&element);
        if markdown && is_text(&element) {
            let typed = original_text(&element).unwrap_or_default().to_owned();
            let text_changed = before.and_then(original_text) != Some(typed.as_str());
            let kept_raw = file
                .filter(|_| !text_changed)
                .and_then(|file| file.get("rawText"))
                .cloned();
            element["rawText"] = kept_raw.unwrap_or_else(|| Value::String(typed.clone()));
            if deleted {
                merge.text.push((id.to_owned(), Entry::Remove));
            } else if text_changed {
                merge.text.push((
                    id.to_owned(),
                    if block_ref_id(id) {
                        Entry::Set(typed)
                    } else {
                        Entry::Remove
                    },
                ));
            }
        }
        let new_link = link(&element);
        let link_changed = before.and_then(link) != new_link;
        if deleted {
            merge.links.push((id.to_owned(), Entry::Remove));
        } else if link_changed {
            let entry = match markdown_link(new_link) {
                Some(link) if block_ref_id(id) => Entry::Set(link.to_owned()),
                _ => Entry::Remove,
            };
            merge.links.push((id.to_owned(), entry));
        }
        // Older plugin files mark a shape's link as `[[link]] ^id` under
        // `## Text Elements`; that marker would override the new link.
        if markdown && !is_text(&element) && (deleted || link_changed) {
            merge.text.push((id.to_owned(), Entry::Remove));
        }
        merge.elements.push(element);
    }
    // File elements the edited scene no longer has. Tombstones stay, as do
    // elements the editor never loaded; the rest were deleted in the editor and
    // are written as tombstones so an open Obsidian view removes them too.
    for file in original {
        let Some(id) = element_id(file) else {
            merge.elements.push(file.clone());
            continue;
        };
        if seen.contains(id) {
            continue;
        }
        let unseen = baseline_ids
            .as_ref()
            .is_some_and(|ids| !ids.contains_key(id));
        if is_deleted(file) || unseen {
            merge.elements.push(file.clone());
            continue;
        }
        let mut tombstone = file.clone();
        tombstone["isDeleted"] = Value::Bool(true);
        bump_version(&mut tombstone, file);
        let nonce = file
            .get("versionNonce")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        tombstone["versionNonce"] =
            Value::from((nonce.wrapping_mul(48_271) + 1).rem_euclid(1 << 31));
        merge.elements.push(tombstone);
        merge.text.push((id.to_owned(), Entry::Remove));
        merge.links.push((id.to_owned(), Entry::Remove));
    }
    Ok(merge)
}

/// The plugin's incremental sync takes an incoming element only when its
/// `version` is at least the open one, so a changed element must move forward.
fn bump_version(element: &mut Value, file: &Value) {
    let file_version = file.get("version").and_then(Value::as_i64).unwrap_or(0);
    let version = element.get("version").and_then(Value::as_i64).unwrap_or(0);
    if version <= file_version {
        element["version"] = Value::from(file_version + 1);
    }
}

fn merge_app_state(scene: &mut Value, original: &Value, baseline: Option<&Value>, edited: &Value) {
    let reference = baseline.unwrap_or(original);
    for key in USER_APP_STATE {
        let Some(value) = edited.get("appState").and_then(|state| state.get(*key)) else {
            continue;
        };
        if reference.get("appState").and_then(|state| state.get(*key)) == Some(value) {
            continue;
        }
        let state = scene
            .as_object_mut()
            .expect("scene is an object")
            .entry("appState")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(state) = state.as_object_mut() {
            state.insert((*key).to_owned(), value.clone());
        }
    }
}

/// Keeps the file's `files` and adds files new in the editor. Files listed in
/// `## Embedded Files` live in the vault; an editor host inlines them for
/// display only, and they must never be written into the JSON.
fn merge_files(scene: &mut Value, edited: &Value, embedded: &HashSet<&str>) {
    let Some(files) = edited.get("files").and_then(Value::as_object) else {
        return;
    };
    for (id, file) in files {
        if embedded.contains(id.as_str()) {
            continue;
        }
        let target = scene
            .as_object_mut()
            .expect("scene is an object")
            .entry("files")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(target) = target.as_object_mut() {
            target.entry(id.clone()).or_insert_with(|| file.clone());
        }
    }
}

fn to_json(scene: &Value, indent: &[u8]) -> Result<String> {
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(indent);
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
    serde::Serialize::serialize(scene, &mut serializer)?;
    Ok(String::from_utf8(out)?)
}

fn newline(base: &str) -> &'static str {
    if base.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

/// A plain `.excalidraw` file, written as excalidraw.com does (2-space JSON).
fn render_plain(base: &str, scene: &Value) -> Result<String> {
    let nl = newline(base);
    let mut out = String::new();
    if base.starts_with('\u{feff}') {
        out.push('\u{feff}');
    }
    out.push_str(&to_json(scene, b"  ")?.replace('\n', nl));
    if base.ends_with('\n') {
        out.push_str(nl);
    }
    Ok(out)
}

/// Byte ranges of the parts of an `.excalidraw.md` file the writer replaces.
struct Layout {
    /// Body of `## Text Elements` (after its header line), if present.
    text: Option<Section>,
    /// Body of `## Element Links`, if present.
    links: Option<Section>,
    /// Body of `## Embedded Files`, if present.
    embedded: Option<Range<usize>>,
    /// Where a missing `## Text Elements` section goes.
    text_insert: usize,
    /// Where a missing `## Element Links` section goes.
    links_insert: usize,
    compressed: bool,
    /// The Drawing fence body, between the opening and closing fence lines.
    drawing: Range<usize>,
}

struct Section {
    body: Range<usize>,
    chunks: Vec<Chunk>,
}

/// A run of section bytes: one entry (with its trailing blank lines) or other text.
struct Chunk {
    id: Option<String>,
    span: Range<usize>,
}

/// One line: its byte range including the line break, and its trimmed text.
struct Line<'a> {
    span: Range<usize>,
    text: &'a str,
}

fn lines(source: &str) -> Vec<Line<'_>> {
    let mut start = 0;
    source
        .split_inclusive('\n')
        .map(|line| {
            let span = start..start + line.len();
            start = span.end;
            Line {
                span,
                text: line.trim(),
            }
        })
        .collect()
}

static TEXT_ENTRY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s\^([^\r\n]{8})(?:\r?\n)+").expect("valid regex"));
static LINK_ENTRY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z0-9_-]+):[ \t]").expect("valid regex"));

fn is_text_header(line: &str) -> bool {
    line == "## Text Elements" || line == "# Text Elements"
}
fn is_links_header(line: &str) -> bool {
    line == "## Element Links"
}
fn is_embedded_header(line: &str) -> bool {
    line.eq_ignore_ascii_case("## Embedded Files")
}
/// A line that ends a section of the Excalidraw Data block.
fn ends_section(line: &str) -> bool {
    line == "%%"
        || line == "## Drawing"
        || line == "# Drawing"
        || is_text_header(line)
        || is_links_header(line)
        || is_embedded_header(line)
}

impl Layout {
    fn parse(source: &str) -> Result<Self> {
        let all = lines(source);
        // Mirror `Scene::parse`: the first `## Drawing`, then its first fence.
        let header = all
            .iter()
            .position(|line| line.text == "## Drawing")
            .context("Missing Drawing section")?;
        let (fence, compressed) = all[header + 1..]
            .iter()
            .enumerate()
            .find_map(|(offset, line)| match line.text {
                "```json" => Some((header + 1 + offset, false)),
                "```compressed-json" => Some((header + 1 + offset, true)),
                _ => None,
            })
            .context("Missing Drawing JSON block")?;
        let close = all[fence + 1..]
            .iter()
            .position(|line| line.text == "```")
            .map(|offset| fence + 1 + offset)
            .context("Missing or truncated Drawing JSON block")?;
        let drawing = all[fence].span.end..all[close].span.start;
        let data_end = all[header].span.start;
        let data = all[..header]
            .iter()
            .rposition(|line| line.text == "# Excalidraw Data")
            .map_or(0, |index| index + 1);
        let find = |is: fn(&str) -> bool| (data..header).find(|&index| is(all[index].text));
        // Up to the line that ends the section that starts after `index`.
        let section_end = |index: usize| {
            (index + 1..header)
                .find(|&next| ends_section(all[next].text))
                .map_or(data_end, |next| all[next].span.start)
        };
        // The `%%` that opens the Drawing comment, when the text is visible.
        let comment = (data..header)
            .rev()
            .find(|&index| !all[index].text.is_empty())
            .filter(|&index| all[index].text == "%%")
            .map(|index| all[index].span.start);
        let (text_header, links_header, embedded_header) = (
            find(is_text_header),
            find(is_links_header),
            find(is_embedded_header),
        );
        let text =
            text_header.map(|index| Self::text_section(source, all[index].span.end, data_end));
        let links = links_header
            .map(|index| Self::link_section(source, all[index].span.end..section_end(index)));
        let embedded = embedded_header.map(|index| all[index].span.end..section_end(index));
        // Missing sections go where the plugin writes them: Text Elements, then
        // Element Links, then Embedded Files, then the Drawing comment.
        let links_insert = embedded_header
            .map(|index| all[index].span.start)
            .or(comment)
            .unwrap_or(data_end);
        let text_insert = links_header
            .map(|index| all[index].span.start)
            .unwrap_or(links_insert);
        Ok(Self {
            text,
            links,
            embedded,
            text_insert,
            links_insert,
            compressed,
            drawing,
        })
    }

    /// Entries are `<raw text> ^<id>` followed by blank lines; raw text may span
    /// lines. The section ends at a section boundary between entries.
    fn text_section(source: &str, start: usize, limit: usize) -> Section {
        let mut chunks = Vec::new();
        let mut position = start;
        let boundary = |position: usize| {
            lines(&source[position..limit])
                .into_iter()
                .find(|line| ends_section(line.text))
                .map_or(limit, |line| position + line.span.start)
        };
        let end = loop {
            let next = boundary(position);
            if next == position {
                break position;
            }
            // An entry whose text would cross a section boundary is not one.
            let entry = TEXT_ENTRY
                .captures(&source[position..limit])
                .filter(|entry| position + entry.get(0).expect("match").start() < next);
            match entry {
                Some(entry) => {
                    let whole = entry.get(0).expect("match");
                    let id = entry[1].to_owned();
                    let end = position + whole.end();
                    chunks.push(Chunk {
                        id: Some(id),
                        span: position..end,
                    });
                    position = end;
                }
                None => {
                    if next > position {
                        chunks.push(Chunk {
                            id: None,
                            span: position..next,
                        });
                    }
                    break next;
                }
            }
        };
        Section {
            body: start..end,
            chunks,
        }
    }

    /// Entries are `<id>: <link>` lines; blank lines belong to the entry above.
    fn link_section(source: &str, body: Range<usize>) -> Section {
        let mut chunks: Vec<Chunk> = Vec::new();
        for line in lines(&source[body.clone()]) {
            let span = body.start + line.span.start..body.start + line.span.end;
            let id = LINK_ENTRY
                .captures(line.text)
                .map(|entry| entry[1].to_owned());
            match chunks.last_mut() {
                Some(last) if id.is_none() && line.text.is_empty() => last.span.end = span.end,
                _ => chunks.push(Chunk { id, span }),
            }
        }
        Section { body, chunks }
    }

    fn embedded_ids<'a>(&self, source: &'a str) -> HashSet<&'a str> {
        let Some(body) = &self.embedded else {
            return HashSet::new();
        };
        source[body.clone()]
            .lines()
            .filter_map(|line| Some(line.trim().split_once(": ")?.0))
            .filter(|id| !id.is_empty() && !id.contains(char::is_whitespace))
            .collect()
    }

    fn render(
        &self,
        source: &str,
        scene: &Value,
        text: &[(String, Entry)],
        links: &[(String, Entry)],
    ) -> Result<String> {
        let nl = newline(source);
        let text_entry = |id: &str, raw: &str| format!("{} ^{id}{nl}{nl}", raw.replace('\n', nl));
        let link_entry = |id: &str, link: &str| format!("{id}: {link}{nl}{nl}");
        let mut edits: Vec<(Range<usize>, String)> = Vec::new();
        let mut section = |section: &Option<Section>,
                           insert: usize,
                           header: &str,
                           ops: &[(String, Entry)],
                           entry: &dyn Fn(&str, &str) -> String| {
            match section {
                Some(section) => {
                    let body = rebuild(source, section, ops, entry, nl);
                    if body != source[section.body.clone()] {
                        edits.push((section.body.clone(), body));
                    }
                }
                None => {
                    let added: String = ops
                        .iter()
                        .filter_map(|(id, op)| match op {
                            Entry::Set(value) => Some(entry(id, value)),
                            Entry::Remove => None,
                        })
                        .collect();
                    if !added.is_empty() {
                        edits.push((insert..insert, format!("{header}{nl}{added}")));
                    }
                }
            }
        };
        section(
            &self.text,
            self.text_insert,
            "## Text Elements",
            text,
            &text_entry,
        );
        section(
            &self.links,
            self.links_insert,
            "## Element Links",
            links,
            &link_entry,
        );
        let json = to_json(scene, b"\t")?;
        let body = if self.compressed {
            let compressed = lz::compress(&json);
            let chunks: Vec<&str> = compressed
                .as_bytes()
                .chunks(CHUNK)
                .map(|chunk| std::str::from_utf8(chunk).expect("Base64 is ASCII"))
                .collect();
            format!("{}{nl}", chunks.join(&format!("{nl}{nl}")))
        } else {
            format!("{}{nl}", json.replace('\n', nl))
        };
        edits.push((self.drawing.clone(), body));
        // Insertions at one point keep their order (text before links).
        edits.sort_by_key(|(range, _)| (range.start, range.end));
        let mut out = String::with_capacity(source.len() + 1024);
        let mut position = 0;
        for (range, text) in edits {
            if range.start < position {
                bail!("Overlapping drawing file edits");
            }
            out.push_str(&source[position..range.start]);
            out.push_str(&text);
            position = range.end;
        }
        out.push_str(&source[position..]);
        Ok(out)
    }
}

/// Applies entry changes to one section, keeping every untouched byte.
fn rebuild(
    source: &str,
    section: &Section,
    ops: &[(String, Entry)],
    entry: &dyn Fn(&str, &str) -> String,
    nl: &str,
) -> String {
    let wanted: HashMap<&str, &Entry> = ops.iter().map(|(id, op)| (id.as_str(), op)).collect();
    let mut written = HashSet::new();
    let mut parts: Vec<String> = Vec::new();
    // New entries go after the last existing entry.
    let mut insert_at = 0;
    for chunk in &section.chunks {
        let original = &source[chunk.span.clone()];
        let Some(id) = chunk.id.as_deref() else {
            parts.push(original.to_owned());
            continue;
        };
        match wanted.get(id) {
            None => parts.push(original.to_owned()),
            Some(Entry::Remove) => {}
            Some(Entry::Set(value)) => {
                // A repeated id keeps only its first entry.
                if written.insert(id) {
                    parts.push(entry(id, value));
                }
            }
        }
        insert_at = parts.len();
    }
    let added: Vec<String> = ops
        .iter()
        .filter_map(|(id, op)| match op {
            Entry::Set(value) if !written.contains(id.as_str()) => Some(entry(id, value)),
            _ => None,
        })
        .collect();
    if !added.is_empty() {
        // The plugin expects each entry to end in a blank line.
        if let Some(last) = parts[..insert_at].last_mut() {
            let double = format!("{nl}{nl}");
            while !last.ends_with(&double) {
                last.push_str(nl);
            }
        }
        parts.splice(insert_at..insert_at, added);
    }
    parts.concat()
}
