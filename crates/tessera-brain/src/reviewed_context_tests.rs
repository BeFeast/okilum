use super::*;
use crate::{
    context::{ReviewedPacket, ReviewedPacketRef},
    retrieval::{Citation, SearchScope, SourceMetadata},
};
use std::{
    fs,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tessera_core::source::WriteBoundary;

struct Fixture {
    _temp: tempfile::TempDir,
    config: RunnerConfig,
    goal_id: String,
    app: Application,
}
impl Fixture {
    fn new() -> (Self, Runner) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("brain");
        let operational_dir = temp.path().join("runtime");
        fs::create_dir_all(root.join("records")).unwrap();
        fs::create_dir_all(&operational_dir).unwrap();
        let config = RunnerConfig {
            brain_id: Uuid::new_v4().to_string(),
            root,
            operational_dir,
            records_dir: "records".into(),
            boundary: WriteBoundary::Managed,
        };
        let mut runner = Runner::open(copy_config(&config)).unwrap();
        let goal_id = Uuid::new_v4().to_string();
        runner
            .create_goal(
                Goal {
                    id: goal_id.clone(),
                    title: "Review shipping decision".into(),
                    status: "draft".into(),
                    criteria: vec![Criterion {
                        id: "C1".into(),
                        description: "Explain the decision with evidence".into(),
                        requires_human: false,
                    }],
                    stage_ids: vec![],
                    task_ref: None,
                    extra: BTreeMap::new(),
                },
                "\n# Goal\n".into(),
            )
            .unwrap();
        let mut app = Application::unconfigured();
        app.t3_target = Some(BTreeMap::new());
        (
            Self {
                _temp: temp,
                config,
                goal_id,
                app,
            },
            runner,
        )
    }
    fn packet(&self, runner: &mut Runner) -> ReviewedPacket {
        let text = "# Decision\r\n\r\nShip after review.\r\n\r\nUNSELECTED-PRIVATE-TAIL\r\n";
        fs::write(self.config.root.join("decision.md"), text).unwrap();
        let source = runner.read_source("decision.md").unwrap();
        let excerpt = text.split_inclusive('\n').take(3).collect::<String>();
        let citation = Citation {
            citation_id: crate::retrieval::citation_id("decision.md", &source.revision, 1, 3),
            path: "decision.md".into(),
            revision: source.revision,
            start_line: 1,
            end_line: 3,
            locator: "L1-L3".into(),
            excerpt,
            metadata: SourceMetadata::default(),
        };
        let scope = SearchScope {
            goal_id: self.goal_id.clone(),
            mode: "project".into(),
            ..Default::default()
        };
        let created = self
            .app
            .context_prepare(
                runner,
                self.goal_id.clone(),
                "Explain shipping".into(),
                scope,
                vec![citation],
                vec![],
            )
            .unwrap();
        let packet: ReviewedPacket = serde_json::from_value(created["packet"].clone()).unwrap();
        assert!(!packet.reviewed);
        let reviewed = self
            .app
            .context_revise(
                runner,
                self.goal_id.clone(),
                packet.id,
                packet.revision,
                "VISIBLE-GUIDANCE: explain the selected decision only.\n".into(),
            )
            .unwrap();
        serde_json::from_value(reviewed["packet"].clone()).unwrap()
    }
}
#[test]
fn reviewed_packet_roundtrips_nested_markdown_dividers_in_yaml_citations() {
    let (f, mut runner) = Fixture::new();
    let text = "# Decision\n\n---\nKeep exact evidence.\n";
    fs::write(f.config.root.join("divider.md"), text).unwrap();
    let source = runner.read_source("divider.md").unwrap();
    let citation = Citation {
        citation_id: crate::retrieval::citation_id("divider.md", &source.revision, 1, 4),
        path: "divider.md".into(),
        revision: source.revision,
        start_line: 1,
        end_line: 4,
        locator: "L1-L4".into(),
        excerpt: text.into(),
        metadata: SourceMetadata::default(),
    };
    let created = f
        .app
        .context_prepare(
            &mut runner,
            f.goal_id.clone(),
            "Review divider evidence".into(),
            SearchScope {
                goal_id: f.goal_id.clone(),
                mode: "project".into(),
                ..Default::default()
            },
            vec![citation.clone()],
            vec![],
        )
        .unwrap();
    let packet: ReviewedPacket = serde_json::from_value(created["packet"].clone()).unwrap();
    let reviewed = f
        .app
        .context_revise(
            &mut runner,
            f.goal_id.clone(),
            packet.id.clone(),
            packet.revision,
            "Keep source divider intact.\n".into(),
        )
        .unwrap();
    let packet: ReviewedPacket = serde_json::from_value(reviewed["packet"].clone()).unwrap();
    let (loaded, _) =
        crate::context::require_reviewed(&runner, &f.goal_id, &packet.reference()).unwrap();
    assert!(loaded.reviewed);
    assert_eq!(loaded.citations[0].excerpt, text);
    assert_eq!(loaded.text, "Keep source divider intact.\n");
    let source = runner
        .read_source(&runner.path("reviewed-context", &packet.id))
        .unwrap();
    let raw = String::from_utf8(STANDARD.decode(&source.content_base64).unwrap()).unwrap();
    assert!(
        raw.lines()
            .any(|line| line.starts_with(' ') && line.trim() == "---"),
        "exercise a serialized YAML literal divider"
    );
    for newline in ["\n", "\r\n"] {
        let mut variant = source.clone();
        let raw = format!(
            "\u{feff}{}",
            raw.split_inclusive('\n')
                .map(|line| if line == "---\n" { "--- \t\n" } else { line })
                .collect::<String>()
                .replace('\n', newline)
        );
        variant.content_base64 = STANDARD.encode(raw.as_bytes());
        let loaded = crate::context::from_source(&variant).unwrap();
        // YAML normalizes line endings inside literal scalars, while the body is lossless.
        assert_eq!(loaded.citations[0].excerpt, text);
        assert_eq!(loaded.text, format!("Keep source divider intact.{newline}"));
    }
}
fn binding() -> EngineRef {
    EngineRef {
        engine: "t3".into(),
        instance_id: "fixture".into(),
        thread_id: Some("thread".into()),
        turn_id: Some("turn".into()),
        task_id: None,
    }
}
struct Fake {
    starts: Arc<AtomicUsize>,
    events: Vec<EngineEvent>,
}
impl Adapter for Fake {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            engine: "t3".into(),
            cancel: false,
        }
    }
    fn start(&mut self, _: &StartEnvelope) -> Result<StartReply> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(StartReply::Accepted { binding: binding() })
    }
    fn observe(&mut self, _: &EngineRef, _: &BTreeMap<String, String>) -> Result<Vec<EngineEvent>> {
        Ok(std::mem::take(&mut self.events))
    }
    fn reconcile(&mut self, _: &StartEnvelope, _: Option<&EngineRef>) -> Result<ReconcileReply> {
        Ok(ReconcileReply::Running {
            binding: binding(),
            evidence: "exact accepted mapping".into(),
        })
    }
}
#[test]
fn reviewed_dispatch_is_exact_once_and_external_packet_edits_revoke_review() {
    let (f, mut runner) = Fixture::new();
    let packet = f.packet(&mut runner);
    assert!(packet.reviewed);
    // A forged exact citation cannot make a non-Markdown attachment eligible.
    fs::write(
        f.config.root.join("attachment.txt"),
        "Attachment evidence\n",
    )
    .unwrap();
    let attachment = runner.read_source("attachment.txt").unwrap();
    let mut forged = packet.citations[0].clone();
    forged.path = attachment.path.clone();
    forged.revision = attachment.revision.clone();
    forged.start_line = 1;
    forged.end_line = 1;
    forged.locator = "L1-L1".into();
    forged.excerpt = "Attachment evidence\n".into();
    forged.citation_id = crate::retrieval::citation_id(&forged.path, &forged.revision, 1, 1);
    assert!(crate::retrieval::validate_citation(
        &attachment,
        &forged,
        &f.goal_id,
        "project",
        "records"
    )
    .is_err());
    assert!(f
        .app
        .stage_prepare(
            &mut runner,
            f.goal_id.clone(),
            None,
            vec![],
            vec!["C1".into()],
            "legacy bypass".into()
        )
        .is_err());
    f.app
        .stage_prepare_reviewed(
            &mut runner,
            f.goal_id.clone(),
            vec!["C1".into()],
            packet.reference(),
            None,
        )
        .unwrap();
    f.app.validate_reviewed_dispatch(&runner).unwrap();
    let dispatch = runner.snapshot().unwrap().dispatch.unwrap();
    let serialized = serde_json::to_string(&dispatch.packet).unwrap();
    assert_eq!(serialized.matches("VISIBLE-GUIDANCE").count(), 1);
    assert_eq!(serialized.matches("Ship after review.").count(), 1);
    assert!(!serialized.contains("UNSELECTED-PRIVATE-TAIL"));
    let reference: ReviewedPacketRef =
        serde_json::from_value(dispatch.packet.extra["reviewed_packet"].clone()).unwrap();
    assert_eq!(reference, packet.reference());
    assert_eq!(
        dispatch.packet.extra["source_excerpts"],
        serde_json::to_value(&packet.citations).unwrap()
    );
    let source = runner
        .read_source(&runner.path("reviewed-context", &packet.id))
        .unwrap();
    let mut raw = STANDARD.decode(&source.content_base64).unwrap();
    raw.extend_from_slice(b"External unreviewed direction.\n");
    fs::write(f.config.root.join(source.path), raw).unwrap();
    let changed = crate::context::read(&runner, &f.goal_id, &packet.id).unwrap();
    assert!(!changed.reviewed);
    assert!(f.app.validate_reviewed_dispatch(&runner).is_err());
}
#[test]
fn accepted_reviewed_stage_survives_source_change_and_restart_without_redispatch() {
    let (f, mut runner) = Fixture::new();
    let packet = f.packet(&mut runner);
    f.app
        .stage_prepare_reviewed(
            &mut runner,
            f.goal_id.clone(),
            vec!["C1".into()],
            packet.reference(),
            None,
        )
        .unwrap();
    f.app.validate_reviewed_dispatch(&runner).unwrap();
    let mut adapter = Fake {
        starts: Arc::new(AtomicUsize::new(0)),
        events: vec![],
    };
    runner.start(&mut adapter).unwrap();
    let frozen = runner.snapshot().unwrap().dispatch.unwrap();
    fs::write(
        f.config.root.join("decision.md"),
        "# Changed after accepted dispatch\n",
    )
    .unwrap();
    assert!(crate::context::require_reviewed(&runner, &f.goal_id, &packet.reference()).is_err());
    drop(runner);
    let mut runner = Runner::open(copy_config(&f.config)).unwrap();
    runner.reconcile(&mut adapter).unwrap();
    adapter.events.push(EngineEvent {
        operation_id: frozen.operation_id.clone(),
        engine_ref: binding(),
        event_id: "result-1".into(),
        stream_id: "thread/turn".into(),
        sequence: Some(1),
        cursor: Some("1".into()),
        observed_at: "2026-09-06T20:00:00Z".into(),
        payload: EventPayload::Outcome(Outcome {
            outcome: "succeeded".into(),
            summary: "The accepted source version was explained".into(),
            sources: frozen.packet.sources.clone(),
            evidence: vec![],
            verification: "unverified".into(),
            criterion_evaluations: vec![],
        }),
    });
    let result = runner.poll(&mut adapter).unwrap();
    assert_eq!(result.phase.as_deref(), Some("outcome_ready"));
    let kept = result.dispatch.unwrap();
    assert_eq!(kept.operation_id, frozen.operation_id);
    assert_eq!(
        kept.packet.sources[0].revision.as_deref(),
        Some(packet.citations[0].revision.as_str())
    );
    assert_eq!(
        kept.packet.extra["source_excerpts"],
        serde_json::to_value(packet.citations).unwrap()
    );
    assert_eq!(adapter.starts.load(Ordering::SeqCst), 1);
    assert!(runner.start(&mut adapter).is_err());
}

fn copy_config(c: &RunnerConfig) -> RunnerConfig {
    RunnerConfig {
        brain_id: c.brain_id.clone(),
        root: c.root.clone(),
        operational_dir: c.operational_dir.clone(),
        records_dir: c.records_dir.clone(),
        boundary: WriteBoundary::Managed,
    }
}
