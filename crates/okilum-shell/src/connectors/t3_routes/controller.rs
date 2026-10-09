//! Pure selection state used by the ordinary Connections form.
use crate::brain::t3_route_outbox::Pending;
use serde_json::json;
use serde_json::Value;

#[derive(Default)]
pub(super) struct Selection {
    pub(super) review: Option<Value>,
    candidate: Option<Value>,
}
impl Selection {
    pub(super) fn prepare(&mut self, candidate: Value) -> Value {
        self.clear();
        json!({"op":"t3_target_prepare","candidate":candidate})
    }
    pub(super) fn prepare_with_manifest(
        &mut self,
        candidate: Value,
        manifest: Option<Value>,
    ) -> Value {
        let mut wire = self.prepare(candidate);
        if let Some(manifest) = manifest {
            wire["compatibility_manifest"] = manifest;
        }
        wire
    }
    pub(super) fn observe_manifest(&mut self, manifest: &Option<Value>) {
        if self
            .review
            .as_ref()
            .is_some_and(|r| r["compatibility_manifest"] != manifest.clone().unwrap_or(Value::Null))
        {
            self.clear();
        }
    }
    pub(super) fn accept_with_manifest(
        &mut self,
        requested: Value,
        current: &Value,
        manifest: Option<Value>,
        current_manifest: &Option<Value>,
        review: Value,
    ) -> Result<(), String> {
        self.clear();
        if manifest != *current_manifest
            || review["compatibility_manifest"] != manifest.clone().unwrap_or(Value::Null)
        {
            return Err(
                "Compatibility evidence changed while reviewing. Prepare a new review.".into(),
            );
        }
        self.accept(requested, current, review)
    }
    pub(super) fn adoption(&self, workspace: Value, candidate: &Value) -> Option<Pending> {
        self.can_adopt(candidate)
            .then(|| Pending::prepare(workspace, self.review.clone().unwrap()))
    }
    pub(super) fn clear(&mut self) {
        self.review = None;
        self.candidate = None;
    }
    pub(super) fn observe(&mut self, candidate: &Value) {
        if self
            .candidate
            .as_ref()
            .is_some_and(|saved| saved != candidate)
        {
            self.clear();
        }
    }
    pub(super) fn accept(
        &mut self,
        requested: Value,
        current: &Value,
        review: Value,
    ) -> Result<(), String> {
        self.clear();
        if requested != *current {
            return Err(
                "Candidate changed while reviewing. Review the new target explicitly.".into(),
            );
        }
        if review["schema"] != "tessera-t3-target/v1"
            || review["candidate"] != requested
            || !review["guard"].is_object()
            || review["guard"]["revision"]
                .as_str()
                .is_none_or(str::is_empty)
            || review["guard"]["inventory_digest"]
                .as_str()
                .is_none_or(str::is_empty)
            || review["guard"]["candidate_digest"]
                .as_str()
                .is_none_or(str::is_empty)
            || !review["ready"].is_boolean()
            || !review["blockers"].is_array()
            || !review["associations"].is_array()
        {
            return Err(
                "Target review is not supported or is incomplete. Nothing was adopted.".into(),
            );
        }
        self.candidate = Some(requested);
        self.review = Some(review);
        Ok(())
    }
    pub(super) fn can_adopt(&self, current: &Value) -> bool {
        self.candidate.as_ref() == Some(current)
            && self.review.as_ref().is_some_and(|r| {
                r["ready"] == true && r["blockers"].as_array().is_some_and(Vec::is_empty)
            })
    }
}
pub(super) fn unknown_history_count(review: &Value) -> usize {
    review["associations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["association"]["kind"] == "historical_terminal_unroutable")
        .count()
}
/// Generic Save cannot silently adopt a candidate or replace durable selection.
/// An unchanged selected target is represented by its saved baseline in that API.
pub(super) fn generic_save_config(
    mut form: Value,
    baseline: &Value,
    current: &Value,
) -> Result<Value, String> {
    if current["active_generation"].is_null() {
        return Ok(form);
    }
    if form["t3"] != current["active"] && form["t3"] != *baseline {
        return Err("T3 candidate changes require Review future target and explicit adoption. Save other connector edits after restoring the unchanged T3 selection.".into());
    }
    form["t3"] = baseline.clone();
    Ok(form)
}
/// Hydration is reserved for loading saved settings, not candidate discovery.
pub(super) fn hydrated_form(
    before: &Value,
    now: &Value,
    current: &Value,
    hydrate: bool,
) -> Option<Value> {
    if !hydrate || before != now || !current["active"].is_object() {
        return None;
    }
    let mut form = now.clone();
    form["t3"] = current["active"].clone();
    Some(form)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    fn candidate() -> Value {
        json!({"base_url":"http://localhost:21001","environment_id":"distinct-env","project_id":"project"})
    }
    fn review() -> Value {
        json!({"schema":"tessera-t3-target/v1","candidate":candidate(),"previous":{"environment_id":"old-env"},"guard":{"revision":"r1","inventory_digest":"inventory","candidate_digest":"candidate"},"ready":true,"blockers":[],"associations":[]})
    }
    #[test]
    fn check_preserves_candidate_and_unrelated_save_keeps_baseline_after_adoption() {
        let old = json!({"environment_id":"old","base_url":"http://localhost:21000"});
        let new = candidate();
        let form = json!({"chat":{"model":"changed"},"t3":new});
        let selected = json!({"active_generation":"new-generation","active":candidate()});
        assert!(hydrated_form(&form, &form, &json!({"active":old}), false).is_none());
        let saved = generic_save_config(form.clone(), &old, &selected).unwrap();
        assert_eq!(saved["t3"], old);
        assert_eq!(saved["chat"], form["chat"]);
        let mut candidate_edit = form.clone();
        candidate_edit["t3"]["project_id"] = json!("unreviewed-project");
        assert!(generic_save_config(candidate_edit.clone(), &old, &selected).is_err());
        assert!(hydrated_form(&form, &candidate_edit, &selected, true).is_none());
        assert_eq!(
            hydrated_form(&json!({"t3":old}), &json!({"t3":old}), &selected, true).unwrap()["t3"],
            candidate()
        );
    }
    #[test]
    fn compatibility_manifest_is_explicit_bound_and_edit_invalidated() {
        let mut s = Selection::default();
        let c = candidate();
        let manifest = Some(
            json!({"entries":[{"operation_id":"original-op","artifact_path":"/fixture/original.json","artifact_sha256":"pinned","provenance":"fixture"}]}),
        );
        let wire = s.prepare_with_manifest(c.clone(), manifest.clone());
        assert_eq!(wire["compatibility_manifest"], manifest.clone().unwrap());
        let mut r = review();
        r["compatibility_manifest"] = manifest.clone().unwrap();
        s.accept_with_manifest(c.clone(), &c, manifest.clone(), &manifest, r.clone())
            .unwrap();
        assert!(s.can_adopt(&c));
        s.observe_manifest(&None);
        assert!(!s.can_adopt(&c));
        assert!(s
            .accept_with_manifest(c.clone(), &c, manifest, &None, r)
            .is_err());
    }
    #[test]
    fn fake_transport_observes_prepare_only_until_explicit_adopt() {
        let mut selection = Selection::default();
        let candidate = candidate();
        let mut sent = Vec::new();
        let wire = selection.prepare(candidate.clone());
        sent.push(wire);
        selection
            .accept(candidate.clone(), &candidate, review())
            .unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["op"], "t3_target_prepare");
        // No request was produced by receiving a ready review. Only this explicit
        // adoption action creates an intent, which UI retains before transmission.
        let pending = selection
            .adoption(json!({"brain_id":"fixture"}), &candidate)
            .unwrap();
        sent.push(pending.wire());
        assert_eq!(sent[1]["op"], "t3_target_adopt");
        assert_eq!(sent[1]["request"]["review"], review());
        selection.observe(&json!({"environment_id":"edited"}));
        assert!(selection
            .adoption(json!({"brain_id":"fixture"}), &candidate)
            .is_none());
    }
    #[test]
    fn review_is_explicit_candidate_bound_and_invalidated_by_edits() {
        let mut s = Selection::default();
        let c = candidate();
        assert!(!s.can_adopt(&c));
        s.accept(c.clone(), &c, review()).unwrap();
        assert!(s.can_adopt(&c));
        let mut changed = c.clone();
        changed["project_id"] = json!("other");
        assert!(!s.can_adopt(&changed));
        s.observe(&changed);
        assert!(!s.can_adopt(&c));
        assert!(s.accept(c, &changed, review()).is_err());
    }
    #[test]
    fn blockers_and_malformed_review_never_enable_adoption() {
        let c = candidate();
        let mut s = Selection::default();
        let mut blocked = review();
        blocked["blockers"] = json!([{"code":"unsettled_work"}]);
        s.accept(c.clone(), &c, blocked).unwrap();
        assert!(!s.can_adopt(&c));
        let mut malformed = review();
        malformed["guard"]["candidate_digest"] = Value::Null;
        assert!(s.accept(c.clone(), &c, malformed).is_err());
        assert!(!s.can_adopt(&c));
    }
    #[test]
    fn unknown_origin_warning_counts_only_explicit_unroutable_associations() {
        let mut r = review();
        r["associations"] = json!([{"association":{"kind":"historical_terminal_unroutable"}},{"association":{"kind":"known_route","generation_id":"g"}}]);
        assert_eq!(unknown_history_count(&r), 1);
    }
}
