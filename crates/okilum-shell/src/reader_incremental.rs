//! Known source batches never enter the full vault preparation loop.
use super::*;

impl Reader {
    /// A successful local mutation already knows its endpoints. Queue it even
    /// while another batch/checkpoint is running; watcher hints may follow.
    #[cfg(any(unix, windows))]
    pub(super) fn queue_vault_mutation(
        &mut self,
        changes: okilum_core::Changes,
        cx: &mut Context<Self>,
    ) {
        self.deferred_vault_changes.changed.extend(changes.changed);
        self.deferred_vault_changes.removed.extend(changes.removed);
        self.deferred_vault_changes
            .directories
            .extend(changes.directories);
        self.deferred_vault_changes.rescan |= changes.rescan;
        let handle = self.reader_window;
        cx.spawn(async move |this, cx| {
            let _ = handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |this, cx| this.poll_vault(window, cx));
            });
        })
        .detach();
    }

    #[cfg(any(unix, windows))]
    pub(super) fn queue_saved_source(&mut self, cx: &mut Context<Self>) {
        if let Some(trace) = self
            .loading
            .as_ref()
            .and_then(|load| load.opts.diagnostics.as_ref())
        {
            trace.event(
                "save_source_queued",
                serde_json::json!({"path":self.current_rel}),
            );
        }
        self.queue_vault_mutation(
            okilum_core::Changes {
                changed: std::collections::BTreeSet::from([self.current_rel.clone()]),
                ..Default::default()
            },
            cx,
        );
    }

    pub(super) fn start_incremental(
        &mut self,
        changes: okilum_core::Changes,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.incremental_active || self.incremental_initializing {
            self.deferred_vault_changes.rescan |= changes.rescan;
            self.deferred_vault_changes
                .directories
                .extend(changes.directories);
            self.deferred_vault_changes.changed.extend(changes.changed);
            self.deferred_vault_changes.removed.extend(changes.removed);
            return;
        }
        let Some(mut state) = self.incremental_state.take() else {
            self.refresh_inventory(changes, window, cx);
            return;
        };
        let old_searcher = self.searcher.clone().expect("incremental search baseline");
        let root = self.vault_root.clone();
        self.incremental_epoch = self.incremental_epoch.wrapping_add(1);
        let epoch = self.incremental_epoch;
        let cancel = reader_loading::Cancellation::default();
        self.incremental_cancel = Some(cancel.clone());
        let trace = self
            .loading
            .as_ref()
            .and_then(|load| load.opts.diagnostics.clone());
        self.incremental_active = true;
        self.invalidate_links();
        let working = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let progress = working.clone();
        // Visible progress only if this worker takes over half a second.
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let _ = this.update(cx, |this, cx| {
                if progress.load(std::sync::atomic::Ordering::Acquire)
                    && this.incremental_active
                    && this.incremental_epoch == epoch
                    && this.link_notice.is_none()
                {
                    this.link_notice = Some("Updating search data…".into());
                    cx.notify();
                }
            });
        })
        .detach();
        let worker_changes = changes.clone();
        let requested = std::time::Instant::now();
        let publish_trace = trace.clone();
        let cache = self.index_dir.clone();
        // Session copies of the index go under the cache, not the system temp folder (#933).
        let session_dir = self.index_dir.as_ref().map(|dir| dir.join("sessions"));
        let cache_lease = self.cache_lease.clone();
        let worker_cancel = cancel.clone();
        let worker_trace = trace.clone();
        #[cfg(test)]
        let hold = self.incremental_hold.take();
        let mut tasks = self.tasks_index.clone().unwrap_or_default();
        let mut projects = (*self.projects).clone();
        let task = cx.background_executor().spawn(async move {
            #[cfg(test)]
            if let Some(hold) = hold { let _ = hold.recv().await; }
            worker_cancel.check()?;
            let start = std::time::Instant::now();
            let batch = state.apply(&worker_changes, &mut |_, _| worker_cancel.check())?;
            let mut next_tasks = (*tasks).clone();
            for path in &batch.removed { projects.remove(path); }
            for path in &batch.changed { projects.replace_snapshot(&state.snapshot, path); }
            projects.refresh();
            let projects = Arc::new(projects);
            let mut tasks_changed = false;
            for path in &batch.removed { tasks_changed |= next_tasks.remove(path); }
            for path in &batch.changed {
                tasks_changed |= if let Some(raw) = state.snapshot.source(path) { next_tasks.replace(path, &raw) }
                    else { next_tasks.remove(path) };
            }
            if tasks_changed { tasks = Arc::new(next_tasks); }
            let source_ms = start.elapsed().as_secs_f64() * 1000.;
            let searcher = if batch.affected.is_empty() || old_searcher.is_session() { old_searcher } else {
                let forked = match &session_dir {
                    Some(dir) => old_searcher.fork_session_in(dir),
                    None => old_searcher.fork_session(),
                };
                Arc::new(forked.map_err(|error| error.context("Fork the search session"))?)
            };
            let mut sources = std::collections::HashMap::new();
            let mut titles = std::collections::HashMap::new();
            let mut documents = Vec::new();
            let mut removed: Vec<_> = batch.removed.iter().cloned().collect();
            for path in &batch.affected {
                if let Some(raw) = state.snapshot.source(path) {
                    documents.push(okilum_core::search::SearchDocument { path: path.clone(), title: state.vault.note_title(path), text: raw.clone() });
                    let title = display_title_source(&raw).unwrap_or_else(|| state.vault.note_title(path));
                    titles.insert(path.clone(), title);
                    sources.insert(path.clone(), raw);
                } else { removed.push(path.clone()); }
            }
            worker_cancel.check()?;
            if !batch.affected.is_empty() {
                searcher.update_snapshot_batch(&state.vault, &documents, &removed)
                    .map_err(|error| error.context("Update the search index"))?;
            }
            worker_cancel.check()?;
            working.store(false, std::sync::atomic::Ordering::Release);
            let published = Arc::new(state.vault.clone());
            let inventory = Arc::new(okilum_core::quick_open::inventory(
                state.vault.notes.clone(),
                &state.vault.entries,
            ));
            #[cfg(any(unix, windows))]
            let candidates = Some(state.candidates.clone());
            if let Some(trace) = &worker_trace {
                trace.event("incremental_update", serde_json::json!({"directory_hints":worker_changes.directories.len(), "topology_changed":batch.topology_changed, "changed": batch.changed.len(), "removed": batch.removed.len(), "read": batch.read, "affected": batch.affected.len(), "source_graph_ms":source_ms, "duration_ms":start.elapsed().as_secs_f64()*1000.}));
            }
            Ok::<_, anyhow::Error>((state, tasks, projects, searcher, sources, titles, batch, published, inventory,
                { #[cfg(any(unix, windows))] { candidates } #[cfg(not(any(unix, windows)))] { None::<()> } }))
        });
        cx.spawn_in(window, async move |this, cx| {
            // Pins this root through UI publication and subsequent persistence,
            // including cancellation while a replacement vault is opening.
            let _cache_lease = cache_lease;
            let result = task.await;
            let ready = this
                .update_in(cx, |this, window, cx| {
                    if this.vault_root != root || this.incremental_epoch != epoch {
                        return None;
                    }
                    match result {
                        Ok((
                            state,
                            tasks,
                            projects,
                            searcher,
                            sources,
                            titles,
                            batch,
                            published,
                            inventory,
                            candidates,
                        )) => {
                            this.tasks_index = Some(tasks);
                            #[cfg(any(unix, windows))]
                            this.reminder_tick(cx);
                            this.projects = projects;
                            this.vault = published;
                            this.searcher = Some(searcher.clone());
                            #[cfg(any(unix, windows))]
                            {
                                this.move_index = candidates;
                            }
                            #[cfg(not(any(unix, windows)))]
                            let _ = candidates;
                            let known = Arc::make_mut(&mut this.backlink_titles);
                            for path in &batch.removed {
                                known.remove(path);
                            }
                            known.extend(titles);
                            this.set_backlinks(this.vault.backlinks(&this.current_rel), false);
                            if batch.topology_changed {
                                this.sync_tree();
                            }
                            this.quick_open.inventory = Some(inventory);
                            this.refresh_quick_open(cx);
                            if sources.contains_key(&this.current_rel)
                                || batch.removed.contains(&this.current_rel)
                            {
                                this.reconcile_inventory_document(&sources, window, cx);
                            } else {
                                this.refresh_link_preparation(cx);
                            }
                            if this.link_notice.as_deref() == Some("Updating search data…") {
                                this.link_notice = None;
                            }
                            if let Some(trace) = &publish_trace {
                                trace.event("incremental_ui_publish", serde_json::json!({"duration_ms":requested.elapsed().as_secs_f64()*1000.,"epoch":epoch}));
                            }
                            cx.notify();
                            Some((state, searcher, !batch.affected.is_empty() || batch.topology_changed, !batch.affected.is_empty()))
                        }
                        Err(error) => {
                            if let Some(trace) = this
                                .loading
                                .as_ref()
                                .and_then(|load| load.opts.diagnostics.as_ref())
                            {
                                trace.event(
                                    "incremental_fallback",
                                    serde_json::json!({"cause":format!("{error:#}")}),
                                );
                            }
                            // Partial worker mutations are discarded. Existing UI and
                            // search remain available during a fresh background reconcile.
                            this.incremental_active = false;
                            this.incremental_cancel = None;
                            if this.link_notice.as_deref() == Some("Updating search data…") {
                                this.link_notice = None;
                            }
                            this.refresh_inventory(changes.clone(), window, cx);
                            cx.notify();
                            None
                        }
                    }
                })
                .ok()
                .flatten();
            let Some((mut state, searcher, persist, search_changed)) = ready else {
                return;
            };
            // Publish committed search/graph first; checkpoint persistence has
            // one serial owner and cannot delay an already usable Reader frame.
            let persist_task = cx.background_executor().spawn(async move {
                cancel.check()?;
                let start = std::time::Instant::now();
                if let Some(cache) = cache.as_ref().filter(|_| persist) {
                    cancel.check()?;
                    // Held until the delta names the new generation.
                    let mut pin = None;
                    if search_changed {
                    let persisted_search = (|| -> anyhow::Result<String> {
                        use sha2::{Digest, Sha256};
                        reader_loading::validate_external_cache(cache, &state.vault.root)?;
                        // An opaque unique completed generation identifies these exact
                        // committed bytes; unchanged reconciliation can reuse its hint.
                        let generation =
                            format!("{:x}", Sha256::digest(uuid::Uuid::new_v4().as_bytes()));
                        pin = Some(reader_cache::GenerationPin::acquire(cache, &generation)?);
                        let repairs = cache
                            .join("generations")
                            .join(format!("{generation}.repairs"));
                        std::fs::create_dir_all(&repairs)?;
                        let owned = tempfile::Builder::new()
                            .prefix("session-")
                            .tempdir_in(&repairs)?;
                        searcher.copy_committed_to(owned.path())?;
                        cancel.check()?;
                        std::fs::write(owned.path().join("complete"), b"1")?;
                        let _ = owned.keep();
                        Ok(generation)
                    })();
                    match persisted_search {
                        Ok(generation) => state.set_search_generation(Some(generation)),
                        Err(error) => {
                            cancel.check()?;
                            if let Some(trace) = &trace {
                                trace.event(
                                    "incremental_search_persist_failed",
                                    serde_json::json!({"cause":format!("{error:#}")}),
                                );
                            }
                        }
                    }
                    }
                    if let Err(error) = state.persist_delta(cache) {
                        if let Some(trace) = &trace {
                            trace.event(
                                "incremental_persist_failed",
                                serde_json::json!({"cause":format!("{error:#}")}),
                            );
                        }
                    }
                    drop(pin);
                    // Older checkpoints are unreferenced now; Readers still
                    // using one hold their own pin on it.
                    reader_loading::collect_search_generations_at(cache, trace.as_ref());
                }

                if let Some(trace) = &trace {
                    trace.event(
                        "incremental_persist",
                        serde_json::json!({"duration_ms":start.elapsed().as_secs_f64()*1000.}),
                    );
                }
                Ok::<_, anyhow::Error>(state)
            });
            let persisted = persist_task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.vault_root != root || this.incremental_epoch != epoch {
                    return;
                }
                this.incremental_active = false;
                this.incremental_cancel = None;
                match persisted {
                    Ok(state) => this.incremental_state = Some(state),
                    Err(_) => this.refresh_inventory(changes, window, cx),
                }
                if !this.deferred_vault_changes.is_empty() {
                    this.poll_vault(window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn explicit_save_updates_search_backlinks_without_loading_and_keeps_immutable_base(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let cache = temp.path().join("cache");
        let state = temp.path().join("state");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("note.md"), "# Note\n\noldword [[Old]]").unwrap();
        std::fs::write(root.join("Old.md"), "old target").unwrap();
        std::fs::write(root.join("New.md"), "new target").unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("note.md".into()),
                        index_dir: Some(cache.clone()),
                        session_directory: Some(state.clone()),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        let (original, generation) = reader.update_in(visual, |v, window, cx| {
            assert!(v.incremental_state.is_some());
            let baseline = v.searcher.clone().unwrap();
            let generation = v.loading.as_ref().unwrap().generation;
            v.toggle_source(window, cx);
            v.editing
                .as_ref()
                .unwrap()
                .set_value("# Note\n\nnewword [[New]]", window, cx);
            assert!(v.save_source(cx));
            (baseline, generation)
        });
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert!(!v.incremental_active);
            assert!(v.incremental_state.is_some());
            assert_eq!(
                v.loading.as_ref().unwrap().generation,
                generation,
                "no full preparation"
            );
            assert_eq!(
                v.searcher
                    .as_ref()
                    .unwrap()
                    .search("newword", 10)
                    .unwrap()
                    .len(),
                1
            );
            assert!(v
                .searcher
                .as_ref()
                .unwrap()
                .search("oldword", 10)
                .unwrap()
                .is_empty());
            assert!(
                original.search("newword", 10).unwrap().is_empty(),
                "immutable baseline remains unchanged"
            );
            assert_eq!(v.vault.backlinks("New.md").len(), 1);
            assert!(v.vault.backlinks("Old.md").is_empty());
            assert!(v.editing.is_some());
        });
        let saved = okilum_core::vault::warm::Snapshot::load_checked(&cache, &root).unwrap();
        assert!(saved.source("note.md").unwrap().contains("newword"));
        assert!(
            saved.search_generation.is_some(),
            "incremental committed index survives relaunch"
        );
        let (_, _, stats) =
            okilum_core::vault::warm::reconcile(&root, Some(&saved), false, &mut |_, _| Ok(()))
                .unwrap();
        assert_eq!((stats.read, stats.reused), (0, 3));
        assert!(stats.graph_reused);
        reader.update_in(visual, |v, window, cx| {
            v.start_loading(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("note.md".into()),
                    index_dir: Some(cache.clone()),
                    session_directory: Some(state.clone()),
                    index_build_hook: Some(Arc::new(|_| {
                        panic!("committed incremental generation must be opened, never rebuilt")
                    })),
                    ..Default::default()
                },
                window,
                cx,
            );
        });
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert!(!v.loading.as_ref().unwrap().active);
            assert!(v.incremental_state.is_some());
            assert_eq!(
                v.searcher
                    .as_ref()
                    .unwrap()
                    .search("newword", 10)
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(v.vault.backlinks("New.md").len(), 1);
        });
    }

    #[gpui::test]
    fn repeated_saves_retain_bounded_generations_and_reopen_warm(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        let cache = temp.path().join("cache");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("note.md"), "# Note\n\noriginalword").unwrap();
        let opts = || Opts {
            vault: Some(root.clone()),
            note: Some("note.md".into()),
            index_dir: Some(cache.clone()),
            session_directory: Some(temp.path().join("state")),
            ..Default::default()
        };
        let families = || {
            std::fs::read_dir(cache.join("generations"))
                .unwrap()
                .map(|entry| {
                    let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                    name.trim_end_matches(".repairs").to_owned()
                })
                .collect::<std::collections::BTreeSet<_>>()
        };
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| Reader::new(opts(), window, cx));
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        let full = okilum_core::vault::warm::Snapshot::load_checked(&cache, &root)
            .unwrap()
            .search_generation
            .unwrap();
        let mut checkpoints = Vec::new();
        for (n, word) in ["alphaword", "bravoword", "charlieword", "deltaword"]
            .into_iter()
            .enumerate()
        {
            reader.update_in(visual, |v, window, cx| {
                if v.editing.is_none() {
                    v.toggle_source(window, cx);
                }
                v.editing
                    .as_ref()
                    .unwrap()
                    .set_value(&format!("# Note\n\n{word}"), window, cx);
                assert!(v.save_source(cx));
            });
            visual.run_until_parked();
            let saved = okilum_core::vault::warm::Snapshot::load_checked(&cache, &root).unwrap();
            checkpoints.push(saved.search_generation.unwrap());
            assert_eq!(
                families(),
                std::collections::BTreeSet::from([full.clone(), checkpoints[n].clone()]),
                "only the source bank and current delta generations remain"
            );
        }
        assert_eq!(
            checkpoints
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            4,
            "positive control: every save published a new generation"
        );
        reader.update_in(visual, |v, window, cx| {
            v.start_loading(
                Opts {
                    index_build_hook: Some(Arc::new(|_| {
                        panic!("persisted checkpoint generation must be opened, never rebuilt")
                    })),
                    ..opts()
                },
                window,
                cx,
            );
        });
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert!(!v.loading.as_ref().unwrap().active);
            let searcher = v.searcher.as_ref().unwrap();
            assert_eq!(searcher.search("deltaword", 10).unwrap().len(), 1);
            assert!(searcher.search("charlieword", 10).unwrap().is_empty());
        });
    }

    #[gpui::test]
    fn queued_second_change_is_not_lost_and_old_root_batch_cannot_publish(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("a");
        let other = temp.path().join("b");
        for folder in [&root, &other] {
            std::fs::create_dir(folder).unwrap();
            std::fs::write(folder.join("note.md"), "initialword").unwrap();
        }
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("note.md".into()),
                        index_dir: Some(temp.path().join("cache-a")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        let (send, hold) = async_channel::bounded(1);
        reader.update_in(visual, |v, window, cx| {
            v.incremental_hold = Some(hold);
            std::fs::write(root.join("note.md"), "firstword").unwrap();
            v.apply_vault_changes(
                okilum_core::Changes {
                    changed: std::collections::BTreeSet::from(["note.md".into()]),
                    ..Default::default()
                },
                window,
                cx,
            );
        });
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            assert!(v.incremental_active);
            std::fs::write(root.join("note.md"), "secondword").unwrap();
            v.apply_vault_changes(
                okilum_core::Changes {
                    changed: std::collections::BTreeSet::from(["note.md".into()]),
                    ..Default::default()
                },
                window,
                cx,
            );
            assert!(v.deferred_vault_changes.changed.contains("note.md"));
        });
        send.try_send(()).unwrap();
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert_eq!(
                v.searcher
                    .as_ref()
                    .unwrap()
                    .search("secondword", 10)
                    .unwrap()
                    .len(),
                1
            )
        });
        let (send, hold) = async_channel::bounded(1);
        reader.update_in(visual, |v, window, cx| {
            v.incremental_hold = Some(hold);
            v.apply_vault_changes(
                okilum_core::Changes {
                    changed: std::collections::BTreeSet::from(["note.md".into()]),
                    ..Default::default()
                },
                window,
                cx,
            );
            v.start_loading(
                Opts {
                    vault: Some(other.clone()),
                    note: Some("note.md".into()),
                    index_dir: Some(temp.path().join("cache-b")),
                    ..Default::default()
                },
                window,
                cx,
            );
        });
        visual.run_until_parked();
        send.try_send(()).unwrap();
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert_eq!(v.vault_root, other);
            assert_eq!(
                v.searcher
                    .as_ref()
                    .unwrap()
                    .search("initialword", 10)
                    .unwrap()
                    .len(),
                1
            );
            assert!(v
                .searcher
                .as_ref()
                .unwrap()
                .search("secondword", 10)
                .unwrap()
                .is_empty());
            assert!(v.incremental_state.is_some());
        });
    }
}
