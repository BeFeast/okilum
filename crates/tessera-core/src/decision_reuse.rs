//! One lossless automatic-to-manual transition. No storage or execution policy.
use crate::source::{SourceSnapshot, SourceWrite};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const MAX_BYTES: usize = 8192;
pub const AUTOMATIC: &str = "discussion-decision";
pub const MANUAL: &str = "discussion-decision-manual";
const SCHEMA: &str = "tessera-discussion-decision-reuse/v1";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Disposition {
    pub schema: String,
    pub mode: String,
    pub operation_id: String,
    pub actor_id: String,
    pub at: String,
    pub previous_revision: String,
}
impl Disposition {
    pub fn new(operation: String, actor: String, at: String, revision: String) -> Self {
        Self {
            schema: SCHEMA.into(),
            mode: "manual_only".into(),
            operation_id: operation,
            actor_id: actor,
            at,
            previous_revision: revision,
        }
    }
}
fn error(s: impl std::fmt::Display) -> String {
    format!("Cannot change decision reuse: {s}")
}
pub fn revision(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
fn uuid(s: &str) -> bool {
    Uuid::parse_str(s).is_ok_and(|id| id.to_string() == s)
}
struct Document {
    text: String,
    metadata: Value,
    close: usize,
    newline: &'static str,
}
fn document(source: &SourceSnapshot) -> Result<Document, String> {
    if source.schema != "ai-brain/v1"
        || source.media_type != "text/markdown"
        || !uuid(&source.brain_id)
        || source.content_base64.len() > MAX_BYTES.div_ceil(3) * 4
    {
        return Err(error("invalid source or complete decision exceeds 8 KiB"));
    }
    let bytes = STANDARD.decode(&source.content_base64).map_err(error)?;
    if bytes.len() > MAX_BYTES || revision(&bytes) != source.revision {
        return Err(error("invalid bounded source revision"));
    }
    let text = String::from_utf8(bytes).map_err(error)?;
    let newline = if text.starts_with("---\r\n") {
        "\r\n"
    } else if text.starts_with("---\n") {
        "\n"
    } else {
        return Err(error("ordinary YAML frontmatter required"));
    };
    let start = 3 + newline.len();
    let mut close = start;
    for line in text[start..].split_inclusive('\n') {
        if line == format!("---{newline}") {
            let metadata: Value = serde_yaml::from_str(&text[start..close]).map_err(error)?;
            if !metadata.is_mapping() {
                return Err(error("frontmatter must be a mapping"));
            }
            return Ok(Document {
                text,
                metadata,
                close,
                newline,
            });
        }
        close += line.len();
    }
    Err(error("unambiguous closing frontmatter required"))
}
fn identity(d: &Document, s: &SourceSnapshot, goal: &str, decision: &str) -> Result<(), String> {
    if !uuid(goal)
        || !uuid(decision)
        || d.metadata["schema"].as_str() != Some("ai-brain/v1")
        || d.metadata["brain_id"].as_str() != Some(&s.brain_id)
        || d.metadata["goal_id"].as_str() != Some(goal)
        || d.metadata["id"].as_str() != Some(decision)
        || s.path.rsplit('/').next() != Some(format!("discussion-decision-{decision}.md").as_str())
    {
        return Err(error("original goal/decision identity mismatch"));
    }
    Ok(())
}
fn block(policy: &Disposition, newline: &str) -> Result<String, String> {
    let yaml = serde_yaml::to_string(policy).map_err(error)?;
    Ok(format!(
        "reuse_disposition:{newline}{}",
        yaml.lines()
            .map(|l| format!("  {l}{newline}"))
            .collect::<String>()
    ))
}
fn valid_policy(d: &Document, policy: &Disposition) -> Result<(), String> {
    let at =
        time::OffsetDateTime::parse(&policy.at, &time::format_description::well_known::Rfc3339)
            .map_err(error)?;
    if policy.schema != SCHEMA
        || policy.mode != "manual_only"
        || !uuid(&policy.operation_id)
        || policy.actor_id.trim().is_empty()
        || d.metadata["actor_id"].as_str() != Some(&policy.actor_id)
        || !at.offset().is_utc()
    {
        return Err(error("unsupported disposition or original actor mismatch"));
    }
    Ok(())
}
fn replace_kind(front: &str, from: &str, to: &str, newline: &str) -> Result<String, String> {
    let expected = format!("record_type: {from}{newline}");
    if front
        .split_inclusive('\n')
        .filter(|l| *l == expected)
        .count()
        != 1
    {
        return Err(error(
            "record_type needs the original canonical single-line form; inspect Source",
        ));
    }
    Ok(front
        .split_inclusive('\n')
        .map(|l| {
            if l == expected {
                format!("record_type: {to}{newline}")
            } else {
                l.into()
            }
        })
        .collect())
}
/// Recover the exact original bytes; a portable policy is not a journal receipt.
pub fn original(
    source: &SourceSnapshot,
    goal: &str,
    decision: &str,
) -> Result<(SourceSnapshot, Option<Disposition>), String> {
    let d = document(source)?;
    identity(&d, source, goal, decision)?;
    if d.metadata["record_type"].as_str() == Some(AUTOMATIC) {
        if d.metadata
            .as_mapping()
            .unwrap()
            .contains_key(Value::String("reuse_disposition".into()))
        {
            return Err(error("automatic kind contradicts disposition"));
        }
        return Ok((source.clone(), None));
    }
    if d.metadata["record_type"].as_str() != Some(MANUAL) {
        return Err(error("unsupported decision kind"));
    }
    let policy: Disposition =
        serde_yaml::from_value(d.metadata["reuse_disposition"].clone()).map_err(error)?;
    valid_policy(&d, &policy)?;
    let suffix = block(&policy, d.newline)?;
    let front = d.text[..d.close]
        .strip_suffix(&suffix)
        .ok_or_else(|| error("policy block is not the exact canonical appended block"))?;
    let text = format!(
        "{}{}",
        replace_kind(front, MANUAL, AUTOMATIC, d.newline)?,
        &d.text[d.close..]
    );
    if revision(text.as_bytes()) != policy.previous_revision {
        return Err(error("policy predecessor digest mismatch"));
    }
    let mut base = source.clone();
    base.revision = policy.previous_revision.clone();
    base.content_base64 = STANDARD.encode(text);
    let original = document(&base)?;
    if original
        .metadata
        .as_mapping()
        .unwrap()
        .contains_key(Value::String("reuse_disposition".into()))
    {
        return Err(error("duplicate policy"));
    }
    Ok((base, Some(policy)))
}
pub fn transform(
    base: &SourceSnapshot,
    goal: &str,
    decision: &str,
    policy: &Disposition,
) -> Result<String, String> {
    let d = document(base)?;
    identity(&d, base, goal, decision)?;
    if original(base, goal, decision)?.1.is_some() {
        return Err(error("decision is already manual-only"));
    }
    valid_policy(&d, policy)?;
    if policy.previous_revision != base.revision {
        return Err(error("policy base revision mismatch"));
    }
    let text = format!(
        "{}{}{}",
        replace_kind(&d.text[..d.close], AUTOMATIC, MANUAL, d.newline)?,
        block(policy, d.newline)?,
        &d.text[d.close..]
    );
    if text.len() > MAX_BYTES {
        return Err(error(
            "complete decision with policy exceeds 8 KiB; original preserved",
        ));
    }
    Ok(text)
}
pub fn proposed(request: &SourceWrite) -> Result<SourceSnapshot, String> {
    if request.content_base64.len() > MAX_BYTES.div_ceil(3) * 4 {
        return Err(error("decision exceeds 8 KiB"));
    }
    let bytes = STANDARD.decode(&request.content_base64).map_err(error)?;
    if bytes.len() > MAX_BYTES {
        return Err(error("decision exceeds 8 KiB"));
    }
    Ok(SourceSnapshot {
        schema: request.schema.clone(),
        brain_id: request.brain_id.clone(),
        path: request.path.clone(),
        revision: revision(&bytes),
        content_base64: request.content_base64.clone(),
        media_type: "text/markdown".into(),
    })
}
pub fn validate_write(
    base: &SourceSnapshot,
    request: &SourceWrite,
    goal: &str,
    decision: &str,
) -> Result<Disposition, String> {
    let (restored, policy) = original(&proposed(request)?, goal, decision)?;
    let policy = policy.ok_or_else(|| error("manual-only transition required"))?;
    if &restored != base
        || request.expected_revision.as_ref() != Some(&base.revision)
        || request.operation_id != policy.operation_id
    {
        return Err(error(
            "request does not preserve exact reviewed base/operation",
        ));
    }
    Ok(policy)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(newline: &str) -> (SourceSnapshot, String, String) {
        let goal = Uuid::new_v4().to_string();
        let id = Uuid::new_v4().to_string();
        let brain = Uuid::new_v4().to_string();
        let text=format!("---\nschema: ai-brain/v1\nrecord_type: discussion-decision\nbrain_id: {brain}\nid: {id}\ngoal_id: {goal}\nactor_id: local:oleg\n# Keep comment\ncustom: {{foo: '  untouched  '}}\n---\n  Choice λ\n\n").replace('\n',newline);
        (
            SourceSnapshot {
                schema: "ai-brain/v1".into(),
                brain_id: brain,
                path: format!("records/discussion-decision-{id}.md"),
                revision: revision(text.as_bytes()),
                content_base64: STANDARD.encode(text),
                media_type: "text/markdown".into(),
            },
            goal,
            id,
        )
    }
    #[test]
    fn lossless_policy_reconstructs_exact_original_and_rejects_actor_or_body_changes() {
        for nl in ["\n", "\r\n"] {
            let (base, g, id) = fixture(nl);
            let p = Disposition::new(
                Uuid::new_v4().to_string(),
                "local:oleg".into(),
                "2026-09-08T12:00:00Z".into(),
                base.revision.clone(),
            );
            let text = transform(&base, &g, &id, &p).unwrap();
            let mut r = SourceWrite {
                schema: base.schema.clone(),
                brain_id: base.brain_id.clone(),
                path: base.path.clone(),
                operation_id: p.operation_id.clone(),
                expected_revision: Some(base.revision.clone()),
                content_base64: STANDARD.encode(&text),
            };
            assert_eq!(validate_write(&base, &r, &g, &id).unwrap(), p);
            assert_eq!(original(&proposed(&r).unwrap(), &g, &id).unwrap().0, base);
            r.content_base64 = STANDARD.encode(text.replace("  Choice λ", "  Different λ"));
            assert!(validate_write(&base, &r, &g, &id).is_err());
            let mut bad = p.clone();
            bad.actor_id = "other".into();
            assert!(transform(&base, &g, &id, &bad).is_err());
            r.content_base64 =
                STANDARD.encode(text.replace("actor_id: local:oleg", "actor_id: other"));
            assert!(validate_write(&base, &r, &g, &id).is_err());
        }
    }
    #[test]
    fn rejects_duplicate_policy_oversize_and_wrong_identity() {
        let (mut base, g, id) = fixture("\n");
        let p = Disposition::new(
            Uuid::new_v4().to_string(),
            "local:oleg".into(),
            "2026-09-08T12:00:00Z".into(),
            base.revision.clone(),
        );
        assert!(transform(&base, &Uuid::new_v4().to_string(), &id, &p).is_err());
        base.content_base64 = STANDARD.encode("x".repeat(MAX_BYTES + 1));
        assert!(original(&base, &g, &id).is_err());
        let (mut base, g, id) = fixture("\n");
        let text = String::from_utf8(STANDARD.decode(&base.content_base64).unwrap())
            .unwrap()
            .replacen(
                "# Keep comment",
                "reuse_disposition: null\n# Keep comment",
                1,
            );
        base.revision = revision(text.as_bytes());
        base.content_base64 = STANDARD.encode(text);
        assert!(original(&base, &g, &id).is_err());
    }
}
