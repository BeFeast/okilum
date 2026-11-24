//! Evidence saved with a shortcut before its source path disappears.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read},
    path::Path,
    time::SystemTime,
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FileId {
    volume: u64,
    file: u64,
    birth: SystemTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub id: Option<FileId>,
    pub size: Option<u64>,
    modified: Option<SystemTime>,
    pub hash: Option<[u8; 32]>,
}

impl Identity {
    pub fn metadata(path: &Path, native_ids: bool) -> Option<Self> {
        let meta = fs::symlink_metadata(path).ok()?;
        if !meta.is_file() && !meta.is_dir() {
            return None;
        }
        Some(Self {
            id: if native_ids && meta.is_file() {
                meta.created().ok().and_then(|birth| {
                    let (volume, file) =
                        okilum_core::vault::warm::SourceRevision::native_file_id(path)?;
                    Some(FileId {
                        volume,
                        file,
                        birth,
                    })
                })
            } else {
                None
            },
            size: meta.is_file().then_some(meta.len()),
            modified: meta.modified().ok(),
            hash: None,
        })
    }

    pub fn read(path: &Path, native_ids: bool, previous: Option<&Self>) -> Option<Self> {
        let mut current = Self::metadata(path, native_ids)?;
        if let Some(old) = previous.filter(|old| {
            current.id.is_some()
                && current.id == old.id
                && current.size == old.size
                && current.modified == old.modified
        }) {
            return Some(old.clone());
        }
        if let Some(size) = current.size {
            // Hash only this observed revision. Changing files are retried
            // on the next publication.
            let file = fs::File::open(path).ok()?;
            let mut digest = Sha256::new();
            if io::copy(&mut file.take(size.saturating_add(1)), &mut digest).ok()? != size
                || Self::metadata(path, native_ids).as_ref() != Some(&current)
            {
                return None;
            }
            current.hash = Some(digest.finalize().into());
        }
        Some(current)
    }
}
