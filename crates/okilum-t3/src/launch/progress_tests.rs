use super::*;
use crate::gateway::GatewayResult;
use crate::{
    Capabilities, GatewayStatus, ModelSelection, OutgoingMessage, Project, ServerIdentity,
    ThreadConfiguration, ThreadPage,
};
use serde_json::{json, Value};
use std::sync::Mutex;

const FIXTURE: &str = include_str!("../../tests/fixtures/t3-launch-progress-2026-11-17.json");
const REQUEST_ID: &str = "okilum-launch-00000000-0000-4000-8000-000000001420";

fn fixture() -> Value {
    serde_json::from_str(&FIXTURE.replace("{{REQUEST_ID}}", REQUEST_ID)).unwrap()
}

fn request() -> Request {
    Request {
        request_id: REQUEST_ID.into(),
        role: "UI".into(),
        brief_path: "brief.md".into(),
        brief_source: "# brief".into(),
        executor_project_id: "fixture-executors".into(),
        executor_project_root: "/server/repo".into(),
        worktree_path: "/server/worktrees/ui".into(),
        branch: "feature/ui".into(),
        model_selection: ModelSelection {
            instance_id: "codex".into(),
            model: "gpt-6.1-sol".into(),
            options: vec![],
        },
        manager_instructions: String::new(),
    }
}

/// Replays the recorded T3 frames. Each read is logged by thread id.
struct Recorded {
    fixture: Value,
    manager: Mutex<Value>,
    executors: Mutex<Value>,
    reads: Mutex<Vec<String>>,
    lists: Mutex<usize>,
}

impl Recorded {
    fn new() -> Self {
        let fixture = fixture();
        let frame = fixture["frames"][0].clone();
        Self {
            fixture,
            manager: Mutex::new(frame["manager"].clone()),
            executors: Mutex::new(frame["executors"].clone()),
            reads: Mutex::default(),
            lists: Mutex::default(),
        }
    }
    fn frame(&self, index: usize) {
        let frame = &self.fixture["frames"][index];
        *self.manager.lock().unwrap() = frame["manager"].clone();
        *self.executors.lock().unwrap() = frame["executors"].clone();
    }
    fn executors(&self, rows: Value) {
        *self.executors.lock().unwrap() = rows;
    }
    fn receipt(&self) -> SendReceipt {
        serde_json::from_value(self.fixture["receipt"].clone()).unwrap()
    }
    fn baseline(&self) -> Vec<ThreadSummary> {
        serde_json::from_value(self.fixture["frames"][0]["executors"].clone()).unwrap()
    }
    fn reads(&self) -> Vec<String> {
        self.reads.lock().unwrap().clone()
    }
}

impl ManagerGateway for Recorded {
    fn status(&self) -> GatewayStatus {
        GatewayStatus::Connected {
            expires_at: u64::MAX,
        }
    }
    fn server_identity(&self) -> GatewayResult<ServerIdentity> {
        unimplemented!()
    }
    fn list_projects(&self) -> GatewayResult<Vec<Project>> {
        unimplemented!()
    }
    fn list_threads(&self, project: &str) -> GatewayResult<Vec<ThreadSummary>> {
        assert_eq!(project, "fixture-executors");
        *self.lists.lock().unwrap() += 1;
        Ok(serde_json::from_value(self.executors.lock().unwrap().clone()).unwrap())
    }
    fn read_thread(&self, query: &ThreadQuery) -> GatewayResult<ThreadPage> {
        assert_eq!(query.limit, Some(1), "launch progress reads one item");
        assert!(query.after_position.is_none());
        self.reads.lock().unwrap().push(query.thread_id.clone());
        if query.thread_id == "mcp:fixture-manager" {
            assert!(!query.from_start, "only the manager's detail is read");
            assert_eq!(query.max_chars_per_item, Some(1));
            return Ok(serde_json::from_value(json!({
                "thread": self.manager.lock().unwrap().clone(), "items": [], "hasMore": true
            }))
            .unwrap());
        }
        let row = self
            .executors
            .lock()
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["threadId"] == query.thread_id.as_str())
            .cloned()
            .expect("only listed threads are read");
        assert!(
            query.from_start,
            "executor threads are read from the first item"
        );
        let items = timeline_page(&self.fixture["timelines"][&query.thread_id], query);
        Ok(serde_json::from_value(json!({
            "thread": {"threadId": row["threadId"], "projectId": "fixture-executors", "title": row["title"],
                "status": row["status"], "activeRunId": row["activeRunId"], "providerInstanceId": "codex",
                "model": "gpt-6.1-sol", "runtimeMode": "full-access", "linkedPullRequest": null,
                "updatedAt": row["updatedAt"]},
            "nextPosition": items.last().map(|item| item["position"].clone()),
            "items": items, "hasMore": false
        }))
        .unwrap())
    }
    fn send_message(&self, _: &OutgoingMessage) -> GatewayResult<SendReceipt> {
        panic!("launch progress never sends")
    }
    fn pending_requests(&self, _: &str) -> GatewayResult<Vec<String>> {
        unimplemented!()
    }
    fn capabilities(&self) -> GatewayResult<Capabilities> {
        unimplemented!()
    }
    fn thread_configuration(&self, _: &str) -> GatewayResult<ThreadConfiguration> {
        unimplemented!()
    }
    fn configure_model(&self, _: &str, _: &ModelSelection) -> GatewayResult<ThreadConfiguration> {
        panic!("launch progress never configures")
    }
}

/// T3's `t3_thread_read` paging: after a cursor (`-1` = from the first item)
/// oldest first; with no cursor in a thread-scoped session, newest first.
pub(crate) fn timeline_page(timeline: &Value, query: &ThreadQuery) -> Vec<Value> {
    let mut items = timeline.as_array().cloned().unwrap_or_default();
    match (query.after_position, query.from_start) {
        (Some(after), _) => items.retain(|item| item["position"].as_u64() > Some(after)),
        (None, true) => {}
        (None, false) => items.reverse(),
    }
    items.truncate(query.limit.unwrap_or(u32::MAX) as usize);
    items
}

fn step(tracker: &mut Tracker, t3: &Recorded) -> bool {
    let receipt = t3.receipt();
    let observation = tracker.observe(t3, Some(&receipt)).unwrap().unwrap();
    tracker.apply(observation)
}

fn tracker(t3: &Recorded, with_baseline: bool) -> Tracker {
    let baseline = t3.baseline();
    Tracker::new(
        &request(),
        "okilum-client-1".into(),
        "mcp:fixture-manager".into(),
        with_baseline.then_some(baseline.as_slice()),
    )
}

#[test]
fn recorded_t3_status_drives_every_step_and_matches_only_the_first_line_marker() {
    let t3 = Recorded::new();
    let mut launch = tracker(&t3, true);
    assert_eq!(
        launch.progress.line(),
        "[requested] → accepted → preparing → running → completed"
    );
    // The manager is still busy with an earlier turn: queued, not accepted.
    step(&mut launch, &t3);
    assert_eq!(launch.progress.reached, Stage::Requested);
    assert!(launch.progress.note.is_none());
    t3.frame(1);
    assert!(step(&mut launch, &t3));
    assert_eq!(
        launch.progress.line(),
        "requested → [accepted] → preparing → running → completed"
    );
    t3.frame(2);
    assert!(step(&mut launch, &t3));
    assert_eq!(
        launch.progress.line(),
        "requested → accepted → [preparing] → running → completed"
    );
    let executor = launch.progress.executor.clone().unwrap();
    assert_eq!(executor.thread_id, "mcp:fixture-executor-new");
    assert_eq!(executor.status, "preparing");
    t3.frame(3);
    assert!(step(&mut launch, &t3));
    assert_eq!(
        launch.progress.line(),
        "requested → accepted → preparing → [running] → completed"
    );
    t3.frame(4);
    assert!(step(&mut launch, &t3));
    assert_eq!(
        launch.progress.line(),
        "requested → accepted → preparing → running → [completed]"
    );
    assert!(launch.progress.finished());
    let reads = t3.reads();
    assert!(
        !reads.contains(&"mcp:fixture-executor-old".to_owned()),
        "threads that existed before the request are never read"
    );
    for id in ["mcp:fixture-executor-new", "mcp:fixture-executor-unrelated"] {
        assert_eq!(
            reads.iter().filter(|read| *read == id).count(),
            1,
            "each new thread's first message is read once: {reads:?}"
        );
    }
    assert_eq!(
        reads.iter().filter(|r| *r == "mcp:fixture-manager").count(),
        3,
        "the manager is read only until an executor carries the request"
    );
}

#[test]
fn failure_and_cancellation_list_only_confirmed_steps() {
    for (outcome, line) in [
        ("failed", "requested → accepted → preparing → [failed]"),
        (
            "interrupted",
            "requested → accepted → preparing → [cancelled]",
        ),
        (
            "cancelled",
            "requested → accepted → preparing → [cancelled]",
        ),
    ] {
        let t3 = Recorded::new();
        let mut launch = tracker(&t3, true);
        t3.frame(2);
        step(&mut launch, &t3);
        let mut rows = t3.fixture["frames"][2]["executors"].clone();
        rows[2] = t3.fixture["outcomes"][outcome].clone();
        t3.executors(rows);
        step(&mut launch, &t3);
        assert_eq!(launch.progress.line(), line, "{outcome}");
        assert_eq!(launch.progress.executor.as_ref().unwrap().status, outcome);
    }
}

#[test]
fn stale_or_foreign_observations_never_overwrite_a_newer_state() {
    let t3 = Recorded::new();
    let mut launch = tracker(&t3, true);
    let receipt = t3.receipt();
    t3.frame(2);
    let older = launch.observe(&t3, Some(&receipt)).unwrap().unwrap();
    t3.frame(3);
    let newer = launch.observe(&t3, Some(&receipt)).unwrap().unwrap();
    assert!(launch.apply(newer));
    assert_eq!(launch.progress.reached, Stage::Running);
    assert!(
        !launch.apply(older),
        "an observation taken at an older revision is dropped"
    );
    assert_eq!(launch.progress.reached, Stage::Running);

    // Another launch's observation of a completed thread is ignored.
    let mut other_request = request();
    other_request.request_id = "okilum-launch-other".into();
    let other = Tracker::new(
        &other_request,
        "c2".into(),
        "mcp:fixture-manager".into(),
        None,
    );
    t3.frame(4);
    let foreign = other.observe(&t3, Some(&receipt)).unwrap().unwrap();
    assert!(!launch.apply(foreign));
    assert_eq!(launch.progress.reached, Stage::Running);
    assert!(!launch.progress.finished());

    // A later T3 listing that reports an earlier status does not move back.
    t3.frame(2);
    step(&mut launch, &t3);
    assert_eq!(launch.progress.reached, Stage::Running);
}

#[test]
fn ambiguity_missing_threads_and_an_ended_manager_run_stay_visible() {
    // Two threads carry the same request: no winner is picked.
    let t3 = Recorded::new();
    let mut launch = tracker(&t3, false);
    let mut fixture = t3.fixture.clone();
    fixture["timelines"]["mcp:fixture-executor-old"][0]["text"] =
        json!(format!("{MARKER}{REQUEST_ID}"));
    let t3 = Recorded {
        fixture,
        ..Recorded::new()
    };
    t3.frame(2);
    step(&mut launch, &t3);
    assert!(launch.progress.executor.is_none());
    let note = launch.progress.note.clone().unwrap();
    assert!(note.contains("More than one T3 thread"), "{note}");
    assert!(note.contains("mcp:fixture-executor-old") && note.contains("mcp:fixture-executor-new"));
    assert_eq!(launch.progress.reached, Stage::Accepted);

    // The manager run ended without a launch carrying this request.
    let t3 = Recorded::new();
    let mut launch = tracker(&t3, true);
    t3.frame(1);
    step(&mut launch, &t3);
    let mut frame = t3.fixture["frames"][1].clone();
    frame["manager"]["status"] = json!("completed");
    frame["manager"]["activeRunId"] = Value::Null;
    *t3.manager.lock().unwrap() = frame["manager"].clone();
    step(&mut launch, &t3);
    assert_eq!(launch.progress.reached, Stage::Accepted);
    assert!(launch
        .progress
        .note
        .as_deref()
        .unwrap()
        .contains("no longer active (manager thread: completed)"));

    // A matched thread that disappears from the listing is reported.
    let t3 = Recorded::new();
    let mut launch = tracker(&t3, true);
    t3.frame(3);
    step(&mut launch, &t3);
    t3.frame(1);
    step(&mut launch, &t3);
    assert_eq!(launch.progress.reached, Stage::Running);
    assert!(launch.progress.note.as_deref().unwrap().contains("missing"));
}

#[test]
fn no_receipt_reads_nothing_and_a_started_delivery_is_acceptance() {
    let t3 = Recorded::new();
    let mut launch = tracker(&t3, true);
    assert!(launch.observe(&t3, None).unwrap().is_none());
    assert!(t3.reads().is_empty() && *t3.lists.lock().unwrap() == 0);
    let mut receipt = t3.receipt();
    assert!(!launch.record_receipt(&receipt), "queued is not acceptance");
    receipt.delivery = "started".into();
    assert!(launch.record_receipt(&receipt));
    assert_eq!(launch.progress.reached, Stage::Accepted);
}

#[test]
fn unknown_statuses_and_empty_new_threads_advance_nothing() {
    let t3 = Recorded::new();
    let mut launch = tracker(&t3, true);
    let mut fixture = t3.fixture.clone();
    fixture["timelines"]["mcp:fixture-executor-new"] = json!([]);
    let t3 = Recorded {
        fixture,
        ..Recorded::new()
    };
    t3.frame(2);
    step(&mut launch, &t3);
    step(&mut launch, &t3);
    assert_eq!(
        t3.reads()
            .iter()
            .filter(|r| *r == "mcp:fixture-executor-new")
            .count(),
        2,
        "a new thread without its first message yet is read again"
    );
    assert!(launch.progress.executor.is_none());

    let t3 = Recorded::new();
    let mut launch = tracker(&t3, true);
    let mut rows = t3.fixture["frames"][2]["executors"].clone();
    rows[2]["status"] = json!("hibernating");
    t3.executors(rows);
    step(&mut launch, &t3);
    assert_eq!(launch.progress.reached, Stage::Accepted);
    assert!(launch
        .progress
        .note
        .as_deref()
        .unwrap()
        .contains("hibernating"));
}

#[test]
fn review1_an_executor_with_several_messages_is_matched_by_its_first_one() {
    // The first inspection happens when the executor already replied twice:
    // a newest-first read would see its reply, never the marker line.
    let t3 = Recorded::new();
    let mut launch = tracker(&t3, true);
    let newest = timeline_page(
        &t3.fixture["timelines"]["mcp:fixture-executor-new"],
        &ThreadQuery {
            limit: Some(1),
            ..ThreadQuery::messages("mcp:fixture-executor-new")
        },
    );
    assert_eq!(
        newest[0]["position"], 2,
        "the fake keeps T3's newest-first default"
    );
    t3.frame(3);
    assert!(step(&mut launch, &t3));
    assert_eq!(launch.progress.reached, Stage::Running);
    assert_eq!(
        launch.progress.executor.as_ref().unwrap().thread_id,
        "mcp:fixture-executor-new"
    );
}

#[test]
fn review2_a_duplicate_beyond_the_first_batch_keeps_the_outcome_provisional() {
    // Nine new threads: the completed match is newest, a second thread with
    // the same marker is ninth and falls outside the first batch of eight.
    let mut fixture = Recorded::new().fixture;
    let mut rows = vec![fixture["frames"][0]["executors"][0].clone()];
    let mut completed = fixture["frames"][4]["executors"][2].clone();
    completed["updatedAt"] = json!("2026-11-17T12:00:00.000Z");
    rows.push(completed);
    for index in 0..7 {
        let id = format!("mcp:fixture-busy-{index}");
        let mut row = fixture["frames"][2]["executors"][1].clone();
        row["threadId"] = json!(id);
        row["updatedAt"] = json!(format!("2026-11-17T11:0{index}:00.000Z"));
        rows.push(row);
        fixture["timelines"][&id] = fixture["timelines"]["mcp:fixture-executor-unrelated"].clone();
    }
    let mut duplicate = fixture["frames"][2]["executors"][2].clone();
    duplicate["threadId"] = json!("mcp:fixture-executor-duplicate");
    duplicate["updatedAt"] = json!("2026-11-17T10:30:00.000Z");
    rows.push(duplicate);
    fixture["timelines"]["mcp:fixture-executor-duplicate"] =
        fixture["timelines"]["mcp:fixture-executor-new"].clone();
    let t3 = Recorded {
        fixture,
        ..Recorded::new()
    };
    t3.executors(json!(rows));
    let mut launch = tracker(&t3, true);
    step(&mut launch, &t3);
    assert!(
        !launch.progress.finished(),
        "a final outcome waits until the ninth new thread is checked"
    );
    assert!(launch
        .progress
        .note
        .as_deref()
        .unwrap()
        .contains("checking 1 more new T3 threads"));
    step(&mut launch, &t3);
    assert!(
        !launch.progress.finished(),
        "polling continues while ambiguous"
    );
    assert!(launch.progress.executor.is_none());
    let note = launch.progress.note.clone().unwrap();
    assert!(
        note.contains("mcp:fixture-executor-new")
            && note.contains("mcp:fixture-executor-duplicate"),
        "{note}"
    );
    let reads = t3.reads();
    assert_eq!(
        reads
            .iter()
            .filter(|r| r.starts_with("mcp:fixture-busy-"))
            .count(),
        7,
        "every new thread is read exactly once: {reads:?}"
    );
}

#[test]
fn review3_empty_new_threads_rotate_so_a_later_duplicate_is_found() {
    // Ten new threads, newest first: the completed match, then eight threads
    // still without a first message, then a second thread carrying the same
    // marker. Re-reading the same empty threads every poll would never reach
    // the duplicate and would hide the ambiguity forever.
    let mut fixture = Recorded::new().fixture;
    let mut rows = vec![fixture["frames"][0]["executors"][0].clone()];
    let mut completed = fixture["frames"][4]["executors"][2].clone();
    completed["updatedAt"] = json!("2026-11-17T12:00:00.000Z");
    rows.push(completed);
    for index in 0..8 {
        let id = format!("mcp:fixture-empty-{index}");
        let mut row = fixture["frames"][2]["executors"][1].clone();
        row["threadId"] = json!(id);
        row["status"] = json!("preparing");
        row["updatedAt"] = json!(format!("2026-11-17T11:0{}:00.000Z", 8 - index));
        rows.push(row);
        fixture["timelines"][&id] = json!([]);
    }
    let mut duplicate = fixture["frames"][2]["executors"][2].clone();
    duplicate["threadId"] = json!("mcp:fixture-executor-duplicate");
    duplicate["updatedAt"] = json!("2026-11-17T10:30:00.000Z");
    rows.push(duplicate);
    fixture["timelines"]["mcp:fixture-executor-duplicate"] =
        fixture["timelines"]["mcp:fixture-executor-new"].clone();
    let t3 = Recorded {
        fixture,
        ..Recorded::new()
    };
    t3.executors(json!(rows));
    let mut launch = tracker(&t3, true);

    // First batch: the completed match and seven empty threads.
    step(&mut launch, &t3);
    assert!(
        !launch.progress.finished(),
        "nine new threads remain unchecked"
    );
    assert!(launch
        .progress
        .note
        .as_deref()
        .unwrap()
        .contains("checking 9 more new T3 threads"));
    // Second batch starts with the never-read empty thread and the duplicate.
    step(&mut launch, &t3);
    assert!(
        !launch.progress.finished(),
        "an ambiguous launch never finishes"
    );
    assert!(launch.progress.executor.is_none());
    let note = launch.progress.note.clone().unwrap();
    assert!(
        note.contains("mcp:fixture-executor-new")
            && note.contains("mcp:fixture-executor-duplicate"),
        "{note}"
    );
    let reads = t3.reads();
    assert_eq!(
        reads
            .iter()
            .filter(|r| *r == "mcp:fixture-executor-duplicate")
            .count(),
        1
    );
    // Every empty thread is read once before any is read a second time.
    let empty_reads = reads
        .iter()
        .filter(|r| r.starts_with("mcp:fixture-empty-"))
        .collect::<Vec<_>>();
    assert_eq!(empty_reads.len(), 14, "{reads:?}");
    let distinct = empty_reads[..8].iter().collect::<BTreeSet<_>>();
    assert_eq!(distinct.len(), 8, "{empty_reads:?}");
    step(&mut launch, &t3);
    assert!(!launch.progress.finished());
}
