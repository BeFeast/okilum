//! Existing list snapshots and already cached messages only: no requests or timer.
use okilum_t3::{board::Scope, chat::Conversation, ThreadSummary};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum State {
    Working,
    Waiting,
    Idle,
    Unavailable,
}
impl State {
    fn read(status: &str) -> Self {
        match status {
            "preparing" | "starting" | "running" => Self::Working,
            "queued" | "waiting" => Self::Waiting,
            "idle" | "completed" | "interrupted" | "failed" | "cancelled" | "rolled_back" => {
                Self::Idle
            }
            _ => Self::Unavailable,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Idle => "idle",
            Self::Unavailable => "status unavailable",
        }
    }
}
pub(super) struct Agent<'a> {
    pub thread: &'a ThreadSummary,
    state: State,
    age: Option<u64>,
    pub stuck: bool,
}
impl Agent<'_> {
    pub fn working(&self) -> bool {
        self.state == State::Working
    }
    pub fn label(&self) -> String {
        let mut label = self.state.label().to_owned();
        if self.thread.status == "failed" {
            label.push_str(" · failed");
        }
        if let Some(age) = self.age {
            let elapsed = if age < 60 {
                format!("{age}s")
            } else if age < 3600 {
                format!("{}m", age / 60)
            } else {
                format!("{}h {}m", age / 3600, age % 3600 / 60)
            };
            label.push_str(&format!(" · updated {elapsed} ago"));
        }
        label
    }
}
pub(super) fn thread_id(scope: &Scope) -> Option<&str> {
    scope.agent_thread_id.as_deref()
}
pub(super) fn agent<'a>(
    scope: &Scope,
    threads: &'a [ThreadSummary],
    at: u64,
    threshold: u64,
) -> Option<Agent<'a>> {
    let id = thread_id(scope)?;
    let mut matches = threads.iter().filter(|t| t.thread_id == id);
    let thread = matches.next()?;
    if matches.next().is_some() {
        return None;
    } // Ambiguous sources never choose a winner.
    let state = State::read(&thread.status);
    let age = chrono::DateTime::parse_from_rfc3339(&thread.updated_at)
        .ok()
        .and_then(|date| u64::try_from(date.timestamp()).ok())
        .and_then(|updated| at.checked_sub(updated));
    let stuck =
        matches!(state, State::Working | State::Waiting) && age.is_some_and(|age| age >= threshold);
    Some(Agent {
        thread,
        state,
        age,
        stuck,
    })
}

/// Progress is issue-specific only with exact thread/run evidence already cached
/// by the Manager. A summary title or a different run is never progress text.
pub(super) fn progress(
    scope: &Scope,
    thread: &ThreadSummary,
    cached: Option<&Conversation>,
) -> Option<String> {
    if thread_id(scope)? != thread.thread_id {
        return None;
    }
    let run = scope.execution.as_ref()?;
    if run.thread_id != thread.thread_id
        || thread.latest_run_id.as_deref() != Some(run.run_id.as_str())
    {
        return None;
    }
    let cached = cached.filter(|c| c.thread_id == thread.thread_id)?;
    let text = cached
        .items
        .values()
        .rev()
        .find(|item| {
            item.kind == "assistant_message"
                && item.run_id.as_deref() == Some(run.run_id.as_str())
                && item.text.as_ref().is_some_and(|t| !t.trim().is_empty())
        })?
        .text
        .as_ref()?;
    Some(
        text.lines()
            .find(|line| !line.trim().is_empty())?
            .trim()
            .chars()
            .take(160)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use okilum_t3::board::Binding;
    fn fixture() -> (Scope, ThreadSummary) {
        let scope = Binding::parse(include_str!(
            "../../../../../docs/examples/okilum-board.yaml"
        ))
        .unwrap()
        .issues
        .remove(0);
        let thread = serde_json::from_value(serde_json::json!({
            "threadId":"agent-a","title":"UI","status":"running","latestRunId":"run-a",
            "providerInstanceId":"codex","model":"model","runtimeMode":"full-access","updatedAt":"1970-01-01T00:01:40Z"
        })).unwrap();
        (scope, thread)
    }
    #[test]
    fn live_state_requires_one_literal_thread_not_an_executor_name() {
        let (mut scope, thread) = fixture();
        scope.executor = "UI".into();
        assert_eq!(State::read("queued"), State::Waiting);
        assert!(agent(&scope, std::slice::from_ref(&thread), 200, 1800).is_none());
        scope.execution = Some(okilum_t3::board::Execution {
            thread_id: "agent-a".into(),
            run_id: "run-a".into(),
        });
        assert!(
            agent(&scope, std::slice::from_ref(&thread), 200, 1800).is_none(),
            "execution is not an agent binding"
        );
        scope.agent_thread_id = Some("agent-a".into());
        assert_eq!(
            agent(&scope, std::slice::from_ref(&thread), 200, 1800)
                .unwrap()
                .state,
            State::Working
        );
        assert!(agent(&scope, &[thread.clone(), thread], 200, 1800).is_none());
    }
    #[test]
    fn stuck_uses_last_activity_age_only_for_working_or_waiting() {
        let (mut scope, mut thread) = fixture();
        scope.agent_thread_id = Some("agent-a".into());
        assert!(
            !agent(&scope, std::slice::from_ref(&thread), 1899, 1800)
                .unwrap()
                .stuck
        );
        let stale = agent(&scope, std::slice::from_ref(&thread), 1900, 1800).unwrap();
        assert!(stale.stuck);
        assert!(stale.label().contains("updated 30m ago"));
        thread.status = "waiting".into();
        assert!(
            agent(&scope, std::slice::from_ref(&thread), 1900, 1800)
                .unwrap()
                .stuck
        );
        thread.status = "completed".into();
        assert!(
            !agent(&scope, std::slice::from_ref(&thread), 10000, 1800)
                .unwrap()
                .stuck
        );
    }
    #[test]
    fn invalid_future_and_unknown_status_are_not_fabricated_activity() {
        let (mut scope, mut thread) = fixture();
        scope.agent_thread_id = Some("agent-a".into());
        assert!(agent(&scope, std::slice::from_ref(&thread), 99, 1)
            .unwrap()
            .age
            .is_none());
        thread.updated_at = "invalid".into();
        thread.status = "new-server-state".into();
        let value = agent(&scope, std::slice::from_ref(&thread), 1000, 1).unwrap();
        assert_eq!(value.state, State::Unavailable);
        assert!(!value.stuck);
        assert!(value.age.is_none());
    }
    #[test]
    fn progress_reuses_only_the_matching_already_cached_run() {
        let (mut scope, thread) = fixture();
        let mut cached = Conversation::new("agent-a");
        cached.items.insert(1, serde_json::from_value(serde_json::json!({
            "position":1,"itemId":"i","runId":"run-a","type":"assistant_message","status":"completed",
            "text":"Writing the regression\nLong details", "updatedAt":"now"
        })).unwrap());
        assert!(progress(&scope, &thread, Some(&cached)).is_none());
        scope.execution = Some(okilum_t3::board::Execution {
            thread_id: "agent-a".into(),
            run_id: "run-a".into(),
        });
        assert!(
            progress(&scope, &thread, Some(&cached)).is_none(),
            "missing agent ID is silent even with execution"
        );
        scope.agent_thread_id = Some("agent-a".into());
        assert_eq!(
            progress(&scope, &thread, Some(&cached)).as_deref(),
            Some("Writing the regression")
        );
        scope.execution.as_mut().unwrap().run_id = "other-run".into();
        assert!(progress(&scope, &thread, Some(&cached)).is_none());
    }
}
