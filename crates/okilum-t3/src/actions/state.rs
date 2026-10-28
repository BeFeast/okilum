//! Command availability derives only from the bound live projection. Inherited
//! history, dismissed/resolved requests and unknown capabilities never grant controls.
use super::*;
use crate::projection::Projection;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub ready: bool,
    pub active_run: Option<String>,
    pub can_steer: bool,
    pub requests: Vec<Request>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub id: String,
    pub item: Value,
    pub answerable: bool,
    pub dismissible: bool,
}
impl Projection {
    pub fn action_state(&self) -> State {
        let active = self
            .runs
            .values()
            .filter(|r| {
                matches!(
                    r["status"].as_str(),
                    Some("preparing" | "starting" | "running" | "waiting")
                )
            })
            .max_by_key(|r| r["ordinal"].as_u64().unwrap_or(0));
        let session = active
            .and_then(|run| run["providerThreadId"].as_str())
            .and_then(|id| self.provider_threads.get(id))
            .and_then(|thread| thread["providerSessionId"].as_str())
            .and_then(|id| self.provider_sessions.get(id));
        let mut requests: Vec<_> = self
            .requests
            .values()
            .filter(|r| r["status"] == "pending")
            .filter_map(|r| {
                let id = r["id"].as_str()?;
                let item = self.items.values().find(|item| {
                    item["requestId"] == id
                        && matches!(
                            item["type"].as_str(),
                            Some("approval_request" | "user_input_request")
                        )
                })?;
                let mode = r["responseCapability"]["type"].as_str();
                Some(Request {
                    id: id.into(),
                    item: item.as_ref().clone(),
                    answerable: matches!(mode, Some("live" | "message")),
                    dismissible: item["type"] == "user_input_request" && mode == Some("message"),
                })
            })
            .collect();
        requests.sort_by(|a, b| {
            (a.item["ordinal"].as_u64(), &a.id).cmp(&(b.item["ordinal"].as_u64(), &b.id))
        });
        State {
            ready: self.actions_ready && !self.metadata.deleted,
            active_run: active.and_then(|r| r["id"].as_str()).map(str::to_owned),
            can_steer: session
                .is_some_and(|s| s["capabilities"]["turns"]["supportsActiveSteering"] == true),
            requests,
        }
    }
}
impl State {
    pub fn allows(&self, intent: &Intent) -> bool {
        if !self.ready || !intent.validate() {
            return false;
        }
        match intent {
            Intent::Message { mode, .. } => match mode {
                DispatchMode::StartImmediately => self.active_run.is_none(),
                DispatchMode::QueueAfterActive => true,
                DispatchMode::SteerActive { target_run_id } => {
                    self.can_steer && self.active_run.as_ref() == Some(target_run_id)
                }
            },
            Intent::Interrupt { run_id } => self.active_run.as_ref() == Some(run_id),
            Intent::Dismiss { request_id } => self
                .requests
                .iter()
                .any(|r| &r.id == request_id && r.dismissible),
            Intent::Respond {
                request_id,
                decision,
                answers,
            } => self.requests.iter().any(|r| {
                if &r.id != request_id || !r.answerable {
                    return false;
                }
                if r.item["type"] == "approval_request" {
                    let Some(d) = decision else {
                        return false;
                    };
                    if answers.is_some() {
                        return false;
                    }
                    match r.item["options"].as_array() {
                        Some(options) => options.iter().any(|o| o["decision"] == *d),
                        None => matches!(
                            d.as_str(),
                            "accept" | "acceptForSession" | "decline" | "cancel"
                        ),
                    }
                } else {
                    decision.is_none()
                        && answers.as_ref().is_some_and(|a| valid_answers(&r.item, a))
                }
            }),
        }
    }
}
pub fn valid_answers(item: &Value, answers: &BTreeMap<String, Value>) -> bool {
    let Some(questions) = item["questions"].as_array().filter(|q| !q.is_empty()) else {
        return false;
    };
    if answers
        .keys()
        .any(|id| !questions.iter().any(|q| q["id"] == *id))
    {
        return false;
    }
    questions.iter().all(|q| {
        let Some(id) = q["id"].as_str() else {
            return false;
        };
        let Some(answer) = answers.get(id) else {
            return q["required"] == false;
        };
        let option = |value: &str| {
            q["options"].as_array().is_some_and(|options| {
                options
                    .iter()
                    .any(|o| o["value"].as_str().or(o["label"].as_str()) == Some(value))
            })
        };
        match answer {
            Value::String(s) => {
                !s.trim().is_empty() && (q["allowCustomAnswer"] != false || option(s))
            }
            Value::Array(values) if q["multiSelect"] == true => {
                !values.is_empty() && values.iter().all(|v| v.as_str().is_some_and(option))
            }
            _ => false,
        }
    })
}
