//! Linux Manager workspace. The area's Manager binding is never rewritten when
//! another thread is inspected; every thread retains its own composer and journal.
use super::*;
use okilum_t3::stream::{shell, CancelSubscription, StreamError};
use std::collections::BTreeMap;
use std::sync::Mutex;

#[derive(Default)]
pub(super) struct Directory {
    pub enabled: bool,
    pub selected: Option<String>,
    pub views: BTreeMap<(String, String), Entity<ProjectView>>,
    pub model: Option<Arc<Mutex<shell::Directory>>>,
    open: bool,
    input: Option<Entity<InputState>>,
    input_changes: Option<Subscription>,
    worker: Option<Task<()>>,
    cancel: Option<CancelSubscription>,
    epoch: u64,
    error: Option<String>,
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
    fn thread_selection_retains_independent_drafts_and_never_changes_the_area_manager(
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
                view.compose.update(cx, |input, cx| input.set_value("Keep manager draft", window, cx));
                view
            });
            entity = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = entity.unwrap();
        visual.run_until_parked();
        assert!(visual.debug_bounds("manager-workspace").is_some());
        view.update_in(visual, |view, window, cx| {
            view.select_native_thread("executor-a".into(), window, cx);
            let child = view.native_thread_view("executor-a").unwrap().clone();
            child.update(cx, |child, cx| {
                assert_eq!(child.chat.as_ref().unwrap().thread_id, "executor-a");
                child.compose.update(cx, |input, cx| {
                    input.set_value("Keep executor draft", window, cx)
                });
            });
            view.select_native_thread("executor-b".into(), window, cx);
            view.select_native_thread("executor-a".into(), window, cx);
            assert_eq!(
                child.read(cx).compose.read(cx).value().as_str(),
                "Keep executor draft"
            );
            assert_eq!(
                view.binding.as_ref().unwrap().manager.thread_id.as_deref(),
                Some("mcp:manager")
            );
            view.select_native_thread("foreign".into(), window, cx);
            assert_eq!(view.threads.selected.as_deref(), Some("executor-a"));
            let original = view.environment.clone();
            view.environment = Some(Environment::new("other", "https://other.invalid").unwrap());
            assert!(
                view.native_thread_view("executor-a").is_none(),
                "equal thread IDs on another origin cannot reuse drafts"
            );
            view.environment = original;
            view.select_manager_thread(window, cx);
            assert_eq!(view.compose.read(cx).value().as_str(), "Keep manager draft");
            reader_features::set_enabled(reader_features::Feature::Agents, false, cx);
            view.select_native_thread("executor-b".into(), window, cx);
            assert!(
                view.threads.selected.is_none(),
                "a disabled module cannot activate a thread"
            );
            view.set_active(false, window, cx);
            assert!(view.threads.worker.is_none() && view.threads.cancel.is_none());
        });
    }
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
        for view in self.views.values() {
            view.update(cx, |view, _| {
                view.active = false;
                view.native.pause();
            });
        }
        let (enabled, epoch) = (self.enabled, self.epoch);
        let views = std::mem::take(&mut self.views);
        *self = Self::default();
        self.enabled = enabled;
        self.epoch = epoch;
        self.views = views;
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        self.pause();
    }
}
impl ProjectView {
    pub(super) fn native_thread_view(&self, id: &str) -> Option<&Entity<ProjectView>> {
        let origin = self.environment.as_ref()?.origin();
        self.threads.views.get(&(origin, id.to_owned()))
    }
    pub(super) fn rebind_selected_native_thread(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = &self.threads.selected else {
            return;
        };
        let Some(view) = self.native_thread_view(id) else {
            return;
        };
        let Some((client, expiry)) = self.native.connection() else {
            return;
        };
        view.update(cx, |view, cx| {
            view.active = self.active;
            view.install_native(client, expiry, window, cx);
        });
    }
    pub(super) fn set_threads_active(
        &mut self,
        active: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !active {
            self.threads.pause();
        }
        let origin = self.environment.as_ref().map(Environment::origin);
        for ((server, id), view) in &self.threads.views {
            let selected = active
                && origin.as_ref() == Some(server)
                && self.threads.selected.as_ref() == Some(id);
            view.update(cx, |view, cx| view.set_active(selected, window, cx));
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
        if self.threads.input.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search threads…"));
            self.threads.input_changes =
                Some(cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify()));
            self.threads.input = Some(input);
        }
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
                let message = result.as_ref().err().map(ToString::to_string);
                let accepted = this
                    .update_in(cx, |this, window, cx| {
                        if !this.active || this.threads.epoch != epoch {
                            return false;
                        }
                        if let Some(id) = &this.threads.selected {
                            if !model.lock().unwrap().threads.contains_key(id) {
                                if let Some(view) = this.native_thread_view(id) {
                                    view.update(cx, |view, cx| view.set_active(false, window, cx));
                                }
                            }
                        }
                        if changed || this.threads.error != message {
                            this.threads.error = message;
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !accepted {
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
        if let Some(id) = self.threads.selected.take() {
            if let Some(view) = self.native_thread_view(&id) {
                view.update(cx, |view, cx| view.set_active(false, window, cx));
            }
        }
        self.threads.open = false;
        self.native_start(window, cx);
        cx.notify();
    }
    fn select_native_thread(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.active || !reader_features::enabled(reader_features::Feature::Agents, cx) {
            return;
        }
        if self
            .binding
            .as_ref()
            .and_then(|b| b.manager.thread_id.as_ref())
            == Some(&id)
        {
            self.select_manager_thread(window, cx);
            return;
        }
        let row = self
            .threads
            .model
            .as_ref()
            .and_then(|m| m.lock().unwrap().threads.get(&id).cloned());
        let Some(row) = row else {
            return;
        };
        let Some((client, expiry)) = self.native.connection() else {
            return;
        };
        let Some(mut binding) = self.binding.clone() else {
            return;
        };
        if let Some(previous) = &self.threads.selected {
            if let Some(view) = self.native_thread_view(previous) {
                view.update(cx, |view, cx| view.set_active(false, window, cx));
            }
        }
        self.native.pause();
        binding.manager.thread_id = Some(row.id.clone());
        binding.manager.project_id = Some(row.project_id);
        let view = if let Some(view) = self.native_thread_view(&id) {
            view.clone()
        } else {
            let view = cx.new(|cx| {
                let mut view = ProjectView::empty(self.state.clone(), window, cx);
                view.binding = Some(binding);
                view.area = self.area.clone();
                view.context_path = self.context_path.clone();
                view.environment = self.environment.clone();
                view.reason = None;
                view.chat = Some(Conversation::new(&id));
                view.install_native(client, expiry, window, cx);
                view
            });
            cx.observe(&view, |_, _, cx| cx.notify()).detach();
            self.threads.views.insert(
                (self.environment.as_ref().unwrap().origin(), id.clone()),
                view.clone(),
            );
            view
        };
        let (client, expiry) = self.native.connection().unwrap();
        view.update(cx, |view, cx| {
            view.active = true;
            if view
                .native
                .connection()
                .is_none_or(|(previous, _)| !Arc::ptr_eq(&previous, &client))
            {
                view.install_native(client, expiry, window, cx);
            }
            view.set_active(true, window, cx);
        });
        self.threads.selected = Some(id);
        self.threads.open = false;
        cx.notify();
    }

    pub(super) fn render_thread_directory(&self, cx: &mut Context<Self>) -> AnyElement {
        let manager = self
            .binding
            .as_ref()
            .and_then(|b| b.manager.thread_id.as_deref());
        let selected = self.threads.selected.as_deref().or(manager);
        let rows = self
            .threads
            .model
            .as_ref()
            .map(|m| {
                m.lock()
                    .unwrap()
                    .threads
                    .values()
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let current = rows.iter().find(|row| Some(row.id.as_str()) == selected);
        let title = current.map_or("Manager", |row| row.title.as_str());
        let mut view =
            v_flex()
                .w_full()
                .min_w_0()
                .gap_1()
                .px_3()
                .py_2()
                .debug_selector(|| "manager-workspace".into())
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(
                            Button::new("manager-thread-picker")
                                .ghost()
                                .label(title.to_owned())
                                .dropdown_caret(true)
                                .debug_selector(|| "manager-thread-picker".into())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.threads.open = !this.threads.open;
                                    cx.notify();
                                })),
                        )
                        .child(div().flex_1())
                        .when(!self.native.active(), |line| {
                            line.child(
                                Button::new("manager-native-pair")
                                    .ghost()
                                    .small()
                                    .label("Pair")
                                    .disabled(self.native_pairing_busy())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.pair_manager(window, cx)
                                    })),
                            )
                        })
                        .when(self.threads.selected.is_some(), |line| {
                            line.child(
                                Button::new("manager-return")
                                    .ghost()
                                    .label("Manager")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.select_manager_thread(window, cx)
                                    })),
                            )
                        }),
                );
        if self.threads.error.is_some() {
            view = view.child(
                div()
                    .text_color(cx.theme().danger)
                    .whitespace_normal()
                    .child("Could not load threads. Open Diagnostics or reconnect."),
            );
        }
        if !self.native.active() {
            view = view.child(
                div()
                    .text_color(brand::palette(cx).text_muted)
                    .child("Pair with T3 to load threads and live messages."),
            );
        }
        if self.threads.open {
            if let Some(input) = &self.threads.input {
                view = view.child(Input::new(input).w_full());
                let query = input.read(cx).value().to_lowercase();
                let rows: Vec<_> = rows
                    .into_iter()
                    .filter(|r| {
                        r.title.to_lowercase().contains(&query)
                            || r.id.to_lowercase().contains(&query)
                    })
                    .collect();
                let count = rows.len();
                let current = selected.map(str::to_owned);
                let manager = manager.map(str::to_owned);
                let list = gpui::uniform_list(
                    "manager-thread-list",
                    count,
                    cx.processor(move |_this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|index| {
                                let row = &rows[index];
                                let id = row.id.clone();
                                let title = if row.title.is_empty() {
                                    "Untitled thread"
                                } else {
                                    &row.title
                                };
                                Button::new(SharedString::from(format!("manager-thread-{id}")))
                                    .ghost()
                                    .w_full()
                                    .h(px(36.))
                                    .label(format!(
                                        "{}{} · {}",
                                        if manager.as_ref() == Some(&id) {
                                            "Manager · "
                                        } else {
                                            ""
                                        },
                                        title,
                                        row.status
                                    ))
                                    .selected(current.as_ref() == Some(&id))
                                    .tooltip("Open conversation")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.select_native_thread(id.clone(), window, cx)
                                    }))
                                    .into_any_element()
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .h(px((count.clamp(1, 7) as f32) * 36.));
                view = view.child(list);
                if count == 0 {
                    view = view.child("No matching threads in the bound projects.");
                }
            }
        }
        view.into_any_element()
    }
}
