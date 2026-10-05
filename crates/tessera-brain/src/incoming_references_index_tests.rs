use super::*;
use crate::incoming_references::{Graph, Request as IncomingRequest};
fn fixture() -> (tempfile::TempDir, BrainIndex, String) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let operational = temp.path().join("state");
    fs::create_dir_all(root.join("records")).unwrap();
    fs::create_dir_all(operational.join("source")).unwrap();
    let cache_root = operational.join("derived/brain-index-v1");
    fs::create_dir_all(&cache_root).unwrap();
    fs::write(root.join("target.md"), "# Target\n").unwrap();
    fs::write(root.join("ref.md"), "# Referrer\n[[target]]\n").unwrap();
    let brain = Uuid::new_v4().to_string();
    let source = SourceStore::open(
        &brain,
        &root,
        &operational.join("source"),
        tessera_core::source::WriteBoundary::Managed,
    )
    .unwrap();
    let (trigger, _rx) = mpsc::channel();
    (
        temp,
        BrainIndex {
            brain_id: brain,
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
        },
        Uuid::new_v4().to_string(),
    )
}
fn request(index: &BrainIndex, goal: &str) -> IncomingRequest {
    IncomingRequest {
        path: "target.md".into(),
        expected_revision: index
            .source
            .read_bounded("target.md", MAX_SOURCE_BYTES)
            .unwrap()
            .revision,
        scope: SearchScope {
            goal_id: goal.into(),
            mode: "project".into(),
            ..Default::default()
        },
        limit: 10,
        cursor: None,
    }
}
#[test]
fn incoming_revision_generation_cache_and_read_only_boundary() {
    let (_temp, index, goal) = fixture();
    let request = request(&index, &goal);
    assert!(index
        .source_backlinks(request.clone())
        .unwrap_err()
        .to_string()
        .contains("not ready"));
    index.refresh(false).unwrap();
    let response = index.source_backlinks(request.clone()).unwrap();
    assert_eq!(response.rows.len(), 1);
    assert_eq!(response.rows[0].start_line, 2);
    let original = fs::read(index.root.join("ref.md")).unwrap();
    let cache = index.read_cache().unwrap();
    let roundtrip: CachedIndex =
        serde_json::from_slice(&serde_json::to_vec(&cache).unwrap()).unwrap();
    assert_eq!(
        roundtrip
            .incoming
            .page(&request, &cache.generation)
            .unwrap()
            .0
            .len(),
        1
    );
    let first_generation = index.status().generation;
    index.refresh(false).unwrap();
    assert_eq!(index.status().generation, first_generation);
    assert_eq!(fs::read(index.root.join("ref.md")).unwrap(), original);
    index.view.write().unwrap().status.status = "stale".into();
    assert!(index
        .source_backlinks(request.clone())
        .unwrap_err()
        .to_string()
        .contains("updating"));
    index.view.write().unwrap().status.status = "ready".into();
    fs::write(index.root.join("ref.md"), "# Changed\n[[target]]\n").unwrap();
    assert!(index
        .source_backlinks(request.clone())
        .unwrap_err()
        .to_string()
        .contains("changed"));
    index.refresh(false).unwrap();
    assert!(index.source_backlinks(request.clone()).is_ok());
    fs::write(index.root.join("target.md"), "# Changed target\n").unwrap();
    assert!(index.source_backlinks(request).is_err());
}
#[test]
fn missing_backlinks_cache_never_claims_empty_and_search_remains_usable() {
    let (_temp, index, goal) = fixture();
    index.refresh(false).unwrap();
    let request = request(&index, &goal);
    let mut cached = index.read_cache().unwrap();
    cached.incoming = Graph::default();
    assert!(cached.incoming.page(&request, &cached.generation).is_err());
    let limited = Graph::build_bounded(
        &index.inventory().unwrap(),
        "records",
        0,
        crate::incoming_references::MAX_BYTES,
    );
    {
        let mut view = index.view.write().unwrap();
        Arc::get_mut(view.generation.as_mut().unwrap())
            .unwrap()
            .cache
            .incoming = limited;
    }
    assert!(index
        .source_backlinks(request.clone())
        .unwrap_err()
        .to_string()
        .contains("budget"));
    let response = index
        .search(SearchRequest {
            query: "Referrer".into(),
            scope: request.scope,
            mode: "lexical".into(),
            limit: 10,
            max_excerpt_bytes: 4096,
        })
        .unwrap();
    assert!(!response.hits.is_empty());
}
