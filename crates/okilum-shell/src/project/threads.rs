//! Configured Manager workspace. Other conversations are opened in T3 (#2203).
use super::*;
use okilum_t3::stream::{shell, CancelSubscription, StreamError};
use std::sync::Mutex;
#[cfg(feature = "project-ui-harness")]
pub(in crate::project) mod native_fixture;

#[derive(Default)]
pub(super) struct Directory {
    pub enabled: bool,
    pub(super) draft: super::drafts::State,
    pub(super) link_notice: Option<String>,
    pub model: Option<Arc<Mutex<shell::Directory>>>,
    worker: Option<Task<()>>,
    cancel: Option<CancelSubscription>,
    epoch: u64,
    error: Option<String>,
}

impl Directory {
    fn pause(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        self.worker.take();
    }
    pub(super) fn reset(&mut self, cx: &mut App) {
        self.pause();
        let (enabled, epoch) = (self.enabled, self.epoch);
        let draft = std::mem::take(&mut self.draft);
        *self = Self::default();
        self.enabled = enabled;
        self.epoch = epoch;
        self.draft = draft;
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        self.pause();
    }
}
impl ProjectView {
    pub(super) fn set_threads_active(
        &mut self,
        active: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !active {
            self.threads.pause();
        }
        if active {
            self.start_thread_directory(window, cx);
        }
    }
    pub(super) fn start_thread_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.threads.enabled
            || !self.active
            || self.threads.worker.is_some()
            || !reader_features::enabled(reader_features::Feature::Agents, cx)
        {
            return;
        }
        let Some((client, _)) = self.native.connection() else {
            return;
        };
        if !client.access_allowed() {
            return;
        }
        let Some(binding) = &self.binding else {
            return;
        };
        let projects = binding
            .manager
            .project_id
            .iter()
            .cloned()
            .chain(std::iter::once(binding.executor.project_id.clone()));
        let model = self
            .threads
            .model
            .get_or_insert_with(|| Arc::new(Mutex::new(shell::Directory::new(projects))))
            .clone();
        self.threads.epoch = self.threads.epoch.wrapping_add(1);
        let epoch = self.threads.epoch;
        self.threads.worker = Some(cx.spawn_in(window, async move |this, cx| {
            let mut socket: Option<shell::Subscription> = None;
            let mut failures = 0u32;
            loop {
                let active = this
                    .update(cx, |this, _| this.active && this.threads.epoch == epoch)
                    .unwrap_or(false);
                if !active {
                    return;
                }
                if socket.is_none() {
                    let connection = client.clone();
                    match cx
                        .background_executor()
                        .spawn(async move { connection.subscribe_shell() })
                        .await
                    {
                        Ok(stream) => {
                            let cancel = stream.cancellation().ok();
                            let accepted = this
                                .update(cx, |this, _| {
                                    if !this.active || this.threads.epoch != epoch {
                                        return false;
                                    }
                                    this.threads.cancel = cancel;
                                    true
                                })
                                .unwrap_or(false);
                            if !accepted {
                                return;
                            }
                            socket = Some(stream);
                        }
                        Err(error) => {
                            let terminal = matches!(
                                error,
                                StreamError::Expired
                                    | StreamError::PairingRequired
                                    | StreamError::WrongOrigin
                                    | StreamError::MissingReadPermission
                                    | StreamError::Closed
                            );
                            let message = error.to_string();
                            let accepted = this
                                .update(cx, |this, cx| {
                                    if this.threads.epoch != epoch {
                                        return false;
                                    }
                                    if this.threads.error.as_ref() != Some(&message) {
                                        this.threads.error = Some(message);
                                        cx.notify();
                                    }
                                    true
                                })
                                .unwrap_or(false);
                            if !accepted || terminal {
                                return;
                            }
                            failures = failures.saturating_add(1);
                            cx.background_executor()
                                .timer(Duration::from_secs(u64::from(failures.min(6)) * 2))
                                .await;
                            continue;
                        }
                    }
                }
                let mut stream = socket.take().unwrap();
                let directory = model.clone();
                let (stream, result, changed) = cx
                    .background_executor()
                    .spawn(async move {
                        let mut changed = false;
                        let result = stream.receive(|updates| {
                            changed = directory.lock().unwrap().apply(updates);
                            Ok(())
                        });
                        (stream, result, changed)
                    })
                    .await;
                let terminal_error = matches!(
                    &result,
                    Err(StreamError::Expired
                        | StreamError::PairingRequired
                        | StreamError::WrongOrigin
                        | StreamError::MissingReadPermission
                        | StreamError::Closed)
                );
                let message = result.as_ref().err().map(ToString::to_string);
                let accepted = this
                    .update(cx, |this, cx| {
                        if !this.active || this.threads.epoch != epoch {
                            return false;
                        }
                        if changed || this.threads.error != message {
                            this.threads.error = message;
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !accepted || terminal_error {
                    return;
                }
                if result.is_ok() {
                    failures = 0;
                    socket = Some(stream);
                } else {
                    drop(stream);
                    failures = failures.saturating_add(1);
                    cx.background_executor()
                        .timer(Duration::from_secs(u64::from(failures.min(6)) * 2))
                        .await;
                }
            }
        }));
    }

    pub(super) fn select_manager_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.native_start(window, cx);
        cx.notify();
    }

    pub(super) fn render_manager_notices(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .w_full()
            .min_w_0()
            .gap_1()
            .px_3()
            .debug_selector(|| "manager-workspace".into())
            .when_some(self.draft_notice(), |view, error| {
                view.child(
                    div()
                        .text_color(cx.theme().danger)
                        .whitespace_normal()
                        .child(error.to_owned()),
                )
            })
            .when_some(self.threads.link_notice.as_ref(), |view, notice| {
                view.child(
                    div()
                        .text_color(brand::palette(cx).text_muted)
                        .whitespace_normal()
                        .child(notice.clone()),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use okilum_t3::stream::{NativeClient, NativeToken};
    use serde_json::json;

    fn row(id: &str, project_id: &str) -> shell::Thread {
        shell::Thread {
            id: id.into(),
            project_id: project_id.into(),
            title: id.into(),
            status: "idle".into(),
            active_run_id: None,
            latest_run_id: None,
            archived_at: None,
            deleted_at: None,
        }
    }

    #[gpui::test]
    fn configured_manager_has_no_other_thread_navigation_and_keeps_its_draft(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::test_init);
        let state = tempfile::tempdir().unwrap();
        let environment = Environment::new("fixture", "http://127.0.0.1:1").unwrap();
        let expiry = chrono::Utc::now().timestamp() as u64 + 3600;
        let token = NativeToken::from_secret(
            &environment,
            &serde_json::to_vec(&json!({
                "origin":environment.origin(), "access_token":"fixture-not-a-secret",
                "expires_at":expiry, "scope":"orchestration:read orchestration:operate"
            }))
            .unwrap(),
        )
        .unwrap();
        // Transport tests separately prove enabled real socket traffic and its
        // revocation control. This UI fixture cannot open a real connection.
        let client = Arc::new(
            NativeClient::new(environment.clone(), token, Duration::from_secs(1))
                .unwrap()
                .with_access(okilum_t3::NetworkAccess::new(false)),
        );
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view = ProjectView::empty(Some(state.path().to_owned()), window, cx);
                view.binding = AreaBinding::parse("t3:\n  schema_version: 1\n  environment: fixture\n  manager: {project_id: managers, thread_id: 'mcp:manager', workspace_mode: local}\n  executor: {project_id: executors}\n  default_models:\n    manager: {instanceId: codex, model: gpt-6.1-sol}\n    executor: {instanceId: codex, model: gpt-6.1-sol}\n").unwrap();
                view.chat = Some(Conversation::new("mcp:manager"));
                view.environment = Some(environment.clone());
                view.reason = None;
                view.threads.enabled = true;
                view.active = false;
                view.install_native(client, expiry, window, cx);
                let mut directory = shell::Directory::new(["managers".into(), "executors".into()]);
                directory.apply(&[shell::Update::Snapshot(vec![
                    row("mcp:manager", "managers"), row("executor-a", "executors"),
                    row("executor-b", "executors"), row("foreign", "unbound"),
                ])]);
                view.threads.model = Some(Arc::new(Mutex::new(directory)));
                view.active = true;
                view
            });
            entity = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = entity.unwrap();
        visual.run_until_parked();
        assert!(visual.debug_bounds("manager-workspace").is_some());
        for selector in [
            "manager-thread-picker",
            "manager-new-thread",
            "manager-return",
            "manager-thread-list",
        ] {
            assert!(
                visual.debug_bounds(selector).is_none(),
                "{selector} is not in the Manager workflow"
            );
        }
        assert!(visual.debug_bounds("project-manager-title").is_some());
        view.update_in(visual, |view, window, cx| {
            assert!(view.threads.enabled, "native auth/send must stay enabled");
            view.compose.update(cx, |input, cx| {
                input.set_value("Keep manager draft", window, cx)
            });
            view.select_manager_thread(window, cx);
            assert_eq!(view.chat.as_ref().unwrap().thread_id, "mcp:manager");
            assert_eq!(
                view.binding.as_ref().unwrap().manager.thread_id.as_deref(),
                Some("mcp:manager")
            );
            assert_eq!(view.compose.read(cx).value().as_str(), "Keep manager draft");
            view.threads
                .model
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .apply(&[
                    shell::Update::Thread(row("executor-a", "managers"), true),
                    shell::Update::Thread(row("foreign", "unbound"), true),
                ]);
            // Shell activity for other threads cannot retarget any conversation.
            assert_eq!(view.chat.as_ref().unwrap().thread_id, "mcp:manager");
            assert_eq!(view.compose.read(cx).value().as_str(), "Keep manager draft");
            view.set_active(false, window, cx);
            assert!(view.threads.worker.is_none() && view.threads.cancel.is_none());
        });
        visual.run_until_parked();
        for selector in [
            "manager-thread-picker",
            "manager-new-thread",
            "manager-return",
            "manager-thread-list",
        ] {
            assert!(visual.debug_bounds(selector).is_none());
        }
    }
}
