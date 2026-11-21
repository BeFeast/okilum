//! #1944: pins the TLS/proxy policy of every outbound HTTP/WebSocket client in
//! the workspace. A production client either builds through `okilum_http`
//! (explicit SChannel + WinHTTP static/PAC/WPAD on Windows) or is listed here
//! with the reason it deliberately keeps another policy. A new raw
//! `reqwest`/`tungstenite` client, or a listed one that moves, fails this test,
//! so the Windows policy can no longer depend on Cargo feature unification.
//! Design: docs/windows-network-policy.md.
use std::{collections::BTreeMap, fs, path::Path};

/// Construction sites that pick a TLS backend or proxy policy on their own.
const PATTERNS: &[&str] = &[
    "Client::builder()",
    "Client::new()",
    "ClientBuilder::new(",
    "reqwest::get(",
    "blocking::get(",
    "client_tls_with_config(",
    "tungstenite::connect(",
];

/// Path, raw construction sites in that file, and why it stays outside the policy.
const EXCEPTIONS: &[(&str, usize, &str)] = &[
    (
        "crates/okilum-http/src/lib.rs",
        2,
        "the shared policy itself",
    ),
    (
        "crates/okilum-core/src/remote_log/transport.rs",
        1,
        "cfg(test) synthetic-clock client, loopback HTTP only",
    ),
    (
        "crates/okilum-sync/src/lib.rs",
        1,
        "Syncthing REST: literal loopback, no proxy",
    ),
    (
        "crates/okilum-sync-controller/src/pairing.rs",
        1,
        "sync pairing: pinned root, explicitly direct",
    ),
    (
        "crates/okilum-sync-controller/src/sidecar/macos/connect.rs",
        1,
        "macOS-only sidecar on loopback",
    ),
    (
        "crates/okilum-appimage/src/lib.rs",
        1,
        "Linux AppImage updater, not in the Windows package",
    ),
    (
        "crates/okilum-shell/src/reader_project_navigation.rs",
        1,
        "cfg(test) loopback WebSocket probe",
    ),
    // okilum-brain links into okilum-cored, not into the Windows package.
    (
        "crates/okilum-brain/src/t3.rs",
        2,
        "okilum-cored only: T3 ticket + WebSocket",
    ),
    (
        "crates/okilum-brain/src/chat.rs",
        1,
        "okilum-cored only: model provider",
    ),
    (
        "crates/okilum-brain/src/retrieval.rs",
        1,
        "okilum-cored only: retrieval provider",
    ),
    (
        "crates/okilum-brain/src/settings.rs",
        1,
        "okilum-cored only: provider settings probe",
    ),
    (
        "crates/okilum-brain/src/todoist.rs",
        1,
        "okilum-cored only: Todoist",
    ),
];

/// Raw construction that still carries the shared policy: each file must keep
/// every marker. The T3 WebSocket cannot use reqwest, so it takes the
/// `okilum_http` socket route, tunnels through the proxy with CONNECT and picks
/// SChannel explicitly on Windows.
const POLICED: &[(&str, usize, &[&str])] = &[(
    "crates/okilum-t3/src/stream.rs",
    1,
    &[
        "okilum_http::local_network_socket_route(&url)",
        "okilum_http::connect_tunnel(",
        "tungstenite::Connector::NativeTls(",
        "client_tls_with_config(url.as_str(), stream, Some(config), connector()?)",
    ],
)];

fn test_only(relative: &str) -> bool {
    let name = relative.rsplit('/').next().unwrap_or_default();
    relative.split('/').any(|part| {
        matches!(
            part,
            "tests" | "test_support" | "examples" | "benches" | "testing"
        )
    }) || name == "tests.rs"
        || name == "testing.rs"
        || name.ends_with("_tests.rs")
        || name.ends_with("_test.rs")
}

fn scan(dir: &Path, root: &Path, found: &mut BTreeMap<String, usize>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            scan(&path, root, found);
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if test_only(&relative) {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        let count: usize = PATTERNS.iter().map(|p| text.matches(p).count()).sum();
        if count > 0 {
            found.insert(relative, count);
        }
    }
}

fn workspace() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

#[test]
fn every_outbound_client_takes_the_shared_policy_or_a_recorded_exception() {
    let root = workspace();
    let mut found = BTreeMap::new();
    scan(&root.join("crates"), &root, &mut found);
    // Positive control: the scanner really sees the shared builder's sites.
    assert_eq!(found.get("crates/okilum-http/src/lib.rs"), Some(&2));
    let expected: BTreeMap<String, usize> = EXCEPTIONS
        .iter()
        .map(|(path, count, _)| ((*path).to_owned(), *count))
        .chain(
            POLICED
                .iter()
                .map(|(path, count, _)| ((*path).to_owned(), *count)),
        )
        .collect();
    let unexpected: Vec<_> = found
        .iter()
        .filter(|(path, count)| expected.get(*path) != Some(count))
        .collect();
    assert!(
        unexpected.is_empty(),
        "raw HTTP/WebSocket clients outside okilum_http (build through \
         okilum_http::builder/async_builder, or record the exception and its \
         reason in this file and docs/windows-network-policy.md): {unexpected:?}"
    );
    let stale: Vec<_> = expected
        .keys()
        .filter(|path| !found.contains_key(*path))
        .collect();
    assert!(
        stale.is_empty(),
        "exceptions no longer present; drop them here and in the doc: {stale:?}"
    );
}

/// The paths this issue moved must keep building through the shared policy, and
/// each keeps its own pre-#1944 connect budget instead of the shared 5 s default
/// (which also applies on macOS/Linux). `None`: the policy's default is the
/// client's own, unchanged budget.
const MIGRATED: &[(&str, &str, Option<&str>)] = &[
    (
        "crates/okilum-core/src/remote_log/transport.rs",
        "okilum_http::async_builder(",
        Some(".connect_timeout(Duration::from_secs(3))"),
    ),
    (
        "crates/okilum-core/src/log/http/mod.rs",
        "okilum_http::builder(",
        None,
    ),
    (
        "crates/okilum-shell/src/reader_feedback/model.rs",
        "okilum_http::async_client(",
        None,
    ),
    (
        "crates/okilum-shell/src/updater/whats_new/model.rs",
        "okilum_http::builder(",
        Some(".connect_timeout(profile.first_connect)"),
    ),
    (
        "crates/okilum-t3/src/network.rs",
        "okilum_http::local_network_builder(",
        None,
    ),
    (
        "crates/okilum-t3/src/mcp.rs",
        "crate::network::builder(",
        Some(".connect_timeout(Duration::from_secs(10))"),
    ),
    (
        "crates/okilum-t3/src/session.rs",
        "crate::network::builder(",
        Some(".connect_timeout(CONNECT_TIMEOUT)"),
    ),
    (
        "crates/okilum-t3/src/oauth.rs",
        "crate::network::builder(",
        Some(".connect_timeout(Duration::from_secs(10))"),
    ),
    (
        "crates/okilum-t3/src/stream/auth.rs",
        "crate::network::builder(",
        Some(".connect_timeout(timeout)"),
    ),
    (
        "crates/okilum-t3/src/projects.rs",
        "crate::network::builder(",
        Some(".connect_timeout(std::time::Duration::from_secs(10))"),
    ),
    // Board issue reads had a 10 s total and no connect cap before #1944.
    (
        "crates/okilum-t3/src/board.rs",
        "crate::network::builder(",
        Some(".connect_timeout(Duration::from_secs(10))"),
    ),
    (
        "crates/okilum-t3/src/board/live.rs",
        "crate::network::builder(",
        Some(".connect_timeout(Duration::from_secs(3))"),
    ),
    (
        "crates/okilum-t3/src/board/peek.rs",
        "crate::network::builder(",
        Some(".connect_timeout(Duration::from_secs(3))"),
    ),
    // Multi-origin clients: the proxy is decided per request's origin.
    (
        "crates/okilum-shell/src/note_paste/fetch.rs",
        "okilum_http::per_request_builder(",
        Some(".connect_timeout(TIMEOUT)"),
    ),
    (
        "crates/okilum-shell/src/note_paste/download.rs",
        "okilum_http::per_request_builder(",
        Some(".connect_timeout(TIMEOUT)"),
    ),
    (
        "crates/okilum-t3/src/projects/icon.rs",
        "okilum_http::local_network_per_request_builder(",
        Some(".connect_timeout(std::time::Duration::from_secs(5))"),
    ),
    (
        "crates/okilum-t3/src/board/delivery/read.rs",
        "okilum_http::local_network_per_request_builder(",
        Some(".connect_timeout(Duration::from_secs(3))"),
    ),
];

#[test]
fn migrated_clients_build_through_okilum_http_with_their_own_connect_budget() {
    let root = workspace();
    for (path, marker, connect) in MIGRATED {
        let text = fs::read_to_string(root.join(path)).unwrap();
        let sites: Vec<_> = text.match_indices(marker).map(|(at, _)| at).collect();
        assert!(!sites.is_empty(), "{path} no longer uses {marker}");
        let Some(connect) = connect else { continue };
        for at in sites {
            // The builder chain runs from the policy call to its `.build()`.
            let chain = &text[at..];
            let chain = &chain[..chain.find(".build()").unwrap_or(chain.len())];
            assert!(
                chain.contains(connect),
                "{path}: client from {marker} lost its connect budget {connect}"
            );
        }
    }
}

#[test]
fn policed_raw_clients_keep_the_shared_route_and_tls_choice() {
    let root = workspace();
    for (path, _, markers) in POLICED {
        let text = fs::read_to_string(root.join(path)).unwrap();
        for marker in *markers {
            assert!(text.contains(marker), "{path} lost {marker}");
        }
    }
    // The Windows connector exists only with tungstenite's native-tls feature,
    // enabled for Windows in okilum-t3's own manifest, not by unification.
    let manifest = fs::read_to_string(root.join("crates/okilum-t3/Cargo.toml")).unwrap();
    let windows = manifest
        .split("[target.'cfg(windows)'.dependencies]")
        .nth(1)
        .expect("okilum-t3 has Windows dependencies");
    let windows = windows.split("\n[").next().unwrap_or_default();
    assert!(windows.contains("\"native-tls\"]"), "{windows}");
    assert!(windows.contains("native-tls = "), "{windows}");
}

/// T3 and Board peers on a LAN/tailnet literal stay direct on Windows.
#[test]
fn t3_and_board_use_the_local_network_policy() {
    let root = workspace();
    let network = fs::read_to_string(root.join("crates/okilum-t3/src/network.rs")).unwrap();
    assert!(network.contains("okilum_http::local_network_builder("));
    assert!(!network.contains("okilum_http::builder("));
    let lib = fs::read_to_string(root.join("crates/okilum-http/src/lib.rs")).unwrap();
    assert!(lib.contains("blocking_builder(endpoint, version, timeout, is_private_literal)"));
}

/// #1944 review: the Windows per-request hook is installed on every build. An
/// environment proxy no longer skips it, because reqwest's own matcher would
/// then apply `HTTP_PROXY` to http only and never read PAC for https.
#[test]
fn per_request_hook_is_installed_whatever_the_environment() {
    let lib = fs::read_to_string(workspace().join("crates/okilum-http/src/lib.rs")).unwrap();
    let body = &lib[lib.find("fn per_request(").expect("per_request")..];
    let body = &body[..body.find("\n}\n").expect("end of per_request")];
    assert!(
        body.contains(".proxy(route::PerRequest::new(direct, windows_proxy::lookup).into_proxy())"),
        "{body}"
    );
    assert!(!body.contains("env_proxy"), "{body}");
}

/// Crate-only Windows test builds must link the same backend as okilum.exe:
/// each crate that owns a migrated client depends on okilum-http directly.
#[test]
fn client_crates_depend_on_okilum_http_directly() {
    let root = workspace();
    for krate in ["okilum-core", "okilum-t3", "okilum-shell"] {
        let manifest =
            fs::read_to_string(root.join("crates").join(krate).join("Cargo.toml")).unwrap();
        let dependencies = manifest
            .split("[dev-dependencies]")
            .next()
            .unwrap_or_default();
        assert!(
            dependencies.contains("okilum-http = { path = \"../okilum-http\" }"),
            "{krate} must depend on okilum-http outside dev-dependencies"
        );
    }
}
