//! Injected unchanged except marked API adapters into each exact benchmark pin.
use std::{collections::BTreeMap, time::Instant};
use sha2::{Digest, Sha256};
use tessera_core::{Vault, Searcher, VaultWatcher};

#[test]
fn reader_open_phase_profile() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("vault");
    std::fs::create_dir(&root).unwrap();
    let mut manifest = BTreeMap::new();
    for i in 0..5001 {
        let path = format!("note{i:04}.md");
        let body = format!("---\r\ntitle: Note {i}\r\n---\r\n# Heading {i}\r\n\r\n[[note0000]] [[note0001]]\r\n\r\n{}\r\n", "Unicode текст paragraph with **formatting** and `[[example]]`.\r\n\r\n".repeat(240));
        std::fs::write(root.join(&path), &body).unwrap();
        manifest.insert(path, body);
    }
    let bytes: usize = manifest.values().map(String::len).sum();
    let mut hash = Sha256::new();
    for (name, text) in &manifest { hash.update(name.as_bytes()); hash.update([0]); hash.update(text.as_bytes()); }
    let identity = format!("{:x}", hash.finalize());
    let cache = fixture.path().join("cache");
    for mode in ["cold_index", "warm_index"] {
        let total = Instant::now();
        // API_FIRST: production prepares first document after the full scan/index.
        let scan = Instant::now();
        // API_SCAN
        let scan_ms = scan.elapsed().as_secs_f64() * 1000.;
        let search = Instant::now();
        let searcher = Searcher::open_or_build(&vault, &cache).unwrap();
        let search_ms = search.elapsed().as_secs_f64() * 1000.;
        assert!(!searcher.search("paragraph", 5).unwrap().is_empty());
        let watch = Instant::now();
        let watcher = VaultWatcher::new(&root).unwrap();
        let watch_ms = watch.elapsed().as_secs_f64() * 1000.;
        // API_LATE_FIRST
        let whole_ms = total.elapsed().as_secs_f64() * 1000.;
        let snapshot = Instant::now();
        // API_SNAPSHOT
        let snapshot_ms = snapshot.elapsed().as_secs_f64() * 1000.;
        let links = Instant::now();
        let count: usize = manifest.keys().map(|path| vault.outbound_links(path).len()).sum();
        assert_eq!(count, 10002);
        let links_ms = links.elapsed().as_secs_f64() * 1000.;
        assert_eq!(vault.notes.len(), 5001);
        assert!(!vault.backlinks("note0000.md").is_empty());
        drop(watcher);
        for (name, text) in &manifest { assert_eq!(std::fs::read(root.join(name)).unwrap(), text.as_bytes()); }
        eprintln!("READER_PROFILE {}", serde_json::json!({"pin": env!("READER_PROFILE_PIN"), "mode":mode,"notes":5001,"bytes":bytes,"fixture_sha256":identity,"first_prepared_ms":first_ms,"scan_backlinks_ms":scan_ms,"snapshot_ms":snapshot_ms,"outbound_links_ms":links_ms,"search_ms":search_ms,"watch_ms":watch_ms,"whole_ms":whole_ms,"limits":"core backend phases; no GPUI frame/input timing; OS page cache not evicted; search uses shared open_or_build for comparable core phases, actual338 immutable-cache timing tested separately"}));
    }
}

fn prepare(vault: &Vault) {
    let doc = tessera_core::render::reader_document(vault, "note0000.md").unwrap();
    let arena = comrak::Arena::new();
    let root = comrak::parse_document(&arena, &doc.rendered, &tessera_core::render::comrak_options());
    assert!(root.children().count() > 1);
}
