//! Progressive Reader preparation. Workers own data, never GPUI entities.
use super::*;
use anyhow::{bail, Context as _, Result};
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_MEMORY_SEARCH_BYTES: usize = 128 * 1024 * 1024;

pub(super) struct SessionRecord {
    pub(super) root: PathBuf,
    pub(super) document: String,
    pub(super) single_file: bool,
    pub(super) cache: Option<PathBuf>,
    pub(super) cache_lease: Option<Arc<reader_cache::Lease>>,
    pub(super) diagnostics: Option<reader_diagnostics::Trace>,
}

#[derive(Clone, Default)]
pub(crate) struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn check(&self) -> Result<()> {
        if self.0.load(Ordering::Acquire) {
            bail!("Opening cancelled");
        }
        Ok(())
    }
}

pub(crate) struct Loading {
    pub generation: u64,
    pub cancellation: Cancellation,
    pub phase: String,
    pub active: bool,
    pub published: bool,
    pub warm: bool,
    pub show_progress: bool,
    pub opts: Opts,
    pub warnings: Vec<tessera_core::vault::UnreadableEntry>,
}
impl Drop for Loading {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

enum Event {
    First {
        intent: reader_open::OpenIntent,
        cache_lease: Option<Arc<reader_cache::Lease>>,
        vault: Vault,
        searcher: Option<Box<Searcher>>,
        snapshot: Option<Box<tessera_core::vault::warm::Snapshot>>,
        document: Option<(String, prepared_links::PreparedDocument)>,
        published: Option<async_channel::Sender<()>>,
        recovery_notice: Option<String>,
    },
    Progress(String),
    Siblings(Vault),
    SearchInventory {
        notes: Vec<tessera_core::vault::Note>,
        reconciled: Option<Box<Vault>>,
    },
    PanelPreferences {
        root: PathBuf,
        path: Option<PathBuf>,
        widths: reader_layout::Widths,
        revision: u64,
    },
    Ready {
        move_snapshot: Box<tessera_core::vault::warm::Snapshot>,
        vault: Vault,
        searcher: Option<Box<Searcher>>,
        watcher: Option<VaultWatcher>,
        warnings: Vec<tessera_core::vault::UnreadableEntry>,
        index: Option<PathBuf>,
        sources: std::collections::HashMap<String, String>,
        titles: std::collections::HashMap<String, String>,
    },
    Failed(String),
}

#[cfg(test)]
fn prepare_first(opts: &Opts, cancel: &Cancellation) -> Result<Event> {
    prepare_first_with_ui_notes(opts, cancel, &Default::default())
}

fn prepare_first_with_ui_notes(
    opts: &Opts,
    cancel: &Cancellation,
    notes: &std::collections::BTreeMap<PathBuf, String>,
) -> Result<Event> {
    let mut recovery_notice = None;
    let mut event = prepare_first_with_last_document(opts, cancel, |root| {
        if let Some(note) = notes.get(root).filter(|note| !note.is_empty()) {
            if root.join(note).is_file() {
                return Ok(Some(note.clone()));
            }
        }
        match &opts.session_directory {
            Some(directory) => {
                let (document, notice) =
                    crate::reader_history::ReadingHistory::last_document_or_recover(
                        directory, root,
                    )?;
                recovery_notice = notice;
                Ok(document)
            }
            None => Ok(None),
        }
    })?;
    if let Event::First {
        recovery_notice: notice,
        ..
    } = &mut event
    {
        *notice = recovery_notice;
    }
    Ok(event)
}

fn prepare_first_with_last_document(
    opts: &Opts,
    cancel: &Cancellation,
    last_document: impl FnOnce(&Path) -> Result<Option<String>>,
) -> Result<Event> {
    cancel.check()?;
    let root = opts.vault.as_deref();
    let path = opts
        .open_path
        .clone()
        .unwrap_or_else(|| match (&opts.vault, &opts.note) {
            (Some(root), Some(note)) => root.join(note),
            (Some(root), None) => root.clone(),
            _ => PathBuf::new(),
        });
    let resolve_phase = opts
        .diagnostics
        .as_ref()
        .map(|trace| trace.phase("requested_path_resolve"));
    let canonical_path = path.canonicalize().with_context(|| {
        format!(
            "Resolve requested path {}",
            tessera_core::vault::display_path(&path)
        )
    })?;
    drop(resolve_phase);
    let reusable = opts
        .reusable_roots
        .iter()
        .find(|root| canonical_path.starts_with(root));
    if opts.single_file || (canonical_path.is_file() && root.is_none() && reusable.is_none()) {
        let mut intent = reader_open::OpenIntent::validate_cached(&canonical_path, root, None)?;
        intent.single_file = true;
        let rel = intent
            .note
            .clone()
            .context("A quick viewer requires a document")?;
        let mut vault = Vault::from_note_paths([rel.clone()]);
        vault.root = intent.root.clone();
        vault.single_file = true;
        let document = if opts.use_html {
            prepared_links::PreparedDocument {
                source: tessera_core::render_html(&vault, &rel, "InspiredGitHub")?,
                original: None,
                identities: Vec::new(),
                frontmatter: None,
            }
        } else {
            let document = tessera_core::render::reader_document(&vault, &rel)?;
            prepared_links::PreparedDocument {
                source: document.rendered,
                original: Some(document.original_body),
                identities: document.links,
                frontmatter: document.frontmatter,
            }
        };
        return Ok(Event::First {
            intent,
            vault,
            cache_lease: None,
            searcher: None,
            snapshot: None,
            document: Some((rel, document)),
            published: None,
            recovery_notice: None,
        });
    }
    // Local derived data is prepared before preferences, recursive watching or
    // reconciliation. Publish it atomically with the first usable document.
    let candidate_root = if canonical_path.is_dir() {
        Some(canonical_path.clone())
    } else {
        root.or(reusable.map(PathBuf::as_path))
            .and_then(|root| root.canonicalize().ok())
    };
    let mut scoped_opts = opts.clone();
    if let (Some(trace), Some(root)) = (&opts.diagnostics, &candidate_root) {
        scoped_opts.diagnostics = Some(trace.for_root(root.clone()));
    }
    if opts.index_dir.is_none() {
        if let Some(root) = &candidate_root {
            scoped_opts.cache_lease = acquire_cache_lease(root, opts);
        }
    }
    let opts = &scoped_opts;
    let snapshot = candidate_root.as_ref().and_then(|root| {
        let base = reader_open::cache_path_for(root, opts)
            .ok()?;
        validate_external_cache(&base, root).ok()?;
        let _phase = opts.diagnostics.as_ref().map(|trace| trace.phase("startup_snapshot_load"));
        let result = tessera_core::vault::warm::StartupSnapshot::load(&base, root);
        if let Some(trace) = &opts.diagnostics {
            trace.event("warm_cache", serde_json::json!({ "found": result.is_ok(), "reason": result.as_ref().err().map(|error| format!("{error:#}")), "startup_bytes": std::fs::metadata(base.join("reader-startup.json")).ok().map(|m| m.len()), "source_bank_bytes": std::fs::metadata(base.join("reader-snapshot.json")).ok().map(|m| m.len()) }));
        }
        result.ok()
    });
    let cached_primary = candidate_root
        .as_ref()
        .and_then(|root| {
            let rel = canonical_path.strip_prefix(root).ok()?;
            snapshot
                .as_ref()?
                .source(&tessera_core::vault::note_path(rel))
        })
        .is_some();
    let validation_phase = opts
        .diagnostics
        .as_ref()
        .map(|trace| trace.phase("primary_path_validation"));
    let intent = if cached_primary {
        reader_open::OpenIntent::validate_cached(
            &canonical_path,
            root,
            reusable.map(PathBuf::as_path),
        )?
    } else {
        reader_open::OpenIntent::validate(&canonical_path, root, reusable.map(PathBuf::as_path))?
    };
    drop(validation_phase);
    let mut validated_opts = opts.clone();
    if let Some(trace) = &opts.diagnostics {
        validated_opts.diagnostics = Some(trace.for_root(intent.root.clone()));
    }
    if validated_opts.index_dir.is_none() {
        validated_opts.cache_lease = acquire_cache_lease(&intent.root, opts);
    }
    let opts = &validated_opts;
    cancel.check()?;
    let selection_phase = opts
        .diagnostics
        .as_ref()
        .map(|trace| trace.phase("history_and_primary_discovery"));
    if opts.exact_restore && (Some(&intent.root) != opts.vault.as_ref() || intent.note != opts.note)
    {
        bail!("Saved Reader path changed identity");
    }
    let selected = match &intent.note {
        Some(note) => Some(note.clone()),
        None if opts.exact_restore => {
            if Vault::discover_document(&intent.root, &mut || cancel.check())?.is_some() {
                bail!("Saved empty Reader root now contains documents; choose one explicitly");
            }
            None
        }
        None => {
            let saved = last_document(&intent.root)?;
            let explicitly_empty = saved.as_deref() == Some("");
            let hint = saved.filter(|hint| !hint.is_empty()).and_then(|hint| {
                let path = intent.root.join(&hint);
                let validated = if snapshot.as_ref().and_then(|s| s.source(&hint)).is_some() {
                    reader_open::OpenIntent::validate_cached(&path, Some(&intent.root), None)
                } else {
                    reader_open::OpenIntent::validate(&path, Some(&intent.root), None)
                };
                validated.ok().and_then(|validated| validated.note)
            });
            match hint {
                Some(note) => Some(note),
                None if explicitly_empty => Some(String::new()),
                None => Vault::discover_document(&intent.root, &mut || cancel.check())?,
            }
        }
    };
    drop(selection_phase);
    let inventory_phase = opts
        .diagnostics
        .as_ref()
        .map(|trace| trace.phase("cached_inventory_construct"));
    let mut vault = snapshot
        .as_ref()
        .map(|snapshot| snapshot.vault())
        .unwrap_or_else(|| {
            Vault::from_note_paths(selected.iter().filter(|rel| !rel.is_empty()).cloned())
        });
    vault.root = intent.root.clone();
    drop(inventory_phase);
    if let Some(trace) = &opts.diagnostics {
        trace.event("primary_selection", serde_json::json!({ "path": selected, "cached": selected.as_ref().is_some_and(|path| snapshot.as_ref().and_then(|s| s.source(path)).is_some()) }));
    }
    let document_phase = opts
        .diagnostics
        .as_ref()
        .map(|trace| trace.phase("primary_source_and_render"));
    let document = selected
        .map(|rel| {
            cancel.check()?;
            let saved = snapshot.as_ref().and_then(|snapshot| snapshot.source(&rel));
            let document = if rel.is_empty() {
                prepared_links::PreparedDocument {
                    source: String::new(),
                    original: Some(String::new()),
                    identities: Vec::new(),
                    frontmatter: None,
                }
            } else if opts.use_html {
                prepared_links::PreparedDocument {
                    source: match &saved {
                        Some(raw) => tessera_core::render::render_html_from_source(
                            &vault,
                            &rel,
                            raw,
                            "InspiredGitHub",
                        )?,
                        None => tessera_core::render_html(&vault, &rel, "InspiredGitHub")?,
                    },
                    original: None,
                    identities: Vec::new(),
                    frontmatter: None,
                }
            } else {
                let document = match &saved {
                    Some(raw) => {
                        tessera_core::render::reader_document_from_source(&vault, &rel, raw)
                    }
                    None => tessera_core::render::reader_document(&vault, &rel)?,
                };
                prepared_links::PreparedDocument {
                    source: document.rendered,
                    original: Some(document.original_body),
                    identities: document.links,
                    frontmatter: document.frontmatter,
                }
            };
            cancel.check()?;
            Ok::<_, anyhow::Error>((rel, document))
        })
        .transpose()?;
    drop(document_phase);
    let search_phase = opts
        .diagnostics
        .as_ref()
        .map(|trace| trace.phase("saved_search_index_open"));
    let searcher = snapshot.as_ref().and_then(|snapshot| {
        let generation = snapshot.search_generation.as_ref()?;
        let base = reader_open::cache_path_for(&intent.root, opts).ok()?;
        open_completed_generation(&base.join("generations").join(generation))
    });
    drop(search_phase);
    if let Some(trace) = &opts.diagnostics {
        trace.event("first_worker_ready", serde_json::json!({ "notes": vault.notes.len(), "warm": snapshot.is_some(), "cached_search": searcher.is_some() }));
    }
    Ok(Event::First {
        cache_lease: opts.cache_lease.clone(),
        intent,
        vault,
        searcher: searcher.map(Box::new),
        snapshot: snapshot.and_then(|snapshot| snapshot.into_previous()),
        document,
        published: None,
        recovery_notice: None,
    })
}

/// Idle cleanup after the persisted hints name the published generation. A
/// failure leaves derived generations in place and is retried next time.
pub(super) fn collect_search_generations(root: &Path, opts: &Opts) {
    let Ok(base) = reader_open::cache_path_for(root, opts) else {
        return;
    };
    if validate_external_cache(&base, root).is_ok() {
        collect_search_generations_at(&base, opts.diagnostics.as_ref());
    }
}

pub(super) fn collect_search_generations_at(
    base: &Path,
    diagnostics: Option<&super::reader_diagnostics::Trace>,
) {
    let started = std::time::Instant::now();
    let result = reader_cache::collect_generations(base);
    if let Some(trace) = diagnostics {
        trace.event(
            "search_generation_retention",
            serde_json::json!({
                "duration_ms":started.elapsed().as_secs_f64()*1000.,
                "retired":result.as_ref().ok().map(|report| report.retired.len()),
                "retained":result.as_ref().ok().map(|report| report.retained),
                "skipped":result.as_ref().ok().and_then(|report| report.skipped),
                "cleanup_errors":result.as_ref().ok().map(|report| &report.cleanup_errors),
                "error":result.as_ref().err().map(|e| format!("{e:#}")),
            }),
        );
    }
}

fn acquire_cache_lease(root: &Path, opts: &Opts) -> Option<Arc<reader_cache::Lease>> {
    let result = reader_open::cache_path_for(root, opts).and_then(|path| {
        if let Some(lease) = opts
            .cache_lease
            .as_ref()
            .filter(|lease| lease.root == root && lease.path == path)
        {
            return Ok(lease.clone());
        }
        reader_cache::Lease::acquire(path, root).map(Arc::new)
    });
    if let Some(trace) = &opts.diagnostics {
        trace.for_root(root.to_owned()).event(
            "vault_cache_lease",
            serde_json::json!({
                "acquired":result.is_ok(), "error":result.as_ref().err().map(|e| format!("{e:#}")),
            }),
        );
    }
    result.ok()
}

#[cfg(test)]
fn prepare_rest(
    root: &Path,
    opts: &Opts,
    cancel: &Cancellation,
    send: &async_channel::Sender<Event>,
) -> Result<Event> {
    prepare_rest_with_reader(root, opts, cancel, send, &mut |path| std::fs::read(path))
}

fn prepare_rest_with_snapshot(
    root: &Path,
    opts: &Opts,
    cancel: &Cancellation,
    send: &async_channel::Sender<Event>,
    previous: Option<tessera_core::vault::warm::Snapshot>,
) -> Result<Event> {
    prepare_rest_with_io_and_snapshot(
        root,
        opts,
        cancel,
        send,
        &mut |path| std::fs::read(path),
        |root| {
            if tessera_core::watch::is_network_root(root) {
                bail!("Auto-refresh is limited on this network drive. Use Rescan to check changes made by other clients.");
            }
            VaultWatcher::new(root).map_err(anyhow::Error::new)
        },
        MAX_MEMORY_SEARCH_BYTES,
        previous,
        true,
    )
}

#[cfg(test)]
fn prepare_rest_with_reader(
    root: &Path,
    opts: &Opts,
    cancel: &Cancellation,
    send: &async_channel::Sender<Event>,
    read: &mut impl FnMut(&Path) -> std::io::Result<Vec<u8>>,
) -> Result<Event> {
    prepare_rest_with_io(
        root,
        opts,
        cancel,
        send,
        read,
        |root| VaultWatcher::new(root).map_err(anyhow::Error::new),
        MAX_MEMORY_SEARCH_BYTES,
    )
}

#[cfg(test)]
fn prepare_rest_with_io(
    root: &Path,
    opts: &Opts,
    cancel: &Cancellation,
    send: &async_channel::Sender<Event>,
    read: &mut impl FnMut(&Path) -> std::io::Result<Vec<u8>>,
    watch: impl FnOnce(&Path) -> Result<VaultWatcher>,
    memory_search_budget: usize,
) -> Result<Event> {
    prepare_rest_with_io_and_snapshot(
        root,
        opts,
        cancel,
        send,
        read,
        watch,
        memory_search_budget,
        None,
        false,
    )
}

// The final inputs inject I/O failures in preparation tests.
#[allow(clippy::too_many_arguments)]
fn prepare_rest_with_io_and_snapshot(
    root: &Path,
    opts: &Opts,
    cancel: &Cancellation,
    send: &async_channel::Sender<Event>,
    read: &mut impl FnMut(&Path) -> std::io::Result<Vec<u8>>,
    watch: impl FnOnce(&Path) -> Result<VaultWatcher>,
    memory_search_budget: usize,
    previous: Option<tessera_core::vault::warm::Snapshot>,
    parallel: bool,
) -> Result<Event> {
    cancel.check()?;
    send.send_blocking(Event::Progress("Watching for changes".into()))
        .map_err(|_| anyhow::anyhow!("Reader closed"))?;
    let base =
        reader_open::cache_path_for(root, opts).context("Choose external Reader search cache")?;
    let mut warnings = Vec::new();
    let disk_cache = match validate_external_cache(&base, root) {
        Ok(()) => true,
        Err(error) if error.chain().any(|cause| cause.is::<std::io::Error>()) => {
            warnings.push(preparation_warning(&base, "validate search cache", &error));
            false
        }
        Err(error) => return Err(error), // Unsafe cache locations remain a failed open.
    };
    let mut disk_search = disk_cache;
    // Register before inventory so events during scan/index are retained.
    let watcher_phase = opts
        .diagnostics
        .as_ref()
        .map(|trace| trace.phase("watcher_registration"));
    let mut watcher = match watch(root) {
        Ok(watcher) => Some(watcher),
        Err(error) => {
            warnings.push(preparation_warning(
                root,
                "watch vault (automatic refresh unavailable)",
                &error.context(format!(
                    "Register recursive change watcher {}",
                    tessera_core::vault::display_path(root)
                )),
            ));
            None
        }
    };
    drop(watcher_phase);
    use tessera_core::vault::warm;
    let bank_phase = opts
        .diagnostics
        .as_ref()
        .map(|trace| trace.phase("reconcile_source_bank_load"));
    let mut previous = if disk_search {
        previous.or_else(|| {
            let loaded = warm::Snapshot::load_checked(&base, root);
            if let Some(trace) = &opts.diagnostics {
                trace.event(
                    "reconcile_source_cache",
                    serde_json::json!({
                        "found": loaded.is_ok(),
                        "reason": loaded.as_ref().err().map(|error| format!("{error:#}")),
                    }),
                );
            }
            loaded.ok()
        })
    } else {
        None
    };
    drop(bank_phase);
    let replay_phase = opts
        .diagnostics
        .as_ref()
        .map(|trace| trace.phase("replay_cursor_load"));
    let replay = reader_replay::prepare(
        root,
        opts.session_directory.as_deref(),
        previous.as_ref().map(|p| p.id.as_str()),
    );
    drop(replay_phase);
    if let Some(trace) = &opts.diagnostics {
        trace.event(
            "replay_invalidations",
            serde_json::json!({
                "dirty_paths": replay.dirty.len(),
                "whole_root_dirty": replay.dirty.iter().any(String::is_empty),
                "directory_paths": replay.directories.len(),
                "root_directory_changed": replay.directories.iter().any(String::is_empty),
                "force_all": replay.force_all,
            }),
        );
    }
    if let Some(snapshot) = &mut previous {
        snapshot.invalidate_paths(&replay.dirty);
    }
    let force_source_read = replay.force_all || opts.force_source_read;
    let mut priority = opts.note.iter().cloned().collect::<Vec<_>>();
    if let Some(current) = &opts.note {
        if let Some(parent) = Path::new(current).parent() {
            let parent = tessera_core::vault::note_path(parent);
            priority.push(if parent.is_empty() {
                "/".into()
            } else {
                format!("{parent}/")
            });
        }
        if let Some(saved) = &previous {
            // Reuse the prepared graph, including Markdown/property/ambiguous
            // targets. Scheduling must not introduce per-link filesystem I/O.
            priority.extend(saved.linked_paths_from(current));
        }
    }
    priority.extend(opts.reconcile_recent.iter().cloned());
    // Checkpoint generations published here stay pinned until the persisted
    // hints name them, so idle collection cannot retire them in between.
    let mut publication_pins = Vec::new();
    loop {
        cancel.check()?;
        let mut last = std::time::Instant::now();
        let mut measured_phase = String::new();
        let mut phase_started = std::time::Instant::now();
        let reconcile_phase = opts
            .diagnostics
            .as_ref()
            .map(|trace| trace.phase("background_reconcile"));
        let mut report = |phase: &str, count| {
            if phase != measured_phase {
                if !measured_phase.is_empty() {
                    if let Some(trace) = &opts.diagnostics {
                        trace.event("reconcile_phase", serde_json::json!({ "name": measured_phase, "duration_ms": phase_started.elapsed().as_secs_f64() * 1000., "notes": count }));
                    }
                }
                measured_phase = phase.to_owned();
                phase_started = std::time::Instant::now();
            }
            cancel.check()?;
            if last.elapsed() >= Duration::from_millis(80) {
                progress(send, format!("{phase} · {count} notes"))?;
                last = std::time::Instant::now();
            }
            Ok(())
        };
        let reconciled = if parallel {
            warm::reconcile_parallel_prioritized(
                root,
                previous.as_ref(),
                force_source_read,
                &priority,
                &mut report,
            )
        } else {
            warm::reconcile_with_reader(
                root,
                previous.as_ref(),
                force_source_read,
                &mut report,
                read,
            )
        };
        let (vault, mut snapshot, stats) = reconciled.with_context(|| {
            format!(
                "Scan vault inventory {}",
                tessera_core::vault::display_path(root)
            )
        })?;
        if let Some(trace) = &opts.diagnostics {
            trace.event("reconcile_phase", serde_json::json!({ "name": measured_phase, "duration_ms": phase_started.elapsed().as_secs_f64() * 1000. }));
        }
        drop(reconcile_phase);
        if let Some(trace) = &opts.diagnostics {
            trace.event("reconcile_stats", serde_json::json!({ "notes": vault.notes.len(), "read": stats.read, "reused": stats.reused, "unreadable": vault.unreadable.len(), "replay_force_all": replay.force_all, "manual_force_read": opts.force_source_read, "reuse": stats.reuse, "graph_reused": stats.graph_reused, "graph_updated_sources":if stats.graph_reused { stats.affected.len() } else { vault.notes.len() } }));
        }
        let search_phase = opts
            .diagnostics
            .as_ref()
            .map(|trace| trace.phase("background_search_prepare"));
        let mut source_snapshot = snapshot
            .sources()
            .context("Decode Reader source snapshot")?;
        let readable = vault
            .notes
            .iter()
            .filter(|note| source_snapshot.contains_key(&note.path))
            .cloned();
        send.send_blocking(Event::SearchInventory {
            notes: tessera_core::quick_open::inventory(readable, &vault.entries),
            // Publish cold and warm trees before search preparation; a usable
            // inventory does not depend on a writable search cache.
            reconciled: Some(Box::new(vault.clone())),
        })
        .map_err(|_| anyhow::anyhow!("Reader closed"))?;
        progress(send, "Checking search data".into())?;
        // Content-addressed immutable generations need no shared mutable pointer.
        // Concurrent attempts may reuse the same completed bytes, never delete an
        // active index, or publish an older generation over newer data.
        use sha2::{Digest, Sha256};
        let mut fingerprint = Sha256::new();
        fingerprint.update(b"reader-search-v2");
        let mut documents = Vec::with_capacity(vault.notes.len());
        for note in &vault.notes {
            cancel.check()?;
            // Unreadable identities still affect link resolution in the index.
            fingerprint.update(note.path.len().to_le_bytes());
            fingerprint.update(note.path.as_bytes());
            let Some(source) = source_snapshot.remove(&note.path) else {
                fingerprint.update([0]);
                continue;
            };
            fingerprint.update([1]);
            fingerprint.update(source.len().to_le_bytes());
            fingerprint.update(&source);
            documents.push(tessera_core::search::SearchDocument {
                path: note.path.clone(),
                title: note.title.clone(),
                text: String::from_utf8_lossy(&source).into_owned(),
            });
        }
        #[cfg(test)]
        if let Some(hook) = &opts.rest_snapshot_hook {
            hook();
        }
        let fingerprint = format!("{:x}", fingerprint.finalize());
        let mut generation = fingerprint;
        let mut searcher = None;
        if disk_search && stats.graph_reused {
            if let Some(old_generation) = &snapshot.search_generation {
                let prior = base.join("generations").join(old_generation);
                if stats.affected.is_empty() {
                    generation = old_generation.clone();
                } else {
                    let updated: Vec<_> = documents
                        .iter()
                        .filter(|document| stats.affected.contains(&document.path))
                        .map(|document| tessera_core::search::SearchDocument {
                            path: document.path.clone(),
                            title: document.title.clone(),
                            text: document.text.clone(),
                        })
                        .collect();
                    let source_paths: std::collections::HashSet<_> =
                        snapshot.source_paths().collect();
                    let removed: Vec<_> = stats
                        .affected
                        .iter()
                        .filter(|path| !source_paths.contains(path.as_str()))
                        .cloned()
                        .collect();
                    match prepare_search_batch(&vault, &updated, &removed, &base, &prior, cancel) {
                        Ok(Some((prepared, published, pin))) => {
                            searcher = Some(prepared);
                            generation = published;
                            publication_pins.push(pin);
                            if let Some(trace) = &opts.diagnostics {
                                trace.event("search_incremental_prepare", serde_json::json!({"updated":updated.len(),"removed":removed.len(),"baseline_hit":true}));
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            cancel.check()?;
                            if let Some(trace) = &opts.diagnostics {
                                trace.event(
                                    "search_incremental_fallback",
                                    serde_json::json!({"cause":format!("{error:#}")}),
                                );
                            }
                        }
                    }
                }
            }
        }
        let destination = base.join("generations").join(&generation);
        if disk_search && searcher.is_none() {
            match prepare_search_generation(
                &vault,
                &documents,
                &base,
                &destination,
                opts,
                cancel,
                send,
            ) {
                Ok(prepared) => searcher = Some(prepared),
                Err(error) => {
                    cancel.check()?; // Cancellation is never converted into partial success.
                    warnings.push(preparation_warning(&base, "persist search cache", &error));
                    disk_search = false;
                }
            }
        }
        if searcher.is_none() {
            let source_bytes = documents.iter().fold(0usize, |total, document| {
                total.saturating_add(document.text.len())
            });
            if source_bytes <= memory_search_budget {
                progress(send, "Preparing search in memory".into())?;
                searcher = Some(
                    Searcher::build_snapshot_in_memory(&vault, &documents, &mut |_, _| {
                        cancel.check()
                    })
                    .context("Build in-memory Reader search fallback")?,
                );
            } else {
                warnings.push(preparation_warning(&base, "prepare search (unavailable)",
                    &anyhow::anyhow!("Search is unavailable: {source_bytes} bytes of readable content exceeds the {memory_search_budget}-byte in-memory fallback limit. Reading, note-name search and backlinks remain available.")));
            }
        }
        drop(search_phase);
        snapshot.search_generation = disk_search.then_some(generation);
        cancel.check()?;
        if let Some(changes) = watcher
            .as_mut()
            .and_then(VaultWatcher::drain_preparation_changes)
        {
            if let Some(trace) = &opts.diagnostics {
                trace.event("reconcile_scan_time_changes", serde_json::json!({
                    "changed":changes.changed.len(), "removed":changes.removed.len(),
                    "directory_hints":changes.directories.len(), "rescan":changes.rescan,
                    "examples":changes.changed.iter().chain(&changes.removed).chain(&changes.directories).take(8).collect::<Vec<_>>()
                }));
            }
            previous = Some(snapshot);
            progress(send, "Updating changed notes".into())?;
            continue;
        }
        if disk_cache && vault.inventory_scanned {
            let _phase = opts
                .diagnostics
                .as_ref()
                .map(|trace| trace.phase("snapshot_persist"));
            match warm::save_provisional(&snapshot, &vault, &base, opts.note.as_deref()) {
                Ok(()) => {
                    if let Err(error) = replay.save(&snapshot.id) {
                        eprintln!("Cannot save replay cursor: {error}");
                    }
                }
                Err(error) => {
                    eprintln!("Cannot save warm Reader snapshot: {error}");
                    if let Some(trace) = &opts.diagnostics {
                        trace.event(
                            "snapshot_persist_failed",
                            serde_json::json!({ "error": format!("{error:#}") }),
                        );
                    }
                }
            }
        }
        super::reader_diagnostics::record_scan_with_warnings(
            &vault,
            &warnings,
            opts.session_directory.as_deref(),
        );
        let title_phase = opts
            .diagnostics
            .as_ref()
            .map(|trace| trace.phase("cached_backlink_titles"));
        let sources: std::collections::HashMap<String, String> = documents
            .into_iter()
            .map(|document| (document.path, document.text))
            .collect();
        let titles = vault
            .notes
            .iter()
            .map(|note| {
                let title = sources
                    .get(&note.path)
                    .and_then(|raw| display_title_source(raw))
                    .unwrap_or_else(|| note.title.clone());
                (note.path.clone(), title)
            })
            .collect();
        drop(title_phase);
        drop(publication_pins);
        return Ok(Event::Ready {
            move_snapshot: Box::new(snapshot),
            vault,
            searcher: searcher.map(Box::new),
            watcher,
            warnings,
            index: Some(base),
            sources,
            titles,
        });
    }
}

fn preparation_warning(
    path: &Path,
    operation: &'static str,
    error: &anyhow::Error,
) -> tessera_core::vault::UnreadableEntry {
    tessera_core::vault::UnreadableEntry {
        path: path.to_path_buf(),
        operation,
        error: tessera_core::vault::display_error(&format!("{error:#}")),
    }
}

/// Fork a completed immutable baseline; never mutate an index used by First or
/// another Reader. Publication uses the same complete-marker scheme as save.
fn prepare_search_batch(
    vault: &Vault,
    documents: &[tessera_core::search::SearchDocument],
    removed: &[String],
    base: &Path,
    prior: &Path,
    cancel: &Cancellation,
) -> Result<Option<(Searcher, String, reader_cache::GenerationPin)>> {
    // Repairs belong to this exact generation, never an older ID. Incremental
    // checkpoints (and Windows full builds) intentionally have no direct index.
    let Some(old) = open_completed_generation(prior) else {
        return Ok(None);
    };
    cancel.check()?;
    let searcher = old.fork_session()?;
    drop(old);
    searcher.update_snapshot_batch(vault, documents, removed)?;
    cancel.check()?;
    use sha2::{Digest, Sha256};
    let generation = format!("{:x}", Sha256::digest(uuid::Uuid::new_v4().as_bytes()));
    let pin = reader_cache::GenerationPin::acquire(base, &generation)?;
    let repairs = base
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
    Ok(Some((searcher, generation, pin)))
}

/// The pin is taken before lookup and staging and travels with the returned
/// searcher, which reads the published directory for its whole lifetime.
fn prepare_search_generation(
    vault: &Vault,
    documents: &[tessera_core::search::SearchDocument],
    base: &Path,
    destination: &Path,
    opts: &Opts,
    cancel: &Cancellation,
    send: &async_channel::Sender<Event>,
) -> Result<Searcher> {
    let pin = pin_generation(destination).with_context(|| {
        format!(
            "Pin external search generation {}",
            tessera_core::vault::display_path(destination)
        )
    })?;
    prepare_search_generation_pinned(vault, documents, base, destination, opts, cancel, send)
        .map(|searcher| searcher.pinned(pin))
}

fn prepare_search_generation_pinned(
    vault: &Vault,
    documents: &[tessera_core::search::SearchDocument],
    base: &Path,
    destination: &Path,
    _opts: &Opts,
    cancel: &Cancellation,
    send: &async_channel::Sender<Event>,
) -> Result<Searcher> {
    use tessera_core::vault::display_path;
    let cached = {
        let _phase = _opts
            .diagnostics
            .as_ref()
            .map(|trace| trace.phase("search_generation_lookup"));
        open_completed_unpinned(destination)
    };
    if let Some(trace) = &_opts.diagnostics {
        trace.event(
            "search_generation_cache",
            serde_json::json!({"hit":cached.is_some()}),
        );
    }
    if let Some(searcher) = cached {
        return Ok(searcher);
    }
    // Windows does not permit moving directories with open descendant handles.
    // Build an immutable UUID sibling in the existing repairs namespace instead.
    // The completion marker publishes it; unfinished builds remain invisible.
    let in_place = cfg!(windows);
    #[cfg(test)]
    let in_place = in_place || _opts.search_publish_in_place;
    let id = uuid::Uuid::new_v4();
    let mut owned = Staging(
        if in_place {
            destination.with_extension("repairs").join(id.to_string())
        } else {
            // Named by family so collection can reclaim a crashed attempt.
            base.join("attempts").join(format!(
                "{}.{id}",
                destination
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            ))
        },
        true,
    );
    progress(send, "Preparing search".into())?;
    let mut last = std::time::Instant::now();
    let built = Searcher::build_snapshot(vault, documents, &owned.0, &mut |phase, count| {
        #[cfg(test)]
        if let Some(hook) = &_opts.index_build_hook {
            hook(&owned.0);
        }
        cancel.check()?;
        if last.elapsed() >= Duration::from_millis(80) {
            progress(send, format!("{phase} · {count} notes"))?;
            last = std::time::Instant::now();
        }
        Ok(())
    })
    .with_context(|| {
        format!(
            "Build external search generation {}",
            display_path(&owned.0)
        )
    })?;
    built
        .finish_build()
        .context("Finish search indexing and merges")?;
    cancel.check()?;
    let marker = owned.0.join("complete");
    std::fs::write(&marker, b"1")
        .with_context(|| format!("Write search completion marker {}", display_path(&marker)))?;
    if in_place {
        // Publication transfers cleanup ownership before opening: another Reader
        // can already use the completed generation even if our open fails.
        owned.1 = false;
        return Searcher::open(&owned.0).with_context(|| {
            format!(
                "Open published search generation {}",
                display_path(&owned.0)
            )
        });
    }
    let generations = destination.parent().unwrap();
    std::fs::create_dir_all(generations).with_context(|| {
        format!(
            "Create search generations directory {}",
            display_path(generations)
        )
    })?;
    match std::fs::rename(&owned.0, destination) {
        Ok(()) => Searcher::open(destination).with_context(|| {
            format!(
                "Open published search generation {}",
                display_path(destination)
            )
        }),
        Err(publish_error) => {
            if let Some(searcher) = open_completed_unpinned(destination) {
                return Ok(searcher);
            }
            // Never delete a corrupt/concurrently published generation in use.
            let repairs = destination.with_extension("repairs");
            let repair = (|| -> Result<Searcher> {
                std::fs::create_dir_all(&repairs).with_context(|| {
                    format!("Create search repair directory {}", display_path(&repairs))
                })?;
                let repaired = repairs.join(uuid::Uuid::new_v4().to_string());
                cancel.check()?;
                std::fs::rename(&owned.0, &repaired).with_context(|| {
                    format!(
                        "Move search staging {} to {}",
                        display_path(&owned.0),
                        display_path(&repaired)
                    )
                })?;
                Searcher::open(&repaired).with_context(|| {
                    format!(
                        "Open repaired search generation {}",
                        display_path(&repaired)
                    )
                })
            })();
            repair.with_context(|| {
                format!(
                    "Publish search generation {} to {} failed: {publish_error}",
                    display_path(&owned.0),
                    display_path(destination)
                )
            })
        }
    }
}

fn pin_generation(destination: &Path) -> Result<reader_cache::GenerationPin> {
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .context("Invalid search generation path")?;
    let base = destination
        .parent()
        .and_then(Path::parent)
        .context("Search generation has no cache directory")?;
    reader_cache::GenerationPin::acquire(base, name)
}

/// Open a completed generation pinned for the searcher's lifetime. Without a
/// pin the generation is not used: collection could retire it under us.
fn open_completed_generation(destination: &Path) -> Option<Searcher> {
    let pin = pin_generation(destination).ok()?;
    open_completed_unpinned(destination).map(|searcher| searcher.pinned(pin))
}

/// Callers must already hold a pin on the generation family.
fn open_completed_unpinned(destination: &Path) -> Option<Searcher> {
    let open = |path: &Path| {
        path.join("complete")
            .is_file()
            .then(|| Searcher::open(path).ok())
            .flatten()
    };
    open(destination).or_else(|| {
        std::fs::read_dir(destination.with_extension("repairs"))
            .ok()?
            .filter_map(Result::ok)
            .find_map(|entry| open(&entry.path()))
    })
}

fn progress(send: &async_channel::Sender<Event>, phase: String) -> Result<()> {
    send.send_blocking(Event::Progress(phase))
        .map_err(|_| anyhow::anyhow!("Reader closed"))
}

struct Staging(PathBuf, bool);
impl Drop for Staging {
    fn drop(&mut self) {
        if self.1 {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

pub(super) fn validate_external_cache(base: &Path, root: &Path) -> Result<()> {
    if base
        .components()
        .any(|component| component == std::path::Component::ParentDir)
    {
        bail!("Search data directory must not contain parent traversal");
    }
    let absolute = if base.is_absolute() {
        base.to_path_buf()
    } else {
        std::env::current_dir()?.join(base)
    };
    if absolute.starts_with(root) {
        bail!("Search data must be stored outside the document folder");
    }
    let existing = existing_cache_ancestor(&absolute, |path| std::fs::metadata(path).map(|_| ()))?;
    let resolved = existing
        .canonicalize()
        .with_context(|| {
            format!(
                "Resolve search cache ancestor {}",
                tessera_core::vault::display_path(existing)
            )
        })?
        .join(absolute.strip_prefix(existing)?);
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if absolute.starts_with(root) || resolved.starts_with(canonical_root) {
        bail!("Search data must be stored outside the document folder");
    }
    Ok(())
}

fn existing_cache_ancestor(
    absolute: &Path,
    mut metadata: impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<&Path> {
    for candidate in absolute.ancestors() {
        match metadata(candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "Read search cache ancestor metadata {}",
                        tessera_core::vault::display_path(candidate)
                    )
                })
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "No accessible cache ancestor",
    ))
    .with_context(|| {
        format!(
            "Locate search cache ancestor for {}",
            tessera_core::vault::display_path(absolute)
        )
    })
}

pub(crate) struct PendingDocument {
    intent: reader_open::OpenIntent,
    vault: Vault,
    searcher: Option<Box<Searcher>>,
    rel: String,
    document: prepared_links::PreparedDocument,
    content: Entity<TextViewState>,
    published: Option<async_channel::Sender<()>>,
    _observer: Subscription,
    languages_prepared: bool,
    language_task: Option<Task<()>>,
}

impl Reader {
    #[allow(clippy::too_many_arguments)] // One atomic document/inventory publication.
    fn stage_first_document(
        &mut self,
        intent: reader_open::OpenIntent,
        vault: Vault,
        document: Option<(String, prepared_links::PreparedDocument)>,
        searcher: Option<Box<Searcher>>,
        published: Option<async_channel::Sender<()>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((rel, document)) = document else {
            // A terminal candidate result is not a publication. Keep every
            // field of the previously usable Reader, including its watcher.
            if let Some(load) = &mut self.loading {
                load.phase = "This folder contains no readable Markdown documents.".into();
                load.active = false;
                load.opts.cache_lease = None;
                load.cancellation.cancel();
            }
            drop(published);
            cx.notify();
            return;
        };
        let configured = reader_plugins(
            self.vault_root.clone(),
            TextView::markdown("prepared-reader-configuration", ""),
            cx.entity().downgrade(),
            self.sel_format,
            Arc::default(),
            &self.link_identities,
        );
        let _phase = self.loading.as_ref().and_then(|load| {
            load.opts
                .diagnostics
                .as_ref()
                .map(|trace| trace.phase("text_state_stage"))
        });
        let content = cx.new(|cx| {
            let mut state = if self.use_html {
                TextViewState::html("", cx)
            } else {
                TextViewState::markdown("", cx)
            }
            .scrollable(true)
            .selectable(true)
            .selection_format(self.sel_format)
            .retain_selection_on_layout(true);
            configured.prepare_state(&mut state, cx);
            state.set_text_with_source(
                &document.source,
                document.original.clone().map(SharedString::from),
                cx,
            );
            state
        });
        let observer = cx.observe_in(&content, window, |this, _, window, cx| {
            this.publish_pending_document(window, cx)
        });
        self.pending_open_document = Some(PendingDocument {
            intent,
            vault,
            searcher,
            rel,
            document,
            content,
            published,
            _observer: observer,
            languages_prepared: false,
            language_task: None,
        });
        self.publish_pending_document(window, cx);
    }

    fn publish_pending_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(status) = self
            .pending_open_document
            .as_ref()
            .and_then(|pending| pending.content.read(cx).preparation_status())
        else {
            return;
        };
        if status.is_ok() {
            let pending = self.pending_open_document.as_mut().unwrap();
            if !pending.languages_prepared {
                if pending.language_task.is_none() {
                    let content = pending.content.clone();
                    let work = content
                        .read(cx)
                        .prepare_code_languages(reader_code_language::resolver(), cx);
                    pending.language_task = Some(cx.spawn_in(window, async move |this, cx| {
                        work.await;
                        let _ = this.update_in(cx, |this, window, cx| {
                            let Some(pending) = this.pending_open_document.as_mut() else {
                                return;
                            };
                            if pending.content != content {
                                return;
                            }
                            pending.languages_prepared = true;
                            this.publish_pending_document(window, cx);
                        });
                    }));
                }
                return;
            }
        }
        let Some(pending) = self.pending_open_document.take() else {
            return;
        };
        if let Err(error) = status {
            if let Some(load) = &mut self.loading {
                load.phase = error;
                load.active = false;
                load.cancellation.cancel();
            }
            cx.notify();
            return;
        }
        self.document_preparation_generation = self.document_preparation_generation.wrapping_add(1);
        #[cfg(unix)]
        {
            self.creation = None;
        }
        // Upgrading the displayed quick-view folder changes its capabilities,
        // not its document. A cold candidate has no attachment inventory yet;
        // replacing the render would drop images and reset the viewport (#593).
        // Ready reconciles any source edits against the complete inventory.
        let preserve_document = self.single_file
            && !pending.intent.single_file
            && self.vault_root == pending.intent.root
            && self.current_rel == pending.rel
            && self.document_ready();
        self.vault_root = pending.intent.root.clone();
        let opts = &self.loading.as_ref().unwrap().opts;
        self.single_file = pending.intent.single_file;
        self.index_dir = if self.single_file {
            None
        } else {
            reader_open::cache_path_for(&self.vault_root, opts).ok()
        };
        self.cache_lease = opts.cache_lease.clone();
        // No predecessor-root capability survives publication. Eligibility is
        // installed only by the matching background preference result.
        self.panel_settings = None;
        self.panel_preferences_ready = false;
        self.resizing_panel = None;
        self.vault = Arc::new(pending.vault);
        self.quick_open.invalidate();
        self.quick_open.recent.clear();
        let warm = self.vault.inventory_scanned;
        self.quick_open.inventory = warm.then(|| {
            Arc::new(tessera_core::quick_open::inventory(
                self.vault.notes.clone(),
                &self.vault.entries,
            ))
        });
        self.searcher = pending.searcher.map(|searcher| Arc::new(*searcher));
        self.loading.as_mut().unwrap().warm = warm;
        self.loading.as_mut().unwrap().show_progress = !warm;
        if warm {
            // Late Inbox rows must not re-center an already usable warm tree.
            self.inbox_ready_root = Some(self.vault_root.clone());
            self.loading.as_mut().unwrap().phase = "Checking search data".into();
            self.delay_warm_progress(cx);
        }
        #[cfg(unix)]
        {
            self.move_index = None;
        }
        self.watcher = None;
        self.watcher_generation = self.watcher_generation.wrapping_add(1);
        self.deferred_vault_changes = Default::default();
        if !preserve_document {
            self.history.clear();
            self.history_positions.clear();
            self.history_ix = 0;
        }
        self.loading.as_mut().unwrap().published = true;
        reader_open::register(cx.entity().downgrade(), pending.intent.root, cx);
        if preserve_document {
            self.link_notice = None;
            self.last_recorded_document = None;
            self.record_usable_document(cx);
        } else {
            self.accept_prepared_document_using(
                prepared_links::DocumentRequest {
                    rel: pending.rel,
                    jump: None,
                    heading: None,
                    history_index: None,
                    restore_position: None,
                },
                Ok(pending.document),
                Some(pending.content),
                window,
                cx,
            );
        }
        self.restore_ui_state(window, cx);
        self.refresh_quick_open(cx);
        if let Some(trace) = &self.loading.as_ref().unwrap().opts.diagnostics {
            trace.event("document_published", serde_json::json!({ "notes": self.vault.notes.len(), "warm": warm, "unreadable": self.vault.unreadable.len() }));
        }
        if let Some(published) = pending.published {
            let _ = published.try_send(());
        }
    }

    pub(super) fn reconcile_inventory_document(
        &mut self,
        sources: &std::collections::HashMap<String, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // An explicitly closed selection has no document to reconcile.
        if self.current_rel.is_empty() {
            return;
        }
        if self.editing.is_some() {
            self.refresh_source_from_disk(window, cx);
            self.refresh_link_preparation(cx);
            return;
        }
        let Some(raw) = sources.get(&self.current_rel) else {
            if self.document_ready() {
                self.link_notice = Some("The displayed document is no longer available; showing its last readable contents.".into());
            }
            self.refresh_link_preparation(cx);
            return;
        };
        let pending_embed = self.note_source.contains(&format!(
            "{EMBED_LANG} {} ",
            tessera_core::render::EMBED_PENDING
        ));
        if !pending_embed
            && self.link_original_source.as_deref()
                == Some(tessera_core::render::without_frontmatter(raw))
        {
            self.refresh_link_preparation(cx);
            return;
        }
        let vault = self.vault.clone();
        let rel = self.current_rel.clone();
        let root = self.vault_root.clone();
        let navigation = self.navigation_generation;
        let generation = self.document_preparation_generation;
        let html = self.use_html;
        cx.spawn_in(window, async move |this, cx| {
            let target = rel.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    if html {
                        tessera_core::render_html(&vault, &target, "InspiredGitHub").map(|source| {
                            prepared_links::PreparedDocument {
                                source,
                                original: None,
                                identities: Vec::new(),
                                frontmatter: None,
                            }
                        })
                    } else {
                        tessera_core::render::reader_document(&vault, &target).map(|doc| {
                            prepared_links::PreparedDocument {
                                source: doc.rendered,
                                original: Some(doc.original_body),
                                identities: doc.links,
                                frontmatter: doc.frontmatter,
                            }
                        })
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.vault_root != root
                    || this.current_rel != rel
                    || this.navigation_generation != navigation
                    || this.document_preparation_generation != generation
                {
                    return;
                }
                if result.as_ref().is_ok_and(|document| {
                    document.source == this.note_source
                        && document.original == this.link_original_source
                }) {
                    this.refresh_link_preparation(cx);
                    return;
                }
                // Sample the position after preparation, so intervening scrolling
                // cannot be overwritten with the earlier reconciliation position.
                // A consumed session snapshot may still await its first layout
                // landing. Carry that intent through this same-document derived
                // replacement instead of substituting the not-yet-landed top.
                let position = this
                    .pending_landing
                    .unwrap_or_else(|| this.content.read(cx).list_state().logical_scroll_top());
                let request = prepared_links::DocumentRequest {
                    rel,
                    jump: None,
                    heading: None,
                    history_index: Some(this.history_ix),
                    restore_position: Some(position),
                };
                this.accept_prepared_document(request, result, window, cx);
            });
        })
        .detach();
    }

    fn delay_warm_progress(&self, cx: &mut Context<Self>) {
        let generation = self.loading.as_ref().unwrap().generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(load) = this
                    .loading
                    .as_mut()
                    .filter(|load| load.active && load.warm && load.generation == generation)
                {
                    load.show_progress = true;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(crate) fn start_session_records(&mut self, cx: &mut Context<Self>) {
        let Some(directory) = self.session_directory.clone() else {
            return;
        };
        let (send, receive) = async_channel::unbounded::<SessionRecord>();
        self.session_records = Some(send);
        cx.spawn(async move |this, cx| {
            while let Ok(SessionRecord {
                root,
                document,
                single_file,
                cache,
                cache_lease,
                diagnostics,
            }) = receive.recv().await
            {
                let directory = directory.clone();
                let requested_root = root.clone();
                let requested_document = document.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        let _cache_lease = cache_lease;
                        let result = crate::reader_history::ReadingHistory::record_document_mode(
                            &directory, &root, &document, single_file,
                        );
                        if !document.is_empty() {
                            if let Some(base) = cache {
                                let started = std::time::Instant::now();
                                let cached = validate_external_cache(&base, &root).and_then(|_| {
                                    tessera_core::vault::warm::remember_primary(
                                        &base, &root, &document,
                                    )
                                });
                                if let Some(trace) = &diagnostics {
                                    trace.event("last_document_cache", serde_json::json!({
                                        "path": document, "duration_ms": started.elapsed().as_secs_f64() * 1000.,
                                        "saved": cached.is_ok(), "error": cached.as_ref().err().map(|e| format!("{e:#}")),
                                    }));
                                }
                                if let Err(error) = cached {
                                    eprintln!("Cannot cache last Reader document: {error:#}");
                                }
                            }
                        }
                        result
                    })
                    .await;
                if let Err(error) = result {
                    let _ = this.update(cx, |this, cx| {
                        if this.vault_root == requested_root
                            && this.current_rel == requested_document
                        {
                            this.link_notice =
                                Some(format!("Reading history could not be saved: {error:#}").into());
                            cx.notify();
                        }
                    });
                }
            }
        })
        .detach();
    }

    pub(crate) fn record_usable_document(&mut self, cx: &Context<Self>) {
        if !self.document_ready()
            || !matches!(self.content.read(cx).preparation_status(), Some(Ok(())))
        {
            return;
        }
        let identity = (
            self.vault_root.clone(),
            self.current_rel.clone(),
            self.navigation_generation,
        );
        if self.last_recorded_document.as_ref() == Some(&identity) {
            return;
        }
        if let Some(send) = &self.session_records {
            // A pending candidate's options do not own the displayed document.
            let cache = if self.single_file {
                None
            } else {
                self.index_dir
                    .clone()
                    .or_else(|| reader_open::cache_candidate_path(&identity.0).ok())
            };
            if send
                .try_send(SessionRecord {
                    root: identity.0.clone(),
                    document: identity.1.clone(),
                    single_file: self.single_file,
                    cache,
                    cache_lease: self.cache_lease.clone(),
                    diagnostics: self
                        .loading
                        .as_ref()
                        .and_then(|load| load.opts.diagnostics.clone()),
                })
                .is_ok()
            {
                self.last_recorded_document = Some(identity);
            }
        }
    }

    pub(crate) fn document_ready(&self) -> bool {
        self.usable_document
    }

    pub(crate) fn cancel_loading(&mut self, cx: &mut Context<Self>) {
        if let Some(load) = self.loading.as_mut().filter(|l| l.active) {
            load.cancellation.cancel();
            self.pending_open_document = None;
            load.active = false;
            load.phase = "Preparation cancelled".into();
            load.opts.cache_lease = None;
            self.document_preparation_generation =
                self.document_preparation_generation.wrapping_add(1);
            cx.notify();
        }
    }

    pub(crate) fn start_loading(
        &mut self,
        opts: Opts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.leave_source(cx) {
            return;
        }
        self.file_preview = None;
        self.file_menu = None;
        self.start_preparation(opts, None, window, cx);
    }

    pub(crate) fn refresh_inventory(
        &mut self,
        changes: tessera_core::Changes,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.refresh_inventory_with_read(changes, false, window, cx);
    }

    fn refresh_inventory_with_read(
        &mut self,
        changes: tessera_core::Changes,
        force_source_read: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.single_file {
            self.load_quick_folder(String::new(), cx);
            self.refresh_quick_document(window, cx);
            return;
        }
        self.invalidate_links();
        let opts = Opts {
            force_source_read,
            reconcile_recent: self.quick_open.recent.clone(),
            vault: Some(self.vault_root.clone()),
            note: Some(self.current_rel.clone()),
            diagnostics: self
                .loading
                .as_ref()
                .and_then(|load| load.opts.diagnostics.clone()),
            index_dir: self.index_dir.clone(),
            cache_lease: self.cache_lease.clone(),
            use_html: self.use_html,
            ..Default::default()
        };
        self.start_preparation(opts, Some(changes), window, cx);
    }

    fn start_preparation(
        &mut self,
        mut opts: Opts,
        refresh: Option<tessera_core::Changes>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if opts.session_directory.is_none() {
            opts.session_directory = self.session_directory.clone();
        }
        if opts.reconcile_recent.is_empty() {
            opts.reconcile_recent = self.quick_open.recent.clone();
        }
        if let Some(cancel) = self.incremental_cancel.take() {
            cancel.cancel();
        }
        self.incremental_epoch = self.incremental_epoch.wrapping_add(1);
        self.incremental_state = None;
        self.tasks_index = None;
        self.incremental_initializing = false;
        self.incremental_active = false;
        #[cfg(unix)]
        {
            self.move_index = None;
        }
        // Supersession is not terminal session failure: the replacement still
        // owns the intent to publish. Explicit Cancel uses cancel_loading.
        if let Some(load) = &self.loading {
            load.cancellation.cancel();
        }
        self.pending_open_document = None;
        let generation = self
            .loading
            .as_ref()
            .map_or(1, |l| l.generation.wrapping_add(1));
        let cancel = Cancellation::default();
        self.loading = Some(Loading {
            generation,
            cancellation: cancel.clone(),
            phase: "Opening document".into(),
            active: true,
            published: refresh.is_some() && self.document_ready(),
            warm: refresh.is_some() && self.vault.inventory_scanned,
            show_progress: !(refresh.is_some() && self.vault.inventory_scanned),
            opts: opts.clone(),
            warnings: Vec::new(),
        });
        if self.loading.as_ref().unwrap().warm {
            self.loading.as_mut().unwrap().phase = "Checking search data".into();
            self.delay_warm_progress(cx);
        }
        self.document_preparation_generation = self.document_preparation_generation.wrapping_add(1);
        if refresh.is_none() {
            self.cancel_pending_landing();
        }
        let (send, receive) = async_channel::unbounded();
        let refresh_worker = refresh.is_some();
        let prepare_preferences = !refresh_worker || !self.panel_preferences_ready;
        let panel_revision = self.panel_widths_revision;
        let recovery_startup = self.recovery_startup;
        let ui_notes = reader_ui_state::notes(cx);
        cx.background_executor()
            .spawn(async move {
                let mut opts = opts;
                let result = async {
                    #[cfg(test)]
                    if let Some(hold) = &opts.validation_hold {
                        hold.recv()
                            .await
                            .map_err(|_| anyhow::anyhow!("Test hold closed"))?;
                        cancel.check()?;
                    }
                    let (root, previous) = if refresh_worker {
                        (opts.vault.clone().expect("refresh root"), None)
                    } else {
                        if let Some(trace) = &opts.diagnostics {
                            trace.event("first_worker_start", serde_json::json!({}));
                        }
                        let mut first = prepare_first_with_ui_notes(&opts, &cancel, &ui_notes)?;
                        if let Event::First { document, .. } = &first {
                            opts.note = document.as_ref().map(|(path, _)| path.clone());
                        }
                        let Event::First {
                            ref intent,
                            ref cache_lease,
                            ref mut published,
                            ref mut snapshot,
                            ..
                        } = first
                        else {
                            unreachable!()
                        };
                        opts.single_file = intent.single_file;
                        let root = intent.root.clone();
                        opts.cache_lease = cache_lease.clone();
                        if let Some(trace) = &opts.diagnostics {
                            opts.diagnostics = Some(trace.for_root(root.clone()));
                        }
                        let previous = snapshot.take().map(|snapshot| *snapshot);
                        let (ack, accepted) = async_channel::bounded(1);
                        *published = Some(ack);
                        send.send_blocking(first)
                            .map_err(|_| anyhow::anyhow!("Reader closed"))?;
                        accepted
                            .recv()
                            .await
                            .map_err(|_| anyhow::anyhow!("Reader closed before publication"))?;
                        if let Some(lease) = &opts.cache_lease {
                            let result = lease.mark_published();
                            if let Some(trace) = &opts.diagnostics {
                                trace.event("vault_cache_published", serde_json::json!({"saved":result.is_ok(), "error":result.as_ref().err().map(|e| format!("{e:#}"))}));
                            }
                        }
                        (root, previous)
                    };
                    cancel.check()?;
                    if prepare_preferences {
                        // First usable publication never waits for preference I/O.
                        // This is the existing worker, not a second scan/job.
                        #[cfg(test)]
                        if let Some(hold) = &opts.panel_preferences_hold {
                            let _ = hold.recv().await;
                        }
                        cancel.check()?;
                        #[cfg(test)]
                        let path = match &opts.panel_settings_override {
                            Some(path) => reader_layout::settings_path_at(&root, path.clone()),
                            None => reader_layout::settings_path(&root),
                        };
                        #[cfg(not(test))]
                        let path = reader_layout::settings_path(&root);
                        let widths = path
                            .as_ref()
                            .filter(|_| !recovery_startup)
                            .map(|path| reader_layout::Widths::load(path))
                            .unwrap_or_default();
                        cancel.check()?;
                        send.send_blocking(Event::PanelPreferences {
                            root: root.clone(),
                            path,
                            widths,
                            revision: panel_revision,
                        })
                        .map_err(|_| anyhow::anyhow!("Reader closed"))?;
                    }
                    if opts.single_file {
                        let vault = quick_folder(&root, "", &[], &cancel)?;
                        send.send_blocking(Event::Siblings(vault)).map_err(|_| anyhow::anyhow!("Reader closed"))?;
                        return Ok(());
                    }
                    #[cfg(test)]
                    if let Some(hold) = &opts.preparation_hold {
                        hold.recv()
                            .await
                            .map_err(|_| anyhow::anyhow!("Test hold closed"))?;
                        cancel.check()?;
                    }
                    let ready = prepare_rest_with_snapshot(&root, &opts, &cancel, &send, previous)
                        .with_context(|| {
                            format!("Prepare vault {}", tessera_core::vault::display_path(&root))
                        })
                        .inspect_err(|error| {
                            if cancel.check().is_ok() {
                                super::reader_diagnostics::record_failure(
                                    &root,
                                    opts.session_directory.as_deref(),
                                    error,
                                );
                            }
                        })?;
                    cancel.check()?;
                    if let Some(trace) = &opts.diagnostics {
                        trace.event("ready_worker_send", serde_json::json!({}));
                    }
                    send.send_blocking(ready)
                        .map_err(|_| anyhow::anyhow!("Reader closed"))?;
                    collect_search_generations(&root, &opts);
                    if !refresh_worker {
                        if let Some(lease) = &opts.cache_lease {
                            let started = std::time::Instant::now();
                            let result = lease.prune();
                            if let Some(trace) = &opts.diagnostics {
                                trace.event(
                                    "cache_retention",
                                    serde_json::json!({
                                        "duration_ms":started.elapsed().as_secs_f64()*1000.,
                                        "evicted":result.as_ref().ok().map(|report| report.removed.len()),
                                        "cleanup_errors":result.as_ref().ok().map(|report| &report.cleanup_errors),
                                        "error":result.as_ref().err().map(|e| format!("{e:#}")),
                                    }),
                                );
                            }
                        }
                    }
                    Ok::<_, anyhow::Error>(())
                }
                .await;
                if let Err(error) = result {
                    if cancel.check().is_ok() {
                        let _ = send.send_blocking(Event::Failed(
                            tessera_core::vault::display_error(&format!("{error:#}")),
                        ));
                    }
                }
            })
            .detach();
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = receive.recv().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        if !this
                            .loading
                            .as_ref()
                            .is_some_and(|l| l.active && l.generation == generation)
                        {
                            return;
                        }
                        if let Event::First { intent, .. } = &event {
                            let load = this.loading.as_mut().unwrap();
                            if let Some(trace) = &load.opts.diagnostics {
                                load.opts.diagnostics = Some(trace.for_root(intent.root.clone()));
                            }
                        }
                        let event_phase = match &event {
                            Event::First { .. } => Some("first_ui_stage"),
                            Event::SearchInventory { .. } => Some("inventory_ui_publish"),
                            Event::Ready { .. } => Some("ready_ui_publish"),
                            _ => None,
                        };
                        if matches!(&event, Event::Ready { .. }) {
                            if let Some(trace) = &this.loading.as_ref().unwrap().opts.diagnostics {
                                trace.event("ready_event_received", serde_json::json!({}));
                            }
                        }
                        let _ui_phase = event_phase.and_then(|name| this.loading.as_ref()
                            .and_then(|load| load.opts.diagnostics.as_ref()).map(|trace| trace.phase(name)));
                        match event {
                            Event::First {
                                intent,
                                cache_lease,
                                vault,
                                document,
                                searcher,
                                snapshot: _,
                                published,
                                recovery_notice,
                            } => {
                                if let Some(trace) = &this.loading.as_ref().unwrap().opts.diagnostics { trace.event("first_event_received", serde_json::json!({})); }
                                this.loading.as_mut().unwrap().opts.cache_lease = cache_lease;
                                this.stage_first_document(
                                    intent, vault, document, searcher, published, window, cx,
                                );
                                if let Some(notice) = recovery_notice {
                                    reader_toast::error(notice, window, cx);
                                }
                            }
                            Event::Siblings(vault) => {
                                this.publish_quick_folder(vault, cx);
                                this.refresh_quick_document(window, cx);
                                this.loading.as_mut().unwrap().active = false;
                                this.loading.as_mut().unwrap().phase = "Ready".into();
                            }
                            Event::Progress(phase) => {
                                let load = this.loading.as_mut().unwrap();
                                if !load.warm {
                                    load.phase = phase;
                                }
                            }
                            Event::PanelPreferences {
                                root,
                                path,
                                widths,
                                revision,
                            } => {
                                if this.vault_root != root
                                    || !this.loading.as_ref().unwrap().published
                                {
                                    return;
                                }
                                this.panel_settings = path;
                                this.panel_preferences_ready = true;
                                if this.panel_widths_revision == revision
                                    && this.resizing_panel.is_none()
                                {
                                    this.panel_widths = widths;
                                }
                            }
                            Event::SearchInventory { notes, reconciled } => {
                                if let Some(vault) = reconciled {
                                    this.vault = Arc::new(*vault);
                                    this.backlinks = this.vault.backlinks(&this.current_rel);
                                }
                                this.quick_open.inventory = Some(Arc::new(notes));
                                this.refresh_quick_open(cx);
                            }
                            Event::Ready {
                                move_snapshot,
                                vault,
                                searcher,
                                watcher,
                                warnings,
                                index,
                                sources,
                                titles,
                            } => {
                                this.incremental_initializing = true;
                                let index_root = this.vault_root.clone();
                                let generation = this.loading.as_ref().map(|load| load.generation);
                                let epoch = this.incremental_epoch;
                                let ready_vault = Arc::new(vault);
                                let state_vault = ready_vault.clone();
                                #[cfg(unix)]
                                { this.move_index = None; }
                                let index_task = cx.background_executor().spawn(async move {
                                    let tasks = reader_tasks::from_snapshot(&move_snapshot);
                                    let state = tessera_core::vault::warm::incremental::State::new((*state_vault).clone(), *move_snapshot);
                                    #[cfg(unix)]
                                    let candidates = state.candidates.clone();
                                    (state, tasks, { #[cfg(unix)] { Some(candidates) } #[cfg(not(unix))] { None::<()> } })
                                });
                                cx.spawn(async move |this, cx| {
                                    let (state, tasks, candidates) = index_task.await;
                                    let _ = this.update(cx, |this, cx| {
                                        if this.vault_root == index_root && this.incremental_epoch == epoch
                                            && this.loading.as_ref().map(|load| load.generation) == generation {
                                            this.tasks_index = Some(tasks);
                                            this.incremental_state = Some(state);
                                            this.incremental_initializing = false;
                                            #[cfg(unix)]
                                            { this.move_index = candidates; }
                                            #[cfg(not(unix))]
                                            let _ = candidates;
                                            cx.notify();
                                        }
                                    });
                                }).detach();
                                this.vault = ready_vault;
                                this.searcher = searcher.map(|searcher| Arc::new(*searcher));
                                this.watcher = watcher;
                                this.watcher_generation = this.watcher_generation.wrapping_add(1);
                                this.index_dir = index;
                                this.backlinks = this.vault.backlinks(&this.current_rel);
                                this.sync_tree();
                                this.backlink_titles = Arc::new(titles);
                                this.reconcile_inventory_document(&sources, window, cx);
                                this.loading.as_mut().unwrap().active = false;
                                this.loading.as_mut().unwrap().warnings = warnings;
                                this.refresh_quick_open(cx);
                                this.loading.as_mut().unwrap().phase = "Ready".into();
                                if let Some(trace) = &this.loading.as_ref().unwrap().opts.diagnostics { trace.event("vault_ready", serde_json::json!({ "notes": this.vault.notes.len(), "unreadable": this.vault.unreadable.len() })); }
                            }
                            Event::Failed(error) => {
                                this.loading.as_mut().unwrap().active = false;
                                this.loading.as_mut().unwrap().phase = error;
                                this.loading.as_mut().unwrap().opts.cache_lease = None;
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn render_loading(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // docs/design/reader.md §Status text: idle states say nothing; only
        // active loading and failures are visible.
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let issue_count = self.vault.unreadable.len()
            + self.loading.as_ref().map_or(0, |load| load.warnings.len());
        h_flex()
            .id("reader-loading")
            .flex_shrink(1.)
            .min_w_0()
            .max_w(px(420.))
            .gap_2()
            .text_xs()
            .when(issue_count > 0, |view| {
                view.child(
                    Button::new("reader-unreadable-items")
                        .ghost()
                        .small()
                            .label(if issue_count == 1 {
                                "1 item unreadable".to_string()
                            } else {
                                format!("{issue_count} items unreadable")
                            })
                            .tooltip("Show items needing attention")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.show_unreadable_items(window, cx);
                        })),
                )
            })
            .when(self.loading.as_ref().is_some_and(|load| !load.active &&
                load.warnings.iter().any(|warning| warning.operation.starts_with("watch vault"))), |view| {
                view.child(div().text_color(muted).child("Auto-refresh limited"))
                    .child(Button::new("rescan-network-vault").small().label("Rescan")
                    .tooltip("Automatic refresh is unavailable here. Use Rescan to check changes made by other clients.")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.refresh_inventory_with_read(tessera_core::Changes::default(), true, window, cx);
                    })))
            })
            .when_some(
                self.loading.as_ref().filter(|load| {
                    if load.active {
                        load.show_progress
                    } else {
                        load.phase != "Ready"
                    }
                }),
                |view, load| {
                    view.when(load.active && !load.warm, |view| {
                        view.child(
                            gpui_component::spinner::Spinner::new()
                                .xsmall()
                                .color(muted),
                        )
                    })
                    .child({
                        // Error chains can be long; keep Retry and the header
                        // controls reachable and show the full text on hover.
                        let failed = !load.active && !load.phase.contains("cancelled");
                        let phase = load.phase.clone();
                        div()
                            .id("reader-loading-phase")
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(if failed { danger } else { muted })
                            .child(load.phase.clone())
                            .tooltip(move |window, cx| {
                                gpui_component::tooltip::Tooltip::new(phase.clone())
                                    .build(window, cx)
                            })
                    })
                    .when(load.active && !load.warm, |view| {
                        view.child(
                            Button::new("cancel-reader-loading")
                                .label("Cancel")
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| this.cancel_loading(cx))),
                        )
                    })
                    .when(!load.active && load.phase != "Ready", |view| {
                        view.child(
                            Button::new("retry-reader-loading")
                                .label("Retry")
                                .small()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    if let Some(opts) =
                                        this.loading.as_ref().map(|l| l.opts.clone())
                                    {
                                        if this.loading.as_ref().is_some_and(|load| load.published)
                                        {
                                            this.refresh_inventory(
                                                tessera_core::Changes::default(),
                                                window,
                                                cx,
                                            );
                                        } else {
                                            this.start_loading(opts, window, cx);
                                        }
                                    }
                                })),
                        )
                    })
                },
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("tessera-review-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[gpui::test]
    fn cache_is_bound_to_new_vault_before_background_ready(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = TestDirectory::new();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        for root in [&a, &b] {
            std::fs::create_dir(root).unwrap();
            std::fs::write(root.join("start.md"), "# Start").unwrap();
        }
        let a = a.canonicalize().unwrap();
        let b = b.canonicalize().unwrap();
        let opts_for = |root: &Path| Opts {
            vault: Some(root.to_owned()),
            cache_base_override: Some(temp.path().join("os-cache")),
            session_directory: Some(temp.path().join("state")),
            ..Default::default()
        };
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let v = cx.new(|cx| Reader::new(opts_for(&a), window, cx));
            reader = Some(v.clone());
            Root::new(v, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        let old_lease = reader.read_with(visual, |v, _| {
            Arc::downgrade(v.cache_lease.as_ref().unwrap())
        });
        assert!(
            old_lease.upgrade().is_some(),
            "positive control: published old-root lease"
        );
        let old_cache = reader.read_with(visual, |v, _| v.index_dir.clone().unwrap());
        let (release, hold) = async_channel::bounded(1);
        let mut opts = opts_for(&b);
        opts.preparation_hold = Some(hold);
        let new_cache = reader_open::cache_path_for(&b, &opts).unwrap();
        reader.update_in(visual, |v, window, cx| v.start_loading(opts, window, cx));
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert!(v.document_ready() && v.loading.as_ref().unwrap().active);
            assert_eq!(v.vault_root, b);
            let lease = v.cache_lease.as_ref().unwrap();
            assert_eq!(lease.root, b);
            assert_eq!(lease.path, new_cache);
            assert!(
                old_lease.upgrade().is_none(),
                "new publication releases predecessor lease"
            );
            assert_ne!(
                old_cache, new_cache,
                "positive control: vault-scoped cache paths"
            );
            assert_eq!(
                v.index_dir.as_ref(),
                Some(&new_cache),
                "Retry/Rescan must not reuse the previous vault's cache before Ready"
            );
        });
        release.try_send(()).unwrap();
        visual.run_until_parked();
        let published_lease = reader.read_with(visual, |v, _| {
            Arc::downgrade(v.cache_lease.as_ref().unwrap())
        });
        let empty = temp.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        let empty = empty.canonicalize().unwrap();
        let opts = opts_for(&empty);
        let unused_cache = reader_open::cache_path_for(&empty, &opts).unwrap();
        reader.update_in(visual, |v, window, cx| v.start_loading(opts, window, cx));
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert_eq!(v.vault_root, b);
            assert_eq!(v.index_dir.as_ref(), Some(&new_cache));
            assert!(published_lease.upgrade().is_some());
            assert!(v.loading.as_ref().unwrap().opts.cache_lease.is_none());
        });
        assert!(
            !unused_cache.exists(),
            "failed candidate creates no empty recent cache"
        );
    }

    #[test]
    fn quick_file_open_discards_a_foreign_candidate_lease() {
        let temp = TestDirectory::new();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        for root in [&a, &b] {
            std::fs::create_dir(root).unwrap();
            std::fs::write(root.join("start.md"), "# Start").unwrap();
        }
        let a = a.canonicalize().unwrap();
        let b = b.canonicalize().unwrap();
        let mut opts = Opts {
            open_path: Some(b.join("start.md")),
            cache_base_override: Some(temp.path().join("os-cache")),
            ..Default::default()
        };
        let old_path = reader_open::cache_path_for(&a, &opts).unwrap();
        opts.cache_lease = Some(Arc::new(
            reader_cache::Lease::acquire(old_path.clone(), &a).unwrap(),
        ));
        opts.cache_lease.as_ref().unwrap().mark_published().unwrap();
        assert!(old_path.exists(), "published vault cache positive control");
        let quick_cache = reader_open::cache_path_for(&b, &opts).unwrap();
        let Event::First {
            intent,
            cache_lease,
            ..
        } = prepare_first_with_last_document(&opts, &Cancellation::default(), |_| Ok(None))
            .unwrap()
        else {
            unreachable!()
        };
        assert!(cache_lease.is_none());
        assert!(intent.single_file);
        assert_eq!(intent.root, b);
        assert!(old_path.exists(), "existing published cache remains intact");
        assert!(!quick_cache.exists(), "quick open never creates its cache");
    }

    #[gpui::test]
    fn switching_vaults_reuses_each_tree_last_document_and_search_before_reconcile(
        cx: &mut TestAppContext,
    ) {
        warm_switch_fixture(cx, 16);
    }

    #[gpui::test]
    #[ignore = "same-host 5001-note warm-switch profile"]
    fn five_thousand_note_warm_switch_profile(cx: &mut TestAppContext) {
        warm_switch_fixture(cx, 5001);
    }

    fn warm_switch_fixture(cx: &mut TestAppContext, count: usize) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = TestDirectory::new();
        let roots: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|name| {
                let root = temp.path().join(name);
                std::fs::create_dir(&root).unwrap();
                for n in 0..count {
                    std::fs::write(
                        root.join(format!("{n:05}.md")),
                        format!(
                            "# {name} {n}\n\n{}",
                            if name == "a" {
                                "orchardvaultsentinel"
                            } else {
                                "mountainvaultsentinel"
                            }
                        ),
                    )
                    .unwrap();
                }
                root.canonicalize().unwrap()
            })
            .collect();
        let opts_for = |root: &Path, note: Option<String>| Opts {
            vault: Some(root.to_owned()),
            note,
            cache_base_override: Some(temp.path().join("os-cache")),
            session_directory: Some(temp.path().join("state")),
            ..Default::default()
        };
        let note = format!("{:05}.md", count - 1);
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let v = cx.new(|cx| Reader::new(opts_for(&roots[0], Some(note.clone())), window, cx));
            reader = Some(v.clone());
            Root::new(v, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            v.start_loading(opts_for(&roots[1], Some("00000.md".into())), window, cx)
        });
        visual.run_until_parked();
        for index in [0, 1, 0] {
            let root = &roots[index];
            let expected = if index == 0 { &note } else { "00000.md" };
            let (release, hold) = async_channel::bounded(1);
            let mut opts = opts_for(root, None);
            opts.preparation_hold = Some(hold);
            let started = std::time::Instant::now();
            reader.update_in(visual, |v, window, cx| v.start_loading(opts, window, cx));
            visual.run_until_parked();
            let first = started.elapsed();
            reader.read_with(visual, |v, cx| {
                assert!(
                    v.document_ready()
                        && v.loading.as_ref().unwrap().warm
                        && v.loading.as_ref().unwrap().active
                );
                assert_eq!(v.vault_root, *root);
                assert_eq!(v.current_rel, expected);
                assert_eq!(
                    v.vault.notes.len(),
                    count,
                    "full cached tree before reconcile"
                );
                assert!(v.vault.inventory_scanned);
                assert_eq!(v.content.read(cx).preparation_status(), Some(Ok(())));
                let own = if index == 0 {
                    "orchardvaultsentinel"
                } else {
                    "mountainvaultsentinel"
                };
                let other = if index == 0 {
                    "mountainvaultsentinel"
                } else {
                    "orchardvaultsentinel"
                };
                let search = v
                    .searcher
                    .as_ref()
                    .expect("saved search available before reconcile");
                assert!(!search.search(own, 1).unwrap().is_empty());
                assert!(
                    search.search(other, 1).unwrap().is_empty(),
                    "no predecessor-root results"
                );
            });
            eprintln!("{count}-note warm switch to vault {index}: first_usable={first:?}; background held; native timing NOT_RUN");
            release.try_send(()).unwrap();
            visual.run_until_parked();
        }
    }

    #[gpui::test]
    fn folder_picker_open_binds_worker_and_ui_diagnostics_to_resolved_root(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let temp = TestDirectory::new();
        let root = temp.path().join("source");
        let state = temp.path().join("state");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("first.md"), "# Positive control").unwrap();
        let trace = reader_diagnostics::Trace::new(Some(state.clone()), None);
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(root.clone()),
                        diagnostics: Some(trace),
                        index_dir: Some(temp.path().join("cache")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            Root::new(view, window, cx)
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let expected = tessera_core::vault::display_path(&root.canonicalize().unwrap());
        let mut events = Vec::new();
        for _ in 0..100 {
            let text =
                std::fs::read_to_string(state.join("reader-diagnostic.log")).unwrap_or_default();
            events = text
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .collect();
            if events.iter().any(|event| event["phase"] == "vault_ready") {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        for phase in [
            "first_worker_ready",
            "reconcile_stats",
            "first_ui_stage",
            "vault_ready",
        ] {
            let event = events
                .iter()
                .find(|event| event["phase"] == phase)
                .unwrap_or_else(|| panic!("missing positive control phase {phase}"));
            assert_eq!(event["vault"], expected, "resolved context for {phase}");
        }
    }

    #[test]
    fn search_publication_failure_still_persists_warm_inventory_and_sources() {
        let temp = TestDirectory::new();
        let root = temp.path().join("source");
        let cache = temp.path().join("cache");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(root.join("last.md"), "# Last\n\npositivecontrol [[target]]").unwrap();
        std::fs::write(root.join("target.md"), "# Target").unwrap();
        // Deny only search publication; the source-bank directory is writable.
        std::fs::write(cache.join("generations"), "search publication obstruction").unwrap();
        let before = source_manifest(&root);
        let opts = Opts {
            vault: Some(root.clone()),
            note: Some("last.md".into()),
            index_dir: Some(cache.clone()),
            ..Default::default()
        };
        let (send, _receive) = async_channel::unbounded();
        let mut reads = 0;
        let Event::Ready {
            searcher: Some(searcher),
            warnings,
            ..
        } = prepare_rest_with_io(
            &root,
            &opts,
            &Cancellation::default(),
            &send,
            &mut |path| {
                reads += 1;
                std::fs::read(path)
            },
            |_| Err(anyhow::anyhow!("test watcher unavailable")),
            MAX_MEMORY_SEARCH_BYTES,
        )
        .unwrap()
        else {
            panic!("Ready with memory search")
        };
        assert_eq!(reads, 2, "cold source-read positive control");
        assert!(warnings
            .iter()
            .any(|w| w.operation == "persist search cache"));
        assert_eq!(searcher.search("positivecontrol", 10).unwrap().len(), 1);
        let saved = tessera_core::vault::warm::Snapshot::load_checked(&cache, &root).unwrap();
        assert!(
            saved.search_generation.is_none(),
            "never persist a nonexistent disk generation"
        );
        assert_eq!(
            saved.source("last.md").as_deref(),
            Some("# Last\n\npositivecontrol [[target]]")
        );
        let Event::First {
            vault,
            document,
            searcher,
            ..
        } = prepare_first(&opts, &Cancellation::default()).unwrap()
        else {
            panic!("warm First")
        };
        assert!(vault.inventory_scanned);
        assert_eq!(vault.notes.len(), 2);
        assert!(document.unwrap().1.source.contains("positivecontrol"));
        assert!(searcher.is_none());
        let Event::Ready { .. } = prepare_rest_with_io(
            &root,
            &opts,
            &Cancellation::default(),
            &send,
            &mut |_| panic!("unchanged precise source must be reused despite search failure"),
            |_| Err(anyhow::anyhow!("test watcher unavailable")),
            MAX_MEMORY_SEARCH_BYTES,
        )
        .unwrap() else {
            panic!("warm Ready")
        };
        assert_eq!(source_manifest(&root), before);
    }

    #[test]
    fn cold_inventory_is_published_before_search_build() {
        let temp = TestDirectory::new();
        let root = temp.path().join("source");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("last.md"), "# Last [[target]]").unwrap();
        std::fs::write(root.join("target.md"), "# Target").unwrap();
        let (send, receive) = async_channel::unbounded();
        let observed = Arc::new(AtomicBool::new(false));
        let hook_observed = observed.clone();
        let hook = Arc::new(move |_: &Path| {
            while let Ok(event) = receive.try_recv() {
                if let Event::SearchInventory {
                    notes,
                    reconciled: Some(vault),
                } = event
                {
                    assert_eq!(notes.len(), 2);
                    assert_eq!(vault.notes.len(), 2);
                    assert!(vault.inventory_scanned);
                    assert_eq!(vault.backlinks("target.md").len(), 1);
                    hook_observed.store(true, Ordering::SeqCst);
                }
            }
            assert!(
                hook_observed.load(Ordering::SeqCst),
                "tree inventory precedes any search work"
            );
        });
        prepare_rest(
            &root,
            &Opts {
                index_dir: Some(temp.path().join("cache")),
                index_build_hook: Some(hook),
                ..Default::default()
            },
            &Cancellation::default(),
            &send,
        )
        .unwrap();
        assert!(
            observed.load(Ordering::SeqCst),
            "build checkpoint positive control"
        );
    }

    #[test]
    fn in_place_search_publication_is_complete_immutable_and_reusable() {
        let temp = TestDirectory::new();
        let root = temp.path().join("source");
        let base = temp.path().join("cache");
        let destination = base.join("generations").join("a".repeat(64));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("last.md"), "positivecontrol").unwrap();
        let vault = Vault::scan(&root).unwrap();
        let documents = vec![tessera_core::search::SearchDocument {
            path: "last.md".into(),
            title: "Last".into(),
            text: "positivecontrol".into(),
        }];
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let builds = attempts.clone();
        let hook_destination = destination.clone();
        let hook = Arc::new(move |_: &Path| {
            builds.fetch_add(1, Ordering::SeqCst);
            assert!(
                open_completed_generation(&hook_destination).is_none(),
                "unfinished index must never be published"
            );
        });
        let opts = Opts {
            search_publish_in_place: true,
            index_build_hook: Some(hook),
            ..Default::default()
        };
        let cancel = Cancellation::default();
        let (send, _receive) = async_channel::unbounded();
        let active = prepare_search_generation(
            &vault,
            &documents,
            &base,
            &destination,
            &opts,
            &cancel,
            &send,
        )
        .unwrap();
        assert!(
            attempts.load(Ordering::SeqCst) > 0,
            "build positive control"
        );
        assert!(!base.join("attempts").exists());
        assert!(
            !destination.exists(),
            "no directory move or mutable pointer"
        );
        let completed = std::fs::read_dir(destination.with_extension("repairs"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(completed.len(), 1);
        assert_eq!(std::fs::read(completed[0].join("complete")).unwrap(), b"1");
        let count = attempts.load(Ordering::SeqCst);
        let reused = prepare_search_generation(
            &vault,
            &documents,
            &base,
            &destination,
            &opts,
            &cancel,
            &send,
        )
        .unwrap();
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            count,
            "warm search never rebuilds"
        );
        assert_eq!(active.search("positivecontrol", 10).unwrap().len(), 1);
        assert_eq!(reused.search("positivecontrol", 10).unwrap().len(), 1);
    }

    #[test]
    fn manual_rescan_reads_unchanged_revisions_and_replaces_search_contents() {
        let temp = TestDirectory::new();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("note.md"), "# Note\noldword").unwrap();
        let mut opts = Opts {
            index_dir: Some(temp.path().join("cache")),
            ..Default::default()
        };
        let (send, _receive) = async_channel::unbounded();
        drop(prepare_rest(&root, &opts, &Cancellation::default(), &send).unwrap());
        let mut reads = 0;
        let Event::Ready {
            searcher: Some(searcher),
            ..
        } = prepare_rest_with_reader(&root, &opts, &Cancellation::default(), &send, &mut |_| {
            reads += 1;
            Ok(b"# Note\nnewword".to_vec())
        })
        .unwrap()
        else {
            panic!("warm Ready")
        };
        assert_eq!(reads, 0, "precise unchanged revision positive control");
        assert_eq!(searcher.search("oldword", 10).unwrap().len(), 1);
        drop(searcher);
        opts.force_source_read = true;
        let Event::Ready {
            searcher: Some(searcher),
            sources,
            ..
        } = prepare_rest_with_reader(&root, &opts, &Cancellation::default(), &send, &mut |_| {
            reads += 1;
            Ok(b"# Note\nnewword".to_vec())
        })
        .unwrap()
        else {
            panic!("manual Rescan Ready")
        };
        assert_eq!(reads, 1, "manual refresh reads despite unchanged metadata");
        assert!(sources["note.md"].contains("newword"));
        assert_eq!(searcher.search("oldword", 10).unwrap().len(), 0);
        assert_eq!(searcher.search("newword", 10).unwrap().len(), 1);
        assert_eq!(
            std::fs::read_to_string(root.join("note.md")).unwrap(),
            "# Note\noldword"
        );
    }

    #[test]
    fn prepare_publishes_readable_inventory_when_an_entry_is_permission_denied() {
        let temp = TestDirectory::new();
        let root = temp.path().join("source");
        let cache = temp.path().join("cache");
        let state = temp.path().join("state");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("readable.md"),
            "# Readable\n\npositivecontrol [[locked]]",
        )
        .unwrap();
        std::fs::write(root.join("locked.md"), "# Locked\n\ndeniedcontent").unwrap();
        let before = source_manifest(&root);
        let mut denied = 0;
        let mut watch_attempted = false;
        let (send, _receive) = async_channel::unbounded();
        let Event::Ready {
            vault,
            searcher: Some(searcher),
            sources,
            warnings,
            titles,
            watcher,
            ..
        } = prepare_rest_with_io(
            &root,
            &Opts {
                index_dir: Some(cache.clone()),
                session_directory: Some(state.clone()),
                ..Default::default()
            },
            &Cancellation::default(),
            &send,
            &mut |path| {
                if path == root.join("locked.md") {
                    denied += 1;
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "Access is denied. (os error 5)",
                    ))
                } else {
                    std::fs::read(path)
                }
            },
            // A real recursive watcher depends on the host fd/inotify limits
            // and is not under test here; keep this test independent of them.
            |path| {
                watch_attempted = true;
                assert_eq!(path, root);
                Err(anyhow::anyhow!("test watcher disabled"))
            },
            MAX_MEMORY_SEARCH_BYTES,
        )
        .unwrap()
        else {
            panic!("Ready must publish readable inventory")
        };
        assert_eq!(denied, 1, "The denied read actually fired");
        assert!(watch_attempted, "Stub watcher positive control");
        assert!(watcher.is_none());
        assert!(vault.inventory_scanned);
        assert!(!vault.inventory_complete);
        assert_eq!(vault.unreadable.len(), 1);
        assert_eq!(vault.unreadable[0].path, root.join("locked.md"));
        assert_eq!(vault.unreadable[0].operation, "read note");
        // The only preparation warning is the stubbed watcher; no search
        // cache warning means disk search was validated, built and persisted.
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].operation.starts_with("watch vault"));
        assert!(warnings[0].error.contains("test watcher disabled"));
        assert!(sources.contains_key("readable.md"));
        assert!(!sources.contains_key("locked.md"));
        assert_eq!(titles["readable.md"], "Readable");
        assert_eq!(
            titles["locked.md"], "locked",
            "unavailable title keeps the filename without another read"
        );
        assert_eq!(vault.backlinks("locked.md").len(), 1);
        assert_eq!(searcher.search("positivecontrol", 10).unwrap().len(), 1);
        assert!(searcher.search("deniedcontent", 10).unwrap().is_empty());
        let saved = tessera_core::vault::warm::Snapshot::load(&cache, &root).unwrap();
        assert!(
            !saved.vault().inventory_complete,
            "Partial cache stays provisional"
        );
        assert!(
            saved.search_generation.is_some(),
            "Disk search positive control"
        );
        let startup = tessera_core::vault::warm::StartupSnapshot::load(&cache, &root).unwrap();
        assert_eq!(startup.vault().notes.len(), 2);
        assert_eq!(startup.vault().unreadable.len(), 1);
        let Event::First {
            vault: first_vault,
            searcher: first_search,
            ..
        } = prepare_first(
            &Opts {
                vault: Some(root.clone()),
                note: Some("readable.md".into()),
                index_dir: Some(cache.clone()),
                ..Default::default()
            },
            &Cancellation::default(),
        )
        .unwrap()
        else {
            panic!("cached First expected")
        };
        assert!(first_vault.inventory_scanned);
        assert!(!first_vault.inventory_complete);
        assert_eq!(first_vault.notes.len(), 2);
        assert_eq!(first_vault.unreadable.len(), 1);
        assert_eq!(
            first_search
                .unwrap()
                .search("positivecontrol", 10)
                .unwrap()
                .len(),
            1
        );
        let log = std::fs::read_to_string(state.join("reader-diagnostic.log")).unwrap();
        assert!(
            log.contains("locked.md") && log.contains("read note") && log.contains("os error 5")
        );
        assert_eq!(
            source_manifest(&root),
            before,
            "No canonical bytes or paths changed"
        );
    }

    #[test]
    fn denied_cache_keeps_inventory_backlinks_and_search_and_reports_operation_chain() {
        let temp = TestDirectory::new();
        let root = temp.path().join("source");
        let cache = temp.path().join("blocked-cache");
        let state = temp.path().join("state");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("readable.md"),
            "# Readable\n\npositivecontrol [[target]]",
        )
        .unwrap();
        std::fs::write(root.join("target.md"), "# Target").unwrap();
        // A real, deterministic filesystem failure, independent of root/ACL privileges.
        std::fs::write(&cache, "cache path is a file").unwrap();
        let before = source_manifest(&root);
        let (send, _receive) = async_channel::unbounded();
        let opts = Opts {
            index_dir: Some(cache.clone()),
            session_directory: Some(state.clone()),
            ..Default::default()
        };
        let Event::Ready {
            vault,
            searcher: Some(searcher),
            sources,
            warnings,
            ..
        } = prepare_rest(&root, &opts, &Cancellation::default(), &send).unwrap()
        else {
            panic!("Ready despite cache failure")
        };
        assert!(vault.inventory_scanned && vault.inventory_complete);
        assert!(vault.unreadable.is_empty());
        assert_eq!(sources.len(), 2);
        assert_eq!(vault.backlinks("target.md").len(), 1);
        assert_eq!(searcher.search("positivecontrol", 10).unwrap().len(), 1);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].path, cache);
        // Pinning is the first cache write of a generation build.
        assert!(warnings[0].error.contains("Pin external search generation"));
        assert!(warnings[0]
            .error
            .contains("Create search generation pin directory"));
        assert!(warnings[0].error.contains("blocked-cache"));
        let log = std::fs::read_to_string(state.join("reader-diagnostic.log")).unwrap();
        assert!(
            log.contains("preparation_warnings")
                && log.contains("Create search generation pin directory")
        );
        assert_eq!(
            std::fs::read_to_string(&cache).unwrap(),
            "cache path is a file"
        );
        assert_eq!(source_manifest(&root), before);
        // Retry after the external obstruction is removed returns to persistent search.
        std::fs::remove_file(&cache).unwrap();
        let Event::Ready { warnings, .. } =
            prepare_rest(&root, &opts, &Cancellation::default(), &send).unwrap()
        else {
            panic!("Ready after recovery")
        };
        assert!(warnings.is_empty());
        assert!(cache.join("reader-snapshot.json").is_file());
        assert_eq!(source_manifest(&root), before);
    }

    #[test]
    fn displayed_error_chains_hide_verbatim_prefixes_without_changing_io_paths() {
        use tessera_core::vault::{display_error, display_path};
        let path = PathBuf::from(r"\\?\C:\Users\user\Obsidian Vault");
        #[cfg(windows)]
        assert_eq!(display_path(&path), r"C:\Users\user\Obsidian Vault");
        #[cfg(not(windows))]
        assert_eq!(display_path(&path), path.to_str().unwrap());
        assert_eq!(path.to_str().unwrap(), r"\\?\C:\Users\user\Obsidian Vault");
        let message = r"Prepare vault \\?\C:\vault: Read note \\?\UNC\server\share\locked.md: Access is denied. (os error 5)";
        #[cfg(windows)]
        assert_eq!(
            display_error(message),
            r"Prepare vault C:\vault: Read note \\server\share\locked.md: Access is denied. (os error 5)"
        );
        #[cfg(not(windows))]
        assert_eq!(display_error(message), message);
    }

    #[test]
    fn watcher_permission_denied_is_visible_without_blocking_readable_inventory() {
        let temp = TestDirectory::new();
        let root = temp.path().join("source");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("readable.md"), "# Readable\n\npositivecontrol").unwrap();
        let before = source_manifest(&root);
        let (send, _receive) = async_channel::unbounded();
        let mut attempted = false;
        let Event::Ready {
            vault,
            watcher,
            warnings,
            searcher: Some(searcher),
            ..
        } = prepare_rest_with_io(
            &root,
            &Opts {
                index_dir: Some(temp.path().join("cache")),
                ..Default::default()
            },
            &Cancellation::default(),
            &send,
            &mut |path| std::fs::read(path),
            |path| {
                attempted = true;
                assert_eq!(path, root);
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "Access is denied. (os error 5)",
                )
                .into())
            },
            MAX_MEMORY_SEARCH_BYTES,
        )
        .unwrap()
        else {
            panic!("Ready despite watcher failure")
        };
        assert!(attempted, "Watcher failure positive control");
        assert!(vault.inventory_scanned && vault.inventory_complete);
        assert!(watcher.is_none());
        assert_eq!(searcher.search("positivecontrol", 10).unwrap().len(), 1);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].path, root);
        assert!(warnings[0]
            .operation
            .contains("automatic refresh unavailable"));
        assert!(
            warnings[0]
                .error
                .contains("Register recursive change watcher")
                && warnings[0].error.contains("os error 5")
        );
        assert_eq!(source_manifest(&root), before);
    }

    #[test]
    fn cache_metadata_denial_retains_io_cause_and_exact_operation_path() {
        let path = PathBuf::from("/locked-cache/reader");
        let mut probes = 0;
        let error = existing_cache_ancestor(&path, |candidate| {
            probes += 1;
            assert_eq!(candidate, path);
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Access is denied. (os error 5)",
            ))
        })
        .unwrap_err();
        assert_eq!(probes, 1);
        assert!(
            error.chain().any(|cause| cause.is::<std::io::Error>()),
            "Cache-validation denial takes the warning path"
        );
        let chain = format!("{error:#}");
        assert!(
            chain.contains("Read search cache ancestor metadata")
                && chain.contains("locked-cache")
                && chain.contains("os error 5")
        );
        let missing = existing_cache_ancestor(&path, |_| Err(std::io::ErrorKind::NotFound.into()))
            .unwrap_err();
        assert!(missing.chain().any(|cause| cause.is::<std::io::Error>()));
    }

    #[test]
    fn oversized_memory_fallback_keeps_notes_ready_and_reports_search_unavailable() {
        let temp = TestDirectory::new();
        let root = temp.path().join("source");
        let cache = temp.path().join("blocked-cache");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("readable.md"),
            "# Readable\n\npositivecontrol [[target]]",
        )
        .unwrap();
        std::fs::write(root.join("target.md"), "# Target").unwrap();
        std::fs::write(&cache, "external cache obstruction").unwrap();
        let before = source_manifest(&root);
        let (send, _receive) = async_channel::unbounded();
        let Event::Ready {
            vault,
            sources,
            searcher,
            warnings,
            ..
        } = prepare_rest_with_io(
            &root,
            &Opts {
                index_dir: Some(cache),
                ..Default::default()
            },
            &Cancellation::default(),
            &send,
            &mut |path| std::fs::read(path),
            |path| VaultWatcher::new(path).map_err(anyhow::Error::new),
            1,
        )
        .unwrap()
        else {
            panic!("Ready without content search")
        };
        assert!(vault.inventory_scanned && vault.inventory_complete);
        assert_eq!(sources.len(), 2);
        assert_eq!(vault.backlinks("target.md").len(), 1);
        assert!(searcher.is_none());
        assert!(
            warnings.iter().any(|issue| issue
                .error
                .contains("Create search generation pin directory")),
            "A real cache failure precedes the budget guard"
        );
        assert!(warnings
            .iter()
            .any(|issue| issue.error.contains("in-memory fallback limit")));
        assert_eq!(source_manifest(&root), before);
    }

    #[gpui::test]
    fn panel_preferences_rebind_without_writing_new_root(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = TestDirectory::new();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        for root in [&a, &b] {
            std::fs::create_dir(root).unwrap();
            std::fs::write(root.join("start.md"), "# Readable").unwrap();
        }
        let settings = b.join("config/tessera/reader-layout.json");
        let opts = Opts {
            vault: Some(a.clone()),
            panel_settings_override: Some(settings.clone()),
            index_dir: Some(temp.path().join("cache")),
            ..Default::default()
        };
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| Reader::new(opts.clone(), window, cx));
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |v, _, _| {
            assert_eq!(v.panel_settings.as_ref(), Some(&settings));
            v.set_panel_width(reader_layout::Panel::Notes, 410.);
            v.persist_panel_width(reader_layout::Panel::Notes);
        });
        let bytes = std::fs::read(&settings).unwrap(); // Save positive control.
        let (release, hold) = async_channel::bounded(1);
        reader.update_in(visual, |v, window, cx| {
            v.start_loading(
                Opts {
                    vault: Some(b.clone()),
                    panel_preferences_hold: Some(hold),
                    ..opts.clone()
                },
                window,
                cx,
            )
        });
        visual.run_until_parked();
        reader.update_in(visual, |v, _, _| {
            assert_eq!(v.vault_root, b);
            assert!(v.document_ready());
            assert!(
                v.panel_settings.is_none(),
                "publication revokes predecessor capability"
            );
            v.set_panel_width(reader_layout::Panel::Notes, 510.);
            v.persist_panel_width(reader_layout::Panel::Notes);
        });
        assert_eq!(std::fs::read(&settings).unwrap(), bytes);
        release.try_send(()).unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            assert!(v.panel_preferences_ready);
            assert!(
                v.panel_settings.is_none(),
                "settings inside accepted root refused"
            );
            v.persist_panel_width(reader_layout::Panel::Notes);
            v.start_loading(
                Opts {
                    vault: Some(temp.path().join("missing")),
                    ..opts.clone()
                },
                window,
                cx,
            );
        });
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert_eq!(v.vault_root, b);
            assert!(v.panel_settings.is_none());
        });
        assert_eq!(std::fs::read(&settings).unwrap(), bytes);
    }

    #[gpui::test]
    fn panel_preferences_held_io_keeps_document_input_and_live_widths(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = TestDirectory::new();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("start.md"), "# Usable before preferences").unwrap();
        let settings = temp.path().join("settings.json");
        std::fs::write(&settings, r#"{"notes":620,"backlinks":420}"#).unwrap();
        let (release, hold) = async_channel::bounded(1);
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        panel_settings_override: Some(settings.clone()),
                        panel_preferences_hold: Some(hold),
                        index_dir: Some(temp.path().join("cache")),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            assert!(!reader.read(cx).panel_preferences_ready);
            assert_eq!(
                reader.read(cx).panel_widths.notes,
                reader_layout::DEFAULT_PANEL_WIDTH
            );
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            assert!(v.document_ready());
            assert!(!v.panel_preferences_ready);
            v.open_find(window, cx);
            v.find_input
                .update(cx, |input, cx| input.set_value("Usable", window, cx));
            v.set_panel_width(reader_layout::Panel::Notes, 350.);
        });
        release.try_send(()).unwrap();
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert!(v.panel_preferences_ready);
            assert_eq!(v.panel_settings.as_ref(), Some(&settings));
            assert_eq!(
                v.panel_widths.notes, 350.,
                "late disk widths cannot overwrite live edit"
            );
        });
        let (release, hold) = async_channel::bounded(1);
        reader.update_in(visual, |v, window, cx| {
            v.start_loading(
                Opts {
                    vault: Some(root.clone()),
                    panel_preferences_hold: Some(hold),
                    panel_settings_override: Some(settings.clone()),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        visual.run_until_parked();
        reader.update_in(visual, |v, _, cx| {
            assert!(v.document_ready());
            v.cancel_loading(cx);
            assert!(!v.loading.as_ref().unwrap().active);
        });
        let next = temp.path().join("next");
        std::fs::create_dir(&next).unwrap();
        std::fs::write(next.join("next.md"), "# Next root").unwrap();
        let next_settings = temp.path().join("next-settings.json");
        std::fs::write(&next_settings, r#"{"notes":470,"backlinks":430}"#).unwrap();
        reader.update_in(visual, |v, window, cx| {
            v.start_loading(
                Opts {
                    vault: Some(next.clone()),
                    panel_settings_override: Some(next_settings.clone()),
                    index_dir: Some(temp.path().join("next-cache")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        visual.run_until_parked();
        release.try_send(()).unwrap();
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert_eq!(v.vault_root, next);
            assert_eq!(v.panel_settings.as_ref(), Some(&next_settings));
            assert_eq!(
                v.panel_widths.notes, 470.,
                "stale held job cannot override new root"
            );
        });
    }

    #[gpui::test]
    fn panel_preferences_failed_root_exact_retry_rebinds(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = TestDirectory::new();
        let root = temp.path().join("later");
        let settings = temp.path().join("settings.json");
        std::fs::write(&settings, r#"{"notes":620,"backlinks":420}"#).unwrap();
        let opts = Opts {
            open_path: Some(root.join("start.md")),
            panel_settings_override: Some(settings.clone()),
            index_dir: Some(temp.path().join("cache")),
            ..Default::default()
        };
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| Reader::new(opts.clone(), window, cx));
            entity = Some(reader.clone());
            Root::new(reader, window, cx)
        });
        let reader = entity.unwrap();
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert!(!v.document_ready());
            assert!(v.panel_settings.is_none());
        });
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("start.md"), "# Exact retry").unwrap();
        reader.update_in(visual, |v, window, cx| {
            let retry = v.loading.as_ref().unwrap().opts.clone();
            v.start_loading(retry, window, cx);
        });
        visual.run_until_parked();
        reader.update_in(visual, |v, _, _| {
            assert_eq!(v.vault_root, root);
            assert_eq!(v.current_rel, "start.md");
            assert_eq!(v.panel_settings.as_ref(), Some(&settings));
            assert_eq!(v.panel_widths.notes, 620.);
            v.set_panel_width(reader_layout::Panel::Notes, 480.);
            v.persist_panel_width(reader_layout::Panel::Notes);
        });
        assert_eq!(reader_layout::Widths::load(&settings).notes, 480.);
    }

    #[gpui::test]
    fn retained_watcher_survives_candidate_cancel_and_new_root_clears_search_rows(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = TestDirectory::new();
        for name in ["a", "b", "empty"] {
            std::fs::create_dir(temp.path().join(name)).unwrap();
        }
        std::fs::write(
            temp.path().join("a/shared.md"),
            format!(
                "# A\n\noldrootword\n\n{}",
                "Distinct paragraph for scrolling.\n\n".repeat(40)
            ),
        )
        .unwrap();
        std::fs::write(temp.path().join("b/shared.md"), "# B\n\nnewrootword").unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(temp.path().join("a")),
                        note: Some("shared.md".into()),
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
        let (release_poll, hold_poll) = async_channel::bounded(1);
        let (release_validation, hold_validation) = async_channel::bounded(1);
        let published_cache = reader.read_with(visual, |v, _| v.index_dir.clone());
        let (content, epoch) = reader.update_in(visual, |v, window, cx| {
            assert!(v.watcher.is_some());
            assert_eq!(
                v.searcher
                    .as_ref()
                    .unwrap()
                    .search("oldrootword", 30)
                    .unwrap()
                    .len(),
                1,
                "old search positive control"
            );
            v.content.read(cx).list_state().scroll_to(ListOffset {
                item_ix: 5,
                offset_in_item: px(3.),
            });
            window.focus(&v.content.read(cx).focus_handle().clone(), cx);
            v.watcher_poll_hold = Some(hold_poll);
            v.poll_vault(window, cx);
            assert!(v.watcher.is_none(), "worker really borrowed the watcher");
            let identity = (v.content.entity_id(), v.watcher_generation);
            v.start_loading(
                Opts {
                    vault: Some(temp.path().join("b")),
                    validation_hold: Some(hold_validation),
                    ..Default::default()
                },
                window,
                cx,
            );
            identity
        });
        release_poll.try_send(()).unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            assert!(
                v.watcher.is_some(),
                "old published root regains its borrowed watcher during candidate load"
            );
            assert_eq!(v.watcher_generation, epoch);
            v.cancel_loading(cx);
            assert_eq!(v.content.entity_id(), content);
            assert_eq!(
                v.searcher
                    .as_ref()
                    .unwrap()
                    .search("oldrootword", 30)
                    .unwrap()
                    .len(),
                1
            );
            assert!(v.content.read(cx).focus_handle().is_focused(window));
        });
        release_validation.try_send(()).unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            v.start_loading(
                Opts {
                    vault: Some(temp.path().join("empty")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            assert!(v.document_ready());
            assert!(v.watcher.is_some());
            assert_eq!(v.content.entity_id(), content);
            assert_eq!(v.vault_root, temp.path().join("a"));
            assert_eq!(v.index_dir, published_cache);
            assert!(
                v.loading.as_ref().unwrap().opts.cache_lease.is_none(),
                "failed candidate no longer pins an unused cache"
            );
            assert_eq!(
                v.content.read(cx).list_state().logical_scroll_top().item_ix,
                5
            );
            assert!(v.content.read(cx).focus_handle().is_focused(window));
            assert_eq!(
                v.searcher
                    .as_ref()
                    .unwrap()
                    .search("oldrootword", 30)
                    .unwrap()
                    .len(),
                1
            );
        });
        let path = temp.path().join("a/shared.md");
        let updated = format!(
            "{}\nWatcherSurvivedPositiveControl\n",
            std::fs::read_to_string(&path).unwrap()
        );
        std::fs::write(&path, updated).unwrap();
        std::thread::sleep(Duration::from_millis(40));
        reader.update_in(visual, |v, window, cx| v.poll_vault(window, cx));
        visual.run_until_parked();
        std::thread::sleep(Duration::from_millis(350));
        reader.update_in(visual, |v, window, cx| v.poll_vault(window, cx));
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert!(
                v.note_source.contains("WatcherSurvivedPositiveControl"),
                "the retained watcher actually delivers later source changes"
            )
        });
        let (release_rest, hold_rest) = async_channel::bounded(1);
        reader.update_in(visual, |v, window, cx| {
            v.start_loading(
                Opts {
                    vault: Some(temp.path().join("b")),
                    note: Some("shared.md".into()),
                    preparation_hold: Some(hold_rest),
                    index_dir: Some(temp.path().join("cache-b")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        visual.run_until_parked();
        reader.update_in(visual, |v, _, cx| {
            assert_eq!(v.vault_root, temp.path().join("b"));
            assert!(v.note_source.contains("newrootword"));
            assert!(
                v.searcher.is_none(),
                "old-root rows disappear at first publication"
            );
            v.cancel_loading(cx);
            assert!(v.searcher.is_none());
            assert!(v.document_ready());
        });
        release_rest.try_send(()).unwrap();
        visual.run_until_parked();
    }

    #[test]
    fn cancelled_staged_build_cannot_remove_concurrent_completed_generation() {
        cancelled_search_build(false);
        cancelled_search_build(true);
    }

    fn cancelled_search_build(in_place: bool) {
        let temp = TestDirectory::new();
        let root = temp.path().join("source");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("note.md"), "# Concurrent\n\nsearchable").unwrap();
        let manifest = source_manifest(&root);
        let (entered, received) = std::sync::mpsc::channel();
        let (release, held) = std::sync::mpsc::channel();
        let held = std::sync::Mutex::new(held);
        let once = AtomicBool::new(false);
        let hook = Arc::new(move |path: &Path| {
            if path.exists() && !once.swap(true, Ordering::SeqCst) {
                entered.send(path.to_path_buf()).unwrap();
                held.lock().unwrap().recv().unwrap();
            }
        });
        let cache = temp.path().join("cache");
        let opts = Opts {
            index_dir: Some(cache),
            index_build_hook: Some(hook),
            search_publish_in_place: in_place,
            ..Default::default()
        };
        let cancellation = Cancellation::default();
        let worker_cancel = cancellation.clone();
        let worker_root = root.clone();
        let worker_opts = opts.clone();
        let worker = std::thread::spawn(move || {
            let (send, _receive) = async_channel::unbounded();
            prepare_rest(&worker_root, &worker_opts, &worker_cancel, &send)
        });
        let staged = received.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(
            staged.is_dir(),
            "first worker owns an actual in-progress index"
        );
        let (send, _receive) = async_channel::unbounded();
        let Event::Ready {
            searcher: Some(winner),
            ..
        } = prepare_rest(
            &root,
            &Opts {
                index_build_hook: None,
                ..opts
            },
            &Cancellation::default(),
            &send,
        )
        .unwrap()
        else {
            panic!("winner")
        };
        cancellation.cancel();
        release.send(()).unwrap();
        assert!(worker.join().unwrap().is_err());
        assert!(
            !staged.exists(),
            "only cancelled attempt's staging is removed"
        );
        assert_eq!(winner.search("searchable", 10).unwrap().len(), 1);
        assert_eq!(source_manifest(&root), manifest);
    }

    #[gpui::test]
    fn parser_configuration_revision_is_part_of_preparation_readiness(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let text = "# Title\n\nLong prepared paragraph.\n\n".repeat(300);
        let content = cx.new(|cx| TextViewState::markdown(&text, cx));
        cx.run_until_parked();
        content.update(cx, |state, cx| {
            assert_eq!(state.preparation_status(), Some(Ok(())));
            let configured = markdown_plugins(
                TextView::markdown("configuration-regression", ""),
                Arc::new(|_, _, _, _| {}),
                Arc::new(|_| None),
                SelectionFormat::Plain,
            );
            configured.prepare_state(state, cx);
            assert_eq!(
                state.preparation_status(),
                None,
                "equal source bytes cannot acknowledge a pending configuration parse"
            );
        });
        cx.run_until_parked();
        content.read_with(cx, |state, _| {
            assert_eq!(state.preparation_status(), Some(Ok(())))
        });
    }

    #[test]
    fn corrupt_generation_rebuilds_without_deleting_active_cache_or_source() {
        let temp = TestDirectory::new();
        let root = temp.path().join("source");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("note.md"), "# Repair\n\nsearchable").unwrap();
        let cache = temp.path().join("cache");
        let opts = Opts {
            index_dir: Some(cache.clone()),
            ..Default::default()
        };
        let (send, _receive) = async_channel::unbounded();
        let Event::Ready {
            searcher: Some(old),
            ..
        } = prepare_rest(&root, &opts, &Cancellation::default(), &send).unwrap()
        else {
            panic!("ready")
        };
        let old_directory = std::fs::read_dir(cache.join("generations"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::write(
            root.join("note.md"),
            "# Repair\n\nsearchable new generation",
        )
        .unwrap();
        let manifest = source_manifest(&root);
        assert!(matches!(
            prepare_rest(&root, &opts, &Cancellation::default(), &send).unwrap(),
            Event::Ready { .. }
        ));
        let generation = std::fs::read_dir(cache.join("generations"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| *path != old_directory)
            .unwrap();
        std::fs::write(generation.join("meta.json"), "broken metadata").unwrap();
        let Event::Ready {
            searcher: Some(repaired),
            ..
        } = prepare_rest(&root, &opts, &Cancellation::default(), &send).unwrap()
        else {
            panic!("repaired")
        };
        assert_eq!(old.search("searchable", 10).unwrap().len(), 1);
        assert_eq!(repaired.search("searchable", 10).unwrap().len(), 1);
        assert_eq!(
            std::fs::read_to_string(generation.join("meta.json")).unwrap(),
            "broken metadata"
        );
        let repairs = generation.with_extension("repairs");
        let count = std::fs::read_dir(&repairs).unwrap().count();
        assert!(count > 0);
        assert!(matches!(
            prepare_rest(&root, &opts, &Cancellation::default(), &send).unwrap(),
            Event::Ready { .. }
        ));
        assert_eq!(
            std::fs::read_dir(&repairs).unwrap().count(),
            count,
            "warm reuse of healthy repair"
        );
        assert_eq!(source_manifest(&root), manifest);
        let invalid = Opts {
            index_dir: Some(temp.path().join("not-created/../source/cache")),
            ..Default::default()
        };
        assert!(prepare_rest(&root, &invalid, &Cancellation::default(), &send).is_err());
        assert!(!temp.path().join("not-created").exists());
        assert_eq!(
            source_manifest(&root),
            manifest,
            "F8: no cache names or bytes under source"
        );
    }

    #[gpui::test]
    fn configured_first_document_reconciles_pending_embeds_and_scan_time_edits(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = TestDirectory::new();
        let root = temp.path().join("source");
        std::fs::create_dir(&root).unwrap();
        let source = format!(
            "# Start\n\n> [!note] SpecialCallout\n> ==highlighted body==\n\n![[target]]\n\n{}",
            "Paragraph filler for asynchronous parsing.\n\n".repeat(180)
        );
        std::fs::write(root.join("start.md"), &source).unwrap();
        std::fs::write(
            root.join("target.md"),
            "# Embedded\n\nEmbeddedPositiveControl",
        )
        .unwrap();
        let (release, hold) = async_channel::bounded(1);
        let changed = root.join("start.md");
        let hook_source = format!("{source}\nScanTimePositiveControl\n");
        let once = Arc::new(AtomicBool::new(false));
        let hook = Arc::new(move || {
            if !once.swap(true, Ordering::SeqCst) {
                std::fs::write(&changed, &hook_source).unwrap();
                // Let the real notify receiver observe the edit before draining.
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("start.md".into()),
                        index_dir: Some(temp.path().join("cache")),
                        preparation_hold: Some(hold),
                        rest_snapshot_hook: Some(hook),
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
        reader.update_in(visual, |v, window, cx| {
            assert!(v.document_ready());
            assert!(v.searcher.is_none());
            assert!(v.note_source.contains("pending target"));
            assert!(!v.note_source.contains("missing target"));
            assert_eq!(v.content.read(cx).preparation_status(), Some(Ok(())));
            let configured = reader_plugins(
                v.vault_root.clone(),
                TextView::new(&v.content),
                cx.entity().downgrade(),
                v.sel_format,
                v.prepared_links.clone(),
                &v.link_identities,
            );
            v.content.update(cx, |state, cx| {
                configured.prepare_state(state, cx);
                assert_eq!(
                    state.preparation_status(),
                    Some(Ok(())),
                    "visible parser configuration must not start a replacement parse"
                );
                state.set_search_query("SpecialCallout", cx);
                assert!(
                    state.search_status().1 > 0,
                    "parsed custom callout positive control"
                );
            });
            window.focus(&v.content.read(cx).focus_handle().clone(), cx);
            v.content.read(cx).list_state().scroll_to(ListOffset {
                item_ix: 10,
                offset_in_item: px(3.),
            });
        });
        visual.run_until_parked();
        let (history, position) = reader.read_with(visual, |v, cx| {
            (
                v.history.clone(),
                v.content.read(cx).list_state().logical_scroll_top(),
            )
        });
        // This edit happens before watcher registration, independently of the
        // scan-time hook. Both must be reconciled by the prepared source snapshot.
        std::fs::write(
            root.join("target.md"),
            "# Embedded\n\nEmbeddedPositiveControl changed before watcher",
        )
        .unwrap();
        release.try_send(()).unwrap();
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(300));
        visual.run_until_parked();
        reader.read_with(visual, |v, cx| {
            assert!(v.note_source.contains("ScanTimePositiveControl"));
            assert!(v
                .note_source
                .contains("EmbeddedPositiveControl changed before watcher"));
            assert!(!v.note_source.contains("pending target"));
            assert_eq!(v.history, history);
            let now = v.content.read(cx).list_state().logical_scroll_top();
            assert_eq!(now.item_ix, position.item_ix);
            assert_eq!(now.offset_in_item, position.offset_in_item);
            assert!(v
                .searcher
                .as_ref()
                .unwrap()
                .search("ScanTimePositiveControl", 10)
                .unwrap()
                .iter()
                .any(|hit| hit.path == "start.md"));
        });
        std::fs::remove_file(root.join("start.md")).unwrap();
        reader.update_in(visual, |v, window, cx| {
            v.refresh_inventory(Default::default(), window, cx)
        });
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert!(v
                .link_notice
                .as_ref()
                .is_some_and(|notice| notice.contains("no longer available")));
            assert_eq!(v.current_rel, "start.md");
            assert_eq!(v.history, history);
        });
    }

    fn source_manifest(root: &Path) -> std::collections::BTreeMap<PathBuf, String> {
        fn visit(base: &Path, path: &Path, out: &mut std::collections::BTreeMap<PathBuf, String>) {
            use sha2::{Digest, Sha256};
            for entry in std::fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    out.insert(path.strip_prefix(base).unwrap().into(), "directory".into());
                    visit(base, &path, out);
                } else {
                    out.insert(
                        path.strip_prefix(base).unwrap().into(),
                        format!("{:x}", Sha256::digest(std::fs::read(path).unwrap())),
                    );
                }
            }
        }
        let mut result = std::collections::BTreeMap::new();
        visit(root, root, &mut result);
        result
    }

    #[test]
    fn explicit_missing_never_falls_back_and_discovery_is_read_only() {
        let root =
            std::env::temp_dir().join(format!("tessera-progressive-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("other.md"), "# Other").unwrap();
        let mut opts = Opts {
            vault: Some(root.clone()),
            note: Some("absent.md".into()),
            ..Default::default()
        };
        assert!(prepare_first(&opts, &Cancellation::default()).is_err());
        opts.note = None;
        let Event::First {
            document, vault, ..
        } = prepare_first(&opts, &Cancellation::default()).unwrap()
        else {
            panic!("first")
        };
        assert_eq!(document.unwrap().0, "other.md");
        assert!(!vault.inventory_complete);
        assert_eq!(
            std::fs::read_to_string(root.join("other.md")).unwrap(),
            "# Other"
        );
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        let cancel = Cancellation::default();
        cancel.cancel();
        assert!(prepare_first(&opts, &cancel).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn warm_tree_note_and_search_precede_background_work_and_keep_position(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let fixture =
            std::env::temp_dir().join(format!("tessera-warm-ui-{}", uuid::Uuid::new_v4()));
        let root = fixture.join("vault");
        std::fs::create_dir_all(root.join("folder")).unwrap();
        let source = (0..100)
            .map(|n| format!("## Section {n}\n\nSaved paragraph {n}.\n\n"))
            .collect::<String>();
        std::fs::write(root.join("last.md"), &source).unwrap();
        for n in 0..100 {
            std::fs::write(root.join(format!("stable-{n:02}.md")), "stable canary").unwrap();
        }
        std::fs::write(root.join("folder/old.md"), "oldcanary").unwrap();
        std::fs::write(root.join("unreadable.md"), [0xff]).unwrap();
        let state = fixture.join("state");
        let diagnostics = reader_diagnostics::Trace::new(Some(state.clone()), Some(root.clone()));
        let opts = Opts {
            diagnostics: Some(diagnostics),
            vault: Some(root.clone()),
            index_dir: Some(fixture.join("cache")),
            session_directory: Some(fixture.join("state")),
            ..Default::default()
        };
        let (send, _receive) = async_channel::unbounded();
        drop(
            prepare_rest(
                &root,
                &Opts {
                    note: Some("last.md".into()),
                    ..opts.clone()
                },
                &Cancellation::default(),
                &send,
            )
            .unwrap(),
        );
        std::fs::remove_file(root.join("unreadable.md")).unwrap();
        reader_history::ReadingHistory::record_usable_document(
            opts.session_directory.as_ref().unwrap(),
            &root,
            "last.md",
        )
        .unwrap();
        std::fs::remove_file(root.join("folder/old.md")).unwrap();
        std::fs::write(root.join("folder/new.md"), "newcanary").unwrap();
        let (release, hold) = async_channel::bounded(1);
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        preparation_hold: Some(hold),
                        ..opts
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        reader.update_in(visual, |v, _, _| v.panels.open(reader_layout::Panel::Notes));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        reader.update_in(visual, |v, window, cx| {
            assert!(v.document_ready());
            assert_eq!(v.current_rel, "last.md");
            assert_eq!(v.vault.notes.len(), 103);
            assert_eq!(
                v.vault.unreadable.len(),
                1,
                "cached partial inventory remains explicit"
            );
            assert!(v.vault.entries.iter().any(|e| e.path == "folder/old.md"));
            assert!(v.loading.as_ref().unwrap().active);
            assert!(v.loading.as_ref().unwrap().warm);
            assert!(!v.loading.as_ref().unwrap().show_progress);
            assert!(!v.vault.inventory_complete, "reconciliation is held");
            assert_eq!(
                v.searcher
                    .as_ref()
                    .unwrap()
                    .search("oldcanary", 5)
                    .unwrap()
                    .len(),
                1
            );
            v.sync_tree();
            v.tree.toggle("folder");
            v.tree.cursor = Some("stable-30.md".into());
            v.tree_scroll.scroll_to_item_strict(20, ScrollStrategy::Top);
            v.content.update(cx, |state, cx| state.select_all(cx));
            v.content.read(cx).list_state().scroll_to(ListOffset {
                item_ix: 20,
                offset_in_item: px(6.),
            });
            v.open_quick_open(false, window, cx);
            v.quick_open
                .input
                .update(cx, |input, cx| input.set_value("old", window, cx));
            v.refresh_quick_open(cx);
        });
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert_eq!(v.quick_open.rows[0].path, "folder/old.md")
        });
        visual.executor().advance_clock(Duration::from_millis(499));
        visual.run_until_parked();
        reader.read_with(visual, |v, _| {
            assert!(!v.loading.as_ref().unwrap().show_progress)
        });
        visual.executor().advance_clock(Duration::from_millis(1));
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let (content, selected, tree_offset) = reader.read_with(visual, |v, cx| {
            assert!(v.loading.as_ref().unwrap().show_progress);
            assert_eq!(v.loading.as_ref().unwrap().phase, "Checking search data");
            let selected = v.content.read(cx).selected_text();
            assert!(
                !selected.is_empty(),
                "selection established before reconciliation"
            );
            let tree_offset = v.tree_scroll.0.borrow().base_handle.offset().y;
            assert!(
                tree_offset < px(-100.),
                "nonzero tree-scroll positive control"
            );
            (v.content.entity_id(), selected, tree_offset)
        });
        release.try_send(()).unwrap();
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        reader.read_with(visual, |v, cx| {
            assert!(v.vault.inventory_complete, "completion positive control");
            assert!(!v.loading.as_ref().unwrap().active);
            assert!(!v.vault.entries.iter().any(|e| e.path == "folder/old.md"));
            assert!(v.vault.entries.iter().any(|e| e.path == "folder/new.md"));
            assert!(
                v.tree.rows.iter().any(|row| row.path == "folder/new.md"),
                "expanded folder survived"
            );
            assert_eq!(v.tree.cursor.as_deref(), Some("stable-30.md"));
            assert_eq!(v.tree_scroll.0.borrow().base_handle.offset().y, tree_offset);
            assert_eq!(v.content.entity_id(), content);
            assert_eq!(v.content.read(cx).selected_text(), selected);
            assert_eq!(
                v.content.read(cx).list_state().logical_scroll_top().item_ix,
                20
            );
            assert_eq!(
                v.content
                    .read(cx)
                    .list_state()
                    .logical_scroll_top()
                    .offset_in_item,
                px(6.)
            );
            assert_eq!(
                v.searcher
                    .as_ref()
                    .unwrap()
                    .search("newcanary", 5)
                    .unwrap()
                    .len(),
                1
            );
            assert!(v
                .searcher
                .as_ref()
                .unwrap()
                .search("oldcanary", 5)
                .unwrap()
                .is_empty());
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let log =
                std::fs::read_to_string(state.join("reader-diagnostic.log")).unwrap_or_default();
            let events = log
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .collect::<Vec<_>>();
            if events
                .iter()
                .any(|event| event["phase"] == "inventory_first_paint")
            {
                assert!(events.iter().any(
                    |event| event["phase"] == "warm_cache" && event["details"]["found"] == true
                ));
                assert!(events
                    .iter()
                    .any(|event| event["phase"] == "document_published"
                        && event["details"]["warm"] == true));
                assert!(events
                    .iter()
                    .any(|event| event["phase"] == "primary_selection"
                        && event["details"]["cached"] == true));
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "real paint callback timing positive control"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        visual.update(|window, _| window.remove_window());
        visual.run_until_parked();
        std::fs::remove_dir_all(fixture).unwrap();
    }

    /// Same-host test-renderer publication probe. Optional Linux preload delays
    /// actual vault I/O before publication; the preferences hold only isolates
    /// that phase from the deterministic executor's serial background work.
    #[gpui::test]
    #[ignore = "manual before/after warm first-tree frame measurement"]
    fn warm_first_tree_frame_profile(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let fixture =
            std::env::temp_dir().join(format!("tessera-warm-frame-{}", uuid::Uuid::new_v4()));
        let root = fixture.join("vault");
        std::fs::create_dir_all(&root).unwrap();
        let paragraphs = std::env::var("TESSERA_WARM_PROFILE_PARAGRAPHS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(40);
        #[cfg(unix)]
        std::fs::write(
            root.join("asset:literal-name.bin"),
            "literal name cache positive control",
        )
        .unwrap();
        let filesystem_delay = std::env::var_os("TESSERA_SLOW_FS_MS").is_some();
        let set_fs_phase = |name: &'static std::ffi::CStr| {
            if !filesystem_delay {
                return;
            }
            #[cfg(target_os = "linux")]
            unsafe {
                #[link(name = "dl")]
                unsafe extern "C" {
                    fn dlsym(
                        handle: *mut std::ffi::c_void,
                        name: *const std::ffi::c_char,
                    ) -> *mut std::ffi::c_void;
                }
                let symbol = dlsym(std::ptr::null_mut(), c"tessera_slow_fs_phase".as_ptr());
                assert!(!symbol.is_null(), "use the slow-vault-fs preload");
                let set: unsafe extern "C" fn(*const std::ffi::c_char) =
                    std::mem::transmute(symbol);
                set(name.as_ptr());
            }
            #[cfg(not(target_os = "linux"))]
            panic!("filesystem-delay probe is Linux-only: {name:?}");
        };
        let unreadable = std::env::var_os("TESSERA_WARM_PROFILE_UNREADABLE").is_some();
        let total_notes = if unreadable { 5002 } else { 5001 };
        if unreadable {
            std::fs::write(root.join("unreadable.md"), [0xff]).unwrap();
        }
        for section in 0..10 {
            let dir = root.join(format!("section-{section}"));
            std::fs::create_dir(&dir).unwrap();
            for note in 0..500 {
                std::fs::write(
                    dir.join(format!("note-{section}-{note}.md")),
                    format!(
                        "# Note {section} {note}\n\n[[last]]\n\n{}",
                        "Unicode текст paragraph.\n\n".repeat(paragraphs)
                    ),
                )
                .unwrap();
            }
        }
        let links = std::env::var("TESSERA_WARM_PROFILE_LINKS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        assert!(links <= 5000);
        let mut primary = String::from("# Last\n\nSaved searchcanary\n\n");
        for n in 0..links {
            primary.push_str(&format!("[Note {n}](note-{}-{}.md)\n\n", n / 500, n % 500));
        }
        std::fs::write(root.join("last.md"), &primary).unwrap();
        if filesystem_delay {
            set_fs_phase(c"positive_control");
            let delay: u64 = std::env::var("TESSERA_SLOW_FS_MS")
                .unwrap()
                .parse()
                .unwrap();
            let start = std::time::Instant::now();
            std::fs::metadata(root.join("last.md")).unwrap();
            assert!(start.elapsed() >= Duration::from_millis(delay));
            let start = std::time::Instant::now();
            assert_eq!(
                std::fs::read_to_string(root.join("last.md")).unwrap(),
                primary
            );
            assert!(start.elapsed() >= Duration::from_millis(delay));
            set_fs_phase(c"setup");
        }
        let opts = Opts {
            vault: Some(root.clone()),
            note: Some("last.md".into()),
            index_dir: Some(fixture.join("cache")),
            ..Default::default()
        };
        let (send, _receive) = async_channel::unbounded();
        drop(prepare_rest(&root, &opts, &Cancellation::default(), &send).unwrap());
        let delays = if filesystem_delay {
            vec![0]
        } else {
            vec![0, 650]
        };
        let samples = if filesystem_delay { 3 } else { 5 };
        for delay_ms in delays {
            for sample in 0..samples {
                let (release, hold) = async_channel::bounded(1);
                let mut reader = None;
                set_fs_phase(c"warm_primary");
                let start = std::time::Instant::now();
                let (_, visual) = cx.add_window_view(|window, cx| {
                    let view = cx.new(|cx| {
                        Reader::new(
                            Opts {
                                panel_preferences_hold: Some(hold),
                                ..opts.clone()
                            },
                            window,
                            cx,
                        )
                    });
                    reader = Some(view.clone());
                    Root::new(view, window, cx)
                });
                let reader = reader.unwrap();
                reader.update_in(visual, |v, _, _| v.panels.open(reader_layout::Panel::Notes));
                visual.run_until_parked();
                visual.update(|window, cx| window.draw(cx).clear(cx));
                let first_document_ms = start.elapsed().as_secs_f64() * 1000.;
                let early_tree =
                    reader.read_with(visual, |v, _| v.vault.notes.len() == total_notes);
                let mut first_tree_ms = early_tree.then(|| start.elapsed().as_secs_f64() * 1000.);
                set_fs_phase(c"setup");
                reader.update_in(visual, |v, _, cx| {
                    assert!(v.document_ready());
                    assert_eq!(v.link_identities.len(), links);
                    assert_eq!(v.link_original_source.as_deref(), Some(primary.as_str()));
                    assert!(v.tree.rows.iter().any(|row| row.path == "last.md"));
                    v.content.update(cx, |state, cx| {
                        state.set_search_query("searchcanary", cx);
                        assert!(
                            state.search_status().1 > 0,
                            "parsed document input positive control"
                        );
                    });
                });
                // Positive control: preferences/reconciliation are provably unfinished.
                reader.read_with(visual, |v, _| assert!(v.loading.as_ref().unwrap().active));
                if filesystem_delay {
                    // The deterministic executor would run the blocking rest
                    // worker serially. Measure first publication, then cancel;
                    // normal regressions cover reconciliation and retention.
                    assert!(early_tree, "complete cached tree precedes reconciliation");
                    reader.update_in(visual, |v, _, cx| v.cancel_loading(cx));
                }
                std::thread::sleep(Duration::from_millis(delay_ms));
                release.try_send(()).unwrap();
                visual.run_until_parked();
                visual.update(|window, cx| window.draw(cx).clear(cx));
                let tree_ms = first_tree_ms
                    .take()
                    .unwrap_or_else(|| start.elapsed().as_secs_f64() * 1000.);
                reader.read_with(visual, |v, _| {
                    assert_eq!(v.vault.notes.len(), total_notes);
                    assert_eq!(v.current_rel, "last.md");
                    assert!(!v.loading.as_ref().unwrap().active);
                    assert_eq!(
                        v.searcher
                            .as_ref()
                            .unwrap()
                            .search("searchcanary", 5)
                            .unwrap()
                            .len(),
                        1
                    );
                });
                visual.update(|window, _| window.remove_window());
                visual.run_until_parked();
                eprintln!("WARM_FRAME_PROFILE sample={sample} notes={total_notes} links={links} filesystem_delay={filesystem_delay} paragraphs={paragraphs} unreadable={unreadable} first_document_ms={first_document_ms:.2} first_tree_frame_ms={tree_ms:.2} tree_before_reconcile={early_tree}; GPUI test renderer, OS-warm fixture, controlled {delay_ms}ms preferences hold, native timing NOT_RUN");
            }
        }
        std::fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn warm_snapshot_is_published_before_reconciliation_and_updates_search() {
        let temp = std::env::temp_dir().join(format!("tessera-warm-{}", uuid::Uuid::new_v4()));
        let root = temp.join("vault");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Target.md"), "# Target").unwrap();
        std::fs::write(root.join("Note.md"), "[[Target]] oldcanary").unwrap();
        let opts = Opts {
            vault: Some(root.clone()),
            index_dir: Some(temp.join("cache")),
            session_directory: Some(temp.join("state")),
            ..Default::default()
        };
        let (send, receive) = async_channel::unbounded();
        let cold = prepare_rest(
            &root,
            &Opts {
                note: Some("Note.md".into()),
                ..opts.clone()
            },
            &Cancellation::default(),
            &send,
        )
        .unwrap();
        while let Ok(event) = receive.try_recv() {
            assert!(!matches!(event, Event::First { .. }));
        }
        drop(cold);
        std::fs::write(
            root.join("Note.md"),
            "[[Target]] newcanary plus changed length",
        )
        .unwrap();
        let (send, receive) = async_channel::unbounded();
        let Event::First {
            vault,
            searcher,
            document,
            ..
        } = prepare_first_with_last_document(&opts, &Cancellation::default(), |_| {
            Ok(Some("Note.md".into()))
        })
        .unwrap()
        else {
            panic!("first publication expected")
        };
        assert_eq!(vault.notes.len(), 2);
        assert_eq!(vault.backlinks("Target.md").len(), 1);
        assert!(!vault.inventory_complete, "saved state is provisional");
        assert_eq!(searcher.unwrap().search("oldcanary", 10).unwrap().len(), 1);
        assert!(document.unwrap().1.source.contains("oldcanary"));
        let ready = prepare_rest(&root, &opts, &Cancellation::default(), &send).unwrap();
        while receive.try_recv().is_ok() {}
        let Event::Ready {
            vault, searcher, ..
        } = ready
        else {
            panic!("ready inventory expected")
        };
        assert!(vault.inventory_complete);
        let searcher = searcher.unwrap();
        assert!(searcher.search("oldcanary", 10).unwrap().is_empty());
        assert_eq!(searcher.search("newcanary", 10).unwrap().len(), 1);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn corrupt_history_does_not_block_folder_open() {
        let temp =
            std::env::temp_dir().join(format!("tessera-open-history-{}", uuid::Uuid::new_v4()));
        let root = temp.join("vault");
        let state = temp.join("state");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(root.join("first.md"), "# First").unwrap();
        for bytes in [b"broken".as_slice(), b"{\"schema\":999}"] {
            std::fs::write(state.join("update-session.json"), bytes).unwrap();
            let Event::First {
                document,
                recovery_notice,
                ..
            } = prepare_first(
                &Opts {
                    vault: Some(root.clone()),
                    session_directory: Some(state.clone()),
                    ..Default::default()
                },
                &Cancellation::default(),
            )
            .unwrap()
            else {
                panic!("first document expected")
            };
            assert_eq!(document.unwrap().0, "first.md");
            assert!(recovery_notice.is_some());
            assert_eq!(std::fs::read(root.join("first.md")).unwrap(), b"# First");
        }
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn root_bound_last_document_hint_is_validated_and_missing_hint_falls_back() {
        let root = std::env::temp_dir().join(format!("tessera-last-hint-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("first.md"), "# First").unwrap();
        std::fs::write(root.join("last.md"), "# Last").unwrap();
        let opts = Opts {
            vault: Some(root.clone()),
            ..Default::default()
        };
        let Event::First { document, .. } =
            prepare_first_with_last_document(&opts, &Cancellation::default(), |queried| {
                assert_eq!(queried, root.canonicalize().unwrap());
                Ok(Some("last.md".into()))
            })
            .unwrap()
        else {
            panic!("first")
        };
        assert_eq!(document.unwrap().0, "last.md");
        std::fs::remove_file(root.join("last.md")).unwrap();
        let Event::First { document, .. } =
            prepare_first_with_last_document(&opts, &Cancellation::default(), |_| {
                Ok(Some("last.md".into()))
            })
            .unwrap()
        else {
            panic!("first")
        };
        assert_eq!(document.unwrap().0, "first.md");
        let explicit = Opts {
            note: Some("absent.md".into()),
            ..opts
        };
        assert!(
            prepare_first_with_last_document(&explicit, &Cancellation::default(), |_| panic!(
                "explicit request must not consult last document"
            ))
            .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn uncached_deep_document_switches_before_inventory_and_survives_completion(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let temp = TestDirectory::new();
        let root = temp.path().join("vault");
        std::fs::create_dir_all(root.join("deep/new")).unwrap();
        std::fs::write(root.join("first.md"), "# Initial").unwrap();
        for n in 0..5000 {
            std::fs::write(root.join(format!("note{n}.md")), "# Inventory\n\n[[first]]").unwrap();
        }
        let (release, hold) = async_channel::bounded(1);
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("first.md".into()),
                        index_dir: Some(temp.path().join("cache")),
                        preparation_hold: Some(hold),
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
        // Created AFTER first publication, never inventoried or cached.
        let deep = "deep/new/Unicode-заметка.md";
        let source = (0..300)
            .map(|n| format!("## Fresh {n}\n\nUncached readable paragraph {n}.\n\n"))
            .collect::<String>();
        std::fs::write(root.join(deep), &source).unwrap();
        let started = std::time::Instant::now();
        reader.update_in(visual, |v, window, cx| {
            assert!(v.loading.as_ref().unwrap().active);
            assert!(!v.vault.inventory_complete);
            assert!(!v.vault.notes.iter().any(|note| note.path == deep));
            assert!(v.searcher.is_none());
            v.open_note(deep, None, window, cx);
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let readable = started.elapsed();
        reader.update_in(visual, |v, window, cx| {
            assert_eq!(v.current_rel, deep);
            assert!(v.note_source.contains("Fresh 299"));
            assert_eq!(v.content.read(cx).preparation_status(), Some(Ok(())));
            assert!(v.searcher.is_none());
            v.open_find(window, cx);
            v.find_input
                .update(cx, |input, cx| input.set_value("Uncached", window, cx));
            v.run_find(window, cx);
            assert_eq!(v.content.read(cx).search_status().1, 300);
        });
        // Find intentionally reveals its first match on the next layout.
        // Settle that user action before establishing the scroll baseline.
        visual.run_until_parked();
        for _ in 0..3 {
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.run_until_parked();
        }
        reader.update_in(visual, |v, _, cx| {
            v.content.read(cx).list_state().scroll_to(ListOffset {
                item_ix: 10,
                offset_in_item: px(7.),
            });
        });
        visual.update(|window, cx| window.draw(cx).clear(cx));
        visual.run_until_parked();
        let content = reader.read_with(visual, |v, cx| {
            let position = v.content.read(cx).list_state().logical_scroll_top();
            assert_eq!(
                position.item_ix, 10,
                "nonzero baseline established BEFORE inventory release"
            );
            assert_eq!(position.offset_in_item, px(7.));
            assert!(!v.vault.inventory_complete);
            v.content.entity_id()
        });
        release.try_send(()).unwrap();
        visual.run_until_parked();
        reader.read_with(visual, |v, cx| {
            assert_eq!(v.current_rel, deep);
            assert_eq!(
                v.content.entity_id(),
                content,
                "inventory must not replace unchanged selected document"
            );
            assert!(v.vault.inventory_complete);
            assert!(v.searcher.is_some());
            assert_eq!(
                v.content.read(cx).list_state().logical_scroll_top().item_ix,
                10
            );
            assert_eq!(
                v.content
                    .read(cx)
                    .list_state()
                    .logical_scroll_top()
                    .offset_in_item,
                px(7.)
            );
            assert_eq!(v.find_input.read(cx).value().as_ref(), "Uncached");
        });
        assert_eq!(std::fs::read_to_string(root.join(deep)).unwrap(), source);
        eprintln!("338 uncached deep >4KiB selected after publication, 5002 synthetic notes: readable={readable:?}; inventory held; no native timing claim");
    }

    #[gpui::test]
    fn first_document_and_input_work_before_inventory_release(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root =
            std::env::temp_dir().join(format!("tessera-progressive-ui-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let source = (0..200)
            .map(|i| format!("## Section {i}\n\nReadable paragraph {i}.\n\n"))
            .collect::<String>();
        std::fs::write(root.join("first.md"), &source).unwrap();
        for n in 0..5000 {
            std::fs::write(
                root.join(format!("note-{n}.md")),
                format!("# Synthetic {n}\n\n[[first]]"),
            )
            .unwrap();
        }
        let (release, hold) = async_channel::bounded(1);
        let mut reader = None;
        let start = std::time::Instant::now();
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("first.md".into()),
                        preparation_hold: Some(hold),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let first_frame = start.elapsed();
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let first = start.elapsed();
        reader.update_in(visual, |v, window, cx| {
            assert!(v.document_ready());
            assert_eq!(v.current_rel, "first.md");
            assert!(v.loading.as_ref().unwrap().active);
            assert!(v.searcher.is_none(), "positive unfinished-work control");
            assert!(!v.vault.inventory_complete);
            v.open_find(window, cx);
            v.find_input
                .update(cx, |input, cx| input.set_value("Readable", window, cx));
            v.run_find(window, cx);
            v.content.read(cx).list_state().scroll_to_reveal_item(100);
        });
        visual.run_until_parked();
        let input = start.elapsed();
        reader.update_in(visual, |v, window, cx| {
            assert_eq!(v.find_input.read(cx).value().as_ref(), "Readable");
            assert!(v.content.read(cx).list_state().logical_scroll_top().item_ix > 0);
            v.dismiss(window, cx);
            assert!(!v.loading.as_ref().unwrap().active);
            assert!(v.document_ready());
            assert_eq!(v.current_rel, "first.md");
        });
        let cancelled = start.elapsed();
        // An independently delayed worker phase remains held for >3 seconds.
        // Input and cancellation above ran while it was provably unfinished.
        let delayed_release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(3100));
            release
        });
        delayed_release.join().unwrap().try_send(()).unwrap();
        visual.run_until_parked();
        let teardown = start.elapsed();
        reader.update_in(visual, |v, _, _| {
            assert!(v.searcher.is_none());
            assert_eq!(v.current_rel, "first.md");
            assert_eq!(v.loading.as_ref().unwrap().phase, "Preparation cancelled");
        });
        eprintln!("progressive GPUI harness, 5001 synthetic notes: first_frame={first_frame:?}, first_document={first:?}, input_scroll={input:?}, cancel_ack={cancelled:?}, worker_teardown={teardown:?}; all input before >3s inventory release; whole_ready=cancelled; native timing NOT_RUN");
        assert_eq!(
            std::fs::read_to_string(root.join("first.md")).unwrap(),
            source
        );
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 5001);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[gpui::test]
    fn cancelled_prepublication_and_superseded_open_preserve_current_document(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let root = std::env::temp_dir().join(format!("tessera-open-race-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        std::fs::write(root.join("a/first.md"), "# Old document").unwrap();
        std::fs::write(root.join("b/second.md"), "# New document").unwrap();
        let (old_release, old_hold) = async_channel::bounded(1);
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.join("a")),
                        note: Some("first.md".into()),
                        preparation_hold: Some(old_hold),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        let (late_release, late_hold) = async_channel::bounded(1);
        reader.update_in(visual, |v, window, cx| {
            let content = v.content.entity_id();
            let history = v.history.clone();
            v.start_loading(
                Opts {
                    vault: Some(root.join("missing")),
                    validation_hold: Some(late_hold),
                    ..Default::default()
                },
                window,
                cx,
            );
            v.cancel_loading(cx);
            assert_eq!(v.content.entity_id(), content);
            assert_eq!(v.history, history);
            assert_eq!(v.vault_root, root.join("a"));
            assert!(v.document_ready());
        });
        let (new_release, new_hold) = async_channel::bounded(1);
        reader.update_in(visual, |v, window, cx| {
            v.start_loading(
                Opts {
                    vault: Some(root.join("b")),
                    note: Some("second.md".into()),
                    preparation_hold: Some(new_hold),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        visual.run_until_parked();
        old_release.try_send(()).unwrap();
        late_release.try_send(()).unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |v, _, cx| {
            assert_eq!(v.current_rel, "second.md");
            assert_eq!(v.vault_root, root.join("b"));
            assert_eq!(v.history, ["second.md"]);
            assert!(v.link_notice.is_none());
            assert!(v.loading.as_ref().unwrap().active);
            v.cancel_loading(cx);
        });
        new_release.try_send(()).unwrap();
        visual.run_until_parked();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn search_batch_uses_only_the_named_complete_generation_including_repairs() {
        use tessera_core::search::SearchDocument;
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("vault");
        let base = fixture.path().join("cache");
        std::fs::create_dir(&root).unwrap();
        for path in ["a.md", "b.md"] {
            std::fs::write(root.join(path), "source").unwrap();
        }
        let vault = Vault::scan(&root).unwrap();
        let documents = [
            SearchDocument {
                path: "a.md".into(),
                title: "A".into(),
                text: "initialcanary".into(),
            },
            SearchDocument {
                path: "b.md".into(),
                title: "B".into(),
                text: "retainedcanary".into(),
            },
        ];
        let baseline =
            Searcher::build_snapshot_in_memory(&vault, &documents, &mut |_, _| Ok(())).unwrap();
        let prior = base.join("generations").join("a".repeat(64));
        let completed = prior.with_extension("repairs").join("completed");
        std::fs::create_dir_all(&completed).unwrap();
        baseline.copy_committed_to(&completed).unwrap();
        std::fs::write(completed.join("complete"), b"1").unwrap();
        assert!(
            !prior.exists(),
            "Windows/checkpoint storage positive control"
        );
        let updated = [SearchDocument {
            path: "a.md".into(),
            title: "A".into(),
            text: "updatedcanary".into(),
        }];
        let cancel = Cancellation::default();
        let (first, generation, _first_pin) =
            prepare_search_batch(&vault, &updated, &[], &base, &prior, &cancel)
                .unwrap()
                .unwrap();
        assert!(first.search("initialcanary", 5).unwrap().is_empty());
        assert_eq!(first.search("updatedcanary", 5).unwrap().len(), 1);
        assert_eq!(first.search("retainedcanary", 5).unwrap().len(), 1);
        assert_eq!(baseline.search("initialcanary", 5).unwrap().len(), 1);
        let next = base.join("generations").join(generation);
        assert!(!next.exists());
        let (second, _, _) =
            prepare_search_batch(&vault, &[], &["b.md".into()], &base, &next, &cancel)
                .unwrap()
                .unwrap();
        assert_eq!(second.search("updatedcanary", 5).unwrap().len(), 1);
        assert!(second.search("retainedcanary", 5).unwrap().is_empty());
        assert_eq!(first.search("retainedcanary", 5).unwrap().len(), 1);
        // Other IDs' completed repairs cannot satisfy a missing baseline.
        let missing = base.join("generations").join("b".repeat(64));
        assert!(
            prepare_search_batch(&vault, &[], &[], &base, &missing, &cancel)
                .unwrap()
                .is_none()
        );
        // Index files copied before a crash are not published without the marker.
        let incomplete = missing.with_extension("repairs").join("incomplete");
        std::fs::create_dir_all(&incomplete).unwrap();
        first.copy_committed_to(&incomplete).unwrap();
        assert!(
            prepare_search_batch(&vault, &[], &[], &base, &missing, &cancel)
                .unwrap()
                .is_none()
        );
    }

    /// Each launch is a new process with only the persisted snapshot/search bank.
    #[test]
    fn second_unchanged_relaunch_reuses_sources_and_graph() {
        unchanged_relaunch_fixture(false, 0);
    }

    #[test]
    fn second_unchanged_relaunch_after_topology_delta_reuses_sources_and_graph() {
        unchanged_relaunch_fixture(true, 0);
    }

    #[test]
    fn persisted_relaunch_with_small_source_batch_refreshes_graph_and_search() {
        for changed in [1, 10] {
            unchanged_relaunch_fixture(false, changed);
        }
    }

    fn unchanged_relaunch_fixture(topology_delta: bool, changed_sources: usize) {
        let fixture = tempfile::Builder::new()
            .prefix("tessera-unchanged-relaunch-")
            .tempdir()
            .unwrap();
        let root = fixture.path().join("vault");
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::write(root.join("last.md"), "# Last\n\nunchangedcanary").unwrap();
        for n in 0..5000 {
            std::fs::write(
                root.join(format!("notes/note-{n}.md")),
                format!(
                    "# Note {n}\n\n[[last]]\n[Last](../last.md)\n{}",
                    "Stable paragraph.\n\n".repeat(40)
                ),
            )
            .unwrap();
            std::fs::File::options()
                .write(true)
                .open(root.join(format!("notes/note-{n}.md")))
                .unwrap()
                .set_times(
                    std::fs::FileTimes::new().set_modified(
                        std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + n),
                    ),
                )
                .unwrap();
        }
        for launch in 0..=2 {
            if launch == 1 {
                for n in 0..changed_sources {
                    std::fs::write(root.join(format!("notes/note-{n}.md")),
                        format!("# Changed {n}\n\nnewbatchcanary\n[[notes/note-4999]]\n[New](note-4999.md)")).unwrap();
                }
            }
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "reader_loading::tests::unchanged_relaunch_probe_child",
                    "--exact",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("TESSERA_UNCHANGED_RELAUNCH_FIXTURE", fixture.path())
                .env("TESSERA_UNCHANGED_RELAUNCH_NUMBER", launch.to_string())
                .env(
                    "TESSERA_RELAUNCH_CHANGED_SOURCES",
                    changed_sources.to_string(),
                )
                .env(
                    "TESSERA_RELAUNCH_TOPOLOGY_DELTA",
                    if topology_delta { "1" } else { "0" },
                )
                .status()
                .unwrap();
            assert!(status.success(), "fresh-process launch {launch}");
        }
    }

    #[test]
    fn unchanged_relaunch_probe_child() {
        let Some(fixture) = std::env::var_os("TESSERA_UNCHANGED_RELAUNCH_FIXTURE") else {
            return;
        };
        let fixture = PathBuf::from(fixture);
        let root = fixture.join("vault").canonicalize().unwrap();
        let launch: usize = std::env::var("TESSERA_UNCHANGED_RELAUNCH_NUMBER")
            .unwrap()
            .parse()
            .unwrap();
        let changed_sources: usize = std::env::var("TESSERA_RELAUNCH_CHANGED_SOURCES")
            .unwrap_or_else(|_| "0".into())
            .parse()
            .unwrap();
        #[cfg(target_os = "linux")]
        let calls = if std::env::var_os("TESSERA_SLOW_FS_MS").is_some() {
            #[link(name = "dl")]
            unsafe extern "C" {
                fn dlsym(
                    handle: *mut std::ffi::c_void,
                    name: *const std::ffi::c_char,
                ) -> *mut std::ffi::c_void;
            }
            unsafe {
                let set = dlsym(std::ptr::null_mut(), c"tessera_slow_fs_phase".as_ptr());
                let count = dlsym(std::ptr::null_mut(), c"tessera_slow_fs_count".as_ptr());
                assert!(
                    !set.is_null() && !count.is_null(),
                    "filesystem injection must be installed"
                );
                let set: unsafe extern "C" fn(*const std::ffi::c_char) = std::mem::transmute::<
                    *mut std::ffi::c_void,
                    unsafe extern "C" fn(*const std::ffi::c_char),
                >(set);
                let count: unsafe extern "C" fn(i32) -> std::ffi::c_ulong = std::mem::transmute::<
                    *mut std::ffi::c_void,
                    unsafe extern "C" fn(i32) -> std::ffi::c_ulong,
                >(count);
                set(c"positive_control".as_ptr());
                std::fs::read(root.join("last.md")).unwrap();
                tessera_core::vault::warm::SourceRevision::read(&root.join("last.md")).unwrap();
                assert!(
                    count(0) > 0 && count(1) > 0 && count(2) > 0,
                    "open/read/stat positive control"
                );
                set(c"warm_reconcile".as_ptr());
                Some(count)
            }
        } else {
            None
        };
        let state = fixture.join(format!("trace-{launch}"));
        let session = fixture.join("session");
        let trace = reader_diagnostics::Trace::new(Some(state.clone()), Some(root.clone()));
        let opts = Opts {
            vault: Some(root.clone()),
            note: Some("last.md".into()),
            index_dir: Some(fixture.join("cache")),
            session_directory: Some(session),
            diagnostics: Some(trace.clone()),
            ..Default::default()
        };
        let started = std::time::Instant::now();
        let first = prepare_first(&opts, &Cancellation::default()).unwrap();
        let first_ms = started.elapsed().as_secs_f64() * 1000.;
        let Event::First {
            snapshot,
            vault: first_vault,
            searcher: first_searcher,
            ..
        } = first
        else {
            panic!("first publication")
        };
        if launch > 0 {
            assert_eq!(first_vault.notes.len(), 5001, "cached first inventory");
            assert!(
                first_searcher.is_some(),
                "persisted search generation must open before reconcile"
            );
        }
        let (send, _receive) = async_channel::unbounded();
        let ready = prepare_rest_with_snapshot(
            &root,
            &opts,
            &Cancellation::default(),
            &send,
            snapshot.map(|snapshot| *snapshot),
        )
        .unwrap();
        let ready_ms = started.elapsed().as_secs_f64() * 1000.;
        if launch == 0 && std::env::var("TESSERA_RELAUNCH_TOPOLOGY_DELTA").as_deref() == Ok("1") {
            let Event::Ready {
                move_snapshot,
                vault,
                searcher: Some(searcher),
                ..
            } = ready
            else {
                panic!("ready baseline")
            };
            let mut state =
                tessera_core::vault::warm::incremental::State::new(vault, *move_snapshot);
            let session = searcher.fork_session().unwrap();
            let transient = root.join("transient.md");
            for present in [true, false] {
                if present {
                    std::fs::write(&transient, "# Transient\n\n[[last]]").unwrap();
                } else {
                    std::fs::remove_file(&transient).unwrap();
                }
                let mut changes = tessera_core::Changes::default();
                if present {
                    changes.changed.insert("transient.md".into());
                } else {
                    changes.removed.insert("transient.md".into());
                }
                let batch = state.apply(&changes, &mut |_, _| Ok(())).unwrap();
                let documents: Vec<_> = batch
                    .affected
                    .iter()
                    .filter_map(|path| {
                        Some(tessera_core::search::SearchDocument {
                            path: path.clone(),
                            title: path.clone(),
                            text: state.snapshot.source(path)?,
                        })
                    })
                    .collect();
                session
                    .update_snapshot_batch(
                        &state.vault,
                        &documents,
                        &batch.removed.into_iter().collect::<Vec<_>>(),
                    )
                    .unwrap();
            }
            let generation = "abcdef0123456789".repeat(4);
            let cache = fixture.join("cache");
            let destination = cache.join("generations").join(&generation);
            std::fs::create_dir_all(&destination).unwrap();
            session.copy_committed_to(&destination).unwrap();
            std::fs::write(destination.join("complete"), b"1").unwrap();
            state.set_search_generation(Some(generation));
            state.persist_delta(&cache).unwrap();
        } else {
            let Event::Ready {
                vault,
                searcher: Some(searcher),
                ..
            } = ready
            else {
                panic!("ready search");
            };
            if changed_sources > 0 && launch > 0 {
                assert_eq!(
                    searcher.search("newbatchcanary", 20).unwrap().len(),
                    changed_sources
                );
                assert_eq!(
                    vault.backlinks("notes/note-4999.md").len(),
                    2 * changed_sources
                );
                assert_eq!(
                    vault.backlinks("last.md").len(),
                    2 * (5000 - changed_sources)
                );
                // First's prior generation stays immutable while reconcile commits.
                assert_eq!(
                    first_searcher
                        .unwrap()
                        .search("newbatchcanary", 20)
                        .unwrap()
                        .len(),
                    if launch == 1 { 0 } else { changed_sources }
                );
            }
        }
        trace.event("unchanged_relaunch_probe_done", serde_json::json!({}));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let events = loop {
            let events: Vec<serde_json::Value> =
                std::fs::read_to_string(state.join("reader-diagnostic.log"))
                    .unwrap_or_default()
                    .lines()
                    .filter_map(|line| serde_json::from_str(line).ok())
                    .collect();
            if events
                .iter()
                .any(|e| e["phase"] == "unchanged_relaunch_probe_done")
            {
                break events;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "diagnostic delivery positive control"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        let stats: Vec<_> = events
            .iter()
            .filter(|e| e["phase"] == "reconcile_stats")
            .map(|e| e["details"].clone())
            .collect();
        let replay = events
            .iter()
            .find(|e| e["phase"] == "replay_invalidations")
            .unwrap()["details"]
            .clone();
        assert!(!stats.is_empty());
        if launch == 0 {
            assert_eq!(stats[0]["read"], 5001, "cold read counter positive control");
            assert_eq!(stats[0]["reused"], 0);
        }
        if launch == 1 && changed_sources > 0 {
            let batch = events
                .iter()
                .find(|event| event["phase"] == "search_incremental_prepare")
                .expect("incremental search must run against persisted generation");
            assert_eq!(batch["details"]["updated"], changed_sources);
            let graph = events
                .iter()
                .find(|event| {
                    event["phase"] == "reconcile_phase"
                        && event["details"]["name"] == "Preparing backlinks"
                })
                .expect("graph refresh positive control");
            if std::env::var_os("TESSERA_SLOW_FS_MS").is_some() {
                assert!(
                    graph["details"]["duration_ms"].as_f64().unwrap() < 300.,
                    "bounded graph refresh budget"
                );
            }
            eprintln!(
                "CHANGED_RELAUNCH graph_ms={} search_ms={}",
                graph["details"]["duration_ms"],
                events
                    .iter()
                    .find(|e| e["phase"] == "background_search_prepare")
                    .unwrap()["details"]["duration_ms"]
            );
            for s in &stats {
                assert_eq!(s["read"], changed_sources);
                assert_eq!(s["reused"], 5001 - changed_sources);
                assert_eq!(s["graph_reused"], true);
                assert_eq!(s["graph_updated_sources"], changed_sources);
            }
        } else if launch > 0 {
            let search_lookups: Vec<_> = events
                .iter()
                .filter(|event| event["phase"] == "search_generation_cache")
                .collect();
            assert!(
                !search_lookups.is_empty(),
                "search cache lookup positive control"
            );
            assert!(
                search_lookups
                    .iter()
                    .all(|event| event["details"]["hit"] == true),
                "unchanged relaunch must not rebuild search"
            );
            for s in &stats {
                assert_eq!(
                    s["read"], 0,
                    "unchanged fresh relaunch must not read sources: {s}"
                );
                assert_eq!(s["reused"], 5001);
                assert_eq!(s["graph_reused"], true);
                assert_eq!(s["replay_force_all"], false);
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(count) = calls {
            eprintln!(
                "ACTUAL_FS_CALLS launch={launch} {:?}",
                (0..4).map(|i| unsafe { count(i) }).collect::<Vec<_>>()
            );
        }
        eprintln!("UNCHANGED_RELAUNCH launch={launch} first_ms={first_ms:.2} ready_ms={ready_ms:.2} stats={} replay={replay}",serde_json::to_string(&stats).unwrap());
    }

    #[test]
    fn cold_warm_cache_is_external_complete_and_immutable() {
        let fixture =
            std::env::temp_dir().join(format!("tessera-cache-generation-{}", uuid::Uuid::new_v4()));
        let root = fixture.join("source");
        let cache = fixture.join("cache");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.md"), "# Alpha\n\noldword").unwrap();
        for folder in 0..10 {
            let directory = root.join(format!("section-{folder}"));
            std::fs::create_dir_all(&directory).unwrap();
            for note in 0..500 {
                std::fs::write(
                    directory.join(format!("synthetic-{note}.md")),
                    format!("# Synthetic {folder} {note}\n\n[[a]] generated corpus"),
                )
                .unwrap();
            }
        }
        let before = source_manifest(&root);
        let opts = Opts {
            vault: Some(root.clone()),
            index_dir: Some(cache.clone()),
            ..Default::default()
        };
        let (send, _receive) = async_channel::unbounded();
        let started = std::time::Instant::now();
        let first = prepare_rest(&root, &opts, &Cancellation::default(), &send).unwrap();
        let cold_ready = started.elapsed();
        let Event::Ready {
            searcher: Some(first),
            ..
        } = first
        else {
            panic!("ready")
        };
        assert_eq!(first.search("oldword", 10).unwrap().len(), 1);
        let entries = std::fs::read_dir(cache.join("generations"))
            .unwrap()
            .count();
        let started = std::time::Instant::now();
        let warm = prepare_rest(&root, &opts, &Cancellation::default(), &send).unwrap();
        let warm_ready = started.elapsed();
        assert_eq!(
            source_manifest(&root),
            before,
            "all source names/bytes unchanged by cold and warm preparation"
        );
        eprintln!("5001 nested synthetic notes: cold_whole_ready={cold_ready:?}, warm_whole_ready={warm_ready:?}; same host/session, external cache");
        assert!(matches!(warm, Event::Ready { .. }));
        assert_eq!(
            std::fs::read_dir(cache.join("generations"))
                .unwrap()
                .count(),
            entries
        );
        std::fs::write(root.join("a.md"), "# Alpha\n\nnewword").unwrap();
        let next = prepare_rest(&root, &opts, &Cancellation::default(), &send).unwrap();
        let Event::Ready {
            searcher: Some(next),
            ..
        } = next
        else {
            panic!("ready")
        };
        assert_eq!(next.search("newword", 10).unwrap().len(), 1);
        assert_eq!(
            first.search("oldword", 10).unwrap().len(),
            1,
            "active previous generation remains readable"
        );
        assert!(validate_external_cache(&root.join("derived"), &root).is_err());
        assert!(validate_external_cache(&fixture.join("absent/../source"), &root).is_err());
        assert!(!fixture.join("absent").exists(), "validation is read-only");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 11);
        assert_eq!(
            std::fs::read_to_string(root.join("a.md")).unwrap(),
            "# Alpha\n\nnewword"
        );
        drop((first, next, warm));
        std::fs::remove_dir_all(fixture).unwrap();
    }
    #[gpui::test]
    fn inventory_completion_does_not_reopen_initial_document(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let fixture =
            std::env::temp_dir().join(format!("tessera-ready-navigation-{}", uuid::Uuid::new_v4()));
        let root = fixture.join("source");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("first.md"), "# First").unwrap();
        std::fs::write(
            root.join("second.md"),
            "# Second\n\nReadable second document",
        )
        .unwrap();
        let before = source_manifest(&root);
        let (release, hold) = async_channel::bounded(1);
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        note: Some("first.md".into()),
                        index_dir: Some(fixture.join("cache")),
                        preparation_hold: Some(hold),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            assert!(v.document_ready());
            assert!(v.searcher.is_none());
            v.open_note("second.md", None, window, cx);
        });
        visual.run_until_parked();
        let (content, history) = reader.update_in(visual, |v, _, _| {
            assert_eq!(v.current_rel, "second.md");
            (v.content.entity_id(), v.history.clone())
        });
        release.try_send(()).unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |v, _, _| {
            assert_eq!(v.current_rel, "second.md");
            assert_eq!(v.content.entity_id(), content);
            assert_eq!(v.history, history);
            assert!(v.vault.inventory_complete);
            assert!(
                v.searcher.is_some(),
                "uncancelled completion positive control"
            );
            assert_eq!(v.loading.as_ref().unwrap().phase, "Ready");
        });
        assert_eq!(source_manifest(&root), before);
        std::fs::create_dir_all(fixture.join("empty")).unwrap();
        reader.update_in(visual, |v, window, cx| {
            v.start_loading(
                Opts {
                    vault: Some(fixture.join("empty")),
                    index_dir: Some(fixture.join("empty-cache")),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        visual.run_until_parked();
        reader.update_in(visual, |v, _, _| {
            assert!(v.document_ready(), "previous document remains usable");
            assert_eq!(v.current_rel, "second.md");
            assert_eq!(v.content.entity_id(), content);
            assert_eq!(v.history, history);
            assert!(!v.loading.as_ref().unwrap().published);
            assert!(!v.loading.as_ref().unwrap().active);
            assert!(v.loading.as_ref().unwrap().phase.contains("no readable"));
        });
        std::fs::remove_dir_all(fixture).unwrap();
    }
    #[gpui::test]
    fn shared_session_history_records_only_published_documents_and_is_root_specific(
        cx: &mut TestAppContext,
    ) {
        use crate::reader_history::ReadingHistory;
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let fixture =
            std::env::temp_dir().join(format!("tessera-shared-history-{}", uuid::Uuid::new_v4()));
        let root = fixture.join("a");
        let other = fixture.join("b");
        let state = fixture.join("state");
        let cache = fixture.join("cache");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(root.join("first.md"), "# First").unwrap();
        std::fs::write(root.join("last.md"), "# Last").unwrap();
        std::fs::write(other.join("other.md"), "# Other root").unwrap();
        ReadingHistory::record_usable_document(&state, &root, "last.md").unwrap();
        let (release, hold) = async_channel::bounded(1);
        let mut reader = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                Reader::new(
                    Opts {
                        vault: Some(root.clone()),
                        index_dir: Some(cache.clone()),
                        session_directory: Some(state.clone()),
                        preparation_hold: Some(hold),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            });
            reader = Some(view.clone());
            Root::new(view, window, cx)
        });
        let reader = reader.unwrap();
        visual.run_until_parked();
        reader.update_in(visual, |v, window, cx| {
            assert_eq!(
                v.current_rel, "last.md",
                "shared saved hint selected before inventory"
            );
            v.open_note("first.md", None, window, cx);
        });
        visual.run_until_parked();
        assert_eq!(
            ReadingHistory::last_document(&state, &root)
                .unwrap()
                .as_deref(),
            Some("first.md")
        );
        // Reconcile publication can trail navigation; the first frame of the
        // next launch must use the newly selected source without hydrating it.
        let (send, _receive) = async_channel::unbounded();
        drop(
            prepare_rest(
                &root,
                &Opts {
                    note: Some("last.md".into()),
                    index_dir: Some(cache.clone()),
                    session_directory: Some(state.clone()),
                    ..Default::default()
                },
                &Cancellation::default(),
                &send,
            )
            .unwrap(),
        );
        std::fs::write(root.join("first.md"), [0xff]).unwrap();
        let Event::First { document, .. } = prepare_first(
            &Opts {
                vault: Some(root.clone()),
                index_dir: Some(cache.clone()),
                session_directory: Some(state.clone()),
                ..Default::default()
            },
            &Cancellation::default(),
        )
        .unwrap() else {
            panic!("cached First expected")
        };
        let (rel, document) = document.unwrap();
        assert_eq!(rel, "first.md");
        assert!(document.source.contains("First"));
        let (release_validation, validation_hold) = async_channel::bounded(1);
        reader.update_in(visual, |v, window, cx| {
            v.start_loading(
                Opts {
                    vault: Some(other.clone()),
                    validation_hold: Some(validation_hold),
                    ..Default::default()
                },
                window,
                cx,
            );
            v.cancel_loading(cx);
        });
        release.try_send(()).unwrap();
        release_validation.try_send(()).unwrap();
        visual.run_until_parked();
        assert_eq!(ReadingHistory::last_document(&state, &other).unwrap(), None);
        let Event::First { document, .. } = prepare_first(
            &Opts {
                vault: Some(other.clone()),
                session_directory: Some(state.clone()),
                ..Default::default()
            },
            &Cancellation::default(),
        )
        .unwrap() else {
            panic!("first")
        };
        assert_eq!(document.unwrap().0, "other.md");
        reader.update_in(visual, |v, _, _| assert_eq!(v.vault_root, root));
        std::fs::remove_dir_all(fixture).unwrap();
    }
}

/// A directory is enumerated only after publication or an explicit expansion.
/// No recursive scanner, source reads, watcher, index, or cache participates.
fn quick_folder(
    root: &Path,
    folder: &str,
    previous: &[tessera_core::vault::VaultEntry],
    cancel: &Cancellation,
) -> Result<Vault> {
    use tessera_core::vault::{EntryKind, VaultEntry};
    let directory = root.join(folder).canonicalize()?;
    if !directory.starts_with(root) {
        bail!("Folder is outside the quick viewer root");
    }
    let mut entries: Vec<_> = previous
        .iter()
        .filter(|entry| Path::new(&entry.path).parent() != Some(Path::new(folder)))
        .cloned()
        .collect();
    let mut warnings = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        cancel.check()?;
        let entry = entry?;
        let kind = entry.file_type()?;
        // Match the Reader inventory policy: do not follow symlink entries.
        if kind.is_symlink() {
            continue;
        }
        let entry_path = entry.path();
        let Some(path) = entry_path.strip_prefix(root)?.to_str() else {
            warnings.push(tessera_core::vault::UnreadableEntry {
                path: entry_path,
                operation: "list siblings",
                error: "Filename is not valid UTF-8".into(),
            });
            continue;
        };
        let path = path.replace('\\', "/");
        let kind = if kind.is_dir() {
            EntryKind::Directory
        } else if kind.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("md"))
        {
            EntryKind::Markdown
        } else if kind.is_file() {
            EntryKind::Attachment
        } else {
            continue;
        };
        entries.push(VaultEntry { path, kind });
    }
    let mut vault = quick_vault(root, entries);
    vault.inventory_complete = warnings.is_empty();
    vault.unreadable = warnings;
    Ok(vault)
}

fn quick_vault(root: &Path, mut entries: Vec<tessera_core::vault::VaultEntry>) -> Vault {
    use tessera_core::vault::EntryKind;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let mut vault = Vault::from_note_paths(
        entries
            .iter()
            .filter(|e| e.kind == EntryKind::Markdown)
            .map(|e| e.path.clone()),
    );
    vault.root = root.to_owned();
    vault.entries = entries;
    // Complete within the explicitly browsed folders, never a vault-wide graph.
    vault.inventory_complete = true;
    vault.inventory_scanned = true;
    vault.single_file = true;
    vault
}

impl Reader {
    fn refresh_quick_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.vault_root.clone();
        let rel = self.current_rel.clone();
        let navigation = self.navigation_generation;
        cx.spawn_in(window, async move |this, cx| {
            let path = root.join(&rel);
            let source = cx
                .background_executor()
                .spawn(async move { std::fs::read_to_string(path) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.single_file
                    || this.vault_root != root
                    || this.navigation_generation != navigation
                {
                    return;
                }
                let sources = source
                    .ok()
                    .map(|source| (rel, source))
                    .into_iter()
                    .collect();
                this.reconcile_inventory_document(&sources, window, cx);
            });
        })
        .detach();
    }

    fn publish_quick_folder(&mut self, vault: Vault, cx: &mut Context<Self>) {
        self.vault = Arc::new(vault);
        self.quick_open.inventory = Some(Arc::new(tessera_core::quick_open::inventory(
            self.vault.notes.clone(),
            &self.vault.entries,
        )));
        self.sync_tree();
        self.refresh_link_preparation(cx);
        self.refresh_quick_open(cx);
        cx.notify();
    }

    pub(crate) fn load_quick_folder(&mut self, folder: String, cx: &mut Context<Self>) {
        let root = self.vault_root.clone();
        let generation = self.loading.as_ref().map(|l| l.generation);
        cx.spawn(async move |this, cx| {
            let expected_root = root.clone();
            let expected_folder = folder.clone();
            let result = cx
                .background_executor()
                .spawn(async move { quick_folder(&root, &folder, &[], &Cancellation::default()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if !this.single_file
                    || this.vault_root != expected_root
                    || this.loading.as_ref().map(|l| l.generation) != generation
                {
                    return;
                }
                match result {
                    Ok(vault) => {
                        let mut entries: Vec<_> = this
                            .vault
                            .entries
                            .iter()
                            .filter(|entry| {
                                Path::new(&entry.path).parent() != Some(Path::new(&expected_folder))
                            })
                            .cloned()
                            .collect();
                        entries.extend(vault.entries);
                        let mut updated = quick_vault(&expected_root, entries);
                        updated.unreadable = this
                            .vault
                            .unreadable
                            .iter()
                            .filter(|warning| {
                                warning.path.parent()
                                    != Some(expected_root.join(&expected_folder).as_path())
                            })
                            .cloned()
                            .collect();
                        updated.unreadable.extend(vault.unreadable);
                        updated.inventory_complete = updated.unreadable.is_empty();
                        this.publish_quick_folder(updated, cx);
                    }
                    Err(error) => {
                        this.link_notice = Some(format!("Cannot list folder: {error:#}").into());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod quick_view_tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn quick_first_has_no_cache_and_siblings_are_shallow() {
        let directory =
            std::env::temp_dir().join(format!("tessera-quick-{}", uuid::Uuid::new_v4()));
        let root = directory.join("notes");
        std::fs::create_dir_all(root.join("child")).unwrap();
        std::fs::create_dir(root.join(".obsidian")).unwrap();
        std::fs::write(
            root.join("start.md"),
            "# Start\n\n![image](picture.png)\n\n[[sibling]]",
        )
        .unwrap();
        std::fs::write(root.join("sibling.md"), "# Sibling").unwrap();
        std::fs::write(root.join("child/deep.md"), "# Deep").unwrap();
        std::fs::write(root.join("picture.png"), b"image").unwrap();
        let cache = directory.join("cache");
        let opts = Opts {
            open_path: Some(root.join("start.md")),
            index_dir: Some(cache.clone()),
            ..Default::default()
        };
        let Event::First {
            intent,
            vault,
            document,
            cache_lease,
            snapshot,
            searcher,
            ..
        } = prepare_first(&opts, &Cancellation::default()).unwrap()
        else {
            panic!("first document");
        };
        assert!(intent.single_file);
        assert_eq!(
            vault.notes.len(),
            1,
            "first publication never enumerates siblings"
        );
        assert!(document.unwrap().1.source.contains("Start"));
        assert!(cache_lease.is_none() && snapshot.is_none() && searcher.is_none());
        let siblings = quick_folder(&root, "", &[], &Cancellation::default()).unwrap();
        assert!(siblings.entries.iter().any(|e| e.path == "child"));
        assert!(siblings.entries.iter().any(|e| e.path == "picture.png"));
        assert!(!siblings.entries.iter().any(|e| e.path == "child/deep.md"));
        assert!(matches!(
            siblings.resolve("sibling"),
            tessera_core::vault::Resolution::Resolved { .. }
        ));
        assert!(matches!(
            siblings.resolve_markdown("child/deep.md", "start.md"),
            tessera_core::vault::Resolution::Resolved { .. }
        ));
        assert!(matches!(
            siblings.resolve_markdown("/child/deep.md", "start.md"),
            tessera_core::vault::Resolution::Resolved { .. }
        ));
        assert_eq!(
            siblings.resolve_asset("picture.png", "start.md"),
            Some(root.join("picture.png"))
        );
        let expanded =
            quick_folder(&root, "child", &siblings.entries, &Cancellation::default()).unwrap();
        assert!(expanded.entries.iter().any(|e| e.path == "child/deep.md"));
        assert!(!cache.exists(), "neither quick phase writes an index");
        let (send, _receive) = async_channel::unbounded();
        let full = Opts {
            vault: Some(root.clone()),
            index_dir: Some(cache.clone()),
            ..Default::default()
        };
        assert!(matches!(
            prepare_rest(&root, &full, &Cancellation::default(), &send).unwrap(),
            Event::Ready {
                searcher: Some(_),
                ..
            }
        ));
        assert!(cache.exists(), "positive full-vault index creation control");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn quick_inventory_preserves_wikilink_ambiguity() {
        use tessera_core::vault::{EntryKind, Resolution, VaultEntry};
        let vault = quick_vault(
            Path::new("/notes"),
            ["one/same.md", "two/same.md"]
                .into_iter()
                .map(|path| VaultEntry {
                    path: path.into(),
                    kind: EntryKind::Markdown,
                })
                .collect(),
        );
        assert!(
            matches!(vault.resolve("same"), Resolution::Ambiguous { candidates } if candidates.len() == 2)
        );
        assert!(
            matches!(vault.resolve("one/same"), Resolution::Resolved { path } if path == "one/same.md")
        );
    }

    #[cfg(unix)]
    #[test]
    fn quick_siblings_keep_usable_entries_when_a_name_is_not_utf8() {
        use std::os::unix::ffi::OsStringExt;
        let root =
            std::env::temp_dir().join(format!("tessera-quick-names-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("valid.md"), "# Valid").unwrap();
        std::fs::write(
            root.join(std::ffi::OsString::from_vec(b"invalid-\xff.md".to_vec())),
            "# Other",
        )
        .unwrap();
        let vault = quick_folder(&root, "", &[], &Cancellation::default()).unwrap();
        assert_eq!(vault.notes.len(), 1);
        assert_eq!(vault.notes[0].path, "valid.md");
        assert_eq!(vault.unreadable.len(), 1);
        assert!(!vault.inventory_complete);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn quick_upgrade_preserves_relative_image_and_viewport(cx: &mut TestAppContext) {
        check_quick_upgrade(cx, false);
    }

    #[gpui::test]
    fn quick_upgrade_reconciles_edits_without_losing_image_or_viewport(cx: &mut TestAppContext) {
        check_quick_upgrade(cx, true);
    }

    fn check_quick_upgrade(cx: &mut TestAppContext, edit_during_scan: bool) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let directory =
            std::env::temp_dir().join(format!("tessera-upgrade-{}", uuid::Uuid::new_v4()));
        let root = directory.join("notes");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("img.png"), b"image resolution fixture").unwrap();
        std::fs::write(
            root.join("start.md"),
            format!(
                "# Start\n\n![image](./img.png)\n\n{}",
                "Paragraph.\n\n".repeat(80)
            ),
        )
        .unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(root.join("start.md")),
                        session_directory: Some(directory.join("state")),
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
        let (content, source) = reader.read_with(visual, |reader, cx| {
            assert!(reader.single_file);
            assert!(reader.note_source.contains("file://"));
            assert!(reader.note_source.contains("img.png"));
            reader.content.read(cx).list_state().scroll_to(ListOffset {
                item_ix: 12,
                offset_in_item: px(3.),
            });
            (reader.content.clone(), reader.note_source.clone())
        });
        assert_eq!(
            crate::reader_history::ReadingHistory::quick_document(&directory.join("state"), &root)
                .unwrap()
                .as_deref(),
            Some("start.md")
        );
        let (release, hold) = async_channel::bounded(1);
        reader.update_in(visual, |reader, window, cx| {
            reader.start_loading(
                Opts {
                    vault: Some(root.clone()),
                    note: Some("start.md".into()),
                    index_dir: Some(directory.join("cache")),
                    preparation_hold: Some(hold),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        visual.run_until_parked();
        for ready in [false, true] {
            if ready {
                if edit_during_scan {
                    let path = root.join("start.md");
                    let raw = std::fs::read_to_string(&path).unwrap();
                    std::fs::write(path, format!("{raw}\nScan edit positive control\n")).unwrap();
                }
                release.try_send(()).unwrap();
                visual.run_until_parked();
                // Source replacement restores its viewport after parser readiness.
                visual.executor().advance_clock(Duration::from_millis(300));
                visual.run_until_parked();
            }
            reader.read_with(visual, |reader, cx| {
                assert!(!reader.single_file);
                assert_eq!(reader.searcher.is_some(), ready);
                if ready && edit_during_scan {
                    assert!(reader.note_source.contains("Scan edit positive control"));
                    assert!(reader.note_source.contains("file://"));
                    assert!(reader.note_source.contains("img.png"));
                } else {
                    assert_eq!(
                        reader.content, content,
                        "upgrade retains the rendered document"
                    );
                    assert_eq!(
                        reader.note_source, source,
                        "relative image survives publication and inventory"
                    );
                }
                let position = reader.content.read(cx).list_state().logical_scroll_top();
                assert_eq!(position.item_ix, 12);
                assert_eq!(position.offset_in_item, px(3.));
            });
        }
        assert_eq!(
            crate::reader_history::ReadingHistory::quick_document(&directory.join("state"), &root)
                .unwrap(),
            None,
            "upgrade persists vault mode even when keeping the rendered document"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn quick_reader_navigation_refresh_and_upgrade(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_keys(cx);
        });
        let directory =
            std::env::temp_dir().join(format!("tessera-quick-ui-{}", uuid::Uuid::new_v4()));
        let root = directory.join("notes");
        let cache = directory.join("index");
        let state = directory.join("state");
        std::fs::create_dir_all(root.join("child")).unwrap();
        std::fs::write(root.join("start.md"), "# Start\n\n[[sibling]]").unwrap();
        std::fs::write(root.join("sibling.md"), "# Sibling").unwrap();
        std::fs::write(root.join("child/deep.md"), "# Deep").unwrap();
        let mut entity = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            let reader = cx.new(|cx| {
                Reader::new(
                    Opts {
                        open_path: Some(root.join("start.md")),
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
        reader.update_in(visual, |reader, window, cx| {
            assert!(reader.single_file && reader.document_ready());
            assert!(
                reader.index_dir.is_none() && reader.searcher.is_none() && reader.watcher.is_none()
            );
            assert!(reader.vault.entries.iter().any(|e| e.path == "child"));
            assert!(!reader
                .vault
                .entries
                .iter()
                .any(|e| e.path == "child/deep.md"));
            reader.open_note("sibling.md", None, window, cx);
        });
        visual.run_until_parked();
        reader.update_in(visual, |reader, window, cx| {
            assert_eq!(reader.current_rel, "sibling.md");
            reader.refresh_inventory(Default::default(), window, cx);
        });
        visual.run_until_parked();
        #[cfg(unix)]
        {
            reader.update_in(visual, |reader, window, cx| {
                reader.toggle_source(window, cx)
            });
            visual.run_until_parked();
            let input = reader.read_with(visual, |reader, _| {
                reader.editing.as_ref().unwrap().test_input()
            });
            input.update_in(visual, |input, window, cx| {
                input.replace_text_in_range(Some(0..0), "edited ", window, cx);
            });
            visual.run_until_parked();
            reader.update_in(visual, |reader, _, cx| assert!(reader.save_source(cx)));
            visual.run_until_parked();
            assert_eq!(
                std::fs::read_to_string(root.join("sibling.md")).unwrap(),
                "edited # Sibling"
            );
        }
        assert!(!cache.exists());
        assert_eq!(
            crate::reader_history::ReadingHistory::quick_document(&state, &root)
                .unwrap()
                .as_deref(),
            Some("sibling.md")
        );
        reader.update_in(visual, |reader, window, cx| {
            assert!(reader.single_file && reader.searcher.is_none());
            reader.start_loading(
                Opts {
                    vault: Some(root.clone()),
                    note: Some(reader.current_rel.clone()),
                    index_dir: Some(cache.clone()),
                    ..Default::default()
                },
                window,
                cx,
            );
        });
        visual.run_until_parked();
        reader.read_with(visual, |reader, _| {
            assert!(!reader.single_file);
            assert!(reader.searcher.is_some());
            assert_eq!(reader.current_rel, "sibling.md");
        });
        assert!(cache.exists());
        assert_eq!(
            crate::reader_history::ReadingHistory::quick_document(&state, &root).unwrap(),
            None
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
