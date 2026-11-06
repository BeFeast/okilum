use super::*;
use ::core::prelude::v1::test;
use std::sync::Mutex;
struct Surface;
impl Render for Surface {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

#[test]
fn recognizes_only_same_origin_pair_links_and_keeps_secrets_out_of_debug() {
    let env = Environment::new("app", "https://t3.example").unwrap();
    let code = "23456789ABCD";
    let text = format!("https://t3.example/pair#token={code}");
    let link = PairLink::recognize(&env, &text).expect("positive control: T3's Copy format");
    assert_eq!(link.credential(), code);
    assert!(link.matches_clipboard(&text));
    assert!(!link.matches_clipboard("a newer clipboard value"));
    assert!(!format!("{link:?}").contains(code));
    for invalid in [
        "https://foreign.example/pair#token=23456789ABCD",
        "http://t3.example/pair#token=23456789ABCD",
        "https://t3.example:444/pair#token=23456789ABCD",
        "https://t3.example.evil/pair#token=23456789ABCD",
        "https://user@t3.example/pair#token=23456789ABCD",
        "https://t3.example/pair?host=foreign.example#token=23456789ABCD",
        "https://t3.example/pair#token=first&token=second",
        "https://t3.example/pair#token=",
        "https://t3.example/pair#token=with%0Anewline",
        "https://t3.example/settings/connections#token=23456789ABCD",
        "an ordinary copied sentence",
        "23456789ABCD",
    ] {
        assert!(PairLink::recognize(&env, invalid).is_none());
    }
    assert!(PairLink::recognize(&env, "https://T3.EXAMPLE:443/pair#token=23456789ABCD").is_some());
    assert!(PairLink::manual(&env, code).is_some());
    assert!(PairLink::manual(&env, "https://foreign.example/pair#token=23456789ABCD").is_none());
    assert_eq!(
        connections_url(&env),
        "https://t3.example/settings/connections"
    );
}

#[test]
fn every_state_has_plain_copy_and_one_primary_action() {
    for stage in [
        Stage::SignedOut,
        Stage::NotConnected,
        Stage::Waiting,
        Stage::Connecting,
        Stage::Connected,
        Stage::Offline,
        Stage::Expired,
        Stage::Error,
    ] {
        assert!(!stage.sentence().is_empty());
        assert!(!stage.action().is_empty());
        for technical in ["https://", "token", "orchestration:", "UTC", "Diagnostics"] {
            assert!(!stage.sentence().contains(technical));
        }
    }
    assert_eq!(Stage::NotConnected.action(), "Connect manager");
    assert_eq!(Stage::Expired.action(), "Reconnect");
}

#[gpui::test]
fn clipboard_is_untouched_without_consent_for_foreign_links_and_after_cancel(
    cx: &mut TestAppContext,
) {
    cx.update(crate::test_init);
    cx.update(reader_features::enable_test_modules);
    let env = Environment::new("app", "https://t3.example").unwrap();
    let link = "https://t3.example/pair#token=23456789ABCD";
    cx.update(|cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(link.into()));
        activated(cx);
        assert_eq!(entry(&env, cx).stage, Stage::NotConnected);
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some(link)
        );
        begin(&env, cx);
        assert_eq!(entry(&env, cx).stage, Stage::Waiting);
        let foreign = "https://foreign.example/pair#token=23456789ABCD";
        cx.write_to_clipboard(ClipboardItem::new_string(foreign.into()));
        activated(cx);
        assert_eq!(entry(&env, cx).stage, Stage::Waiting);
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some(foreign)
        );
        cancel(&env, cx);
        cx.write_to_clipboard(ClipboardItem::new_string(link.into()));
        activated(cx);
        assert_eq!(entry(&env, cx).stage, Stage::NotConnected);
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some(link)
        );
    });
}

#[gpui::test]
async fn focus_return_pairs_once_narrows_scope_and_persists_only_the_native_credential(
    cx: &mut TestAppContext,
) {
    use std::io::{BufRead, BufReader, Read, Write};
    cx.update(crate::test_init);
    cx.update(reader_features::enable_test_modules);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let env =
        Environment::new("app", &format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let server = std::thread::spawn(move || {
        for (status, body) in [
            (200, serde_json::json!({"access_token":"synthetic-native-secret", "token_type":"Bearer", "expires_in":3600, "scope":"orchestration:read orchestration:operate"}).to_string()),
            (200, serde_json::json!({"access_token":"synthetic-native-secret", "token_type":"Bearer", "expires_in":3600, "scope":"orchestration:read orchestration:operate"}).to_string()),
            (401, "DO NOT LOG 23456789ABCD synthetic-native-secret".into()),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            assert_eq!(request, "POST /oauth/token HTTP/1.1\r\n");
            let mut size = 0;
            loop {
                let mut line = String::new(); reader.read_line(&mut line).unwrap();
                if line == "\r\n" { break; }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") { size = value.trim().parse().unwrap(); }
            }
            let mut body_bytes = vec![0; size]; reader.read_exact(&mut body_bytes).unwrap();
            let form: std::collections::HashMap<_, _> = url::form_urlencoded::parse(&body_bytes).collect();
            assert_eq!(form["subject_token"], "23456789ABCD");
            assert_eq!(form["scope"], "orchestration:read orchestration:operate");
            assert_eq!(form["client_os"], std::env::consts::OS);
            drop(reader);
            write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    let store = Arc::new(Mutex::new(project::credentials::TestStore::default()));
    let mcp_key = env.credential_url();
    store.lock().unwrap().entries.insert(
        mcp_key.clone(),
        ("Okilum".into(), b"synthetic-mcp-secret".to_vec()),
    );
    store.lock().unwrap().refuse_write = true;
    let store_for_focus = store.clone();
    let (root, visual) = cx.add_window_view(|window, cx| {
        cx.observe_window_activation(window, move |_, window, cx| {
            if window.is_window_active() {
                consume_clipboard(
                    project::credentials::Store {
                        test: Some(store_for_focus.clone()),
                    },
                    cx,
                );
            }
        })
        .detach();
        let surface = cx.new(|_| Surface);
        Root::new(surface, window, cx)
    });
    let text = format!("{}/pair#token=23456789ABCD", env.origin());
    visual.update(|window, cx| {
        begin(&env, cx);
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        // The production activation handler takes this same path.
        window.activate_window();
        consume_clipboard(
            project::credentials::Store {
                test: Some(store.clone()),
            },
            cx,
        );
        assert_eq!(entry(&env, cx).stage, Stage::Connecting);
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("")
        );
        activated(cx); // Duplicate activation must not exchange twice.
    });
    visual
        .condition(&root, |_, cx| entry(&env, cx).stage != Stage::Connecting)
        .await;
    visual.update(|_, cx| {
        let e = entry(&env, cx);
        assert_eq!(e.stage, Stage::Error);
        assert!(
            e.client.is_none(),
            "failed OS persistence cannot connect a session-only credential"
        );
        assert!(!store
            .lock()
            .unwrap()
            .entries
            .contains_key(&NativeToken::credential_key(&env)));
        store.lock().unwrap().refuse_write = false;
        begin(&env, cx);
        cx.write_to_clipboard(ClipboardItem::new_string(format!(
            "{}/pair#token=23456789ABCD",
            env.origin()
        )));
        consume_clipboard(
            project::credentials::Store {
                test: Some(store.clone()),
            },
            cx,
        );
    });
    visual
        .condition(&root, |_, cx| entry(&env, cx).stage != Stage::Connecting)
        .await;
    visual.update(|_, cx| {
        let e = entry(&env, cx);
        assert_eq!(e.stage, Stage::Connected);
        assert!(e.client.is_some());
        assert!(e.expiry.is_some());
        let entries = &store.lock().unwrap().entries;
        assert_eq!(entries[&mcp_key].1, b"synthetic-mcp-secret");
        let bytes = &entries[&NativeToken::credential_key(&env)].1;
        assert!(!String::from_utf8_lossy(bytes).contains("23456789ABCD"));
        NativeToken::from_secret(&env, bytes).unwrap();
        begin(&env, cx);
        let link = PairLink::recognize(&env, &format!("{}/pair#token=23456789ABCD", env.origin()))
            .unwrap();
        cx.write_to_clipboard(ClipboardItem::new_string("a newer value".into()));
        pair(
            env.clone(),
            link,
            project::credentials::Store {
                test: Some(store.clone()),
            },
            cx,
        );
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("a newer value")
        );
    });
    visual
        .condition(&root, |_, cx| entry(&env, cx).stage == Stage::Error)
        .await;
    visual.update(|_, cx| {
        let error = entry(&env, cx).error.unwrap();
        assert!(!error.contains("23456789ABCD"));
        assert!(!error.contains("synthetic-native-secret"));
        assert!(!error.contains("DO NOT LOG"));
    });
    server.join().unwrap();
}
