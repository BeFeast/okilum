//! Revision-bound, lossless link rewrite previews for ordinary note moves.
pub use crate::link_candidates::CandidateIndex;
mod operation;
use crate::link_candidates::syntax;
pub use operation::{Applied, Operation, RecoveryList};

use crate::{Resolution, Vault};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, ops::Range, path::Path};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Change {
    pub path: String,
    pub line: usize,
    pub range: Range<usize>,
    pub before: String,
    pub after: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Skipped {
    pub path: String,
    pub line: usize,
    pub target: String,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkippedFile {
    pub path: String,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preview {
    #[serde(default)]
    pub timings: PreviewTimings,
    #[serde(default)]
    pub directory: Option<crate::note_move::DirectorySnapshot>,
    pub from: String,
    pub to: String,
    pub changes: Vec<Change>,
    pub skipped: Vec<Skipped>,
    #[serde(default)]
    pub skipped_files: Vec<SkippedFile>,
    #[serde(default)]
    enumeration_skips: Vec<String>,
    // All scanned notes participate in revision validation: a new incoming link
    // or new ambiguity invalidates approval just like an edited destination.
    snapshots: BTreeMap<String, String>,
    #[serde(default)]
    unchanged: BTreeMap<String, crate::vault::warm::SourceRevision>,
    inventory: Vec<String>,
}
/// Diagnostic phase timings measured on the caller's machine.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PreviewTimings {
    pub plan_ms: f64,
    pub inventory_ms: f64,
    pub candidates_ms: f64,
    pub indexed: bool,
    pub read_ms: f64,
    pub rewrite_ms: f64,
    pub total_ms: f64,
    pub files_read: usize,
}
impl Preview {
    pub fn prepare(root: &Path, from: &str, to: &str) -> Result<Self> {
        Self::prepare_with(root, from, to, None, &mut |_, _| Ok(()))
    }

    /// Full scan is an explicit caller choice (`index == None`). Checkpoints
    /// support cancellation before each source read and during inventory walks.
    pub fn prepare_with(
        root: &Path,
        from: &str,
        to: &str,
        index: Option<&CandidateIndex>,
        checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
    ) -> Result<Self> {
        Self::prepare_with_reader(
            root,
            from,
            to,
            index,
            checkpoint,
            &mut crate::vault::read_source,
        )
    }

    fn prepare_with_reader(
        root: &Path,
        from: &str,
        to: &str,
        index: Option<&CandidateIndex>,
        checkpoint: &mut impl FnMut(&str, usize) -> Result<()>,
        read: &mut impl FnMut(&Path) -> std::io::Result<String>,
    ) -> Result<Self> {
        let started = std::time::Instant::now();
        let directory = if fs::symlink_metadata(root.join(from))?.is_dir() {
            let snapshot = crate::note_move::DirectorySnapshot::read(root, Path::new(from))?;
            crate::note_move::DirectoryMovePlan::prepare(
                root,
                Path::new(from),
                Path::new(to),
                &snapshot,
            )?;
            Some(snapshot)
        } else {
            crate::note_move::MovePlan::prepare(root, Path::new(from), Path::new(to))?;
            None
        };
        let plan_ms = started.elapsed().as_secs_f64() * 1000.;
        let inventory_started = std::time::Instant::now();
        let vault = Vault::scan_metadata_with(root, checkpoint)?;
        let inventory_ms = inventory_started.elapsed().as_secs_f64() * 1000.;

        ensure!(
            directory.is_some() || vault.notes.iter().any(|n| n.path == from),
            "The moved note is excluded from this folder inventory"
        );
        let mut after =
            Vault::from_note_paths(vault.notes.iter().map(|n| moved_path(&n.path, from, to)));
        // Simulated inventory must not consult pre-move filesystem entries.
        after.root = root.to_path_buf();
        let candidates_started = std::time::Instant::now();
        let (candidates, unchanged) = if let Some(index) = index {
            let (paths, unchanged) = index.select(root, &vault, from, to, checkpoint)?;
            (Some(paths), unchanged)
        } else {
            (None, BTreeMap::new())
        };
        let mut preview = Self {
            timings: PreviewTimings {
                plan_ms,
                inventory_ms,
                candidates_ms: candidates_started.elapsed().as_secs_f64() * 1000.,
                indexed: index.is_some(),
                ..Default::default()
            },
            directory,
            from: from.into(),
            to: to.into(),
            changes: vec![],
            skipped: vec![],
            skipped_files: vault
                .unreadable
                .iter()
                .map(|entry| SkippedFile {
                    path: entry
                        .path
                        .strip_prefix(root)
                        .unwrap_or(&entry.path)
                        .to_string_lossy()
                        .into_owned(),
                    reason: entry.error.clone(),
                })
                .collect(),
            enumeration_skips: vault
                .unreadable
                .iter()
                .map(|entry| {
                    entry
                        .path
                        .strip_prefix(root)
                        .unwrap_or(&entry.path)
                        .to_string_lossy()
                        .into_owned()
                })
                .collect(),
            snapshots: BTreeMap::new(),
            unchanged,
            inventory: vault.entries.iter().map(|e| e.path.clone()).collect(),
        };
        preview.enumeration_skips.sort();
        for (count, note) in vault.notes.iter().enumerate() {
            if candidates
                .as_ref()
                .is_some_and(|paths| !paths.contains(&note.path))
            {
                continue;
            }
            checkpoint("Reading link candidates", count)?;
            let read_started = std::time::Instant::now();
            preview.timings.files_read += 1;
            let source = read(&root.join(&note.path));
            preview.timings.read_ms += read_started.elapsed().as_secs_f64() * 1000.;
            let source = match source {
                Ok(source) => source,
                Err(error) if note.path != from || preview.directory.is_some() => {
                    preview.skipped_files.push(SkippedFile {
                        path: note.path.clone(),
                        reason: error.to_string(),
                    });
                    continue;
                }
                Err(error) => {
                    return Err(error).with_context(|| format!("Read moved note {}", note.path))
                }
            };
            let rewrite_started = std::time::Instant::now();
            for target in syntax::targets_in_vault(&source, &vault, &note.path) {
                let line = source[..target.range.start]
                    .bytes()
                    .filter(|b| *b == b'\n')
                    .count()
                    + 1;
                let replacement = replacement(&vault, &after, from, to, &note.path, &target)
                    .and_then(|next| {
                        if let Some(next) = &next {
                            syntax::validate_frontmatter_replacement(
                                &source,
                                target.range.clone(),
                                next,
                            )?;
                        }
                        Ok(next)
                    });
                match replacement {
                    Ok(Some(replacement)) if replacement != target.text => {
                        preview.changes.push(Change {
                            path: note.path.clone(),
                            line,
                            range: target.range,
                            before: target.text,
                            after: replacement,
                        })
                    }
                    Err(error) => preview.skipped.push(Skipped {
                        path: note.path.clone(),
                        line,
                        target: target.text,
                        reason: error.to_string(),
                    }),
                    _ => {}
                }
            }
            preview.timings.rewrite_ms += rewrite_started.elapsed().as_secs_f64() * 1000.;
            preview.snapshots.insert(note.path.clone(), source);
        }
        preview.skipped_files.sort_by(|a, b| a.path.cmp(&b.path));
        preview.skipped_files.dedup_by(|a, b| a.path == b.path);
        // Reject overlapping edits rather than allowing an AST/parser discrepancy
        // to apply a target against a different string.
        for path in preview.snapshots.keys() {
            preview.rewritten(path)?;
        }
        preview.timings.total_ms = started.elapsed().as_secs_f64() * 1000.;
        Ok(preview)
    }
    pub fn changed_notes(&self) -> usize {
        self.changes
            .iter()
            .map(|c| &c.path)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }
    pub fn affected_paths(&self) -> Vec<String> {
        let mut paths: Vec<_> = self
            .changes
            .iter()
            .map(|c| c.path.clone())
            .chain(
                self.snapshots
                    .keys()
                    .filter(|p| moved_path(p, &self.from, &self.to) != **p)
                    .cloned(),
            )
            .collect();
        paths.sort();
        paths.dedup();
        paths
    }
    /// Include displayed descendants even when their source was unreadable/skipped.
    pub fn editor_paths(&self) -> Vec<String> {
        let mut paths = self.affected_paths();
        if self.directory.is_some() {
            paths.extend(
                self.inventory
                    .iter()
                    .filter(|p| {
                        moved_path(p, &self.from, &self.to) != **p
                            && Path::new(p)
                                .extension()
                                .is_some_and(|e| e.eq_ignore_ascii_case("md"))
                    })
                    .cloned(),
            );
        }
        paths.sort();
        paths.dedup();
        paths
    }

    pub fn validate(&self, root: &Path) -> Result<()> {
        let vault = Vault::scan_metadata(root)?;
        let mut enumeration_skips: Vec<_> = vault
            .unreadable
            .iter()
            .map(|entry| {
                entry
                    .path
                    .strip_prefix(root)
                    .unwrap_or(&entry.path)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        enumeration_skips.sort();
        ensure!(
            enumeration_skips == self.enumeration_skips,
            "Skipped file inventory changed; preview again. Nothing was written"
        );
        ensure!(
            vault.entries.iter().map(|e| &e.path).collect::<Vec<_>>()
                == self.inventory.iter().collect::<Vec<_>>(),
            "Folder inventory changed; preview again. Nothing was written"
        );
        let paths: std::collections::BTreeSet<_> = vault.notes.iter().map(|n| &n.path).collect();
        ensure!(
            paths
                == self
                    .snapshots
                    .keys()
                    .chain(self.unchanged.keys())
                    .chain(
                        self.skipped_files
                            .iter()
                            .map(|s| &s.path)
                            .filter(|p| paths.contains(p))
                    )
                    .collect(),
            "Folder contents changed; preview again. Nothing was written"
        );
        for skipped in &self.skipped_files {
            if paths.contains(&skipped.path) {
                ensure!(
                    crate::vault::read_source(&root.join(&skipped.path)).is_err(),
                    "{} is now readable; preview again to include its links. Nothing was written",
                    skipped.path
                );
            }
        }
        for (path, revision) in &self.unchanged {
            ensure!(
                crate::vault::warm::SourceRevision::read(&root.join(path))
                    .ok()
                    .as_ref()
                    == Some(revision),
                "{path} changed since the link index; preview again. Nothing was written"
            );
        }
        for (path, before) in &self.snapshots {
            ensure!(
                crate::vault::read_source(&root.join(path))? == *before,
                "{path} changed; preview again. Nothing was written"
            );
        }
        if let Some(snapshot) = &self.directory {
            crate::note_move::DirectoryMovePlan::prepare(
                root,
                Path::new(&self.from),
                Path::new(&self.to),
                snapshot,
            )?;
        } else {
            crate::note_move::MovePlan::prepare(root, Path::new(&self.from), Path::new(&self.to))?;
        }
        Ok(())
    }
    pub fn rewritten(&self, path: &str) -> Result<String> {
        let source = self.snapshots.get(path).context("Unknown preview path")?;
        let mut result = source.clone();
        let mut limit = source.len();
        for change in self.changes.iter().filter(|c| c.path == path).rev() {
            ensure!(
                change.range.end <= limit
                    && source.get(change.range.clone()) == Some(change.before.as_str()),
                "Invalid or overlapping source ranges"
            );
            result.replace_range(change.range.clone(), &change.after);
            limit = change.range.start;
        }
        Ok(result)
    }
}

/// Map one identity through a file or directory move, never a textual prefix twin.
pub fn moved_path(path: &str, from: &str, to: &str) -> String {
    if path == from {
        return to.to_owned();
    }
    path.strip_prefix(from)
        .and_then(|rest| rest.strip_prefix('/'))
        .map_or_else(|| path.to_owned(), |rest| format!("{to}/{rest}"))
}

fn replacement(
    before: &Vault,
    after: &Vault,
    from: &str,
    to: &str,
    source: &str,
    target: &syntax::Target,
) -> Result<Option<String>> {
    let (base, fragment) = target
        .text
        .find('#')
        .map_or((target.text.as_str(), ""), |i| {
            (&target.text[..i], &target.text[i..])
        });
    if base.is_empty() || base.contains(':') || base.starts_with("//") {
        return Ok(None);
    }
    if let Some(reason) = &target.reason {
        bail!("{reason}");
    }
    // Decode valid URL escapes exactly once; CommonMark permits a literal %
    // when it is not followed by two hex digits.
    let decoded = if target.wiki {
        base.to_owned()
    } else {
        decode_path(base)?
    };
    let extension = decoded.rsplit('.').next().unwrap_or("").to_lowercase();
    let asset = (!target.wiki && extension != "md")
        || (target.embedded
            && [
                "png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "pdf", "mp3", "mp4", "wav",
                "mov",
            ]
            .contains(&extension.as_str()));
    let resolution = if asset {
        Resolution::Resolved {
            path: resolve_attachment(before, source, &decoded)?,
        }
    } else if target.wiki {
        before.resolve_from(&decoded, source)
    } else {
        before.resolve_markdown(&decoded, source)
    };
    let resolved = match resolution {
        Resolution::Resolved { path } => path,
        Resolution::Ambiguous { candidates } => bail!("Ambiguous: {}", candidates.join(", ")),
        Resolution::Unresolved => {
            if moved_path(source, from, to) == source {
                bail!("Unresolved target");
            }
            resolve_attachment(before, source, &decoded)?
        }
    };
    if !before.inventory_complete {
        // Root-exact and explicit relative references do not depend on missing
        // siblings. Suffix/attachment-name guesses still require a full inventory.
        let candidate = if target.wiki && !decoded.starts_with("./") && !decoded.starts_with("../")
        {
            Some(decoded.trim_start_matches('/').to_owned())
        } else {
            normalize(source, &decoded)
        };
        let exact = candidate.is_some_and(|mut path| {
            if !asset && !path.to_lowercase().ends_with(".md") {
                path.push_str(".md");
            }
            path.eq_ignore_ascii_case(&resolved)
        });
        ensure!(
            exact,
            "Folder inventory is incomplete; suffix link target cannot be verified"
        );
    }
    let is_note = before.notes.iter().any(|n| n.path == resolved);
    if is_note {
        ensure!(
            before
                .notes
                .iter()
                .filter(|n| n.path.to_lowercase() == resolved.to_lowercase())
                .count()
                == 1,
            "Case-ambiguous note path"
        );
        let mapped = moved_path(&resolved, from, to);
        let desired = mapped.as_str();
        ensure!(
            after
                .notes
                .iter()
                .filter(|n| n.path.to_lowercase() == desired.to_lowercase())
                .count()
                == 1,
            "Destination would be case-ambiguous"
        );
    }
    let mapped = moved_path(&resolved, from, to);
    let desired = mapped.as_str();
    let mapped_source = moved_path(source, from, to);
    let new_source = mapped_source.as_str();
    let relative = decoded.starts_with("./") || decoded.starts_with("../");
    let next = if !is_note {
        let unchanged = if target.wiki {
            resolve_attachment(before, new_source, &decoded).ok()
        } else {
            normalize(new_source, &decoded)
        };
        if unchanged.as_deref() == Some(desired)
            && (before.inventory_complete
                || normalize(new_source, &decoded).as_deref() == Some(desired))
        {
            return Ok(None);
        }
        relative_path(new_source, desired)
    } else if target.wiki {
        if relative {
            let path = relative_path(new_source, desired);
            if decoded.to_lowercase().ends_with(".md") {
                path
            } else {
                path.strip_suffix(".md").unwrap_or(&path).into()
            }
        } else {
            if after.resolve_from(&decoded, new_source).path() == Some(desired)
                && (before.inventory_complete
                    || decoded
                        .trim_start_matches('/')
                        .to_lowercase()
                        .trim_end_matches(".md")
                        == desired.to_lowercase().trim_end_matches(".md"))
            {
                return Ok(None);
            }
            let extension = decoded.to_lowercase().ends_with(".md");
            let full = if extension {
                desired
            } else {
                desired.strip_suffix(".md").unwrap_or(desired)
            };
            if decoded.contains('/') || !before.inventory_complete {
                format!(
                    "{}{}",
                    if decoded.starts_with('/') { "/" } else { "" },
                    full
                )
            } else {
                let parts: Vec<_> = full.split('/').collect();
                let mut found = None;
                for i in (0..parts.len()).rev() {
                    let candidate = parts[i..].join("/");
                    if after.resolve_from(&candidate, new_source).path() == Some(desired) {
                        found = Some(candidate);
                        break;
                    }
                }
                found.context("No unambiguous replacement")?
            }
        }
    } else {
        if is_note
            && source != from
            && resolved != from
            && after.resolve_markdown(&decoded, new_source).path() == Some(desired)
        {
            return Ok(None);
        }
        if decoded.starts_with('/') {
            format!("/{desired}")
        } else {
            relative_path(new_source, desired)
        }
    };
    // Characters with syntax meaning are encoded for Markdown; unsafe wiki
    // filenames are reported rather than creating a different link grammar.
    if target.wiki {
        ensure!(
            !next.contains(['[', ']', '|', '#', '^', '\n', '\r', '\\']),
            "Destination requires unsupported wiki escaping"
        );
    }
    let next = if !target.wiki {
        encode_path(&next)
    } else {
        next
    };
    Ok(Some(format!("{next}{fragment}")))
}
fn resolve_attachment(vault: &Vault, source: &str, target: &str) -> Result<String> {
    let exact = normalize(source, target).context("Path escapes the folder")?;
    let assets: Vec<_> = vault
        .entries
        .iter()
        .filter(|e| e.kind == crate::vault::EntryKind::Attachment)
        .map(|e| e.path.as_str())
        .collect();
    let explicit = target.starts_with("./") || target.starts_with("../");
    let candidate = if explicit || (!target.starts_with('/') && assets.contains(&exact.as_str())) {
        ensure!(assets.contains(&exact.as_str()), "Unresolved attachment");
        exact
    } else {
        let root = target.trim_start_matches('/');
        if assets.contains(&root) {
            root.to_owned()
        } else {
            ensure!(!target.starts_with('/'), "Unresolved root attachment");
            let suffix = format!("/{root}");
            let candidates: Vec<_> = assets.iter().filter(|p| p.ends_with(&suffix)).collect();
            ensure!(candidates.len() == 1, "Unresolved or ambiguous attachment");
            (*candidates[0]).to_owned()
        }
    };
    let absolute = vault.root.join(&candidate);
    let canonical = absolute.canonicalize()?;
    ensure!(
        canonical.starts_with(&vault.root) && canonical.is_file(),
        "Attachment is outside the folder or not a file"
    );
    Ok(candidate)
}
fn normalize(source: &str, target: &str) -> Option<String> {
    let mut parts: Vec<&str> = if target.starts_with('/') {
        vec![]
    } else {
        let mut parts: Vec<_> = source.split('/').collect();
        parts.pop();
        parts
    };
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(part),
        }
    }
    Some(parts.join("/"))
}
fn relative_path(source: &str, target: &str) -> String {
    let mut a: Vec<_> = source.split('/').collect();
    a.pop();
    let b: Vec<_> = target.split('/').collect();
    let shared = a.iter().zip(&b).take_while(|(a, b)| a == b).count();
    let result = std::iter::repeat_n("..", a.len() - shared)
        .chain(b[shared..].iter().copied())
        .collect::<Vec<_>>()
        .join("/");
    if result.starts_with("../") {
        result
    } else {
        format!("./{result}")
    }
}
fn decode_path(s: &str) -> Result<String> {
    let mut out = Vec::new();
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let Some(byte) = s
                .get(i + 1..i + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
            {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    Ok(String::from_utf8(out)?)
}
fn encode_path(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_whitespace()
            || matches!(
                c,
                '%' | '&' | '[' | ']' | '#' | '?' | '(' | ')' | '<' | '>' | '\\' | '"'
            )
        {
            use std::fmt::Write;
            let _ = write!(out, "%{:02X}", c as u32);
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indexed_preview_matches_full_scan_and_detects_new_incoming_links() {
        let dir = fixture();
        let root = dir.path().canonicalize().unwrap();
        // Use the same snapshot/revisions as Reader, never a second file crawler.
        let (_, snapshot, _) =
            crate::vault::warm::reconcile(&root, None, false, &mut |_, _| Ok(())).unwrap();
        let index = CandidateIndex::from_snapshot(&snapshot);
        let full = Preview::prepare(&root, "Old/Заметка 🧠.md", "Новое.md").unwrap();
        let fast = Preview::prepare_with(
            &root,
            "Old/Заметка 🧠.md",
            "Новое.md",
            Some(&index),
            &mut |_, _| Ok(()),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&fast.changes).unwrap(),
            serde_json::to_value(&full.changes).unwrap()
        );
        fs::write(root.join("new-reference.md"), "[[Old/Заметка 🧠]]").unwrap();
        assert!(fast.validate(&root).is_err());
        let refreshed = Preview::prepare_with(
            &root,
            "Old/Заметка 🧠.md",
            "Новое.md",
            Some(&index),
            &mut |_, _| Ok(()),
        )
        .unwrap();
        assert!(refreshed
            .changes
            .iter()
            .any(|change| change.path == "new-reference.md"));
    }

    #[test]
    fn indexed_preview_includes_newly_resolvable_raw_space_markdown() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("ref.md"), "[raw](Target name.md)").unwrap();
        let (_, snapshot, _) =
            crate::vault::warm::reconcile(root, None, false, &mut |_, _| Ok(())).unwrap();
        let index = CandidateIndex::from_snapshot(&snapshot);
        // The fallback was unresolved when the index was built. Source did not change.
        fs::write(root.join("Target name.md"), "target").unwrap();
        let full = Preview::prepare(root, "Target name.md", "New name.md").unwrap();
        let fast = Preview::prepare_with(
            root,
            "Target name.md",
            "New name.md",
            Some(&index),
            &mut |_, _| Ok(()),
        )
        .unwrap();
        assert_eq!(full.changes.len(), 1);
        assert_eq!(
            serde_json::to_value(&fast.changes).unwrap(),
            serde_json::to_value(&full.changes).unwrap()
        );
        let state = tempfile::tempdir().unwrap();
        assert!(
            fast.apply(root, state.path(), &mut BTreeMap::new())
                .unwrap()
                .moved
        );
        assert!(!root.join("Target name.md").exists());
        assert!(fs::read_to_string(root.join("ref.md"))
            .unwrap()
            .contains("New%20name.md"));
    }

    #[test]
    fn indexed_preview_rechecks_non_candidates_and_is_cancellable() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("from.md"), "source").unwrap();
        fs::write(root.join("ref.md"), "[[from]]").unwrap();
        fs::write(root.join("unrelated.md"), "no link").unwrap();
        let (_, snapshot, _) =
            crate::vault::warm::reconcile(root, None, false, &mut |_, _| Ok(())).unwrap();
        let index = CandidateIndex::from_snapshot(&snapshot);
        let fast =
            Preview::prepare_with(root, "from.md", "to.md", Some(&index), &mut |_, _| Ok(()))
                .unwrap();
        assert_eq!(fast.timings.files_read, 2);
        fast.validate(root).unwrap();
        fs::write(root.join("unrelated.md"), "[new](from.md)").unwrap();
        assert!(fast.validate(root).is_err());
        let refreshed =
            Preview::prepare_with(root, "from.md", "to.md", Some(&index), &mut |_, _| Ok(()))
                .unwrap();
        assert_eq!(refreshed.changed_notes(), 2);
        for index in [Some(&index), None] {
            assert!(
                Preview::prepare_with(root, "from.md", "to.md", index, &mut |phase, _| {
                    if phase == "Reading link candidates" {
                        bail!("cancelled");
                    }
                    Ok(())
                })
                .is_err()
            );
        }
        assert!(!root.join("to.md").exists());
    }

    #[test]
    fn indexed_preview_reuses_imported_sources_but_detects_preserved_mtime_new_links() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let imported = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        for (path, raw) in [
            ("from.md", "source"),
            ("ref.md", "[[from]]"),
            ("other.md", "abcdefghijklmn"),
        ] {
            fs::write(root.join(path), raw).unwrap();
            fs::File::options()
                .write(true)
                .open(root.join(path))
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(imported))
                .unwrap();
        }
        let (_, snapshot, _) =
            crate::vault::warm::reconcile(root, None, false, &mut |_, _| Ok(())).unwrap();
        let index = CandidateIndex::from_snapshot(&snapshot);
        let fast =
            Preview::prepare_with(root, "from.md", "to.md", Some(&index), &mut |_, _| Ok(()))
                .unwrap();
        assert_eq!(
            fast.timings.files_read, 2,
            "unchanged non-candidate with imported mtime must stay unread"
        );
        fast.validate(root).unwrap();
        fs::write(root.join("other.md"), "[[from]]abcd..").unwrap();
        fs::File::options()
            .write(true)
            .open(root.join("other.md"))
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(imported))
            .unwrap();
        assert!(
            fast.validate(root).is_err(),
            "same-size write with restored mtime invalidates the old preview"
        );
        let fresh =
            Preview::prepare_with(root, "from.md", "to.md", Some(&index), &mut |_, _| Ok(()))
                .unwrap();
        assert_eq!(fresh.timings.files_read, 3);
        assert_eq!(fresh.changed_notes(), 2);
    }

    #[test]
    #[ignore = "same-machine 5k-note preview profile"]
    fn profile_preview_5000_notes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("target.md"), "# Target\n").unwrap();
        for n in 0..5000 {
            let source = if n < 4 {
                "---\nrelated: '[[target]]'\n---\n[[target|alias]] [relative](target.md)\n"
                    .to_owned()
            } else {
                format!(
                    "# Note {n}\n\n{}\n",
                    "Ordinary Кириллица 🧠 prose with no incoming link. ".repeat(40)
                )
            };
            fs::write(root.join(format!("note-{n}.md")), source).unwrap();
        }
        let delay_ms: u64 = std::env::var("TESSERA_PREVIEW_READ_DELAY_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let startup = std::time::Instant::now();
        let (_, snapshot, _) = crate::vault::warm::reconcile_with_reader(
            root,
            None,
            false,
            &mut |_, _| Ok(()),
            &mut |path| {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                crate::vault::read_source(path).map(String::into_bytes)
            },
        )
        .unwrap();
        let baseline_startup_ms = startup.elapsed().as_secs_f64() * 1000.;
        let index_started = std::time::Instant::now();
        let index = CandidateIndex::from_snapshot(&snapshot);
        eprintln!("MOVE_INDEX_STARTUP read_delay_ms={delay_ms} baseline_ms={baseline_startup_ms:.2} index_build_ms={:.2} with_index_ms={:.2}",
            index_started.elapsed().as_secs_f64()*1000., startup.elapsed().as_secs_f64()*1000.);
        let mut read = |path: &Path| {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            crate::vault::read_source(path)
        };
        for sample in 0..3 {
            let preview = Preview::prepare_with_reader(
                root,
                "target.md",
                "renamed.md",
                None,
                &mut |_, _| Ok(()),
                &mut read,
            )
            .unwrap();
            assert_eq!(preview.changed_notes(), 4);
            assert_eq!(preview.changes.len(), 12);
            eprintln!(
                "MOVE_PREVIEW_BASELINE sample={sample} notes=5001 {:?}",
                preview.timings
            );
            let fast = Preview::prepare_with_reader(
                root,
                "target.md",
                "renamed.md",
                Some(&index),
                &mut |_, _| Ok(()),
                &mut read,
            )
            .unwrap();
            assert_eq!(
                serde_json::to_value(&fast.changes).unwrap(),
                serde_json::to_value(&preview.changes).unwrap()
            );
            assert_eq!(fast.timings.files_read, 5);
            eprintln!(
                "MOVE_PREVIEW_INDEXED sample={sample} notes=5001 {:?}",
                fast.timings
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn sidecars_and_invalid_utf8_do_not_abort_move_or_enter_search() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("SAP")).unwrap();
        assert!(crate::note_files::create(root, Path::new("._new.md")).is_err());
        fs::write(root.join("from.md"), "# Source").unwrap();
        fs::write(root.join("ref.md"), "[[from]] ordinaryneedle").unwrap();
        fs::write(root.join("SAP/.__index.md"), [0xff, 0xfe]).unwrap();
        use std::os::unix::ffi::OsStringExt;
        let invalid_name = std::ffi::OsString::from_vec(b"._bad\xff.md".to_vec());
        assert!(crate::vault::service_path(Path::new(&invalid_name)));
        // APFS rejects non-UTF-8 filenames; Linux also exercises disk discovery.
        #[cfg(target_os = "linux")]
        fs::write(root.join(&invalid_name), [0xff]).unwrap();
        fs::write(root.join("._valid.md"), "[[from]] sidecarneedle").unwrap();
        fs::write(root.join("invalid.md"), b"badneedle [[from]]\xff").unwrap();
        let (vault, sources) = Vault::scan_snapshot_with(root, &mut |_, _| Ok(())).unwrap();
        assert!(!vault.notes.iter().any(|n| n.path.contains("._")));
        assert!(!vault.entries.iter().any(|n| n.path.contains("._")));
        assert!(!sources.contains_key("invalid.md"));
        assert!(vault
            .unreadable
            .iter()
            .any(|n| n.path.ends_with("invalid.md")));
        assert_eq!(vault.backlinks("from.md").len(), 1);
        let (warm, snapshot, _) =
            crate::vault::warm::reconcile(root, None, false, &mut |_, _| Ok(())).unwrap();
        assert!(!warm.entries.iter().any(|n| n.path.contains("._")));
        assert!(!snapshot.sources().unwrap().contains_key("invalid.md"));
        assert!(warm
            .unreadable
            .iter()
            .any(|entry| entry.path.ends_with("invalid.md")));
        assert_eq!(warm.backlinks("from.md").len(), 1);
        let index = tempfile::tempdir().unwrap();
        let search = crate::search::Searcher::build(&vault, index.path()).unwrap();
        assert!(!search.search("ordinaryneedle", 10).unwrap().is_empty());
        assert!(search.search("sidecarneedle", 10).unwrap().is_empty());
        assert!(search.search("badneedle", 10).unwrap().is_empty());
        search.update_note(&vault, "._valid.md").unwrap();
        assert!(search.search("sidecarneedle", 10).unwrap().is_empty());
        let preview = Preview::prepare(root, "from.md", "to.md").unwrap();
        assert_eq!(preview.changes.len(), 1);
        assert_eq!(preview.skipped_files.len(), 1);
        assert_eq!(preview.skipped_files[0].path, "invalid.md");
        let state = tempfile::tempdir().unwrap();
        preview
            .apply(root, state.path(), &mut BTreeMap::new())
            .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("ref.md")).unwrap(),
            "[[to]] ordinaryneedle"
        );
        assert_eq!(
            fs::read(root.join("invalid.md")).unwrap(),
            b"badneedle [[from]]\xff"
        );
        assert_eq!(
            fs::read(root.join("SAP/.__index.md")).unwrap(),
            [0xff, 0xfe]
        );
    }

    #[test]
    fn skipped_file_becoming_readable_requires_new_preview() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("from.md"), "source").unwrap();
        fs::write(root.join("invalid.md"), [0xff]).unwrap();
        let preview = Preview::prepare(root, "from.md", "to.md").unwrap();
        fs::write(root.join("invalid.md"), "[[from]]").unwrap();
        assert!(preview.validate(root).is_err());
        assert!(root.join("from.md").exists());
        assert!(!root.join("to.md").exists());
    }

    #[test]
    fn icloud_placeholder_is_reported_and_move_can_continue_without_guessing_links() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("from.md"), "source").unwrap();
        fs::write(root.join("ref.md"), "[[from]]").unwrap();
        fs::write(root.join(".Other.md.icloud"), [0xff]).unwrap();
        let preview = Preview::prepare(root, "from.md", "to.md").unwrap();
        assert!(preview
            .skipped_files
            .iter()
            .any(|f| f.path == ".Other.md.icloud" && f.reason.contains("iCloud")));
        assert_eq!(preview.changes.len(), 1);
        assert_eq!(preview.rewritten("ref.md").unwrap(), "[[to]]");
        let state = tempfile::tempdir().unwrap();
        preview
            .apply(root, state.path(), &mut BTreeMap::new())
            .unwrap();
        assert!(root.join("to.md").exists());
        assert_eq!(fs::read_to_string(root.join("ref.md")).unwrap(), "[[to]]");
    }

    #[test]
    fn partial_inventory_updates_exact_links_but_not_suffix_guesses() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("Old")).unwrap();
        fs::write(root.join("Old/from.md"), "source").unwrap();
        fs::write(
            root.join("refs.md"),
            "[[from]] [[Old/from]] [relative](Old/from.md)",
        )
        .unwrap();
        fs::write(root.join(".unknown.md.icloud"), "").unwrap();
        let preview = Preview::prepare(root, "Old/from.md", "new.md").unwrap();
        assert_eq!(preview.changes.len(), 2);
        assert!(preview
            .skipped
            .iter()
            .any(|s| s.target == "from" && s.reason.contains("suffix")));
        assert_eq!(
            preview.rewritten("refs.md").unwrap(),
            "[[from]] [[new]] [relative](./new.md)"
        );
    }

    #[test]
    fn partial_inventory_never_keeps_a_link_that_becomes_suffix_dependent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("New")).unwrap();
        fs::write(root.join("from.md"), "source").unwrap();
        fs::write(root.join("ref.md"), "[[from]]").unwrap();
        fs::write(root.join(".unknown.md.icloud"), "").unwrap();
        let preview = Preview::prepare(root, "from.md", "New/from.md").unwrap();
        assert_eq!(preview.rewritten("ref.md").unwrap(), "[[New/from]]");
    }

    pub(super) fn fixture() -> tempfile::TempDir {
        let dir = tempfile::Builder::new()
            .prefix("tessera-link-fixture-")
            .tempdir()
            .unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/link-move");
        for entry in walkdir::WalkDir::new(&source) {
            let entry = entry.unwrap();
            let target = dir.path().join(entry.path().strip_prefix(&source).unwrap());
            if entry.file_type().is_dir() {
                fs::create_dir_all(target).unwrap();
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
        dir
    }
    #[test]
    fn fixture_preserves_source_bytes_and_resolves_all_changed_targets() {
        let dir = fixture();
        let root = dir.path().canonicalize().unwrap();
        let p = Preview::prepare(&root, "Old/Заметка 🧠.md", "New/Новое 🧠.md").unwrap();
        assert_eq!(p.changes.len(), 3, "{:#?}", p.changes);
        let incoming = fs::read_to_string(root.join("Refs/Входящие.md")).unwrap();
        let expected = incoming
            .replacen("Old/Заметка 🧠#Раздел", "New/Новое 🧠#Раздел", 1)
            .replacen("Заметка 🧠#^block", "Новое 🧠#^block", 1)
            .replacen(
                "../Old/Заметка 🧠.md#heading",
                "../New/Новое%20🧠.md#heading",
                1,
            );
        assert_eq!(p.rewritten("Refs/Входящие.md").unwrap(), expected);
        assert_eq!(
            p.rewritten("Old/Заметка 🧠.md").unwrap(),
            fs::read_to_string(root.join("Old/Заметка 🧠.md")).unwrap()
        );
        assert!(p
            .skipped
            .iter()
            .any(|s| s.target == "Same" && s.reason.contains("Ambiguous")));
        let state = tempfile::tempdir().unwrap();
        let applied = p.apply(&root, state.path(), &mut BTreeMap::new()).unwrap();
        assert!(applied.moved, "{:?}", applied.warning);
        assert_eq!(
            fs::read_to_string(root.join("Refs/Входящие.md")).unwrap(),
            expected
        );
        Operation::revert(&applied.journal, state.path()).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("Refs/Входящие.md")).unwrap(),
            incoming
        );
        assert!(root.join("Old/Заметка 🧠.md").exists());
    }
    #[test]
    fn stale_preview_and_dirty_draft_write_nothing() {
        let dir = fixture();
        let root = dir.path().canonicalize().unwrap();
        let state = tempfile::tempdir().unwrap();
        let p = Preview::prepare(&root, "Old/Заметка 🧠.md", "New/Новое.md").unwrap();
        fs::write(root.join("Other/Цель.md"), "changed").unwrap();
        assert!(p.apply(&root, state.path(), &mut BTreeMap::new()).is_err());
        assert!(root.join(&p.from).exists());
        assert!(!root.join(&p.to).exists());
        let p = Preview::prepare(&root, &p.from, &p.to).unwrap();
        {
            let mut editor = crate::file_editor::FileEditor::open(
                &root.join("Refs/Входящие.md"),
                &state.path().join("editor-drafts"),
            )
            .unwrap();
            editor.set_text("unsaved".into()).unwrap();
        }
        assert!(p.apply(&root, state.path(), &mut BTreeMap::new()).is_err());
        assert!(!state.path().join("link-moves").exists());
    }
    #[test]
    fn outgoing_attachments_and_relative_links_follow_context() {
        let dir = fixture();
        let root = dir.path().canonicalize().unwrap();
        let p = Preview::prepare(&root, "Old/Заметка 🧠.md", "Новое.md").unwrap();
        let text = p.rewritten(&p.from).unwrap();
        assert!(text.starts_with('\u{feff}'));
        assert!(text.contains("[[./Other/Цель|связь]]"), "{text}");
        assert!(text.contains("(./Other/Цель.md#е́)"), "{text}");
        assert!(text.contains("(./Assets/pic.svg)"), "{text}");
        assert!(text.contains("[[#Локальный]]"));
    }
    #[test]
    fn rollback_refuses_external_edits_and_source_collision() {
        let dir = fixture();
        let root = dir.path().canonicalize().unwrap();
        let state = tempfile::tempdir().unwrap();
        let p = Preview::prepare(&root, "Old/Заметка 🧠.md", "New/Новое.md").unwrap();
        let applied = p.apply(&root, state.path(), &mut BTreeMap::new()).unwrap();
        assert!(applied.moved);
        fs::write(root.join("Refs/Входящие.md"), "external edit").unwrap();
        assert!(Operation::revert(&applied.journal, state.path()).is_err());
        assert_eq!(
            fs::read_to_string(root.join("Refs/Входящие.md")).unwrap(),
            "external edit"
        );
        fs::write(root.join(&p.from), "new unrelated note").unwrap();
        assert!(Operation::revert(&applied.journal, state.path()).is_err());
        assert_eq!(
            fs::read_to_string(root.join(&p.from)).unwrap(),
            "new unrelated note"
        );
    }
    #[test]
    fn yaml_quotes_cannot_escape_into_properties() {
        let dir = fixture();
        let root = dir.path().canonicalize().unwrap();
        let p = Preview::prepare(&root, "Old/Заметка 🧠.md", "New/Quote\".md").unwrap();
        let original = fs::read_to_string(root.join("Refs/Входящие.md")).unwrap();
        let rewritten = p.rewritten("Refs/Входящие.md").unwrap();
        assert_eq!(
            crate::properties::frontmatter_block(&rewritten),
            crate::properties::frontmatter_block(&original)
        );
        assert!(
            p.skipped.iter().any(|s| s.reason.contains("YAML escaping")),
            "{:?}",
            p.skipped
        );
    }
    #[test]
    fn attachment_destinations_do_not_infer_markdown_and_entities_are_encoded() {
        let dir = fixture();
        let root = dir.path().canonicalize().unwrap();
        fs::write(root.join("Old/pic.png"), "asset").unwrap();
        fs::write(root.join("Old/pic.png.md"), "note").unwrap();
        fs::write(root.join("Old/n.md"), "![image](pic.png) ![[pic.png]]").unwrap();
        let p = Preview::prepare(&root, "Old/n.md", "New/n.md").unwrap();
        assert_eq!(
            p.rewritten("Old/n.md").unwrap(),
            "![image](../Old/pic.png) ![[pic.png]]"
        );
        fs::write(root.join("New/pic.png"), "different asset").unwrap();
        let p = Preview::prepare(&root, "Old/n.md", "New/n.md").unwrap();
        assert_eq!(
            p.rewritten("Old/n.md").unwrap(),
            "![image](../Old/pic.png) ![[../Old/pic.png]]"
        );
        fs::write(root.join("Old.md"), "source").unwrap();
        fs::write(root.join("ref.md"), "[label](Old.md)").unwrap();
        let p = Preview::prepare(&root, "Old.md", "A&copy;.md").unwrap();
        assert_eq!(p.rewritten("ref.md").unwrap(), "[label](./A%26copy;.md)");
    }
    #[test]
    fn new_relative_namesake_does_not_redirect_existing_link() {
        let dir = fixture();
        let root = dir.path().canonicalize().unwrap();
        fs::write(root.join("Target.md"), "target").unwrap();
        fs::write(root.join("Old.md"), "source").unwrap();
        fs::write(root.join("New/ref.md"), "[label](Target.md)").unwrap();
        let p = Preview::prepare(&root, "Old.md", "New/Target.md").unwrap();
        assert_eq!(p.rewritten("New/ref.md").unwrap(), "[label](../Target.md)");
    }
    #[test]
    fn unsafe_wiki_delimiters_and_single_quoted_yaml_are_not_updated() {
        let dir = fixture();
        let root = dir.path().canonicalize().unwrap();
        fs::write(root.join("Old.md"), "source").unwrap();
        fs::write(root.join("ref.md"), "---\nrelated: '[[Old]]'\n---\n[[Old]]").unwrap();
        let p = Preview::prepare(&root, "Old.md", "A^b.md").unwrap();
        assert!(p.changes.is_empty());
        assert_eq!(p.skipped.iter().filter(|s| s.target == "Old").count(), 2);
        let p = Preview::prepare(&root, "Old.md", "O'Brien.md").unwrap();
        assert_eq!(
            p.rewritten("ref.md").unwrap(),
            "---\nrelated: '[[Old]]'\n---\n[[O'Brien]]"
        );
        assert!(p.skipped.iter().any(|s| s.reason.contains("YAML escaping")));
    }
    #[test]
    fn literal_percent_filenames_are_rewritten_without_double_decoding() {
        let dir = fixture();
        let root = dir.path().canonicalize().unwrap();
        fs::write(root.join("Old/100%.png"), "image").unwrap();
        fs::write(root.join("Old/n.md"), "![image](100%.png)").unwrap();
        let p = Preview::prepare(&root, "Old/n.md", "New/n.md").unwrap();
        assert_eq!(
            p.rewritten("Old/n.md").unwrap(),
            "![image](../Old/100%25.png)"
        );
    }
}
#[cfg(test)]
mod directory_tests {
    use super::*;
    use crate::note_move::{DirectoryMovePlan, DirectorySnapshot};
    use std::collections::BTreeMap;

    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("Old/sub")).unwrap();
        fs::create_dir_all(root.path().join("Old/media")).unwrap();
        fs::create_dir(root.path().join("Parent")).unwrap();
        fs::write(root.path().join("Old/a.md"), "\u{feff}# Привет\r\n[[./sub/b#^id|🧠]]\r\n[out](../Outside.md) ![asset](media/p.png)\r\n").unwrap();
        fs::write(root.path().join("Old/sub/b.md"), "# B\n^id\n[[../a]]\n").unwrap();
        fs::write(root.path().join("Outside.md"), "---\r\nrelated: '[[Old/a#Привет|e\u{301}]]'\r\n---\r\n![[Old/sub/b#^id|🧠]]\r\n![p](Old/media/p.png)\r\n").unwrap();
        fs::write(root.path().join("Old/media/p.png"), [0, 255, 1, 128]).unwrap();
        fs::write(root.path().join("Old/._a.md"), [255, 254, 0]).unwrap();
        fs::write(root.path().join("Old/bad.md"), [255, 0]).unwrap();
        root
    }

    #[test]
    fn directory_move_updates_lossless_links_assets_and_restores_preimages() {
        let root = fixture();
        let state = tempfile::tempdir().unwrap();
        let before: BTreeMap<_, _> = ["Old/a.md", "Old/sub/b.md", "Outside.md"]
            .map(|p| (p, fs::read(root.path().join(p)).unwrap()))
            .into_iter()
            .collect();
        let preview = Preview::prepare(root.path(), "Old", "Parent/New").unwrap();
        assert!(preview.directory.is_some());
        assert!(preview.skipped_files.iter().any(|s| s.path == "Old/bad.md"));
        assert!(preview.changes.iter().any(|c| c.path == "Outside.md"));
        assert!(preview.changes.iter().any(|c| c.path == "Old/a.md"));
        let result = preview
            .apply(root.path(), state.path(), &mut BTreeMap::new())
            .unwrap();
        assert!(result.moved, "{:?}", result.warning);
        assert!(result.warning.is_none(), "{:?}", result.warning);
        assert!(!root.path().join("Old").exists());
        let versions = crate::source_history::move_versions(state.path(), root.path()).unwrap();
        assert!(versions.versions.iter().any(|v| v.note
            == root.path().canonicalize().unwrap().join("Parent/New/a.md")
            && v.text.as_bytes() == before["Old/a.md"]));
        let moved = fs::read_to_string(root.path().join("Parent/New/a.md")).unwrap();
        assert!(
            moved.starts_with("\u{feff}# Привет\r\n[[./sub/b#^id|🧠]]\r\n"),
            "{moved}"
        );
        assert!(moved.contains("[out](../../Outside.md)"), "{moved}");
        assert!(moved.contains("![asset](media/p.png)"), "{moved}");
        let outside = fs::read_to_string(root.path().join("Outside.md")).unwrap();
        assert!(
            outside.contains("'[[Parent/New/a#Привет|e\u{301}]]'"),
            "{outside}"
        );
        assert!(
            outside.contains("![[Parent/New/sub/b#^id|🧠]]"),
            "{outside}"
        );
        assert!(
            outside.contains("![p](./Parent/New/media/p.png)"),
            "{outside}"
        );
        assert_eq!(
            fs::read(root.path().join("Parent/New/media/p.png")).unwrap(),
            [0, 255, 1, 128]
        );
        assert_eq!(
            fs::read(root.path().join("Parent/New/._a.md")).unwrap(),
            [255, 254, 0]
        );
        assert_eq!(
            fs::read(root.path().join("Parent/New/bad.md")).unwrap(),
            [255, 0]
        );
        Operation::revert(&result.journal, state.path()).unwrap();
        for (path, bytes) in before {
            assert_eq!(fs::read(root.path().join(path)).unwrap(), bytes);
        }
        assert!(!root.path().join("Parent/New").exists());
    }

    #[test]
    fn indexed_directory_preview_matches_full_scan_and_includes_changed_sources() {
        let root = fixture();
        fs::write(root.path().join("Unrelated.md"), "unrelated").unwrap();
        let (_, snapshot, _) =
            crate::vault::warm::reconcile(root.path(), None, false, &mut |_, _| Ok(())).unwrap();
        let index = CandidateIndex::from_snapshot(&snapshot);
        let full = Preview::prepare(root.path(), "Old", "Parent/New").unwrap();
        let fast = Preview::prepare_with(
            root.path(),
            "Old",
            "Parent/New",
            Some(&index),
            &mut |_, _| Ok(()),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&full.changes).unwrap(),
            serde_json::to_value(&fast.changes).unwrap()
        );
        assert!(fast.timings.files_read < full.timings.files_read);
        fs::write(root.path().join("Unrelated.md"), "[[Old/a]]").unwrap();
        assert!(fast.validate(root.path()).is_err());
        let refreshed = Preview::prepare_with(
            root.path(),
            "Old",
            "Parent/New",
            Some(&index),
            &mut |_, _| Ok(()),
        )
        .unwrap();
        assert!(refreshed.changes.iter().any(|c| c.path == "Unrelated.md"));
    }

    #[test]
    fn directory_rejects_preview_changes_collisions_nested_target_and_preserves_binary_edits() {
        let root = fixture();
        let state = tempfile::tempdir().unwrap();
        let preview = Preview::prepare(root.path(), "Old", "Parent/New").unwrap();
        fs::write(root.path().join("Old/media/p.png"), b"external").unwrap();
        assert!(preview
            .apply(root.path(), state.path(), &mut BTreeMap::new())
            .is_err());
        assert!(root.path().join("Old/a.md").exists());
        assert!(Preview::prepare(root.path(), "Old", "Old/sub/New").is_err());
        fs::create_dir(root.path().join("Existing")).unwrap();
        assert!(Preview::prepare(root.path(), "Old", "Existing").is_err());
        let preview = Preview::prepare(root.path(), "Old", "Parent/New").unwrap();
        let moved = preview
            .apply(root.path(), state.path(), &mut BTreeMap::new())
            .unwrap();
        assert!(moved.moved, "{:?}", moved.warning);
        fs::write(root.path().join("Parent/New/media/p.png"), b"new external").unwrap();
        assert!(Operation::revert(&moved.journal, state.path()).is_err());
        assert_eq!(
            fs::read(root.path().join("Parent/New/media/p.png")).unwrap(),
            b"new external"
        );
    }

    #[test]
    fn directory_destination_preserves_orphaned_descendant_drafts() {
        let root = fixture();
        let state = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("Parent/New")).unwrap();
        let destination = root.path().join("Parent/New/a.md");
        fs::write(&destination, "Old destination").unwrap();
        let mut editor =
            crate::file_editor::FileEditor::open(&destination, &state.path().join("editor-drafts"))
                .unwrap();
        editor.set_text("Unsaved destination draft".into()).unwrap();
        drop(editor);
        fs::remove_file(&destination).unwrap();
        fs::remove_dir(root.path().join("Parent/New")).unwrap();
        let preview = Preview::prepare(root.path(), "Old", "Parent/New").unwrap();
        let error = preview
            .apply(root.path(), state.path(), &mut BTreeMap::new())
            .err()
            .unwrap();
        assert!(error.to_string().contains("recovery draft"), "{error:#}");
        assert!(root.path().join("Old/a.md").exists());
        assert!(!root.path().join("Parent/New").exists());
    }

    #[test]
    fn empty_directory_moves_and_reverts_without_making_a_markdown_file() {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("Empty")).unwrap();
        let preview = Preview::prepare(root.path(), "Empty", "New Name").unwrap();
        let moved = preview
            .apply(root.path(), state.path(), &mut BTreeMap::new())
            .unwrap();
        assert!(moved.moved, "{:?}", moved.warning);
        assert!(root.path().join("New Name").is_dir());
        Operation::revert(&moved.journal, state.path()).unwrap();
        assert!(root.path().join("Empty").is_dir());
        let snapshot = DirectorySnapshot::read(root.path(), Path::new("Empty")).unwrap();
        let plan = DirectoryMovePlan::prepare(
            root.path(),
            Path::new("Empty"),
            Path::new("Racing"),
            &snapshot,
        )
        .unwrap();
        fs::create_dir(root.path().join("Racing")).unwrap();
        assert!(plan.commit().is_err());
        assert!(root.path().join("Empty").is_dir());
    }
}
