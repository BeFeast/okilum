//! Durable, URL-bound remote drafts, separate from rebuildable indexes.
use super::{
    save::{Attempt, Revision},
    Source, BYTE_LIMIT,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub version: u32,
    pub url: String,
    pub base: String,
    pub text: String,
    pub pending: Option<Attempt>,
}
impl Record {
    pub fn dirty(&self) -> bool {
        self.text != self.base || self.pending.is_some()
    }
    pub fn attempt(&mut self) -> Attempt {
        let base = Revision::of(&self.base);
        if self
            .pending
            .as_ref()
            .is_none_or(|a| a.base != base || a.text != self.text)
        {
            self.pending = Some(Attempt::new(base, self.text.clone()));
        }
        self.pending.clone().unwrap()
    }
    pub fn acknowledge(&mut self, attempt: &Attempt) {
        self.base = attempt.text.clone();
        self.pending = None;
    }
    fn validate(&self, source: &Source) -> io::Result<()> {
        if self.version != 1
            || self.url != source.url()
            || self.base.len() > BYTE_LIMIT
            || self.text.len() > BYTE_LIMIT
            || self.pending.as_ref().is_some_and(|a| {
                a.text.len() > BYTE_LIMIT
                    || a.base != Revision::of(&self.base)
                    || uuid::Uuid::parse_str(&a.id).is_err()
            })
        {
            return Err(io::Error::other(
                "Remote draft identity or size is invalid; its files were kept",
            ));
        }
        Ok(())
    }
}
struct Position {
    epoch: u64,
    revision: u64,
}
#[derive(Clone)]
pub struct Recovery {
    directory: PathBuf,
    position: Arc<Mutex<Position>>,
    _lock: Arc<crate::file_editor::EditorLock>,
}
impl Recovery {
    pub fn open(root: &Path, source: &Source, opened: String) -> io::Result<(Self, Record)> {
        Self::load(root, source, Some(opened))
    }
    /// Reopen a protected dirty draft even if the remote read is disconnected.
    pub fn recover(root: &Path, source: &Source) -> io::Result<(Self, Record)> {
        Self::load(root, source, None)
    }
    fn load(root: &Path, source: &Source, opened: Option<String>) -> io::Result<(Self, Record)> {
        let directory = root.join(format!("{:x}", Sha256::digest(source.url().as_bytes())));
        fs::create_dir_all(&directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        }
        let lock = crate::file_editor::EditorLock::acquire(&directory.join("owner.lock")).map_err(
            |_| {
                io::Error::other(
                    "This remote draft is already open or its recovery folder is unavailable",
                )
            },
        )?;
        let mut revision = 0;
        for entry in fs::read_dir(&directory)? {
            if let Some(n) = entry?
                .file_name()
                .to_str()
                .and_then(|s| s.strip_suffix(".json"))
                .and_then(|s| s.parse::<u64>().ok())
            {
                revision = revision.max(n);
            }
        }
        let mut record = if revision == 0 {
            let opened = opened.as_ref().ok_or_else(||io::Error::other("No protected local draft exists for this remote file. Reopen it when the connection is available."))?;
            Record {
                version: 1,
                url: source.url().into(),
                base: opened.clone(),
                text: opened.clone(),
                pending: None,
            }
        } else {
            let mut bytes = Vec::new();
            fs::File::open(directory.join(format!("{revision}.json")))?
                .take((BYTE_LIMIT * 18 + 65536 + 1) as u64)
                .read_to_end(&mut bytes)?;
            if bytes.len() > BYTE_LIMIT * 18 + 65536 {
                return Err(io::Error::other("Remote draft recovery exceeds its bound"));
            }
            serde_json::from_slice::<Record>(&bytes).map_err(io::Error::other)?
        };
        record.validate(source)?;
        if !record.dirty() {
            let opened=opened.ok_or_else(||io::Error::other("No unsaved local changes exist. Reopen the remote file when the connection is available."))?;
            record.base = opened.clone();
            record.text = opened;
        }
        let recovery = Self {
            directory,
            position: Arc::new(Mutex::new(Position { epoch: 0, revision })),
            _lock: Arc::new(lock),
        };
        recovery.persist(0, &record)?;
        Ok((recovery, record))
    }
    /// Stale background protection can never overwrite a newer explicit flush.
    pub fn persist(&self, epoch: u64, record: &Record) -> io::Result<bool> {
        let source = Source::parse(&record.url)?;
        record.validate(&source)?;
        let mut position = self
            .position
            .lock()
            .map_err(|_| io::Error::other("Remote recovery writer failed"))?;
        if epoch < position.epoch {
            return Ok(false);
        }
        let bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
        if bytes.len() > BYTE_LIMIT * 18 + 65536 {
            return Err(io::Error::other("Remote recovery exceeds its bound"));
        }
        let revision = position
            .revision
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Remote recovery revision exhausted"))?;
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist_noclobber(self.directory.join(format!("{revision}.json")))
            .map_err(|e| e.error)?;
        #[cfg(unix)]
        fs::File::open(&self.directory)?.sync_all()?;
        position.epoch = epoch;
        position.revision = revision;
        Ok(true)
    }
}
