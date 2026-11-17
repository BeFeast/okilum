use super::*;
use crate::gateway::GatewayResult;
use crate::projects::{Appearance, Kind};
use crate::{
    Capabilities, GatewayStatus, ModelSelection, OutgoingMessage, SendReceipt, ServerIdentity,
    ThreadConfiguration, ThreadPage, ThreadQuery, ThreadSummary,
};
use std::sync::{Arc, Mutex};

struct Fake {
    calls: Mutex<Vec<String>>,
    answers: BTreeMap<String, GatewayResult<Vec<String>>>,
}
impl Fake {
    fn new(answers: impl IntoIterator<Item = (&'static str, GatewayResult<Vec<String>>)>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            answers: answers
                .into_iter()
                .map(|(thread, answer)| (thread.to_owned(), answer))
                .collect(),
        }
    }
}
impl crate::ManagerGateway for Fake {
    fn status(&self) -> GatewayStatus {
        unimplemented!()
    }
    fn server_identity(&self) -> GatewayResult<ServerIdentity> {
        unimplemented!()
    }
    fn list_projects(&self) -> GatewayResult<Vec<crate::Project>> {
        unimplemented!()
    }
    fn list_threads(&self, _: &str) -> GatewayResult<Vec<ThreadSummary>> {
        unimplemented!()
    }
    fn pending_requests(&self, thread: &str) -> GatewayResult<Vec<String>> {
        self.calls.lock().unwrap().push(thread.into());
        self.answers
            .get(thread)
            .cloned()
            .unwrap_or_else(|| Ok(Vec::new()))
    }
    fn read_thread(&self, _: &ThreadQuery) -> GatewayResult<ThreadPage> {
        unimplemented!()
    }
    fn send_message(&self, _: &OutgoingMessage) -> GatewayResult<SendReceipt> {
        unimplemented!()
    }
    fn capabilities(&self) -> GatewayResult<Capabilities> {
        unimplemented!()
    }
    fn thread_configuration(&self, _: &str) -> GatewayResult<ThreadConfiguration> {
        unimplemented!()
    }
    fn configure_model(&self, _: &str, _: &ModelSelection) -> GatewayResult<ThreadConfiguration> {
        unimplemented!()
    }
}

fn project(folder: &str, environment: Option<&str>, manager: Manager) -> Project {
    Project {
        folder: PathBuf::from(folder),
        name: folder.rsplit('/').next().unwrap().into(),
        kind: Kind::Code,
        environment: environment.map(str::to_owned),
        manager_project: environment.map(|_| "managers".to_owned()),
        manager,
        appearance: Appearance::default(),
    }
}
fn thread(id: &str) -> Manager {
    Manager::Thread(id.into())
}
fn gateways(fake: &Arc<Fake>) -> BTreeMap<String, std::sync::Arc<dyn crate::ManagerGateway>> {
    BTreeMap::from([(
        "home".to_owned(),
        fake.clone() as Arc<dyn crate::ManagerGateway>,
    )])
}
fn source(folder: &str, thread_id: &str) -> Source {
    Source {
        folder: PathBuf::from(folder),
        environment: "home".into(),
        project_id: "managers".into(),
        thread_id: thread_id.into(),
    }
}

#[test]
fn only_bound_manager_threads_are_sources_and_incomplete_manifests_are_explicit() {
    let projects = [
        project("/v/Dev/App", Some("home"), thread("mcp:app")),
        project("/v/Dev/Idle", Some("home"), Manager::NotStarted),
        project("/v/Dev/Broken", None, thread("mcp:broken")),
    ];
    let sources = sources(&projects);
    assert_eq!(sources.len(), 2, "a project without a manager is no source");
    assert_eq!(sources[0].1, Ok(source("/v/Dev/App", "mcp:app")));
    let problem = sources[1].1.clone().unwrap_err();
    assert!(problem.contains("t3.environment"), "{problem}");
}

#[test]
fn rows_carry_the_exact_project_thread_and_request_identity() {
    let fake = Arc::new(Fake::new([
        ("mcp:app", Ok(vec!["q2".into(), "q1".into(), "q1".into()])),
        ("mcp:music", Ok(Vec::new())),
    ]));
    let projects = [
        project("/v/Dev/App", Some("home"), thread("mcp:app")),
        project("/v/Music/Album", Some("home"), thread("mcp:music")),
    ];
    let reads = read(
        &sources(&projects),
        &gateways(&fake),
        &NetworkAccess::new(true),
    );
    assert_eq!(*fake.calls.lock().unwrap(), ["mcp:app", "mcp:music"]);
    let mut ledger = Ledger::default();
    ledger.apply(reads, 100);
    let view = ledger.view();
    assert_eq!(view.sources, 2);
    assert!(!view.partial());
    assert_eq!(
        view.rows,
        ["q1", "q2"].map(|id| Row {
            source: source("/v/Dev/App", "mcp:app"),
            request_id: id.into(),
            stale: false,
            observed_at: 100,
        })
    );
    assert!(ledger.contains(&source("/v/Dev/App", "mcp:app"), "q1"));
    assert!(!ledger.contains(&source("/v/Dev/App", "mcp:other"), "q1"));
    assert!(!ledger.contains(&source("/v/Music/Album", "mcp:music"), "q1"));
}

#[test]
fn a_failed_read_keeps_the_confirmed_rows_stale_and_reports_the_source() {
    let projects = [
        project("/v/Dev/App", Some("home"), thread("mcp:app")),
        project("/v/Dev/Other", Some("home"), thread("mcp:other")),
    ];
    let mut ledger = Ledger::default();
    let ok = Arc::new(Fake::new([("mcp:app", Ok(vec!["q1".into()]))]));
    ledger.apply(
        read(
            &sources(&projects),
            &gateways(&ok),
            &NetworkAccess::new(true),
        ),
        100,
    );
    let failing = Arc::new(Fake::new([(
        "mcp:app",
        Err(crate::GatewayError::Unreachable("offline".into())),
    )]));
    ledger.apply(
        read(
            &sources(&projects),
            &gateways(&failing),
            &NetworkAccess::new(true),
        ),
        200,
    );
    let view = ledger.view();
    assert!(view.partial());
    assert_eq!(view.rows.len(), 1);
    assert!(view.rows[0].stale);
    assert_eq!(
        view.rows[0].observed_at, 100,
        "stale rows keep their own time"
    );
    assert_eq!(view.unavailable.len(), 1);
    assert_eq!(view.unavailable[0].folder, PathBuf::from("/v/Dev/App"));
    assert_eq!(view.unavailable[0].observed_at, Some(100));
    assert!(view.unavailable[0].problem.contains("offline"));
}

#[test]
fn an_unreachable_environment_is_an_unavailable_source_not_an_empty_one() {
    let projects = [project("/v/Dev/App", Some("away"), thread("mcp:app"))];
    let fake = Arc::new(Fake::new([]));
    let mut ledger = Ledger::default();
    ledger.apply(
        read(
            &sources(&projects),
            &gateways(&fake),
            &NetworkAccess::new(true),
        ),
        100,
    );
    assert!(fake.calls.lock().unwrap().is_empty());
    let view = ledger.view();
    assert!(view.rows.is_empty());
    assert_eq!(view.unavailable.len(), 1);
    assert_eq!(view.unavailable[0].observed_at, None);
}

#[test]
fn a_rebound_or_removed_manager_never_keeps_the_old_threads_questions() {
    let mut ledger = Ledger::default();
    let ok = Arc::new(Fake::new([("mcp:old", Ok(vec!["q1".into()]))]));
    let before = [
        project("/v/Dev/App", Some("home"), thread("mcp:old")),
        project("/v/Dev/Gone", Some("home"), thread("mcp:old")),
    ];
    ledger.apply(
        read(&sources(&before), &gateways(&ok), &NetworkAccess::new(true)),
        100,
    );
    assert_eq!(ledger.view().rows.len(), 2);
    // The new thread cannot be read; the old thread's question must not
    // reappear as a stale row of the rebound project.
    let failing = Arc::new(Fake::new([(
        "mcp:new",
        Err(crate::GatewayError::Unreachable("offline".into())),
    )]));
    let after = [project("/v/Dev/App", Some("home"), thread("mcp:new"))];
    ledger.apply(
        read(
            &sources(&after),
            &gateways(&failing),
            &NetworkAccess::new(true),
        ),
        200,
    );
    let view = ledger.view();
    assert!(view.rows.is_empty(), "{:?}", view.rows);
    assert_eq!(view.sources, 1, "a removed project is no longer a source");
    assert!(!ledger.contains(&source("/v/Dev/App", "mcp:old"), "q1"));
}

#[test]
fn a_revoked_permit_dispatches_no_further_requests() {
    let fake = Arc::new(Fake::new([("mcp:app", Ok(vec!["q1".into()]))]));
    let projects = [project("/v/Dev/App", Some("home"), thread("mcp:app"))];
    let permit = NetworkAccess::new(true);
    permit.revoke();
    let reads = read(&sources(&projects), &gateways(&fake), &permit);
    assert!(fake.calls.lock().unwrap().is_empty());
    assert_eq!(
        reads[&PathBuf::from("/v/Dev/App")].ids,
        Err(CANCELLED.to_owned())
    );
}

#[test]
fn rows_per_source_are_bounded_and_the_rest_is_counted() {
    let ids: Vec<String> = (0..ROWS_PER_SOURCE + 3)
        .map(|i| format!("q{i:03}"))
        .collect();
    let fake = Arc::new(Fake::new([("mcp:app", Ok(ids))]));
    let projects = [project("/v/Dev/App", Some("home"), thread("mcp:app"))];
    let mut ledger = Ledger::default();
    ledger.apply(
        read(
            &sources(&projects),
            &gateways(&fake),
            &NetworkAccess::new(true),
        ),
        1,
    );
    let view = ledger.view();
    assert_eq!(view.rows.len(), ROWS_PER_SOURCE);
    assert_eq!(view.omitted, [(source("/v/Dev/App", "mcp:app"), 3)]);
    assert_eq!(view.checked_at, Some(1));
}
