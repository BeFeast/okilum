//! Discussion receipts exercise the real application/checkpoint/canonical path.
use super::goal_brief::tests::{add_result, fixture, save_reply};
use super::*;
use crate::application::Application;
use crate::chat::{ChatClient, ChatConfig, ChatEvent};
use serde_json::json;
use std::path::Path;

fn app() -> Application {
    let mut app = Application::unconfigured();
    app.chat = Some(ChatConfig {
        base_url: "http://127.0.0.1:1/v1".into(),
        model: "fixture-model".into(),
        api_key: "synthetic".into(),
        idle_timeout: std::time::Duration::from_secs(1),
    });
    app
}
fn start(
    r: &mut Runner,
    goal: &str,
    text: &str,
    paths: Vec<String>,
    id: Option<String>,
) -> Result<crate::discussion_context::Prepared> {
    r.with_goal(goal, |r| {
        app().chat_start_prepared(r, goal.into(), text.into(), paths, id)
    })
}
fn read(r: &mut Runner, goal: &str, id: &str, turn: Option<&str>) -> Value {
    r.with_goal(goal, |r| app().chat_get_context(r, id, turn))
        .unwrap()
}
fn complete(r: &mut Runner, goal: &str, id: &str, text: &str) {
    r.with_goal(goal, |r| {
        Application::chat_event(r, id, ChatEvent::Complete { text: text.into() })
    })
    .unwrap();
}
fn metadata(path: &Path) -> (serde_yaml::Mapping, String) {
    let text = fs::read_to_string(path).unwrap();
    let rest = text.strip_prefix("---\n").unwrap();
    let (yaml, body) = rest.split_once("---\n").unwrap();
    (serde_yaml::from_str(yaml).unwrap(), body.into())
}
fn rewrite(path: &Path, edit: impl FnOnce(&mut serde_yaml::Mapping)) {
    let (mut m, body) = metadata(path);
    edit(&mut m);
    fs::write(
        path,
        format!("---\n{}---\n{body}", serde_yaml::to_string(&m).unwrap()),
    )
    .unwrap();
}

#[test]
fn discussion_freezes_saved_inputs_latest_result_manual_other_goal_and_reopen() {
    let (_temp, mut r, config, a, b) = fixture();
    let a_reply = save_reply(&mut r, &a, "Preserve exact A decision.\r\n");
    let b_reply = save_reply(&mut r, &b, "Explicit cross-goal manual reference B.");
    let result = add_result(&mut r, &a);
    // The selected/newest stage is empty; the previous stage still owns the latest result.
    r.with_goal(&a, |r| {
        let id = Uuid::new_v4().to_string();
        r.queue(
            "stage",
            &id,
            &Stage {
                id: id.clone(),
                goal_id: a.clone(),
                engine: "t3".into(),
                status: "prepared".into(),
                criterion_ids: vec!["C1".into()],
                context_id: Uuid::new_v4().to_string(),
                result_ids: vec![],
                extra: BTreeMap::new(),
            },
            None,
        )?;
        let (mut g, _) = r.record::<Goal>("goal", &a)?;
        g.stage_ids.push(id.clone());
        r.state.stage_id = Some(id);
        r.queue("goal", &a, &g, None)?;
        r.persist()?;
        r.flush_writes()
    })
    .unwrap();
    let prepared = start(
        &mut r,
        &a,
        "First exact request\r\n",
        vec![a_reply.path.clone().unwrap(), b_reply.path.clone().unwrap()],
        None,
    )
    .unwrap();
    assert_eq!(
        prepared.body,
        ChatClient::prepare_body(&prepared.config.model, &prepared.request).unwrap()
    );
    let first = read(&mut r, &a, &prepared.id, Some(&prepared.turn_id))["context_snapshot"]
        ["snapshot"]
        .clone();
    assert_eq!(
        first["request_sha256"],
        crate::retrieval::sha(prepared.body.as_bytes())
    );
    assert_eq!(first["limits"]["request_bytes"], prepared.body.len());
    let inputs = first["inputs"].as_array().unwrap();
    assert_eq!(inputs.len(), 3);
    let saved = inputs
        .iter()
        .find(|i| i["id"] == a_reply.decision_id.clone().unwrap())
        .unwrap();
    assert_eq!(saved["reasons"], json!(["manual", "saved_decision"]));
    assert_eq!(saved["source_identity"]["actor_id"], "operator");
    assert_eq!(
        saved["text"],
        fs::read_to_string(config.root.join(a_reply.path.as_ref().unwrap())).unwrap()
    );
    let manual = inputs
        .iter()
        .find(|i| i["id"] == b_reply.decision_id.clone().unwrap())
        .unwrap();
    assert_eq!(manual["reasons"], json!(["manual"]));
    assert_eq!(manual["owner_goal_id"], b);
    assert_eq!(manual["verification"], "unverified");
    assert!(inputs
        .iter()
        .any(|i| i["id"] == result && i["reasons"] == json!(["latest_result"])));
    assert!(!prepared
        .request
        .context
        .decisions
        .iter()
        .any(|s| s.contains("Explicit cross-goal")));
    assert!(prepared
        .request
        .context
        .previous_result
        .as_ref()
        .unwrap()
        .contains(&result));
    complete(&mut r, &a, &prepared.id, "First answer");
    let changed_path = config.root.join(a_reply.path.unwrap());
    let changed = fs::read_to_string(&changed_path).unwrap().replace(
        "Preserve exact A decision.",
        "Revised A decision after first request.",
    );
    fs::write(changed_path, changed).unwrap();
    let second = start(
        &mut r,
        &a,
        "Second request",
        vec![],
        Some(prepared.id.clone()),
    )
    .unwrap();
    complete(&mut r, &a, &prepared.id, "Second answer");
    assert_eq!(
        read(&mut r, &a, &prepared.id, Some(&prepared.turn_id))["context_snapshot"]["snapshot"],
        first
    );
    assert_ne!(second.turn_id, prepared.turn_id);
    drop(r);
    let mut r = Runner::open(config).unwrap();
    assert_eq!(
        read(&mut r, &a, &prepared.id, Some(&prepared.turn_id))["context_snapshot"]["snapshot"],
        first
    );
    assert_eq!(
        read(&mut r, &a, &prepared.id, None)["turn_contexts"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn discussion_serialized_budget_and_utf8_refuse_before_mutation() {
    let (_temp, mut r, config, a, _b) = fixture();
    let first = start(&mut r, &a, "Initial", vec![], None).unwrap();
    complete(&mut r, &a, &first.id, "Answer");
    let before = fs::read(config.operational_dir.join("state.json")).unwrap();
    let path = config.root.join(r.path("conversation", &first.id));
    let source = fs::read(&path).unwrap();
    // Small raw text becomes too large when JSON escapes every control byte.
    assert!(start(
        &mut r,
        &a,
        &"\u{1}".repeat(50_000),
        vec![],
        Some(first.id.clone())
    )
    .err()
    .unwrap()
    .to_string()
    .contains("chat_context_limit"));
    fs::write(config.root.join("invalid.md"), [0xff, 0xfe]).unwrap();
    assert!(start(
        &mut r,
        &a,
        "Valid message",
        vec!["invalid.md".into()],
        Some(first.id.clone())
    )
    .is_err());
    assert_eq!(
        fs::read(config.operational_dir.join("state.json")).unwrap(),
        before
    );
    assert_eq!(fs::read(&path).unwrap(), source);
    // No arbitrary 64 KiB cap for ordinary user or manual text.
    fs::write(config.root.join("large.md"), "m".repeat(70_000)).unwrap();
    let large = start(
        &mut r,
        &a,
        &"u".repeat(70_000),
        vec!["large.md".into()],
        Some(first.id),
    )
    .unwrap();
    assert!(large.body.len() <= crate::discussion_context::MAX_REQUEST);
}

#[test]
fn discussion_missing_corrupt_future_and_transcript_binding_are_never_reconstructed() {
    for case in ["legacy", "corrupt", "future", "binding"] {
        let (_temp, mut r, config, a, _b) = fixture();
        let p = start(&mut r, &a, "First", vec![], None).unwrap();
        complete(&mut r, &a, &p.id, "Answer");
        let path = config.root.join(r.path("conversation", &p.id));
        rewrite(&path, |m| {
            let key = serde_yaml::Value::String("discussion_context".into());
            match case {
                "legacy" => {
                    m.remove(&key);
                }
                "corrupt" => {
                    m.insert(key, serde_yaml::Value::String("broken".into()));
                }
                "future" => {
                    m.get_mut(&key).unwrap()["schema"] =
                        serde_yaml::Value::String("tessera-discussion-context/v999".into())
                }
                "binding" => {
                    m.get_mut(&key).unwrap()["conversation_id"] =
                        serde_yaml::Value::String(Uuid::new_v4().to_string())
                }
                _ => unreachable!(),
            }
        });
        let before = fs::read(&path).unwrap();
        let view = read(&mut r, &a, &p.id, None);
        assert_eq!(view["turn_contexts"], json!([]));
        assert_eq!(view["context_history"]["unrecorded_turn_count"], 1);
        if case == "legacy" {
            let next = start(&mut r, &a, "Next", vec![], Some(p.id.clone())).unwrap();
            let view = read(&mut r, &a, &p.id, Some(&next.turn_id));
            assert_eq!(
                view["context_history"]["unrecorded_reason"],
                "historical_context_unavailable"
            );
            assert_eq!(view["turn_contexts"][0]["availability"], "available");
        } else {
            assert!(start(&mut r, &a, "Next", vec![], Some(p.id.clone())).is_err());
            assert_eq!(fs::read(&path).unwrap(), before);
            let view = read(&mut r, &a, &p.id, Some(&p.turn_id));
            assert!(view["context_snapshot"]["snapshot"].is_null());
        }
    }
}

#[test]
fn discussion_maximum_escaped_assistant_projection_is_readable_and_receipt_unchanged() {
    let (_temp, mut r, config, a, _b) = fixture();
    let p = start(&mut r, &a, "Request", vec![], None).unwrap();
    let before = read(&mut r, &a, &p.id, Some(&p.turn_id))["context_snapshot"].clone();
    // YAML uses six bytes for several controls; raw readable-body duplication is additional.
    let output = "\u{1}\u{7}\u{b}\u{1b}".repeat(2 * 1024 * 1024);
    assert_eq!(output.len(), 8 * 1024 * 1024);
    complete(&mut r, &a, &p.id, &output);
    let path = config.root.join(r.path("conversation", &p.id));
    assert!(fs::metadata(&path).unwrap().len() <= crate::discussion_context::MAX_CONVERSATION);
    assert_eq!(
        read(&mut r, &a, &p.id, Some(&p.turn_id))["context_snapshot"],
        before
    );
    drop(r);
    let mut r = Runner::open(config).unwrap();
    assert_eq!(
        read(&mut r, &a, &p.id, Some(&p.turn_id))["context_snapshot"],
        before
    );
}

#[test]
fn discussion_after_source_crash_retains_pending_receipt_and_recovers_without_retry() {
    let (_temp, mut r, config, a, _b) = fixture();
    r.interrupt_after_write = Some(1);
    assert!(start(&mut r, &a, "Durable intent", vec![], None).is_err());
    let owner = r
        .with_goal(&a, |r| Ok(r.state.application.clone()))
        .unwrap();
    let id = owner["selected_conversation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let queued = r.state.pending_writes.first().unwrap().clone();
    let bytes = STANDARD.decode(&queued.content_base64).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("discussion_context:"));
    drop(r);
    let mut r = Runner::open(config).unwrap();
    r.with_goal(&a, |r| app().recover(r)).unwrap();
    let view = read(&mut r, &a, &id, None);
    assert_eq!(view["status"], "interrupted");
    assert_eq!(view["turn_contexts"][0]["availability"], "available");
    assert!(r.state.pending_writes.is_empty());
    let id2 = start(&mut r, &a, "Explicit retry", vec![], Some(id.clone())).unwrap();
    assert_eq!(
        read(&mut r, &a, &id, Some(&id2.turn_id))["turn_contexts"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn discussion_passed_criteria_are_still_in_exact_provider_context() {
    let (_temp, mut r, _config, a, _b) = fixture();
    super::goal_brief::tests::passing_result(&mut r, &a, false);
    let p = start(
        &mut r,
        &a,
        "Remember the complete original criteria",
        vec![],
        None,
    )
    .unwrap();
    let snapshot =
        read(&mut r, &a, &p.id, Some(&p.turn_id))["context_snapshot"]["snapshot"].clone();
    assert_eq!(snapshot["remaining_criteria"], json!([]));
    let observation = p
        .request
        .context
        .constraints
        .last()
        .unwrap()
        .strip_prefix("Saved-context observation (reference data): ")
        .unwrap();
    let observation: Value = serde_json::from_str(observation).unwrap();
    assert_eq!(
        observation["goal"]["criteria"],
        snapshot["goal"]["criteria"]
    );
    assert_eq!(observation["goal"]["criteria"][0]["id"], "C1");
    assert_eq!(observation["goal"]["criteria"][0]["requires_human"], false);
    assert_eq!(
        snapshot["request_sha256"],
        crate::retrieval::sha(p.body.as_bytes())
    );
}

#[test]
fn discussion_failure_before_or_after_journal_preserves_correct_intent_boundary() {
    for phase in [1, 2] {
        let (_temp, mut r, config, a, _b) = fixture();
        let before = fs::read(config.operational_dir.join("state.json")).unwrap();
        r.discussion_fault = Some(phase);
        assert!(start(&mut r, &a, "Prepared, delivery is unknown", vec![], None).is_err());
        if phase == 1 {
            assert_eq!(
                fs::read(config.operational_dir.join("state.json")).unwrap(),
                before
            );
            assert!(r.state.pending_writes.is_empty());
        } else {
            assert_eq!(r.state.pending_writes.len(), 1);
            let pending = r.state.pending_writes[0].clone();
            assert!(!config.root.join(&pending.path).exists());
            let exact_bytes = STANDARD.decode(&pending.content_base64).unwrap();
            drop(r);
            let mut r = Runner::open(config).unwrap();
            assert_eq!(
                STANDARD
                    .decode(r.read_source(&pending.path).unwrap().content_base64)
                    .unwrap(),
                exact_bytes
            );
            let state = r
                .with_goal(&a, |r| {
                    app().recover(r)?;
                    Ok(r.state.application.clone())
                })
                .unwrap();
            let id = state["selected_conversation_id"].as_str().unwrap();
            assert_eq!(read(&mut r, &a, id, None)["status"], "interrupted");
        }
    }
}

#[test]
fn discussion_automatic_escaping_budget_omits_without_rejecting_mandatory_request() {
    let (_temp, mut r, _config, a, _b) = fixture();
    // JSON nested reference strings escape every backslash again. The brief's
    // raw 48 KiB admission cannot stand in for the actual provider contribution.
    for _ in 0..10 {
        save_reply(&mut r, &a, &"\\\"".repeat(2200));
    }
    let p = start(&mut r, &a, "Keep the mandatory request", vec![], None).unwrap();
    let s = read(&mut r, &a, &p.id, Some(&p.turn_id))["context_snapshot"]["snapshot"].clone();
    assert!(s["limits"]["automatic_bytes"].as_u64().unwrap() <= 48 * 1024);
    assert!(
        s["omissions"]["counts"]["chat_automatic_budget"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(!s["inputs"].as_array().unwrap().is_empty());
    assert!(p.body.len() <= crate::discussion_context::MAX_REQUEST);
}

#[test]
fn discussion_turn_limit_never_evicts_earlier_receipts() {
    let (_temp, mut r, config, a, _b) = fixture();
    let first = start(&mut r, &a, "1", vec![], None).unwrap();
    complete(&mut r, &a, &first.id, "ok");
    let receipt = read(&mut r, &a, &first.id, Some(&first.turn_id))["context_snapshot"].clone();
    for _ in 1..64 {
        start(&mut r, &a, "next", vec![], Some(first.id.clone())).unwrap();
        complete(&mut r, &a, &first.id, "ok");
    }
    let source = config.root.join(r.path("conversation", &first.id));
    let before = fs::read(&source).unwrap();
    assert!(
        start(&mut r, &a, "65 refused", vec![], Some(first.id.clone()))
            .err()
            .unwrap()
            .to_string()
            .contains("retained turn limit")
    );
    assert_eq!(fs::read(&source).unwrap(), before);
    assert_eq!(
        read(&mut r, &a, &first.id, Some(&first.turn_id))["context_snapshot"],
        receipt
    );
    assert!(!String::from_utf8(before).unwrap().contains("transcript:\n"));
}

#[test]
#[ignore = "requires empty TESSERA_DISCUSSION_FIXTURE directory"]
fn export_discussion_service_fixture() {
    let target = PathBuf::from(std::env::var_os("TESSERA_DISCUSSION_FIXTURE").unwrap());
    assert!(target.is_dir() && fs::read_dir(&target).unwrap().next().is_none());
    fs::create_dir_all(target.join("brain/records")).unwrap();
    fs::create_dir(target.join("state")).unwrap();
    let config = RunnerConfig {
        brain_id: Uuid::new_v4().to_string(),
        root: target.join("brain"),
        operational_dir: target.join("state"),
        records_dir: "records".into(),
        boundary: tessera_core::source::WriteBoundary::Managed,
    };
    let (mut r, a, b) = super::goal_brief::tests::fixture_goals(&config);
    let reply_a = save_reply(
        &mut r,
        &a,
        "AMBER: keep the local plan; ask before booking.\r\nThe appointment remains unconfirmed.",
    );
    let reply_b = save_reply(&mut r, &b, "COBALT: this decision belongs to another goal.");
    let result = add_result(&mut r, &a);
    let empty_stage = Uuid::new_v4().to_string();
    r.with_goal(&a, |r| {
        let context_id = Uuid::new_v4().to_string();
        let (mut goal, source) = r.record::<Goal>("goal", &a)?;
        r.queue(
            "context",
            &context_id,
            &ContextPacket {
                id: context_id.clone(),
                goal_id: a.clone(),
                stage_id: empty_stage.clone(),
                goal_revision: source.revision,
                goal: goal.title.clone(),
                decisions: vec![],
                constraints: vec![],
                sources: vec![],
                previous_result_id: Some(result.clone()),
                next_step: "Clarify the appointment; fixture stage has never run".into(),
                extra: BTreeMap::new(),
            },
            None,
        )?;
        r.queue(
            "stage",
            &empty_stage,
            &Stage {
                id: empty_stage.clone(),
                goal_id: a.clone(),
                engine: "t3".into(),
                status: "prepared".into(),
                criterion_ids: vec!["C1".into()],
                context_id,
                result_ids: vec![],
                extra: BTreeMap::new(),
            },
            None,
        )?;
        goal.stage_ids.push(empty_stage.clone());
        r.queue("goal", &a, &goal, None)?;
        r.state.stage_id = Some(empty_stage.clone());
        r.persist()?;
        r.flush_writes()
    })
    .unwrap();
    let fixture = json!({"brain_id": config.brain_id, "goal_a": a, "goal_b": b,
        "decision_a": reply_a, "decision_b": reply_b, "result_a": result, "empty_stage_a":empty_stage,
        "decision_a_path":reply_a.path,"decision_b_path":reply_b.path,"result_a_path":r.path("result", &result),"new_stage_a":empty_stage,
        "setup_scope":"Isolated canonical same-goal Attention reply and saved unverified outcome with a newer empty selected stage; separate goal B reply. No Discussion turn, provider request or engine execution."});
    drop(r);
    fs::write(
        target.join("fixture.json"),
        serde_json::to_vec_pretty(&fixture).unwrap(),
    )
    .unwrap();
    let mut reopened = Runner::open(RunnerConfig {
        root: target.join("brain"),
        operational_dir: target.join("state"),
        ..config
    })
    .unwrap();
    assert_eq!(
        reopened
            .with_goal(&a, |r| r.goal_context_brief(&a))
            .unwrap()["inputs"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn discussion_legacy_summary_overhead_stays_constant_for_many_user_messages() {
    let (_temp, mut r, config, a, _b) = fixture();
    let p = start(&mut r, &a, "Captured first turn", vec![], None).unwrap();
    complete(&mut r, &a, &p.id, "Answer");
    // Use the actual compatible Journal checkpoint, as an old writer does.
    r.with_goal(&a, |r| {
        let mut journal = r.application_state();
        let c = &mut journal["conversations"][&p.id];
        let messages = c["messages"].as_array_mut().unwrap();
        for _ in 0..100_000 {
            messages.push(json!({"role":"user","text":"legacy"}));
        }
        let record = c.clone();
        r.checkpoint_application(
            journal,
            Some(("conversation", &p.id, record, "Legacy readable body".into())),
            None,
        )
    })
    .unwrap();
    let view = read(&mut r, &a, &p.id, None);
    assert_eq!(view["turn_contexts"].as_array().unwrap().len(), 1);
    assert_eq!(view["context_history"]["unrecorded_turn_count"], 100_000);
    let added = serde_json::to_vec(&json!({"turn_contexts":view["turn_contexts"],"context_history":view["context_history"],"context_snapshot":view["context_snapshot"]})).unwrap();
    assert!(added.len() < 1024);
    assert_eq!(
        read(&mut r, &a, &p.id, Some(&p.turn_id))["context_snapshot"]["availability"],
        "available"
    );
    let path = config.root.join(r.path("conversation", &p.id));
    rewrite(&path, |m| {
        m.remove(serde_yaml::Value::String("discussion_context".into()));
    });
    let view = read(&mut r, &a, &p.id, None);
    assert_eq!(view["turn_contexts"], json!([]));
    assert_eq!(view["context_history"]["unrecorded_turn_count"], 100_001);
}

#[test]
fn discussion_exact_projection_limit_counts_preserved_unknown_metadata() {
    let (_temp, mut r, config, a, _b) = fixture();
    let p = start(&mut r, &a, "First", vec![], None).unwrap();
    complete(&mut r, &a, &p.id, "Answer");
    let path = config.root.join(r.path("conversation", &p.id));
    // This is within the canonical read bound but exceeds candidate output reserve.
    rewrite(&path, |m| {
        m.insert(
            serde_yaml::Value::String("older_writer_metadata".into()),
            serde_yaml::Value::String("x".repeat(crate::discussion_context::MAX_CANDIDATE)),
        );
    });
    let before_revision = r
        .read_source(&r.path("conversation", &p.id))
        .unwrap()
        .revision;
    let state_before = fs::read(config.operational_dir.join("state.json")).unwrap();
    assert!(start(
        &mut r,
        &a,
        "Must refuse without mutation",
        vec![],
        Some(p.id.clone())
    )
    .err()
    .unwrap()
    .to_string()
    .contains("projection exceeds safe output reserve"));
    assert_eq!(
        r.read_source(&r.path("conversation", &p.id))
            .unwrap()
            .revision,
        before_revision
    );
    assert_eq!(
        fs::read(config.operational_dir.join("state.json")).unwrap(),
        state_before
    );
}

#[test]
fn discussion_cumulative_envelope_limit_is_exact_and_does_not_evict() {
    let (_temp, mut r, config, a, _b) = fixture();
    let p = start(&mut r, &a, "First", vec![], None).unwrap();
    let snapshot =
        read(&mut r, &a, &p.id, Some(&p.turn_id))["context_snapshot"]["snapshot"].clone();
    let path = config.root.join(r.path("conversation", &p.id));
    let (m, _) = metadata(&path);
    let raw = serde_json::to_value(m).unwrap()["discussion_context"].clone();
    let mut envelope: crate::discussion_context::Envelope =
        serde_json::from_value(raw.clone()).unwrap();
    let mut oversized = snapshot;
    oversized["constraints"] = json!(["x".repeat(8 * 1024 * 1024)]);
    let turn = serde_json::from_value(oversized).unwrap();
    assert!(envelope
        .append(turn)
        .err()
        .unwrap()
        .to_string()
        .contains("retained context limit"));
    let (m, _) = metadata(&path);
    assert_eq!(serde_json::to_value(m).unwrap()["discussion_context"], raw);
}

#[test]
fn decision_reuse_manual_selection_is_explicit_and_old_review_stays_stale() {
    use super::discussion_decision::tests::{get, save, turn};
    use tessera_core::decision_reuse::{self as reuse, Disposition};
    let (_dir, mut r, _, a, b) = fixture();
    let key = turn(&mut r, &a, "Unique retained constraint 261", None);
    let view = get(&mut r, &key);
    save(&mut r, &key, &view).unwrap();
    let id = view["decision_id"].as_str().unwrap();
    let path = view["path"].as_str().unwrap();
    let inspected = r
        .with_goal(&a, |r| {
            r.discussion_decision_reuse_get(&a, id, None, &key.expected_actor_id)
        })
        .unwrap();
    let citation: crate::retrieval::Citation =
        serde_json::from_value(inspected["citation"].clone()).unwrap();
    let packet = r
        .with_goal(&a, |r| {
            app().context_prepare(
                r,
                a.clone(),
                "Explicit review".into(),
                crate::retrieval::SearchScope {
                    goal_id: a.clone(),
                    mode: "goal".into(),
                    ..Default::default()
                },
                vec![citation.clone()],
                vec![citation.citation_id.clone()],
            )
        })
        .unwrap();
    let packet: crate::context::ReviewedPacket =
        serde_json::from_value(packet["packet"].clone()).unwrap();
    let packet_source = r
        .read_source(&r.path("reviewed-context", &packet.id))
        .unwrap();
    let automatic = start(&mut r, &a, "Positive automatic control", vec![], None).unwrap();
    assert!(automatic.body.contains("Unique retained constraint 261"));
    complete(&mut r, &a, &automatic.id, "Done");
    let base: SourceSnapshot = serde_json::from_value(inspected["source"].clone()).unwrap();
    let p = Disposition::new(
        Uuid::new_v4().to_string(),
        key.expected_actor_id.clone(),
        "2026-09-08T16:00:00Z".into(),
        base.revision.clone(),
    );
    let proposed = reuse::transform(&base, &a, id, &p).unwrap();
    let request = SourceWrite {
        schema: SCHEMA.into(),
        brain_id: base.brain_id.clone(),
        operation_id: p.operation_id,
        path: base.path.clone(),
        expected_revision: Some(base.revision.clone()),
        content_base64: STANDARD.encode(proposed),
    };
    r.with_goal(&a, |r| {
        r.discussion_decision_reuse_write(&a, id, request, base, &key.expected_actor_id)
    })
    .unwrap();
    let next = start(&mut r, &a, "New automatic discussion", vec![], None).unwrap();
    assert!(!next.body.contains("Unique retained constraint 261"));
    complete(&mut r, &a, &next.id, "Done");
    let manual = start(
        &mut r,
        &a,
        "Explicit manual reference",
        vec![path.into()],
        None,
    )
    .unwrap();
    assert!(manual.body.contains("Unique retained constraint 261"));
    complete(&mut r, &a, &manual.id, "Done");
    let other = start(&mut r, &b, "Other goal", vec![], None).unwrap();
    assert!(!other.body.contains("Unique retained constraint 261"));
    assert_eq!(r.read_source(&packet_source.path).unwrap(), packet_source);
    assert!(r
        .with_goal(&a, |r| crate::context::selected_citations(
            r,
            &a,
            &packet.scope,
            &packet.citations
        ))
        .is_err());
}
