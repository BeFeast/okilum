// Exact saved Source -> provisional Context citation. No storage or backend calls.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Component, Path};

pub const MAX_SOURCE: usize = 8192;
/// Matches production Context selected_citations source-read admission.
pub const MAX_CONTEXT_SOURCE: usize = 1024 * 1024;
pub const MAX_FORM: usize = 64 * 1024;
fn string(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', '\0'])
        && !Path::new(path).is_absolute()
        && Path::new(path)
            .components()
            .all(|p| matches!(p, Component::Normal(n) if !n.to_string_lossy().starts_with('.')))
}
fn delimiter(line: &str) -> bool {
    line.trim_end_matches(['\r', '\n', ' ', '\t']) == "---"
}
fn metadata(raw: &str) -> Result<(Value, usize), String> {
    let lines: Vec<_> = raw.split_inclusive('\n').collect();
    if lines
        .first()
        .is_some_and(|l| delimiter(l.strip_prefix('\u{feff}').unwrap_or(l)))
    {
        let end = lines
            .iter()
            .enumerate()
            .skip(1)
            .find_map(|(i, l)| delimiter(l).then_some(i))
            .ok_or("Incomplete frontmatter. Inspect Source before using it in Context.")?;
        let parsed: serde_yaml::Value =
            serde_yaml::from_str(&lines[1..end].concat()).map_err(|_| {
                "Invalid or ambiguous frontmatter. Inspect Source before using it in Context."
            })?;
        let metadata =
            serde_json::to_value(parsed).map_err(|_| "Unsupported frontmatter values.")?;
        if !metadata.is_object() {
            return Err("Frontmatter must be a mapping.".into());
        }
        return Ok((metadata, lines[..=end].iter().map(|l| l.len()).sum()));
    }
    Ok((json!({}), 0))
}
fn bounded_field(v: &Value, keys: &[&str]) -> Value {
    keys.iter()
        .find_map(|k| v[*k].as_str().filter(|s| s.len() <= 256))
        .map_or(Value::Null, |s| json!(s))
}
pub fn validate_scope(scope: &Value, goal: &str, path: &str) -> Result<(), String> {
    if scope["goal_id"] != goal || !matches!(scope["mode"].as_str(), Some("goal" | "project")) {
        return Err("Context scope belongs to another goal or is unsupported.".into());
    }
    if let Some(prefix) = scope["path_prefix"].as_str() {
        let prefix = prefix.trim_end_matches('/');
        if !valid_path(prefix) || !(path == prefix || path.starts_with(&format!("{prefix}/"))) {
            return Err("This note is outside the retained Context folder scope.".into());
        }
    } else if !scope["path_prefix"].is_null() {
        return Err("Invalid Context folder scope.".into());
    }
    for key in ["include_paths", "exclude_paths"] {
        if let Some(paths) = scope[key].as_array() {
            if paths.len() > 100 || paths.iter().any(|p| !p.as_str().is_some_and(valid_path)) {
                return Err("Invalid retained Context path restrictions.".into());
            }
        } else if !scope[key].is_null() {
            return Err("Invalid retained Context path restrictions.".into());
        }
    }
    if scope["include_paths"]
        .as_array()
        .is_some_and(|p| !p.is_empty() && !p.iter().any(|p| p == path))
        || scope["exclude_paths"]
            .as_array()
            .is_some_and(|p| p.iter().any(|p| p == path))
    {
        return Err("This note is excluded by the retained Context scope.".into());
    }
    Ok(())
}
/// Capture only the saved observation's ownership and historical provenance.
/// Inactive links remain evidence; current provider connectivity is irrelevant.
pub fn observation_history(
    source: &Value,
    workspace: &Value,
    goal: &str,
    view: &Value,
) -> Result<Value, String> {
    let records = string(&workspace["records_dir"]);
    let path = string(&source["path"]);
    if !path.starts_with(&format!("{records}/")) {
        return Ok(Value::Null);
    }
    let error = "Saved observation history is missing, contradictory or needs recovery.";
    let id = path
        .strip_prefix(&format!("{records}/maestro-observation-"))
        .and_then(|s| s.strip_suffix(".md"))
        .ok_or(error)?;
    if uuid::Uuid::parse_str(id).is_err()
        || view["schema"] != "tessera-maestro-observation/v1"
        || view["goal_id"] != goal
        || view["recovery_required"] != false
        || view["source_paths"]["observations"][id] != path
    {
        return Err(error.into());
    }
    let history = view["history"].as_array().ok_or(error)?;
    let matches: Vec<_> = history
        .iter()
        .filter(|link| {
            link["observation_ids"]
                .as_array()
                .is_some_and(|ids| ids.iter().any(|value| value == id))
        })
        .collect();
    if matches.len() != 1 {
        return Err(error.into());
    }
    let link = matches[0];
    let ids = link["observation_ids"].as_array().ok_or(error)?;
    if link["goal_id"] != goal
        || uuid::Uuid::parse_str(string(&link["id"])).is_err()
        || history
            .iter()
            .filter(|other| other["id"] == link["id"])
            .count()
            != 1
        || ids.iter().filter(|value| *value == id).count() != 1
        || link["active"].as_bool().is_none()
        || link["issue_number"].as_u64().is_none_or(|n| n == 0)
        || ["project_id", "project_name", "repo"]
            .iter()
            .any(|key| string(&link[*key]).is_empty())
        || ["base_url", "instance_id"]
            .iter()
            .any(|key| string(&link["instance"][*key]).is_empty())
    {
        return Err(error.into());
    }
    Ok(
        json!({"id":id,"path":path,"link_id":link["id"],"goal_id":goal,
        "instance":link["instance"],"project_id":link["project_id"],"project_name":link["project_name"],
        "repo":link["repo"],"issue_number":link["issue_number"],"active":link["active"]}),
    )
}

fn validate_observation(
    m: &Value,
    source: &tessera_core::source::SourceSnapshot,
    workspace: &Value,
    goal: &str,
    history: &Value,
) -> Result<(), String> {
    use tessera_core::maestro_observation::Observation;
    use time::{format_description::well_known::Rfc3339, OffsetDateTime};
    let error = "This Source is not a saved observation owned by this goal's Maestro history.";
    if ["review", "decision_receipt", "execution_status"]
        .iter()
        .any(|key| m.get(*key).is_some())
        || m["schema"] != "ai-brain/v1"
        || m["record_type"] != "maestro-observation"
        || m["brain_id"] != source.brain_id
    {
        return Err(error.into());
    }
    let observation: Observation = serde_json::from_value(m.clone()).map_err(|_| error)?;
    if [&observation.id, &observation.link_id, &observation.goal_id]
        .iter()
        .any(|id| uuid::Uuid::parse_str(id).is_err())
        || observation.goal_id != goal
        || observation.verification != "unverified"
        || [&observation.observed_at, &observation.remote_at]
            .iter()
            .any(|at| OffsetDateTime::parse(at, &Rfc3339).is_err())
        || source.path
            != format!(
                "{}/maestro-observation-{}.md",
                string(&workspace["records_dir"]),
                observation.id
            )
        || history["id"] != observation.id
        || history["link_id"] != observation.link_id
        || history["goal_id"] != goal
        || history["path"] != source.path
        || history["issue_number"] != observation.issue.number
    {
        return Err(error.into());
    }
    Ok(())
}

pub fn citation_with_history(
    snapshot: &Value,
    workspace: &Value,
    goal: &str,
    scope: &Value,
    history: &Value,
) -> Result<Value, String> {
    if history.is_null() {
        citation(snapshot, workspace, goal, scope)
    } else {
        citation_impl(snapshot, workspace, goal, scope, history, None)
    }
}
pub fn citation(
    snapshot: &Value,
    workspace: &Value,
    goal: &str,
    scope: &Value,
) -> Result<Value, String> {
    citation_impl(snapshot, workspace, goal, scope, &Value::Null, None)
}
/// Exact complete source lines touched by a nonempty half-open byte selection.
/// The caller previews any boundary expansion before explicitly staging it.
pub fn selected_lines(
    snapshot: &Value,
    workspace: &Value,
    goal: &str,
    scope: &Value,
    selection: std::ops::Range<usize>,
) -> Result<Value, String> {
    citation_impl(
        snapshot,
        workspace,
        goal,
        scope,
        &Value::Null,
        Some(selection),
    )
}
fn citation_impl(
    snapshot: &Value,
    workspace: &Value,
    goal: &str,
    scope: &Value,
    history: &Value,
    selection: Option<std::ops::Range<usize>>,
) -> Result<Value, String> {
    let source: tessera_core::source::SourceSnapshot = serde_json::from_value(snapshot.clone())
        .map_err(|_| "Choose a saved Markdown Source note first.")?;
    if source.schema != "ai-brain/v1"
        || source.media_type != "text/markdown"
        || workspace["managed"] != true
        || workspace["brain_id"] != source.brain_id
        || uuid::Uuid::parse_str(goal).is_err()
        || !valid_path(&source.path)
        || !Path::new(&source.path)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("md"))
    {
        return Err("Source ownership, path or Markdown type is unavailable.".into());
    }
    let records = string(&workspace["records_dir"]);
    if !valid_path(records)
        || source.path == records
        || (source.path.starts_with(&format!("{records}/")) && history.is_null())
    {
        return Err("Operational records use their existing Context actions. Open Context to select a passage.".into());
    }
    let (limit, size_error) = if selection.is_some() {
        (
            MAX_CONTEXT_SOURCE,
            "This saved note exceeds the 1 MiB Context source limit. Choose a smaller note.",
        )
    } else {
        (
            MAX_SOURCE,
            "The complete note exceeds 8 KiB. Open Context search to select a passage.",
        )
    };
    if source.content_base64.len() > limit.div_ceil(3) * 4 {
        return Err(size_error.into());
    }
    let bytes = STANDARD
        .decode(&source.content_base64)
        .map_err(|_| "Invalid Source bytes.")?;
    if bytes.len() > limit {
        return Err(size_error.into());
    }
    if source.revision != format!("sha256:{}", sha(&bytes)) {
        return Err("Source revision does not match its bytes. Reload and inspect Source.".into());
    }
    let raw =
        String::from_utf8(bytes).map_err(|_| "Source is not UTF-8; its bytes remain unchanged.")?;
    let (m, body) = metadata(&raw)?;
    if raw[body..].trim_start_matches('\u{feff}').trim().is_empty() {
        return Err("This note has no nonempty text to include.".into());
    }
    if !history.is_null() {
        validate_observation(&m, &source, workspace, goal, history)?;
    } else if m.get("record_type").is_some()
        || m.get("type")
            .is_some_and(|v| !v.as_str().is_some_and(|s| s.eq_ignore_ascii_case("note")))
    {
        return Err(
            "This record uses a specialized Context flow. Open Context to select a passage.".into(),
        );
    }
    if m.get("goal_id")
        .is_some_and(|owner| owner.as_str() != Some(goal))
    {
        return Err("This note belongs to another goal.".into());
    }
    validate_scope(scope, goal, &source.path)?;
    let verification = bounded_field(&m, &["verification"]);
    let metadata = json!({"record_type":bounded_field(&m,&["record_type","type"]),"status":bounded_field(&m,&["status"]),"verification":if verification.is_null(){bounded_field(&m["outcome"],&["verification"])}else{verification},"observed_at":bounded_field(&m,&["observed_at","received_at","created_at","created","date","updated"]),"owner_goal_id":bounded_field(&m,&["goal_id"])});
    let (start_line, end_line, excerpt) = if let Some(range) = selection {
        if range.start >= range.end
            || range.end > raw.len()
            || !raw.is_char_boundary(range.start)
            || !raw.is_char_boundary(range.end)
        {
            return Err(
                "Select nonempty text within the saved Source before using selected lines.".into(),
            );
        }
        let mut offset = 0;
        let mut bounds = None;
        for (index, line) in raw.split_inclusive('\n').enumerate() {
            let end = offset + line.len();
            if offset < range.end && end > range.start {
                let entry = bounds.get_or_insert((index + 1, index + 1, offset, end));
                entry.1 = index + 1;
                entry.3 = end;
            }
            offset = end;
            if offset >= range.end {
                break;
            }
        }
        let (first, last, start, end) = bounds.ok_or("The selection has no source lines.")?;
        let excerpt = &raw[start..end];
        if excerpt.len() > MAX_SOURCE {
            return Err("The complete selected lines exceed 8 KiB. Select fewer lines.".into());
        }
        if excerpt.trim_start_matches('\u{feff}').trim().is_empty() {
            return Err("The selected lines contain no nonempty text.".into());
        }
        (first, last, excerpt)
    } else {
        (1, raw.split_inclusive('\n').count(), raw.as_str())
    };
    let id = format!(
        "c_{}",
        sha(format!(
            "{}\n{}\n{start_line}\n{end_line}",
            source.path, source.revision
        )
        .as_bytes())
    );
    Ok(
        json!({"citation_id":id,"path":source.path,"revision":source.revision,"start_line":start_line,"end_line":end_line,"locator":format!("L{start_line}-L{end_line}"),"excerpt":excerpt,"metadata":metadata}),
    )
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Addition {
    Add,
    AlreadyIncluded,
}
pub fn preflight(citation: &Value, chosen: &[Value], guidance: &str) -> Result<Addition, String> {
    let same: Vec<_> = chosen
        .iter()
        .filter(|c| c["path"] == citation["path"])
        .collect();
    if same.iter().any(|c| c["revision"] != citation["revision"]) {
        return Err("Context contains an older version of this note. Remove it explicitly in Context before including the current Source.".into());
    }
    if !same.is_empty() {
        return Ok(Addition::AlreadyIncluded);
    }
    if chosen.len() >= 20 {
        return Err(
            "Context already contains twenty citations. Remove one before adding this note.".into(),
        );
    }
    let size = guidance.len()
        + chosen
            .iter()
            .map(|c| string(&c["excerpt"]).len())
            .sum::<usize>()
        + string(&citation["excerpt"]).len();
    if size > MAX_FORM {
        return Err("Guidance and excerpts would exceed 64 KiB. Remove text or a citation before adding this note.".into());
    }
    Ok(Addition::Add)
}
/// Prepare hydration without mutating either visible draft. Retain hidden scope
/// restrictions from the saved packet as well as transient local form values.
pub fn hydrate(local: Option<&Value>, packet: &Value, goal: &str) -> Result<Value, String> {
    if packet.is_object() && packet["goal_id"] != goal {
        return Err("Saved Context belongs to another goal.".into());
    }
    if local.is_some_and(|f| f["packet"].is_object()) {
        return Ok(local.unwrap().clone());
    }
    let mut form = json!({"query":string(&packet["query"]),"guidance":string(&packet["text"]),"mode":"hybrid","scope_mode":packet["scope"]["mode"].as_str().unwrap_or("project"),"prefix":string(&packet["scope"]["path_prefix"]),"chosen":packet["citations"].as_array().cloned().unwrap_or_default(),"pinned":packet["pinned_citation_ids"].as_array().cloned().unwrap_or_default(),"packet":packet,"selection_changed":false});
    if let Some(local) = local {
        for key in ["query", "guidance", "prefix"] {
            if !string(&local[key]).is_empty() {
                form[key] = local[key].clone();
            }
        }
        // Two distinct folder restrictions must not silently widen each other.
        let saved = string(&packet["scope"]["path_prefix"]).trim_end_matches('/');
        let current = string(&local["prefix"]).trim_end_matches('/');
        if !saved.is_empty() && !current.is_empty() && saved != current {
            return Err("Saved and local Context use different folders. Open Context to inspect the scope first.".into());
        }
        if local["scope_mode"] == "goal" {
            form["scope_mode"] = json!("goal");
        }
        form["mode"] = local["mode"].clone();
        let mut chosen = form["chosen"].as_array().unwrap().clone();
        for c in local["chosen"].as_array().into_iter().flatten() {
            if let Some(old) = chosen
                .iter()
                .find(|old| old["citation_id"] == c["citation_id"])
            {
                if old != c {
                    return Err("Saved and local Context contain different bytes for one citation. Inspect Context first.".into());
                }
            } else {
                chosen.push(c.clone());
            }
        }
        let mut pins = form["pinned"].as_array().unwrap().clone();
        for pin in local["pinned"].as_array().into_iter().flatten() {
            if !pins.contains(pin) {
                pins.push(pin.clone());
            }
        }
        form["chosen"] = json!(chosen);
        form["pinned"] = json!(pins);
        form["selection_changed"] = json!(
            local["selection_changed"] == true
                || form["chosen"] != packet["citations"] && packet.is_object()
        );
    }
    Ok(form)
}
pub fn form_scope(form: &Value, goal: &str) -> Value {
    let mut scope = if form["packet"]["scope"].is_object() {
        form["packet"]["scope"].clone()
    } else {
        json!({"include_paths":[],"exclude_paths":[]})
    };
    scope["goal_id"] = json!(goal);
    scope["mode"] = form["scope_mode"].clone();
    let prefix = string(&form["prefix"]).trim();
    scope["path_prefix"] = if prefix.is_empty() {
        Value::Null
    } else {
        json!(prefix)
    };
    scope
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    pub(super) fn fixture(raw: &str) -> (Value, Value, String, Value) {
        let goal = uuid::Uuid::new_v4().to_string();
        let brain = uuid::Uuid::new_v4().to_string();
        let workspace = json!({"brain_id":brain,"managed":true,"records_dir":"records","root":"/isolated/brain"});
        let source = json!({"schema":"ai-brain/v1","media_type":"text/markdown","brain_id":brain,"path":"notes/choice.md","revision":format!("sha256:{}",sha(raw.as_bytes())),"content_base64":STANDARD.encode(raw)});
        let scope = json!({"goal_id":goal,"mode":"project","path_prefix":"notes","include_paths":["notes/choice.md"],"exclude_paths":[]});
        (source, workspace, goal, scope)
    }
    pub(crate) fn observation_fixture(workspace: &Value, goal: &str) -> (Value, Value) {
        let id = uuid::Uuid::new_v4().to_string();
        let link = uuid::Uuid::new_v4().to_string();
        let path = format!("records/maestro-observation-{id}.md");
        let m = json!({"schema":"ai-brain/v1","brain_id":workspace["brain_id"],"record_type":"maestro-observation",
            "id":id,"link_id":link,"goal_id":goal,"observed_at":"2026-09-07T01:00:01Z","remote_at":"2026-09-07T01:00:00Z",
            "paused":true,"verification":"unverified","issue":{"number":42,"title":"Historical work","attempts":[],"approvals":[]},
            "authored_unknown":{"retain":"verbatim"}});
        let raw = format!(
            "---\n{}---\n# Exact historical observation\n",
            serde_yaml::to_string(&m).unwrap()
        );
        let source = json!({"schema":"ai-brain/v1","brain_id":workspace["brain_id"],"path":path,"revision":format!("sha256:{}",sha(raw.as_bytes())),"content_base64":STANDARD.encode(raw),"media_type":"text/markdown"});
        let view = json!({"schema":"tessera-maestro-observation/v1","goal_id":goal,"recovery_required":false,"link":null,
            "history":[{"id":link,"goal_id":goal,"active":false,"observation_ids":[id],"issue_number":42,
            "instance":{"base_url":"https://example.test/","instance_id":"fixture"},"project_id":"project","project_name":"Historical project","repo":"fixture/test"}],
            "source_paths":{"observations":{id:path}}});
        (source, view)
    }
    #[test]
    fn source_selection_lines_preserve_half_open_boundaries_and_original_bytes() {
        let raw = "\u{feff}Alpha 😀\r\nBeta λ\r\nFinal";
        let (source, workspace, goal, scope) = fixture(raw);
        let beta = raw.find("Beta").unwrap();
        let final_line = raw.find("Final").unwrap();
        for (range, first, last, excerpt) in [
            (3..beta, 1, 1, &raw[..beta]),
            (beta + 1..final_line, 2, 2, &raw[beta..final_line]),
            (beta - 1..beta, 1, 1, &raw[..beta]),
            (beta - 2..beta - 1, 1, 1, &raw[..beta]),
            (final_line + 1..raw.len(), 3, 3, &raw[final_line..]),
            (beta - 1..beta + 1, 1, 2, &raw[..final_line]),
        ] {
            let actual = selected_lines(&source, &workspace, &goal, &scope, range).unwrap();
            assert_eq!(actual["start_line"], first);
            assert_eq!(actual["end_line"], last);
            assert_eq!(actual["excerpt"], excerpt);
        }
        for raw in ["A\n", "A\r\n"] {
            let (source, workspace, goal, scope) = fixture(raw);
            let actual =
                selected_lines(&source, &workspace, &goal, &scope, raw.len() - 1..raw.len())
                    .unwrap();
            assert_eq!(actual["end_line"], 1);
            assert_eq!(actual["excerpt"], raw);
        }
        for range in [
            0..0,
            0..raw.len() + 1,
            1..3,
            std::ops::Range { start: 3, end: 1 },
        ] {
            assert!(selected_lines(&source, &workspace, &goal, &scope, range).is_err());
        }
    }
    #[test]
    fn source_selection_lines_have_independent_excerpt_and_context_source_caps() {
        for size in [MAX_SOURCE, MAX_SOURCE + 1] {
            let raw = format!("{}\nSmall\n", "x".repeat(size - 1));
            let (source, workspace, goal, scope) = fixture(&raw);
            let result = selected_lines(&source, &workspace, &goal, &scope, 0..1);
            assert_eq!(result.is_ok(), size == MAX_SOURCE);
            assert!(citation(&source, &workspace, &goal, &scope).is_err());
            let small = selected_lines(&source, &workspace, &goal, &scope, size..size + 1).unwrap();
            assert_eq!(small["excerpt"], "Small\n");
        }
        for size in [MAX_CONTEXT_SOURCE, MAX_CONTEXT_SOURCE + 1] {
            let raw = format!("Small\n{}", "x".repeat(size - 6));
            let (source, workspace, goal, scope) = fixture(&raw);
            let result = selected_lines(&source, &workspace, &goal, &scope, 0..1);
            assert_eq!(result.is_ok(), size == MAX_CONTEXT_SOURCE);
            if size > MAX_CONTEXT_SOURCE {
                assert!(result.unwrap_err().contains("1 MiB"));
            }
        }
        let (source, workspace, goal, scope) = fixture("content\n  \r\n");
        assert!(selected_lines(&source, &workspace, &goal, &scope, 8..9).is_err());
    }
    #[test]
    fn source_context_observation_identity_history_and_receipt_refusals() {
        let (_, workspace, goal, _) = fixture("note");
        let (source, view) = observation_fixture(&workspace, &goal);
        let scope = json!({"goal_id":goal,"mode":"project","path_prefix":null});
        let history = observation_history(&source, &workspace, &goal, &view).unwrap();
        let actual = citation_with_history(&source, &workspace, &goal, &scope, &history).unwrap();
        assert_eq!(actual["metadata"]["verification"], "unverified");
        assert_eq!(actual["metadata"]["observed_at"], "2026-09-07T01:00:01Z");
        assert!(citation(&source, &workspace, &goal, &scope).is_err());
        for key in [
            "review",
            "decision_receipt",
            "execution_status",
            "goal_id",
            "brain_id",
            "id",
            "link_id",
            "issue",
            "observed_at",
            "remote_at",
            "verification",
            "paused",
            "schema",
            "record_type",
        ] {
            let raw =
                String::from_utf8(STANDARD.decode(string(&source["content_base64"])).unwrap())
                    .unwrap();
            let (mut m, body) = metadata(&raw).unwrap();
            m[key] = json!("wrong");
            let raw = format!(
                "---\n{}---\n{}",
                serde_yaml::to_string(&m).unwrap(),
                &raw[body..]
            );
            let mut bad = source.clone();
            bad["revision"] = json!(format!("sha256:{}", sha(raw.as_bytes())));
            bad["content_base64"] = json!(STANDARD.encode(raw));
            assert!(
                citation_with_history(&bad, &workspace, &goal, &scope, &history).is_err(),
                "{key}"
            );
        }
        for pointer in [
            "/goal_id",
            "/recovery_required",
            "/history/0/goal_id",
            "/history/0/id",
            "/history/0/observation_ids",
            "/history/0/issue_number",
            "/history/0/instance",
            "/history/0/repo",
        ] {
            let mut bad = view.clone();
            *bad.pointer_mut(pointer).unwrap() = Value::Null;
            assert!(
                observation_history(&source, &workspace, &goal, &bad).is_err(),
                "{pointer}"
            );
        }
        let mut bad = view.clone();
        bad["history"]
            .as_array_mut()
            .unwrap()
            .push(view["history"][0].clone());
        assert!(observation_history(&source, &workspace, &goal, &bad).is_err());
        let mut bad = view.clone();
        bad["history"][0]["issue_number"] = json!(43);
        let changed = observation_history(&source, &workspace, &goal, &bad).unwrap();
        assert_ne!(changed, history);
        assert!(citation_with_history(&source, &workspace, &goal, &scope, &changed).is_err());
        let mut bad = view.clone();
        bad["history"][0]["repo"] = json!("other/repo");
        assert_ne!(
            observation_history(&source, &workspace, &goal, &bad).unwrap(),
            history
        );
        let mut wrong = source.clone();
        wrong["path"] = json!(format!(
            "records/maestro-observation-{}.md",
            uuid::Uuid::new_v4()
        ));
        assert!(observation_history(&wrong, &workspace, &goal, &view).is_err());
        let mut excluded = scope.clone();
        excluded["exclude_paths"] = json!([source["path"]]);
        assert!(citation_with_history(&source, &workspace, &goal, &excluded, &history).is_err());
    }
    #[test]
    fn source_context_exact_bytes_metadata_and_scope() {
        let raw="\u{feff}---\r\ntype: nOtE\r\nverification: unverified\r\nassistant_origin: {model: synthetic}\r\ncustom: ' untouched '\r\n---\r\n  λ choice without final newline";
        let (source, workspace, goal, scope) = fixture(raw);
        let c = citation(&source, &workspace, &goal, &scope).unwrap();
        assert_eq!(c["excerpt"], raw);
        assert_eq!(c["metadata"]["record_type"], "nOtE");
        assert_eq!(c["metadata"]["verification"], "unverified");
        assert_eq!(c["end_line"], 7);
        for field in ["brain_id", "path", "revision"] {
            let mut bad = source.clone();
            bad[field] = json!("wrong");
            assert!(citation(&bad, &workspace, &goal, &scope).is_err());
        }
        let mut excluded = scope.clone();
        excluded["exclude_paths"] = json!(["notes/choice.md"]);
        assert!(citation(&source, &workspace, &goal, &excluded).is_err());
        let mut prefix = scope.clone();
        prefix["path_prefix"] = json!("elsewhere");
        assert!(citation(&source, &workspace, &goal, &prefix).is_err());
    }
    #[test]
    fn source_context_refuses_unsupported_empty_oversize_and_duplicate_metadata() {
        for raw in [
            "",
            " \n",
            "---\ntype: Note\n---\n ",
            "---\ntype: Note\ntype: Note\n---\nText",
            "---\ntype: Note\nText",
            "---\nrecord_type: conversation\n---\nText",
            "---\ntype: Project\n---\nText",
            "---\ngoal_id: other\n---\nText",
        ] {
            let (source, workspace, goal, scope) = fixture(raw);
            assert!(
                citation(&source, &workspace, &goal, &scope).is_err(),
                "{raw}"
            );
        }
        let (source, workspace, goal, scope) = fixture(&"x".repeat(MAX_SOURCE + 1));
        assert!(citation(&source, &workspace, &goal, &scope).is_err());
    }
    #[test]
    fn source_context_duplicate_passage_old_revision_and_form_limits() {
        let (source, workspace, goal, scope) = fixture("Exact note");
        let c = citation(&source, &workspace, &goal, &scope).unwrap();
        let mut passage = c.clone();
        passage["citation_id"] = json!("existing-passage");
        passage["excerpt"] = json!("Exact");
        assert_eq!(
            preflight(&c, &[passage], "").unwrap(),
            Addition::AlreadyIncluded
        );
        let mut old = c.clone();
        old["revision"] = json!("old");
        assert!(preflight(&c, &[old], "").is_err());
        let other = json!({"path":"other.md","excerpt":""});
        assert!(preflight(&c, &vec![other; 20], "").is_err());
        assert!(preflight(&c, &[], &"x".repeat(MAX_FORM)).is_err());
        assert_eq!(
            preflight(&c, &[], &"x".repeat(MAX_FORM - string(&c["excerpt"]).len())).unwrap(),
            Addition::Add
        );
    }
    #[test]
    fn source_context_hydration_preserves_null_packet_draft_and_hidden_restrictions() {
        let local = json!({"packet":null,"query":"Local question","guidance":"Unsent text","mode":"lexical","scope_mode":"goal","prefix":"notes","chosen":[{"citation_id":"local","path":"notes/a.md"}],"pinned":["local"],"selection_changed":true});
        let packet = json!({"goal_id":"goal","query":"Saved question","text":"Saved text","scope":{"mode":"project","path_prefix":"notes","include_paths":["notes/a.md","notes/b.md"],"exclude_paths":["notes/private.md"]},"citations":[{"citation_id":"saved","path":"notes/b.md"}],"pinned_citation_ids":["saved"]});
        let form = hydrate(Some(&local), &packet, "goal").unwrap();
        assert_eq!(form["guidance"], "Unsent text");
        assert_eq!(form["query"], "Local question");
        assert_eq!(form["chosen"].as_array().unwrap().len(), 2);
        assert_eq!(form["pinned"].as_array().unwrap().len(), 2);
        let scope = form_scope(&form, "goal");
        assert_eq!(scope["mode"], "goal");
        assert_eq!(scope["exclude_paths"], packet["scope"]["exclude_paths"]);
        assert_eq!(scope["include_paths"], packet["scope"]["include_paths"]);
        assert!(validate_scope(&scope, "goal", "notes/private.md").is_err());
    }
}
