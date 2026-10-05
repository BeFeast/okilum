//! Bounded receipt compatibility for preserved original compact JSON envelopes.
//!
//! Only nested object member order may differ. This is not route/origin proof.
//! `ContextPacket` has five known optional opaque JSON extension fields: the four
//! legacy fields and `reviewed_packet`. Their entire values are compared without
//! normalization, dropping keys, sorting arrays, or reconstructing old bytes.
use crate::StartEnvelope;
use anyhow::{bail, ensure, Result};
use serde::{
    de::{self, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    io::Read,
    path::PathBuf,
};

const MAX_ARTIFACT: u64 = 64 * 1024 * 1024;
const PROOF_SCHEMA: &str = "tessera-t3-envelope-compat/v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactEntry {
    pub operation_id: String,
    pub artifact_path: PathBuf,
    pub artifact_sha256: String,
    pub provenance: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub entries: Vec<ArtifactEntry>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedProof {
    pub schema: String,
    pub operation_id: String,
    pub artifact_sha256: String,
    pub provenance: String,
    /// JSON pointer inside the pinned original artifact; never a mutable pathname.
    pub source_locator: String,
    pub original_envelope: String,
    pub envelope_sha256: String,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

// serde_json ordinarily permits duplicate keys. Refuse them at every nesting
// level before either extraction or typed decoding; escaped aliases also collide.
struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Unique;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("JSON without duplicate object keys")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Unique, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| Unique(Value::Number(n)))
                    .ok_or_else(|| E::custom("invalid number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut values = vec![];
                while let Some(Unique(v)) = a.next_element()? {
                    values.push(v);
                }
                Ok(Unique(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut values = Map::new();
                let mut seen = BTreeSet::new();
                while let Some(k) = a.next_key::<String>()? {
                    if !seen.insert(k.clone()) {
                        return Err(de::Error::custom("duplicate key"));
                    }
                    let Unique(v) = a.next_value()?;
                    values.insert(k, v);
                }
                Ok(Unique(Value::Object(values)))
            }
        }
        d.deserialize_any(V)
    }
}
fn unique(bytes: &[u8]) -> Result<Value> {
    // Do not leak original source content through parser diagnostics.
    serde_json::from_slice::<Unique>(bytes)
        .map(|v| v.0)
        .map_err(|_| anyhow::anyhow!("compat_invalid_or_duplicate_json"))
}

/// Locate exact JSON value spans; syntax/duplicate keys were already validated.
/// Tracks decoded pointer components so arbitrary string contents never count as
/// dispatch structure. Raw UTF-8 string/value bytes are never rewritten.
fn spans(bytes: &[u8]) -> Result<Vec<(Vec<String>, usize, usize)>> {
    fn ws(b: &[u8], i: &mut usize) {
        while *i < b.len() && b[*i].is_ascii_whitespace() {
            *i += 1;
        }
    }
    fn string_end(b: &[u8], i: &mut usize) -> Result<()> {
        ensure!(b.get(*i) == Some(&b'"'), "compat_bad_span");
        *i += 1;
        while let Some(c) = b.get(*i) {
            *i += 1;
            match c {
                b'\\' => *i += 1,
                b'"' => return Ok(()),
                _ => {}
            }
        }
        bail!("compat_bad_span")
    }
    fn value(
        b: &[u8],
        i: &mut usize,
        path: &mut Vec<String>,
        out: &mut Vec<(Vec<String>, usize, usize)>,
    ) -> Result<()> {
        ws(b, i);
        let start = *i;
        match b.get(*i) {
            Some(b'{') => {
                *i += 1;
                ws(b, i);
                if b.get(*i) != Some(&b'}') {
                    loop {
                        ws(b, i);
                        let key_start = *i;
                        string_end(b, i)?;
                        let key: String = serde_json::from_slice(&b[key_start..*i])
                            .map_err(|_| anyhow::anyhow!("compat_bad_span"))?;
                        ws(b, i);
                        ensure!(b.get(*i) == Some(&b':'), "compat_bad_span");
                        *i += 1;
                        path.push(key);
                        value(b, i, path, out)?;
                        path.pop();
                        ws(b, i);
                        if b.get(*i) != Some(&b',') {
                            break;
                        }
                        *i += 1;
                    }
                }
                ensure!(b.get(*i) == Some(&b'}'), "compat_bad_span");
                *i += 1;
            }
            Some(b'[') => {
                *i += 1;
                ws(b, i);
                let mut n = 0;
                if b.get(*i) != Some(&b']') {
                    loop {
                        path.push(n.to_string());
                        value(b, i, path, out)?;
                        path.pop();
                        n += 1;
                        ws(b, i);
                        if b.get(*i) != Some(&b',') {
                            break;
                        }
                        *i += 1;
                    }
                }
                ensure!(b.get(*i) == Some(&b']'), "compat_bad_span");
                *i += 1;
            }
            Some(b'"') => string_end(b, i)?,
            Some(_) => {
                while *i < b.len()
                    && !matches!(b[*i], b',' | b']' | b'}' | b' ' | b'\n' | b'\r' | b'\t')
                {
                    *i += 1;
                }
            }
            None => bail!("compat_bad_span"),
        }
        if path.last().is_some_and(|s| s == "envelope")
            || b.get(start)
                .is_some_and(|c| c.is_ascii_digit() || *c == b'-')
        {
            out.push((path.clone(), start, *i));
        }
        Ok(())
    }
    let mut out = vec![];
    let mut i = 0;
    value(bytes, &mut i, &mut vec![], &mut out)?;
    ws(bytes, &mut i);
    ensure!(i == bytes.len(), "compat_bad_span");
    Ok(out)
}
fn dispatch_path(path: &[String]) -> bool {
    let path = if path.first().is_some_and(|p| p == "state") {
        &path[1..]
    } else {
        path
    };
    let path = if path.first().is_some_and(|p| p == "other_goals") {
        if path.len() < 3 {
            return false;
        }
        &path[2..]
    } else {
        path
    };
    matches!(path,[a,b] if a=="dispatch" && b=="envelope")
        || matches!(path,[a,_,c,d] if a=="previous_stages" && c=="dispatch" && d=="envelope")
}
type EnvelopeSpans = BTreeMap<String, Vec<(Vec<String>, usize, usize)>>;
fn envelope_spans(bytes: &[u8]) -> Result<EnvelopeSpans> {
    let artifact = unique(bytes)?;
    ensure!(artifact.is_object(), "compat_invalid_artifact");
    let wrapped = artifact.get("schema").is_some();
    if wrapped {
        ensure!(
            artifact["schema"] == "tessera-runtime/routes-v2" && artifact["state"].is_object(),
            "compat_unsupported_artifact_schema"
        );
    }
    let mut found = EnvelopeSpans::new();
    for (path, start, end) in spans(bytes)? {
        if dispatch_path(&path) && (path.first().is_some_and(|p| p == "state") == wrapped) {
            let mut value = &artifact;
            for part in &path {
                value = if value.is_array() {
                    part.parse::<usize>().ok().and_then(|i| value.get(i))
                } else {
                    value.get(part)
                }
                .ok_or_else(|| anyhow::anyhow!("compat_invalid_artifact"))?;
            }
            if let Some(operation) = value["operation_id"].as_str() {
                found
                    .entry(operation.to_owned())
                    .or_default()
                    .push((path, start, end));
            }
        }
    }
    Ok(found)
}
fn original_span<'a>(bytes: &'a [u8], operation: &str) -> Result<(Vec<String>, &'a [u8])> {
    let mut index = envelope_spans(bytes)?;
    let mut found = index.remove(operation).unwrap_or_default();
    ensure!(found.len() == 1, "compat_ambiguous_or_missing_operation");
    let (path, start, end) = found.pop().unwrap();
    Ok((path, &bytes[start..end]))
}
/// Check original persisted bytes before typed Runner decoding can discard data.
/// Scan the journal once; compatibility-only identity restrictions are not loader rules.
pub fn validate_current_artifacts(bytes: &[u8], currents: &[&StartEnvelope]) -> Result<()> {
    ensure!(
        bytes.len() as u64 <= MAX_ARTIFACT,
        "compat_invalid_artifact"
    );
    let index = envelope_spans(bytes)?;
    for current in currents {
        let found = index
            .get(&current.operation_id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        ensure!(found.len() == 1, "compat_ambiguous_or_missing_operation");
        let (_, start, end) = &found[0];
        ensure!(
            lossless_typed(&bytes[*start..*end])? == **current,
            "compat_current_decode_changed"
        );
    }
    Ok(())
}
pub fn validate_current_artifact(bytes: &[u8], current: &StartEnvelope) -> Result<()> {
    validate_current_artifacts(bytes, &[current])
}

// JSON Value's floating-number parser can coerce a changed raw token to the
// same f64. Compare number lexemes by decoded structural path, permitting only
// surrounding whitespace and object order changes, never numeric precision loss.
fn number_tokens(bytes: &[u8]) -> Result<BTreeMap<Vec<String>, Vec<u8>>> {
    Ok(spans(bytes)?
        .into_iter()
        .filter(|(_, start, _)| bytes[*start].is_ascii_digit() || bytes[*start] == b'-')
        .map(|(path, start, end)| (path, bytes[start..end].to_vec()))
        .collect())
}
fn lossless_typed(raw: &[u8]) -> Result<StartEnvelope> {
    let value = unique(raw)?;
    let envelope: StartEnvelope = serde_json::from_value(value.clone())
        .map_err(|_| anyhow::anyhow!("compat_unsupported_schema"))?;
    ensure!(
        envelope.schema == crate::SCHEMA,
        "compat_unsupported_schema"
    );
    // Round-trip comparison rejects missing/defaulted or silently dropped fields
    // in typed structures, including SourceRef's optional members.
    ensure!(
        serde_json::to_value(&envelope)? == value,
        "compat_lossy_schema"
    );
    ensure!(
        number_tokens(raw)? == number_tokens(&serde_json::to_vec(&envelope)?)?,
        "compat_lossy_number"
    );
    Ok(envelope)
}
fn typed(raw: &[u8]) -> Result<StartEnvelope> {
    let envelope = lossless_typed(raw)?;
    const EXTENSIONS: &[&str] = &[
        "conversation",
        "previous_result",
        "source_excerpts",
        "source_snapshots",
        "reviewed_packet",
    ];
    ensure!(
        envelope
            .packet
            .extra
            .keys()
            .all(|k| EXTENSIONS.contains(&k.as_str())),
        "compat_unknown_extension"
    );
    ensure!(
        envelope.target.len() == 3
            && ["created_at", "environment_id", "project_id"]
                .iter()
                .all(|k| envelope
                    .target
                    .get(*k)
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())),
        "compat_unsupported_target"
    );
    ensure!(
        envelope.packet.id == envelope.context_id
            && envelope.packet.goal_id == envelope.goal_id
            && envelope.packet.stage_id == envelope.stage_id,
        "compat_identity_mismatch"
    );
    for id in [
        &envelope.operation_id,
        &envelope.goal_id,
        &envelope.stage_id,
        &envelope.context_id,
    ] {
        ensure!(
            uuid::Uuid::parse_str(id).is_ok_and(|u| u.to_string() == *id),
            "compat_identity_mismatch"
        );
    }
    ensure!(
        !envelope.context_revision.is_empty() && !envelope.packet.goal_revision.is_empty(),
        "compat_identity_mismatch"
    );
    Ok(envelope)
}
fn verify_envelope(original: &str, current: &StartEnvelope, fingerprint: &str) -> Result<()> {
    ensure!(
        sha256(fingerprint) && hash(original.as_bytes()) == fingerprint,
        "compat_receipt_fingerprint_mismatch"
    );
    let old = typed(original.as_bytes())?;
    // Supported compact struct serializer must reproduce the preserved bytes.
    // No whitespace compaction, key permutation, or historical-order guessing.
    ensure!(
        serde_json::to_vec(&old)? == original.as_bytes(),
        "compat_not_original_compact_serialization"
    );
    let now = typed(&serde_json::to_vec(current)?)?;
    ensure!(old == now, "compat_envelope_changed");
    Ok(())
}

pub fn verify(
    entry: &ArtifactEntry,
    current: &StartEnvelope,
    receipt_fingerprint: &str,
) -> Result<VerifiedProof> {
    ensure!(
        entry.operation_id == current.operation_id,
        "compat_operation_mismatch"
    );
    ensure!(
        sha256(&entry.artifact_sha256)
            && !entry.provenance.trim().is_empty()
            && entry.provenance.len() <= 4096,
        "compat_missing_provenance"
    );
    ensure!(
        entry.artifact_path.is_absolute(),
        "compat_artifact_path_not_absolute"
    );
    let meta = fs::symlink_metadata(&entry.artifact_path)
        .map_err(|_| anyhow::anyhow!("compat_artifact_unavailable"))?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= MAX_ARTIFACT,
        "compat_invalid_artifact"
    );
    let mut bytes = vec![];
    fs::File::open(&entry.artifact_path)
        .map_err(|_| anyhow::anyhow!("compat_artifact_unavailable"))?
        .take(MAX_ARTIFACT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("compat_artifact_unavailable"))?;
    ensure!(
        bytes.len() as u64 <= MAX_ARTIFACT && hash(&bytes) == entry.artifact_sha256,
        "compat_artifact_digest_mismatch"
    );
    let (path, raw) = original_span(&bytes, &entry.operation_id)?;
    let original = std::str::from_utf8(raw).map_err(|_| anyhow::anyhow!("compat_non_utf8"))?;
    verify_envelope(original, current, receipt_fingerprint)?;
    Ok(VerifiedProof {
        schema: PROOF_SCHEMA.into(),
        operation_id: entry.operation_id.clone(),
        artifact_sha256: entry.artifact_sha256.clone(),
        provenance: entry.provenance.clone(),
        source_locator: format!(
            "/{}",
            path.iter()
                .map(|p| p.replace('~', "~0").replace('/', "~1"))
                .collect::<Vec<_>>()
                .join("/")
        ),
        original_envelope: original.into(),
        envelope_sha256: hash(original.as_bytes()),
    })
}
/// Retained verification requires no original backup or filesystem/network access.
pub fn verify_retained(
    proof: &VerifiedProof,
    current: &StartEnvelope,
    receipt_fingerprint: &str,
) -> Result<()> {
    ensure!(
        proof.schema == PROOF_SCHEMA
            && proof.operation_id == current.operation_id
            && sha256(&proof.artifact_sha256)
            && !proof.provenance.trim().is_empty()
            && proof.provenance.len() <= 4096,
        "compat_invalid_proof"
    );
    let path = proof
        .source_locator
        .strip_prefix('/')
        .ok_or_else(|| anyhow::anyhow!("compat_invalid_locator"))?
        .split('/')
        .map(|p| p.replace("~1", "/").replace("~0", "~"))
        .collect::<Vec<_>>();
    ensure!(
        dispatch_path(&path) && proof.envelope_sha256 == receipt_fingerprint,
        "compat_invalid_proof"
    );
    verify_envelope(&proof.original_envelope, current, receipt_fingerprint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn envelope() -> StartEnvelope {
        serde_json::from_value(json!({
            "schema":"ai-brain/v1","operation_id":"06000000-0000-4000-8000-000000000001",
            "goal_id":"01000000-0000-4000-8000-000000000001","stage_id":"02000000-0000-4000-8000-000000000001",
            "context_id":"03000000-0000-4000-8000-000000000001","context_revision":"context-revision",
            "packet":{"id":"03000000-0000-4000-8000-000000000001","goal_id":"01000000-0000-4000-8000-000000000001","stage_id":"02000000-0000-4000-8000-000000000001","goal_revision":"goal-revision","goal":"fixture goal","decisions":["first","second"],"constraints":[],"sources":[{"uri":"brain://fixture/source.md","revision":"r1","locator":null}],"previous_result_id":null,"next_step":"fixture guidance","conversation":null,"previous_result":null,"source_excerpts":[],"source_snapshots":[]},
            "target":{"created_at":"2026-09-01T00:00:00Z","environment_id":"old-environment","project_id":"old-project"}
        })).unwrap()
    }
    struct Fixture {
        entry: ArtifactEntry,
        original: StartEnvelope,
        current: StartEnvelope,
        raw: String,
        fingerprint: String,
    }
    impl Fixture {
        fn new() -> Self {
            let mut original = envelope();
            original.packet.extra.insert(
                "conversation".into(),
                serde_json::from_str(r#"{"z":1,"a":{"y":"original","b":2},"list":[1,2]}"#).unwrap(),
            );
            let mut current = original.clone();
            current.packet.extra.insert(
                "conversation".into(),
                serde_json::from_str(r#"{"a":{"b":2,"y":"original"},"list":[1,2],"z":1}"#).unwrap(),
            );
            let raw = String::from_utf8(serde_json::to_vec(&original).unwrap()).unwrap();
            let fingerprint = hash(raw.as_bytes());
            let artifact_path = std::env::temp_dir().join(format!(
                "tessera-compat-fixture-{}.json",
                uuid::Uuid::new_v4()
            ));
            let mut f = Self {
                entry: ArtifactEntry {
                    operation_id: current.operation_id.clone(),
                    artifact_path,
                    artifact_sha256: String::new(),
                    provenance: "synthetic original serializer artifact".into(),
                },
                original,
                current,
                raw,
                fingerprint,
            };
            f.artifact(format!(r#"{{"dispatch":{{"envelope":{}}}}}"#, f.raw));
            f
        }
        fn artifact(&mut self, value: String) {
            fs::write(&self.entry.artifact_path, value.as_bytes()).unwrap();
            self.entry.artifact_sha256 = hash(value.as_bytes());
        }
        fn result(&self) -> Result<VerifiedProof> {
            verify(&self.entry, &self.current, &self.fingerprint)
        }
        fn original_bytes(&mut self, raw: String) {
            self.fingerprint = hash(raw.as_bytes());
            self.artifact(format!(r#"{{"dispatch":{{"envelope":{raw}}}}}"#));
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.entry.artifact_path);
        }
    }
    #[test]
    fn exact_original_compact_span_proves_only_object_order_and_survives_backup_removal() {
        let f = Fixture::new();
        assert_ne!(
            hash(&serde_json::to_vec(&f.current).unwrap()),
            f.fingerprint,
            "positive control must actually exercise historical nested object order"
        );
        let proof = f.result().unwrap();
        assert_eq!(proof.original_envelope, f.raw);
        assert_eq!(proof.source_locator, "/dispatch/envelope");
        let proof: VerifiedProof =
            serde_json::from_slice(&serde_json::to_vec(&proof).unwrap()).unwrap();
        fs::remove_file(&f.entry.artifact_path).unwrap();
        verify_retained(&proof, &f.current, &f.fingerprint).unwrap();
    }
    #[test]
    fn semantic_values_identities_sources_and_array_order_are_never_compatible() {
        let f = Fixture::new();
        let proof = f.result().unwrap();
        for n in 0..8 {
            let mut changed = f.current.clone();
            match n {
                0 => changed.packet.next_step = "changed guidance".into(),
                1 => changed.packet.sources[0].revision = Some("changed-source".into()),
                2 => changed
                    .target
                    .insert("environment_id".into(), json!("new-environment"))
                    .map(|_| ())
                    .unwrap(),
                3 => changed.packet.decisions.reverse(),
                4 => {
                    changed.packet.extra.get_mut("conversation").unwrap()["a"]["y"] =
                        json!("changed nested value")
                }
                5 => changed.packet.extra.get_mut("conversation").unwrap()["list"] = json!([2, 1]),
                6 => changed.operation_id = "06000000-0000-4000-8000-000000000099".into(),
                _ => changed.context_revision = "changed-context".into(),
            }
            assert!(
                verify_retained(&proof, &changed, &f.fingerprint).is_err(),
                "change {n} must fail"
            );
        }
    }
    #[test]
    fn pinned_artifact_receipt_and_provenance_are_required() {
        let mut f = Fixture::new();
        f.result().unwrap();
        let good = f.entry.clone();
        f.entry.artifact_sha256 = "0".repeat(64);
        assert!(f.result().is_err());
        f.entry = good.clone();
        f.entry.provenance.clear();
        assert!(f.result().is_err());
        f.entry = good.clone();
        assert!(verify(&f.entry, &f.current, &"0".repeat(64)).is_err());
        fs::write(&f.entry.artifact_path, b"{}").unwrap();
        assert!(f.result().is_err());
    }
    #[test]
    fn duplicate_keys_ambiguous_operation_and_pretty_spans_fail_closed() {
        let mut f = Fixture::new();
        let raw = f.raw.clone();
        f.artifact(format!(r#"{{"dispatch":{{"envelope":{raw}}},"previous_stages":{{"stage":{{"dispatch":{{"envelope":{raw}}}}}}}}}"#));
        assert!(f.result().is_err());
        f.artifact(format!(r#"{{"dispatch":{{"envelope":{raw}}},"other_goals":{{"duplicate":{{"dispatch":{{"envelope":{raw}}}}}}}}}"#));
        assert!(f.result().is_err());
        let duplicate = raw.replace("\"z\":1", "\"z\":1,\"z\":1");
        f.original_bytes(duplicate);
        assert!(f.result().is_err());
        let escaped = raw.replace("\"z\":1", "\"z\":1,\"\\u007a\":1");
        f.original_bytes(escaped);
        assert!(f.result().is_err());
        let pretty = serde_json::to_string_pretty(&f.original).unwrap();
        f.original_bytes(pretty);
        assert!(f.result().is_err());
    }
    #[test]
    fn unsupported_missing_and_unknown_structural_fields_are_losslessly_rejected() {
        let mut f = Fixture::new();
        let mut value = serde_json::to_value(&f.original).unwrap();
        value["unknown_outer"] = json!(true);
        f.original_bytes(serde_json::to_string(&value).unwrap());
        assert!(f.result().is_err());
        let mut value = serde_json::to_value(&f.original).unwrap();
        value["packet"]
            .as_object_mut()
            .unwrap()
            .remove("previous_result_id");
        f.original_bytes(serde_json::to_string(&value).unwrap());
        assert!(f.result().is_err());
        let mut value = serde_json::to_value(&f.original).unwrap();
        value["packet"]["sources"][0]["ignored_source_field"] = json!("must not drop");
        f.original_bytes(serde_json::to_string(&value).unwrap());
        assert!(f.result().is_err());
        let mut value = serde_json::to_value(&f.original).unwrap();
        value["schema"] = json!("ai-brain/v99");
        f.original_bytes(serde_json::to_string(&value).unwrap());
        assert!(f.result().is_err());
        f.current = f.original.clone();
        f.current.packet.extra.insert(
            "unknown_extension".into(),
            json!({"opaque":"not registered"}),
        );
        f.original_bytes(serde_json::to_string(&f.current).unwrap());
        assert!(f.result().is_err());
    }
    #[test]
    fn persisted_current_raw_decode_rejects_unknown_missing_duplicate_before_runner_open() {
        let f = Fixture::new();
        let current = serde_json::to_value(&f.current).unwrap();
        let artifact = serde_json::to_vec(&json!({"dispatch":{"envelope":current}})).unwrap();
        validate_current_artifact(&artifact, &f.current).unwrap();
        let wrapped=serde_json::to_vec(&json!({"schema":"tessera-runtime/routes-v2","state":{"dispatch":{"envelope":current},"t3_routes":{}}})).unwrap();
        validate_current_artifact(&wrapped, &f.current).unwrap();
        let mut unknown = current.clone();
        unknown["unknown_top_level"] = json!(true);
        assert!(validate_current_artifact(
            &serde_json::to_vec(&json!({"dispatch":{"envelope":unknown}})).unwrap(),
            &f.current
        )
        .is_err());
        let mut missing = current;
        missing["packet"]
            .as_object_mut()
            .unwrap()
            .remove("previous_result_id");
        assert!(validate_current_artifact(
            &serde_json::to_vec(&json!({"dispatch":{"envelope":missing}})).unwrap(),
            &f.current
        )
        .is_err());
        let duplicate = String::from_utf8(artifact)
            .unwrap()
            .replace("\"z\":1", "\"z\":1,\"z\":1");
        assert!(validate_current_artifact(duplicate.as_bytes(), &f.current).is_err());
    }
    #[test]
    fn batched_loader_accepts_lossless_noncompatibility_envelopes_and_checks_all_slots() {
        let f = Fixture::new();
        let mut other = f.current.clone();
        other.operation_id = "legacy-non-uuid".into();
        other.target.insert("adapter_option".into(), json!(true));
        other
            .packet
            .extra
            .insert("future_registered_extension".into(), json!({"value": 5}));
        let artifact = serde_json::to_vec(&json!({
            "dispatch":{"envelope":f.current},
            "other_goals":{"second":{"dispatch":{"envelope":other}}}
        }))
        .unwrap();
        validate_current_artifacts(&artifact, &[&f.current, &other]).unwrap();
        assert!(typed(&serde_json::to_vec(&other).unwrap()).is_err());
        let mut changed = other.clone();
        changed.packet.next_step = "changed".into();
        assert!(validate_current_artifacts(&artifact, &[&f.current, &changed]).is_err());
        let duplicated = serde_json::to_vec(&json!({"dispatch":{"envelope":other},
            "other_goals":{"second":{"dispatch":{"envelope":other}}}}))
        .unwrap();
        assert!(validate_current_artifacts(&duplicated, &[&other]).is_err());
    }
    #[test]
    fn changed_raw_numeric_precision_is_not_erased_by_f64_decoding() {
        let mut f = Fixture::new();
        f.original
            .packet
            .extra
            .insert("conversation".into(), json!({"n":1.0}));
        f.current = f.original.clone();
        f.raw = serde_json::to_string(&f.original).unwrap();
        f.fingerprint = hash(f.raw.as_bytes());
        f.artifact(format!(r#"{{"dispatch":{{"envelope":{}}}}}"#, f.raw));
        let proof = f.result().unwrap();
        let pretty =
            serde_json::to_string_pretty(&json!({"dispatch":{"envelope":f.current}})).unwrap();
        validate_current_artifact(pretty.as_bytes(), &f.current).unwrap();
        let changed = pretty.replace("\"n\": 1.0", "\"n\": 1.00000000000000001");
        assert_ne!(pretty, changed);
        assert!(validate_current_artifact(changed.as_bytes(), &f.current).is_err());
        verify_retained(&proof, &f.current, &f.fingerprint).unwrap();
        let scalar = json!({"n":1.0});
        assert_eq!(
            serde_json::from_str::<Value>(r#"{"n":1.00000000000000001}"#).unwrap(),
            scalar,
            "positive control proves the default parser would erase the mutation"
        );
    }
    #[test]
    fn string_contents_cannot_fake_an_operation_span_and_null_slots_are_supported() {
        let mut f = Fixture::new();
        let raw = f.raw.clone();
        let artifact = json!({"dispatch":null,"other_goals":{"goal/~":{"previous_stages":{"old":{"dispatch":{"envelope":"RAW_ENVELOPE"}}}}},"unrelated_text":"envelope dispatch operation_id {}"});
        f.artifact(
            serde_json::to_string(&artifact)
                .unwrap()
                .replace("\"RAW_ENVELOPE\"", &raw),
        );
        let proof = f.result().unwrap();
        assert_eq!(
            proof.source_locator,
            "/other_goals/goal~1~0/previous_stages/old/dispatch/envelope"
        );
        verify_retained(&proof, &f.current, &f.fingerprint).unwrap();
        let mut changed = proof;
        changed.original_envelope.push(' ');
        assert!(verify_retained(&changed, &f.current, &f.fingerprint).is_err());
    }
}
