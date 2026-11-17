//! Scoped evidence export (#3012). A bundle is a new directory holding what
//! the Workspace observed and loaded, never more: the typed facts of the
//! selection, its related Events, the exact log bytes received in this
//! session, a `manifest.json` saying where each came from and what was not
//! collected, and `SHA256SUMS` over every file. Nothing is read from the
//! cluster to build it. docs/kubernetes-workspace.md §Evidence export.
use super::dock::{Phase, Session};
use super::model::{self, Data, EventGroup, Key, LogTarget, Resolved, UidCheck};
use okilum_core::kubernetes::{Completeness, Failure, List, Slot, Staleness, View};
use okilum_core::log::stream::{MAX_BYTES, MAX_LINES, MAX_LINE_BYTES};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub(crate) const FORMAT: &str = "okilum.kubernetes-evidence/1";

/// Kept out of every bundle, and said so in the manifest.
pub(crate) const EXCLUDED: [&str; 5] = [
    "kubeconfig users, tokens, client certificates and exec credential settings (never read)",
    "Secrets (never read)",
    "container environment values and exec probe commands (not retained by the read adapter)",
    "kubectl stderr diagnostics of log sessions",
    "failure details of authentication, credential-helper, kubeconfig and unclassified kubectl failures (kind only)",
];

/// The log view's state at export time, from the LogView itself.
pub(crate) struct LogViewState {
    pub snapshot: Option<std::sync::Arc<okilum_core::log::stream::Snapshot>>,
    pub filter: Value,
}

pub(crate) struct Inputs<'a> {
    pub data: Option<&'a Data>,
    pub selected: Option<&'a Key>,
    pub related_uids: Vec<String>,
    pub lens: &'static str,
    pub session: Option<&'a Session>,
    pub displayed: Option<&'a LogTarget>,
    pub tail_lines: u32,
    pub view: Option<LogViewState>,
    pub generated_at: SystemTime,
    pub app_version: &'a str,
    pub app_source: &'a str,
}

/// Files in bundle order. `manifest.json` and `SHA256SUMS` come last.
pub(crate) struct Bundle {
    pub name: String,
    pub files: Vec<(String, Vec<u8>)>,
}

fn rfc3339(at: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(at).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A failure as evidence: its kind always; its detail only for kinds whose
/// detail is the API server's own answer, never credential-helper output.
pub(crate) fn failure_json(failure: &Failure) -> Value {
    let detail = match failure {
        Failure::Forbidden { .. }
        | Failure::NotFound { .. }
        | Failure::Unavailable { .. }
        | Failure::InvalidRequest { .. }
        | Failure::Malformed { .. }
        | Failure::Oversized { .. }
        | Failure::TimedOut { .. }
        | Failure::Cancelled => Some(failure.summary()),
        Failure::AuthExpired { .. }
        | Failure::InteractiveLogin { .. }
        | Failure::Configuration { .. }
        | Failure::Failed { .. } => None,
    };
    json!({"kind": model::failure_kind(failure), "detail": detail})
}

fn source_json<T>(name: &str, slot: &Slot<List<T>>) -> Value {
    let list = |o: &okilum_core::kubernetes::Observation<List<T>>| {
        json!({
            "observed_at": rfc3339(o.observed_at),
            "resource_version": o.value.resource_version.as_ref().map(|v| v.0.clone()),
            "items": o.value.items.len(),
            "complete": o.value.is_complete(),
            "truncated": match o.value.completeness {
                Completeness::Complete => Value::Null,
                Completeness::Truncated { remaining } => json!({"remaining": remaining}),
            },
            "rejected_items": o.value.rejected_total,
        })
    };
    let (state, last, failure) = match slot.view(SystemTime::now(), model::FRESH_FOR) {
        View::Unread => ("unread", Value::Null, Value::Null),
        View::Fresh(o) => ("fresh", list(o), Value::Null),
        View::Stale {
            last,
            why: Staleness::Aged { .. },
        } => ("aged", list(last), Value::Null),
        View::Stale {
            last,
            why: Staleness::Failed { failure, at },
        } => (
            "stale",
            list(last),
            json!({"at": rfc3339(at), "failure": failure_json(failure)}),
        ),
        View::Failed { failure, at } => (
            "failed",
            Value::Null,
            json!({"at": rfc3339(at), "failure": failure_json(failure)}),
        ),
    };
    json!({"kind": name, "state": state, "last_read": last, "latest_failure": failure})
}

fn selection_json(data: &Data, key: &Key) -> Value {
    match model::resolve(data, key) {
        Resolved::Gone { replacement } => json!({
            "kind": key_kind(key),
            "uid": key.uid(),
            "state": "not in the latest read",
            "same_name_replacement_uid": replacement.as_ref().map(|k| k.uid().to_string()),
        }),
        resolved => {
            let meta = match &resolved {
                Resolved::Pod(p) | Resolved::Container(p, _) => &p.meta,
                Resolved::Deployment(d) => &d.meta,
                Resolved::ReplicaSet(r) => &r.meta,
                Resolved::Service(s) => &s.meta,
                Resolved::Gone { .. } => unreachable!(),
            };
            let container = match &resolved {
                Resolved::Container(_, c) => json!({
                    "name": c.name,
                    "kind": model::kind_label(c.kind),
                    "status": model::container_status(c),
                    "restart_count": c.restart_count,
                    "previous_instance_observed": c.previous_instance_observed(),
                }),
                _ => Value::Null,
            };
            json!({
                "kind": key_kind(key),
                "name": meta.name,
                "namespace": meta.namespace,
                "uid": meta.uid,
                "resource_version": meta.resource_version.0,
                "container": container,
            })
        }
    }
}

fn key_kind(key: &Key) -> &'static str {
    match key {
        Key::Object { kind, .. } => kind.name(),
        Key::Container { .. } => "Pod",
    }
}

fn target_json(target: &LogTarget) -> Value {
    json!({
        "context": target.context,
        "cluster": target.cluster,
        "server": target.server,
        "namespace": target.namespace,
        "pod": target.pod,
        "pod_uid": target.pod_uid,
        "container": target.container,
        "instance": if target.previous { "previous" } else { "current" },
    })
}

fn phase_name(phase: &Phase) -> &'static str {
    match phase {
        Phase::Verifying => "verifying",
        Phase::Receiving => "receiving",
        Phase::Stopping => "stopping",
        Phase::Ended => "ended",
        Phase::Stopped => "stopped",
        Phase::Truncated(_) => "stopped at a loading limit",
        Phase::Disconnected(_) => "disconnected",
        Phase::Failed(_) => "failed",
        Phase::Replaced { .. } => "not started: pod replaced",
    }
}

/// Safe in a file name on every desktop OS.
fn slug(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    cleaned.chars().take(60).collect()
}

pub(crate) fn build(inputs: Inputs) -> Bundle {
    let stamp = chrono::DateTime::<chrono::Utc>::from(inputs.generated_at)
        .format("%Y%m%dT%H%M%SZ")
        .to_string();
    let name = match inputs.data {
        Some(data) => format!(
            "okilum-kubernetes-evidence-{}-{}-{stamp}",
            slug(&data.scope.connection.context),
            slug(&data.scope.namespace)
        ),
        None => format!("okilum-kubernetes-evidence-{stamp}"),
    };
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();

    let mut selection = Value::Null;
    if let (Some(data), Some(key)) = (inputs.data, inputs.selected) {
        selection = selection_json(data, key);
        if let Some(text) = model::manifest(&model::resolve(data, key)) {
            let body = format!("# {}\n{text}", model::MANIFEST_NOTE);
            files.push(("facts/selection.yaml".into(), body.into_bytes()));
        }
        let uids: Vec<&str> = inputs.related_uids.iter().map(String::as_str).collect();
        let events: Vec<Value> = model::event_groups(data, &uids)
            .into_iter()
            .map(|g: EventGroup| {
                json!({
                    "involved": g.involved, "involved_uid": g.involved_uid, "type": g.kind,
                    "reason": g.reason, "count": g.count, "last": g.last, "message": g.message,
                })
            })
            .collect();
        let events = json!({
            "source": source_json("Event", &data.events),
            "selected_by": "involvedObject.uid of the selection and its owned objects",
            "groups": events,
        });
        files.push((
            "facts/events.json".into(),
            serde_json::to_vec_pretty(&events).unwrap_or_default(),
        ));
    }

    let mut log = Value::Null;
    if let Some(session) = inputs.session {
        let snapshot = inputs.view.as_ref().and_then(|v| v.snapshot.clone());
        let file = snapshot.as_ref().map(|snapshot| {
            let path = format!(
                "logs/{}_{}_{}.log",
                slug(&session.target.pod),
                slug(&session.target.container),
                if session.target.previous {
                    "previous"
                } else {
                    "current"
                }
            );
            files.push((path.clone(), snapshot.bytes().to_vec()));
            (path, snapshot.bytes().len(), snapshot.index().len())
        });
        let uid_check = inputs
            .data
            .map(|data| match model::check_listed(&session.target, data) {
                UidCheck::Same => json!("same"),
                UidCheck::Replaced { uid } => json!({"replaced_by": uid}),
                UidCheck::Missing => json!("missing"),
                UidCheck::Unknown => json!("unknown"),
            });
        let rows_from = inputs.displayed.map(target_json);
        log = json!({
            "target": target_json(&session.target),
            "phase": phase_name(&session.phase),
            "phase_detail": match &session.phase {
                // Bounds and transport errors of the session itself; kubectl
                // stderr is not exported.
                Phase::Truncated(e) | Phase::Disconnected(e) | Phase::Failed(e) => json!(e),
                Phase::Replaced { uid } => json!({"current_uid": uid}),
                _ => Value::Null,
            },
            "uid_verified_at": session.verified_at.map(rfc3339),
            "uid_against_latest_pods_read": uid_check,
            "stream_new_lines": session.follow,
            "initial_tail_lines": inputs.tail_lines,
            "rows_from_target": rows_from,
            "rows_from_target_differs": inputs.displayed.is_some_and(|d| d != &session.target),
            "file": file.as_ref().map(|f| f.0.clone()),
            "bytes": file.as_ref().map(|f| f.1),
            "lines": file.as_ref().map(|f| f.2),
            "incomplete_trailing_bytes_not_exported": session.pending_bytes,
            "limits": {"max_bytes": MAX_BYTES, "max_lines": MAX_LINES, "max_line_bytes": MAX_LINE_BYTES},
            "view_filter": inputs.view.as_ref().map(|v| v.filter.clone()),
            "coverage": format!(
                "Only complete lines received in this session: up to {} initial lines, then {}. \
                 Older history, other instances and rotated logs are not included. kubectl logs \
                 addresses the Pod by name; compare uid_verified_at and uid_against_latest_pods_read.",
                inputs.tail_lines,
                if session.follow { "new lines until the session ended" } else { "no further lines" }
            ),
            "log_file_is_unfiltered": true,
        });
    }

    let artifacts: Vec<Value> = files
        .iter()
        .map(|(path, bytes)| json!({"path": path, "bytes": bytes.len(), "sha256": sha256(bytes)}))
        .collect();
    let (scope, sources) = match inputs.data {
        Some(data) => (
            json!({
                "context": data.scope.connection.context,
                "cluster": data.scope.connection.cluster,
                "server": data.scope.connection.server,
                "namespace": data.scope.namespace,
            }),
            json!([
                source_json("Pod", &data.pods),
                source_json("Deployment", &data.deployments),
                source_json("ReplicaSet", &data.replica_sets),
                source_json("Service", &data.services),
                source_json("EndpointSlice", &data.slices),
                source_json("Event", &data.events),
                source_json("HorizontalPodAutoscaler", &data.hpas),
            ]),
        ),
        None => (Value::Null, json!([])),
    };
    let manifest = json!({
        "format": FORMAT,
        "generated_at": rfc3339(inputs.generated_at),
        "app": {"version": inputs.app_version, "source": inputs.app_source},
        "read_only": true,
        "collected_by": "kubectl get --raw (one bounded page per kind) and kubectl logs; nothing was read to build this bundle",
        "scope": scope,
        "lens": inputs.lens,
        "sources": sources,
        "selection": selection,
        "log_session": log,
        "excluded": EXCLUDED,
        "artifacts": artifacts,
    });
    let manifest = serde_json::to_vec_pretty(&manifest).unwrap_or_default();
    files.push(("manifest.json".into(), manifest));
    let sums: String = files
        .iter()
        .map(|(path, bytes)| format!("{}  {path}\n", sha256(bytes)))
        .collect();
    files.push(("SHA256SUMS".into(), sums.into_bytes()));
    Bundle { name, files }
}

/// Writes the bundle as a new directory inside `parent`. The directory
/// appears whole or not at all, and never replaces an existing one.
pub(crate) fn write(parent: &Path, bundle: &Bundle) -> std::io::Result<PathBuf> {
    let target = parent.join(&bundle.name);
    if target.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} already exists", target.display()),
        ));
    }
    let staging = parent.join(format!(".{}.partial", bundle.name));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    let result = (|| {
        for (path, bytes) in &bundle.files {
            let file = staging.join(path);
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&file, bytes)?;
        }
        std::fs::rename(&staging, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result.map(|()| target)
}

/// File count and size, for the confirmation line.
pub(crate) fn summary(bundle: &Bundle) -> String {
    let bytes: usize = bundle.files.iter().map(|(_, b)| b.len()).sum();
    format!("{} files, {} bytes", bundle.files.len(), bytes)
}

#[cfg(test)]
#[path = "evidence_tests.rs"]
mod tests;
