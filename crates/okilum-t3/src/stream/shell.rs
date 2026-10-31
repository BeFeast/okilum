//! Live thread directory over T3's subscribeShell RPC. A reconnect requests a
//! fresh complete snapshot; no cursor survives without its projection.
use super::*;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub status: String,
    #[serde(default)]
    pub active_run_id: Option<String>,
    #[serde(default)]
    pub latest_run_id: Option<String>,
    #[serde(default)]
    pub archived_at: Option<String>,
    #[serde(default)]
    pub deleted_at: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Update {
    Snapshot(Vec<Thread>),
    Thread(Thread, bool),
    RemoveThread(String),
    RemoveProject(String),
    Unchanged,
}

/// Only the explicitly bound Manager/executor projects enter the UI. A server
/// directory cannot redirect an action to a similarly named foreign project.
pub struct Directory {
    allowed: BTreeSet<String>,
    pub threads: BTreeMap<String, Thread>,
    pub synchronized: bool,
}
impl Directory {
    pub fn new(projects: impl IntoIterator<Item = String>) -> Self {
        Self {
            allowed: projects.into_iter().filter(|s| !s.is_empty()).collect(),
            threads: BTreeMap::new(),
            synchronized: false,
        }
    }
    fn visible(&self, thread: &Thread) -> bool {
        self.allowed.contains(&thread.project_id)
            && thread.archived_at.is_none()
            && thread.deleted_at.is_none()
    }
    pub fn apply(&mut self, updates: &[Update]) -> bool {
        let mut changed = false;
        for update in updates {
            match update {
                Update::Snapshot(threads) => {
                    let next = threads
                        .iter()
                        .filter(|t| self.visible(t))
                        .map(|t| (t.id.clone(), t.clone()))
                        .collect();
                    changed |= self.threads != next || !self.synchronized;
                    self.threads = next;
                    self.synchronized = true;
                }
                Update::Thread(thread, active) => {
                    if *active && self.visible(thread) {
                        if self.threads.get(&thread.id) != Some(thread) {
                            self.threads.insert(thread.id.clone(), thread.clone());
                            changed = true;
                        }
                    } else {
                        changed |= self.threads.remove(&thread.id).is_some();
                    }
                }
                Update::RemoveThread(id) => changed |= self.threads.remove(id).is_some(),
                Update::RemoveProject(id) => {
                    let before = self.threads.len();
                    self.threads.retain(|_, t| &t.project_id != id);
                    changed |= self.threads.len() != before;
                }
                Update::Unchanged => {}
            }
        }
        changed
    }
}

fn invalid() -> StreamError {
    StreamError::Protocol("invalid thread directory")
}
fn id(value: &Value, name: &str) -> StreamResult<String> {
    value[name]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(invalid)
}
fn sequence(value: &Value) -> StreamResult<u64> {
    value
        .as_u64()
        .filter(|s| *s <= 9_007_199_254_740_991)
        .ok_or_else(invalid)
}
fn thread(value: &Value) -> StreamResult<Thread> {
    let row: Thread = serde_json::from_value(value.clone()).map_err(|_| invalid())?;
    if row.id.trim().is_empty() || row.project_id.trim().is_empty() || row.status.trim().is_empty()
    {
        return Err(invalid());
    }
    Ok(row)
}
fn decode(value: &Value, mut cursor: Option<u64>) -> StreamResult<(Vec<Update>, Option<u64>)> {
    let values = value["values"]
        .as_array()
        .filter(|s| !s.is_empty())
        .ok_or_else(invalid)?;
    let mut updates = Vec::with_capacity(values.len());
    for value in values {
        if value["kind"] == "snapshot" {
            let snapshot = &value["snapshot"];
            let next = sequence(&snapshot["snapshotSequence"])?;
            if let Some(roots) = value.get("resolvedRepositoryIdentityRoots") {
                // T3's shellReducer treats this as repository-identity enrichment:
                // it changes neither thread structure nor the replay sequence.
                // Its empty threads array is not an authoritative replacement.
                if cursor.is_none()
                    || !roots
                        .as_array()
                        .is_some_and(|roots| roots.iter().all(Value::is_string))
                {
                    return Err(invalid());
                }
                updates.push(Update::Unchanged);
                continue;
            }
            let rows = snapshot["threads"].as_array().ok_or_else(invalid)?;
            let mut seen = BTreeSet::new();
            let mut threads = Vec::with_capacity(rows.len());
            for row in rows {
                let row = thread(row)?;
                if !seen.insert(row.id.clone()) {
                    return Err(invalid());
                }
                threads.push(row);
            }
            updates.push(Update::Snapshot(threads));
            cursor = Some(next);
            continue;
        }
        if cursor.is_none() {
            return Err(invalid());
        }
        if value["kind"] == "synchronized" {
            continue;
        }
        let next = sequence(&value["sequence"])?;
        if cursor.is_some_and(|old| next <= old) {
            continue;
        }
        let update = match value["kind"].as_str() {
            Some("thread.updated") => {
                let active = match value["location"].as_str() {
                    Some("active") => true,
                    Some("archive") => false,
                    _ => return Err(invalid()),
                };
                Update::Thread(thread(&value["thread"])?, active)
            }
            Some("thread.removed") => Update::RemoveThread(id(value, "threadId")?),
            Some("project.removed") => Update::RemoveProject(id(value, "projectId")?),
            Some("project.updated") => {
                id(&value["project"], "id")?;
                Update::Unchanged
            }
            _ => return Err(invalid()),
        };
        updates.push(update);
        cursor = Some(next);
    }
    Ok((updates, cursor))
}

pub struct Subscription {
    inner: ThreadSubscription,
    sequence: Option<u64>,
}
impl NativeClient {
    pub fn subscribe_shell(&self) -> StreamResult<Subscription> {
        if !self.access.allowed() {
            return Err(StreamError::Closed);
        }
        self.token.validate(&self.environment, crate::unix_now())?;
        let (socket, cancel) = self.open_socket()?;
        let mut inner = ThreadSubscription {
            access: self.access.clone(),
            socket,
            cancel,
            origin: self.environment.origin(),
            thread_id: String::new(),
            cursor: None,
            closed: false,
        };
        inner.send(json!({"_tag":"Request","id":"1","tag":"orchestration.subscribeShell","payload":{"requestCompletionMarker":true},"headers":[]}))?;
        Ok(Subscription {
            inner,
            sequence: None,
        })
    }
}
impl Subscription {
    pub fn cancellation(&self) -> StreamResult<CancelSubscription> {
        self.inner.cancellation()
    }
    /// Validate the whole chunk, apply it atomically, then acknowledge it.
    pub fn receive(
        &mut self,
        apply: impl FnOnce(&[Update]) -> StreamResult<()>,
    ) -> StreamResult<()> {
        let result = (|| {
            if self.inner.closed || !self.inner.access.allowed() {
                return Err(StreamError::Closed);
            }
            let value = self.inner.read_chunk()?;
            let (updates, next) = decode(&value, self.sequence)?;
            apply(&updates)?;
            self.sequence = next;
            self.inner.send(json!({"_tag":"Ack","requestId":"1"}))
        })();
        if result.is_err() {
            self.inner.closed = true;
            let _ = self.inner.cancel.shutdown(Shutdown::Both);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(id: &str, project: &str) -> Value {
        json!({"id":id,"projectId":project,"title":"Fixture","status":"idle"})
    }
    #[test]
    fn scoped_directory_tracks_live_changes_and_snapshot_replacement() {
        let mut directory = Directory::new(["bound".into()]);
        let (updates, sequence) = decode(&json!({"values":[{"kind":"snapshot","snapshot":{"snapshotSequence":20,"threads":[row("manager","bound"),row("foreign","other")]}}]}), None).unwrap();
        assert!(directory.apply(&updates));
        assert_eq!(
            directory.threads.keys().collect::<Vec<_>>(),
            [&"manager".to_owned()]
        );
        let mut running = row("manager", "bound");
        running["status"] = "running".into();
        running["activeRunId"] = "run-a".into();
        let (updates, _) = decode(&json!({"values":[{"kind":"thread.updated","location":"active","sequence":23,"thread":running}]}),sequence).unwrap();
        assert!(directory.apply(&updates));
        assert_eq!(
            directory.threads["manager"].active_run_id.as_deref(),
            Some("run-a")
        );
        assert!(
            !directory.apply(&updates),
            "unchanged metadata does not redraw"
        );
        let (updates, _) = decode(&json!({"values":[{"kind":"snapshot","snapshot":{"snapshotSequence":1,"threads":[row("replacement","bound")]}}]}),Some(23)).unwrap();
        assert!(directory.apply(&updates));
        assert!(!directory.threads.contains_key("manager"));
        assert!(directory.threads.contains_key("replacement"));
    }
    #[test]
    fn malformed_chunk_does_not_publish_a_partial_directory() {
        let mut directory = Directory::new(["bound".into()]);
        let chunk = json!({"values":[{"kind":"snapshot","snapshot":{"snapshotSequence":4,"threads":[row("valid","bound"),{"id":"invalid"}]}}]});
        assert!(decode(&chunk, None).is_err());
        assert!(!directory.synchronized && directory.threads.is_empty());
        let (updates, _) = decode(&json!({"values":[{"kind":"snapshot","snapshot":{"snapshotSequence":4,"threads":[row("valid","bound")]}}]}),None).unwrap();
        assert!(
            directory.apply(&updates),
            "positive control: the valid snapshot publishes"
        );
        let (updates, _) = decode(&json!({"values":[{"kind":"thread.updated","sequence":5,"location":"archive","thread":row("valid","bound")}]}),Some(4)).unwrap();
        assert!(directory.apply(&updates));
        assert!(
            directory.threads.is_empty(),
            "archived threads do not remain actionable"
        );
    }

    #[test]
    fn repository_enrichment_keeps_threads_and_does_not_advance_the_structural_cursor() {
        let mut directory = Directory::new(["bound".into()]);
        let mut running = row("manager", "bound");
        running["status"] = json!("running");
        let batch = json!({"values":[
            {"kind":"snapshot","snapshot":{"snapshotSequence":20,"threads":[row("manager","bound")]}},
            {"kind":"snapshot","resolvedRepositoryIdentityRoots":["/repo"],"snapshot":{"snapshotSequence":900,"threads":[],"projects":[]}},
            {"kind":"thread.updated","location":"active","sequence":21,"thread":running}
        ]});
        let (updates, cursor) = decode(&batch, None).unwrap();
        directory.apply(&updates);
        assert_eq!(cursor, Some(21));
        assert_eq!(directory.threads.len(), 1);
        assert_eq!(directory.threads["manager"].status, "running");
        let invalid = json!({"values":[{"kind":"snapshot","resolvedRepositoryIdentityRoots":false,"snapshot":{"snapshotSequence":22,"threads":[]}}]});
        assert!(decode(&invalid, cursor).is_err());
        // An authoritative empty snapshot must still clear the directory.
        let empty =
            json!({"values":[{"kind":"snapshot","snapshot":{"snapshotSequence":1,"threads":[]}}]});
        directory.apply(&decode(&empty, cursor).unwrap().0);
        assert!(directory.threads.is_empty());
    }
}
