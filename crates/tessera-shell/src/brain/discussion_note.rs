//! Preparation and create-only writes for an explicitly reviewed Discussion note.
//! Canonical origin and operation receipts are validated independently of UI state.
use super::editor_recovery::MAX_TEXT_BYTES;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Selection {
    pub workspace: Value,
    pub goal_id: String,
    pub conversation_id: String,
    pub path: String,
    pub message_index: usize,
    pub text: String,
}

/// A user-authored note has no conversation identity or assistant provenance.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Target {
    UserAuthored { workspace: Value, goal_id: String },
    Discussion(Selection),
}

impl Target {
    pub fn user_authored(workspace: &Value, goal_id: &str) -> Result<Self, String> {
        uuid(field(workspace, "brain_id")?)?;
        uuid(goal_id)?;
        if workspace["managed"] != true {
            return Err("A writable managed workspace is required to create a note.".into());
        }
        Ok(Self::UserAuthored {
            workspace: workspace.clone(),
            goal_id: goal_id.into(),
        })
    }
    pub fn workspace(&self) -> &Value {
        match self {
            Self::UserAuthored { workspace, .. } => workspace,
            Self::Discussion(selection) => &selection.workspace,
        }
    }
    pub fn goal_id(&self) -> &str {
        match self {
            Self::UserAuthored { goal_id, .. } => goal_id,
            Self::Discussion(selection) => &selection.goal_id,
        }
    }
    pub fn is_user_authored(&self) -> bool {
        matches!(self, Self::UserAuthored { .. })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Origin {
    pub selection: Selection,
    pub source_revision: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct PendingSave {
    pub workspace: Value,
    pub request: Value,
    pub path: String,
    pub revision: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Readback {
    Exact,
    Changed,
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("Missing Discussion note {key}."))
}

fn uuid(value: &str) -> Result<(), String> {
    if Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value) {
        Ok(())
    } else {
        Err("Discussion note identity is invalid. Reload the saved conversation.".into())
    }
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.contains(['\\', '\0', ':'])
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn revision(bytes: &[u8]) -> String {
    format!("sha256:{}", digest(bytes))
}

impl Selection {
    pub fn capture(
        workspace: &Value,
        goal_id: &str,
        conversation: &Value,
        message_index: usize,
    ) -> Result<Self, String> {
        uuid(field(workspace, "brain_id")?)?;
        uuid(goal_id)?;
        let id = field(conversation, "id")?;
        uuid(id)?;
        let path = field(conversation, "path")?;
        if conversation["goal_id"] != goal_id || !valid_path(path) {
            return Err(
                "The saved conversation belongs to another goal or path. Reload it.".into(),
            );
        }
        let message = conversation["messages"]
            .as_array()
            .and_then(|messages| messages.get(message_index))
            .ok_or("The selected saved answer is unavailable. Reload the conversation.")?;
        let text = field(message, "text")?;
        if message["role"] != "assistant" || text.trim().is_empty() {
            return Err("Choose a completed saved assistant answer.".into());
        }
        Ok(Self {
            workspace: workspace.clone(),
            goal_id: goal_id.into(),
            conversation_id: id.into(),
            path: path.into(),
            message_index,
            text: text.into(),
        })
    }
}

fn snapshot_text(snapshot: &Value, workspace: &Value, path: &str) -> Result<String, String> {
    if snapshot["schema"] != "ai-brain/v1"
        || snapshot["brain_id"] != workspace["brain_id"]
        || snapshot["path"] != path
        || !valid_path(path)
    {
        return Err("The source snapshot belongs to another workspace or path. Reload it.".into());
    }
    let encoded = field(snapshot, "content_base64")?;
    if encoded.len() > MAX_TEXT_BYTES.div_ceil(3) * 4 {
        return Err(
            "The source exceeds the existing 8 MiB editor limit. Nothing was saved.".into(),
        );
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| "The source snapshot has invalid encoded bytes.")?;
    if bytes.len() > MAX_TEXT_BYTES || field(snapshot, "revision")? != revision(&bytes) {
        return Err("The source revision does not match its exact bytes. Reload it.".into());
    }
    String::from_utf8(bytes).map_err(|_| "The saved source is not valid UTF-8.".into())
}

fn metadata(text: &str) -> Result<Value, String> {
    let mut lines = text.trim_start_matches('\u{feff}').split_inclusive('\n');
    if lines.next().map(str::trim_end) != Some("---") {
        return Err("Canonical conversation metadata is missing. Reload the conversation.".into());
    }
    let mut yaml = String::new();
    for line in lines {
        if line.trim_end() == "---" {
            // Deserialize through YAML Value to reject duplicate mapping keys.
            let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml)
                .map_err(|_| "Canonical conversation metadata is invalid.")?;
            return serde_json::to_value(parsed)
                .map_err(|_| "Canonical conversation metadata is not a valid object.".into());
        }
        yaml.push_str(line);
    }
    Err("Canonical conversation metadata is incomplete. Reload the conversation.".into())
}

pub(super) fn prepare(selection: &Selection, snapshot: &Value) -> Result<Origin, String> {
    let text = snapshot_text(snapshot, &selection.workspace, &selection.path)?;
    let record = metadata(&text)?;
    let message = record["messages"]
        .as_array()
        .and_then(|messages| messages.get(selection.message_index));
    if record["schema"] != "ai-brain/v1"
        || record["record_type"] != "conversation"
        || record["brain_id"] != selection.workspace["brain_id"]
        || record["id"] != selection.conversation_id
        || record["goal_id"] != selection.goal_id
        || record["path"] != selection.path
        || message.is_none_or(|m| m["role"] != "assistant" || m["text"] != selection.text)
    {
        return Err("The canonical conversation does not match this saved answer. Its projection may be pending or its history changed; reload before saving a note.".into());
    }
    Ok(Origin {
        selection: selection.clone(),
        source_revision: field(snapshot, "revision")?.into(),
    })
}

fn truncate_utf8(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

pub(super) fn suggested_title(origin: &Origin) -> String {
    let title = origin
        .selection
        .text
        .lines()
        .map(str::trim)
        .map(|line| line.trim_start_matches('#').trim())
        .find(|line| !line.is_empty())
        .unwrap_or("Discussion note");
    truncate_utf8(title, 512).into()
}

fn basename(filename: &str) -> Result<(), String> {
    if filename.len() > 255
        || filename.starts_with(['.', '_'])
        || filename.contains('/')
        || !valid_path(filename)
        || !filename.ends_with(".md")
        || filename.trim() != filename
        || filename.chars().any(char::is_control)
    {
        return Err("Use a .md filename of at most 255 bytes, without folders or internal names starting with a dot or underscore.".into());
    }
    Ok(())
}

pub(super) fn default_filename(title: &str, operation_id: &str) -> Result<String, String> {
    filename_with_fallback(title, operation_id, "discussion-note")
}

fn filename_with_fallback(
    title: &str,
    operation_id: &str,
    fallback: &str,
) -> Result<String, String> {
    uuid(operation_id)?;
    let suffix = format!("-{operation_id}.md");
    let mut slug = String::new();
    for ch in title.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            slug.push(ch);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = truncate_utf8(&slug, 255 - suffix.len()).trim_matches('-');
    let slug = if slug.is_empty() { fallback } else { slug };
    let filename = format!("{slug}{suffix}");
    basename(&filename)?;
    Ok(filename)
}

// Percent-encode Markdown destination punctuation and non-ASCII UTF-8 bytes.
// Keep path separators so the relative link points to the captured source.
fn link_path(path: &str) -> String {
    let mut escaped = String::new();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            escaped.push(byte as char);
        } else {
            use std::fmt::Write;
            let _ = write!(escaped, "%{byte:02X}");
        }
    }
    escaped
}

#[cfg(test)]
pub(super) fn freeze(
    origin: &Origin,
    title: &str,
    body: &str,
    filename: Option<&str>,
    operation_id: &str,
) -> Result<PendingSave, String> {
    freeze_target(
        &Target::Discussion(origin.selection.clone()),
        Some(origin),
        title,
        body,
        filename,
        operation_id,
    )
}

pub(super) fn freeze_target(
    target: &Target,
    origin: Option<&Origin>,
    title: &str,
    body: &str,
    filename: Option<&str>,
    operation_id: &str,
) -> Result<PendingSave, String> {
    uuid(operation_id)?;
    uuid(field(target.workspace(), "brain_id")?)?;
    uuid(target.goal_id())?;
    let discussion_origin = match target {
        Target::Discussion(selection) => {
            let origin = origin.filter(|origin| origin.selection == *selection)
                .ok_or("The Discussion note needs its matching canonical source proof. Reload the saved answer.")?;
            if body.trim().is_empty() {
                return Err("Add a note body before saving.".into());
            }
            Some(origin)
        }
        Target::UserAuthored { .. } => {
            if origin.is_some() {
                return Err("A user-authored note must not carry assistant provenance.".into());
            }
            None
        }
    };
    if title.trim().is_empty() || title.len() > 512 || title.chars().any(char::is_control) {
        return Err("Add a nonempty, single-line title of at most 512 bytes.".into());
    }
    if body.len() > MAX_TEXT_BYTES {
        return Err(
            "The complete note exceeds 8 MiB. Your review is retained; shorten it before saving."
                .into(),
        );
    }
    let path: String = match filename {
        Some(name) if !name.is_empty() => name.into(),
        _ if target.is_user_authored() => filename_with_fallback(title, operation_id, "note")?,
        _ => default_filename(title, operation_id)?,
    };
    basename(&path)?;
    let mut frontmatter = json!({ "type":"Note", "goal_id":target.goal_id() });
    let attribution = if let Some(origin) = discussion_origin {
        let edited = body != origin.selection.text;
        frontmatter["verification"] = json!("unverified");
        frontmatter["assistant_origin"] = json!({
            "conversation_id":origin.selection.conversation_id,
            "conversation_path":origin.selection.path,
            "source_revision":origin.source_revision,
            "message_index":origin.selection.message_index,
            "original_text_sha256":digest(origin.selection.text.as_bytes()),
            "body_edited":edited
        });
        format!(
            "Derived from an assistant answer in [Discussion](<{}>); unverified.{}\n\n",
            link_path(&origin.selection.path),
            if edited { " Edited before saving." } else { "" }
        )
    } else {
        String::new()
    };
    let yaml = serde_yaml::to_string(&frontmatter)
        .map_err(|_| "Could not serialize the note; your review is retained.")?;
    let markdown = format!("---\n{yaml}---\n# {title}\n\n{attribution}{body}");
    if markdown.len() > MAX_TEXT_BYTES {
        return Err("The complete note, including its title and metadata, exceeds 8 MiB. Your review is retained; shorten it before saving.".into());
    }
    let revision = revision(markdown.as_bytes());
    Ok(PendingSave {
        workspace: target.workspace().clone(),
        request: json!({"op":"source_write","request":{
            "schema":"ai-brain/v1", "operation_id":operation_id,
            "brain_id":target.workspace()["brain_id"], "path":path,
            "expected_revision":null, "content_base64":STANDARD.encode(markdown.as_bytes())
        }}),
        path,
        revision,
    })
}

pub(super) fn validate_receipt(pending: &PendingSave, receipt: &Value) -> Result<(), String> {
    if receipt["operation_id"] != pending.request["request"]["operation_id"]
        || receipt["path"] != pending.path
        || receipt.get("previous_revision") != Some(&Value::Null)
        || receipt["revision"] != pending.revision
        || receipt["outcome"] != "written"
    {
        return Err("The save receipt does not match this exact note. Keep the review and retry the same save; no new note was requested.".into());
    }
    Ok(())
}

pub(super) fn validate_readback(
    pending: &PendingSave,
    snapshot: &Value,
) -> Result<Readback, String> {
    let text = snapshot_text(snapshot, &pending.workspace, &pending.path)?;
    if snapshot["revision"] != pending.revision {
        return Ok(Readback::Changed);
    }
    if STANDARD.encode(text.as_bytes()) != pending.request["request"]["content_base64"] {
        return Err(
            "The saved note bytes do not match the acknowledged save. Reload the source.".into(),
        );
    }
    Ok(Readback::Exact)
}

pub(super) fn validate_filename_conflict(
    pending: &PendingSave,
    view: &Value,
) -> Result<(), String> {
    let conflict = &view["conflict"];
    let observed_revision = conflict["current_revision"].as_str().unwrap_or("");
    let valid_revision = observed_revision.len() == 71
        && observed_revision.starts_with("sha256:")
        && observed_revision[7..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
    if conflict["conflict_id"] != pending.request["request"]["operation_id"]
        || conflict["path"] != pending.path
        || conflict.get("expected_revision") != Some(&Value::Null)
        || conflict["reason"] != "stale_revision"
        || !valid_revision
        || view.get("base") != Some(&Value::Null)
        || validate_readback(pending, &view["proposed"])? != Readback::Exact
    {
        return Err("The filename conflict does not identify this exact save. Retain the review and retry the same operation.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const BRAIN: &str = "01000000-0000-4000-8000-000000000001";
    const GOAL: &str = "02000000-0000-4000-8000-000000000001";
    const CONVERSATION: &str = "03000000-0000-4000-8000-000000000001";
    const OPERATION: &str = "04000000-0000-4000-8000-000000000001";

    fn snapshot(path: &str, bytes: &[u8]) -> Value {
        json!({"schema":"ai-brain/v1", "brain_id":BRAIN, "path":path,
            "revision":revision(bytes), "content_base64":STANDARD.encode(bytes),
            "media_type":"text/markdown"})
    }

    fn fixture() -> (Selection, Value) {
        let conversation = json!({"id":CONVERSATION,"goal_id":GOAL,
            "path":format!("records/conversation-{CONVERSATION}.md"),
            "status":"completed", "partial":"not a saved answer",
            "messages":[{"role":"user","text":"Question"},
                {"role":"assistant","text":"A saved answer: α\r\n\n[[source]]"}]});
        let workspace = json!({"brain_id":BRAIN,"root":"/fixture/brain","state":"/fixture/state"});
        let selection = Selection::capture(&workspace, GOAL, &conversation, 1).unwrap();
        let mut record = conversation;
        record["schema"] = json!("ai-brain/v1");
        record["record_type"] = json!("conversation");
        record["brain_id"] = json!(BRAIN);
        let bytes = format!(
            "---\n{}---\n# Goal conversation\n",
            serde_yaml::to_string(&record).unwrap()
        );
        let source = snapshot(&selection.path, bytes.as_bytes());
        (selection, source)
    }

    fn origin() -> Origin {
        let (selection, source) = fixture();
        prepare(&selection, &source).unwrap()
    }

    fn pending() -> PendingSave {
        let origin = origin();
        freeze(
            &origin,
            "Reviewed title",
            &origin.selection.text,
            None,
            OPERATION,
        )
        .unwrap()
    }

    fn saved_snapshot(pending: &PendingSave) -> Value {
        let bytes = STANDARD
            .decode(
                pending.request["request"]["content_base64"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
        snapshot(&pending.path, &bytes)
    }

    fn receipt(pending: &PendingSave) -> Value {
        json!({"operation_id":OPERATION,"path":pending.path,
            "previous_revision":null,"revision":pending.revision,"outcome":"written"})
    }

    #[test]
    fn new_note_empty_and_exact_body_have_only_user_metadata() {
        let workspace = json!({"brain_id":BRAIN,"managed":true});
        let target = Target::user_authored(&workspace, GOAL).unwrap();
        for body in ["", "  λ\r\n\n[[source]]\n"] {
            let pending = freeze_target(&target, None, "My note", body, None, OPERATION).unwrap();
            let bytes = STANDARD
                .decode(
                    pending.request["request"]["content_base64"]
                        .as_str()
                        .unwrap(),
                )
                .unwrap();
            let markdown = String::from_utf8(bytes).unwrap();
            let (yaml, content) = markdown
                .strip_prefix("---\n")
                .unwrap()
                .split_once("---\n")
                .unwrap();
            let metadata: Value = serde_yaml::from_str(yaml).unwrap();
            assert_eq!(metadata, json!({"type":"Note","goal_id":GOAL}));
            assert_eq!(content, format!("# My note\n\n{body}"));
            assert!(pending.request["request"]["expected_revision"].is_null());
            assert!(pending.request.get("base").is_none());
        }
        assert!(Target::user_authored(&workspace, "").is_err());
        assert!(Target::user_authored(&json!({"brain_id":BRAIN,"managed":false}), GOAL).is_err());
    }

    #[test]
    fn new_note_does_not_weaken_discussion_origin_or_body_requirements() {
        let origin = origin();
        let user = Target::user_authored(&json!({"brain_id":BRAIN,"managed":true}), GOAL).unwrap();
        assert!(freeze_target(&user, Some(&origin), "Title", "", None, OPERATION).is_err());
        let discussion = Target::Discussion(origin.selection.clone());
        assert!(freeze_target(&discussion, None, "Title", "Body", None, OPERATION).is_err());
        assert!(freeze_target(
            &discussion,
            Some(&origin),
            "Title",
            " \r\n",
            None,
            OPERATION
        )
        .is_err());
        let mut wrong = origin.clone();
        wrong.selection.text.push('!');
        assert!(
            freeze_target(&discussion, Some(&wrong), "Title", "Body", None, OPERATION).is_err()
        );
    }

    #[test]
    fn new_note_complete_serialized_limit_includes_title_and_metadata() {
        let target =
            Target::user_authored(&json!({"brain_id":BRAIN,"managed":true}), GOAL).unwrap();
        let empty = freeze_target(&target, None, "Title", "", None, OPERATION).unwrap();
        let overhead = STANDARD
            .decode(empty.request["request"]["content_base64"].as_str().unwrap())
            .unwrap()
            .len();
        let mut body = "x".repeat(MAX_TEXT_BYTES - overhead);
        assert!(freeze_target(&target, None, "Title", &body, None, OPERATION).is_ok());
        body.push('x');
        assert!(freeze_target(&target, None, "Title", &body, None, OPERATION).is_err());
        for title in ["", " ", "two\nlines", &"λ".repeat(257)] {
            assert!(freeze_target(&target, None, title, "", None, OPERATION).is_err());
        }
        for filename in ["_reserved.md", "../note.md", ".hidden.md"] {
            assert!(freeze_target(&target, None, "Title", "", Some(filename), OPERATION).is_err());
        }
    }

    #[test]
    fn discussion_note_capture_excludes_user_partial_and_wrong_goal() {
        let (selection, _) = fixture();
        let mut conversation = json!({"id":CONVERSATION,"goal_id":GOAL,"path":selection.path,
            "partial":"partial","messages":[{"role":"user","text":"question"}]});
        assert!(Selection::capture(&selection.workspace, GOAL, &conversation, 0).is_err());
        assert!(Selection::capture(&selection.workspace, GOAL, &conversation, 1).is_err());
        conversation["messages"][0] = json!({"role":"assistant","text":"complete"});
        assert!(Selection::capture(&selection.workspace, GOAL, &conversation, 0).is_ok());
        conversation["goal_id"] = json!(BRAIN);
        assert!(Selection::capture(&selection.workspace, GOAL, &conversation, 0).is_err());
    }

    #[test]
    fn discussion_note_requires_canonical_row_and_actual_revision() {
        let (selection, source) = fixture();
        let prepared = prepare(&selection, &source).unwrap();
        assert_eq!(prepared.source_revision, source["revision"]);
        for key in ["schema", "brain_id", "path", "revision"] {
            let mut wrong = source.clone();
            wrong[key] = json!("wrong");
            assert!(prepare(&selection, &wrong).is_err(), "{key}");
        }
        let bytes = STANDARD
            .decode(source["content_base64"].as_str().unwrap())
            .unwrap();
        let original = metadata(std::str::from_utf8(&bytes).unwrap()).unwrap();
        for key in ["schema", "record_type", "brain_id", "id", "goal_id", "path"] {
            let mut wrong = original.clone();
            wrong[key] = json!("wrong");
            let text = format!("---\n{}---\n", serde_yaml::to_string(&wrong).unwrap());
            assert!(
                prepare(&selection, &snapshot(&selection.path, text.as_bytes())).is_err(),
                "{key}"
            );
        }
        for changed in [
            json!({"role":"assistant","text":"earlier projection"}),
            json!({"role":"user","text":selection.text}),
        ] {
            let mut wrong = original.clone();
            wrong["messages"][1] = changed;
            let text = format!("---\n{}---\n", serde_yaml::to_string(&wrong).unwrap());
            assert!(prepare(&selection, &snapshot(&selection.path, text.as_bytes())).is_err());
        }
    }

    #[test]
    fn discussion_note_malformed_source_never_falls_back_to_journal() {
        let (selection, source) = fixture();
        let mut bad = source.clone();
        bad["content_base64"] = json!("!!!!");
        assert!(prepare(&selection, &bad).is_err());
        for bytes in [
            &b"\xff"[..],
            &b"# No metadata"[..],
            &b"---\na: 1\na: 2\n---\n"[..],
        ] {
            assert!(prepare(&selection, &snapshot(&selection.path, bytes)).is_err());
        }
        let source_bytes = STANDARD
            .decode(source["content_base64"].as_str().unwrap())
            .unwrap();
        let mut bom = vec![0xef, 0xbb, 0xbf];
        bom.extend(source_bytes);
        assert!(prepare(&selection, &snapshot(&selection.path, &bom)).is_ok());
    }

    #[test]
    fn discussion_note_serialization_preserves_ordinary_authority_and_original_body() {
        let origin = origin();
        let pending = pending();
        let bytes = STANDARD
            .decode(
                pending.request["request"]["content_base64"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let meta = metadata(&text).unwrap();
        assert_eq!(meta["type"], "Note");
        assert_eq!(meta["goal_id"], GOAL);
        assert_eq!(meta["verification"], "unverified");
        assert!(meta.get("record_type").is_none());
        assert!(meta.get("title").is_none());
        assert_eq!(
            meta["assistant_origin"]["source_revision"],
            origin.source_revision
        );
        assert_eq!(
            meta["assistant_origin"]["original_text_sha256"],
            digest(origin.selection.text.as_bytes())
        );
        assert_eq!(meta["assistant_origin"]["body_edited"], false);
        assert!(text.contains("# Reviewed title\n"));
        assert!(text.ends_with(&origin.selection.text));
        assert!(pending.request.get("base").is_none());
        assert_eq!(pending.request["request"]["expected_revision"], Value::Null);
    }

    #[test]
    fn discussion_note_edited_body_is_distinct_from_frozen_provenance() {
        let origin = origin();
        let body = "---\nverification: verified\n---\n# Edited\r\nשלום";
        let pending = freeze(
            &origin,
            "A: quoted title",
            body,
            Some("my-note.md"),
            OPERATION,
        )
        .unwrap();
        let text =
            snapshot_text(&saved_snapshot(&pending), &pending.workspace, &pending.path).unwrap();
        let meta = metadata(&text).unwrap();
        assert_eq!(meta["verification"], "unverified");
        assert_eq!(meta["assistant_origin"]["body_edited"], true);
        assert_eq!(
            meta["assistant_origin"]["original_text_sha256"],
            digest(origin.selection.text.as_bytes())
        );
        assert!(text.contains("Edited before saving."));
        assert!(text.ends_with(body));
    }

    #[test]
    fn discussion_note_filename_accounts_for_suffix_and_utf8() {
        let name = default_filename(&"界".repeat(170), OPERATION).unwrap();
        assert!(name.len() <= 255);
        assert!(name.ends_with(&format!("-{OPERATION}.md")));
        assert!(name.starts_with('界'));
        assert_eq!(
            default_filename("?!", OPERATION).unwrap(),
            format!("discussion-note-{OPERATION}.md")
        );
        for name in [
            "../evil.md",
            "notes/new.md",
            "_index.md",
            ".tessera.md",
            "a\\b.md",
            "a:b.md",
            "note.txt",
            "\n.md",
            " note.md",
            "",
        ] {
            assert!(basename(name).is_err(), "{name:?}");
        }
        assert!(basename(&format!("{}.md", "a".repeat(252))).is_ok());
        assert!(basename(&format!("{}.md", "a".repeat(253))).is_err());
        assert!(default_filename("Title", "not-an-operation").is_err());
    }

    #[test]
    fn discussion_note_link_encodes_destination_punctuation() {
        assert_eq!(
            link_path("records/a #](x).md"),
            "records/a%20%23%5D%28x%29.md"
        );
    }

    #[test]
    fn discussion_note_complete_serialization_limit_includes_metadata() {
        let origin = origin();
        let minimal = freeze(&origin, "Title", "x", Some("note.md"), OPERATION).unwrap();
        let overhead = STANDARD
            .decode(
                minimal.request["request"]["content_base64"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap()
            .len()
            - 1;
        let mut body = "x".repeat(MAX_TEXT_BYTES - overhead);
        let accepted = freeze(&origin, "Title", &body, Some("note.md"), OPERATION).unwrap();
        assert_eq!(
            STANDARD
                .decode(
                    accepted.request["request"]["content_base64"]
                        .as_str()
                        .unwrap()
                )
                .unwrap()
                .len(),
            MAX_TEXT_BYTES
        );
        body.push('x');
        let original_len = body.len();
        assert!(freeze(&origin, "Title", &body, Some("note.md"), OPERATION).is_err());
        assert_eq!(body.len(), original_len);
    }

    #[test]
    fn discussion_note_title_validation_and_suggestion_keep_utf8() {
        let mut origin = origin();
        origin.selection.text = format!("\n## {}", "界".repeat(200));
        let title = suggested_title(&origin);
        assert_eq!(title.len(), 510);
        for invalid in ["", "  ", "line\nbreak", "tab\ttitle", "\0"] {
            assert!(freeze(&origin, invalid, "Body", None, OPERATION).is_err());
        }
        assert!(freeze(&origin, &"a".repeat(513), "Body", None, OPERATION).is_err());
    }

    #[test]
    fn discussion_note_retry_request_is_exact_and_receipt_identity_is_required() {
        let origin = origin();
        let pending = pending();
        assert_eq!(
            pending,
            freeze(
                &origin,
                "Reviewed title",
                &origin.selection.text,
                None,
                OPERATION
            )
            .unwrap()
        );
        let valid = receipt(&pending);
        validate_receipt(&pending, &valid).unwrap();
        for key in [
            "operation_id",
            "path",
            "previous_revision",
            "revision",
            "outcome",
        ] {
            let mut wrong = valid.clone();
            wrong[key] = json!("wrong");
            assert!(validate_receipt(&pending, &wrong).is_err(), "{key}");
            wrong.as_object_mut().unwrap().remove(key);
            assert!(validate_receipt(&pending, &wrong).is_err(), "missing {key}");
        }
    }

    #[test]
    fn discussion_note_readback_distinguishes_changed_content_without_recreating() {
        let pending = pending();
        let unchanged_pending = pending.clone();
        let source = saved_snapshot(&pending);
        assert_eq!(
            validate_readback(&pending, &source).unwrap(),
            Readback::Exact
        );
        let changed = snapshot(&pending.path, b"# Later independent edit\n");
        assert_eq!(
            validate_readback(&pending, &changed).unwrap(),
            Readback::Changed
        );
        assert!(validate_readback(&pending, &Value::Null).is_err());
        for key in ["schema", "brain_id", "path", "revision", "content_base64"] {
            let mut wrong = source.clone();
            wrong[key] = json!("wrong");
            assert!(validate_readback(&pending, &wrong).is_err(), "{key}");
        }
        assert_eq!(pending, unchanged_pending);
    }

    #[test]
    fn discussion_note_only_exact_filename_conflict_permits_new_path() {
        let pending = pending();
        let view = json!({"base":null,"current":snapshot(&pending.path,b"occupied"),
            "proposed":saved_snapshot(&pending),"conflict":{
                "conflict_id":OPERATION,"path":pending.path,"expected_revision":null,
                "current_revision":revision(b"occupied"),"reason":"stale_revision"}});
        validate_filename_conflict(&pending, &view).unwrap();
        for key in [
            "conflict_id",
            "path",
            "expected_revision",
            "current_revision",
            "reason",
        ] {
            let mut wrong = view.clone();
            wrong["conflict"][key] = json!("wrong");
            assert!(
                validate_filename_conflict(&pending, &wrong).is_err(),
                "{key}"
            );
        }
        let mut wrong = view.clone();
        wrong["proposed"] = snapshot(&pending.path, b"unrelated proposed content");
        assert!(validate_filename_conflict(&pending, &wrong).is_err());
        let mut unmanaged = view;
        unmanaged["conflict"]["reason"] = json!("unmanaged_writers");
        assert!(validate_filename_conflict(&pending, &unmanaged).is_err());
    }
}
