use super::*;
use crate::{Criterion, RunnerConfig};
use okilum_core::source::WriteBoundary;
use std::fs;
struct Fixture {
    _temp: tempfile::TempDir,
    root: std::path::PathBuf,
    brain: String,
    goal: String,
}
impl Fixture {
    fn new() -> (Self, Runner, Form) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("brain");
        let operational = temp.path().join("state");
        fs::create_dir_all(root.join("records")).unwrap();
        fs::create_dir(&operational).unwrap();
        let brain = Uuid::new_v4().to_string();
        let goal = Uuid::new_v4().to_string();
        let mut runner = Runner::open(RunnerConfig {
            brain_id: brain.clone(),
            root: root.clone(),
            records_dir: "records".into(),
            operational_dir: operational,
            boundary: WriteBoundary::Managed,
        })
        .unwrap();
        runner
            .create_goal(
                Goal {
                    id: goal.clone(),
                    title: "Preserve manually selected context".into(),
                    status: "draft".into(),
                    criteria: vec![Criterion {
                        id: "C1".into(),
                        description: "One original target survives replay".into(),
                        requires_human: false,
                    }],
                    stage_ids: vec![],
                    task_ref: None,
                    extra: BTreeMap::new(),
                },
                "# Goal\n".into(),
            )
            .unwrap();
        let text = "# Evidence\r\n\r\nKeep the original source.\r\n";
        fs::write(root.join("evidence.md"), text).unwrap();
        let source = runner.read_source("evidence.md").unwrap();
        let citation = Citation {
            citation_id: retrieval::citation_id("evidence.md", &source.revision, 1, 3),
            path: "evidence.md".into(),
            revision: source.revision,
            start_line: 1,
            end_line: 3,
            locator: "L1-L3".into(),
            excerpt: text.into(),
            metadata: Default::default(),
        };
        let form = Form {
            goal_id: goal.clone(),
            expected_goal_revision: runner.goal_source().unwrap().revision,
            query: "Use my selected evidence".into(),
            scope: SearchScope {
                goal_id: goal.clone(),
                mode: "project".into(),
                ..Default::default()
            },
            pinned_citation_ids: vec![citation.citation_id.clone()],
            citations: vec![citation],
            guidance: "  MANUAL guidance — 雪\r\n\r\n---\r\nDo not regenerate this text.  ".into(),
        };
        (
            Self {
                _temp: temp,
                root,
                brain,
                goal,
            },
            runner,
            form,
        )
    }
}
#[test]
fn frozen_restore_never_recaptures_clock_goal_source_or_identifiers() {
    let (f, mut runner, form) = Fixture::new();
    let first =
        prepare_with_clock(&runner, form.clone(), || "2026-09-07T10:00:00Z".into()).unwrap();
    let second =
        prepare_with_clock(&runner, form.clone(), || "2026-09-08T11:00:00Z".into()).unwrap();
    assert_ne!(first.packet_id(), second.packet_id());
    assert_ne!(first.created_at(), second.created_at());
    let stored = serde_json::to_vec(&first).unwrap();
    let before = first.source_write().clone();
    let packet = first.packet().unwrap();
    assert_eq!(packet.text, form.guidance);
    assert_eq!(packet.citations, form.citations);
    assert_eq!(packet.pinned_citation_ids, form.pinned_citation_ids);
    assert!(!packet.reviewed);
    assert!(packet.reviewed_content_sha256.is_none());
    assert!(
        !f.root.join(&before.path).exists(),
        "preparation is not a write or adoption"
    );
    fs::write(f.root.join("evidence.md"), "Changed source").unwrap();
    let goal_source = runner.goal_source().unwrap();
    let goal_text = String::from_utf8(STANDARD.decode(goal_source.content_base64).unwrap())
        .unwrap()
        .replace("Preserve manually selected context", "Changed goal");
    runner
        .write_source(SourceWrite {
            schema: crate::SCHEMA.into(),
            operation_id: Uuid::new_v4().to_string(),
            brain_id: f.brain.clone(),
            path: goal_source.path,
            expected_revision: Some(goal_source.revision),
            content_base64: STANDARD.encode(goal_text),
        })
        .unwrap();
    assert!(prepare_with_clock(&runner, form, || panic!(
        "stale prepare must not capture a clock"
    ))
    .is_err());
    drop(runner);
    let restored = FrozenTarget::restore(&stored, &f.brain, "records", &f.goal).unwrap();
    assert_eq!(restored.source_write(), &before);
    assert_eq!(serde_json::to_vec(&restored).unwrap(), stored);
    assert_eq!(restored.goal_id(), f.goal);
    assert_eq!(restored.created_at(), "2026-09-07T10:00:00Z");
    assert_eq!(restored.revision(), first.revision());
    assert_eq!(restored.packet().unwrap().text, packet.text);
    assert!(!f.root.join(&before.path).exists());
}
#[test]
fn fresh_preparation_refuses_changed_source_and_cross_goal_scope_or_pins() {
    let (f, runner, form) = Fixture::new();
    assert!(prepare(&runner, form.clone()).is_ok());
    for fault in [
        "goal",
        "scope",
        "pin",
        "citation",
        "duplicate_pin",
        "excluded",
    ] {
        let mut changed = form.clone();
        match fault {
            "goal" => changed.goal_id = Uuid::new_v4().to_string(),
            "scope" => changed.scope.goal_id = Uuid::new_v4().to_string(),
            "pin" => changed.pinned_citation_ids[0] = "missing-pin".into(),
            "citation" => changed.citations[0].citation_id = "wrong-citation".into(),
            "duplicate_pin" => changed
                .pinned_citation_ids
                .push(changed.pinned_citation_ids[0].clone()),
            _ => changed.scope.exclude_paths.push("evidence.md".into()),
        }
        assert!(prepare(&runner, changed).is_err(), "{fault}");
    }
    fs::write(
        f.root.join("evidence.md"),
        "Changed without updating citation",
    )
    .unwrap();
    assert!(prepare_with_clock(&runner, form, || panic!(
        "stale source must not capture clock"
    ))
    .is_err());
}
#[test]
fn frozen_target_is_readable_as_existing_context_and_requires_explicit_review() {
    let (f, mut runner, form) = Fixture::new();
    let target = prepare(&runner, form.clone()).unwrap();
    let receipt = runner.write_source(target.source_write().clone()).unwrap();
    assert_eq!(receipt.revision, target.revision());
    let packet = super::super::read(&runner, &f.goal, target.packet_id()).unwrap();
    assert_eq!(packet.text, form.guidance);
    assert!(!packet.reviewed);
    assert!(!packet.stale);
    assert!(super::super::require_reviewed(&runner, &f.goal, &packet.reference()).is_err());
    let mut marked = packet;
    marked.mark_reviewed().unwrap();
    let mut metadata = marked.metadata().unwrap();
    metadata["schema"] = json!(crate::SCHEMA);
    metadata["brain_id"] = json!(f.brain);
    metadata["record_type"] = json!("reviewed-context");
    let bytes = format!(
        "---\n{}---\n{}",
        serde_yaml::to_string(&metadata).unwrap(),
        marked.text
    );
    let source = SourceSnapshot {
        schema: crate::SCHEMA.into(),
        brain_id: f.brain,
        path: target.source_write().path.clone(),
        revision: format!("sha256:{}", retrieval::sha(bytes.as_bytes())),
        content_base64: STANDARD.encode(bytes),
        media_type: "text/markdown".into(),
    };
    assert!(
        from_source(&source).unwrap().reviewed,
        "ordinary review hashing semantics stay compatible"
    );
}
#[test]
fn checked_restore_refuses_wrong_owners_modified_bytes_and_reviewed_targets() {
    let (f, runner, form) = Fixture::new();
    let target = prepare(&runner, form).unwrap();
    let bytes = serde_json::to_vec(&target).unwrap();
    assert!(
        FrozenTarget::restore(&bytes, &Uuid::new_v4().to_string(), "records", &f.goal).is_err()
    );
    assert!(FrozenTarget::restore(&bytes, &f.brain, "different-records", &f.goal).is_err());
    assert!(
        FrozenTarget::restore(&bytes, &f.brain, "records", &Uuid::new_v4().to_string()).is_err()
    );
    for key in ["packet_id", "created_at", "revision", "source_write"] {
        let mut value: Value = serde_json::from_slice(&bytes).unwrap();
        match key {
            "packet_id" => value[key] = json!(Uuid::new_v4().to_string()),
            "created_at" => value[key] = json!("2026-09-08T00:00:00Z"),
            "revision" => value[key] = json!("sha256:changed"),
            _ => value[key]["expected_revision"] = json!(target.revision()),
        };
        assert!(FrozenTarget::restore(
            &serde_json::to_vec(&value).unwrap(),
            &f.brain,
            "records",
            &f.goal
        )
        .is_err());
    }
    let mut value: Value = serde_json::from_slice(&bytes).unwrap();
    let raw = STANDARD
        .decode(value["source_write"]["content_base64"].as_str().unwrap())
        .unwrap();
    let changed = String::from_utf8(raw)
        .unwrap()
        .replace("reviewed: false", "reviewed: true");
    value["source_write"]["content_base64"] = json!(STANDARD.encode(changed.as_bytes()));
    value["revision"] = json!(format!("sha256:{}", retrieval::sha(changed.as_bytes())));
    assert!(FrozenTarget::restore(
        &serde_json::to_vec(&value).unwrap(),
        &f.brain,
        "records",
        &f.goal
    )
    .is_err());
}
#[test]
fn target_budget_counts_manual_guidance_and_evidence_together() {
    let (_f, runner, mut form) = Fixture::new();
    let excerpts = form
        .citations
        .iter()
        .map(|c| c.excerpt.len())
        .sum::<usize>();
    form.guidance = "x".repeat(MAX_PACKET_BYTES - excerpts);
    assert!(prepare(&runner, form.clone()).is_ok());
    form.guidance.push('x');
    assert!(prepare(&runner, form).is_err());
}

#[test]
fn citation_goal_ownership_is_checked_against_actual_source_not_supplied_metadata() {
    let (f, runner, mut form) = Fixture::new();
    let other = Uuid::new_v4().to_string();
    let text=format!("---\nschema: {}\nrecord_type: decision\nbrain_id: {}\nid: {}\ngoal_id: {other}\n---\nOther goal evidence\n",crate::SCHEMA,f.brain,Uuid::new_v4());
    fs::write(f.root.join("evidence.md"), &text).unwrap();
    let source = runner.read_source("evidence.md").unwrap();
    let lines = text.split_inclusive('\n').count();
    form.scope.mode = "goal".into();
    form.citations[0] = Citation {
        citation_id: retrieval::citation_id("evidence.md", &source.revision, 1, lines),
        path: "evidence.md".into(),
        revision: source.revision,
        start_line: 1,
        end_line: lines,
        locator: format!("L1-L{lines}"),
        excerpt: text.clone(),
        metadata: retrieval::source_metadata(&text),
    };
    form.pinned_citation_ids = vec![form.citations[0].citation_id.clone()];
    assert!(prepare(&runner, form.clone()).is_err());
    form.citations[0].metadata = Default::default();
    assert!(
        prepare(&runner, form).is_err(),
        "hiding owner metadata must not bypass canonical provenance"
    );
}

#[test]
fn canonical_metadata_fits_the_existing_reader_at_the_exact_byte_limit() {
    let (_f, runner, mut form) = Fixture::new();
    form.scope.exclude_paths = vec!["x".into()];
    let fixed = || "2026-09-07T10:00:00Z".to_owned();
    let baseline = prepare_with_clock(&runner, form.clone(), fixed).unwrap();
    let size = STANDARD
        .decode(&baseline.source_write().content_base64)
        .unwrap()
        .len();
    form.scope.exclude_paths[0] = "x".repeat(MAX_CANONICAL_BYTES - size + 1);
    let boundary = prepare_with_clock(&runner, form.clone(), fixed).unwrap();
    assert_eq!(
        STANDARD
            .decode(&boundary.source_write().content_base64)
            .unwrap()
            .len(),
        MAX_CANONICAL_BYTES
    );
    form.scope.exclude_paths[0].push('x');
    assert!(
        prepare_with_clock(&runner, form, fixed).is_err(),
        "valid scope metadata must not produce an unreadable target"
    );
}
