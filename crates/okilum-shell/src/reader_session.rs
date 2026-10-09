//! Explicit duplicate windows share one watcher, mutable graph baseline and
//! search index. Workers belong to the session, so closing a window cannot
//! abandon an in-flight batch or its watcher.
use super::*;
use std::sync::Mutex;

type Baseline = okilum_core::vault::warm::incremental::State;

#[derive(Clone)]
struct Published {
    vault: Arc<Vault>,
    searcher: Arc<Searcher>,
    tasks: Arc<okilum_core::tasks::Index>,
    #[cfg(any(unix, windows))]
    candidates: Option<Arc<okilum_core::link_rewrite::CandidateIndex>>,
    titles: Arc<std::collections::HashMap<String, String>>,
    version: u64,
    warnings: Vec<okilum_core::vault::UnreadableEntry>,
    network: bool,
}

pub(crate) struct Session {
    published: Published,
    state: Option<Baseline>,
    watcher: Option<VaultWatcher>,
    pending: okilum_core::Changes,
    busy: bool,
    reload: bool,
    force_source_read: bool,
    reload_owner: Option<WeakEntity<Reader>>,
    index: Option<PathBuf>,
    // Pin the cache for every window and all outstanding workers.
    lease: Option<Arc<reader_cache::Lease>>,
    #[cfg(test)]
    pub(crate) hold: Option<async_channel::Receiver<()>>,
}

pub(crate) type Shared = Arc<Mutex<Session>>;

#[cfg(test)]
pub(crate) fn with_worker_baseline_detached(session: &Shared, f: impl FnOnce()) {
    let state = session
        .lock()
        .unwrap()
        .state
        .take()
        .expect("published baseline");
    f();
    session.lock().unwrap().state = Some(state);
}

fn merge(into: &mut okilum_core::Changes, changes: okilum_core::Changes) {
    into.changed.extend(changes.changed);
    into.removed.extend(changes.removed);
    into.directories.extend(changes.directories);
    into.rescan |= changes.rescan;
}

impl Reader {
    pub(crate) fn share_session(&mut self) -> Option<Shared> {
        if let Some(session) = &self.shared_session {
            return Some(session.clone());
        }
        if self.single_file
            || self.loading.as_ref().is_some_and(|load| load.active)
            || self.watcher_poll_active
            || self.incremental_active
            || self.incremental_initializing
        {
            return None;
        }
        let searcher = self.searcher.clone()?;
        let state = self.incremental_state.take()?;
        let session = Arc::new(Mutex::new(Session {
            published: Published {
                network: self.loading.as_ref().is_some_and(|load| load.network),
                vault: self.vault.clone(),
                searcher,
                tasks: self.tasks_index.clone().unwrap_or_default(),
                #[cfg(any(unix, windows))]
                candidates: self.move_index.clone(),
                titles: self.backlink_titles.clone(),
                version: 1,
                warnings: self
                    .loading
                    .as_ref()
                    .map(|load| load.warnings.clone())
                    .unwrap_or_default(),
            },
            state: Some(state),
            watcher: self.watcher.take(),
            pending: std::mem::take(&mut self.deferred_vault_changes),
            busy: false,
            reload: false,
            force_source_read: false,
            reload_owner: None,
            index: self.index_dir.clone(),
            lease: self.cache_lease.clone(),
            #[cfg(test)]
            hold: None,
        }));
        self.shared_version = 1;
        self.shared_session = Some(session.clone());
        Some(session)
    }

    pub(crate) fn attach_session(
        &mut self,
        session: Shared,
        note: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        {
            let shared = session.lock().unwrap();
            self.vault_root = shared.published.vault.root.clone();
            self.index_dir = shared.index.clone();
            self.cache_lease = shared.lease.clone();
        }
        self.shared_session = Some(session);
        self.sync_shared_session(window, cx);
        // Preserve explicit intent through the shared-session restore lifecycle.
        if let Some(load) = &mut self.loading {
            load.opts.note = Some(note.clone());
        }
        reader_open::register(cx.entity().downgrade(), self.vault_root.clone(), cx);
        self.open_note(&note, None, window, cx);
        // Markdown restores after asynchronous publication; previews/empty views
        // are ready synchronously. The store restore is idempotent per root.
        if note.is_empty() || self.file_preview.is_some() {
            self.restore_ui_state(window, cx);
        }
    }

    fn sync_shared_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.shared_session else {
            return;
        };
        let (published, sources) = {
            let shared = session.lock().unwrap();
            if self.shared_version == shared.published.version {
                return;
            }
            let sources = shared.state.as_ref().map(|state| {
                state
                    .snapshot
                    .source(&self.current_rel)
                    .map(|raw| (self.current_rel.clone(), raw))
                    .into_iter()
                    .collect()
            });
            (shared.published.clone(), sources)
        };
        if self.loading.is_none() {
            self.loading = Some(reader_loading::Loading {
                generation: 0,
                cancellation: Default::default(),
                phase: "Ready".into(),
                active: false,
                published: true,
                warm: true,
                show_progress: false,
                opts: Opts {
                    vault: Some(self.vault_root.clone()),
                    index_dir: self.index_dir.clone(),
                    session_directory: self.session_directory.clone(),
                    ..Default::default()
                },
                warnings: Vec::new(),
                network: false,
                network_waiting: false,
                progress_revision: 0,
            });
        }
        if let Some(load) = self.loading.as_mut().filter(|load| !load.active) {
            load.network = published.network;
            load.warnings = published.warnings;
        }
        self.shared_version = published.version;
        self.vault = published.vault;
        self.searcher = Some(published.searcher);
        self.tasks_index = Some(published.tasks);
        #[cfg(any(unix, windows))]
        {
            self.move_index = published.candidates;
            self.reminder_tick(cx);
        }
        self.backlink_titles = published.titles.clone();
        self.backlinks = self.vault.backlinks(&self.current_rel);
        self.quick_open.inventory = Some(Arc::new(self.vault.notes.clone()));
        self.sync_tree();
        reader_drawing::invalidate(&self.vault_root, cx);
        self.invalidate_links();
        self.refresh_quick_open(cx);
        if !self.current_rel.is_empty() {
            // Re-read only this window's displayed document; the shared graph
            // and search work has already happened once in the session worker.
            if let Some(sources) = sources {
                self.reconcile_inventory_document(&sources, window, cx);
            } else {
                self.reconcile_published_document(window, cx);
            }
        }
        cx.notify();
    }

    pub(crate) fn publish_shared_ready(&mut self) {
        let Some(session) = &self.shared_session else {
            return;
        };
        let Some(state) = self.incremental_state.take() else {
            return;
        };
        let Some(searcher) = self.searcher.clone() else {
            return;
        };
        let mut shared = session.lock().unwrap();
        shared.state = Some(state);
        shared.watcher = self.watcher.take();
        shared.published = Published {
            network: self.loading.as_ref().is_some_and(|load| load.network),
            vault: self.vault.clone(),
            searcher,
            tasks: self.tasks_index.clone().unwrap_or_default(),
            #[cfg(any(unix, windows))]
            candidates: self.move_index.clone(),
            titles: self.backlink_titles.clone(),
            version: shared.published.version.wrapping_add(1),
            warnings: self
                .loading
                .as_ref()
                .map(|load| load.warnings.clone())
                .unwrap_or_default(),
        };
        shared.busy = false;
        shared.reload = false;
        shared.reload_owner = None;
        self.shared_version = shared.published.version;
    }

    pub(crate) fn finish_shared_refresh_error(&mut self) {
        if let Some(session) = &self.shared_session {
            let mut shared = session.lock().unwrap();
            shared.busy = false;
            shared.reload = false;
            shared.reload_owner = None;
        }
    }

    pub(crate) fn queue_shared_reconcile(
        &mut self,
        changes: okilum_core::Changes,
        force_source_read: bool,
    ) {
        let mut shared = self.shared_session.as_ref().unwrap().lock().unwrap();
        merge(&mut shared.pending, changes);
        shared.pending.rescan = true;
        shared.force_source_read |= force_source_read;
    }

    pub(crate) fn poll_shared_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_shared_session(window, cx);
        let session = self.shared_session.as_ref().unwrap().clone();
        let mut shared = session.lock().unwrap();
        merge(
            &mut shared.pending,
            std::mem::take(&mut self.deferred_vault_changes),
        );
        if let Some(owner) = &shared.reload_owner {
            let alive = if owner.entity_id() == cx.entity_id() {
                self.loading.as_ref().is_some_and(|load| load.active)
                    || self.incremental_initializing
            } else {
                owner.upgrade().is_some_and(|reader| {
                    let reader = reader.read(cx);
                    cx.windows().contains(&reader.reader_window)
                        && (reader.loading.as_ref().is_some_and(|load| load.active)
                            || reader.incremental_initializing)
                })
            };
            if !alive {
                shared.busy = false;
                shared.reload = true;
                shared.reload_owner = None;
            }
        }
        if shared.busy {
            return;
        }
        if shared.reload || shared.pending.rescan {
            shared.busy = true;
            shared.reload_owner = Some(cx.entity().downgrade());
            shared.pending = Default::default();
            let force_source_read = std::mem::take(&mut shared.force_source_read);
            drop(shared);
            self.refresh_inventory_unshared(
                okilum_core::Changes {
                    rescan: true,
                    ..Default::default()
                },
                force_source_read,
                window,
                cx,
            );
            return;
        }
        let Some(mut state) = shared.state.take() else {
            return;
        };
        let mut watcher = shared.watcher.take();
        let mut changes = std::mem::take(&mut shared.pending);
        let previous = shared.published.clone();
        let index = shared.index.clone();
        #[cfg(test)]
        let hold = shared.hold.take();
        shared.busy = true;
        drop(shared);
        cx.background_executor()
            .spawn(async move {
                #[cfg(test)]
                if let Some(hold) = hold {
                    let _ = hold.recv().await;
                }
                if let Some(observed) = watcher.as_mut().and_then(VaultWatcher::poll) {
                    merge(&mut changes, observed);
                }
                let result = if changes.is_empty() {
                    Ok(None)
                } else if changes.rescan {
                    Err(anyhow::anyhow!("Watcher overflow needs reconciliation"))
                } else {
                    update(&mut state, previous, &changes, index.as_deref()).map(Some)
                };
                let mut shared = session.lock().unwrap();
                shared.watcher = watcher;
                shared.state = Some(state);
                match result {
                    Ok(Some(mut published)) => {
                        published.version = shared.published.version.wrapping_add(1);
                        shared.published = published;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        eprintln!("Shared vault needs reconciliation: {error:#}");
                        shared.reload = true;
                        merge(&mut shared.pending, changes);
                    }
                }
                shared.busy = false;
            })
            .detach();
    }
}

fn update(
    state: &mut Baseline,
    mut published: Published,
    changes: &okilum_core::Changes,
    index: Option<&Path>,
) -> anyhow::Result<Published> {
    let batch = state.apply(changes, &mut |_, _| Ok(()))?;
    let mut tasks = (*published.tasks).clone();
    for path in &batch.removed {
        tasks.remove(path);
        Arc::make_mut(&mut published.titles).remove(path);
    }
    for path in &batch.changed {
        if let Some(raw) = state.snapshot.source(path) {
            tasks.replace(path, &raw);
        } else {
            tasks.remove(path);
        }
    }
    let mut documents = Vec::new();
    let mut removed: Vec<_> = batch.removed.iter().cloned().collect();
    for path in &batch.affected {
        if let Some(raw) = state.snapshot.source(path) {
            let title = state.vault.note_title(path);
            Arc::make_mut(&mut published.titles).insert(
                path.clone(),
                display_title_source(&raw).unwrap_or_else(|| title.clone()),
            );
            documents.push(okilum_core::search::SearchDocument {
                path: path.clone(),
                title,
                text: raw,
            });
        } else {
            removed.push(path.clone());
        }
    }
    if !batch.affected.is_empty() {
        if !published.searcher.is_session() {
            published.searcher = Arc::new(published.searcher.fork_session()?);
        }
        published
            .searcher
            .update_snapshot_batch(&state.vault, &documents, &removed)?;
    }
    // Cache failures never discard a valid in-memory update. Startup validates
    // the canonical source revisions before reusing an older checkpoint.
    if let Some(index) = index {
        if let Err(error) = persist(
            state,
            &published.searcher,
            index,
            !batch.affected.is_empty(),
        ) {
            eprintln!("Shared vault cache checkpoint failed: {error:#}");
        }
    }
    published.vault = Arc::new(state.vault.clone());
    published.tasks = Arc::new(tasks);
    #[cfg(any(unix, windows))]
    {
        published.candidates = Some(state.candidates.clone());
    }
    Ok(published)
}

fn persist(
    state: &mut Baseline,
    searcher: &Searcher,
    index: &Path,
    search_changed: bool,
) -> anyhow::Result<()> {
    reader_loading::validate_external_cache(index, &state.vault.root)?;
    if search_changed {
        use sha2::{Digest, Sha256};
        let generation = format!("{:x}", Sha256::digest(uuid::Uuid::new_v4().as_bytes()));
        let repairs = index
            .join("generations")
            .join(format!("{generation}.repairs"));
        std::fs::create_dir_all(&repairs)?;
        let owned = tempfile::Builder::new()
            .prefix("session-")
            .tempdir_in(&repairs)?;
        searcher.copy_committed_to(owned.path())?;
        std::fs::write(owned.path().join("complete"), b"1")?;
        let _ = owned.keep();
        state.set_search_generation(Some(generation));
    }
    state.persist_delta(index)
}
