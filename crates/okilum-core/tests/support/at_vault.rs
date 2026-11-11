use std::path::Path;

/// Full mixed fixture, shared by core and native UI regressions.
/// The probe exercises the actual filesystem; OS names do not imply case behavior.
pub fn populate(root: &Path) -> bool {
    use base64::Engine as _;
    let probe = tempfile::Builder::new()
        .prefix("CaseProbe-")
        .tempfile_in(root)
        .unwrap();
    let upper = probe
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_ascii_uppercase();
    let case_sensitive = !root.join(upper).exists();
    let manifest: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/at-vault.json"
    )))
    .unwrap();
    let write = |path: &str, bytes: &[u8]| {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    };
    for (path, text) in manifest["text"].as_object().unwrap() {
        write(path, text.as_str().unwrap().as_bytes());
    }
    for (path, bytes) in manifest["base64"].as_object().unwrap() {
        write(
            path,
            &base64::engine::general_purpose::STANDARD
                .decode(bytes.as_str().unwrap())
                .unwrap(),
        );
    }
    if case_sensitive {
        for (path, text) in manifest["case_sensitive"].as_object().unwrap() {
            write(path, text.as_str().unwrap().as_bytes());
        }
    }
    case_sensitive
}
