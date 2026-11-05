//! Read-only, bounded discovery of Syncthing's default local version store.
//! Archives stay canonical to Syncthing: never prune or consume them here.
use super::{Listing, Version};
use anyhow::{ensure, Result};
use rustix::fs::{open, openat, Dir, FileType, Mode, OFlags};
use std::{
    fs::File,
    io::Read,
    path::{Component, Path},
};

const MAX_ENTRIES: usize = 10_000;
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_VERSIONS: usize = 100;
fn flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

/// `note` restricts discovery to one vault-relative note. None discovers deleted
/// notes for Recover notes. Every ancestry is opened descriptor-relative.
pub fn list(root: &Path, note: Option<&Path>) -> Result<Listing> {
    if let Some(note) = note {
        ensure!(
            !note.as_os_str().is_empty()
                && note.components().all(|c| matches!(c, Component::Normal(_))),
            "Invalid history note path"
        );
    }
    let canonical = root.canonicalize()?;
    ensure!(root == canonical, "The history vault path changed");
    let root_fd = open(root, flags(), Mode::empty())?;
    let store = match openat(&root_fd, ".stversions", flags(), Mode::empty()) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(Listing::default()),
        Err(error) => return Err(error.into()),
    };
    let mut listing = Listing::default();
    let mut entries = 0;
    let mut bytes = 0;
    visit(
        &store,
        Path::new(""),
        &canonical,
        note,
        &mut entries,
        &mut bytes,
        &mut listing,
        0,
    )?;
    if entries >= MAX_ENTRIES || bytes >= MAX_BYTES || listing.versions.len() >= MAX_VERSIONS {
        listing.warnings.push("Sync history limit reached; results are incomplete. Open Note history for a specific note to narrow the search.".into());
    }
    listing
        .versions
        .sort_by_key(|v| std::cmp::Reverse(v.created));
    Ok(listing)
}

#[allow(clippy::too_many_arguments)]
fn visit(
    directory: &rustix::fd::OwnedFd,
    relative: &Path,
    root: &Path,
    note: Option<&Path>,
    entries: &mut usize,
    bytes: &mut usize,
    listing: &mut Listing,
    depth: usize,
) -> Result<()> {
    for entry in Dir::read_from(directory)? {
        if *entries >= MAX_ENTRIES || *bytes >= MAX_BYTES || listing.versions.len() >= MAX_VERSIONS
        {
            break;
        }
        *entries += 1;
        let entry = entry?;
        let Ok(name) = entry.file_name().to_str() else {
            continue;
        };
        if name == "." || name == ".." {
            continue;
        }
        let rel = relative.join(name);
        let kind = if entry.file_type() == FileType::Unknown {
            match rustix::fs::statat(
                directory,
                entry.file_name(),
                rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
            ) {
                Ok(stat) => FileType::from_raw_mode(stat.st_mode),
                Err(error) => {
                    listing.warnings.push(format!(
                        "Cannot inspect Sync archive {}: {error}",
                        rel.display()
                    ));
                    continue;
                }
            }
        } else {
            entry.file_type()
        };
        if kind == FileType::Symlink {
            listing.warnings.push(format!(
                "Sync archive {} is a symbolic link; it was not followed",
                rel.display()
            ));
            continue;
        }
        if kind == FileType::Directory {
            if note.is_some_and(|n| !n.starts_with(&rel)) {
                continue;
            }
            if depth >= 64 {
                listing.warnings.push(
                    "Sync history directory depth limit reached; results are incomplete.".into(),
                );
                continue;
            }
            let result = (|| -> Result<()> {
                let child = openat(directory, entry.file_name(), flags(), Mode::empty())?;
                visit(&child, &rel, root, note, entries, bytes, listing, depth + 1)
            })();
            if let Err(error) = result {
                listing.warnings.push(format!(
                    "Cannot read Sync history folder {}: {error}",
                    rel.display()
                ));
            }
            continue;
        }
        let Some((original, created)) = original_name(name) else {
            continue;
        };
        let original = relative.join(original);
        if crate::vault::service_path(&original) {
            continue;
        }
        if note.is_some_and(|n| n != original) {
            continue;
        }
        // Recover notes has no current-note selector. Live notes must not use
        // its version or byte budget and hide unrelated deletions.
        if note.is_none() {
            match std::fs::symlink_metadata(root.join(&original)) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Ok(_) => continue,
                Err(error) => {
                    listing.warnings.push(format!(
                        "Cannot inspect original note {}: {error}",
                        original.display()
                    ));
                    continue;
                }
            }
        }
        let read = (|| -> Result<Vec<u8>> {
            let fd = openat(
                directory,
                entry.file_name(),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            let file = File::from(fd);
            ensure!(file.metadata()?.is_file(), "Not a regular archive");
            let mut data = Vec::new();
            file.take((MAX_BYTES - *bytes + 1) as u64)
                .read_to_end(&mut data)?;
            if data.len() > MAX_BYTES - *bytes {
                *bytes = MAX_BYTES;
                anyhow::bail!("Sync version exceeds preview limit");
            }
            Ok(data)
        })();
        match read {
            Ok(data) => {
                *bytes += data.len();
                listing.versions.push(Version {
                    note: root.join(original),
                    text: String::from_utf8_lossy(&data).into_owned(),
                    raw: Some(data),
                    created,
                    label: "From another device · Sync".into(),
                    protected: false,
                    link_move: false,
                });
            }
            Err(error) => listing.warnings.push(format!(
                "Cannot read Sync version {}: {error}",
                rel.display()
            )),
        }
    }
    Ok(())
}

fn original_name(name: &str) -> Option<(String, u64)> {
    // Syncthing tags immediately before the extension (or after a bare name).
    let (stem, extension) = name
        .rsplit_once('.')
        .map_or((name, String::new()), |(s, e)| (s, format!(".{e}")));
    let (stem, stamp) = stem.rsplit_once('~')?;
    let format = time::macros::format_description!("[year][month][day]-[hour][minute][second]");
    if stamp.len() != 15 {
        return None;
    }
    let date = time::PrimitiveDateTime::parse(stamp, &format)
        .ok()?
        .assume_utc();
    let micros = u64::try_from(date.unix_timestamp_nanos() / 1000).ok()?;
    let original = format!("{stem}{extension}");
    if original
        .rsplit_once('.')
        .is_none_or(|(_, extension)| !extension.eq_ignore_ascii_case("md"))
    {
        return None;
    }
    Some((original, micros))
}

/// Restore a reviewed deletion by exclusive creation. An appearing local edit
/// fails publication; the retained archive remains available for another try.
pub fn restore_deleted(root: &Path, note: &Path, drafts: &Path, bytes: &[u8]) -> Result<()> {
    super::save_copy_bytes(root, note, drafts, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn issue2045_archives_are_exact_bounded_and_restore_never_clobbers() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        fs::create_dir_all(root.join(".stversions/nested"))?;
        let raw = b"\xef\xbb\xbf# old\r\n\xff\r\n";
        fs::write(root.join(".stversions/nested/note~20261102-120000.md"), raw)?;
        fs::write(
            root.join(".stversions/nested/note~20260230-120000.md"),
            "invalid",
        )?;
        fs::create_dir(root.join("nested"))?;
        let listing = list(&root, Some(Path::new("nested/note.md")))?;
        assert_eq!(listing.versions.len(), 1);
        let v = &listing.versions[0];
        assert_eq!(v.bytes(), raw);
        let state = tempfile::tempdir()?;
        restore_deleted(&root, Path::new("nested/note.md"), state.path(), v.bytes())?;
        assert_eq!(fs::read(&v.note)?, raw);
        fs::write(&v.note, "new local edit")?;
        assert!(restore_deleted(&root, Path::new("nested/note.md"), state.path(), raw).is_err());
        assert_eq!(fs::read(&v.note)?, b"new local edit");
        assert!(list(&root, Some(Path::new("../escape.md"))).is_err());
        assert!(list(&root, None)?.versions.is_empty());
        let mut editor = crate::file_editor::FileEditor::open(&v.note, state.path())?;
        editor.set_text("unsaved local draft".into())?;
        drop(editor);
        fs::remove_file(&v.note)?;
        assert!(restore_deleted(&root, Path::new("nested/note.md"), state.path(), raw).is_err());
        assert!(
            !v.note.exists(),
            "a dirty recovery journal must block Restore"
        );
        Ok(())
    }
    #[test]
    fn issue2045_symlinks_never_escape_and_scan_limits_are_visible() -> Result<()> {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let outside = tempfile::tempdir()?;
        fs::write(outside.path().join("secret~20261102-120000.md"), "secret")?;
        symlink(outside.path(), root.join(".stversions"))?;
        assert!(list(&root, None).is_err());
        fs::remove_file(root.join(".stversions"))?;
        fs::create_dir(root.join(".stversions"))?;
        symlink(outside.path(), root.join(".stversions/escape"))?;
        symlink(
            outside.path().join("secret~20261102-120000.md"),
            root.join(".stversions/note~20261102-120000.md"),
        )?;
        let l = list(&root, None)?;
        assert!(l.versions.is_empty());
        assert!(!l.warnings.is_empty());
        fs::remove_file(root.join(".stversions/note~20261102-120000.md"))?;
        for n in 0..101 {
            fs::write(
                root.join(format!(".stversions/n{n}~20261102-120000.md")),
                "retained",
            )?;
        }
        let l = list(&root, None)?;
        assert_eq!(l.versions.len(), 100);
        assert!(!l.warnings.is_empty());
        Ok(())
    }
    #[test]
    fn issue2045_live_versions_do_not_hide_deleted_notes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        fs::create_dir_all(root.join(".stversions/live"))?;
        fs::create_dir(root.join("live"))?;
        fs::write(root.join("live/note.md"), "current")?;
        for n in 0..150 {
            fs::write(
                root.join(format!(
                    ".stversions/live/note~20261102-12{:02}{:02}.md",
                    n / 60,
                    n % 60
                )),
                "old live version",
            )?;
        }
        // A separate subtree ensures the live archives are encountered as a
        // group, independently of filesystem directory ordering.
        fs::create_dir(root.join(".stversions/deleted"))?;
        fs::write(
            root.join(".stversions/deleted/note~20261102-130000.md"),
            "deleted bytes",
        )?;
        let listing = list(&root, None)?;
        assert_eq!(listing.versions.len(), 1);
        assert_eq!(listing.versions[0].note, root.join("deleted/note.md"));
        assert_eq!(listing.versions[0].bytes(), b"deleted bytes");
        assert!(listing.warnings.is_empty());
        // Specific Note history still discovers live versions and caps them.
        assert_eq!(
            list(&root, Some(Path::new("live/note.md")))?.versions.len(),
            100
        );
        Ok(())
    }
    #[test]
    fn issue2045_oversized_archive_exhausts_the_read_budget_once() -> Result<()> {
        let root = tempfile::tempdir()?;
        let root = root.path().canonicalize()?;
        fs::create_dir(root.join(".stversions"))?;
        for name in ["first", "second"] {
            fs::File::create(root.join(format!(".stversions/{name}~20261102-120000.md")))?
                .set_len((MAX_BYTES + 1) as u64)?;
        }
        let listing = list(&root, None)?;
        assert!(listing.versions.is_empty());
        assert_eq!(
            listing
                .warnings
                .iter()
                .filter(|w| w.contains("exceeds preview limit"))
                .count(),
            1,
            "the first oversized read must stop further reads"
        );
        assert!(listing.warnings.iter().any(|w| w.contains("limit reached")));
        Ok(())
    }
}
