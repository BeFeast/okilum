//! Transient bounded Inbox browsing. Durable authority stays in TaskBinding and
//! the existing application checkpoint; provider reads never own the backend lock.
use super::*;
use crate::todoist::{Task, Todoist};
use std::collections::{BTreeMap, BTreeSet};

const MAX_ROWS: usize = 500;
const MAX_PAGES: usize = 10;
const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct Error {
    pub code: &'static str,
    pub message: &'static str,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for Error {}
fn failure(code: &'static str, message: &'static str) -> anyhow::Error {
    Error { code, message }.into()
}

#[derive(Default)]
pub(super) struct State {
    session: Option<Session>,
    // Reconnect invalidates captured adapters even when settings return to the
    // same value. This is ephemeral and introduces no journal compatibility field.
    epoch: u64,
}
impl State {
    pub(super) fn invalidate(&mut self) {
        self.session = None;
        self.epoch = self.epoch.wrapping_add(1);
    }
}
#[derive(Clone, PartialEq)]
struct Target {
    workspace: Value,
    goal: String,
    task_generation: u64,
    config: Value,
    account: Option<String>,
    saved_config: Option<Value>,
    epoch: u64,
}
struct Session {
    id: String,
    target: Target,
    inbox: Option<String>,
    rows: BTreeMap<String, Task>,
    order: Vec<String>,
    cursor: Option<String>,
    cursors: BTreeSet<String>,
    pages: usize,
    bytes: usize,
    complete: bool,
    limited: bool,
    busy: bool,
}
fn item(task: &Task) -> Value {
    json!({"task_id":task.id,"content":task.content,"description":task.description,"due":task.due,"labels":task.labels,"priority":task.priority,"url":format!("https://app.todoist.com/app/task/{}",task.id)})
}
impl Session {
    fn new(target: Target) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            target,
            inbox: None,
            rows: BTreeMap::new(),
            order: Vec::new(),
            cursor: None,
            cursors: BTreeSet::new(),
            pages: 0,
            bytes: 0,
            complete: false,
            limited: false,
            busy: false,
        }
    }
    fn view(&self) -> Value {
        let items: Vec<Value> = self.order.iter().map(|id| item(&self.rows[id])).collect();
        json!({"schema":"tessera-todoist-inbox/v1","session_id":self.id,"goal_id":self.target.goal,"account_id":self.target.account,"inbox_project_id":self.inbox,"items":items,"complete":self.complete,"can_load_more":!self.complete&&!self.limited,"limit_reached":self.limited})
    }
    fn merge(&mut self, page: crate::todoist::InboxPage) -> Result<()> {
        if page
            .next_cursor
            .as_ref()
            .is_some_and(|cursor| self.cursors.contains(cursor))
        {
            return Err(failure(
                "todoist_pagination_invalid",
                "Todoist repeated a pagination cursor. Refresh the Inbox.",
            ));
        }
        // Validate into temporary storage; a failed page cannot advance cursor,
        // counters, or any previously visible/frozen row.
        let mut added = BTreeMap::new();
        let mut order = Vec::new();
        let mut bytes = self.bytes;
        for task in page.results {
            if self.rows.contains_key(&task.id) || added.contains_key(&task.id) {
                continue;
            }
            // Include duplicated identity in the generated link. Charging only
            // Task would permit an ordinary RPC response larger than its bound.
            bytes += serde_json::to_vec(&task)?
                .len()
                .max(serde_json::to_vec(&item(&task))?.len());
            if bytes > MAX_BYTES || self.rows.len() + added.len() >= MAX_ROWS {
                self.limited = true;
                break;
            }
            order.push(task.id.clone());
            added.insert(task.id.clone(), task);
        }
        for id in &order {
            self.rows.insert(id.clone(), added.remove(id).unwrap());
        }
        self.order.extend(order);
        self.bytes = bytes.min(MAX_BYTES);
        self.pages += 1;
        self.cursor = page.next_cursor;
        if let Some(cursor) = &self.cursor {
            self.cursors.insert(cursor.clone());
        }
        self.complete = self.cursor.is_none() && !self.limited;
        self.limited |= !self.complete && (self.pages >= MAX_PAGES || self.rows.len() >= MAX_ROWS);
        Ok(())
    }
}

fn capture(owner: &mut Backend, goal: &str) -> Result<(Target, Todoist)> {
    let generation = owner.runner.with_goal(goal, |runner| {
        owner.app.guard_target(runner)?;
        Application::task_generation(runner, goal)
    })?;
    let saved = crate::settings::load(owner.runner.operational_root())?;
    let saved_config = saved
        .as_ref()
        .map(|(config, _)| serde_json::to_value(config))
        .transpose()?;
    let config = serde_json::to_value(&owner.app.settings)?;
    if saved_config.as_ref().is_some_and(|saved| saved != &config) {
        return Err(failure("todoist_configuration_changed", "Saved Todoist configuration differs from the active connection. Reconnect before browsing or linking."));
    }
    let account = saved.and_then(|(_, pin)| pin);
    let adapter = owner.app.account_adapter().ok_or_else(|| {
        failure(
            "todoist_unavailable",
            "Todoist is unavailable. Reconnect the saved account.",
        )
    })?;
    Ok((
        Target {
            workspace: owner.runner.workspace_identity(),
            goal: goal.to_owned(),
            task_generation: generation,
            config,
            account,
            saved_config,
            epoch: owner.todoist_picker.epoch,
        },
        adapter,
    ))
}
fn validate(owner: &mut Backend, target: &Target) -> Result<()> {
    let (current, _) = capture(owner, &target.goal)?;
    if current != *target {
        return Err(failure(
            "todoist_selection_stale",
            "Task target or account changed. Refresh before linking.",
        ));
    }
    Ok(())
}
fn selected<'a>(owner: &'a mut Backend, id: &str) -> Result<&'a mut Session> {
    owner
        .todoist_picker
        .session
        .as_mut()
        .filter(|s| s.id == id)
        .ok_or_else(|| {
            failure(
                "todoist_selection_stale",
                "Inbox selection expired. Browse the Inbox again.",
            )
        })
}
fn lock(backend: &Arc<Mutex<Backend>>) -> Result<std::sync::MutexGuard<'_, Backend>> {
    backend
        .lock()
        .map_err(|_| anyhow::anyhow!("backend unavailable"))
}
fn release(backend: &Arc<Mutex<Backend>>, id: &str) {
    if let Ok(mut owner) = backend.lock() {
        if let Some(session) = owner.todoist_picker.session.as_mut().filter(|s| s.id == id) {
            session.busy = false;
        }
    }
}

pub(super) fn list(
    backend: &Arc<Mutex<Backend>>,
    goal: String,
    session_id: Option<String>,
) -> Result<Value> {
    let (target, adapter, id, cursor, inbox) = {
        let mut owner = lock(backend)?;
        let (target, adapter) = capture(&mut owner, &goal)?;
        if target.account.is_none() {
            return Err(failure(
                "todoist_account_unverified",
                "Reconnect Todoist to verify the saved account before browsing.",
            ));
        }
        if session_id.is_none() {
            owner.todoist_picker.session = Some(Session::new(target.clone()));
        }
        let id =
            session_id.unwrap_or_else(|| owner.todoist_picker.session.as_ref().unwrap().id.clone());
        let session = selected(&mut owner, &id)?;
        if session.target != target {
            return Err(failure(
                "todoist_selection_stale",
                "Inbox target changed. Refresh the Inbox.",
            ));
        }
        if session.busy {
            return Err(failure(
                "todoist_picker_busy",
                "An Inbox request is already in progress.",
            ));
        }
        if session.complete || session.limited {
            return Ok(session.view());
        }
        session.busy = true;
        (
            target,
            adapter,
            id,
            session.cursor.clone(),
            session.inbox.clone(),
        )
    };
    let result = (|| {
        let account = adapter.inbox_account()?;
        if Some(&account.id) != target.account.as_ref()
            || inbox
                .as_ref()
                .is_some_and(|id| id != &account.inbox_project_id)
        {
            return Err(failure(
                "todoist_account_changed",
                "Todoist account or Inbox changed. Reconnect the saved account.",
            ));
        }
        let page = adapter.inbox_page(&account.inbox_project_id, cursor.as_deref())?;
        let mut owner = lock(backend)?;
        validate(&mut owner, &target)?;
        let session = selected(&mut owner, &id)?;
        session.merge(page)?;
        session.inbox = Some(account.inbox_project_id);
        Ok(session.view())
    })();
    release(backend, &id);
    result
}

pub(super) fn link(
    backend: &Arc<Mutex<Backend>>,
    goal: String,
    task_id: String,
    session_id: Option<String>,
) -> Result<Value> {
    let (target, adapter, frozen, inbox) = {
        let mut owner = lock(backend)?;
        let (target, adapter) = capture(&mut owner, &goal)?;
        let (frozen, inbox) = if let Some(id) = &session_id {
            let session = selected(&mut owner, id)?;
            if session.target != target {
                return Err(failure(
                    "todoist_selection_stale",
                    "Task target changed. Browse the Inbox again.",
                ));
            }
            if session.busy {
                return Err(failure(
                    "todoist_picker_busy",
                    "An Inbox request is already in progress.",
                ));
            }
            let task = session.rows.get(&task_id).cloned().ok_or_else(|| {
                failure(
                    "todoist_selection_stale",
                    "Task is absent from the observed Inbox. Refresh the Inbox.",
                )
            })?;
            session.busy = true;
            (Some(task), session.inbox.clone())
        } else {
            (None, None)
        };
        (target, adapter, frozen, inbox)
    };
    let result = (|| {
        // Direct callers retain known-ID semantics: no Inbox/open-state requirement.
        // If a saved pin exists, compare it using this exact captured adapter.
        if frozen.is_some() {
            let account = adapter.inbox_account()?;
            if Some(&account.id) != target.account.as_ref()
                || Some(&account.inbox_project_id) != inbox.as_ref()
            {
                return Err(failure(
                    "todoist_account_changed",
                    "Todoist account or Inbox changed. Refresh the Inbox.",
                ));
            }
        } else if let Some(expected) = &target.account {
            if adapter.account_id()? != *expected {
                return Err(failure(
                    "todoist_account_changed",
                    "Todoist account changed. Reconnect the saved account.",
                ));
            }
        }
        let observed = adapter.associate(goal.clone(), task_id)?;
        if let Some(frozen) = &frozen {
            if observed.task != *frozen
                || observed.task.checked
                || observed.task.is_deleted
                || Some(&observed.task.project_id) != inbox.as_ref()
            {
                return Err(failure(
                    "todoist_selection_stale",
                    "Selected task changed or left the Inbox. Refresh before linking.",
                ));
            }
        }
        let mut owner = lock(backend)?;
        validate(&mut owner, &target)?;
        if let Some(id) = &session_id {
            selected(&mut owner, id)?;
        }
        // Consume before checkpoint: a durably accepted pending source projection
        // must never be linked a second time under the same frozen selection.
        owner.todoist_picker.session = None;
        let Backend { runner, app, .. } = &mut *owner;
        runner.with_goal(&goal, |runner| {
            app.task_link_observed(runner, goal.clone(), observed)
        })
    })();
    if let Some(id) = &session_id {
        release(backend, id);
    }
    result
}

#[cfg(test)]
#[path = "todoist_picker_tests.rs"]
mod tests;
