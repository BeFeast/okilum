//! Bounded edits to a goal's criteria subtree. No storage or runtime policy.
use crate::source::{SourceSnapshot, SourceWrite};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, ops::Range};
use uuid::Uuid;

pub const MAX_CRITERIA: usize = 64;
pub const MAX_DESCRIPTION_BYTES: usize = 8 * 1024;
pub const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;
const SCHEMA: &str = "ai-brain/v1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CriterionEdit {
    pub id: String,
    pub description: String,
    pub requires_human: bool,
}

struct Document {
    text: String,
    metadata: Mapping,
    criteria: Vec<Mapping>,
    region: Range<usize>,
    newline: &'static str,
}

fn unsupported(reason: &str) -> String {
    format!("Cannot edit outcome criteria: {reason}. Use Source to inspect this goal.")
}

fn key(name: &str) -> Value {
    Value::String(name.into())
}

fn canonical_uuid(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}

fn revision(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn decode(encoded: &str) -> Result<Vec<u8>, String> {
    if encoded.len() > MAX_SOURCE_BYTES.div_ceil(3) * 4 {
        return Err(unsupported("complete source exceeds 8 MiB"));
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| unsupported("source bytes are not valid base64"))?;
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(unsupported("complete source exceeds 8 MiB"));
    }
    Ok(bytes)
}

fn edits(mappings: &[Mapping]) -> Result<Vec<CriterionEdit>, String> {
    if mappings.len() > MAX_CRITERIA {
        return Err(unsupported("this form supports at most 64 criteria"));
    }
    let mut ids = BTreeSet::new();
    mappings
        .iter()
        .map(|mapping| {
            let id = mapping
                .get(key("id"))
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .ok_or_else(|| unsupported("a criterion has no string ID"))?;
            if !ids.insert(id) {
                return Err(unsupported("criterion IDs are duplicated"));
            }
            let description = mapping
                .get(key("description"))
                .and_then(Value::as_str)
                .ok_or_else(|| unsupported("a criterion description is not text"))?;
            validate_description(description)?;
            let requires_human = mapping
                .get(key("requires_human"))
                .and_then(Value::as_bool)
                .ok_or_else(|| unsupported("a criterion confirmation kind is not a boolean"))?;
            Ok(CriterionEdit {
                id: id.into(),
                description: description.into(),
                requires_human,
            })
        })
        .collect()
}

fn validate_description(description: &str) -> Result<(), String> {
    if description.trim().is_empty() || description.len() > MAX_DESCRIPTION_BYTES {
        return Err(unsupported(
            "each description must be nonempty and at most 8 KiB",
        ));
    }
    Ok(())
}

fn parse(snapshot: &SourceSnapshot, goal_id: &str) -> Result<Document, String> {
    if snapshot.schema != SCHEMA
        || snapshot.media_type != "text/markdown"
        || !canonical_uuid(&snapshot.brain_id)
        || !canonical_uuid(goal_id)
    {
        return Err(unsupported(
            "source schema or goal/workspace identity is invalid",
        ));
    }
    let bytes = decode(&snapshot.content_base64)?;
    if revision(&bytes) != snapshot.revision {
        return Err(unsupported(
            "source revision does not match its exact bytes",
        ));
    }
    let text = String::from_utf8(bytes).map_err(|_| unsupported("source is not UTF-8"))?;
    let (front_start, newline) = if text.starts_with("---\r\n") {
        (5, "\r\n")
    } else if text.starts_with("---\n") {
        (4, "\n")
    } else {
        return Err(unsupported("source needs ordinary YAML frontmatter"));
    };
    let mut offset = front_start;
    let mut front_end = None;
    for line in text[front_start..].split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            front_end = Some(offset);
            break;
        }
        offset += line.len();
    }
    let front_end = front_end.ok_or_else(|| unsupported("frontmatter is unterminated"))?;
    let front = &text[front_start..front_end];
    let metadata: Mapping = serde_yaml::from_str(front)
        .map_err(|_| unsupported("frontmatter is invalid or has duplicate keys"))?;
    for (name, expected) in [
        ("schema", SCHEMA),
        ("brain_id", snapshot.brain_id.as_str()),
        ("record_type", "goal"),
        ("id", goal_id),
    ] {
        if metadata.get(key(name)).and_then(Value::as_str) != Some(expected) {
            return Err(unsupported(
                "canonical goal identity does not match the source",
            ));
        }
    }

    // Only plain root block keys are supported. Indentless sequence items are
    // emitted by serde_yaml for normal Okilum records and are not root keys.
    let mut start = None;
    let mut end = None;
    let mut last_content_end = None;
    let mut in_criteria = false;
    offset = front_start;
    for line in front.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        let trimmed = content.trim_start();
        let ignored = trimmed.is_empty() || trimmed.starts_with('#');
        let unindented = content == trimmed;
        if !ignored && unindented && !content.starts_with("- ") && content != "-" {
            let (name, rest) = content
                .split_once(':')
                .ok_or_else(|| unsupported("root YAML layout is not a plain block mapping"))?;
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                || (!rest.is_empty() && !rest.starts_with([' ', '\t']))
            {
                return Err(unsupported(
                    "root YAML keys require a supported plain block layout",
                ));
            }
            if in_criteria {
                end = last_content_end;
                in_criteria = false;
            }
            if name == "criteria" {
                if start.is_some() {
                    return Err(unsupported("criteria appears more than once"));
                }
                if !rest.trim().is_empty() && !rest.trim_start().starts_with('#') {
                    return Err(unsupported("criteria must be an ordinary block sequence"));
                }
                start = Some(offset);
                in_criteria = true;
            }
        }
        if in_criteria && !ignored {
            last_content_end = Some(offset + line.len());
        }
        offset += line.len();
    }
    if in_criteria {
        end = last_content_end;
    }
    let region = start.ok_or_else(|| unsupported("plain criteria block was not found"))?
        ..end.ok_or_else(|| unsupported("criteria block has no supported extent"))?;
    let criteria = metadata
        .get(key("criteria"))
        .and_then(Value::as_sequence)
        .ok_or_else(|| unsupported("criteria is not a sequence"))?
        .iter()
        .map(|entry| {
            entry
                .as_mapping()
                .cloned()
                .ok_or_else(|| unsupported("each criterion must be a mapping"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    edits(&criteria)?;
    Ok(Document {
        text,
        metadata,
        criteria,
        region,
        newline,
    })
}

/// Read the editable fields without discarding unknown criterion mapping values.
pub fn inspect(base: &SourceSnapshot, goal_id: &str) -> Result<Vec<CriterionEdit>, String> {
    edits(&parse(base, goal_id)?.criteria)
}

/// Preserve all source bytes outside criteria; normalize only its YAML subtree.
pub fn transform(
    base: &SourceSnapshot,
    goal_id: &str,
    criteria: &[CriterionEdit],
) -> Result<String, String> {
    let original = parse(base, goal_id)?;
    let old = edits(&original.criteria)?;
    if criteria.is_empty() || criteria.len() < old.len() || criteria.len() > MAX_CRITERIA {
        return Err(unsupported(
            "saved rows cannot be removed; use 1 to 64 criteria",
        ));
    }
    let mut ids = BTreeSet::new();
    let mut mappings = Vec::with_capacity(criteria.len());
    for (index, criterion) in criteria.iter().enumerate() {
        validate_description(&criterion.description)?;
        if !ids.insert(&criterion.id) {
            return Err(unsupported("criterion IDs must remain unique"));
        }
        let mut mapping = if let Some(saved) = old.get(index) {
            if criterion.id != saved.id || criterion.requires_human != saved.requires_human {
                return Err(unsupported(
                    "saved criterion IDs, order and confirmation kinds are fixed",
                ));
            }
            original.criteria[index].clone()
        } else {
            if !canonical_uuid(&criterion.id) {
                return Err(unsupported(
                    "each new criterion needs a stable canonical UUID",
                ));
            }
            let mut mapping = Mapping::new();
            mapping.insert(key("id"), key(&criterion.id));
            mapping.insert(key("requires_human"), Value::Bool(criterion.requires_human));
            mapping
        };
        mapping.insert(key("description"), key(&criterion.description));
        mappings.push(Value::Mapping(mapping));
    }
    if criteria == old {
        return Ok(original.text);
    }
    let mut subtree = Mapping::new();
    subtree.insert(key("criteria"), Value::Sequence(mappings));
    let rendered = serde_yaml::to_string(&subtree)
        .map_err(|_| unsupported("criteria cannot be represented as YAML"))?;
    let rendered = if original.newline == "\r\n" {
        rendered.replace('\n', "\r\n")
    } else {
        rendered
    };
    let size = original.text.len() - original.region.len() + rendered.len();
    if size > MAX_SOURCE_BYTES {
        return Err(unsupported("complete proposed source exceeds 8 MiB"));
    }
    let mut text = String::with_capacity(size);
    text.push_str(&original.text[..original.region.start]);
    text.push_str(&rendered);
    text.push_str(&original.text[original.region.end..]);
    let proposed = SourceSnapshot {
        revision: revision(text.as_bytes()),
        content_base64: STANDARD.encode(text.as_bytes()),
        ..base.clone()
    };
    let updated = parse(&proposed, goal_id)?;
    let preserved_criteria = updated.metadata.get(key("criteria")) == subtree.get(key("criteria"));
    let mut before = original.metadata;
    let mut after = updated.metadata;
    before.remove(key("criteria"));
    after.remove(key("criteria"));
    if before != after || !preserved_criteria || edits(&updated.criteria)? != criteria {
        return Err(unsupported(
            "YAML aliases or layout change values outside the allowed edits",
        ));
    }
    Ok(text)
}

/// Require the exact canonical candidate generated from this bound base.
/// Runtime eligibility and configured goal placement are checked by the caller.
pub fn validate_write(
    base: &SourceSnapshot,
    request: &SourceWrite,
    goal_id: &str,
) -> Result<(), String> {
    if request.schema != SCHEMA
        || !canonical_uuid(&request.operation_id)
        || request.brain_id != base.brain_id
        || request.path != base.path
        || request.expected_revision.as_ref() != Some(&base.revision)
    {
        return Err(unsupported(
            "Save does not match the original source identity and revision",
        ));
    }
    let bytes = decode(&request.content_base64)?;
    let proposed = SourceSnapshot {
        revision: revision(&bytes),
        content_base64: request.content_base64.clone(),
        ..base.clone()
    };
    let fields = inspect(&proposed, goal_id)?;
    if transform(base, goal_id, &fields)?.as_bytes() != bytes {
        return Err(unsupported(
            "Save changes source outside the permitted criteria edits",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRAIN: &str = "10000000-0000-4000-8000-000000000001";
    const GOAL: &str = "20000000-0000-4000-8000-000000000001";
    const NEW: &str = "30000000-0000-4000-8000-000000000001";

    fn snapshot(text: &str) -> SourceSnapshot {
        SourceSnapshot {
            schema: SCHEMA.into(),
            brain_id: BRAIN.into(),
            path: format!("records/goal-{GOAL}.md"),
            revision: revision(text.as_bytes()),
            content_base64: STANDARD.encode(text.as_bytes()),
            media_type: "text/markdown".into(),
        }
    }

    fn source(criteria: &str) -> String {
        format!(
            "---\nschema: {SCHEMA}\nbrain_id: {BRAIN}\nrecord_type: goal\nid: {GOAL}\ntitle: Keep this\nstatus: completed\n{criteria}# Leave this comment with the next field.\nunknown: {{nested: [one, two], flag: true}}\nstage_ids:\n- old-stage\ntask_ref: {{provider: custom, external_id: opaque}}\n---\n\n# Exact body\r\n\nNo final newline"
        )
    }

    fn base() -> SourceSnapshot {
        snapshot(&source(
            "criteria:\n- id: C1\n  description: Old description\n  requires_human: true\n  unknown_row: {nested: [yes, 42], custom: value}\n",
        ))
    }

    fn write(base: &SourceSnapshot, proposed: &str) -> SourceWrite {
        SourceWrite {
            schema: SCHEMA.into(),
            operation_id: "40000000-0000-4000-8000-000000000001".into(),
            brain_id: base.brain_id.clone(),
            path: base.path.clone(),
            expected_revision: Some(base.revision.clone()),
            content_base64: STANDARD.encode(proposed.as_bytes()),
        }
    }

    #[test]
    fn edit_and_append_preserve_unknown_values_and_exact_surrounding_source() {
        let base = base();
        let old = parse(&base, GOAL).unwrap();
        let mut fields = inspect(&base, GOAL).unwrap();
        fields[0].description = "New description\nWith Unicode 😀 and: punctuation".into();
        fields.push(CriterionEdit {
            id: NEW.into(),
            description: "Additional criterion".into(),
            requires_human: false,
        });
        let result = transform(&base, GOAL, &fields).unwrap();
        let new = parse(&snapshot(&result), GOAL).unwrap();
        assert_eq!(&old.text[..old.region.start], &new.text[..new.region.start]);
        assert_eq!(&old.text[old.region.end..], &new.text[new.region.end..]);
        assert_eq!(
            old.criteria[0].get(key("unknown_row")),
            new.criteria[0].get(key("unknown_row"))
        );
        assert_eq!(new.metadata[key("status")], key("completed"));
        assert_eq!(inspect(&snapshot(&result), GOAL).unwrap(), fields);
        validate_write(&base, &write(&base, &result), GOAL).unwrap();
    }

    #[test]
    fn no_op_keeps_original_criteria_formatting_and_comments() {
        let base = snapshot(&source("criteria: # keep\n  - id: 'C1'\n    description: \"Old description\" # keep too\n    requires_human: true\n"));
        let result = transform(&base, GOAL, &inspect(&base, GOAL).unwrap()).unwrap();
        assert_eq!(result.as_bytes(), decode(&base.content_base64).unwrap());
        validate_write(&base, &write(&base, &result), GOAL).unwrap();
    }

    #[test]
    fn trailing_keep_scalar_cannot_silently_change_unknown_row_value() {
        let base = snapshot(&source(
            "criteria:\n- id: C1\n  description: Old\n  requires_human: true\n  unknown_row: |+\n    Keep these newlines\n\n\n",
        ));
        let original = parse(&base, GOAL).unwrap();
        assert_eq!(
            original.criteria[0].get(key("unknown_row")),
            Some(&key("Keep these newlines\n\n\n"))
        );
        let mut fields = inspect(&base, GOAL).unwrap();
        assert_eq!(transform(&base, GOAL, &fields).unwrap(), original.text);
        fields[0].description = "Changed".into();
        assert!(transform(&base, GOAL, &fields)
            .unwrap_err()
            .contains("Use Source"));
    }

    #[test]
    fn crlf_frontmatter_and_mixed_body_remain_exact_outside_criteria() {
        let original =
            source("criteria:\n  - id: C1\n    description: Old\n    requires_human: false\n");
        let (front, body) = original.rsplit_once("---\n").unwrap();
        let base = snapshot(&format!("{}---\r\n{body}", front.replace('\n', "\r\n")));
        let original = parse(&base, GOAL).unwrap();
        let mut fields = inspect(&base, GOAL).unwrap();
        fields[0].description = "Two\nlines".into();
        let result = transform(&base, GOAL, &fields).unwrap();
        let updated = parse(&snapshot(&result), GOAL).unwrap();
        assert_eq!(
            &original.text[original.region.end..],
            &updated.text[updated.region.end..]
        );
        assert_eq!(
            &original.text[..original.region.start],
            &updated.text[..updated.region.start]
        );
        assert!(result.ends_with(body));
        assert!(result[updated.region]
            .replace("\r\n", "")
            .find('\n')
            .is_none());
    }

    #[test]
    fn saved_identity_order_and_confirmation_kind_cannot_change() {
        let base = snapshot(&source("criteria:\n- id: C1\n  description: First\n  requires_human: true\n- id: C2\n  description: Second\n  requires_human: false\n"));
        let fields = inspect(&base, GOAL).unwrap();
        let mut changed = fields.clone();
        changed.swap(0, 1);
        assert!(transform(&base, GOAL, &changed).is_err());
        changed = fields.clone();
        changed[0].id = NEW.into();
        assert!(transform(&base, GOAL, &changed).is_err());
        changed = fields.clone();
        changed[0].requires_human = false;
        assert!(transform(&base, GOAL, &changed).is_err());
        assert!(transform(&base, GOAL, &fields[..1]).is_err());
    }

    #[test]
    fn appended_ids_and_description_bounds_are_explicit() {
        let base = base();
        let mut fields = inspect(&base, GOAL).unwrap();
        for id in ["C2", "30000000-0000-4000-8000-00000000000A", "C1"] {
            let mut added = fields.clone();
            added.push(CriterionEdit {
                id: id.into(),
                description: "New".into(),
                requires_human: true,
            });
            assert!(transform(&base, GOAL, &added).is_err());
        }
        fields[0].description = "é".repeat(MAX_DESCRIPTION_BYTES / 2);
        assert!(transform(&base, GOAL, &fields).is_ok());
        fields[0].description.push('!');
        assert!(transform(&base, GOAL, &fields).is_err());
        fields[0].description = " \r\n ".into();
        assert!(transform(&base, GOAL, &fields).is_err());
    }

    #[test]
    fn unsupported_or_duplicate_yaml_is_not_silently_normalized() {
        for criteria in [
            "criteria: [{id: C1, description: Old, requires_human: true}]\n",
            "'criteria':\n- id: C1\n  description: Old\n  requires_human: true\n",
            "criteria:\n- id: C1\n  description: Old\n  requires_human: true\ncriteria: []\n",
            "criteria:\n- id: C1\n  id: C2\n  description: Old\n  requires_human: true\n",
            "criteria:\n- id: C1\n  description: Old\n  requires_human: true\n- id: C1\n  description: Duplicate\n  requires_human: false\n",
        ] {
            assert!(inspect(&snapshot(&source(criteria)), GOAL)
                .unwrap_err()
                .contains("Use Source"));
        }
    }

    #[test]
    fn alias_dependency_outside_criteria_is_refused_on_change() {
        let original = source(
            "criteria:\n- &criterion\n  id: C1\n  description: Old\n  requires_human: true\n",
        )
        .replace("unknown:", "reference: *criterion\nunknown:");
        let base = snapshot(&original);
        let mut fields = inspect(&base, GOAL).unwrap();
        fields[0].description = "Changed".into();
        assert!(transform(&base, GOAL, &fields)
            .unwrap_err()
            .contains("Use Source"));
    }

    #[test]
    fn bounded_form_refuses_oversized_existing_data() {
        let mut criteria = String::from("criteria:\n");
        for index in 0..=MAX_CRITERIA {
            criteria.push_str(&format!(
                "- id: C{index}\n  description: Text\n  requires_human: false\n"
            ));
        }
        assert!(inspect(&snapshot(&source(&criteria)), GOAL).is_err());
        let oversized = format!(
            "{}{}",
            source("criteria:\n- id: C1\n  description: Old\n  requires_human: false\n"),
            "x".repeat(MAX_SOURCE_BYTES)
        );
        assert!(inspect(&snapshot(&oversized), GOAL).is_err());
    }

    #[test]
    fn validate_write_rejects_tampering_with_unknown_metadata_body_and_owner() {
        let base = base();
        let mut fields = inspect(&base, GOAL).unwrap();
        fields[0].description = "Updated".into();
        let candidate = transform(&base, GOAL, &fields).unwrap();
        for changed in [
            candidate.replace("custom: value", "custom: replaced"),
            candidate.replace("flag: true", "flag: false"),
            candidate.replace("No final newline", "Different body"),
            candidate.replace("status: completed", "status: active"),
            candidate.replace("external_id: opaque", "external_id: replaced"),
        ] {
            assert_ne!(candidate, changed);
            assert!(validate_write(&base, &write(&base, &changed), GOAL).is_err());
        }
        let request = write(&base, &candidate);
        let mut bad = request.clone();
        bad.path = "records/other.md".into();
        assert!(validate_write(&base, &bad, GOAL).is_err());
        bad = request.clone();
        bad.operation_id = "not-an-operation".into();
        assert!(validate_write(&base, &bad, GOAL).is_err());
        bad = request;
        bad.expected_revision = Some(revision(b"other"));
        assert!(validate_write(&base, &bad, GOAL).is_err());
        let mut bad_base = base.clone();
        bad_base.revision = revision(b"different base");
        assert!(inspect(&bad_base, GOAL).is_err());
        assert!(inspect(&base, NEW).is_err());
    }
}
