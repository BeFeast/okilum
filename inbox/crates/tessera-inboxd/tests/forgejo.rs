use serde_json::json;
use tessera_inboxd::forgejo::Cache;
use uuid::Uuid;
#[test]
fn cache_freshness_owner_and_unavailability_are_explicit() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("cache");
    let cache = Cache(path.clone());
    let owner = Uuid::new_v4();
    assert!(cache.read(owner, 100).is_err());
    let mut value = json!({"version":1,"owner_id":owner,"projects":{},"discovered_at":100,"error":null,"repos":[{"id":1,"synced_at":100,"error":null,"issues":[],"pulls":[],"releases":[]}]});
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(cache.read(owner, 100).unwrap()["repos"][0]["stale"], false);
    assert_eq!(cache.read(owner, 1001).unwrap()["stale"], true);
    assert!(cache.read(Uuid::new_v4(), 100).is_err());
    assert_eq!(cache.read(owner, 99).unwrap()["stale"], true);
    value["error"] = json!("source_unavailable");
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(cache.read(owner, 101).unwrap()["repos"][0]["stale"], true);
    std::fs::write(&path, b"{partial").unwrap();
    assert!(cache.read(owner, 100).is_err());
}
