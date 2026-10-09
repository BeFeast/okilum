use super::*;
use crate::incoming_references::Request as IncomingRequest;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Instant,
};

struct Provider {
    settings: EmbeddingSettings,
    entered: mpsc::Receiver<Value>,
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
                    if entered.send(request).is_err() {
                        break;
                    }
                    let success = wait.recv_timeout(Duration::from_secs(10)).unwrap_or(false);
                    json!({"embeddings":if success { vec![vec![1.0,0.0]; count] } else { vec![] }})
                };
                let body = response.to_string();
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
            }
        });
        Self {
            settings,
            entered: received,
            release,
            stopped,
            worker: Some(worker),
        }
    }
    fn blocked(&self) -> Value {
        self.entered.recv_timeout(Duration::from_secs(8)).unwrap()
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
    fn start_worker(&mut self) {
        self.index = BrainIndex::start(
            self.index.brain_id.clone(),
            self.index.root.clone(),
            "records".into(),
            self.index.operational.clone(),
            true,
        )
        .unwrap();
    }
    fn refresh(&self) -> thread::JoinHandle<Result<()>> {
        let index = self.index.clone();
        thread::spawn(move || index.refresh(false))
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
fn until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !condition() {
        assert!(Instant::now() < deadline, "condition timeout");
        thread::sleep(Duration::from_millis(10));
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
    // Start a new index/worker from the persisted partial cache.
    fixture.start_worker();
    let pending = provider.blocked();
    assert_eq!(pending["input"].as_array().unwrap().len(), 3);
    fixture.usable("indexing");
    provider.release.send(true).unwrap();
    until(|| fixture.index.status().semantic_status == "ready");
    fixture.usable("ready");
}

#[test]
fn source_settings_and_rebuild_invalidation_stop_obsolete_embedding_tail() {
    for change in ["source", "settings", "rebuild"] {
        let provider = Provider::start();
        let mut fixture = Fixture::new(&provider, 10);
        fixture.start_worker();
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
                until(|| fixture.index.view.read().unwrap().epoch > epoch);
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
        until(|| {
            fixture.index.status().status == "ready"
                && fixture.index.status().semantic_status == "unconfigured"
        });
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
