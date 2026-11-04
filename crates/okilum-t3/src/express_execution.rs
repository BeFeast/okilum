//! Express execution boundary. No production executor endpoint is invented here.
//! Adapters must implement server-side idempotency and safe-boundary priority.
use crate::express_session::{Platform, Request};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const BLOCKER: &str = "Express execution unavailable: this connection has no safe-boundary priority/isolated-build endpoint or approved recipient delivery endpoint. Manager request tracking remains available.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub request_id: String,
    pub environment_id: String,
    pub project_id: String,
    pub manager_thread_id: String,
    pub request_hash: String,
    pub source: String,
    pub branch: String,
    pub platforms: Vec<Platform>,
    pub recipient: String,
}
impl Plan {
    /// Select the exact existing durable Manager operation. Unacknowledged
    /// messages remain uncertain tracking intents and cannot become executions.
    pub fn from_retained(
        journal: &crate::chat::MessageJournal,
        request_id: &str,
        source: &str,
    ) -> Result<Self> {
        let mut selected = None;
        for record in journal.records()? {
            let Some(payload) = record
                .message
                .text
                .strip_prefix(crate::express_session::PREFIX)
            else {
                continue;
            };
            let request: Request = serde_json::from_str(payload)?;
            if request.request_id != request_id {
                continue;
            }
            ensure!(selected.is_none(), "Ambiguous retained Express identity");
            ensure!(
                record.message.thread_id == request.manager_thread_id
                    && record
                        .receipt
                        .as_ref()
                        .is_some_and(|r| r.thread_id == request.manager_thread_id),
                "Manager delivery is unconfirmed; reconcile the existing request first"
            );
            selected = Some(Self::new(&request, source)?);
        }
        selected.context("Exact retained Express request is missing")
    }

    /// Resolve an exact source before any executor side effect. Existing request
    /// IDs and selected source revisions remain unchanged in their original journal.
    pub fn new(request: &Request, source: &str) -> Result<Self> {
        request.draft.validate()?;
        ensure!(sha(source), "Build source must be an exact 40-hex commit");
        for value in [
            &request.request_id,
            &request.project_id,
            &request.manager_thread_id,
        ] {
            ensure!(
                !value.trim().is_empty() && value.len() <= 1024,
                "Missing Express identity"
            );
        }
        let environment_id = request
            .environment_id
            .clone()
            .filter(|s| !s.trim().is_empty())
            .context("Missing server identity")?;
        let request_hash = digest(&serde_json::to_vec(request)?);
        Ok(Self {
            request_id: request.request_id.clone(),
            environment_id,
            project_id: request.project_id.clone(),
            manager_thread_id: request.manager_thread_id.clone(),
            branch: format!("sprint/express-{}", &request_hash[..24]),
            request_hash,
            source: source.into(),
            platforms: request.draft.platforms.clone(),
            recipient: request.draft.recipient.clone(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispatchReceipt {
    pub operation: String,
    pub executor_thread_id: String,
    pub environment_id: String,
    pub project_id: String,
    pub source: String,
    pub branch: String,
    pub platforms: Vec<Platform>,
    /// Server acknowledgement, not a client-side inference from idle state.
    pub priority_at_safe_boundary: bool,
    pub isolated_workspace: bool,
}
impl DispatchReceipt {
    fn validate(&self, plan: &Plan) -> Result<()> {
        ensure!(
            self.operation == plan.request_hash
                && self.environment_id == plan.environment_id
                && self.project_id == plan.project_id
                && self.source == plan.source
                && self.branch == plan.branch
                && self.platforms == plan.platforms
                && !self.executor_thread_id.trim().is_empty()
                && self.priority_at_safe_boundary
                && self.isolated_workspace,
            "Dispatch receipt does not prove this exact isolated safe-boundary request"
        );
        Ok(())
    }
}

/// Source/fake boundary only. Implementors must not substitute chat queue sends
/// for execution, interrupt a running writer, or reuse the working installation.
pub trait Executor {
    fn available(&self) -> bool;
    fn dispatch(&self, plan: &Plan) -> Result<DispatchReceipt>;
    fn reconcile(&self, plan: &Plan) -> Result<Option<DispatchReceipt>>;
}
pub struct UnsupportedExecutor;
impl Executor for UnsupportedExecutor {
    fn available(&self) -> bool {
        false
    }
    fn dispatch(&self, _: &Plan) -> Result<DispatchReceipt> {
        anyhow::bail!(BLOCKER)
    }
    fn reconcile(&self, _: &Plan) -> Result<Option<DispatchReceipt>> {
        anyhow::bail!(BLOCKER)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum State {
    Prepared,
    /// Published before dispatch; restart never dispatches it again.
    Dispatching,
    Dispatched(DispatchReceipt),
    Cancelled,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Event {
    version: u32,
    plan: Plan,
    state: State,
}

/// Durable application state, never the rebuildable index or canonical vault.
/// Immutable numbered events use atomic no-clobber publication and fsync.
pub struct Store {
    directory: PathBuf,
    plan: Plan,
}
impl Store {
    pub fn new(state: &Path, plan: Plan) -> Result<Self> {
        let key = digest(&serde_json::to_vec(&(
            &plan.environment_id,
            &plan.project_id,
            &plan.manager_thread_id,
            &plan.request_id,
        ))?);
        let store = Self {
            directory: state.join("t3-express-execution").join(key),
            plan,
        };
        std::fs::create_dir_all(&store.directory)?;
        if store.read()?.is_none() {
            store.publish(0, State::Prepared)?;
        }
        Ok(store)
    }
    pub fn state(&self) -> Result<State> {
        Ok(self.read()?.context("Missing execution intent")?.1)
    }
    fn read(&self) -> Result<Option<(u64, State)>> {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&self.directory)? {
            let name = entry?
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("Invalid execution event name"))?;
            if name.ends_with(".tmp") {
                continue;
            }
            let number = name
                .strip_suffix(".json")
                .context("Unexpected execution state file")?
                .parse::<u64>()?;
            entries.push(number);
        }
        entries.sort_unstable();
        ensure!(entries.len() <= 5, "Execution history exceeds bound");
        let mut last = None;
        for (expected, number) in entries.into_iter().enumerate() {
            ensure!(
                number == expected as u64,
                "Execution history gap; held for recovery"
            );
            let mut bytes = Vec::new();
            std::fs::File::open(self.directory.join(format!("{number:020}.json")))?
                .take(65537)
                .read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= 65536, "Execution event too large");
            let event: Event = serde_json::from_slice(&bytes)?;
            ensure!(
                event.version == 1 && event.plan == self.plan,
                "Stale Express request/source identity"
            );
            let valid = match (&last, &event.state) {
                (None, State::Prepared) => true,
                (Some((_, State::Prepared)), State::Dispatching | State::Cancelled) => true,
                (Some((_, State::Dispatching)), State::Dispatched(receipt)) => {
                    receipt.validate(&self.plan)?;
                    true
                }
                _ => false,
            };
            ensure!(valid, "Invalid execution transition; held for recovery");
            last = Some((number, event.state));
        }
        Ok(last)
    }
    fn publish(&self, number: u64, state: State) -> Result<()> {
        let temporary = self.directory.join(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&serde_json::to_vec(&Event {
                version: 1,
                plan: self.plan.clone(),
                state,
            })?)?;
            file.sync_all()?;
            std::fs::hard_link(
                &temporary,
                self.directory.join(format!("{number:020}.json")),
            )
            .context("Execution state changed; no overwrite or automatic retry")?;
            std::fs::File::open(&self.directory)?.sync_all()?;
            Ok(())
        })();
        let _ = std::fs::remove_file(temporary);
        result
    }
    pub fn cancel(&self) -> Result<()> {
        ensure!(
            self.state()? == State::Prepared,
            "Dispatch may have happened; cancellation needs executor reconciliation"
        );
        self.publish(1, State::Cancelled)
    }
    pub fn dispatch(
        &self,
        executor: &dyn Executor,
        authorize: impl Fn() -> Result<()>,
    ) -> Result<DispatchReceipt> {
        ensure!(executor.available(), BLOCKER);
        ensure!(
            self.state()? == State::Prepared,
            "Existing dispatch must be reconciled, never replayed"
        );
        authorize()?;
        self.publish(1, State::Dispatching)?;
        // A revoked owner after retention leaves a held intent, never a new send.
        authorize()?;
        let receipt = executor.dispatch(&self.plan)?;
        receipt.validate(&self.plan)?;
        self.publish(2, State::Dispatched(receipt.clone()))?;
        Ok(receipt)
    }
    pub fn reconcile(&self, executor: &dyn Executor) -> Result<Option<DispatchReceipt>> {
        ensure!(
            self.state()? == State::Dispatching,
            "Only uncertain dispatch needs reconciliation"
        );
        let receipt = executor.reconcile(&self.plan)?;
        if let Some(receipt) = &receipt {
            receipt.validate(&self.plan)?;
            self.publish(2, State::Dispatched(receipt.clone()))?;
        }
        Ok(receipt)
    }
}
pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    fn fixture() -> (PathBuf, Plan) {
        let request = Request {
            request_id: "request".into(),
            project_id: "project".into(),
            manager_thread_id: "manager".into(),
            environment_id: Some("environment".into()),
            draft: crate::express_session::Draft {
                goal: "Urgent fix".into(),
                platforms: vec![Platform::Linux],
                recipient: "Synthetic tester".into(),
                source: None,
            },
            instructions: "Retained instructions".into(),
        };
        (
            std::env::temp_dir().join(format!("express-execution-{}", uuid::Uuid::new_v4())),
            Plan::new(&request, &"a".repeat(40)).unwrap(),
        )
    }
    struct Fake {
        calls: AtomicUsize,
        receipt: DispatchReceipt,
        fail: bool,
    }
    impl Fake {
        fn new(plan: &Plan) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                fail: false,
                receipt: DispatchReceipt {
                    operation: plan.request_hash.clone(),
                    executor_thread_id: "executor".into(),
                    environment_id: plan.environment_id.clone(),
                    project_id: plan.project_id.clone(),
                    source: plan.source.clone(),
                    branch: plan.branch.clone(),
                    platforms: plan.platforms.clone(),
                    priority_at_safe_boundary: true,
                    isolated_workspace: true,
                },
            }
        }
    }
    impl Executor for Fake {
        fn available(&self) -> bool {
            true
        }
        fn dispatch(&self, _: &Plan) -> Result<DispatchReceipt> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            ensure!(!self.fail, "Uncertain response");
            Ok(self.receipt.clone())
        }
        fn reconcile(&self, _: &Plan) -> Result<Option<DispatchReceipt>> {
            Ok(Some(self.receipt.clone()))
        }
    }
    #[test]
    fn uncertain_dispatch_survives_restart_without_replay() {
        let (root, plan) = fixture();
        let store = Store::new(&root, plan.clone()).unwrap();
        let mut fake = Fake::new(&plan);
        fake.fail = true;
        assert!(store.dispatch(&fake, || Ok(())).is_err());
        let reopened = Store::new(&root, plan).unwrap();
        assert_eq!(reopened.state().unwrap(), State::Dispatching);
        assert!(reopened.dispatch(&fake, || Ok(())).is_err());
        assert!(reopened.cancel().is_err());
        reopened.reconcile(&fake).unwrap().unwrap();
        assert!(matches!(reopened.state().unwrap(), State::Dispatched(_)));
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn revoked_owner_after_retention_never_calls_executor() {
        let (root, plan) = fixture();
        let store = Store::new(&root, plan.clone()).unwrap();
        let fake = Fake::new(&plan);
        let checks = AtomicUsize::new(0);
        assert!(store
            .dispatch(&fake, || {
                ensure!(checks.fetch_add(1, Ordering::SeqCst) == 0, "Owner changed");
                Ok(())
            })
            .is_err());
        assert_eq!(store.state().unwrap(), State::Dispatching);
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn stale_source_cancellation_and_unsupported_never_dispatch() {
        let (root, plan) = fixture();
        let store = Store::new(&root, plan.clone()).unwrap();
        let fake = Fake::new(&plan);
        assert!(store.dispatch(&UnsupportedExecutor, || Ok(())).is_err());
        assert!(store
            .dispatch(&fake, || anyhow::bail!("Owner revoked"))
            .is_err());
        assert_eq!(store.state().unwrap(), State::Prepared);
        let mut changed = plan;
        changed.source = "b".repeat(40);
        assert!(Store::new(&root, changed).is_err());
        store.cancel().unwrap();
        assert!(store.dispatch(&fake, || Ok(())).is_err());
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn no_clobber_corruption_and_failed_storage_hold_original() {
        let (root, plan) = fixture();
        let store = Store::new(&root, plan.clone()).unwrap();
        let peer = Store::new(&root, plan.clone()).unwrap();
        store.cancel().unwrap();
        assert!(peer.publish(1, State::Dispatching).is_err());
        assert_eq!(store.state().unwrap(), State::Cancelled);
        std::fs::write(
            store.directory.join("00000000000000000001.json"),
            b"{partial",
        )
        .unwrap();
        assert!(Store::new(&root, plan.clone()).is_err());
        let bad_root = root.join("not-a-directory");
        std::fs::write(&bad_root, b"preserve").unwrap();
        assert!(Store::new(&bad_root, plan).is_err());
        assert_eq!(std::fs::read(bad_root).unwrap(), b"preserve");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn foreign_receipt_is_held_and_positive_control_dispatches_once() {
        let (root, plan) = fixture();
        let store = Store::new(&root, plan.clone()).unwrap();
        let mut fake = Fake::new(&plan);
        fake.receipt.source = "b".repeat(40);
        assert!(store.dispatch(&fake, || Ok(())).is_err());
        assert_eq!(store.state().unwrap(), State::Dispatching);
        assert!(store.reconcile(&fake).is_err());
        fake.receipt.source = plan.source;
        store.reconcile(&fake).unwrap();
        assert!(matches!(store.state().unwrap(), State::Dispatched(_)));
        assert!(store.dispatch(&fake, || Ok(())).is_err());
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(root).unwrap();
    }
}
