//! Explicit project viewer associations and a deliberately bounded Draft 2020-12 subset.
//! Call from a worker. No network, writes, defaults, schema discovery or OS associations.
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Component, Path, PathBuf},
};

const CONFIG_BYTES: usize = 64 * 1024;
const FILE_BYTES: usize = 256 * 1024;
const TOTAL_BYTES: usize = 2 * 1024 * 1024;
pub const DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_NODES: usize = 4096;
const MAX_STEPS: usize = 100_000;
const MAX_FIELDS: usize = 10_000;
const MAX_ISSUES: usize = 100;
const MAX_DEPTH: usize = 32;
const DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";

#[derive(Clone, Debug, Default)]
pub struct Association {
    pub project: PathBuf,
    pub config: Option<PathBuf>,
    pub json: bool,
    pub schema: Option<PathBuf>,
    pub document: Option<PathBuf>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: u32,
    #[serde(default)]
    json: Vec<ViewerRule>,
    #[serde(default)]
    schemas: Vec<SchemaRule>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewerRule {
    pattern: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SchemaRule {
    pattern: String,
    schema: String,
}

fn read(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    if !std::fs::metadata(path)
        .map_err(|e| e.to_string())?
        .is_file()
    {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err(format!("{} exceeds {limit} bytes", path.display()));
    }
    Ok(bytes)
}
fn relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':'])
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}
fn relative_local_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':'])
        && Path::new(path).components().all(|c| {
            matches!(
                c,
                Component::Normal(_) | Component::CurDir | Component::ParentDir
            )
        })
}
fn matches(pattern: &str, rel: &str) -> Result<bool, String> {
    if pattern.len() > 512 || !relative(pattern) {
        return Err(format!("Invalid project pattern: {pattern}"));
    }
    globset::GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map(|g| g.compile_matcher().is_match(rel))
        .map_err(|e| e.to_string())
}
/// Nearest ancestor config inside the Reader root wins. Last matching rule wins
/// independently in each array. Paths/patterns are relative to that project.
pub fn associate(root: &Path, path: &Path) -> Result<Association, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    if !path.starts_with(&root) {
        return Err("Document is outside the Reader root".into());
    }
    let mut directory = path.parent().ok_or("Document has no parent")?;
    for _ in 0..128 {
        let config = directory.join(".okilum/viewers.json");
        match config.symlink_metadata() {
            Ok(_) => {
                let canonical = config.canonicalize().map_err(|e| e.to_string())?;
                if !canonical.starts_with(directory) {
                    return Err("Viewer config escapes its project".into());
                }
                let config_data: Config = serde_json::from_slice(&read(&canonical, CONFIG_BYTES)?)
                    .map_err(|e| format!("{}: {e}", config.display()))?;
                if config_data.version != 1
                    || config_data.json.len() + config_data.schemas.len() > 128
                {
                    return Err(
                        "Unsupported viewer config version or more than 128 associations".into(),
                    );
                }
                let rel = path
                    .strip_prefix(directory)
                    .map_err(|e| e.to_string())?
                    .to_string_lossy()
                    .replace('\\', "/");
                let mut association = Association {
                    project: directory.to_path_buf(),
                    config: Some(config),
                    document: Some(path.clone()),
                    ..Default::default()
                };
                for rule in config_data.json {
                    if matches(&rule.pattern, &rel)? {
                        association.json = true;
                    }
                }
                for rule in config_data.schemas {
                    if !relative(&rule.schema) {
                        return Err("Schema must be a relative project path".into());
                    }
                    if matches(&rule.pattern, &rel)? {
                        association.schema = Some(directory.join(rule.schema));
                    }
                }
                return Ok(association);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", config.display())),
        }
        if directory == root {
            return Ok(Association {
                project: root,
                document: Some(path),
                ..Default::default()
            });
        }
        directory = directory
            .parent()
            .ok_or("Project search escaped Reader root")?;
    }
    Err("Project search exceeds 128 ancestors".into())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Field {
    pub title: Option<String>,
    pub description: Option<String>,
    pub types: Vec<String>,
    pub required: bool,
    pub property: bool,
    pub enumeration: Option<String>,
    pub missing: bool,
}
impl Field {
    pub fn explain(&self) -> String {
        let mut parts = Vec::new();
        if let Some(title) = &self.title {
            parts.push(title.clone());
        }
        if let Some(description) = &self.description {
            parts.push(description.clone());
        }
        if !self.types.is_empty() {
            parts.push(format!("Declared type: {}", self.types.join(" | ")));
        }
        if self.property {
            parts.push(
                if self.required {
                    "Required"
                } else {
                    "Optional"
                }
                .into(),
            );
        }
        if let Some(enumeration) = &self.enumeration {
            parts.push(format!("Enum: {enumeration}"));
        }
        if self.missing {
            parts.push("Absent from document".into());
        }
        parts.join("\n")
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    pub path: String,
    pub message: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Valid,
    Invalid,
    Incomplete,
    Unavailable,
    SyntaxInvalid,
}
#[derive(Clone, Copy, Debug)]
enum ItemKind {
    Object,
    Array,
    Scalar,
}

#[derive(Debug)]
pub struct Report {
    pub schema: Option<PathBuf>,
    pub verdict: Verdict,
    pub summary: String,
    pub fields: BTreeMap<String, Field>,
    pub violations: Vec<Violation>,
    array_kinds: Vec<ItemKind>,
}
impl Report {
    pub fn unavailable(schema: Option<PathBuf>, message: String) -> Self {
        Self {
            schema,
            verdict: Verdict::Unavailable,
            summary: format!(
                "Schema unavailable: {message}. Ordinary JSON views remain available."
            ),
            fields: BTreeMap::new(),
            violations: Vec::new(),
            array_kinds: Vec::new(),
        }
    }
    /// Table's source index, never its sorted/filtered display ordinal.
    pub fn table_explanation(&self, source: usize, column: &str, header: bool) -> String {
        let parent = format!("/{source}");
        let path = match self.array_kinds.get(source) {
            Some(ItemKind::Object) => pointer_child(&parent, column),
            Some(ItemKind::Array) => match column
                .strip_prefix("Column ")
                .and_then(|n| n.parse::<usize>().ok())
                .and_then(|n| n.checked_sub(1))
            {
                Some(index) => pointer_child(&parent, &index.to_string()),
                None => {
                    return "No schema field declaration available for this table column".into()
                }
            },
            Some(ItemKind::Scalar) if column == "Value" => parent,
            _ => return "No schema field declaration available for this source row".into(),
        };
        let explanation = self.explanation(&path);
        if header {
            format!("Column metadata from example source row {source}\n{explanation}")
        } else {
            explanation
        }
    }
    pub fn explanation(&self, pointer: &str) -> String {
        let path = if pointer.is_empty() {
            "/ (root)"
        } else {
            pointer
        };
        let mut text = format!("JSON Pointer: {path}");
        if let Some(field) = self.fields.get(pointer) {
            text.push('\n');
            text.push_str(&field.explain());
        } else {
            text.push_str("\nNo schema field declaration available");
        }
        for issue in self.violations.iter().filter(|v| v.path == pointer) {
            text.push('\n');
            text.push_str(&issue.message);
        }
        text
    }
}
pub fn pointer_child(parent: &str, key: &str) -> String {
    format!("{parent}/{}", key.replace('~', "~0").replace('/', "~1"))
}

// JSON Schema compares numeric values, including 1 and 1.0. Keep the
// supported numeric domain explicit rather than rounding large integers.
fn safe_numbers(value: &Value) -> Result<(), String> {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Number(n) if n.as_f64().is_none_or(|n| n.abs() > 9_007_199_254_740_991.) => {
                return Err("Numbers outside the exact supported range ±(2^53−1)".into())
            }
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => pending.extend(values.values()),
            _ => {}
        }
    }
    Ok(())
}
fn equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equal(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, a)| b.get(key).is_some_and(|b| equal(a, b)))
        }
        _ => a == b,
    }
}
fn parse_unique(bytes: &[u8]) -> Result<Value, serde_json::Error> {
    struct Unique(Value);
    impl<'de> Deserialize<'de> for Unique {
        fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
            struct Visitor;
            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = Unique;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("JSON with unique object properties")
                }
                fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Unique, E> {
                    serde_json::Number::from_f64(v)
                        .map(|n| Unique(Value::Number(n)))
                        .ok_or_else(|| E::custom("Nonfinite JSON number"))
                }
                fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_unit<E: serde::de::Error>(self) -> Result<Unique, E> {
                    Ok(Unique(Value::Null))
                }
                fn visit_seq<A: serde::de::SeqAccess<'de>>(
                    self,
                    mut seq: A,
                ) -> Result<Unique, A::Error> {
                    let mut values = Vec::new();
                    while let Some(Unique(v)) = seq.next_element()? {
                        values.push(v);
                    }
                    Ok(Unique(Value::Array(values)))
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut map: A,
                ) -> Result<Unique, A::Error> {
                    let mut values = serde_json::Map::new();
                    while let Some((key, Unique(v))) = map.next_entry::<String, Unique>()? {
                        if values.insert(key.clone(), v).is_some() {
                            return Err(serde::de::Error::custom(format!(
                                "Duplicate JSON property: {key}"
                            )));
                        }
                    }
                    Ok(Unique(Value::Object(values)))
                }
            }
            de.deserialize_any(Visitor)
        }
    }
    serde_json::from_slice::<Unique>(bytes).map(|v| v.0)
}

#[derive(Default)]
struct Node {
    boolean: Option<bool>,
    field: Field,
    reference: Option<Box<Node>>,
    all_of: Vec<Node>,
    any_of: Vec<Node>,
    one_of: Vec<Node>,
    properties: BTreeMap<String, Node>,
    required: BTreeSet<String>,
    items: Option<Box<Node>>,
    additional: Option<bool>,
    enumeration: Option<Vec<Value>>,
    constant: Option<Value>,
    minimum: Option<f64>,
    maximum: Option<f64>,
    min_items: Option<u64>,
    max_items: Option<u64>,
    min_length: Option<u64>,
    max_length: Option<u64>,
}
struct Loader {
    root: PathBuf,
    files: BTreeMap<PathBuf, Value>,
    bytes: usize,
    nodes: usize,
    stack: BTreeSet<(PathBuf, String)>,
    started: std::time::Instant,
}
impl Loader {
    fn file(&mut self, path: &Path) -> Result<(PathBuf, Value), String> {
        let path = path
            .canonicalize()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if !path.starts_with(&self.root) {
            return Err("Schema/reference escapes project (including symlinks)".into());
        }
        if let Some(value) = self.files.get(&path) {
            return Ok((path, value.clone()));
        }
        if self.files.len() >= 16 {
            return Err("More than 16 local schema files".into());
        }
        let bytes = read(&path, FILE_BYTES)?;
        self.bytes += bytes.len();
        if self.bytes > TOTAL_BYTES {
            return Err("Schema set exceeds 2 MiB".into());
        }
        let value = parse_unique(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        safe_numbers(&value)?;
        if value.get("$schema").and_then(Value::as_str) != Some(DRAFT) {
            return Err(format!(
                "{} must declare JSON Schema Draft 2020-12",
                path.display()
            ));
        }
        self.files.insert(path.clone(), value.clone());
        Ok((path, value))
    }
    fn reference(&mut self, base: &Path, reference: &str, depth: usize) -> Result<Node, String> {
        let (file, fragment) = reference.split_once('#').unwrap_or((reference, ""));
        if !file.is_empty() && !relative_local_path(file) {
            return Err(format!("Unsupported nonlocal reference: {reference}"));
        }
        // Only literal JSON Pointers; URI encoding/anchors are explicitly unsupported.
        if (!fragment.is_empty() && !fragment.starts_with('/')) || reference.contains('%') {
            return Err(format!("Unsupported reference fragment: {reference}"));
        }
        let target = if file.is_empty() {
            base.to_path_buf()
        } else {
            base.parent().ok_or("Schema has no directory")?.join(file)
        };
        let (path, document) = self.file(&target)?;
        let value = document
            .pointer(fragment)
            .ok_or_else(|| format!("Missing reference: {reference}"))?;
        let key = (path.clone(), fragment.to_owned());
        if !self.stack.insert(key.clone()) {
            return Err(format!("Recursive reference: {reference}"));
        }
        let result = self.compile(value, &path, depth + 1);
        self.stack.remove(&key);
        result
    }
    fn compile(&mut self, value: &Value, base: &Path, depth: usize) -> Result<Node, String> {
        self.nodes += 1;
        if self.started.elapsed() > std::time::Duration::from_secs(2) {
            return Err("Schema resolution exceeded two seconds".into());
        }
        if depth > MAX_DEPTH || self.nodes > MAX_NODES {
            return Err("Schema exceeds depth 32 or 4096 expanded nodes".into());
        }
        if let Some(boolean) = value.as_bool() {
            return Ok(Node {
                boolean: Some(boolean),
                ..Default::default()
            });
        }
        let map = value
            .as_object()
            .ok_or("Schema must be an object or boolean")?;
        const KEYS: &[&str] = &[
            "$schema",
            "$ref",
            "$defs",
            "definitions",
            "allOf",
            "anyOf",
            "oneOf",
            "title",
            "description",
            "type",
            "properties",
            "required",
            "items",
            "additionalProperties",
            "enum",
            "const",
            "minimum",
            "maximum",
            "minItems",
            "maxItems",
            "minLength",
            "maxLength",
            "$comment",
            "default",
            "examples",
        ];
        for key in map.keys() {
            if !KEYS.contains(&key.as_str()) {
                return Err(format!("Unsupported schema keyword: {key}"));
            }
        }
        if let Some(draft) = map.get("$schema") {
            if draft.as_str() != Some(DRAFT) {
                return Err("Unsupported schema draft".into());
            }
        }
        let string = |key: &str| -> Result<Option<String>, String> {
            map.get(key)
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| format!("{key} must be a string"))
                })
                .transpose()
        };
        let mut node = Node::default();
        node.field.title = string("title")?;
        node.field.description = string("description")?;
        if node.field.title.as_ref().is_some_and(|s| s.len() > 2048)
            || node
                .field
                .description
                .as_ref()
                .is_some_and(|s| s.len() > 8192)
        {
            return Err("Schema title/description exceeds inspection limits".into());
        }
        if let Some(types) = map.get("type") {
            let types = if let Some(t) = types.as_str() {
                vec![t.to_owned()]
            } else {
                types
                    .as_array()
                    .ok_or("type must be a string or array")?
                    .iter()
                    .map(|v| v.as_str().map(str::to_owned).ok_or("Invalid type array"))
                    .collect::<Result<Vec<_>, _>>()?
            };
            if types.is_empty()
                || types.iter().any(|t| {
                    ![
                        "object", "array", "string", "number", "integer", "boolean", "null",
                    ]
                    .contains(&t.as_str())
                })
            {
                return Err("Invalid declared type".into());
            }
            node.field.types = types;
        }
        if let Some(required) = map.get("required") {
            for key in required.as_array().ok_or("required must be an array")? {
                if !node.required.insert(
                    key.as_str()
                        .ok_or("required entries must be strings")?
                        .into(),
                ) {
                    return Err("Duplicate required entry".into());
                }
            }
        }
        if let Some(enumeration) = map.get("enum") {
            let values = enumeration
                .as_array()
                .filter(|a| !a.is_empty())
                .ok_or("enum must be a nonempty array")?;
            node.enumeration = Some(values.clone());
            let text = enumeration.to_string();
            if text.len() > 8192 {
                return Err("Schema enum exceeds 8192 display bytes".into());
            }
            node.field.enumeration = Some(text);
        }
        node.constant = map.get("const").cloned();
        for (key, dest) in [
            ("minimum", &mut node.minimum),
            ("maximum", &mut node.maximum),
        ] {
            if let Some(v) = map.get(key) {
                *dest = Some(
                    v.as_f64()
                        .ok_or_else(|| format!("{key} must be a number"))?,
                );
            }
        }
        for (key, dest) in [
            ("minItems", &mut node.min_items),
            ("maxItems", &mut node.max_items),
            ("minLength", &mut node.min_length),
            ("maxLength", &mut node.max_length),
        ] {
            if let Some(v) = map.get(key) {
                *dest = Some(
                    v.as_u64()
                        .ok_or_else(|| format!("{key} must be a nonnegative integer"))?,
                );
            }
        }
        if let Some(v) = map.get("additionalProperties") {
            node.additional = Some(
                v.as_bool()
                    .ok_or("Only boolean additionalProperties is supported")?,
            );
        }
        if let Some(v) = map.get("properties") {
            for (key, value) in v.as_object().ok_or("properties must be an object")? {
                node.properties
                    .insert(key.clone(), self.compile(value, base, depth + 1)?);
            }
        }
        if let Some(v) = map.get("items") {
            node.items = Some(Box::new(self.compile(v, base, depth + 1)?));
        }
        if let Some(v) = map.get("$ref") {
            node.reference = Some(Box::new(self.reference(
                base,
                v.as_str().ok_or("$ref must be a string")?,
                depth,
            )?));
        }
        for (key, dest) in [
            ("allOf", &mut node.all_of),
            ("anyOf", &mut node.any_of),
            ("oneOf", &mut node.one_of),
        ] {
            if let Some(v) = map.get(key) {
                let items = v
                    .as_array()
                    .filter(|a| !a.is_empty())
                    .ok_or_else(|| format!("{key} must be a nonempty array"))?;
                for item in items {
                    dest.push(self.compile(item, base, depth + 1)?);
                }
            }
        }
        // Preflight even unused definitions: unsupported constraints never become a valid verdict.
        for key in ["$defs", "definitions"] {
            if let Some(v) = map.get(key) {
                for value in v
                    .as_object()
                    .ok_or_else(|| format!("{key} must be an object"))?
                    .values()
                {
                    self.compile(value, base, depth + 1)?;
                }
            }
        }
        if let Some(v) = map.get("$comment") {
            if !v.is_string() {
                return Err("$comment must be a string".into());
            }
        }
        if let Some(v) = map.get("examples") {
            if !v.is_array() {
                return Err("examples must be an array".into());
            }
        }
        Ok(node)
    }
}

struct Validation {
    report: Report,
    steps: usize,
    field_bytes: usize,
    required_paths: BTreeSet<String>,
    started: std::time::Instant,
}
impl Validation {
    fn issue(&mut self, path: &str, message: String) -> Result<(), String> {
        if self.report.violations.len() >= MAX_ISSUES {
            return Err("More than 100 schema violations".into());
        }
        self.report.violations.push(Violation {
            path: path.into(),
            message,
        });
        Ok(())
    }
    fn field(
        &mut self,
        node: &Node,
        path: &str,
        required: bool,
        missing: bool,
        property: bool,
    ) -> Result<(), String> {
        if let Some(reference) = &node.reference {
            self.field(reference, path, required, missing, property)?;
        }
        for branch in &node.all_of {
            self.field(branch, path, required, missing, property)?;
        }
        let mut field = self.report.fields.get(path).cloned().unwrap_or_default();
        field.required |= required || self.required_paths.contains(path);
        field.property |= property;
        field.missing = missing;
        if node.field.title.is_some() {
            field.title.clone_from(&node.field.title);
        }
        if node.field.description.is_some() {
            field.description.clone_from(&node.field.description);
        }
        if !node.field.types.is_empty() {
            field.types.clone_from(&node.field.types);
        }
        if node.field.enumeration.is_some() {
            field.enumeration.clone_from(&node.field.enumeration);
        }
        self.field_bytes += path.len() + field.explain().len();
        if self.field_bytes > DOCUMENT_BYTES || self.report.fields.len() >= MAX_FIELDS {
            return Err("Schema field projection limit reached".into());
        }
        self.report.fields.insert(path.into(), field);
        Ok(())
    }
    fn merge_fields(&mut self, fields: BTreeMap<String, Field>) -> Result<(), String> {
        for (path, incoming) in fields {
            let mut field = match self.report.fields.get(&path) {
                Some(existing) => {
                    let mut merged = existing.clone();
                    merged.missing &= incoming.missing;
                    merged
                }
                None => Field {
                    missing: incoming.missing,
                    ..Default::default()
                },
            };
            field.required |= incoming.required || self.required_paths.contains(&path);
            field.property |= incoming.property;
            if incoming.title.is_some() {
                field.title = incoming.title;
            }
            if incoming.description.is_some() {
                field.description = incoming.description;
            }
            if !incoming.types.is_empty() {
                field.types = incoming.types;
            }
            if incoming.enumeration.is_some() {
                field.enumeration = incoming.enumeration;
            }
            self.field_bytes += path.len() + field.explain().len();
            if self.field_bytes > DOCUMENT_BYTES || self.report.fields.len() >= MAX_FIELDS {
                return Err("Schema field projection limit reached".into());
            }
            self.report.fields.insert(path, field);
        }
        Ok(())
    }
    fn evaluate_branch(
        &mut self,
        branch: &Node,
        value: &Value,
        path: &str,
        depth: usize,
    ) -> Result<Option<BTreeMap<String, Field>>, String> {
        let mut probe = Validation {
            report: Report {
                schema: None,
                verdict: Verdict::Valid,
                summary: String::new(),
                fields: BTreeMap::new(),
                violations: Vec::new(),
                array_kinds: Vec::new(),
            },
            steps: self.steps,
            field_bytes: self.field_bytes,
            required_paths: self.required_paths.clone(),
            started: self.started,
        };
        let result = probe
            .field(branch, path, false, false, false)
            .and_then(|()| probe.visit(branch, value, path, depth + 1));
        self.steps = probe.steps;
        result?;
        if probe.report.violations.is_empty() {
            Ok(Some(probe.report.fields))
        } else {
            Ok(None)
        }
    }
    fn visit(
        &mut self,
        node: &Node,
        value: &Value,
        path: &str,
        depth: usize,
    ) -> Result<(), String> {
        self.steps += 1;
        if self.started.elapsed() > std::time::Duration::from_secs(2) {
            return Err("Schema validation exceeded two seconds".into());
        }
        if self.steps > MAX_STEPS || depth > 64 {
            return Err("Schema validation exceeds 100000 steps or depth 64".into());
        }
        if let Some(reference) = &node.reference {
            self.visit(reference, value, path, depth + 1)?;
        }
        for branch in &node.all_of {
            self.field(branch, path, false, false, false)?;
            self.visit(branch, value, path, depth + 1)?;
        }
        if !node.any_of.is_empty() {
            let mut matched = Vec::new();
            for branch in &node.any_of {
                if let Some(fields) = self.evaluate_branch(branch, value, path, depth)? {
                    matched.push(fields);
                }
            }
            if matched.is_empty() {
                self.issue(path, "Value does not match any schema in anyOf".into())?;
            } else {
                for fields in matched {
                    self.merge_fields(fields)?;
                }
            }
        }
        if !node.one_of.is_empty() {
            let mut matched = Vec::new();
            for branch in &node.one_of {
                if let Some(fields) = self.evaluate_branch(branch, value, path, depth)? {
                    matched.push(fields);
                }
            }
            match matched.len() {
                0 => self.issue(path, "Value does not match any schema in oneOf".into())?,
                1 => self.merge_fields(matched.pop().unwrap())?,
                _ => self.issue(path, "Value matches more than one schema in oneOf".into())?,
            }
        }
        if node.boolean == Some(false) {
            self.issue(path, "Schema disallows this value".into())?;
        }
        if !node.field.types.is_empty()
            && !node.field.types.iter().any(|t| match t.as_str() {
                "object" => value.is_object(),
                "array" => value.is_array(),
                "string" => value.is_string(),
                "number" => value.is_number(),
                "integer" => value.as_f64().is_some_and(|n| n.fract() == 0.),
                "boolean" => value.is_boolean(),
                "null" => value.is_null(),
                _ => false,
            })
        {
            self.issue(
                path,
                format!("Expected type {}", node.field.types.join(" | ")),
            )?;
        }
        if node
            .enumeration
            .as_ref()
            .is_some_and(|values| !values.iter().any(|candidate| equal(candidate, value)))
        {
            self.issue(
                path,
                format!(
                    "Value is outside enum {}",
                    node.field.enumeration.as_deref().unwrap_or_default()
                ),
            )?;
        }
        if node
            .constant
            .as_ref()
            .is_some_and(|constant| !equal(constant, value))
        {
            self.issue(path, "Value differs from const".into())?;
        }
        if let Some(n) = value.as_f64() {
            if node.minimum.is_some_and(|m| n < m) {
                self.issue(
                    path,
                    format!("Value is below minimum {}", node.minimum.unwrap()),
                )?;
            }
            if node.maximum.is_some_and(|m| n > m) {
                self.issue(
                    path,
                    format!("Value is above maximum {}", node.maximum.unwrap()),
                )?;
            }
        }
        if let Some(s) = value.as_str() {
            let len = s.chars().count() as u64;
            if node.min_length.is_some_and(|m| len < m) {
                self.issue(path, "String is shorter than minLength".into())?;
            }
            if node.max_length.is_some_and(|m| len > m) {
                self.issue(path, "String is longer than maxLength".into())?;
            }
        }
        if let Some(array) = value.as_array() {
            if node.min_items.is_some_and(|m| (array.len() as u64) < m) {
                self.issue(path, "Array has fewer than minItems".into())?;
            }
            if node.max_items.is_some_and(|m| (array.len() as u64) > m) {
                self.issue(path, "Array has more than maxItems".into())?;
            }
            if let Some(items) = &node.items {
                for (i, value) in array.iter().enumerate() {
                    let path = pointer_child(path, &i.to_string());
                    self.field(items, &path, false, false, false)?;
                    self.visit(items, value, &path, depth + 1)?;
                }
            }
        }
        if let Some(object) = value.as_object() {
            for key in &node.required {
                let pointer = pointer_child(path, key);
                self.required_paths.insert(pointer.clone());
                // required can be declared separately from properties, including
                // in a reference whose sibling supplies the property metadata.
                self.field(
                    &Node::default(),
                    &pointer,
                    true,
                    !object.contains_key(key),
                    true,
                )?;
                if !object.contains_key(key) {
                    self.issue(
                        &pointer_child(path, key),
                        "Required property is missing (location: containing object)".into(),
                    )?;
                }
            }
            for (key, property) in &node.properties {
                let pointer = pointer_child(path, key);
                self.field(
                    property,
                    &pointer,
                    node.required.contains(key),
                    !object.contains_key(key),
                    true,
                )?;
                if let Some(value) = object.get(key) {
                    self.visit(property, value, &pointer, depth + 1)?;
                }
            }
            if node.additional == Some(false) {
                for key in object.keys() {
                    if !node.properties.contains_key(key) {
                        self.issue(
                            &pointer_child(path, key),
                            "Additional property is not allowed".into(),
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
}
fn infile_schema(association: &Association, text: &str) -> Result<Option<PathBuf>, String> {
    let Some(document) = association.document.as_deref() else {
        return Ok(None);
    };
    if text.len() > DOCUMENT_BYTES {
        return Ok(None);
    }
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if !trimmed.starts_with('{') {
        return Ok(None);
    }
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(trimmed) else {
        return Ok(None);
    };
    let Some(raw) = map.get("$schema").and_then(Value::as_str) else {
        return Ok(None);
    };
    if raw.starts_with("https://json-schema.org/") || raw.starts_with("http://json-schema.org/") {
        // The document itself declares a JSON Schema meta-schema, not a local instance schema.
        return Ok(None);
    }
    if raw.contains("://") {
        return Err(format!(
            "Remote or URI $schema is not supported (no remote fetches): {raw}"
        ));
    }
    if !relative_local_path(raw) {
        return Err(format!(
            "In-file $schema must be a local relative path inside the project: {raw}"
        ));
    }
    let parent = document
        .parent()
        .ok_or_else(|| "Document has no parent directory".to_owned())?;
    Ok(Some(parent.join(raw)))
}

/// Validates a snapshot only. Any unsupported draft/keyword/ref makes the whole
/// schema unavailable; resource limits are Incomplete, never Valid.
///
/// Schema association precedence (deterministic):
/// 1. Explicit `.okilum/viewers.json` `schemas` rule (last match wins).
/// 2. Standard in-file `"$schema": "./path/to/schema.json"` local relative path,
///    resolved relative to the document and bounded inside the project/vault root.
pub fn inspect(association: &Association, text: &str) -> Option<Report> {
    let schema = match association.schema.clone() {
        Some(schema) => schema,
        None => match infile_schema(association, text) {
            Ok(Some(schema)) => schema,
            Ok(None) => return None,
            Err(error) => return Some(Report::unavailable(None, error)),
        },
    };
    let result = (|| {
        let root = association
            .project
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let mut loader = Loader {
            root,
            files: BTreeMap::new(),
            bytes: 0,
            nodes: 0,
            stack: BTreeSet::new(),
            started: std::time::Instant::now(),
        };
        loader.reference(&schema, "#", 0)
    })();
    let node = match result {
        Ok(node) => node,
        Err(e) => return Some(Report::unavailable(Some(schema), e)),
    };
    let mut validation = Validation {
        report: Report {
            schema: Some(schema),
            verdict: Verdict::Valid,
            summary: String::new(),
            fields: BTreeMap::new(),
            violations: Vec::new(),
            array_kinds: Vec::new(),
        },
        steps: 0,
        field_bytes: 0,
        required_paths: BTreeSet::new(),
        started: std::time::Instant::now(),
    };
    let result = if text.len() > DOCUMENT_BYTES {
        Err("Document exceeds schema validation limit of 4 MiB".into())
    } else {
        match parse_unique(text.trim_start_matches('\u{feff}').as_bytes()) {
            Ok(value) => {
                validation.report.array_kinds = value
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .take(100_000)
                            .map(|item| match item {
                                Value::Object(_) => ItemKind::Object,
                                Value::Array(_) => ItemKind::Array,
                                _ => ItemKind::Scalar,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                safe_numbers(&value)
                    .and_then(|()| validation.field(&node, "", false, false, false))
                    .and_then(|()| validation.visit(&node, &value, "", 0))
            }
            Err(e) if e.to_string().contains("Duplicate JSON property") => {
                Err(format!("Ambiguous document: {e}"))
            }
            Err(e) => {
                validation.report.verdict = Verdict::SyntaxInvalid;
                validation.report.summary = format!(
                    "Schema validation not run: JSON syntax error at line {}, column {}",
                    e.line(),
                    e.column()
                );
                return Some(validation.report);
            }
        }
    };
    validation.report.summary = match result {
        Err(e) => {
            validation.report.verdict = Verdict::Incomplete;
            format!(
                "Schema validation incomplete: {e}; {} violations found so far",
                validation.report.violations.len()
            )
        }
        Ok(()) if !validation.report.violations.is_empty() => {
            validation.report.verdict = Verdict::Invalid;
            format!(
                "Schema validation: {} violations",
                validation.report.violations.len()
            )
        }
        Ok(()) => "Schema validation: valid for the supported Draft 2020-12 subset".into(),
    };
    Some(validation.report)
}

#[cfg(test)]
mod tests;
