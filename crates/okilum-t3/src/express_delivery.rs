//! Exact-build recipient handoff contracts. No signing, sending or script execution.
use crate::{
    express_execution::{digest, Plan, State, Store},
    express_session::Platform,
};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::Path,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    pub request_hash: String,
    pub recipient: String,
    pub platform: Platform,
    pub source: String,
    pub build: String,
    pub artifact: String,
    pub artifact_sha256: String,
    pub tester_page: String,
    pub script_sha256: String,
    pub signer: String,
}
impl Delivery {
    pub fn validate(&self, plan: &Plan) -> Result<()> {
        ensure!(
            self.request_hash == plan.request_hash
                && self.recipient == plan.recipient
                && self.source == plan.source
                && plan.platforms.contains(&self.platform),
            "Delivery belongs to another request, source, platform or recipient"
        );
        ensure!(
            !self.build.trim().is_empty()
                && self.build.len() <= 256
                && !self.signer.trim().is_empty()
                && self.signer.len() <= 256,
            "Missing exact build or trusted signer"
        );
        for hash in [&self.artifact_sha256, &self.script_sha256] {
            ensure!(
                hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                "Missing artifact/script SHA256"
            );
        }
        for link in [&self.artifact, &self.tester_page] {
            let url = url::Url::parse(link)?;
            ensure!(
                url.scheme() == "https"
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none(),
                "Delivery needs a durable HTTPS URL without credentials or expiring query"
            );
        }
        Ok(())
    }
    /// Accessible text for a future approved tester-page host. Values remain
    /// escaped text; this packet contains no executable controls or remote code.
    pub fn tester_page_html(&self, plan: &Plan) -> Result<String> {
        self.validate(plan)?;
        Ok(format!("<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><title>Express build handoff</title><main><h1>Express build for {}</h1><p>Build {} · {:?} · source {}</p><p><a href=\"{}\">Download artifact</a></p><p>Artifact SHA256: <code>{}</code></p><p>Required script signer: {}. Script SHA256: <code>{}</code>.</p><p>Verify the script signature against the trusted signer, inspect its full contents, then explicitly confirm before running. Use a vault copy and the separate Sprint app identity with its own state and auto update disabled. Installation and log upload each require recipient consent. This handoff does not confirm delivery, execution or acceptance.</p></main></html>", escape(&self.recipient), escape(&self.build), self.platform, escape(&self.source), escape(&self.artifact), escape(&self.artifact_sha256), escape(&self.signer), escape(&self.script_sha256)))
    }
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    pub delivery: Delivery,
    pub approved_by: String,
    pub recorded_at: u64,
}
impl Approval {
    /// Retains an explicit local approval only. No transport or recipient action.
    /// Publishing a changed artifact for the same platform requires reconciliation.
    pub fn retain(
        state: &Path,
        plan: &Plan,
        execution: &Store,
        delivery: Delivery,
        approved_by: &str,
        authorize: impl Fn() -> Result<()>,
    ) -> Result<Self> {
        delivery.validate(plan)?;
        ensure!(
            matches!(execution.state()?, State::Dispatched(receipt) if receipt.operation == plan.request_hash
            && receipt.source == plan.source && receipt.environment_id == plan.environment_id
            && receipt.project_id == plan.project_id && receipt.branch == plan.branch
            && receipt.platforms == plan.platforms),
            "A queued or foreign request cannot approve a build"
        );
        ensure!(
            !approved_by.trim().is_empty() && approved_by.len() <= 256,
            "Explicit approver required"
        );
        authorize()?;
        let approval = Self {
            delivery,
            approved_by: approved_by.into(),
            recorded_at: crate::unix_now(),
        };
        let directory = state
            .join("t3-express-delivery")
            .join(digest(&serde_json::to_vec(plan)?));
        std::fs::create_dir_all(&directory)?;
        let target = directory.join(format!("{:?}.json", approval.delivery.platform));
        let temporary = directory.join(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(&serde_json::to_vec(&approval)?)?;
            file.sync_all()?;
            authorize()?;
            std::fs::hard_link(&temporary, &target)?;
            std::fs::File::open(&directory)?.sync_all()?;
            Ok(approval)
        })();
        let _ = std::fs::remove_file(temporary);
        result
    }
    pub fn read(state: &Path, plan: &Plan, platform: Platform) -> Result<Option<Self>> {
        let path = state
            .join("t3-express-delivery")
            .join(digest(&serde_json::to_vec(plan)?))
            .join(format!("{platform:?}.json"));
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut bytes = Vec::new();
        file.take(32769).read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 32768, "Delivery receipt exceeds bound");
        let approval: Self = serde_json::from_slice(&bytes)?;
        approval.delivery.validate(plan)?;
        ensure!(
            approval.delivery.platform == platform && !approval.approved_by.trim().is_empty(),
            "Invalid delivery approval identity"
        );
        Ok(Some(approval))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::express_execution::{DispatchReceipt, Executor};
    fn fixture() -> (std::path::PathBuf, Plan, Delivery) {
        let plan = Plan {
            request_id: "request".into(),
            request_hash: "c".repeat(64),
            environment_id: "env".into(),
            project_id: "project".into(),
            manager_thread_id: "manager".into(),
            source: "a".repeat(40),
            branch: "sprint/express-test".into(),
            platforms: vec![Platform::Linux],
            recipient: "<Synthetic tester>".into(),
        };
        let delivery = Delivery {
            request_hash: plan.request_hash.clone(),
            recipient: plan.recipient.clone(),
            platform: Platform::Linux,
            source: plan.source.clone(),
            build: "build-1".into(),
            artifact: "https://example.test/artifact".into(),
            artifact_sha256: "b".repeat(64),
            tester_page: "https://example.test/tester".into(),
            script_sha256: "d".repeat(64),
            signer: "Synthetic signer".into(),
        };
        (
            std::env::temp_dir().join(format!("express-delivery-{}", uuid::Uuid::new_v4())),
            plan,
            delivery,
        )
    }
    struct Fake;
    impl Executor for Fake {
        fn available(&self) -> bool {
            true
        }
        fn dispatch(&self, p: &Plan) -> Result<DispatchReceipt> {
            Ok(DispatchReceipt {
                operation: p.request_hash.clone(),
                executor_thread_id: "executor".into(),
                environment_id: p.environment_id.clone(),
                project_id: p.project_id.clone(),
                source: p.source.clone(),
                branch: p.branch.clone(),
                platforms: p.platforms.clone(),
                priority_at_safe_boundary: true,
                isolated_workspace: true,
            })
        }
        fn reconcile(&self, _: &Plan) -> Result<Option<DispatchReceipt>> {
            Ok(None)
        }
    }
    #[test]
    fn approval_requires_dispatch_and_consent_then_survives_restart_without_clobber() {
        let (root, plan, delivery) = fixture();
        let store = Store::new(&root, plan.clone()).unwrap();
        assert!(
            Approval::retain(&root, &plan, &store, delivery.clone(), "Tester", || Ok(())).is_err()
        );
        store.dispatch(&Fake, || Ok(())).unwrap();
        assert!(Approval::retain(
            &root,
            &plan,
            &store,
            delivery.clone(),
            "Tester",
            || anyhow::bail!("Cancelled")
        )
        .is_err());
        assert!(Approval::read(&root, &plan, Platform::Linux)
            .unwrap()
            .is_none());
        let approved =
            Approval::retain(&root, &plan, &store, delivery.clone(), "Tester", || Ok(())).unwrap();
        assert_eq!(
            Approval::read(&root, &plan, Platform::Linux).unwrap(),
            Some(approved.clone())
        );
        let mut other = delivery;
        other.artifact = "https://example.test/replaced".into();
        assert!(Approval::retain(&root, &plan, &store, other, "Tester", || Ok(())).is_err());
        assert_eq!(
            Approval::read(&root, &plan, Platform::Linux).unwrap(),
            Some(approved)
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn foreign_source_and_expiring_url_are_rejected_and_html_is_escaped() {
        let (_, plan, delivery) = fixture();
        let html = delivery.tester_page_html(&plan).unwrap();
        assert!(html.contains("&lt;Synthetic tester&gt;"));
        assert!(!html.contains("<Synthetic tester>"));
        let mut changed = delivery.clone();
        changed.source = "f".repeat(40);
        assert!(changed.validate(&plan).is_err());
        changed = delivery;
        changed.artifact.push_str("?token=expires");
        assert!(changed.validate(&plan).is_err());
    }
}
