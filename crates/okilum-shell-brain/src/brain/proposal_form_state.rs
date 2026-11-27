//! Pure adoption form preparation. Opening/editing/cancelling never writes a target.
//! UI integration follows the stable adoption service contract.
#![cfg_attr(not(test), allow(dead_code))]
use super::*;
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(super) struct InboxDraft {
    pub title: String,
    pub criteria: String,
    pub human: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(super) struct ContextDraft {
    pub guidance: String,
    pub citations: Vec<Value>,
    pub pins: Vec<String>,
}
pub(super) fn eligible(detail: &Value, workspace: &Value, owner: &Value) -> bool {
    super::proposal_ui::valid_detail(detail, workspace, owner)
        && detail["record"]["attempt"]["state"] == "draft"
        && matches!(
            detail["record"]["disposition"]["kind"].as_str(),
            Some("unreviewed" | "snoozed")
        )
        && detail["projection_pending"] == false
        && detail["source"]["revision"] == detail["current_revision"]
        && detail["stale_reasons"]
            .as_array()
            .is_some_and(Vec::is_empty)
        && detail["record"]["generated"].is_object()
}
fn generated_lines(generated: &Value, key: &str) -> Result<Vec<String>, String> {
    generated[key]
        .as_array()
        .ok_or_else(|| "Suggestion fields are incomplete.".to_string())?
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
                .ok_or("Suggestion contains an empty field.".into())
        })
        .collect()
}
pub(super) fn inbox_prefill(
    detail: &Value,
    workspace: &Value,
    human: bool,
) -> Result<InboxDraft, String> {
    if !eligible(detail, workspace, &Value::Null)
        || detail["record"]["trigger"]["identity"]["kind"] != "inbox_saved"
    {
        return Err("This suggestion does not belong to an unplanned Inbox thought.".into());
    }
    let generated = &detail["record"]["generated"];
    let title = generated["title"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or("Suggestion title is unavailable.")?;
    let criteria = generated_lines(generated, "criteria")?;
    if title.len() > 512
        || criteria.is_empty()
        || criteria.len() > 50
        || criteria
            .iter()
            .any(|s| s.len() > 4096 || s.contains(['\r', '\n']))
    {
        return Err("Suggestion exceeds the editable planning form bounds. Keep the original draft and inspect its source.".into());
    }
    Ok(InboxDraft {
        title: title.into(),
        criteria: criteria.join("\n"),
        human,
    })
}
pub(super) fn context_prefill(
    detail: &Value,
    workspace: &Value,
    goal: &str,
    manual: &ContextDraft,
) -> Result<ContextDraft, String> {
    if !eligible(detail, workspace, &json!(goal)) {
        return Err("This suggestion does not belong to the selected original goal.".into());
    }
    let generated = &detail["record"]["generated"];
    let title = generated["title"]
        .as_str()
        .ok_or("Suggestion title is unavailable.")?;
    let rationale = generated["rationale"]
        .as_str()
        .ok_or("Suggestion rationale is unavailable.")?;
    let criteria = generated_lines(generated, "criteria")?;
    let questions = generated_lines(generated, "open_questions")?;
    let mut guidance = manual.guidance.clone();
    if !guidance.is_empty() {
        guidance.push_str("\n\n");
    }
    guidance.push_str(&format!(
        "## Unverified suggestion\n\n{title}\n\n{rationale}\n\nProposed criteria:\n"
    ));
    for line in criteria {
        guidance.push_str(&format!("- {line}\n"));
    }
    if !questions.is_empty() {
        guidance.push_str("\nOpen questions:\n");
        for line in questions {
            guidance.push_str(&format!("- {line}\n"));
        }
    }
    let mut citations = manual.citations.clone();
    let ids = generated_lines(generated, "citation_ids")?;
    let captured = detail["record"]["captured"]["citations"]
        .as_array()
        .ok_or("Retained citations are unavailable.")?;
    for id in ids {
        let matches: Vec<_> = captured.iter().filter(|v| v["citation_id"] == id).collect();
        if matches.len() != 1 {
            return Err("Retained citation identity is ambiguous or missing.".into());
        }
        let cited = matches[0];
        if let Some(existing) = citations.iter().find(|v| v["citation_id"] == id) {
            if existing != cited {
                return Err("A retained citation conflicts with the existing selection. Both originals remain unchanged.".into());
            }
        } else {
            citations.push(cited.clone());
        }
    }
    let mut seen = BTreeSet::new();
    if citations.iter().any(|v| {
        v["citation_id"]
            .as_str()
            .is_none_or(|id| !seen.insert(id.to_owned()))
    }) || manual.pins.iter().any(|id| !seen.contains(id))
    {
        return Err(
            "Existing citation or pin identity is incomplete; no selection was replaced.".into(),
        );
    }
    let size = citations.iter().try_fold(guidance.len(), |size, c| {
        c["excerpt"]
            .as_str()
            .and_then(|s| size.checked_add(s.len()))
    });
    if citations.len() > 20 || size.is_none_or(|size| size > 64 * 1024) {
        return Err("The combined guidance and retained citations exceed the context budget. Existing pins were preserved.".into());
    }
    Ok(ContextDraft {
        guidance,
        citations,
        pins: manual.pins.clone(),
    })
}

/// Each suggestion retains its edited form independently from the manual form.
/// A dirty destination requires an explicit local choice before applying a prefill.
pub(super) struct DraftShelf<T> {
    drafts: BTreeMap<String, T>,
    active: Option<(String, T)>,
    chosen: bool,
}
impl<T: Clone> Default for DraftShelf<T> {
    fn default() -> Self {
        Self {
            drafts: BTreeMap::new(),
            active: None,
            chosen: false,
        }
    }
}
impl<T: Clone + Serialize> DraftShelf<T> {
    pub fn open(
        &mut self,
        key: String,
        baseline: Value,
        manual: T,
        prefill: T,
        manual_dirty: bool,
    ) -> Result<Option<T>, String> {
        if self.active.is_some() {
            return Err(
                "Keep or cancel the current suggestion form before opening another.".into(),
            );
        }
        // Include canonical destination identity/revision/scope and the complete
        // unsaved manual form. A cached edit cannot replace a newer manual base.
        let binding =
            serde_json::to_vec(&json!({"origin":key,"baseline":baseline,"manual":manual}))
                .map_err(|e| e.to_string())?;
        let key = format!("{:x}", Sha256::digest(binding));
        self.drafts.entry(key.clone()).or_insert(prefill);
        self.active = Some((key, manual));
        self.chosen = !manual_dirty;
        Ok(self.current())
    }
    pub fn choose_suggestion(&mut self) -> Option<T> {
        self.chosen = true;
        self.current()
    }
    pub fn current(&self) -> Option<T> {
        if !self.chosen {
            return None;
        }
        self.active
            .as_ref()
            .and_then(|(key, _)| self.drafts.get(key))
            .cloned()
    }
    pub fn edit(&mut self, form: T) -> Result<(), String> {
        if !self.chosen {
            return Err("Choose the suggestion draft before editing it.".into());
        }
        let (key, _) = self.active.as_ref().ok_or("No suggestion form is open.")?;
        self.drafts.insert(key.clone(), form);
        Ok(())
    }
    pub fn cancel(&mut self) -> Option<T> {
        self.chosen = false;
        self.active.take().map(|(_, manual)| manual)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    fn detail(owner: Value) -> Value {
        let mut d = super::super::proposal_outbox::tests::detail();
        d["record"]["goal_id"] = owner;
        d["record"]["trigger"] = json!({"identity":{"kind":if d["record"]["goal_id"].is_null(){"inbox_saved"}else{"decision_saved"}}});
        d["record"]["generated"] = json!({"title":"Synthetic next action","criteria":["Operator confirms result"],"rationale":"Retained evidence remains unverified","open_questions":[],"citation_ids":[]});
        d["record"]["captured"] = json!({"citations":[]});
        d
    }
    #[test]
    fn adoption_forms_preserve_manual_drafts_and_each_suggestions_edits() {
        let mut shelf = DraftShelf::default();
        let manual = "Exact manual 雪  \n".to_string();
        assert_eq!(
            shelf
                .open(
                    "A:rev1".into(),
                    json!({"packet_revision":"base1"}),
                    manual.clone(),
                    "Suggestion A".into(),
                    true
                )
                .unwrap(),
            None
        );
        assert!(shelf.edit("unselected replacement".into()).is_err());
        assert_eq!(shelf.choose_suggestion(), Some("Suggestion A".into()));
        shelf.edit("Edited A".into()).unwrap();
        assert!(shelf
            .open(
                "B:rev1".into(),
                Value::Null,
                "Other manual".into(),
                "Suggestion B".into(),
                false
            )
            .is_err());
        assert_eq!(shelf.current(), Some("Edited A".into()));
        assert_eq!(shelf.cancel(), Some(manual.clone()));
        assert_eq!(
            shelf
                .open(
                    "B:rev1".into(),
                    Value::Null,
                    "Other manual".into(),
                    "Suggestion B".into(),
                    false
                )
                .unwrap(),
            Some("Suggestion B".into())
        );
        assert_eq!(shelf.cancel(), Some("Other manual".into()));
        assert_eq!(
            shelf
                .open(
                    "A:rev1".into(),
                    json!({"packet_revision":"base1"}),
                    "New manual".into(),
                    "Initial A".into(),
                    false
                )
                .unwrap(),
            Some("Initial A".into())
        );
        assert_eq!(shelf.cancel(), Some("New manual".into()));
        assert_eq!(
            shelf
                .open(
                    "A:rev1".into(),
                    json!({"packet_revision":"base1"}),
                    manual.clone(),
                    "Initial A".into(),
                    false
                )
                .unwrap(),
            Some("Edited A".into())
        );
        assert_eq!(shelf.cancel(), Some(manual.clone()));
        assert_eq!(
            shelf
                .open(
                    "A:rev1".into(),
                    json!({"packet_revision":"base2"}),
                    manual.clone(),
                    "Reconciled A".into(),
                    false
                )
                .unwrap(),
            Some("Reconciled A".into())
        );
        assert_eq!(shelf.cancel(), Some(manual));
    }
    #[test]
    fn adoption_prefills_bind_owner_preserve_human_control_and_exact_pins() {
        let workspace = super::super::proposal_outbox::tests::workspace();
        let goal = "aa000000-0000-4000-8000-000000000194";
        assert!(inbox_prefill(&detail(json!(goal)), &workspace, true).is_err());
        assert!(
            inbox_prefill(&detail(Value::Null), &workspace, true)
                .unwrap()
                .human
        );
        let citation = json!({"citation_id":"same","path":"notes.md","revision":"sha256:original","start_line":1,"end_line":1,"excerpt":"Exact 雪  \n"});
        let manual = ContextDraft {
            guidance: "Manual guidance 雪  \n".into(),
            citations: vec![citation.clone()],
            pins: vec!["same".into()],
        };
        let before = manual.clone();
        let mut d = detail(json!(goal));
        d["record"]["generated"]["citation_ids"] = json!(["same"]);
        d["record"]["captured"]["citations"] = json!([citation]);
        let form = context_prefill(&d, &workspace, goal, &manual).unwrap();
        assert!(form.guidance.starts_with(&manual.guidance));
        assert_eq!(form.citations, manual.citations);
        assert_eq!(form.pins, manual.pins);
        assert_eq!(manual, before);
        assert!(context_prefill(
            &d,
            &workspace,
            "bb000000-0000-4000-8000-000000000194",
            &manual
        )
        .is_err());
        d["record"]["captured"]["citations"][0]["excerpt"] = json!("Changed bytes");
        assert!(context_prefill(&d, &workspace, goal, &manual).is_err());
        assert_eq!(manual, before);
        d = detail(json!(goal));
        let oversized = ContextDraft {
            guidance: "x".repeat(64 * 1024),
            ..manual.clone()
        };
        assert!(context_prefill(&d, &workspace, goal, &oversized).is_err());
        assert_eq!(oversized.guidance.len(), 64 * 1024);
    }
}
