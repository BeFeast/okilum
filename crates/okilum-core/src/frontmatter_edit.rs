//! Revision-bound single-field YAML frontmatter edits for declarative view `set` actions.
//!
//! The read side (`crate::records` and `crate::properties`) stays untouched.
//! This module is intentionally stricter and fails closed whenever the frontmatter
//! cannot be edited losslessly on a single top-level `key: value` line (or appended
//! as one line at the end of a plain block mapping when the key is absent).
//! Every planned edit is verified in memory against the main read projection and
//! byte-preservation invariants before returning a `Plan` or writing through `FileEditor`.

use crate::records::{Field, FieldSchema, FieldType, NoteRecord, Projection, Source, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

/// Closed allowlist of scalar note frontmatter fields writable by view `set` actions.
pub const ALLOWED_FIELDS: [&str; 3] = ["status", "owner", "due"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    path: String,
    revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub path: String,
    pub before_revision: String,
    pub after_revision: String,
    pub before: String,
    pub after: String,
}

pub fn revision(source: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(source.as_bytes()))
}

impl Target {
    /// Create a target bound to the exact vault-relative path and SHA-256 revision
    /// observed in the displayed view projection.
    pub fn new(
        path: impl Into<String>,
        expected_revision: impl Into<String>,
    ) -> Result<Self, String> {
        let path = path.into();
        let expected_revision = expected_revision.into();
        validate_relative_path(&path)?;
        if !is_valid_revision(&expected_revision) {
            return Err("Invalid note revision".into());
        }
        Ok(Self {
            path,
            revision: expected_revision,
        })
    }

    /// Capture from an exact source snapshot.
    pub fn capture(path: &str, source: &str) -> Result<Self, String> {
        Self::new(path, revision(source))
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn source_revision(&self) -> &str {
        &self.revision
    }

    /// Plan a lossless single-field frontmatter `set` mutation against `source`.
    /// Fails closed on stale revisions, disallowed fields, invalid values,
    /// unsupported frontmatter syntax, or post-condition mismatch.
    pub fn plan(
        &self,
        source: &str,
        schema: &Arc<FieldSchema>,
        field: &str,
        value: &str,
    ) -> Result<Plan, String> {
        if revision(source) != self.revision {
            return Err("The note changed, refresh.".into());
        }
        if !schema.includes(&self.path) {
            return Err(format!(
                "Note '{}' is outside the writable view scope",
                self.path
            ));
        }
        if has_lone_cr(source) {
            return Err("Unsupported lone carriage return in note".into());
        }

        // 1. Validate field against closed allowlist and schema writable_by_actions.
        let field_def = validate_writable_field(schema, field)?;

        // 2. Locate and validate the exact frontmatter envelope (same boundary as read side).
        let envelope = parse_frontmatter_envelope(source)?;

        // 3. Project the before-state with the exact main read projection (also gives note layer).
        let before_projection = Projection::build(
            Arc::clone(schema),
            [Source {
                path: &self.path,
                bytes: source.as_bytes(),
                mtime: None,
            }],
        );
        let before_record = before_projection
            .note(&self.path)
            .ok_or_else(|| format!("Note '{}' is unavailable in projection", self.path))?;

        // 4. Validate value against fields.yaml schema rules and note scope.
        let expected_value = validate_field_value(field, field_def, value, before_record)?;

        // 5. Inspect frontmatter structure strictly (plain top-level block mapping only).
        let before_props = crate::properties::parse_for_records(envelope.yaml)
            .map_err(|err| format!("Frontmatter could not be parsed: {err}"))?;
        let analysis = analyze_block_mapping(source, &envelope, field, false)?;

        // 6. Compute the single-line edit (or single-line append if key absent).
        let (after, edit_span, replacement_len, added_lines) = match analysis.target {
            Some(entry) => plan_update_line(source, &entry, field, value)?,
            None => plan_append_line(source, &envelope, field, value),
        };

        // 7. In-memory post-condition verification before returning the Plan.
        verify_postcondition(
            &self.path,
            schema,
            field,
            &expected_value,
            source,
            &after,
            &envelope,
            &before_props,
            before_record,
            edit_span,
            replacement_len,
            added_lines,
        )?;

        Ok(Plan {
            path: self.path.clone(),
            before_revision: self.revision.clone(),
            after_revision: revision(&after),
            before: source.to_owned(),
            after,
        })
    }
}

impl Plan {
    /// Guard before passing `before` to FileEditor as an Undo replacement.
    /// FileEditor still validates again under its lock during atomic save.
    pub fn undo_source<'a>(&'a self, current: &str) -> Result<&'a str, String> {
        if revision(current) != self.after_revision {
            return Err("This note changed again. Undo cannot replace the newer content.".into());
        }
        Ok(&self.before)
    }
}

/// Set the two case fields using the same scalar spans as declarative edits.
pub(crate) fn activate_case(source: &str, today: &str) -> Result<String, String> {
    let parse = |yaml: &str| {
        serde_yaml::from_str::<serde_yaml::Mapping>(yaml)
            .map_err(|error| format!("Invalid case frontmatter: {error}"))
    };
    let before = parse_frontmatter_envelope(source)?;
    let mut expected = parse(before.yaml)?;
    if expected.get("type").and_then(serde_yaml::Value::as_str) != Some("case") {
        return Err("The note is not a case".into());
    }
    let mut after = source.to_owned();
    for (field, value) in [("case_status", "active"), ("updated", today)] {
        let envelope = parse_frontmatter_envelope(&after)?;
        let entry = analyze_block_mapping(&after, &envelope, field, true)?.target;
        let (edited, span, inserted_len, _) = match entry {
            Some(entry) => plan_update_line(&after, &entry, field, value)?,
            None => plan_append_line(&after, &envelope, field, value),
        };
        if edited[..span.start] != after[..span.start]
            || edited[span.start + inserted_len..] != after[span.end..]
        {
            return Err("Case edit did not preserve unrelated source bytes".into());
        }
        expected.insert(field.into(), value.into());
        after = edited;
    }
    if parse(parse_frontmatter_envelope(&after)?.yaml)? != expected {
        return Err("Could not verify the edited case frontmatter".into());
    }
    Ok(after)
}

pub(crate) fn validate_relative_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.contains(['\\', ':', '\0'])
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err("Invalid note path".into());
    }
    Ok(())
}

fn is_valid_revision(rev: &str) -> bool {
    let Some(hex) = rev.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(crate) fn has_lone_cr(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes
        .iter()
        .enumerate()
        .any(|(i, &b)| b == b'\r' && bytes.get(i + 1) != Some(&b'\n'))
}

fn validate_writable_field<'a>(schema: &'a FieldSchema, field: &str) -> Result<&'a Field, String> {
    if !ALLOWED_FIELDS.contains(&field) {
        return Err(format!(
            "Field '{field}' is not writable by view actions (allowed: status, owner, due)"
        ));
    }
    let Some(def) = schema.fields.get(field) else {
        return Err(format!("Field '{field}' is not defined in fields.yaml"));
    };
    if !def.writable_by_actions
        || matches!(def.field_type, FieldType::List(_))
        || def.field_type.is_opaque()
    {
        return Err(format!("Field '{field}' is not a writable scalar field"));
    }
    Ok(def)
}

fn validate_field_value(
    field: &str,
    def: &Field,
    value: &str,
    record: &NoteRecord,
) -> Result<Value, String> {
    if value.is_empty() || value.trim() != value || value.chars().any(char::is_control) {
        return Err(format!("Invalid value {value:?} for '{field}'"));
    }
    match &def.field_type {
        FieldType::Enum => {
            if field == "status" {
                let layer = match record.fields.get("layer") {
                    Some(Value::String(layer)) => Some(layer.as_str()),
                    _ => None,
                };
                let is_decision = matches!(
                    record.fields.get("kind"),
                    Some(Value::String(kind)) if kind == "decisions"
                ) || matches!(
                    record.fields.get("type"),
                    Some(Value::String(kind)) if kind == "decision"
                );
                if is_decision || !matches!(layer, None | Some("state")) {
                    return Err(
                        "Field 'status' is only valid on state or legacy non-decision notes".into(),
                    );
                }
                let layer_key = layer.unwrap_or("legacy");
                let allowed = def
                    .values_by_layer
                    .get(layer_key)
                    .map(Vec::as_slice)
                    .unwrap_or(def.values.as_slice());
                if !allowed.iter().any(|candidate| candidate == value) {
                    return Err(format!(
                        "Invalid status '{value}' (expected one of: {})",
                        allowed.join(", ")
                    ));
                }
                return Ok(Value::String(value.to_owned()));
            }
            if !def.values.iter().any(|candidate| candidate == value) {
                return Err(format!("Invalid enum value '{value}' for '{field}'"));
            }
            Ok(Value::String(value.to_owned()))
        }
        FieldType::Date => {
            let date = crate::records::value::date(value)
                .filter(|d| (1..=9999).contains(&d.year()))
                .ok_or_else(|| {
                    format!("Invalid date '{value}' for '{field}' (expected YYYY-MM-DD)")
                })?;
            Ok(Value::Date(date))
        }
        FieldType::String => {
            if !def.valid_string(value) {
                return Err(format!(
                    "Invalid value '{value}' for '{field}' (expected lowercase slug [a-z0-9-]+)"
                ));
            }
            Ok(Value::String(value.to_owned()))
        }
        _ => Err(format!("Unsupported writable field type for '{field}'")),
    }
}

struct FrontmatterEnvelope<'a> {
    yaml: &'a str,
    body: &'a str,
    fm_start: usize,
    fm_end: usize,
    eol: &'static str,
}

fn parse_frontmatter_envelope(source: &str) -> Result<FrontmatterEnvelope<'_>, String> {
    let Some((yaml, body)) = crate::render::split_frontmatter(source) else {
        return Err("Note has no YAML frontmatter block".into());
    };
    let bom_len = if source.starts_with('\u{feff}') {
        '\u{feff}'.len_utf8()
    } else {
        0
    };
    let after_bom = &source[bom_len..];
    let (eol, open_len) = if after_bom.starts_with("---\r\n") {
        ("\r\n", 5)
    } else if after_bom.starts_with("---\n") {
        ("\n", 4)
    } else {
        return Err("Note has no YAML frontmatter block".into());
    };
    let fm_start = bom_len + open_len;
    let fm_end = fm_start + yaml.len();
    let closing_end = source.len() - body.len();
    let closing = &source[fm_end..closing_end];

    if yaml.contains('\u{feff}') || closing.contains('\u{feff}') {
        return Err("Unexpected UTF-8 BOM inside frontmatter".into());
    }
    if eol == "\r\n" {
        if !closing.ends_with("\r\n") {
            return Err("Mixed line endings in frontmatter".into());
        }
        for line in yaml.split_inclusive('\n') {
            if !line.ends_with("\r\n") {
                return Err("Mixed line endings in frontmatter".into());
            }
        }
    } else {
        if closing.ends_with("\r\n") {
            return Err("Mixed line endings in frontmatter".into());
        }
        for line in yaml.split_inclusive('\n') {
            if line.ends_with("\r\n") {
                return Err("Mixed line endings in frontmatter".into());
            }
        }
    }
    Ok(FrontmatterEnvelope {
        yaml,
        body,
        fm_start,
        fm_end,
        eol,
    })
}

struct TopEntry {
    key: String,
    colon_abs: usize,
    line_end_no_eol: usize,
    has_continuation_lines: bool,
    is_block_scalar_header: bool,
}

struct FrontmatterAnalysis {
    target: Option<TopEntry>,
}

fn analyze_block_mapping(
    source: &str,
    envelope: &FrontmatterEnvelope<'_>,
    target_field: &str,
    reject_flow_mapping: bool,
) -> Result<FrontmatterAnalysis, String> {
    let yaml = envelope.yaml;
    if yaml.trim().is_empty() {
        // Empty frontmatter block (`---\n---\n`): safe to append the first key.
        return Ok(FrontmatterAnalysis { target: None });
    }

    let parsed: serde_yaml::Value =
        serde_yaml::from_str(yaml).map_err(|err| format!("Invalid YAML frontmatter: {err}"))?;
    let serde_yaml::Value::Mapping(ref map) = parsed else {
        return Err("Frontmatter is not a plain block mapping".into());
    };

    // Reject YAML tags and merge keys anywhere in the frontmatter AST, and reject
    // the target field if it appears inside any nested mapping.
    check_ast_constraints(map, target_field)?;

    // Line-by-line strict top-level block-mapping scan.
    let mut entries: Vec<TopEntry> = Vec::new();
    let mut seen_keys: HashSet<String> = HashSet::new();
    let mut block_scalar_indent: Option<usize> = None;
    let mut flow_depth: usize = 0;
    let mut open_quote: Option<char> = None;
    let mut current_entry: Option<usize> = None;

    let mut offset = envelope.fm_start;
    for raw_with_eol in yaml.split_inclusive('\n') {
        let line_abs_start = offset;
        offset += raw_with_eol.len();
        let raw = raw_with_eol
            .strip_suffix(envelope.eol)
            .ok_or_else(|| "Mixed line endings in frontmatter".to_owned())?;
        let line_end_no_eol = line_abs_start + raw.len();

        if let Some(header_indent) = block_scalar_indent {
            if raw.bytes().all(|b| b == b' ') {
                continue;
            }
            if raw.starts_with('\t') {
                return Err("Tab indentation in frontmatter is not supported".into());
            }
            let indent = raw.bytes().take_while(|&b| b == b' ').count();
            if indent > header_indent {
                if let Some(cur) = current_entry {
                    entries[cur].has_continuation_lines = true;
                }
                continue;
            }
            block_scalar_indent = None;
        }

        if open_quote.is_some() {
            let Some(cur) = current_entry else {
                return Err("Frontmatter is not a plain block mapping".into());
            };
            entries[cur].has_continuation_lines = true;
            scan_yaml_fragment(
                raw,
                &mut open_quote,
                &mut flow_depth,
                false,
                reject_flow_mapping,
            )?;
            if flow_depth > 0 {
                return Err("Multi-line flow collections in frontmatter are not supported".into());
            }
            continue;
        }

        if raw.bytes().all(|b| matches!(b, b' ' | b'\t')) {
            continue;
        }
        let indent = raw.bytes().take_while(|&b| b == b' ').count();
        let trimmed_start = &raw[indent..];
        if trimmed_start.starts_with('#') {
            continue;
        }
        if raw.starts_with('\t') || trimmed_start.starts_with('\t') {
            return Err("Tab indentation in frontmatter is not supported".into());
        }

        if indent > 0 {
            let Some(cur) = current_entry else {
                return Err("Frontmatter keys are indented; edit the note directly".into());
            };
            entries[cur].has_continuation_lines = true;
            let starts_block = scan_indented_line(
                trimmed_start,
                indent,
                &mut open_quote,
                &mut flow_depth,
                reject_flow_mapping,
            )?;
            if flow_depth > 0 {
                return Err("Multi-line flow collections in frontmatter are not supported".into());
            }
            if starts_block {
                block_scalar_indent = Some(indent);
            }
            continue;
        }

        // Column 0 non-blank, non-comment line: must be a top-level mapping entry.
        let first = raw.as_bytes()[0];
        match first {
            b'{' | b'}' | b'[' | b']' | b',' => {
                return Err("Flow-style frontmatter mapping is not supported".into());
            }
            b'-' => {
                return Err("Frontmatter is not a block mapping".into());
            }
            b'?' => {
                return Err("Complex YAML keys in frontmatter are not supported".into());
            }
            b'%' => {
                return Err("YAML directives in frontmatter are not supported".into());
            }
            b'&' | b'*' | b'!' => {
                return Err(
                    "YAML anchors, aliases, and tags in frontmatter are not supported".into(),
                );
            }
            b'|' | b'>' | b':' | b'@' | b'`' => {
                return Err("Frontmatter is not a plain block mapping".into());
            }
            _ => {}
        }

        let (key, colon_rel) = parse_top_level_key(raw)?;
        if key == "<<" {
            return Err("YAML merge keys in frontmatter are not supported".into());
        }
        if !seen_keys.insert(key.clone()) {
            return Err(format!("Duplicate frontmatter key '{key}'"));
        }
        let after_colon = &raw[colon_rel + 1..];
        let is_block_scalar_header = scan_value_after_colon(
            after_colon,
            &mut open_quote,
            &mut flow_depth,
            reject_flow_mapping,
        )?;
        if flow_depth > 0 {
            return Err("Multi-line flow collections in frontmatter are not supported".into());
        }
        if is_block_scalar_header {
            block_scalar_indent = Some(0);
        }

        entries.push(TopEntry {
            key,
            colon_abs: line_abs_start + colon_rel,
            line_end_no_eol,
            has_continuation_lines: false,
            is_block_scalar_header,
        });
        current_entry = Some(entries.len() - 1);
    }

    if open_quote.is_some() || flow_depth > 0 {
        return Err("Unterminated flow collection or quoted string in frontmatter".into());
    }

    // Fail closed if serde_yaml and our top-level block-mapping scan disagree.
    if entries.len() != map.len() {
        return Err("Frontmatter structure could not be verified losslessly".into());
    }
    for entry in &entries {
        if !map.contains_key(serde_yaml::Value::String(entry.key.clone())) {
            return Err("Frontmatter structure could not be verified losslessly".into());
        }
    }

    let target = entries.into_iter().find(|entry| entry.key == target_field);
    if let Some(ref entry) = target {
        // Also ensure serde_yaml saw a scalar or null value for the target key.
        let yaml_val = map.get(serde_yaml::Value::String(entry.key.clone()));
        if !matches!(
            yaml_val,
            Some(
                serde_yaml::Value::Null
                    | serde_yaml::Value::String(_)
                    | serde_yaml::Value::Bool(_)
                    | serde_yaml::Value::Number(_)
            )
        ) {
            return Err(format!("Field '{}' is not a plain scalar value", entry.key));
        }
        // Double check source slicing aligns.
        if source.as_bytes().get(entry.colon_abs) != Some(&b':') {
            return Err("Frontmatter structure could not be verified losslessly".into());
        }
    }

    Ok(FrontmatterAnalysis { target })
}

fn check_ast_constraints(map: &serde_yaml::Mapping, target_field: &str) -> Result<(), String> {
    fn walk_value(
        value: &serde_yaml::Value,
        target_field: &str,
        in_nested: bool,
    ) -> Result<(), String> {
        match value {
            serde_yaml::Value::Tagged(_) => {
                Err("YAML anchors, aliases, and tags in frontmatter are not supported".into())
            }
            serde_yaml::Value::Sequence(items) => {
                for item in items {
                    walk_value(item, target_field, true)?;
                }
                Ok(())
            }
            serde_yaml::Value::Mapping(mapping) => {
                for (k, v) in mapping {
                    if let serde_yaml::Value::Tagged(_) = k {
                        return Err(
                            "YAML anchors, aliases, and tags in frontmatter are not supported"
                                .into(),
                        );
                    }
                    if let Some(name) = k.as_str() {
                        if name == "<<" {
                            return Err("YAML merge keys in frontmatter are not supported".into());
                        }
                        if in_nested && name == target_field {
                            return Err(format!(
                                "Field '{target_field}' appears inside a nested mapping"
                            ));
                        }
                    }
                    walk_value(v, target_field, true)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    for (k, v) in map {
        if let serde_yaml::Value::Tagged(_) = k {
            return Err("YAML anchors, aliases, and tags in frontmatter are not supported".into());
        }
        if k.as_str() == Some("<<") {
            return Err("YAML merge keys in frontmatter are not supported".into());
        }
        walk_value(v, target_field, true)?;
    }
    Ok(())
}

fn parse_top_level_key(raw: &str) -> Result<(String, usize), String> {
    let bytes = raw.as_bytes();
    if bytes[0] == b'"' || bytes[0] == b'\'' {
        let quote = bytes[0];
        let close = find_closing_quote(raw, 0, quote).ok_or_else(|| {
            "Multi-line or unclosed key in frontmatter is not supported".to_owned()
        })?;
        let raw_key = &raw[..=close];
        let mut rest_ix = close + 1;
        while rest_ix < bytes.len() && matches!(bytes[rest_ix], b' ' | b'\t') {
            rest_ix += 1;
        }
        if bytes.get(rest_ix) != Some(&b':') {
            return Err("Frontmatter line is not a plain key: value entry".into());
        }
        if let Some(&after) = bytes.get(rest_ix + 1) {
            if !matches!(after, b' ' | b'\t') {
                return Err("Frontmatter line is not a plain key: value entry".into());
            }
        }
        let decoded: String = serde_yaml::from_str(raw_key)
            .map_err(|_| "Unsupported quoted key in frontmatter".to_owned())?;
        return Ok((decoded, rest_ix));
    }

    let mut colon_rel = None;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b':' && matches!(bytes.get(i + 1), None | Some(b' ') | Some(b'\t')) {
            colon_rel = Some(i);
            break;
        }
    }
    let Some(colon_rel) = colon_rel else {
        return Err("Frontmatter line is not a plain key: value entry".into());
    };
    let key = raw[..colon_rel].trim_end_matches([' ', '\t']);
    if key.is_empty() || key.contains(['#', '{', '}', '[', ']', ',', '?', '&', '*', '!', '"', '\''])
    {
        return Err("Unsupported frontmatter key syntax".into());
    }
    Ok((key.to_owned(), colon_rel))
}

fn find_closing_quote(text: &str, start: usize, quote: u8) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = start + 1;
    while i < bytes.len() {
        if quote == b'"' {
            if bytes[i] == b'\\' {
                i += 2;
                continue;
            }
            if bytes[i] == b'"' {
                return Some(i);
            }
        } else if bytes[i] == b'\'' {
            if bytes.get(i + 1) == Some(&b'\'') {
                i += 2;
                continue;
            }
            return Some(i);
        }
        i += 1;
    }
    None
}

fn is_block_scalar_indicator(trimmed: &str) -> bool {
    let Some(first) = trimmed.as_bytes().first() else {
        return false;
    };
    if !matches!(first, b'|' | b'>') {
        return false;
    }
    let rest = &trimmed[1..];
    // Optional chomping (`+`/`-`) and indentation digit (`1`-`9`), in either order,
    // followed by end-of-line or whitespace (+ optional comment).
    let mut chars = rest.chars().peekable();
    let mut seen_chomp = false;
    let mut seen_indent = false;
    while let Some(&ch) = chars.peek() {
        if matches!(ch, '+' | '-') && !seen_chomp {
            seen_chomp = true;
            chars.next();
        } else if ('1'..='9').contains(&ch) && !seen_indent {
            seen_indent = true;
            chars.next();
        } else {
            break;
        }
    }
    let rem: String = chars.collect();
    let rem_trimmed = rem.trim_start_matches([' ', '\t']);
    rem.is_empty()
        || (rem.starts_with([' ', '\t'])
            && (rem_trimmed.is_empty() || rem_trimmed.starts_with('#')))
}

fn has_node_property_prefix(trimmed: &str) -> bool {
    let bytes = trimmed.as_bytes();
    match bytes.first() {
        Some(b'&' | b'*') => bytes
            .get(1)
            .is_some_and(|b| !matches!(b, b' ' | b'\t' | b',' | b'[' | b']' | b'{' | b'}')),
        Some(b'!') => true,
        _ => false,
    }
}

fn scan_value_after_colon(
    after_colon: &str,
    open_quote: &mut Option<char>,
    flow_depth: &mut usize,
    reject_flow_mapping: bool,
) -> Result<bool, String> {
    let trimmed = after_colon.trim_start_matches([' ', '\t']);
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return Ok(false);
    }
    if has_node_property_prefix(trimmed) {
        return Err("YAML anchors, aliases, and tags in frontmatter are not supported".into());
    }
    if is_block_scalar_indicator(trimmed) {
        return Ok(true);
    }
    scan_yaml_fragment(trimmed, open_quote, flow_depth, true, reject_flow_mapping)?;
    Ok(false)
}

fn scan_indented_line(
    trimmed: &str,
    _indent: usize,
    open_quote: &mut Option<char>,
    flow_depth: &mut usize,
    reject_flow_mapping: bool,
) -> Result<bool, String> {
    let mut rest = trimmed;
    loop {
        if has_node_property_prefix(rest) {
            return Err("YAML anchors, aliases, and tags in frontmatter are not supported".into());
        }
        if rest == "?" || rest.starts_with("? ") || rest.starts_with("?\t") {
            return Err("Complex YAML keys in frontmatter are not supported".into());
        }
        if let Some(after_dash) = rest
            .strip_prefix("- ")
            .or_else(|| rest.strip_prefix("-\t"))
            .or_else(|| (rest == "-").then_some(""))
        {
            rest = after_dash.trim_start_matches([' ', '\t']);
            if rest.is_empty() || rest.starts_with('#') {
                return Ok(false);
            }
            if is_block_scalar_indicator(rest) {
                return Ok(true);
            }
            continue;
        }
        break;
    }
    // Nested mapping entry `subkey: ...`
    if let Some(colon) = find_unquoted_mapping_colon(rest) {
        let val = &rest[colon + 1..];
        return scan_value_after_colon(val, open_quote, flow_depth, reject_flow_mapping);
    }
    scan_yaml_fragment(rest, open_quote, flow_depth, true, reject_flow_mapping)?;
    Ok(false)
}

fn find_unquoted_mapping_colon(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    let mut flow: usize = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            if q == b'"' && b == b'\\' {
                i += 2;
                continue;
            }
            if q == b'\'' && b == b'\'' && bytes.get(i + 1) == Some(&b'\'') {
                i += 2;
                continue;
            }
            if b == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' if i == 0 => quote = Some(b),
            b'[' | b'{' => flow += 1,
            b']' | b'}' => flow = flow.saturating_sub(1),
            b'#' if i == 0 || matches!(bytes[i - 1], b' ' | b'\t') => break,
            b':' if flow == 0 && matches!(bytes.get(i + 1), None | Some(b' ') | Some(b'\t')) => {
                return Some(i);
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn scan_yaml_fragment(
    text: &str,
    open_quote: &mut Option<char>,
    flow_depth: &mut usize,
    mut at_node_start: bool,
    reject_flow_mapping: bool,
) -> Result<(), String> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(q) = *open_quote {
            let qb = q as u8;
            if qb == b'"' && bytes[i] == b'\\' {
                i += 2;
                continue;
            }
            if qb == b'\'' && bytes[i] == b'\'' && bytes.get(i + 1) == Some(&b'\'') {
                i += 2;
                continue;
            }
            if bytes[i] == qb {
                *open_quote = None;
            }
            i += 1;
            continue;
        }

        let b = bytes[i];
        if b == b'#' && (i == 0 || matches!(bytes[i - 1], b' ' | b'\t')) {
            break;
        }
        if matches!(b, b' ' | b'\t') {
            i += 1;
            continue;
        }
        if at_node_start && has_node_property_prefix(&text[i..]) {
            return Err("YAML anchors, aliases, and tags in frontmatter are not supported".into());
        }
        match b {
            b'"' | b'\'' if at_node_start => {
                *open_quote = Some(b as char);
                at_node_start = false;
            }
            b'{' if reject_flow_mapping && (at_node_start || *flow_depth > 0) => {
                return Err("Flow-style frontmatter mapping is not supported".into());
            }
            b'[' | b'{' => {
                *flow_depth += 1;
                at_node_start = true;
            }
            b']' | b'}' => {
                *flow_depth = flow_depth.saturating_sub(1);
                at_node_start = false;
            }
            b',' if *flow_depth > 0 => {
                at_node_start = true;
            }
            b':' if *flow_depth > 0
                && matches!(
                    bytes.get(i + 1),
                    None | Some(b' ' | b'\t' | b',' | b']' | b'}' | b'&' | b'*' | b'!')
                ) =>
            {
                if reject_flow_mapping {
                    return Err("Flow-style frontmatter mapping is not supported".into());
                }
                at_node_start = true;
            }
            _ => {
                at_node_start = false;
            }
        }
        i += 1;
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QuoteStyle {
    Plain,
    Double,
    Single,
}

fn needs_yaml_quotes(value: &str) -> bool {
    if value.is_empty()
        || value.starts_with('-')
        || matches!(
            value.to_ascii_lowercase().as_str(),
            "true" | "false" | "yes" | "no" | "on" | "off" | "y" | "n" | "null" | "~"
        )
    {
        return true;
    }
    !matches!(
        serde_yaml::from_str::<serde_yaml::Value>(value),
        Ok(serde_yaml::Value::String(parsed)) if parsed == value
    )
}

fn format_yaml_scalar(value: &str, style: QuoteStyle) -> String {
    match style {
        QuoteStyle::Double => format!("\"{value}\""),
        QuoteStyle::Single => format!("'{value}'"),
        QuoteStyle::Plain => {
            if needs_yaml_quotes(value) {
                format!("\"{value}\"")
            } else {
                value.to_owned()
            }
        }
    }
}

fn has_inline_comment(after_colon: &str) -> bool {
    let bytes = after_colon.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            if q == b'"' && b == b'\\' {
                i += 2;
                continue;
            }
            if q == b'\'' && b == b'\'' && bytes.get(i + 1) == Some(&b'\'') {
                i += 2;
                continue;
            }
            if b == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' if after_colon[..i].bytes().all(|c| matches!(c, b' ' | b'\t')) => {
                quote = Some(b);
            }
            b'#' if i == 0 || matches!(bytes[i - 1], b' ' | b'\t') => {
                return true;
            }
            _ => {}
        }
        i += 1;
    }
    false
}

fn plan_update_line(
    source: &str,
    entry: &TopEntry,
    field: &str,
    value: &str,
) -> Result<(String, Range<usize>, usize, usize), String> {
    if entry.has_continuation_lines || entry.is_block_scalar_header {
        return Err(format!(
            "Multi-line or block scalar value for '{field}' is not supported"
        ));
    }
    let after_colon_start = entry.colon_abs + 1;
    let after_colon = &source[after_colon_start..entry.line_end_no_eol];
    if has_inline_comment(after_colon) {
        return Err(format!(
            "Cannot edit '{field}' because that line has a comment"
        ));
    }
    let lead = after_colon.len() - after_colon.trim_start_matches([' ', '\t']).len();
    let trimmed = after_colon[lead..].trim_end_matches([' ', '\t']);
    let val_abs_start = after_colon_start + lead;
    let val_abs_end = val_abs_start + trimmed.len();

    let (replace_span, replacement) = if trimmed.is_empty() {
        let formatted = format_yaml_scalar(value, QuoteStyle::Plain);
        if after_colon.is_empty() {
            (
                after_colon_start..after_colon_start,
                format!(" {formatted}"),
            )
        } else {
            // Preserve existing whitespace after colon if it starts with a space/tab.
            (entry.line_end_no_eol..entry.line_end_no_eol, formatted)
        }
    } else {
        let first = trimmed.as_bytes()[0];
        if matches!(first, b'{' | b'[') {
            return Err(format!("Flow-style value for '{field}' is not supported"));
        }
        if matches!(first, b'|' | b'>') {
            return Err(format!("Block scalar value for '{field}' is not supported"));
        }
        if matches!(first, b'&' | b'*' | b'!') {
            return Err(format!(
                "YAML anchors, aliases, and tags for '{field}' are not supported"
            ));
        }
        let style = if first == b'"' || first == b'\'' {
            let close = find_closing_quote(trimmed, 0, first)
                .ok_or_else(|| format!("Unterminated quoted value for '{field}'"))?;
            if close + 1 != trimmed.len() {
                return Err(format!("Unsupported trailing syntax on '{field}' line"));
            }
            if first == b'"' {
                QuoteStyle::Double
            } else {
                QuoteStyle::Single
            }
        } else {
            QuoteStyle::Plain
        };
        let formatted = format_yaml_scalar(value, style);
        (val_abs_start..val_abs_end, formatted)
    };

    let mut after = source.to_owned();
    let replacement_len = replacement.len();
    after.replace_range(replace_span.clone(), &replacement);
    Ok((after, replace_span, replacement_len, 0))
}

fn plan_append_line(
    source: &str,
    envelope: &FrontmatterEnvelope<'_>,
    field: &str,
    value: &str,
) -> (String, Range<usize>, usize, usize) {
    let formatted = format_yaml_scalar(value, QuoteStyle::Plain);
    let inserted = format!("{field}: {formatted}{}", envelope.eol);
    let mut after = source.to_owned();
    let insert_at = envelope.fm_end;
    after.insert_str(insert_at, &inserted);
    (after, insert_at..insert_at, inserted.len(), 1)
}

#[allow(clippy::too_many_arguments)]
fn verify_postcondition(
    path: &str,
    schema: &Arc<FieldSchema>,
    field: &str,
    expected_value: &Value,
    before: &str,
    after: &str,
    before_envelope: &FrontmatterEnvelope<'_>,
    before_props: &[crate::properties::Property],
    before_record: &NoteRecord,
    edit_span: Range<usize>,
    replacement_len: usize,
    added_lines: usize,
) -> Result<(), String> {
    // 1. Byte-level preservation invariant outside the single edited span.
    let expected_len = before.len() - (edit_span.end - edit_span.start) + replacement_len;
    let bytes_preserved = after.len() == expected_len
        && after.as_bytes()[..edit_span.start] == before.as_bytes()[..edit_span.start]
        && after.as_bytes()[edit_span.start + replacement_len..]
            == before.as_bytes()[edit_span.end..];
    if !bytes_preserved || has_lone_cr(after) {
        return Err("Frontmatter edit violated byte-preservation invariant".into());
    }

    // 2. Frontmatter envelope and body byte preservation.
    let after_envelope = parse_frontmatter_envelope(after)?;
    if after_envelope.body.as_bytes() != before_envelope.body.as_bytes()
        || after_envelope.eol != before_envelope.eol
    {
        return Err("Frontmatter edit would alter note body or line endings".into());
    }

    // 3. Raw frontmatter properties invariant: every other key/value is untouched.
    let after_props = crate::properties::parse_for_records(after_envelope.yaml)
        .map_err(|err| format!("Edited frontmatter failed YAML parse: {err}"))?;
    let target_prop_count = after_props.iter().filter(|p| p.key == field).count();
    if target_prop_count != 1 {
        return Err("Edited frontmatter does not contain a unique target field".into());
    }
    let before_other: Vec<_> = before_props.iter().filter(|p| p.key != field).collect();
    let after_other: Vec<_> = after_props.iter().filter(|p| p.key != field).collect();
    if before_other != after_other {
        return Err("Frontmatter edit would alter other frontmatter properties".into());
    }

    // 4. Main read-projection invariant: target equals expected_value, everything else unchanged.
    let after_projection = Projection::build(
        Arc::clone(schema),
        [Source {
            path,
            bytes: after.as_bytes(),
            mtime: None,
        }],
    );
    let after_record = after_projection
        .note(path)
        .ok_or_else(|| "Edited note disappeared from projection".to_owned())?;

    if after_record.fields.get(field) != Some(expected_value) {
        return Err(format!(
            "Edited note did not project '{field}' to the requested value"
        ));
    }

    fn other_fields<'a>(record: &'a NoteRecord, field: &str) -> BTreeMap<&'a str, &'a Value> {
        record
            .fields
            .iter()
            .filter(|(k, _)| k.as_str() != field && k.as_str() != "rev")
            .map(|(k, v)| (k.as_str(), v))
            .collect()
    }
    if other_fields(before_record, field) != other_fields(after_record, field) {
        return Err("Frontmatter edit would alter another projected field".into());
    }

    // Tasks and sections stay identical (with line shifted by `added_lines` on insert).
    if before_record.tasks.len() != after_record.tasks.len()
        || before_record.sections.len() != after_record.sections.len()
    {
        return Err("Frontmatter edit would alter note tasks or sections".into());
    }
    for (b, a) in before_record.tasks.iter().zip(after_record.tasks.iter()) {
        if a.line != b.line + added_lines {
            return Err("Frontmatter edit shifted task lines unexpectedly".into());
        }
        let mut expected_fields = b.fields.clone();
        expected_fields.insert("line".into(), Value::Number(a.line.into()));
        if a.fields != expected_fields {
            return Err("Frontmatter edit would alter projected task fields".into());
        }
    }
    for (b, a) in before_record
        .sections
        .iter()
        .zip(after_record.sections.iter())
    {
        if a.line != b.line + added_lines || a.anchor != b.anchor {
            return Err("Frontmatter edit shifted section headings unexpectedly".into());
        }
        let mut expected_fields = b.fields.clone();
        expected_fields.insert("line".into(), Value::Number(a.line.into()));
        if a.fields != expected_fields {
            return Err("Frontmatter edit would alter projected section fields".into());
        }
    }

    // No finding on the edited field or frontmatter, and no change to findings on other fields.
    if after_record
        .findings
        .iter()
        .any(|f| f.field == field || f.field == "frontmatter")
    {
        return Err(format!(
            "Frontmatter edit introduced a finding on '{field}'"
        ));
    }
    fn shifted_other_findings<'a>(
        record: &'a NoteRecord,
        field: &str,
        delta: usize,
    ) -> Vec<(&'static str, &'a str, Option<usize>, &'a str)> {
        record
            .findings
            .iter()
            .filter(|f| f.field != field && f.field != "frontmatter")
            .map(|f| {
                (
                    f.code,
                    f.field.as_str(),
                    f.line.map(|l| l + delta),
                    f.message.as_str(),
                )
            })
            .collect()
    }
    if shifted_other_findings(before_record, field, added_lines)
        != shifted_other_findings(after_record, field, 0)
    {
        return Err("Frontmatter edit would alter findings on other fields".into());
    }

    Ok(())
}

/// Safe local frontmatter writes through FileEditor (lock, dirty-recovery check,
/// atomic save, history preimage, write authority, and exact-byte Undo).
#[cfg(any(unix, windows))]
pub mod write {
    use super::{Plan, Target};
    use crate::file_editor::{FileEditor, Save};
    use crate::records::FieldSchema;
    use anyhow::{bail, ensure, Context, Result};
    use std::path::{Component, Path, PathBuf};
    use std::sync::Arc;

    /// An in-process Undo capability issued only after a successful save.
    #[derive(Clone, Debug)]
    pub struct Receipt {
        root: PathBuf,
        plan: Plan,
    }

    impl Receipt {
        pub fn path(&self) -> &str {
            &self.plan.path
        }

        pub fn saved_revision(&self) -> &str {
            &self.plan.after_revision
        }

        pub fn undo(&self, root: &Path, drafts: &Path) -> Result<()> {
            ensure!(
                root.canonicalize()? == self.root,
                "Undo belongs to a different vault"
            );
            let path = bound_path(&self.root, &self.plan.path)?;
            let mut editor = open_clean(&path, drafts)?;
            let before = self
                .plan
                .undo_source(editor.text())
                .map_err(anyhow::Error::msg)?;
            let _admission = okilum_write_authority::operation(&self.root)?;
            editor.set_text(before.to_owned())?;
            save(&mut editor)
        }
    }

    /// Apply a frontmatter `set` mutation through FileEditor.
    /// Returns `Ok(None)` when the field already has the exact byte representation (no-op).
    pub fn apply(
        root: &Path,
        drafts: &Path,
        schema: &Arc<FieldSchema>,
        target: &Target,
        field: &str,
        value: &str,
    ) -> Result<Option<Receipt>> {
        let root = root.canonicalize()?;
        okilum_write_authority::Authority::for_root(&root)?;
        let path = bound_path(&root, target.path())?;
        let mut editor = open_clean(&path, drafts)?;
        let plan = target
            .plan(editor.text(), schema, field, value)
            .map_err(anyhow::Error::msg)?;
        commit_plan(root, &mut editor, plan)
    }

    fn commit_plan(root: PathBuf, editor: &mut FileEditor, plan: Plan) -> Result<Option<Receipt>> {
        if plan.before == plan.after {
            return Ok(None);
        }
        let _admission = okilum_write_authority::operation(&root)?;
        editor.set_text(plan.after.clone())?;
        save(editor)?;
        Ok(Some(Receipt { root, plan }))
    }

    fn open_clean(path: &Path, drafts: &Path) -> Result<FileEditor> {
        let editor = FileEditor::open(path, drafts)?;
        ensure!(
            editor.path() == path,
            "The note location changed while opening it"
        );
        ensure!(
            !editor.dirty(),
            "This note has unsaved edits. Open its source before changing properties."
        );
        Ok(editor)
    }

    fn save(editor: &mut FileEditor) -> Result<()> {
        match editor.save()? {
            Save::Saved => Ok(()),
            Save::Conflict => {
                bail!(
                    "This note changed. The property change was not saved; its draft is retained."
                )
            }
        }
    }

    fn bound_path(root: &Path, relative: &str) -> Result<PathBuf> {
        okilum_write_authority::Authority::for_root(root)?;
        let rel = Path::new(relative);
        ensure!(
            !relative.is_empty() && rel.components().all(|c| matches!(c, Component::Normal(_))),
            "Invalid note path"
        );
        let mut path = root.to_owned();
        for component in rel.components() {
            path.push(component);
            let metadata = std::fs::symlink_metadata(&path).context("The note is unavailable")?;
            ensure!(
                !metadata.file_type().is_symlink(),
                "Notes cannot traverse symlinks"
            );
        }
        ensure!(path.canonicalize()? == path, "The note location changed");
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_editor::FileEditor;
    use std::path::Path;

    const FIXTURE_VIEWS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/views");

    fn schema() -> Arc<FieldSchema> {
        Arc::new(FieldSchema::load(Path::new(FIXTURE_VIEWS)).unwrap())
    }

    #[test]
    fn preserves_bom_crlf_comments_unrelated_flow_and_body_bytes() {
        let schema = schema();
        let source = concat!(
            "\u{feff}---\r\n",
            "# Header comment &not-anchor *not-alias !not-tag\r\n",
            "type: project\r\n",
            "kind: project\r\n",
            "area: Dev\r\n",
            "status: active\r\n",
            "title: Okilum\r\n",
            "source: https://example.com/page?a=1&b=2\r\n",
            "tags: [dev, don't-panic]\r\n",
            "related_to: [\"[[2026-11-24-views]]\"]\r\n",
            "notes: |\r\n",
            "  &literal-inside-block\r\n",
            "  status: not-a-key\r\n",
            "# Footer comment\r\n",
            "---\r\n",
            "# Okilum\r\n\r\nBody with `status: active` untouched.\r\n",
        );
        let target = Target::capture("state/dev/okilum/index.md", source).unwrap();
        let plan = target.plan(source, &schema, "status", "parked").unwrap();
        let expected = source.replacen("status: active\r\n", "status: parked\r\n", 1);
        assert_eq!(plan.after.as_bytes(), expected.as_bytes());
        assert_eq!(
            plan.undo_source(&plan.after).unwrap().as_bytes(),
            source.as_bytes()
        );
    }

    #[test]
    fn preserves_quoted_values_and_spacing_on_update() {
        let schema = schema();
        let double_quoted = "---\ntype: project\nkind: project\narea: Dev\nstatus:   \"active\"\nowner: 'oleg'\ndue: \"2026-12-01\"\n---\n# Body\n";
        let target = Target::capture("state/dev/okilum/index.md", double_quoted).unwrap();
        let s_plan = target
            .plan(double_quoted, &schema, "status", "done")
            .unwrap();
        assert_eq!(
            s_plan.after,
            double_quoted.replacen("status:   \"active\"", "status:   \"done\"", 1)
        );
        let o_plan = target
            .plan(double_quoted, &schema, "owner", "okilum-assistant")
            .unwrap();
        assert_eq!(
            o_plan.after,
            double_quoted.replacen("owner: 'oleg'", "owner: 'okilum-assistant'", 1)
        );
        let d_plan = target
            .plan(double_quoted, &schema, "due", "2026-12-31")
            .unwrap();
        assert_eq!(
            d_plan.after,
            double_quoted.replacen("due: \"2026-12-01\"", "due: \"2026-12-31\"", 1)
        );
    }

    #[test]
    fn appends_absent_key_at_end_of_mapping_in_lf_and_crlf() {
        let schema = schema();
        let lf = "---\ntype: project\nkind: project\narea: Dev\nstatus: active\n# keep\n---\n# Okilum\n- [ ] Task\n";
        let target = Target::capture("state/dev/okilum/index.md", lf).unwrap();
        let plan = target.plan(lf, &schema, "due", "2026-12-20").unwrap();
        assert_eq!(
            plan.after,
            "---\ntype: project\nkind: project\narea: Dev\nstatus: active\n# keep\ndue: 2026-12-20\n---\n# Okilum\n- [ ] Task\n"
        );

        let crlf = "\u{feff}---\r\ntype: project\r\nkind: project\r\narea: Dev\r\nstatus: active\r\n...\r\n# Okilum\r\n";
        let target_crlf = Target::capture("state/dev/okilum/index.md", crlf).unwrap();
        let plan_crlf = target_crlf.plan(crlf, &schema, "owner", "oleg").unwrap();
        assert_eq!(
            plan_crlf.after,
            "\u{feff}---\r\ntype: project\r\nkind: project\r\narea: Dev\r\nstatus: active\r\nowner: oleg\r\n...\r\n# Okilum\r\n"
        );
    }

    #[test]
    fn quotes_ambiguous_yaml_owner_slugs_so_read_projection_round_trips() {
        let schema = schema();
        let source = "---\ntype: project\nkind: project\narea: Dev\nstatus: active\nowner: oleg\n---\n# Okilum\n";
        let target = Target::capture("state/dev/okilum/index.md", source).unwrap();
        for slug in ["true", "false", "null", "no", "yes", "123", "-foo", "---"] {
            let plan = target.plan(source, &schema, "owner", slug).unwrap();
            assert_eq!(
                plan.after,
                source.replacen("owner: oleg", &format!("owner: \"{slug}\""), 1),
                "slug {slug}"
            );
        }
    }

    #[test]
    fn refuses_every_unsupported_frontmatter_and_leaves_file_untouched() {
        let schema = schema();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let drafts = temp.path().join("state/editor-drafts");
        std::fs::create_dir_all(root.join("state/dev/okilum")).unwrap();
        std::fs::create_dir_all(root.join("log/dev/okilum/decisions")).unwrap();
        std::fs::create_dir_all(root.join("knowledge")).unwrap();

        let refusal_cases: &[(&str, &str, &str, &str)] = &[
            // No frontmatter
            (
                "no-frontmatter",
                "state/dev/okilum/a.md",
                "# Plain note\nNo frontmatter.\n",
                "status",
            ),
            // Unterminated frontmatter
            (
                "unterminated",
                "state/dev/okilum/b.md",
                "---\nstatus: active\n# never closed\n",
                "status",
            ),
            // Closing delimiter without trailing newline (not recognized by split_frontmatter)
            (
                "closing-no-nl",
                "state/dev/okilum/c.md",
                "---\nstatus: active\n---",
                "status",
            ),
            // Flow style root mapping
            (
                "flow-root",
                "state/dev/okilum/d.md",
                "---\n{type: project, status: active}\n---\n# Body\n",
                "status",
            ),
            // Flow style value for target key
            (
                "flow-value-map",
                "state/dev/okilum/e.md",
                "---\ntype: project\nstatus: {a: active}\n---\n# Body\n",
                "status",
            ),
            (
                "flow-value-seq",
                "state/dev/okilum/f.md",
                "---\ntype: project\nstatus: [active]\n---\n# Body\n",
                "status",
            ),
            // Multi-line / block scalars for target key
            (
                "block-literal",
                "state/dev/okilum/g.md",
                "---\ntype: project\nstatus: |\n  active\n---\n# Body\n",
                "status",
            ),
            (
                "block-folded",
                "state/dev/okilum/h.md",
                "---\ntype: project\nstatus: >-\n  active\n---\n# Body\n",
                "status",
            ),
            (
                "multiline-indented",
                "state/dev/okilum/i.md",
                "---\ntype: project\nstatus:\n  active\n---\n# Body\n",
                "status",
            ),
            (
                "multiline-quoted",
                "state/dev/okilum/j.md",
                "---\ntype: project\nowner: \"ol\n  eg\"\n---\n# Body\n",
                "owner",
            ),
            // Anchors, aliases, tags, merge keys
            (
                "anchor-target",
                "state/dev/okilum/k.md",
                "---\ntype: project\nstatus: &s active\n---\n# Body\n",
                "status",
            ),
            (
                "alias-target",
                "state/dev/okilum/l.md",
                "---\ndefaults: &s active\nstatus: *s\n---\n# Body\n",
                "status",
            ),
            (
                "anchor-other",
                "state/dev/okilum/m.md",
                "---\ntags: [&t dev]\nstatus: active\n---\n# Body\n",
                "status",
            ),
            (
                "anchor-nested-seq",
                "state/dev/okilum/m2.md",
                "---\nmatrix:\n  - - &a dev\nstatus: active\n---\n# Body\n",
                "status",
            ),
            (
                "anchor-json-flow",
                "state/dev/okilum/m3.md",
                "---\nmeta: {\"a\":&x 1}\nstatus: active\n---\n# Body\n",
                "status",
            ),
            (
                "multiline-flow-anchor-alias",
                "state/dev/okilum/m4.md",
                "---\nmeta: [\n  &a foo,\n  *a\n]\nstatus: active\n---\n# Body\n",
                "status",
            ),
            (
                "multiline-flow-anchor-only",
                "state/dev/okilum/m5.md",
                "---\nmeta: [\n  &a foo\n]\nstatus: active\n---\n# Body\n",
                "status",
            ),
            (
                "multiline-flow-tag",
                "state/dev/okilum/m6.md",
                "---\nmeta: [\n  !!str foo\n]\nstatus: active\n---\n# Body\n",
                "status",
            ),
            (
                "multiline-flow-map-anchor",
                "state/dev/okilum/m7.md",
                "---\nmeta: {\n  a:\n    &x 1\n}\nstatus: active\n---\n# Body\n",
                "status",
            ),
            (
                "multiline-flow-map-value-anchor",
                "state/dev/okilum/m8.md",
                "---\nmeta: {a:\n  &x 1}\nstatus: active\n---\n# Body\n",
                "status",
            ),
            (
                "multiline-flow-plain",
                "state/dev/okilum/m9.md",
                "---\nmeta: [\n  foo,\n  bar\n]\nstatus: active\n---\n# Body\n",
                "status",
            ),
            (
                "indented-root-mapping",
                "state/dev/okilum/m10.md",
                "---\n  type: project\n  status: active\n---\n# Body\n",
                "status",
            ),
            (
                "tag-target",
                "state/dev/okilum/n.md",
                "---\ntype: project\nstatus: !!str active\n---\n# Body\n",
                "status",
            ),
            (
                "tag-other",
                "state/dev/okilum/o.md",
                "---\ntype: !custom project\nstatus: active\n---\n# Body\n",
                "status",
            ),
            // Duplicate keys (same spelling & quoted variant & unrelated duplicate)
            (
                "dup-target",
                "state/dev/okilum/p.md",
                "---\nstatus: active\nstatus: done\n---\n# Body\n",
                "status",
            ),
            (
                "dup-quoted",
                "state/dev/okilum/q.md",
                "---\nstatus: active\n\"status\": done\n---\n# Body\n",
                "status",
            ),
            (
                "dup-other",
                "state/dev/okilum/r.md",
                "---\ntype: project\ntype: note\nstatus: active\n---\n# Body\n",
                "status",
            ),
            // Inline comment on target line
            (
                "comment-target",
                "state/dev/okilum/s.md",
                "---\ntype: project\nstatus: active # keep me\n---\n# Body\n",
                "status",
            ),
            (
                "comment-quoted",
                "state/dev/okilum/t.md",
                "---\ntype: project\nstatus: \"active\" # keep me\n---\n# Body\n",
                "status",
            ),
            (
                "comment-empty",
                "state/dev/okilum/u.md",
                "---\ntype: project\ndue: # todo\n---\n# Body\n",
                "due",
            ),
            // Key inside nested mapping
            (
                "nested-block",
                "state/dev/okilum/v.md",
                "---\ntype: project\nmeta:\n  status: active\n---\n# Body\n",
                "status",
            ),
            (
                "nested-flow",
                "state/dev/okilum/w.md",
                "---\ntype: project\nmeta: {status: active}\n---\n# Body\n",
                "status",
            ),
            (
                "nested-seq",
                "state/dev/okilum/x.md",
                "---\ntype: project\nitems:\n  - status: active\n---\n# Body\n",
                "status",
            ),
            // Mixed CRLF/LF and inner BOM and lone CR
            (
                "mixed-eol-open",
                "state/dev/okilum/y.md",
                "---\r\ntype: project\nstatus: active\r\n---\r\n# Body\r\n",
                "status",
            ),
            (
                "mixed-eol-close",
                "state/dev/okilum/z.md",
                "---\ntype: project\nstatus: active\n---\r\n# Body\n",
                "status",
            ),
            (
                "inner-bom",
                "state/dev/okilum/aa.md",
                "---\n\u{feff}status: active\n---\n# Body\n",
                "status",
            ),
            (
                "lone-cr-fm",
                "state/dev/okilum/ab.md",
                "---\nstatus: active\r---\n# Body\n",
                "status",
            ),
            (
                "lone-cr-body",
                "state/dev/okilum/ac.md",
                "---\nstatus: active\n---\nBody\rline\n",
                "status",
            ),
            // Out-of-scope note layers/types for status
            (
                "decision-note",
                "log/dev/okilum/decisions/2026-11-24-x.md",
                "---\ntype: decision\ndecision_status: proposed\n---\n# Decision\n",
                "status",
            ),
            (
                "knowledge-note",
                "knowledge/ref.md",
                "---\ntype: note\narea: Dev\n---\n# Knowledge\n",
                "status",
            ),
        ];

        for &(label, rel, content, field) in refusal_cases {
            let file = root.join(rel);
            std::fs::write(&file, content).unwrap();
            let target = Target::capture(rel, content).unwrap();
            let value = match field {
                "status" => "parked",
                "owner" => "oleg",
                "due" => "2026-12-15",
                _ => unreachable!(),
            };
            let err = write::apply(&root, &drafts, &schema, &target, field, value)
                .expect_err(&format!("expected refusal for case '{label}'"));
            if label == "indented-root-mapping" {
                assert_eq!(
                    err.to_string(),
                    "Frontmatter keys are indented; edit the note directly"
                );
            }
            assert_eq!(
                std::fs::read(&file).unwrap(),
                content.as_bytes(),
                "file must be untouched for refused case '{label}'"
            );
        }
    }

    #[test]
    fn refuses_disallowed_fields_and_invalid_values() {
        let schema = schema();
        let source =
            "---\ntype: project\nkind: project\narea: Dev\nstatus: active\n---\n# Okilum\n";
        let target = Target::capture("state/dev/okilum/index.md", source).unwrap();

        for bad_field in [
            "area",
            "title",
            "type",
            "tags",
            "decision_status",
            "unknown",
        ] {
            assert!(
                target.plan(source, &schema, bad_field, "active").is_err(),
                "field {bad_field} must be refused"
            );
        }
        for (field, bad_val) in [
            ("status", ""),
            ("status", "invalid"),
            ("status", "Active"),
            ("status", "blocked"), // legacy alias, not canonical
            ("status", "parked\narea: Health"),
            ("status", " parked "),
            ("due", ""),
            ("due", "2026-02-30"),
            ("due", "2026-13-01"),
            ("due", "2026-11-24T10:00"),
            ("due", "tomorrow"),
            ("owner", ""),
            ("owner", "Oleg"),
            ("owner", "oleg smith"),
            ("owner", "oleg_1"),
            ("owner", "oleg\nstatus: done"),
        ] {
            assert!(
                target.plan(source, &schema, field, bad_val).is_err(),
                "{field}={bad_val:?} must be refused"
            );
        }
    }

    #[test]
    fn write_apply_refuses_stale_revision_dirty_draft_and_undoes_exact_bytes() {
        let schema = schema();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let drafts = temp.path().join("state/editor-drafts");
        std::fs::create_dir_all(root.join("state/dev/okilum")).unwrap();

        let original = "\u{feff}---\r\ntype: project\r\nkind: project\r\narea: Dev\r\nstatus: active\r\n# keep\r\n---\r\n# Okilum\r\n";
        let rel = "state/dev/okilum/index.md";
        let file = root.join(rel);
        std::fs::write(&file, original).unwrap();
        let target = Target::capture(rel, original).unwrap();

        // No-op returns Ok(None) and writes nothing.
        assert!(
            write::apply(&root, &drafts, &schema, &target, "status", "active")
                .unwrap()
                .is_none()
        );

        // Real edit writes exact bytes and returns Receipt.
        let receipt = write::apply(&root, &drafts, &schema, &target, "status", "parked")
            .unwrap()
            .unwrap();
        let expected = original.replacen("status: active", "status: parked", 1);
        assert_eq!(std::fs::read(&file).unwrap(), expected.as_bytes());

        // Stale target refuses with clear message and leaves file untouched.
        let err = write::apply(&root, &drafts, &schema, &target, "status", "done").unwrap_err();
        assert!(
            err.to_string().contains("The note changed, refresh"),
            "{err}"
        );
        assert_eq!(std::fs::read(&file).unwrap(), expected.as_bytes());

        // Active editor lock refuses Undo.
        let mut editor = FileEditor::open(&file, &drafts).unwrap();
        assert!(receipt.undo(&root, &drafts).is_err());
        let dirty_text = format!("{expected}Unsaved\r\n");
        editor.set_text(dirty_text.clone()).unwrap();
        drop(editor);
        // Dirty retained draft also refuses Undo.
        assert!(receipt.undo(&root, &drafts).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), expected.as_bytes());
        let mut recovered = FileEditor::open(&file, &drafts).unwrap();
        recovered.reload().unwrap();
        drop(recovered);

        // Undo restores the exact preimage bytes (including BOM and CRLF).
        receipt.undo(&root, &drafts).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), original.as_bytes());

        // Second undo refuses because revision changed back.
        assert!(receipt.undo(&root, &drafts).is_err());
    }

    #[test]
    fn refuses_opaque_writable_field_and_preserves_opaque_frontmatter_lines() {
        let original_owner = concat!(
            "  owner:\n",
            "    type: string\n",
            "    format: slug\n",
            "    origin: stored\n",
            "    rule: identifies the person or assistant expected to act\n",
            "    writeback: none\n",
            "    writers: [people, agents]\n",
            "    required: {}\n",
            "    index: {indexed: true, facet: true, sort: true}\n",
            "    writable_by_actions: true",
        );

        let schema_with_waits = Arc::new(
            FieldSchema::from_documents(|name| {
                let mut text =
                    std::fs::read_to_string(Path::new(FIXTURE_VIEWS).join("@Schema").join(name))?;
                if name == "fields.yaml" {
                    text = text.replace(
                        "\nbuiltin_defaults:\n",
                        "\n  waits:\n    type: list<object>\n    index: {indexed: false}\n    writable_by_actions: true\n\nbuiltin_defaults:\n",
                    );
                }
                Ok(text)
            })
            .unwrap(),
        );

        let source = concat!(
            "---\n",
            "type: project\n",
            "kind: project\n",
            "area: Dev\n",
            "status: active\n",
            "waits:\n",
            "  - on: \"vendor\"\n",
            "    since: 2026-11-27\n",
            "    next_check: 2026-11-27\n",
            "---\n",
            "# Okilum\n",
        );
        let target = Target::capture("state/dev/okilum/index.md", source).unwrap();
        let plan = target
            .plan(source, &schema_with_waits, "status", "parked")
            .unwrap();
        assert_eq!(
            plan.after,
            source.replacen("status: active\n", "status: parked\n", 1)
        );

        for owner_def in [
            "  owner:\n    type: custom_blob\n    index: {indexed: false}\n    writable_by_actions: true",
            "  owner:\n    type: object\n    writable_by_actions: true",
        ] {
            let opaque_schema = Arc::new(
                FieldSchema::from_documents(|name| {
                    let mut text = std::fs::read_to_string(
                        Path::new(FIXTURE_VIEWS).join("@Schema").join(name),
                    )?;
                    if name == "fields.yaml" {
                        assert!(text.contains(original_owner));
                        text = text.replace(original_owner, owner_def);
                    }
                    Ok(text)
                })
                .unwrap(),
            );
            let err = target
                .plan(source, &opaque_schema, "owner", "alice")
                .unwrap_err();
            assert!(
                err.contains("not a writable scalar field"),
                "unexpected error: {err}"
            );
        }
    }
}
