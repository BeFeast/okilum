//! Rebuildable goal inputs. This read does not select, prepare or execute work.
use super::*;
use crate::retrieval::{self, Citation};
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

const MAX_SOURCE_BYTES: u64 = 8192;
// Match the existing reviewed-context source read bound; citation admission
// stays separate from reading canonical outcome evidence.
const MAX_RESULT_BYTES: u64 = 1024 * 1024;
const MAX_INPUTS: usize = 20;
const MAX_INPUT_BYTES: usize = 48 * 1024;
const MAX_DECISION_FILES: usize = 1024;

#[derive(Serialize)]
struct Input {
    id: String,
    kind: String,
    title: String,
    actor_id: Option<String>,
    received_at: String,
    #[serde(skip)]
    received_order: i128,
    verification: String,
    body: String,
    citation: Citation,
}
#[derive(Serialize, PartialEq, Eq, PartialOrd, Ord)]
struct Omission {
    path: String,
    code: String,
}
fn omit(omissions: &mut BTreeSet<Omission>, path: &str, code: &str) {
    omissions.insert(Omission {
        path: path.into(),
        code: code.into(),
    });
}

impl Runner {
    fn brief_input(&self, path: &str, goal: &str, kind: &str) -> Result<Input> {
        let source = self.read_preview_source(path, MAX_SOURCE_BYTES)?;
        self.brief_input_from_source(&source, goal, kind)
    }

    fn brief_input_from_source(
        &self,
        source: &SourceSnapshot,
        goal: &str,
        kind: &str,
    ) -> Result<Input> {
        let path = source.path.as_str();
        let bytes = STANDARD.decode(&source.content_base64)?;
        ensure!(
            bytes.len() as u64 <= MAX_SOURCE_BYTES,
            "brief citation exceeds source budget"
        );
        let text = std::str::from_utf8(&bytes)?;
        let (metadata, body_line) = retrieval::metadata(text);
        let id = metadata["id"].as_str().context("record ID missing")?;
        uuid(id)?;
        ensure!(
            metadata["schema"] == SCHEMA
                && metadata["record_type"] == kind
                && metadata["brain_id"] == self.state.brain_id
                && metadata["goal_id"] == goal
                && self.path(kind, id) == path,
            "canonical record identity mismatch"
        );
        let at = metadata["received_at"]
            .as_str()
            .context("record timestamp missing")?;
        valid_time(at)?;
        let verification = metadata["verification"]
            .as_str()
            .context("verification missing")?;
        let actor = if kind == "decision" {
            ensure!(
                verification == "unverified",
                "Attention replies remain unverified input"
            );
            ensure!(
                metadata["attention_id"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
                    && metadata["attention_revision"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty()),
                "Attention provenance missing"
            );
            let actor = metadata["actor_id"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .context("decision actor missing")?;
            let provenance: crate::inbox::SourceIdentity =
                serde_json::from_value(metadata["source"].clone())?;
            provenance.validate()?;
            ensure!(
                provenance.actor_id == actor,
                "decision actor and source disagree"
            );
            if provenance.channel == "native" {
                provenance.validate_native(actor)?;
            }
            Some(actor.to_owned())
        } else if kind == super::discussion_decision::KIND {
            Some(self.admit_discussion_decision(source)?.actor_id)
        } else if kind == "result" {
            let record: ResultRecord = serde_json::from_value(metadata.clone())?;
            ensure!(
                ["verified", "unverified", "failed"]
                    .contains(&record.outcome.verification.as_str()),
                "unknown outcome verification"
            );
            None
        } else {
            anyhow::bail!("Unknown brief record kind")
        };
        let lines: Vec<_> = text.split_inclusive('\n').collect();
        let citation = Citation {
            citation_id: retrieval::citation_id(path, &source.revision, 1, lines.len()),
            path: path.into(),
            revision: source.revision.clone(),
            start_line: 1,
            end_line: lines.len(),
            locator: format!("L1-L{}", lines.len()),
            excerpt: text.into(),
            metadata: retrieval::source_metadata(text),
        };
        // Existing citations include exact canonical frontmatter so actor/source
        // provenance survives old readers, T3 packets and AI excerpt export too.
        retrieval::validate_citation(source, &citation, goal, "goal", &self.state.records_dir)?;
        Ok(Input {
            id: id.into(),
            kind: kind.into(),
            title: if kind == "decision" {
                "Saved Attention reply"
            } else if kind == super::discussion_decision::KIND {
                "Saved user decision"
            } else {
                "Latest saved outcome"
            }
            .into(),
            actor_id: actor,
            received_at: at.into(),
            received_order: time::OffsetDateTime::parse(
                at,
                &time::format_description::well_known::Rfc3339,
            )?
            .unix_timestamp_nanos(),
            verification: verification.into(),
            body: lines[body_line..].concat(),
            citation,
        })
    }

    fn brief_result(
        &self,
        source: &SourceSnapshot,
        goal: &str,
        stage: &str,
        id: &str,
    ) -> Result<ResultRecord> {
        let bytes = STANDARD.decode(&source.content_base64)?;
        let text = std::str::from_utf8(&bytes)?;
        let (metadata, _) = retrieval::metadata(text);
        ensure!(
            metadata["schema"] == SCHEMA
                && metadata["record_type"] == "result"
                && metadata["brain_id"] == self.state.brain_id
                && metadata["id"] == id
                && metadata["goal_id"] == goal
                && metadata["stage_id"] == stage
                && source.path == self.path("result", id),
            "canonical result identity mismatch"
        );
        uuid(id)?;
        let result: ResultRecord = serde_json::from_value(metadata)?;
        valid_time(&result.received_at)?;
        ensure!(
            ["verified", "unverified", "failed"].contains(&result.outcome.verification.as_str()),
            "unknown outcome verification"
        );
        Ok(result)
    }

    pub(crate) fn goal_context_brief(&self, goal_id: &str) -> Result<Value> {
        uuid(goal_id)?;
        ensure!(
            self.state.goal_id.as_deref() == Some(goal_id),
            "goal brief routing mismatch"
        );
        let (goal, goal_source) = self.record::<Goal>("goal", goal_id)?;
        ensure!(goal.id == goal_id, "goal brief ownership mismatch");
        let mut omissions = BTreeSet::new();
        let mut candidates = BTreeMap::<String, Option<String>>::new();
        // Retained receipts discover a missing canonical file and prevent an
        // externally changed owner field from moving a known reply to another goal.
        for (path, owner) in self.attention_decision_inventory()? {
            candidates.insert(path, Some(owner));
        }
        ensure!(
            candidates.len() <= MAX_DECISION_FILES,
            "Goal brief decision inventory exceeds limit"
        );
        for entry in fs::read_dir(self.root.join(&self.state.records_dir))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if (name.starts_with("decision-") || name.starts_with("discussion-decision-"))
                && name.ends_with(".md")
            {
                candidates
                    .entry(format!("{}/{}", self.state.records_dir, name))
                    .or_default();
            }
            ensure!(
                candidates.len() <= MAX_DECISION_FILES,
                "Goal brief decision inventory exceeds limit"
            );
        }
        let mut decisions = Vec::new();
        let mut manual_only = Vec::new();
        let mut manual_only_count = 0usize;
        for (path, known_owner) in candidates {
            if known_owner.as_deref().is_some_and(|owner| owner != goal_id) {
                continue;
            }
            if known_owner.is_none() {
                let source = match self.read_preview_source(&path, MAX_SOURCE_BYTES) {
                    Ok(source) => source,
                    Err(_) => {
                        omit(&mut omissions, &path, "source_unavailable_or_oversized");
                        continue;
                    }
                };
                let bytes = STANDARD.decode(&source.content_base64)?;
                let Ok(text) = std::str::from_utf8(&bytes) else {
                    omit(&mut omissions, &path, "invalid_record");
                    continue;
                };
                let (meta, _) = retrieval::metadata(text);
                if meta["goal_id"].as_str().is_none() {
                    omit(&mut omissions, &path, "unknown_owner");
                    continue;
                }
                if meta["goal_id"] != goal_id {
                    continue;
                }
            }
            let kind = if path
                .rsplit('/')
                .next()
                .is_some_and(|name| name.starts_with("discussion-decision-"))
            {
                super::discussion_decision::KIND
            } else {
                "decision"
            };
            if kind == super::discussion_decision::KIND {
                if let Ok(source) = self.read_preview_source(&path, MAX_SOURCE_BYTES) {
                    let bytes = STANDARD.decode(&source.content_base64)?;
                    if let Ok(text) = std::str::from_utf8(&bytes) {
                        let (meta, _) = retrieval::metadata(text);
                        if meta["record_type"] == okilum_core::decision_reuse::MANUAL {
                            let id = meta["id"].as_str().unwrap_or("");
                            match self.decision_reuse_admission(&source, goal_id, id) {
                                Ok((_, Some(_))) => {
                                    manual_only_count += 1;
                                    let row = json!({"id":id,"path":path,"status":"manual_only"});
                                    if manual_only.len() < 20 {
                                        manual_only.push(row);
                                        if serde_json::to_vec(&manual_only)?.len() > 2048 {
                                            manual_only.pop();
                                        }
                                    }
                                }
                                _ => omit(
                                    &mut omissions,
                                    &path,
                                    "source_unavailable_oversized_or_invalid",
                                ),
                            }
                            continue;
                        }
                    }
                }
            }
            match self.brief_input(&path, goal_id, kind) {
                Ok(input) => decisions.push(input),
                Err(_) => omit(
                    &mut omissions,
                    &path,
                    "source_unavailable_oversized_or_invalid",
                ),
            }
        }
        decisions.sort_by(|a, b| {
            b.received_order
                .cmp(&a.received_order)
                .then_with(|| a.id.cmp(&b.id))
        });
        let mut inputs = Vec::new();
        let mut latest = None;
        for stage_id in goal.stage_ids.iter().rev() {
            let stage_path = self.path("stage", stage_id);
            let stage = match self.record::<Stage>("stage", stage_id) {
                Ok((stage, _)) if stage.id == *stage_id && stage.goal_id == goal_id => stage,
                _ => {
                    omit(
                        &mut omissions,
                        &stage_path,
                        "latest_stage_unavailable_or_invalid",
                    );
                    break;
                }
            };
            if let Some(result_id) = stage.result_ids.last() {
                let result_path = self.path("result", result_id);
                let outcome = self
                    .read_preview_source(&result_path, MAX_RESULT_BYTES)
                    .and_then(|source| {
                        let result = self.brief_result(&source, goal_id, stage_id, result_id)?;
                        Ok((source, result))
                    });
                match outcome {
                    Ok((source, result)) => {
                        // Derive criteria from canonical evidence even when the exact
                        // source cannot fit into a selectable citation. Both views
                        // use the same source snapshot, avoiding a second-read race.
                        match self.brief_input_from_source(&source, goal_id, "result") {
                            Ok(input) => inputs.push(input),
                            Err(_) => omit(
                                &mut omissions,
                                &result_path,
                                "latest_result_unavailable_oversized_or_invalid",
                            ),
                        }
                        latest = Some((stage, result));
                    }
                    Err(_) => omit(
                        &mut omissions,
                        &result_path,
                        "latest_result_unavailable_oversized_or_invalid",
                    ),
                }
                break;
            }
        }
        let mut remaining = Vec::new();
        for criterion in &goal.criteria {
            let mut single = goal.clone();
            single.criteria = vec![criterion.clone()];
            let passed = if let Some((stage, result)) = &latest {
                if stage.status == "cancelled" {
                    false
                } else {
                    match self.criteria_pass(&single, &result.outcome) {
                        Ok(passed) => passed,
                        Err(_) => {
                            omit(
                                &mut omissions,
                                &goal_source.path,
                                "criterion_evidence_unavailable",
                            );
                            false
                        }
                    }
                }
            } else {
                false
            };
            if !passed {
                remaining.push(criterion.clone());
            }
        }
        inputs.extend(decisions);
        let mut total = 0;
        let mut accepted = Vec::new();
        for input in inputs {
            if accepted.len() >= MAX_INPUTS
                || total + input.citation.excerpt.len() > MAX_INPUT_BYTES
            {
                omit(&mut omissions, &input.citation.path, "brief_input_budget");
            } else {
                total += input.citation.excerpt.len();
                accepted.push(input);
            }
        }
        let mut value = serde_json::json!({"schema":"okilum-goal-brief/v1", "goal_id":goal_id,
            "goal_revision":goal_source.revision, "inputs":accepted, "remaining_criteria":remaining,
            "omissions":omissions, "complete":omissions.is_empty()});
        // Keep pre-policy JSON and its generation byte-exact when absent.
        if manual_only_count > 0 {
            let truncated = manual_only_count > manual_only.len();
            value["manual_only"] = json!(manual_only);
            if truncated {
                value["manual_only_truncated"] = json!(true);
            }
        }
        value["generation"] = serde_json::json!(retrieval::sha(&serde_json::to_vec(&value)?));
        Ok(value)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::{
        application::Application,
        attention::{Mutation, Target},
        inbox::SourceIdentity,
    };
    use okilum_core::source::WriteBoundary;

    pub(crate) fn fixture() -> (tempfile::TempDir, Runner, RunnerConfig, String, String) {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("brain/records")).unwrap();
        fs::create_dir(temp.path().join("state")).unwrap();
        let config = RunnerConfig {
            brain_id: Uuid::new_v4().to_string(),
            root: temp.path().join("brain"),
            operational_dir: temp.path().join("state"),
            records_dir: "records".into(),
            boundary: WriteBoundary::Managed,
        };
        let (runner, a, b) = fixture_goals(&config);
        (temp, runner, config, a, b)
    }
    pub(crate) fn fixture_goals(config: &RunnerConfig) -> (Runner, String, String) {
        let mut runner = Runner::open(RunnerConfig {
            brain_id: config.brain_id.clone(),
            root: config.root.clone(),
            operational_dir: config.operational_dir.clone(),
            records_dir: config.records_dir.clone(),
            boundary: config.boundary,
        })
        .unwrap();
        let mut goals = Vec::new();
        for title in ["Goal A", "Goal B"] {
            let id = Uuid::new_v4().to_string();
            runner
                .create_goal(
                    Goal {
                        id: id.clone(),
                        title: title.into(),
                        status: "draft".into(),
                        criteria: vec![Criterion {
                            id: "C1".into(),
                            description: "Actual behavior must be observed".into(),
                            requires_human: true,
                        }],
                        stage_ids: vec![],
                        task_ref: None,
                        extra: BTreeMap::new(),
                    },
                    "# Original goal\n".into(),
                )
                .unwrap();
            goals.push(id);
        }
        (runner, goals[0].clone(), goals[1].clone())
    }
    fn raw(runner: &mut Runner) -> Result<Vec<Value>> {
        Application::unconfigured().attention_items(runner)
    }
    pub(crate) fn save_reply(
        runner: &mut Runner,
        goal: &str,
        text: &str,
    ) -> crate::attention::Outcome {
        runner
            .with_goal(goal, |r| {
                r.attention("decision", "Choose next action");
                r.persist()
            })
            .unwrap();
        let items = raw(runner).unwrap();
        let listed = runner
            .attention_list(items, "operator", None, None, None)
            .unwrap();
        let item = listed
            .items
            .iter()
            .find(|item| item.goal_id == goal)
            .unwrap();
        let operation = Uuid::new_v4().to_string();
        runner
            .attention_mutate(
                Mutation {
                    operation_id: operation.clone(),
                    target: Target {
                        goal_id: goal.into(),
                        attention_id: item.attention_id.clone(),
                        expected_revision: item.revision.clone(),
                        stage_id: item.stage_id.clone(),
                    },
                    source: SourceIdentity {
                        channel: "native".into(),
                        instance_id: Uuid::new_v4().to_string(),
                        account_id: "local".into(),
                        actor_id: "operator".into(),
                        chat_id: None,
                        topic_id: None,
                        message_id: operation.clone(),
                        update_id: operation,
                        uri: None,
                    },
                },
                "save_decision",
                Some(text),
                "operator",
                raw,
            )
            .unwrap()
    }
    fn brief(runner: &mut Runner, goal: &str) -> Value {
        runner
            .with_goal(goal, |r| r.goal_context_brief(goal))
            .unwrap()
    }
    pub(crate) fn add_result(runner: &mut Runner, goal_id: &str) -> String {
        runner
            .with_goal(goal_id, |r| {
                let stage_id = Uuid::new_v4().to_string();
                let result_id = Uuid::new_v4().to_string();
                let result = ResultRecord {
                    id: result_id.clone(),
                    goal_id: goal_id.into(),
                    stage_id: stage_id.clone(),
                    operation_id: Uuid::new_v4().to_string(),
                    engine_ref: EngineRef {
                        engine: "t3".into(),
                        instance_id: "fixture".into(),
                        thread_id: Some("retained-thread".into()),
                        turn_id: Some("retained-turn".into()),
                        task_id: None,
                    },
                    received_at: "2026-09-07T03:00:00Z".into(),
                    outcome: Outcome {
                        outcome: "succeeded".into(),
                        summary: "Saved outcome A; actual behavior remains unverified".into(),
                        sources: vec![],
                        evidence: vec![],
                        verification: "unverified".into(),
                        criterion_evaluations: vec![],
                    },
                };
                r.queue(
                    "result",
                    &result_id,
                    &result,
                    Some("# Saved outcome A\nActual behavior remains unverified.\n"),
                )?;
                let stage = Stage {
                    id: stage_id.clone(),
                    goal_id: goal_id.into(),
                    engine: "t3".into(),
                    status: "outcome_ready".into(),
                    criterion_ids: vec!["C1".into()],
                    context_id: Uuid::new_v4().to_string(),
                    result_ids: vec![result_id.clone()],
                    extra: BTreeMap::new(),
                };
                r.queue("stage", &stage_id, &stage, None)?;
                let (mut goal, _) = r.record::<Goal>("goal", goal_id)?;
                goal.stage_ids.push(stage_id);
                r.queue("goal", goal_id, &goal, None)?;
                r.persist()?;
                r.flush_writes()?;
                Ok(result_id)
            })
            .unwrap()
    }
    #[test]
    fn saved_reply_and_latest_result_are_owned_exact_once_read_only_and_restart_stable() {
        let (_temp, mut r, config, a, b) = fixture();
        let reply = save_reply(
            &mut r,
            &a,
            "Use the existing sources.\r\nDo not invent a launch test.\r\n",
        );
        let result = add_result(&mut r, &a);
        let state = fs::read(config.operational_dir.join("state.json")).unwrap();
        let source_before = fs::read(config.root.join(reply.path.as_ref().unwrap())).unwrap();
        let first = brief(&mut r, &a);
        assert_eq!(first["inputs"].as_array().unwrap().len(), 2);
        assert_eq!(first["inputs"][0]["id"], result);
        assert_eq!(first["inputs"][1]["id"], reply.decision_id.unwrap());
        assert_eq!(first["inputs"][1]["actor_id"], "operator");
        assert_eq!(first["inputs"][1]["verification"], "unverified");
        assert_eq!(
            first["inputs"][1]["citation"]["excerpt"]
                .as_str()
                .unwrap()
                .as_bytes(),
            source_before
        );
        assert_eq!(brief(&mut r, &b)["inputs"], serde_json::json!([]));
        assert_eq!(
            fs::read(config.operational_dir.join("state.json")).unwrap(),
            state
        );
        assert_eq!(
            fs::read(config.root.join(reply.path.as_ref().unwrap())).unwrap(),
            source_before
        );
        drop(r);
        let mut r = Runner::open(config).unwrap();
        assert_eq!(brief(&mut r, &a), first);
    }
    #[test]
    fn brief_citations_seed_existing_packet_without_replacing_pins_and_become_stale() {
        let (_temp, mut r, config, a, _b) = fixture();
        save_reply(&mut r, &a, "An earlier manual pin.");
        let manual: Citation =
            serde_json::from_value(brief(&mut r, &a)["inputs"][0]["citation"].clone()).unwrap();
        let reply = save_reply(&mut r, &a, "Preserve this attributed reply.");
        let data = brief(&mut r, &a);
        let citation: Citation = serde_json::from_value(
            data["inputs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["id"] == reply.decision_id.as_ref().unwrap().as_str())
                .unwrap()["citation"]
                .clone(),
        )
        .unwrap();
        let app = Application::unconfigured();
        r.with_goal(&a, |r| {
            let scope = crate::retrieval::SearchScope {
                goal_id: a.clone(),
                mode: "goal".into(),
                ..Default::default()
            };
            let packet = app.context_prepare(
                r,
                a.clone(),
                "Prepare next action".into(),
                scope,
                vec![manual.clone(), citation.clone()],
                vec![manual.citation_id.clone(), citation.citation_id.clone()],
            )?["packet"]
                .clone();
            let frozen = app.context_revise(
                r,
                a.clone(),
                packet["id"].as_str().unwrap().into(),
                packet["revision"].as_str().unwrap().into(),
                packet["text"].as_str().unwrap().into(),
            )?["packet"]
                .clone();
            let reference = crate::context::ReviewedPacketRef {
                id: frozen["id"].as_str().unwrap().into(),
                revision: frozen["revision"].as_str().unwrap().into(),
            };
            let (current, _) = crate::context::require_reviewed(r, &a, &reference)?;
            assert_eq!(
                current.pinned_citation_ids,
                vec![manual.citation_id.clone(), citation.citation_id.clone()]
            );
            assert!(current.citations[0].excerpt.contains("actor_id: operator"));
            let before = r.read_source(&r.path("reviewed-context", &current.id))?;
            assert_eq!(
                r.goal_context_brief(&a)?["inputs"]
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
            assert_eq!(r.read_source(&before.path)?.revision, before.revision);
            let path = config.root.join(reply.path.as_ref().unwrap());
            fs::write(
                &path,
                fs::read_to_string(&path)?.replace("Preserve this", "Revised: preserve this"),
            )?;
            assert!(crate::context::require_reviewed(r, &a, &reference).is_err());
            assert_ne!(
                r.goal_context_brief(&a)?["inputs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| v["id"] == reply.decision_id.as_ref().unwrap().as_str())
                    .unwrap()["citation"]["revision"],
                citation.revision
            );
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn missing_oversized_and_changed_owner_records_are_visible_omissions() {
        let (_temp, mut r, config, a, b) = fixture();
        let missing = save_reply(&mut r, &a, "Missing decision");
        let changed = save_reply(&mut r, &a, "Ownership must not change");
        let huge = save_reply(&mut r, &a, &"x".repeat(9000));
        fs::remove_file(config.root.join(missing.path.unwrap())).unwrap();
        let path = config.root.join(changed.path.unwrap());
        fs::write(&path, fs::read_to_string(&path).unwrap().replace(&a, &b)).unwrap();
        let data = brief(&mut r, &a);
        assert_eq!(data["inputs"], serde_json::json!([]));
        assert_eq!(data["omissions"].as_array().unwrap().len(), 3);
        assert_eq!(data["complete"], false);
        assert!(data["omissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["path"] == huge.path.as_ref().unwrap().as_str()));
        assert_eq!(brief(&mut r, &b)["inputs"], serde_json::json!([]));
    }
    #[test]
    fn missing_latest_result_never_substitutes_an_older_outcome() {
        let (_temp, mut r, config, a, _) = fixture();
        let older = add_result(&mut r, &a);
        let latest = add_result(&mut r, &a);
        assert_eq!(brief(&mut r, &a)["inputs"][0]["id"], latest);
        fs::remove_file(config.root.join(r.path("result", &latest))).unwrap();
        let data = brief(&mut r, &a);
        assert_eq!(data["inputs"], serde_json::json!([]));
        assert_eq!(data["complete"], false);
        assert!(data["omissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["path"] == r.path("result", &latest)));
        assert!(!data.to_string().contains(&older));
    }
    #[test]
    fn canonical_only_replies_remain_readable_and_budget_omissions_are_explicit() {
        let (_temp, mut r, _config, a, b) = fixture();
        for _ in 0..21 {
            save_reply(&mut r, &a, "An attributed saved input.");
        }
        let before = brief(&mut r, &a);
        assert_eq!(before["inputs"].as_array().unwrap().len(), MAX_INPUTS);
        assert_eq!(before["omissions"].as_array().unwrap().len(), 1);
        assert_eq!(before["omissions"][0]["code"], "brief_input_budget");
        r.state.attention_journal = Default::default();
        assert_eq!(brief(&mut r, &a), before);
        assert_eq!(brief(&mut r, &b)["inputs"], serde_json::json!([]));
    }
    #[test]
    fn chronological_order_preserves_original_fractional_and_utc_timestamp_spellings() {
        let (_temp, mut r, config, a, _) = fixture();
        let mut ids = Vec::new();
        for at in [
            "2026-09-07T04:00:00Z",
            "2026-09-07T04:00:00.1Z",
            "2026-09-07T04:00:00.2+00:00",
        ] {
            let reply = save_reply(&mut r, &a, "Timestamp fixture.");
            let path = config.root.join(reply.path.as_ref().unwrap());
            let source = fs::read_to_string(&path).unwrap();
            fs::write(
                &path,
                source.replace(reply.received_at.as_ref().unwrap(), at),
            )
            .unwrap();
            ids.push((reply.decision_id.unwrap(), at));
        }
        let data = brief(&mut r, &a);
        for (input, (id, at)) in data["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .zip(ids.iter().rev())
        {
            assert_eq!(input["id"], *id);
            assert_eq!(input["received_at"], *at);
            assert!(input["citation"]["excerpt"].as_str().unwrap().contains(at));
        }
        assert_eq!(data["inputs"].as_array().unwrap().len(), 3);
    }
    #[test]
    fn restored_replies_with_invalid_source_or_conflicting_actor_are_omitted() {
        let (_temp, mut r, config, a, _) = fixture();
        for (from, to) in [
            ("channel: native", "channel: unknown"),
            ("  actor_id: operator", "  actor_id: someone-else"),
            ("account_id: local", "account_id: ''"),
        ] {
            let reply = save_reply(&mut r, &a, "Restored provenance fixture.");
            let path = config.root.join(reply.path.unwrap());
            let text = fs::read_to_string(&path).unwrap();
            assert!(text.contains(from));
            fs::write(&path, text.replace(from, to)).unwrap();
        }
        r.state.attention_journal = Default::default();
        let data = brief(&mut r, &a);
        assert_eq!(data["inputs"], serde_json::json!([]));
        assert_eq!(data["omissions"].as_array().unwrap().len(), 3);
        assert_eq!(data["complete"], false);
    }
    pub(crate) fn passing_result(r: &mut Runner, goal_id: &str, large: bool) -> String {
        let result_id = add_result(r, goal_id);
        r.with_goal(goal_id, |r| {
            let (mut goal, _) = r.record::<Goal>("goal", goal_id)?;
            goal.criteria[0].requires_human = false;
            goal.status = "completed".into();
            r.queue("goal", goal_id, &goal, None)?;
            r.persist()?;
            r.flush_writes()?;
            let (_, source) = r.record::<Goal>("goal", goal_id)?;
            r.state.retained_goal = Some(source.clone());
            let (mut result, _) = r.record::<ResultRecord>("result", &result_id)?;
            result.outcome.verification = "verified".into();
            result.outcome.summary = if large {
                "S".repeat(24_210)
            } else {
                "Validated outcome".into()
            };
            result.outcome.evidence = vec![Evidence {
                id: "saved-observation".into(),
                kind: "artifact".into(),
                source: SourceRef {
                    uri: "fixture://observed-behavior".into(),
                    revision: None,
                    locator: None,
                },
                description: if large {
                    "E".repeat(24_210)
                } else {
                    "Observed".into()
                },
                observed_at: "2026-09-07T03:00:01Z".into(),
                status: "passed".into(),
            }];
            result.outcome.criterion_evaluations = vec![CriterionEvaluation {
                criterion_id: "C1".into(),
                goal_revision: source.revision,
                status: "passed".into(),
                evidence_ids: vec!["saved-observation".into()],
                evaluated_by: "Fixture supervisor".into(),
                evaluated_at: "2026-09-07T03:00:02Z".into(),
            }];
            let body = if large {
                "B".repeat(49_642)
            } else {
                "# Saved result\n".into()
            };
            r.queue("result", &result_id, &result, Some(&body))?;
            let (mut stage, _) = r.record::<Stage>("stage", &result.stage_id)?;
            stage.status = "completed".into();
            r.queue("stage", &stage.id, &stage, None)?;
            r.persist()?;
            r.flush_writes()?;
            assert!(r.criteria_pass(&goal, &result.outcome)?);
            if large {
                let path = r.root.join(r.path("result", &result_id));
                let mut bytes = fs::read(&path)?;
                assert!(bytes.len() < 102_462);
                bytes.resize(102_462, b' ');
                fs::write(path, bytes)?;
            }
            Ok(())
        })
        .unwrap();
        result_id
    }

    #[test]
    fn passed_criteria_do_not_depend_on_citation_size_and_reads_preserve_completion() {
        for large in [false, true] {
            let (_temp, mut r, config, a, b) = fixture();
            let result = passing_result(&mut r, &a, large);
            let path = r.path("result", &result);
            let before = fs::read(config.root.join(&path)).unwrap();
            let state = fs::read(config.operational_dir.join("state.json")).unwrap();
            let data = brief(&mut r, &a);
            assert_eq!(data["remaining_criteria"], serde_json::json!([]));
            assert_eq!(
                data["inputs"].as_array().unwrap().len(),
                usize::from(!large)
            );
            assert_eq!(data["complete"], !large);
            if large {
                assert_eq!(before.len(), 102_462);
                assert_eq!(data["omissions"][0]["path"], path);
            }
            assert_eq!(fs::read(config.root.join(&path)).unwrap(), before);
            assert_eq!(
                fs::read(config.operational_dir.join("state.json")).unwrap(),
                state
            );
            assert_eq!(brief(&mut r, &b)["inputs"], serde_json::json!([]));
            drop(r);
            let mut r = Runner::open(config).unwrap();
            assert_eq!(brief(&mut r, &a), data);
            assert_eq!(
                r.with_goal(&a, |r| r.record::<Goal>("goal", &a))
                    .unwrap()
                    .0
                    .status,
                "completed"
            );
        }
    }

    #[test]
    fn large_result_validation_and_cancelled_stage_cannot_invent_passed_criteria() {
        for case in [
            "missing",
            "wrong_goal",
            "wrong_stage",
            "wrong_id",
            "wrong_brain",
            "bad_schema",
            "bad_type",
            "bad_time",
            "bad_verification",
            "malformed",
            "over_read_bound",
            "cancelled",
        ] {
            let (_temp, mut r, config, a, b) = fixture();
            let result = passing_result(&mut r, &a, true);
            let path = config.root.join(r.path("result", &result));
            let (record, _) = r
                .with_goal(&a, |r| r.record::<ResultRecord>("result", &result))
                .unwrap();
            let source = fs::read_to_string(&path).unwrap();
            let changed = match case {
                "missing" => {
                    fs::remove_file(&path).unwrap();
                    None
                }
                "wrong_goal" => Some(source.replace(&a, &b)),
                "wrong_stage" => {
                    Some(source.replace(&record.stage_id, &Uuid::new_v4().to_string()))
                }
                "wrong_id" => Some(source.replace(&result, &Uuid::new_v4().to_string())),
                "wrong_brain" => {
                    Some(source.replace(&config.brain_id, &Uuid::new_v4().to_string()))
                }
                "bad_schema" => {
                    Some(source.replace("schema: ai-brain/v1", "schema: ai-brain/invalid"))
                }
                "bad_type" => Some(source.replace("record_type: result", "record_type: decision")),
                "bad_time" => Some(source.replace("2026-09-07T03:00:00Z", "invalid-time")),
                "bad_verification" => {
                    Some(source.replace("verification: verified", "verification: fabricated"))
                }
                "malformed" => Some("---\nnot: [valid\n---\n".into()),
                "over_read_bound" => {
                    Some(format!("{source}{}", "x".repeat(MAX_RESULT_BYTES as usize)))
                }
                "cancelled" => {
                    let stage_path = config.root.join(r.path("stage", &record.stage_id));
                    let stage = fs::read_to_string(&stage_path).unwrap();
                    fs::write(
                        stage_path,
                        stage.replace("status: completed", "status: cancelled"),
                    )
                    .unwrap();
                    None
                }
                _ => unreachable!(),
            };
            if let Some(changed) = changed {
                assert_ne!(changed, source, "{case}");
                fs::write(&path, changed).unwrap();
            }
            let data = brief(&mut r, &a);
            assert_eq!(data["remaining_criteria"][0]["id"], "C1", "{case}: {data}");
            assert_eq!(data["inputs"], serde_json::json!([]), "{case}");
            assert_eq!(data["complete"], false, "{case}");
        }
    }
}
