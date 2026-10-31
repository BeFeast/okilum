//! Bounded Manager reads and model changes over the native WebSocket audience.
use super::*;
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ManagerThread {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub status: String,
    pub model_selection: crate::ModelSelection,
    pub runtime_mode: String,
    pub interaction_mode: String,
}

impl NativeClient {
    /// The native environment identity comes from this server, never its local alias.
    pub fn server_identity(&self) -> StreamResult<crate::ServerIdentity> {
        let config = self.manager_rpc("server.getConfig", json!({}), false)?;
        let identity: crate::ServerIdentity = serde_json::from_value(config["environment"].clone())
            .map_err(|_| StreamError::Protocol("missing native environment identity"))?;
        if identity.environment_id.trim().is_empty() || identity.label.trim().is_empty() {
            return Err(StreamError::Protocol("invalid native environment identity"));
        }
        Ok(identity)
    }

    fn manager_rpc(&self, method: &str, payload: Value, snapshot: bool) -> StreamResult<Value> {
        self.token.validate(&self.environment, crate::unix_now())?;
        let (socket, cancel) = self.open_socket()?;
        let mut rpc = ThreadSubscription {
            socket,
            cancel,
            origin: self.environment.origin(),
            thread_id: String::new(),
            cursor: None,
            closed: false,
            access: self.access.clone(),
        };
        rpc.send(json!({"_tag":"Request","id":"1","tag":method,"payload":payload,"headers":[]}))?;
        let deadline = Instant::now() + self.timeout;
        while Instant::now() < deadline {
            let value: Value = match rpc.socket.read().map_err(|_| StreamError::Closed)? {
                Message::Text(text) => serde_json::from_str(&text)
                    .map_err(|_| StreamError::Protocol("invalid Manager response"))?,
                Message::Ping(_) => {
                    rpc.socket.flush().map_err(|_| StreamError::Closed)?;
                    continue;
                }
                Message::Pong(_) => continue,
                _ => return Err(StreamError::Closed),
            };
            match value["_tag"].as_str() {
                Some("Ping") => rpc.send(json!({"_tag":"Pong"}))?,
                Some("Pong") => {}
                Some("Chunk") if value["requestId"] == "1" && snapshot => {
                    let items = value["values"]
                        .as_array()
                        .ok_or(StreamError::Protocol("invalid Manager list"))?;
                    let found = items
                        .iter()
                        .find(|item| item["kind"] == "snapshot")
                        .map(|item| item["snapshot"].clone());
                    rpc.send(json!({"_tag":"Ack","requestId":"1"}))?;
                    if let Some(found) = found {
                        rpc.close();
                        return Ok(found);
                    }
                }
                Some("Exit") if value["requestId"] == "1" => {
                    if value["exit"]["_tag"] != "Success" {
                        return Err(StreamError::Rejected);
                    }
                    return Ok(value["exit"]["value"].clone());
                }
                _ => return Err(StreamError::Protocol("unexpected Manager response")),
            }
        }
        Err(StreamError::Unreachable)
    }

    pub fn project_threads(&self, project: &str) -> StreamResult<Vec<ManagerThread>> {
        let snapshot = self.manager_rpc(
            "orchestration.subscribeShell",
            json!({"requestCompletionMarker":true}),
            true,
        )?;
        let threads = snapshot["threads"]
            .as_array()
            .ok_or(StreamError::Protocol("missing Manager thread list"))?;
        let mut result = Vec::new();
        for thread in threads
            .iter()
            .filter(|t| t["projectId"] == project && t["deletedAt"].is_null())
        {
            result.push(
                serde_json::from_value(thread.clone())
                    .map_err(|_| StreamError::Protocol("invalid Manager thread"))?,
            );
        }
        Ok(result)
    }

    pub fn manager_models(
        &self,
        target: &crate::models::Target,
    ) -> StreamResult<crate::models::Snapshot> {
        let thread = self
            .project_threads(&target.project_id)?
            .into_iter()
            .find(|t| t.id == target.thread_id)
            .ok_or(StreamError::Protocol(
                "thread no longer belongs to this project",
            ))?;
        let config = self.manager_rpc("server.getConfig", json!({}), false)?;
        let providers = config["providers"]
            .as_array()
            .ok_or(StreamError::Protocol("missing model catalog"))?;
        let providers = providers.iter().filter(|p| p["enabled"] != false).map(|p| json!({
            "providerInstanceId":p["instanceId"], "driverKind":p["driver"], "displayName":p["displayName"],
            "models":p["models"].as_array().into_iter().flatten().map(|m|json!({"id":m["slug"],"label":m["name"]})).collect::<Vec<_>>()
        })).collect::<Vec<_>>();
        let catalog = serde_json::from_value(
            json!({"runtimeMode":thread.runtime_mode,"providers":providers}),
        )
        .map_err(|_| StreamError::Protocol("invalid model catalog"))?;
        Ok(crate::models::Snapshot {
            target: target.clone(),
            catalog,
            configuration: crate::ThreadConfiguration {
                thread_id: thread.id,
                model_selection: thread.model_selection,
                runtime_mode: thread.runtime_mode,
                interaction_mode: thread.interaction_mode,
            },
        })
    }

    pub fn configure_manager_model(
        &self,
        target: &crate::models::Target,
        selection: &crate::ModelSelection,
    ) -> StreamResult<crate::models::Snapshot> {
        if !self.can_operate() {
            return Err(StreamError::Rejected);
        }
        let current = self.manager_models(target)?;
        current
            .validate(selection)
            .map_err(|_| StreamError::Rejected)?;
        self.manager_rpc("orchestration.dispatchCommand", json!({"type":"thread.model-selection.set","commandId":uuid::Uuid::new_v4().to_string(),"threadId":target.thread_id,"modelSelection":selection}), false)?;
        let actual = self.manager_models(target)?;
        if actual.configuration.model_selection != *selection {
            return Err(StreamError::Protocol("model change not confirmed"));
        }
        Ok(actual)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{DispatchMode, Intent, Journal};
    use crate::projection::{
        presentation::{self, Body},
        Projection,
    };

    #[test]
    #[ignore = "explicit isolated real-T3 Linux QA only"]
    fn live_isolated_manager_roundtrip() {
        let credential =
            std::env::var("OKILUM_T3_QA_CREDENTIAL_FILE").expect("private native credential file");
        let origin = std::env::var("OKILUM_T3_QA_ORIGIN")
            .unwrap_or_else(|_| "http://127.0.0.1:23110".into());
        let project = std::env::var("OKILUM_T3_QA_PROJECT").expect("isolated project");
        let thread = std::env::var("OKILUM_T3_QA_THREAD").expect("isolated thread");
        let state = std::env::var("OKILUM_T3_QA_STATE").expect("isolated durable state");
        let env = Environment::new("qa2098", &origin).unwrap();
        let token = NativeToken::from_secret(&env, &std::fs::read(credential).unwrap()).unwrap();
        let client = NativeClient::new(env.clone(), token, Duration::from_secs(20)).unwrap();
        let threads = client.project_threads(&project).unwrap();
        let target = threads.iter().find(|t| t.id == thread).unwrap();
        assert!(
            target.title.starts_with("Okilum QA2098"),
            "never operate a work thread"
        );
        let mut directory = shell::Directory::new([project.clone()]);
        let mut listing = client.subscribe_shell().unwrap();
        listing
            .receive(|updates| {
                directory.apply(updates);
                Ok(())
            })
            .unwrap();
        assert!(directory.synchronized);
        assert!(directory.threads.contains_key(&thread));
        assert!(directory
            .threads
            .values()
            .all(|row| row.project_id == project));
        drop(listing);
        let model = client
            .manager_models(&crate::models::Target {
                thread_id: thread.clone(),
                project_id: project.clone(),
            })
            .unwrap();
        assert_eq!(model.configuration.model_selection, target.model_selection);
        let mut stream = client.subscribe(&thread, None).unwrap();
        let cancel = stream.cancellation().unwrap();
        let (finished, stop) = std::sync::mpsc::channel();
        let watchdog = std::thread::spawn(move || {
            if matches!(
                stop.recv_timeout(Duration::from_secs(150)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ) {
                cancel.cancel();
            }
        });
        let mut projection = Projection::new(&thread, &project);
        while !projection.action_state().ready {
            stream.receive(|updates| projection.apply(updates)).unwrap();
        }
        assert!(
            projection.action_state().active_run.is_none(),
            "QA thread must be idle"
        );
        let marker = format!("OKILUM2098_{}", uuid::Uuid::new_v4().simple());
        let journal = Journal::new(std::path::Path::new(&state), &env, &thread);
        let record = journal.prepare(Intent::message(format!("Isolated UI verification. Do not use tools or change any files. Reply with exactly {marker}."),DispatchMode::StartImmediately)).unwrap();
        let receipt = journal
            .send(&record, |command| client.dispatch(command))
            .unwrap();
        assert!(receipt.sequence.is_some());
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut working = false;
        let mut answered = false;
        let mut batches = 0;
        while Instant::now() < deadline && !answered {
            stream.receive(|updates| projection.apply(updates)).unwrap();
            batches += 1;
            working |= projection.action_state().active_run.is_some();
            answered = projection.keys().iter().filter_map(|key|projection.row(key)).any(|row|
                matches!(presentation::body(&row.item),Body::Message {user:false,text} if text.contains(&marker)));
        }
        stream.close();
        let _ = finished.send(());
        watchdog.join().unwrap();
        assert!(working, "real run state must reach the retained projection");
        assert!(
            answered,
            "assistant response must arrive over the original subscription"
        );
        println!("QA2098 thread-list=PASS model=PASS durable-send=PASS working=PASS live-reply=PASS batches={batches}; work-thread mutations=0");
    }
}
