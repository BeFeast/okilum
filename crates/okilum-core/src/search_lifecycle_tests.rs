//! Observe reload subscriptions without filesystem polling or sleeps.
use super::*;
use std::io;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use tantivy::directory::{
    error::{DeleteError, LockError, OpenReadError, OpenWriteError},
    DirectoryLock, FileHandle, Lock, MmapDirectory, WatchCallback, WatchCallbackList, WatchHandle,
    WritePtr, META_LOCK,
};

#[derive(Clone)]
struct ObservedDirectory {
    inner: MmapDirectory,
    callbacks: Arc<WatchCallbackList>,
    retained: Arc<Mutex<Vec<WatchHandle>>>,
    subscriptions: Arc<AtomicUsize>,
}
impl std::fmt::Debug for ObservedDirectory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObservedDirectory").finish_non_exhaustive()
    }
}
impl Directory for ObservedDirectory {
    fn get_file_handle(
        &self,
        path: &Path,
    ) -> std::result::Result<Arc<dyn FileHandle>, OpenReadError> {
        self.inner.get_file_handle(path)
    }
    fn delete(&self, path: &Path) -> std::result::Result<(), DeleteError> {
        self.inner.delete(path)
    }
    fn exists(&self, path: &Path) -> std::result::Result<bool, OpenReadError> {
        self.inner.exists(path)
    }
    fn open_write(&self, path: &Path) -> std::result::Result<WritePtr, OpenWriteError> {
        self.inner.open_write(path)
    }
    fn atomic_read(&self, path: &Path) -> std::result::Result<Vec<u8>, OpenReadError> {
        self.inner.atomic_read(path)
    }
    fn atomic_write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.inner.atomic_write(path, data)
    }
    fn sync_directory(&self) -> io::Result<()> {
        self.inner.sync_directory()
    }
    fn acquire_lock(&self, lock: &Lock) -> std::result::Result<DirectoryLock, LockError> {
        self.inner.acquire_lock(lock)
    }
    fn watch(&self, callback: WatchCallback) -> tantivy::Result<WatchHandle> {
        self.subscriptions.fetch_add(1, Ordering::SeqCst);
        // A real broadcast snapshots callbacks before starting its thread.
        // Retain this one explicitly so the test controls that late delivery.
        self.retained
            .lock()
            .unwrap()
            .push(self.callbacks.subscribe(callback.clone()));
        Ok(self.callbacks.subscribe(callback))
    }
}

#[test]
fn short_lived_queries_never_leave_delayed_reload_writers() -> Result<()> {
    let root = tempfile::tempdir()?;
    Searcher::build_documents(
        &[SearchDocument {
            path: "a.md".into(),
            title: "A".into(),
            text: "lifecycleprobe".into(),
        }],
        root.path(),
    )?
    .finish_build()?;
    let directory = ObservedDirectory {
        inner: MmapDirectory::open(root.path())?,
        callbacks: Arc::default(),
        retained: Arc::default(),
        subscriptions: Arc::default(),
    };
    let index = Index::open(directory.clone())?;
    index
        .tokenizers()
        .register(analyzer::TOKENIZER, analyzer::analyzer());
    let searcher = Searcher {
        f: Fields::from_schema(&index.schema())?,
        index,
        writer: Mutex::new(None),
        session: None,
        pin: None,
    };

    // Positive control: default Tantivy readers register a callback that can
    // recreate a lock file AFTER their reader and the build writer have dropped.
    drop(searcher.index.reader()?);
    assert_eq!(directory.subscriptions.load(Ordering::SeqCst), 1);
    let meta_lock = root.path().join(&META_LOCK.filepath);
    std::fs::remove_file(&meta_lock)?;
    directory.callbacks.broadcast().wait()?;
    assert!(
        meta_lock.is_file(),
        "late reload recreated the directory lock"
    );
    directory.retained.lock().unwrap().clear();
    directory.subscriptions.store(0, Ordering::SeqCst);

    assert_eq!(searcher.search("lifecycleprobe", 5)?.len(), 1);
    assert!(searcher.explain("lifecycleprobe", "a.md")?.is_some());
    assert_eq!(
        directory.subscriptions.load(Ordering::SeqCst),
        0,
        "query/explain must not leave callbacks that can write after finish_build"
    );
    searcher.finish_build()?;
    Ok(())
}

#[test]
fn rebuilding_a_live_writer_is_rejected_before_any_index_is_removed() -> Result<()> {
    let root = tempfile::tempdir()?;
    let documents = [SearchDocument {
        path: "a.md".into(),
        title: "A".into(),
        text: "lifecycleprobe".into(),
    }];
    let live = Searcher::build_documents(&documents, root.path())?;
    let metadata = std::fs::read(root.path().join("meta.json"))?;
    let result = Searcher::build_documents(&documents, root.path());
    assert!(
        result.is_err(),
        "a live writer must never be reclaimed as owned staging"
    );
    assert_eq!(std::fs::read(root.path().join("meta.json"))?, metadata);
    assert_eq!(live.search("lifecycleprobe", 5)?.len(), 1);
    live.finish_build()?;
    Searcher::build_documents(&documents, root.path())?.finish_build()?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn edit2803_search_checkpoints_share_immutable_segments_and_keep_independent_metadata() -> Result<()>
{
    use std::os::unix::fs::MetadataExt;
    let root = tempfile::tempdir()?;
    let index = root.path().join("index");
    std::fs::create_dir(&index)?;
    let base = Searcher::build_documents(
        &[SearchDocument {
            path: "note.md".into(),
            title: "Note".into(),
            text: "originalword ".repeat(10000),
        }],
        &index,
    )?;
    base.finish_build()?;
    let base = Searcher::open(&index)?;
    let session = base.fork_session_in(root.path())?;
    let session_path = session.session.as_ref().unwrap().dir.path();
    let checkpoint = root.path().join("checkpoint");
    std::fs::create_dir(&checkpoint)?;
    session.copy_committed_to(&checkpoint)?;
    let mut shared = 0;
    for entry in std::fs::read_dir(&checkpoint)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "meta.json" || name == ".managed.json" {
            continue;
        }
        assert_eq!(
            entry.metadata()?.ino(),
            std::fs::metadata(session_path.join(&name))?.ino()
        );
        shared += 1;
    }
    assert!(
        shared > 0,
        "positive control: real segments were checkpointed"
    );
    assert_ne!(
        std::fs::metadata(checkpoint.join("meta.json"))?.ino(),
        std::fs::metadata(session_path.join("meta.json"))?.ino()
    );
    let reopened = Searcher::open(&checkpoint)?;
    assert_eq!(reopened.search("originalword", 10)?.len(), 1);
    Ok(())
}
