//! Saved, read-only criteria-based log sources (#1382).
//! Only the JSON `.okilog` definition is canonical; selections and merges are derived.
use std::path::{Component, Path, PathBuf};

use anyhow::{ensure, Context, Result};
use globset::{GlobBuilder, GlobMatcher};
use serde::{Deserialize, Serialize};

use super::{is_log_path, LogFile, MergedLog, Query};

pub mod watcher;

pub const MAX_FILES: usize = 128;
pub const MAX_CANDIDATES: usize = 4096;
pub const MAX_ENTRIES: usize = 16384;
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// Explicit paths are relative to either the definition's folder or the vault.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub vault_root: bool,
    pub glob: String,
    /// A filename, not a path. Matches the base and its supported rotations.
    pub rotation: Option<String>,
    pub time: Option<TimeRange>,
    pub query: Option<String>,
}

/// Inclusive Unix nanoseconds; the caller injects `now` when selecting.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum TimeRange {
    Absolute { start: i64, end: i64 },
    LastSeconds(u32),
}

pub struct SelectionPlan {
    root: PathBuf,
    glob: GlobMatcher,
    rotation: Option<String>,
    range: Option<(i64, i64)>,
    query: Option<Query>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    Matched,
    NameMismatch,
    RotationMismatch,
    OutsideRoot,
    DuplicateSource,
    NoTimestamps,
    OutsideTimeRange,
    NoMatchingRecord,
    SizeLimit,
    FileLimit,
    IncompleteWindow,
    Unreadable(String),
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub path: PathBuf,
    pub reason: Reason,
}

pub struct Selection {
    pub candidates: Vec<Candidate>,
    /// Same order as the file IDs in `merged`.
    pub files: Vec<LogFile>,
    pub merged: MergedLog,
    /// Enumeration or candidate bound reached: never imply complete coverage.
    pub incomplete: bool,
    pub checked: usize,
    pub bytes_checked: u64,
    pub decoded_bytes_checked: u64,
}

impl Definition {
    pub fn parse(source: &str) -> Result<Self> {
        ensure!(
            source.len() <= 64 * 1024,
            "Virtual log definition exceeds 64 KiB"
        );
        let definition: Self = serde_json::from_str(source)?;
        ensure!(definition.version == 1, "Unsupported virtual log version");
        ensure!(
            !definition.name.trim().is_empty(),
            "Virtual log needs a name"
        );
        Ok(definition)
    }

    pub fn plan(&self, vault: &Path, definition: &Path, now: i64) -> Result<SelectionPlan> {
        ensure!(self.version == 1, "Unsupported virtual log version");
        ensure!(!self.name.trim().is_empty(), "Virtual log needs a name");
        let vault = vault.canonicalize()?;
        let definition = definition.canonicalize()?;
        ensure!(
            definition.starts_with(&vault),
            "Definition is outside the vault"
        );
        ensure!(
            definition.extension().is_some_and(|ext| ext == "okilog"),
            "Expected .okilog definition"
        );
        ensure!(!self.glob.is_empty(), "Virtual log needs a glob");
        let pattern = Path::new(&self.glob);
        ensure!(
            !pattern.is_absolute() && !self.glob.contains('\\') && !self.glob.contains(':'),
            "Glob must be relative"
        );
        ensure!(
            !pattern
                .components()
                .any(|c| matches!(c, Component::ParentDir)),
            "Glob cannot escape its folder"
        );
        let glob = GlobBuilder::new(&self.glob)
            .literal_separator(true)
            .build()?
            .compile_matcher();
        if let Some(rotation) = &self.rotation {
            ensure!(
                Path::new(rotation).components().count() == 1
                    && !rotation.contains(['/', '\\'])
                    && is_log_path(Path::new(rotation)),
                "Rotation must name a log file"
            );
        }
        let range = match self.time {
            Some(TimeRange::Absolute { start, end }) => {
                ensure!(start <= end, "Time range is reversed");
                Some((start, end))
            }
            Some(TimeRange::LastSeconds(seconds)) => {
                ensure!(seconds > 0, "Relative time range must be positive");
                Some((now.saturating_sub(i64::from(seconds) * 1_000_000_000), now))
            }
            None => None,
        };
        Ok(SelectionPlan {
            root: if self.vault_root {
                vault
            } else {
                definition
                    .parent()
                    .context("Definition needs a folder")?
                    .to_owned()
            },
            glob,
            rotation: self.rotation.clone(),
            range,
            query: self.query.as_deref().map(Query::parse).transpose()?,
        })
    }
}

impl SelectionPlan {
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Progress fires after every opened candidate, including failed reads. No writes.
    pub fn select(&self, progress: impl FnMut(usize, u64)) -> Selection {
        self.select_excluding(&std::collections::HashSet::new(), progress)
    }

    /// Retained Follow identities are not reopened/reindexed during membership
    /// discovery, even after a rename or via another canonical-path alias.
    pub fn select_excluding(
        &self,
        retained: &std::collections::HashSet<super::tail::FileIdentity>,
        mut progress: impl FnMut(usize, u64),
    ) -> Selection {
        let mut result = Selection {
            candidates: Vec::new(),
            files: Vec::new(),
            merged: MergedLog::default(),
            incomplete: false,
            checked: 0,
            bytes_checked: 0,
            decoded_bytes_checked: 0,
        };
        let mut selected_paths = std::collections::HashSet::new();
        let mut represented = retained.clone();
        let mut entries = walkdir::WalkDir::new(&self.root)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter();
        for _ in 0..MAX_ENTRIES {
            let Some(entry) = entries.next() else {
                return finish(result);
            };
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    result.incomplete = true;
                    result.candidates.push(Candidate {
                        path: error.path().unwrap_or(&self.root).to_owned(),
                        reason: Reason::Unreadable(error.to_string()),
                    });
                    continue;
                }
            };
            if !(entry.file_type().is_file() || entry.file_type().is_symlink())
                || !is_log_path(entry.path())
            {
                continue;
            }
            if result.candidates.len() >= MAX_CANDIDATES {
                result.incomplete = true;
                return finish(result);
            }
            let path = entry.path().to_owned();
            let relative = path
                .strip_prefix(&self.root)
                .expect("walk stays under root");
            let reason = if !self.glob.is_match(relative) {
                Reason::NameMismatch
            } else if self
                .rotation
                .as_ref()
                .is_some_and(|base| !rotation_matches(&path, base))
            {
                Reason::RotationMismatch
            } else if result.files.len() >= MAX_FILES {
                Reason::FileLimit
            } else {
                match path.canonicalize() {
                    Ok(canonical) if !canonical.starts_with(&self.root) => Reason::OutsideRoot,
                    Ok(canonical) if selected_paths.contains(&canonical) => Reason::DuplicateSource,
                    Ok(canonical) => {
                        let reason = match super::tail::path_identity(&path) {
                            Ok(Some(id)) if represented.contains(&id) => Reason::DuplicateSource,
                            Ok(_) => self.check(&path, &represented, &mut result, &mut progress),
                            Err(error) => Reason::Unreadable(error.to_string()),
                        };
                        if reason == Reason::Matched {
                            represented
                                .insert(result.files.last().expect("matched file").identity());
                            selected_paths.insert(canonical);
                        }
                        reason
                    }
                    Err(error) => Reason::Unreadable(error.to_string()),
                }
            };
            result.candidates.push(Candidate { path, reason });
        }
        result.incomplete |= entries.next().is_some();
        finish(result)
    }

    fn check(
        &self,
        path: &Path,
        retained: &std::collections::HashSet<super::tail::FileIdentity>,
        result: &mut Selection,
        progress: &mut impl FnMut(usize, u64),
    ) -> Reason {
        let size = match std::fs::metadata(path) {
            Ok(meta) => meta.len(),
            Err(error) => return Reason::Unreadable(error.to_string()),
        };
        if size > MAX_FILE_BYTES
            || result.bytes_checked.saturating_add(size) > MAX_TOTAL_BYTES
            || result.decoded_bytes_checked >= MAX_TOTAL_BYTES
        {
            return Reason::SizeLimit;
        }
        // Reserve attempted input even on a failed decode, so failures cannot bypass the bound.
        result.bytes_checked += size;
        result.checked += 1;
        let opened = LogFile::open(path);
        progress(result.checked, result.bytes_checked);
        let file = match opened {
            Ok(file) => file,
            Err(error) => return Reason::Unreadable(error.to_string()),
        };
        // Close replacement races after the pre-open identity observation.
        if retained.contains(&file.identity()) {
            return Reason::DuplicateSource;
        }
        result.decoded_bytes_checked += file.bytes().len() as u64;
        if file.bytes().len() as u64 > MAX_FILE_BYTES
            || result.decoded_bytes_checked > MAX_TOTAL_BYTES
        {
            return Reason::SizeLimit;
        }
        let reason = self.snapshot_reason(&file.snapshot());
        if reason != Reason::Matched {
            return reason;
        }
        result.files.push(file);
        Reason::Matched
    }
    /// Evaluate existing observed bytes without reopening their pathname.
    pub fn snapshot_reason(&self, snapshot: &super::LogSnapshot) -> Reason {
        if snapshot.bytes().len() as u64 > MAX_FILE_BYTES {
            return Reason::SizeLimit;
        }
        let window = snapshot.window();
        if window.tail || window.oversized {
            return Reason::IncompleteWindow;
        }
        if let Some((start, end)) = self.range {
            let mut timestamps = snapshot
                .index()
                .entries()
                .iter()
                .filter_map(|entry| entry.timestamp());
            let Some(first) = timestamps.next() else {
                return Reason::NoTimestamps;
            };
            let (min, max) = timestamps.fold((first, first), |(min, max), time| {
                (min.min(time), max.max(time))
            });
            if max < start || min > end {
                return Reason::OutsideTimeRange;
            }
        }
        if let Some(query) = &self.query {
            if !snapshot
                .index()
                .entries()
                .iter()
                .enumerate()
                .any(|(i, entry)| {
                    query.matches_indexed_line(
                        entry,
                        snapshot.raw(i).unwrap_or_default(),
                        snapshot.index(),
                    )
                })
            {
                return Reason::NoMatchingRecord;
            }
        }
        Reason::Matched
    }
}

fn finish(mut result: Selection) -> Selection {
    let indexes: Vec<_> = result.files.iter().map(LogFile::index).collect();
    result.merged = MergedLog::build(&indexes);
    result
}

/// Whether a path belongs to the explicitly named rotation family.
pub fn rotation_matches(path: &Path, base: &str) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let name = super::compression(path).map_or(name, |ext| &name[..name.len() - ext.len() - 1]);
    if name == base {
        return true;
    }
    if let Some(suffix) = name.strip_prefix(base) {
        return suffix.strip_prefix('.').is_some_and(|value| {
            !value.is_empty()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte == b'-')
        }) || suffix.strip_prefix('-').is_some_and(|value| {
            value.len() == 8 && value.bytes().all(|byte| byte.is_ascii_digit())
        });
    }
    // glog severity aliases, e.g. prog.INFO -> prog.host.user.log.INFO.<time>.<pid>.
    let Some((program, level)) = base.rsplit_once('.') else {
        return false;
    };
    ["INFO", "WARNING", "ERROR", "FATAL"].contains(&level)
        && name.starts_with(&format!("{program}."))
        && name.contains(&format!(".log.{level}."))
}

#[cfg(test)]
mod tests;
