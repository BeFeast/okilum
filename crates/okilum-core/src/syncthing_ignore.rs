//! Syncthing's `.stignore` in a vault that is a Syncthing folder (#1248).
//!
//! Okilum keeps per-machine files inside the vault folder: the search index and the temp and
//! recovery files of a safe save. When the folder is shared by Syncthing and its `.stignore`
//! does not list them, Syncthing spreads them to every peer. This module answers one question,
//! whether the Okilum lines are there, and makes one edit, appending a block with a comment.
//! Every other byte of the file is left as it is.
//!
//! Only the vault root is looked at (`.stfolder` there). `#include` is not followed. A pattern
//! counts as covered when a line names it (`x`, `/x` or `**/x`, with the `(?i)`/`(?d)` flags)
//! and the first line that names it is not a negation; broader globs such as `.okilum-*` are not
//! evaluated, so a vault that ignores the files that way is offered a redundant block, never a
//! missing one.
use anyhow::{bail, Context, Result};
use std::{fs::OpenOptions, io::Write as _, path::Path};

/// The folder marker Syncthing keeps at the root of a shared folder.
pub const MARKER: &str = ".stfolder";
pub const IGNORE_FILE: &str = ".stignore";
/// What Okilum needs ignored: the derived index and the in-flight safe-save files.
pub const PATTERNS: [&str; 2] = [".okilum-index", ".okilum-save-*"];
/// The comment of the appended block (the wording of the owner's hand-edited file).
pub const BLOCK_COMMENT: &str =
    "// Okilum: derived index and in-flight safe-save temp files are per-machine. Never sync.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// The vault root has no `.stfolder`: not a Syncthing folder, nothing to offer.
    NotSyncthing,
    /// Every Okilum pattern is ignored.
    Covered,
    /// These patterns are not ignored (all of them when `.stignore` does not exist).
    Missing(Vec<&'static str>),
    /// `.stignore` includes other files, which are not followed: the answer is unknown.
    Include,
    /// `.stignore` or the marker could not be read.
    Unreadable(String),
}

/// What [`add_block`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Added {
    /// The block with these patterns was appended (or the file created).
    Appended(Vec<&'static str>),
    /// Nothing to add: the file already covers them.
    AlreadyCovered,
}

pub fn status(root: &Path) -> Status {
    match std::fs::symlink_metadata(root.join(MARKER)) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Status::NotSyncthing,
        Err(error) => return Status::Unreadable(format!("{MARKER}: {error}")),
    }
    match std::fs::read(root.join(IGNORE_FILE)) {
        Ok(bytes) => status_of(&bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Status::Missing(PATTERNS.to_vec())
        }
        Err(error) => Status::Unreadable(format!("{IGNORE_FILE}: {error}")),
    }
}

/// The status of a Syncthing folder whose `.stignore` holds `bytes`.
pub fn status_of(bytes: &[u8]) -> Status {
    let text = String::from_utf8_lossy(bytes);
    let mut includes = false;
    // `None`: no line names the pattern yet; `Some(ignored)`: the first line that does.
    let mut first: [Option<bool>; PATTERNS.len()] = [None; PATTERNS.len()];
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("#include") {
            includes = true;
            continue;
        }
        if line.is_empty() || line.starts_with("//") || line.starts_with('#') {
            continue;
        }
        let (negated, pattern) = split_line(line);
        for (ix, wanted) in PATTERNS.iter().enumerate() {
            if first[ix].is_none() && names(pattern, wanted) {
                first[ix] = Some(!negated);
            }
        }
    }
    let missing: Vec<&'static str> = PATTERNS
        .iter()
        .zip(first)
        .filter(|(_, ignored)| *ignored != Some(true))
        .map(|(pattern, _)| *pattern)
        .collect();
    if missing.is_empty() {
        Status::Covered
    } else if includes {
        Status::Include
    } else {
        Status::Missing(missing)
    }
}

/// `(negated, pattern)` of one `.stignore` line, without the `(?i)` / `(?d)` flags.
fn split_line(line: &str) -> (bool, &str) {
    let mut rest = line;
    let mut negated = false;
    loop {
        if let Some(after) = rest.strip_prefix('!') {
            negated = true;
            rest = after;
        } else if let Some(after) = rest.strip_prefix("(?i)").or_else(|| rest.strip_prefix("(?d)"))
        {
            rest = after;
        } else {
            return (negated, rest);
        }
    }
}

fn names(pattern: &str, wanted: &str) -> bool {
    pattern == wanted
        || pattern.strip_prefix('/') == Some(wanted)
        || pattern.strip_prefix("**/") == Some(wanted)
}

/// Append the Okilum block to the vault's `.stignore` (creating it when it does not exist).
///
/// Only a [`Status::Missing`] vault is edited. The file is read again here and the append is
/// refused if it changed since, so an edit made by someone else in the meantime is never
/// overwritten; existing bytes, line endings and a missing final newline are kept.
pub fn add_block(root: &Path) -> Result<Added> {
    match std::fs::symlink_metadata(root.join(MARKER)) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("{} is not a Syncthing folder", root.display())
        }
        Err(error) => return Err(error).context(MARKER),
    }
    let path = root.join(IGNORE_FILE);
    let existing = match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).with_context(|| format!("reading {IGNORE_FILE}")),
    };
    let missing = match existing.as_deref().map(status_of) {
        None => PATTERNS.to_vec(),
        Some(Status::Covered) => return Ok(Added::AlreadyCovered),
        Some(Status::Missing(missing)) => missing,
        Some(Status::Include) => bail!(
            "{IGNORE_FILE} includes other files; add {} there by hand",
            PATTERNS.join(" and ")
        ),
        Some(_) => bail!("{IGNORE_FILE} could not be checked"),
    };
    let block = block_for(existing.as_deref().unwrap_or_default(), &missing);
    match existing {
        None => {
            // `create_new`: a file that appeared in the meantime is not overwritten.
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .with_context(|| format!("creating {IGNORE_FILE}"))?;
            file.write_all(block.as_bytes())
                .with_context(|| format!("writing {IGNORE_FILE}"))?;
        }
        Some(bytes) => {
            let mut file = OpenOptions::new()
                .append(true)
                .open(&path)
                .with_context(|| format!("opening {IGNORE_FILE}"))?;
            let len = file.metadata().context(IGNORE_FILE)?.len();
            if len != bytes.len() as u64 {
                bail!("{IGNORE_FILE} changed while it was being checked; try again");
            }
            file.write_all(block.as_bytes())
                .with_context(|| format!("writing {IGNORE_FILE}"))?;
        }
    }
    Ok(Added::Appended(missing))
}

/// The bytes to append to a file holding `existing`: a blank line apart, the file's own line
/// ending, the comment and the missing patterns.
fn block_for(existing: &[u8], missing: &[&str]) -> String {
    let eol = if existing.windows(2).any(|pair| pair == b"\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut block = String::new();
    if !existing.is_empty() {
        if !existing.ends_with(b"\n") {
            block.push_str(eol);
        }
        block.push_str(eol);
    }
    block.push_str(BLOCK_COMMENT);
    block.push_str(eol);
    for pattern in missing {
        block.push_str(pattern);
        block.push_str(eol);
    }
    block
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(ignore: Option<&[u8]>) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(MARKER)).unwrap();
        if let Some(bytes) = ignore {
            std::fs::write(root.path().join(IGNORE_FILE), bytes).unwrap();
        }
        root
    }

    #[test]
    fn a_folder_that_is_not_shared_by_syncthing_gets_no_offer() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(IGNORE_FILE), b"x\n").unwrap();
        assert_eq!(status(root.path()), Status::NotSyncthing);
        assert!(add_block(root.path()).is_err());
        // It was not edited either.
        assert_eq!(
            std::fs::read(root.path().join(IGNORE_FILE)).unwrap(),
            b"x\n"
        );
    }

    #[test]
    fn missing_lines_are_offered_and_present_ones_are_not() {
        let none = folder(Some(b".tessera-index\n"));
        assert_eq!(status(none.path()), Status::Missing(PATTERNS.to_vec()));
        let absent = folder(None);
        assert_eq!(status(absent.path()), Status::Missing(PATTERNS.to_vec()));
        let one = folder(Some(b".okilum-index\n"));
        assert_eq!(status(one.path()), Status::Missing(vec![".okilum-save-*"]));
        let both = folder(Some(b".okilum-index\n.okilum-save-*\n"));
        assert_eq!(status(both.path()), Status::Covered);
    }

    #[test]
    fn the_owners_hand_edited_block_counts_as_covered() {
        let owner = folder(Some(
            b"// Tessera search index: derived data, per-machine, rebuilt on demand. Never sync.\n\
.tessera-index\n\
// Okilum: derived index and in-flight safe-save temp files are per-machine. Never sync.\n\
.okilum-index\n\
.okilum-save-*\n\
\n\
/.claude/worktrees\n",
        ));
        assert_eq!(status(owner.path()), Status::Covered);
    }

    #[test]
    fn rooted_and_nested_forms_and_flags_count_and_negations_and_comments_do_not() {
        for covered in [
            &b"/.okilum-index\n**/.okilum-save-*\n"[..],
            b"(?i).okilum-index\n(?d)(?i)**/.okilum-save-*\n",
            b"  .okilum-index  \r\n.okilum-save-*\r\n",
        ] {
            assert_eq!(status_of(covered), Status::Covered, "{covered:?}");
        }
        // Comments name nothing; a negation that comes first wins, one that comes later does not.
        assert_eq!(
            status_of(b"// .okilum-index\n# .okilum-save-*\n"),
            Status::Missing(PATTERNS.to_vec())
        );
        assert_eq!(
            status_of(b"!.okilum-index\n.okilum-index\n.okilum-save-*\n"),
            Status::Missing(vec![".okilum-index"])
        );
        assert_eq!(
            status_of(b".okilum-index\n!.okilum-index\n.okilum-save-*\n"),
            Status::Covered
        );
    }

    #[test]
    fn an_include_is_not_followed() {
        assert_eq!(status_of(b"#include shared.txt\n"), Status::Include);
        // What the file itself names still counts.
        assert_eq!(
            status_of(b"#include shared.txt\n.okilum-index\n.okilum-save-*\n"),
            Status::Covered
        );
        let root = folder(Some(b"#include shared.txt\n"));
        let before = std::fs::read(root.path().join(IGNORE_FILE)).unwrap();
        assert!(add_block(root.path()).is_err());
        assert_eq!(std::fs::read(root.path().join(IGNORE_FILE)).unwrap(), before);
    }

    #[test]
    fn the_block_is_appended_and_every_other_byte_stays() {
        // LF, a missing final newline, CRLF, and bytes that are not UTF-8.
        let cases: [&[u8]; 5] = [
            b".tessera-index\n",
            b".tessera-index",
            b".tessera-index\r\n// c\r\n",
            b"caf\xE9\n\xFF\xFE\n",
            b"\n",
        ];
        for original in cases {
            let root = folder(Some(original));
            let added = add_block(root.path()).unwrap();
            assert_eq!(added, Added::Appended(PATTERNS.to_vec()));
            let after = std::fs::read(root.path().join(IGNORE_FILE)).unwrap();
            assert!(
                after.starts_with(original),
                "{original:?} was changed: {after:?}"
            );
            let appended = String::from_utf8(after[original.len()..].to_vec()).unwrap();
            let eol = if original.windows(2).any(|pair| pair == b"\r\n") {
                "\r\n"
            } else {
                "\n"
            };
            assert!(appended.contains(BLOCK_COMMENT), "{appended:?}");
            assert!(appended.contains(".okilum-index") && appended.contains(".okilum-save-*"));
            assert!(
                appended.lines().count() >= 3
                    && appended.ends_with(eol)
                    && appended.matches(eol).count() == appended.matches('\n').count(),
                "one line ending style: {appended:?}"
            );
            // The result is covered, and a second call adds nothing.
            assert_eq!(status(root.path()), Status::Covered, "{original:?}");
            assert_eq!(add_block(root.path()).unwrap(), Added::AlreadyCovered);
            assert_eq!(std::fs::read(root.path().join(IGNORE_FILE)).unwrap(), after);
        }
    }

    #[test]
    fn the_appended_block_is_the_owners_format() {
        let root = folder(Some(b".tessera-index\n"));
        add_block(root.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join(IGNORE_FILE)).unwrap(),
            ".tessera-index\n\n\
// Okilum: derived index and in-flight safe-save temp files are per-machine. Never sync.\n\
.okilum-index\n\
.okilum-save-*\n"
        );
    }

    #[test]
    fn only_the_missing_patterns_are_added_and_a_missing_file_is_created() {
        let one = folder(Some(b".okilum-index\n"));
        assert_eq!(
            add_block(one.path()).unwrap(),
            Added::Appended(vec![".okilum-save-*"])
        );
        let text = std::fs::read_to_string(one.path().join(IGNORE_FILE)).unwrap();
        assert_eq!(text.matches(".okilum-index").count(), 1, "{text:?}");
        assert_eq!(status(one.path()), Status::Covered);

        let absent = folder(None);
        add_block(absent.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(absent.path().join(IGNORE_FILE)).unwrap(),
            format!("{BLOCK_COMMENT}\n.okilum-index\n.okilum-save-*\n")
        );
    }
}
