//! Separate allowlisted connector entrypoint using the existing backend owner.
use super::*;
use crate::connector::{self, Authorized, Request, TrustedConfig};
use crate::inbox::SourceAuthority;
use serde::Deserializer;
use std::io::Read;

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum MobileCommand {
    Capabilities {},
    InboxList {
        limit: Option<usize>,
        cursor: Option<String>,
    },
    InboxGet {
        capture_id: String,
    },
    InboxCapture {
        operation_id: String,
        text: String,
    },
    AttentionList {
        limit: Option<usize>,
        cursor: Option<String>,
    },
    AttentionGet {
        goal_id: String,
        attention_id: String,
        revision: Option<String>,
    },
    AttentionReply {
        operation_id: String,
        goal_id: String,
        attention_id: String,
        expected_revision: String,
        #[serde(deserialize_with = "required_nullable")]
        stage_id: Option<String>,
        text: String,
    },
    AttentionAck {
        operation_id: String,
        goal_id: String,
        attention_id: String,
        expected_revision: String,
        #[serde(deserialize_with = "required_nullable")]
        stage_id: Option<String>,
    },
}
fn required_nullable<'de, D: Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<String>, D::Error> {
    Option::<String>::deserialize(d)
}
fn execute(
    backend: &mut Backend,
    context: &Authorized,
    config: &TrustedConfig,
    command: MobileCommand,
) -> Result<Value> {
    let Backend { runner, app, .. } = backend;
    let authority = SourceAuthority::Connector(context);
    match command {
        MobileCommand::Capabilities {} => Ok(
            json!({"connector_id":config.id(),"policy_fingerprint":config.policy_fingerprint(),"workspace":runner.workspace_identity(),"actor":context.actor(),"channel":"telegram","inbox_read":true,"inbox_capture":runner.inbox_writable(),"attention_read":runner.attention_readable(),"attention_reply":runner.attention_writable(),"attention_ack":runner.attention_writable()}),
        ),
        MobileCommand::InboxList { limit, cursor } => Ok(serde_json::to_value(
            runner.inbox_list(limit, cursor.as_deref())?,
        )?),
        MobileCommand::InboxGet { capture_id } => {
            Ok(serde_json::to_value(runner.inbox_get(&capture_id)?)?)
        }
        MobileCommand::InboxCapture { operation_id, text } => {
            Ok(serde_json::to_value(runner.inbox_capture_authorized(
                crate::inbox::CaptureRequest {
                    operation_id,
                    text,
                    source: context.source()?,
                },
                &authority,
            )?)?)
        }
        MobileCommand::AttentionList { limit, cursor } => {
            let items = app.attention_items(runner)?;
            Ok(serde_json::to_value(runner.attention_list_connector(
                items,
                context,
                limit,
                cursor.as_deref(),
            )?)?)
        }
        MobileCommand::AttentionGet {
            goal_id,
            attention_id,
            revision,
        } => {
            let items = app.attention_items(runner)?;
            runner.attention_get_connector(
                items,
                context,
                &goal_id,
                &attention_id,
                revision.as_deref(),
            )
        }
        MobileCommand::AttentionReply {
            operation_id,
            goal_id,
            attention_id,
            expected_revision,
            stage_id,
            text,
        } => {
            let mutation = crate::attention::Mutation {
                operation_id,
                target: crate::attention::Target {
                    goal_id,
                    attention_id,
                    expected_revision,
                    stage_id,
                },
                source: context.source()?,
            };
            Ok(serde_json::to_value(runner.attention_mutate_authorized(
                mutation,
                "save_decision",
                Some(&text),
                &authority,
                |r| app.attention_items(r),
            )?)?)
        }
        MobileCommand::AttentionAck {
            operation_id,
            goal_id,
            attention_id,
            expected_revision,
            stage_id,
        } => {
            let mutation = crate::attention::Mutation {
                operation_id,
                target: crate::attention::Target {
                    goal_id,
                    attention_id,
                    expected_revision,
                    stage_id,
                },
                source: context.source()?,
            };
            Ok(serde_json::to_value(runner.attention_mutate_authorized(
                mutation,
                "ack_seen",
                None,
                &authority,
                |r| app.attention_items(r),
            )?)?)
        }
    }
}
fn handle(line: &str, backend: &Arc<Mutex<Backend>>, config: &TrustedConfig) -> Value {
    let value = serde_json::from_str::<Value>(line).ok();
    let id = value
        .as_ref()
        .and_then(|v| v.get("id"))
        .filter(|id| id.as_str().is_some_and(|s| s.len() <= 256))
        .cloned()
        .unwrap_or(Value::Null);
    let result = (|| -> Result<Value> {
        let request: Request = serde_json::from_str(line).map_err(|_| {
            connector::error("connector_invalid_request", "Invalid connector request")
        })?;
        if request.schema != connector::SCHEMA
            || request.id.trim().is_empty()
            || request.id.len() > 256
        {
            return Err(connector::error(
                "connector_invalid_request",
                "Invalid connector schema or correlation identity",
            ));
        }
        let mut backend = backend.lock().map_err(|_| {
            connector::error("connector_unavailable", "Backend owner is unavailable")
        })?;
        let context = config.authorize(&request, &backend.runner.workspace_identity())?;
        let name = request
            .command
            .get("op")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !matches!(
            name,
            "capabilities"
                | "inbox_list"
                | "inbox_get"
                | "inbox_capture"
                | "attention_list"
                | "attention_get"
                | "attention_reply"
                | "attention_ack"
        ) {
            return Err(connector::error(
                "connector_operation_forbidden",
                "Operation is outside connector scope",
            ));
        }
        let command = serde_json::from_value(request.command).map_err(|_| {
            connector::error(
                "connector_invalid_request",
                "Invalid connector command fields",
            )
        })?;
        execute(&mut backend, &context, config, command)
    })();
    match result {
        Ok(data) => json!({"schema":connector::SCHEMA,"id":id,"ok":true,"data":data}),
        Err(error) => {
            json!({"schema":connector::SCHEMA,"id":id,"ok":false,"error":error_value(&error)})
        }
    }
}
fn connection(
    mut stream: TcpStream,
    backend: Arc<Mutex<Backend>>,
    config: Arc<TrustedConfig>,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(15)))?;
    let mut input = BufReader::new(stream.try_clone()?);
    loop {
        let mut line = String::new();
        let count = (&mut input).take(1_048_577).read_line(&mut line)?;
        if count == 0 {
            return Ok(());
        }
        if count > 1_048_576 || !line.ends_with('\n') {
            return Err(anyhow::anyhow!(
                "connector frame exceeds limit or is incomplete"
            ));
        }
        let response = handle(&line, &backend, &config);
        writeln!(stream, "{response}")?;
        stream.flush()?;
    }
}
pub(super) fn start(listener: crate::connector::Listener, backend: Arc<Mutex<Backend>>) {
    let config = Arc::new(listener.config);
    std::thread::spawn(move || {
        for stream in listener.socket.incoming() {
            let Ok(stream) = stream else { return };
            let backend = backend.clone();
            let config = config.clone();
            std::thread::spawn(move || {
                let _ = connection(stream, backend, config);
            });
        }
    });
}
#[cfg(test)]
#[path = "connector_service_tests.rs"]
mod tests;
