//! Read-only archive UI, successor note lookup, and quick-viewer "Open in vault" (VD2).
use super::*;
use okilum_core::vault_descriptor::{ArchiveState, PathMap, Successor};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

const MAX_MAP_BYTES: u64 = 64 * 1024 * 1024;

/// Copy rule (Oleg 24.11): UI copy always says generic "vault"; a vault's name
/// appears only as quoted data.
pub(crate) const READ_ONLY_ARCHIVE_LABEL: &str = "Read-only archive";

pub(crate) fn open_in_vault_label(name: &str) -> String {
    format!("Open in vault \"{name}\"")
}

pub(crate) fn locate_vault_label(name: &str) -> String {
    format!("Locate vault \"{name}\"\u{2026}")
}

pub(crate) fn open_path_map_label(name: &str) -> String {
    format!("Open path map in vault \"{name}\"")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QuickViewerVault {
    pub root: PathBuf,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArchiveLookupNotice {
    pub message: String,
    pub path_map: Option<PathMapFallback>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PathMapFallback {
    pub successor_root: PathBuf,
    pub expected_vault_id: String,
    pub note: String,
    pub vault_name: String,
}

/// Successor vaults explicitly located by the user via "Locate vault …" in this session.
#[derive(Default)]
pub(crate) struct LocatedSuccessors(pub BTreeMap<String, PathBuf>);
impl Global for LocatedSuccessors {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SuccessorMatch {
    Found(PathBuf),
    NotFound,
    Ambiguous(Vec<PathBuf>),
}

/// Resolve a successor vault strictly by its canonical `vault_id` UUID.
/// Matching display or folder name alone is never sufficient.
pub(crate) fn resolve_successor_root(
    archive_root: &Path,
    expected_vault_id: &str,
    open_roots: &[PathBuf],
    known_roots: &[PathBuf],
    located: Option<&Path>,
) -> SuccessorMatch {
    let canonical_archive = archive_root
        .canonicalize()
        .unwrap_or_else(|_| archive_root.to_path_buf());
    let mut matches = BTreeSet::new();
    for candidate in open_roots
        .iter()
        .chain(known_roots.iter())
        .map(PathBuf::as_path)
        .chain(located)
    {
        let Ok(canonical) = candidate.canonicalize() else {
            continue;
        };
        if canonical == canonical_archive || !canonical.is_dir() {
            continue;
        }
        if okilum_core::vault_link_identity::read(&canonical)
            .ok()
            .flatten()
            .as_deref()
            == Some(expected_vault_id)
        {
            matches.insert(canonical);
        }
    }
    match matches.len() {
        0 => SuccessorMatch::NotFound,
        1 => SuccessorMatch::Found(matches.into_iter().next().unwrap()),
        _ => SuccessorMatch::Ambiguous(matches.into_iter().collect()),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LookupOutcome {
    Resolved { target: String },
    Retired { reason: Option<String> },
    Ambiguous { candidates: Vec<String> },
    Unresolved,
    Error(String),
}

#[derive(Deserialize)]
struct CliLookupResponse {
    status: String,
    old_path: Option<String>,
    path: Option<String>,
    #[serde(default)]
    candidates: Vec<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct JsonlRow {
    old_path: String,
    new_path: Option<String>,
    #[serde(default)]
    action: String,
    #[serde(default)]
    reason: String,
}

fn verify_successor_identity(root: &Path, expected_vault_id: &str) -> Result<(), String> {
    match okilum_core::vault_link_identity::read(root) {
        Ok(Some(id)) if id == expected_vault_id => Ok(()),
        Ok(_) => Err("The successor vault identity no longer matches.".into()),
        Err(error) => Err(format!("Cannot verify successor vault identity: {error:#}")),
    }
}

fn verify_target_file(successor_root: &Path, target: &str) -> Result<String, String> {
    if !is_safe_relative(target) {
        return Err("The mapped destination path is invalid.".into());
    }
    let checked = reader_files::checked_path(successor_root, target).map_err(|_| {
        format!("The mapped note \"{target}\" is outside or missing from the vault.")
    })?;
    if !checked.is_file() {
        return Err(format!(
            "The mapped note \"{target}\" does not exist in the vault."
        ));
    }
    Ok(target.to_owned())
}

fn is_safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', '\0'])
        && path
            .split('/')
            .all(|part| !part.is_empty() && !matches!(part, "." | ".."))
        && Path::new(path)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
}

/// Look up `source_path` in the verified successor vault using the configured
/// `brain where` CLI adapter when present, falling back to reading the sealed
/// `@views/path-map.jsonl` directly. Lookup runs only after an explicit click.
pub(crate) fn lookup_successor_note(
    successor_root: &Path,
    expected_vault_id: &str,
    path_map: &PathMap,
    source_path: &str,
) -> LookupOutcome {
    if let Err(message) = verify_successor_identity(successor_root, expected_vault_id) {
        return LookupOutcome::Error(message);
    }
    let jsonl_rel = path_map.jsonl_path();
    let jsonl_abs = match reader_files::checked_path(successor_root, &jsonl_rel) {
        Ok(path) if path.is_file() => path,
        _ => {
            return LookupOutcome::Error(format!(
                "Path map \"{jsonl_rel}\" is unavailable in the successor vault."
            ));
        }
    };
    if path_map.command.as_deref() == Some("brain where") {
        match run_brain_where(&jsonl_abs, source_path) {
            CliInvocation::Result(outcome) => {
                return match outcome {
                    LookupOutcome::Resolved { target } => {
                        match verify_target_file(successor_root, &target) {
                            Ok(target) => LookupOutcome::Resolved { target },
                            Err(message) => LookupOutcome::Error(message),
                        }
                    }
                    other => other,
                };
            }
            CliInvocation::NotInstalled => {}
        }
    }
    lookup_in_jsonl(successor_root, &jsonl_abs, source_path)
}

enum CliInvocation {
    Result(LookupOutcome),
    NotInstalled,
}

fn run_brain_where(jsonl_abs: &Path, source_path: &str) -> CliInvocation {
    let output = match std::process::Command::new("brain")
        .arg("where")
        .arg("--json")
        .arg("--map")
        .arg(jsonl_abs)
        .arg("--")
        .arg(source_path)
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CliInvocation::NotInstalled;
        }
        Err(error) => {
            return CliInvocation::Result(LookupOutcome::Error(format!(
                "Cannot run path-map lookup: {error}"
            )));
        }
    };
    let parsed: CliLookupResponse = match serde_json::from_slice(&output.stdout) {
        Ok(response) => response,
        Err(_) => {
            return CliInvocation::Result(LookupOutcome::Error(
                "Path-map lookup returned an unreadable response.".into(),
            ));
        }
    };
    let code = output.status.code();
    let outcome = match (parsed.status.as_str(), code) {
        ("resolved", Some(0)) => match (parsed.old_path.as_deref(), parsed.path) {
            (Some(old), Some(target)) if old == source_path => LookupOutcome::Resolved { target },
            _ => LookupOutcome::Error("Path-map lookup did not match the displayed note.".into()),
        },
        ("retired", Some(1)) => LookupOutcome::Retired { reason: None },
        ("ambiguous", Some(1)) => LookupOutcome::Ambiguous {
            candidates: parsed.candidates,
        },
        ("unresolved", Some(1)) => LookupOutcome::Unresolved,
        ("error", _) => LookupOutcome::Error(
            parsed
                .error
                .unwrap_or_else(|| "Path-map lookup failed.".into()),
        ),
        _ => LookupOutcome::Error("Path-map lookup returned an unexpected status.".into()),
    };
    CliInvocation::Result(outcome)
}

pub(crate) fn lookup_in_jsonl(
    successor_root: &Path,
    jsonl_abs: &Path,
    source_path: &str,
) -> LookupOutcome {
    let file = match File::open(jsonl_abs) {
        Ok(file) => file,
        Err(error) => return LookupOutcome::Error(format!("Cannot open path map: {error}")),
    };
    match file.metadata() {
        Ok(meta) if meta.len() <= MAX_MAP_BYTES => {}
        Ok(_) => return LookupOutcome::Error("The path map is too large to read.".into()),
        Err(error) => return LookupOutcome::Error(format!("Cannot inspect path map: {error}")),
    }

    let mut exact_targets = Vec::<(Option<String>, Option<String>)>::new();
    let mut opaque_dirs = BTreeMap::<String, Vec<String>>::new();
    for line in BufReader::new(file).lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => return LookupOutcome::Error(format!("Cannot read path map: {error}")),
        };
        if line.trim().is_empty() {
            continue;
        }
        let row: JsonlRow = match serde_json::from_str(&line) {
            Ok(row) => row,
            Err(_) => return LookupOutcome::Error("The path map contains invalid JSON.".into()),
        };
        if row.old_path == source_path {
            let reason = (!row.reason.trim().is_empty()).then_some(row.reason);
            exact_targets.push((row.new_path, reason));
        } else if matches!(row.action.as_str(), "keep-nested" | "keep-unit") {
            if let Some(target) = row.new_path {
                opaque_dirs.entry(row.old_path).or_default().push(target);
            }
        }
    }

    if !exact_targets.is_empty() {
        let unique: BTreeSet<_> = exact_targets
            .iter()
            .map(|(target, _)| target.clone())
            .collect();
        if unique.len() > 1 {
            return LookupOutcome::Ambiguous {
                candidates: unique.into_iter().flatten().collect(),
            };
        }
        let (target, reason) = exact_targets.into_iter().next().unwrap();
        return match target {
            Some(target) => match verify_target_file(successor_root, &target) {
                Ok(target) => LookupOutcome::Resolved { target },
                Err(message) => LookupOutcome::Error(message),
            },
            None => LookupOutcome::Retired { reason },
        };
    }

    // Check opaque directory ancestors (keep-nested / keep-unit).
    if is_safe_relative(source_path) {
        let mut ancestor = source_path;
        while let Some((parent, _)) = ancestor.rsplit_once('/') {
            if let Some(targets) = opaque_dirs.get(parent) {
                let unique: BTreeSet<_> = targets.iter().cloned().collect();
                if unique.len() > 1 {
                    return LookupOutcome::Ambiguous {
                        candidates: unique.into_iter().collect(),
                    };
                }
                if let Some(base) = unique.into_iter().next() {
                    let target = format!("{base}{}", &source_path[parent.len()..]);
                    return match verify_target_file(successor_root, &target) {
                        Ok(target) => LookupOutcome::Resolved { target },
                        Err(message) => LookupOutcome::Error(message),
                    };
                }
            }
            ancestor = parent;
        }
    }

    LookupOutcome::Unresolved
}

pub(crate) struct ArchiveAction {
    pub id: &'static str,
    pub label: String,
    pub tooltip: String,
}

impl Reader {
    pub(super) fn refresh_archive_state(&mut self, reset: bool, cx: &mut Context<Self>) {
        if self.single_file {
            self.archive_state = ArchiveState::Active;
            self.archive_successor_root = None;
            self.archive_successor_ambiguous.clear();
            self.archive_lookup_notice = None;
            return;
        }
        if reset {
            self.archive_state = ArchiveState::Active;
            self.archive_successor_root = None;
            self.archive_successor_ambiguous.clear();
            self.archive_lookup_notice = None;
        }
        let root = self.vault_root.clone();
        let self_id = Some(cx.entity_id());
        let open_roots = reader_open::open_vault_roots_except(self_id, cx);
        let known_roots =
            reader_open::known_vault_roots_except(self_id, self.session_directory.as_deref(), cx);
        let located = cx
            .try_global::<LocatedSuccessors>()
            .map(|cache| cache.0.clone())
            .unwrap_or_default();
        cx.spawn(async move |this, cx| {
            let probe_root = root.clone();
            let (next_state, explicit_active, resolved) = cx
                .background_executor()
                .spawn(async move {
                    let (state, explicit_active) = ArchiveState::load_refresh(&probe_root);
                    let resolved = state.successor().map(|successor| {
                        let located_root = located.get(&successor.vault_id).map(PathBuf::as_path);
                        let matched = resolve_successor_root(
                            &probe_root,
                            &successor.vault_id,
                            &open_roots,
                            &known_roots,
                            located_root,
                        );
                        let display_name = match &matched {
                            SuccessorMatch::Found(path) => {
                                vault_name::read(path).unwrap_or_else(|| successor.name.clone())
                            }
                            _ => successor.name.clone(),
                        };
                        (matched, display_name)
                    });
                    (state, explicit_active, resolved)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.single_file || this.vault_root != root {
                    return;
                }
                let merged = this
                    .archive_state
                    .merge_refresh(next_state, explicit_active);
                let changed = this.archive_state != merged;
                this.archive_state = merged;
                let (next_root, next_ambiguous) = match resolved {
                    Some((SuccessorMatch::Found(path), _)) => (Some(path), Vec::new()),
                    Some((SuccessorMatch::Ambiguous(paths), _)) => (None, paths),
                    _ => (None, Vec::new()),
                };
                if changed
                    || this.archive_successor_root != next_root
                    || this.archive_successor_ambiguous != next_ambiguous
                {
                    this.archive_successor_root = next_root;
                    this.archive_successor_ambiguous = next_ambiguous;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn refresh_quick_viewer_vault(&mut self, cx: &mut Context<Self>) {
        if !self.single_file {
            self.quick_viewer_vault = None;
            return;
        }
        let target_path = if self.selected_file().is_empty() {
            self.vault_root.clone()
        } else {
            self.vault_root.join(self.selected_file())
        };
        let known_roots = reader_open::known_vault_roots_except(
            Some(cx.entity_id()),
            self.session_directory.as_deref(),
            cx,
        );
        let root_snapshot = self.vault_root.clone();
        let file_snapshot = self.selected_file().to_owned();
        cx.spawn(async move |this, cx| {
            let discovered = cx
                .background_executor()
                .spawn(async move {
                    let root = okilum_core::vault_descriptor::find_enclosing_marked_vault(
                        &target_path,
                        &known_roots,
                    )?;
                    let name =
                        vault_name::read(&root).unwrap_or_else(|| vault_name::folder_name(&root));
                    Some(QuickViewerVault { root, name })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if !this.single_file
                    || this.vault_root != root_snapshot
                    || (!file_snapshot.is_empty() && this.selected_file() != file_snapshot)
                {
                    return;
                }
                let discovered = discovered.map(|mut info| {
                    if let Some(shown) = vault_name::stored(&info.root, cx) {
                        info.name = shown;
                    }
                    info
                });
                if this.quick_viewer_vault != discovered {
                    this.quick_viewer_vault = discovered;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn archive_successor_action(&self, cx: &App) -> Option<ArchiveAction> {
        if self.single_file || self.selected_file().is_empty() {
            return None;
        }
        let successor = self.archive_state.successor()?;
        self.archive_state.path_map()?;
        if let Some(root) = &self.archive_successor_root {
            let name = vault_name::stored(root, cx).unwrap_or_else(|| successor.name.clone());
            Some(ArchiveAction {
                id: "archive-open-in-vault",
                label: open_in_vault_label(&name),
                tooltip: vault_name::path_tooltip(root),
            })
        } else {
            Some(ArchiveAction {
                id: "archive-locate-vault",
                label: locate_vault_label(&successor.name),
                tooltip: format!("Locate the folder for vault \"{}\"", successor.name),
            })
        }
    }

    pub(super) fn invoke_archive_successor_action(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(successor) = self.archive_state.successor().cloned() else {
            return;
        };
        let Some(path_map) = self.archive_state.path_map().cloned() else {
            return;
        };
        let source_path = self.selected_file().to_owned();
        if source_path.is_empty() {
            return;
        }
        if !self.archive_successor_ambiguous.is_empty() {
            self.archive_lookup_notice = Some(ArchiveLookupNotice {
                message: format!(
                    "Multiple local folders match vault \"{}\". Close or remove duplicate copies to choose one.",
                    successor.name
                ),
                path_map: None,
            });
            cx.notify();
            return;
        }
        if let Some(root) = self.archive_successor_root.clone() {
            self.run_archive_successor_lookup(root, successor, path_map, source_path, cx);
            return;
        }
        // Not yet known locally: prompt user to locate the successor vault folder.
        let prompt_label = locate_vault_label(&successor.name);
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(prompt_label.into()),
        });
        cx.spawn(async move |this, cx| {
            let picked = match picker.await {
                Ok(Ok(Some(mut paths))) => paths.pop(),
                _ => None,
            };
            let Some(chosen) = picked else {
                return;
            };
            let expected_id = successor.vault_id.clone();
            let expected_name = successor.name.clone();
            let verified = cx
                .background_executor()
                .spawn(async move {
                    let canonical = okilum_core::vault::canonical_root(&chosen)
                        .map_err(|_| "The selected folder could not be opened.".to_owned())?;
                    if !canonical.is_dir() {
                        return Err("Please select a vault folder.".to_owned());
                    }
                    match okilum_core::vault_link_identity::read(&canonical) {
                        Ok(Some(id)) if id == expected_id => Ok(canonical),
                        Ok(Some(_)) => Err(format!(
                            "That folder does not match vault \"{expected_name}\" (different vault identity).",
                        )),
                        Ok(None) => Err(format!(
                            "That folder has no link identity matching vault \"{expected_name}\".",
                        )),
                        Err(error) => Err(format!(
                            "Cannot read link identity in that folder: {error:#}"
                        )),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| match verified {
                Ok(root) => {
                    cx.default_global::<LocatedSuccessors>()
                        .0
                        .insert(successor.vault_id.clone(), root.clone());
                    this.archive_successor_root = Some(root.clone());
                    this.archive_successor_ambiguous.clear();
                    this.archive_lookup_notice = None;
                    cx.notify();
                    this.run_archive_successor_lookup(root, successor, path_map, source_path, cx);
                }
                Err(message) => {
                    this.archive_lookup_notice = Some(ArchiveLookupNotice {
                        message,
                        path_map: None,
                    });
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn run_archive_successor_lookup(
        &mut self,
        successor_root: PathBuf,
        successor: Successor,
        path_map: PathMap,
        source_path: String,
        cx: &mut Context<Self>,
    ) {
        let vault_name_shown =
            vault_name::stored(&successor_root, cx).unwrap_or_else(|| successor.name.clone());
        let session_dir = self.session_directory.clone();
        let archive_root = self.vault_root.clone();
        cx.spawn(async move |this, cx| {
            let (outcome, path_map_available) = cx
                .background_executor()
                .spawn({
                    let successor_root = successor_root.clone();
                    let expected_id = successor.vault_id.clone();
                    let path_map = path_map.clone();
                    let source_path = source_path.clone();
                    async move {
                        let outcome = lookup_successor_note(
                            &successor_root,
                            &expected_id,
                            &path_map,
                            &source_path,
                        );
                        let map_ok = verify_successor_identity(&successor_root, &expected_id).is_ok()
                            && reader_files::checked_path(&successor_root, &path_map.note)
                                .is_ok_and(|path| path.is_file());
                        (outcome, map_ok)
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.vault_root != archive_root || this.selected_file() != source_path {
                    return;
                }
                let fallback = path_map_available.then(|| PathMapFallback {
                    successor_root: successor_root.clone(),
                    expected_vault_id: successor.vault_id.clone(),
                    note: path_map.note.clone(),
                    vault_name: vault_name_shown.clone(),
                });
                match outcome {
                    LookupOutcome::Resolved { target } => {
                        this.archive_lookup_notice = None;
                        cx.notify();
                        cx.defer(move |cx| {
                            reader_open::open_vault_note_reusing_window(
                                successor_root,
                                Some(target),
                                session_dir,
                                cx,
                            );
                        });
                    }
                    LookupOutcome::Retired { reason } => {
                        let message = match reason {
                            Some(reason) => format!(
                                "This note was retired in vault \"{vault_name_shown}\": {reason}"
                            ),
                            None => {
                                format!("This note was retired in vault \"{vault_name_shown}\".")
                            }
                        };
                        this.archive_lookup_notice = Some(ArchiveLookupNotice {
                            message,
                            path_map: fallback,
                        });
                        cx.notify();
                    }
                    LookupOutcome::Ambiguous { candidates } => {
                        let detail = if candidates.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", candidates.join(", "))
                        };
                        this.archive_lookup_notice = Some(ArchiveLookupNotice {
                            message: format!(
                                "Multiple destinations match in vault \"{vault_name_shown}\"{detail}."
                            ),
                            path_map: fallback,
                        });
                        cx.notify();
                    }
                    LookupOutcome::Unresolved => {
                        this.archive_lookup_notice = Some(ArchiveLookupNotice {
                            message: format!(
                                "No mapped note was found in vault \"{vault_name_shown}\"."
                            ),
                            path_map: fallback,
                        });
                        cx.notify();
                    }
                    LookupOutcome::Error(error) => {
                        this.archive_lookup_notice = Some(ArchiveLookupNotice {
                            message: error,
                            path_map: fallback,
                        });
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub(super) fn open_archive_path_map(
        &mut self,
        fallback: PathMapFallback,
        cx: &mut Context<Self>,
    ) {
        let session_dir = self.session_directory.clone();
        cx.spawn(async move |this, cx| {
            let checked = cx
                .background_executor()
                .spawn({
                    let root = fallback.successor_root.clone();
                    let id = fallback.expected_vault_id.clone();
                    let note = fallback.note.clone();
                    async move {
                        verify_successor_identity(&root, &id)?;
                        verify_target_file(&root, &note)?;
                        Ok::<_, String>((root, note))
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| match checked {
                Ok((root, note)) => {
                    this.archive_lookup_notice = None;
                    cx.notify();
                    cx.defer(move |cx| {
                        reader_open::open_vault_note_reusing_window(
                            root,
                            Some(note),
                            session_dir,
                            cx,
                        );
                    });
                }
                Err(message) => {
                    this.archive_lookup_notice = Some(ArchiveLookupNotice {
                        message,
                        path_map: None,
                    });
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn open_quick_viewer_in_vault(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.quick_viewer_vault.clone() else {
            return;
        };
        let file_path = if self.selected_file().is_empty() {
            self.vault_root.clone()
        } else {
            self.vault_root.join(self.selected_file())
        };
        let session_dir = self.session_directory.clone();
        cx.spawn(async move |this, cx| {
            let verified = cx
                .background_executor()
                .spawn(async move {
                    let canonical_root = okilum_core::vault::canonical_root(&target.root)
                        .map_err(|_| "The vault folder is no longer accessible.".to_owned())?;
                    if !canonical_root.is_dir() {
                        return Err("The vault folder is no longer accessible.".to_owned());
                    }
                    let canonical_file = okilum_core::vault::canonical_root(&file_path)
                        .map_err(|_| "The displayed file is no longer accessible.".to_owned())?;
                    let relative = canonical_file
                        .strip_prefix(&canonical_root)
                        .map_err(|_| "The displayed file is outside that vault.".to_owned())?;
                    let note = (!relative.as_os_str().is_empty())
                        .then(|| okilum_core::vault::note_path(relative));
                    Ok::<_, String>((canonical_root, note))
                })
                .await;
            let _ = this.update(cx, |this, cx| match verified {
                Ok((root, note)) => {
                    cx.defer(move |cx| {
                        reader_open::open_vault_note_reusing_window(root, note, session_dir, cx);
                    });
                }
                Err(message) => {
                    this.link_notice = Some(message.into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    #[inline(never)]
    pub(super) fn render_archive_lookup_banner(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let notice = self.archive_lookup_notice.as_ref()?;
        let fallback = notice.path_map.clone();
        let message = notice.message.clone();
        Some(
            h_flex()
                .debug_selector(|| "archive-lookup-notice".into())
                .px_4()
                .py_1p5()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(brand::palette(cx).border_subtle)
                .bg(cx.theme().secondary)
                .text_sm()
                .text_color(cx.theme().foreground)
                .child(div().flex_1().min_w_0().child(message))
                .when_some(fallback, |row, fallback| {
                    let label = open_path_map_label(&fallback.vault_name);
                    row.child(
                        Button::new("archive-open-path-map")
                            .debug_selector(|| "archive-open-path-map".into())
                            .small()
                            .label(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_archive_path_map(fallback.clone(), cx);
                            })),
                    )
                })
                .child(
                    Button::new("archive-notice-dismiss")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("Dismiss")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.archive_lookup_notice = None;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use okilum_core::vault_descriptor::{find_enclosing_marked_vault, has_vault_marker};

    const ARCHIVE_ID: &str = "cc480ea3-0b98-4783-960f-3a64fa4c274c";
    const GROVE_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

    fn write_file(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn copy_rule_quotes_vault_name_as_data_with_generic_vault_noun() {
        assert_eq!(READ_ONLY_ARCHIVE_LABEL, "Read-only archive");
        assert_eq!(
            open_in_vault_label("Main Brain"),
            "Open in vault \"Main Brain\""
        );
        assert_eq!(
            locate_vault_label("Main Brain"),
            "Locate vault \"Main Brain\"\u{2026}"
        );
        assert_eq!(
            open_path_map_label("Main Brain"),
            "Open path map in vault \"Main Brain\""
        );
    }

    #[test]
    fn resolve_successor_matches_uuid_only_and_detects_duplicates() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("archive");
        let grove = temp.path().join("grove");
        let namesake = temp.path().join("Main Brain");
        let duplicate = temp.path().join("grove-copy");
        write_file(
            &archive,
            ".okilum/link-identity.json",
            &format!(r#"{{"version":1,"id":"{ARCHIVE_ID}"}}"#),
        );
        write_file(
            &grove,
            ".okilum/link-identity.json",
            &format!(r#"{{"version":1,"id":"{GROVE_ID}"}}"#),
        );
        write_file(&namesake, ".okilum/vault.json", r#"{"name":"Main Brain"}"#);
        write_file(
            &duplicate,
            ".okilum/link-identity.json",
            &format!(r#"{{"version":1,"id":"{GROVE_ID}"}}"#),
        );

        // Matching name without matching UUID is never accepted.
        assert_eq!(
            resolve_successor_root(&archive, GROVE_ID, &[], &[namesake.clone()], None),
            SuccessorMatch::NotFound
        );
        // Unique UUID match resolves cleanly.
        assert_eq!(
            resolve_successor_root(&archive, GROVE_ID, &[grove.clone()], &[namesake], None),
            SuccessorMatch::Found(grove.canonicalize().unwrap())
        );
        // Duplicate distinct roots with the same UUID require a choice.
        assert!(matches!(
            resolve_successor_root(&archive, GROVE_ID, &[grove], &[duplicate], None),
            SuccessorMatch::Ambiguous(roots) if roots.len() == 2
        ));
    }

    #[test]
    fn lookup_jsonl_resolves_exact_old_path_and_handles_retired_unresolved_ambiguous() {
        let temp = tempfile::tempdir().unwrap();
        let grove = temp.path().join("grove");
        write_file(
            &grove,
            ".okilum/link-identity.json",
            &format!(r#"{{"version":1,"id":"{GROVE_ID}"}}"#),
        );
        write_file(&grove, "state/dev/migrated.md", "# Migrated\n");
        write_file(&grove, "vaults/Nested/notes/a.md", "# Nested\n");
        write_file(
            &grove,
            "@views/path-map.jsonl",
            concat!(
                r#"{"old_path":"Legacy/Guide.md","new_path":"state/dev/migrated.md","action":"move","rule":"r","reason":"moved","aliases":["Alias.md"]}"#,
                "\n",
                r#"{"old_path":"Legacy/Retired.md","new_path":null,"action":"retire","rule":"r","reason":"archived away","aliases":[]}"#,
                "\n",
                r#"{"old_path":"Legacy/Dup.md","new_path":"state/dev/migrated.md","action":"move","rule":"r","reason":"","aliases":[]}"#,
                "\n",
                r#"{"old_path":"Legacy/Dup.md","new_path":"state/dev/other.md","action":"move","rule":"r","reason":"","aliases":[]}"#,
                "\n",
                r#"{"old_path":"Obsidian Vaults","new_path":"vaults/Nested","action":"keep-nested","rule":"r","reason":"","aliases":[]}"#,
                "\n",
            ),
        );
        let path_map = PathMap {
            note: "@views/path-map.md".into(),
            command: None,
            mcp_tool: Some("brain_where".into()),
        };

        assert_eq!(
            lookup_successor_note(&grove, GROVE_ID, &path_map, "Legacy/Guide.md"),
            LookupOutcome::Resolved {
                target: "state/dev/migrated.md".into()
            }
        );
        // Aliases must not auto-resolve.
        assert_eq!(
            lookup_successor_note(&grove, GROVE_ID, &path_map, "Alias.md"),
            LookupOutcome::Unresolved
        );
        // Retired note reports retired reason.
        assert_eq!(
            lookup_successor_note(&grove, GROVE_ID, &path_map, "Legacy/Retired.md"),
            LookupOutcome::Retired {
                reason: Some("archived away".into())
            }
        );
        // Ambiguous mapping reports ambiguous.
        assert!(matches!(
            lookup_successor_note(&grove, GROVE_ID, &path_map, "Legacy/Dup.md"),
            LookupOutcome::Ambiguous { .. }
        ));
        // Opaque nested directory resolves descendants.
        assert_eq!(
            lookup_successor_note(&grove, GROVE_ID, &path_map, "Obsidian Vaults/notes/a.md"),
            LookupOutcome::Resolved {
                target: "vaults/Nested/notes/a.md".into()
            }
        );
        // Wrong UUID refuses lookup.
        assert!(matches!(
            lookup_successor_note(&grove, ARCHIVE_ID, &path_map, "Legacy/Guide.md"),
            LookupOutcome::Error(_)
        ));
    }

    #[test]
    fn quick_viewer_marked_vault_discovery_prefers_innermost_known_and_ignores_bare_okilum() {
        let temp = tempfile::tempdir().unwrap();
        let outer = temp.path().join("outer");
        let bare = outer.join("bare");
        let inner = bare.join("inner");
        let note = inner.join("sub/note.md");

        write_file(&outer, ".okilum/vault.json", r#"{"name":"Outer"}"#);
        std::fs::create_dir_all(bare.join(".okilum")).unwrap();
        write_file(
            &inner,
            ".okilum/link-identity.json",
            &format!(r#"{{"version":1,"id":"{GROVE_ID}"}}"#),
        );
        write_file(&inner, "sub/note.md", "# Hi\n");

        assert!(has_vault_marker(&outer));
        assert!(!has_vault_marker(&bare), "bare .okilum/ is insufficient");
        assert!(has_vault_marker(&inner));

        // With no known roots: innermost valid marker wins.
        assert_eq!(
            find_enclosing_marked_vault(&note, &[]),
            Some(inner.canonicalize().unwrap())
        );
        // When outer is known and inner is not: outer (innermost known) wins.
        assert_eq!(
            find_enclosing_marked_vault(&note, &[outer.clone()]),
            Some(outer.canonicalize().unwrap())
        );
        // When both are known: innermost known (inner) wins.
        assert_eq!(
            find_enclosing_marked_vault(&note, &[outer, inner.clone()]),
            Some(inner.canonicalize().unwrap())
        );
    }
}
