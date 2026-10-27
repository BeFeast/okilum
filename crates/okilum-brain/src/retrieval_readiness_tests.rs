use super::*;
use crate::incoming_references::Request as IncomingRequest;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

enum ProviderEvent {
    Embedding(Value),
    Refreshed(Result<()>),
}

struct Provider {
    settings: EmbeddingSettings,
    entered: mpsc::Receiver<ProviderEvent>,
    events: mpsc::Sender<ProviderEvent>,
    release: mpsc::Sender<bool>,
    stopped: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Provider {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let settings = EmbeddingSettings {
            base_url: format!("http://{}", listener.local_addr().unwrap()),
            model: "controlled-embedding".into(),
            digest: "a".repeat(64),
            dimensions: 2,
        };
        let model = settings.clone();
        let (entered, received) = mpsc::channel();
        let events = entered.clone();
        let (release, wait) = mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let worker = thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(stream) => stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("fixture accept: {e}"),
                };
                let (header, request) = read_request(&mut stream);
                let response = if header.starts_with("GET /api/tags ") {
                    json!({"models":[{"name":model.model,"digest":model.digest}]})
                } else {
                    assert!(header.starts_with("POST /api/embed "));
                    let count = request["input"].as_array().unwrap().len();
                    if entered.send(ProviderEvent::Embedding(request)).is_err() {
                        break;
                    }
                    let success = wait.recv().unwrap_or(false);
                    json!({"embeddings":if success { vec![vec![1.0,0.0]; count] } else { vec![] }})
                };
                let body = response.to_string();
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
            }
        });
        Self {
            settings,
            entered: received,
            events,
            release,
            stopped,
            worker: Some(worker),
        }
    }
    fn blocked(&self) -> Value {
        match self.entered.recv().unwrap() {
            ProviderEvent::Embedding(request) => request,
            ProviderEvent::Refreshed(result) => {
                panic!("refresh completed before embedding: {result:?}")
            }
        }
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = self.release.send(false);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn read_request(stream: &mut TcpStream) -> (String, Value) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut b = [0];
        stream.read_exact(&mut b).unwrap();
        bytes.push(b[0]);
    }
    let header = String::from_utf8(bytes).unwrap();
    let length = header
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    (
        header,
        if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap()
        },
    )
}
struct Fixture {
    _temp: tempfile::TempDir,
    index: Arc<BrainIndex>,
    _triggers: mpsc::Receiver<bool>,
    goal: String,
}
impl Fixture {
    fn new(provider: &Provider, references: usize) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("brain");
        let operational = temp.path().join("state");
        fs::create_dir_all(root.join("records")).unwrap();
        fs::create_dir_all(operational.join("source")).unwrap();
        let cache_root = operational.join("derived/brain-index-v1");
        fs::create_dir_all(&cache_root).unwrap();
        fs::write(root.join("target.md"), "# Target\nshipping\n").unwrap();
        for i in 0..references {
            fs::write(
                root.join(format!("ref{i:02}.md")),
                format!("# Referrer{i}\nshipping [[target]]\n"),
            )
            .unwrap();
        }
        fs::write(
            operational.join("retrieval-settings.json"),
            serde_json::to_vec(&provider.settings).unwrap(),
        )
        .unwrap();
        let brain_id = Uuid::new_v4().to_string();
        let source =
            SourceStore::open_read_only(&brain_id, &root, &operational.join("source")).unwrap();
        let (trigger, triggers) = mpsc::channel();
        let index = Arc::new(BrainIndex {
            brain_id,
            root,
            records_dir: "records".into(),
            operational,
            cache_root,
            source,
            view: RwLock::new(View {
                epoch: 0,
                status: IndexStatus::default(),
                generation: None,
            }),
            trigger,
            _watcher: Mutex::new(None),
        });
        Self {
            _temp: temp,
            index,
            _triggers: triggers,
            goal: Uuid::new_v4().to_string(),
        }
    }
    fn restart_from_partial_cache(&mut self) {
        let previous = &self.index;
        let (trigger, requests) = mpsc::channel();
        // This test owns cache persistence/reuse, not watcher/debounce timing.
        // Reopen real source handles and retain only the persisted cache; the
        // fresh view cannot accidentally reuse the previous in-memory vectors.
        self.index = Arc::new(BrainIndex {
            brain_id: previous.brain_id.clone(),
            root: previous.root.clone(),
            records_dir: previous.records_dir.clone(),
            operational: previous.operational.clone(),
            cache_root: previous.cache_root.clone(),
            source: SourceStore::open_read_only(
                &previous.brain_id,
                &previous.root,
                &previous.operational.join("source"),
            )
            .unwrap(),
            view: RwLock::new(View {
                epoch: 0,
                status: IndexStatus::default(),
                generation: None,
            }),
            trigger,
            _watcher: Mutex::new(None),
        });
        self._triggers = requests;
    }
    fn refresh(&self) -> thread::JoinHandle<Result<()>> {
        let index = self.index.clone();
        thread::spawn(move || index.refresh(false))
    }
    fn observed_refresh(&self, provider: &Provider) -> thread::JoinHandle<()> {
        let index = self.index.clone();
        let events = provider.events.clone();
        thread::spawn(move || {
            events
                .send(ProviderEvent::Refreshed(
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| index.refresh(false)))
                        .unwrap_or_else(|_| Err(anyhow::anyhow!("refresh thread panicked"))),
                ))
                .unwrap();
        })
    }
    fn incoming(&self) -> IncomingRequest {
        IncomingRequest {
            path: "target.md".into(),
            expected_revision: self
                .index
                .source
                .read_bounded("target.md", MAX_SOURCE_BYTES)
                .unwrap()
                .revision,
            scope: SearchScope {
                goal_id: self.goal.clone(),
                mode: "project".into(),
                ..Default::default()
            },
            limit: 1,
            cursor: None,
        }
    }
    fn search(&self, mode: &str) -> SearchRequest {
        SearchRequest {
            query: "shipping".into(),
            scope: self.incoming().scope,
            mode: mode.into(),
            limit: 5,
            max_excerpt_bytes: 4096,
        }
    }
    fn usable(&self, semantics: &str) {
        assert_eq!(self.index.status().status, "ready");
        assert_eq!(self.index.status().semantic_status, semantics);
        assert!(!self
            .index
            .search(self.search("lexical"))
            .unwrap()
            .hits
            .is_empty());
        let page = self.index.source_backlinks(self.incoming()).unwrap();
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].start_line, 2);
        assert_eq!(
            page.rows[0].revision,
            self.index
                .source
                .read_bounded(&page.rows[0].path, MAX_SOURCE_BYTES)
                .unwrap()
                .revision
        );
    }
}
#[test]
fn blocked_document_embeddings_leave_lexical_backlinks_ready_then_publish_new_semantic_generation()
{
    let provider = Provider::start();
    let fixture = Fixture::new(&provider, 2);
    let canonical = fs::read(fixture.index.root.join("ref00.md")).unwrap();
    let refresh = fixture.refresh();
    let request = provider.blocked();
    assert_eq!(request["input"].as_array().unwrap().len(), 3);
    assert!(!request["input"][0]
        .as_str()
        .unwrap()
        .starts_with(QUERY_PREFIX));
    fixture.usable("indexing");
    assert_eq!(
        fixture.index.status().model.unwrap(),
        provider.settings.metadata()
    );
    let hybrid = fixture.index.search(fixture.search("hybrid")).unwrap();
    assert_eq!(hybrid.mode_used, "lexical");
    assert!(!hybrid.warnings.is_empty());
    assert_eq!(
        fixture
            .index
            .search(fixture.search("semantic"))
            .unwrap_err()
            .downcast_ref::<RetrievalError>()
            .unwrap()
            .code,
        "semantic_unavailable"
    );
    let mut next = fixture.incoming();
    next.cursor = fixture
        .index
        .source_backlinks(next.clone())
        .unwrap()
        .next_cursor;
    assert!(next.cursor.is_some());
    let first = fixture.index.status().generation.unwrap();
    assert!(fixture
        .index
        .read_cache()
        .unwrap()
        .chunks
        .iter()
        .all(|c| c.vector.is_none()));
    provider.release.send(true).unwrap();
    refresh.join().unwrap().unwrap();
    fixture.usable("ready");
    assert_ne!(
        fixture.index.status().generation.as_deref(),
        Some(first.as_str())
    );
    assert!(fixture.index.source_backlinks(next).is_err());
    provider.release.send(true).unwrap();
    assert!(!fixture
        .index
        .search(fixture.search("semantic"))
        .unwrap()
        .hits
        .is_empty());
    assert!(provider.blocked()["input"][0]
        .as_str()
        .unwrap()
        .starts_with(QUERY_PREFIX));
    assert_eq!(
        fs::read(fixture.index.root.join("ref00.md")).unwrap(),
        canonical
    );
    let second = fixture.index.status().generation;
    fixture.index.refresh(false).unwrap();
    assert_eq!(fixture.index.status().generation, second);
}

#[test]
fn failed_enrichment_retains_partial_vectors_and_restart_reuses_only_complete_vectors() {
    let provider = Provider::start();
    let mut fixture = Fixture::new(&provider, 10);
    let refresh = fixture.refresh();
    let first = provider.blocked();
    assert_eq!(first["input"].as_array().unwrap().len(), 8);
    provider.release.send(true).unwrap();
    let second = provider.blocked();
    assert_eq!(second["input"].as_array().unwrap().len(), 3);
    provider.release.send(false).unwrap();
    refresh.join().unwrap().unwrap();
    fixture.usable("unavailable");
    let cached = fixture.index.read_cache().unwrap();
    assert_eq!(
        cached.chunks.iter().filter(|c| c.vector.is_some()).count(),
        8
    );
    // Exercise the same-document fast path first: missing vectors must stay pending.
    let refresh = fixture.refresh();
    let pending = provider.blocked();
    assert_eq!(pending["input"].as_array().unwrap().len(), 3);
    fixture.usable("indexing");
    provider.release.send(false).unwrap();
    refresh.join().unwrap().unwrap();
    // Restart from persisted bytes, then observe the actual refresh result.
    fixture.restart_from_partial_cache();
    let refresh = fixture.observed_refresh(&provider);
    let pending = provider.blocked();
    assert_eq!(pending["input"].as_array().unwrap().len(), 3);
    fixture.usable("indexing");
    provider.release.send(true).unwrap();
    match provider.entered.recv().unwrap() {
        ProviderEvent::Refreshed(result) => result.unwrap(),
        ProviderEvent::Embedding(_) => panic!("restart requested more than the missing vectors"),
    }
    refresh.join().unwrap();
    fixture.usable("ready");
}

#[test]
fn source_settings_and_rebuild_invalidation_stop_obsolete_embedding_tail() {
    for change in ["source", "settings", "rebuild"] {
        let provider = Provider::start();
        let fixture = Fixture::new(&provider, 10);
        // Drive refresh/invalidation directly: this contract is independent of
        // filesystem notification latency and the background worker's debounce.
        let refresh = fixture.observed_refresh(&provider);
        let first_call = provider.blocked();
        assert_eq!(first_call["input"].as_array().unwrap().len(), 8);
        fixture.usable("indexing");
        let first = fixture.index.status().generation.unwrap();
        let epoch = fixture.index.view.read().unwrap().epoch;
        let pointer = fs::read(fixture.index.cache_root.join("current.json")).unwrap();
        match change {
            "source" => {
                fs::write(
                    fixture.index.root.join("ref00.md"),
                    "# Changed\ncurrent shipping [[target]]\n",
                )
                .unwrap();
                // Model the watcher's invalidation before releasing the provider.
                fixture.index.rebuild().unwrap();
            }
            "settings" => {
                let mut settings = provider.settings.clone();
                settings.base_url.push('/');
                fs::write(
                    fixture.index.operational.join("retrieval-settings.json"),
                    serde_json::to_vec(&settings).unwrap(),
                )
                .unwrap();
            }
            _ => {
                fixture.index.rebuild().unwrap();
                assert!(fixture.index.view.read().unwrap().epoch > epoch);
            }
        }
        assert_eq!(
            fs::read(fixture.index.cache_root.join("current.json")).unwrap(),
            pointer
        );
        provider.release.send(true).unwrap();
        match provider.entered.recv().unwrap() {
            ProviderEvent::Refreshed(result) => {
                let error = result.unwrap_err().to_string();
                assert!(
                    error.contains("superseded") || error.contains("settings changed"),
                    "{error}"
                );
            }
            ProviderEvent::Embedding(_) => {
                panic!("obsolete refresh started another embedding batch")
            }
        }
        refresh.join().unwrap();
        // rebuild/source queue forced work; settings invalidation queues ordinary work.
        assert_eq!(fixture._triggers.try_recv().unwrap(), change != "settings");
        let refresh = fixture.observed_refresh(&provider);
        let new_call = provider.blocked();
        // The next provider call belongs to a new complete phase1, not the old tail.
        assert_ne!(
            fixture.index.status().generation.as_deref(),
            Some(first.as_str())
        );
        assert!(fixture.index.view.read().unwrap().epoch > epoch);
        assert_eq!(new_call["input"].as_array().unwrap().len(), 8);
        assert_eq!(
            fixture
                .index
                .read_cache()
                .unwrap()
                .chunks
                .iter()
                .filter(|c| c.vector.is_some())
                .count(),
            0
        );
        if change == "source" {
            assert!(new_call["input"].to_string().contains("current shipping"));
        }
        fs::remove_file(fixture.index.operational.join("retrieval-settings.json")).unwrap();
        provider.release.send(true).unwrap();
        match provider.entered.recv().unwrap() {
            ProviderEvent::Refreshed(result) => {
                assert!(result.unwrap_err().to_string().contains("settings changed"));
            }
            ProviderEvent::Embedding(_) => {
                panic!("removed settings allowed an obsolete embedding tail")
            }
        }
        refresh.join().unwrap();
        assert!(!fixture._triggers.try_recv().unwrap());
        fixture.index.refresh(false).unwrap();
        fixture.usable("unconfigured");
    }
}

#[test]
fn obsolete_publications_preserve_pointer_and_unreadable_optional_settings_leave_search_ready() {
    let provider = Provider::start();
    let fixture = Fixture::new(&provider, 2);
    let settings = fixture.index.operational.join("retrieval-settings.json");
    fs::remove_file(&settings).unwrap();
    // A directory is a portable positive control for an unreadable settings file.
    fs::create_dir(&settings).unwrap();
    fixture.index.refresh(false).unwrap();
    fixture.usable("unconfigured");
    assert!(fixture
        .index
        .status()
        .warnings
        .iter()
        .any(|w| w.contains("configuration unavailable")));
    let cache = fixture.index.read_cache().unwrap();
    let pointer = fs::read(fixture.index.cache_root.join("current.json")).unwrap();
    let before_dirs = fs::read_dir(&fixture.index.cache_root).unwrap().count();
    let configuration = fixture.index.configuration_revision();
    let epoch = fixture.index.view.read().unwrap().epoch;
    fixture.index.rebuild().unwrap();
    for owner in [None, Some(cache.generation.as_str())] {
        assert!(fixture
            .index
            .publish_generation(
                cache.clone(),
                None,
                "unconfigured",
                epoch,
                &configuration,
                owner
            )
            .is_err());
        assert_eq!(
            fs::read(fixture.index.cache_root.join("current.json")).unwrap(),
            pointer
        );
        assert_eq!(
            fs::read_dir(&fixture.index.cache_root).unwrap().count(),
            before_dirs
        );
        assert_eq!(fixture.index.status().status, "indexing");
    }
}
