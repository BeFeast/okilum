//! UI workers retain displayed evidence and delegate every write to FileEditor.
use super::*;
use gpui_component::notification::Notification;
use okilum_core::{
    task_edit::{self, write::Receipt, Change},
    tasks::{Index, Task},
};
use std::sync::atomic::{AtomicBool, Ordering};

/// A calendar belongs to the exact Reader, dashboard and displayed index which
/// opened it. Navigation/rebind cannot retarget it to a matching row elsewhere.
#[derive(Clone)]
pub(super) struct TaskAction {
    owner: Owner,
    index: Arc<Index>,
    publication: Arc<Index>,
    task: Task,
}
#[derive(Clone)]
struct Owner {
    root: PathBuf,
    state: PathBuf,
    index_directory: Option<PathBuf>,
    navigation: u64,
    document: String,
    content: Entity<TextViewState>,
}
impl Owner {
    fn capture(reader: &Reader) -> Option<Self> {
        Some(Self {
            root: reader.vault_root.clone(),
            state: reader.session_directory.clone()?,
            index_directory: reader.index_dir.clone(),
            navigation: reader.navigation.generation,
            document: reader.current_rel.clone(),
            content: reader.content.clone(),
        })
    }
    fn current(&self, reader: &Reader) -> bool {
        reader.vault_root == self.root
            && reader.session_directory.as_ref() == Some(&self.state)
            && reader.index_dir == self.index_directory
            && reader.navigation.generation == self.navigation
            && reader.current_rel == self.document
            && reader.content == self.content
            && reader.editing.is_none()
    }
}
impl TaskAction {
    pub(super) fn capture(reader: &Reader, index: Arc<Index>, task: Task) -> Option<Self> {
        let action = Self::from_displayed(reader, index, task)?;
        // A newly opened calendar must use the CURRENT displayed publication.
        Arc::ptr_eq(&action.index, &action.publication).then_some(action)
    }
    fn from_displayed(reader: &Reader, index: Arc<Index>, task: Task) -> Option<Self> {
        let owner = Owner::capture(reader)?;
        let publication = reader.tasks_index.clone()?;
        let action = Self {
            owner,
            index,
            publication,
            task,
        };
        action.current(reader).then_some(action)
    }
    pub(super) fn scheduled_date(&self) -> time::Date {
        self.task.scheduled.unwrap_or_else(reader_tasks::today)
    }
    fn current(&self, reader: &Reader) -> bool {
        self.owner.current(reader)
            && reader
                .tasks_index
                .as_ref()
                .is_some_and(|index| Arc::ptr_eq(index, &self.publication))
            && self.index.note_tasks(&self.task.path).contains(&self.task)
    }
    pub(super) fn apply(
        &self,
        reader: &mut Reader,
        change: Change,
        window: &mut Window,
        cx: &mut Context<Reader>,
    ) {
        if !self.current(reader) {
            reader_toast::error(
                "Task view changed. Reopen its date menu before editing.",
                window,
                cx,
            );
            return;
        }
        reader.run_task_change(self.clone(), change, window, cx);
    }
}

impl Reader {
    pub(super) fn apply_task_change(
        &mut self,
        index: Arc<Index>,
        task: Task,
        change: Change,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Preserve the existing direct-action contract: the original displayed
        // source evidence may become valid again after exact Undo. Never replace
        // that evidence with a newer index; FileEditor still checks its revision.
        match TaskAction::from_displayed(self, index, task) {
            Some(action) => action.apply(self, change, window, cx),
            None => reader_toast::error(
                "Task view or recovery storage changed. Refresh before editing.",
                window,
                cx,
            ),
        }
    }

    fn run_task_change(
        &mut self,
        action: TaskAction,
        change: Change,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let valid = Arc::new(AtomicBool::new(true));
        let observed = action.clone();
        let revoke = valid.clone();
        // The existing Reader notifications revoke queued work before it starts.
        // No new watcher/controller or shared Reader fields are introduced.
        let subscription = cx.observe_self(move |reader, _| {
            if !observed.current(reader) {
                revoke.store(false, Ordering::Release);
            }
        });
        let on_release = valid.clone();
        let release = cx.on_release(move |_, _| on_release.store(false, Ordering::Release));
        let drafts = action.owner.state.join("editor-drafts");
        #[cfg(test)]
        let hooks = test_hooks::take(cx, &action.owner.root);
        cx.spawn_in(window, async move |reader, cx| {
            let job = action.clone();
            let allowed = valid.clone();
            let storage = drafts.clone();
            #[cfg(test)]
            let worker_hooks = hooks.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    #[cfg(test)]
                    test_hooks::wait_before(&worker_hooks).await;
                    anyhow::ensure!(
                        allowed.load(Ordering::Acquire),
                        "Task view changed before save; nothing was written"
                    );
                    // FileEditor independently locks, validates full-source revision,
                    // refuses dirty recovery and commits via its atomic safe-save API.
                    let result = task_edit::write::apply_indexed(
                        &job.owner.root,
                        &storage,
                        &job.index,
                        &job.task,
                        change,
                    );
                    #[cfg(test)]
                    test_hooks::wait_after(&worker_hooks).await;
                    result
                })
                .await;
            drop(subscription);
            drop(release);
            let _ = reader.update_in_own_window(cx, |reader, window, cx| {
                let current = valid.load(Ordering::Acquire) && action.current(reader);
                #[cfg(test)]
                test_hooks::completed(cx, &action.owner.root, &result, current);
                if !current {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(Some(receipt)) => {
                        reader.refresh_task_source(receipt.path(), cx);
                        let weak = cx.weak_entity();
                        let owner = action.owner.clone();
                        reader_toast::push(
                            Notification::new()
                                .message("Task updated")
                                .content(move |_, _, _| {
                                    let reader = weak.clone();
                                    let receipt = receipt.clone();
                                    let owner = owner.clone();
                                    Button::new("task-undo")
                                        .debug_selector(|| "task-undo".into())
                                        .small()
                                        .ghost()
                                        .label("Undo")
                                        .on_click(move |_, window, cx| {
                                            let _ = reader.update(cx, |reader, cx| {
                                                reader.undo_task(
                                                    receipt.clone(),
                                                    owner.clone(),
                                                    window,
                                                    cx,
                                                )
                                            });
                                        })
                                        .into_any_element()
                                }),
                            Some(Duration::from_secs(8)),
                            window,
                            cx,
                        );
                    }
                    Ok(None) => {}
                    Err(error) => {
                        reader_toast::error(format!("Task unchanged: {error}"), window, cx)
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn refresh_task_source(&mut self, path: &str, cx: &mut Context<Self>) {
        self.queue_vault_mutation(
            okilum_core::Changes {
                changed: [path.to_owned()].into_iter().collect(),
                ..Default::default()
            },
            cx,
        );
    }

    fn undo_task(
        &mut self,
        receipt: Receipt,
        owner: Owner,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !owner.current(self) {
            reader_toast::error(
                "Task Undo belongs to the previous Reader view. Its saved source was kept.",
                window,
                cx,
            );
            return;
        }
        let valid = Arc::new(AtomicBool::new(true));
        let lease = valid.clone();
        let expected = owner.clone();
        let subscription = cx.observe_self(move |reader, _| {
            if !expected.current(reader) {
                lease.store(false, Ordering::Release);
            }
        });
        let on_release = valid.clone();
        let release = cx.on_release(move |_, _| on_release.store(false, Ordering::Release));
        let path = receipt.path().to_owned();
        let job_owner = owner.clone();
        let allowed = valid.clone();
        cx.spawn_in(window, async move |reader, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    anyhow::ensure!(
                        allowed.load(Ordering::Acquire),
                        "Task Undo view changed; source was kept"
                    );
                    receipt.undo(&job_owner.root, &job_owner.state.join("editor-drafts"))
                })
                .await;
            drop(subscription);
            drop(release);
            let _ = reader.update_in_own_window(cx, |reader, window, cx| {
                #[cfg(test)]
                {
                    if cx.try_global::<test_hooks::Harness>().is_none() {
                        cx.set_global(test_hooks::Harness::default());
                    }
                    cx.global_mut::<test_hooks::Harness>()
                        .undo_results
                        .entry(owner.root.clone())
                        .or_default()
                        .push(result.is_ok());
                    cx.notify();
                }
                if !valid.load(Ordering::Acquire) || !owner.current(reader) {
                    return;
                }
                match result {
                    Ok(()) => {
                        reader.refresh_task_source(&path, cx);
                        reader_toast::transient("Task change undone", window, cx);
                    }
                    Err(error) => {
                        reader_toast::error(format!("Cannot undo task change: {error}"), window, cx)
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
pub(crate) mod test_hooks {
    use super::*;
    #[derive(Clone, Default)]
    pub(crate) struct Hooks {
        pub before: Option<async_channel::Receiver<()>>,
        pub after: Option<async_channel::Receiver<()>>,
        pub entered: Option<async_channel::Sender<()>>,
    }
    #[derive(Default)]
    pub(crate) struct Harness {
        pub queued: std::collections::HashMap<PathBuf, Hooks>,
        pub results: std::collections::HashMap<PathBuf, Vec<(bool, bool)>>,
        pub undo_results: std::collections::HashMap<PathBuf, Vec<bool>>,
    }
    impl Global for Harness {}
    pub(crate) fn take(cx: &mut App, root: &Path) -> Hooks {
        cx.try_global::<Harness>()
            .is_some()
            .then(|| cx.global_mut::<Harness>().queued.remove(root))
            .flatten()
            .unwrap_or_default()
    }
    pub(crate) async fn wait_before(hooks: &Hooks) {
        if let Some(hold) = &hooks.before {
            if let Some(entered) = &hooks.entered {
                let _ = entered.try_send(());
            }
            let _ = hold.recv().await;
        }
    }
    pub(crate) async fn wait_after(hooks: &Hooks) {
        if let Some(hold) = &hooks.after {
            if let Some(entered) = &hooks.entered {
                let _ = entered.try_send(());
            }
            let _ = hold.recv().await;
        }
    }
    pub(crate) fn completed(
        cx: &mut App,
        root: &Path,
        result: &anyhow::Result<Option<Receipt>>,
        current: bool,
    ) {
        if cx.try_global::<Harness>().is_none() {
            cx.set_global(Harness::default());
        }
        cx.global_mut::<Harness>()
            .results
            .entry(root.into())
            .or_default()
            .push((result.is_ok(), current));
    }
}
