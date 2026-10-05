//! T3 adapter for a dedicated, single-operation thread. Wire shapes follow T3
//! packages/contracts/{orchestration,environmentHttp}.ts and Effect RpcMessage.
//! Reusable threads need a public message-to-turn mapping; T3 user-message
//! snapshots currently have null turnId. Never bind an arbitrary latest turn.
use crate::types::*;
use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    net::{TcpStream, ToSocketAddrs},
    path::PathBuf,
    time::{Duration, Instant},
};
use tungstenite::{stream::MaybeTlsStream, Message, WebSocket};
use uuid::Uuid;

/// Runtime-only secrets. Intentionally no Debug or Serialize implementation.
pub struct T3Config {
    /// Environment HTTP origin (an existing, already-authorized session).
    pub base_url: String,
    pub bearer_token: String,
    pub environment_id: String,
    pub project_id: String,
    pub model_instance_id: String,
    pub model: String,
    /// Explicit operator choice; never silently elevate permissions.
    pub runtime_mode: String,
    pub interaction_mode: String,
    pub timeout: Duration,
    /// Durable adapter receipts under the runner operational store, never the
    /// brain or rebuildable index. The runner owns this directory lifecycle.
    pub receipt_dir: PathBuf,
}

pub struct T3Adapter {
    config: T3Config,
    http: reqwest::blocking::Client,
    envelopes: BTreeMap<String, StartEnvelope>,
    compatibility_proofs: BTreeMap<String, crate::t3_compat::VerifiedProof>,
}

impl T3Adapter {
    pub fn new(config: T3Config) -> Result<Self> {
        let url = reqwest::Url::parse(&config.base_url).map_err(|_| anyhow!("invalid_t3_url"))?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "invalid_t3_url"
        );
        ensure!(
            !config.bearer_token.trim().is_empty()
                && !config.environment_id.is_empty()
                && !config.project_id.is_empty()
                && !config.model_instance_id.is_empty()
                && !config.model.is_empty()
                && !config.timeout.is_zero(),
            "invalid_t3_config"
        );
        ensure!(
            [
                "approval-required",
                "auto-accept-edits",
                "auto",
                "full-access"
            ]
            .contains(&config.runtime_mode.as_str())
                && ["default", "plan"].contains(&config.interaction_mode.as_str()),
            "invalid_t3_modes"
        );
        ensure!(
            config.receipt_dir.is_absolute(),
            "t3_receipt_directory_must_be_absolute"
        );
        let http = reqwest::blocking::Client::builder()
            .timeout(config.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|_| anyhow!("t3_http_configuration_failed"))?;
        Ok(Self {
            config,
            http,
            envelopes: BTreeMap::new(),
            compatibility_proofs: BTreeMap::new(),
        })
    }

    pub(crate) fn retain_compatibility_proof(&mut self, proof: crate::t3_compat::VerifiedProof) {
        self.compatibility_proofs
            .insert(proof.operation_id.clone(), proof);
    }

    /// Deterministic, domain-separated IDs, allocated from the persisted operation.
    fn id(envelope: &StartEnvelope, purpose: &str) -> Result<String> {
        let operation =
            Uuid::parse_str(&envelope.operation_id).map_err(|_| anyhow!("invalid_operation_id"))?;
        Ok(Uuid::new_v5(&operation, purpose.as_bytes()).to_string())
    }
    pub fn thread_id(envelope: &StartEnvelope) -> Result<String> {
        Self::id(envelope, "t3-thread")
    }

    pub fn thread_url(&self, thread_id: &str) -> Result<String> {
        let mut url =
            reqwest::Url::parse(&self.config.base_url).map_err(|_| anyhow!("invalid_t3_url"))?;
        url.path_segments_mut()
            .map_err(|_| anyhow!("invalid_t3_url"))?
            .pop_if_empty()
            .push(&self.config.environment_id)
            .push(thread_id);
        Ok(url.into())
    }
    fn target<'a>(&self, envelope: &'a StartEnvelope) -> Result<&'a str> {
        ensure!(envelope.schema == SCHEMA, "unsupported_schema");
        ensure!(
            envelope.packet.id == envelope.context_id
                && envelope.packet.goal_id == envelope.goal_id
                && envelope.packet.stage_id == envelope.stage_id,
            "context_identity_mismatch"
        );
        for (key, expected) in [
            ("environment_id", &self.config.environment_id),
            ("project_id", &self.config.project_id),
        ] {
            ensure!(
                envelope.target.get(key).and_then(Value::as_str) == Some(expected.as_str()),
                "t3_target_mismatch"
            );
        }
        // Required in the persisted dispatch envelope so retries/reconciliation
        // never invent a new timestamp or silently change the remote command.
        let created = envelope
            .target
            .get("created_at")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("missing_created_at"))?;
        time::OffsetDateTime::parse(created, &time::format_description::well_known::Rfc3339)
            .map_err(|_| anyhow!("invalid_created_at"))?;
        Ok(created)
    }
    fn prompt(envelope: &StartEnvelope) -> Result<String> {
        // Includes caller-prepared source excerpts in packet.extra when supplied.
        Ok(format!("Execute only the delegated stage below. Return the outcome, source references and concrete evidence. Do not claim unmet criteria passed.\n{}",
            serde_json::to_string(&envelope.packet)?))
    }
    fn command(&self, envelope: &StartEnvelope) -> Result<Value> {
        let created = self.target(envelope)?;
        let selection =
            json!({"instanceId":self.config.model_instance_id,"model":self.config.model});
        Ok(
            json!({"type":"thread.turn.start", "commandId":Self::id(envelope,"t3-command")?,
            "threadId":Self::thread_id(envelope)?,
            "message":{"messageId":Self::id(envelope,"t3-message")?,"role":"user","text":Self::prompt(envelope)?,"attachments":[]},
            "modelSelection":selection,"runtimeMode":self.config.runtime_mode,"interactionMode":self.config.interaction_mode,
            "createdAt":created,"bootstrap":{"createThread":{"projectId":self.config.project_id,
                "title":envelope.packet.goal,"modelSelection":selection,"runtimeMode":self.config.runtime_mode,
                "interactionMode":self.config.interaction_mode,"branch":null,"worktreePath":null,"createdAt":created}}}),
        )
    }
    fn connect(&self) -> Result<RpcSocket> {
        let endpoint = format!(
            "{}/api/auth/websocket-ticket",
            self.config.base_url.trim_end_matches('/')
        );
        let response = self
            .http
            .post(endpoint)
            .bearer_auth(&self.config.bearer_token)
            .send()
            .map_err(|error| {
                if error.is_connect() {
                    anyhow!("t3_ticket_connection_failed")
                } else {
                    anyhow!("t3_ticket_transport_error")
                }
            })?;
        ensure!(
            response.status().is_success(),
            "t3_ticket_http_{}",
            response.status().as_u16()
        );
        let ticket: Value = response
            .json()
            .map_err(|_| anyhow!("t3_ticket_protocol_error"))?;
        let ticket = ticket
            .get("ticket")
            .and_then(Value::as_str)
            .filter(|x| !x.is_empty())
            .ok_or_else(|| anyhow!("t3_missing_ticket"))?;
        let mut url = reqwest::Url::parse(&format!(
            "{}/ws",
            self.config.base_url.trim_end_matches('/')
        ))
        .map_err(|_| anyhow!("invalid_t3_url"))?;
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        url.set_scheme(scheme)
            .map_err(|_| anyhow!("invalid_t3_url"))?;
        url.query_pairs_mut().append_pair("wsTicket", ticket);
        let addresses = (
            url.host_str().ok_or_else(|| anyhow!("invalid_t3_host"))?,
            url.port_or_known_default().unwrap_or(80),
        )
            .to_socket_addrs()
            .map_err(|_| anyhow!("t3_dns_failed"))?;
        let mut stream = None;
        for address in addresses {
            if let Ok(value) = TcpStream::connect_timeout(&address, self.config.timeout) {
                stream = Some(value);
                break;
            }
        }
        let stream = stream.ok_or_else(|| anyhow!("t3_connect_failed"))?;
        stream.set_read_timeout(Some(self.config.timeout))?;
        stream.set_write_timeout(Some(self.config.timeout))?;
        let (socket, _) = tungstenite::client_tls_with_config(url.as_str(), stream, None, None)
            .map_err(|_| anyhow!("t3_websocket_handshake_failed"))?;
        Ok(RpcSocket {
            socket,
            counter: 0,
            deadline: Instant::now() + self.config.timeout,
        })
    }
    fn snapshot(&self, envelope: &StartEnvelope) -> Result<Value> {
        self.target(envelope)?;
        let mut rpc = self.connect()?;
        rpc.snapshot(&Self::thread_id(envelope)?)
    }
    fn binding(&self, envelope: &StartEnvelope, snapshot: &Value) -> Result<EngineRef> {
        self.target(envelope)?;
        // Never make absence claims from a paginated projection.
        ensure!(
            snapshot.get("page").is_none(),
            "t3_windowed_snapshot_unsupported"
        );
        ensure!(
            snapshot
                .get("snapshotSequence")
                .and_then(Value::as_u64)
                .is_some(),
            "t3_missing_sequence"
        );
        let thread = &snapshot["thread"];
        let thread_id = Self::thread_id(envelope)?;
        ensure!(
            thread["id"].as_str() == Some(&thread_id)
                && thread["projectId"].as_str() == Some(&self.config.project_id),
            "t3_thread_mismatch"
        );
        let messages = thread["messages"]
            .as_array()
            .ok_or_else(|| anyhow!("t3_missing_messages"))?;
        let users: Vec<_> = messages.iter().filter(|m| m["role"] == "user").collect();
        ensure!(
            users.len() == 1
                && users[0]["id"].as_str() == Some(&Self::id(envelope, "t3-message")?)
                && users[0]["text"].as_str() == Some(&Self::prompt(envelope)?),
            "t3_user_message_correlation_unknown"
        );
        let latest = &thread["latestTurn"];
        // T3 timestamps the accepted user message on the server, independently
        // of our earlier prepared command.createdAt. ProjectionPipeline copies
        // that exact pending message timestamp into the provider turn. This is
        // safe only after the full-snapshot, single-user ID/text checks above.
        // Reusable threads still require a public message-to-turn receipt.
        let accepted_at = users[0]["createdAt"]
            .as_str()
            .ok_or_else(|| anyhow!("t3_missing_user_timestamp"))?;
        time::OffsetDateTime::parse(accepted_at, &time::format_description::well_known::Rfc3339)
            .map_err(|_| anyhow!("t3_invalid_user_timestamp"))?;
        ensure!(
            latest["requestedAt"].as_str() == Some(accepted_at),
            "t3_turn_correlation_unknown"
        );
        let turn = latest["turnId"]
            .as_str()
            .filter(|x| !x.is_empty())
            .ok_or_else(|| anyhow!("t3_turn_not_assigned"))?;
        Ok(EngineRef {
            engine: "t3".into(),
            instance_id: self.config.environment_id.clone(),
            thread_id: Some(thread_id),
            turn_id: Some(turn.into()),
            task_id: None,
        })
    }
    fn fingerprint(envelope: &StartEnvelope) -> Result<String> {
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(envelope)?)
        ))
    }
    fn receipt_path(&self, envelope: &StartEnvelope) -> Result<PathBuf> {
        let operation =
            Uuid::parse_str(&envelope.operation_id).map_err(|_| anyhow!("invalid_operation_id"))?;
        Ok(self.config.receipt_dir.join(format!("{operation}.json")))
    }
    fn receipt(
        &self,
        envelope: &StartEnvelope,
        binding: Option<&EngineRef>,
    ) -> Result<Option<EngineEvent>> {
        self.target(envelope)?;
        let path = self.receipt_path(envelope)?;
        read_terminal_receipt(
            &path,
            envelope,
            binding,
            &self.config.environment_id,
            self.compatibility_proofs.get(&envelope.operation_id),
        )
    }
    fn freeze(&self, envelope: &StartEnvelope, event: EngineEvent) -> Result<EngineEvent> {
        if let Some(existing) = self.receipt(envelope, Some(&event.engine_ref))? {
            return Ok(existing);
        }
        match fs::create_dir(&self.config.receipt_dir) {
            Ok(()) => {
                let parent = self
                    .config
                    .receipt_dir
                    .parent()
                    .ok_or_else(|| anyhow!("t3_receipt_directory_failed"))?;
                fs::File::open(parent)
                    .and_then(|dir| dir.sync_all())
                    .map_err(|_| anyhow!("t3_receipt_parent_sync_failed"))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => bail!("t3_receipt_directory_failed"),
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&self.config.receipt_dir)
            .map_err(|_| anyhow!("t3_receipt_write_failed"))?;
        let record = TerminalReceipt {
            fingerprint: Self::fingerprint(envelope)?,
            event: event.clone(),
        };
        temporary
            .write_all(&serde_json::to_vec(&record)?)
            .map_err(|_| anyhow!("t3_receipt_write_failed"))?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|_| anyhow!("t3_receipt_sync_failed"))?;
        match temporary.persist_noclobber(self.receipt_path(envelope)?) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                return self
                    .receipt(envelope, Some(&event.engine_ref))?
                    .ok_or_else(|| anyhow!("t3_receipt_disappeared"));
            }
            Err(_) => bail!("t3_receipt_commit_failed"),
        }
        fs::File::open(&self.config.receipt_dir)
            .and_then(|dir| dir.sync_all())
            .map_err(|_| anyhow!("t3_receipt_directory_sync_failed"))?;
        Ok(event)
    }
    fn events(
        &self,
        envelope: &StartEnvelope,
        binding: &EngineRef,
        snapshot: &Value,
    ) -> Result<Vec<EngineEvent>> {
        // A receipt is immutable once exposed to the runner. New snapshot
        // timestamps, edits or later diff availability cannot rewrite its event.
        // Evidence enrichment requires a separate future contract.
        if let Some(receipt) = self.receipt(envelope, Some(binding))? {
            return Ok(vec![receipt]);
        }
        ensure!(
            &self.binding(envelope, snapshot)? == binding,
            "t3_binding_changed"
        );
        let thread = &snapshot["thread"];
        let turn = binding
            .turn_id
            .as_deref()
            .ok_or_else(|| anyhow!("t3_missing_turn"))?;
        let sequence = snapshot["snapshotSequence"]
            .as_u64()
            .ok_or_else(|| anyhow!("t3_missing_sequence"))?;
        let state = thread["latestTurn"]["state"]
            .as_str()
            .ok_or_else(|| anyhow!("t3_missing_state"))?;
        let observed = thread["updatedAt"]
            .as_str()
            .ok_or_else(|| anyhow!("t3_missing_timestamp"))?;
        time::OffsetDateTime::parse(observed, &time::format_description::well_known::Rfc3339)
            .map_err(|_| anyhow!("t3_invalid_timestamp"))?;
        let stream = format!(
            "t3:{}:{}",
            self.config.environment_id,
            binding.thread_id.as_deref().unwrap()
        );
        let make = |event_id: String, payload| EngineEvent {
            operation_id: envelope.operation_id.clone(),
            engine_ref: binding.clone(),
            event_id,
            stream_id: stream.clone(),
            sequence: Some(sequence),
            cursor: Some(sequence.to_string()),
            observed_at: observed.into(),
            payload,
        };
        if state == "running" {
            return Ok(vec![make(
                format!("{turn}:status:{sequence}"),
                EventPayload::Status {
                    state: "running".into(),
                },
            )]);
        }
        ensure!(
            ["completed", "interrupted", "error"].contains(&state),
            "t3_unknown_turn_state"
        );
        let url = self.thread_url(binding.thread_id.as_deref().unwrap())?;
        let checkpoints = thread["checkpoints"]
            .as_array()
            .ok_or_else(|| anyhow!("t3_missing_checkpoints"))?;
        let checkpoint = checkpoints.iter().find(|c| c["turnId"] == turn);
        let mut evidence = Vec::new();
        let messages = thread["messages"]
            .as_array()
            .ok_or_else(|| anyhow!("t3_missing_messages"))?;
        // A completed provider turn can reach the projection before its final
        // message. Wait for the exact declared final assistant message, never
        // freeze a partial or unrelated output as successful.
        if state == "completed" {
            let final_id = thread["latestTurn"]["assistantMessageId"]
                .as_str()
                .filter(|id| !id.is_empty());
            let final_message = final_id.and_then(|id| {
                messages.iter().find(|message| {
                    message["id"].as_str() == Some(id)
                        && message["role"] == "assistant"
                        && message["turnId"] == turn
                })
            });
            if !final_message.is_some_and(|message| {
                message["streaming"] == false
                    && message["text"]
                        .as_str()
                        .is_some_and(|text| !text.trim().is_empty())
            }) {
                return Ok(vec![make(
                    format!("{turn}:awaiting-final-message"),
                    EventPayload::Attention {
                        message: "T3 turn completed; waiting for its correlated final response."
                            .into(),
                    },
                )]);
            }
        }
        let mut summaries = Vec::new();
        for message in messages
            .iter()
            .filter(|m| m["role"] == "assistant" && m["turnId"] == turn)
        {
            ensure!(
                message["streaming"] == false,
                "t3_assistant_still_streaming"
            );
            let text = message["text"]
                .as_str()
                .ok_or_else(|| anyhow!("t3_invalid_message"))?;
            let id = message["id"]
                .as_str()
                .ok_or_else(|| anyhow!("t3_missing_message_id"))?;
            summaries.push(text);
            evidence.push(Evidence {
                id: format!("t3-message:{id}"),
                kind: "engine_message".into(),
                source: SourceRef {
                    uri: url.clone(),
                    revision: None,
                    locator: Some(id.into()),
                },
                description: text.into(),
                observed_at: observed.into(),
                status: "unverified".into(),
            });
        }
        if let Some(checkpoint) = checkpoint {
            let revision = checkpoint["checkpointRef"].as_str().map(str::to_string);
            evidence.push(Evidence {
                id: format!("t3-checkpoint:{turn}"),
                kind: "engine_checkpoint".into(),
                source: SourceRef {
                    uri: url.clone(),
                    revision: revision.clone(),
                    locator: Some(turn.into()),
                },
                description: serde_json::to_string(checkpoint)?,
                observed_at: observed.into(),
                status: "unverified".into(),
            });
            if checkpoint["status"] == "ready" {
                if let Some(count) = checkpoint["checkpointTurnCount"]
                    .as_u64()
                    .filter(|n| *n > 0)
                {
                    let diff = self.connect().and_then(|mut rpc| rpc.call("orchestration.getTurnDiff",json!({"threadId":binding.thread_id,"fromTurnCount":count-1,"toTurnCount":count})));
                    let diff = diff.and_then(|diff| {
                        ensure!(
                            diff["threadId"].as_str() == binding.thread_id.as_deref()
                                && diff["fromTurnCount"].as_u64() == Some(count - 1)
                                && diff["toTurnCount"].as_u64() == Some(count)
                                && diff["diff"].as_str().is_some(),
                            "t3_diff_correlation_mismatch"
                        );
                        Ok(diff)
                    });
                    if let Ok(diff) = diff {
                        evidence.push(Evidence {
                            id: format!("t3-diff:{turn}"),
                            kind: "engine_diff".into(),
                            source: SourceRef {
                                uri: url.clone(),
                                revision,
                                locator: Some(turn.into()),
                            },
                            description: serde_json::to_string(&diff)?,
                            observed_at: observed.into(),
                            status: "unverified".into(),
                        });
                    } else {
                        evidence.push(Evidence {
                            id: format!("t3-diff-unavailable:{turn}"),
                            kind: "engine_diff_unavailable".into(),
                            source: SourceRef {
                                uri: url.clone(),
                                revision,
                                locator: Some(turn.into()),
                            },
                            description:
                                "The checkpoint exists but its diff could not be retrieved.".into(),
                            observed_at: observed.into(),
                            status: "unverified".into(),
                        });
                    }
                }
            }
        }
        if checkpoint.is_none() {
            // Non-git projects never publish checkpoints. A complete assistant
            // outcome remains useful evidence, but does not prove any criterion.
            // Freeze what was observed now; late checkpoint enrichment must be a
            // distinct future event, never a rewrite of this durable receipt.
            evidence.push(Evidence {
                id: format!("t3-checkpoint-unavailable:{turn}"),
                kind: "engine_checkpoint_unavailable".into(),
                source: SourceRef {
                    uri: url,
                    revision: None,
                    locator: Some(turn.into()),
                },
                description: "No checkpoint was available when the terminal response was observed; no file or diff verification is claimed.".into(),
                observed_at: observed.into(),
                status: "unverified".into(),
            });
        }
        let outcome = match state {
            "completed" => "succeeded",
            "interrupted" => "cancelled",
            _ => "failed",
        };
        Ok(vec![self.freeze(
            envelope,
            make(
                format!("{turn}:outcome"),
                EventPayload::Outcome(Outcome {
                    outcome: outcome.into(),
                    summary: if summaries.is_empty() {
                        format!("T3 turn {state}; no assistant summary available.")
                    } else {
                        summaries.join("\n\n")
                    },
                    sources: envelope.packet.sources.clone(),
                    evidence,
                    verification: "unverified".into(),
                    criterion_evaluations: vec![],
                }),
            ),
        )?])
    }
}

#[derive(Serialize, Deserialize)]
struct TerminalReceipt {
    fingerprint: String,
    event: EngineEvent,
}

impl Adapter for T3Adapter {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            engine: "t3".into(),
            cancel: false,
        }
    }
    fn start(&mut self, envelope: &StartEnvelope) -> Result<StartReply> {
        let command = match self.command(envelope) {
            Ok(v) => v,
            Err(e) => {
                return Ok(StartReply::Rejected {
                    reason: e.to_string(),
                })
            }
        };
        self.envelopes
            .insert(Self::thread_id(envelope)?, envelope.clone());
        let mut rpc = match self.connect() {
            Ok(v) => v,
            Err(e) => {
                return Ok(StartReply::Rejected {
                    reason: e.to_string(),
                })
            }
        };
        // Once a command send is attempted, any failure may follow acceptance.
        if !rpc
            .call("orchestration.dispatchCommand", command)
            .is_ok_and(|receipt| receipt["sequence"].as_u64().is_some())
        {
            return Ok(StartReply::Indeterminate {
                reason: "t3_dispatch_unacknowledged; reconcile the same operation".into(),
            });
        }
        match self
            .snapshot(envelope)
            .and_then(|s| self.binding(envelope, &s))
        {
            Ok(binding) => Ok(StartReply::Accepted { binding }),
            Err(_) => Ok(StartReply::Indeterminate {
                reason: "t3_command_accepted_waiting_for_correlated_turn".into(),
            }),
        }
    }
    fn reconcile(
        &mut self,
        envelope: &StartEnvelope,
        expected: Option<&EngineRef>,
    ) -> Result<ReconcileReply> {
        self.envelopes
            .insert(Self::thread_id(envelope)?, envelope.clone());
        // The first terminal receipt already contains provider evidence and
        // exact correlation. Recover it before any network call: T3 may have
        // disappeared after freeze but before the runner persisted the result.
        match self.receipt(envelope, expected) {
            Ok(Some(event)) => return Ok(ReconcileReply::OutcomeAvailable {
                binding: event.engine_ref.clone(), events: vec![event],
                evidence: "Recovered immutable local T3 receipt with matching envelope fingerprint and binding.".into(),
            }),
            Ok(None) => {},
            Err(error) => return Ok(ReconcileReply::Unknown { reason: error.to_string() }),
        }
        let result = self.snapshot(envelope).and_then(|snapshot| {
            let binding = self.binding(envelope,&snapshot)?;
            ensure!(expected.is_none_or(|b| b == &binding),"t3_binding_changed");
            let events = self.events(envelope,&binding,&snapshot)?;
            if events.iter().any(|e| matches!(e.payload,EventPayload::Outcome(_))) {
                Ok(ReconcileReply::OutcomeAvailable {binding,events,evidence:"Full T3 snapshot verifies dedicated thread, original message and requestedAt/actual turn.".into()})
            } else { Ok(ReconcileReply::Running {binding,evidence:"Correlated T3 snapshot; outcome not yet available.".into()}) }
        });
        Ok(result.unwrap_or_else(|e| ReconcileReply::Unknown {
            reason: e.to_string(),
        }))
    }
    fn observe(
        &mut self,
        binding: &EngineRef,
        cursors: &BTreeMap<String, String>,
    ) -> Result<Vec<EngineEvent>> {
        let envelope = self
            .envelopes
            .get(binding.thread_id.as_deref().unwrap_or(""))
            .ok_or_else(|| anyhow!("t3_reconcile_required_before_observe"))?;
        let mut events = match self.receipt(envelope, Some(binding))? {
            Some(event) => vec![event],
            None => {
                let snapshot = self.snapshot(envelope)?;
                self.events(envelope, binding, &snapshot)?
            }
        };
        // Full authoritative snapshots intentionally replace incremental replay;
        // no partially reconstructed projection is trusted after reconnect.
        events.retain(|event| {
            cursors
                .get(&event.stream_id)
                .and_then(|s| s.parse::<u64>().ok())
                .is_none_or(|cursor| event.sequence.is_some_and(|s| s > cursor))
        });
        Ok(events)
    }
}

/// Inspect immutable local evidence. This helper cannot issue network requests.
pub(crate) fn read_terminal_receipt(
    path: &std::path::Path,
    envelope: &StartEnvelope,
    binding: Option<&EngineRef>,
    environment_id: &str,
    compatibility: Option<&crate::t3_compat::VerifiedProof>,
) -> Result<Option<EngineEvent>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => bail!("t3_receipt_read_failed"),
    };
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "t3_receipt_not_regular_file"
    );
    let bytes = fs::read(path).map_err(|_| anyhow!("t3_receipt_read_failed"))?;
    let receipt: TerminalReceipt =
        serde_json::from_slice(&bytes).map_err(|_| anyhow!("t3_receipt_corrupt"))?;
    let fingerprint_matches = if let Some(proof) = compatibility {
        crate::t3_compat::verify_retained(proof, envelope, &receipt.fingerprint).is_ok()
    } else {
        receipt.fingerprint == T3Adapter::fingerprint(envelope)?
    };
    ensure!(
        fingerprint_matches
            && receipt.event.operation_id == envelope.operation_id
            && binding.is_none_or(|expected| &receipt.event.engine_ref == expected)
            && receipt.event.engine_ref.engine == "t3"
            && receipt.event.engine_ref.instance_id == environment_id
            && receipt.event.engine_ref.thread_id.as_deref()
                == Some(&T3Adapter::thread_id(envelope)?)
            && receipt
                .event
                .engine_ref
                .turn_id
                .as_deref()
                .is_some_and(|id| !id.is_empty())
            && matches!(receipt.event.payload, EventPayload::Outcome(_)),
        "t3_receipt_identity_conflict"
    );
    Ok(Some(receipt.event))
}

/// Fetch existing project/model choices without creating a thread or provider turn.
pub fn discover(base_url: &str, token: &str) -> Result<Value> {
    let adapter = T3Adapter::new(T3Config {
        base_url: base_url.into(),
        bearer_token: token.into(),
        environment_id: "discovery".into(),
        project_id: "discovery".into(),
        model_instance_id: "discovery".into(),
        model: "discovery".into(),
        runtime_mode: "approval-required".into(),
        interaction_mode: "default".into(),
        timeout: Duration::from_secs(8),
        receipt_dir: std::env::temp_dir(),
    })?;
    let mut socket = adapter.connect()?;
    let config = socket.call("server.getConfig", json!({}))?;
    let mut socket = adapter.connect()?;
    let shell = socket.subscription_snapshot(
        "orchestration.subscribeShell",
        json!({"requestCompletionMarker":true}),
    )?;
    let projects=shell["projects"].as_array().context("T3 did not return project choices")?.iter()
        .map(|p|json!({"id":p["id"],"name":p["title"].as_str().or(p["name"].as_str()).unwrap_or("Project")})).collect::<Vec<_>>();
    let models=config["providers"].as_array().into_iter().flatten().flat_map(|provider| {
        provider["models"].as_array().into_iter().flatten().map(move |model|json!({"instance_id":provider["instanceId"],"model":model["slug"],"name":model["name"]}))
    }).collect::<Vec<_>>();
    Ok(
        json!({"environment_id":config["environment"]["environmentId"],"projects":projects,"models":models}),
    )
}

struct RpcSocket {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    counter: u64,
    deadline: Instant,
}
impl RpcSocket {
    fn send(&mut self, value: Value) -> Result<()> {
        self.socket
            .send(Message::Text(value.to_string().into()))
            .map_err(|_| anyhow!("t3_socket_write_failed"))
    }
    fn request(&mut self, tag: &str, payload: Value) -> Result<String> {
        self.counter += 1;
        let id = self.counter.to_string();
        self.send(json!({"_tag":"Request","id":id,"tag":tag,"payload":payload,"headers":[]}))?;
        Ok(id)
    }
    fn read(&mut self) -> Result<Value> {
        loop {
            ensure!(Instant::now() < self.deadline, "t3_rpc_timeout");
            let message = self
                .socket
                .read()
                .map_err(|_| anyhow!("t3_socket_read_failed"))?;
            let value: Value = match message {
                Message::Text(text) => {
                    serde_json::from_str(&text).map_err(|_| anyhow!("t3_invalid_rpc_json"))?
                }
                Message::Ping(_) | Message::Pong(_) => continue,
                Message::Close(_) => bail!("t3_socket_closed"),
                _ => bail!("t3_unsupported_rpc_frame"),
            };
            if value["_tag"] == "Ping" {
                self.send(json!({"_tag":"Pong"}))?;
                continue;
            }
            return Ok(value);
        }
    }
    fn call(&mut self, tag: &str, payload: Value) -> Result<Value> {
        let id = self.request(tag, payload)?;
        let value = self.read()?;
        ensure!(
            value["requestId"] == id && value["_tag"] == "Exit",
            "t3_unexpected_rpc_reply"
        );
        ensure!(value["exit"]["_tag"] == "Success", "t3_rpc_rejected");
        Ok(value["exit"]["value"].clone())
    }
    fn snapshot(&mut self, thread: &str) -> Result<Value> {
        self.subscription_snapshot(
            "orchestration.subscribeThread",
            json!({"threadId":thread,"requestCompletionMarker":true}),
        )
    }
    fn subscription_snapshot(&mut self, tag: &str, payload: Value) -> Result<Value> {
        let id = self.request(tag, payload)?;
        let mut snapshot = None;
        loop {
            let value = self.read()?;
            ensure!(value["requestId"] == id, "t3_unexpected_rpc_reply");
            ensure!(value["_tag"] == "Chunk", "t3_snapshot_stream_rejected");
            let items = value["values"]
                .as_array()
                .ok_or_else(|| anyhow!("t3_invalid_chunk"))?;
            for item in items {
                match item["kind"].as_str() {
                    Some("snapshot") => snapshot = Some(item["snapshot"].clone()),
                    Some("synchronized") => {
                        self.send(json!({"_tag":"Ack","requestId":id}))?;
                        self.send(json!({"_tag":"Interrupt","requestId":id,"interruptors":[]}))?;
                        return snapshot.ok_or_else(|| anyhow!("t3_missing_full_snapshot"));
                    }
                    // A full subscription sends snapshot before synchronization;
                    // unexpected intervening events require a new full snapshot,
                    // not an improvised partial projector that may lose changes.
                    _ => bail!("t3_snapshot_changed_during_catchup"),
                }
            }
            self.send(json!({"_tag":"Ack","requestId":id}))?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
        thread,
    };
    const WHEN: &str = "2026-09-05T12:00:00Z";
    const OP: &str = "06000000-0000-4000-8000-000000000001";
    #[derive(Clone)]
    enum Script {
        Dispatch(bool),
        Snapshot(Value),
        Diff(Value),
        RpcFailure,
    }
    fn envelope() -> StartEnvelope {
        StartEnvelope {
            schema: SCHEMA.into(),
            operation_id: OP.into(),
            goal_id: "goal".into(),
            stage_id: "stage".into(),
            context_id: "context".into(),
            context_revision: "sha256:fixture".into(),
            packet: ContextPacket {
                id: "context".into(),
                goal_id: "goal".into(),
                stage_id: "stage".into(),
                goal_revision: "sha256:goal".into(),
                goal: "Explain a change".into(),
                decisions: vec![],
                constraints: vec!["Use the supplied source".into()],
                sources: vec![SourceRef {
                    uri: "brain:notes/change.md".into(),
                    revision: Some("sha256:fixture".into()),
                    locator: None,
                }],
                previous_result_id: None,
                next_step: "Explain with evidence".into(),
                extra: BTreeMap::from([(
                    "source_excerpts".into(),
                    json!([{"uri":"brain:notes/change.md","text":"The selected source bytes."}]),
                )]),
            },
            target: BTreeMap::from([
                ("environment_id".into(), json!("fixture-env")),
                ("project_id".into(), json!("fixture-project")),
                ("created_at".into(), json!(WHEN)),
            ]),
        }
    }
    fn snapshot(state: &str, sequence: u64) -> Value {
        let envelope = envelope();
        // Full public OrchestrationThreadDetailSnapshot, including the actual
        // null user turnId and server-derived latestTurn.requestedAt.
        json!({"snapshotSequence":sequence,"thread":{
            "id":T3Adapter::thread_id(&envelope).unwrap(),"projectId":"fixture-project","title":"Explain a change",
            "modelSelection":{"instanceId":"fixture-provider","model":"fixture-model"},"runtimeMode":"approval-required","interactionMode":"default",
            "branch":null,"worktreePath":null,"createdAt":WHEN,"updatedAt":"2026-09-05T12:01:00Z","deletedAt":null,
            "latestTurn":{"turnId":"provider-turn-1","state":state,"requestedAt":WHEN,"startedAt":WHEN,
                "completedAt":if state=="running" {Value::Null}else{json!("2026-09-05T12:01:00Z")},"assistantMessageId":"assistant-1"},
            "messages":[{"id":T3Adapter::id(&envelope,"t3-message").unwrap(),"role":"user","text":T3Adapter::prompt(&envelope).unwrap(),"turnId":null,"streaming":false,"createdAt":WHEN,"updatedAt":WHEN},
                {"id":"assistant-1","role":"assistant","text":"Explanation without verification claims.","turnId":"provider-turn-1","streaming":false,"createdAt":WHEN,"updatedAt":WHEN}],
            "activities":[],"proposedPlans":[],"checkpoints":if state=="running" {json!([])} else {json!([{"turnId":"provider-turn-1","checkpointTurnCount":1,
                "checkpointRef":"refs/t3/checkpoints/fixture/1","status":"ready","files":[{"path":"example.md","kind":"modified","additions":1,"deletions":0}],
                "assistantMessageId":"assistant-1","completedAt":"2026-09-05T12:01:00Z"}])},"session":null}})
    }
    type Fixture = (T3Adapter, Arc<Mutex<Vec<Value>>>, thread::JoinHandle<()>);
    #[allow(clippy::result_large_err)] // tungstenite prescribes the handshake callback error type.
    fn fixture(scripts: Vec<Script>) -> Fixture {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let captured = seen.clone();
        let handle = thread::spawn(move || {
            for script in scripts {
                // Exact authenticated bearer-ticket request precedes each socket;
                // neither bearer session issuance nor pairing is exercised.
                let (mut http, _) = listener.accept().unwrap();
                http.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut bytes = vec![];
                let mut byte = [0];
                while !bytes.ends_with(b"\r\n\r\n") {
                    http.read_exact(&mut byte).unwrap();
                    bytes.push(byte[0]);
                }
                let headers = String::from_utf8(bytes).unwrap();
                assert!(headers.starts_with("POST /api/auth/websocket-ticket HTTP/1.1"));
                assert!(headers
                    .to_ascii_lowercase()
                    .contains("authorization: bearer fixture-bearer"));
                let body = r#"{"ticket":"fixture-ticket","expiresAt":"2026-09-05T12:02:00Z"}"#;
                write!(http,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
                drop(http);
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut ws = tungstenite::accept_hdr(
                    stream,
                    |request: &tungstenite::handshake::server::Request,
                     response: tungstenite::handshake::server::Response| {
                        assert_eq!(request.uri().to_string(), "/ws?wsTicket=fixture-ticket");
                        assert!(request.headers().get("Authorization").is_none());
                        Ok(response)
                    },
                )
                .unwrap();
                let request: Value =
                    serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
                assert_eq!(request["_tag"], "Request");
                assert_eq!(request["headers"], json!([]));
                captured.lock().unwrap().push(request.clone());
                let id = &request["id"];
                let reply = match script {
                    Script::Dispatch(drop_reply) => {
                        assert_eq!(request["tag"], "orchestration.dispatchCommand");
                        assert_eq!(request["payload"]["type"], "thread.turn.start");
                        assert_eq!(
                            request["payload"]["bootstrap"]["createThread"]["projectId"],
                            "fixture-project"
                        );
                        assert_eq!(request["payload"]["message"]["role"], "user");
                        assert!(request["payload"]["message"]["text"]
                            .as_str()
                            .unwrap()
                            .contains("The selected source bytes."));
                        if drop_reply {
                            continue;
                        }
                        json!({"_tag":"Exit","requestId":id,"exit":{"_tag":"Success","value":{"sequence":10}}})
                    }
                    Script::Snapshot(snapshot) => {
                        assert_eq!(request["tag"], "orchestration.subscribeThread");
                        assert_eq!(request["payload"]["requestCompletionMarker"], true);
                        assert!(request["payload"].get("turnLimit").is_none());
                        assert!(request["payload"].get("afterSequence").is_none());
                        json!({"_tag":"Chunk","requestId":id,"values":[{"kind":"snapshot","snapshot":snapshot},{"kind":"synchronized"}]})
                    }
                    Script::Diff(diff) => {
                        assert_eq!(request["tag"], "orchestration.getTurnDiff");
                        assert_eq!(request["payload"]["fromTurnCount"], 0);
                        assert_eq!(request["payload"]["toTurnCount"], 1);
                        json!({"_tag":"Exit","requestId":id,"exit":{"_tag":"Success","value":diff}})
                    }
                    Script::RpcFailure => {
                        json!({"_tag":"Exit","requestId":id,"exit":{"_tag":"Failure","cause":[{"_tag":"Fail","error":{"_tag":"OrchestrationGetSnapshotError","message":"fixture-bearer"}}]}})
                    }
                };
                ws.send(Message::Text(reply.to_string().into())).unwrap();
                if request["tag"] == "orchestration.subscribeThread" && reply["_tag"] == "Chunk" {
                    let ack: Value =
                        serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
                    assert_eq!(ack, json!({"_tag":"Ack","requestId":id}));
                    let interrupt: Value =
                        serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
                    assert_eq!(
                        interrupt,
                        json!({"_tag":"Interrupt","requestId":id,"interruptors":[]})
                    );
                }
            }
        });
        let adapter = T3Adapter::new(T3Config {
            base_url: format!("http://{address}"),
            bearer_token: "fixture-bearer".into(),
            environment_id: "fixture-env".into(),
            project_id: "fixture-project".into(),
            model_instance_id: "fixture-provider".into(),
            model: "fixture-model".into(),
            runtime_mode: "approval-required".into(),
            interaction_mode: "default".into(),
            timeout: Duration::from_secs(3),
            receipt_dir: std::env::temp_dir()
                .join(format!("tessera-t3-receipts-{}", Uuid::new_v4())),
        })
        .unwrap();
        (adapter, seen, handle)
    }

    #[test]
    fn lost_reply_reconciles_existing_turn_without_a_second_dispatch() {
        let (mut adapter, seen, server) = fixture(vec![
            Script::Dispatch(true),
            Script::Snapshot(snapshot("running", 12)),
            Script::Snapshot(snapshot("running", 12)),
        ]);
        let envelope = envelope();
        assert!(matches!(
            adapter.start(&envelope).unwrap(),
            StartReply::Indeterminate { .. }
        ));
        let reply = adapter.reconcile(&envelope, None).unwrap();
        let ReconcileReply::Running { binding, .. } = reply else {
            panic!("{reply:?}")
        };
        assert_eq!(binding.turn_id.as_deref(), Some("provider-turn-1"));
        let stream = format!("t3:fixture-env:{}", binding.thread_id.as_deref().unwrap());
        assert!(adapter
            .observe(&binding, &BTreeMap::from([(stream, "12".into())]))
            .unwrap()
            .is_empty());
        server.join().unwrap();
        assert_eq!(
            seen.lock()
                .unwrap()
                .iter()
                .filter(|r| r["tag"] == "orchestration.dispatchCommand")
                .count(),
            1
        );
    }

    #[test]
    fn accepted_dispatch_returns_actual_turn_and_terminal_evidence_stays_unverified() {
        let done = snapshot("completed", 20);
        let diff = json!({"threadId":T3Adapter::thread_id(&envelope()).unwrap(),"fromTurnCount":0,"toTurnCount":1,"diff":"diff --git a/example.md b/example.md\n+change"});
        let (mut adapter, _, server) = fixture(vec![
            Script::Dispatch(false),
            Script::Snapshot(snapshot("running", 12)),
            Script::Snapshot(done),
            Script::Diff(diff),
        ]);
        let StartReply::Accepted { binding } = adapter.start(&envelope()).unwrap() else {
            panic!("expected actual turn")
        };
        let first = adapter.observe(&binding, &BTreeMap::new()).unwrap();
        let second = adapter.observe(&binding, &BTreeMap::new()).unwrap();
        assert_eq!(first[0].event_id, second[0].event_id);
        assert_eq!(first[0].engine_ref, binding);
        let EventPayload::Outcome(outcome) = &first[0].payload else {
            panic!("expected outcome")
        };
        assert_eq!(outcome.outcome, "succeeded");
        assert_eq!(outcome.verification, "unverified");
        assert!(outcome.criterion_evaluations.is_empty());
        assert_eq!(outcome.sources, envelope().packet.sources);
        assert_eq!(outcome.evidence.len(), 3);
        assert!(outcome.evidence.iter().all(|e| e.status == "unverified"));
        assert!(adapter
            .thread_url(binding.thread_id.as_deref().unwrap())
            .unwrap()
            .contains("/fixture-env/"));
        server.join().unwrap();
    }

    // Captured from the actual 2026-09-05 T3 public subscribeThread stream.
    // IDs, title and text are sanitized; projection shape and server times are
    // retained. The command was prepared before the user clicked Start.
    fn live_planning_snapshot() -> Value {
        let mut value: Value =
            serde_json::from_str(include_str!("../tests/fixtures/t3-planning-completed.json"))
                .unwrap();
        let env = envelope();
        value["thread"]["id"] = json!(T3Adapter::thread_id(&env).unwrap());
        value["thread"]["messages"][0]["id"] = json!(T3Adapter::id(&env, "t3-message").unwrap());
        value["thread"]["messages"][0]["text"] = json!(T3Adapter::prompt(&env).unwrap());
        value
    }

    #[test]
    fn server_accepted_time_recovers_completed_planning_without_redispatch() {
        let snapshot = live_planning_snapshot();
        let (mut adapter, seen, server) = fixture(vec![Script::Snapshot(snapshot)]);
        let ReconcileReply::OutcomeAvailable {
            binding, events, ..
        } = adapter.reconcile(&envelope(), None).unwrap()
        else {
            panic!("expected exact existing planning outcome");
        };
        assert_eq!(
            binding.turn_id.as_deref(),
            Some("01a0727b-1ae5-7e13-81b3-0436223e40b4")
        );
        let EventPayload::Outcome(outcome) = &events[0].payload else {
            panic!("expected outcome");
        };
        assert_eq!(outcome.outcome, "succeeded");
        assert_eq!(outcome.verification, "unverified");
        assert!(outcome.criterion_evaluations.is_empty());
        assert_eq!(outcome.evidence.len(), 2);
        assert!(outcome.evidence.iter().all(|e| e.status == "unverified"));
        server.join().unwrap();
        assert!(seen
            .lock()
            .unwrap()
            .iter()
            .all(|r| r["tag"] == "orchestration.subscribeThread"));
        // Restart and unavailable server recover the exact first receipt.
        let mut restarted = T3Adapter::new(adapter.config).unwrap();
        let ReconcileReply::OutcomeAvailable {
            events: recovered, ..
        } = restarted.reconcile(&envelope(), None).unwrap()
        else {
            panic!("expected receipt");
        };
        assert_eq!(events, recovered);
    }

    #[test]
    fn completed_without_exact_nonempty_final_assistant_cannot_emit_outcome() {
        for (field, replacement) in [
            ("text", json!("  ")),
            ("streaming", json!(true)),
            ("turnId", json!("unrelated-turn")),
            ("id", json!("unrelated-message")),
        ] {
            let mut value = live_planning_snapshot();
            value["thread"]["messages"][1][field] = replacement;
            let (mut adapter, _, server) = fixture(vec![Script::Snapshot(value)]);
            let reply = adapter.reconcile(&envelope(), None).unwrap();
            assert!(!matches!(reply, ReconcileReply::OutcomeAvailable { .. }));
            assert!(!adapter.receipt_path(&envelope()).unwrap().exists());
            server.join().unwrap();
        }
    }

    #[test]
    fn concurrent_user_turn_windowing_and_timestamp_mismatch_never_bind_latest_turn() {
        let mut concurrent = snapshot("running", 13);
        concurrent["thread"]["messages"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"another-user","role":"user","text":"other task","turnId":null}));
        let mut windowed = snapshot("running", 13);
        windowed["page"] = json!({"hasMore":false,"beforeCursor":null,"snapshotSequence":13});
        let mut wrong = snapshot("running", 13);
        wrong["thread"]["latestTurn"]["requestedAt"] = json!("2026-09-05T12:00:01Z");
        for value in [concurrent, windowed, wrong] {
            let (mut adapter, _, server) = fixture(vec![Script::Snapshot(value)]);
            assert!(matches!(
                adapter.reconcile(&envelope(), None).unwrap(),
                ReconcileReply::Unknown { .. }
            ));
            server.join().unwrap();
        }
    }

    #[test]
    fn absent_thread_protocol_error_and_wrong_binding_never_prove_not_started() {
        let (mut adapter, _, server) = fixture(vec![
            Script::RpcFailure,
            Script::Snapshot(snapshot("running", 12)),
        ]);
        let reply = adapter.reconcile(&envelope(), None).unwrap();
        assert!(matches!(reply, ReconcileReply::Unknown { .. }));
        assert!(!format!("{reply:?}").contains("fixture-bearer"));
        let wrong = EngineRef {
            engine: "t3".into(),
            instance_id: "fixture-env".into(),
            thread_id: Some(T3Adapter::thread_id(&envelope()).unwrap()),
            turn_id: Some("old-turn".into()),
            task_id: None,
        };
        assert!(matches!(
            adapter.reconcile(&envelope(), Some(&wrong)).unwrap(),
            ReconcileReply::Unknown { .. }
        ));
        server.join().unwrap();
    }

    #[test]
    fn completed_without_checkpoint_returns_unverified_output_and_ids_are_stable() {
        let mut done = snapshot("completed", 20);
        done["thread"]["checkpoints"] = json!([]);
        let (mut adapter, _, server) = fixture(vec![
            Script::Dispatch(false),
            Script::Snapshot(snapshot("running", 12)),
            Script::Snapshot(done),
        ]);
        let StartReply::Accepted { binding } = adapter.start(&envelope()).unwrap() else {
            panic!("expected accepted")
        };
        let events = adapter.observe(&binding, &BTreeMap::new()).unwrap();
        let EventPayload::Outcome(outcome) = &events[0].payload else {
            panic!("expected planning output without a git checkpoint");
        };
        assert_eq!(outcome.verification, "unverified");
        assert!(outcome.criterion_evaluations.is_empty());
        assert!(outcome
            .evidence
            .iter()
            .any(|e| e.kind == "engine_checkpoint_unavailable"));
        let later = snapshot("completed", 25);
        assert_eq!(
            events,
            adapter.events(&envelope(), &binding, &later).unwrap()
        );
        let command = adapter.command(&envelope()).unwrap();
        assert_eq!(command, adapter.command(&envelope()).unwrap());
        assert_ne!(command["commandId"], command["message"]["messageId"]);
        server.join().unwrap();
    }
    #[test]
    fn mismatched_diff_is_retained_as_unavailable_not_attached_evidence() {
        let wrong =
            json!({"threadId":"other-thread","fromTurnCount":0,"toTurnCount":1,"diff":"unrelated"});
        let (mut adapter, _, server) = fixture(vec![
            Script::Snapshot(snapshot("completed", 20)),
            Script::Diff(wrong),
        ]);
        let ReconcileReply::OutcomeAvailable { events, .. } =
            adapter.reconcile(&envelope(), None).unwrap()
        else {
            panic!("expected outcome");
        };
        let EventPayload::Outcome(outcome) = &events[0].payload else {
            panic!("expected outcome");
        };
        assert!(outcome
            .evidence
            .iter()
            .any(|e| e.kind == "engine_diff_unavailable"));
        assert!(!outcome
            .evidence
            .iter()
            .any(|e| e.kind == "engine_diff" || e.description.contains("unrelated")));
        server.join().unwrap();
    }
    #[test]
    fn terminal_receipt_survives_newer_snapshots_restart_and_later_diff_availability() {
        let done = snapshot("completed", 20);
        let mut later = snapshot("completed", 30);
        later["thread"]["updatedAt"] = json!("2026-09-05T13:00:00Z");
        // The first diff request fails; the restarted client must not issue
        // another diff request and silently enrich the already emitted event.
        let (mut adapter, seen, server) = fixture(vec![Script::Snapshot(done), Script::RpcFailure]);
        let ReconcileReply::OutcomeAvailable {
            events: first,
            binding,
            ..
        } = adapter.reconcile(&envelope(), None).unwrap()
        else {
            panic!("expected first outcome");
        };
        server.join().unwrap(); // No T3 listener remains for recovery.
        let receipt_path = adapter.receipt_path(&envelope()).unwrap();
        let original = fs::read(&receipt_path).unwrap();
        let mut restarted = T3Adapter::new(adapter.config).unwrap();
        let ReconcileReply::OutcomeAvailable { events: second, .. } =
            restarted.reconcile(&envelope(), Some(&binding)).unwrap()
        else {
            panic!("expected replay");
        };
        assert_eq!(first, second);
        assert_eq!(
            first,
            restarted.events(&envelope(), &binding, &later).unwrap()
        );
        assert_eq!(original, fs::read(&receipt_path).unwrap());
        let mut changed = envelope();
        changed
            .packet
            .constraints
            .push("different instruction".into());
        assert!(restarted
            .receipt(&changed, Some(&binding))
            .unwrap_err()
            .to_string()
            .contains("identity_conflict"));
        fs::write(&receipt_path, b"corrupt").unwrap();
        assert!(restarted
            .receipt(&envelope(), Some(&binding))
            .unwrap_err()
            .to_string()
            .contains("corrupt"));
        assert_eq!(
            seen.lock()
                .unwrap()
                .iter()
                .filter(|r| r["tag"] == "orchestration.getTurnDiff")
                .count(),
            1
        );
        fs::remove_dir_all(&restarted.config.receipt_dir).unwrap();
    }
}

#[cfg(test)]
mod discovery_tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    #[test]
    fn settings_discovery_reads_existing_models_and_projects_without_dispatch() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            for index in 0..2 {
                let (mut http, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(http.try_clone().unwrap());
                let mut headers = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    headers.push_str(&line);
                }
                assert!(headers.starts_with("POST /api/auth/websocket-ticket "));
                assert!(headers.contains("fixture-discovery-token"));
                let body = r#"{"ticket":"fixture-ticket"}"#;
                write!(http,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
                let (stream, _) = listener.accept().unwrap();
                let mut ws = tungstenite::accept(stream).unwrap();
                let request: Value =
                    serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
                if index == 0 {
                    assert_eq!(request["tag"], "server.getConfig");
                    ws.send(Message::Text(json!({"_tag":"Exit","requestId":request["id"],"exit":{"_tag":"Success","value":{"environment":{"environmentId":"environment-a"},"providers":[{"instanceId":"provider-a","models":[{"slug":"model-a","name":"Model A"}]}]}}}).to_string().into())).unwrap();
                } else {
                    assert_eq!(request["tag"], "orchestration.subscribeShell");
                    ws.send(Message::Text(json!({"_tag":"Chunk","requestId":request["id"],"values":[{"kind":"snapshot","snapshot":{"projects":[{"id":"project-a","title":"Project A"}]}},{"kind":"synchronized"}]}).to_string().into())).unwrap();
                    for expected in ["Ack", "Interrupt"] {
                        let reply: Value =
                            serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
                        assert_eq!(reply["_tag"], expected);
                    }
                }
            }
        });
        let choices = discover(&base, "fixture-discovery-token").unwrap();
        assert_eq!(choices["environment_id"], "environment-a");
        assert_eq!(choices["projects"][0]["id"], "project-a");
        assert_eq!(choices["models"][0]["instance_id"], "provider-a");
        assert_eq!(choices["models"][0]["model"], "model-a");
        worker.join().unwrap();
    }
}
