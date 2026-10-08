//! Explicit, bounded inventory of possible Syncthing conflict copies. This is
//! filename evidence, not proof of an unresolved conflict or synchronization.
//! No content is read or changed; no daemon or background scan is started.
use anyhow::Result;
use rustix::fs::{open, openat, Dir, FileType, Mode, OFlags};
use std::{
    ffi::OsStr,
    os::{fd::OwnedFd, unix::ffi::OsStrExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct Inventory {
    pub copies: Vec<PathBuf>,
    /// False for inaccessible entries, resource limits or interrupted enumeration.
    /// Even a complete snapshot makes no assertion about later filesystem changes.
    pub complete: bool,
}

pub fn inspect(root: &Path) -> Result<Inventory> {
    inspect_with_limit(root, 100_000)
}
fn inspect_with_limit(root: &Path, limit: usize) -> Result<Inventory> {
    let fd = open(root, flags(), Mode::empty())?;
    let mut scan = Scan {
        inventory: Inventory {
            copies: vec![],
            complete: true,
        },
        remaining: limit,
        deadline: Instant::now() + Duration::from_secs(2),
    };
    scan.walk(fd, Path::new(""), 0);
    scan.inventory.copies.sort();
    Ok(scan.inventory)
}
fn flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}
struct Scan {
    inventory: Inventory,
    remaining: usize,
    deadline: Instant,
}
impl Scan {
    fn walk(&mut self, fd: OwnedFd, relative: &Path, depth: usize) {
        let Ok(entries) = Dir::read_from(&fd) else {
            self.inventory.complete = false;
            return;
        };
        for entry in entries {
            if self.remaining == 0
                || Instant::now() >= self.deadline
                || self.inventory.copies.len() >= 50
            {
                self.inventory.complete = false;
                return;
            }
            self.remaining -= 1;
            let Ok(entry) = entry else {
                self.inventory.complete = false;
                continue;
            };
            let bytes = entry.file_name().to_bytes();
            if matches!(
                bytes,
                b"." | b".." | b".stversions" | b".stfolder" | b".tessera-index"
            ) {
                continue;
            }
            let path = relative.join(OsStr::from_bytes(bytes));
            match entry.file_type() {
                FileType::Directory if depth < 64 => {
                    // Open relative to the enumerated directory, never through a
                    // potentially replaced ancestor or a directory symlink.
                    match openat(&fd, entry.file_name(), flags(), Mode::empty()) {
                        Ok(child) => self.walk(child, &path, depth + 1),
                        Err(_) => self.inventory.complete = false,
                    }
                }
                FileType::Directory => self.inventory.complete = false,
                FileType::RegularFile if possible_copy(bytes) => self.inventory.copies.push(path),
                FileType::Unknown => self.inventory.complete = false,
                _ => {}
            }
        }
    }
}
fn possible_copy(name: &[u8]) -> bool {
    let marker = b".sync-conflict-";
    name.windows(marker.len()).enumerate().any(|(index, part)| {
        if index == 0 || part != marker {
            return false;
        }
        let suffix = &name[index + marker.len()..];
        suffix.len() >= 23
            && suffix[..8].iter().all(u8::is_ascii_digit)
            && suffix[8] == b'-'
            && suffix[9..15].iter().all(u8::is_ascii_digit)
            && suffix[15] == b'-'
            && suffix[16..23]
                .iter()
                .all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(b))
            && (suffix.len() == 23 || suffix[23] == b'.')
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_copies_without_changing_files_or_following_links() {
        let vault = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let copy = "note.sync-conflict-20261007-123456-ABCDEFG.md";
        std::fs::write(vault.path().join("note.md"), "original").unwrap();
        std::fs::create_dir(vault.path().join("nested")).unwrap();
        std::fs::write(vault.path().join("nested").join(copy), "other version").unwrap();
        std::fs::write(outside.path().join(copy), "outside").unwrap();
        std::os::unix::fs::symlink(outside.path(), vault.path().join("linked")).unwrap();
        std::fs::create_dir(vault.path().join(".stversions")).unwrap();
        std::fs::write(vault.path().join(".stversions").join(copy), "history").unwrap();
        let found = inspect(vault.path()).unwrap();
        assert!(found.complete);
        assert_eq!(found.copies, [PathBuf::from("nested").join(copy)]);
        assert_eq!(
            std::fs::read_to_string(vault.path().join("note.md")).unwrap(),
            "original"
        );
        assert_eq!(
            std::fs::read_to_string(vault.path().join("nested").join(copy)).unwrap(),
            "other version"
        );
        assert!(!inspect_with_limit(vault.path(), 1).unwrap().complete);
    }
    #[test]
    fn missing_folder_and_similar_names_are_not_clean_conflict_receipts() {
        assert!(inspect(Path::new("/nonexistent-tessera-conflict-fixture")).is_err());
        assert!(possible_copy(b"a.sync-conflict-20261007-123456-ABC2345.md"));
        assert!(possible_copy(b"a.sync-conflict-20261007-123456-ABC2345"));
        assert!(!possible_copy(b"a.sync-conflict-not-a-timestamp.md"));
        assert!(!possible_copy(
            b"a.sync-conflict-20261007-123456-ABC2345extra.md"
        ));
    }
}
