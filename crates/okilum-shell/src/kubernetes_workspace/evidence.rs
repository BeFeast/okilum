//! Scoped evidence export (#3012). A bundle is a new directory holding what
//! the Workspace observed and loaded, never more: the typed facts of the
//! selection, its related Events, the exact log bytes received in this
//! session, a `manifest.json` saying where each came from and what was not
//! collected, and `SHA256SUMS` over every file. Nothing is read from the
//! cluster to build it. docs/kubernetes-workspace.md §Evidence export.
use super::dock::{Phase, Session, Shown};
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
    "failure text from kubectl's stderr, including lines that look like a server answer (kind only; text only when Okilum wrote it)",
];

/// The log view's state at export time, from the LogView itself.
#[derive(Clone)]
pub(crate) struct LogViewState {
    /// The rows `displayed` describes: a snapshot not yet handed to the
    /// view is preferred, so bytes and provenance always belong together.
    pub snapshot: Option<std::sync::Arc<okilum_core::log::stream::Snapshot>>,
    pub filter: Value,
}

pub(crate) struct Inputs<'a> {
    pub data: Option<&'a Data>,
    pub selected: Option<&'a Key>,
    pub related_uids: Vec<String>,
    pub lens: &'static str,
    pub session: Option<&'a Session>,
    /// Who sent the rows in the view (may be an earlier session).
    pub displayed: Option<&'a Shown>,
    /// The pinned session's generation, to tell its rows from older ones.
    pub current_generation: u64,
    pub tail_lines: u32,
    pub view: Option<LogViewState>,
    pub generated_at: SystemTime,
    pub app_version: &'a str,
    pub app_source: &'a str,
}

/// An owned copy of everything a bundle is built from, taken on the UI
/// thread (cheap clones; log bytes are shared, not copied). Building,
/// serialising and hashing up to 8 MiB of logs happen later, off it.
#[derive(Clone)]
pub(crate) struct Capture {
    pub data: Option<Data>,
    pub selected: Option<Key>,
    pub related_uids: Vec<String>,
    pub lens: &'static str,
    pub session: Option<Session>,
    pub displayed: Option<Shown>,
    pub current_generation: u64,
    pub tail_lines: u32,
    pub view: Option<LogViewState>,
    pub generated_at: SystemTime,
    pub app_version: &'static str,
    pub app_source: &'static str,
}

impl Capture {
    pub(crate) fn build(&self) -> Bundle {
        build(Inputs {
            data: self.data.as_ref(),
            selected: self.selected.as_ref(),
            related_uids: self.related_uids.clone(),
            lens: self.lens,
            session: self.session.as_ref(),
            displayed: self.displayed.as_ref(),
            current_generation: self.current_generation,
            tail_lines: self.tail_lines,
            view: self.view.clone(),
            generated_at: self.generated_at,
            app_version: self.app_version,
            app_source: self.app_source,
        })
    }
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

/// A failure as evidence: its kind always, and text only when Okilum wrote
/// it (an invalid request, a size or time limit, a cancel). Every other
/// failure's text comes from kubectl's stderr, which a credential helper can
/// also write to, in any form, including lines that look like a server
/// answer. A category is not redaction, and no line of that stream is
/// trusted, so none of it is exported.
pub(crate) fn failure_json(failure: &Failure) -> Value {
    let own = match failure {
        Failure::InvalidRequest { .. }
        | Failure::Oversized { .. }
        | Failure::TimedOut { .. }
        | Failure::Cancelled => Some(failure.summary()),
        _ => None,
    };
    json!({"kind": model::failure_kind(failure), "detail": own})
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
        Phase::NotStarted(_) => "not started: check before kubectl logs failed",
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

    // The rows and the session are described separately: after a retarget,
    // a failed check or a restart the view still shows an earlier session's
    // rows, labelled with that session's own target, time and coverage.
    // Logs and their session belong to a context and namespace. The view
    // keeps them across scope changes, so one from another scope than the
    // bundle's is left out and named, never filed under this scope.
    let scope_of = |t: &LogTarget| format!("{} / {}", t.context, t.namespace);
    let in_scope = |t: &LogTarget| inputs.data.is_some_and(|d| t.in_scope(&d.scope));
    let mut log_rows = Value::Null;
    if let Some(shown) = inputs.displayed.filter(|s| !in_scope(&s.target)) {
        log_rows = json!({
            "excluded": format!(
                "The log view holds rows from {}, not this bundle's scope; they are not exported. Export from that scope to include them.",
                scope_of(&shown.target)
            ),
        });
    } else if let (Some(shown), Some(snapshot)) = (
        inputs.displayed,
        inputs.view.as_ref().and_then(|v| v.snapshot.clone()),
    ) {
        let path = format!(
            "logs/{}_{}_{}.log",
            slug(&shown.target.pod),
            slug(&shown.target.container),
            if shown.target.previous {
                "previous"
            } else {
                "current"
            }
        );
        files.push((path.clone(), snapshot.bytes().to_vec()));
        let current = inputs.session.is_some() && shown.generation == inputs.current_generation;
        log_rows = json!({
            "file": path,
            "bytes": snapshot.bytes().len(),
            "lines": snapshot.index().len(),
            "from_session": {
                "target": target_json(&shown.target),
                "uid_verified_at": shown.verified_at.map(rfc3339),
                "stream_new_lines": shown.follow,
                "initial_tail_lines": shown.tail_lines,
                "is_the_pinned_session": current,
            },
            "incomplete_trailing_bytes_not_exported": if current {
                json!(inputs.session.map(|s| s.pending_bytes))
            } else {
                json!("unknown: from an earlier session")
            },
            "limits": {"max_bytes": MAX_BYTES, "max_lines": MAX_LINES, "max_line_bytes": MAX_LINE_BYTES},
            "view_filter": inputs.view.as_ref().map(|v| v.filter.clone()),
            "log_file_is_unfiltered": true,
            "coverage": format!(
                "Only complete lines received by that session: up to {} initial lines, then {}. \
                 Older history, other instances and rotated logs are not included. kubectl logs \
                 addresses the Pod by name; compare uid_verified_at with the Pods source.",
                shown.tail_lines,
                if shown.follow { "new lines until it ended or was replaced" } else { "no further lines" }
            ),
        });
    }

    let mut log_session = Value::Null;
    if let Some(session) = inputs.session.filter(|s| !in_scope(&s.target)) {
        log_session = json!({
            "excluded": format!(
                "The pinned log session belongs to {}, not this bundle's scope.",
                scope_of(&session.target)
            ),
        });
    } else if let Some(session) = inputs.session {
        let uid_check = inputs
            .data
            .map(|data| match model::check_listed(&session.target, data) {
                UidCheck::Same => json!("same"),
                UidCheck::Replaced { uid } => json!({"replaced_by": uid}),
                UidCheck::Missing => json!("missing"),
                UidCheck::Unknown => json!("unknown"),
            });
        log_session = json!({
            "target": target_json(&session.target),
            "phase": phase_name(&session.phase),
            "phase_detail": match &session.phase {
                // The session's own bounds and transport text; kubectl stderr
                // is never exported.
                Phase::Truncated(e) | Phase::Disconnected(e) | Phase::Failed(e) => json!(e),
                // Only the safe parts of a typed failure (see failure_json).
                Phase::NotStarted(failure) => failure_json(failure),
                Phase::Replaced { uid } => json!({"current_uid": uid}),
                _ => Value::Null,
            },
            "uid_verified_at": session.verified_at.map(rfc3339),
            "uid_against_latest_pods_read": uid_check,
            "stream_new_lines": session.follow,
            "initial_tail_lines": inputs.tail_lines,
            "has_sent_rows": inputs.displayed.is_some_and(|d| d.generation == inputs.current_generation),
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
        "log_rows": log_rows,
        "log_session": log_session,
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
/// appears whole or not at all and never replaces anything: files go to a
/// staging directory created exclusively for this call (no other writer can
/// share or delete it), and the commit cannot overwrite an existing path.
pub(crate) fn write(parent: &Path, bundle: &Bundle) -> std::io::Result<PathBuf> {
    let target = parent.join(&bundle.name);
    // Unique and exclusive (mkdtemp-style); dropped, it removes only itself.
    let staging = tempfile::Builder::new()
        .prefix(&format!(".{}.", bundle.name))
        .suffix(".partial")
        .tempdir_in(parent)?;
    for (path, bytes) in &bundle.files {
        let file = staging.path().join(path);
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&file, bytes)?;
    }
    commit(staging.path(), &target)?;
    // The staging path no longer exists; dropping finds nothing to remove.
    drop(staging);
    Ok(target)
}

/// Moves `staging` to `target` only if nothing is at `target`.
#[cfg(unix)]
fn commit(staging: &Path, target: &Path) -> std::io::Result<()> {
    // A plain rename may replace an empty directory created after any
    // existence check. Claim the name exclusively first: whoever creates
    // it owns it, and renaming onto one's own empty claim is safe.
    std::fs::create_dir(target)?;
    if let Err(error) = std::fs::rename(staging, target) {
        // Removes the claim only while it is still empty, so files someone
        // else put there are never deleted.
        let _ = std::fs::remove_dir(target);
        return Err(error);
    }
    Ok(())
}

/// `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` fails when anything
/// exists at `target`: no check-then-act window.
#[cfg(windows)]
fn commit(staging: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
    let wide = |p: &Path| -> Vec<u16> { p.as_os_str().encode_wide().chain(Some(0)).collect() };
    let (from, to) = (wide(staging), wide(target));
    // SAFETY: both are NUL-terminated UTF-16 paths alive for the call.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// File count and size, for the confirmation line.
pub(crate) fn summary(bundle: &Bundle) -> String {
    let bytes: usize = bundle.files.iter().map(|(_, b)| b.len()).sum();
    format!("{} files, {} bytes", bundle.files.len(), bytes)
}

#[cfg(test)]
#[path = "evidence_tests.rs"]
mod tests;
