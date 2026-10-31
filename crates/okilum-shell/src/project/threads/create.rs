//! Explicit conversation creation in an already bound T3 project.
use super::*;
use okilum_t3::{models::Target, stream::thread_create::Journal};

impl ProjectView {
    pub(super) fn create_native_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.threads.create_busy
            || !self.active
            || !reader_features::enabled(reader_features::Feature::Agents, cx)
        {
            return;
        }
        let Some((client, _)) = self.native.connection().filter(|(c, _)| c.can_operate()) else {
            return;
        };
        let Some(environment) = self.environment.clone() else {
            return;
        };
        let Some(state) = self.state.clone() else {
            self.threads.create_error =
                Some("Draft storage is unavailable. A new conversation was not created.".into());
            cx.notify();
            return;
        };
        let target = if let Some(id) = &self.threads.selected {
            self.threads.model.as_ref().and_then(|m| {
                m.lock().unwrap().threads.get(id).map(|row| Target {
                    thread_id: row.id.clone(),
                    project_id: row.project_id.clone(),
                })
            })
        } else {
            self.binding.as_ref().and_then(|b| {
                Some(Target {
                    thread_id: b.manager.thread_id.clone()?,
                    project_id: b.manager.project_id.clone()?,
                })
            })
        };
        let Some(target) = target else { return };
        let Some(binding) = self.binding.clone() else {
            return;
        };
        // Only explicitly linked projects may receive a new conversation.
        if binding.manager.project_id.as_ref() != Some(&target.project_id)
            && binding.executor.project_id != target.project_id
        {
            return;
        }
        self.draft_flush(cx);
        self.threads.create_busy = true;
        self.threads.create_error = None;
        let origin = environment.origin();
        let generation = self.generation;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move {
                let journal = Journal::new(&state, &environment, &target.project_id);
                let confirmed = client.manager_models(&target)?;
                let intent = journal.prepare(&confirmed, "New thread", &chrono::Utc::now().to_rfc3339())?;
                let sequence = client.create_thread(&intent)?;
                // Keep the same pending identity until source readback succeeds,
                // even when dispatch was accepted but that read is interrupted.
                let thread = client.manager_thread(&Target {
                    thread_id: intent.thread_id().into(), project_id: intent.project_id().into(),
                })?;
                journal.acknowledge(&intent, sequence)?;
                Ok::<_, anyhow::Error>(shell::Thread {
                    id: thread.id, project_id: thread.project_id, title: thread.title,
                    // Creation is confirmed; live shell supplies runtime status.
                    status: "created".into(), active_run_id: None, latest_run_id: None,
                    archived_at: None, deleted_at: None,
                })
            }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.generation != generation
                    || this.environment.as_ref().map(Environment::origin).as_ref() != Some(&origin)
                    || this.binding.as_ref() != Some(&binding)
                {
                    return;
                }
                this.threads.create_busy = false;
                match result {
                    Ok(row) => {
                        let id = row.id.clone();
                        if let Some(model) = &this.threads.model {
                            let mut model = model.lock().unwrap();
                            if !model.threads.contains_key(&id) { model.apply(&[shell::Update::Thread(row, true)]); }
                        }
                        this.select_native_thread(id, window, cx);
                    }
                    Err(_) => this.threads.create_error = Some("Could not confirm the new conversation. Try New thread again to resume an unconfirmed request with the same identity.".into()),
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }
}
